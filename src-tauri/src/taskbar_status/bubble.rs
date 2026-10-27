//! A click on the taskbar Butterfly focuses the assistant in a reused transparent webview.
//! Hovering or dragging over the chip never opens it; only a failed drop surfaces it unfocused.
//! Finishing a turn does not open it.

use super::{Kind, Rect};
use serde_json::{json, Value};
use std::{
    sync::{atomic::{AtomicBool, AtomicUsize, Ordering}, Mutex},
    time::{Duration, Instant},
};
use tauri::{AppHandle, Emitter, Manager, WebviewWindow};
use windows_sys::Win32::{
    Foundation::{HWND, POINT, RECT},
    Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_TRANSITIONS_FORCEDISABLED},
    UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON, VK_RBUTTON},
    UI::WindowsAndMessaging::{GetAncestor, GetCursorPos, GetForegroundWindow, GetWindowLongPtrW, GetWindowRect, GA_ROOT, GA_ROOTOWNER, GWL_EXSTYLE, GetWindowDisplayAffinity, HWND_TOPMOST, IsWindowVisible, WDA_EXCLUDEFROMCAPTURE, WindowFromPoint, SetForegroundWindow, SetWindowLongPtrW, SetWindowPos, ShowWindow, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SWP_SHOWWINDOW, SW_HIDE, SW_SHOW, SW_SHOWNOACTIVATE, WS_EX_TOOLWINDOW},
};

const LABEL: &str = "taskbar-bubble";
/// CSS width including the transparent margin that holds the card's shadow.
const WIDTH: f64 = 560.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Anchor {
    /// Butterfly chip, physical pixels.
    pub chip: Rect,
    /// Work area of the chip's monitor, physical pixels.
    pub work: Rect,
    pub scale: f64,
}

static ANCHOR: Mutex<Option<Anchor>> = Mutex::new(None);
/// The live card, asked for by a click on the Butterfly.
static WANTED: AtomicBool = AtomicBool::new(false);
static CAPTURE_ERROR: Mutex<Option<String>> = Mutex::new(None);
/// Hover never takes focus and closes once the pointer leaves both the Butterfly and the panel.
/// Clicking inside pins the panel for keyboard input until blur or explicit dismissal.
static PANEL: AtomicBool = AtomicBool::new(false);
static PANEL_WATCH: AtomicBool = AtomicBool::new(false);
/// Opened by a right click: start on the quick actions instead of the assistant.
static QUICK: AtomicBool = AtomicBool::new(false);
/// Bumped by hide(): a watcher outliving a quick hide/reopen restarts its bookkeeping.
static OPENED: AtomicUsize = AtomicUsize::new(0);
static BUILD: Mutex<()> = Mutex::new(());
/// Finished result and its rendered revision, kept on the live card even after it is read.
static SHOWN: Mutex<Option<(String, i64)>> = Mutex::new(None);

