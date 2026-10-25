//! Shared screenshot delivery and bounded pixel-only stale-target checks. No OCR or object recognition.
use xcap::image::RgbaImage;

/// Half-open rectangle in original image pixels, never display/cropped-image coordinates.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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

    /// `core_only`: the batch's own earlier input legitimately changed the neighbourhood
    /// (typed text, ticked checkbox); only an overlay or shift right under the target counts.
    pub(crate) fn changed_near(&self, current: &RgbaImage, x: f64, y: f64, core_only: bool) -> bool {
        self.changed_near_excluding(current, x, y, core_only, None)
    }

    /// The caller must verify that the excluded overlay's geometry has not changed.
    pub(crate) fn changed_near_excluding(&self, current: &RgbaImage, x: f64, y: f64, core_only: bool, exclude: Option<PixelRect>) -> bool {
        if current.dimensions() != self.source || !x.is_finite() || !y.is_finite()
            || x < 0. || y < 0. || x >= self.source.0 as f64 || y >= self.source.1 as f64
            || exclude.is_some_and(|r| r.contains(x, y)) {
            return true;
        }
        let cx = (x / self.step as f64) as i64;
        let cy = (y / self.step as f64) as i64;
        let (mut total, mut changed, mut core, mut core_changed) = (0, 0, 0, 0);
        for dy in -24i64..=24 {
            for dx in -24i64..=24 {
                let (px, py) = (cx + dx, cy + dy);
                if px < 0 || py < 0 || px >= self.width as i64 || py >= self.height as i64 { continue; }
                let (sx, sy) = (px as u32 * self.step, py as u32 * self.step);
                if exclude.is_some_and(|r| r.contains(sx as f64, sy as f64)) { continue; }
                let old = self.pixels[(py * self.width as i64 + px) as usize];
                let new = current.get_pixel(sx, sy);
                let different = (0..3).any(|c| old[c].abs_diff(new[c]) > 24);
                total += 1;
                changed += u32::from(different);
                if dx.abs() <= 4 && dy.abs() <= 4 { core += 1; core_changed += u32::from(different); }
            }
        }
        // Ignore small anti-aliasing/caret noise and unrelated animation elsewhere.
        // A changed centre or substantially changed neighbourhood requires a new observation.
        total == 0 || core == 0 || (!core_only && changed * 100 > total * 12) || core_changed * 100 > core * 35
    }
}

/// 截图统一存 JPEG：模型历史里每张图都会被重发，PNG 体积大数倍。落点校验会拿这份文件对比新画面，
/// 质量须保证压缩噪声不触发 `patch_changed`。
pub(crate) fn encode_jpeg(image: &RgbaImage) -> Result<Vec<u8>, String> {
    use xcap::image::buffer::ConvertBuffer;
    let rgb: xcap::image::RgbImage = image.convert();
    let mut bytes = Vec::new();
    xcap::image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 85)
        .encode(rgb.as_raw(), rgb.width(), rgb.height(), xcap::image::ExtendedColorType::Rgb8)
        .map_err(|e| e.to_string())?;
    Ok(bytes)
}

/// What the model holds for one screen/tab stream, before any cursor annotation.
#[derive(Clone)]
pub(crate) struct Delivered {
    pub stream: String,
    pub group: String,
    pub keyframe_id: String,
    pub basis: serde_json::Value,
    pub keyframe: std::sync::Arc<RgbaImage>,
    pub keyframe_path: std::path::PathBuf,
    pub seen: std::sync::Arc<RgbaImage>,
    pub seen_path: std::path::PathBuf,
    pub delta: Option<PixelRect>,
    pub deltas: u8,
}

pub(crate) const MAX_DELTAS: u8 = 8;
const TILE: u32 = 32;
pub(crate) enum Delivery { Unchanged, Delta(PixelRect), Keyframe }

