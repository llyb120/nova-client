//! Unified pointer-driven browser engine shared by Headless Chrome (direct CDP WS)
//! and Real Chrome (extension persistent WS).
use crate::browser::{
    annotate::{process_screenshot_with_guard, ShotMapping, SomBox},
    cdp_direct, ext_ws,
    protocol::{key_spec, normalized_url, upload_files, RawActResult, RawView, Step, TabInfo, ViewOptions},
    tools,
};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};
use tauri::{AppHandle, Listener, Manager};

static APP: OnceLock<AppHandle> = OnceLock::new();

#[derive(Clone)]
struct TabSnapshot {
    id: String,
    url: String,
    captured: Instant,
    mapping: Option<ShotMapping>,
    items: Vec<Value>,
    delivered: Option<crate::visual_guard::Delivered>,
}

#[derive(Default)]
pub(crate) struct EngineState {
    gate: tokio::sync::Mutex<()>,
    /// `"{owner}:{root}" -> active tab tag`
    active_tabs: Mutex<HashMap<String, String>>,
    /// `"{owner}:{root}:{tag}" -> latest TabSnapshot`
    snapshots: Mutex<HashMap<String, TabSnapshot>>,
}

pub(crate) fn init(app: &AppHandle) {
    app.manage(EngineState::default());
    ext_ws::init(app);
    cdp_direct::init_headless(app);
    let enabled = app
        .state::<crate::AppState>()
        .settings
        .lock()
        .unwrap()
        .headless_browser_enabled;
    ext_ws::set_prefer_headless(app, enabled);
    let _ = APP.set(app.clone());
    // Dismiss any active virtual mouse cursor only when the task/turn finishes, cancels, or errors
    // (never mid-task while another turn is actively driving the browser).
    let cleanup_app = app.clone();
    app.listen(crate::acp::EV_TURN, move |e| {
        let Ok(v) = serde_json::from_str::<Value>(e.payload()) else { return };
        if v["running"].as_bool().unwrap_or(false) {
            return;
        }
        let ended_tid = v["threadId"].as_str().map(str::to_owned);
        let app_handle = cleanup_app.clone();
        tauri::async_runtime::spawn(async move {
            let another_running = app_handle.try_state::<crate::AppState>().is_some_and(|st| {
                st.store.lock().ok().is_some_and(|store| {
                    store.threads.iter().any(|t| {
                        ended_tid.as_deref() != Some(t.id.as_str()) && crate::is_running(&st, t)
                    })
                })
            });
            if another_running {
                return;
            }
            cdp_direct::dismiss_headless_cursor(&app_handle, None).await;
            if ext_ws::is_connected(&app_handle) {
                let _ = ext_ws::request(&app_handle, "cursor_hide", json!({})).await;
            }
        });
    });
}

fn app_handle() -> Result<&'static AppHandle, String> {
    APP.get().ok_or_else(|| "浏览器工具仅在 Nova 桌面应用内可用".into())
}

pub(crate) fn tool_owner(root: &Path, owner: &str) -> Result<String, String> {
    if owner.trim().is_empty() || owner.len() > 4096 {
        return Err("缺少有效会话标识".into());
    }
    let root = dunce::canonicalize(root).map_err(|e| e.to_string())?;
    if !root.is_dir() {
        return Err("工具工作目录不存在".into());
    }
    Ok(format!("{owner}:{}", root.display()))
}

fn session_key(root: &Path, owner: &str) -> Result<String, String> {
    tool_owner(root, owner)
}

fn shot_dir(app: &AppHandle, owner: &str) -> PathBuf {
    let dir = crate::jianlai::shot_folder(owner, "browser-shots");
    let _ = std::fs::create_dir_all(&dir);
    if dir.is_dir() {
        dir
    } else {
        let fallback = app.state::<crate::AppState>().config_dir.join("browser-shots");
        let _ = std::fs::create_dir_all(&fallback);
        fallback
    }
}

fn bind_tab(state: &EngineState, skey: &str, tag: &str) {
    state.active_tabs.lock().unwrap().insert(skey.to_string(), tag.to_string());
}

fn resolve_tab(state: &EngineState, skey: &str, explicit: Option<&str>) -> Result<String, String> {
    if let Some(tag) = explicit.map(str::trim).filter(|s| !s.is_empty()) {
        if tag.strip_prefix('C').is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())) {
            // Keep shorthand in the bound browser's namespace; the extension still checks the full tag.
            let active = state.active_tabs.lock().unwrap();
            let (_, client) = active.get(skey).filter(|t| t.starts_with('C')).and_then(|t| t.split_once('-'))
                .ok_or_else(|| format!("短标签 {tag} 缺少浏览器标识；请调用 browser_tabs(action=\"list\") 并使用返回的完整 tag，无需重新打开网页"))?;
            return Ok(format!("{tag}-{client}"));
        }
        return Ok(tag.to_string());
    }
    state
        .active_tabs
        .lock()
        .unwrap()
        .get(skey)
        .cloned()
        .ok_or_else(|| "尚未绑定标签页；请先调用 browser_tabs(action=\"list\") 查找或 browser_tabs(action=\"open\", url=...) 打开网页".into())
}

