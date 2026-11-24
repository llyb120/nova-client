//! Fast JPEG screenshot pipeline & crisp Rust-side Set-of-Mark ([1], [2]...) renderer.
//! Badges are drawn ONLY on the screenshot copy sent to the model — the live webpage stays clean
//! with just the sleek animated virtual mouse cursor.
use image::{
    codecs::jpeg::JpegEncoder,
    imageops::FilterType,
    GenericImageView, ImageFormat, Rgba, RgbaImage,
};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub(crate) struct SomBox {
    pub id: u32,
    /// CSS viewport rectangle `[x, y, width, height]`.
    pub rect: [f64; 4],
}

#[derive(Clone, Debug)]
pub(crate) struct ShotMapping {
    pub path: PathBuf,
    pub pixel_width: u32,
    pub pixel_height: u32,
    pub css_x: f64,
    pub css_y: f64,
    pub css_width: f64,
    pub css_height: f64,
}

impl ShotMapping {
    /// Convert a model-supplied screenshot pixel coordinate `(px, py)` to CSS viewport `(x, y)`.
    pub(crate) fn image_to_css(&self, px: f64, py: f64) -> Result<(f64, f64), String> {
        if !px.is_finite() || !py.is_finite() || px < 0.0 || py < 0.0
            || px > self.pixel_width as f64 || py > self.pixel_height as f64
        {
            return Err(format!(
                "坐标 ({px:.0}, {py:.0}) 超出当前截图范围 {}x{}",
                self.pixel_width, self.pixel_height
            ));
        }
        let x = self.css_x + px * self.css_width / self.pixel_width.max(1) as f64;
        let y = self.css_y + py * self.css_height / self.pixel_height.max(1) as f64;
        Ok((x, y))
    }

    /// Convert a CSS viewport coordinate `(cx, cy)` to screenshot pixel coordinate `(px, py)`.
    pub(crate) fn css_to_image(&self, cx: f64, cy: f64) -> (i32, i32) {
        let px = ((cx - self.css_x) * self.pixel_width as f64 / self.css_width.max(1.0)).round() as i32;
        let py = ((cy - self.css_y) * self.pixel_height as f64 / self.css_height.max(1.0)).round() as i32;
        (px.clamp(0, self.pixel_width as i32), py.clamp(0, self.pixel_height as i32))
    }
}

/// Process a base64 JPEG from CDP `Page.captureScreenshot`:
/// optional crop -> vision-cap downscale (only if needed) -> optional crisp SoM badges -> fast JPEG q85.
pub(crate) fn process_screenshot(
    jpeg_base64: &str,
    css_viewport: (f64, f64),
    crop_css: Option<(f64, f64, f64, f64)>,
    boxes: &[SomBox],
    draw_som: bool,
    out_path: &Path,
) -> Result<ShotMapping, String> {
    let mut dummy = serde_json::Value::Null;
    let (mapping, _) = process_screenshot_with_guard(
        jpeg_base64,
        css_viewport,
        crop_css,
        boxes,
        draw_som,
        out_path,
        &mut dummy,
        None,
        None,
    )?;
    Ok(mapping)
}