/// Compare to the last delivered pixels; deltas accumulate against the keyframe, including
/// reverted areas. Bound both crop area and group length to keep context and prompt-cache churn small.
pub(crate) fn plan_delivery(prior: Option<&Delivered>, pixels: &RgbaImage) -> Delivery {
    let Some(p) = prior.filter(|p| p.seen.dimensions() == pixels.dimensions()) else { return Delivery::Keyframe };
    if changed_tiles(&p.seen, pixels, None).is_some_and(|t| !t.contains(&true)) { return Delivery::Unchanged; }
    if p.deltas >= MAX_DELTAS { return Delivery::Keyframe; }
    let (w, h) = pixels.dimensions();
    let fresh = changed_tiles(&p.keyframe, pixels, None).and_then(|t| changed_box(&t, (w, h)));
    let b = match (fresh, p.delta) {
        (Some(a), Some(b)) => PixelRect { left: a.left.min(b.left), top: a.top.min(b.top),
            right: a.right.max(b.right), bottom: a.bottom.max(b.bottom) },
        (a, b) => match a.or(b) { Some(r) => r, None => return Delivery::Keyframe },
    };
    // ponytail: one bounding box; far-apart changes exceed the quarter-image cap and become a keyframe.
    if (b.right - b.left) as u64 * (b.bottom - b.top) as u64 * 4 <= w as u64 * h as u64 { Delivery::Delta(b) } else { Delivery::Keyframe }
}

fn changed_box(tiles: &[bool], (w, h): (u32, u32)) -> Option<PixelRect> {
    let cols = w.div_ceil(TILE) as usize;
    let mut changed = tiles.iter().enumerate().filter(|(_, c)| **c).map(|(i, _)| ((i % cols) as u32, (i / cols) as u32));
    let (x, y) = changed.next()?;
    let (l, t, r, b) = changed.fold((x, y, x, y), |(l, t, r, b), (x, y)| (l.min(x), t.min(y), r.max(x), b.max(y)));
    Some(PixelRect { left: l.saturating_sub(1) * TILE, top: t.saturating_sub(1) * TILE,
        right: ((r + 2) * TILE).min(w), bottom: ((b + 2) * TILE).min(h) })
}

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

/// Shared desktop/browser delivery. No `path` means no repeated attachment. The base entry describes
/// the complete image; callers may register delta IDs that map local crop coordinates automatically.
pub(crate) fn deliver_frame(entry: &mut serde_json::Value, prior: Option<&Delivered>, stream: Option<String>,
    plain: RgbaImage, annotated: Option<&RgbaImage>, path: std::path::PathBuf)
    -> Result<(Option<Delivered>, Option<serde_json::Value>), String> {
    use serde_json::json;
    let mark = |entry: &mut serde_json::Value, p: &Delivered| {
        entry["frameStream"] = json!(p.stream);
        entry["frameGroup"] = json!(p.group);
        entry["keyframeImageId"] = json!(p.keyframe_id);
        entry["keyframePath"] = json!(p.keyframe_path);
    };
    let delta_entry = |p: &Delivered, b: PixelRect, path: &std::path::Path| {
        let mut delta = json!({"deltaOf":entry["imageId"],"path":path,"x":b.left,"y":b.top,
            "width":b.right-b.left,"height":b.bottom-b.top,"pixelWidth":b.right-b.left,"pixelHeight":b.bottom-b.top});
        if let (Some(x), Some(y)) = (entry["cursor"]["x"].as_u64(), entry["cursor"]["y"].as_u64()) {
            if b.contains(x as f64, y as f64) {
                delta["cursor"] = entry["cursor"].clone();
                delta["cursor"]["x"] = json!(x - b.left as u64);
                delta["cursor"]["y"] = json!(y - b.top as u64);
            }
        }
        mark(&mut delta, p);
        delta
    };
    // Equal resized dimensions alone do not prove the same monitor geometry or source scale.
    let basis = json!([entry["desktopBounds"], entry["originalWidth"], entry["originalHeight"]]);
    let prior = prior.filter(|p| Some(&p.stream) == stream.as_ref() && p.basis == basis);
    let image = annotated.unwrap_or(&plain);
    match plan_delivery(prior, &plain) {
        Delivery::Unchanged => {
            let p = prior.unwrap();
            let latest = p.delta.map(|b| delta_entry(p, b, &p.seen_path));
            entry["unchanged"] = json!(true);
            entry["samePixelsAs"] = json!(p.seen_path);
            mark(entry, p);
            // Reattach this only if compaction also removed the most recent delta.
            if let Some(latest) = latest { entry["latestDelta"] = latest; }
            Ok((Some(p.clone()), None))
        }
        Delivery::Delta(b) => {
            let p = prior.unwrap();
            let crop = xcap::image::imageops::crop_imm(image, b.left, b.top, b.right-b.left, b.bottom-b.top).to_image();
            let path = path.with_file_name(format!("{}-delta.jpg", path.file_stem().unwrap_or_default().to_string_lossy()));
            std::fs::write(&path, encode_jpeg(&crop)?).map_err(|e| e.to_string())?;
            let delta = delta_entry(p, b, &path);
            mark(entry, p);
            Ok((Some(Delivered { seen: plain.into(), seen_path: path, delta: Some(b), deltas: p.deltas+1, ..p.clone() }), Some(delta)))
        }
        Delivery::Keyframe => {
            std::fs::write(&path, encode_jpeg(image)?).map_err(|e| e.to_string())?;
            entry["path"] = json!(path);
            let delivered = stream.map(|stream| {
                let plain = std::sync::Arc::new(plain);
                let p = Delivered { stream, group: entry["snapshotId"].as_str().unwrap_or_default().into(),
                    keyframe_id: entry["imageId"].as_str().unwrap_or_default().into(), basis, keyframe: plain.clone(),
                    keyframe_path: path.clone(), seen: plain, seen_path: path, delta: None, deltas: 0 };
                entry["keyframe"] = json!(true);
                mark(entry, &p);
                p
            });
            Ok((delivered, None))
        }
    }
}