#[cfg(test)]
#[test]
fn short_chrome_tags_stay_in_the_bound_browser() {
    let state = EngineState::default();
    bind_tab(&state, "session", "C4-32d2cdd2");
    assert_eq!(resolve_tab(&state, "session", Some("C3")).unwrap(), "C3-32d2cdd2");
    assert_eq!(resolve_tab(&state, "session", None).unwrap(), "C4-32d2cdd2");
    assert_eq!(resolve_tab(&state, "session", Some("C3-deadbeef")).unwrap(), "C3-deadbeef");
    assert!(resolve_tab(&state, "other", Some("C3")).unwrap_err().contains("完整 tag"));
    bind_tab(&state, "session", "H1");
    assert!(resolve_tab(&state, "session", Some("C3")).is_err());
}

/// Public entry point for Lyra tool execution.
pub(crate) async fn execute_tool(root: &Path, name: &str, args: &Value, owner: &str) -> Result<Value, String> {
    let checked = tools::validate(name, args)?;
    let app = app_handle()?;
    let skey = session_key(root, owner)?;

    if name == "browser_experience" {
        return execute_experience(app, &skey, &checked).await;
    }

    let headless = ext_ws::prefers_headless_for(app, owner);
    let mutating = matches!(name, "browser_act" | "browser_eval")
        || (name == "browser_tabs" && checked["action"].as_str() != Some("list"));

    if !headless {
        crate::employee::claim_desktop(owner, mutating).await?;
    }
    let _lease = if !headless && mutating {
        Some(crate::jianlai::lease_input("chrome")?)
    } else {
        None
    };

    let state = app.state::<EngineState>();
    let _gate = state.gate.lock().await;

    let mut result = match name {
        "browser_tabs" => exec_tabs(app, &state, &skey, owner, headless, &checked).await?,
        "browser_view" => exec_view(app, &state, &skey, owner, headless, &checked).await?,
        "browser_act" => exec_act(app, &state, &skey, root, owner, headless, &checked).await?,
        "browser_eval" => exec_eval(app, &state, &skey, owner, headless, &checked).await?,
        "browser_debug" => exec_debug(app, &state, &skey, owner, headless, &checked).await?,
        _ => return Err(format!("未知浏览器工具：{name}")),
    };

    // Attach site experience hints automatically when first landing on a site.
    if let Some(url) = result["url"].as_str() {
        if let Ok(parsed) = tauri::Url::parse(url) {
            let origin = parsed.origin().ascii_serialization();
            if let Some(hint) = crate::tool_experience::hint(&crate::tool_experience::dir(), "chrome", &skey, &origin) {
                result["experienceHint"] = hint;
            }
        }
    }
    Ok(result)
}

