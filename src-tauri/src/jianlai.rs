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
mod dxgi;
#[cfg(target_os = "linux")]
mod x11;
mod nova_regions;
#[cfg(windows)]
mod text_input;
#[cfg(windows)]
mod uia;

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
pub(crate) fn desktop_owner() -> Option<String> { DESKTOP_OWNER.lock().unwrap().clone() }
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
#[cfg(test)]
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
            return Err("region超出所选图片范围；regionSpace=image用图内像素，source用originalWidth/originalHeight原始像素".into());
        }
        Ok(())
    }
}
/// A single click whose neighbourhood did not change. The next single click on that same
/// image, within a few pixels, is refused so the model re-aims instead of poking again.
#[derive(Clone)]
struct Miss {
    image_id: String,
    x: i32,
    y: i32,
}

/// Window rectangle sampled before activation, in screen pixels. Used only on Windows
/// to tell a real foreground swap from a DXGI frame that is still the previous app.
struct WatchWindow {
    rect: (i32, i32, u32, u32),
    before: Vec<[u8; 3]>,
}

struct Snapshot {
    id: String,
    owner: String,
    taken: Instant,
    window: Option<u32>,
    /// Edge used to fit the images in this snapshot. A crop may upscale with its own edge.
    max_edge: u32,
    /// Edge for the next full-screen feedback. A crop must not replace it.
    desktop_edge: u32,
    foreground: Option<(u32, u32)>,
    shots: Vec<Shot>,
    invalidated: Option<String>,
    /// 后台截图：act 必须同样带 background，两种模式的快照互不通用。
    background: bool,
    /// What the model actually received per image, so unchanged repeats are not resent.
    delivered: Vec<Delivered>,
    missed: Option<Miss>,
}

use crate::visual_guard::Delivered;
#[cfg(test)]
use crate::visual_guard::changed_tiles;

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
    /// 控件观察（elements）专用：x/y 是该 imageId 内的图片像素点。
    x: Option<i32>,
    y: Option<i32>,
    depth: Option<u32>,
    max_items: Option<usize>,
    filter: Option<String>,
    /// act 的坐标空间：image（默认，图片像素）/ screen（屏幕物理像素，UIA 控件矩形可直接用）。
    coordinate_space: Option<String>,
}

