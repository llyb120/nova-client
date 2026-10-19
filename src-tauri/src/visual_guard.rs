//! Bounded pixel-only stale-target checks. This is not OCR or object recognition.
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