async fn exec_tabs(
    app: &AppHandle,
    state: &EngineState,
    skey: &str,
    owner: &str,
    headless: bool,
    args: &Value,
) -> Result<Value, String> {
    let action = args["action"].as_str().unwrap_or("list");
    let with_view = args["view"].as_bool().unwrap_or(true);
    let view_opts = ViewOptions::default();

    if headless {
        let client = cdp_direct::ensure_headless(app).await?;
        match action {
            "list" => {
                let mut tabs = client.list_tabs();
                if let Some(q) = args["query"].as_str().map(str::trim).filter(|s| !s.is_empty()) {
                    let ql = q.to_lowercase();
                    tabs.retain(|t| t.title.to_lowercase().contains(&ql) || t.url.to_lowercase().contains(&ql));
                    if tabs.len() == 1 && tabs[0].controllable {
                        let tag = tabs[0].tag.clone();
                        bind_tab(state, skey, &tag);
                        cdp_direct::claim_tab(app, owner, &tag);
                        let mut out = json!({ "tab": tag, "tabs": compact_tabs(&tabs) });
                        if with_view {
                            if let Ok(raw) = client.view(&tag, &view_opts).await {
                                let formatted = format_view(app, state, skey, owner, &tag, raw, true)?;
                                merge_object(&mut out, formatted);
                            }
                        }
                        return Ok(out);
                    }
                }
                Ok(json!({ "tabs": compact_tabs(&tabs), "mode": "headless" }))
            }
            "open" => {
                let url = args["url"].as_str().unwrap_or("about:blank");
                let tab = client.open_tab(url).await?;
                let tag = tab.tag.clone();
                bind_tab(state, skey, &tag);
                cdp_direct::claim_tab(app, owner, &tag);
                let mut out = json!({ "tab": tag, "tabs": compact_tabs(&client.list_tabs()) });
                if with_view && url != "about:blank" {
                    match client.view(&tag, &view_opts).await {
                        Ok(raw) => merge_object(&mut out, format_view(app, state, skey, owner, &tag, raw, true)?),
                        Err(e) => out["viewError"] = json!(e),
                    }
                }
                Ok(out)
            }
            "goto" | "switch" | "close" | "back" | "forward" | "reload" => {
                let tag = resolve_tab(state, skey, args["tab"].as_str())?;
                match action {
                    "goto" => {
                        let url = args["url"].as_str().ok_or("goto 需要 url")?;
                        client.goto(&tag, url).await?;
                    }
                    "switch" => client.select_tab(&tag).await?,
                    "close" => {
                        client.close_tab(&tag).await?;
                        state.active_tabs.lock().unwrap().retain(|k, v| !(k == skey && v == &tag));
                        state.snapshots.lock().unwrap().remove(&format!("{skey}:{tag}"));
                        return Ok(json!({ "closed": tag, "tabs": compact_tabs(&client.list_tabs()) }));
                    }
                    "back" => client.navigate_history(&tag, -1).await?,
                    "forward" => client.navigate_history(&tag, 1).await?,
                    "reload" => client.reload(&tag).await?,
                    _ => unreachable!(),
                }
                bind_tab(state, skey, &tag);
                cdp_direct::claim_tab(app, owner, &tag);
                let mut out = json!({ "tab": tag, "tabs": compact_tabs(&client.list_tabs()) });
                if with_view {
                    match client.view(&tag, &view_opts).await {
                        Ok(raw) => merge_object(&mut out, format_view(app, state, skey, owner, &tag, raw, true)?),
                        Err(e) => out["viewError"] = json!(e),
                    }
                }
                Ok(out)
            }
            _ => Err(format!("未知 action：{action}")),
        }
    } else {
        // Real Chrome via persistent WebSocket extension
        match action {
            "list" => {
                let filter = args["incognito"].as_bool().map_or_else(|| json!({}), |v| json!({ "incognito": v }));
                let res = ext_ws::request(app, "tabs", filter).await?;
                let mut tabs: Vec<TabInfo> = serde_json::from_value(res["tabs"].clone())
                    .map_err(|e| format!("无法解析 Chrome 标签列表：{e}"))?;
                if let Some(q) = args["query"].as_str().map(str::trim).filter(|s| !s.is_empty()) {
                    let ql = q.to_lowercase();
                    tabs.retain(|t| t.title.to_lowercase().contains(&ql) || t.url.to_lowercase().contains(&ql));
                    if tabs.len() == 1 && tabs[0].controllable {
                        let tag = tabs[0].tag.clone();
                        bind_tab(state, skey, &tag);
                        let mut out = json!({ "tab": tag, "tabs": compact_tabs(&tabs) });
                        if with_view {
                            if let Ok(v) = ext_ws::request(app, "view", json!({ "tabTag": tag, "viewOpts": view_opts })).await {
                                if let Ok(raw) = serde_json::from_value::<RawView>(v) {
                                    merge_object(&mut out, format_view(app, state, skey, owner, &tag, raw, true)?);
                                }
                            }
                        }
                        return Ok(out);
                    }
                }
                Ok(json!({ "tabs": compact_tabs(&tabs), "incognitoAllowed": res["incognitoAllowed"] }))
            }
            "open" => {
                let url = match args["url"].as_str() {
                    Some(u) => Some(normalized_url(u)?.to_string()),
                    None => None,
                };
                let mut payload = json!({ "url": url, "incognito": args.get("incognito") });
                if with_view && url.is_some() {
                    payload["viewOpts"] = serde_json::to_value(&view_opts).unwrap();
                }
                let res = ext_ws::request(app, "open", payload).await?;
                let tag = res["tabTag"].as_str().ok_or("打开标签失败")?.to_string();
                bind_tab(state, skey, &tag);
                let tabs: Vec<TabInfo> = serde_json::from_value(res["tabs"].clone()).unwrap_or_default();
                let mut out = json!({ "tab": tag, "tabs": compact_tabs(&tabs) });
                if let Ok(raw) = serde_json::from_value::<RawView>(res["view"].clone()) {
                    merge_object(&mut out, format_view(app, state, skey, owner, &tag, raw, true)?);
                }
                Ok(out)
            }
            "goto" | "switch" | "close" | "back" | "forward" | "reload" => {
                let tag = resolve_tab(state, skey, args["tab"].as_str())?;
                let op = match action {
                    "switch" => "select_tab",
                    "close" => "close_tab",
                    other => other,
                };
                let mut payload = json!({ "tabTag": tag });
                if action == "goto" {
                    let u = args["url"].as_str().ok_or("goto 需要 url")?;
                    payload["url"] = json!(normalized_url(u)?.to_string());
                }
                if action != "close" && with_view {
                    payload["viewOpts"] = serde_json::to_value(&view_opts).unwrap();
                }
                let res = ext_ws::request(app, op, payload).await?;
                let tabs: Vec<TabInfo> = serde_json::from_value(res["tabs"].clone()).unwrap_or_default();
                if action == "close" {
                    state.active_tabs.lock().unwrap().retain(|k, v| !(k == skey && v == &tag));
                    state.snapshots.lock().unwrap().remove(&format!("{skey}:{tag}"));
                    return Ok(json!({ "closed": tag, "tabs": compact_tabs(&tabs) }));
                }
                bind_tab(state, skey, &tag);
                let mut out = json!({ "tab": tag, "tabs": compact_tabs(&tabs) });
                if let Ok(raw) = serde_json::from_value::<RawView>(res["view"].clone()) {
                    merge_object(&mut out, format_view(app, state, skey, owner, &tag, raw, true)?);
                }
                Ok(out)
            }
            _ => Err(format!("未知 action：{action}")),
        }
    }
}

