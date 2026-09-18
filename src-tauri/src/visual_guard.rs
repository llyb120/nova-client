//! Bounded pixel-only stale-target checks. This is not OCR or object recognition.
use xcap::image::RgbaImage;

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
        // Ignore small anti-aliasing/caret noise and unrelated animation elsewhere.
        // A changed centre or substantially changed neighbourhood requires a new observation.
        total == 0 || changed * 100 > total * 12 || core_changed * 100 > core * 35
    }
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
    fn stale_targets_not_background_animation_or_caret_noise() {
        let original = RgbaImage::from_pixel(240, 140, Rgba([80, 80, 80, 255]));
        let guard = VisualGuard::new(&original);
        let mut current = original.clone();
        for y in 0..40 { for x in 180..240 { current.put_pixel(x,y,Rgba([255,0,0,255])); } }
        assert!(!guard.changed_near(&current, 30., 60.));
        for y in 52..68 { current.put_pixel(30,y,Rgba([255,255,255,255])); }
        assert!(!guard.changed_near(&current, 30., 60.));
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
