//! 档位：这一帧该占多大格子。移植自 dsh-see-now 的 `src/tiers.js`。
//!
//! 档位 k 的含义是「格子边长 = 画布的 1/k」，一张图最多放 k² 格。它由**密度**决定：
//! 最近 1 秒留了几帧（每秒几帧，帧/秒），走一阶 EMA 平滑后按 7 道阈值分档，带 5% 滞回。
//! 阈值取自 docx（每帧碰上切点的概率 × 30fps），档位越高格子越小。

/// 七道阈值，帧/秒。档位 = 1 + 「密度 ≥ 第几道」。
const LEVEL_THRESHOLDS: [f32; 7] = [0.26, 0.6, 0.9, 1.2, 1.8, 2.4, 3.0];
const HYSTERESIS: f32 = 0.05;
pub(super) const TIER_MIN: u32 = 1;
pub(super) const TIER_MAX: u32 = 8;

/// 档位策略。原仓默认 `tierStyle: 'standard'`：1 档压成 1/2。一格占满整张图时，一张图只放一帧，
/// 静态画面会把 75% 画布白扔掉。其它档位（native / full / compact）没做成设置，这里不做。
pub(super) const TIER_FLOOR: u32 = 2;

/// 密度窗口固定 1 秒；EMA 每帧走一步（`smooth` 帧的时间常数）。
pub(super) struct Tiers {
    window: f64,
    alpha: f32,
    up: [f32; 7],
    down: [f32; 7],
    bounds: Vec<f64>,
    smoothed: f32,
    level: u32,
}

impl Tiers {
    /// `smooth` 是 EMA 的时间常数（采样数）。原仓是 6 帧 @12fps；桌面 2fps 下同样是 6 个采样，
    /// 只是它覆盖 3 秒而不是 0.5 秒 —— 档位本来就该慢慢变，不要紧。
    pub(super) fn new(smooth: u32) -> Self {
        Self {
            window: 1.0,
            alpha: 1.0 / smooth.max(1) as f32,
            up: LEVEL_THRESHOLDS.map(|v| v * (1.0 + HYSTERESIS)),
            down: LEVEL_THRESHOLDS.map(|v| v * (1.0 - HYSTERESIS)),
            bounds: Vec::new(),
            smoothed: 0.0,
            level: TIER_MIN,
        }
    }

    /// 记一个边界（这一帧被留下了）。
    pub(super) fn mark(&mut self, t: f64) {
        self.bounds.push(t);
    }

    /// 每次采样都调一次（不管留没留帧），否则 EMA 的步长会跟着出帧间隔走。
    pub(super) fn tick(&mut self, t: f64) {
        let low = t - self.window;
        let kept = self.bounds.iter().rev().take_while(|bound| **bound > low).count();
        self.bounds.retain(|bound| *bound > low - 1.0);
        let raw = kept as f32 / self.window as f32;
        // 一次 tick 多推几步等价于密集采样；这里按采样数加权，2fps 时自然比 12fps 慢。
        self.smoothed += (raw - self.smoothed) * self.alpha;
        let value = self.smoothed;
        let count_at_least = |table: &[f32; 7]| 1 + table.iter().filter(|limit| value >= **limit).count() as u32;
        if self.level < TIER_MAX && value > self.up[(self.level - 1) as usize] {
            self.level = count_at_least(&self.up);
        } else if self.level > TIER_MIN && value < self.down[(self.level - 2) as usize] {
            self.level = count_at_least(&self.down);
        }
        self.level = self.level.clamp(TIER_MIN, TIER_MAX);
    }

    pub(super) fn level(&self) -> u32 {
        self.level
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn density_rises_to_a_small_cell_and_falls_back_with_hysteresis() {
        let mut tiers = Tiers::new(6);
        // 一秒留下 2 帧 → 密度 2 帧/秒 → 档位 6（1.8 以上、2.4 以下）。
        for step in 0..24 {
            let t = step as f64 / 12.0;
            if step % 6 == 0 { tiers.mark(t); }
            tiers.tick(t);
        }
        assert_eq!(tiers.level(), 6);
        // 画面静下来：边界滑出窗口后密度归零，档位回到 1（占满整幅）。
        for step in 24..240 {
            tiers.tick(step as f64 / 12.0);
        }
        assert_eq!(tiers.level(), 1);
    }
}
