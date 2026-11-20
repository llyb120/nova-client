//! Linux pointer readback and the post-input settle watch.
//!
//! Screenshots (XGetImage / xcap) do not include the X cursor sprite, same as DXGI.
//! `XQueryPointer` is the cursor position in root pixels, which is the space `point()` targets.
//!
//! Settle watches the Damage extension from *before* the input, so a paint that finishes
//! during the click is still visible. One real paint — a glyph, a caret, a button highlight —
//! and the following quiet slice end the wait. The pointer sprite is not in Damage. Clock
//! applets, docks' small ticks, and Nova's own windows are ignored, so they cannot end it.
//! No extension: one changed 32px sample outside those windows counts. `press` never uses this.

use super::background;
use crate::visual_guard::DesktopPulse;
use std::time::{Duration, Instant};
use x11rb::connection::Connection;
use x11rb::protocol::damage::{self, ReportLevel};
use x11rb::protocol::xproto::{AtomEnum, ConnectionExt, ImageFormat};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;

pub(super) fn pointer(display: &str) -> Option<(i32, i32)> {
    let (conn, screen) = RustConnection::connect(Some(display)).ok()?;
    let root = conn.setup().roots.get(screen)?.root;
    let reply = conn.query_pointer(root).ok()?.reply().ok()?;
    reply.same_screen.then_some((i32::from(reply.root_x), i32::from(reply.root_y)))
}

/// Any positive damage is a real paint. A glyph is one cell; that used to be "minor" and the
/// wait then slept the whole budget. Zero area is not a paint.
pub(super) fn paint_kind(w: u32, h: u32) -> DesktopPulse {
    if w > 0 && h > 0 {
        DesktopPulse::Content
    } else {
        DesktopPulse::Pointer
    }
}

/// One changed sample is enough. Zero stays quiet, so a slice with no paint can end the wait.
pub(super) fn pulse_from_samples(changed: u32) -> DesktopPulse {
    if changed > 0 { DesktopPulse::Content } else { DesktopPulse::Quiet }
}

/// Windows whose paints must not release the settle wait.
#[derive(Clone, Default)]
struct Ignore {
    /// Ignore every damage rectangle fully inside (clock, Nova, this process on the user display).
    full: Vec<(i32, i32, i32, i32)>,
    /// Ignore only a small rectangle fully inside (a panel clock). A large panel redraw still counts.
    docks: Vec<(i32, i32, i32, i32)>,
}

impl Ignore {
    fn skips(&self, x: i32, y: i32, w: u32, h: u32) -> bool {
        if w == 0 || h == 0 {
            return false;
        }
        let (x1, y1) = (x.saturating_add(w as i32), y.saturating_add(h as i32));
        let inside = |r: &(i32, i32, i32, i32)| x >= r.0 && y >= r.1 && x1 <= r.2 && y1 <= r.3;
        if self.full.iter().any(inside) {
            return true;
        }
        let small = w <= 160 && h <= 48;
        small && self.docks.iter().any(inside)
    }
}

fn clip_rect(area: x11rb::protocol::xproto::Rectangle, target: Option<(i32, i32, u32, u32)>) -> Option<(i32, i32, u32, u32)> {
    let (x, y, w, h) = (i32::from(area.x), i32::from(area.y), i32::from(area.width), i32::from(area.height));
    let (x0, y0, x1, y1) = match target {
        Some((tx, ty, tw, th)) => (tx, ty, tx.saturating_add(tw as i32), ty.saturating_add(th as i32)),
        None => return (w > 0 && h > 0).then_some((x, y, w as u32, h as u32)),
    };
    let (ix0, iy0) = (x.max(x0), y.max(y0));
    let (ix1, iy1) = ((x + w).min(x1), (y + h).min(y1));
    (ix1 > ix0 && iy1 > iy0).then_some((ix0, iy0, (ix1 - ix0) as u32, (iy1 - iy0) as u32))
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
    root: u32,
    display: String,
    ignore: Ignore,
}

impl Watch {
    pub(super) fn off() -> Self {
        Self { mode: Mode::Off, root: 0, display: String::new(), ignore: Ignore::default() }
    }

