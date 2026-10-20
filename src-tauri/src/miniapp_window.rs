//! Independent runtime windows; design sessions remain main-window-only.
use crate::{settings::{ModelTarget, Scene}, AppState};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::HashMap, sync::Mutex};
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindow};
use uuid::Uuid;

// ponytail: serialize all runtime window creation to prevent duplicate labels;
// use per-app locks if concurrent cold window creation becomes a bottleneck.
static BUILD: Mutex<()> = Mutex::new(());
/// Published apps that Nova Chrome may mount on matching pages (pushed by the main window).
static TRIGGERS: Mutex<Vec<BrowserTrigger>> = Mutex::new(Vec::new());
/// The latest launch context per app (page click or drop screenshot), consumed once by its runtime window.
static LAUNCHES: Mutex<Option<HashMap<Uuid, Value>>> = Mutex::new(None);

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BrowserTrigger {
    app_id: String,
    name: String,
    #[serde(rename = "match")]
    patterns: Vec<String>,
}

fn validate_triggers(triggers: &[BrowserTrigger]) -> Result<(), String> {
    if triggers.len() > 100 {
        return Err("网页激活应用最多 100 个".into());
    }
    for trigger in triggers {
        parse_id(&trigger.app_id)?;
        if trigger.name.chars().count() > 200 || trigger.patterns.is_empty() || trigger.patterns.len() > 20 {
            return Err("网页激活配置无效".into());
        }
        if !trigger.patterns.iter().all(|p| p.len() <= 2048 && !p.chars().any(char::is_whitespace)
            && ["http://", "https://", "*://"].iter().any(|scheme| p.starts_with(scheme) && p.len() > scheme.len())) {
            return Err("网页激活网址模式无效".into());
        }
    }
    Ok(())
}

pub(crate) fn browser_triggers() -> Value {
    serde_json::to_value(&*TRIGGERS.lock().unwrap_or_else(|e| e.into_inner())).unwrap_or_default()
}

/// A user click on the Chrome page icon: remember the page, then open the app window.
pub(crate) async fn browser_launch(app: &AppHandle, body: &Value) -> Result<(), String> {
    let app_id = body["appId"].as_str().unwrap_or_default().to_string();
    let id = parse_id(&app_id)?;
    if !TRIGGERS.lock().map_err(|_| "网页激活锁不可用")?.iter().any(|t| t.app_id == app_id) {
        return Err("该应用未发布网页激活配置".into());
    }
    let url = body["url"].as_str()
        .filter(|u| u.len() <= 8192 && (u.starts_with("http://") || u.starts_with("https://")))
        .ok_or("网页地址无效")?;
    let title: String = body["title"].as_str().unwrap_or_default().chars().take(1000).collect();
    let context = json!({"url": url, "title": title});
    let handle = app.clone();
    tauri::async_runtime::spawn_blocking(move || launch(&handle, id, &app_id, context)).await.map_err(|e| e.to_string())?
}

/// Open the app with a one-shot launch context. Call off the UI thread, like `open`.
pub(crate) fn launch(app: &AppHandle, id: Uuid, app_id: &str, context: Value) -> Result<(), String> {
    LAUNCHES.lock().map_err(|_| "启动信息锁不可用")?.get_or_insert_with(HashMap::new).insert(id, context);
    open(app, app_id)?;
    // A cold window takes the launch on mount; a warm one wakes up here.
    app.emit("miniapp:launch", ()).map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) fn miniapp_browser_triggers(window: WebviewWindow, triggers: Vec<BrowserTrigger>) -> Result<(), String> {
    if window.label() != "main" {
        return Err("仅主窗口可以更新网页激活配置".into());
    }
    validate_triggers(&triggers)?;
    *TRIGGERS.lock().map_err(|_| "网页激活锁不可用")? = triggers;
    Ok(())
}

#[tauri::command]
pub(crate) fn miniapp_take_launch(window: WebviewWindow) -> Result<Option<Value>, String> {
    let Some(id) = runtime_app_id(window.label())? else { return Ok(None) };
    Ok(LAUNCHES.lock().map_err(|_| "网页激活锁不可用")?.as_mut().and_then(|launches| launches.remove(&id)))
}

