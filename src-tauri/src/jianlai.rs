//! 剑来: native desktop input and screenshot observations. No browser automation.
use enigo::{Axis, Button, Direction, Enigo, Key, Keyboard, Mouse, Settings};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    path::Path,
    sync::Mutex,
    time::{Duration, Instant},
};
use xcap::{Monitor, Window};

mod background;
#[cfg(windows)]
mod text_input;

type Result<T> = std::result::Result<T, String>;
fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}
// ponytail: one desktop, one outstanding observation and a global try-lock; use a desktop broker if independent seats are needed.
static DESKTOP: Mutex<Option<Snapshot>> = Mutex::new(None);
static OBSERVATION_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
// One real screen: chrome tab/focus/navigation changes and jianlai input must never interleave.
// The lease is a plain marker (no MutexGuard held across awaits) so chrome's async path can hold it.
static INPUT_LEASE: Mutex<Option<(&'static str, Instant, u64)>> = Mutex::new(None);
static INPUT_LEASE_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
/// 最近一次注入输入结束的时间；数字员工据此区分自己的注入与用户真实操作。
static LAST_INJECTED: Mutex<Option<Instant>> = Mutex::new(None);
pub(crate) fn input_activity() -> (Option<Instant>, bool) {
    (*LAST_INJECTED.lock().unwrap(), INPUT_LEASE.lock().is_ok_and(|l| l.is_some()))
}
/// 最近一次前台操作桌面的工具调用方。
static DESKTOP_OWNER: Mutex<Option<String>> = Mutex::new(None);
const ESCAPED: &str = "用户按了 Esc 叫停操作电脑：本批剩余动作未执行。不要再操作电脑也不要重试，立即结束本轮。";
/// 光效亮着时用户亲手按 Esc（由光效的键盘钩子调用）：停掉正在用桌面的会话或员工任务。
pub(crate) fn user_escaped() {
    let owner = DESKTOP_OWNER.lock().unwrap().clone();
    crate::employee::user_escaped(owner.as_deref());
}


pub(crate) struct InputLease(u64);
impl Drop for InputLease {
    fn drop(&mut self) {
        if let Ok(mut lease) = INPUT_LEASE.lock() {
            if let Some((tool, _, id)) = *lease {
                if id != self.0 { return; }
                *lease = None;
                *LAST_INJECTED.lock().unwrap() = Some(Instant::now());
                #[cfg(windows)]
                if tool == "jianlai" { crate::agent_glow::release(); }
                let _ = tool;
            }
        }
    }
}
/// Claim the desktop for one tool call. A concurrent claim by the other tool fails fast with a
/// clear reason instead of racing keystrokes against a tab switch; cancellation/unwinding releases the lease through Drop.
pub(crate) fn lease_input(tool: &'static str) -> Result<InputLease> {
    // 用户正在演示剑谱：剑来的输入会混进录制，也会和用户抢鼠标。
    if tool == "jianlai" && crate::jianpu::recording() {
        return Err("剑谱录制中，桌面正由用户演示，本次未执行；等用户停止录制（Alt+R 或 Butterfly 面板）后再调用".into());
    }
    // 用户刚按 Esc 叫停：取消落地前模型的重试也不执行。
    #[cfg(windows)]
    if crate::agent_glow::escaped_since(crate::threads::now_ms() - 5000) {
        return Err(ESCAPED.into());
    }
    let mut lease = INPUT_LEASE.lock().map_err(|_| "输入闸门不可用")?;
    if let Some((holder, since, _)) = *lease {
        // A live call may exceed 30s (16 waits alone can take 32s); only Drop releases it.
        return Err(format!(
            "{holder} 正在操作桌面/浏览器（已 {}ms），本次未执行；等待其结果返回后再调用，不要并行调用 chrome 与 jianlai",
            since.elapsed().as_millis()
        ));
    }
    let id = INPUT_LEASE_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    *lease = Some((tool, Instant::now(), id));
    // 整批输入期间保持桌面光效，即使调用方不是 Nova 会话（看不到回合状态）。
    #[cfg(windows)]
    if tool == "jianlai" { crate::agent_glow::hold(); }
    Ok(InputLease(id))
}
/// Called by chrome after it changed what is on screen (tab switch, navigation, input). The stale
/// desktop snapshot then fails explicitly with this reason and a fresh observation instead of
/// tripping the foreground/pixel guards mid-batch. Returns whether a live snapshot was affected.
pub(crate) fn invalidate_desktop(reason: &str) -> bool {
    match DESKTOP.try_lock() {
        Ok(mut state) => match state.as_mut() {
            Some(snap) if snap.invalidated.is_none() => { snap.invalidated = Some(reason.to_string()); true }
            _ => false,
        },
        Err(_) => false,
    }
}

pub(crate) fn tool_definition() -> Value {
    serde_json::from_str(include_str!("../../scripts/jianlai-tool.json")).unwrap()
}

#[derive(Clone, Debug, PartialEq)]
struct Surface {
    id: u32,
    pid: Option<u32>,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}
#[derive(Clone)]
struct Shot {
    image_id: String,
    surface: Surface,
    pixels: (u32, u32),
    source_pixels: (u32, u32),
    region: Option<Region>,
    guard: Option<crate::visual_guard::VisualGuard>,
    overlay: Option<crate::taskbar_status::Rect>,
    /// Only visible desktop pixels contain the taskbar; PrintWindow/private seats do not.
    desktop_capture: bool,
}
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Region { x: u32, y: u32, width: u32, height: u32 }
impl Region {
    fn validate(self, pixels: (u32, u32)) -> Result<()> {
        if self.width == 0 || self.height == 0
            || self.x.checked_add(self.width).is_none_or(|v| v > pixels.0)
            || self.y.checked_add(self.height).is_none_or(|v| v > pixels.1) {
            return Err("region超出原始窗口截图范围，必须使用originalWidth/originalHeight像素坐标".into());
        }
        Ok(())
    }
}
struct Snapshot {
    id: String,
    owner: String,
    taken: Instant,
    window: Option<u32>,
    max_edge: u32,
    foreground: Option<(u32, u32)>,
    shots: Vec<Shot>,
    invalidated: Option<String>,
    /// 后台截图：act 必须同样带 background，两种模式的快照互不通用。
    background: bool,
    /// What the model actually received per image, so unchanged repeats are not resent.
    delivered: Vec<Delivered>,
}

/// What the model holds for one imageId: a keyframe (last full image) plus at most one live delta.
/// Pixels are stored before the cursor marker.
#[derive(Clone)]
struct Delivered {
    /// frameStream: one per monitor, plus one shared by all window captures — keyed by screen, not app,
    /// so switching to a same-sized window is just another image change.
    stream: String,
    /// frameGroup = snapshotId of the keyframe capture; Lyra keeps each stream's latest group in context.
    group: String,
    keyframe_id: String,
    keyframe: xcap::image::RgbaImage,
    keyframe_path: std::path::PathBuf,
    /// Keyframe with the latest delta applied, i.e. what the model currently believes is on screen.
    seen: xcap::image::RgbaImage,
    seen_path: std::path::PathBuf,
    delta: Option<crate::visual_guard::PixelRect>,
    deltas: u8,
}

/// A group's deltas stay embedded until the next keyframe, so bound how many it can grow.
const MAX_DELTAS: u8 = 8;

enum Delivery { Unchanged, Delta(crate::visual_guard::PixelRect), Keyframe }

/// Unchanged when nothing differs from what the model already sees.
/// Otherwise a delta: the box of all change since the keyframe,
/// including the previous delta's box (its content may have reverted), so the latest delta alone
/// plus the keyframe describes the screen. Over a quarter of the image means a new keyframe.
fn plan_delivery(prior: Option<&Delivered>, pixels: &xcap::image::RgbaImage) -> Delivery {
    let Some(p) = prior.filter(|p| p.seen.dimensions() == pixels.dimensions()) else { return Delivery::Keyframe };
    if changed_tiles(&p.seen, pixels, None).is_some_and(|t| !t.contains(&true)) { return Delivery::Unchanged; }
    if p.deltas >= MAX_DELTAS { return Delivery::Keyframe; }
    let (w, h) = pixels.dimensions();
    let fresh = changed_tiles(&p.keyframe, pixels, None).and_then(|t| changed_box(&t, (w, h)));
    let b = match (fresh, p.delta) {
        (Some(a), Some(b)) => crate::visual_guard::PixelRect { left: a.left.min(b.left), top: a.top.min(b.top),
            right: a.right.max(b.right), bottom: a.bottom.max(b.bottom) },
        (a, b) => match a.or(b) { Some(r) => r, None => return Delivery::Keyframe },
    };
    // ponytail: one bounding box; far-apart changes (e.g. two corners) exceed the cap and become a keyframe.
    if (b.right - b.left) as u64 * (b.bottom - b.top) as u64 * 4 <= w as u64 * h as u64 { Delivery::Delta(b) } else { Delivery::Keyframe }
}

/// Changed tiles' bounding box padded by one tile.
fn changed_box(tiles: &[bool], (w, h): (u32, u32)) -> Option<crate::visual_guard::PixelRect> {
    let cols = w.div_ceil(TILE) as usize;
    let mut changed = tiles.iter().enumerate().filter(|(_, c)| **c).map(|(i, _)| ((i % cols) as u32, (i / cols) as u32));
    let (x, y) = changed.next()?;
    let (l, t, r, b) = changed.fold((x, y, x, y), |(l, t, r, b), (x, y)| (l.min(x), t.min(y), r.max(x), b.max(y)));
    Some(crate::visual_guard::PixelRect { left: l.saturating_sub(1) * TILE, top: t.saturating_sub(1) * TILE,
        right: ((r + 2) * TILE).min(w), bottom: ((b + 2) * TILE).min(h) })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Request {
    operation: String,
    window_id: Option<u32>,
    monitor_id: Option<u32>,
    region: Option<Region>,
    region_space: Option<String>,
    snapshot_id: Option<String>,
    image_id: Option<String>,
    feedback: Option<String>,
    actions: Option<Vec<Action>>,
    max_edge: Option<u32>,
    image_path: Option<String>,
    notes: Option<String>,
    query: Option<String>,
    observe: Option<bool>,
    background: Option<bool>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Action {
    action: String,
    x: Option<i32>,
    y: Option<i32>,
    to_x: Option<i32>,
    to_y: Option<i32>,
    button: Option<String>,
    text: Option<String>,
    key: Option<String>,
    delta: Option<i32>,
    axis: Option<String>,
    ms: Option<u64>,
    glide: Option<bool>,
}

fn window_surface(w: &Window) -> Result<Surface> {
    Ok(Surface {
        id: w.id().map_err(err)?,
        pid: Some(w.pid().map_err(err)?),
        x: w.x().map_err(err)?,
        y: w.y().map_err(err)?,
        width: w.width().map_err(err)?,
        height: w.height().map_err(err)?,
    })
}
fn monitor_surface(m: &Monitor) -> Result<Surface> {
    // XCap exposes Linux monitor geometry in logical coordinates; X11 input uses physical pixels.
    let scale = if cfg!(target_os = "linux") {
        m.scale_factor().map_err(err)? as f64
    } else {
        1.0
    };
    if !scale.is_finite() || scale <= 0.0 {
        return Err("无效屏幕缩放比例".into());
    }
    Ok(Surface {
        id: m.id().map_err(err)?,
        pid: None,
        x: (m.x().map_err(err)? as f64 * scale).round() as i32,
        y: (m.y().map_err(err)? as f64 * scale).round() as i32,
        width: (m.width().map_err(err)? as f64 * scale).round() as u32,
        height: (m.height().map_err(err)? as f64 * scale).round() as u32,
    })
}
fn window(id: u32) -> Result<Window> {
    Window::all()
        .map_err(err)?
        .into_iter()
        .find(|w| w.id().ok() == Some(id))
        .ok_or_else(|| "程序窗口已关闭，请重新 windows".into())
}
/// 窗口的应用名与标题（Windows 上 id 即 HWND）。
pub(crate) fn window_label(id: u32) -> Option<(String, String)> {
    let w = window(id).ok()?;
    Some((w.app_name().ok()?, w.title().unwrap_or_default()))
}
/// 后台模式没有「前台」可比较：恒为 None，使所有前台一致性校验自然成立。
fn foreground() -> Result<Option<(u32, u32)>> {
    if background::active() { return Ok(None); }
    real_foreground()
}
#[cfg(windows)]
fn real_foreground() -> Result<Option<(u32, u32)>> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};
    // Read only: switching windows remains a mouse/keyboard operation.
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.is_null() { return Ok(None); }
    let mut pid = 0;
    if unsafe { GetWindowThreadProcessId(hwnd, &mut pid) } == 0 {
        return Err("无法读取前台窗口身份".into());
    }
    Ok(Some((hwnd as usize as u32, pid)))
}
#[cfg(not(windows))]
fn real_foreground() -> Result<Option<(u32, u32)>> {
    for w in Window::all().map_err(err)? {
        if w.is_focused().map_err(err)? {
            return Ok(Some((w.id().map_err(err)?, w.pid().map_err(err)?)));
        }
    }
    Ok(None)
}
fn windows() -> Result<Value> {
    let mut rows = Vec::new();
    for w in Window::all().map_err(err)? {
        rows.push(
            json!({"windowId":w.id().map_err(err)?,"pid":w.pid().map_err(err)?,
            "monitorId":w.current_monitor().and_then(|m| m.id()).ok(),
            "width":w.width().ok(),"height":w.height().ok(),
            "app":w.app_name().map_err(err)?,"title":w.title().map_err(err)?,
            "focused":w.is_focused().map_err(err)?,"minimized":w.is_minimized().map_err(err)?}),
        );
    }
    Ok(json!({"windows":rows}))
}
/// Lyra 的 owner 就是会话的 `alkaid/tool-results/<session>` 目录：截图落在其下，随会话删除一并清理。
/// 其它客户端（MCP 进程随机 owner）无法对应到会话，落全局目录，由自动清理按修改时间回收。
pub(crate) fn shot_folder(owner: &str, kind: &str) -> std::path::PathBuf {
    use std::hash::{Hash, Hasher};
    let path = std::path::Path::new(owner);
    let name = |p: Option<&std::path::Path>| p.and_then(|p| p.file_name()).and_then(|n| n.to_str()).map(str::to_owned);
    if path.is_absolute()
        && name(Some(path)).is_some_and(|id| crate::valid_lyra_session_id(&id))
        && name(path.parent()).as_deref() == Some("tool-results")
        && name(path.parent().and_then(|p| p.parent())).as_deref() == Some("alkaid")
    {
        return path.join(kind);
    }
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    owner.hash(&mut hash);
    crate::lyra::config::nova_root().join(kind).join(format!("{:016x}", hash.finish()))
}

fn cursor_position() -> Option<(i32, i32)> {
    #[cfg(windows)]
    {
        let mut p = windows_sys::Win32::Foundation::POINT { x:0, y:0 };
        if unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut p) } != 0 {
            return Some((p.x, p.y));
        }
    }
    None
}

fn mark_cursor(image: &mut xcap::image::RgbaImage, surface: &Surface, source: (u32, u32), region: Option<Region>, position: (i32, i32)) -> Option<(u32, u32)> {
    if surface.width == 0 || surface.height == 0 || source.0 == 0 || source.1 == 0 { return None; }
    let r = region.unwrap_or(Region { x:0, y:0, width:source.0, height:source.1 });
    if r.validate(source).is_err() { return None; }
    let x = (position.0 as i128 - surface.x as i128) * source.0 as i128 - r.x as i128 * surface.width as i128;
    let y = (position.1 as i128 - surface.y as i128) * source.1 as i128 - r.y as i128 * surface.height as i128;
    let width = r.width as i128 * surface.width as i128;
    let height = r.height as i128 * surface.height as i128;
    if x < 0 || y < 0 || x >= width || y >= height { return None; }
    let (x, y) = ((x * image.width() as i128 / width) as i32, (y * image.height() as i128 / height) as i32);
    // 与系统指针同款的剑（系统截图本就不含真实指针），剑尖即热点；光晕在反馈图里刻意很淡，
    // 压住的内容仍然可读，不干扰模型对目标的判断。
    let hot = POINTER_TIP as i32;
    for (i, c) in pointer_pixels(32, MARKER_GLOW).into_iter().enumerate() {
        let a = (c >> 24) as f32 / 255.0;
        let (px, py) = (x + i as i32 % 32 - hot, y + i as i32 / 32 - hot);
        if a == 0.0 || px < 0 || py < 0 || px >= image.width() as i32 || py >= image.height() as i32 { continue; }
        let dst = image.get_pixel_mut(px as u32, py as u32);
        for (k, shift) in [16, 8, 0].into_iter().enumerate() {
            dst.0[k] = (((c >> shift) & 255) as f32 * a + dst.0[k] as f32 * (1.0 - a)).round() as u8;
        }
    }
    Some((x as u32, y as u32))
}

/// 剑尖（即热点）在 32 单位指针坐标中的位置，向内缩一点让圆形光晕完整落在图内。
pub(crate) const POINTER_TIP: f32 = 10.0;
/// 以剑尖为圆心的墨点式光晕半径：一个大而明确的落点参照。
const TIP_GLOW: f32 = 9.5;
/// 剑按这个比例绘制，才能与剑尖光晕并存。
const SWORD_SCALE: f32 = 0.82;
/// 真实系统指针上的光晕浓度：一滴天蓝墨点，用户平时就能找到指针（指针从不进入截图）。
pub(crate) const CURSOR_GLOW: f32 = 0.72;
/// 反馈截图里的光晕浓度：只做淡提示，不遮挡模型要读的内容。
pub(crate) const MARKER_GLOW: f32 = 0.2;