fn bind_input_snapshot<'a>(requested: &mut Option<String>, snapshot: Option<&'a Snapshot>, owner: &str, background: bool) -> Option<&'a Snapshot> {
    let current = snapshot.filter(|snap| snap.owner == owner && snap.background == background
        && snap.invalidated.is_none() && snap.taken.elapsed() <= Duration::from_secs(180));
    if requested.as_deref().is_none_or(|id| id.trim().is_empty()) {
        *requested = current.map(|snap| snap.id.clone());
    }
    current
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
/// Prefer the cursor's desktop rect over the one a capture library reports.
/// Windows xcap uses `DEVMODE.dmPosition`, which disagrees with `GetMonitorInfo.rcMonitor`
/// (the space of `GetCursorPos`, `SetCursorPos`, and DXGI `DesktopCoordinates`) on mixed-DPI layouts.
fn desktop_rect(reported: (i32, i32, u32, u32), cursor_space: Option<(i32, i32, u32, u32)>) -> (i32, i32, u32, u32) {
    match cursor_space {
        Some(rect) if rect.2 > 0 && rect.3 > 0 => rect,
        _ => reported,
    }
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
    let reported = (
        (m.x().map_err(err)? as f64 * scale).round() as i32,
        (m.y().map_err(err)? as f64 * scale).round() as i32,
        (m.width().map_err(err)? as f64 * scale).round() as u32,
        (m.height().map_err(err)? as f64 * scale).round() as u32,
    );
    #[cfg(windows)]
    let cursor = m.name().ok().and_then(|name| cursor_space_rect(&name));
    #[cfg(not(windows))]
    let cursor = None;
    let (x, y, width, height) = desktop_rect(reported, cursor);
    Ok(Surface { id: m.id().map_err(err)?, pid: None, x, y, width, height })
}
/// `GetMonitorInfo.rcMonitor` for this device, in the calling thread's DPI space.
/// Jianlai calls this under Per-Monitor V2, so the rect is physical pixels.
#[cfg(windows)]
fn cursor_space_rect(device: &str) -> Option<(i32, i32, u32, u32)> {
    use windows_sys::Win32::{
        Foundation::RECT,
        Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW},
    };
    struct Slot { name: String, rect: Option<(i32, i32, u32, u32)> }
    // windows-sys 0.61 types LPARAM as isize, not the newtype the `windows` crate uses.
    unsafe extern "system" fn each(monitor: HMONITOR, _: HDC, _: *mut RECT, data: isize) -> i32 {
        let slot = &mut *(data as *mut Slot);
        let mut info: MONITORINFOEXW = std::mem::zeroed();
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        if GetMonitorInfoW(monitor, &mut info as *mut MONITORINFOEXW as *mut MONITORINFO) != 0 {
            let len = info.szDevice.iter().position(|&c| c == 0).unwrap_or(info.szDevice.len());
            let name = String::from_utf16_lossy(&info.szDevice[..len]);
            if name.eq_ignore_ascii_case(&slot.name) {
                let r = info.monitorInfo.rcMonitor;
                let (w, h) = (r.right.saturating_sub(r.left), r.bottom.saturating_sub(r.top));
                if w > 0 && h > 0 { slot.rect = Some((r.left, r.top, w as u32, h as u32)); }
            }
        }
        1
    }
    let mut slot = Slot { name: device.to_string(), rect: None };
    unsafe {
        EnumDisplayMonitors(std::ptr::null_mut(), std::ptr::null(), Some(each), &mut slot as *mut Slot as isize);
    }
    slot.rect
}
/// BitBlt of a cursor-space rect. Used when xcap's DEVMODE origin is not the cursor origin,
/// so the fallback image is the same rectangle the click will target.
#[cfg(windows)]
fn capture_screen_rect(x: i32, y: i32, width: u32, height: u32) -> Result<xcap::image::RgbaImage> {
    use windows_sys::Win32::{
        Graphics::Gdi::*,
        UI::WindowsAndMessaging::GetDesktopWindow,
    };
    if width == 0 || height == 0 || width > 16_384 || height > 16_384 {
        return Err("截图范围无效".into());
    }
    unsafe {
        let desktop = GetDesktopWindow();
        let dc = GetWindowDC(desktop);
        if dc.is_null() { return Err("无法读取桌面".into()); }
        let mem = CreateCompatibleDC(dc);
        let bitmap = CreateCompatibleBitmap(dc, width as i32, height as i32);
        if mem.is_null() || bitmap.is_null() {
            if !bitmap.is_null() { DeleteObject(bitmap); }
            if !mem.is_null() { DeleteDC(mem); }
            ReleaseDC(desktop, dc);
            return Err("无法创建桌面缓冲".into());
        }
        let old = SelectObject(mem, bitmap);
        let copied = BitBlt(mem, 0, 0, width as i32, height as i32, dc, x, y, SRCCOPY);
        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width as i32,
                biHeight: -(height as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut buffer = vec![0u8; width as usize * height as usize * 4];
        let rows = GetDIBits(mem, bitmap, 0, height, buffer.as_mut_ptr().cast(), &mut info, DIB_RGB_COLORS);
        SelectObject(mem, old);
        DeleteObject(bitmap);
        DeleteDC(mem);
        ReleaseDC(desktop, dc);
        if copied == 0 || rows == 0 {
            return Err(format!("桌面截图失败：{}", std::io::Error::last_os_error()));
        }
        for px in buffer.chunks_exact_mut(4) {
            px.swap(0, 2);
            px[3] = 255;
        }
        xcap::image::RgbaImage::from_raw(width, height, buffer).ok_or_else(|| "截图像素无效".into())
    }
}
/// Desktop Duplication on Windows; GDI when DXGI cannot serve this monitor.
/// The rectangle is the cursor-space monitor rect, not xcap's DEVMODE origin.
/// `fresh` is the cache generation from before an action. A cache that has not moved
/// since then is the previous picture, so the screen is read directly instead.
/// The status is `cached` for an ordinary shot, `new` when a present arrived after
/// the action, and `live` when the cache was still the pre-action frame.
fn capture_monitor(m: &Monitor, fresh: Option<u64>, watch: Option<&WatchWindow>) -> Result<(xcap::image::RgbaImage, &'static str)> {
    #[cfg(windows)]
    {
        let s = monitor_surface(m)?;
        let live = |s: &Surface| capture_screen_rect(s.x, s.y, s.width, s.height);
        if let Some(epoch) = fresh {
            match dxgi::grab_fresh(s.x, s.y, s.width, s.height, epoch) {
                Ok((frame, true)) => {
                    let still_old = watch.is_some_and(|w| !w.before.is_empty() && {
                        let after = window_samples_on(&frame, &s, w.rect);
                        !window_painted(&w.before, &after)
                    });
                    if still_old {
                        // The foreground hwnd can move before DWM has put that window's pixels
                        // in the duplication cache. One short wait, then read the composed screen.
                        precise_sleep(Duration::from_millis(80));
                        if let Ok(image) = live(&s) {
                            return Ok((image, "live"));
                        }
                    }
                    return Ok((frame, "new"));
                }
                Ok((_, false)) => {
                    if let Ok(image) = live(&s) {
                        return Ok((image, "live"));
                    }
                }
                Err(dxgi::GrabError::Quiet) => {}
                Err(dxgi::GrabError::Loud(error)) => eprintln!("[jianlai] DXGI截图失败，改用 GDI：{error}"),
            }
        } else {
            match dxgi::grab(s.x, s.y, s.width, s.height) {
                Ok(image) => return Ok((image, "cached")),
                Err(dxgi::GrabError::Quiet) => {}
                Err(dxgi::GrabError::Loud(error)) => eprintln!("[jianlai] DXGI截图失败，改用 GDI：{error}"),
            }
        }
        let reported = (m.x().map_err(err)?, m.y().map_err(err)?, m.width().map_err(err)?, m.height().map_err(err)?);
        if reported == (s.x, s.y, s.width, s.height) {
            return m.capture_image().map(|image| (image, "fallback")).map_err(err);
        }
        eprintln!(
            "[jianlai] 显示器 DEVMODE 原点 ({},{}) {}x{} 与光标坐标 ({},{}) {}x{} 不一致，按光标坐标截取",
            reported.0, reported.1, reported.2, reported.3, s.x, s.y, s.width, s.height
        );
        return live(&s).map(|image| (image, "fallback"));
    }
    #[cfg(not(windows))]
    {
        let _ = (fresh, watch);
        m.capture_image().map(|image| (image, "cached")).map_err(err)
    }
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
    #[cfg(target_os = "linux")]
    {
        // XGetImage does not include the cursor sprite. Read it from the same server the shot came from.
        let display = x11::observation_display(background::active())?;
        return x11::pointer(&display);
    }
    #[cfg(not(target_os = "linux"))]
    None
}

fn cursor_tip(surface: &Surface, source: (u32, u32), region: Option<Region>, position: (i32, i32), image_size: (u32, u32)) -> Option<(u32, u32)> {
    if surface.width == 0 || surface.height == 0 || source.0 == 0 || source.1 == 0 || image_size.0 == 0 || image_size.1 == 0 { return None; }
    let r = region.unwrap_or(Region { x:0, y:0, width:source.0, height:source.1 });
    if r.validate(source).is_err() { return None; }
    let x = (position.0 as i128 - surface.x as i128) * source.0 as i128 - r.x as i128 * surface.width as i128;
    let y = (position.1 as i128 - surface.y as i128) * source.1 as i128 - r.y as i128 * surface.height as i128;
    let width = r.width as i128 * surface.width as i128;
    let height = r.height as i128 * surface.height as i128;
    if x < 0 || y < 0 || x >= width || y >= height { return None; }
    // Map the desktop pixel's centre (like point()), so a click at image (x,y) is marked at (x,y), not one pixel up-left.
    let to_image = |v: i128, pixel: u32, size: i128, edge: u32| ((2 * v + pixel as i128) * edge as i128 / (2 * size)).min(edge as i128 - 1) as i32;
    let (x, y) = (to_image(x, source.0, width, image_size.0), to_image(y, source.1, height, image_size.1));
    Some((x as u32, y as u32))
}

/// Image rectangle covered by the 32×32 sword whose tip is `tip`. Both the old and the new tip
/// are rewritten when only the pointer moved.
fn marker_rect(tip: Option<(u32, u32)>, size: (u32, u32)) -> Option<(u32, u32, u32, u32)> {
    let (x, y) = tip?;
    let hot = POINTER_TIP as i32;
    let (x0, y0) = ((x as i32 - hot).max(0) as u32, (y as i32 - hot).max(0) as u32);
    let x1 = (x as i32 - hot + 32).clamp(0, size.0 as i32) as u32;
    let y1 = (y as i32 - hot + 32).clamp(0, size.1 as i32) as u32;
    (x1 > x0 && y1 > y0).then_some((x0, y0, x1 - x0, y1 - y0))
}

fn mark_cursor(image: &mut xcap::image::RgbaImage, surface: &Surface, source: (u32, u32), region: Option<Region>, position: (i32, i32)) -> Option<(u32, u32)> {
    let (x, y) = cursor_tip(surface, source, region, position, image.dimensions())?;
    let (x, y) = (x as i32, y as i32);
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

/// Target pixel size for a shot. `scale != 1` still goes through Triangle, matching the historical resize.
/// The result stays inside the vision budget `fit_image` uses, so the model is not shown a second,
/// silent downscale whose pixels no longer match `width`/`height`.
fn fit_target(width: u32, height: u32, max_edge: u32, allow_upscale: bool) -> (f64, u32, u32) {
    let long = width.max(height) as f64;
    let requested = if max_edge > 0 && (allow_upscale || long > max_edge as f64) { max_edge as f64 / long } else { 1.0 };
    let scale = requested.min(crate::lyra::bridge::vision_scale(width as f64, height as f64));
    if scale == 1.0 || !scale.is_finite() {
        return (scale, width, height);
    }
    let mut tw = (width as f64 * scale).round().max(1.0) as u32;
    let mut th = (height as f64 * scale).round().max(1.0) as u32;
    while tw > 1 && th > 1 && (tw.max(th) > 1568 || (tw as u64) * (th as u64) > 1_150_000) {
        if tw >= th { tw -= 1; } else { th -= 1; }
    }
    (scale, tw, th)
}

/// A region capture keeps the session's full-screen edge. A full capture adopts the edge it used.
fn next_desktop_edge(previous: Option<u32>, requested: u32, region: bool) -> u32 {
    if region { previous.unwrap_or(1600) } else { requested }
}

/// Full-screen feedback uses the session edge unless this call explicitly asked for another.
fn feedback_edge(requested: Option<u32>, desktop_edge: u32) -> u32 {
    requested.unwrap_or(desktop_edge)
}

const BLIND_REPEAT_PX: i32 = 8;

fn blocks_blind_repeat(action: &str, miss: Option<&Miss>, image_id: &str, x: i32, y: i32) -> bool {
    let Some(miss) = miss else { return false };
    action == "click" && miss.image_id == image_id
        && (miss.x - x).abs() <= BLIND_REPEAT_PX && (miss.y - y).abs() <= BLIND_REPEAT_PX
}

/// Samples inside `rect` (screen pixels) on a monitor image whose top-left is `surface`.
fn window_samples_on(image: &xcap::image::RgbaImage, surface: &Surface, rect: (i32, i32, u32, u32)) -> Vec<[u8; 3]> {
    if surface.width == 0 || surface.height == 0 || image.width() == 0 || image.height() == 0 {
        return Vec::new();
    }
    let x0 = (rect.0 as i64 - surface.x as i64) * image.width() as i64 / surface.width as i64;
    let y0 = (rect.1 as i64 - surface.y as i64) * image.height() as i64 / surface.height as i64;
    let x1 = x0 + rect.2 as i64 * image.width() as i64 / surface.width as i64;
    let y1 = y0 + rect.3 as i64 * image.height() as i64 / surface.height as i64;
    let (w, h) = (image.width() as i64, image.height() as i64);
    let (x0, y0) = (x0.clamp(0, w), y0.clamp(0, h));
    let (x1, y1) = (x1.clamp(0, w), y1.clamp(0, h));
    if x1 <= x0 || y1 <= y0 { return Vec::new(); }
    let step = 32i64;
    let mut out = Vec::new();
    let mut y = y0;
    while y < y1 {
        let mut x = x0;
        while x < x1 {
            let p = image.get_pixel(x as u32, y as u32);
            out.push([p[0], p[1], p[2]]);
            x += step;
        }
        y += step;
    }
    out
}

/// A window activation counts only when its own rectangle changed, not a taskbar blip elsewhere.
fn window_painted(before: &[[u8; 3]], after: &[[u8; 3]]) -> bool {
    if before.is_empty() || before.len() != after.len() { return false; }
    let changed = before.iter().zip(after).filter(|(a, b)| {
        a.iter().zip(b.iter()).any(|(p, q)| p.abs_diff(*q) > 16)
    }).count();
    let need = if before.len() < 4 { 1 } else { (before.len() / 20).max(4) };
    changed >= need
}

fn click_unchanged(before: &Delivered, after: &Delivered, x: f64, y: f64) -> bool {
    if before.stamp == 0 || before.stamp != after.stamp || before.basis != after.basis {
        return false;
    }
    let (Some(old), Some(new)) = (before.clean.as_ref(), after.clean.as_ref()) else { return false };
    if old.dimensions() != new.dimensions() { return false; }
    !crate::visual_guard::VisualGuard::new(old).changed_near(new, x, y)
}

const COORDINATE_NOTE: &str = "x/y 是本次 images 里对应 imageId 的 pixelWidth×pixelHeight，从该图左上角算起。region 非空时不要再加 region 偏移，也不要换算成 originalWidth、desktopBounds 或另一张图的像素。局部图的 maxEdge 只决定这一张的大小；随后的整屏反馈仍用 desktopEdge，不会跟着这次的 maxEdge 变小。landed.at 是本次点击在反馈图里的像素，直接读它，不要手算。";

const EFFECT_NOTE: &str = "这次点击没有改变落点附近，整屏也没有新的一块画面。images 里多了一张落点附近的放大图，它有自己的 imageId 和像素坐标。请按那张图重新选一个不同的点；不要在同一张整屏图的同一像素附近再单击。双击仍会执行。工具不会自动再点一次，也不会替你重试可能发送的动作。";

fn resize_shot(image: xcap::image::RgbaImage, max_edge: u32, allow_upscale: bool) -> xcap::image::RgbaImage {
    let (width, height) = image.dimensions();
    let (scale, tw, th) = fit_target(width, height, max_edge, allow_upscale);
    if scale != 1.0 {
        xcap::image::DynamicImage::ImageRgba8(image)
            .resize_exact(tw, th, xcap::image::imageops::FilterType::Triangle)
            .into_rgba8()
    } else {
        image
    }
}

/// Same JSON basis `deliver_frame` stores, so an identical source can skip resize and encode.
fn observation_basis(surface: &Surface, original: (u32, u32), ignored: &[crate::visual_guard::PixelRect]) -> Value {
    let mut entry = json!({
        "desktopBounds": {"x": surface.x, "y": surface.y, "width": surface.width, "height": surface.height},
        "originalWidth": original.0,
        "originalHeight": original.1,
    });
    if !ignored.is_empty() {
        entry["ignoredRegions"] = json!(ignored);
    }
    json!([entry["desktopBounds"], entry["originalWidth"], entry["originalHeight"], entry["ignoredRegions"]])
}

/// Capture once; animations and loading indicators are for the model to interpret, not a wait gate.
fn sample_image(grab: impl FnOnce() -> Result<xcap::image::RgbaImage>, desktop_surface: Option<&Surface>)
    -> Result<(xcap::image::RgbaImage, Value, Option<crate::taskbar_status::Rect>)> {
    let started = Instant::now();
    let relevant_overlay = || desktop_surface.and_then(|s| overlay_on_surface(s, overlay_bounds(true)));
    let overlay = relevant_overlay();
    let image = grab()?;
    // A changing self-UI overlay must not invalidate an unrelated screen observation.
    let overlay = overlay.filter(|bounds| Some(*bounds) == relevant_overlay());
    let report = json!({"status":"not_checked","samples":1,"elapsedMs":started.elapsed().as_millis()});
    Ok((image, report, overlay))
}

fn capture(owner: &str, window_id: Option<u32>, monitor_id: Option<u32>, region: Option<Region>, max_edge: u32, fresh: Option<u64>, watch: Option<&WatchWindow>, state: &mut Option<Snapshot>) -> Result<Value> {
    let _activity = crate::taskbar_status::activity("截取画面");
    let started = Instant::now();
    let mut capture_ms = 0;
    let mut resize_ms = 0;
    let mut encode_ms = 0;
    let mut reused_frames = 0u32;
    let bg = background::active();
    // Ordinary desktop observations always use visible monitor pixels. A window only locates a monitor;
    // private background seats / process-targeted input retain their separate isolation boundary.
    let (window_id, monitor_id) = if bg { (window_id, monitor_id) } else {
        (None, match window_id {
            Some(id) => Some(window(id)?.current_monitor().map_err(err)?.id().map_err(err)?),
            None => monitor_id,
        })
    };
    // Only this session's recent, same-mode observation counts as something the model has already seen.
    let previous = state.as_ref().filter(|s| s.owner == owner && s.background == bg && s.taken.elapsed() <= Duration::from_secs(180))
        .map(|s| s.delivered.clone()).unwrap_or_default();
    let mut delivered: Vec<Delivered> = Vec::new();
    // Independent stream per monitor (or explicitly isolated background window); zooms stay ungrouped.

    if bg { background::check_scope(window_id.is_some())?; }
    let id = uuid::Uuid::new_v4().to_string();
    let sequence = OBSERVATION_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let folder = shot_folder(owner, "desktop-shots");
    std::fs::create_dir_all(&folder).map_err(err)?;
    let before = foreground()?;
    let cursor_before = cursor_position();
    let nova_bounds = if !bg && region.is_none() { nova_regions::visible() } else { Vec::new() };
    let desktop_edge = next_desktop_edge(
        state.as_ref().filter(|s| s.owner == owner && s.background == bg).map(|s| s.desktop_edge),
        max_edge,
        region.is_some(),
    );
    let mut shots = Vec::new();
    let mut images = Vec::new();
    let target_painted = std::cell::Cell::new(None);
    let mut save = |surface: Surface, image: xcap::image::RgbaImage, stability: Value, overlay, desktop_capture| -> Result<()> {
        if region.is_none() {
            if let Some(watch) = watch {
                if !watch.before.is_empty() {
                    let after = window_samples_on(&image, &surface, watch.rect);
                    target_painted.set(Some(window_painted(&watch.before, &after)));
                }
            }
        }
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
        let ignored_bounds = if desktop_capture && region.is_none() {
            // Mask only stable, actually visible Nova regions; never hide a foreign window above Nova.
            // Butterfly children are included by Z order, independently of the input-guard hit test.
            nova_regions::stable(&nova_bounds, &nova_regions::visible())
        } else { Vec::new() };
        // Retain real source pixels for safety; ignored UI cannot be clicked using a stale retained image.
        let source_ignored: Vec<_> = ignored_bounds.iter().filter_map(|r| overlay_region(&surface, original_pixels, Some(*r))).collect();
        let image = if let Some(r) = region {
            r.validate(original_pixels)?;
            xcap::image::imageops::crop_imm(&image, r.x, r.y, r.width, r.height).to_image()
        } else { image };
        let (_, fit_w, fit_h) = fit_target(image.width(), image.height(), max_edge, region.is_some());
        let ignored: Vec<_> = ignored_bounds.iter().filter_map(|r| overlay_region(&surface, (fit_w, fit_h), Some(*r))).collect();
        let stream = region.is_none().then(|| image_id.clone());
        let basis = observation_basis(&surface, original_pixels, &ignored);
        // Hash the masked comparison image, not the pixels Nova itself is animating.
        let stamp = if stream.is_some() {
            crate::visual_guard::frame_stamp_masked(&image, &source_ignored)
        } else {
            0
        };
        let cursor_at = |size: (u32, u32)| cursor_before.filter(|p| Some(*p) == cursor_position())
            .filter(|_| window_id.is_none() || before.map(|f| f.0) == window_id)
            .and_then(|p| cursor_tip(&surface, original_pixels, region, p, size));
        // Byte-identical source (DXGI cache hit, or a repeated static grab) is the same picture.
        // The sword tip is not part of that hash. Reuse the file only when the tip is the same;
        // a moved pointer gets a new image drawn on the already-resized frame (no second resize).
        if let Some(stream) = &stream {
            if stamp != 0 {
                if let Some(prior) = previous.iter().find(|p| &p.stream == stream && p.stamp == stamp && p.basis == basis && p.seen.dimensions() == (fit_w, fit_h)) {
                    let cursor = cursor_at((fit_w, fit_h));
                    let mut entry = json!({"imageId":&image_id,"width":fit_w,"height":fit_h,
                        "pixelWidth":fit_w,"pixelHeight":fit_h,
                        "snapshotId":id,"capturedAt":captured_at,"stability":stability,
                        "originalWidth":original_pixels.0,"originalHeight":original_pixels.1,
                        "cursor":cursor.map(|(x,y)| json!({"x":x,"y":y,"marker":"sword-tip","source":"system-pointer"})),
                        "region":region.map(|r| json!({"x":r.x,"y":r.y,"width":r.width,"height":r.height})),
                        "desktopBounds":{"x":surface.x,"y":surface.y,"width":surface.width,"height":surface.height}});
                    entry["monitorId"] = json!(window_id.is_none().then_some(surface.id));
                    if !ignored.is_empty() { entry["ignoredRegions"] = json!(ignored); }
                    if prior.marker == cursor {
                        entry["unchanged"] = json!(true);
                        entry["path"] = json!(prior.path);
                        entry["frameStream"] = json!(stream);
                        let mut kept = prior.clone();
                        kept.stamp = stamp;
                        delivered.push(kept);
                        reused_frames += 1;
                        images.push(entry);
                        shots.push(Shot {
                            image_id: image_id.clone(), surface, pixels: (fit_w, fit_h), source_pixels: original_pixels,
                            region, overlay, desktop_capture,
                        });
                        return Ok(());
                    }
                    if prior.clean.as_ref().is_some_and(|c| c.dimensions() == (fit_w, fit_h)) {
                        let mut redrawn = (**prior.clean.as_ref().unwrap()).clone();
                        let screen = cursor_before.filter(|p| Some(*p) == cursor_position())
                            .filter(|_| window_id.is_none() || before.map(|f| f.0) == window_id);
                        let drawn = screen.and_then(|p| mark_cursor(&mut redrawn, &surface, original_pixels, region, p));
                        entry["cursor"] = json!(drawn.map(|(x,y)| json!({"x":x,"y":y,"marker":"sword-tip","source":"system-pointer"})));
                        let mut dirty = Vec::with_capacity(2);
                        if let Some(rect) = marker_rect(prior.marker, (fit_w, fit_h)) { dirty.push(rect); }
                        if let Some(rect) = marker_rect(drawn, (fit_w, fit_h)) { dirty.push(rect); }
                        let encode_started = Instant::now();
                        let mut frame = crate::visual_guard::deliver_frame(&mut entry, Some(prior), Some(stream.clone()),
                            (*prior.seen).clone(), Some(&redrawn), folder.join(format!("{id}-{image_id}.webp")),
                            drawn, prior.clean.clone(), Some(&dirty))?;
                        if let Some(frame) = frame.as_mut() { frame.stamp = stamp; }
                        delivered.extend(frame);
                        encode_ms += encode_started.elapsed().as_millis();
                        images.push(entry);
                        shots.push(Shot {
                            image_id: image_id.clone(), surface, pixels: (fit_w, fit_h), source_pixels: original_pixels,
                            region, overlay, desktop_capture,
                        });
                        return Ok(());
                    }
                }
            }
        }
        let resize_started = Instant::now();
        // Mask before filtering: animated self-UI edges must not bleed into neighbouring cells.
        let plain = (!source_ignored.is_empty()).then(|| resize_shot(nova_regions::comparison_pixels(&image, &source_ignored), max_edge, region.is_some()));
        let mut image = resize_shot(image, max_edge, region.is_some());
        let clean = std::sync::Arc::new(image.clone());
        let plain = plain.unwrap_or_else(|| (*clean).clone());
        resize_ms += resize_started.elapsed().as_millis();
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
        entry["monitorId"] = json!(window_id.is_none().then_some(surface.id));
        if !ignored.is_empty() { entry["ignoredRegions"] = json!(ignored); }
        let prior = previous.iter().find(|p| Some(&p.stream) == stream.as_ref());
        let mut frame = crate::visual_guard::deliver_frame(&mut entry, prior, stream, plain,
            Some(&image), folder.join(format!("{id}-{image_id}.webp")), cursor, Some(clean), Some(&[]))?;
        if let Some(frame) = frame.as_mut() { frame.stamp = stamp; }
        let shot = Shot {
            image_id, surface, pixels: image.dimensions(), source_pixels: original_pixels,
            region, overlay, desktop_capture,
        };
        delivered.extend(frame);
        encode_ms += encode_started.elapsed().as_millis();
        images.push(entry);
        shots.push(shot);
        Ok(())
    };
    if let Some(wid) = window_id {
        let w = window(wid)?;
        if w.is_minimized().map_err(err)? {
            return Err("窗口已最小化，请先通过桌面恢复窗口".into());
        }
        let surface = window_surface(&w)?;
        let capture_started = Instant::now();
        // Only explicit isolated background mode can capture a window, never ordinary desktop input.
        let (image, stability, overlay) = sample_image(|| w.capture_image().map_err(err), None)?;
        capture_ms += capture_started.elapsed().as_millis();
        if window_surface(&w)? != surface {
            return Err("截图期间窗口移动，请重新截图".into());
        }
        save(surface, image, stability, overlay, false)?;
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
            let fresh_status = std::cell::Cell::new(None);
            let (image, mut stability, overlay) = sample_image(|| {
                if foreground()? != before || monitor_surface(&m)? != surface {
                    return Err("取样期间屏幕或焦点改变，请重新截图".into());
                }
                let (image, status) = capture_monitor(&m, fresh, watch)?;
                fresh_status.set(Some(status));
                Ok(image)
            }, Some(&surface))?;
            if fresh.is_some() {
                if let Some(status) = fresh_status.get() {
                    stability["status"] = json!(status);
                }
            }
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
        desktop_edge,
        foreground: before,
        shots,
        invalidated: None,
        background: bg,
        delivered,
        missed: None,
    });
    let mut result = json!({"snapshotId":id,"observationSequence":sequence,"historical":false,"windowId":window_id,"images":images,"coordinateSpace":"image-pixels","expiresInMs":180000,"background":bg,
            "foreground":before,"maxEdge":state.as_ref().map(|s| s.max_edge).unwrap_or(max_edge),"desktopEdge":desktop_edge,"coordinateNote":COORDINATE_NOTE,
            "timingsMs":{"capture":capture_ms,"resize":resize_ms,"encodeAndSave":encode_ms,
                "reusedFrames":reused_frames,
                "other":started.elapsed().as_millis().saturating_sub(capture_ms + resize_ms + encode_ms),
                "total":started.elapsed().as_millis()}});
    if let Some(painted) = target_painted.get() {
        result["targetPainted"] = json!(painted);
        if !painted {
            result["targetNote"] = json!("前台窗口区域内的像素与激活前相比没有明显变化。这张图不能当成已经切到目标应用；不要立刻再次激活。若画面仍是上一个窗口，再截一次。");
        }
    }
    Ok(result)
}

fn action_shot<'a>(shots: &'a [Shot], image_id: Option<&str>) -> Result<&'a Shot> {
    if let Some(id) = image_id {
        if let Some(shot) = shots.iter().find(|s| s.image_id == id) { return Ok(shot); }
    } else if let [only] = shots {
        return Ok(only);
    }
    // Coordinates are per image: list each id with its pixel size/region so the caller can re-map x,y instead of guessing.
    let list = shots.iter().map(|s| format!("{}（{}x{}{}）", s.image_id, s.pixels.0, s.pixels.1,
        s.region.map(|r| format!("，原始区域x{} y{} {}x{}", r.x, r.y, r.width, r.height)).unwrap_or_default())).collect::<Vec<_>>().join(", ");
    Err(format!("imageId缺失或不属于此截图；可用imageId：{list}。x/y须按所选图片的像素坐标重算；沿用当前snapshotId并修正imageId，无需重新截图"))
}