    pub(super) fn arm(display: &str, target: Option<(i32, i32, u32, u32)>) -> Self {
        let Ok((conn, screen_num)) = RustConnection::connect(Some(display)) else {
            return Self::off();
        };
        let Some(screen) = conn.setup().roots.get(screen_num) else {
            return Self::off();
        };
        let root = screen.root;
        let ignore = collect_ignore(&conn, root, ignored_pid(display));
        let damage_ok = damage::query_version(&conn, 1, 1).ok().and_then(|c| c.reply().ok()).is_some();
        if damage_ok {
            if let Ok(id) = conn.generate_id() {
                // Raw rectangles stay inside the window that painted. A bounding box would
                // join a clock tick and Nova's own window into one rect that is inside neither.
                let created = damage::create(&conn, id, root, ReportLevel::RAW_RECTANGLES).ok().and_then(|c| c.check().ok()).is_some();
                if created && conn.flush().is_ok() {
                    let _ = damage::subtract(&conn, id, 0u32, 0u32).ok().and_then(|c| c.check().ok());
                    let _ = conn.flush();
                    drain(&conn);
                    return Self { mode: Mode::Damage { conn, damage: id, target }, root, display: display.to_string(), ignore };
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
        Self { mode: Mode::Sample { conn, root, x, y, width, height, baseline }, root, display: display.to_string(), ignore }
    }

    /// Forget damage from before this action, so only the paint it causes can end the wait.
    pub(super) fn note(&mut self, action: &str) {
        if matches!(action, "press" | "wait" | "move") {
            return;
        }
        let refreshed = self.connection().map(|conn| collect_ignore(conn, self.root, ignored_pid(&self.display)));
        if let Some(ignore) = refreshed {
            self.ignore = ignore;
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

    fn connection(&self) -> Option<&RustConnection> {
        match &self.mode {
            Mode::Damage { conn, .. } | Mode::Sample { conn, .. } => Some(conn),
            Mode::Off => None,
        }
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
        let ignore = self.ignore.clone();
        match &mut self.mode {
            Mode::Off => None,
            Mode::Damage { conn, damage, target } => {
                let mut pulse = DesktopPulse::Quiet;
                let mut dirty = false;
                loop {
                    match conn.poll_for_event() {
                        Ok(Some(Event::DamageNotify(event))) => {
                            dirty = true;
                            if let Some((x, y, w, h)) = clip_rect(event.area, *target) {
                                if !ignore.skips(x, y, w, h) {
                                    pulse = DesktopPulse::Content;
                                }
                            }
                        }
                        Ok(Some(_)) => {}
                        Ok(None) => {
                            if dirty {
                                let cleared = damage::subtract(conn, *damage, 0u32, 0u32).ok().and_then(|c| c.check().ok()).is_some()
                                    && conn.flush().is_ok();
                                if !cleared {
                                    return None;
                                }
                            }
                            return Some(pulse);
                        }
                        Err(_) => return None,
                    }
                }
            }
            Mode::Sample { conn, root, x, y, width, height, baseline } => {
                let grid = grid_of(conn, *root, *x, *y, *width, *height)?;
                let changed = relevant_samples(baseline, &grid, (i32::from(*x), i32::from(*y)), (u32::from(*width), u32::from(*height)), &ignore)?;
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

fn relevant_samples(baseline: &[u8], grid: &[u8], origin: (i32, i32), size: (u32, u32), ignore: &Ignore) -> Option<u32> {
    if baseline.len() != grid.len() || baseline.len() % 3 != 0 {
        return None;
    }
    let cols = size.0.div_ceil(32).max(1);
    let mut changed = 0u32;
    for (i, (a, b)) in baseline.chunks_exact(3).zip(grid.chunks_exact(3)).enumerate() {
        if (0..3).all(|c| a[c].abs_diff(b[c]) <= 16) {
            continue;
        }
        let (col, row) = ((i as u32) % cols, (i as u32) / cols);
        let (x, y) = (origin.0 + (col * 32) as i32, origin.1 + (row * 32) as i32);
        if ignore.skips(x, y, 1, 1) {
            continue;
        }
        changed += 1;
    }
    Some(changed)
}

/// On the seat, test windows share this process and must be able to end the wait.
/// On the user display, Nova's own windows must not.
fn ignored_pid(display: &str) -> Option<u32> {
    if background::seated_display().as_deref() == Some(display) {
        None
    } else {
        Some(std::process::id())
    }
}

struct Atoms {
    class: u32,
    name: u32,
    net_name: u32,
    pid: u32,
    window_type: u32,
    dock: u32,
}

fn collect_ignore(conn: &RustConnection, root: u32, self_pid: Option<u32>) -> Ignore {
    let Some(atoms) = load_atoms(conn) else {
        return Ignore::default();
    };
    let mut wins = child_list(conn, &[root]);
    let nested = child_list(conn, &wins);
    wins.extend(nested);
    wins.truncate(240);
    classify(conn, root, &wins, &atoms, self_pid)
}

fn load_atoms(conn: &RustConnection) -> Option<Atoms> {
    let names: [&[u8]; 6] = [b"WM_CLASS", b"WM_NAME", b"_NET_WM_NAME", b"_NET_WM_PID", b"_NET_WM_WINDOW_TYPE", b"_NET_WM_WINDOW_TYPE_DOCK"];
    let cookies = names.iter().map(|name| conn.intern_atom(false, name)).collect::<Result<Vec<_>, _>>().ok()?;
    conn.flush().ok()?;
    let mut ids = Vec::with_capacity(names.len());
    for cookie in cookies {
        ids.push(cookie.reply().ok()?.atom);
    }
    Some(Atoms { class: ids[0], name: ids[1], net_name: ids[2], pid: ids[3], window_type: ids[4], dock: ids[5] })
}

fn child_list(conn: &RustConnection, windows: &[u32]) -> Vec<u32> {
    if windows.is_empty() {
        return Vec::new();
    }
    let Ok(cookies) = windows.iter().map(|window| conn.query_tree(*window)).collect::<Result<Vec<_>, _>>() else {
        return Vec::new();
    };
    if conn.flush().is_err() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for cookie in cookies {
        if let Ok(reply) = cookie.reply() {
            out.extend(reply.children);
        }
    }
    out
}

fn classify(conn: &RustConnection, root: u32, wins: &[u32], atoms: &Atoms, self_pid: Option<u32>) -> Ignore {
    if wins.is_empty() {
        return Ignore::default();
    }
    let Ok(geos) = wins.iter().map(|w| conn.get_geometry(*w)).collect::<Result<Vec<_>, _>>() else { return Ignore::default(); };
    let Ok(trans) = wins.iter().map(|w| conn.translate_coordinates(*w, root, 0, 0)).collect::<Result<Vec<_>, _>>() else { return Ignore::default(); };
    let Ok(classes) = wins.iter().map(|w| conn.get_property(false, *w, atoms.class, AtomEnum::ANY, 0, 128)).collect::<Result<Vec<_>, _>>() else { return Ignore::default(); };
    let Ok(names) = wins.iter().map(|w| conn.get_property(false, *w, atoms.name, AtomEnum::ANY, 0, 128)).collect::<Result<Vec<_>, _>>() else { return Ignore::default(); };
    let Ok(net_names) = wins.iter().map(|w| conn.get_property(false, *w, atoms.net_name, AtomEnum::ANY, 0, 128)).collect::<Result<Vec<_>, _>>() else { return Ignore::default(); };
    let Ok(pids) = wins.iter().map(|w| conn.get_property(false, *w, atoms.pid, AtomEnum::ANY, 0, 1)).collect::<Result<Vec<_>, _>>() else { return Ignore::default(); };
    let Ok(kinds) = wins.iter().map(|w| conn.get_property(false, *w, atoms.window_type, AtomEnum::ANY, 0, 8)).collect::<Result<Vec<_>, _>>() else { return Ignore::default(); };
    if conn.flush().is_err() {
        return Ignore::default();
    }
    let mut ignore = Ignore::default();
    let mut geos = geos.into_iter();
    let mut trans = trans.into_iter();
    let mut classes = classes.into_iter();
    let mut names = names.into_iter();
    let mut net_names = net_names.into_iter();
    let mut pids = pids.into_iter();
    let mut kinds = kinds.into_iter();
    for _ in 0..wins.len() {
        let geo_c = geos.next();
        let at_c = trans.next();
        let class = classes.next().map(property_bytes).unwrap_or_default();
        let mut name = names.next().map(property_bytes).unwrap_or_default();
        if let Some(net) = net_names.next() {
            name.extend(property_bytes(net));
        }
        let pid = pids.next().and_then(property_first_u32);
        let dock = kinds.next().is_some_and(|cookie| property_u32s(cookie).contains(&atoms.dock));
        let (Some(geo_c), Some(at_c)) = (geo_c, at_c) else { break };
        let (Ok(geo), Ok(at)) = (geo_c.reply(), at_c.reply()) else { continue };
        if geo.width == 0 || geo.height == 0 {
            continue;
        }
        let rect = (i32::from(at.dst_x), i32::from(at.dst_y), i32::from(at.dst_x) + i32::from(geo.width), i32::from(at.dst_y) + i32::from(geo.height));
        let small = u32::from(geo.width) <= 320 && u32::from(geo.height) <= 96;
        if label_ignored(&class, &name, small) || self_pid.is_some_and(|pid_now| pid == Some(pid_now)) {
            ignore.full.push(rect);
        } else if dock {
            ignore.docks.push(rect);
        }
    }
    ignore
}

fn property_bytes(cookie: x11rb::cookie::Cookie<'_, RustConnection, x11rb::protocol::xproto::GetPropertyReply>) -> Vec<u8> {
    cookie.reply().ok().and_then(|reply| reply.value8().map(|v| v.collect())).unwrap_or_default()
}

fn property_u32s(cookie: x11rb::cookie::Cookie<'_, RustConnection, x11rb::protocol::xproto::GetPropertyReply>) -> Vec<u32> {
    cookie.reply().ok().and_then(|reply| reply.value32().map(|v| v.collect())).unwrap_or_default()
}

fn property_first_u32(cookie: x11rb::cookie::Cookie<'_, RustConnection, x11rb::protocol::xproto::GetPropertyReply>) -> Option<u32> {
    property_u32s(cookie).into_iter().next()
}

fn label_ignored(class: &[u8], name: &[u8], small: bool) -> bool {
    const MARK: &[&str] = &["clock", "nova", "butterfly"];
    if MARK.iter().any(|needle| contains_ci(class, needle)) {
        return true;
    }
    small && contains_ci(name, "clock")
}

fn contains_ci(bytes: &[u8], needle: &str) -> bool {
    let needle = needle.as_bytes();
    bytes.windows(needle.len()).any(|window| window.iter().zip(needle).all(|(b, n)| b.to_ascii_lowercase() == *n))
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
    fn a_glyph_is_content_unless_it_is_a_clock_or_nova() {
        assert_eq!(paint_kind(0, 0), DesktopPulse::Pointer);
        assert_eq!(paint_kind(8, 14), DesktopPulse::Content, "one character");
        assert_eq!(paint_kind(12, 12), DesktopPulse::Content, "a button highlight");
        assert_eq!(paint_kind(1, 1), DesktopPulse::Content, "a caret");
        assert_eq!(pulse_from_samples(0), DesktopPulse::Quiet);
        assert_eq!(pulse_from_samples(1), DesktopPulse::Content);
        let mut ignore = Ignore::default();
        ignore.full.push((0, 0, 80, 40));
        ignore.docks.push((0, 860, 1440, 900));
        assert!(ignore.skips(10, 8, 8, 14), "a paint fully inside the clock window");
        assert!(!ignore.skips(200, 200, 8, 14), "a character in the application");
        assert!(ignore.skips(20, 870, 40, 18), "a small tick inside the panel");
        assert!(!ignore.skips(20, 870, 400, 30), "a large panel redraw still counts");
        assert!(label_ignored(b"clock\0Clock\0", b"", false));
        assert!(label_ignored(b"nova\0Nova\0", b"", false));
        assert!(label_ignored(b"butterfly\0Butterfly\0", b"", false));
        assert!(!label_ignored(b"xterm\0XTerm\0", b"user@host: ~", false));
        assert!(label_ignored(b"panel\0Panel\0", b"Clock", true));
        assert!(!label_ignored(b"firefox\0Firefox\0", b"Clock - page", false), "a large window titled clock is the user's app");
        let outside = x11rb::protocol::xproto::Rectangle { x: 0, y: 0, width: 10, height: 10 };
        assert!(clip_rect(outside, Some((100, 100, 50, 50))).is_none());
        let menu = x11rb::protocol::xproto::Rectangle { x: 90, y: 90, width: 30, height: 20 };
        assert_eq!(clip_rect(menu, Some((100, 100, 50, 50))), Some((100, 100, 20, 10)));
    }
}