pub(crate) fn parse_id(value: &str) -> Result<Uuid, String> {
    let id = Uuid::parse_str(value).map_err(|_| "appId/runId 必须是标准小写 UUID")?;
    if id.to_string() != value {
        return Err("appId/runId 必须是标准小写 UUID".into());
    }
    Ok(id)
}

/// None is the trusted main window, never an unrecognized runtime label.
pub(crate) fn runtime_app_id(label: &str) -> Result<Option<Uuid>, String> {
    if label == "main" {
        return Ok(None);
    }
    label.strip_prefix("miniapp-")
        .ok_or_else(|| "仅 main 或有效观星台窗口可访问".to_string())
        .and_then(parse_id)
        .map(Some)
}

pub(crate) fn authorize_app(label: &str, app_id: &Uuid) -> Result<(), String> {
    if runtime_app_id(label)?.is_some_and(|id| id != *app_id) {
        return Err("观星台窗口不能访问其他 app".into());
    }
    Ok(())
}

/// Capabilities restrict plugins, not application commands. Keep runtime windows
/// out of settings, design, deletion and other main-window-only operations.
pub(crate) fn allows_command(label: &str, command: &str) -> bool {
    if !label.starts_with("miniapp-") {
        return true;
    }
    matches!(runtime_app_id(label), Ok(Some(_))) && matches!(command,
        "miniapp_open" | "miniapp_scene_model" | "miniapp_show_thread"
        | "miniapp_pending_permissions" | "miniapp_command_start"
        | "miniapp_command_status" | "miniapp_command_cancel" | "miniapp_take_launch"
        | "scratch_dir" | "create_thread" | "send_prompt" | "list_threads"
        | "get_thread_view" | "cancel_turn" | "respond_permission"
    )
}

fn window_target(app_id: &str) -> Result<(String, WebviewUrl), String> {
    let id = parse_id(app_id)?;
    Ok((format!("miniapp-{id}"), WebviewUrl::App(format!("index.html?miniapp={id}").into())))
}

/// Internal callers (including the bubble) do not go through an invoke command.
/// Call off the UI thread: WebView creation dispatches back to the event loop.
pub(crate) fn open(app: &AppHandle, app_id: &str) -> Result<(), String> {
    let (label, url) = window_target(app_id)?;
    let _building = BUILD.lock().map_err(|_| "观星台窗口锁不可用")?;
    let window = if let Some(window) = app.get_webview_window(&label) {
        window
    } else {
        tauri::WebviewWindowBuilder::new(app, &label, url)
            .title("观星台")
            .inner_size(900.0, 720.0)
            .min_inner_size(320.0, 240.0)
            .center()
            .visible(false)
            .focused(false)
            .build()
            .map_err(|e| e.to_string())?
    };
    window.unminimize().map_err(|e| e.to_string())?;
    window.show().map_err(|e| e.to_string())?;
    window.set_focus().map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) async fn miniapp_open(app: AppHandle, window: WebviewWindow, app_id: String) -> Result<(), String> {
    authorize_app(window.label(), &parse_id(&app_id)?)?;
    tauri::async_runtime::spawn_blocking(move || open(&app, &app_id))
        .await.map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) fn miniapp_scene_model(window: WebviewWindow, state: State<'_, AppState>) -> Result<ModelTarget, String> {
    runtime_app_id(window.label())?;
    let (kind, model) = state.scene_model(Scene::Lightweight)?;
    Ok(ModelTarget { agent_kind: kind.as_str().into(), model })
}

/// Events alone cannot restore approvals emitted while a runtime window was closed.
/// Read only the requested run's pending requests; never answer questions here.
#[tauri::command]
pub(crate) fn miniapp_pending_permissions(window: WebviewWindow, state: State<'_, AppState>, thread_id: String) -> Result<Vec<serde_json::Value>, String> {
    runtime_app_id(window.label())?;
    if thread_id.trim().is_empty() {
        return Err("观星台执行会话不能为空".into());
    }
    Ok(state.remote_permissions.lock().map_err(|_| "待审请求锁不可用")?
        .values().filter(|request| request["threadId"].as_str() == Some(thread_id.as_str()))
        .cloned().collect())
}

