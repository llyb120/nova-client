//! Screenshot pixel coordinates are mapped by the tool, never by the language model.
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub(super) struct ImageMap {
    pub id: String,
    pub path: PathBuf,
    pub rect: Crop,
    pub pixels: (u32, u32),
}
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Crop {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}
impl Crop {
    pub fn validate(&self, width: f64, height: f64) -> Result<(), String> {
        if ![self.x, self.y, self.width, self.height, width, height]
            .iter()
            .all(|v| v.is_finite())
            || self.x < 0.
            || self.y < 0.
            || self.width < 1.
            || self.height < 1.
            || self.x + self.width > width
            || self.y + self.height > height
        {
            return Err("截图 region 超出 CSS 视口范围".into());
        }
        Ok(())
    }
}
impl ImageMap {
    pub fn point(&self, x: f64, y: f64) -> Result<(f64, f64), String> {
        if !x.is_finite()
            || !y.is_finite()
            || x < 0.
            || y < 0.
            || x >= self.pixels.0 as f64
            || y >= self.pixels.1 as f64
            || self.pixels.0 == 0
            || self.pixels.1 == 0
        {
            return Err("坐标超出图片；imageId 坐标使用该图片实际像素".into());
        }
        Ok((
            self.rect.x + x * self.rect.width / self.pixels.0 as f64,
            self.rect.y + y * self.rect.height / self.pixels.1 as f64,
        ))
    }
    pub fn metadata(&self) -> Value {
        json!({"imageId":self.id,"path":self.path,"x":self.rect.x,"y":self.rect.y,
            "width":self.rect.width,"height":self.rect.height,"pixelWidth":self.pixels.0,"pixelHeight":self.pixels.1,
            "inputCoordinateSpace":"image-pixels when action.imageId is provided"})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn screenshot_pixels_dpr_crop_and_tiles() {
        for dpr in [1., 1.25, 1.5, 2., 3.] {
            let m = ImageMap {
                id: "shot".into(),
                path: "unused".into(),
                rect: Crop {
                    x: 80.,
                    y: 90.,
                    width: 800.,
                    height: 400.,
                },
                pixels: ((800. * dpr) as u32, (400. * dpr) as u32),
            };
            assert_eq!(m.point(400. * dpr, 200. * dpr).unwrap(), (480., 290.));
            assert!(m.point(800. * dpr, 0.).is_err());
            for x in [f64::NAN, f64::INFINITY, -1.] {
                assert!(m.point(x, 0.).is_err());
            }
            let tile = ImageMap {
                rect: Crop {
                    x: 2048.,
                    y: 8192.,
                    width: 800.,
                    height: 400.,
                },
                ..m
            };
            assert_eq!(tile.point(400. * dpr, 200. * dpr).unwrap(), (2448., 8392.));
        }
        assert!(Crop {
            x: 0.,
            y: 0.,
            width: f64::INFINITY,
            height: 10.
        }
        .validate(100., 100.)
        .is_err());
        assert!(Crop {
            x: 99.,
            y: 0.,
            width: 2.,
            height: 10.
        }
        .validate(100., 100.)
        .is_err());
        assert!(Crop {
            x: 0.,
            y: 0.,
            width: 100.,
            height: 100.
        }
        .validate(100., 100.)
        .is_ok());
    }
}