/// Signed distance to a polygon (negative inside).
pub(crate) fn polygon_sd(poly: &[(f32, f32)], x: f32, y: f32) -> f32 {
    let (mut inside, mut d) = (false, f32::MAX);
    for i in 0..poly.len() {
        let ((ax, ay), (bx, by)) = (poly[i], poly[(i + 1) % poly.len()]);
        if (ay > y) != (by > y) && x < (bx - ax) * (y - ay) / (by - ay) + ax { inside = !inside; }
        let (ex, ey) = (bx - ax, by - ay);
        let t = (((x - ax) * ex + (y - ay) * ey) / (ex * ex + ey * ey)).clamp(0.0, 1.0);
        d = d.min((x - ax - t * ex).hypot(y - ay - t * ey));
    }
    if inside { -d } else { d }
}

/// 剑来的剑：剑尖朝左上（热点），沿对角线依次是剑身、护手、剑柄、剑首。
/// 坐标 (s, w)：s 从剑尖沿剑身、w 垂直剑身。返回 (有符号距离, 部位颜色)。
fn sword(x: f32, y: f32) -> (f32, (f32, f32, f32)) {
    let (dx, dy) = (x - POINTER_TIP, y - POINTER_TIP);
    let k = std::f32::consts::FRAC_1_SQRT_2 / SWORD_SCALE;
    let (s, w) = ((dx + dy) * k, (dx - dy) * k);
    // 细长的双刃剑身：剑脊一线高光，两侧一亮一暗。
    let blade = if w.abs() < 0.3 { (255.0, 255.0, 255.0) } else if w < 0.0 { (224.0, 242.0, 254.0) } else { (147.0, 197.0, 253.0) };
    // 剑柄缠绳。
    let grip = if (s * 1.6).fract() < 0.5 { (30.0, 64.0, 175.0) } else { (23.0, 37.0, 84.0) };
    [
        (polygon_sd(&[(0.0, 0.0), (5.5, -1.5), (17.0, -1.85), (17.0, 1.85), (5.5, 1.5)], s, w), blade),
        (polygon_sd(&[(16.8, -3.9), (17.8, -4.9), (18.9, -3.9), (18.9, 3.9), (17.8, 4.9), (16.8, 3.9)], s, w), (2.0, 132.0, 199.0)),
        (polygon_sd(&[(18.9, -1.15), (24.0, -1.15), (24.0, 1.15), (18.9, 1.15)], s, w), grip),
        (polygon_sd(&[(23.8, 0.0), (25.2, -1.6), (26.6, 0.0), (25.2, 1.6)], s, w), (14.0, 165.0, 233.0)),
    ].into_iter().min_by(|a, b| a.0.total_cmp(&b.0)).map(|(d, c)| (d * SWORD_SCALE, c)).unwrap()
}

/// Anti-aliased sword pointer: navy outline over a round sky-blue ink dot at the tip.
/// `glow` is the dot's peak alpha. Straight-alpha 0xAARRGGBB.
pub(crate) fn pointer_pixels(size: i32, glow: f32) -> Vec<u32> {
    paint_pointer(size, sword, (12.0, 30.0, 64.0), (56.0, 189.0, 248.0, glow))
}

/// Anti-aliased pointer from a signed-distance `shape` with an outline and a round ink-dot glow
/// (r, g, b, peak alpha) centred on the hotspot. Shared by the sword and the 剑谱 brush.
pub(crate) fn paint_pointer(size: i32, shape: fn(f32, f32) -> (f32, (f32, f32, f32)), outline: (f32, f32, f32), glow: (f32, f32, f32, f32)) -> Vec<u32> {
    const SS: i32 = 4;
    let unit = 32.0 / size as f32;
    let mut pixels = vec![0u32; (size * size) as usize];
    for py in 0..size {
        for px in 0..size {
            let mut acc = [0f32; 4]; // premultiplied r,g,b,a
            for sy in 0..SS {
                for sx in 0..SS {
                    let x = (px as f32 + (sx as f32 + 0.5) / SS as f32) * unit;
                    let y = (py as f32 + (sy as f32 + 0.5) / SS as f32) * unit;
                    let (d, (r, g, b)) = shape(x, y);
                    let (r, g, b, a) = if d <= 0.0 {
                        (r, g, b, 1.0)
                    } else if d < 0.6 {
                        (outline.0, outline.1, outline.2, 1.0)
                    } else {
                        // 以尖端为圆心的墨点，中心浓、向外平滑淡出。
                        let r2 = ((x - POINTER_TIP).powi(2) + (y - POINTER_TIP).powi(2)) / (TIP_GLOW * TIP_GLOW);
                        (glow.0, glow.1, glow.2, glow.3 * (1.0 - r2).max(0.0).powf(1.5))
                    };
                    acc[0] += r * a; acc[1] += g * a; acc[2] += b * a; acc[3] += a;
                }
            }
            let a = acc[3] / (SS * SS) as f32;
            if a <= 0.0 { continue; }
            let c = |v: f32| (v / acc[3]).round().clamp(0.0, 255.0) as u32;
            pixels[(py * size + px) as usize] = ((a * 255.0).round() as u32) << 24 | c(acc[0]) << 16 | c(acc[1]) << 8 | c(acc[2]);
        }
    }
    pixels
}

fn overlay_bounds(desktop_capture: bool) -> Option<crate::taskbar_status::Rect> {
    desktop_capture.then(crate::taskbar_status::bounds).flatten()
}

fn overlay_on_surface(surface: &Surface, bounds: Option<crate::taskbar_status::Rect>) -> Option<crate::taskbar_status::Rect> {
    bounds.filter(|r| (r.left as i64) < surface.x as i64 + surface.width as i64 && r.right as i64 > surface.x as i64
        && (r.top as i64) < surface.y as i64 + surface.height as i64 && r.bottom as i64 > surface.y as i64)
}

fn overlay_region(surface: &Surface, pixels: (u32, u32), bounds: Option<crate::taskbar_status::Rect>) -> Option<crate::visual_guard::PixelRect> {
    let r = bounds?;
    if surface.width == 0 || surface.height == 0 || pixels.0 == 0 || pixels.1 == 0
        || r.left >= r.right || r.top >= r.bottom { return None; }
    let axis = |start: i32, end: i32, origin: i32, size: u32, pixels: u32| {
        let start = (start as i64 - origin as i64).clamp(0, size as i64) as u64 * pixels as u64;
        let end = (end as i64 - origin as i64).clamp(0, size as i64) as u64 * pixels as u64;
        // Round inward: never exempt pixels partly outside the overlay at fractional scales.
        (start.div_ceil(size as u64) as u32, (end / size as u64) as u32)
    };
    let (left, right) = axis(r.left, r.right, surface.x, surface.width, pixels.0);
    let (top, bottom) = axis(r.top, r.bottom, surface.y, surface.height, pixels.1);
    (left < right && top < bottom).then_some(crate::visual_guard::PixelRect { left, top, right, bottom })
}

const TILE: u32 = 32;

/// Which TILE×TILE blocks visibly changed (any channel > 16); None when frames are not comparable.
fn changed_tiles(a: &xcap::image::RgbaImage, b: &xcap::image::RgbaImage, exclude: Option<crate::visual_guard::PixelRect>) -> Option<Vec<bool>> {
    let (w, h) = a.dimensions();
    if b.dimensions() != (w, h) || w == 0 || h == 0 { return None; }
    let cols = w.div_ceil(TILE) as usize;
    let mut tiles = vec![false; cols * h.div_ceil(TILE) as usize];
    let (ra, rb) = (a.as_raw(), b.as_raw());
    // memcmp per frame and per tile row first; the per-pixel scan is slow in unoptimized NovaDev builds.
    if ra == rb { return Some(tiles); }
    for y in 0..h as usize { for col in 0..cols {
        let t = y / TILE as usize * cols + col;
        let x0 = col * TILE as usize;
        let span = (y * w as usize + x0) * 4..(y * w as usize + (x0 + TILE as usize).min(w as usize)) * 4;
        if tiles[t] || ra[span.clone()] == rb[span.clone()] { continue; }
        tiles[t] = ra[span.clone()].chunks_exact(4).zip(rb[span].chunks_exact(4)).enumerate().any(|(i, (p, q))| {
            (0..3).any(|c| p[c].abs_diff(q[c]) > 16)
                && !exclude.is_some_and(|r| r.contains((x0 + i) as f64, y as f64))
        });
    } }
    Some(tiles)
}

/// Capture once; animations and loading indicators are for the model to interpret, not a wait gate.
fn sample_image(grab: impl FnOnce() -> Result<xcap::image::RgbaImage>, desktop_surface: Option<&Surface>)
    -> Result<(xcap::image::RgbaImage, Value, Option<crate::taskbar_status::Rect>)> {
    let started = Instant::now();
    let relevant_overlay = || desktop_surface.and_then(|s| overlay_on_surface(s, overlay_bounds(true)));
    let overlay = relevant_overlay();
    let image = grab()?;
    if overlay != relevant_overlay() {
        return Err("取样期间任务栏状态条范围改变，请重新截图".into());
    }
    let report = json!({"status":"not_checked","samples":1,"elapsedMs":started.elapsed().as_millis()});
    Ok((image, report, overlay))
}

fn capture(owner: &str, window_id: Option<u32>, monitor_id: Option<u32>, region: Option<Region>, max_edge: u32, state: &mut Option<Snapshot>) -> Result<Value> {
    let _activity = crate::taskbar_status::activity("截取画面");
    let started = Instant::now();
    let mut capture_ms = 0;
    let mut resize_ms = 0;
    let mut encode_ms = 0;
    let bg = background::active();
    // Only this session's recent, same-mode observation counts as something the model has already seen.
    let previous = state.take().filter(|s| s.owner == owner && s.background == bg && s.taken.elapsed() <= Duration::from_secs(180))
        .map(|s| s.delivered).unwrap_or_default();
    let mut delivered: Vec<Delivered> = Vec::new();
    // ponytail: keyframe/delta only for a single full image; multi-monitor and region shots are always
    // sent whole and ungrouped (Lyra keeps them by recency).

    if bg { background::check_scope(window_id.is_some())?; }
    let id = uuid::Uuid::new_v4().to_string();
    let sequence = OBSERVATION_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let folder = shot_folder(owner, "desktop-shots");
    std::fs::create_dir_all(&folder).map_err(err)?;
    let before = foreground()?;
    let cursor_before = cursor_position();
    let mut shots = Vec::new();
    let mut images = Vec::new();
    let mut save = |surface: Surface, image: xcap::image::RgbaImage, stability: Value, overlay, desktop_capture| -> Result<()> {
        let captured_at = chrono::Utc::now().to_rfc3339();
        if surface.width == 0 || surface.height == 0 || image.width() == 0 || image.height() == 0 {
            return Err("截图范围为空".into());
        }
        let mut image_id = format!(
            "{}-{}",
            if window_id.is_some() {
                "window"
            } else {
                "monitor"
            },
            surface.id
        );
        if let Some(r) = region {
            image_id.push_str(&format!("-region-{}-{}-{}-{}", r.x, r.y, r.width, r.height));
        }
        let original_pixels = image.dimensions();
        // Retain bounded unannotated pixel samples, never the cursor ring or a guessed target.
        let guard = Some(crate::visual_guard::VisualGuard::new(&image));
        let image = if let Some(r) = region {
            r.validate(original_pixels)?;
            xcap::image::imageops::crop_imm(&image, r.x, r.y, r.width, r.height).to_image()
        } else { image };
        let resize_started = Instant::now();
        let mut image = if max_edge > 0 && (region.is_some() || image.width().max(image.height()) > max_edge) {
            let scale = max_edge as f64 / image.width().max(image.height()) as f64;
            let (width, height) = image.dimensions();
            xcap::image::DynamicImage::ImageRgba8(image).resize_exact(
                (width as f64 * scale).round().max(1.0) as u32,
                (height as f64 * scale).round().max(1.0) as u32,
                xcap::image::imageops::FilterType::Triangle).into_rgba8()
        } else { image };
        resize_ms += resize_started.elapsed().as_millis();
        let plain = image.clone();
        let cursor = cursor_before.filter(|p| Some(*p) == cursor_position())
            .filter(|_| window_id.is_none() || before.map(|f| f.0) == window_id)
            .and_then(|p| mark_cursor(&mut image, &surface, original_pixels, region, p));
        let encode_started = Instant::now();
        let mut entry = json!({"imageId":image_id,"width":image.width(),"height":image.height(),
            "pixelWidth":image.width(),"pixelHeight":image.height(),
            "snapshotId":id,"capturedAt":captured_at,"stability":stability,
            "originalWidth":original_pixels.0,"originalHeight":original_pixels.1,
            "cursor":cursor.map(|(x,y)| json!({"x":x,"y":y,"marker":"sword-tip","source":"system-pointer"})),
            "region":region.map(|r| json!({"x":r.x,"y":r.y,"width":r.width,"height":r.height})),
            "desktopBounds":{"x":surface.x,"y":surface.y,"width":surface.width,"height":surface.height}});
        // The act snapshot always maps the full image: keyframe coordinates stay valid, delta coordinates
        // are offset by the model; the pre-click pixel guard still checks the real current screen.
        // Region zooms are sent whole and ungrouped.
        let stream = region.is_none().then(|| if window_id.is_some() { "window".to_string() } else { image_id.clone() });
        let prior = previous.iter().find(|p| Some(&p.stream) == stream.as_ref());
        let mark = |entry: &mut Value, p: &Delivered| {
            entry["frameStream"] = json!(p.stream);
            entry["frameGroup"] = json!(p.group);
            entry["keyframeImageId"] = json!(p.keyframe_id);
            entry["keyframePath"] = json!(p.keyframe_path);
        };
        let mut delta_entry = None;
        match plan_delivery(prior, &plain) {
            Delivery::Unchanged => {
                let p = prior.unwrap();
                // No path means hosts attach no image; samePixelsAs stays readable through recall.
                entry["unchanged"] = json!(true);
                entry["samePixelsAs"] = json!(p.seen_path);
                mark(&mut entry, p);
                delivered.push(p.clone());
            }
            Delivery::Delta(b) => {
                let p = prior.unwrap();
                let crop = xcap::image::imageops::crop_imm(&image, b.left, b.top, b.right - b.left, b.bottom - b.top).to_image();
                let crop_path = folder.join(format!("{id}-{image_id}-delta.jpg"));
                std::fs::write(&crop_path, crate::visual_guard::encode_jpeg(&crop)?).map_err(err)?;
                mark(&mut entry, p);
                let mut delta = json!({"deltaOf":image_id,"path":crop_path,"x":b.left,"y":b.top,
                    "width":crop.width(),"height":crop.height(),"pixelWidth":crop.width(),"pixelHeight":crop.height(),
                    "cursor":cursor.filter(|&(x, y)| b.contains(x as f64, y as f64))
                        .map(|(x, y)| json!({"x":x - b.left,"y":y - b.top,"marker":"sword-tip","source":"system-pointer"}))});
                mark(&mut delta, p);
                delta_entry = Some(delta);
                delivered.push(Delivered { seen: plain, seen_path: crop_path, delta: Some(b), deltas: p.deltas + 1, ..p.clone() });
            }
            Delivery::Keyframe => {
                let path = folder.join(format!("{id}-{image_id}.jpg"));
                std::fs::write(&path, crate::visual_guard::encode_jpeg(&image)?).map_err(err)?;
                entry["path"] = json!(path);
                if let Some(stream) = stream {
                    let p = Delivered { stream, group: id.clone(), keyframe_id: image_id.clone(), keyframe: plain.clone(),
                        keyframe_path: path.clone(), seen: plain, seen_path: path, delta: None, deltas: 0 };
                    entry["keyframe"] = json!(true);
                    mark(&mut entry, &p);
                    delivered.push(p);
                }
            }
        }
        encode_ms += encode_started.elapsed().as_millis();
        images.push(entry);
        images.extend(delta_entry);
        shots.push(Shot {
            image_id,
            surface,
            pixels: image.dimensions(),
            source_pixels: original_pixels,
            region,
            guard,
            overlay,
            desktop_capture,
        });
        Ok(())
    };
    if let Some(wid) = window_id {
        let w = window(wid)?;
        if w.is_minimized().map_err(err)? {
            return Err("窗口已最小化，请先通过桌面恢复窗口".into());
        }
        let surface = window_surface(&w)?;
        let capture_started = Instant::now();
        // PrintWindow omits separate popup windows (e.g. recipient suggestions).
        // Foreground input must observe the pixels that will actually receive it.
        let desktop_capture = !bg && before == Some((surface.id, surface.pid.unwrap()));
        let (image, stability, overlay) = sample_image(|| {
            if foreground()? != before || window_surface(&w)? != surface {
                return Err("取样期间窗口或焦点改变，请重新截图".into());
            }
            #[cfg(windows)]
            let image = if before == Some((surface.id, surface.pid.unwrap())) {
                let monitor = w.current_monitor().map_err(err)?;
                let bounds = monitor_surface(&monitor)?;
                let image = monitor.capture_image().map_err(err)?;
                visible_window_crop(&surface, &bounds, &image)?
            } else { w.capture_image().map_err(err)? };
            #[cfg(not(windows))]
            let image = w.capture_image().map_err(err)?;
            Ok(image)
        }, desktop_capture.then_some(&surface))?;
        capture_ms += capture_started.elapsed().as_millis();
        if window_surface(&w)? != surface {
            return Err("截图期间窗口移动，请重新截图".into());
        }
        save(surface, image, stability, overlay, desktop_capture)?;
    } else if bg {
        // Linux 座席：像素来自私有 X 服务器；xcap 只读进程的 DISPLAY，不能用于它。
        let surface = background::seat_surface()?;
        let capture_started = Instant::now();
        let (image, stability, overlay) = sample_image(background::seat_grab, None)?;
        capture_ms += capture_started.elapsed().as_millis();
        save(surface, image, stability, overlay, false)?;
    } else {
        let monitors = Monitor::all().map_err(err)?;
        if monitors.is_empty() || monitors.len() > 16 {
            return Err("需要1至16个可截图显示器".into());
        }
        for m in monitors {
            let surface = monitor_surface(&m)?;
            if monitor_id.is_some_and(|id| id != surface.id) { continue; }
            let capture_started = Instant::now();
            let (image, stability, overlay) = sample_image(|| {
                if foreground()? != before || monitor_surface(&m)? != surface {
                    return Err("取样期间屏幕或焦点改变，请重新截图".into());
                }
                m.capture_image().map_err(err)
            }, Some(&surface))?;
            capture_ms += capture_started.elapsed().as_millis();
            save(surface, image, stability, overlay, true)?;
        }
        if shots.is_empty() { return Err("显示器不存在，请重新截图".into()); }
    }
    if foreground()? != before {
        return Err("截图期间焦点改变，请重新截图".into());
    }
    // Other images' keyframes stay usable when this capture targets something else (e.g. a zoom).
    for p in previous { if !delivered.iter().any(|d| d.stream == p.stream) { delivered.push(p); } }
    *state = Some(Snapshot {
        id: id.clone(),
        owner: owner.into(),
        taken: Instant::now(),
        window: window_id,
        max_edge,
        foreground: before,
        shots,
        invalidated: None,
        background: bg,
        delivered,
    });
    let unchanged: Vec<&str> = images.iter().filter(|i| i["unchanged"] == true).filter_map(|i| i["imageId"].as_str()).collect();
    let mut result = json!({"snapshotId":id,"observationSequence":sequence,"historical":false,"windowId":window_id,"images":images,"coordinateSpace":"image-pixels","expiresInMs":180000,"background":bg,
            "foreground":before,"maxEdge":max_edge,
            "timingsMs":{"capture":capture_ms,"resize":resize_ms,"encodeAndSave":encode_ms,
                "other":started.elapsed().as_millis().saturating_sub(capture_ms + resize_ms + encode_ms),
                "total":started.elapsed().as_millis()}});
    let deltas: Vec<String> = images.iter().filter_map(|i| Some(format!("{} 的增量图对应关键帧(当时imageId={})的({},{})起 {}×{}",
        i["deltaOf"].as_str()?, i["keyframeImageId"].as_str()?, i["x"], i["y"], i["width"], i["height"]))).collect();
    if !deltas.is_empty() {
        result["deltaNotice"] = json!(format!("画面局部变化，只发了增量图：{}。增量图与关键帧同比例，是该区域的最新内容并取代之前的增量；区域外与关键帧（同一屏幕或窗口流最近的完整图）一致。点击一律用本次snapshotId和deltaOf的imageId：区域外直接用关键帧坐标，区域内坐标=增量图内坐标+起点偏移。", deltas.join("；")));
    }
    if !unchanged.is_empty() {
        result["unchangedNotice"] = json!(format!("{} 与你已看到的画面（关键帧及最新增量）无明显像素差异（剑形指针位置以cursor字段为准），未重复发送图片：直接沿用已看到的画面判断，坐标对本次snapshotId/imageId同样有效。动作后出现此提示只表示本次取图尚无可见变化，不代表动作失败；若目标仍在加载可显式wait后观察，不重放输入或循环截图；上一张已不在上下文时用recall(imagePath=samePixelsAs)回看。", unchanged.join("、")));
    }
    Ok(result)
}

