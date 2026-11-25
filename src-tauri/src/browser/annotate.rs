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

/// Compute a fast deterministic signature of the mapped SoM boxes so `unchanged` frames
/// are only reused when both the page pixels AND the `[1][2]` badge layout match.
pub(crate) fn som_signature(
    boxes: &[SomBox],
    out_w: u32,
    out_h: u32,
    css_view: (f64, f64, f64, f64),
) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    out_w.hash(&mut hasher);
    out_h.hash(&mut hasher);
    let (vx, vy, vw, vh) = css_view;
    let sx = out_w as f64 / vw.max(1.0);
    let sy = out_h as f64 / vh.max(1.0);
    let w = out_w as i32;
    let h = out_h as i32;
    for b in boxes {
        let cx0 = (((b.rect[0] - vx) * sx).round() as i32).clamp(0, (w - 1).max(0));
        let cy0 = (((b.rect[1] - vy) * sy).round() as i32).clamp(0, (h - 1).max(0));
        let cx1 = (((b.rect[0] + b.rect[2] - vx) * sx).round() as i32).clamp(0, (w - 1).max(0));
        let cy1 = (((b.rect[1] + b.rect[3] - vy) * sy).round() as i32).clamp(0, (h - 1).max(0));
        if cx1 - cx0 >= 4 && cy1 - cy0 >= 4 {
            (b.id, cx0, cy0, cx1, cy1).hash(&mut hasher);
        }
    }
    hasher.finish()
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
        (0.0, 0.0, css_viewport.0, css_viewport.1),
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
/// - When visibly unchanged on the same stream, reuses `prior.path` (`unchanged: true`) with zero SoM drawing or JPEG re-encode.
#[allow(clippy::too_many_arguments)]
pub(crate) fn process_screenshot_with_guard(
    jpeg_base64: &str,
    base_css: (f64, f64, f64, f64),
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
    let (bx, by, vw, vh) = (base_css.0, base_css.1, base_css.2.max(1.0), base_css.3.max(1.0));

    // Zero-reencode fast path when no stream guard is active, no crop, no SoM badges, and already within vision cap.
    if stream.is_none() && crop_css.is_none() && (!draw_som || boxes.is_empty()) {
        if let Ok((w, h)) = image::ImageReader::with_format(std::io::Cursor::new(&raw), ImageFormat::Jpeg).into_dimensions() {
            if w > 0 && h > 0 && crate::lyra::bridge::vision_scale(w as f64, h as f64) >= 1.0 {
                let path = out_path.with_extension("jpg");
                std::fs::write(&path, &raw).map_err(|e| e.to_string())?;
                if entry.is_object() {
                    entry["path"] = json!(path);
                    entry["pixelWidth"] = json!(w);
                    entry["pixelHeight"] = json!(h);
                    entry["x"] = json!(bx);
                    entry["y"] = json!(by);
                    entry["width"] = json!(vw);
                    entry["height"] = json!(vh);
                }
                return Ok((
                    ShotMapping {
                        path,
                        pixel_width: w,
                        pixel_height: h,
                        css_x: bx,
                        css_y: by,
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

    let (cropped, css_x, css_y, css_w, css_h) = match crop_css {
        Some((cx, cy, cw, ch)) => {
            let x0 = cx.clamp(bx, bx + vw);
            let y0 = cy.clamp(by, by + vh);
            let x1 = (cx + cw).clamp(bx, bx + vw);
            let y1 = (cy + ch).clamp(by, by + vh);
            if x1 <= x0 + 1.0 || y1 <= y0 + 1.0 {
                return Err("局部放大区域超出当前视口".into());
            }
            let sx = src_w as f64 / vw;
            let sy = src_h as f64 / vh;
            let l = ((x0 - bx) * sx).round().clamp(0.0, (src_w - 1) as f64) as u32;
            let t = ((y0 - by) * sy).round().clamp(0.0, (src_h - 1) as f64) as u32;
            let r = ((x1 - bx) * sx).round().clamp((l + 1) as f64, src_w as f64) as u32;
            let b = ((y1 - by) * sy).round().clamp((t + 1) as f64, src_h as f64) as u32;
            (
                decoded.crop_imm(l, t, r - l, b - t),
                bx + l as f64 / sx,
                by + t as f64 / sy,
                (r - l) as f64 / sx,
                (b - t) as f64 / sy,
            )
        }
        None => (decoded, bx, by, vw, vh),
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
    let som_sig = if draw_som && !boxes.is_empty() {
        som_signature(boxes, out_w, out_h, (css_x, css_y, css_w, css_h))
    } else {
        0
    };

    if let Some(ref stream_name) = stream {
        let basis = json!([css_x, css_y, css_w, css_h, out_w, out_h, som_sig]);
        let same = prior.filter(|p| {
            p.stream == *stream_name
                && p.basis == basis
                && crate::visual_guard::changed_tiles(&p.seen, &plain, None)
                    .is_some_and(|c| !c.contains(&true))
        });
        if let Some(p) = same {
            if entry.is_object() {
                entry["unchanged"] = json!(true);
                entry["path"] = json!(p.path);
                entry["frameStream"] = json!(stream_name);
                entry["pixelWidth"] = json!(out_w);
                entry["pixelHeight"] = json!(out_h);
                entry["x"] = json!(css_x);
                entry["y"] = json!(css_y);
                entry["width"] = json!(css_w);
                entry["height"] = json!(css_h);
            }
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
    }

    let seen_copy = stream.as_ref().map(|_| plain.clone());
    let mut annotated = plain;
    if draw_som && !boxes.is_empty() {
        draw_som_overlay(&mut annotated, boxes, (css_x, css_y, css_w, css_h));
    }

    let path = out_path.with_extension("jpg");
    let mut encoded = Vec::with_capacity((out_w as usize * out_h as usize) / 8);
    let mut encoder = JpegEncoder::new_with_quality(&mut encoded, 85);
    encoder
        .encode_image(&annotated)
        .map_err(|e| format!("JPEG 编码失败：{e}"))?;
    std::fs::write(&path, &encoded).map_err(|e| e.to_string())?;

    if entry.is_object() {
        entry["path"] = json!(path);
        entry["pixelWidth"] = json!(out_w);
        entry["pixelHeight"] = json!(out_h);
        entry["x"] = json!(css_x);
        entry["y"] = json!(css_y);
        entry["width"] = json!(css_w);
        entry["height"] = json!(css_h);
        if let Some(ref stream_name) = stream {
            entry["frameStream"] = json!(stream_name);
        }
    }

    let delivered = match (stream, seen_copy) {
        (Some(stream_name), Some(seen)) => Some(crate::visual_guard::Delivered {
            stream: stream_name,
            basis: json!([css_x, css_y, css_w, css_h, out_w, out_h, som_sig]),
            path: path.clone(),
            seen: std::sync::Arc::new(seen),
        }),
        _ => None,
    };

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
        delivered,
    ))
}

/// Crate-agnostic entrypoint for drawing `[1][2]` SoM badges directly onto any tightly-packed RGBA8 buffer in-place.
pub(crate) fn draw_som_raw(rgba: &mut [u8], width: u32, height: u32, boxes: &[SomBox], css_view: (f64, f64, f64, f64)) {
    let (w, h) = (width as i32, height as i32);
    if boxes.is_empty() || w <= 0 || h <= 0 || rgba.len() < (width as usize * height as usize * 4) {
        return;
    }
    let (vx, vy, vw, vh) = css_view;
    let sx = w as f64 / vw.max(1.0);
    let sy = h as f64 / vh.max(1.0);

    // 4-color high-contrast palette (pill fill, subtle outline) so dense adjacent badges are distinct.
    const PALETTE: [(Rgba<u8>, Rgba<u8>); 4] = [
        (Rgba([49, 46, 129, 236]), Rgba([79, 70, 229, 108])),   // deep indigo
        (Rgba([6, 78, 59, 236]), Rgba([16, 185, 129, 108])),   // deep emerald
        (Rgba([136, 19, 55, 236]), Rgba([225, 29, 72, 108])),  // deep rose
        (Rgba([12, 74, 110, 236]), Rgba([14, 165, 233, 108])), // deep slate-cyan
    ];
    let border = Rgba([255, 255, 255, 245]);
    let text = Rgba([255, 255, 255, 255]);

    let mut placed: Vec<(i32, i32, i32, i32)> = Vec::with_capacity(boxes.len());
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
        let (fill, outline) = PALETTE[(b.id as usize) % PALETTE.len()];
        draw_rect_outline_raw(rgba, w, h, cx0, cy0, cx1, cy1, outline);
        mapped.push((b.id, cx0, cy0, cx1, cy1, fill));
    }

    let small: Vec<(u32, i32, i32)> = mapped
        .iter()
        .filter(|(_, l, t, r, b, _)| r - l <= 56 && b - t <= 28)
        .map(|&(id, l, t, r, b, _)| (id, (l + r) / 2, (t + b) / 2))
        .collect();

    for (id, x0, y0, x1, y1, fill) in mapped {
        let label = id.to_string();
        let digits = label.len() as i32;
        let pw = digits * (GLYPH_W + 1) - 1 + 6;
        let ph = GLYPH_H + 4;
        if pw > w || ph > h {
            continue;
        }

        let candidates = [
            (x0 - 1, y0 - ph + 1),
            (x0, y0),
            (x1 - pw + 1, y0 - ph + 1),
            (x0 - 1, y1),
            (x1 - pw, y0),
            (x0 - pw, y0),
            (x1, y0),
            (x1 - pw + 1, y1),
        ];
        if let Some((bx, by)) = candidates
            .into_iter()
            .map(|(cx, cy)| (cx.clamp(0, w - pw), cy.clamp(0, h - ph)))
            .filter(|&(bx, by)| !overlaps(&placed, bx, by, pw, ph))
            .min_by_key(|&(bx, by)| {
                small
                    .iter()
                    .any(|&(other, cx, cy)| other != id && (bx..bx + pw).contains(&cx) && (by..by + ph).contains(&cy))
            })
        {
            placed.push((bx, by, bx + pw, by + ph));
            draw_pill_badge_raw(rgba, w, h, bx, by, pw, ph, &label, fill, border, text);
        }
    }
}

/// Draw crisp subtle element outlines + collision-avoiding `[id]` pill badges onto `img`.
pub(crate) fn draw_som_overlay(img: &mut RgbaImage, boxes: &[SomBox], css_view: (f64, f64, f64, f64)) {
    let (w, h) = img.dimensions();
    draw_som_raw(img.as_mut(), w, h, boxes, css_view);
}

fn overlaps(placed: &[(i32, i32, i32, i32)], x: i32, y: i32, w: i32, h: i32) -> bool {
    let (x1, y1) = (x + w, y + h);
    placed.iter().any(|&(ax0, ay0, ax1, ay1)| x < ax1 && x1 > ax0 && y < ay1 && y1 > ay0)
}

#[inline]
fn blend_px_raw(rgba: &mut [u8], w: i32, h: i32, x: i32, y: i32, src: Rgba<u8>) {
    if x < 0 || y < 0 || x >= w || y >= h {
        return;
    }
    let a = src[3] as u32;
    if a == 0 {
        return;
    }
    let idx = ((y * w + x) * 4) as usize;
    if a == 255 {
        rgba[idx..idx + 4].copy_from_slice(&src.0);
        return;
    }
    let inv = 255 - a;
    rgba[idx] = ((src[0] as u32 * a + rgba[idx] as u32 * inv) / 255) as u8;
    rgba[idx + 1] = ((src[1] as u32 * a + rgba[idx + 1] as u32 * inv) / 255) as u8;
    rgba[idx + 2] = ((src[2] as u32 * a + rgba[idx + 2] as u32 * inv) / 255) as u8;
    rgba[idx + 3] = 255;
}

fn draw_rect_outline_raw(rgba: &mut [u8], w: i32, h: i32, x0: i32, y0: i32, x1: i32, y1: i32, color: Rgba<u8>) {
    for x in x0..=x1 {
        blend_px_raw(rgba, w, h, x, y0, color);
        blend_px_raw(rgba, w, h, x, y1, color);
    }
    for y in (y0 + 1)..y1 {
        blend_px_raw(rgba, w, h, x0, y, color);
        blend_px_raw(rgba, w, h, x1, y, color);
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_pill_badge_raw(
    rgba: &mut [u8],
    img_w: i32,
    img_h: i32,
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
            blend_px_raw(rgba, img_w, img_h, x + dx, y + dy, if is_border { border } else { fill });
        }
    }
    let mut cursor_x = x + 3;
    let cursor_y = y + 2;
    for ch in label.bytes() {
        if let Some(glyph) = digit_glyph(ch) {
            for (row, &bits) in glyph.iter().enumerate() {
                for col in 0..GLYPH_W {
                    if (bits >> (GLYPH_W - 1 - col)) & 1 != 0 {
                        blend_px_raw(rgba, img_w, img_h, cursor_x + col, cursor_y + row as i32, text);
                    }
                }
            }
            cursor_x += GLYPH_W + 1;
        }
    }
}

/// Crisp 6x9 bold bitmap font for digits '0'..'9' (readable even after JPEG q82 compression).
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
    fn dense_badges_do_not_overpaint_the_only_available_slot() {
        let paint = |ids: &[u32]| {
            let mut img = RgbaImage::new(12, 13);
            let boxes: Vec<_> = ids.iter().map(|&id| SomBox { id, rect: [0., 0., 11., 12.] }).collect();
            draw_som_overlay(&mut img, &boxes, (0., 0., 12., 13.));
            img
        };
        let (one, two) = (paint(&[1]), paint(&[1, 2]));
        for y in 1..12 {
            for x in 1..11 {
                assert_eq!(one.get_pixel(x, y), two.get_pixel(x, y));
            }
        }
    }

    #[test]
    fn guard_reuses_unchanged_frame_and_invalidates_on_som_change() {
        let dir = tempfile::tempdir().unwrap();
        let canvas = RgbaImage::from_pixel(320, 200, Rgba([248, 250, 252, 255]));
        let rgb = image::DynamicImage::ImageRgba8(canvas).to_rgb8();
        let mut raw = Vec::new();
        JpegEncoder::new_with_quality(&mut raw, 80)
            .encode(rgb.as_raw(), 320, 200, image::ExtendedColorType::Rgb8)
            .unwrap();
        let b64 = base64::engine::general_purpose::STANDARD.encode(&raw);
        let boxes1 = vec![SomBox { id: 1, rect: [20.0, 20.0, 80.0, 30.0] }];
        let boxes2 = vec![SomBox { id: 1, rect: [60.0, 20.0, 80.0, 30.0] }];

        let mut e1 = serde_json::json!({});
        let (_, d1) = process_screenshot_with_guard(
            &b64, (0.0, 0.0, 320.0, 200.0), None, &boxes1, true, &dir.path().join("s1"),
            &mut e1, None, Some("H1:viewport".into()),
        ).unwrap();
        let d1 = d1.unwrap();
        assert!(e1["unchanged"].is_null());

        let mut e2 = serde_json::json!({});
        let (_, _) = process_screenshot_with_guard(
            &b64, (0.0, 0.0, 320.0, 200.0), None, &boxes1, true, &dir.path().join("s2"),
            &mut e2, Some(&d1), Some("H1:viewport".into()),
        ).unwrap();
        assert_eq!(e2["unchanged"], true);
        assert_eq!(e2["path"], e1["path"]);

        let mut e3 = serde_json::json!({});
        let (_, _) = process_screenshot_with_guard(
            &b64, (0.0, 0.0, 320.0, 200.0), None, &boxes2, true, &dir.path().join("s3"),
            &mut e3, Some(&d1), Some("H1:viewport".into()),
        ).unwrap();
        assert!(e3["unchanged"].is_null(), "changed SoM boxes must invalidate unchanged frame reuse");
    }

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
}
