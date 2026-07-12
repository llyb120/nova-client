//! 跨平台系统通知：Windows 用 WinRT Toast（可点击回调），macOS 用 osascript。

use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager, UserAttentionType};

/// 聚焦主窗口（显示、取消最小化、请求注意、尽量设为前台）。
pub fn focus_main_window(app: &AppHandle) {
    if let Some(w) = app
        .get_webview_window("main")
        .or_else(|| app.webview_windows().into_values().next())
    {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.request_user_attention(Some(UserAttentionType::Critical));
        let _ = w.set_focus();
    }
}

fn main_window_focused(app: &AppHandle) -> bool {
    app.get_webview_window("main")
        .and_then(|w| w.is_focused().ok())
        .unwrap_or(false)
}

/// 弹出系统通知。`skip_if_focused` 为 true 时窗口已聚焦则不打扰。
/// `on_activate` 在用户点击通知时调用（macOS 无法可靠回调，仅 Windows 生效；
/// 漫游等场景应在调用前自行 focus）。
pub fn show(
    app: &AppHandle,
    title: &str,
    body: &str,
    skip_if_focused: bool,
    on_activate: Option<Box<dyn FnOnce() + Send + 'static>>,
) {
    if skip_if_focused && main_window_focused(app) {
        return;
    }

    #[cfg(windows)]
    {
        use tauri_winrt_notification::Toast;
        let mut toast = Toast::new(Toast::POWERSHELL_APP_ID)
            .title(title)
            .text1(body);
        if let Some(cb) = on_activate {
            // on_activated 要求 FnMut（可多次触发），而 cb 是 FnOnce。
            // 用 Option + take() 让闭包第一次调用时消费 cb，后续触发为 no-op。
            let mut cb = Some(cb);
            toast = toast.on_activated(move |_| {
                if let Some(f) = cb.take() {
                    f();
                }
                Ok(())
            });
        }
        let _ = toast.show();
    }

    #[cfg(target_os = "macos")]
    {
        let _ = on_activate; // macOS 通知点击无法可靠带自定义回调
        let title_esc = escape_applescript(title);
        let body_esc = escape_applescript(body);
        let script = format!(
            "display notification \"{body_esc}\" with title \"{title_esc}\""
        );
        let _ = std::process::Command::new("osascript")
            .args(["-e", &script])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = (app, title, body, on_activate);
    }
}

/// 任务结束通知：点击（Windows）跳转到会话。
pub fn notify_thread_done(
    app: &AppHandle,
    thread_id: &str,
    title: &str,
    body: &str,
    event: &str,
) {
    let app2 = app.clone();
    let tid = thread_id.to_string();
    let event = event.to_string();
    show(
        app,
        title,
        body,
        true,
        Some(Box::new(move || {
            focus_main_window(&app2);
            let _ = app2.emit(&event, serde_json::json!({ "threadId": tid }));
        })),
    );
}

/// 员工上奏通知：点击打开御书房批阅。
pub fn notify_decision(app: &AppHandle, emp_name: &str, question: &str, event: &str) {
    let app2 = app.clone();
    let event = event.to_string();
    show(
        app,
        &format!("「{emp_name}」有本上奏"),
        question,
        true,
        Some(Box::new(move || {
            focus_main_window(&app2);
            let _ = app2.emit(&event, Value::Object(Default::default()));
        })),
    );
}

/// 漫游请求：无论前后台都唤起窗口；未聚焦时再补系统通知。
pub fn notify_roam_request(app: &AppHandle, from_name: &str, folder_name: &str) {
    let was_focused = main_window_focused(app);
    focus_main_window(app);
    if was_focused {
        return;
    }
    let app2 = app.clone();
    show(
        app,
        "收到漫游请求",
        &format!("{from_name} 想漫游你的「{folder_name}」，点击处理"),
        false,
        Some(Box::new(move || {
            focus_main_window(&app2);
        })),
    );
}

#[cfg(target_os = "macos")]
fn escape_applescript(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}
