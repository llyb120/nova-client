//! Linux pointer readback and the post-input settle watch.
//!
//! Screenshots (XGetImage / xcap) do not include the X cursor sprite, same as DXGI.
//! `XQueryPointer` is the cursor position in root pixels, which is the space `point()` targets.
//!
//! Settle watches the Damage extension from *before* the input, so a paint that finishes
//! during the click is still visible. After a substantial region and one quiet slice, the
//! wait ends. No extension: sample a 32px grid of one XGetImage. `press` never uses this.

use super::background;
use crate::visual_guard::DesktopPulse;
use std::time::{Duration, Instant};
use x11rb::connection::Connection;
use x11rb::protocol::damage::{self, ReportLevel};
use x11rb::protocol::xproto::{ConnectionExt, ImageFormat};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;

pub(super) fn pointer(display: &str) -> Option<(i32, i32)> {
    let (conn, screen) = RustConnection::connect(Some(display)).ok()?;
    let root = conn.setup().roots.get(screen)?.root;
    let reply = conn.query_pointer(root).ok()?.reply().ok()?;
    reply.same_screen.then_some((i32::from(reply.root_x), i32::from(reply.root_y)))
}

/// Cells of a 32px grid covered by a damage rectangle. Six or more is a real paint
/// (menu, window); a caret is one cell. Matches `frame_pulse`'s substantial-sample bar.
pub(super) fn paint_kind(w: u32, h: u32) -> DesktopPulse {
    let cells = w.div_ceil(32).saturating_mul(h.div_ceil(32));
    if cells >= 6 {
        DesktopPulse::Content
    } else if cells > 0 {
        DesktopPulse::Minor
    } else {
        DesktopPulse::Pointer
    }
}

pub(super) fn pulse_from_samples(changed: u32) -> DesktopPulse {
    if changed >= 6 {
        DesktopPulse::Content
    } else if changed > 0 {
        DesktopPulse::Minor
    } else {
        DesktopPulse::Quiet
    }
}

fn merge(a: DesktopPulse, b: DesktopPulse) -> DesktopPulse {
    use DesktopPulse::*;
    match (a, b) {
        (Content, _) | (_, Content) => Content,
        (Minor, _) | (_, Minor) => Minor,
        (Pointer, _) | (_, Pointer) => Pointer,
        _ => Quiet,
    }
}

fn clip(area: x11rb::protocol::xproto::Rectangle, target: Option<(i32, i32, u32, u32)>) -> Option<(u32, u32)> {
    let (x, y, w, h) = (i32::from(area.x), i32::from(area.y), i32::from(area.width), i32::from(area.height));
    let (x0, y0, x1, y1) = match target {
        Some((tx, ty, tw, th)) => (tx, ty, tx.saturating_add(tw as i32), ty.saturating_add(th as i32)),
        None => return (w > 0 && h > 0).then_some((w as u32, h as u32)),
    };
    let (ix0, iy0) = (x.max(x0), y.max(y0));
    let (ix1, iy1) = ((x + w).min(x1), (y + h).min(y1));
    (ix1 > ix0 && iy1 > iy0).then_some(((ix1 - ix0) as u32, (iy1 - iy0) as u32))
}

enum Mode {
    Off,
    Damage {
        conn: RustConnection,
        damage: u32,
        target: Option<(i32, i32, u32, u32)>,
    },
    Sample {
        conn: RustConnection,
        root: u32,
        x: i16,
        y: i16,
        width: u16,
        height: u16,
        baseline: Vec<u8>,
    },
}

pub(super) struct Watch {
    mode: Mode,
}

impl Watch {
    pub(super) fn off() -> Self {
        Self { mode: Mode::Off }
    }

    pub(super) fn arm(display: &str, target: Option<(i32, i32, u32, u32)>) -> Self {
        let Ok((conn, screen_num)) = RustConnection::connect(Some(display)) else {
            return Self::off();
        };
        let Some(screen) = conn.setup().roots.get(screen_num) else {
            return Self::off();
        };
        let root = screen.root;
        let damage_ok = damage::query_version(&conn, 1, 1).ok().and_then(|c| c.reply().ok()).is_some();
        if damage_ok {
            if let Ok(id) = conn.generate_id() {
                let created = damage::create(&conn, id, root, ReportLevel::BOUNDING_BOX).ok().and_then(|c| c.check().ok()).is_some();
                if created && conn.flush().is_ok() {
                    let _ = damage::subtract(&conn, id, 0u32, 0u32).ok().and_then(|c| c.check().ok());
                    let _ = conn.flush();
                    drain(&conn);
                    return Self { mode: Mode::Damage { conn, damage: id, target } };
                }
            }
        }
        let (sw, sh) = (u32::from(screen.width_in_pixels), u32::from(screen.height_in_pixels));
        let Some((x, y, width, height)) = sample_rect(sw, sh, target) else {
            return Self::off();
        };
        let Some(baseline) = grid_of(&conn, root, x, y, width, height) else {
            return Self::off();
        };
        Self { mode: Mode::Sample { conn, root, x, y, width, height, baseline } }
    }

    /// Forget damage from before this action, so only the paint it causes can end the wait.
    pub(super) fn note(&mut self, action: &str) {
        if matches!(action, "press" | "wait" | "move") {
            return;
        }
        if !self.reset() {
            self.mode = Mode::Off;
        }
    }