fn point(shot: &Shot, x: Option<i32>, y: Option<i32>) -> Result<(i32, i32)> {
    let (x, y) = (x.ok_or("缺少x坐标")?, y.ok_or("缺少y坐标")?);
    let (pw, ph) = shot.pixels;
    if x < 0 || y < 0 || x as u32 >= pw || y as u32 >= ph || pw == 0 || ph == 0
        || shot.source_pixels.0 == 0 || shot.source_pixels.1 == 0 {
        return Err(format!("坐标超出当前图片（{pw}x{ph} 像素）。x/y 是这张图的列和行，不要用 originalWidth/originalHeight、desktopBounds 或屏幕坐标"));
    }
    let (x, y) = (x as i128, y as i128);
    let s = &shot.surface;
    let r = shot.region.unwrap_or(Region { x:0, y:0, width:shot.source_pixels.0, height:shot.source_pixels.1 });
    // Image pixel (x, y) covers the cell whose centre is (x+0.5, y+0.5) after the same scale the
    // Triangle resize used. Flooring that centre picks the screen pixel that contains it.
    // Rounding to nearest would step onto the next pixel whenever the fraction is ≥ 0.5.
    let px = s.x as i128 + (2 * r.x as i128 * pw as i128 + (2 * x + 1) * r.width as i128)
        * s.width as i128 / (2 * pw as i128 * shot.source_pixels.0 as i128);
    let py = s.y as i128 + (2 * r.y as i128 * ph as i128 + (2 * y + 1) * r.height as i128)
        * s.height as i128 / (2 * ph as i128 * shot.source_pixels.1 as i128);
    Ok((
        i32::try_from(px).map_err(err)?,
        i32::try_from(py).map_err(err)?,
    ))
}
/// act 的坐标空间：图片像素（默认，需一张观察）或屏幕物理像素（UIA 控件矩形，免截图）。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Space {
    Image,
    Screen,
}

fn resolve_point(
    shot: Option<&Shot>,
    space: Space,
    x: Option<i32>,
    y: Option<i32>,
) -> Result<(i32, i32)> {
    match space {
        // 屏幕空间允许负坐标（多显示器在左/上）；范围由调用方按当前显示器布局另行校验。
        Space::Screen => Ok((x.ok_or("缺少x坐标")?, y.ok_or("缺少y坐标")?)),
        Space::Image => point(shot.ok_or("坐标动作需要截图")?, x, y),
    }
}

fn action_activity(a: &Action) -> &'static str {
    match a.action.as_str() {
        "click" | "double_click" => "点击控件",
        "type" => "输入内容",
        "press" => "发送按键",
        "scroll" => "滚动页面",
        "drag" => "拖动控件",
        "wait" => "等待界面",
        "move" => "移动指针",
        _ => "操作桌面",
    }
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
    validate_in(a, shot, Space::Image)
}