#[cfg(windows)]
fn visible_window_crop(window: &Surface, monitor: &Surface, image: &xcap::image::RgbaImage) -> Result<xcap::image::RgbaImage> {
    let x = window.x as i64 - monitor.x as i64;
    let y = window.y as i64 - monitor.y as i64;
    if image.dimensions() != (monitor.width, monitor.height)
        || x < 0 || y < 0 || window.width == 0 || window.height == 0
        || x + window.width as i64 > monitor.width as i64
        || y + window.height as i64 > monitor.height as i64 {
        return Err("窗口跨屏、部分离屏或屏幕像素比例不一致，请使用monitorId截图定位".into());
    }
    Ok(xcap::image::imageops::crop_imm(image, x as u32, y as u32, window.width, window.height).to_image())
}

fn action_shot<'a>(shots: &'a [Shot], image_id: Option<&str>) -> Result<&'a Shot> {
    if let Some(id) = image_id {
        if let Some(shot) = shots.iter().find(|s| s.image_id == id) { return Ok(shot); }
    } else if let [only] = shots {
        // snapshotId already pins this single image; demanding imageId only cost a retry.
        return Ok(only);
    }
    Err(format!("imageId缺失或不属于此截图；可用imageId：{}。沿用当前snapshotId并修正imageId，无需重新截图；裁剪图坐标从(0,0)开始，不加region偏移", shots.iter().map(|s| s.image_id.as_str()).collect::<Vec<_>>().join(", ")))
}

fn point(shot: &Shot, x: Option<i32>, y: Option<i32>) -> Result<(i32, i32)> {
    let (x, y) = (x.ok_or("缺少x坐标")?, y.ok_or("缺少y坐标")?);
    let (pw, ph) = shot.pixels;
    if x < 0 || y < 0 || x as u32 >= pw || y as u32 >= ph || pw == 0 || ph == 0
        || shot.source_pixels.0 == 0 || shot.source_pixels.1 == 0 {
        return Err("坐标超出原始截图范围".into());
    }
    let s = &shot.surface;
    let r = shot.region.unwrap_or(Region { x:0, y:0, width:shot.source_pixels.0, height:shot.source_pixels.1 });
    let px = s.x as i128 + (r.x as i128 * pw as i128 + x as i128 * r.width as i128)
        * s.width as i128 / (pw as i128 * shot.source_pixels.0 as i128);
    let py = s.y as i128 + (r.y as i128 * ph as i128 + y as i128 * r.height as i128)
        * s.height as i128 / (ph as i128 * shot.source_pixels.1 as i128);
    Ok((
        i32::try_from(px).map_err(err)?,
        i32::try_from(py).map_err(err)?,
    ))
}
fn keys(raw: &str) -> Result<Vec<Key>> {
    let keys = raw
        .split('+')
        .map(|s| match s.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => Ok(Key::Control),
            "alt" | "option" => Ok(Key::Alt),
            "shift" => Ok(Key::Shift),
            "meta" | "super" | "cmd" | "win" => Ok(Key::Meta),
            "enter" | "return" => Ok(Key::Return),
            "tab" => Ok(Key::Tab),
            "escape" | "esc" => Ok(Key::Escape),
            "space" => Ok(Key::Space),
            "backspace" => Ok(Key::Backspace),
            "delete" => Ok(Key::Delete),
            "left" | "arrowleft" => Ok(Key::LeftArrow),
            "right" | "arrowright" => Ok(Key::RightArrow),
            "up" | "arrowup" => Ok(Key::UpArrow),
            "down" | "arrowdown" => Ok(Key::DownArrow),
            "home" => Ok(Key::Home),
            "end" => Ok(Key::End),
            "pageup" => Ok(Key::PageUp),
            "pagedown" => Ok(Key::PageDown),
            "f1" => Ok(Key::F1),
            "f2" => Ok(Key::F2),
            "f3" => Ok(Key::F3),
            "f4" => Ok(Key::F4),
            "f5" => Ok(Key::F5),
            "f6" => Ok(Key::F6),
            "f7" => Ok(Key::F7),
            "f8" => Ok(Key::F8),
            "f9" => Ok(Key::F9),
            "f10" => Ok(Key::F10),
            "f11" => Ok(Key::F11),
            "f12" => Ok(Key::F12),
            // Enigo 0.6 passes VkKeyScanExW's shift-state high byte through as VK.
            // Shortcuts use physical letter/digit VKs; Windows text prefers paste with a clipboard-free fallback.
            #[cfg(windows)]
            _ if s.len() == 1 && s.as_bytes()[0].is_ascii_alphanumeric() =>
                Ok(Key::Other(s.as_bytes()[0].to_ascii_uppercase() as u32)),
            _ if s.chars().count() == 1 => Ok(Key::Unicode(s.chars().next().unwrap())),
            _ => Err(format!("不支持的按键：{s}")),
        })
        .collect::<Result<Vec<_>>>()?;
    if keys.len() > 4 {
        return Err("组合键最多4键".into());
    }
    Ok(keys)
}
fn validate(a: &Action, shot: Option<&Shot>) -> Result<()> {
    if !matches!(
        a.action.as_str(),
        "click" | "double_click" | "move" | "drag" | "type" | "press" | "scroll" | "wait"
    ) {
        return Err("未知剑来动作；可用：click/double_click/move/drag/type/press/scroll/wait，按键用press+key".into());
    }
    if a.button
        .as_deref()
        .is_some_and(|b| !matches!(b, "left" | "right" | "middle"))
        || a.axis
            .as_deref()
            .is_some_and(|v| !matches!(v, "vertical" | "horizontal"))
    {
        return Err("button仅允许left/right/middle；axis仅允许vertical/horizontal".into());
    }
    if a.glide.is_some() && !matches!(a.action.as_str(), "click" | "double_click" | "move" | "drag") {
        return Err("glide仅用于click/double_click/move/drag".into());
    }
    if a.ms.unwrap_or(0) > 2000 {
        return Err(format!("ms={}，允许范围为 0–2000", a.ms.unwrap()));
    }
    if matches!(
        a.action.as_str(),
        "click" | "double_click" | "move" | "drag" | "scroll"
    ) {
        point(shot.ok_or("坐标动作需要截图")?, a.x, a.y)?;
    }
    if a.action == "drag" {
        point(shot.ok_or("坐标动作需要截图")?, a.to_x, a.to_y)?;
    }
    if a.action == "press" {
        keys(a.key.as_deref().ok_or("press 缺少key")?)?;
    }
    if a.action == "type" {
        let text = a.text.as_deref().ok_or("type 缺少text")?;
        if text.chars().count() > 10000 || text.contains('\0') {
            return Err("文本过长或含NUL".into());
        }
    }
    if a.action == "scroll" && !(-100..=100).contains(&a.delta.ok_or("scroll 缺少delta")?) {
        return Err("滚动刻度必须在-100至100之间".into());
    }
    Ok(())
}
fn source_point(shot: &Shot, x: i32, y: i32) -> (f64, f64) {
    let r=shot.region.unwrap_or(Region{x:0,y:0,width:shot.source_pixels.0,height:shot.source_pixels.1});
    (r.x as f64+x as f64*r.width as f64/shot.pixels.0 as f64,
     r.y as f64+y as f64*r.height as f64/shot.pixels.1 as f64)
}
fn image_region(shot: &Shot, region: Region) -> Result<Region> {
    region.validate(shot.pixels)?;
    let (x,y)=source_point(shot,region.x as i32,region.y as i32);
    let (right,bottom)=source_point(shot,(region.x+region.width) as i32,(region.y+region.height) as i32);
    let crop=Region{x:x.floor() as u32,y:y.floor() as u32,width:right.ceil() as u32-x.floor() as u32,height:bottom.ceil() as u32-y.floor() as u32};
    crop.validate(shot.source_pixels)?;
    Ok(crop)
}
fn check_overlay(shot: &Shot, action: &Action, current: Option<crate::taskbar_status::Rect>) -> Result<Option<crate::visual_guard::PixelRect>> {
    let current = overlay_on_surface(&shot.surface, current);
    if guarded_pointer(action) {
        let start = point(shot, action.x, action.y)?;
        let end = if action.action == "drag" { point(shot, action.to_x, action.to_y)? } else { start };
        if [shot.overlay, current].into_iter().flatten().any(|r|
            [start, end].iter().any(|&(x,y)| x >= r.left && x < r.right && y >= r.top && y < r.bottom)) {
            return Err("工具不能点击或拖动任务栏状态提示，请选择提示区域之外的目标".into());
        }
    }
    // A moved/hidden/new strip gets no exemption, but must not invalidate an unrelated
    // app or monitor. Normal target-pixel checks still detect any change near the input.
    Ok((shot.overlay == current).then(|| overlay_region(&shot.surface, shot.source_pixels, current)).flatten())
}

// after_input: earlier actions of this batch already changed the screen, so only the target's core is compared.
fn guard_target(shot:&Shot, action:&Action, image:&xcap::image::RgbaImage, after_input:bool, overlay:Option<crate::taskbar_status::Rect>) -> Result<()> {
    let exclude = check_overlay(shot, action, overlay)?;
    let Some(guard)=&shot.guard else {return Ok(());};
    let mut points=vec![source_point(shot,action.x.unwrap(),action.y.unwrap())];
    if action.action=="drag" {points.push(source_point(shot,action.to_x.unwrap(),action.to_y.unwrap()));}
    if points.iter().any(|&(x,y)|guard.changed_near_excluding(image,x,y,after_input,exclude)) {
        return Err("落点附近画面已变化，停止旧坐标输入；请用返回的新图重新定位，不要原样重试同一坐标，细小目标使用 regionSpace=image 局部截图".into());
    }
    Ok(())
}
// Scroll is excluded: a batch's earlier scroll always changes pixels under the same point, and scrolling is reversible.
fn guarded_pointer(action:&Action) -> bool {matches!(action.action.as_str(),"click"|"double_click"|"drag")}
fn pointer_matches(expected:(i32,i32),actual:(i32,i32)) -> bool {
    // Allow native rounding (e.g. a late SendInput normalization), not a wrong monitor or DPI mapping.
    (expected.0 as i64-actual.0 as i64).abs()<=2 && (expected.1 as i64-actual.1 as i64).abs()<=2
}
fn check_target(snap: &Snapshot, shot: &Shot, a: &Action, after_input: bool) -> Result<()> {
    if snap.taken.elapsed() > Duration::from_secs(180) {
        return Err("截图已过期".into());
    }
    if foreground()? != snap.foreground {
        return Err(match snap.window {
            Some(id) => format!("前台程序已改变；目标窗口仍需操作时直接 activate(windowId={id})，它会返回新图，无需先重新截图"),
            None => "前台程序已改变，请重新截图".into(),
        });
    }
    let bg = background::active();
    let overlay = overlay_on_surface(&shot.surface, overlay_bounds(shot.desktop_capture));
    check_overlay(shot, a, overlay)?;
    if let Some(id) = snap.window {
        // Reuse one Z-order enumeration for geometry and occlusion checks.
        let windows = Window::all().map_err(err)?;
        let w = windows.iter().find(|w| w.id().ok() == Some(id))
            .ok_or("程序窗口已关闭，请重新 windows")?;
        // 后台输入投递给进程，不依赖焦点，也不受遮挡影响；只要求窗口还在原处。
        if bg {
            if w.is_minimized().map_err(err)? || window_surface(w)? != shot.surface {
                return Err("后台窗口已最小化、移动或尺寸改变，请重新截图".into());
            }
        } else if !w.is_focused().map_err(err)?
            || w.is_minimized().map_err(err)?
            || window_surface(w)? != shot.surface
        {
            return Err(format!("目标窗口失焦、移动或尺寸改变；失焦时直接 activate(windowId={id}) 切到前台并返回新图，移动或尺寸改变时重新截图"));
        }
        // Only own-pixel window captures (PrintWindow/macOS/Linux) can show occluded content. A screen-crop shot
        // shows exactly what is on top (emoji picker, IME, other apps' popups), and the visual guard below
        // rejects anything that appeared after the shot, so window enumeration would only add false positives.
        if !bg && !shot.desktop_capture && a.x.is_some() {
            let p = point(shot, a.x, a.y)?;
            let end = if a.action == "drag" {
                point(shot, a.to_x, a.to_y)?
            } else {
                p
            };
            for other in &windows {
                if other.id().map_err(err)? == id {
                    break;
                }
                if other.is_minimized().map_err(err)? {
                    continue;
                }
                let r = window_surface(other)?;
                if [p, end].iter().any(|&(x, y)| {
                    x as i64 >= r.x as i64
                        && y as i64 >= r.y as i64
                        && (x as i64) < r.x as i64 + r.width as i64
                        && (y as i64) < r.y as i64 + r.height as i64
                }) {
                    // Name the occluder: without it a false positive (overlay, floating toolbar) is undiagnosable.
                    return Err(format!("目标坐标被其它窗口遮挡（{} 「{}」 windowId={}），请使用桌面截图",
                        other.app_name().unwrap_or_default(), other.title().unwrap_or_default(), r.id));
                }
            }
        }
        if guarded_pointer(a) && shot.guard.is_some() {
            #[cfg(windows)]
            let image = {
                let monitor=w.current_monitor().map_err(err)?;
                visible_window_crop(&shot.surface,&monitor_surface(&monitor)?,&monitor.capture_image().map_err(err)?)?
            };
            #[cfg(not(windows))]
            let image=w.capture_image().map_err(err)?;
            guard_target(shot,a,&image,after_input,overlay)?;
            if foreground()?!=snap.foreground || window_surface(w)?!=shot.surface {return Err("落点校验期间窗口或焦点改变，请重新观察".into());}
        }
    } else if bg {
        let seat = background::seat_surface()?;
        if snap.shots.iter().any(|s| s.surface != seat) {
            return Err("座席已重启或尺寸改变，请重新截图".into());
        }
        if guarded_pointer(a) && shot.guard.is_some() {
            guard_target(shot, a, &background::seat_grab()?, after_input, overlay)?;
        }
    } else {
        let monitors = Monitor::all().map_err(err)?;
        let current = monitors
            .iter()
            .map(monitor_surface)
            .collect::<Result<Vec<_>>>()?;
        if snap.shots.iter().any(|s| !current.contains(&s.surface))
        {
            return Err("显示器布局已改变，请重新截图".into());
        }
        if guarded_pointer(a) && shot.guard.is_some() {
            let monitor=monitors.iter().find(|m|m.id().ok()==Some(shot.surface.id)).ok_or("显示器已改变")?;
            guard_target(shot,a,&monitor.capture_image().map_err(err)?,after_input,overlay)?;
            if foreground()?!=snap.foreground || monitor_surface(monitor)?!=shot.surface {return Err("落点校验期间屏幕或焦点改变，请重新观察".into());}
        }
    }
    if overlay != overlay_on_surface(&shot.surface, overlay_bounds(shot.desktop_capture)) { return Err("落点校验期间任务栏状态条范围改变，请重新截图".into()); }
    Ok(())
}