/// Built once the Butterfly shows, so a click does not wait for a cold webview.
pub(super) fn window(app: &AppHandle) -> Option<WebviewWindow> {
    let _building = BUILD.lock().unwrap();
    if let Some(window) = app.get_webview_window(LABEL) { return Some(window); }
    let window = tauri::WebviewWindowBuilder::new(app, LABEL, tauri::WebviewUrl::App("index.html?bubble=1".into()))
        .title("Nova").decorations(false).resizable(false).shadow(false)
        .transparent(true).background_color(tauri::window::Color(0, 0, 0, 0))
        .always_on_top(true).skip_taskbar(true).visible(false).focused(false)
        // Wry's Windows native handler consumes text/URL drops without forwarding them.
        // This panel handles DOM files + text; the taskbar chip keeps its native OLE target.
        .disable_drag_drop_handler()
        .inner_size(WIDTH, 640.0)
        .build()
        .map_err(|error| eprintln!("[taskbar-status] cannot create bubble: {error}"))
        .ok()?;
    // Neither native nor CSS entrance transitions: show the assistant immediately. A native hide
    // transition can retain the old WebView surface and flash it at the taskbar edge.
    if let Ok(hwnd) = window.hwnd() {
        let disabled = 1i32;
        let result = unsafe { DwmSetWindowAttribute(hwnd.0 as _, DWMWA_TRANSITIONS_FORCEDISABLED as u32,
            &disabled as *const _ as _, size_of::<i32>() as u32) };
        if result < 0 { eprintln!("[taskbar-status] cannot disable bubble transitions: {result:#x}"); }
        // tao keeps WS_CAPTION on undecorated windows and strips it in WM_NCCALCSIZE, but the
        // frame computed during creation still has the caption; only tao's show() refreshed it.
        // We show natively (see focus()), so refresh the frame once here while still hidden.
        unsafe { SetWindowPos(hwnd.0 as _, std::ptr::null_mut(), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED); }
    }
    let handle = app.clone();
    let panel = window.hwnd().map_or(0, |hwnd| hwnd.0 as isize);
    window.on_window_event(move |event| match event {
        tauri::WindowEvent::Focused(true) => { PANEL.store(false, Ordering::Release); }
        // A file picker opened from the panel is owned by it: not a blur.
        tauri::WindowEvent::Focused(false) => {
            let fg = unsafe { GetForegroundWindow() };
            if fg.is_null() || unsafe { GetAncestor(fg, GA_ROOTOWNER) } as isize != panel { blur(&handle); }
        }
        tauri::WindowEvent::CloseRequested { api, .. } => {
            api.prevent_close();
            hide(&handle);
        }
        _ => {}
    });
    Some(window)
}


/// The first native drop may finish before the WebView's event listeners exist.
pub(super) fn capture_error(app: &AppHandle, error: &str) {
    *CAPTURE_ERROR.lock().unwrap() = Some(error.to_owned());
    let _ = app.emit_to(LABEL, "capsules:error", json!({ "error": error }));
}

pub(super) fn open(app: &AppHandle, anchor: Anchor, quick: bool) {
    PANEL.store(false, Ordering::Release);
    QUICK.store(quick, Ordering::Release);
    *ANCHOR.lock().unwrap() = Some(anchor);
    WANTED.store(true, Ordering::Release);
    // A freshly built page places itself on mount; a warm one reloads on this event.
    if window(app).is_some() { let _ = app.emit_to(LABEL, "taskbar-bubble:open", ()); }
    watch(app);
}

pub(super) fn toggle(app: &AppHandle) {
    if WANTED.load(Ordering::Acquire) { return hide(app); }
    if let Some(anchor) = super::windows::chip_anchor() { open(app, anchor, false); }
}

/// Unfocused preview after a failed drop: show the assistant unless the panel is already open.
pub(super) fn open_panel(app: &AppHandle, anchor: Anchor) {
    if WANTED.load(Ordering::Acquire) { return; }
    PANEL.store(true, Ordering::Release);
    QUICK.store(false, Ordering::Release);
    *ANCHOR.lock().unwrap() = Some(anchor);
    WANTED.store(true, Ordering::Release);
    if window(app).is_some() { let _ = app.emit_to(LABEL, "taskbar-bubble:open", ()); }
    watch(app);
}

fn watch(app: &AppHandle) {
    if PANEL_WATCH.swap(true, Ordering::AcqRel) { return; }
    let app = app.clone();
    std::thread::spawn(move || loop {
        watch_panel(&app);
        PANEL_WATCH.store(false, Ordering::Release);
        // An open racing this exit saw a live watcher and did not start its own.
        if !WANTED.load(Ordering::Acquire) || PANEL_WATCH.swap(true, Ordering::AcqRel) { break; }
    });
}

/// Input in a hover preview becomes an ordinary focused panel, no longer pointer-dismissed.
pub(super) fn activate(app: &AppHandle) {
    if !WANTED.load(Ordering::Acquire) { return; }
    PANEL.store(false, Ordering::Release);
    if let Some(window) = app.get_webview_window(LABEL) { focus(&window); }
}

