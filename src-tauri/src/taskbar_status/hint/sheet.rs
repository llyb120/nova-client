//! 拼图：把留下的帧按档位装进一张画布，图底下再接一条「这段时间屏幕怎么动」的条。
//! 移植自 dsh-see-now 的 `src/sheet.js`（构图 / 编号 / 音轨条）与 `src/tokens.js`（token 阶梯）。
//!
//! 与原始实现相同的三件事：格子边长 = 画布 / 档位（变化越大占得越大）、图上烧编号
//! （积分帧不编号，免得被当成一次换画面）、底部条的高度吃掉「token 阶梯的免费余量」。

use image::{Rgba, RgbaImage};

pub(super) const BACKGROUND: [u8; 3] = [18, 18, 20];
pub(super) const PAD_FILL: [u8; 3] = [42, 42, 46];
const BADGE: [u8; 3] = [255, 235, 59];
const STRIP_BG: [u8; 3] = [11, 11, 13];
const STRIP_BAR: [u8; 3] = [63, 184, 175];
const STRIP_TICK: [u8; 3] = [255, 235, 59];
const STRIP_SEP: [u8; 3] = [60, 60, 66];
/// 每格留 1px 缝：底色透出来就是分隔线。
const SEAM: u32 = 1;
/// token 的一个格子覆盖 42×42 px（14px patch × 3 倍降采样）。
const TOKEN_CELL: u32 = 42;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Via {
    Start,
    Cut,
    Mark,
    Floor,
}

impl Via {
    pub(super) fn label(self) -> &'static str {
        match self { Via::Start => "start", Via::Cut => "cut", Via::Mark => "mark", Via::Floor => "floor" }
    }
}

/// 画布档位（README 的 low / standard / high / ultra）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Quality {
    pub(super) key: &'static str,
    pub(super) width: u32,
    pub(super) height: u32,
}

pub(super) const QUALITIES: [Quality; 4] = [
    Quality { key: "low", width: 798, height: 450 },
    Quality { key: "standard", width: 1050, height: 600 },
    Quality { key: "high", width: 1260, height: 720 },
    Quality { key: "ultra", width: 1596, height: 900 },
];

pub(super) fn quality(key: &str) -> Quality {
    QUALITIES.into_iter().find(|quality| quality.key == key).unwrap_or(QUALITIES[0])
}

pub(super) struct Cell {
    pub(super) tile: RgbaImage,
    pub(super) tier: u32,
    pub(super) at: f64,
    pub(super) via: Via,
    pub(super) score: f32,
}

/// 一张交给模型的图，和它的清单（提示语里逐格列出）。
pub(super) struct Sheet {
    pub(super) jpeg: Vec<u8>,
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) tokens: u32,
    /// 最后一格的时刻（秒）；清单里的「多少秒前」都以它为零点。
    pub(super) to: f64,
    pub(super) cells: Vec<CellInfo>,
}

pub(super) struct CellInfo {
    pub(super) index: usize,
    pub(super) at: f64,
    pub(super) via: Via,
    pub(super) score: f32,
    pub(super) tier: u32,
}

/// 图像 token：每 42px 一格，每行多一个分隔 token，整张再加 2 个。
pub(super) fn tokens(width: u32, height: u32) -> u32 {
    let cells = |px: u32| px.div_ceil(TOKEN_CELL).max(1);
    let (rows, cols) = (cells(height), cells(width));
    rows * (cols + 1) + 2
}