fn action_delay(actions: &[Action], index: usize) -> Duration {
    // Explicit waits already provide reaction time; pointer motion needs no extra delay.
    let redundant = matches!(actions[index].action.as_str(), "wait" | "move")
        || actions.get(index + 1).is_some_and(|a| a.action == "wait");
    // ponytail: a fixed 300ms reaction pause for rendering/clipboard writes, not a readiness guarantee.
    // Slower apps need an explicit wait; animations never extend this pause.
    let reacts = index + 1 == actions.len() || matches!(actions[index].action.as_str(), "click" | "double_click" | "type" | "scroll");
    Duration::from_millis(if redundant { 0 } else if reacts { 300 } else { 80 })
}

fn scroll_frame_index(actions: &[Action], index: usize, feedback: Option<&str>) -> Option<usize> {
    if feedback == Some("none") || actions.get(index + 1).is_some_and(|a| a.action == "wait")
        || !actions[index + 1..].iter().any(|a| a.action != "wait") { return None; }
    // Honour explicit waits before the intermediate frame, but label it with the originating scroll.
    actions[..=index].iter().rposition(|a| a.action != "wait").filter(|&i| actions[i].action == "scroll")
}
/// Desktop pixel -> SendInput VIRTUALDESK normalized value; ceil so floor(n*size/65536) lands on the pixel.
#[cfg_attr(not(windows), allow(dead_code))]
fn virtual_desk_coordinate(p: i32, origin: i32, size: i32) -> i32 {
    if size <= 0 { return 0; }
    ((p as i64 - origin as i64) * 65536 + size as i64 - 1).div_euclid(size as i64).clamp(0, 65535) as i32
}
/// `input_event`: glide/drag path points must be real input (hover, drag thresholds, hooks, raw input).
fn move_pointer(_enigo: &mut Enigo, x: i32, y: i32, input_event: bool) -> Result<()> {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::UI::{Input::KeyboardAndMouse::*, WindowsAndMessaging::*};
        if input_event {
            // Enigo 0.6 normalizes to the primary display only (no VIRTUALDESK), which misses other monitors.
            let mut input = INPUT { r#type: INPUT_MOUSE, ..Default::default() };
            input.Anonymous.mi = MOUSEINPUT {
                dx: virtual_desk_coordinate(x, GetSystemMetrics(SM_XVIRTUALSCREEN), GetSystemMetrics(SM_CXVIRTUALSCREEN)),
                dy: virtual_desk_coordinate(y, GetSystemMetrics(SM_YVIRTUALSCREEN), GetSystemMetrics(SM_CYVIRTUALSCREEN)),
                dwFlags: MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                ..Default::default()
            };
            if SendInput(1, &input, size_of::<INPUT>() as i32) != 1 {
                return Err(err(std::io::Error::last_os_error()));
            }
        }
        // Exact landing in desktop coordinates (incl. negative monitors), immune to normalization rounding.
        if SetCursorPos(x, y) == 0 {
            return Err(err(std::io::Error::last_os_error()));
        }
        Ok(())
    }
    #[cfg(not(windows))]
    _enigo.move_mouse(x, y, enigo::Coordinate::Abs).map_err(err)
}
fn click_button(enigo: &mut Enigo, button: Button) -> Result<()> {
    // Keep the native down/up pair together on the successful fast path.
    // A failed reply can still mean partial input: release, but never replay.
    if let Err(error) = enigo.button(button, Direction::Click) {
        let _ = enigo.button(button, Direction::Release);
        return Err(err(error));
    }
    Ok(())
}
/// Opt-in human-like pointer motion: duration grows with distance (Fitts), minimum-jerk easing, slight arc.
fn glide_ms(from: (i32, i32), to: (i32, i32)) -> f64 {
    let d = ((to.0 - from.0) as f64).hypot((to.1 - from.1) as f64);
    (80.0 + 60.0 * (1.0 + d / 24.0).log2()).min(400.0)
}
fn glide_at(from: (i32, i32), to: (i32, i32), bend: f64, t: f64) -> (i32, i32) {
    let (fx, fy, dx, dy) = (from.0 as f64, from.1 as f64, (to.0 - from.0) as f64, (to.1 - from.1) as f64);
    // Quadratic Bezier whose control point sits `bend`·distance off the midpoint, perpendicular to the line.
    let (cx, cy) = (fx + dx / 2.0 - dy * bend, fy + dy / 2.0 + dx * bend);
    let s = t * t * t * (10.0 - 15.0 * t + 6.0 * t * t);
    let u = 1.0 - s;
    (
        (u * u * fx + 2.0 * u * s * cx + s * s * to.0 as f64).round() as i32,
        (u * u * fy + 2.0 * u * s * cy + s * s * to.1 as f64).round() as i32,
    )
}
#[cfg(windows)]
fn bring_to_front(id: u32) {
    use windows_sys::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
    use windows_sys::Win32::UI::WindowsAndMessaging::{BringWindowToTop, GetForegroundWindow, GetWindowThreadProcessId, IsIconic, ShowWindow, SetForegroundWindow, SW_RESTORE};
    let hwnd = id as usize as windows_sys::Win32::Foundation::HWND;
    unsafe {
        if IsIconic(hwnd) != 0 { ShowWindow(hwnd, SW_RESTORE); }
        if SetForegroundWindow(hwnd) == 0 {
            // Unattended runs start with another app in front; the foreground lock refuses background callers.
            // Sharing the foreground thread's input queue lifts it without injecting keys.
            let fg = GetWindowThreadProcessId(GetForegroundWindow(), std::ptr::null_mut());
            let me = GetCurrentThreadId();
            if fg != 0 && fg != me && AttachThreadInput(me, fg, 1) != 0 {
                BringWindowToTop(hwnd);
                SetForegroundWindow(hwnd);
                AttachThreadInput(me, fg, 0);
            }
        }
    }
}
/// 输入途中前台被通知、弹窗等其他窗口抢走：切回原前台继续，不算中断（只有用户按 Esc 才中断，由 Esc 监听直接停止）。
/// 切不回来时才停，因为此时按键/点击会落到别的窗口。
pub(super) fn regain(expected: Option<(u32, u32)>) -> Result<()> {
    if foreground()? == expected { return Ok(()); }
    #[cfg(windows)]
    if let Some((id, _)) = expected {
        bring_to_front(id);
        std::thread::sleep(Duration::from_millis(150));
        if foreground()? == expected { return Ok(()); }
    }
    Err("前台被其他窗口占用且切不回原窗口，已暂停后续输入（这不是用户接管）；请重新观察并切回目标窗口后继续".into())
}
fn glide(enigo: &mut Enigo, from: (i32, i32), to: (i32, i32), expected_foreground: Option<(u32, u32)>) -> Result<()> {
    use rand_core::{OsRng, RngCore};
    let (ms, bend) = (glide_ms(from, to), (OsRng.next_u32() as f64 / u32::MAX as f64 - 0.5) * 0.2);
    let started = Instant::now();
    loop {
        // Sample by elapsed time, not step count: coarse OS sleep granularity must not stretch the motion.
        let t = (started.elapsed().as_secs_f64() * 1000.0 / ms).min(1.0);
        regain(expected_foreground)?;
        let (x, y) = glide_at(from, to, bend, t);
        move_pointer(enigo, x, y, true)?;
        if t >= 1.0 { return Ok(()); }
        std::thread::sleep(Duration::from_millis(8));
    }
}
fn input(enigo: &mut Enigo, shot: &Shot, a: &Action, expected_foreground: Option<(u32,u32)>) -> Result<()> {
    let _activity = crate::taskbar_status::activity(match a.action.as_str() {
        "click" | "double_click" => "点击控件",
        "type" => "输入内容",
        "press" => "发送按键",
        "scroll" => "滚动页面",
        "drag" => "拖动控件",
        "wait" => "等待界面",
        "move" => "移动指针",
        _ => "操作桌面",
    });
    let button = match a.button.as_deref() {
        Some("right") => Button::Right,
        Some("middle") => Button::Middle,
        _ => Button::Left,
    };
    if matches!(
        a.action.as_str(),
        "click" | "double_click" | "move" | "drag" | "scroll"
    ) {
        let (x, y) = point(shot, a.x, a.y)?;
        // 指针默认沿轨迹可见地移过去（用户能看清剑来在做什么）；glide:false 才瞬移。滚动始终瞬移定位。
        if a.glide.unwrap_or(a.action != "scroll") {
            glide(enigo, enigo.location().map_err(err)?, (x, y), expected_foreground)?;
        } else {
            move_pointer(enigo, x, y, false)?;
        }
        // Read back the actual native pointer. A wrong-DPI/clamped/injected location
        // must not be followed by a button press. No speculative correction/replay.
        let actual=enigo.location().map_err(err)?;
        if !pointer_matches((x,y),actual) {return Err(format!("系统鼠标落点与请求不符（请求{x},{y}，实际{},{}），未按下按钮；请根据新图继续",actual.0,actual.1));}
    }
    if a.action!="wait" { regain(expected_foreground)?; }
    if a.action != "wait" { check_overlay(shot, a, overlay_bounds(shot.desktop_capture))?; }
    match a.action.as_str() {
        "click" => click_button(enigo,button)?,
        "double_click" => {
            click_button(enigo,button)?;
            std::thread::sleep(Duration::from_millis(70));
            if foreground()? != expected_foreground {
                return Err("第一次点击后前台改变，停止第二次点击，请核对新图".into());
            }
            click_button(enigo,button)?;
        }
        "drag" => {
            let (x, y) = point(shot, a.x, a.y)?;
            let (tx, ty) = point(shot, a.to_x, a.to_y)?;
            if let Err(error)=enigo.button(button,Direction::Press) {
                let _=enigo.button(button,Direction::Release);return Err(err(error));
            }
            let moved = if a.glide == Some(true) { glide(enigo, (x, y), (tx, ty), expected_foreground) } else { (1..=12).try_for_each(|i| {
                regain(expected_foreground)?;
                move_pointer(enigo,
                    (x as i64 + (tx as i64 - x as i64) * i / 12) as i32,
                    (y as i64 + (ty as i64 - y as i64) * i / 12) as i32,
                    true,
                )?;
                std::thread::sleep(Duration::from_millis(16));
                Ok::<_, String>(())
            }) };
            let released = enigo.button(button, Direction::Release).map_err(err);
            moved?;
            released?;
        }
        "type" => {
            #[cfg(windows)]
            text_input::type_text(enigo, a.text.as_deref().unwrap(), expected_foreground)?;
            #[cfg(not(windows))]
            enigo.text(a.text.as_deref().unwrap()).map_err(err)?;
        }
        "press" => {
            let keys = keys(a.key.as_deref().unwrap())?;
            let pressed = keys
                .iter()
                .try_for_each(|k| enigo.key(*k, Direction::Press).map_err(err));
            let mut release_error = None;
            for k in keys.iter().rev() {
                if let Err(e) = enigo.key(*k, Direction::Release) {
                    release_error = Some(err(e));
                }
            }
            pressed?;
            if let Some(e) = release_error {
                return Err(e);
            }
        }
        "scroll" => {
            let axis = if a.axis.as_deref() == Some("horizontal") { Axis::Horizontal } else { Axis::Vertical };
            let delta = a.delta.unwrap();
            // One event of N*WHEEL_DELTA scrolls a single step in WeChat/Qt/DuiLib-style apps that count
            // messages, not magnitude; one notch per event moves N steps everywhere.
            for i in 0..delta.unsigned_abs() {
                if i > 0 {
                    std::thread::sleep(Duration::from_millis(8));
                    regain(expected_foreground)?;
                }
                enigo.scroll(delta.signum(), axis).map_err(err)?;
            }
        }
        "wait" => std::thread::sleep(Duration::from_millis(a.ms.unwrap_or(250))),
        "move" => (),
        _ => unreachable!(),
    }
    Ok(())
}

/// macOS 后台：把动作按进程投递给窗口所属应用；坐标是屏幕全局点，不移动真实光标。
fn post_background(shot: &Shot, a: &Action) -> Result<()> {
    let pid = shot.surface.pid.ok_or("后台窗口缺少pid")? as i32;
    let pointer = matches!(a.action.as_str(), "click" | "double_click" | "move" | "drag" | "scroll");
    let at = if pointer { point(shot, a.x, a.y)? } else { (0, 0) };
    let to = if a.action == "drag" { point(shot, a.to_x, a.to_y)? } else { at };
    background::post(pid, shot.surface.id, at, to, a)
}

pub(crate) const COMPLETION_GUIDANCE: &str = "截图只采一帧，不等待画面稳定；动图/视频不妨碍已就绪目标的操作，目标仍在加载或过渡时由模型决定显式wait后再观察。速度优先：用最新截图（含动作自动反馈）核对用户要求的结果；证据已满足目标就立即汇报并结束，禁止再截图、查状态、等待或二次确认；verification=unverified不要求额外检查。仅有明确未完成证据时才做一次针对性复查；没有新信息就如实报告未确认项，不轮询、不重放输入。";

fn run(owner: String, mut args: Value) -> Result<Value> {
    // Some clients omit the operation while supplying a complete action batch. Normalize the
    // unambiguous shape only; an explicitly supplied operation is never overridden.
    if args.get("operation").is_none() && args["actions"].is_array() {
        args["operation"] = json!("act");
    }
    let is_act = args["operation"] == "act";
    let operation = args["operation"].clone();
    let based_on = args["snapshotId"].clone();
    let notes = args.get("notes").cloned();
    let result = run_inner(owner, args);
    let mut result = match result {
        Err(e) if is_act => json!({"status":"not_executed","completedActions":0,
            "error":format!("{e}；本批次尚未执行")}),
        other => other?,
    };
    result["source"] = json!("jianlai");
    result["operation"] = operation;
    if is_act {
        result["basedOnSnapshotId"] = based_on;
        result["verification"] = json!("unverified");
        if result["status"] == "not_executed" { result["inputAttempted"] = json!(false); }
    }
    if is_act || result.get("images").is_some() {
        result["verificationNotice"] = json!(format!("executed只表示输入已发送，不代表界面加载完成或业务成功。{COMPLETION_GUIDANCE}未覆盖完整范围不能断言全部或不存在；notes未经工具验证。"));
    }
    if let Some(notes) = notes.filter(|v| v.as_str().is_some_and(|s| s.chars().count() <= 12000)) {
        result["notes"] = notes;
    }
    Ok(result)
}