/// Show and focus natively. Hover shows with ShowWindow, so tao's VISIBLE flag lags; tao's
/// show() would then flip it and re-apply the window style with SWP_FRAMECHANGED, which
/// flashes the transparent WebView on every click into the panel. hide() already hides
/// natively too, so tao's flag is never needed. Our process owns the click, so
/// SetForegroundWindow is permitted.
fn focus(window: &WebviewWindow) {
    let Ok(hwnd) = window.hwnd() else { return };
    let hwnd = hwnd.0 as windows_sys::Win32::Foundation::HWND;
    unsafe {
        // Already active: do nothing. Re-moving focus into the WebView blurs/refocuses the
        // textarea, whose focus handler calls activate() again, an endless focus loop.
        if IsWindowVisible(hwnd) != 0 && GetForegroundWindow() == hwnd { return; }
        if IsWindowVisible(hwnd) == 0 { ShowWindow(hwnd, SW_SHOW); }
        SetForegroundWindow(hwnd);
    }
    // A foreground HWND does not hand keyboard focus to WebView2 on its first activation
    // (keys then reach the bare host window: no caret, IME at 0,0). Move it in explicitly.
    let _ = AsRef::<tauri::Webview>::as_ref(window).set_focus();
}

fn inside(r: (i32, i32, i32, i32), p: POINT) -> bool { p.x >= r.0 && p.x < r.2 && p.y >= r.1 && p.y < r.3 }

/// Hover preview: close 300ms after the pointer left both the Butterfly and the panel.
/// Clicked (pinned) panel: close once another window takes the foreground. tao only reports
/// Focused(false) after it saw Focused(true), which the first native activation of this
/// WebView can miss, so blur is also checked against the real foreground window.
fn watch_panel(app: &AppHandle) {
    let mut away: Option<Instant> = None;
    // (open generation, foreground when pinning was first seen, panel has been foreground since)
    let mut pinned: Option<(usize, HWND, bool)> = None;
    while WANTED.load(Ordering::Acquire) {
        std::thread::sleep(Duration::from_millis(100));
        let Some(panel) = app.get_webview_window(LABEL).and_then(|w| w.hwnd().ok()).map(|h| h.0 as HWND) else { break };
        if !PANEL.load(Ordering::Acquire) {
            away = None;
            let generation = OPENED.load(Ordering::Acquire);
            let fg = unsafe { GetForegroundWindow() };
            if fg.is_null() { continue; }
            // Owned dialogs (e.g. a file picker) still count as the panel.
            let ours = fg == panel || unsafe { GetAncestor(fg, GA_ROOTOWNER) } == panel;
            match &mut pinned {
                Some((g, first, seen)) if *g == generation => {
                    if ours { *seen = true; continue; }
                    // Never activated (foreground denied): only a change of foreground is a blur.
                    if !*seen && fg == *first { continue; }
                    if OPENED.load(Ordering::Acquire) == generation && !PANEL.load(Ordering::Acquire) { blur(app); }
                    break;
                }
                _ => pinned = Some((generation, fg, ours)),
            }
            continue;
        }
        pinned = None;
        let Some(anchor) = *ANCHOR.lock().unwrap() else { break };
        let mut p = POINT { x: 0, y: 0 };
        if unsafe { GetCursorPos(&mut p) } == 0 { continue; }
        let c = anchor.chip;
        let over_panel = {
            let mut r = RECT { left: 0, top: 0, right: 0, bottom: 0 };
            (unsafe { IsWindowVisible(panel) }) != 0 && (unsafe { GetWindowRect(panel, &mut r) }) != 0 && inside((r.left, r.top, r.right, r.bottom), p)
        };
        // A held button may be a 星海 app dragged out of the panel: keep it until release.
        if inside((c.left, c.top, c.right, c.bottom), p) || over_panel || held(VK_LBUTTON) { away = None; continue; }
        if away.get_or_insert_with(Instant::now).elapsed() >= Duration::from_millis(300) {
            if PANEL.load(Ordering::Acquire) { hide(app); }
            break;
        }
    }
}