async fn exec_view(
    app: &AppHandle,
    state: &EngineState,
    skey: &str,
    owner: &str,
    headless: bool,
    args: &Value,
) -> Result<Value, String> {
    let tag = resolve_tab(state, skey, args["tab"].as_str())?;
    bind_tab(state, skey, &tag);

    let mut opts = ViewOptions {
        scope: args["scope"].as_str().map(str::to_string),
        query: args["query"].as_str().map(str::to_string),
        max_items: args["maxItems"].as_u64().map(|n| n as u32),
        max_text_chars: args["maxTextChars"].as_u64().map(|n| n as u32),
        screenshot: args["screenshot"].as_bool().unwrap_or(true),
        annotate: true,
        zoom_id: None,
        zoom_rect: None,
    };

    if let Some(zoom) = args.get("zoom").and_then(Value::as_object) {
        if let Some(id) = zoom.get("id").and_then(Value::as_u64) {
            opts.zoom_id = Some(id as u32);
        } else if let (Some(x), Some(y), Some(w), Some(h)) = (
            zoom.get("x").and_then(Value::as_f64),
            zoom.get("y").and_then(Value::as_f64),
            zoom.get("width").and_then(Value::as_f64),
            zoom.get("height").and_then(Value::as_f64),
        ) {
            // Map image pixel zoom rect -> CSS coordinates using latest snapshot mapping.
            let snap_key = format!("{skey}:{tag}");
            let css_rect = state
                .snapshots
                .lock()
                .unwrap()
                .get(&snap_key)
                .and_then(|s| s.mapping.as_ref())
                .map(|m| -> Result<[f64; 4], String> {
                    let (cx0, cy0) = m.image_to_css(x, y)?;
                    let (cx1, cy1) = m.image_to_css((x + w).min(m.pixel_width as f64), (y + h).min(m.pixel_height as f64))?;
                    Ok([cx0, cy0, (cx1 - cx0).max(1.0), (cy1 - cy0).max(1.0)])
                })
                .transpose()?
                .unwrap_or([x, y, w, h]);
            opts.zoom_rect = Some(css_rect);
        }
    }

    let raw = if headless {
        let client = cdp_direct::ensure_headless(app).await?;
        cdp_direct::claim_tab(app, owner, &tag);
        client.view(&tag, &opts).await?
    } else {
        let val = ext_ws::request(app, "view", json!({ "tabTag": tag, "viewOpts": opts })).await?;
        serde_json::from_value(val).map_err(|e| format!("解析页面视图失败：{e}"))?
    };

    let track_stream = opts.query.as_deref().is_none_or(str::is_empty) && opts.zoom_id.is_none() && opts.zoom_rect.is_none();
    format_view_opts(app, state, skey, owner, &tag, raw, true, track_stream)
}

