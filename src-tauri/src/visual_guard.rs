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

/// 截图统一存无损 WebP，保留 RGBA 像素，避免压缩噪声影响落点校验。
pub(crate) fn encode_webp(image: &RgbaImage) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    xcap::image::codecs::webp::WebPEncoder::new_lossless(&mut bytes)
        .encode(image.as_raw(), image.width(), image.height(), xcap::image::ExtendedColorType::Rgba8)
        .map_err(|e| e.to_string())?;
    Ok(bytes)
}

/// Delivered comparison pixels before cursor annotation; ignored self-UI regions may be masked.
#[derive(Clone)]
pub(crate) struct Delivered {
    pub stream: String,
    pub basis: serde_json::Value,
    pub path: std::path::PathBuf,
    pub seen: std::sync::Arc<RgbaImage>,
}

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
    plain: RgbaImage, annotated: Option<&RgbaImage>, path: std::path::PathBuf)
    -> Result<Option<Delivered>, String> {
    use serde_json::json;
    // Equal resized dimensions alone do not prove the same monitor geometry or source scale.
    // Changed exclusion geometry must reveal newly exposed areas with a fresh, unmasked image.
    let basis = json!([entry["desktopBounds"], entry["originalWidth"], entry["originalHeight"], entry["ignoredRegions"]]);
    if let Some(stream) = &stream { entry["frameStream"] = json!(stream); }
    let same = prior.filter(|p| Some(&p.stream) == stream.as_ref() && p.basis == basis
        && changed_tiles(&p.seen, &plain, None).is_some_and(|c| !c.contains(&true)));
    if let Some(p) = same {
        entry["unchanged"] = json!(true);
        entry["path"] = json!(p.path);
        return Ok(Some(p.clone()));
    }
    let path = path.with_extension("webp");
    std::fs::write(&path, encode_webp(annotated.unwrap_or(&plain))?).map_err(|e| e.to_string())?;
    entry["path"] = json!(path);
    Ok(stream.map(|stream| Delivered { stream, basis, path, seen: std::sync::Arc::new(plain) }))
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
            let frame = deliver_frame(&mut e, prior, Some(stream.into()), image.clone(), annotated, dir.path().join(format!("{id}.webp"))).unwrap();
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
        assert!(same["unchanged"] == true && same["path"] == first["path"], "cursor annotation alone is not a change");
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