/// Processes a browser screenshot while integrating `visual_guard` unchanged-frame deduplication:
/// - Compares unannotated `plain` pixels + SoM box signature against `prior`;
/// - When visibly unchanged on the same stream, reuses `prior.path` (`unchanged: true`) with zero SoM drawing or JPEG re-encode!
#[allow(clippy::too_many_arguments)]
pub(crate) fn process_screenshot_with_guard(
    jpeg_base64: &str,
    css_viewport: (f64, f64),
    crop_css: Option<(f64, f64, f64, f64)>,
    boxes: &[SomBox],
    draw_som: bool,
    out_path: &Path,
    entry: &mut serde_json::Value,
    prior: Option<&crate::visual_guard::Delivered>,
    stream: Option<String>,
) -> Result<(ShotMapping, Option<crate::visual_guard::Delivered>), String> {
    use base64::Engine;
    use serde_json::json;
    let raw = base64::engine::general_purpose::STANDARD
        .decode(jpeg_base64)
        .map_err(|e| format!("截图 base64 解码失败：{e}"))?;
    let vw = css_viewport.0.max(1.0);
    let vh = css_viewport.1.max(1.0);

    // Zero-reencode fast path when no stream guard is active, no crop, no SoM badges, and already within vision cap.
    if stream.is_none() && crop_css.is_none() && (!draw_som || boxes.is_empty()) {
        if let Ok((w, h)) = image::ImageReader::with_format(std::io::Cursor::new(&raw), ImageFormat::Jpeg).into_dimensions() {
            if w > 0 && h > 0 && crate::lyra::bridge::vision_scale(w as f64, h as f64) >= 1.0 {
                let path = out_path.with_extension("jpg");
                std::fs::write(&path, &raw).map_err(|e| e.to_string())?;
                if entry.is_object() {
                    entry["path"] = json!(path);
                }
                return Ok((
                    ShotMapping {
                        path,
                        pixel_width: w,
                        pixel_height: h,
                        css_x: 0.0,
                        css_y: 0.0,
                        css_width: vw,
                        css_height: vh,
                    },
                    None,
                ));
            }
        }
    }

    let decoded = image::load_from_memory(&raw).map_err(|e| format!("截图解码失败：{e}"))?;
    let (src_w, src_h) = decoded.dimensions();
    if src_w == 0 || src_h == 0 {
        return Err("截图尺寸为空".into());
    }

    let (css_x, css_y, css_w, css_h) = match crop_css {
        Some((cx, cy, cw, ch)) => {
            let x0 = cx.clamp(0.0, vw);
            let y0 = cy.clamp(0.0, vh);
            let x1 = (cx + cw).clamp(0.0, vw);
            let y1 = (cy + ch).clamp(0.0, vh);
            if x1 <= x0 + 1.0 || y1 <= y0 + 1.0 {
                return Err("局部放大区域超出当前视口".into());
            }
            (x0, y0, x1 - x0, y1 - y0)
        }
        None => (0.0, 0.0, vw, vh),
    };

    let cropped = if crop_css.is_some() {
        let sx = src_w as f64 / vw;
        let sy = src_h as f64 / vh;
        let l = (css_x * sx).round().clamp(0.0, (src_w - 1) as f64) as u32;
        let t = (css_y * sy).round().clamp(0.0, (src_h - 1) as f64) as u32;
        let r = ((css_x + css_w) * sx).round().clamp((l + 1) as f64, src_w as f64) as u32;
        let b = ((css_y + css_h) * sy).round().clamp((t + 1) as f64, src_h as f64) as u32;
        decoded.crop_imm(l, t, r - l, b - t)
    } else {
        decoded
    };

    let (cw, ch) = cropped.dimensions();
    let scale = crate::lyra::bridge::vision_scale(cw as f64, ch as f64);
    let plain: RgbaImage = if scale < 1.0 {
        let nw = ((cw as f64 * scale).round() as u32).max(1);
        let nh = ((ch as f64 * scale).round() as u32).max(1);
        cropped.resize_exact(nw, nh, FilterType::Triangle).into_rgba8()
    } else {
        cropped.to_rgba8()
    };

    let (out_w, out_h) = plain.dimensions();

    // Stream-backed unchanged-frame check BEFORE drawing SoM badges or encoding JPEG!
    if let Some(stream_name) = stream {
        let som_sig: u64 = if draw_som {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            boxes.len().hash(&mut hasher);
            for b in boxes {
                b.id.hash(&mut hasher);
                (b.rect[0].round() as i32).hash(&mut hasher);
                (b.rect[1].round() as i32).hash(&mut hasher);
            }
            hasher.finish()
        } else {
            0
        };
        let basis = json!([css_w.round() as i64, css_h.round() as i64, src_w, src_h, som_sig]);
        entry["frameStream"] = json!(stream_name);
        entry["pixelWidth"] = json!(out_w);
        entry["pixelHeight"] = json!(out_h);
        entry["x"] = json!(css_x);
        entry["y"] = json!(css_y);
        entry["width"] = json!(css_w);
        entry["height"] = json!(css_h);

        let same = prior.filter(|p| {
            p.stream == stream_name
                && p.basis == basis
                && crate::visual_guard::changed_tiles(&p.seen, &plain, None).is_some_and(|c| !c.contains(&true))
        });
        if let Some(p) = same {
            entry["unchanged"] = json!(true);
            entry["path"] = json!(p.path);
            return Ok((
                ShotMapping {
                    path: p.path.clone(),
                    pixel_width: out_w,
                    pixel_height: out_h,
                    css_x,
                    css_y,
                    css_width: css_w,
                    css_height: css_h,
                },
                Some(p.clone()),
            ));
        }

        let mut annotated = plain.clone();
        if draw_som && !boxes.is_empty() {
            draw_som_overlay(&mut annotated, boxes, (css_x, css_y, css_w, css_h));
        }
        let path = out_path.with_extension("jpg");
        encode_jpeg_q85(&annotated, &path)?;
        entry["path"] = json!(path);
        let delivered = crate::visual_guard::Delivered {
            stream: stream_name,
            basis,
            path: path.clone(),
            seen: std::sync::Arc::new(plain),
        };
        return Ok((
            ShotMapping {
                path,
                pixel_width: out_w,
                pixel_height: out_h,
                css_x,
                css_y,
                css_width: css_w,
                css_height: css_h,
            },
            Some(delivered),
        ));
    }

    let mut rgba = plain;
    if draw_som && !boxes.is_empty() {
        draw_som_overlay(&mut rgba, boxes, (css_x, css_y, css_w, css_h));
    }
    let path = out_path.with_extension("jpg");
    encode_jpeg_q85(&rgba, &path)?;
    if entry.is_object() {
        entry["path"] = json!(path);
        entry["pixelWidth"] = json!(out_w);
        entry["pixelHeight"] = json!(out_h);
        entry["x"] = json!(css_x);
        entry["y"] = json!(css_y);
        entry["width"] = json!(css_w);
        entry["height"] = json!(css_h);
    }

    Ok((
        ShotMapping {
            path,
            pixel_width: out_w,
            pixel_height: out_h,
            css_x,
            css_y,
            css_width: css_w,
            css_height: css_h,
        },
        None,
    ))
}

