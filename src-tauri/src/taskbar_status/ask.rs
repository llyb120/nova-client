//! 待确认小气泡：有事项要用户确认时，直接在蝴蝶上方弹出一张只含这件事的小卡片（不是完整的蝴蝶面板）。
//! 不抢焦点；点空白、切走窗口都不收起，作答或超时自动处理后才消失。

use super::bubble::{frame, Anchor};
use serde_json::{json, Value};
use std::{
    sync::{atomic::{AtomicBool, Ordering}, Mutex},
    time::{Duration, Instant},
};
use tauri::{AppHandle, Emitter, Manager, WebviewWindow};
use windows_sys::Win32::UI::WindowsAndMessaging::{IsWindowVisible, SetWindowPos, ShowWindow, HWND_TOPMOST, SWP_NOACTIVATE, SW_HIDE, SW_SHOWNOACTIVATE};

const LABEL: &str = "taskbar-ask";
/// CSS 宽度，含放阴影的透明边。
const WIDTH: f64 = 400.0;
static ANCHOR: Mutex<Option<Anchor>> = Mutex::new(None);
static SHOWN: AtomicBool = AtomicBool::new(false);
static WATCHING: AtomicBool = AtomicBool::new(false);
static ALERTED: Mutex<Option<Instant>> = Mutex::new(None);
static BUILD: Mutex<()> = Mutex::new(());

fn window(app: &AppHandle) -> Option<WebviewWindow> {
    let _building = BUILD.lock().unwrap();
    if let Some(window) = app.get_webview_window(LABEL) { return Some(window); }
    tauri::WebviewWindowBuilder::new(app, LABEL, tauri::WebviewUrl::App("index.html?ask=1".into()))
        .title("Nova").decorations(false).resizable(false).shadow(false)
        .transparent(true).background_color(tauri::window::Color(0, 0, 0, 0))
        .always_on_top(true).skip_taskbar(true).visible(false).focused(false)
        .inner_size(WIDTH, 200.0)
        .build()
        .map_err(|error| eprintln!("[taskbar-status] cannot create ask bubble: {error}"))
        .ok()
}

fn permissions(app: &AppHandle) -> Vec<Value> {
    let state = app.state::<crate::AppState>();
    let mut list: Vec<Value> = state.remote_permissions.lock().map(|p| p.values().cloned().collect()).unwrap_or_default();
    if let Ok(store) = state.store.lock() {
        for permission in &mut list {
            let title = permission["threadId"].as_str().and_then(|id| store.get(id)).map(|t| t.title.clone());
            permission["threadTitle"] = json!(title);
        }
    }
    list.sort_by(|a, b| a["requestKey"].as_str().cmp(&b["requestKey"].as_str()));
    list
}

/// 有新的待确认：弹出（或刷新）小气泡。用户正看着 Nova 主窗口时确认就在会话里，除非 `always`。
pub(super) fn alert(app: &AppHandle, always: bool) {
    if !always && app.get_window("main").is_some_and(|w| w.is_focused().unwrap_or(false)) { return; }
    let Some(anchor) = super::windows::chip_anchor() else { return };
    *ANCHOR.lock().unwrap() = Some(anchor);
    *ALERTED.lock().unwrap() = Some(Instant::now());
    SHOWN.store(true, Ordering::Release);
    // 新建的页面挂载后自己加载；已有的页面收到事件刷新。
    if window(app).is_some() { let _ = app.emit_to(LABEL, "taskbar-ask:refresh", ()); }
    watch(app);
}

/// 待确认全部处理完（含超时自动处理）就收起；刚弹出的 2s 内不判，确认事件可能比记录它的监听先到。
fn watch(app: &AppHandle) {
    if WATCHING.swap(true, Ordering::AcqRel) { return; }
    let app = app.clone();
    std::thread::spawn(move || {
        while SHOWN.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_millis(200));
            let fresh = ALERTED.lock().unwrap().is_some_and(|at| at.elapsed() < Duration::from_secs(2));
            if !fresh && permissions(&app).is_empty() && crate::employee::local_helps().is_empty() { hide(&app); }
        }
        WATCHING.store(false, Ordering::Release);
    });
}

pub(super) fn hide(app: &AppHandle) {
    SHOWN.store(false, Ordering::Release);
    if let Some(Ok(hwnd)) = app.get_webview_window(LABEL).map(|w| w.hwnd()) {
        unsafe { ShowWindow(hwnd.0 as _, SW_HIDE); }
    }
}

pub(super) fn payload(app: &AppHandle) -> Value {
    json!({ "permissions": permissions(app), "helps": crate::employee::local_helps(), "light": super::windows::light_theme() })
}

/// 页面报告卡片高度：按蝴蝶位置摆好并不激活地显示。
pub(super) fn place(app: &AppHandle, css_height: f64) -> bool {
    if !SHOWN.load(Ordering::Acquire) { return false; }
    let Some(anchor) = *ANCHOR.lock().unwrap() else { return false };
    let Some(Ok(hwnd)) = app.get_webview_window(LABEL).map(|w| w.hwnd()) else { return false };
    let hwnd = hwnd.0 as windows_sys::Win32::Foundation::HWND;
    let (x, y, width, height) = frame(anchor, WIDTH, css_height);
    unsafe {
        SetWindowPos(hwnd, HWND_TOPMOST, x, y, width, height, SWP_NOACTIVATE);
        if IsWindowVisible(hwnd) == 0 { ShowWindow(hwnd, SW_SHOWNOACTIVATE); }
    }
    true
}