#[tauri::command]
pub(crate) fn miniapp_show_thread(window: WebviewWindow, app: AppHandle, thread_id: String) -> Result<(), String> {
    runtime_app_id(window.label())?;
    crate::sys_notify::focus_main_window(&app);
    app.emit(crate::acp::EV_NOTIFY_OPEN, serde_json::json!({ "threadId": thread_id }))
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const APP: &str = "12345678-1234-4abc-8def-123456789abc";
    const OTHER: &str = "87654321-4321-4abc-8def-123456789abc";

    #[test]
    fn runtime_labels_are_canonical_and_app_scoped() {
        let app = parse_id(APP).unwrap();
        let other = parse_id(OTHER).unwrap();
        let label = format!("miniapp-{APP}");
        assert_eq!(runtime_app_id("main").unwrap(), None);
        assert_eq!(runtime_app_id(&label).unwrap(), Some(app));
        assert!(authorize_app("main", &app).is_ok());
        assert!(authorize_app("main", &other).is_ok());
        assert!(authorize_app(&label, &app).is_ok());
        assert!(authorize_app(&label, &other).is_err());
        for invalid in ["", "Main", "main-other", "miniapp", "miniapp-", "meteor", "taskbar-bubble", APP] {
            assert!(runtime_app_id(invalid).is_err(), "{invalid}");
            assert!(authorize_app(invalid, &app).is_err(), "{invalid}");
        }
        for invalid in [APP.to_uppercase(), APP.replace('-', ""), format!("{{{APP}}}"), format!("urn:uuid:{APP}"), format!(" {APP}"), format!("{APP} "), format!("{APP}?other=1"), format!("{APP}/extra")] {
            assert!(parse_id(&invalid).is_err(), "{invalid}");
            assert!(runtime_app_id(&format!("miniapp-{invalid}")).is_err(), "{invalid}");
            assert!(window_target(&invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn runtime_command_allowlist_keeps_actions_but_denies_admin_commands() {
        let label = format!("miniapp-{APP}");
        for command in ["miniapp_open", "miniapp_scene_model", "miniapp_show_thread", "miniapp_pending_permissions", "miniapp_command_start", "miniapp_command_status", "miniapp_command_cancel", "miniapp_take_launch", "scratch_dir", "create_thread", "send_prompt", "list_threads", "get_thread_view", "cancel_turn", "respond_permission"] {
            assert!(allows_command(&label, command), "{command}");
            assert!(!allows_command("miniapp-not-a-uuid", command));
            assert!(!allows_command(&format!("miniapp-{}", APP.to_uppercase()), command));
        }
        for command in ["set_settings", "miniapp_generate", "miniapp_browser_triggers", "delete_thread", "terminal_create", "get_settings", "miniapp_unknown", ""] {
            assert!(!allows_command(&label, command), "{command}");
            for existing in ["main", "meteor", "taskbar-bubble"] {
                assert!(allows_command(existing, command), "existing window {existing}");
            }
        }
    }

    #[test]
    fn browser_triggers_accept_only_canonical_ids_and_web_patterns() {
        let trigger = |id: &str, pattern: &str| BrowserTrigger { app_id: id.into(), name: "a".into(), patterns: vec![pattern.into()] };
        assert!(validate_triggers(&[trigger(APP, "https://github.com/*/pull/*"), trigger(OTHER, "*://example.com/*")]).is_ok());
        for (id, pattern) in [(APP, "file:///etc/*"), (APP, "https://"), (APP, "https://a b"), ("not-a-uuid", "https://a")] {
            assert!(validate_triggers(&[trigger(id, pattern)]).is_err(), "{id} {pattern}");
        }
        let json: BrowserTrigger = serde_json::from_value(json!({"appId": APP, "name": "a", "match": ["https://a/*"]})).unwrap();
        assert_eq!(serde_json::to_value(json).unwrap()["match"][0], "https://a/*");
    }

    #[test]
    fn window_url_is_local_and_bound_to_its_label() {
        for app in [APP, OTHER] {
            let (label, url) = window_target(app).unwrap();
            assert_eq!(label, format!("miniapp-{app}"));
            assert_eq!(runtime_app_id(&label).unwrap(), Some(parse_id(app).unwrap()));
            assert!(matches!(url, WebviewUrl::App(path) if path == std::path::PathBuf::from(format!("index.html?miniapp={app}"))));
            assert_eq!(window_target(app).unwrap().0, label);
        }
        for invalid in ["../escape", "https://example.com", "", "not-a-uuid"] {
            assert!(window_target(invalid).is_err());
        }
    }
}
