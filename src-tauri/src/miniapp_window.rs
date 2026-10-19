//! Independent runtime windows; design sessions remain main-window-only.
use crate::{settings::{ModelTarget, Scene}, AppState};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindow};
use uuid::Uuid;

// ponytail: serialize all runtime window creation to prevent duplicate labels;
// use per-app locks if concurrent cold window creation becomes a bottleneck.
static BUILD: Mutex<()> = Mutex::new(());

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
        | "miniapp_command_status" | "miniapp_command_cancel"
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
            .min_inner_size(600.0, 480.0)
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
        for command in ["miniapp_open", "miniapp_scene_model", "miniapp_show_thread", "miniapp_pending_permissions", "miniapp_command_start", "miniapp_command_status", "miniapp_command_cancel", "scratch_dir", "create_thread", "send_prompt", "list_threads", "get_thread_view", "cancel_turn", "respond_permission"] {
            assert!(allows_command(&label, command), "{command}");
            assert!(!allows_command("miniapp-not-a-uuid", command));
            assert!(!allows_command(&format!("miniapp-{}", APP.to_uppercase()), command));
        }
        for command in ["set_settings", "miniapp_generate", "delete_thread", "terminal_create", "get_settings", "miniapp_unknown", ""] {
            assert!(!allows_command(&label, command), "{command}");
            for existing in ["main", "meteor", "taskbar-bubble"] {
                assert!(allows_command(existing, command), "existing window {existing}");
            }
        }
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
