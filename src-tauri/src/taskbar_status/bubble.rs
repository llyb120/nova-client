//! A click on the taskbar Butterfly opens the live card (full result, progress and pending
//! confirmations) in a reused transparent webview. Finishing a turn does not open it.

use super::{Kind, Rect};
use serde_json::{json, Value};
use std::{
    sync::{atomic::{AtomicBool, Ordering}, Mutex},
    time::{Duration, Instant},
};
use tauri::{AppHandle, Emitter, Manager, WebviewWindow};
use windows_sys::Win32::{
    Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_TRANSITIONS_FORCEDISABLED},
    UI::WindowsAndMessaging::{SetWindowPos, SWP_NOACTIVATE, SWP_NOZORDER},
};

const LABEL: &str = "taskbar-bubble";
/// CSS width including the transparent margin that holds the card's shadow.
const WIDTH: f64 = 412.0;

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
static HIDDEN_AT: Mutex<Option<Instant>> = Mutex::new(None);
static BUILD: Mutex<()> = Mutex::new(());
/// Finished result on the live card; it counts as viewed once that card closes.
static SHOWN: Mutex<Option<String>> = Mutex::new(None);

/// Open, or the live card was closed by this very click (blur fires on mouse-down, before the chip sees it).
pub(super) fn is_open() -> bool {
    WANTED.load(Ordering::Acquire)
        || HIDDEN_AT.lock().unwrap().is_some_and(|at| at.elapsed() < Duration::from_millis(400))
}

/// Built once the Butterfly shows, so a click does not wait for a cold webview.
pub(super) fn window(app: &AppHandle) -> Option<WebviewWindow> {
    let _building = BUILD.lock().unwrap();
    if let Some(window) = app.get_webview_window(LABEL) { return Some(window); }
    let window = tauri::WebviewWindowBuilder::new(app, LABEL, tauri::WebviewUrl::App("index.html?bubble=1".into()))
        .title("Nova").decorations(false).resizable(false).shadow(false)
        .transparent(true).background_color(tauri::window::Color(0, 0, 0, 0))
        .always_on_top(true).skip_taskbar(true).visible(false).focused(false)
        .inner_size(WIDTH, 200.0)
        .build()
        .map_err(|error| eprintln!("[taskbar-status] cannot create bubble: {error}"))
        .ok()?;
    // The card already animates in CSS. A native hide transition can retain its old WebView
    // surface after blur/DOM cleanup and briefly flash that surface at the taskbar edge.
    if let Ok(hwnd) = window.hwnd() {
        let disabled = 1i32;
        let result = unsafe { DwmSetWindowAttribute(hwnd.0 as _, DWMWA_TRANSITIONS_FORCEDISABLED as u32,
            &disabled as *const _ as _, size_of::<i32>() as u32) };
        if result < 0 { eprintln!("[taskbar-status] cannot disable bubble transitions: {result:#x}"); }
    }
    let handle = app.clone();
    window.on_window_event(move |event| match event {
        tauri::WindowEvent::Focused(false) => hide(&handle),
        tauri::WindowEvent::CloseRequested { api, .. } => {
            api.prevent_close();
            hide(&handle);
        }
        _ => {}
    });
    Some(window)
}

pub(super) fn open(app: &AppHandle, anchor: Anchor) {
    *ANCHOR.lock().unwrap() = Some(anchor);
    WANTED.store(true, Ordering::Release);
    // A freshly built page places itself on mount; a warm one reloads on this event.
    if window(app).is_some() { let _ = app.emit_to(LABEL, "taskbar-bubble:open", ()); }
}

/// Close the card; whatever was on screen counts as viewed.
pub(super) fn hide(app: &AppHandle) {
    if WANTED.swap(false, Ordering::AcqRel) { *HIDDEN_AT.lock().unwrap() = Some(Instant::now()); }
    // Release SHOWN before mark_seen takes the store: payload() locks them the other way round.
    let shown = SHOWN.lock().unwrap().take();
    if let Some(thread) = shown { super::mark_seen(app, &thread); }
    if let Some(window) = app.get_webview_window(LABEL) {
        if window.is_visible().unwrap_or(false) { let _ = window.hide(); }
    }
    let _ = app.emit_to(LABEL, "taskbar-bubble:hidden", ());
    super::windows::wake();
}

/// Close the card, optionally marking the supplied thread as viewed.
pub(super) fn dismiss(app: &AppHandle, thread: Option<String>) {
    if let Some(thread) = thread { super::mark_seen(app, &thread); }
    hide(app);
}

