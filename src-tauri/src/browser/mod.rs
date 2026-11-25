//! Unified next-gen browser automation subsystem:
//! - Top-Layer animated virtual mouse cursor & pointer-driven interaction (`page_runtime.js`)
//! - Direct native CDP WebSocket + Page.startScreencast for Headless Chrome (`cdp_direct.rs`)
//! - Persistent local WebSocket + compound macro commands for Real Chrome (`ext_ws.rs`)
//! - Fast JPEG + Rust-side crisp Set-of-Mark (`[1][2]`) annotation (`annotate.rs`)
//! - Compact 6-tool model interface (`tools.rs`) & unified engine (`engine.rs`)
pub(crate) mod annotate;
pub(crate) mod cdp_direct;
pub(crate) mod engine;
pub(crate) mod ext_ws;
pub(crate) mod protocol;
pub(crate) mod tools;

use serde_json::{json, Value};
use std::{collections::HashSet, path::Path, process::Command};
use tauri::{AppHandle, Manager};

pub(crate) fn init(app: &AppHandle) {
    engine::init(app);
}

pub(crate) fn apply_headless(app: &AppHandle, enabled: bool) {
    ext_ws::set_prefer_headless(app, enabled);
    if !enabled {
        cdp_direct::stop_headless(app);
    }
}

pub(crate) fn stop_headless(app: &AppHandle) {
    cdp_direct::stop_headless(app);
}

pub(crate) fn bind_thread(app: &AppHandle, thread_id: &str, owner: &str) {
    cdp_direct::bind_thread(app, thread_id, owner);
}

pub(crate) fn release_thread(app: &AppHandle, thread_id: &str) {
    cdp_direct::release_thread(app, thread_id);
}

pub(crate) fn tool_owner(root: &Path, owner: &str) -> Result<String, String> {
    engine::tool_owner(root, owner)
}

pub(crate) async fn execute_tool(
    root: &Path,
    name: &str,
    args: &Value,
    owner: &str,
) -> Result<Value, String> {
    engine::execute_tool(root, name, args, owner).await
}

/// 查询真实 Chrome 扩展 WebSocket 连接、已装版本、无痕权限与关联浏览器列表（即使开启无头模式也可用）。
pub(crate) async fn extension_status_summary(app: &AppHandle) -> Value {
    let _ = ext_ws::ensure_listening(app).await;
    let upgraded = ext_ws::upgrade_outdated_extensions(app).await;
    let bundled_version = ext_ws::bundled_version();
    let connected = ext_ws::is_connected(app);
    let status = if connected {
        ext_ws::request(app, "status", json!({})).await.ok()
    } else {
        None
    };
    let installed_version = status
        .as_ref()
        .and_then(|s| s["version"].as_str().map(String::from));
    let incognito_allowed = status.as_ref().map(|s| s["incognitoAllowed"] == true);
    let extensions: Vec<Value> = ext_ws::extension_statuses(app)
        .await
        .into_iter()
        .map(|mut e| {
            e["outdated"] = json!(e["version"].as_str() != bundled_version.as_deref());
            e
        })
        .collect();
    let outdated = connected
        && (installed_version != bundled_version
            || extensions.iter().any(|e| e["outdated"] == true));
    json!({
        "connected": connected,
        "incognitoAllowed": incognito_allowed,
        "version": installed_version,
        "installedVersion": installed_version,
        "bundledVersion": bundled_version,
        "outdated": outdated,
        "upgraded": upgraded,
        "extensions": extensions,
        "headlessPreferred": ext_ws::prefers_headless(app),
        "extensionId": ext_ws::EXTENSION_ID,
    })
}