/// Another window took the foreground. With the left button still down that is probably the
/// source of a drag headed for this panel: keep it until release, then close unless the drop
/// (which re-activates the panel) landed on it.
fn blur(app: &AppHandle) {
    let down = || unsafe { GetAsyncKeyState(VK_LBUTTON as i32) } as u16 & 0x8000 != 0;
    if !down() { return hide(app); }
    let (app, generation) = (app.clone(), OPENED.load(Ordering::Acquire));
    std::thread::spawn(move || {
        let start = Instant::now();
        while down() && start.elapsed() < Duration::from_secs(60) { std::thread::sleep(Duration::from_millis(30)); }
        let panel = app.get_webview_window(LABEL).and_then(|w| w.hwnd().ok()).map(|h| h.0 as HWND);
        let over = panel.is_some_and(|panel| {
            let (mut r, mut p) = (RECT { left: 0, top: 0, right: 0, bottom: 0 }, POINT { x: 0, y: 0 });
            (unsafe { GetWindowRect(panel, &mut r) != 0 && GetCursorPos(&mut p) != 0 }) && inside((r.left, r.top, r.right, r.bottom), p)
        });
        if over { std::thread::sleep(Duration::from_millis(500)); }
        let fg = unsafe { GetForegroundWindow() };
        let ours = panel.is_some_and(|panel| fg == panel || unsafe { GetAncestor(fg, GA_ROOTOWNER) } == panel);
        if OPENED.load(Ordering::Acquire) == generation && !PANEL.load(Ordering::Acquire) && !ours { hide(&app); }
    });
}

/// Close the card without acknowledging updates it has not rendered yet.
pub(super) fn hide(app: &AppHandle) {
    PANEL.store(false, Ordering::Release);
    WANTED.store(false, Ordering::Release);
    OPENED.fetch_add(1, Ordering::AcqRel);
    SHOWN.lock().unwrap().take();
    // Shown natively (see focus()), so hide natively; tao's VISIBLE flag is never relied on.
    if let Some(Ok(hwnd)) = app.get_webview_window(LABEL).map(|window| window.hwnd()) {
        unsafe { ShowWindow(hwnd.0 as _, SW_HIDE); }
    }
    let _ = app.emit_to(LABEL, "taskbar-bubble:hidden", ());
    super::windows::wake();
}

/// Close the card, optionally marking the supplied thread as viewed.
pub(super) fn dismiss(app: &AppHandle, thread: Option<String>) {
    if let Some(thread) = thread {
        if let Ok(mut store) = app.state::<crate::AppState>().store.lock() { super::mark_seen(&mut store, &thread, None); }
    }
    hide(app);
}

/// Size to the rendered cards and sit just above (or below) the taskbar, centred on the chip.
pub(super) fn frame(anchor: Anchor, css_width: f64, css_height: f64) -> (i32, i32, i32, i32) {
    let a = anchor;
    let margin = (8.0 * a.scale).round() as i32;
    let width = ((css_width * a.scale).round() as i32).min((a.work.right - a.work.left - 2 * margin).max(1));
    let height = ((css_height.max(1.0) * a.scale).round() as i32).min((a.work.bottom - a.work.top - margin).max(1));
    let center = (a.chip.left + a.chip.right) / 2;
    let x = (center - width / 2).min(a.work.right - margin - width).max(a.work.left + margin);
    // The window's own transparent margin is the visual gap to the taskbar edge.
    let below = a.chip.top + a.chip.bottom < a.work.top + a.work.bottom;
    let y = if below { a.work.top } else { a.work.bottom - height };
    (x, y, width, height)
}