    /// Run the settle loop. `true` means the budget was spent here (early or in full).
    pub(super) fn wait(&mut self, budget: Duration) -> bool {
        if budget.is_zero() || matches!(self.mode, Mode::Off) {
            return false;
        }
        let deadline = Instant::now() + budget;
        let mut seen = false;
        while Instant::now() < deadline {
            let pulse = match self.pulse() {
                Some(pulse) => pulse,
                None => {
                    self.mode = Mode::Off;
                    return false;
                }
            };
            let (next, stop) = super::settle_step(seen, pulse);
            seen = next;
            if stop {
                return true;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            let slice = if seen { Duration::from_millis(32) } else { Duration::from_millis(16) };
            std::thread::sleep(left.min(slice));
        }
        true
    }

    fn reset(&mut self) -> bool {
        match &mut self.mode {
            Mode::Off => false,
            Mode::Damage { conn, damage, .. } => {
                if damage::subtract(conn, *damage, 0u32, 0u32).ok().and_then(|c| c.check().ok()).is_none() || conn.flush().is_err() {
                    return false;
                }
                drain(conn)
            }
            Mode::Sample { conn, root, x, y, width, height, baseline } => {
                match grid_of(conn, *root, *x, *y, *width, *height) {
                    Some(grid) => {
                        *baseline = grid;
                        true
                    }
                    None => false,
                }
            }
        }
    }

    fn pulse(&mut self) -> Option<DesktopPulse> {
        match &mut self.mode {
            Mode::Off => None,
            Mode::Damage { conn, damage, target } => {
                let mut pulse = DesktopPulse::Quiet;
                loop {
                    match conn.poll_for_event() {
                        Ok(Some(Event::DamageNotify(event))) => {
                            if let Some((w, h)) = clip(event.area, *target) {
                                pulse = merge(pulse, paint_kind(w, h));
                            }
                            if damage::subtract(conn, *damage, 0u32, 0u32).ok().and_then(|c| c.check().ok()).is_none() || conn.flush().is_err() {
                                return None;
                            }
                        }
                        Ok(Some(_)) => {}
                        Ok(None) => return Some(pulse),
                        Err(_) => return None,
                    }
                }
            }
            Mode::Sample { conn, root, x, y, width, height, baseline } => {
                let grid = grid_of(conn, *root, *x, *y, *width, *height)?;
                let changed = fastpix::rgb_diff_count(baseline, &grid).unwrap_or(u32::MAX);
                *baseline = grid;
                Some(pulse_from_samples(changed))
            }
        }
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        if let Mode::Damage { conn, damage, .. } = &self.mode {
            let _ = damage::destroy(conn, *damage).ok().and_then(|c| c.check().ok());
            let _ = conn.flush();
        }
    }
}

fn drain(conn: &RustConnection) -> bool {
    loop {
        match conn.poll_for_event() {
            Ok(Some(_)) => {}
            Ok(None) => return true,
            Err(_) => return false,
        }
    }
}

fn sample_rect(screen_w: u32, screen_h: u32, target: Option<(i32, i32, u32, u32)>) -> Option<(i16, i16, u16, u16)> {
    let (x, y, w, h) = match target {
        Some((x, y, w, h)) => (x, y, w as i32, h as i32),
        None => (0, 0, screen_w as i32, screen_h as i32),
    };
    let (x0, y0) = (x.max(0), y.max(0));
    let (x1, y1) = ((x + w).min(screen_w as i32), (y + h).min(screen_h as i32));
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    let (w, h) = ((x1 - x0) as u32, (y1 - y0) as u32);
    if u64::from(w) * u64::from(h) * 4 > 8_000_000 {
        return None;
    }
    Some((x0 as i16, y0 as i16, w as u16, h as u16))
}

fn grid_of(conn: &RustConnection, root: u32, x: i16, y: i16, width: u16, height: u16) -> Option<Vec<u8>> {
    let reply = conn.get_image(ImageFormat::Z_PIXMAP, root, x, y, width, height, !0u32).ok()?.reply().ok()?;
    Some(fastpix::grid_rgb(&reply.data, u32::from(width), u32::from(height), 32))
}

/// Display used for a desktop watch. Background mode reads the private seat, not `DISPLAY`.
pub(super) fn observation_display(background_mode: bool) -> Option<String> {
    if background_mode {
        return background::seat_display().ok();
    }
    std::env::var("DISPLAY").ok().filter(|d| !d.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn damage_rects_use_the_same_substantial_bar_as_frame_samples() {
        assert_eq!(paint_kind(0, 0), DesktopPulse::Pointer);
        assert_eq!(paint_kind(2, 16), DesktopPulse::Minor, "a caret is one 32px cell");
        assert_eq!(paint_kind(200, 150), DesktopPulse::Content, "a menu covers many cells");
        assert_eq!(paint_kind(1920, 1080), DesktopPulse::Content);
        assert_eq!(pulse_from_samples(0), DesktopPulse::Quiet);
        assert_eq!(pulse_from_samples(5), DesktopPulse::Minor);
        assert_eq!(pulse_from_samples(6), DesktopPulse::Content);
        let outside = x11rb::protocol::xproto::Rectangle { x: 0, y: 0, width: 10, height: 10 };
        assert!(clip(outside, Some((100, 100, 50, 50))).is_none());
        let menu = x11rb::protocol::xproto::Rectangle { x: 100, y: 100, width: 200, height: 150 };
        assert_eq!(clip(menu, Some((0, 0, 1920, 1080))), Some((200, 150)));
    }
}