async fn exec_act(
    app: &AppHandle,
    state: &EngineState,
    skey: &str,
    root: &Path,
    owner: &str,
    headless: bool,
    args: &Value,
) -> Result<Value, String> {
    let tag = resolve_tab(state, skey, args["tab"].as_str())?;
    bind_tab(state, skey, &tag);

    let snap_key = format!("{skey}:{tag}");
    let prev_snap = state.snapshots.lock().unwrap().get(&snap_key).cloned();
    let mapping = prev_snap.as_ref().and_then(|s| s.mapping.clone());

    let raw_steps = args["steps"].as_array().ok_or("缺少 steps")?;
    let mut steps: Vec<Step> = Vec::with_capacity(raw_steps.len());
    for raw in raw_steps {
        let mut step: Step = serde_json::from_value(raw.clone())
            .map_err(|e| format!("动作步骤格式无效：{e}"))?;
        // Validate & map image-pixel coordinates -> CSS viewport coordinates!
        match &mut step {
            Step::Click { id, x, y, .. } | Step::Hover { id, x, y } => {
                if id.is_none() {
                    let (px, py) = (x.ok_or("缺少 id 或 x/y 坐标")?, y.ok_or("缺少 id 或 x/y 坐标")?);
                    let (cx, cy) = map_point(mapping.as_ref(), px, py)?;
                    *x = Some(cx);
                    *y = Some(cy);
                }
            }
            Step::Type { id, x, y, .. } => {
                if id.is_none() && (x.is_some() || y.is_some()) {
                    let (px, py) = (x.ok_or("type 坐标缺少 x")?, y.ok_or("type 坐标缺少 y")?);
                    let (cx, cy) = map_point(mapping.as_ref(), px, py)?;
                    *x = Some(cx);
                    *y = Some(cy);
                }
            }
            Step::Drag { x1, y1, x2, y2, .. } => {
                let (cx1, cy1) = map_point(mapping.as_ref(), *x1, *y1)?;
                let (cx2, cy2) = map_point(mapping.as_ref(), *x2, *y2)?;
                *x1 = cx1; *y1 = cy1; *x2 = cx2; *y2 = cy2;
            }
            Step::Scroll { id, x, y, .. } => {
                if id.is_none() && (x.is_some() || y.is_some()) {
                    let (px, py) = (x.unwrap_or(640.0), y.unwrap_or(400.0));
                    let (cx, cy) = map_point(mapping.as_ref(), px, py)?;
                    *x = Some(cx);
                    *y = Some(cy);
                }
            }
            Step::Press { key } => {
                if key_spec(key).is_none() {
                    return Err(format!("不支持的按键：{key}"));
                }
            }
            Step::Upload { selector, files } => {
                if selector.trim().is_empty() {
                    return Err("upload 需要非空 selector".into());
                }
                *files = upload_files(root, files)?;
            }
            Step::Wait { .. } | Step::Dismiss => {}
        }
        steps.push(step);
    }

    let clipboard_before = crate::clipboard::sequence();
    let view_opts = ViewOptions {
        screenshot: args["screenshot"].as_bool().unwrap_or(true),
        ..Default::default()
    };

    let act_res: RawActResult = if headless {
        let client = cdp_direct::ensure_headless(app).await?;
        cdp_direct::claim_tab(app, owner, &tag);
        client.act(&tag, &steps, root, Some(&view_opts)).await?
    } else {
        let val = ext_ws::request(
            app,
            "act",
            json!({
                "tabTag": tag,
                "steps": steps,
                "viewOpts": view_opts,
            }),
        )
        .await?;
        serde_json::from_value(val).map_err(|e| format!("解析动作结果失败：{e}"))?
    };

    // If a single controllable popup tab opened, automatically bind the session to it!
    let final_tag = if act_res.opened_tabs.len() == 1 && act_res.opened_tabs[0].controllable {
        let new_tag = act_res.opened_tabs[0].tag.clone();
        bind_tab(state, skey, &new_tag);
        if headless {
            cdp_direct::claim_tab(app, owner, &new_tag);
        }
        new_tag
    } else {
        tag.clone()
    };

    let mut out = json!({
        "ok": act_res.ok,
        "tab": final_tag,
        "completed": act_res.completed,
        "actions": act_res.actions,
        "actionMs": act_res.action_ms,
    });
    if let Some(err) = act_res.error {
        out["error"] = json!(err);
    }
    if let Some(notes) = args.get("notes").filter(|v| !v.is_null()) {
        out["notes"] = notes.clone();
    }
    if !act_res.opened_tabs.is_empty() {
        out["openedTabs"] = json!(compact_tabs(&act_res.opened_tabs));
    }
    if let Some(copied) = crate::clipboard::copied_since(clipboard_before) {
        out["clipboard"] = copied;
    }

    // Record semantic steps into site experience auto-trail.
    let before_url = prev_snap.as_ref().map(|s| s.url.as_str()).unwrap_or_default();
    let trail_steps: Vec<String> = steps[..act_res.completed.min(steps.len())]
        .iter()
        .filter_map(|st| trail_step(prev_snap.as_ref().map(|s| s.items.as_slice()).unwrap_or(&[]), st))
        .collect();

    if let Some(raw_view) = act_res.view {
        let after_url = raw_view.url.clone();
        let after_title = raw_view.title.clone();
        let formatted = format_view(app, state, skey, owner, &final_tag, raw_view, true)?;
        merge_object(&mut out, formatted);
        if !trail_steps.is_empty() {
            crate::tool_experience::record("chrome", skey, before_url, &after_url, &after_title, trail_steps);
        }
    }

    Ok(out)
}

async fn exec_eval(
    app: &AppHandle,
    state: &EngineState,
    skey: &str,
    owner: &str,
    headless: bool,
    args: &Value,
) -> Result<Value, String> {
    let tag = resolve_tab(state, skey, args["tab"].as_str())?;
    bind_tab(state, skey, &tag);
    let code = args["code"].as_str().ok_or("缺少 code")?;
    let with_view = args["view"].as_bool().unwrap_or(false);

    if headless {
        let client = cdp_direct::ensure_headless(app).await?;
        cdp_direct::claim_tab(app, owner, &tag);
        let mut res = client.eval(&tag, code, with_view).await?;
        res["tab"] = json!(tag);
        if let Some(raw_val) = res.as_object_mut().and_then(|o| o.remove("rawView")) {
            if let Ok(raw) = serde_json::from_value::<RawView>(raw_val) {
                merge_object(&mut res, format_view(app, state, skey, owner, &tag, raw, true)?);
            }
        }
        Ok(res)
    } else {
        let mut res = ext_ws::request(
            app,
            "eval",
            json!({ "tabTag": tag, "code": code, "withView": with_view }),
        )
        .await?;
        res["tab"] = json!(tag);
        if let Some(raw_val) = res.as_object_mut().and_then(|o| o.remove("view")) {
            if let Ok(raw) = serde_json::from_value::<RawView>(raw_val) {
                merge_object(&mut res, format_view(app, state, skey, owner, &tag, raw, true)?);
            }
        }
        Ok(res)
    }
}

async fn exec_debug(
    app: &AppHandle,
    state: &EngineState,
    skey: &str,
    owner: &str,
    headless: bool,
    args: &Value,
) -> Result<Value, String> {
    let kind = args["kind"].as_str().unwrap_or("console");
    if kind == "downloads" {
        if headless {
            return Ok(json!({ "downloads": [], "notice": "无头模式暂不支持 Chrome downloads 查询，请检查下载目录或切回真实 Chrome" }));
        }
        return ext_ws::request(
            app,
            "downloads",
            json!({ "downloadId": args.get("downloadId"), "since": args.get("since") }),
        )
        .await;
    }
    let tag = resolve_tab(state, skey, args["tab"].as_str())?;
    if headless {
        let client = cdp_direct::ensure_headless(app).await?;
        cdp_direct::claim_tab(app, owner, &tag);
        client
            .debug(
                &tag,
                kind,
                args["query"].as_str(),
                args["level"].as_str(),
                args["limit"].as_u64().unwrap_or(100) as usize,
                args["requestId"].as_str(),
                args["clear"] == true,
            )
            .await
    } else {
        ext_ws::request(
            app,
            "debug",
            json!({
                "tabTag": tag,
                "kind": kind,
                "query": args.get("query"),
                "level": args.get("level"),
                "limit": args.get("limit"),
                "requestId": args.get("requestId"),
                "clear": args.get("clear"),
            }),
        )
        .await
    }
}