pub(super) fn place(app: &AppHandle, css_height: f64) -> bool {
    // Clearing cards on hide fires ResizeObserver again. Do not move/resize the hidden surface.
    if !WANTED.load(Ordering::Acquire) { return false; }
    let Some(anchor) = *ANCHOR.lock().unwrap() else { return false };
    let Some(window) = app.get_webview_window(LABEL) else { return false };
    let Ok(hwnd) = window.hwnd() else { return false };
    let hwnd = hwnd.0 as windows_sys::Win32::Foundation::HWND;
    let (x, y, width, height) = frame(anchor, WIDTH, css_height);
    // Not tauri's set_size/set_position: tao re-shows with activation on each resize.
    unsafe { SetWindowPos(hwnd, std::ptr::null_mut(), x, y, width, height, SWP_NOZORDER | SWP_NOACTIVATE); }
    let wanted = WANTED.load(Ordering::Acquire);
    if wanted && PANEL.load(Ordering::Acquire) {
        // Hover must not steal focus from the app the user is working in.
        if unsafe { IsWindowVisible(hwnd) } == 0 { unsafe { ShowWindow(hwnd, SW_SHOWNOACTIVATE); } }
        // The pointer may have left (watch_panel hid it) between the check above and the show.
        if !WANTED.load(Ordering::Acquire) { unsafe { ShowWindow(hwnd, SW_HIDE); } return false; }
        return true;
    }
    if wanted && (unsafe { IsWindowVisible(hwnd) } == 0 || !window.is_focused().unwrap_or(true)) { focus(&window); }
    if WANTED.load(Ordering::Acquire) && unsafe { IsWindowVisible(hwnd) } != 0 {
        // Acknowledge only the revision rendered by the page, not a newer store update.
        let shown = SHOWN.lock().unwrap().clone();
        if let Some((thread, stamp)) = shown {
            if let Ok(mut store) = app.state::<crate::AppState>().store.lock() { super::mark_seen(&mut store, &thread, Some(stamp)); }
        }
        super::windows::wake();
    }
    wanted
}

/// Bubble "open session" / Butterfly double-click: hand over to the full Nova window.
pub(super) fn open_thread(app: &AppHandle, thread: Option<String>) {
    hide(app);
    crate::sys_notify::focus_main_window(app);
    if let Some(thread) = thread {
        if let Ok(mut store) = app.state::<crate::AppState>().store.lock() { super::mark_seen(&mut store, &thread, None); }
        super::windows::wake();
        let _ = app.emit(crate::acp::EV_NOTIFY_OPEN, json!({ "threadId": thread }));
    }
}

/// Quick reply typed on a card. Goes through the same main-window entry as remote control, so
/// Slash commands expand like Composer input; a busy thread queues it behind its turn.
pub(super) fn reply(app: &AppHandle, thread: Option<&str>, text: &str) -> Result<(), String> {
    let text = text.trim();
    if text.is_empty() { return Err("回复内容不能为空".into()); }
    let state = app.state::<crate::AppState>();
    let busy = if let Some(thread) = thread {
        let store = state.store.lock().map_err(|_| "会话存储不可用")?;
        let t = store.get(thread).ok_or("会话不存在")?;
        if t.subagent { return Err("子 Agent 会话不能直接回复".into()); }
        crate::is_running(&state, t)
    } else { false };
    let event = if busy { crate::remote::EV_PROMPT_QUEUE_ENQUEUE } else { crate::remote::EV_REMOTE_PROMPT_DISPATCH };
    app.emit(event, json!({ "threadId": thread, "text": text, "images": [] }))
        .map_err(|error| format!("发送回复失败：{error}"))
}

/// The current turn's latest reply, or its error, in full.
fn latest_reply(items: &[crate::threads::Item]) -> (Kind, String) {
    use crate::threads::Item;
    items.iter().rev()
        .take_while(|item| !matches!(item, Item::User { .. }))
        .find_map(|item| match item {
            Item::Assistant { text, .. } if !text.trim().is_empty() => Some((Kind::Done, text.clone())),
            Item::System { text, level, .. } if level == "error" => Some((Kind::Error, text.clone())),
            _ => None,
        })
        .unwrap_or((Kind::Done, String::new()))
}

fn name(kind: Kind) -> &'static str {
    match kind {
        Kind::Idle => "idle",
        Kind::Running => "running",
        Kind::Waiting => "waiting",
        Kind::Done => "done",
        Kind::Error => "error",
    }
}

