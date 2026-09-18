//! Local pixel witnesses: fail closed when a screenshot's intended landing area changes.
//! This is not OCR, target recognition, or a guarantee of business success.
use xcap::image::{DynamicImage, RgbaImage, imageops::FilterType};

pub(super) fn bounded(image: &RgbaImage) -> RgbaImage {
    if image.width().max(image.height()) <= 2048 { return image.clone(); }
    DynamicImage::ImageRgba8(image.clone()).resize(2048,2048,FilterType::Triangle).into_rgba8()
}

pub(super) fn matches(a: &RgbaImage, b: &RgbaImage, x: u32, y: u32) -> bool {
    if a.dimensions()!=b.dimensions() || a.is_empty() || x>=a.width() || y>=a.height() { return false; }
    // Check both the centre (small targets) and context (moved rows/overlays).
    for radius in [4u32,24] {
        let mut changed=0; let mut total=0;
        for py in y.saturating_sub(radius)..=(y+radius).min(a.height()-1) {
            for px in x.saturating_sub(radius)..=(x+radius).min(a.width()-1) {
                total+=1;
                let (left,right)=(a.get_pixel(px,py),b.get_pixel(px,py));
                if (0..3).any(|c|left[c].abs_diff(right[c])>24) { changed+=1; }
            }
        }
        if changed > (total/50).max(1) { return false; }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use xcap::image::Rgba;
    #[test]
    fn checks_small_target_and_context_not_unrelated_animation() {
        let a=RgbaImage::from_pixel(400,300,Rgba([255,255,255,255]));
        assert!(matches(&a,&a,200,100));
        let mut b=a.clone();
        for y in 250..280 {for x in 300..350 {b.put_pixel(x,y,Rgba([0,0,0,255]));}}
        assert!(matches(&a,&b,200,100));
        for y in 100..103 {for x in 200..203 {b.put_pixel(x,y,Rgba([0,0,0,255]));}}
        assert!(!matches(&a,&b,200,100));
        assert!(!matches(&a,&RgbaImage::new(10,10),0,0));
        assert!(!matches(&a,&a,400,0));
        assert!(matches(&a,&a,0,0));
        let huge=RgbaImage::from_pixel(4096,2160,Rgba([1,2,3,255]));
        let small=bounded(&huge);
        assert_eq!(small.width(),2048);
        assert!(small.as_raw().len() <= 2048*2048*4);
    }
}