fn run_inner(owner: String, mut args: Value) -> Result<Value> {
    // Read-only requests often inherit unrelated optional fields from a prior action.
    // Keep the actual observation parameters strict, especially crop coordinates.
    let operation = args["operation"].as_str().unwrap_or_default().to_owned();
    if matches!(operation.as_str(), "windows" | "screenshot") {
        if let Some(map) = args.as_object_mut() {
            map.retain(|key, _| matches!(key.as_str(), "operation" | "windowId" | "maxEdge" | "background" | "notes")
                || if operation == "windows" { matches!(key.as_str(), "query" | "observe") }
                else { matches!(key.as_str(), "monitorId" | "region" | "regionSpace" | "snapshotId" | "imageId") });
        }
    }
    let is_act = args["operation"] == "act";
    let request: Request = match serde_json::from_value(args) {
        Ok(request) => request,
        Err(e) if is_act => return Ok(json!({"status":"not_executed","completedActions":0,
            "error":format!("参数无效：{e}；本批次尚未执行。动作项只接受 action/x/y/toX/toY/button/text/key/delta/axis/ms/glide，action 取 click/double_click/move/drag/scroll/type/press/wait")})),
        Err(e) => return Err(err(e)),
    };
    let max_edge = request.max_edge.unwrap_or(1600);
    if request.region.is_some() && request.operation != "screenshot" {return Err("region仅用于screenshot".into());}
    if request.region_space.as_deref().is_some_and(|s|!matches!(s,"source"|"image")) {return Err("regionSpace必须是source或image".into());}
    if request.region_space.as_deref()==Some("image") && request.region.is_none() {return Err("regionSpace=image需要region及最新snapshotId/imageId".into());}
    if request.monitor_id.is_some() && request.operation != "screenshot" {
        return Err("monitorId仅用于screenshot；act由imageId选择屏幕".into());
    }
    if max_edge != 0 && !(640..=3840).contains(&max_edge) {
        return Err("maxEdge允许0（原分辨率）或640–3840；本批次尚未执行".into());
    }
    if request.notes.as_ref().is_some_and(|s| s.chars().count() > 12000) {
        return Err("notes最多12000字符；本批次尚未执行".into());
    }
    if request
        .feedback
        .as_deref()
            .is_some_and(|s| !matches!(s, "screenshot" | "desktop" | "none"))
    {
        return Err("无效feedback".into());
    }
    if (request.query.is_some() || request.observe.is_some()) && request.operation != "windows" {
        return Err("query/observe仅用于windows".into());
    }
    if request.background.is_some() && !matches!(request.operation.as_str(), "windows" | "screenshot" | "act") {
        return Err("background仅用于windows/screenshot/act".into());
    }
    if request.background == Some(true) && request.monitor_id.is_some() {
        return Err("后台模式不使用monitorId：macOS用windowId，Linux座席只有一个桌面".into());
    }
    // 先于桌面锁进入：Linux 首次会拉起座席（最多 2 秒），不应占着锁。
    let bg = request.background == Some(true);
    let _background = background::enter(bg)?;
    let mut state = DESKTOP
        .try_lock()
        .map_err(|_| "剑来正在操作桌面，请勿并行调用")?;
    let _activity = crate::taskbar_status::activity(match request.operation.as_str() {
        "windows" => "观察窗口",
        "screenshot" => "截取画面",
        "act" => "操作桌面",
        "activate" => "切换窗口",
        _ => "查看桌面记录",
    });
    match request.operation.as_str() {
        "windows" => {
            // One round-trip start: list windows and, on request, observe the bound target immediately.
            // 座席没有窗口管理器；xcap 的窗口枚举读的是用户自己的显示，后台下不能碰。
            let mut result = if bg && cfg!(target_os = "linux") { json!({"windows":[]}) } else { windows()? };
            if bg { result["background"] = background::info()?; }
            let all = result["windows"].as_array().cloned().unwrap_or_default();
            let mut target = request.window_id;
            if let Some(q) = request.query.as_deref().map(str::trim).filter(|q| !q.is_empty()) {
                let q = q.to_lowercase();
                let matched: Vec<Value> = all.iter().filter(|w| ["app", "title"].iter()
                    .any(|k| w[*k].as_str().is_some_and(|s| s.to_lowercase().contains(&q)))).cloned().collect();
                let usable: Vec<&Value> = matched.iter().filter(|w| w["minimized"] != true
                    && w["width"].as_u64().unwrap_or(0) > 32 && w["height"].as_u64().unwrap_or(0) > 32).collect();
                result["totalWindows"] = json!(all.len());
                result["query"] = json!(q);
                match usable.as_slice() {
                    [one] if target.is_none() => target = one["windowId"].as_u64().map(|id| id as u32),
                    [] => result["next"] = json!("没有可见窗口匹配query（最小化窗口需先在桌面恢复）；核对完整列表或换关键词"),
                    _ if target.is_none() => result["next"] = json!("query匹配到多个窗口，请指定windowId后再screenshot/windows(observe)"),
                    _ => (),
                }
                result["windows"] = json!(matched);
            }
            if request.observe == Some(true) && (target.is_some() || request.query.as_deref().is_none_or(|q| q.trim().is_empty())) {
                // Background windows capture via PrintWindow; the fallback is the focused window, then the desktop.
                let focused = all.iter().find(|w| w["focused"] == true && w["minimized"] != true)
                    .and_then(|w| w["windowId"].as_u64()).map(|id| id as u32);
                // 后台没有「前台窗口」可回退：只观察指定目标（Linux 座席为桌面）。
                let window_id = if bg { target } else { target.or(focused) };
                match capture(&owner, window_id, None, None, max_edge, &mut state) {
                    Ok(observed) => {
                        result.as_object_mut().unwrap().extend(observed.as_object().unwrap().clone());
                        result["observedWindowId"] = json!(window_id);
                        result["observedBy"] = json!(if target.is_some() {"query/windowId"} else if bg {"background"} else if focused.is_some() {"foreground"} else {"desktop"});
                        if !bg && window_id.is_some() && window_id != focused {
                            result["next"] = json!("目标窗口不在前台；输入前先 activate(windowId)，使用其返回的新图继续。");
                        }
                    }
                    Err(e) => result["observationError"] = json!(e),
                }
            }
            Ok(result)
        }
        "activate" => {
            let id = request.window_id.ok_or("activate需要windows返回的windowId（不是pid）")?;
            let target = window(id)?;
            let _lease = lease_input("jianlai")?;
            #[cfg(windows)]
            {
                *state = None;
                bring_to_front(id);
                std::thread::sleep(Duration::from_millis(150));
                if foreground()? != Some((id, target.pid().map_err(err)?)) {
                    return Err("系统未允许激活该窗口；请观察当前桌面，不要重复激活或发送按键".into());
                }
                let mut result = capture(&owner, Some(id), None, None, max_edge, &mut state)?;
                result["activated"] = json!(true);
                Ok(result)
            }
            #[cfg(not(windows))]
            { let _ = target; Err("activate当前仅支持Windows，请通过桌面可见入口切窗".into()) }
        }
        "screenshot" => {
            if request.window_id.is_some() && request.monitor_id.is_some() {
                return Err("windowId和monitorId不能同时用于截图".into());
            }
            let (wid,mid,region)=if request.region_space.as_deref()==Some("image") {
                let snap=state.as_ref().filter(|s|s.owner==owner && Some(&s.id)==request.snapshot_id.as_ref() && s.taken.elapsed()<=Duration::from_secs(180)).ok_or("局部截图依据已失效，请先重新截图")?;
                if snap.background != bg {return Err("局部截图依据的快照与本次background模式不一致，请重新截图".into());}
                let shot=snap.shots.iter().find(|s|Some(&s.image_id)==request.image_id.as_ref()).ok_or("imageId不属于最新截图")?;
                let action:Action=serde_json::from_value(json!({"action":"move","x":0,"y":0})).map_err(err)?;
                check_target(snap,shot,&action,false)?;
                if request.window_id.is_some_and(|id|Some(id)!=snap.window) || request.monitor_id.is_some_and(|id|snap.window.is_some()||id!=shot.surface.id) {return Err("局部截图目标与snapshotId/imageId不一致".into());}
                (snap.window, snap.window.is_none().then_some(shot.surface.id), Some(image_region(shot,request.region.unwrap())?))
            } else {
                if request.region.is_some() && request.window_id.is_none() && request.monitor_id.is_none() {return Err("原始像素region需要windowId或monitorId".into());}
                (request.window_id,request.monitor_id,request.region)
            };
            let mut result = capture(&owner, wid, mid, region, max_edge, &mut state)?;
            result["notes"] = json!(request.notes);
            if region.is_some() {
                result["coordinateNotice"] = json!("这是新裁剪图：act坐标从此图左上角(0,0)开始，禁止加region偏移或使用显示放大后的坐标；按images中的width/height定位。新snapshotId/imageId替换旧图，act反馈恢复完整窗口。");
            }
            Ok(result)
        }
        "recall" => {
            let path = dunce::canonicalize(request.image_path.ok_or("recall缺少imagePath")?).map_err(err)?;
            let folder = dunce::canonicalize(shot_folder(&owner, "desktop-shots")).map_err(err)?;
            if !path.starts_with(folder) || !matches!(path.extension().and_then(|s| s.to_str()), Some("jpg" | "png")) {
                return Err("只能回看本会话的桌面截图".into());
            }
            Ok(json!({"images":[{"path":path}],"historical":true,
                "notice":"历史截图仅供阅读，不产生可操作快照；操作前请使用当前截图"}))
        }
        "act" => {
            let actions = request.actions.as_deref().ok_or("缺少actions")?;
            if actions.is_empty() || actions.len() > 16 {
                return Err("每次需要1至16个动作".into());
            }
            if actions.iter().all(|a| a.action == "wait") {
                // Waiting injects no input: old/cropped image IDs and age cannot make it unsafe.
                // Preserve ownership/mode boundaries and observe the bound target, never chase focus.
                let snap = state.as_ref();
                if snap.is_some_and(|s| s.owner != owner || s.background != bg) {
                    return Err("等待观察不能使用其它会话或不同background模式的快照，请先重新截图".into());
                }
                if snap.is_some_and(|s| s.window.is_some() && request.window_id.is_some() && s.window != request.window_id) {
                    return Err("windowId与截图不符".into());
                }
                for (index, a) in actions.iter().enumerate() {
                    validate(a, None).map_err(|e| format!("actions[{index}].{e}"))?;
                }
                let window_id = request.window_id.or_else(|| snap.and_then(|s| s.window));
                let max_edge = request.max_edge.or_else(|| snap.map(|s| s.max_edge)).unwrap_or(1600);
                for a in actions { std::thread::sleep(Duration::from_millis(a.ms.unwrap_or(250))); }
                let mut result = json!({"status":"executed","completedActions":actions.len(),"inputAttempted":false});
                if request.feedback.as_deref() != Some("none") {
                    let window_id = if request.feedback.as_deref() == Some("desktop") { None } else { window_id };
                    match capture(&owner, window_id, None, None, max_edge, &mut state) {
                        Ok(value) => result.as_object_mut().unwrap().extend(value.as_object().unwrap().clone()),
                        Err(e) => result["observationError"] = json!(e),
                    }
                }
                return Ok(result);
            }
            // 座席是 Xvfb（X11），与用户会话是否 Wayland 无关。
            if !bg && cfg!(target_os = "linux") && std::env::var_os("WAYLAND_DISPLAY").is_some() {
                return Err(
                    "剑来 Linux 输入当前仅支持 X11；Wayland 不会静默使用 XWayland 操作".into(),
                );
            }
            let missing_snapshot = request.snapshot_id.as_deref().is_none_or(|id| id.trim().is_empty());
            let current = state.as_ref().filter(|snap| snap.owner == owner && snap.background == bg
                && snap.invalidated.is_none() && snap.taken.elapsed() <= Duration::from_secs(180));
            if missing_snapshot || current.is_some_and(|snap| Some(&snap.id) != request.snapshot_id.as_ref()) {
                // Reject, never replay. Keep the latest usable observation instead of rotating IDs/images
                // on every parameter error; check_target still validates it before any subsequent input.
                let mut result = json!({"status":"not_executed","completedActions":0,"required":["snapshotId"]});
                if missing_snapshot {
                    result["errorCode"] = json!("missing_snapshot_id");
                    result["error"] = json!("act 缺少 snapshotId（不是快照过期）；未执行、未重新截图。请原样填写上次观察返回的 snapshotId 后提交动作。");
                } else {
                    result["errorCode"] = json!("stale_snapshot_id");
                    result["error"] = json!("旧snapshotId不可用于输入；最新快照仍可用，未执行、未重新截图。请核对最新图及completedActions，不要重放已执行动作。");
                }
                if let Some(snap) = current {
                    result["snapshotId"] = json!(snap.id);
                    result["snapshotPreserved"] = json!(true);
                    result["imageIds"] = json!(snap.shots.iter().map(|shot| &shot.image_id).collect::<Vec<_>>());
                    result["nextArguments"] = json!({"operation":"act","snapshotId":snap.id});
                    if snap.shots.len() == 1 { result["nextArguments"]["imageId"] = json!(snap.shots[0].image_id); }
                    result["next"] = json!("使用已有最新图片和 nextArguments 修正参数，重新决策剩余 actions；不要重放已执行动作。若上下文中已看不到该图，先用 recall 回看或重新 screenshot，不要凭记忆操作。");
                } else {
                    result["next"] = json!("当前没有本会话未失效的同模式快照；先 windows(observe:true) 或 screenshot 获取 snapshotId，不要继续省略它。");
                }
                return Ok(result);
            }
            if state.is_none() {
                let mut result = json!({"status":"not_executed","completedActions":0,"error":"没有可用快照，附当前观察；基于新图重新决策"});
                observe(&owner, request.window_id, None, request.feedback.as_deref() == Some("desktop"), max_edge, &mut state, &mut result);
                return Ok(result);
            }
            let snap = state.as_ref().unwrap();
            if snap.owner != owner {
                return Err("快照无效或已过期，请重新截图；不能重放动作".into());
            }
            if Some(&snap.id) != request.snapshot_id.as_ref() {
                let window_id = snap.window;
                let max_edge = snap.max_edge;
                let mut result = json!({"status":"not_executed","completedActions":0,
                    "error":"旧snapshotId已失效，附当前观察；请基于新图重新决策，不自动重放"});
                observe(&owner, window_id, None, request.feedback.as_deref() == Some("desktop"), max_edge, &mut state, &mut result);
                return Ok(result);
            }
            if let Some(reason) = snap.invalidated.clone() {
                let window_id = snap.window;
                let max_edge = snap.max_edge;
                let mut result = json!({"status":"not_executed","completedActions":0,
                    "error":format!("快照在截图后被其它工具改变的画面作废：{reason}；附当前观察，请基于新图重新决策")});
                observe(&owner, window_id, None, request.feedback.as_deref() == Some("desktop"), max_edge, &mut state, &mut result);
                return Ok(result);
            }
            if snap.background != bg {
                // 绝不让「想后台」的请求落到真实输入上，反过来也一样。
                return Err(if bg { "当前快照是前台截图；后台 act 需要先用 background:true 截图，本次不会改用真实输入" }
                    else { "当前快照是后台截图；act 也必须带 background:true，本次不会改用真实输入" }.into());
            }
            // 后台不注入真实输入：不占输入闸门（不挡用户的 Chrome），也不能记为「刚注入」掩盖用户的真实操作。
            let _lease = if bg { None } else { Some(lease_input("jianlai")?) };
            if snap.window.is_some() && request.window_id.is_some() && request.window_id != snap.window {
                return Err("windowId与截图不符".into());
            }
            let shot = action_shot(&snap.shots, request.image_id.as_deref())?.clone();
            for (index, a) in actions.iter().enumerate() {
                if let Err(e) = validate(a, Some(&shot)).and_then(|_| if bg { background::validate(a) } else { Ok(()) }) {
                    return Ok(json!({"status":"not_executed","completedActions":0,
                        "error":format!("actions[{index}].{e}；本批次尚未执行")}));
                }
            }
            let max_edge = request.max_edge.unwrap_or(snap.max_edge);
            // 后台的观察范围由快照决定（Linux 座席是桌面），不听请求里多余的 windowId。
            let feedback_window = if bg { snap.window } else { request.window_id.or(snap.window) };
            let monitor_id = snap.window.is_none().then_some(shot.surface.id);
            // macOS 后台按进程投递事件：不创建 Enigo（它只发给 HID 层，还会弹权限提示）。
            let direct = bg && cfg!(target_os = "macos");
            let mut enigo = if direct { None } else {
                Some(Enigo::new(&if bg { background::seat_settings()? } else { Settings::default() }).map_err(err)?)
            };
            let foreground_before = if direct { real_foreground().ok().flatten() } else { None };
            // Consume before the first OS event; all later failures are explicitly non-retryable.
            let mut snap = state.take().unwrap();
            let mut completed = 0;
            let mut failure = None;
            #[cfg(windows)]
            let batch_started = crate::threads::now_ms();
            let mut attempted = false;
            let mut clipboard = crate::clipboard::sequence();
            let mut copied_before_paste = None;
            let mut frames = Vec::new();
            for (index, a) in actions.iter().enumerate() {
                // 光效亮着时用户亲手按了 Esc：立即停下，剩余动作不再执行。
                #[cfg(windows)]
                if !bg && crate::agent_glow::escaped_since(batch_started) {
                    failure = Some(ESCAPED.to_string());
                    break;
                }
                if a.action != "wait" {
                    // Earlier input (incl. hover from move) changes nearby pixels by design; relax to the target core.
                    let after_input = actions[..index].iter().any(|p| p.action != "wait");
                    if let Err(e) = check_target(&snap, &shot, a, after_input) {
                        failure = Some(e);
                        break;
                    }
                }
                let clipboard_paste = cfg!(windows) && a.action == "type" && a.text.as_deref().is_some_and(|text| !text.is_empty());
                if clipboard_paste {
                    if let Some(copied) = crate::clipboard::copied_since(clipboard) { copied_before_paste = Some(copied); }
                }
                attempted |= a.action != "wait";
                let sent = match enigo.as_mut() {
                    Some(enigo) => input(enigo, &shot, a, snap.foreground),
                    None => post_background(&shot, a),
                };
                // Our temporary paste/restore is not a copy made by the target application.
                if clipboard_paste { clipboard = crate::clipboard::sequence(); }
                if let Err(e) = sent {
                    failure = Some(e);
                    break;
                }
                completed += 1;
                std::thread::sleep(action_delay(&actions, index));
                // Keep reading frames between inputs, but never duplicate the final scroll + wait feedback.
                if let Some(scroll_index) = scroll_frame_index(&actions, index, request.feedback.as_deref()) {
                    let (mut frame, mut scratch) = (json!({}), None);
                    observe(&owner, feedback_window, monitor_id, request.feedback.as_deref() == Some("desktop"), max_edge, &mut scratch, &mut frame);
                    for mut image in frame["images"].as_array().cloned().unwrap_or_default() {
                        image["frame"] = json!(scroll_index);
                        frames.push(image);
                    }
                }
                // Let an explicit wait finish before observing a click/key's focus transition.
                if actions.get(index + 1).is_some_and(|a| a.action == "wait") { continue; }
                match foreground() {
                    Ok(f) => {
                        if f != snap.foreground && actions[index + 1..].iter().any(|a| a.action != "wait") {
                            failure = Some("动作后前台窗口改变，已停止后续输入；请根据新截图继续，不要重放整批".into());
                            break;
                        }
                        snap.foreground = f;
                    }
                    Err(e) => {
                        failure = Some(e);
                        break;
                    }
                }
            }
            let mut result = json!({"status":if failure.is_some(){if attempted {"needs_review"} else {"not_executed"}}else{"executed"},"completedActions":completed,"inputAttempted":attempted,"error":failure,"notes":request.notes});
            if shot.region.is_some() {
                result["coordinateNotice"] = json!("局部操作后的反馈恢复完整窗口；使用新的imageId和完整图片坐标，不沿用局部坐标");
            }
            if failure.is_some() || request.feedback.as_deref() != Some("none") {
                observe(&owner, feedback_window, monitor_id, request.feedback.as_deref() == Some("desktop"), max_edge, &mut state, &mut result);
            }
            if direct {
                // 输入节拍和反馈截图后再读真实前台，检测应用收到点击后是否自行激活。
                if let Some(changed) = background::foreground_notice(foreground_before, real_foreground().ok().flatten()) {
                    result["foregroundChanged"] = changed;
                }
            }
            // Read after the fixed reaction pause and capture; clipboard writes may be asynchronous.
            if let Some(copied) = crate::clipboard::copied_since(clipboard).or(copied_before_paste) { result["clipboard"] = copied; }
            // One fresh feedback is enough on failure; request a native crop only if more detail is needed.
            if !frames.is_empty() {
                frames.extend(result["images"].as_array().cloned().unwrap_or_default());
                result["images"] = json!(frames);
                result["framesNotice"] = json!("images中带frame的是第frame个scroll后的只读中间帧，仅供阅读；act只用顶层snapshotId及最后一张图的坐标");
            }
            Ok(result)
        }
        _ => Err("未知剑来操作；可用：windows/screenshot/act/activate/recall/run/experience_search/experience_save/experience_feedback，切窗用activate+windowId".into()),
    }
}
// Observation never retries input; follow the foreground and fall back to desktop if needed.
fn observe(owner: &str, previous_window: Option<u32>, monitor_id: Option<u32>, desktop: bool, max_edge: u32, state: &mut Option<Snapshot>, result: &mut Value) {
    if background::active() {
        // 后台没有「前台」可跟随：始终重拍同一目标（macOS 的同一窗口 / Linux 座席桌面），不回退到用户的桌面。
        match capture(owner, previous_window, None, None, max_edge, state) {
            Ok(value) => result.as_object_mut().unwrap().extend(value.as_object().unwrap().clone()),
            Err(e) => result["observationError"] = json!(e),
        }
        return;
    }
    // Follow the actual foreground after opening a compose window/dialog, never activate it.
    // Shell task view and hidden helper windows must stay in desktop scope.
    let focused = foreground().ok().flatten().and_then(|(id, _)| window(id).ok())
        .filter(|w| w.width().unwrap_or(0) > 32 && w.height().unwrap_or(0) > 32
            && !w.is_minimized().unwrap_or(true));
    let monitor_id = monitor_id.or_else(|| focused.as_ref()?.current_monitor().ok()?.id().ok());
    let window_id = if desktop { None } else { focused.as_ref().and_then(|w| w.id().ok()) };
    if window_id != previous_window {
        result["observationScopeChanged"] = json!(true);
        result["coordinateNotice"] = json!("截图范围已变化，必须使用本次imageId和图片内坐标，不沿用上一张图坐标");
    }
    let observation = capture(owner, window_id, if window_id.is_some() { None } else { monitor_id }, None, max_edge, state).or_else(|e| {
        if window_id.is_none() && monitor_id.is_none() { return Err(e); }
        result["windowObservationError"] = json!(e);
        capture(owner, None, monitor_id, None, max_edge, state)
    });
    match observation {
        Ok(value) => result.as_object_mut().unwrap().extend(value.as_object().unwrap().clone()),
        Err(e) => result["observationError"] = json!(e),
    }
}

