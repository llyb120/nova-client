//! 逐帧画面变化分。移植自 dsh-see-now 的 `src/cuts.js`（原算法出自 storyboard 的 scan.js）：
//! 64×36 网格上的 HSV 通道加权绝对差，另外给一份运动补偿的"变了多少格"。
//!
//! 没移植的：亮度归一闪避补（REF_V 96 / gain 1.85 / 黑边占比）。那两条是给暗场影视和
//! 宽银幕黑边用的，桌面截图的 V 均值基本都 ≥ 96、也没有 letterbox，gain 恒为 1。

pub(super) const GRID_W: usize = 64;
pub(super) const GRID_H: usize = 36;
const CELLS: usize = GRID_W * GRID_H;

/// 一帧的 64×36 HSV，三通道各 0–255，逐格连续存放。
pub(super) type Grid = Vec<f32>;

pub(super) fn grid(pixels: &image::RgbaImage) -> Grid {
    let small = image::imageops::resize(pixels, GRID_W as u32, GRID_H as u32, image::imageops::FilterType::Triangle);
    let mut out = vec![0.0; CELLS * 3];
    for (index, pixel) in small.pixels().enumerate() {
        let [h, s, v] = hsv(pixel[0], pixel[1], pixel[2]);
        out[index * 3] = h;
        out[index * 3 + 1] = s;
        out[index * 3 + 2] = v;
    }
    out
}

/// RGB → HSV，与 `cuts.js` 同刻度：H 由角度压到 0–255，S/V 各 0–255。
fn hsv(r: u8, g: u8, b: u8) -> [f32; 3] {
    let (r, g, b) = (f32::from(r) / 255.0, f32::from(g) / 255.0, f32::from(b) / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = max - min;
    let mut hue = if delta == 0.0 {
        0.0
    } else if max == r {
        ((g - b) / delta) % 6.0
    } else if max == g {
        (b - r) / delta + 2.0
    } else {
        (r - g) / delta + 4.0
    };
    hue *= 60.0;
    if hue < 0.0 { hue += 360.0; }
    [hue / 360.0 * 255.0, if max == 0.0 { 0.0 } else { delta / max * 255.0 }, max * 255.0]
}

/// 两帧 HSV 的加权绝对差均值（0–255）。色相按环形处理，低饱和像素的色相降权。
pub(super) fn score_pair(prev: &[f32], cur: &[f32]) -> f32 {
    let mut sum = 0.0;
    for cell in 0..CELLS {
        let i = cell * 3;
        let dh = (cur[i] - prev[i]).abs();
        let hue = dh.min(255.0 - dh);
        let weight = cur[i + 1].min(prev[i + 1]) / 255.0;
        sum += hue * weight + (cur[i + 1] - prev[i + 1]).abs() + (cur[i + 2] - prev[i + 2]).abs();
    }
    sum / (CELLS * 3) as f32
}

/// 运动补偿：整屏平移能在邻域里找得到对应，换画面找不到。
pub(super) struct Motion {
    /// 变了多少格（占比 ×255）。
    pub score: f32,
    /// 变化包围盒占整屏的比例（×255）。
    pub bbox: f32,
    /// 包围盒里的填充率。
    pub fill: f32,
}

/// 每格先在**同位置**比一次（静止画面最省），再在 ±radius 邻域里找最像的。
pub(super) fn motion(prev: &[f32], cur: &[f32], tau: f32, radius: i32) -> Motion {
    let diff = |a: usize, b: usize| {
        let dh = (cur[a * 3] - prev[b * 3]).abs();
        let hue = dh.min(255.0 - dh);
        let weight = cur[a * 3 + 1].min(prev[b * 3 + 1]) / 255.0;
        (hue * weight + (cur[a * 3 + 1] - prev[b * 3 + 1]).abs() + (cur[a * 3 + 2] - prev[b * 3 + 2]).abs()) / 3.0
    };
    let (mut changed, mut min_x, mut min_y, mut max_x, mut max_y) = (0u32, GRID_W, GRID_H, 0usize, 0usize);
    for y in 0..GRID_H {
        for x in 0..GRID_W {
            let cell = y * GRID_W + x;
            let mut matched = diff(cell, cell) <= tau;
            for dy in -radius..=radius {
                if matched { break; }
                for dx in -radius..=radius {
                    if matched || (dx == 0 && dy == 0) { continue; }
                    let (xx, yy) = (x as i32 + dx, y as i32 + dy);
                    if xx < 0 || yy < 0 || xx >= GRID_W as i32 || yy >= GRID_H as i32 { continue; }
                    if diff(cell, yy as usize * GRID_W + xx as usize) <= tau { matched = true; break; }
                }
            }
            if matched { continue; }
            changed += 1;
            min_x = min_x.min(x);
            max_x = max_x.max(x);
            min_y = min_y.min(y);
            max_y = max_y.max(y);
        }
    }
    let (width, height) = if changed == 0 { (0, 0) } else { (max_x - min_x + 1, max_y - min_y + 1) };
    Motion {
        score: changed as f32 / CELLS as f32 * 255.0,
        bbox: (width * height) as f32 / CELLS as f32 * 255.0,
        fill: if changed == 0 { 0.0 } else { changed as f32 / (width * height) as f32 },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(r: u8, g: u8, b: u8) -> image::RgbaImage {
        image::RgbaImage::from_pixel(640, 360, image::Rgba([r, g, b, 255]))
    }

    #[test]
    fn identical_frames_score_zero_and_a_new_picture_scores_high() {
        let a = grid(&plain(240, 240, 240));
        let b = grid(&plain(240, 240, 240));
        assert_eq!(score_pair(&a, &b), 0.0);
        // 只改亮度：视频里实测约 5 分，够不到默认门槛 12。
        let brighter = score_pair(&a, &grid(&plain(255, 255, 255)));
        assert!(brighter > 0.0 && brighter < 12.0, "{brighter}");
        // 整屏换色：远高于门槛。
        assert!(score_pair(&a, &grid(&plain(20, 60, 200))) > 12.0);
        let m = motion(&a, &grid(&plain(20, 60, 200)), 15.0, 2);
        assert!(m.score > 200.0 && (m.fill - 1.0).abs() < 0.01, "{} {}", m.score, m.fill);
    }

    #[test]
    fn still_screen_has_no_motion_but_a_shifted_one_is_absorbed_by_the_neighbourhood() {
        let base = grid(&plain(200, 200, 200));
        assert_eq!(motion(&base, &base, 15.0, 2).score, 0.0);
        // 12 格宽的竖条：同位置判变了；邻域内找不到（比 radius 宽），仍算变化。
        let mut picture = image::RgbaImage::from_pixel(640, 360, image::Rgba([200, 200, 200, 255]));
        for y in 0..360 { for x in 120..132 { picture.put_pixel(x, y, image::Rgba([10, 10, 10, 255])); } }
        let shifted = motion(&base, &grid(&picture), 15.0, 2);
        assert!(shifted.score > 5.0, "{}", shifted.score);
        assert!(shifted.fill > 0.9, "竖条占满高度、填充率高：{}", shifted.fill);
        // 只有一格高的细条：邻域能吸收掉大部分。
        let mut line = image::RgbaImage::from_pixel(640, 360, image::Rgba([200, 200, 200, 255]));
        for x in 0..640 { for y in 178..182 { line.put_pixel(x, y, image::Rgba([10, 10, 10, 255])); } }
        let thin = motion(&base, &grid(&line), 15.0, 2);
        assert!(thin.score < shifted.score, "{} < {}", thin.score, shifted.score);
    }
}
