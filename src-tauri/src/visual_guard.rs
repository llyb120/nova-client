//! Shared screenshot delivery and bounded pixel-only stale-target checks. No OCR or object recognition.
use xcap::image::RgbaImage;

/// Half-open rectangle in original image pixels, never display/cropped-image coordinates.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
pub(crate) struct PixelRect {
    pub left: u32,
    pub top: u32,
    pub right: u32,
    pub bottom: u32,
}
impl PixelRect {
    pub(crate) fn contains(self, x: f64, y: f64) -> bool {
        x >= self.left as f64 && x < self.right as f64
            && y >= self.top as f64 && y < self.bottom as f64
    }
}

#[derive(Clone)]
pub(crate) struct VisualGuard {
    width: u32,
    height: u32,
    source: (u32, u32),
    step: u32,
    pixels: std::sync::Arc<Vec<[u8; 3]>>,
}
impl VisualGuard {
    // At most 1600^2 RGB samples (~7.7 MB); clones share the backing storage.
    pub(crate) fn new(image: &RgbaImage) -> Self {
        let step = image.width().max(image.height()).div_ceil(1600).max(1);
        let width = image.width().div_ceil(step);
        let height = image.height().div_ceil(step);
        let pixels = (0..height).flat_map(|y| (0..width).map(move |x| {
            let p = image.get_pixel(x * step, y * step);
            [p[0], p[1], p[2]]
        })).collect();
        Self { width, height, source: image.dimensions(), step, pixels: std::sync::Arc::new(pixels) }
    }

    pub(crate) fn changed_near(&self, current: &RgbaImage, x: f64, y: f64) -> bool {
        if current.dimensions() != self.source || !x.is_finite() || !y.is_finite()
            || x < 0. || y < 0. || x >= self.source.0 as f64 || y >= self.source.1 as f64 {
            return true;
        }
        let cx = (x / self.step as f64) as i64;
        let cy = (y / self.step as f64) as i64;
        let (mut total, mut changed, mut core, mut core_changed) = (0, 0, 0, 0);
        for dy in -24i64..=24 {
            for dx in -24i64..=24 {
                let (px, py) = (cx + dx, cy + dy);
                if px < 0 || py < 0 || px >= self.width as i64 || py >= self.height as i64 { continue; }
                let old = self.pixels[(py * self.width as i64 + px) as usize];
                let new = current.get_pixel(px as u32 * self.step, py as u32 * self.step);
                let different = (0..3).any(|c| old[c].abs_diff(new[c]) > 24);
                total += 1;
                changed += u32::from(different);
                if dx.abs() <= 4 && dy.abs() <= 4 { core += 1; core_changed += u32::from(different); }
            }
        }
        // Ignore small anti-aliasing/caret noise; a changed centre or substantially changed neighbourhood counts.
        core == 0 || core_changed * 100 > core * 35 || changed * 100 > total * 12
    }
}

/// Lossless WebP for paths that still compare decoded pixels (Chrome tiles, guards).
pub(crate) fn encode_webp(image: &RgbaImage) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    xcap::image::codecs::webp::WebPEncoder::new_lossless(&mut bytes)
        .encode(image.as_raw(), image.width(), image.height(), xcap::image::ExtendedColorType::Rgba8)
        .map_err(|e| e.to_string())?;
    Ok(bytes)
}

pub(crate) fn frame_stamp(image: &RgbaImage) -> u128 {
    xxhash_rust::xxh3::xxh3_128(image.as_raw())
}