const WAND: &str = "drag-wand";
/// Logical size of the magic-wand window centred on the pointer while a 星海 app is dragged out.
const WAND_SIZE: f64 = 280.0;
static WAND_ACTIVE: AtomicBool = AtomicBool::new(false);
/// Bumped per drag: a lingering release burst never hides the next drag's wand.
static WAND_DRAG: AtomicUsize = AtomicUsize::new(0);

fn held(key: u16) -> bool { (unsafe { GetAsyncKeyState(key as i32) }) as u16 & 0x8000 != 0 }

/// Click-through, never focused, excluded from every capture (so never in the drop screenshot).
fn wand_window(app: &AppHandle) -> Option<WebviewWindow> {
    if let Some(window) = app.get_webview_window(WAND) { return Some(window); }
    // Empty title: xcap only skips untitled tool windows.
    let window = tauri::WebviewWindowBuilder::new(app, WAND, tauri::WebviewUrl::App("drag-wand.html".into()))
        .title("").decorations(false).resizable(false).shadow(false)
        .transparent(true).background_color(tauri::window::Color(0, 0, 0, 0))
        .always_on_top(true).skip_taskbar(true).visible(false).focused(false).focusable(false)
        .content_protected(true).inner_size(WAND_SIZE, WAND_SIZE)
        .build().map_err(|error| eprintln!("[taskbar-status] cannot create drag wand: {error}")).ok()?;
    window.set_ignore_cursor_events(true).ok()?;
    Some(window)
}

/// A 星海 app left the panel: a small magic wand rides the pointer (the window moves, the page only
/// animates), scattering sparkles opposite to the motion. Release bursts; a right click cancels the
/// drag and the panel is told so its release launches nothing.
fn drag_start(app: &AppHandle) {
    if WAND_ACTIVE.swap(true, Ordering::AcqRel) { return; }
    let drag = WAND_DRAG.fetch_add(1, Ordering::AcqRel) + 1;
    let app = app.clone();
    std::thread::spawn(move || {
        let Some((window, hwnd)) = wand_window(&app).and_then(|w| { let h = w.hwnd().ok()?.0 as HWND; Some((w, h)) }) else {
            WAND_ACTIVE.store(false, Ordering::Release);
            return;
        };
        let size = (WAND_SIZE * ANCHOR.lock().unwrap().map_or(1.0, |anchor| anchor.scale)).round() as i32;
        let _ = window.eval("window.wand && wand.reset()");
        let excluded = unsafe {
            // tao drops the added TOOLWINDOW when it recomputes styles; keep it off Alt+Tab.
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, GetWindowLongPtrW(hwnd, GWL_EXSTYLE) | WS_EX_TOOLWINDOW as isize);
            let mut affinity = 0;
            GetWindowDisplayAffinity(hwnd, &mut affinity) != 0 && affinity == WDA_EXCLUDEFROMCAPTURE
        };
        let (mut p, mut shown) = (POINT { x: 0, y: 0 }, false);
        let mut previous: Option<POINT> = None;
        let cancelled = loop {
            if !held(VK_LBUTTON) { break false; }
            if held(VK_RBUTTON) { break true; }
            if unsafe { GetCursorPos(&mut p) } != 0 && previous.map_or(true, |q| q.x != p.x || q.y != p.y) {
                previous = Some(p);
                unsafe { SetWindowPos(hwnd, HWND_TOPMOST, p.x - size / 2, p.y - size / 2, size, size, SWP_NOACTIVATE | SWP_SHOWWINDOW); }
                // Also through tao once: a Win32-only show leaves WebView2 thinking it is hidden, animations frozen.
                if !shown { shown = true; let _ = window.show(); }
                // The meteor tail is drawn relative to the newest point; send it with every window move,
                // so the tail head never lags behind where the window already is.
                let _ = window.eval(format!("window.wand && wand.at({},{})", p.x, p.y));
            }
            std::thread::sleep(Duration::from_millis(8));
        };
        WAND_ACTIVE.store(false, Ordering::Release);
        let linger = if cancelled {
            let _ = app.emit_to(LABEL, "taskbar-bubble:drag-cancel", ());
            let _ = window.eval("window.wand && wand.cancel()");
            // Long enough for the small firework at the tip to burn out.
            950
        } else {
            let _ = window.eval("window.wand && wand.release()");
            // Without capture exclusion the wand must be gone before the drop screenshot (150ms).
            if excluded { 700 } else { 0 }
        };
        std::thread::sleep(Duration::from_millis(linger));
        if WAND_DRAG.load(Ordering::Acquire) == drag {
            let _ = window.hide();
            unsafe { ShowWindow(hwnd, SW_HIDE); }
        }
    });
}