/// Detect Chrome binary, extension WebSocket connection, version & incognito permission,
/// and release bundled extension files (v0.2.0) to disk.
#[tauri::command]
pub async fn chrome_dependency_status(
    app: AppHandle,
    quick: Option<bool>,
) -> Result<Value, String> {
    let dir = ext_ws::release_extension(&app)?;
    let bundled_version = ext_ws::bundled_version();
    if quick == Some(true) {
        return Ok(json!({
            "chromePath": ext_ws::find_chrome().map(|p| p.to_string_lossy().to_string()),
            "bundledVersion": bundled_version,
            "extensionDir": dir.to_string_lossy(),
            "extensionId": ext_ws::EXTENSION_ID,
        }));
    }
    let mut out = extension_status_summary(&app).await;
    if let Some(obj) = out.as_object_mut() {
        obj.insert("chromePath".into(), json!(ext_ws::find_chrome().map(|p| p.to_string_lossy().to_string())));
        obj.insert("extensionDir".into(), json!(dir.to_string_lossy()));
    }
    Ok(out)
}

#[tauri::command]
pub async fn chrome_browser_ui(
    app: AppHandle,
    webview: tauri::Webview,
    operation: String,
    args: Value,
) -> Result<Value, String> {
    if webview.label() != "main" {
        return Err("仅 Nova 主界面可用".into());
    }
    if matches!(operation.as_str(), "connect" | "status") {
        let port = ext_ws::ensure_listening(&app).await?;
        let mut browsers = ext_ws::live_browsers(&app);
        let headless_connected = cdp_direct::current_client(&app).is_some();
        if headless_connected {
            browsers.push("Google Chrome (无头)".into());
        }
        return Ok(json!({
            "connected": ext_ws::is_connected(&app) || headless_connected,
            "browser": ext_ws::primary_browser(&app),
            "browsers": browsers,
            "origin": format!("http://127.0.0.1:{port}"),
            "extensionId": ext_ws::EXTENSION_ID,
            "headlessPreferred": ext_ws::prefers_headless(&app),
            "headlessConnected": headless_connected,
        }));
    }
    let state = app.state::<crate::AppState>();
    let id = state
        .active_thread
        .lock()
        .unwrap()
        .clone()
        .ok_or("请先打开会话")?;
    let root = state
        .store
        .lock()
        .unwrap()
        .get(&id)
        .map(|t| std::path::PathBuf::from(&t.cwd))
        .ok_or("会话不存在")?;
    let (tool_name, tool_args) = match operation.as_str() {
        "tabs" => ("browser_tabs", json!({"action": "list"})),
        "open" => ("browser_tabs", json!({"action": "open", "url": args["url"]})),
        "view" | "inspect" => ("browser_view", args),
        _ => (operation.as_str(), args),
    };
    engine::execute_tool(&root, tool_name, &tool_args, &format!("ui:{id}")).await
}

/// Sidebar Headless Browser view: returns this session's tabs + ensures smooth real-time
/// Page.startScreencast pushing live frames (showing the animated virtual cursor!).
#[tauri::command]
pub async fn headless_browser_view(
    app: AppHandle,
    webview: tauri::Webview,
    thread_id: Option<String>,
    tab_tag: Option<String>,
) -> Result<Value, String> {
    if webview.label() != "main" {
        return Err("仅 Nova 主界面可用".into());
    }
    if !ext_ws::prefers_headless(&app) {
        return Ok(json!({ "enabled": false }));
    }
    let Some(client) = cdp_direct::current_client(&app) else {
        return Ok(json!({ "enabled": true, "running": false }));
    };
    cdp_direct::touch(&app);
    let all = client.list_tabs();
    let live: HashSet<String> = all.iter().map(|t| t.tag.clone()).collect();
    cdp_direct::prune_dead_tabs(&app, &live);

    let owned = thread_id.as_deref().map(|id| cdp_direct::session_tags(&app, id));
    let mut list: Vec<Value> = all
        .into_iter()
        .filter(|t| owned.as_ref().is_none_or(|set| set.contains(&t.tag)))
        .map(|t| {
            json!({
                "tag": t.tag,
                "title": t.title,
                "url": t.url,
                "active": t.active,
                "loading": t.loading,
            })
        })
        .collect();

    let mut chosen = tab_tag.filter(|tag| list.iter().any(|t| t["tag"] == *tag));
    if chosen.is_none() {
        chosen = list
            .iter()
            .find(|t| t["active"] == true)
            .and_then(|t| t["tag"].as_str().map(str::to_string))
            .or_else(|| list.first().and_then(|t| t["tag"].as_str().map(str::to_string)));
    }
    let Some(tag) = chosen else {
        return Ok(json!({
            "enabled": true,
            "running": true,
            "connected": true,
            "tabs": list,
            "tabTag": Value::Null,
            "notice": cdp_direct::notice(&app),
        }));
    };
    // Highlight chosen tab in list
    for item in &mut list {
        item["active"] = json!(item["tag"] == tag);
    }
    let current = list
        .iter()
        .find(|t| t["tag"] == tag)
        .cloned()
        .unwrap_or(Value::Null);

    let (image, image_error) = match client.ensure_screencast(&tag, thread_id).await {
        Ok(Some(b64)) => (Some(format!("data:image/jpeg;base64,{b64}")), Value::Null),
        Ok(None) => (None, Value::Null),
        Err(err) => (None, json!(err)),
    };

    Ok(json!({
        "enabled": true,
        "running": true,
        "connected": true,
        "tabs": list,
        "tabTag": tag,
        "title": current["title"],
        "url": current["url"],
        "loading": current["loading"],
        "image": image,
        "imageError": image_error,
        "notice": cdp_direct::notice(&app),
    }))
}