async fn execute_experience(app: &AppHandle, skey: &str, args: &Value) -> Result<Value, String> {
    let action = args["action"].as_str().ok_or("缺少 action")?;
    let op = match action {
        "search" => "experience_search",
        "save" => "experience_save",
        "feedback" => "experience_feedback",
        _ => return Err(format!("未知经验动作：{action}")),
    };
    let mut exp = args.clone();
    if let Some(o) = exp.as_object_mut() {
        o.remove("action");
        o.remove("tab");
        o.remove("snapshotId");
        if let Some(id) = o.remove("routeId") {
            o.insert("id".into(), id);
        }
        if op == "experience_save" {
            o.insert("outcome".into(), json!("success"));
        }
    }
    let mut wrapped = json!({ "operation": op, "experience": exp });

    // Jianpu recording save path
    if let Some(rec_id) = args["recordingId"].as_str().filter(|s| !s.is_empty()).map(str::to_string) {
        if op != "experience_save" {
            return Err("recordingId 仅用于 save".into());
        }
        let scope = args["scope"].as_str().unwrap_or_default().to_string();
        let skey = skey.to_string();
        return tokio::task::spawn_blocking(move || {
            let observed = crate::jianpu::scope_of(&rec_id, "chrome", &scope)?;
            wrapped["snapshotId"] = json!(format!("{}{rec_id}", crate::tool_experience::DEMO));
            let result = crate::tool_experience::execute(
                &crate::tool_experience::dir(),
                "chrome",
                &skey,
                &wrapped,
                Some(&observed),
            )?;
            if let Some(route) = result["experience"]["id"].as_str() {
                crate::jianpu::link_route(&rec_id, route);
            }
            Ok(result)
        })
        .await
        .map_err(|e| e.to_string())?;
    }

    let observed_origin = if op == "experience_search" {
        None
    } else {
        let state = app.state::<EngineState>();
        let tag = resolve_tab(&state, skey, args["tab"].as_str())?;
        let snap = state
            .snapshots
            .lock()
            .unwrap()
            .get(&format!("{skey}:{tag}"))
            .filter(|s| s.captured.elapsed() <= Duration::from_secs(180))
            .cloned()
            .ok_or("保存或反馈经验前需先查看该标签页（180秒内）")?;
        wrapped["snapshotId"] = json!(args["snapshotId"].as_str().unwrap_or(&snap.id));
        let parsed = tauri::Url::parse(&snap.url).map_err(|e| e.to_string())?;
        Some(parsed.origin().ascii_serialization())
    };

    let skey = skey.to_string();
    if op == "experience_save" {
        return crate::tool_experience::execute_async(
            &crate::tool_experience::dir(),
            "chrome",
            &skey,
            &wrapped,
            observed_origin.as_deref(),
        );
    }
    tokio::task::spawn_blocking(move || {
        crate::tool_experience::execute(
            &crate::tool_experience::dir(),
            "chrome",
            &skey,
            &wrapped,
            observed_origin.as_deref(),
        )
    })
    .await
    .map_err(|e| e.to_string())?
}

fn format_view(
    app: &AppHandle,
    state: &EngineState,
    skey: &str,
    owner: &str,
    tag: &str,
    raw: RawView,
    draw_som: bool,
) -> Result<Value, String> {
    format_view_opts(app, state, skey, owner, tag, raw, draw_som, true)
}