/// The window under a screen point (or its monitor, e.g. desktop/taskbar) as a prompt image.
/// The caller hides the panel first, so it is never in the picture.
fn capture_at(p: POINT) -> Result<Value, String> {
    use base64::Engine;
    use xcap::image::{codecs::jpeg::JpegEncoder, imageops, DynamicImage, ExtendedColorType};
    let hwnd = unsafe { GetAncestor(WindowFromPoint(p), GA_ROOT) };
    let mut image = xcap::Window::all().ok()
        .and_then(|all| all.into_iter().find(|w| w.id().ok() == Some(hwnd as usize as u32)))
        .and_then(|w| w.capture_image().ok())
        .or_else(|| xcap::Monitor::from_point(p.x, p.y).ok()?.capture_image().ok())
        .ok_or("无法截取松开位置的窗口")?;
    let long = image.width().max(image.height());
    if long > 2048 {
        let k = 2048.0 / long as f32;
        image = imageops::resize(&image, (image.width() as f32 * k) as u32, (image.height() as f32 * k) as u32, imageops::FilterType::Triangle);
    }
    let rgb = DynamicImage::ImageRgba8(image).into_rgb8();
    let mut bytes = Vec::new();
    JpegEncoder::new_with_quality(&mut bytes, 85)
        .encode(rgb.as_raw(), rgb.width(), rgb.height(), ExtendedColorType::Rgb8)
        .map_err(|e| e.to_string())?;
    Ok(json!({ "name": "拖放位置截图.jpg", "mimeType": "image/jpeg", "size": bytes.len(),
        "data": base64::engine::general_purpose::STANDARD.encode(&bytes) }))
}

/// Panel quick actions. Anything that needs the full window hands over to it and closes the panel.
pub(super) fn quick(app: &AppHandle, action: &str) -> Result<Value, String> {
    // A 星海 app dropped outside the panel: capture where it landed, then open the app with it.
    if let Some(app_id) = action.strip_prefix("miniapp-drop:") {
        let id = crate::miniapp_window::parse_id(app_id)?;
        let mut p = POINT { x: 0, y: 0 };
        unsafe { GetCursorPos(&mut p) };
        hide(app);
        // ponytail: one fixed wait for DWM to compose the hidden panel away; transitions are off,
        // so a couple of frames suffice. Poll the screen if a slow machine still shows it.
        std::thread::sleep(Duration::from_millis(150));
        let context = capture_at(p).map(|image| json!({ "image": image })).unwrap_or_else(|error| json!({ "imageError": error }));
        crate::miniapp_window::launch(app, id, app_id, context)?;
        return Ok(Value::Null);
    }
    let request = match action {
        "drag-start" => { drag_start(app); return Ok(Value::Null); }
        "jianpu" => return crate::jianpu::toggle(),
        "knowledge" => json!({ "view": "knowledge" }),
        "miniapps" => json!({ "view": "miniapps", "create": true }),
        _ => {
            let id = action.strip_prefix("miniapp:")
                .ok_or_else(|| format!("未知快捷操作：{action}"))?;
            let uuid = crate::miniapp_window::parse_id(id)?;
            hide(app);
            // An empty launch context still marks a fresh start, so autoRun fires on every launch.
            crate::miniapp_window::launch(app, uuid, id, json!({}))?;
            return Ok(Value::Null);
        }
    };
    hide(app);
    crate::sys_notify::focus_main_window(app);
    app.emit("nova:open-view", request).map_err(|e| e.to_string())?;
    Ok(Value::Null)
}