fn encode_jpeg_q85(rgba: &RgbaImage, path: &Path) -> Result<(), String> {
    let (out_w, out_h) = rgba.dimensions();
    let rgb = image::DynamicImage::ImageRgba8(rgba.clone()).to_rgb8();
    let mut encoded = Vec::with_capacity((out_w as usize * out_h as usize) / 7);
    let mut encoder = JpegEncoder::new_with_quality(&mut encoded, 85);
    encoder
        .encode(rgb.as_raw(), out_w, out_h, image::ExtendedColorType::Rgb8)
        .map_err(|e| format!("JPEG 编码失败：{e}"))?;
    std::fs::write(path, &encoded).map_err(|e| e.to_string())
}

/// Crate-agnostic entrypoint for drawing `[1][2]` SoM badges directly onto any tightly-packed RGBA8 buffer.
pub(crate) fn draw_som_raw(rgba: &mut [u8], width: u32, height: u32, boxes: &[SomBox], css_view: (f64, f64, f64, f64)) {
    if boxes.is_empty() || width == 0 || height == 0 {
        return;
    }
    if let Some(mut img) = RgbaImage::from_raw(width, height, rgba.to_vec()) {
        draw_som_overlay(&mut img, boxes, css_view);
        rgba.copy_from_slice(img.as_raw());
    }
}