#[allow(clippy::too_many_arguments)]
fn format_view_opts(
    app: &AppHandle,
    state: &EngineState,
    skey: &str,
    owner: &str,
    tag: &str,
    raw: RawView,
    draw_som: bool,
    track_stream: bool,
) -> Result<Value, String> {
    let snap_id = uuid::Uuid::new_v4().to_string();
    let css_w = raw.viewport[0].max(1.0);
    let css_h = raw.viewport[1].max(1.0);
    let snap_key = format!("{skey}:{tag}");
    let prev_delivered = state
        .snapshots
        .lock()
        .unwrap()
        .get(&snap_key)
        .filter(|s| s.captured.elapsed() <= Duration::from_secs(180))
        .and_then(|s| s.delivered.clone());

    // Build SoM boxes from visible items
    let som_boxes: Vec<SomBox> = raw
        .items
        .iter()
        .filter_map(|it| {
            let id = it["id"].as_u64()? as u32;
            let r = it["rect"].as_array()?;
            if r.len() != 4 { return None; }
            Some(SomBox {
                id,
                rect: [
                    r[0].as_f64().unwrap_or(0.0),
                    r[1].as_f64().unwrap_or(0.0),
                    r[2].as_f64().unwrap_or(0.0),
                    r[3].as_f64().unwrap_or(0.0),
                ],
            })
        })
        .collect();

    let crop_css = raw.zoom_css.map(|r| (r[0], r[1], r[2], r[3]));
    let mut mapping: Option<ShotMapping> = None;
    let mut new_delivered: Option<crate::visual_guard::Delivered> = None;
    let mut shot_error = raw.screenshot_error.clone();
    let mut images_val = Vec::new();

    if let Some(ref b64) = raw.screenshot_b64 {
        let out_path = shot_dir(app, owner).join(&snap_id);
        let stream = (track_stream && crop_css.is_none()).then(|| format!("{tag}:viewport"));
        let mut entry = json!({
            "imageId": format!("{snap_id}-0"),
            "snapshotId": snap_id,
        });
        match process_screenshot_with_guard(
            b64,
            (css_w, css_h),
            crop_css,
            &som_boxes,
            draw_som,
            &out_path,
            &mut entry,
            prev_delivered.as_ref(),
            stream,
        ) {
            Ok((m, deliv)) => {
                images_val.push(entry);
                mapping = Some(m);
                new_delivered = deliv;
            }
            Err(err) => shot_error = Some(err),
        }
    }

    // Format compact element lines with exact screenshot-pixel @(x, y) coordinates!
    let mut lines = Vec::with_capacity(raw.items.len());
    for it in &raw.items {
        let id = it["id"].as_u64().unwrap_or(0);
        let role = it["role"].as_str().unwrap_or("el");
        let name = it["name"].as_str().unwrap_or_default();
        let cx = it["x"].as_f64().unwrap_or(0.0);
        let cy = it["y"].as_f64().unwrap_or(0.0);
        let (px, py) = mapping
            .as_ref()
            .map(|m| m.css_to_image(cx, cy))
            .unwrap_or((cx.round() as i32, cy.round() as i32));
        let mut line = format!("[{id}] {role} {}", json!(name));
        if let Some(val) = it["val"].as_str().filter(|s| !s.is_empty()) {
            line.push_str(&format!(" val={}", json!(val)));
        }
        if let Some(opts_arr) = it["opts"].as_array().filter(|a| !a.is_empty()) {
            line.push_str(&format!(" options={}", json!(opts_arr)));
        }
        if let Some(chk) = it["checked"].as_bool() {
            line.push_str(&format!(" checked={chk}"));
        }
        if let Some(exp) = it["expanded"].as_bool() {
            line.push_str(&format!(" expanded={exp}"));
        }
        if let Some(href) = it["href"].as_str().filter(|s| !s.is_empty()) {
            line.push_str(&format!(" href={}", json!(href)));
        }
        if let Some(cv) = it["canvas"].as_array() {
            line.push_str(&format!(" canvas={}x{}", cv.first().and_then(Value::as_u64).unwrap_or(0), cv.get(1).and_then(Value::as_u64).unwrap_or(0)));
        }
        if it["editable"] == true { line.push_str(" editable"); }
        if it["disabled"] == true { line.push_str(" disabled"); }
        if it["scroll"] == true {
            if let Some(sy) = it["scrollY"].as_array().filter(|a| a.len() == 2) {
                let top = sy[0].as_i64().unwrap_or(0);
                let max = sy[1].as_i64().unwrap_or(0);
                if max > top + 20 {
                    line.push_str(&format!(" scroll={top}/{max}(↓more)"));
                } else {
                    line.push_str(&format!(" scroll={top}/{max}"));
                }
            } else {
                line.push_str(" scroll");
            }
        }
        if it["inOverlay"] == true { line.push_str(" [overlay]"); }
        if it["inView"] == false { line.push_str(" offscreen"); }
        line.push_str(&format!(" @({px},{py})"));
        lines.push(line);
    }

    let overlay_out = raw.overlay.map(|mut ov| {
        if let Some(m) = mapping.as_ref() {
            if let Some(pt) = ov["outsidePoint"].as_array().filter(|a| a.len() == 2) {
                let (px, py) = m.css_to_image(pt[0].as_f64().unwrap_or(0.0), pt[1].as_f64().unwrap_or(0.0));
                ov["outsidePoint"] = json!([px, py]);
            }
            if let Some(r) = ov["rect"].as_array().filter(|a| a.len() == 4) {
                let (x0, y0) = m.css_to_image(r[0].as_f64().unwrap_or(0.0), r[1].as_f64().unwrap_or(0.0));
                let (x1, y1) = m.css_to_image(
                    r[0].as_f64().unwrap_or(0.0) + r[2].as_f64().unwrap_or(0.0),
                    r[1].as_f64().unwrap_or(0.0) + r[3].as_f64().unwrap_or(0.0),
                );
                ov["rect"] = json!([x0, y0, (x1 - x0).max(0), (y1 - y0).max(0)]);
            }
        }
        if ov["hasConfirm"] == false {
            if let Some(pt) = ov["outsidePoint"].as_array().filter(|a| a.len() == 2) {
                ov["hint"] = json!(format!(
                    "当前弹层内无确认按钮；选完后可用 op=\"dismiss\" 或点击空白处 @({},{}) 收起弹层使选择生效（切勿误点已选标签内的 ×）",
                    pt[0], pt[1]
                ));
            } else {
                ov["hint"] = json!("当前弹层内无确认按钮；选完后可用 op=\"dismiss\" 收起弹层（切勿误点已选标签内的 ×）");
            }
        }
        ov
    });

    let cursor_img = raw.cursor.map(|[cx, cy]| {
        let (px, py) = mapping
            .as_ref()
            .map(|m| m.css_to_image(cx, cy))
            .unwrap_or((cx.round() as i32, cy.round() as i32));
        json!([px, py])
    });

    let vp_out = mapping
        .as_ref()
        .map(|m| json!([m.pixel_width, m.pixel_height]))
        .unwrap_or_else(|| json!([css_w.round() as i32, css_h.round() as i32]));

    state.snapshots.lock().unwrap().insert(
        snap_key,
        TabSnapshot {
            id: snap_id.clone(),
            url: raw.url.clone(),
            captured: Instant::now(),
            mapping,
            items: raw.items,
            delivered: new_delivered.or(prev_delivered),
        },
    );

    let (scroll_y_out, scroll_total_out, scroll_hint) = if let Some(ref si) = raw.scroll_info {
        let y = si["y"].as_i64().unwrap_or(raw.scroll[1].round() as i64);
        let max_y = si["maxY"].as_i64().unwrap_or(0);
        let view_h = si["viewH"].as_i64().unwrap_or(css_h.round() as i64).max(1);
        let total_h = si["totalH"].as_i64().unwrap_or(raw.scroll[3].round() as i64);
        let more = si["moreBelow"].as_i64().unwrap_or((max_y - y).max(0));
        let hint = if more > 80 {
            let screens = (more as f64 / view_h as f64).max(0.2);
            match si["containerId"].as_u64() {
                Some(cid) => Some(format!("下方还有约 {screens:.1} 屏未显示（主滚动区 [{cid}]，y={y}/{max_y}，可用 op=\"scroll\" dy=600 向下查看完整表格/明细）")),
                None => Some(format!("下方还有约 {screens:.1} 屏未显示（y={y}/{max_y}，可用 op=\"scroll\" dy=600 向下查看完整内容）")),
            }
        } else {
            None
        };
        (y, total_h, hint)
    } else {
        (raw.scroll[1].round() as i64, raw.scroll[3].round() as i64, None)
    };

    let mut out = json!({
        "tab": tag,
        "snapshotId": snap_id,
        "url": raw.url,
        "title": raw.title,
        "viewport": vp_out,
        "scroll": [scroll_y_out, scroll_total_out],
        "elements": lines.join("\n"),
        "text": raw.text,
        "observeMs": raw.observe_ms,
    });
    if let Some(hint) = scroll_hint {
        out["scrollHint"] = json!(hint);
    }
    if let Some(c) = cursor_img {
        out["cursor"] = c;
    }
    if let Some(f) = raw.focused {
        out["focused"] = f;
    }
    if let Some(ov) = overlay_out {
        out["overlay"] = ov;
    }
    if !images_val.is_empty() {
        out["images"] = json!(images_val);
    }
    if let Some(err) = shot_error {
        out["screenshotError"] = json!(err);
    }
    Ok(out)
}