/// Size to the rendered cards and sit just above (or below) the taskbar, centred on the chip.
fn frame(anchor: Anchor, css_height: f64) -> (i32, i32, i32, i32) {
    let a = anchor;
    let margin = (8.0 * a.scale).round() as i32;
    let width = (WIDTH * a.scale).round() as i32;
    let height = ((css_height.max(1.0) * a.scale).round() as i32).min((a.work.bottom - a.work.top - margin).max(1));
    let center = (a.chip.left + a.chip.right) / 2;
    let x = (center - width / 2).min(a.work.right - margin - width).max(a.work.left + margin);
    // The window's own transparent margin is the visual gap to the taskbar edge.
    let below = a.chip.top + a.chip.bottom < a.work.top + a.work.bottom;
    let y = if below { a.work.top } else { a.work.bottom - height };
    (x, y, width, height)
}

pub(super) fn place(app: &AppHandle, css_height: f64) -> bool {
    // Clearing cards on hide fires ResizeObserver again. Do not move/resize the hidden
    // surface; is_open() is deliberately unsuitable here because it includes a click grace period.
    if !WANTED.load(Ordering::Acquire) { return false; }
    let Some(anchor) = *ANCHOR.lock().unwrap() else { return false };
    let Some(window) = app.get_webview_window(LABEL) else { return false };
    let Ok(hwnd) = window.hwnd() else { return false };
    let hwnd = hwnd.0 as windows_sys::Win32::Foundation::HWND;
    let (x, y, width, height) = frame(anchor, css_height);
    // Not tauri's set_size/set_position: tao re-shows with activation on each resize.
    unsafe { SetWindowPos(hwnd, std::ptr::null_mut(), x, y, width, height, SWP_NOZORDER | SWP_NOACTIVATE); }
    let wanted = WANTED.load(Ordering::Acquire);
    if wanted && !window.is_visible().unwrap_or(true) {
        let _ = window.show();
        let _ = window.set_focus();
    }
    wanted
}

/// Bubble "open session" / Butterfly double-click: hand over to the full Nova window.
pub(super) fn open_thread(app: &AppHandle, thread: Option<String>) {
    hide(app);
    crate::sys_notify::focus_main_window(app);
    if let Some(thread) = thread {
        super::mark_seen(app, &thread);
        super::windows::wake();
        let _ = app.emit(crate::acp::EV_NOTIFY_OPEN, json!({ "threadId": thread }));
    }
}

/// Quick reply typed on a card. Goes through the same main-window entry as remote control, so
/// slash commands and workflows expand like Composer input; a busy thread queues it behind its turn.
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

/// The live card is available only after a click on the Butterfly.
pub(super) fn payload(app: &AppHandle) -> Value {
    let Some(status) = super::snapshot(app) else { return Value::Null };
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
            let (title, body) = status.thread.as_deref().and_then(|id| store.get(id))
                .map(|t| (t.title.clone(), latest_reply(&t.items).1)).unwrap_or_default();
            if settled { *SHOWN.lock().unwrap() = status.thread.clone(); }
            // Keyed by thread: a half-typed reply never carries over to another thread's card.
            let key = format!("live:{}", status.thread.as_deref().unwrap_or(""));
            cards.push(json!({ "key": key, "live": true, "kind": name(status.kind), "text": if status.kind == Kind::Idle { "新会话 · 临时目录" } else { &status.text }, "trailing": status.trailing,
                "threadId": status.thread, "title": title, "body": body }));
        }
        cards
    };
    permissions.sort_by(|a, b| a["requestKey"].as_str().cmp(&b["requestKey"].as_str()));
    // Cards follow the taskbar they pop out of, like Windows' own taskbar flyouts.
    json!({ "cards": cards, "permissions": permissions, "light": super::windows::light_theme() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bubble_hugs_the_taskbar_edge_and_stays_on_the_chip_monitor() {
        let work = Rect { left: -1920, top: 0, right: 0, bottom: 1032 };
        let chip = Rect { left: -60, top: 1040, right: -10, bottom: 1068 };
        let (x, y, w, h) = frame(Anchor { chip, work, scale: 1.0 }, 300.0);
        assert_eq!((w, h, y + h), (412, 300, 1032), "bottom taskbar: above the work area edge");
        assert_eq!(x + w, -8, "clamped inside the right edge");
        let top = Anchor { chip: Rect { left: -1000, top: 6, right: -800, bottom: 34 }, work: Rect { top: 40, ..work }, scale: 1.5 };
        let (x, y, w, h) = frame(top, 5000.0);
        assert_eq!((y, w), (40, 618), "top taskbar: below it, scaled");
        assert!(h <= 1032 - 40 && x == -900 - w / 2);
    }

}