/// The session tab remains available alongside the default assistant and quick actions.
pub(super) fn payload(app: &AppHandle) -> Value {
    let shown = SHOWN.lock().unwrap().clone();
    let Some(status) = super::snapshot(app, shown.as_ref().map(|(thread, _)| thread.as_str())) else { return Value::Null };
    let state = app.state::<crate::AppState>();
    let mut permissions: Vec<Value> = state.remote_permissions.lock().map(|p| p.values().cloned().collect()).unwrap_or_default();
    let cards = {
        let Ok(store) = state.store.lock() else { return Value::Null };
        for permission in &mut permissions {
            let title = permission["threadId"].as_str().and_then(|id| store.get(id)).map(|t| t.title.clone());
            permission["threadTitle"] = json!(title);
        }
        let mut cards = Vec::new();
        let settled = matches!(status.kind, Kind::Done | Kind::Error);
        if WANTED.load(Ordering::Acquire) {
            let thread = status.thread.as_deref().and_then(|id| store.get(id));
            let (title, body) = thread.map(|t| (t.title.clone(), latest_reply(&t.items).1)).unwrap_or_default();
            *SHOWN.lock().unwrap() = thread.filter(|_| settled).map(|t| (t.id.clone(), t.updated_at));
            // Keyed by thread: a half-typed reply never carries over to another thread's card.
            let key = format!("live:{}", status.thread.as_deref().unwrap_or(""));
            cards.push(json!({ "key": key, "live": true, "kind": name(status.kind), "text": if status.kind == Kind::Idle { "新会话 · 临时目录" } else { &status.text }, "trailing": status.trailing,
                "threadId": status.thread, "title": title, "body": body }));
        }
        cards
    };
    permissions.sort_by(|a, b| a["requestKey"].as_str().cmp(&b["requestKey"].as_str()));
    // Cards follow the taskbar they pop out of, like Windows' own taskbar flyouts.
    // `tab`: left click/hover open the assistant, right click the quick actions; `panel` only controls focus behavior.
    let capture_error = if cards.is_empty() { None } else { CAPTURE_ERROR.lock().unwrap().take() };
    let tab = if QUICK.load(Ordering::Acquire) { "quick" } else { "assistant" };
    json!({ "cards": cards, "permissions": permissions, "light": super::windows::light_theme(), "panel": PANEL.load(Ordering::Acquire), "tab": tab, "captureError": capture_error,
        "jianpu": crate::jianpu::status() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bubble_hugs_the_taskbar_edge_and_stays_on_the_chip_monitor() {
        let work = Rect { left: -1920, top: 0, right: 0, bottom: 1032 };
        let chip = Rect { left: -60, top: 1040, right: -10, bottom: 1068 };
        let (x, y, w, h) = frame(Anchor { chip, work, scale: 1.0 }, WIDTH, 300.0);
        assert_eq!((w, h, y + h), (560, 300, 1032), "bottom taskbar: above the work area edge");
        assert_eq!(x + w, -8, "clamped inside the right edge");
        let top = Anchor { chip: Rect { left: -1000, top: 6, right: -800, bottom: 34 }, work: Rect { top: 40, ..work }, scale: 1.5 };
        let (x, y, w, h) = frame(top, WIDTH, 5000.0);
        assert_eq!((y, w), (40, 840), "top taskbar: below it, scaled");
        assert!(h <= 1032 - 40 && x == -900 - w / 2);
        let narrow = Anchor { work: Rect { left: 0, top: 0, right: 480, bottom: 400 }, chip: Rect { left: 420, top: 400, right: 470, bottom: 430 }, scale: 1.0 };
        let (x, y, w, h) = frame(narrow, WIDTH, 640.0);
        assert_eq!((x, w, h), (8, 464, 392));
        assert!(y >= narrow.work.top && y + h <= narrow.work.bottom);
    }

}