/// Draw crisp subtle element outlines + collision-avoiding `[id]` pill badges onto `img`.
pub(crate) fn draw_som_overlay(img: &mut RgbaImage, boxes: &[SomBox], css_view: (f64, f64, f64, f64)) {
    let (w, h) = (img.width() as i32, img.height() as i32);
    let (vx, vy, vw, vh) = css_view;
    let sx = w as f64 / vw.max(1.0);
    let sy = h as f64 / vh.max(1.0);

    // Subtle Indigo bounding stroke + high-contrast dark-slate pill with white border & crisp digits.
    let outline = Rgba([79, 70, 229, 96]);
    let fill = Rgba([30, 41, 59, 232]);
    let border = Rgba([255, 255, 255, 245]);
    let text = Rgba([255, 255, 255, 255]);

    let mut placed: Vec<(i32, i32, i32, i32)> = Vec::with_capacity(boxes.len());

    // First pass: draw subtle 1px outlines so badges always sit crisply on top.
    let mut mapped = Vec::with_capacity(boxes.len());
    for b in boxes {
        let x0 = ((b.rect[0] - vx) * sx).round() as i32;
        let y0 = ((b.rect[1] - vy) * sy).round() as i32;
        let x1 = ((b.rect[0] + b.rect[2] - vx) * sx).round() as i32;
        let y1 = ((b.rect[1] + b.rect[3] - vy) * sy).round() as i32;
        let cx0 = x0.clamp(0, w - 1);
        let cy0 = y0.clamp(0, h - 1);
        let cx1 = x1.clamp(0, w - 1);
        let cy1 = y1.clamp(0, h - 1);
        if cx1 - cx0 < 4 || cy1 - cy0 < 4 {
            continue;
        }
        draw_rect_outline(img, cx0, cy0, cx1, cy1, outline);
        mapped.push((b.id, cx0, cy0, cx1, cy1));
    }

    // Second pass: place non-overlapping numbered pills while avoiding tiny sibling control centers.
    let tiny_centers: Vec<(u32, i32, i32)> = mapped
        .iter()
        .filter(|&&(_, x0, y0, x1, y1)| (x1 - x0) <= 56 && (y1 - y0) <= 28)
        .map(|&(id, x0, y0, x1, y1)| (id, (x0 + x1) / 2, (y0 + y1) / 2))
        .collect();

    for &(id, x0, y0, x1, y1) in &mapped {
        let label = id.to_string();
        let digits = label.len() as i32;
        let pw = digits * (GLYPH_W + 1) - 1 + 6; // 3px horizontal padding each side
        let ph = GLYPH_H + 4;                     // 2px vertical padding each side

        let covers_tiny = |bx: i32, by: i32| {
            tiny_centers
                .iter()
                .any(|&(tid, tx, ty)| tid != id && tx >= bx && tx <= bx + pw && ty >= by && ty <= by + ph)
        };

        // Prefer outside edges first (zero occlusion of element label), then inside corners.
        let candidates = [
            (x0 - 1, y0 - ph + 1),
            (x1 - pw + 1, y0 - ph + 1),
            (x0 - pw, y0),
            (x0 - 1, y1),
            (x0, y0),
            (x1 - pw, y0),
            (x1, y0),
            (x1 - pw + 1, y1),
        ];
        let mut found: Option<(i32, i32)> = None;
        for (cx, cy) in candidates {
            let bx = cx.clamp(0, (w - pw).max(0));
            let by = cy.clamp(0, (h - ph).max(0));
            if !overlaps(&placed, bx, by, pw, ph) && !covers_tiny(bx, by) {
                found = Some((bx, by));
                break;
            }
        }
        // Fallback: allow covering a neighbor edge as long as pills themselves don't collide.
        if found.is_none() {
            for (cx, cy) in candidates {
                let bx = cx.clamp(0, (w - pw).max(0));
                let by = cy.clamp(0, (h - ph).max(0));
                if !overlaps(&placed, bx, by, pw, ph) {
                    found = Some((bx, by));
                    break;
                }
            }
        }
        // Nudge search in dense table rows.
        if found.is_none() {
            let base = (x0.clamp(0, (w - pw).max(0)), (y0 - ph + 1).clamp(0, (h - ph).max(0)));
            for (dx, dy) in [(pw + 1, 0), (0, ph + 1), (-(pw + 1), 0), (0, -(ph + 1))] {
                let bx = (base.0 + dx).clamp(0, (w - pw).max(0));
                let by = (base.1 + dy).clamp(0, (h - ph).max(0));
                if !overlaps(&placed, bx, by, pw, ph) {
                    found = Some((bx, by));
                    break;
                }
            }
        }
        // In ultra-dense grids where all positions still collide with an already-drawn pill,
        // keep only the 1px Indigo outline + `[id] @(x,y)` text entry rather than painting illegible overlapping pills!
        if let Some((bx, by)) = found {
            placed.push((bx, by, bx + pw, by + ph));
            draw_pill_badge(img, bx, by, pw, ph, &label, fill, border, text);
        }
    }
}

fn overlaps(placed: &[(i32, i32, i32, i32)], x: i32, y: i32, w: i32, h: i32) -> bool {
    let (x1, y1) = (x + w, y + h);
    placed.iter().any(|&(ax0, ay0, ax1, ay1)| x < ax1 && x1 > ax0 && y < ay1 && y1 > ay0)
}