/// 条高 = 「画面本来没用满的那点余量」，不跨 token 行，所以不涨钱。
pub(super) fn strip_height(quality: Quality) -> u32 {
    let rows = quality.height.div_ceil(TOKEN_CELL).max(1);
    let room = rows * TOKEN_CELL - quality.height;
    let cap = 22u32.max((quality.width as f32 * 0.055).round() as u32);
    if room < 10 { cap.min(22) } else { room.min(cap) }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Rect {
    pub(super) x: u32,
    pub(super) y: u32,
    pub(super) w: u32,
    pub(super) h: u32,
}

/// 行流式装格：边长 = 画布 / 档位，行高取该行最高的一格。装不下返回 None。
pub(super) fn pack(tiers: &[u32], canvas: (u32, u32)) -> Option<Vec<Rect>> {
    let mut rects = Vec::with_capacity(tiers.len());
    let (mut x, mut y, mut row) = (0u32, 0u32, 0u32);
    for tier in tiers {
        let (w, h) = (canvas.0 / tier.max(&1), canvas.1 / tier.max(&1));
        if x + w > canvas.0 {
            x = 0;
            y += row;
            row = 0;
        }
        if y + h > canvas.1 { return None; }
        rects.push(Rect { x, y, w, h });
        x += w;
        row = row.max(h);
    }
    Some(rects)
}

/// 一帧一档拼成一张图。`history` 是这期间的每采样变化分（画底部条用）。
pub(super) fn compose(cells: &[Cell], quality: Quality, history: &[(f64, f32)]) -> Result<Sheet, String> {
    let rects = pack(&cells.iter().map(|cell| cell.tier).collect::<Vec<_>>(), (quality.width, quality.height))
        .ok_or("这批帧装不进画布")?;
    let strip = strip_height(quality);
    let (width, height) = (quality.width, quality.height + strip);
    let mut canvas = RgbaImage::from_pixel(width, height, rgba(BACKGROUND));
    let pads = RgbaImage::from_pixel(quality.width, quality.height, rgba(PAD_FILL));
    image::imageops::replace(&mut canvas, &pads, 0, 0);
    for (cell, rect) in cells.iter().zip(&rects) {
        let (w, h) = (rect.w.saturating_sub(SEAM).max(1), rect.h.saturating_sub(SEAM).max(1));
        let fitted = cover(&cell.tile, w, h);
        image::imageops::replace(&mut canvas, &fitted, i64::from(rect.x), i64::from(rect.y));
    }
    // 编号单独一轮：后画的格子会盖掉前一格伸出来的描边。
    let scale = if width >= 1050 { 3 } else { 2 };
    for (index, (cell, rect)) in cells.iter().zip(&rects).enumerate() {
        // 积分帧不是"换画面"，不烧号 —— 图上因此会跳号，清单里的序号才是准的。
        if cell.via == Via::Mark { continue; }
        badge(&mut canvas, rect.x + 2, rect.y + 1, index + 1, scale);
    }
    let from = cells.first().map_or(0.0, |cell| cell.at);
    let to = cells.last().map_or(from, |cell| cell.at);
    strip_area(&mut canvas, quality.height, strip, (from, to), history, cells);
    let rgb = image::DynamicImage::ImageRgba8(canvas).into_rgb8();
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 78)
        .encode(rgb.as_raw(), rgb.width(), rgb.height(), image::ExtendedColorType::Rgb8)
        .map_err(|e| format!("编码拼图失败：{e}"))?;
    Ok(Sheet {
        jpeg, width, height, tokens: tokens(width, height), to,
        cells: cells.iter().enumerate().map(|(index, cell)| CellInfo {
            index: index + 1, at: cell.at, via: cell.via, score: cell.score, tier: cell.tier,
        }).collect(),
    })
}

fn rgba(color: [u8; 3]) -> Rgba<u8> {
    Rgba([color[0], color[1], color[2], 255])
}

/// 保持比例铺满格子（多余部分裁掉），不拉伸。
fn cover(tile: &RgbaImage, width: u32, height: u32) -> RgbaImage {
    let (tw, th) = tile.dimensions();
    if tw == 0 || th == 0 { return RgbaImage::from_pixel(width, height, rgba(PAD_FILL)); }
    let scale = (width as f32 / tw as f32).max(height as f32 / th as f32);
    let (sw, sh) = ((tw as f32 * scale).round().max(1.0) as u32, (th as f32 * scale).round().max(1.0) as u32);
    let scaled = image::imageops::resize(tile, sw, sh, image::imageops::FilterType::Triangle);
    image::imageops::crop_imm(&scaled, (sw - width) / 2, (sh - height) / 2, width, height).to_image()
}

/// 3×5 点阵数字。
const DIGITS: [[u8; 5]; 10] = [
    [0b111, 0b101, 0b101, 0b101, 0b111],
    [0b010, 0b110, 0b010, 0b010, 0b111],
    [0b111, 0b001, 0b111, 0b100, 0b111],
    [0b111, 0b001, 0b111, 0b001, 0b111],
    [0b101, 0b101, 0b111, 0b001, 0b001],
    [0b111, 0b100, 0b111, 0b001, 0b111],
    [0b111, 0b100, 0b111, 0b101, 0b111],
    [0b111, 0b001, 0b001, 0b001, 0b001],
    [0b111, 0b101, 0b111, 0b101, 0b111],
    [0b111, 0b101, 0b111, 0b001, 0b111],
];

fn badge(canvas: &mut RgbaImage, x: u32, y: u32, number: usize, scale: u32) {
    let glyphs: Vec<&[u8; 5]> = number.to_string().bytes()
        .map(|digit| &DIGITS[(digit - b'0') as usize]).collect();
    // 先描边再压黄字：小格子上也认得出。
    for (dx, dy) in [(-1i32, 0i32), (1, 0), (0, -1), (0, 1), (-1, -1), (1, -1), (-1, 1), (1, 1)] {
        draw(canvas, &glyphs, x, y, scale, dx, dy, [0, 0, 0]);
    }
    draw(canvas, &glyphs, x, y, scale, 0, 0, BADGE);
}

fn draw(canvas: &mut RgbaImage, glyphs: &[&[u8; 5]], x: u32, y: u32, scale: u32,
    dx: i32, dy: i32, color: [u8; 3]) {
    for (index, glyph) in glyphs.iter().enumerate() {
        let left = i64::from(x) + index as i64 * i64::from(4 * scale) + i64::from(dx);
        for (row, bits) in glyph.iter().enumerate() {
            for column in 0..3u32 {
                if bits & (1 << (2 - column)) == 0 { continue; }
                for sy in 0..scale {
                    for sx in 0..scale {
                        let px = left + i64::from(column * scale + sx);
                        let py = i64::from(y) + i64::from(row as u32 * scale + sy) + i64::from(dy);
                        if px >= 0 && py >= 0 && (px as u32) < canvas.width() && (py as u32) < canvas.height() {
                            canvas.put_pixel(px as u32, py as u32, rgba(color));
                        }
                    }
                }
            }
        }
    }
}