fn has_target(args: &Value) -> bool {
    args["actions"].as_array().into_iter().flatten().chain(std::iter::once(&args["action"])).any(|a| a["target"].is_string())
}

pub(crate) async fn execute(root: &Path, args: &Value, owner: &str) -> Result<Value> {
    let args = tolerate_slips(args);
    // 观察只给已点亮的光效续期；真正点亮在拿到输入闸门时（act/activate）。
    // 后台模式不碰用户的指针与前台，经验读写不操作桌面，都不续期。
    #[cfg(windows)]
    if args["background"] != json!(true) && !crate::tool_experience::is_operation(&args) {
        crate::agent_glow::touch();
    }
    // 数字员工任务操作桌面前排队，用户在用电脑时先询问。
    if args["background"] != json!(true) && !crate::tool_experience::is_operation(&args) {
        // 观察放行；第一次动鼠标键盘/切窗前（呼吸灯亮起前）才征得用户同意。
        let input = matches!(args["operation"].as_str(), Some("act" | "run" | "activate"));
        crate::employee::claim_desktop(owner, input).await?;
        // 记下是谁在用桌面：用户按 Esc 时停的就是它。
        *DESKTOP_OWNER.lock().unwrap() = Some(owner.to_string());
    }
    execute_inner(root, &args, owner).await
}

/// look/vision are unused but still tolerated, and a lone `action` (chrome's shape, often reused by
/// weaker models such as DeepSeek) becomes a one-item batch; the desktop request schema stays strict.
fn tolerate_slips(args: &Value) -> Value {
    let mut inner = args.clone();
    if let Some(map) = inner.as_object_mut() {
        map.remove("look"); map.remove("vision");
        if !map.contains_key("actions") && map.get("action").is_some_and(Value::is_object) {
            let action = map.remove("action").unwrap();
            map.insert("actions".into(), json!([action]));
        }
        if let Some(actions) = map.get_mut("actions").and_then(Value::as_array_mut) {
            for action in actions { normalize_action(action); }
        }
    }
    inner
}

/// 弱模型（Devin 等）反复搞错的动作形状：动作名写在 `type` 上、拿 text/key 当动作名、
/// 坐标写成字符串或小数。这些写法没有歧义，归一后直接执行比退回让模型重试更快也更稳；
/// 其余字段仍走严格 schema，真正说不清的形状照旧报错。
fn normalize_action(action: &mut Value) {
    let Some(map) = action.as_object_mut() else { return };
    if !map.contains_key("action") {
        if let Some(kind) = map.remove("type").filter(Value::is_string) { map.insert("action".into(), kind); }
    }
    for (from, to) in [("keys", "key"), ("value", "text"), ("content", "text"), ("to_x", "toX"), ("to_y", "toY"),
        ("duration", "ms"), ("durationMs", "ms"), ("duration_ms", "ms")] {
        if !map.contains_key(to) { if let Some(v) = map.remove(from) { map.insert(to.into(), v); } }
    }
    for key in ["x", "y", "toX", "toY", "delta", "ms"] {
        let fixed = match map.get(key) {
            Some(Value::String(s)) => s.trim().parse::<f64>().ok(),
            Some(v) => v.as_f64().filter(|f| f.fract() != 0.0),
            None => None,
        };
        if let Some(v) = fixed { map.insert(key.into(), json!(v.round() as i64)); }
    }
    let canonical = match map.get("action").and_then(Value::as_str).unwrap_or_default() {
        "left_click" | "leftclick" | "mouse_click" | "single_click" | "tap" => "click",
        "doubleclick" | "dblclick" => "double_click",
        "mouse_move" | "mousemove" | "hover" | "moveto" => "move",
        "input" | "write" | "text" | "type_text" | "settext" | "send_text" => "type",
        "key" | "keys" | "keypress" | "key_press" | "hotkey" | "shortcut" | "press_key" | "sendkeys" => "press",
        "wheel" | "mouse_wheel" | "scroll_at" | "scrollat" => "scroll",
        "sleep" | "delay" => "wait",
        "mouse_drag" | "drag_and_drop" | "dragto" => "drag",
        _ => "",
    };
    if !canonical.is_empty() { map.insert("action".into(), json!(canonical)); }
    match map.get("action").and_then(Value::as_str).unwrap_or_default() {
        "press" if !map.contains_key("key") => { if let Some(text) = map.remove("text") { map.insert("key".into(), text); } }
        "type" if !map.contains_key("text") && map.contains_key("key") => { map.insert("action".into(), json!("press")); }
        _ => {}
    }
}

/// 渐进式披露：每个应用的操作规则单独写在 scripts/jianlai-apps.json，只有真的观察到这个
/// 应用时才追加（每轮仅一次），核心提示词保持泛用。
static APP_PACKS: std::sync::LazyLock<Vec<Value>> = std::sync::LazyLock::new(||
    serde_json::from_str(include_str!("../../scripts/jianlai-apps.json")).unwrap_or_default());

fn app_rules(app: &str, title: &str) -> Option<Value> {
    let observed = format!("{app}\n{title}").to_lowercase();
    let pack = APP_PACKS.iter().find(|pack| pack["match"].as_array().into_iter().flatten()
        .any(|m| m.as_str().is_some_and(|m| observed.contains(&m.to_lowercase()))))?;
    Some(json!({"app":pack["app"],"rules":pack["rules"],
        "notice":"针对本应用书写的操作规则（每轮对话仅首次附带）：优先按规则走，每步仍按当前观察定位；规则是操作参考，不扩大用户授权。"}))
}