/// Open the current headless tab in Real Chrome, syncing cookies + localStorage back!
#[tauri::command]
pub async fn headless_browser_open(
    app: AppHandle,
    webview: tauri::Webview,
    tab_tag: String,
) -> Result<Value, String> {
    if webview.label() != "main" {
        return Err("仅 Nova 主界面可用".into());
    }
    let client = cdp_direct::current_client(&app).ok_or("无头浏览器未启动")?;
    let tab = client
        .list_tabs()
        .into_iter()
        .find(|t| t.tag == tab_tag)
        .ok_or("标签已关闭，请刷新面板")?;
    let url = tab.url;
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err("只能在真实浏览器打开 HTTP(S) 页面".into());
    }
    let mut warnings = Vec::new();
    let cookies = client.export_cookies().await.unwrap_or_else(|e| {
        warnings.push(format!("读取无头 cookie 失败：{e}"));
        Vec::new()
    });
    let storage = client.export_storage(&tab_tag).await.unwrap_or_else(|e| {
        warnings.push(format!("读取无头 localStorage 失败：{e}"));
        Vec::new()
    });
    if !ext_ws::is_connected(&app) {
        let chrome = ext_ws::find_chrome().ok_or("未找到 Google Chrome")?;
        Command::new(chrome)
            .arg(&url)
            .spawn()
            .map_err(|e| format!("打开真实 Chrome 失败：{e}"))?;
        warnings.push("真实 Chrome 未连接 Nova 扩展：只打开页面，未回写无头登录态".into());
        return Ok(json!({
            "opened": true,
            "browser": "system",
            "cookies": 0,
            "localStorage": 0,
            "warnings": warnings,
        }));
    }
    if cookies.is_empty() && storage.is_empty() {
        warnings.push("无头浏览器当前没有登录态可回写".into());
    }
    let imported = ext_ws::import_cookies(&app, &cookies).await.unwrap_or_else(|e| {
        warnings.push(format!("回写 cookie 失败：{e}"));
        0
    });
    let opened = ext_ws::request(&app, "open", json!({ "url": url, "focus": true })).await?;
    let real_tag = opened["tabTag"].as_str().map(str::to_string);
    let mut restored = 0usize;
    if let Some(ref rt) = real_tag {
        if !storage.is_empty() {
            match ext_ws::request(&app, "apply_storage", json!({ "tabTag": rt, "pairs": storage })).await {
                Ok(r) => restored = r["count"].as_u64().unwrap_or(0) as usize,
                Err(e) => warnings.push(format!("回写 localStorage 失败：{e}")),
            }
        }
    }
    Ok(json!({
        "opened": real_tag.is_some(),
        "browser": "chrome",
        "tabTag": real_tag,
        "cookies": imported,
        "localStorage": restored,
        "warnings": warnings,
    }))
}