#[inline]
fn blend_px(img: &mut RgbaImage, x: i32, y: i32, src: Rgba<u8>) {
    if x < 0 || y < 0 || x >= img.width() as i32 || y >= img.height() as i32 {
        return;
    }
    let a = src[3] as u32;
    if a == 0 {
        return;
    }
    let dst = img.get_pixel_mut(x as u32, y as u32);
    if a == 255 {
        *dst = src;
        return;
    }
    let inv = 255 - a;
    dst[0] = ((src[0] as u32 * a + dst[0] as u32 * inv) / 255) as u8;
    dst[1] = ((src[1] as u32 * a + dst[1] as u32 * inv) / 255) as u8;
    dst[2] = ((src[2] as u32 * a + dst[2] as u32 * inv) / 255) as u8;
    dst[3] = 255;
}

fn draw_rect_outline(img: &mut RgbaImage, x0: i32, y0: i32, x1: i32, y1: i32, color: Rgba<u8>) {
    for x in x0..=x1 {
        blend_px(img, x, y0, color);
        blend_px(img, x, y1, color);
    }
    for y in (y0 + 1)..y1 {
        blend_px(img, x0, y, color);
        blend_px(img, x1, y, color);
    }
}

fn draw_pill_badge(
    img: &mut RgbaImage,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    label: &str,
    fill: Rgba<u8>,
    border: Rgba<u8>,
    text: Rgba<u8>,
) {
    // Rounded 1px corners
    for dy in 0..h {
        for dx in 0..w {
            let corner = (dx == 0 || dx == w - 1) && (dy == 0 || dy == h - 1);
            if corner {
                continue;
            }
            let is_border = dx == 0 || dx == w - 1 || dy == 0 || dy == h - 1
                || ((dx == 1 || dx == w - 2) && (dy == 1 || dy == h - 2));
            blend_px(img, x + dx, y + dy, if is_border { border } else { fill });
        }
    }
    let mut cursor_x = x + 3;
    let cursor_y = y + 2;
    for ch in label.bytes() {
        if let Some(glyph) = digit_glyph(ch) {
            for (row, &bits) in glyph.iter().enumerate() {
                for col in 0..GLYPH_W {
                    if (bits >> (GLYPH_W - 1 - col)) & 1 != 0 {
                        blend_px(img, cursor_x + col, cursor_y + row as i32, text);
                    }
                }
            }
            cursor_x += GLYPH_W + 1;
        }
    }
}

/// Crisp 6x9 bold bitmap font for digits '0'..'9' (readable even after JPEG q85 compression).
const GLYPH_W: i32 = 6;
const GLYPH_H: i32 = 9;