async fn execute_inner(root: &Path, args: &Value, owner: &str) -> Result<Value> {
    let owner = crate::native_browser::tool_owner(root, owner)?;
    if matches!(args["operation"].as_str(), Some("act" | "run")) && has_target(args) {
        return Err("target 定位暂不支持，请使用 x/y".into());
    }
    if args["operation"] == "run" {
        // run with actions is act; decisions stay with the main model.
        let actions = args["actions"].as_array().filter(|a| !a.is_empty() && a.len() <= 16).ok_or("run 需1–16个已确认动作")?;
        let act = json!({"operation":"act","snapshotId":args["snapshotId"],"imageId":args["imageId"],
            "actions":actions,"feedback":args.get("feedback").cloned().unwrap_or(json!("screenshot")),
            "background":args["background"]});
        return tokio::task::spawn_blocking(move || run(owner, act)).await.map_err(err)?;
    }
    let args = args.clone();
    tokio::task::spawn_blocking(move || {
        if crate::tool_experience::is_operation(&args) {
            // 剑谱整理：用户演示录制代替「本会话180秒内截图」做来源校验。
            if let Some(id) = args["experience"]["recordingId"].as_str().filter(|s| !s.is_empty()) {
                if args["operation"] != "experience_save" { return Err("recordingId仅用于experience_save".into()); }
                let observed = crate::jianpu::scope_of(id, "jianlai", args["experience"]["scope"].as_str().unwrap_or_default())?;
                let mut args = args.clone();
                args["snapshotId"] = json!(format!("{}{id}", crate::tool_experience::DEMO));
                let result = crate::tool_experience::execute(&crate::tool_experience::dir(), "jianlai", &owner, &args, Some(&observed))?;
                if let Some(route) = result["experience"]["id"].as_str() { crate::jianpu::link_route(id, route); }
                return Ok(result);
            }
            let observed = if args["operation"] == "experience_search" { None } else {
                let state = DESKTOP.try_lock().map_err(|_| "剑来正在操作桌面")?;
                let snap = state.as_ref().filter(|s| s.owner == owner && args["snapshotId"].as_str() == Some(&s.id)
                    && s.taken.elapsed() <= Duration::from_secs(180)).ok_or("保存/反馈经验前需本会话最新截图（180秒内）")?;
                let id = snap.window.or(snap.foreground.map(|(id, _)| id)).ok_or("请截目标应用窗口后记录经验")?;
                Some(crate::tool_experience::scope("jianlai", &window(id)?.app_name().map_err(err)?)?)
            };
            if args["operation"] == "experience_save" {
                return crate::tool_experience::execute_async(&crate::tool_experience::dir(), "jianlai", &owner, &args, observed.as_deref());
            }
            return crate::tool_experience::execute(&crate::tool_experience::dir(), "jianlai", &owner, &args, observed.as_deref());
        }
        let entering = matches!(args["operation"].as_str(), Some("windows" | "screenshot" | "activate"));
        let mut result = run(owner.clone(), args)?;
        // Entering an app: surface its verified routes once per session (see tool_experience::hint).
        let id = result["windowId"].as_u64().or(result["foreground"][0].as_u64());
        if let Some((app, title)) = id.filter(|_| entering).and_then(|id| window_label(id as u32)) {
            if let Some(hint) = crate::tool_experience::hint(&crate::tool_experience::dir(), "jianlai", &owner, &app) {
                result["experienceHint"] = hint;
            }
            // 本应用单独书写的规则，按需追加一次（见 app_rules）。
            if let Some(rules) = app_rules(&app, &title)
                .filter(|_| crate::tool_experience::once_per_turn(&owner, &format!("app-rules\n{app}"))) {
                result["appRules"] = rules;
            }
        }
        Ok(result)
    })
        .await
        .map_err(err)?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lyra_shots_live_under_session_tool_results_and_others_stay_global() {
        let session = std::env::temp_dir().join("alkaid").join("tool-results").join("abc-1");
        assert_eq!(shot_folder(&session.to_string_lossy(), "browser-shots"), session.join("browser-shots"));
        for owner in ["3f2a-uuid".into(), session.join("..").to_string_lossy().into_owned(), session.with_file_name("a b").to_string_lossy().into_owned()] {
            assert!(!shot_folder(&owner, "desktop-shots").starts_with(session.parent().unwrap()), "{owner}");
        }
    }
    #[test]
    fn keyframe_then_cumulative_deltas() {
        use xcap::image::{Rgba, RgbaImage};
        use crate::visual_guard::PixelRect;
        let page = RgbaImage::from_pixel(320, 320, Rgba([255, 255, 255, 255]));
        let p0 = Delivered { stream: "window".into(), group: "snap".into(), keyframe_id: "window-1".into(), keyframe: page.clone(), keyframe_path: "k.jpg".into(),
            seen: page.clone(), seen_path: "k.jpg".into(), delta: None, deltas: 0 };
        let rect = |l, t, r, b| PixelRect { left: l, top: t, right: r, bottom: b };
        assert!(matches!(plan_delivery(None, &page), Delivery::Keyframe));
        assert!(matches!(plan_delivery(Some(&p0), &page), Delivery::Unchanged));
        assert!(matches!(plan_delivery(Some(&p0), &RgbaImage::new(321, 320)), Delivery::Keyframe), "resized window");
        let mut typed = page.clone();
        typed.put_pixel(150, 150, Rgba([0, 0, 0, 255]));
        assert!(matches!(plan_delivery(Some(&p0), &typed), Delivery::Delta(b) if b == rect(96, 96, 192, 192)), "a single glyph or animation frame is sent");
        // After a delta, a revert to the keyframe resends the old box so the latest delta stays authoritative.
        let p1 = Delivered { seen: typed.clone(), delta: Some(rect(96, 96, 192, 192)), deltas: 1, ..p0.clone() };
        assert!(matches!(plan_delivery(Some(&p1), &typed), Delivery::Unchanged));
        assert!(matches!(plan_delivery(Some(&p1), &page), Delivery::Delta(b) if b == rect(96, 96, 192, 192)));
        // Change accumulates against the keyframe; over a quarter of the image becomes a new keyframe.
        let mut more = typed.clone();
        more.put_pixel(40, 40, Rgba([0, 0, 0, 255]));
        assert!(matches!(plan_delivery(Some(&p1), &more), Delivery::Keyframe));
        let capped = Delivered { deltas: MAX_DELTAS, ..p1.clone() };
        assert!(matches!(plan_delivery(Some(&capped), &page), Delivery::Keyframe));
    }
    #[test]
    fn lone_action_becomes_a_one_item_batch() {
        let click = json!({"action":"click","x":1,"y":2});
        let args = tolerate_slips(&json!({"operation":"act","snapshotId":"s","action":click,"look":true}));
        assert_eq!(args, json!({"operation":"act","snapshotId":"s","actions":[click]}));
        assert!(serde_json::from_value::<Request>(args).is_ok());
        // Ambiguous shapes stay untouched so the strict schema still reports them.
        let both = json!({"operation":"act","action":click,"actions":[]});
        assert_eq!(tolerate_slips(&both), both);
    }
    #[test]
    fn weak_model_action_shapes_are_normalized_instead_of_bounced() {
        let args = tolerate_slips(&json!({"operation":"act","snapshotId":"s","actions":[
            {"type":"left_click","x":"12","y":34.6},
            {"action":"text","value":"hi"},
            {"action":"key","text":"Enter"},
            {"action":"sleep","duration":200}]}));
        assert_eq!(args["actions"], json!([
            {"action":"click","x":12,"y":35},
            {"action":"type","text":"hi"},
            {"action":"press","key":"Enter"},
            {"action":"wait","ms":200}]));
        assert!(serde_json::from_value::<Request>(args).is_ok());
    }
    #[test]
    fn app_rules_only_follow_the_observed_app() {
        let pack = app_rules("Excel.exe", "销售表 - Excel").expect("表格类规则");
        assert!(pack["rules"].as_array().unwrap().iter().any(|r| r.as_str().unwrap_or_default().contains("名称框")));
        assert!(app_rules("explorer.exe", "文档").is_some());
        assert!(app_rules("SomeInternalTool.exe", "面板").is_none(), "没写规则的应用不追加提示词");
    }
    #[test]
    fn target_actions_map_through_the_guarded_point() {
        assert!(has_target(&json!({"actions":[{"action":"press","key":"Enter"},{"action":"click","target":"v2"}]})));
        assert!(has_target(&json!({"action":{"action":"click","target":"搜索框"}})));
        assert!(!has_target(&json!({"actions":[{"action":"click","x":1,"y":2}]})));
        // Image pixel (800,400) → desktop surface, through the same region/DPI mapping as x/y input.
        let shot = Shot { image_id:"crop".into(), surface:Surface{id:1,pid:None,x:-1920,y:-100,width:1920,height:1080},
            pixels:(1600,800),source_pixels:(3840,2160),region:Some(Region{x:200,y:100,width:800,height:400}),guard:None,overlay:None,desktop_capture:false };
        assert_eq!(point(&shot, Some(800), Some(400)).unwrap(), (-1620,50));
    }    #[test]
    fn local_image_regions_and_actual_pointer_validation() {
        let shot=Shot {image_id:"crop".into(),surface:Surface{id:1,pid:None,x:-1920,y:-100,width:1920,height:1080},
            pixels:(400,200),source_pixels:(3840,2160),region:Some(Region{x:200,y:100,width:800,height:400}),guard:None,overlay:None,desktop_capture:false};
        let region=image_region(&shot,Region{x:50,y:25,width:100,height:50}).unwrap();
        assert_eq!((region.x,region.y,region.width,region.height),(300,150,200,100));
        assert!(image_region(&shot,Region{x:399,y:0,width:2,height:1}).is_err());
        assert!(pointer_matches((-1920,-100),(-1919,-99)));
        assert!(pointer_matches((-1920,-100),(-1918,-98)));
        assert!(!pointer_matches((-1920,-100),(-1917,-100)));
        assert!(!pointer_matches((2250,55),(2250,58)));
        // VIRTUALDESK mapping: Windows floors n*size/65536; edges and negative origins must land exactly.
        for (origin, size) in [(-1920, 4480), (0, 1920), (-3840, 11520)] {
            for p in [origin, origin + 1, origin + size / 2, origin + size - 1] {
                let n = virtual_desk_coordinate(p, origin, size) as i64;
                assert_eq!(origin as i64 + n * size as i64 / 65536, p as i64);
            }
        }
        assert_eq!(virtual_desk_coordinate(-5000, -1920, 4480), 0);
        assert!(!pointer_matches((2250,55),(2054,188))); // Reported multi-monitor miss is not rounding.
        assert!(!pointer_matches((i32::MIN,0),(i32::MAX,0)));
        let original=xcap::image::RgbaImage::from_pixel(200,100,xcap::image::Rgba([20,20,20,255]));
        let guarded=Shot{pixels:(200,100),source_pixels:(200,100),region:None,guard:Some(crate::visual_guard::VisualGuard::new(&original)),..shot};
        let action:Action=serde_json::from_value(json!({"action":"click","x":80,"y":60})).unwrap();
        assert!(guard_target(&guarded,&action,&original,false,None).is_ok());
        let changed=xcap::image::RgbaImage::from_pixel(200,100,xcap::image::Rgba([200,200,200,255]));
        assert!(guard_target(&guarded,&action,&changed,false,None).is_err());
        assert!(guard_target(&guarded,&action,&changed,true,None).is_err());
        let bad=run("test".into(),json!({"operation":"act","regionSpace":"unknown"})).unwrap();
        assert_eq!(bad["status"],"not_executed");
    }
    #[test]
    fn observation_samples_once_even_with_animation_or_delayed_content() {
        use xcap::image::{Rgba, RgbaImage};
        let old = RgbaImage::from_pixel(100, 100, Rgba([255, 255, 255, 255]));
        let new = RgbaImage::from_pixel(100, 100, Rgba([0, 0, 0, 255]));
        let mut caret = old.clone();
        caret.put_pixel(40, 5, Rgba([0, 0, 0, 255]));
        assert_eq!(changed_tiles(&old, &caret, None).unwrap().iter().filter(|c| **c).count(), 1);
        assert!(changed_tiles(&old, &caret, None).unwrap()[1]);
        assert!(changed_tiles(&old, &RgbaImage::new(101, 100), None).is_none());

        // Single-frame observation must not chase either a blinking caret or a full-screen animation.
        for moving in [&caret, &new] {
            let mut calls = 0;
            for expected in [&old, moving, &old, moving] {
                let before = calls;
                let (frame, report, overlay) = sample_image(|| {
                    calls += 1;
                    Ok(if calls % 2 == 1 { old.clone() } else { moving.clone() })
                }, None).unwrap();
                assert_eq!(calls, before + 1);
                assert_eq!(&frame, expected);
                assert_eq!(report["samples"], 1);
                assert_eq!(report["status"], "not_checked");
                assert!(report.get("ignoredMotion").is_none());
                assert!(overlay.is_none());
            }
        }
        // Delayed content is not polled implicitly; return the current frame without claiming readiness.
        let (frame, _, _) = sample_image(|| Ok(old), None).unwrap();
        assert_ne!(frame, new);
        assert!(matches!(sample_image(|| Err("capture failed".into()), None), Err(e) if e == "capture failed"));
        let result = run("validation".into(), json!({"operation":"act", "snapshotId":"old", "unexpected":true})).unwrap();
        assert_eq!(result["operation"], "act");
        assert_eq!(result["basedOnSnapshotId"], "old");
        assert_eq!(result["verification"], "unverified");
    }
    #[test]
    fn taskbar_overlay_updates_only_exempt_unchanged_geometry() {
        use xcap::image::{Rgba, RgbaImage};
        use crate::taskbar_status::Rect;
        let original = RgbaImage::from_pixel(200, 200, Rgba([20,20,20,255]));
        let bounds = Rect { left:-50, top:-30, right:-20, bottom:30 };
        let shot = Shot { image_id:"crop".into(), surface:Surface{id:1,pid:None,x:-100,y:-50,width:100,height:100},
            pixels:(100,100), source_pixels:(200,200), region:Some(Region{x:40,y:40,width:100,height:100}),
            guard:Some(crate::visual_guard::VisualGuard::new(&original)), overlay:Some(bounds), desktop_capture:true };
        let exclude = overlay_region(&shot.surface, shot.source_pixels, shot.overlay).unwrap();
        assert_eq!(exclude, crate::visual_guard::PixelRect { left:100, top:40, right:160, bottom:160 });
        let mut updated = original.clone();
        for y in exclude.top..exclude.bottom { for x in exclude.left..exclude.right {
            updated.put_pixel(x,y,Rgba([220,220,220,255]));
        } }
        let action:Action = serde_json::from_value(json!({"action":"click","x":56,"y":60})).unwrap();
        assert_eq!(point(&shot,action.x,action.y).unwrap(),(-52,0));
        assert!(shot.guard.as_ref().unwrap().changed_near(&updated,96.,100.,false));
        assert!(guard_target(&shot,&action,&updated,false,Some(bounds)).is_ok());
        assert!(guard_target(&shot,&action,&updated,true,Some(bounds)).is_ok());
        let still = |a: &RgbaImage, b: &RgbaImage, ex| !changed_tiles(a, b, ex).unwrap().contains(&true);
        assert!(still(&original,&updated,Some(exclude)));
        assert!(!still(&original,&updated,None));
        // Comparisons never mask or alter the pixels returned to the model.
        assert_eq!(updated.get_pixel(100,100).0,[220,220,220,255]);
        for current in [None, Some(Rect{left:-49,..bounds}), Some(Rect{right:-19,..bounds}),
            Some(Rect{top:-29,..bounds}), Some(Rect{bottom:31,..bounds})] {
            assert!(check_overlay(&shot,&action,current).unwrap().is_none());
            assert!(guard_target(&shot,&action,&updated,false,current).is_err());
        }
        let absent = Shot { overlay:None, ..shot.clone() };
        assert!(check_overlay(&absent,&action,Some(bounds)).unwrap().is_none());
        assert!(guard_target(&absent,&action,&updated,false,Some(bounds)).is_err());
        for value in [json!({"action":"click","x":60,"y":60}),
            json!({"action":"drag","x":56,"y":60,"toX":60,"toY":60})] {
            let inside:Action = serde_json::from_value(value).unwrap();
            assert!(guard_target(&shot,&inside,&original,false,Some(bounds)).is_err());
        }
        for y in 96..105 { for x in 92..100 { updated.put_pixel(x,y,Rgba([200,0,0,255])); } }
        assert!(guard_target(&shot,&action,&updated,false,Some(bounds)).is_err());
        assert!(guard_target(&shot,&action,&updated,true,Some(bounds)).is_err());
        assert!(!still(&original,&updated,Some(exclude)));
        let other_monitor = Surface { x:0,y:0,..shot.surface.clone() };
        assert!(overlay_region(&other_monitor,shot.source_pixels,Some(bounds)).is_none());
        let other_shot = Shot { surface:other_monitor, overlay:None, ..shot.clone() };
        assert!(check_overlay(&other_shot,&action,Some(bounds)).is_ok());
        let fractional = Surface { x:0,y:0,width:3,height:3,..shot.surface.clone() };
        assert!(overlay_region(&fractional,(2,2),Some(Rect{left:1,top:1,right:2,bottom:2})).is_none());
        assert!(overlay_region(&shot.surface,shot.source_pixels,None).is_none());
    }
    #[test]
    fn cursor_marker_tracks_actual_position_in_scaled_regions() {
        let surface = Surface { id:1, pid:None, x:-1920, y:-100, width:1920, height:1080 };
        let region = Region { x:200, y:100, width:800, height:400 };
        let original = xcap::image::RgbaImage::from_pixel(400, 200, xcap::image::Rgba([255,255,255,255]));
        let mut image = original.clone();
        assert_eq!(mark_cursor(&mut image, &surface, (3840,2160), Some(region), (-1620,50)), Some((200,100)));
        let tip = image.get_pixel(200,100).0;
        assert!(tip[0] < 250 && tip != [255,255,255,255], "剑尖落在热点上: {tip:?}");
        let blade = image.get_pixel(208,107).0;
        assert!(blade[2] > 200 && blade[0] < 200, "剑身自剑尖向右下延伸: {blade:?}");
        assert_eq!(image.get_pixel(190,90), original.get_pixel(190,90), "光晕之外不改动原图");
        assert_eq!(mark_cursor(&mut image, &surface, (3840,2160), Some(region), (-1820,-50)), Some((0,0)));
        let unchanged = image.clone();
        assert_eq!(mark_cursor(&mut image, &surface, (3840,2160), Some(region), (-1821,-50)), None);
        assert_eq!(mark_cursor(&mut image, &surface, (3840,2160), Some(region), (-1420,-50)), None);
        assert_eq!(image, unchanged);
    }
    // Deterministic screenshot pipeline benchmark; no desktop input or live capture.
    #[test]
    #[ignore]
    fn screenshot_pipeline_benchmark() {
        let source = xcap::image::RgbaImage::from_fn(1920, 1080, |x, y| {
            let v = if (x / 120 + y / 24) % 2 == 0 { 245 } else { 45 };
            xcap::image::Rgba([v, v, v, 255])
        });
        let expected = xcap::image::imageops::resize(&source, 1600, 900, xcap::image::imageops::FilterType::Triangle);
        for _ in 0..3 {
            let started = Instant::now();
            let resized = xcap::image::DynamicImage::ImageRgba8(source.clone())
                .resize_exact(1600, 900, xcap::image::imageops::FilterType::Triangle).into_rgba8();
            let resize_ms = started.elapsed().as_millis();
            assert_eq!(resized, expected);
            let started = Instant::now();
            let encoded = crate::visual_guard::encode_jpeg(&resized).unwrap();
            let encode_ms = started.elapsed().as_millis();
            assert_eq!(xcap::image::load_from_memory(&encoded).unwrap().to_rgba8().dimensions(), resized.dimensions());
            eprintln!("screenshot pipeline: resize={resize_ms}ms encode={encode_ms}ms bytes={}", encoded.len());
        }
    }
    #[cfg(windows)]
    #[test]
    fn windows_shortcuts_and_visible_crop() {
        for letter in b'a'..=b'z' {
            let lower = keys(&format!("Ctrl+{}", letter as char)).unwrap();
            let upper = keys(&format!("Ctrl+{}", letter.to_ascii_uppercase() as char)).unwrap();
            assert_eq!(lower, upper);
            assert_eq!(upper, vec![Key::Control, Key::Other(letter.to_ascii_uppercase() as u32)]);
        }
        assert_eq!(keys("Ctrl+Shift+S").unwrap(), vec![Key::Control, Key::Shift, Key::Other(0x53)]);
        assert_eq!(keys("Win+1").unwrap(), vec![Key::Meta, Key::Other(0x31)]);
        let monitor = Surface { id:1, pid:None, x:-100, y:20, width:100, height:80 };
        let window = Surface { x:-80, y:30, width:40, height:30, ..monitor.clone() };
        let image = xcap::image::RgbaImage::from_fn(100, 80, |x, y| xcap::image::Rgba([x as u8, y as u8, 0, 255]));
        let crop = visible_window_crop(&window, &monitor, &image).unwrap();
        assert_eq!(crop.dimensions(), (40,30));
        assert_eq!(crop.get_pixel(0,0), image.get_pixel(20,10));
        assert_eq!(crop.get_pixel(39,29), image.get_pixel(59,39));
        assert!(visible_window_crop(&Surface { x:-101, ..window.clone() }, &monitor, &image).is_err());
        assert!(visible_window_crop(&Surface { x:-10, ..window }, &monitor, &image).is_err());
        assert!(visible_window_crop(&monitor, &monitor, &crop).is_err());
    }
    #[test]
    fn glide_ends_exactly_and_stays_bounded() {
        for (from, to, bend) in [((0, 0), (800, 300), 0.1), ((500, 500), (10, 20), -0.1), ((7, 7), (7, 7), 0.05)] {
            assert_eq!(glide_at(from, to, bend, 0.0), from);
            assert_eq!(glide_at(from, to, bend, 1.0), to);
            let ms = glide_ms(from, to);
            assert!((80.0..=400.0).contains(&ms));
            let d = |p: (i32, i32)| ((to.0 - p.0) as f64).hypot((to.1 - p.1) as f64);
            let path: Vec<_> = (0..=50).map(|i| glide_at(from, to, bend, i as f64 / 50.0)).collect();
            // Approaches the target without overshooting or wandering back.
            assert!(path.windows(2).all(|w| d(w[1]) <= d(w[0]) + 1.0), "{from:?}->{to:?}");
        }
        assert!(glide_ms((0, 0), (2000, 0)) > glide_ms((0, 0), (50, 0)));
    }

    #[test]
    fn waits_are_not_paid_twice() {
        let actions: Vec<Action> = serde_json::from_value(json!([
            {"action":"click","x":10,"y":10}, {"action":"wait","ms":400},
            {"action":"type","text":"test"}, {"action":"wait","ms":500},
            {"action":"move","x":20,"y":20}, {"action":"click","x":20,"y":20},
            {"action":"type","text":"next"}
        ])).unwrap();
        let delays: Vec<_> = (0..actions.len()).map(|i| action_delay(&actions, i).as_millis()).collect();
        assert_eq!(delays, [0, 0, 0, 0, 0, 300, 300]);
        let chained: Vec<Action> = serde_json::from_value(json!([
            {"action":"click","x":10,"y":10}, {"action":"type","text":"a"},
            {"action":"press","key":"Enter"}, {"action":"press","key":"Tab"}
        ])).unwrap();
        let delays: Vec<_> = (0..chained.len()).map(|i| action_delay(&chained, i).as_millis()).collect();
        assert_eq!(delays, [300, 300, 80, 300]);
        for action in ["click", "double_click", "drag", "type", "press", "scroll", "wait", "move"] {
            let batch: Vec<Action> = serde_json::from_value(json!([{ "action": action }])).unwrap();
            assert_eq!(action_delay(&batch, 0).as_millis(), if matches!(action, "move" | "wait") { 0 } else { 300 }, "{action}");
        }
        let explicit: Vec<Action> = serde_json::from_value(json!([{"action":"press","key":"Ctrl+C"},{"action":"wait","ms":400}])).unwrap();
        assert_eq!(action_delay(&explicit, 0), Duration::ZERO);
        assert_eq!(action_delay(&explicit, 1), Duration::ZERO);
        assert!(run("validation".into(), json!({"operation":"screenshot","windowId":1,"monitorId":2})).is_err());
        // Consecutive scrolls at one point must not be blocked by the pre-batch pixel guard.
        let scroll: Action = serde_json::from_value(json!({"action":"scroll","x":1,"y":1,"delta":3})).unwrap();
        assert!(!guarded_pointer(&scroll) && guarded_pointer(&actions[0]));
        assert_eq!(action_delay(&[scroll, chained.into_iter().next().unwrap()], 0).as_millis(), 300);
    }
    #[test]
    fn scroll_frames_respect_feedback_and_skip_trailing_waits() {
        let actions: Vec<Action> = serde_json::from_value(json!([
            {"action":"scroll","x":1,"y":1,"delta":3}, {"action":"wait","ms":400},
            {"action":"scroll","x":1,"y":1,"delta":3}, {"action":"wait","ms":400}
        ])).unwrap();
        for feedback in [None, Some("screenshot"), Some("desktop")] {
            let frames: Vec<_> = (0..actions.len()).filter_map(|i| scroll_frame_index(&actions, i, feedback).map(|s| (i, s))).collect();
            assert_eq!(frames, [(1, 0)]); // Capture after wait; last scroll is covered by the final feedback.
        }
        assert!((0..actions.len()).all(|i| scroll_frame_index(&actions, i, Some("none")).is_none()));
        assert!(scroll_frame_index(&actions[..1], 0, None).is_none());
        let consecutive: Vec<Action> = serde_json::from_value(json!([
            {"action":"scroll"}, {"action":"scroll"}, {"action":"wait"}, {"action":"wait"}, {"action":"press"}
        ])).unwrap();
        let frames: Vec<_> = (0..consecutive.len()).filter_map(|i| scroll_frame_index(&consecutive, i, None).map(|s| (i, s))).collect();
        assert_eq!(frames, [(0, 0), (3, 1)]);
    }

    #[test]
    fn coordinates_keys_and_validation() {
        let shot = Shot { guard: None, overlay:None, desktop_capture:false,
            image_id: "test".into(),
            surface: Surface {
                id: 1,
                pid: None,
                x: -1920,
                y: 0,
                width: 1920,
                height: 1080,
            },
            pixels: (3840, 2160),
            source_pixels: (3840, 2160), region: None,
        };
        let shots = vec![shot.clone()];
        assert_eq!(action_shot(&shots, None).unwrap().image_id, "test");
        assert!(action_shot(&shots, Some("wrong")).is_err());
        assert!(action_shot(&[shot.clone(), shot.clone()], None).is_err());
        assert!(action_shot(&shots, Some("test")).is_ok());
        assert_eq!(point(&shot, Some(1920), Some(1080)).unwrap(), (-960, 540));
        assert!(point(&shot, Some(3840), Some(0)).is_err());
        assert!(point(&shot, Some(-1), Some(0)).is_err());
        assert!(point(&shot, None, Some(0)).is_err());
        let scaled = Shot { guard: None, pixels: (960, 540), ..shot.clone() };
        assert_eq!(point(&scaled, Some(480), Some(270)).unwrap(), (-960, 540));
        assert_eq!(point(&scaled, Some(959), Some(539)).unwrap(), (-2, 1078));
        let desktop = Shot { guard: None, surface: Surface { x:0, y:0, ..shot.surface.clone() }, pixels:(1600,900), ..shot.clone() };
        // The reported click was on the toolbar in the source image, not a DPI offset.
        assert_eq!(point(&desktop, Some(860), Some(520)).unwrap(), (1032, 624));
        let region = Region { x:200, y:100, width:800, height:400 };
        region.validate(shot.source_pixels).unwrap();
        let cropped = Shot { guard: None, region:Some(region), pixels:(400,200), ..shot.clone() };
        assert_eq!(point(&cropped, Some(0), Some(0)).unwrap(), (-1820,50));
        assert_eq!(point(&cropped, Some(200), Some(100)).unwrap(), (-1620,150));
        assert!(point(&cropped, Some(400), Some(0)).is_err());
        assert_eq!(cropped.surface, shot.surface); // Geometry checks still cover the whole window.
        assert!(Region { x:u32::MAX, ..region }.validate(shot.source_pixels).is_err());
        assert!(Region { width:0, ..region }.validate(shot.source_pixels).is_err());
        assert!(Region { y:2100, ..region }.validate(shot.source_pixels).is_err());
        let wait: Action = serde_json::from_value(json!({"action":"wait","ms":8000})).unwrap();
        assert_eq!(validate(&wait, Some(&shot)).unwrap_err(), "ms=8000，允许范围为 0–2000");
        assert!(validate(&serde_json::from_value(json!({"action":"wait","ms":2000})).unwrap(), None).is_ok());
        assert!(validate(&serde_json::from_value(json!({"action":"drag","x":1,"y":1,"toX":2,"toY":2,"glide":true})).unwrap(), Some(&shot)).is_ok());
        assert_eq!(keys("Ctrl+Shift+S").unwrap().len(), 3);
        assert!(keys("Ctrl+unknown").is_err());
        assert!(keys("Ctrl+Shift+Alt+Meta+S").is_err());
        for value in [
            json!({"action":"scroll","x":1,"y":1,"delta":101}),
            json!({"action":"wait","ms":2001}),
            json!({"action":"drag","x":1,"y":1}),
            json!({"action":"scroll","x":1,"y":1,"delta":1,"glide":true}),
        ] {
            assert!(validate(&serde_json::from_value(value).unwrap(), Some(&shot)).is_err());
        }
    }
    #[test]
    fn read_only_requests_ignore_unrelated_fields_but_validate_observation_parameters() {
        let windows = run("validation".into(), json!({"operation":"windows","maxEdge":1,
            "actions":"not an array","region":"not a region","snapshotId":123})).unwrap_err();
        assert!(windows.contains("maxEdge"), "unrelated action/crop fields must not block windows: {windows}");
        let screenshot = run("validation".into(), json!({"operation":"screenshot","actions":"not an array",
            "observe":[],"query":{},"regionSpace":"invalid"})).unwrap_err();
        assert!(screenshot.contains("regionSpace"));
        let crop = run("validation".into(), json!({"operation":"screenshot","region":{"x":-1,"y":0,"width":1,"height":1}})).unwrap_err();
        assert!(crop.contains("u32"), "crop coordinates must still be validated: {crop}");
    }

    #[test]
    fn invalid_wait_rejects_whole_batch_before_input() {
        let shot = Shot { guard: None, overlay:None, desktop_capture:false, image_id:"monitor-1".into(),
            surface:Surface {id:1,pid:None,x:0,y:0,width:100,height:100}, pixels:(100,100), source_pixels:(100,100), region:None };
        *DESKTOP.lock().unwrap() = Some(Snapshot {id:"validation".into(),owner:"validation".into(),
            taken:Instant::now(),window:None,max_edge:1600,foreground:None,shots:vec![shot],invalidated:None,background:false,delivered:vec![]});
        let foreign = run("other-owner".into(), json!({"operation":"act","snapshotId":"old"})).unwrap();
        assert_eq!(foreign["completedActions"], 0);
        assert!(foreign.get("images").is_none());
        assert_eq!(DESKTOP.lock().unwrap().as_ref().unwrap().id, "validation");
        let original_taken = DESKTOP.lock().unwrap().as_ref().unwrap().taken;
        for request in [
            json!({"operation":"act","imageId":"monitor-1","actions":[{"action":"click","x":1,"y":1}]}),
            json!({"operation":"act","snapshotId":"","actions":[{"action":"click","x":1,"y":1}]}),
            json!({"actions":[{"action":"click","x":1,"y":1}]}),
        ] {
            let result = run("validation".into(), request).unwrap();
            assert_eq!(result["errorCode"], "missing_snapshot_id");
            assert_eq!(result["status"], "not_executed");
            assert_eq!(result["completedActions"], 0);
            assert_eq!(result["snapshotId"], "validation");
            assert_eq!(result["nextArguments"], json!({"operation":"act","snapshotId":"validation","imageId":"monitor-1"}));
            assert!(result.get("images").is_none(), "missing parameters must not generate a new screenshot");
            let state = DESKTOP.lock().unwrap();
            assert_eq!(state.as_ref().unwrap().id, "validation");
            assert_eq!(state.as_ref().unwrap().taken, original_taken);
        }
        for _ in 0..3 {
            let result = run("validation".into(), json!({"operation":"act","snapshotId":"old",
                "actions":[{"action":"click","x":1,"y":1}]})).unwrap();
            assert_eq!(result["errorCode"], "stale_snapshot_id");
            assert_eq!(result["status"], "not_executed");
            assert_eq!(result["completedActions"], 0);
            assert_eq!(result["snapshotPreserved"], true);
            assert_eq!(result["nextArguments"], json!({"operation":"act","snapshotId":"validation","imageId":"monitor-1"}));
            assert!(result.get("images").is_none(), "stale IDs must not churn screenshots");
            let state = DESKTOP.lock().unwrap();
            assert_eq!(state.as_ref().unwrap().id, "validation");
            assert_eq!(state.as_ref().unwrap().taken, original_taken);
        }
        for (owner, invalidated, expired, background) in [("other-owner", false, false, false),
            ("validation", true, false, false), ("validation", false, true, false), ("validation", false, false, true)] {
            {
                let mut state = DESKTOP.lock().unwrap();
                let snap = state.as_mut().unwrap();
                snap.invalidated = invalidated.then(|| "changed by browser".into());
                snap.taken = if expired { original_taken - Duration::from_secs(181) } else { original_taken };
                snap.background = background;
            }
            let result = run(owner.into(), json!({"operation":"act","actions":[{"action":"click","x":1,"y":1}]})).unwrap();
            assert_eq!(result["completedActions"], 0);
            assert!(result.get("snapshotId").is_none());
            assert!(result.get("nextArguments").is_none());
        }
        {
            let mut state = DESKTOP.lock().unwrap();
            let snap = state.as_mut().unwrap();
            snap.invalidated = None; snap.taken = original_taken; snap.background = false;
        }
        // Reproduce the user's FIRST call: correct snapshotId but omitted operation. An invalid
        // wait prevents real input while proving it reaches the shared act validation path.
        let result = run("validation".into(), json!({"snapshotId":"validation",
            "imageId":"monitor-1","actions":[{"action":"click","x":1,"y":1},{"action":"wait","ms":8000}]})).unwrap();
        assert_eq!(result["operation"], "act");
        assert_eq!(result["status"], "not_executed");
        assert_eq!(result["completedActions"], 0);
        assert!(result["error"].as_str().unwrap().contains("actions[1].ms=8000"));
        let mut actions = vec![json!({"action":"wait","ms":0});16];
        actions[15] = json!({"action":"wait","ms":8000});
        let result = run("validation".into(), json!({"operation":"act","snapshotId":"validation",
            "imageId":"monitor-1","actions":actions})).unwrap();
        assert_eq!(result["completedActions"], 0);
        assert!(result["error"].as_str().unwrap().contains("actions[15].ms=8000"));
        actions.push(json!({"action":"wait","ms":0}));
        let result = run("validation".into(), json!({"operation":"act","snapshotId":"validation",
            "imageId":"monitor-1","actions":actions})).unwrap();
        assert_eq!(result["status"], "not_executed");
        assert_eq!(result["completedActions"], 0);
        assert!(result["error"].as_str().unwrap().contains("1至16"));
        // Pure waits use no input lease or native input API, do not consume the image, and
        // accept absent/old IDs even after cropping, expiry or browser invalidation.
        {
            let mut state = DESKTOP.lock().unwrap();
            let snap = state.as_mut().unwrap();
            snap.invalidated = Some("changed by browser".into());
            snap.taken = original_taken - Duration::from_secs(181);
        }
        let _input_busy = lease_input("wait-test").unwrap();
        for snapshot_id in [Value::Null, json!("before-crop")] {
            let result = run("validation".into(), json!({"operation":"act","snapshotId":snapshot_id,
                "imageId":"before-crop","feedback":"none","actions":[{"action":"wait","ms":0}]})).unwrap();
            assert_eq!(result["status"], "executed");
            assert_eq!(result["inputAttempted"], false);
            assert_eq!(result["completedActions"], 1);
            assert_eq!(DESKTOP.lock().unwrap().as_ref().unwrap().id, "validation");
        }
        let foreign = run("other-owner".into(), json!({"operation":"act","feedback":"none","actions":[{"action":"wait","ms":0}]})).unwrap();
        assert_eq!(foreign["status"], "not_executed");
        DESKTOP.lock().unwrap().as_mut().unwrap().background = true;
        let wrong_mode = run("validation".into(), json!({"operation":"act","feedback":"none","actions":[{"action":"wait","ms":0}]})).unwrap();
        assert_eq!(wrong_mode["status"], "not_executed");
        assert!(DESKTOP.lock().unwrap().take().is_some());
        let wait = run("validation".into(), json!({"operation":"act","feedback":"none","actions":[{"action":"wait","ms":0}]})).unwrap();
        assert_eq!(wait["status"], "executed");
        assert!(DESKTOP.lock().unwrap().is_none());
        let invalid = run("validation".into(), json!({"operation":"act","actions":[{"action":"wait","ms":-1}]})).unwrap();
        assert_eq!(invalid["status"], "not_executed");
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "Moves the pointer across all monitors; use an idle test desktop"]
    fn pointer_reaches_each_monitor() {
        use windows_sys::Win32::UI::HiDpi::{SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
        // The app runs Per-Monitor V2 (tao); a bare test process would see virtualized coordinates instead.
        assert!(!unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) }.is_null());
        let mut enigo = Enigo::new(&Settings::default()).unwrap();
        let before = enigo.location().unwrap();
        let checked = (|| -> Result<()> {
            for monitor in Monitor::all().map_err(err)? {
                let s = monitor_surface(&monitor)?;
                for (dx, dy) in [(0, 0), (s.width / 2, s.height / 2), (s.width - 1, s.height - 1)] {
                    for input_event in [false, true] {
                        let expected = (s.x + dx as i32, s.y + dy as i32);
                        move_pointer(&mut enigo, expected.0, expected.1, input_event)?;
                        std::thread::sleep(Duration::from_millis(20)); // let a queued SendInput land first
                        let actual = enigo.location().map_err(err)?;
                        if !pointer_matches(expected, actual) {
                            return Err(format!("monitor {} input_event={input_event}: expected {expected:?}, actual {actual:?}", s.id));
                        }
                    }
                }
            }
            Ok(())
        })();
        let restored = move_pointer(&mut enigo, before.0, before.1, false);
        checked.unwrap();
        restored.unwrap();
    }

    // Opt-in real desktop smoke test: captures and moves only the pointer; never clicks/types into user applications.
    #[test]
    #[ignore]
    fn desktop_smoke() {
        #[cfg(windows)]
        {
            let before = foreground().unwrap();
            let expected = Window::all().unwrap().into_iter()
                .find(|w| w.is_focused().unwrap_or(false))
                .map(|w| (w.id().unwrap(), w.pid().unwrap()));
            let after = foreground().unwrap();
            // Shell surfaces may be focused but excluded by XCap's application-window filter.
            if expected.is_some() && before == after { assert_eq!(after, expected); }
        }
        // A departed target must yield an actionable desktop, not its old window image.
        let mut observation = json!({});
        let mut snapshot = None;
        observe("test", Some(u32::MAX), None, true, 1600, false, &mut snapshot, &mut observation);
        assert!(observation["windowId"].is_null());
        assert!(snapshot.is_some(), "{observation}");
        assert!(snapshot.unwrap().window.is_none());
        let mut focused_result = json!({});
        let mut focused_snapshot = None;
        observe("test", None, None, false, 1600, false, &mut focused_snapshot, &mut focused_result);
        assert!(focused_snapshot.is_some(), "{focused_result}");
        if let Some(id) = focused_result["windowId"].as_u64() {
            assert_eq!(focused_result["foreground"][0], id);
            let cropped = run("test".into(), json!({"operation":"screenshot","windowId":id,
                "maxEdge":0,"region":{"x":0,"y":0,"width":32,"height":32}})).unwrap();
            assert_eq!(cropped["images"][0]["width"], 32);
            assert_eq!(cropped["images"][0]["height"], 32);
            let _ = std::fs::remove_file(cropped["images"][0]["path"].as_str().unwrap());
        }
        let shot = run("test".into(), json!({"operation":"screenshot"})).unwrap();
        assert!(!shot["images"].as_array().unwrap().is_empty());
        let args = json!({"operation":"act","snapshotId":shot["snapshotId"],"imageId":shot["images"][0]["imageId"],"feedback":"desktop","actions":[{"action":"move","x":10,"y":10}]});
        let result = run("test".into(), args.clone()).unwrap();
        assert_eq!(result["status"], "executed");
        assert_eq!(result["images"].as_array().unwrap().len(), 1);
        assert_eq!(result["images"][0]["imageId"], shot["images"][0]["imageId"]);
        #[cfg(windows)]
        {
            let cursor = &result["images"][0]["cursor"];
            assert!((cursor["x"].as_i64().expect("stationary system cursor") - 10).abs() <= 1);
            assert!((cursor["y"].as_i64().unwrap() - 10).abs() <= 1);
        }
        eprintln!("single-screen feedback timings: {}", result["timingsMs"]);
        let stale = run("test".into(), args).unwrap();
        assert_eq!(stale["status"], "not_executed");
        assert_eq!(stale["completedActions"], 0);
        assert!(stale.get("images").is_none());
        assert_eq!(stale["snapshotId"], result["snapshotId"]);
        assert_eq!(stale["snapshotPreserved"], true);
        // Simulate a stale foreground identity without actually changing the user's focus.
        DESKTOP.lock().unwrap().as_mut().unwrap().foreground = Some((u32::MAX, u32::MAX));
        let recovered = run("test".into(), json!({"operation":"act","snapshotId":result["snapshotId"],
            "imageId":result["images"][0]["imageId"],"feedback":"none",
            "actions":[{"action":"move","x":10,"y":10}]})).unwrap();
        assert_eq!(recovered["status"], "not_executed");
        assert_eq!(recovered["completedActions"], 0);
        assert_ne!(recovered["snapshotId"], result["snapshotId"]);
        assert!(!recovered["images"].as_array().unwrap().is_empty());
        if let Ok(id) = std::env::var("NOVA_JIANLAI_TEST_WINDOW") {
            // Only target an explicitly supplied disposable test window; never type into an arbitrary desktop app.
            let id: u32 = id.parse().unwrap();
            let frame = run(
                "test".into(),
                json!({"operation":"screenshot","windowId":id}),
            )
            .unwrap();
            let input = run(
                "test".into(),
                json!({"operation":"act","snapshotId":frame["snapshotId"],
                "imageId":frame["images"][0]["imageId"],"actions":[{"action":"click","x":40,"y":40},
                    {"action":"type","text":"jianlai"},{"action":"press","key":"Enter"}]}),
            )
            .unwrap();
            assert_eq!(input["status"], "executed", "{input}");
            assert_eq!(input["completedActions"], 3);
            assert!(input["images"].as_array().is_some());
            for result in [frame, input] {
                for img in result["images"].as_array().unwrap() {
                    let _ = std::fs::remove_file(img["path"].as_str().unwrap());
                }
            }
        }
        for result in [observation, focused_result, shot, result, stale, recovered] {
            for img in result["images"].as_array().into_iter().flatten() {
                let _ = std::fs::remove_file(img["path"].as_str().unwrap());
            }
        }
    }
}