/// Stamp of the comparison image: `exclude` rectangles are zeros, matching `comparison_pixels`.
/// A repaint inside an ignored Nova/clock region does not change the stamp, so capture can
/// skip resize and encode. Visible pixels are hashed in place; the full frame is not cloned.
pub(crate) fn frame_stamp_masked(image: &RgbaImage, exclude: &[PixelRect]) -> u128 {
    if exclude.is_empty() {
        return frame_stamp(image);
    }
    let mut hasher = xxhash_rust::xxh3::Xxh3::new();
    let (w, h) = image.dimensions();
    let raw = image.as_raw();
    let mut cuts: Vec<(u32, u32)> = Vec::with_capacity(exclude.len());
    for y in 0..h {
        cuts.clear();
        for r in exclude {
            if y < r.top || y >= r.bottom || r.left >= r.right {
                continue;
            }
            let (a, b) = (r.left.min(w), r.right.min(w));
            if a < b {
                cuts.push((a, b));
            }
        }
        if cuts.is_empty() {
            let row = y as usize * w as usize * 4;
            hasher.update(&raw[row..row + w as usize * 4]);
            continue;
        }
        cuts.sort_unstable();
        let mut n = 0usize;
        for i in 0..cuts.len() {
            if n > 0 && cuts[i].0 <= cuts[n - 1].1 {
                cuts[n - 1].1 = cuts[n - 1].1.max(cuts[i].1);
            } else {
                cuts[n] = cuts[i];
                n += 1;
            }
        }
        cuts.truncate(n);
        let mut x = 0u32;
        let row = y as usize * w as usize;
        for &(a, b) in &cuts {
            if a > x {
                hasher.update(&raw[(row + x as usize) * 4..(row + a as usize) * 4]);
            }
            hash_zeros(&mut hasher, b.saturating_sub(a.max(x)) as usize * 4);
            x = b.max(x);
        }
        if x < w {
            hasher.update(&raw[(row + x as usize) * 4..(row + w as usize) * 4]);
        }
    }
    hasher.digest128()
}

fn hash_zeros(hasher: &mut xxhash_rust::xxh3::Xxh3, mut n: usize) {
    const ZEROS: [u8; 4096] = [0; 4096];
    while n > 0 {
        let k = n.min(ZEROS.len());
        hasher.update(&ZEROS[..k]);
        n -= k;
    }
}

/// Coarse pulse used to end an input wait early. A caret or clock is Minor; a menu or window is Content.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn frame_pulse(previous: Option<&RgbaImage>, next: &RgbaImage) -> DesktopPulse {
    let Some(previous) = previous.filter(|p| p.dimensions() == next.dimensions()) else {
        return DesktopPulse::Content;
    };
    match changed_samples(previous, next) {
        0 => DesktopPulse::Pointer,
        n if n >= SUBSTANTIAL_SAMPLES => DesktopPulse::Content,
        _ => DesktopPulse::Minor,
    }
}

#[cfg_attr(not(windows), allow(dead_code))]
fn changed_samples(a: &RgbaImage, b: &RgbaImage) -> u32 {
    let (w, h) = a.dimensions();
    let (ra, rb) = (a.as_raw(), b.as_raw());
    if ra == rb {
        return 0;
    }
    let step = 32usize;
    let mut changed = 0u32;
    let mut y = 0usize;
    while y < h as usize {
        let row = y * w as usize * 4;
        let mut x = 0usize;
        while x < w as usize {
            let i = row + x * 4;
            if ra[i].abs_diff(rb[i]) > 16 || ra[i + 1].abs_diff(rb[i + 1]) > 16 || ra[i + 2].abs_diff(rb[i + 2]) > 16 {
                changed += 1;
            }
            x += step;
        }
        y += step;
    }
    changed
}

/// Delivered comparison pixels before cursor annotation; ignored self-UI regions may be masked.
#[derive(Clone)]
pub(crate) struct Delivered {
    pub stream: String,
    pub basis: serde_json::Value,
    pub path: std::path::PathBuf,
    pub seen: std::sync::Arc<RgbaImage>,
    /// Resized frame before the sword tip is drawn. Used to retarget the marker without resizing again.
    pub clean: Option<std::sync::Arc<RgbaImage>>,
    /// Sword tip baked into `path`, in delivered image pixels. `None` means the file has no marker.
    pub marker: Option<(u32, u32)>,
    /// xxh3 of the pre-resize source. Zero means unset. Identical stamps skip resize and encode.
    pub stamp: u128,
}

/// What changed between two desktop frames, for the action settle loop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) enum DesktopPulse {
    Quiet,
    Pointer,
    Minor,
    Content,
}

/// Samples at least this many 32px-grid points before a paint counts as "the UI changed".
const SUBSTANTIAL_SAMPLES: u32 = 6;

const TILE: u32 = 32;