/// 底部条：变化强度柱 + 每秒分隔 + 每帧的黄色刻度。
fn strip_area(canvas: &mut RgbaImage, top: u32, height: u32, window: (f64, f64), history: &[(f64, f32)],
    cells: &[Cell]) {
    if height == 0 { return; }
    let width = canvas.width();
    for y in top..(top + height).min(canvas.height()) {
        for x in 0..width { canvas.put_pixel(x, y, rgba(STRIP_BG)); }
    }
    let span = (window.1 - window.0).max(0.001);
    let at_x = |t: f64| (((t - window.0) / span) * f64::from(width - 1)).round().clamp(0.0, f64::from(width - 1)) as u32;
    // 每秒一条分隔线。
    let mut second = window.0.ceil();
    while second <= window.1 {
        let x = at_x(second);
        for y in top..(top + height).min(canvas.height()) { canvas.put_pixel(x, y, rgba(STRIP_SEP)); }
        second += 1.0;
    }
    // 变化强度柱：一格一列，取这列覆盖到的采样里的最高分。
    let usable = height.saturating_sub(2).max(1);
    for x in 0..width {
        let from = window.0 + span * f64::from(x) / f64::from(width);
        let to = window.0 + span * f64::from(x + 1) / f64::from(width);
        let peak = history.iter().filter(|(t, _)| *t >= from && *t < to).map(|(_, score)| *score).fold(0.0f32, f32::max);
        let filled = ((peak / 60.0).clamp(0.0, 1.0) * usable as f32).round() as u32;
        for y in 0..filled {
            let py = top + height - 1 - y;
            if py > top { canvas.put_pixel(x, py, rgba(STRIP_BAR)); }
        }
    }
    // 黄色刻度：每一帧落在条上的位置，和格子里的编号对得上。
    for cell in cells {
        let x = at_x(cell.at);
        for dx in 0..2 {
            let px = (x + dx).min(width - 1);
            for dy in 0..2 {
                let py = (top + dy).min(canvas.height() - 1);
                canvas.put_pixel(px, py, rgba(STRIP_TICK));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(tier: u32, at: f64, via: Via) -> Cell {
        Cell { tile: RgbaImage::from_pixel(798, 450, rgba([80, 90, 100])), tier, at, via, score: 20.0 }
    }

    #[test]
    fn canvas_sizes_cost_exactly_what_the_original_readme_claims() {
        for (quality, expected) in [(QUALITIES[0], 222), (QUALITIES[1], 392), (QUALITIES[2], 560), (QUALITIES[3], 860)] {
            let height = quality.height + strip_height(quality);
            assert_eq!(tokens(quality.width, height), expected, "{}", quality.key);
        }
    }

    #[test]
    fn cells_pack_row_major_and_the_canvas_refuses_what_does_not_fit() {
        let canvas = (798, 450);
        // 2 档 = 1/2 边长 → 一张图 4 格。
        let rects = pack(&[2, 2, 2, 2], canvas).unwrap();
        assert_eq!(rects[1], Rect { x: 399, y: 0, w: 399, h: 225 });
        assert_eq!(rects[3], Rect { x: 399, y: 225, w: 399, h: 225 });
        // 3 档 = 九宫格，正好铺满。
        let nine = pack(&[3; 9], canvas).unwrap();
        assert_eq!((nine[8].x, nine[8].y), (532, 300));
        // 1 档占满整幅。
        assert_eq!(pack(&[1], canvas).unwrap(), vec![Rect { x: 0, y: 0, w: 798, h: 450 }]);
        // 混排：三个 3 档一格一行，第四个换到下一行。
        let mixed = pack(&[3, 3, 3, 3], canvas).unwrap();
        assert_eq!((mixed[3].x, mixed[3].y), (0, 150));
        // 一个整幅的 1 档格子之后就没有地方了。
        assert!(pack(&[1, 3], canvas).is_none());
        assert!(pack(&[2; 5], canvas).is_none(), "第五个 2 档格子放不下");
    }

    #[test]
    fn sheet_keeps_its_pixel_size_and_carries_a_timeline_strip() {
        let cells = vec![cell(2, 0.0, Via::Start), cell(2, 3.0, Via::Cut), cell(2, 9.0, Via::Mark)];
        let history = vec![(0.0, 2.0), (3.0, 40.0), (9.0, 12.0)];
        let sheet = compose(&cells, QUALITIES[0], &history).unwrap();
        assert_eq!((sheet.width, sheet.height), (798, 462));
        assert!(sheet.jpeg.starts_with(&[0xFF, 0xD8]), "应当是 JPEG");
        assert_eq!(sheet.cells.len(), 3);
        assert_eq!(sheet.cells[1].at, 3.0);
        assert_eq!(sheet.to, 9.0);
        // 积分帧不烧号，但仍占一个格子、仍在清单里。
        assert_eq!(sheet.cells[2].via, Via::Mark);
    }
}
