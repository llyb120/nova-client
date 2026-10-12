//! Single-click detail bubble for the taskbar island: full result, live progress and
//! pending confirmations. A lazily built, reused transparent webview; hides on blur.

use super::{Kind, Rect};
use serde_json::{json, Value};
use std::{
    sync::{atomic::{AtomicBool, Ordering}, Mutex},
    time::{Duration, Instant},
};
use tauri::{AppHandle, Emitter, Manager, WebviewWindow};

const LABEL: &str = "taskbar-bubble";
/// CSS width including the transparent margin that holds the card's shadow.
const WIDTH: f64 = 412.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Anchor {
    /// Island chip, physical pixels.
    pub chip: Rect,
    /// Work area of the chip's monitor, physical pixels.
    pub work: Rect,
    pub scale: f64,
}

static ANCHOR: Mutex<Option<Anchor>> = Mutex::new(None);
static WANTED: AtomicBool = AtomicBool::new(false);
static HIDDEN_AT: Mutex<Option<Instant>> = Mutex::new(None);
static BUILD: Mutex<()> = Mutex::new(());
/// Finished result on display; it counts as viewed once the bubble closes.
static SHOWN: Mutex<Option<String>> = Mutex::new(None);

/// Open, or closed by this very click (blur fires on mouse-down, before the chip sees it).
pub(super) fn is_open() -> bool {
    WANTED.load(Ordering::Acquire)
        || HIDDEN_AT.lock().unwrap().is_some_and(|at| at.elapsed() < Duration::from_millis(400))
}

/// Build the webview ahead of the double-click timeout so the first bubble appears promptly.
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

pub(super) fn hide(app: &AppHandle) {
    if WANTED.swap(false, Ordering::AcqRel) { *HIDDEN_AT.lock().unwrap() = Some(Instant::now()); }
    if let Some(window) = app.get_webview_window(LABEL) {
        if window.is_visible().unwrap_or(false) { let _ = window.hide(); }
    }
    let _ = app.emit_to(LABEL, "taskbar-bubble:hidden", ());
    if let Some(thread) = SHOWN.lock().unwrap().take() {
        super::mark_seen(app, &thread);
        super::windows::wake();
    }
}

/// Size to the rendered card and sit just above (or below) the taskbar, centred on the chip.
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
    let Some(anchor) = *ANCHOR.lock().unwrap() else { return false };
    let Some(window) = app.get_webview_window(LABEL) else { return false };
    let (x, y, width, height) = frame(anchor, css_height);
    let _ = window.set_size(tauri::PhysicalSize::new(width as u32, height as u32));
    let _ = window.set_position(tauri::PhysicalPosition::new(x, y));
    let wanted = WANTED.load(Ordering::Acquire);
    if wanted && !window.is_visible().unwrap_or(true) {
        let _ = window.show();
        let _ = window.set_focus();
    }
    wanted
}

/// Bubble double-click / "open session": hand over to the full Nova window.
pub(super) fn open_thread(app: &AppHandle, thread: Option<String>) {
    hide(app);
    crate::sys_notify::focus_main_window(app);
    if let Some(thread) = thread {
        super::mark_seen(app, &thread);
        super::windows::wake();
        let _ = app.emit(crate::acp::EV_NOTIFY_OPEN, json!({ "threadId": thread }));
    }
}

/// The current turn's latest reply, or its error, in full.
fn latest_reply(items: &[crate::threads::Item]) -> String {
    use crate::threads::Item;
    items.iter().rev()
        .take_while(|item| !matches!(item, Item::User { .. }))
        .find_map(|item| match item {
            Item::Assistant { text, .. } if !text.trim().is_empty() => Some(text.clone()),
            Item::System { text, level, .. } if level == "error" => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

pub(super) fn payload(app: &AppHandle) -> Value {
    let Some(status) = super::snapshot(app) else { return Value::Null };
    let state = app.state::<crate::AppState>();
    let mut permissions: Vec<Value> = state.remote_permissions.lock().map(|p| p.values().cloned().collect()).unwrap_or_default();
    let (title, body) = {
        let Ok(store) = state.store.lock() else { return Value::Null };
        let title_of = |id: &str| store.threads.iter().find(|t| t.id == id).map(|t| t.title.clone());
        for permission in &mut permissions {
            let title = permission["threadId"].as_str().and_then(title_of);
            permission["threadTitle"] = json!(title);
        }
        status.thread.as_deref()
            .and_then(|id| store.threads.iter().find(|t| t.id == id))
            .map(|t| (t.title.clone(), latest_reply(&t.items)))
            .unwrap_or_default()
    };
    permissions.sort_by(|a, b| a["requestKey"].as_str().cmp(&b["requestKey"].as_str()));
    let kind = match status.kind {
        Kind::Idle => "idle",
        Kind::Running => "running",
        Kind::Waiting => "waiting",
        Kind::Done => "done",
        Kind::Error => "error",
    };
    if WANTED.load(Ordering::Acquire) && matches!(status.kind, Kind::Done | Kind::Error) {
        *SHOWN.lock().unwrap() = status.thread.clone();
    }
    json!({
        "kind": kind,
        "text": status.text,
        "trailing": status.trailing,
        "threadId": status.thread,
        "title": title,
        "body": body,
        "permissions": permissions,
    })
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