/// Which 32×32 blocks visibly changed (any RGB channel > 16).
pub(crate) fn changed_tiles(a: &RgbaImage, b: &RgbaImage, exclude: Option<PixelRect>) -> Option<Vec<bool>> {
    let (w, h) = a.dimensions();
    if b.dimensions() != (w, h) || w == 0 || h == 0 { return None; }
    let cols = w.div_ceil(TILE) as usize;
    let mut tiles = vec![false; cols * h.div_ceil(TILE) as usize];
    let (ra, rb) = (a.as_raw(), b.as_raw());
    // memcmp before per-pixel scans keeps unoptimized NovaDev builds fast.
    if ra == rb { return Some(tiles); }
    for y in 0..h as usize { for col in 0..cols {
        let t = y / TILE as usize * cols + col;
        let x0 = col * TILE as usize;
        let span = (y * w as usize + x0) * 4..(y * w as usize + (x0 + TILE as usize).min(w as usize)) * 4;
        if tiles[t] || ra[span.clone()] == rb[span.clone()] { continue; }
        tiles[t] = ra[span.clone()].chunks_exact(4).zip(rb[span].chunks_exact(4)).enumerate().any(|(i, (p, q))| {
            (0..3).any(|c| p[c].abs_diff(q[c]) > 16)
                && !exclude.is_some_and(|r| r.contains((x0 + i) as f64, y as f64))
        });
    } }
    Some(tiles)
}

/// Shared desktop/browser delivery: every frame is a complete image. A visibly unchanged frame
/// (same stream and geometry) reuses the previously delivered file and is marked `unchanged`.
pub(crate) fn deliver_frame(entry: &mut serde_json::Value, prior: Option<&Delivered>, stream: Option<String>,
    plain: RgbaImage, annotated: Option<&RgbaImage>, path: std::path::PathBuf,
    marker: Option<(u32, u32)>, clean: Option<std::sync::Arc<RgbaImage>>)
    -> Result<Option<Delivered>, String> {
    use serde_json::json;
    // Equal resized dimensions alone do not prove the same monitor geometry or source scale.
    // Changed exclusion geometry must reveal newly exposed areas with a fresh, unmasked image.
    // A moved sword tip is a different observation even when the desktop pixels match: the model
    // aims at the tip it sees, so the previous file's tip must not be reused.
    let basis = json!([entry["desktopBounds"], entry["originalWidth"], entry["originalHeight"], entry["ignoredRegions"]]);
    if let Some(stream) = &stream { entry["frameStream"] = json!(stream); }
    let same = prior.filter(|p| Some(&p.stream) == stream.as_ref() && p.basis == basis && p.marker == marker
        && changed_tiles(&p.seen, &plain, None).is_some_and(|c| !c.contains(&true)));
    if let Some(p) = same {
        entry["unchanged"] = json!(true);
        entry["path"] = json!(p.path);
        return Ok(Some(p.clone()));
    }
    // Lossless: click safety and seat checks read pixels back, and on UI frames this crate's
    // JPEG q90 was both larger and slower than WebP (measured 1600×900: 679KB/240ms vs 513KB/124ms).
    let path = path.with_extension("webp");
    let encoded = encode_webp(annotated.unwrap_or(&plain))?;
    std::fs::write(&path, encoded).map_err(|e| e.to_string())?;
    entry["path"] = json!(path);
    Ok(stream.map(|stream| Delivered {
        stream, basis, path, seen: std::sync::Arc::new(plain), clean, marker, stamp: 0,
    }))
}

pub(crate) fn patch_changed(before: &RgbaImage, after: &RgbaImage) -> bool {
    if before.width() == 0 || before.height() == 0 || after.width() == 0 || after.height() == 0 { return true; }
    let after = if before.dimensions() == after.dimensions() { after.clone() } else {
        xcap::image::imageops::resize(after, before.width(), before.height(), xcap::image::imageops::FilterType::Triangle)
    };
    VisualGuard::new(before).changed_near(&after, before.width() as f64 / 2., before.height() as f64 / 2.)
}