fn map_point(mapping: Option<&ShotMapping>, px: f64, py: f64) -> Result<(f64, f64), String> {
    if let Some(m) = mapping {
        m.image_to_css(px, py)
    } else if px.is_finite() && py.is_finite() && px >= 0.0 && py >= 0.0 {
        Ok((px, py))
    } else {
        Err("坐标无效".into())
    }
}

fn compact_tabs(tabs: &[TabInfo]) -> Vec<Value> {
    tabs.iter()
        .map(|t| {
            let url: String = if t.url.chars().count() > 120 {
                format!("{}…", t.url.chars().take(120).collect::<String>())
            } else {
                t.url.clone()
            };
            let mut obj = json!({ "tag": t.tag, "title": t.title, "url": url });
            if t.active { obj["active"] = json!(true); }
            if t.incognito { obj["incognito"] = json!(true); }
            if t.loading { obj["loading"] = json!(true); }
            if !t.controllable { obj["controllable"] = json!(false); }
            obj
        })
        .collect()
}

fn merge_object(dst: &mut Value, src: Value) {
    if let (Some(d), Some(s)) = (dst.as_object_mut(), src.as_object()) {
        for (k, v) in s {
            d.insert(k.clone(), v.clone());
        }
    }
}

fn trail_step(items: &[Value], step: &Step) -> Option<String> {
    let find = |id: u32| {
        items.iter().find(|i| i["id"].as_u64() == Some(id as u64)).map(|i| {
            let name = crate::tool_experience::step_name(i["name"].as_str().unwrap_or_default());
            let role = i["role"].as_str().unwrap_or("元素");
            if name.is_empty() { role.to_string() } else { format!("{role}「{name}」") }
        })
    };
    match step {
        Step::Click { id: Some(id), .. } => Some(format!("点击 {}", find(*id)?)),
        Step::Type { id: Some(id), .. } => Some(format!("填写 {}", find(*id)?)),
        Step::Press { key } => Some(format!("按 {key}")),
        Step::Dismiss => Some("点击空白处收起弹层".into()),
        _ => None,
    }
}