pub(crate) fn patch_changed(before: &RgbaImage, after: &RgbaImage) -> bool {
    if before.width() == 0 || before.height() == 0 || after.width() == 0 || after.height() == 0 { return true; }
    let after = if before.dimensions() == after.dimensions() { after.clone() } else {
        xcap::image::imageops::resize(after, before.width(), before.height(), xcap::image::imageops::FilterType::Triangle)
    };
    VisualGuard::new(before).changed_near(&after, before.width() as f64 / 2., before.height() as f64 / 2., false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use xcap::image::Rgba;
    #[test]
    fn delivery_compares_plain_pixels_and_keeps_cursor_in_crop_coordinates() {
        use serde_json::json;
        let dir = tempfile::tempdir().unwrap();
        let page = RgbaImage::from_pixel(320, 320, Rgba([255, 255, 255, 255]));
        let entry = |id| json!({"imageId":"window-1","snapshotId":id,"cursor":{"x":150,"y":150,"marker":"sword-tip"}});
        let mut first = entry("key");
        let (prior, _) = deliver_frame(&mut first, None, Some("window".into()), page.clone(), None, dir.path().join("key.jpg")).unwrap();
        let mut annotated = page.clone();
        annotated.put_pixel(150, 150, Rgba([0, 100, 255, 255]));
        let mut same = entry("same");
        let (_, delta) = deliver_frame(&mut same, prior.as_ref(), Some("window".into()), page.clone(), Some(&annotated), dir.path().join("same.jpg")).unwrap();
        assert!(same["unchanged"] == true && delta.is_none(), "cursor annotation alone is not a change");
        let mut changed = page;
        changed.put_pixel(150, 150, Rgba([0, 0, 0, 255]));
        let mut next = entry("delta");
        let (seen, delta) = deliver_frame(&mut next, prior.as_ref(), Some("window".into()), changed.clone(), Some(&annotated), dir.path().join("delta.jpg")).unwrap();
        let delta = delta.unwrap();
        assert_eq!((delta["cursor"]["x"].as_u64(), delta["cursor"]["y"].as_u64()), (Some(54), Some(54)));
        assert_eq!(seen.unwrap().seen.as_ref(), &changed, "guard pixels never contain the cursor marker");
    }

    #[test]
    fn jpeg_screenshot_noise_is_not_a_visual_change() {
        // 密集细字 + 彩色控件：JPEG 最容易产生振铃的画面。
        let source = RgbaImage::from_fn(240, 140, |x, y| {
            if (y / 12) % 2 == 0 && (x % 5 == 0 || (x + y) % 7 == 0) { Rgba([20, 20, 20, 255]) }
            else if (100..180).contains(&x) && (90..120).contains(&y) { Rgba([30, 110, 230, 255]) }
            else { Rgba([250, 250, 250, 255]) }
        });
        let decoded = xcap::image::load_from_memory(&encode_jpeg(&source).unwrap()).unwrap().to_rgba8();
        assert_eq!(decoded.dimensions(), source.dimensions());
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
        assert!(!guard.changed_near(&current, 30., 60., false));
        for y in 52..68 { current.put_pixel(30,y,Rgba([255,255,255,255])); }
        assert!(!guard.changed_near(&current, 30., 60., false));
        // Typed text beside the target: stale for a fresh batch, fine after the batch's own input.
        let mut typed = original.clone();
        for y in 40..56 { for x in 10..60 { typed.put_pixel(x,y,Rgba([0,0,0,255])); } }
        assert!(guard.changed_near(&typed, 30., 60., false));
        assert!(!guard.changed_near(&typed, 30., 60., true));
        for y in 56..65 { for x in 26..35 { current.put_pixel(x,y,Rgba([0,255,0,255])); } }
        assert!(guard.changed_near(&current, 30., 60., false));
        assert!(guard.changed_near(&current, 30., 60., true));
        assert!(guard.changed_near(&current, f64::NAN, 60., false));
        assert!(guard.changed_near(&RgbaImage::new(240,141),30.,60.,false));
    }
    #[test]
    fn explicit_exclusion_keeps_nearby_targets_guarded() {
        let original = RgbaImage::from_pixel(100, 100, Rgba([20,20,20,255]));
        let guard = VisualGuard::new(&original);
        let rect = PixelRect { left:50, top:20, right:80, bottom:80 };
        let mut current = original.clone();
        for y in rect.top..rect.bottom { for x in rect.left..rect.right {
            current.put_pixel(x, y, Rgba([220,220,220,255]));
        } }
        assert!(guard.changed_near(&current, 48., 50., false));
        for core_only in [false, true] {
            assert!(!guard.changed_near_excluding(&current, 48., 50., core_only, Some(rect)));
            assert!(guard.changed_near_excluding(&current, 50., 50., core_only, Some(rect)));
        }
        for y in 46..55 { for x in 44..50 { current.put_pixel(x, y, Rgba([200,0,0,255])); } }
        assert!(guard.changed_near_excluding(&current, 48., 50., false, Some(rect)));
        assert!(guard.changed_near_excluding(&current, 48., 50., true, Some(rect)));
        // No exemptions for the browser caller or an image whose dimensions changed.
        assert!(patch_changed(&original, &current));
        assert!(guard.changed_near_excluding(&RgbaImage::new(101,100), 48., 50., false, Some(rect)));
    }
    #[test]
    fn high_dpi_and_image_boundaries_are_bounded() {
        let original=RgbaImage::from_pixel(3840,2160,Rgba([15,15,15,255]));
        let guard=VisualGuard::new(&original);
        assert!(guard.pixels.len() <= 1600 * 1600);
        assert!(!guard.changed_near(&original,3839.,2159.,false));
        assert!(guard.changed_near(&original,3840.,2159.,false));
        let a=RgbaImage::from_pixel(48,48,Rgba([15,15,15,255]));
        let b=RgbaImage::from_pixel(96,96,Rgba([15,15,15,255]));
        assert!(!patch_changed(&a,&b));
        assert!(patch_changed(&a,&RgbaImage::from_pixel(48,48,Rgba([255,255,255,255]))));
    }
}