#[cfg(test)]
mod tests {
    use super::*;
    use xcap::image::Rgba;
    #[test]
    fn delivery_reuses_unchanged_frames_and_resets_on_any_other_change() {
        use serde_json::json;
        let dir = tempfile::tempdir().unwrap();
        let page = RgbaImage::from_pixel(320, 320, Rgba([255, 255, 255, 255]));
        let deliver = |id: &str, prior: Option<&Delivered>, stream: &str, bounds: i64, image: &RgbaImage, annotated: Option<&RgbaImage>| {
            let mut e = json!({"imageId":"window-1","snapshotId":id,"desktopBounds":{"x":bounds}});
            let frame = deliver_frame(&mut e, prior, Some(stream.into()), image.clone(), annotated, dir.path().join(format!("{id}.webp")), None, None).unwrap();
            (e, frame.unwrap())
        };
        let (first, prior) = deliver("key", None, "window", 0, &page, None);
        let path = std::path::Path::new(first["path"].as_str().unwrap());
        let bytes = std::fs::read(path).unwrap();
        assert_eq!(xcap::image::guess_format(&bytes).unwrap(), xcap::image::ImageFormat::WebP);
        assert_eq!(xcap::image::load_from_memory(&bytes).unwrap().to_rgba8(), page);
        assert_eq!(first["frameStream"], "window");
        let mut annotated = page.clone();
        annotated.put_pixel(150, 150, Rgba([0, 100, 255, 255]));
        let (same, _) = deliver("same", Some(&prior), "window", 0, &page, Some(&annotated));
        assert!(same["unchanged"] == true && same["path"] == first["path"], "same sword tip reuses the file");
        let (moved, retargeted) = {
            let mut e = json!({"imageId":"window-1","snapshotId":"moved","desktopBounds":{"x":0}});
            let frame = deliver_frame(&mut e, Some(&prior), Some("window".into()), page.clone(), Some(&annotated),
                dir.path().join("moved.webp"), Some((150, 150)), None).unwrap();
            (e, frame.unwrap())
        };
        assert!(moved["unchanged"].is_null() && moved["path"] != first["path"], "a new sword tip is a new observation");
        assert_eq!(xcap::image::open(moved["path"].as_str().unwrap()).unwrap().to_rgba8(), annotated);
        assert_eq!(retargeted.marker, Some((150, 150)));
        assert_eq!(retargeted.seen.as_ref(), &page, "guard pixels never contain the cursor marker");
        let mut changed = page.clone();
        changed.put_pixel(150, 150, Rgba([0, 0, 0, 255]));
        let (next, seen) = deliver("next", Some(&prior), "window", 0, &changed, Some(&annotated));
        assert!(next["unchanged"].is_null() && next["path"] != first["path"]);
        assert_eq!(xcap::image::open(next["path"].as_str().unwrap()).unwrap().to_rgba8(), annotated);
        assert_eq!(seen.seen.as_ref(), &changed, "guard pixels never contain the cursor marker");
        // Another stream or changed geometry never reuses the prior frame.
        for (stream, bounds) in [("other", 0), ("window", 10)] {
            let (e, _) = deliver("reset", Some(&prior), stream, bounds, &page, None);
            assert!(e["unchanged"].is_null() && e["path"] != first["path"]);
        }
    }

    #[test]
    fn changed_tiles_cover_clipped_edges() {
        for size in [(1,1),(2,2),(641,479)] {
            let blank = RgbaImage::new(size.0,size.1);
            let mut edge = blank.clone();
            edge.put_pixel(size.0-1,size.1-1,Rgba([255,255,255,255]));
            assert_eq!(changed_tiles(&blank,&edge,None).unwrap().last(), Some(&true), "clipped edge cells are still detected");
        }
    }

    #[test]
    fn frame_stamp_masked_matches_zeroed_pixels_and_ignores_repaints_inside_them() {
        let page = RgbaImage::from_pixel(64, 40, Rgba([10, 20, 30, 255]));
        let exclude = [
            PixelRect { left: 4, top: 2, right: 20, bottom: 18 },
            PixelRect { left: 10, top: 8, right: 30, bottom: 12 },
        ];
        let mut masked = page.clone();
        for r in exclude {
            for y in r.top..r.bottom {
                for x in r.left..r.right {
                    masked.put_pixel(x, y, Rgba([0, 0, 0, 0]));
                }
            }
        }
        assert_eq!(frame_stamp_masked(&page, &exclude), frame_stamp(&masked));
        assert_eq!(frame_stamp_masked(&page, &[]), frame_stamp(&page));
        let mut inside = page.clone();
        inside.put_pixel(8, 6, Rgba([255, 0, 0, 255]));
        inside.put_pixel(12, 10, Rgba([0, 255, 0, 128]));
        assert_eq!(frame_stamp_masked(&inside, &exclude), frame_stamp_masked(&page, &exclude));
        let mut outside = inside.clone();
        outside.put_pixel(40, 30, Rgba([255, 0, 0, 255]));
        assert_ne!(frame_stamp_masked(&outside, &exclude), frame_stamp_masked(&page, &exclude));
        assert_eq!(frame_stamp(&page), frame_stamp(&page.clone()));
        let mut one = page.clone();
        one.put_pixel(0, 0, Rgba([1, 1, 1, 255]));
        assert_ne!(frame_stamp(&page), frame_stamp(&one));
    }