fn validate_in(a: &Action, shot: Option<&Shot>, space: Space) -> Result<()> {
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
        resolve_point(shot, space, a.x, a.y)?;
    }
    if a.action == "drag" {
        resolve_point(shot, space, a.to_x, a.to_y)?;
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
fn source_point(shot: &Shot, x: f64, y: f64) -> (f64, f64) {
    let r=shot.region.unwrap_or(Region{x:0,y:0,width:shot.source_pixels.0,height:shot.source_pixels.1});
    (r.x as f64+x as f64*r.width as f64/shot.pixels.0 as f64,
     r.y as f64+y as f64*r.height as f64/shot.pixels.1 as f64)
}
fn image_region(shot: &Shot, region: Region) -> Result<Region> {
    region.validate(shot.pixels)?;
    let (x,y)=source_point(shot,region.x as f64,region.y as f64);
    let (right,bottom)=source_point(shot,(region.x+region.width) as f64,(region.y+region.height) as f64);
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

// Scroll is excluded: it never lands on the taskbar status strip in a harmful way and is reversible.
fn guarded_pointer(action:&Action) -> bool {matches!(action.action.as_str(),"click"|"double_click"|"drag")}
fn pointer_matches(expected:(i32,i32),actual:(i32,i32)) -> bool {
    // Allow native rounding (e.g. a late SendInput normalization), not a wrong monitor or DPI mapping.
    (expected.0 as i64-actual.0 as i64).abs()<=2 && (expected.1 as i64-actual.1 as i64).abs()<=2
}
/// First readback, or one corrective move's readback. A miss that is still wrong after that
/// correction is rejected — the reported multi-monitor miss is not "close enough".
fn readback_accepts(expected: (i32, i32), first: (i32, i32), corrected: Option<(i32, i32)>) -> bool {
    pointer_matches(expected, first) || corrected.is_some_and(|p| pointer_matches(expected, p))
}
fn confirm_pointer(enigo: &mut Enigo, at: (i32, i32), miss: &str) -> Result<()> {
    let first = enigo.location().map_err(err)?;
    if pointer_matches(at, first) { return Ok(()); }
    // The absolute move can land a pixel off until SetCursorPos sticks. One correction, then stop.
    move_pointer(enigo, at.0, at.1, true)?;
    let second = enigo.location().map_err(err)?;
    if readback_accepts(at, first, Some(second)) { return Ok(()); }
    Err(format!("系统鼠标落点与请求不符（请求{},{}，实际{},{}），未按下按钮；{miss}", at.0, at.1, second.0, second.1))
}
fn check_target(snap: &Snapshot, shot: &Shot, a: &Action) -> Result<()> {
    if snap.taken.elapsed() > Duration::from_secs(180) {
        return Err("截图已过期".into());
    }
    if matches!(a.action.as_str(), "type" | "press") { regain(snap.foreground)?; }
    let bg = background::active();
    let overlay = overlay_on_surface(&shot.surface, overlay_bounds(shot.desktop_capture));
    check_overlay(shot, a, overlay)?;
    if let Some(id) = snap.window {
        // Only explicit isolated background mode binds a window/process.
        if !bg { return Err("旧窗口快照不再用于桌面输入，请重新截屏".into()); }
        let w = window(id)?;
        if w.is_minimized().map_err(err)? || window_surface(&w)? != shot.surface {
            return Err("后台窗口已最小化、移动或尺寸改变，请重新截图".into());
        }
    } else if bg {
        let seat = background::seat_surface()?;
        if snap.shots.iter().any(|s| s.surface != seat) {
            return Err("座席已重启或尺寸改变，请重新截图".into());
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
    }
    if overlay != overlay_on_surface(&shot.surface, overlay_bounds(shot.desktop_capture)) { return Err("落点校验期间任务栏状态条范围改变，请重新截图".into()); }
    Ok(())
}

fn action_delay(actions: &[Action], index: usize) -> Duration {
    // Explicit waits already provide reaction time; pointer motion needs no extra delay.
    let redundant = matches!(actions[index].action.as_str(), "wait" | "move")
        || actions.get(index + 1).is_some_and(|a| a.action == "wait");
    if redundant {
        return Duration::ZERO;
    }
    // Ceilings. Windows returns early once a substantial paint is followed by a quiet frame.
    // The trailing budget is what the feedback shot waits; intermediate clicks still allow a slow popup.
    let last = index + 1 == actions.len();
    let paints = matches!(actions[index].action.as_str(), "click" | "double_click" | "type" | "scroll");
    Duration::from_millis(if last { 180 } else if paints { 300 } else { 60 })
}

/// One step of the post-input wait. `stop` means a real paint already happened and the desktop went quiet.
#[cfg_attr(not(windows), allow(dead_code))]
pub(super) fn settle_step(seen_content: bool, pulse: crate::visual_guard::DesktopPulse) -> (bool, bool) {
    use crate::visual_guard::DesktopPulse;
    match pulse {
        DesktopPulse::Content => (true, false),
        DesktopPulse::Quiet if seen_content => (true, true),
        DesktopPulse::Quiet | DesktopPulse::Pointer | DesktopPulse::Minor => (seen_content, false),
    }
}

struct TimerResolution;
impl TimerResolution {
    fn request() -> Self {
        #[cfg(windows)]
        unsafe { windows_sys::Win32::Media::timeBeginPeriod(1); }
        Self
    }
}
impl Drop for TimerResolution {
    fn drop(&mut self) {
        #[cfg(windows)]
        unsafe { windows_sys::Win32::Media::timeEndPeriod(1); }
    }
}

fn precise_wait(deadline: Instant) {
    loop {
        let now = Instant::now();
        if now >= deadline { return; }
        let left = deadline.saturating_duration_since(now);
        if left > Duration::from_millis(3) {
            std::thread::sleep(left - Duration::from_millis(2));
        } else {
            std::hint::spin_loop();
        }
    }
}

fn precise_sleep(total: Duration) {
    if !total.is_zero() { precise_wait(Instant::now() + total); }
}

/// Armed before a batch. Linux records damage from before each non-press action; Windows already
/// has a live duplication session. `press` always sleeps the full budget (clipboard may not paint).
struct PaintWatch {
    #[cfg(target_os = "linux")]
    inner: x11::Watch,
}

impl PaintWatch {
    fn arm(background_mode: bool, target: Option<(i32, i32, u32, u32)>) -> Self {
        #[cfg(target_os = "linux")]
        {
            let inner = match x11::observation_display(background_mode) {
                Some(display) => x11::Watch::arm(&display, target),
                None => x11::Watch::off(),
            };
            return Self { inner };
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (background_mode, target);
            Self {}
        }
    }

    fn note(&mut self, action: &str) {
        #[cfg(target_os = "linux")]
        self.inner.note(action);
        #[cfg(not(target_os = "linux"))]
        let _ = action;
    }

    /// `true` when this call already waited (settled early or used the whole budget).
    fn finished(&mut self, action: &str, budget: Duration, target: Option<(i32, i32, u32, u32)>) -> bool {
        if budget.is_zero() || action == "press" {
            return false;
        }
        #[cfg(windows)]
        if dxgi::settle(target, budget) {
            return true;
        }
        #[cfg(target_os = "linux")]
        {
            let _ = target;
            if self.inner.wait(budget) {
                return true;
            }
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        let _ = target;
        false
    }
}

fn pause_for_app(actions: &[Action], index: usize, target: Option<(i32, i32, u32, u32)>, watch: &mut PaintWatch) {
    let budget = action_delay(actions, index);
    if budget.is_zero() { return; }
    // A key often updates the clipboard without painting. Don't treat an unrelated frame as "done".
    if !watch.finished(&actions[index].action, budget, target) {
        precise_sleep(budget);
    }
}

// 只概括已完成的输入；notes 是调用方的意图，不是业务成功证明。
fn executed_notice(actions: &[Action], completed: usize, notes: Option<&str>) -> Option<String> {
    let done: Vec<String> = actions.iter().take(completed).filter_map(|a| Some(match a.action.as_str() {
        "click" | "double_click" | "move" => format!("{}({},{})", a.action, a.x?, a.y?),
        "drag" => format!("drag({},{}→{},{})", a.x?, a.y?, a.to_x?, a.to_y?),
        "type" => format!("type({}字符)", a.text.as_deref()?.chars().count()),
        "press" => format!("press({})", a.key.as_deref()?),
        "scroll" => format!("scroll({},{})", a.axis.as_deref().unwrap_or("vertical"), a.delta?),
        _ => return None,
    })).collect();
    if done.is_empty() { return None; }
    let mut notice = format!("本批已执行：{}。", done.join("、"));
    if let Some(intent) = notes.map(str::trim).filter(|n| !n.is_empty()) {
        notice.push_str(&format!("你为这次操作写的意图：{intent}"));
    }
    Some(notice)
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
    // Visible, eased travel without a 400ms tail. ~40ms for a hop, ~170ms across a window, cap 210ms.
    (40.0 + 34.0 * (1.0 + d / 32.0).log2()).min(210.0)
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
/// Keyboard focus is a safety check, never permission to activate a window behind the screenshot.
pub(super) fn regain(expected: Option<(u32, u32)>) -> Result<()> {
    if foreground()? == expected { return Ok(()); }
    Err("输入焦点已改变，已停止按键/文字输入；请根据新截图继续，不自动切回窗口".into())
}
fn travel(enigo: &mut Enigo, from: (i32, i32), to: (i32, i32), bend: f64, ms: f64) -> Result<()> {
    if from == to || ms < 8.0 {
        return move_pointer(enigo, to.0, to.1, true);
    }
    // Sample by elapsed time. precise_wait keeps the 8ms spacing when the OS timer is coarse.
    let started = Instant::now();
    let step = Duration::from_millis(8);
    let mut tick = started;
    loop {
        let t = (started.elapsed().as_secs_f64() * 1000.0 / ms).min(1.0);
        let (x, y) = glide_at(from, to, bend, t);
        move_pointer(enigo, x, y, true)?;
        if t >= 1.0 { return Ok(()); }
        tick += step;
        if tick < Instant::now() { tick = Instant::now(); }
        precise_wait(tick);
    }
}

fn glide(enigo: &mut Enigo, from: (i32, i32), to: (i32, i32)) -> Result<()> {
    use rand_core::{OsRng, RngCore};
    let bend = (OsRng.next_u32() as f64 / u32::MAX as f64 - 0.5) * 0.2;
    travel(enigo, from, to, bend, glide_ms(from, to))
}
fn input(enigo: &mut Enigo, shot: &Shot, a: &Action, snap: &Snapshot) -> Result<()> {
    let expected_foreground = snap.foreground;
    let _activity = crate::taskbar_status::activity(action_activity(a));
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
        // 指针默认沿轨迹可见地移过去（用户能看清剑来在做什么）；glide:false 和滚动瞬移，
        // 但仍然先发一条绝对移动，再 SetCursorPos。只 SetCursorPos 不会给目标窗口 WM_MOUSEMOVE，
        // 点击会落在应用记住的旧位置。
        if a.glide.unwrap_or(a.action != "scroll") {
            glide(enigo, enigo.location().map_err(err)?, (x, y))?;
        } else {
            move_pointer(enigo, x, y, true)?;
        }
        confirm_pointer(enigo, (x, y), "请根据新图继续")?;
    }
    // Hover can open a popup during the glide. Recheck the visible target instead of restoring focus.
    if guarded_pointer(a) { check_target(snap, shot, a)?; }
    if matches!(a.action.as_str(), "type" | "press") { regain(expected_foreground)?; }
    if a.action != "wait" { check_overlay(shot, a, overlay_bounds(shot.desktop_capture))?; }
    match a.action.as_str() {
        "click" => click_button(enigo,button)?,
        "double_click" => {
            click_button(enigo,button)?;
            precise_sleep(Duration::from_millis(70));
            check_target(snap, shot, a)?;
            let (x, y) = point(shot, a.x, a.y)?;
            confirm_pointer(enigo, (x, y), "请根据新图继续")?;
            click_button(enigo,button)?;
        }
        "drag" => {
            let (x, y) = point(shot, a.x, a.y)?;
            let (tx, ty) = point(shot, a.to_x, a.to_y)?;
            if let Err(error)=enigo.button(button,Direction::Press) {
                let _=enigo.button(button,Direction::Release);return Err(err(error));
            }
            // Straight line, same duration curve as glide. A fixed 12×16ms drag was ~200ms
            // even for a short move, and a 15.6ms scheduler quantum stretched it further.
            let moved = if a.glide == Some(true) {
                glide(enigo, (x, y), (tx, ty))
            } else {
                travel(enigo, (x, y), (tx, ty), 0.0, glide_ms((x, y), (tx, ty)))
            };
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
            let (x, y) = point(shot, a.x, a.y)?;
            // One event of N*WHEEL_DELTA scrolls a single step in WeChat/Qt/DuiLib-style apps that count
            // messages, not magnitude; one notch per event moves N steps everywhere.
            for i in 0..delta.unsigned_abs() {
                if i > 0 {
                    precise_sleep(Duration::from_millis(8));
                    // A nudge between notches must not send the rest of the wheel somewhere else.
                    move_pointer(enigo, x, y, false)?;
                }
                enigo.scroll(delta.signum(), axis).map_err(err)?;
            }
        }
        "wait" => precise_sleep(Duration::from_millis(a.ms.unwrap_or(250))),
        "move" => (),
        _ => unreachable!(),
    }
    Ok(())
}

/// 屏幕物理像素输入的纯前置校验：不碰桌面，便于单测。
/// 这种模式没有观察基准，所以只做指针动作；键盘焦点校验必须先有一张观察。
fn screen_precheck(request: &Request, actions: &[Action]) -> Result<()> {
    if request.background == Some(true) {
        return Err("屏幕坐标只支持普通桌面：后台输入要用快照绑定窗口".into());
    }
    if request.image_id.is_some() {
        return Err("屏幕坐标不使用 imageId；改用图片坐标或去掉它".into());
    }
    if request.snapshot_id.is_some() {
        return Err("屏幕坐标不使用 snapshotId；本模式不需要观察".into());
    }
    for (index, a) in actions.iter().enumerate() {
        if matches!(a.action.as_str(), "type" | "press") {
            return Err(format!(
                "actions[{index}].屏幕坐标只做指针动作；type/press 的焦点校验需要一张观察，先截图再用图片坐标"
            ));
        }
        validate_in(a, None, Space::Screen).map_err(|e| format!("actions[{index}].{e}"))?;
    }
    Ok(())
}

/// 屏幕坐标的单步执行：与 input() 的指针分支同一套真实输入与落点读回校验，只是没有图片基准。
fn screen_input(enigo: &mut Enigo, a: &Action) -> Result<()> {
    let _activity = crate::taskbar_status::activity(action_activity(a));
    let button = match a.button.as_deref() {
        Some("right") => Button::Right,
        Some("middle") => Button::Middle,
        _ => Button::Left,
    };
    let pointer = matches!(
        a.action.as_str(),
        "click" | "double_click" | "move" | "drag" | "scroll"
    );
    let mut at = (0, 0);
    if pointer {
        let (x, y) = resolve_point(None, Space::Screen, a.x, a.y)?;
        at = (x, y);
        // 与图片坐标同一条路径：轨迹或一条绝对移动，读回不符则矫正一次。
        if a.glide.unwrap_or(a.action != "scroll") {
            glide(enigo, enigo.location().map_err(err)?, (x, y))?;
        } else {
            move_pointer(enigo, x, y, true)?;
        }
        confirm_pointer(enigo, (x, y), "请重新读取控件矩形")?;
    }
    match a.action.as_str() {
        "click" => click_button(enigo, button)?,
        "double_click" => {
            click_button(enigo, button)?;
            precise_sleep(Duration::from_millis(70));
            confirm_pointer(enigo, at, "请重新读取控件矩形")?;
            click_button(enigo, button)?;
        }
        "drag" => {
            let (x, y) = at;
            let (tx, ty) = resolve_point(None, Space::Screen, a.to_x, a.to_y)?;
            if let Err(error) = enigo.button(button, Direction::Press) {
                let _ = enigo.button(button, Direction::Release);
                return Err(err(error));
            }
            let moved = if a.glide == Some(true) {
                glide(enigo, (x, y), (tx, ty))
            } else {
                travel(enigo, (x, y), (tx, ty), 0.0, glide_ms((x, y), (tx, ty)))
            };
            let released = enigo.button(button, Direction::Release).map_err(err);
            moved?;
            released?;
        }
        "scroll" => {
            let axis = if a.axis.as_deref() == Some("horizontal") { Axis::Horizontal } else { Axis::Vertical };
            let delta = a.delta.unwrap_or(0);
            for i in 0..delta.unsigned_abs() {
                if i > 0 {
                    precise_sleep(Duration::from_millis(8));
                    move_pointer(enigo, at.0, at.1, false)?;
                }
                enigo.scroll(delta.signum(), axis).map_err(err)?;
            }
        }
        "wait" => precise_sleep(Duration::from_millis(a.ms.unwrap_or(250))),
        _ => (),
    }
    Ok(())
}

/// act 的屏幕坐标模式：不需要快照，直接按屏幕物理像素输入（UIA 控件矩形就是这个坐标系）。
fn act_on_screen(request: &Request, actions: &[Action]) -> Result<Value> {
    screen_precheck(request, actions)?;
    // 坐标必须落在一块当前显示器内：屏幕空间允许负值，但不能点到桌面之外。
    let monitors = Monitor::all().map_err(err)?;
    let surfaces = monitors.iter().map(monitor_surface).collect::<Result<Vec<_>>>()?;
    let inside = |(x, y): (i32, i32)| {
        surfaces.iter().any(|s| {
            x >= s.x
                && y >= s.y
                && x < s.x.saturating_add(s.width as i32)
                && y < s.y.saturating_add(s.height as i32)
        })
    };
    for (index, a) in actions.iter().enumerate() {
        let targets = if a.action == "drag" { vec![(a.x, a.y), (a.to_x, a.to_y)] } else { vec![(a.x, a.y)] };
        for (x, y) in targets {
            if x.is_none() && y.is_none() {
                continue;
            }
            let at = resolve_point(None, Space::Screen, x, y).map_err(|e| format!("actions[{index}].{e}"))?;
            if !inside(at) {
                return Err(format!("actions[{index}].({},{}) 不在任何显示器内", at.0, at.1));
            }
        }
    }
    let _lease = lease_input("jianlai")?;
    let mut enigo = Enigo::new(&Settings::default()).map_err(err)?;
    let mut completed = 0;
    let mut failure = None;
    let mut attempted = false;
    #[cfg(windows)]
    let batch_started = crate::threads::now_ms();
    let _timer = TimerResolution::request();
    let mut watch = PaintWatch::arm(false, None);
    for (index, a) in actions.iter().enumerate() {
        #[cfg(windows)]
        if crate::agent_glow::escaped_since(batch_started) {
            failure = Some(ESCAPED.to_string());
            break;
        }
        // 任务栏状态提示是 Nova 自己的可见界面，一律不点；范围按实时取值核对。
        if guarded_pointer(a) {
            let start = resolve_point(None, Space::Screen, a.x, a.y)?;
            let end = if a.action == "drag" {
                resolve_point(None, Space::Screen, a.to_x, a.to_y)?
            } else {
                start
            };
            if overlay_bounds(true).is_some_and(|r| {
                [start, end]
                    .iter()
                    .any(|&(x, y)| x >= r.left && x < r.right && y >= r.top && y < r.bottom)
            }) {
                failure = Some("工具不能点击或拖动任务栏状态提示，请选择提示区域之外的目标".into());
                break;
            }
        }
        attempted |= a.action != "wait";
        watch.note(&a.action);
        if let Err(e) = screen_input(&mut enigo, a) {
            failure = Some(e);
            break;
        }
        completed += 1;
        pause_for_app(actions, index, None, &mut watch);
    }
    let mut result = json!({
        "status": if failure.is_some() { if attempted { "needs_review" } else { "not_executed" } } else { "executed" },
        "completedActions": completed, "inputAttempted": attempted, "error": failure,
        "coordinateSpace": "screen-pixels", "notes": request.notes,
        "note": "屏幕坐标输入没有反馈图：需要确认画面时再截图，不要重放已执行动作。",
    });
    if let Some(notice) = executed_notice(actions, completed, request.notes.as_deref()) {
        result["executedNotice"] = json!(notice);
    }
    Ok(result)
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

/// `source_point` 的逆：屏幕物理像素 → 该 shot 的原始像素。
#[cfg_attr(not(windows), allow(dead_code))]
fn screen_to_source(shot: &Shot, sx: i32, sy: i32) -> (f64, f64) {
    let s = &shot.surface;
    (
        (sx as f64 - s.x as f64) * shot.source_pixels.0 as f64 / s.width.max(1) as f64,
        (sy as f64 - s.y as f64) * shot.source_pixels.1 as f64 / s.height.max(1) as f64,
    )
}
/// 控件在图片里的矩形，以及 `point()` 会点中的中心像素。
/// 中心用剑尖同一条逆映射，不用「矩形宽高再整除」——那一步和像素中心取整差一格时，小控件会点偏。
fn control_image_rect(shot: &Shot, x: i32, y: i32, width: i32, height: i32) -> Option<(i64, i64, i64, i64, i64, i64)> {
    let (left, top) = (shot.surface.x, shot.surface.y);
    let (right, bottom) = (left.saturating_add(shot.surface.width as i32), top.saturating_add(shot.surface.height as i32));
    let (x0, y0) = (x.max(left), y.max(top));
    let (x1, y1) = (x.saturating_add(width).min(right), y.saturating_add(height).min(bottom));
    if x1 <= x0 || y1 <= y0 { return None; }
    let (ix0, iy0) = source_to_image(shot, screen_to_source(shot, x0, y0));
    let (ix1, iy1) = source_to_image(shot, screen_to_source(shot, x1, y1));
    let clamp = |v: f64, limit: u32| v.round().clamp(0.0, limit.saturating_sub(1) as f64) as i64;
    let (sx, sy) = (x0 + (x1 - x0) / 2, y0 + (y1 - y0) / 2);
    let hit = cursor_tip(&shot.surface, shot.source_pixels, shot.region, (sx, sy), shot.pixels)
        .map(|(px, py)| (px as i64, py as i64));
    let (mut ix, mut iy) = (clamp(ix0, shot.pixels.0), clamp(iy0, shot.pixels.1));
    let (mut iw, mut ih) = (clamp(ix1, shot.pixels.0).saturating_sub(ix).max(0), clamp(iy1, shot.pixels.1).saturating_sub(iy).max(0));
    if iw == 0 || ih == 0 {
        // Smaller than one image pixel: still hand back the pixel act will actually click.
        let Some((cx, cy)) = hit else { return None; };
        return Some((cx, cy, 1, 1, cx, cy));
    }
    let (cx, cy) = hit.unwrap_or((ix + iw / 2, iy + ih / 2));
    if cx < ix { iw += ix - cx; ix = cx; }
    if cy < iy { ih += iy - cy; iy = cy; }
    if cx >= ix + iw { iw = cx - ix + 1; }
    if cy >= iy + ih { ih = cy - iy + 1; }
    Some((ix, iy, iw, ih, cx, cy))
}
/// 原始像素 → 图片像素；越界由调用方夹到图片范围，`source_point` 是它的逆。
#[cfg_attr(not(windows), allow(dead_code))]
fn source_to_image(shot: &Shot, o: (f64, f64)) -> (f64, f64) {
    let r = shot.region.unwrap_or(Region { x: 0, y: 0, width: shot.source_pixels.0, height: shot.source_pixels.1 });
    (
        (o.0 - r.x as f64) * shot.pixels.0 as f64 / r.width.max(1) as f64,
        (o.1 - r.y as f64) * shot.pixels.1 as f64 / r.height.max(1) as f64,
    )
}

/// 控件观察：只读系统控件树，把控件矩形换算成当前快照的图片像素；不产生新图、不发送输入。
/// 没有可用快照时仍然读树（坐标退回屏幕物理像素）：先问结构再决定要不要截图。
#[cfg(windows)]
fn elements(request: &Request, state: &Option<Snapshot>, owner: &str, background: bool) -> Result<Value> {
    if background {
        return Err("控件观察（UIAutomation）只支持 Windows 普通桌面；后台模式仍按截图定位".into());
    }
    // invalidated 只说明图片旧了，控件树是刚读的：几何（surface/region/pixels）不变，图片像素仍指同一块屏幕。
    let snap = state.as_ref().filter(|snap| snap.owner == owner && !snap.background
        && snap.taken.elapsed() <= Duration::from_secs(180));
    let Some(snap) = snap else {
        let window = target_window(None, request.window_id, request.x, request.y, None)?;
        return controls(window, None, request);
    };
    let shot = action_shot(&snap.shots, request.image_id.as_deref())?;
    let window = target_window(Some(shot), request.window_id, request.x, request.y, snap.foreground.map(|(hwnd, _)| hwnd))?;
    controls(window, Some((&snap.id, shot)), request)
}

/// 控件矩形按调用方要用的坐标系输出：有图就是该图的图片像素（可直接点），没有就是屏幕物理像素。
#[cfg(windows)]
fn controls(window: u32, image: Option<(&str, &Shot)>, request: &Request) -> Result<Value> {
    let report = uia::collect(window, request.depth.unwrap_or(3) as usize, request.max_items.unwrap_or(40), request.filter.as_deref())?;
    let mut items = Vec::new();
    for item in report.items {
        let (x, y, width, height) = item.rect;
        let (ix, iy, iw, ih, cx, cy) = match image {
            Some((_, shot)) => match control_image_rect(shot, x, y, width, height) {
                Some(rect) => rect,
                None => continue,
            },
            None => (x as i64, y as i64, width as i64, height as i64, x as i64 + width as i64 / 2, y as i64 + height as i64 / 2),
        };
        items.push(json!({
            "role": item.role, "kind": item.kind.0, "name": item.name, "automationId": item.automation_id,
            "x": ix, "y": iy, "width": iw, "height": ih,
            "center": { "x": cx, "y": cy },
            "screen": { "x": x, "y": y, "width": width, "height": height }, "depth": item.depth,
        }));
    }
    let (app, title) = window_label(window).unzip();
    let empty = items.is_empty();
    let mut result = json!({
        "windowId": window, "app": app, "title": title,
        "items": items, "truncated": report.truncated, "truncatedBy": report.truncated_by,
        "visited": report.visited, "elapsedMs": report.elapsed_ms,
    });
    let truncation = "truncatedBy 说明触到哪个上限：itemLimit 用 filter 或加大 maxItems，visitLimit/deadline 用 windowId 或 x/y 缩小目标，queryFailed 表示有子树读取失败、列表不完整。";
    match image {
        Some((snapshot_id, shot)) => {
            result["snapshotId"] = json!(snapshot_id);
            result["imageId"] = json!(shot.image_id);
            result["coordinateSpace"] = json!("image-pixels");
            result["note"] = json!(format!("坐标是 imageId 图片像素，可直接作为 jianlai_act 的 x/y；沿用这张快照、不产生新图，点击不激活窗口；act 只核对布局、焦点与指针落点，不做像素比对。窗口被移动或布局改变后图片像素失效，先重新截图。{truncation}"));
        }
        None => {
            result["snapshotId"] = Value::Null;
            result["imageId"] = Value::Null;
            result["coordinateSpace"] = json!("screen-pixels");
            result["note"] = json!(format!("没有可用快照：x/y/width/height/center 是屏幕物理像素，可据此判断结构与控件名；要点击先 jianlai_windows(observe) 或 jianlai_screenshot，再用图片像素调 act。{truncation}"));
        }
    }
    if empty {
        result["hint"] = json!("没有带名称的可见控件：加大 depth、去掉 filter，或该界面是自绘/未开辅助功能（Chrome/Electron 常只到文档层），改用截图定位");
    }
    Ok(result)
}

#[cfg(not(windows))]
fn elements(_: &Request, _: &Option<Snapshot>, _: &str, _: bool) -> Result<Value> {
    Err("控件观察（UIAutomation）目前仅支持 Windows；其它平台仍按截图定位".into())
}

/// 控件观察的目标窗口：给了图片像素点就取该点最上层窗口；否则用截图时记录的前台窗口
/// （比实时前台更贴合这张图的坐标空间），没有记录才退回当前前台。
#[cfg(windows)]
/// 观察对象优先级：windowId > x/y 所在窗口 > 快照记录的前台窗口 > 实时前台。
/// shot 仅给 x/y 时需要（x/y 是图片像素）；没有快照时仍可按 windowId 或前台观察。
fn target_window(shot: Option<&Shot>, window_id: Option<u32>, x: Option<i32>, y: Option<i32>, observed: Option<u32>) -> Result<u32> {
    use windows_sys::Win32::Foundation::{HWND, POINT};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetAncestor, GetForegroundWindow, GetWindowThreadProcessId, IsWindow, WindowFromPoint, GA_ROOT,
    };
    let window = if let Some(id) = window_id {
        let hwnd = id as usize as HWND;
        if unsafe { IsWindow(hwnd) } == 0 {
            return Err("windowId 已失效：先用 jianlai_windows 重新确认窗口".into());
        }
        unsafe { GetAncestor(hwnd, GA_ROOT) }
    } else {
        match (x, y) {
            (Some(x), Some(y)) => {
                let shot = shot.ok_or("x/y 是图片像素坐标，需要本会话有一次可用截图")?;
                let (sx, sy) = point(shot, Some(x), Some(y))?;
                unsafe { GetAncestor(WindowFromPoint(POINT { x: sx, y: sy }), GA_ROOT) }
            }
            _ => observed
                .map(|id| id as usize as windows_sys::Win32::Foundation::HWND)
                .unwrap_or_else(|| unsafe { GetForegroundWindow() }),
        }
    };
    if window.is_null() {
        return Err("该位置没有可观察的窗口；先截图确认目标在桌面上可见".into());
    }
    // 自己的窗口要从消息循环取控件树，与调用方互相等待：直接拒绝而不是冒死锁风险。
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(window, &mut pid) };
    if pid == std::process::id() {
        return Err("不能观察 Nova 自身的窗口；请对目标应用截图后再用 elements".into());
    }
    Ok(window as usize as u32)
}

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
        if !result.get("basedOnSnapshotId").is_some_and(|v| !v.is_null()) {
            result["basedOnSnapshotId"] = based_on;
        }
        result["verification"] = json!("unverified");
        if result["status"] == "not_executed" { result["inputAttempted"] = json!(false); }
    }
    if let Some(notes) = notes.filter(|v| v.as_str().is_some_and(|s| s.chars().count() <= 12000)) {
        result["notes"] = notes;
    }
    Ok(result)
}

fn run_inner(owner: String, mut args: Value) -> Result<Value> {
    // One physical coordinate plane for enumeration, capture, cursor readback and SendInput.
    // Blocking-pool threads may inherit a different DPI context; restore it on every exit.
    #[cfg(windows)]
    let _dpi = {
        use windows_sys::Win32::UI::HiDpi::*;
        struct RestoreDpi(DPI_AWARENESS_CONTEXT);
        impl Drop for RestoreDpi {
            fn drop(&mut self) { unsafe { SetThreadDpiAwarenessContext(self.0); } }
        }
        let previous = unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        if previous.is_null() { return Err(format!("无法进入物理像素坐标模式：{}", std::io::Error::last_os_error())); }
        RestoreDpi(previous)
    };
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
    let mut request: Request = match serde_json::from_value(args) {
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
    if request.operation != "elements"
        && (request.x.is_some() || request.y.is_some() || request.depth.is_some() || request.max_items.is_some() || request.filter.is_some())
    {
        return Err("x/y/depth/maxItems/filter 仅用于 elements".into());
    }
    if request.operation == "elements" {
        // 后台模式已在上面按 operation 白名单拒掉（background 只允许 windows/screenshot/act）。
        if request.window_id.is_some() && (request.x.is_some() || request.y.is_some()) {
            return Err("elements 的 windowId 与 x/y 只能二选一：windowId 观察该窗口，x/y 观察该点最上层窗口".into());
        }
        if request.x.is_some() != request.y.is_some() {
            return Err("elements 的 x/y 需同时给出（图片像素），或都省略以观察截图时的前台窗口".into());
        }
        if request.x.is_some_and(|v| v < 0) || request.y.is_some_and(|v| v < 0) {
            return Err("elements 的 x/y 是图片像素，不能为负".into());
        }
        if !(1..=6).contains(&request.depth.unwrap_or(3)) {
            return Err("depth 允许 1–6，默认 3".into());
        }
        if !(1..=120).contains(&request.max_items.unwrap_or(40)) {
            return Err("maxItems 允许 1–120，默认 40".into());
        }
        if request.filter.as_ref().is_some_and(|f| f.chars().count() > 60) {
            return Err("filter 最多 60 字符".into());
        }
    }
    if request.coordinate_space.is_some() && request.operation != "act" {
        return Err("coordinateSpace 仅用于 act".into());
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
        "elements" => "观察控件",
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
                // No target means all monitors; a target only selects its monitor in ordinary mode.
                let window_id = target;
                match capture(&owner, window_id, None, None, max_edge, None, None, &mut state) {
                    Ok(observed) => {
                        result.as_object_mut().unwrap().extend(observed.as_object().unwrap().clone());
                        result["observedWindowId"] = json!(window_id);
                        if let Some(id) = window_id.filter(|_| !bg) {
                            let focused = result["foreground"][0].as_u64() == Some(id as u64);
                            result["observedWindowFocused"] = json!(focused);
                            if !focused { result["nextCall"] = json!({"operation":"activate","windowId":id}); }
                        }
                        result["observedBy"] = json!(if target.is_some() {"query/windowId"} else if bg {"background"} else {"desktop"});
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
                if let Some(snap) = state.as_mut() { snap.invalidated = Some("窗口已激活".into()); }
                let bounds = (target.x().unwrap_or(0), target.y().unwrap_or(0), target.width().unwrap_or(0), target.height().unwrap_or(0));
                let surface = target.current_monitor().ok().and_then(|m| monitor_surface(&m).ok());
                // Sample, then read the generation. Reading the epoch first would count this
                // sample itself as the frame that arrived after activation.
                let mut watch_window = None;
                let action_epoch = if let Some(surface) = surface.as_ref() {
                    if bounds.2 > 0 && bounds.3 > 0 {
                        if let Ok(frame) = dxgi::grab(surface.x, surface.y, surface.width, surface.height) {
                            let before = window_samples_on(&frame, surface, bounds);
                            if !before.is_empty() { watch_window = Some(WatchWindow { rect: bounds, before }); }
                        }
                    }
                    Some(dxgi::epoch(surface.x, surface.y, surface.width, surface.height))
                } else { None };
                bring_to_front(id);
                let _timer = TimerResolution::request();
                let settled = match surface.as_ref() {
                    Some(surface) => dxgi::settle(Some((surface.x, surface.y, surface.width, surface.height)), Duration::from_millis(150)),
                    None => dxgi::settle(None, Duration::from_millis(150)),
                };
                if !settled { precise_sleep(Duration::from_millis(150)); }
                if foreground()? != Some((id, target.pid().map_err(err)?)) {
                    return Err("系统未允许激活该窗口；请观察当前桌面，不要重复激活或发送按键".into());
                }
                let mut result = capture(&owner, Some(id), None, None, max_edge, action_epoch, watch_window.as_ref(), &mut state)?;
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
                check_target(snap,shot,&action)?;
                if request.window_id.is_some_and(|id|Some(id)!=snap.window) || request.monitor_id.is_some_and(|id|snap.window.is_some()||id!=shot.surface.id) {return Err("局部截图目标与snapshotId/imageId不一致".into());}
                (snap.window, snap.window.is_none().then_some(shot.surface.id), Some(image_region(shot,request.region.unwrap())?))
            } else {
                if request.region.is_some() && request.window_id.is_none() && request.monitor_id.is_none() {return Err("原始像素region需要windowId或monitorId".into());}
                (request.window_id,request.monitor_id,request.region)
            };
            let mut result = capture(&owner, wid, mid, region, max_edge, None, None, &mut state)?;
            result["notes"] = json!(request.notes);
            Ok(result)
        }
        "recall" => {
            let path = dunce::canonicalize(request.image_path.ok_or("recall缺少imagePath")?).map_err(err)?;
            let folder = dunce::canonicalize(shot_folder(&owner, "desktop-shots")).map_err(err)?;
            if !path.starts_with(folder) || !matches!(path.extension().and_then(|s| s.to_str()), Some("webp" | "jpg" | "png")) {
                return Err("只能回看本会话的桌面截图".into());
            }
            Ok(json!({"images":[{"path":path}],"historical":true}))
        }
        "elements" => elements(&request, &state, &owner, bg),
        "act" => {
            let actions = request.actions.as_deref().ok_or("缺少actions")?;
            if actions.is_empty() || actions.len() > 16 {
                return Err("每次需要1至16个动作".into());
            }
            match request.coordinate_space.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                None | Some("image") => {}
                Some("screen") => return act_on_screen(&request, actions),
                Some(other) => return Err(format!("coordinateSpace 只允许 image 或 screen，收到 {other}")),
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
                let max_edge = request.max_edge.or_else(|| snap.map(|s| s.desktop_edge)).unwrap_or(1600);
                for a in actions { precise_sleep(Duration::from_millis(a.ms.unwrap_or(250))); }
                let mut result = json!({"status":"executed","completedActions":actions.len(),"inputAttempted":false});
                if request.feedback.as_deref() != Some("none") {
                    let desktop = request.feedback.as_deref() == Some("desktop");
                    let monitor_id = if bg || desktop { None } else {
                        snap.and_then(|s| action_shot(&s.shots, request.image_id.as_deref()).ok()
                            .or_else(|| s.shots.first().filter(|first| s.shots.iter().all(|s| s.surface.id == first.surface.id))))
                            .map(|s| s.surface.id)
                    };
                    let window_id = if bg { window_id } else { None };
                    match capture(&owner, window_id, monitor_id, None, max_edge, None, None, &mut state) {
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
            let current = bind_input_snapshot(&mut request.snapshot_id, state.as_ref(), &owner, bg);
            let missing_snapshot = request.snapshot_id.is_none();
            if missing_snapshot || current.is_some_and(|snap| Some(&snap.id) != request.snapshot_id.as_ref()) {
                // Reject, never replay. Keep the latest usable observation instead of rotating IDs/images
                // on every parameter error; check_target still validates it before any subsequent input.
                let mut result = json!({"status":"not_executed","completedActions":0,"inputAttempted":false});
                if missing_snapshot {
                    result["required"] = json!(["observation"]);
                    result["errorCode"] = json!("observation_required");
                    result["error"] = json!("没有本会话同模式的可用观察；本批次未执行。先观察目标画面再决策，不需要补填 snapshotId。");
                } else {
                    result["required"] = json!(["snapshotId"]);
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
                    result["next"] = json!("当前没有本会话未失效的同模式观察；先 windows(observe:true) 或 screenshot 看目标画面，再决策剩余动作。snapshotId 可省略。");
                }
                return Ok(result);
            }
            if state.is_none() {
                let mut result = json!({"status":"not_executed","completedActions":0,"error":"没有可用快照，附当前观察；基于新图重新决策"});
                observe(&owner, request.window_id, None, request.feedback.as_deref() == Some("desktop"), max_edge, None, &mut state, &mut result);
                return Ok(result);
            }
            let snap = state.as_ref().unwrap();
            if snap.owner != owner {
                return Err("快照无效或已过期，请重新截图；不能重放动作".into());
            }
            if Some(&snap.id) != request.snapshot_id.as_ref() {
                let window_id = snap.window;
                let max_edge = snap.desktop_edge;
                let mut result = json!({"status":"not_executed","completedActions":0,
                    "error":"旧snapshotId已失效，附当前观察；请基于新图重新决策，不自动重放"});
                observe(&owner, window_id, None, request.feedback.as_deref() == Some("desktop"), max_edge, None, &mut state, &mut result);
                return Ok(result);
            }
            if let Some(reason) = snap.invalidated.clone() {
                let window_id = snap.window;
                let max_edge = snap.desktop_edge;
                let mut result = json!({"status":"not_executed","completedActions":0,
                    "error":format!("快照在截图后被其它工具改变的画面作废：{reason}；附当前观察，请基于新图重新决策")});
                observe(&owner, window_id, None, request.feedback.as_deref() == Some("desktop"), max_edge, None, &mut state, &mut result);
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
            if actions.iter().any(|a| blocks_blind_repeat(&a.action, snap.missed.as_ref(), &shot.image_id, a.x.unwrap_or(i32::MIN), a.y.unwrap_or(i32::MIN))) {
                return Ok(json!({"status":"not_executed","completedActions":0,"inputAttempted":false,
                    "errorCode":"repeat_unchanged","basedOnSnapshotId":snap.id,
                    "error":"上一次单击没有改变落点附近。本批次未执行，也没有自动再点。请改用反馈里那张放大图的 imageId 选择不同像素，或重新 screenshot。double_click 仍会执行。不要重试可能发送的动作。"}));
            }
            let max_edge = feedback_edge(request.max_edge, snap.desktop_edge);
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
            let _timer = TimerResolution::request();
            let paint_target = (!bg).then_some((shot.surface.x, shot.surface.y, shot.surface.width, shot.surface.height));
            let mut watch = PaintWatch::arm(bg, paint_target);
            let prior_frames = snap.delivered.clone();
            let acted_on = json!({"snapshotId":snap.id,"imageId":shot.image_id,"pixelWidth":shot.pixels.0,"pixelHeight":shot.pixels.1,
                "region":shot.region.map(|r| json!({"x":r.x,"y":r.y,"width":r.width,"height":r.height}))});
            #[cfg(windows)]
            let fresh = Some(dxgi::epoch(shot.surface.x, shot.surface.y, shot.surface.width, shot.surface.height));
            #[cfg(not(windows))]
            let fresh: Option<u64> = None;
            let mut aimed: Vec<(String, String, i32, i32, i32, i32)> = Vec::new();
            for (index, a) in actions.iter().enumerate() {
                // 光效亮着时用户亲手按了 Esc：立即停下，剩余动作不再执行。
                #[cfg(windows)]
                if !bg && crate::agent_glow::escaped_since(batch_started) {
                    failure = Some(ESCAPED.to_string());
                    break;
                }
                if a.action != "wait" {
                    if let Err(e) = check_target(&snap, &shot, a) {
                        failure = Some(e);
                        break;
                    }
                }
                let clipboard_paste = cfg!(windows) && a.action == "type" && a.text.as_deref().is_some_and(|text| !text.is_empty());
                if clipboard_paste {
                    if let Some(copied) = crate::clipboard::copied_since(clipboard) { copied_before_paste = Some(copied); }
                }
                attempted |= a.action != "wait";
                watch.note(&a.action);
                let sent = match enigo.as_mut() {
                    Some(enigo) => input(enigo, &shot, a, &snap),
                    None => post_background(&shot, a),
                };
                // Our temporary paste/restore is not a copy made by the target application.
                if clipboard_paste { clipboard = crate::clipboard::sequence(); }
                if let Err(e) = sent {
                    failure = Some(e);
                    break;
                }
                completed += 1;
                if matches!(a.action.as_str(), "click" | "double_click") {
                    if let (Some(x), Some(y)) = (a.x, a.y) {
                        if let Ok(screen) = point(&shot, Some(x), Some(y)) {
                            aimed.push((a.action.clone(), shot.image_id.clone(), x, y, screen.0, screen.1));
                        }
                    }
                }
                pause_for_app(&actions, index, paint_target, &mut watch);
                // Keep reading frames between inputs, but never duplicate the final scroll + wait feedback.
                if let Some(scroll_index) = scroll_frame_index(&actions, index, request.feedback.as_deref()) {
                    let (mut frame, mut scratch) = (json!({}), None);
                    observe(&owner, feedback_window, monitor_id, request.feedback.as_deref() == Some("desktop"), max_edge, fresh, &mut scratch, &mut frame);
                    for mut image in frame["images"].as_array().cloned().unwrap_or_default() {
                        image["frame"] = json!(scroll_index);
                        frames.push(image);
                    }
                }
                // A focus transition changes where subsequent keys go, not the screenshot baseline.
                if actions.get(index + 1).is_some_and(|a| a.action == "wait") { continue; }
                match foreground() {
                    Ok(f) => {
                        if f != snap.foreground && actions[index + 1..].iter().any(|a| matches!(a.action.as_str(), "type" | "press")) {
                            failure = Some("动作后输入焦点改变，已停止后续按键/文字输入；请根据新截图继续，不要重放整批".into());
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
            // Consuming input IDs must not discard the per-monitor frames needed for unchanged-frame reuse.
            snap.invalidated = Some("动作已执行，请先观察新画面，不重放输入".into());
            *state = Some(snap);
            let mut result = json!({"status":if failure.is_some(){if attempted {"needs_review"} else {"not_executed"}}else{"executed"},"completedActions":completed,"inputAttempted":attempted,"error":failure,"notes":request.notes,
                "basedOnSnapshotId":acted_on["snapshotId"],"actedOn":acted_on});
            if let Some(notice) = executed_notice(actions, completed, request.notes.as_deref()) {
                result["executedNotice"] = json!(notice);
            }
            if failure.is_some() || request.feedback.as_deref() != Some("none") {
                observe(&owner, feedback_window, monitor_id, request.feedback.as_deref() == Some("desktop"), max_edge, fresh, &mut state, &mut result);
                note_click_effect(&owner, &aimed, &prior_frames, acted_on["snapshotId"].as_str().unwrap_or(""), &mut state, &mut result);
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
            }
            Ok(result)
        }
        _ => Err("未知剑来操作；可用：windows/screenshot/act/activate/recall/run/experience_search/experience_save/experience_feedback，切窗用activate+windowId".into()),
    }
}
/// Reports where the click landed on the feedback image, and when that neighbourhood did not
/// change, attaches one local zoom and remembers the point so the next single click there is refused.
fn note_click_effect(owner: &str, aimed: &[(String, String, i32, i32, i32, i32)], prior: &[Delivered], prior_id: &str, state: &mut Option<Snapshot>, result: &mut Value) {
    let Some(state) = state.as_mut() else { return };
    // A failed recapture leaves the pre-action snapshot in place. Its pixels matching themselves
    // is not evidence that the click changed nothing.
    if state.id == prior_id { return; }
    let Some(shot_index) = state.shots.iter().position(|s| s.region.is_none()) else { return };
    let mut landed = Vec::new();
    for (action, image_id, x, y, sx, sy) in aimed {
        let shot = &state.shots[shot_index];
        if let Some((px, py)) = cursor_tip(&shot.surface, shot.source_pixels, shot.region, (*sx, *sy), shot.pixels) {
            landed.push(json!({"action":action,"from":{"imageId":image_id,"x":x,"y":y},"at":{"imageId":shot.image_id,"x":px,"y":py}}));
        }
    }
    if !landed.is_empty() { result["landed"] = json!(landed); }
    let Some((action, _, _, _, sx, sy)) = aimed.iter().rev().find(|(action, ..)| matches!(action.as_str(), "click" | "double_click")).cloned() else { return };
    let (image_id, tip) = {
        let shot = &state.shots[shot_index];
        (shot.image_id.clone(), cursor_tip(&shot.surface, shot.source_pixels, shot.region, (sx, sy), shot.pixels))
    };
    let Some((px, py)) = tip else { return };
    let after = state.delivered.iter().find(|d| d.stream == image_id).cloned();
    let before = prior.iter().find(|d| d.stream == image_id).cloned();
    let (Some(before), Some(after)) = (before, after) else { return };
    if click_unchanged(&before, &after, px as f64, py as f64) {
        result["effect"] = json!("unchanged");
        result["effectNote"] = json!(EFFECT_NOTE);
        if action == "click" {
            state.missed = Some(Miss { image_id, x: px as i32, y: py as i32 });
        }
        attach_detail(owner, state, result, shot_index, (px, py));
    } else {
        result["effect"] = json!("changed");
        state.missed = None;
    }
}

fn attach_detail(owner: &str, state: &mut Snapshot, result: &mut Value, shot_index: usize, at: (u32, u32)) {
    let Some(clean) = state.delivered.iter().find(|d| d.stream == state.shots[shot_index].image_id).and_then(|d| d.clean.clone()) else { return };
    let (pw, ph) = clean.dimensions();
    if pw == 0 || ph == 0 { return; }
    let side = 96u32;
    let x0 = at.0.saturating_sub(side / 2).min(pw - 1);
    let y0 = at.1.saturating_sub(side / 2).min(ph - 1);
    let w = side.min(pw - x0).max(1);
    let h = side.min(ph - y0).max(1);
    let zoomed = resize_shot(xcap::image::imageops::crop_imm(clean.as_ref(), x0, y0, w, h).to_image(), 480, true);
    let (parent_id, surface, source_pixels, parent_region, overlay) = {
        let parent = &state.shots[shot_index];
        (parent.image_id.clone(), parent.surface.clone(), parent.source_pixels,
            parent.region.unwrap_or(Region { x: 0, y: 0, width: parent.source_pixels.0, height: parent.source_pixels.1 }),
            parent.overlay)
    };
    let to_source = |v: u32, edge: u32, span: u32| if edge == 0 { 0 } else { (v as u64 * span as u64 / edge as u64) as u32 };
    let sx = parent_region.x.saturating_add(to_source(x0, pw, parent_region.width)).min(source_pixels.0.saturating_sub(1));
    let sy = parent_region.y.saturating_add(to_source(y0, ph, parent_region.height)).min(source_pixels.1.saturating_sub(1));
    let sw = to_source(w, pw, parent_region.width).max(1).min(source_pixels.0 - sx);
    let sh = to_source(h, ph, parent_region.height).max(1).min(source_pixels.1 - sy);
    let region = Region { x: sx, y: sy, width: sw, height: sh };
    let image_id = format!("{parent_id}-detail-{}-{}-{}-{}", region.x, region.y, region.width, region.height);
    let pixels = zoomed.dimensions();
    let bytes = match crate::visual_guard::encode_webp(&zoomed) { Ok(bytes) => bytes, Err(_) => return };
    let folder = shot_folder(owner, "desktop-shots");
    if std::fs::create_dir_all(&folder).is_err() { return; }
    let path = folder.join(format!("{}-{image_id}.webp", state.id));
    if std::fs::write(&path, bytes).is_err() { return; }
    let entry = json!({"imageId":image_id,"width":pixels.0,"height":pixels.1,"pixelWidth":pixels.0,"pixelHeight":pixels.1,
        "snapshotId":state.id,"detail":true,"originalWidth":source_pixels.0,"originalHeight":source_pixels.1,
        "region":{"x":region.x,"y":region.y,"width":region.width,"height":region.height},
        "desktopBounds":{"x":surface.x,"y":surface.y,"width":surface.width,"height":surface.height},
        "path":path});
    if let Some(images) = result.get_mut("images").and_then(|v| v.as_array_mut()) { images.push(entry); }
    state.shots.push(Shot { image_id, surface, pixels, source_pixels, region: Some(region), overlay, desktop_capture: false });
}

// Feedback stays on the input image's monitor, never on whichever popup gained focus.
fn observe(owner: &str, previous_window: Option<u32>, monitor_id: Option<u32>, desktop: bool, max_edge: u32, fresh: Option<u64>, state: &mut Option<Snapshot>, result: &mut Value) {
    if background::active() {
        // 后台没有「前台」可跟随：始终重拍同一目标（macOS 的同一窗口 / Linux 座席桌面），不回退到用户的桌面。
        match capture(owner, previous_window, None, None, max_edge, fresh, None, state) {
            Ok(value) => result.as_object_mut().unwrap().extend(value.as_object().unwrap().clone()),
            Err(e) => result["observationError"] = json!(e),
        }
        return;
    }
    let monitor_id = if desktop { None } else { monitor_id };
    let observation = capture(owner, None, monitor_id, None, max_edge, fresh, None, state);
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
    fn control_rects_round_trip_to_the_image_pixels_they_will_click() {
        // 负坐标屏 + 裁剪 + 缩放：UIA 矩形按屏幕像素给出，换算回来必须落在同一格附近。
        let shot = Shot {
            image_id: "monitor-0-region".into(),
            surface: Surface { id: 0, pid: None, x: -1920, y: 120, width: 1920, height: 1080 },
            pixels: (1600, 900),
            source_pixels: (1920, 1080),
            region: Some(Region { x: 100, y: 50, width: 800, height: 600 }),
            overlay: None,
            desktop_capture: true,
        };
        for (x, y) in [(0_i32, 0_i32), (1, 1), (799, 599), (400, 300)] {
            // point() 瞄准像素中心，反算回来允许一个像素内的整数截断。
            let screen = point(&shot, Some(x), Some(y)).expect("in range");
            let back = source_to_image(&shot, screen_to_source(&shot, screen.0, screen.1));
            assert!((back.0 - x as f64).abs() <= 1.0 && (back.1 - y as f64).abs() <= 1.0, "pixel {x},{y} -> {back:?}");
        }
        // 裁剪区左上角正好是图片 (0,0)，差一个 region 偏移就会被这里抓住。
        let (x0, y0) = source_to_image(&shot, screen_to_source(&shot, -1920 + 100, 120 + 50));
        assert!(x0.abs() < 1e-6 && y0.abs() < 1e-6, "{x0},{y0}");
    }
    #[test]
    fn lyra_shots_live_under_session_tool_results_and_others_stay_global() {
        let session = std::env::temp_dir().join("alkaid").join("tool-results").join("abc-1");
        assert_eq!(shot_folder(&session.to_string_lossy(), "browser-shots"), session.join("browser-shots"));
        for owner in ["3f2a-uuid".into(), session.join("..").to_string_lossy().into_owned(), session.with_file_name("a b").to_string_lossy().into_owned()] {
            assert!(!shot_folder(&owner, "desktop-shots").starts_with(session.parent().unwrap()), "{owner}");
        }
    }
    #[cfg(windows)]
    #[test]
    fn desktop_requests_restore_the_callers_dpi_context_on_error() {
        use windows_sys::Win32::UI::HiDpi::*;
        unsafe {
            let original = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_UNAWARE);
            assert!(!original.is_null());
            let before = GetThreadDpiAwarenessContext();
            // Rejected before capture or input: safe to run without touching the desktop.
            let result = run_inner("dpi-test".into(),json!({"operation":"screenshot","maxEdge":1}));
            let restored = AreDpiAwarenessContextsEqual(before,GetThreadDpiAwarenessContext());
            SetThreadDpiAwarenessContext(original);
            assert!(result.unwrap_err().contains("maxEdge"));
            assert_ne!(restored,0,"blocking-pool callers must not inherit our physical coordinate scope");
        }
    }

    #[test]
    fn nova_refreshes_do_not_force_new_desktop_frames() {
        use xcap::image::{Rgba, RgbaImage};
        use crate::visual_guard::PixelRect;
        let dir = tempfile::tempdir().unwrap();
        let page = RgbaImage::from_pixel(640, 480, Rgba([255, 255, 255, 255]));
        let ignored = [PixelRect { left:0, top:0, right:96, bottom:480 },
            PixelRect { left:560, top:400, right:640, bottom:480 }];
        let deliver = |id: &str, prior: Option<&Delivered>, image: &RgbaImage, regions: &[PixelRect]| {
            let mut entry = json!({"imageId":"monitor-1","snapshotId":id,"ignoredRegions":regions});
            let frame = crate::visual_guard::deliver_frame(&mut entry, prior, Some("monitor-1".into()),
                nova_regions::comparison_pixels(image, regions), Some(image), dir.path().join(format!("{id}.webp")), None, None, None).unwrap();
            (entry, frame.unwrap())
        };
        let (_, base) = deliver("key", None, &page, &ignored);
        let saved = xcap::image::open(&base.path).unwrap().to_rgba8();
        assert_eq!(saved, page, "actual screenshot must not be blacked out or lose pixels");
        let mut refresh = page.clone();
        refresh.put_pixel(48, 32, Rgba([0,0,0,255]));
        refresh.put_pixel(608, 448, Rgba([0,0,0,255]));
        let (same, _) = deliver("refresh", Some(&base), &refresh, &ignored);
        assert_eq!(same["unchanged"], true);
        refresh.put_pixel(320, 240, Rgba([0,0,0,255]));
        let (target, next) = deliver("target", Some(&base), &refresh, &ignored);
        assert!(target["unchanged"].is_null() && next.path != base.path);
        // Moving/removing an exclusion must expose the real scene, not preserve stale hidden pixels.
        let (revealed, _) = deliver("revealed", Some(&next), &refresh, &[]);
        assert!(revealed["unchanged"].is_null());
    }

    #[test]
    fn monitor_frames_keep_independent_bases() {
        use xcap::image::{Rgba, RgbaImage};
        let dir = tempfile::tempdir().unwrap();
        let page = RgbaImage::from_pixel(320, 320, Rgba([255,255,255,255]));
        let shot = Shot { image_id:"monitor-1".into(), surface:Surface{id:1,pid:None,x:-1920,y:-100,width:1920,height:1080},
            pixels:(320,320), source_pixels:(3840,2160), region:None,  overlay:None, desktop_capture:true };
        let other = Shot { image_id:"monitor-2".into(), surface:Surface{id:2,x:0,y:0,..shot.surface.clone()}, ..shot.clone() };
        let deliver = |shot: &Shot, prior: Option<&Delivered>, name: &str, pixels: &RgbaImage| {
            let mut entry = json!({"imageId":shot.image_id,"snapshotId":name,
                "desktopBounds":{"x":shot.surface.x,"y":shot.surface.y,"width":shot.surface.width,"height":shot.surface.height},
                "originalWidth":shot.source_pixels.0,"originalHeight":shot.source_pixels.1});
            let frame = crate::visual_guard::deliver_frame(&mut entry,prior,Some(shot.image_id.clone()),pixels.clone(),None,dir.path().join(format!("{name}.webp")), None, None, None).unwrap();
            (entry, frame.unwrap())
        };
        let (_, first) = deliver(&shot,None,"first",&page);
        let (_, second) = deliver(&other,None,"second",&page);
        let (entry, _) = deliver(&shot,Some(&first),"same",&page);
        assert_eq!(entry["unchanged"], true);
        let (entry, _) = deliver(&other,Some(&second),"same-other",&page);
        assert_eq!(entry["unchanged"], true);
        let (entry, _) = deliver(&other,Some(&first),"wrong-stream",&page);
        assert!(entry["unchanged"].is_null());
        // Layout/native scale changes must reset the basis even if delivered dimensions are identical.
        for moved in [Shot{surface:Surface{x:10,..shot.surface.clone()},..shot.clone()},
            Shot{source_pixels:(1920,1080),..shot.clone()}] {
            let (entry,_) = deliver(&moved,Some(&first),"new-basis",&page);
            assert!(entry["unchanged"].is_null());
        }
        // Full-image coordinates map through a fractional scale and negative monitor origin.
        assert_eq!(point(&shot,Some(150),Some(150)).unwrap(),(-1017,407));
        assert!(point(&shot,Some(320),Some(0)).is_err());
        assert!(point(&shot,Some(-1),Some(0)).is_err());
        assert!(action_shot(&[shot.clone(),other.clone()],None).is_err());
        assert_eq!(action_shot(&[shot.clone(),other],Some("monitor-1")).unwrap().image_id,"monitor-1");
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
            pixels:(1600,800),source_pixels:(3840,2160),region:Some(Region{x:200,y:100,width:800,height:400}),overlay:None,desktop_capture:false };
        assert_eq!(point(&shot, Some(800), Some(400)).unwrap(), (-1620,50));
    }    #[test]
    fn local_image_regions_and_actual_pointer_validation() {
        let shot=Shot {image_id:"crop".into(),surface:Surface{id:1,pid:None,x:-1920,y:-100,width:1920,height:1080},
            pixels:(400,200),source_pixels:(3840,2160),region:Some(Region{x:200,y:100,width:800,height:400}),overlay:None,desktop_capture:false};
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
        let result = run("validation".into(), json!({"operation":"act", "snapshotId":"old", "unexpected":true, "notes":"unverified notes"})).unwrap();
        assert_eq!(result["operation"], "act");
        assert_eq!(result["basedOnSnapshotId"], "old");
        assert_eq!(result["verification"], "unverified");
        assert!(result.get("verificationNotice").is_none());
        assert_eq!(result["status"], "not_executed");
        assert_eq!(result["completedActions"], 0);
        assert_eq!(result["inputAttempted"], false);
        assert!(result["error"].is_string());
        assert_eq!(result["notes"], "unverified notes");
    }
    #[cfg(windows)]
    #[test]
    #[ignore = "needs a real desktop: cargo test --lib elements_reports_screen_pixels_without_a_snapshot -- --ignored"]
    fn elements_reports_screen_pixels_without_a_snapshot() {
        // 没有快照也要能读控件树：坐标原样是屏幕物理像素，不做图片换算。
        let own = std::process::id();
        let target = xcap::Window::all().ok().and_then(|all| {
            all.into_iter().find(|w| {
                w.pid().ok() != Some(own) && w.is_minimized().ok() == Some(false) && w.width().ok().unwrap_or(0) > 200
            })
        });
        let Some(window) = target else {
            eprintln!("skipped: 没有其它可见窗口可观察");
            return;
        };
        let request: Request = serde_json::from_value(json!({"operation":"elements","maxItems":5})).unwrap();
        let value = controls(window.id().unwrap(), None, &request).expect("controls");
        assert_eq!(value["coordinateSpace"], "screen-pixels");
        assert!(value["snapshotId"].is_null() && value["imageId"].is_null());
        for item in value["items"].as_array().unwrap().iter() {
            let (x, y) = (item["x"].as_i64().unwrap(), item["y"].as_i64().unwrap());
            let (w, h) = (item["width"].as_i64().unwrap(), item["height"].as_i64().unwrap());
            assert!(w > 0 && h > 0, "{item}");
            assert_eq!(item["center"]["x"].as_i64().unwrap(), x + w / 2);
            assert_eq!(item["center"]["y"].as_i64().unwrap(), y + h / 2);
        }
        eprintln!(
            "windowId={} items={} first={:?}",
            window.id().unwrap(),
            value["items"].as_array().unwrap().len(),
            value["items"].as_array().unwrap().first().map(|item| item["name"].clone())
        );
    }

    #[test]
    fn screen_space_skips_the_shot_and_keeps_keyboard_out() {
        let request = |v: Value| serde_json::from_value::<Request>(v).unwrap();
        let actions = |v: Value| serde_json::from_value::<Vec<Action>>(v).unwrap();
        // 屏幕空间允许负坐标（多显示器在左/上），图片空间仍必须有图。
        assert_eq!(resolve_point(None, Space::Screen, Some(-1200), Some(40)).unwrap(), (-1200, 40));
        assert!(resolve_point(None, Space::Image, Some(10), Some(10)).is_err());
        // 屏幕模式只做指针动作：键盘/文字的焦点校验必须先有一张观察。
        for batch in [json!([{"action":"type","text":"x"}]), json!([{"action":"press","key":"Enter"}])] {
            let err = screen_precheck(&request(json!({"operation":"act"})), &actions(batch)).unwrap_err();
            assert!(err.contains("type/press"), "{err}");
        }
        // 不需要也不接受观察/后台模式参数。
        let click = actions(json!([{"action":"click","x":1,"y":2}]));
        for extra in [json!({"background":true}), json!({"imageId":"i"}), json!({"snapshotId":"s"})] {
            let mut args = json!({"operation":"act"});
            for (key, value) in extra.as_object().unwrap() { args[key.as_str()] = value.clone(); }
            assert!(screen_precheck(&request(args), &click).is_err(), "{extra}");
        }
        assert!(screen_precheck(&request(json!({"operation":"act"})), &click).is_ok());
        // 坐标缺失仍按动作校验：scroll 需要定位点，drag 需要终点。
        assert!(screen_precheck(&request(json!({"operation":"act"})), &actions(json!([{"action":"scroll","delta":1}]))).is_err());
        assert!(screen_precheck(&request(json!({"operation":"act"})), &actions(json!([{"action":"drag","x":1,"y":2}]))).is_err());
    }

    #[test]
    fn taskbar_overlay_updates_only_exempt_unchanged_geometry() {
        use xcap::image::{Rgba, RgbaImage};
        use crate::taskbar_status::Rect;
        let original = RgbaImage::from_pixel(200, 200, Rgba([20,20,20,255]));
        let bounds = Rect { left:-50, top:-30, right:-20, bottom:30 };
        let shot = Shot { image_id:"crop".into(), surface:Surface{id:1,pid:None,x:-100,y:-50,width:100,height:100},
            pixels:(100,100), source_pixels:(200,200), region:Some(Region{x:40,y:40,width:100,height:100}),
            overlay:Some(bounds), desktop_capture:true };
        let exclude = overlay_region(&shot.surface, shot.source_pixels, shot.overlay).unwrap();
        assert_eq!(exclude, crate::visual_guard::PixelRect { left:100, top:40, right:160, bottom:160 });
        let mut updated = original.clone();
        for y in exclude.top..exclude.bottom { for x in exclude.left..exclude.right {
            updated.put_pixel(x,y,Rgba([220,220,220,255]));
        } }
        let action:Action = serde_json::from_value(json!({"action":"click","x":56,"y":60})).unwrap();
        assert_eq!(point(&shot,action.x,action.y).unwrap(),(-52,0));
        let still = |a: &RgbaImage, b: &RgbaImage, ex| !changed_tiles(a, b, ex).unwrap().contains(&true);
        assert!(still(&original,&updated,Some(exclude)));
        assert!(!still(&original,&updated,None));
        // Comparisons never mask or alter the pixels returned to the model.
        assert_eq!(updated.get_pixel(100,100).0,[220,220,220,255]);
        for current in [None, Some(Rect{left:-49,..bounds}), Some(Rect{right:-19,..bounds}),
            Some(Rect{top:-29,..bounds}), Some(Rect{bottom:31,..bounds})] {
            assert!(check_overlay(&shot,&action,current).unwrap().is_none());
        }
        let absent = Shot { overlay:None, ..shot.clone() };
        assert!(check_overlay(&absent,&action,Some(bounds)).unwrap().is_none());
        for value in [json!({"action":"click","x":60,"y":60}),
            json!({"action":"drag","x":56,"y":60,"toX":60,"toY":60})] {
            let inside:Action = serde_json::from_value(value).unwrap();
            assert!(check_overlay(&shot,&inside,Some(bounds)).is_err());
        }
        for y in 96..105 { for x in 92..100 { updated.put_pixel(x,y,Rgba([200,0,0,255])); } }
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
        // Round trip with a 1920→1600 downscale: image (485,151) clicks desktop (582,181) and is marked back there.
        let full = Surface { id:1, pid:None, x:0, y:0, width:1920, height:1080 };
        let mut shot = xcap::image::RgbaImage::new(1600, 900);
        assert_eq!(mark_cursor(&mut shot, &full, (1920,1080), None, (582,181)), Some((485,151)));
    }
    #[test]
    fn image_pixels_land_on_the_screen_pixel_that_contains_their_centre() {
        // Non-1:1 observation, fractional scale, negative origin, and a crop.
        let cases = [
            Shot { image_id:"full".into(), surface:Surface{id:1,pid:None,x:0,y:0,width:1920,height:1080},
                pixels:(1430,804), source_pixels:(1920,1080), region:None, overlay:None, desktop_capture:true },
            Shot { image_id:"dpi".into(), surface:Surface{id:1,pid:None,x:0,y:0,width:2880,height:1620},
                pixels:(1440,810), source_pixels:(2880,1620), region:None, overlay:None, desktop_capture:true },
            Shot { image_id:"left".into(), surface:Surface{id:2,pid:None,x:-1920,y:-1080,width:1920,height:1080},
                pixels:(800,450), source_pixels:(1920,1080), region:None, overlay:None, desktop_capture:true },
            Shot { image_id:"crop".into(), surface:Surface{id:1,pid:None,x:2560,y:-120,width:1920,height:1080},
                pixels:(400,200), source_pixels:(3840,2160), region:Some(Region{x:200,y:100,width:800,height:400}),
                overlay:None, desktop_capture:false },
        ];
        for shot in &cases {
            let r = shot.region.unwrap_or(Region { x:0, y:0, width:shot.source_pixels.0, height:shot.source_pixels.1 });
            for (x, y) in [(0i32, 0), (1, 1), (shot.pixels.0 as i32 / 2, shot.pixels.1 as i32 / 2), (shot.pixels.0 as i32 - 1, shot.pixels.1 as i32 - 1)] {
                let (sx, sy) = point(shot, Some(x), Some(y)).unwrap();
                let continuous = |axis_image: i32, image: u32, region_origin: u32, region: u32, source: u32, origin: i32, surface: u32| {
                    origin as f64 + (region_origin as f64 + (axis_image as f64 + 0.5) * region as f64 / image as f64) * surface as f64 / source as f64
                };
                let cx = continuous(x, shot.pixels.0, r.x, r.width, shot.source_pixels.0, shot.surface.x, shot.surface.width);
                let cy = continuous(y, shot.pixels.1, r.y, r.height, shot.source_pixels.1, shot.surface.y, shot.surface.height);
                assert!(sx as f64 <= cx && cx < sx as f64 + 1.0, "{} pixel {x} -> screen {sx}, centre {cx}", shot.image_id);
                assert!(sy as f64 <= cy && cy < sy as f64 + 1.0, "{} pixel {y} -> screen {sy}, centre {cy}", shot.image_id);
                let back = cursor_tip(&shot.surface, shot.source_pixels, shot.region, (sx, sy), shot.pixels).unwrap();
                assert!((back.0 as i32 - x).abs() <= 1 && (back.1 as i32 - y).abs() <= 1, "{} round trip {x},{y} -> {back:?}", shot.image_id);
            }
        }
        // 150% framebuffer: the image centre is the physical centre, not the 1920×1080 logical centre.
        let physical = &cases[1];
        let (x, y) = point(physical, Some(720), Some(405)).unwrap();
        assert!((x - 1440).abs() <= 1 && (y - 810).abs() <= 1, "physical centre {x},{y}");
        let logical = Shot { surface: Surface { width: 1920, height: 1080, ..physical.surface.clone() },
            source_pixels: (1920, 1080), ..physical.clone() };
        let (lx, ly) = point(&logical, Some(720), Some(405)).unwrap();
        assert!((lx - x).abs() > 400 && (ly - y).abs() > 200, "logical surface would miss: {lx},{ly} vs {x},{y}");
        // Secondary monitor origin is not dropped.
        let right = Shot { image_id:"right".into(), surface:Surface{id:3,pid:None,x:2560,y:-120,width:1920,height:1080},
            pixels:(800,450), source_pixels:(1920,1080), region:None, overlay:None, desktop_capture:true };
        assert_eq!(point(&right, Some(0), Some(0)).unwrap(), (2561, -119));
        assert_eq!(point(&right, Some(799), Some(449)).unwrap(), (4478, 958));
        assert_eq!(cursor_tip(&right.surface, right.source_pixels, None, (2561, -119), right.pixels), Some((0, 0)));
    }
    #[test]
    fn element_center_clicks_inside_the_control_on_a_scaled_monitor() {
        let shot = Shot { image_id:"m".into(), surface:Surface{id:1,pid:None,x:-1920,y:0,width:2880,height:1620},
            pixels:(1440,810), source_pixels:(2880,1620), region:None, overlay:None, desktop_capture:true };
        // A 40×16 physical control. Integer centre of the image rect is not the pixel point() clicks.
        let (ix, iy, iw, ih, cx, cy) = control_image_rect(&shot, -400, 200, 40, 16).unwrap();
        assert!(cx >= ix && cx < ix + iw && cy >= iy && cy < iy + ih, "center {cx},{cy} outside {ix},{iy} {iw}x{ih}");
        let (sx, sy) = point(&shot, Some(cx as i32), Some(cy as i32)).unwrap();
        assert!((-400..-360).contains(&sx) && (200..216).contains(&sy), "click {sx},{sy} missed the control");
        // Mixed-DPI origin: DEVMODE might say 1707 while the cursor space says 2560. Clicks use the latter.
        assert_eq!(desktop_rect((1707, 0, 1920, 1080), Some((2560, 0, 1920, 1080))), (2560, 0, 1920, 1080));
        assert_eq!(desktop_rect((0, 0, 1920, 1080), None), (0, 0, 1920, 1080));
        assert_eq!(desktop_rect((0, 0, 1920, 1080), Some((0, 0, 0, 1080))), (0, 0, 1920, 1080));
    }
    #[test]
    fn readback_corrects_once_and_still_rejects_a_wrong_monitor() {
        assert!(readback_accepts((10, 10), (12, 11), None));
        assert!(readback_accepts((10, 10), (14, 10), Some((10, 10))));
        assert!(!readback_accepts((2250, 55), (2054, 188), None));
        assert!(!readback_accepts((2250, 55), (2054, 188), Some((2052, 188))));
        assert!(!readback_accepts((i32::MIN, 0), (i32::MAX, 0), Some((i32::MAX, 0))));
    }
    #[test]
    fn fitted_observations_stay_inside_the_vision_budget() {
        assert_eq!((fit_target(1920, 1080, 1600, false).1, fit_target(1920, 1080, 1600, false).2), (1430, 804));
        for w in [1u32, 2, 100, 1569, 1920, 2560, 3440, 3840, 5120, 7680] {
            for h in [1u32, 720, 1080, 1440, 1600, 2160, 4320] {
                for upscale in [false, true] {
                    let (_, tw, th) = fit_target(w, h, 1600, upscale);
                    assert!(tw.max(th) <= 1568, "{w}x{h} upscale={upscale} -> {tw}x{th}");
                    assert!((tw as u64) * (th as u64) <= 1_150_000, "{w}x{h} -> {tw}x{th}");
                    assert!(crate::lyra::bridge::vision_scale(tw as f64, th as f64) >= 1.0, "{tw}x{th} would be downscaled again");
                }
            }
        }
        // A crop may ask to grow, and still must not cross the budget the model will enforce.
        let (_, tw, th) = fit_target(100, 50, 4000, true);
        assert!((tw as u64) * (th as u64) <= 1_150_000 && tw.max(th) <= 1568, "{tw}x{th}");
    }
    #[test]
    fn crop_edge_does_not_replace_the_desktop_and_clicks_stay_on_their_image() {
        assert_eq!((fit_target(2880, 1800, 1600, false).1, fit_target(2880, 1800, 1600, false).2), (1356, 848));
        assert_eq!((fit_target(2880, 1800, 1200, false).1, fit_target(2880, 1800, 1200, false).2), (1200, 750));
        assert_eq!(next_desktop_edge(Some(1600), 1200, true), 1600);
        assert_eq!(next_desktop_edge(Some(1600), 800, true), 1600);
        assert_eq!(next_desktop_edge(Some(1600), 1200, false), 1200);
        assert_eq!(next_desktop_edge(None, 900, true), 1600);
        assert_eq!(feedback_edge(None, 1600), 1600);
        assert_eq!(feedback_edge(Some(900), 1600), 900);

        let full = Shot { image_id:"monitor-1".into(), surface:Surface{id:1,pid:None,x:0,y:0,width:2880,height:1800},
            pixels:(1356,848), source_pixels:(2880,1800), region:None, overlay:None, desktop_capture:true };
        let source = image_region(&full, Region { x:370, y:480, width:400, height:180 }).unwrap();
        assert_eq!((source.x, source.y, source.width, source.height), (785, 1018, 851, 383));
        let (_, zw, zh) = fit_target(source.width, source.height, 1200, true);
        assert_eq!((zw, zh), (1200, 540));
        let zoom = Shot { image_id:"crop".into(), surface:full.surface.clone(), pixels:(zw, zh),
            source_pixels:full.source_pixels, region:Some(source), overlay:None, desktop_capture:false };
        let screen = point(&zoom, Some(60), Some(427)).unwrap();
        let on_full = cursor_tip(&full.surface, full.source_pixels, None, screen, full.pixels).unwrap();
        let on_stuck = cursor_tip(&full.surface, full.source_pixels, None, screen, (1200, 750)).unwrap();
        assert_ne!(on_full, on_stuck, "reading the same screen point on the stuck 1200-wide frame is a different pixel");
        let back = point(&full, Some(on_full.0 as i32), Some(on_full.1 as i32)).unwrap();
        assert!((back.0 - screen.0).abs() <= 1 && (back.1 - screen.1).abs() <= 1, "feedback pixel {on_full:?} must map back to {screen:?}, got {back:?}");

        let miss = Miss { image_id:"monitor-1".into(), x:387, y:577 };
        assert!(blocks_blind_repeat("click", Some(&miss), "monitor-1", 385, 570));
        assert!(!blocks_blind_repeat("click", Some(&miss), "monitor-1", 396, 577));
        assert!(!blocks_blind_repeat("double_click", Some(&miss), "monitor-1", 387, 577));
        assert!(!blocks_blind_repeat("click", Some(&miss), "monitor-1-detail-1-2-3-4", 387, 577));
        assert!(!blocks_blind_repeat("click", None, "monitor-1", 387, 577));
        assert!(!blocks_blind_repeat("type", Some(&miss), "monitor-1", 387, 577));

        use xcap::image::{Rgba, RgbaImage};
        let surface = Surface { id:1, pid:None, x:0, y:0, width:200, height:120 };
        let base = RgbaImage::from_pixel(200, 120, Rgba([12, 12, 12, 255]));
        let before = window_samples_on(&base, &surface, (0, 0, 128, 96));
        let mut outside = base.clone();
        outside.put_pixel(180, 100, Rgba([255, 0, 0, 255]));
        assert!(!window_painted(&before, &window_samples_on(&outside, &surface, (0, 0, 128, 96))));
        let mut inside = base.clone();
        for (x, y) in [(0u32, 0u32), (32, 0), (64, 0), (0, 32)] {
            inside.put_pixel(x, y, Rgba([240, 240, 240, 255]));
        }
        assert!(window_painted(&before, &window_samples_on(&inside, &surface, (0, 0, 128, 96))));
        assert!(!window_painted(&[], &[]));
        let tiny_surface = Surface { id:1, pid:None, x:0, y:0, width:40, height:40 };
        let tiny = RgbaImage::from_pixel(40, 40, Rgba([4, 4, 4, 255]));
        let tiny_before = window_samples_on(&tiny, &tiny_surface, (0, 0, 20, 20));
        assert!(tiny_before.len() < 4 && !tiny_before.is_empty());
        assert!(!window_painted(&tiny_before, &tiny_before));
        let mut tiny_after = tiny.clone();
        tiny_after.put_pixel(0, 0, Rgba([255, 0, 0, 255]));
        assert!(window_painted(&tiny_before, &window_samples_on(&tiny_after, &tiny_surface, (0, 0, 20, 20))));

        let frame = |stamp, image: RgbaImage| Delivered {
            stream:"monitor-1".into(), basis:json!(1), path:std::path::PathBuf::from("a"),
            seen:std::sync::Arc::new(image.clone()), clean:Some(std::sync::Arc::new(image)),
            marker:None, stamp, rows:None,
        };
        let still = RgbaImage::from_pixel(80, 80, Rgba([8, 8, 8, 255]));
        let same = frame(5, still.clone());
        assert!(click_unchanged(&same, &frame(5, still.clone()), 10.0, 10.0));
        // The source stamp is what says the desktop changed. One sample inside an identical stamp
        // is the anti-alias noise the neighbourhood check already ignores.
        let mut speck = still.clone();
        speck.put_pixel(10, 10, Rgba([255, 0, 0, 255]));
        assert!(click_unchanged(&same, &frame(5, speck.clone()), 10.0, 10.0));
        assert!(!click_unchanged(&same, &frame(9, speck), 10.0, 10.0));
        let mut moved = still.clone();
        for y in 6..16 { for x in 6..16 { moved.put_pixel(x, y, Rgba([255, 0, 0, 255])); } }
        assert!(!click_unchanged(&same, &frame(5, moved), 10.0, 10.0));
        assert!(!click_unchanged(&same, &frame(6, still.clone()), 10.0, 10.0));
        assert!(!click_unchanged(&same, &frame(0, still), 10.0, 10.0));
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
            let encoded = crate::visual_guard::encode_webp(&resized).unwrap();
            let encode_ms = started.elapsed().as_millis();
            assert_eq!(xcap::image::load_from_memory(&encoded).unwrap().to_rgba8(), resized);
            eprintln!("screenshot pipeline (lossless WebP): resize={resize_ms}ms encode={encode_ms}ms bytes={}", encoded.len());
        }
    }
    #[cfg(windows)]
    #[test]
    fn windows_shortcuts() {
        for letter in b'a'..=b'z' {
            let lower = keys(&format!("Ctrl+{}", letter as char)).unwrap();
            let upper = keys(&format!("Ctrl+{}", letter.to_ascii_uppercase() as char)).unwrap();
            assert_eq!(lower, upper);
            assert_eq!(upper, vec![Key::Control, Key::Other(letter.to_ascii_uppercase() as u32)]);
        }
        assert_eq!(keys("Ctrl+Shift+S").unwrap(), vec![Key::Control, Key::Shift, Key::Other(0x53)]);
        assert_eq!(keys("Win+1").unwrap(), vec![Key::Meta, Key::Other(0x31)]);

    }
    #[test]
    fn glide_ends_exactly_and_stays_bounded() {
        for (from, to, bend) in [((0, 0), (800, 300), 0.1), ((500, 500), (10, 20), -0.1), ((7, 7), (7, 7), 0.05)] {
            assert_eq!(glide_at(from, to, bend, 0.0), from);
            assert_eq!(glide_at(from, to, bend, 1.0), to);
            let ms = glide_ms(from, to);
            assert!((40.0..=210.0).contains(&ms), "{ms}");
            let d = |p: (i32, i32)| ((to.0 - p.0) as f64).hypot((to.1 - p.1) as f64);
            let path: Vec<_> = (0..=50).map(|i| glide_at(from, to, bend, i as f64 / 50.0)).collect();
            // Approaches the target without overshooting or wandering back.
            assert!(path.windows(2).all(|w| d(w[1]) <= d(w[0]) + 1.0), "{from:?}->{to:?}");
        }
        assert!(glide_ms((0, 0), (2000, 0)) > glide_ms((0, 0), (50, 0)));
    }

    #[test]
    fn settle_waits_for_a_paint_then_a_quiet_frame() {
        use crate::visual_guard::DesktopPulse::*;
        assert_eq!(settle_step(false, Pointer), (false, false));
        assert_eq!(settle_step(false, Minor), (false, false));
        assert_eq!(settle_step(false, Quiet), (false, false));
        assert_eq!(settle_step(false, Content), (true, false), "the first paint is not the end: a menu may still be drawing");
        assert_eq!(settle_step(true, Content), (true, false));
        assert_eq!(settle_step(true, Pointer), (true, false));
        assert_eq!(settle_step(true, Minor), (true, false));
        assert_eq!(settle_step(true, Quiet), (true, true));
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
        assert_eq!(delays, [0, 0, 0, 0, 0, 300, 180]);
        let chained: Vec<Action> = serde_json::from_value(json!([
            {"action":"click","x":10,"y":10}, {"action":"type","text":"a"},
            {"action":"press","key":"Enter"}, {"action":"press","key":"Tab"}
        ])).unwrap();
        let delays: Vec<_> = (0..chained.len()).map(|i| action_delay(&chained, i).as_millis()).collect();
        assert_eq!(delays, [300, 300, 60, 180]);
        for action in ["click", "double_click", "drag", "type", "press", "scroll", "wait", "move"] {
            let batch: Vec<Action> = serde_json::from_value(json!([{ "action": action }])).unwrap();
            assert_eq!(action_delay(&batch, 0).as_millis(), if matches!(action, "move" | "wait") { 0 } else { 180 }, "{action}");
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
    fn executed_notice_reports_only_completed_inputs_and_intent() {
        let actions: Vec<Action> = serde_json::from_value(json!([
            {"action":"click","x":649,"y":341}, {"action":"wait","ms":200},
            {"action":"press","key":"Enter"}])).unwrap();
        assert_eq!(executed_notice(&actions, 2, Some("发送搜索结果里的表情")).as_deref(),
            Some("本批已执行：click(649,341)。你为这次操作写的意图：发送搜索结果里的表情"));
        assert_eq!(executed_notice(&actions, 1, None).as_deref(), Some("本批已执行：click(649,341)。"));
        assert_eq!(executed_notice(&actions, 3, Some(" ")).as_deref(),
            Some("本批已执行：click(649,341)、press(Enter)。"));
        assert!(executed_notice(&actions, 0, Some("发送")).is_none());
        assert!(executed_notice(&actions[1..2], 1, None).is_none());
        let others: Vec<Action> = serde_json::from_value(json!([
            {"action":"drag","x":1,"y":2,"toX":3,"toY":4},
            {"action":"scroll","x":5,"y":6,"delta":-2,"axis":"horizontal"},
            {"action":"type","text":"你好🦀"}])).unwrap();
        assert_eq!(executed_notice(&others, 3, None).as_deref(),
            Some("本批已执行：drag(1,2→3,4)、scroll(horizontal,-2)、type(3字符)。"));
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
        let shot = Shot { overlay:None, desktop_capture:false,
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
        let scaled = Shot { pixels: (960, 540), ..shot.clone() };
        assert_eq!(point(&scaled, Some(480), Some(270)).unwrap(), (-959, 541));
        assert_eq!(point(&scaled, Some(959), Some(539)).unwrap(), (-1, 1079));
        let desktop = Shot { surface: Surface { x:0, y:0, ..shot.surface.clone() }, pixels:(1600,900), ..shot.clone() };
        // The reported click was on the toolbar in the source image, not a DPI offset.
        assert_eq!(point(&desktop, Some(860), Some(520)).unwrap(), (1032, 624));
        let region = Region { x:200, y:100, width:800, height:400 };
        region.validate(shot.source_pixels).unwrap();
        let cropped = Shot { region:Some(region), pixels:(400,200), ..shot.clone() };
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
    fn omitted_snapshot_binds_only_the_owned_live_mode_without_refreshing_it() {
        let mut snap = Snapshot {id:"current".into(),owner:"owner".into(),taken:Instant::now(),window:None,max_edge:1600,desktop_edge:1600,
            foreground:None,shots:vec![],invalidated:None,background:false,delivered:vec![],missed:None};
        let taken = snap.taken;
        for mut id in [None,Some(String::new()),Some("  ".into())] {
            assert!(bind_input_snapshot(&mut id,Some(&snap),"owner",false).is_some());
            assert_eq!(id.as_deref(),Some("current")); assert_eq!(snap.taken,taken);
        }
        let mut explicit = Some("old".into());
        assert!(bind_input_snapshot(&mut explicit,Some(&snap),"owner",false).is_some());
        assert_eq!(explicit.as_deref(),Some("old"),"an explicit different basis is never silently replaced");
        for (owner,background,invalidated,expired) in [("other",false,false,false),("owner",true,false,false),
            ("owner",false,true,false),("owner",false,false,true)] {
            snap.invalidated=invalidated.then(||"consumed".into());
            snap.taken=if expired {taken-Duration::from_secs(181)} else {taken};
            let mut id=None;
            assert!(bind_input_snapshot(&mut id,Some(&snap),owner,background).is_none()); assert!(id.is_none());
        }
        let mut id=None; assert!(bind_input_snapshot(&mut id,None,"owner",false).is_none());
    }

    #[test]
    fn invalid_wait_rejects_whole_batch_before_input() {
        let shot = Shot { overlay:None, desktop_capture:false, image_id:"monitor-1".into(),
            surface:Surface {id:1,pid:None,x:0,y:0,width:100,height:100}, pixels:(100,100), source_pixels:(100,100), region:None };
        *DESKTOP.lock().unwrap() = Some(Snapshot {id:"validation".into(),owner:"validation".into(),
            taken:Instant::now(),window:None,max_edge:1600,desktop_edge:1600,foreground:None,shots:vec![shot],invalidated:None,background:false,delivered:vec![],missed:None});
        let foreign = run("other-owner".into(), json!({"operation":"act","snapshotId":"old"})).unwrap();
        assert_eq!(foreign["completedActions"], 0);
        assert!(foreign.get("images").is_none());
        assert_eq!(DESKTOP.lock().unwrap().as_ref().unwrap().id, "validation");
        let original_taken = DESKTOP.lock().unwrap().as_ref().unwrap().taken;
        for request in [
            json!({"operation":"act","imageId":"monitor-1","actions":[{"action":"click","x":1,"y":1},{"action":"wait","ms":8000}]}),
            json!({"operation":"act","snapshotId":"","actions":[{"action":"click","x":1,"y":1},{"action":"wait","ms":8000}]}),
            json!({"actions":[{"action":"click","x":1,"y":1},{"action":"wait","ms":8000}]}),
        ] {
            // Invalid wait proves the omitted ID reached batch validation without injecting real input.
            let result = run("validation".into(), request).unwrap();
            assert!(result["error"].as_str().unwrap().contains("actions[1].ms=8000"));
            assert_eq!(result["status"], "not_executed");
            assert_eq!(result["completedActions"], 0);
            assert!(result.get("images").is_none(), "binding the current observation must not generate a new screenshot");
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
            assert_eq!(result["errorCode"], "observation_required");
            assert_eq!(result["inputAttempted"], false);
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
        observe("test", Some(u32::MAX), None, true, 1600, None, &mut snapshot, &mut observation);
        assert!(observation["windowId"].is_null());
        assert!(snapshot.is_some(), "{observation}");
        assert!(snapshot.unwrap().window.is_none());
        let mut focused_result = json!({});
        let mut focused_snapshot = None;
        observe("test", None, None, false, 1600, None, &mut focused_snapshot, &mut focused_result);
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
        assert_eq!(result["images"].as_array().unwrap().iter().filter(|i| i["deltaOf"].is_null()).count(), shot["images"].as_array().unwrap().len());
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
            "actions":[{"action":"press","key":"Tab"}]})).unwrap();
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