fn digit_glyph(b: u8) -> Option<&'static [u8; 9]> {
    const DIGITS: [[u8; 9]; 10] = [
        // 0
        [0b011110, 0b110011, 0b110011, 0b110011, 0b110011, 0b110011, 0b110011, 0b110011, 0b011110],
        // 1
        [0b001100, 0b011100, 0b111100, 0b001100, 0b001100, 0b001100, 0b001100, 0b001100, 0b111111],
        // 2
        [0b011110, 0b110011, 0b000011, 0b000110, 0b001100, 0b011000, 0b110000, 0b110011, 0b111111],
        // 3
        [0b011110, 0b110011, 0b000011, 0b001110, 0b000011, 0b000011, 0b000011, 0b110011, 0b011110],
        // 4
        [0b000110, 0b001110, 0b011110, 0b110110, 0b110110, 0b111111, 0b000110, 0b000110, 0b000110],
        // 5
        [0b111111, 0b110000, 0b110000, 0b111110, 0b000011, 0b000011, 0b000011, 0b110011, 0b011110],
        // 6
        [0b011110, 0b110011, 0b110000, 0b111110, 0b110011, 0b110011, 0b110011, 0b110011, 0b011110],
        // 7
        [0b111111, 0b110011, 0b000011, 0b000110, 0b001100, 0b001100, 0b011000, 0b011000, 0b011000],
        // 8
        [0b011110, 0b110011, 0b110011, 0b011110, 0b110011, 0b110011, 0b110011, 0b110011, 0b011110],
        // 9
        [0b011110, 0b110011, 0b110011, 0b110011, 0b011111, 0b000011, 0b000011, 0b110011, 0b011110],
    ];
    if (b'0'..=b'9').contains(&b) {
        Some(&DIGITS[(b - b'0') as usize])
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    #[test]
    fn annotates_som_badges_and_preserves_exact_coordinate_mapping() {
        let dir = tempfile::tempdir().unwrap();
        let canvas = RgbaImage::from_pixel(1280, 800, Rgba([248, 250, 252, 255]));
        let rgb = image::DynamicImage::ImageRgba8(canvas).to_rgb8();
        let mut raw = Vec::new();
        JpegEncoder::new_with_quality(&mut raw, 80)
            .encode(rgb.as_raw(), 1280, 800, image::ExtendedColorType::Rgb8)
            .unwrap();
        let b64 = base64::engine::general_purpose::STANDARD.encode(&raw);
        let boxes = vec![
            SomBox { id: 1, rect: [100.0, 50.0, 200.0, 36.0] },
            SomBox { id: 24, rect: [102.0, 52.0, 120.0, 30.0] },
        ];
        let out = dir.path().join("shot");
        let map = process_screenshot(&b64, (1280.0, 800.0), None, &boxes, true, &out).unwrap();
        assert_eq!((map.pixel_width, map.pixel_height), (1280, 800));
        assert_eq!(map.image_to_css(640.0, 400.0).unwrap(), (640.0, 400.0));
        assert_eq!(map.css_to_image(100.0, 50.0), (100, 50));
        assert!(map.path.exists());
        // Verify crop mapping scales accurately
        let crop = process_screenshot(&b64, (1280.0, 800.0), Some((200.0, 100.0, 400.0, 200.0)), &boxes, false, &dir.path().join("crop")).unwrap();
        assert_eq!((crop.pixel_width, crop.pixel_height), (400, 200));
        assert_eq!(crop.image_to_css(200.0, 100.0).unwrap(), (400.0, 200.0));
    }

    #[test]
    fn unchanged_browser_frame_reuses_delivered_jpeg_until_pixels_or_som_change() {
        let dir = tempfile::tempdir().unwrap();
        let canvas = RgbaImage::from_pixel(640, 400, Rgba([248, 250, 252, 255]));
        let encode = |img: &RgbaImage| {
            let rgb = image::DynamicImage::ImageRgba8(img.clone()).to_rgb8();
            let mut raw = Vec::new();
            JpegEncoder::new_with_quality(&mut raw, 85)
                .encode(rgb.as_raw(), img.width(), img.height(), image::ExtendedColorType::Rgb8)
                .unwrap();
            base64::engine::general_purpose::STANDARD.encode(&raw)
        };
        let b64 = encode(&canvas);
        let boxes = vec![SomBox { id: 1, rect: [80.0, 40.0, 120.0, 32.0] }];
        let mut e1 = serde_json::json!({"imageId": "s1-0"});
        let (_, d1) = process_screenshot_with_guard(
            &b64,
            (640.0, 400.0),
            None,
            &boxes,
            true,
            &dir.path().join("s1"),
            &mut e1,
            None,
            Some("C1:viewport".into()),
        )
        .unwrap();
        assert!(e1["unchanged"].is_null());

        let mut e2 = serde_json::json!({"imageId": "s2-0"});
        let (_, d2) = process_screenshot_with_guard(
            &b64,
            (640.0, 400.0),
            None,
            &boxes,
            true,
            &dir.path().join("s2"),
            &mut e2,
            d1.as_ref(),
            Some("C1:viewport".into()),
        )
        .unwrap();
        assert_eq!(e2["unchanged"], true);
        assert_eq!(e2["path"], e1["path"]);

        // New DOM control box resets unchanged reuse and emits a newly annotated JPEG.
        let mut e3 = serde_json::json!({"imageId": "s3-0"});
        let boxes_changed = vec![
            SomBox { id: 1, rect: [80.0, 40.0, 120.0, 32.0] },
            SomBox { id: 2, rect: [240.0, 40.0, 120.0, 32.0] },
        ];
        process_screenshot_with_guard(
            &b64,
            (640.0, 400.0),
            None,
            &boxes_changed,
            true,
            &dir.path().join("s3"),
            &mut e3,
            d2.as_ref(),
            Some("C1:viewport".into()),
        )
        .unwrap();
        assert!(e3["unchanged"].is_null());
        assert_ne!(e3["path"], e1["path"]);
    }
}