    #[test]
    fn frame_pulse_ignores_a_caret_and_notices_a_dialog() {
        let base = RgbaImage::from_pixel(320, 200, Rgba([20, 20, 20, 255]));
        assert_eq!(frame_pulse(None, &base), DesktopPulse::Content);
        assert_eq!(frame_pulse(Some(&base), &base), DesktopPulse::Pointer);
        let mut caret = base.clone();
        caret.put_pixel(0, 0, Rgba([255, 255, 255, 255]));
        assert_eq!(frame_pulse(Some(&base), &caret), DesktopPulse::Minor);
        let mut dialog = base.clone();
        for y in 0..160 { for x in 0..200 { dialog.put_pixel(x, y, Rgba([250, 250, 250, 255])); } }
        assert_eq!(frame_pulse(Some(&base), &dialog), DesktopPulse::Content);
        assert_eq!(frame_pulse(Some(&base), &RgbaImage::new(10, 10)), DesktopPulse::Content);
    }

    #[test]
    fn webp_roundtrip_preserves_pixels_and_detects_visual_changes() {
        // 密集细字、彩色控件及透明度均须逐像素保真。
        let source = RgbaImage::from_fn(240, 140, |x, y| {
            if (y / 12) % 2 == 0 && (x % 5 == 0 || (x + y) % 7 == 0) { Rgba([20, 20, 20, 255]) }
            else if (100..180).contains(&x) && (90..120).contains(&y) { Rgba([30, 110, 230, 128]) }
            else { Rgba([250, 250, 250, ((x + y) % 256) as u8]) }
        });
        let decoded = xcap::image::load_from_memory(&encode_webp(&source).unwrap()).unwrap().to_rgba8();
        assert_eq!(decoded, source);
        assert!(!patch_changed(&decoded, &source));
        let mut moved = source.clone();
        for y in 0..140 { for x in 0..240 { moved.put_pixel(x, y, *source.get_pixel((x + 3) % 240, y)); } }
        assert!(patch_changed(&decoded, &moved), "real content shifts must still be detected");
    }

    #[test]
    fn stale_targets_not_background_animation_or_caret_noise() {
        let original = RgbaImage::from_pixel(240, 140, Rgba([80, 80, 80, 255]));
        let guard = VisualGuard::new(&original);
        let mut current = original.clone();
        for y in 0..40 { for x in 180..240 { current.put_pixel(x,y,Rgba([255,0,0,255])); } }
        assert!(!guard.changed_near(&current, 30., 60.));
        for y in 52..68 { current.put_pixel(30,y,Rgba([255,255,255,255])); }
        assert!(!guard.changed_near(&current, 30., 60.));
        // Typed text beside the target counts as a change.
        let mut typed = original.clone();
        for y in 40..56 { for x in 10..60 { typed.put_pixel(x,y,Rgba([0,0,0,255])); } }
        assert!(guard.changed_near(&typed, 30., 60.));
        for y in 56..65 { for x in 26..35 { current.put_pixel(x,y,Rgba([0,255,0,255])); } }
        assert!(guard.changed_near(&current, 30., 60.));
        assert!(guard.changed_near(&current, f64::NAN, 60.));
        assert!(guard.changed_near(&RgbaImage::new(240,141),30.,60.));
    }
    #[test]
    fn high_dpi_and_image_boundaries_are_bounded() {
        let original=RgbaImage::from_pixel(3840,2160,Rgba([15,15,15,255]));
        let guard=VisualGuard::new(&original);
        assert!(guard.pixels.len() <= 1600 * 1600);
        assert!(!guard.changed_near(&original,3839.,2159.));
        assert!(guard.changed_near(&original,3840.,2159.));
        let a=RgbaImage::from_pixel(48,48,Rgba([15,15,15,255]));
        let b=RgbaImage::from_pixel(96,96,Rgba([15,15,15,255]));
        assert!(!patch_changed(&a,&b));
        assert!(patch_changed(&a,&RgbaImage::from_pixel(48,48,Rgba([255,255,255,255]))));
    }
}
