//! Right sidebar browser. Native WebView2 COM/CDP only; no Playwright or debug port.
use crate::AppState;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::Duration,
};
use tauri::{AppHandle, Emitter, Manager};

static APP: OnceLock<AppHandle> = OnceLock::new();
#[cfg(windows)]
struct Download {
    thread: String,
    tab: String,
    id: String,
    started: i64,
    operation: webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2DownloadOperation,
}
// COM download objects stay on the UI apartment; queries marshal only JSON back.
#[cfg(windows)]
thread_local! { static DOWNLOADS: std::cell::RefCell<Vec<Download>> = const { std::cell::RefCell::new(Vec::new()) }; }

async fn downloads(app: &AppHandle, thread: &str, args: &Value) -> Result<Value, String> {
    let id = match args.get("downloadId") {
        Some(value) => Some(value.as_str().filter(|v| !v.is_empty() && v.len() <= 80).ok_or("无效 downloadId")?.to_owned()),
        None => None,
    };
    let since = match args.get("since").filter(|_| id.is_none()) {
        Some(value) => value.as_i64().filter(|v| (0..=8_640_000_000_000_000).contains(v)).ok_or("since 必须为有效的 Unix 毫秒时间戳")?,
        None => chrono::Utc::now().timestamp_millis() - 600_000,
    };
    #[cfg(not(windows))]
    { let _ = (app, thread, since, id); Err("需要 Windows WebView2".into()) }
    #[cfg(windows)]
    {
        use webview2_com::Microsoft::Web::WebView2::Win32::*;
        let thread = thread.to_owned();
        let (tx, rx) = tokio::sync::oneshot::channel();
        app.run_on_main_thread(move || {
            let result = DOWNLOADS.with(|records| -> Result<Value, String> {
                let records = records.borrow();
                let mut items = Vec::new();
                for record in records.iter().rev().filter(|d| d.thread == thread && id.as_ref().map_or(d.started >= since, |id| d.id == *id)).take(100) {
                    let read = || -> windows::core::Result<Value> { unsafe {
                        let op = &record.operation;
                        let mut state = COREWEBVIEW2_DOWNLOAD_STATE::default();
                        let mut reason = COREWEBVIEW2_DOWNLOAD_INTERRUPT_REASON::default();
                        let (mut received, mut total) = (0, 0);
                        op.State(&mut state)?;
                        op.InterruptReason(&mut reason)?;
                        op.BytesReceived(&mut received)?;
                        op.TotalBytesToReceive(&mut total)?;
                        let mut raw = windows::core::PWSTR::null();
                        op.ResultFilePath(&mut raw)?;
                        let path = webview2_com::take_pwstr(raw);
                        op.Uri(&mut raw)?;
                        let full_url = webview2_com::take_pwstr(raw);
                        let url: String = full_url.chars().take(4096).collect();
                        let state = if state == COREWEBVIEW2_DOWNLOAD_STATE_COMPLETED { "complete" }
                            else if state == COREWEBVIEW2_DOWNLOAD_STATE_INTERRUPTED { "interrupted" } else { "in_progress" };
                        Ok(json!({"id":record.id,"tabId":record.tab,"startTime":record.started,
                            "url":url,"urlTruncated":url.len()!=full_url.len(),"path":path,"state":state,"bytesReceived":received,"totalBytes":total,
                            "exists":Path::new(&path).is_file(),
                            "error":if state == "interrupted" {
                                let reasons = ["NONE", "FILE_FAILED", "FILE_ACCESS_DENIED", "FILE_NO_SPACE", "FILE_NAME_TOO_LONG", "FILE_TOO_LARGE", "FILE_MALICIOUS", "FILE_TRANSIENT_ERROR", "FILE_BLOCKED_BY_POLICY", "FILE_SECURITY_CHECK_FAILED", "FILE_TOO_SHORT", "FILE_HASH_MISMATCH", "NETWORK_FAILED", "NETWORK_TIMEOUT", "NETWORK_DISCONNECTED", "NETWORK_SERVER_DOWN", "NETWORK_INVALID_REQUEST", "SERVER_FAILED", "SERVER_NO_RANGE", "SERVER_BAD_CONTENT", "SERVER_UNAUTHORIZED", "SERVER_CERTIFICATE_PROBLEM", "SERVER_FORBIDDEN", "SERVER_UNEXPECTED_RESPONSE", "SERVER_CONTENT_LENGTH_MISMATCH", "SERVER_CROSS_ORIGIN_REDIRECT", "USER_CANCELED", "USER_SHUTDOWN", "USER_PAUSED", "DOWNLOAD_PROCESS_CRASHED"];
                                Some(format!("{} ({})", reasons.get(reason.0 as usize).unwrap_or(&"UNKNOWN"), reason.0))
                            } else { None }}))
                    }};
                    // Closing a tab may invalidate its COM object. Never report that as completion.
                    items.push(read().unwrap_or_else(|e| json!({"id":record.id,"tabId":record.tab,"state":"unknown","error":e.to_string()})));
                }
                Ok(json!({"scope":"session","queriedAt":chrono::Utc::now().timestamp_millis(),"limit":100,"downloads":items,
                    "notice":"仅保留本次应用运行最近 200 条下载。空列表不代表导出失败；页面稳定不代表下载完成。只有 state=complete 才表示完成，原生保存/安全提示仍需处理。"}))
            });
            let _ = tx.send(result);
        }).map_err(|e| e.to_string())?;
        tokio::time::timeout(Duration::from_secs(5), rx).await
            .map_err(|_| "下载查询超时，请处理原生对话框后重新查询")?.map_err(|e| e.to_string())?
    }
}
tokio::task_local! { static CONTROL_TAB: String; }
#[cfg(windows)]
struct Popup {
    args: webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2NewWindowRequestedEventArgs,
    deferral: webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Deferral,
    environment: webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Environment,
}
#[cfg(windows)]
impl Drop for Popup {
    fn drop(&mut self) {
        unsafe {
            let _ = self.deferral.Complete();
        }
    }
}
#[cfg(windows)]
thread_local! { static POPUPS: std::cell::RefCell<std::collections::HashMap<String,Popup>> = std::cell::RefCell::new(std::collections::HashMap::new()); }
const PAGE_SCRIPT: &str = include_str!("native_browser_page.js");

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Session {
    browser_id: String,
    thread_id: String,
    url: String,
    visible: bool,
    busy: bool,
    status: String,
    active_tab: String,
    tabs: Vec<Tab>,
    #[serde(skip)]
    bounds: Option<[f64; 4]>,
    #[serde(skip)]
    cancel: Arc<AtomicBool>,
}

#[derive(Clone, Serialize)]
struct Tab {
    id: String,
    url: String,
    title: String,
}

#[derive(Default)]
struct BrowserState {
    // ponytail: keep live pages until explicit close/app exit; eviction would lose form/JS state.
    session: Mutex<Option<Session>>,
    parked: Mutex<std::collections::HashMap<String, Session>>,
    gate: tokio::sync::Mutex<()>,
    frame_sessions: Mutex<std::collections::HashMap<String, String>>,
    observations: Mutex<std::collections::HashMap<String, Observation>>,
}

fn active_label(app: &AppHandle) -> Result<String, String> {
    if let Ok(label) = CONTROL_TAB.try_with(Clone::clone) {
        return Ok(label);
    }
    app.state::<BrowserState>()
        .session
        .lock()
        .unwrap()
        .as_ref()
        .map(|s| s.active_tab.clone())
        .ok_or("浏览器未创建".into())
}

fn update_session(app: &AppHandle, thread: &str, f: impl FnOnce(&mut Session)) {
    let state = app.state::<BrowserState>();
    let mut current = state.session.lock().unwrap();
    if let Some(s) = current.as_mut().filter(|s| s.thread_id == thread) {
        f(s);
    } else if let Some(s) = state.parked.lock().unwrap().get_mut(thread) {
        f(s);
    }
}

fn show_tab(app: &AppHandle, s: &Session) -> Result<(), String> {
    let view = app.get_webview(&s.active_tab).ok_or("标签页不存在")?;
    if let Some([x, y, w, h]) = s.bounds {
        view.set_position(tauri::LogicalPosition::new(x, y))
            .map_err(|e| e.to_string())?;
        view.set_size(tauri::LogicalSize::new(w, h))
            .map_err(|e| e.to_string())?;
        if s.visible {
            view.show().map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

#[cfg(windows)]
fn create_tab(
    app: &AppHandle,
    thread: &str,
    url: tauri::Url,
    popup_id: Option<String>,
) -> Result<Tab, String> {
    let id = format!("nova-browser-{}", uuid::Uuid::new_v4().simple());
    let tab = Tab {
        id: id.clone(),
        url: url.to_string(),
        title: String::new(),
    };
    let nav_app = app.clone();
    let nav_thread = thread.to_owned();
    let nav_id = id.clone();
    let title_app = app.clone();
    let title_thread = thread.to_owned();
    let title_id = id.clone();
    let mut builder = tauri::WebviewBuilder::new(&id, tauri::WebviewUrl::External(url))
        .data_directory(state_dir(app).join("native-browser-profile"))
        .on_navigation(move |url| {
            if !matches!(url.scheme(), "http" | "https" | "about" | "blob" | "data") {
                return false;
            }
            nav_app
                .state::<BrowserState>()
                .observations
                .lock()
                .unwrap()
                .remove(&nav_id);
            update_session(&nav_app, &nav_thread, |s| {
                if let Some(t) = s.tabs.iter_mut().find(|t| t.id == nav_id) {
                    t.url = url.to_string();
                }
                if s.active_tab == nav_id {
                    s.url = url.to_string();
                }
            });
            emit(&nav_app);
            true
        })
        .on_document_title_changed(move |_, title| {
            update_session(&title_app, &title_thread, |s| {
                if let Some(t) = s.tabs.iter_mut().find(|t| t.id == title_id) {
                    t.title = title.clone();
                }
            });
            emit(&title_app);
        });
    if let Some(key) = popup_id.as_ref() {
        if let Some(environment) =
            POPUPS.with(|p| p.borrow().get(key).map(|p| p.environment.clone()))
        {
            builder = builder.with_environment(environment);
        }
    }
    let view = app
        .get_window("main")
        .ok_or("主窗口不存在")?
        .add_child(
            builder,
            tauri::LogicalPosition::new(0., 0.),
            tauri::LogicalSize::new(1., 1.),
        )
        .map_err(|e| e.to_string())?;
    view.hide().map_err(|e| e.to_string())?;
    let popup_app = app.clone();
    let popup_thread = thread.to_owned();
    let download_thread = thread.to_owned();
    let download_tab = id.clone();
    view.with_webview(move |native| unsafe {
        let Ok(core) = native.controller().CoreWebView2() else {
            return;
        };
        use windows::core::Interface;
        use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2_4;
        let registration = core.cast::<ICoreWebView2_4>().and_then(|core| core.add_DownloadStarting(
            &webview2_com::DownloadStartingEventHandler::create(Box::new(move |_, args| {
                if let Some(args) = args {
                    let operation = args.DownloadOperation()?;
                    DOWNLOADS.with(|records| {
                        let mut records = records.borrow_mut();
                        // ponytail: retain the latest 200 operations; use a persistent history if needed.
                        if records.len() >= 200 { records.remove(0); }
                        records.push(Download { thread: download_thread.clone(), tab: download_tab.clone(),
                            id: uuid::Uuid::new_v4().to_string(), started: chrono::Utc::now().timestamp_millis(), operation });
                    });
                }
                Ok(())
            })), &mut 0));
        if let Err(error) = registration { eprintln!("browser download handler: {error}"); }
        if let Some(key) = popup_id {
            if let Some(popup) = POPUPS.with(|p| p.borrow_mut().remove(&key)) {
                if let Err(e) = popup.args.SetNewWindow(&core) {
                    update_session(&popup_app, &popup_thread, |s| {
                        s.status = format!("新窗口接管失败：{e}")
                    });
                }
                // Drop completes the deferral on this same UI apartment.
            }
        }
        let environment = native.environment();
        let mut token = 0;
        let registration = core.add_NewWindowRequested(
            &webview2_com::NewWindowRequestedEventHandler::create(Box::new(move |_, args| {
                let Some(args) = args else {
                    return Ok(());
                };
                args.SetHandled(true)?;
                let mut raw = windows::core::PWSTR::null();
                args.Uri(&mut raw)?;
                let url = webview2_com::take_pwstr(raw);
                if !tauri::Url::parse(&url).is_ok_and(|u| {
                    matches!(u.scheme(), "http" | "https" | "blob" | "data") || u.as_str() == "about:blank"
                }) {
                    return Ok(());
                }
                let key = uuid::Uuid::new_v4().to_string();
                let deferral = args.GetDeferral()?;
                POPUPS.with(|p| {
                    p.borrow_mut().insert(
                        key.clone(),
                        Popup {
                            args: args.clone(),
                            deferral,
                            environment: environment.clone(),
                        },
                    )
                });
                let app = popup_app.clone();
                let thread = popup_thread.clone();
                // Leave the COM event before constructing a child WebView (WebView2 disallows reentrancy).
                tauri::async_runtime::spawn(async move {
                    if let Err(error) =
                        change_tab(&app, &thread, "new_tab", json!({"popupId":key})).await
                    {
                        update_session(&app, &thread, |s| s.status = error);
                        emit(&app);
                        let _ = app.run_on_main_thread(move || {
                            POPUPS.with(|p| p.borrow_mut().remove(&key));
                        });
                    }
                });
                Ok(())
            })),
            &mut token,
        );
        if let Err(e) = registration {
            eprintln!("browser popup handler: {e}");
        }
    })
    .map_err(|e| e.to_string())?;
    Ok(tab)
}

async fn change_tab(
    app: &AppHandle,
    thread: &str,
    operation: &str,
    args: Value,
) -> Result<Value, String> {
    #[cfg(not(windows))]
    {
        let _ = (app, thread, operation, args);
        return Err("需要 Windows WebView2".into());
    }
    #[cfg(windows)]
    {
        let app2 = app.clone();
        let thread = thread.to_string();
        let op = operation.to_string();
        let (tx, rx) = tokio::sync::oneshot::channel();
        app.run_on_main_thread(move || {
            let result = (|| {
                let state = app2.state::<BrowserState>();
                let mut s = state
                    .session
                    .lock()
                    .unwrap()
                    .as_ref()
                    .filter(|s| s.thread_id == thread)
                    .cloned()
                    .or_else(|| state.parked.lock().unwrap().get(&thread).cloned())
                    .ok_or("会话浏览器不存在")?;
                let target = args["tabId"].as_str().unwrap_or(&s.active_tab).to_owned();
                if op != "new_tab" && !s.tabs.iter().any(|t| t.id == target) {
                    return Err("标签页不存在".to_string());
                }
                s.cancel.store(true, Ordering::SeqCst);
                s.busy = false;
                if let Some(v) = app2.get_webview(&s.active_tab) {
                    v.hide().map_err(|e| e.to_string())?;
                }
                if op == "new_tab" || (op == "close_tab" && s.tabs.len() == 1) {
                    let url = if let Some(url) = args["url"].as_str() {
                        normalized_url(url)?
                    } else {
                        "about:blank".parse().unwrap()
                    };
                    let tab = create_tab(
                        &app2,
                        &thread,
                        url,
                        args["popupId"].as_str().map(str::to_owned),
                    )?;
                    s.active_tab = tab.id.clone();
                    s.tabs.push(tab);
                } else if op == "select_tab" {
                    s.active_tab = target.clone();
                }
                if op == "close_tab" {
                    if let Some(v) = app2.get_webview(&target) {
                        v.close().map_err(|e| e.to_string())?;
                    }
                    s.tabs.retain(|t| t.id != target);
                    state.observations.lock().unwrap().remove(&target);
                    if s.active_tab == target {
                        s.active_tab = s.tabs.last().unwrap().id.clone();
                    }
                }
                s.url = s
                    .tabs
                    .iter()
                    .find(|t| t.id == s.active_tab)
                    .unwrap()
                    .url
                    .clone();
                s.status = "就绪".into();
                show_tab(&app2, &s)?;
                update_session(&app2, &thread, |current| *current = s.clone());
                emit(&app2);
                Ok(json!(s))
            })();
            let _ = tx.send(result);
        })
        .map_err(|e| e.to_string())?;
        rx.await.map_err(|e| e.to_string())?
    }
}

pub(crate) fn init(app: &AppHandle) {
    app.manage(BrowserState::default());
    crate::chrome_browser::init(app);
    let _ = APP.set(app.clone());
}

pub(crate) fn tool_definition() -> Value {
    serde_json::from_str(include_str!("../../scripts/webview-tool.json"))
        .expect("webview tool schema")
}

fn emit(app: &AppHandle) {
    let session = app.state::<BrowserState>().session.lock().unwrap().clone();
    let _ = app.emit_to("main", "native-browser:state", session);
}

fn session(app: &AppHandle, id: &str) -> Result<Session, String> {
    app.state::<BrowserState>()
        .session
        .lock()
        .unwrap()
        .as_ref()
        .filter(|s| s.browser_id == id)
        .cloned()
        .ok_or("浏览器已关闭或属于其它会话，请重新打开".into())
}

fn check(app: &AppHandle, current: &Session) -> Result<(), String> {
    if current.cancel.load(Ordering::SeqCst) {
        return Err("已停止浏览器控制".into());
    }
    // Chrome targets an explicitly authorized tab, independently of Nova's visible thread.
    if current.browser_id == "chrome" {
        return Ok(());
    }
    let active = app
        .state::<AppState>()
        .active_thread
        .lock()
        .unwrap()
        .clone();
    let latest = session(app, &current.browser_id)?;
    if !latest.visible || active.as_deref() != Some(&current.thread_id) {
        return Err("浏览器不在当前可见会话中，已停止操作".into());
    }
    Ok(())
}

fn normalized_url(raw: &str) -> Result<tauri::Url, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("请输入网址".into());
    }
    let value = if raw.contains("://") {
        raw.to_string()
    } else if raw.starts_with("localhost")
        || raw.starts_with("127.0.0.1")
        || raw.starts_with("[::1]")
    {
        format!("http://{raw}")
    } else {
        format!("https://{raw}")
    };
    let url = tauri::Url::parse(&value).map_err(|_| "网址无效")?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("仅支持不含凭据的 HTTP(S) 网址".into());
    }
    Ok(url)
}

async fn mount(app: &AppHandle, thread_id: String) -> Result<Value, String> {
    #[cfg(not(windows))]
    return Err("原生浏览器控制当前支持 Windows WebView2".into());
    #[cfg(windows)]
    {
        if app
            .state::<AppState>()
            .store
            .lock()
            .unwrap()
            .get(&thread_id)
            .is_none()
        {
            return Err("会话不存在".into());
        }
        let (tx, rx) = tokio::sync::oneshot::channel();
        let handle = app.clone();
        app.run_on_main_thread(move || {
            let result = (|| -> Result<Value, String> {
                let state = handle.state::<BrowserState>();
                if let Some(s) = state
                    .session
                    .lock()
                    .unwrap()
                    .as_ref()
                    .filter(|s| s.thread_id == thread_id)
                {
                    return Ok(json!(s));
                }
                if let Some(mut old) = state.session.lock().unwrap().take() {
                    old.cancel.store(true, Ordering::SeqCst);
                    old.busy = false;
                    old.visible = false;
                    if let Some(view) = handle.get_webview(&old.active_tab) {
                        view.hide().map_err(|e| e.to_string())?;
                    }
                    state
                        .parked
                        .lock()
                        .unwrap()
                        .insert(old.thread_id.clone(), old);
                }
                let restored = state.parked.lock().unwrap().remove(&thread_id);
                let s = if let Some(mut s) = restored {
                    s.visible = false;
                    s
                } else {
                    let tab =
                        create_tab(&handle, &thread_id, "about:blank".parse().unwrap(), None)?;
                    Session {
                        browser_id: uuid::Uuid::new_v4().to_string(),
                        thread_id,
                        url: tab.url.clone(),
                        active_tab: tab.id.clone(),
                        tabs: vec![tab],
                        bounds: None,
                        visible: false,
                        busy: false,
                        status: "就绪".into(),
                        cancel: Arc::new(AtomicBool::new(false)),
                    }
                };
                *state.session.lock().unwrap() = Some(s.clone());
                Ok(json!(s))
            })();
            let _ = tx.send(result);
        })
        .map_err(|e| e.to_string())?;
        rx.await.map_err(|e| e.to_string())?
    }
}

#[tauri::command]
pub async fn native_browser_ui(
    app: AppHandle,
    webview: tauri::Webview,
    thread_id: String,
    operation: String,
    args: Value,
) -> Result<Value, String> {
    if webview.label() != "main" {
        return Err("仅 Nova 主界面可以控制浏览器".into());
    }
    if operation == "mount" {
        return mount(&app, thread_id).await;
    }
    if matches!(operation.as_str(), "new_tab" | "select_tab" | "close_tab") {
        return change_tab(&app, &thread_id, &operation, args).await;
    }
    let state = app.state::<BrowserState>();
    let s = state
        .session
        .lock()
        .unwrap()
        .as_ref()
        .filter(|s| s.thread_id == thread_id)
        .cloned()
        .ok_or("请先打开浏览器标签页")?;
    match operation.as_str() {
        "layout" => {
            if args["tabId"].as_str().is_some_and(|id| id != s.active_tab) {
                return Ok(Value::Null);
            }
            let visible = args["visible"] == true;
            let view = app.get_webview(&s.active_tab).ok_or("浏览器未创建")?;
            if visible {
                let n = |key: &str| {
                    args[key]
                        .as_f64()
                        .filter(|n| n.is_finite() && *n >= 0.0 && *n <= 32768.0)
                        .ok_or("无效浏览器布局")
                };
                let (x, y, width, height) = (n("x")?, n("y")?, n("width")?, n("height")?);
                if width < 1.0 || height < 1.0 {
                    return Err("浏览器区域过小".into());
                }
                view.set_position(tauri::LogicalPosition::new(x, y))
                    .map_err(|e| e.to_string())?;
                view.set_size(tauri::LogicalSize::new(width, height))
                    .map_err(|e| e.to_string())?;
                view.show().map_err(|e| e.to_string())?;
                update_session(&app, &thread_id, |s| s.bounds = Some([x, y, width, height]));
            } else {
                s.cancel.store(true, Ordering::SeqCst);
                view.hide().map_err(|e| e.to_string())?;
            }
            if let Some(current) = state.session.lock().unwrap().as_mut() {
                current.visible = visible;
            }
            Ok(Value::Null)
        }
        "stop" => {
            s.cancel.store(true, Ordering::SeqCst);
            state.observations.lock().unwrap().remove(&s.active_tab);
            Ok(json!({"stopped":true}))
        }
        "status" => Ok(json!(s)),
        "downloads" => downloads(&app, &thread_id, &args).await,
        "goto" | "back" | "forward" | "reload" => {
            let _guard = state
                .gate
                .try_lock()
                .map_err(|_| "浏览器正在执行动作，请先停止")?;
            let view = app.get_webview(&s.active_tab).ok_or("浏览器未创建")?;
            if operation == "goto" {
                view.navigate(normalized_url(args["url"].as_str().unwrap_or_default())?)
                    .map_err(|e| e.to_string())?;
            } else {
                view.eval(match operation.as_str() {
                    "back" => "history.back()",
                    "forward" => "history.forward()",
                    _ => "location.reload()",
                })
                .map_err(|e| e.to_string())?;
            }
            Ok(Value::Null)
        }
        "inspect" | "screenshot" | "act" => control(&app, &s.browser_id, &operation, &args).await,
        _ => Err("未知浏览器操作".into()),
    }
}

async fn cdp(
    app: &AppHandle,
    method: &str,
    params: Value,
    session_id: Option<&str>,
    cancel: Option<Arc<AtomicBool>>,
) -> Result<Value, String> {
    if cancel
        .as_ref()
        .is_some_and(|flag| flag.load(Ordering::SeqCst))
    {
        return Err("已停止".into());
    }
    let label = active_label(app)?;
    if label.starts_with("chrome:") {
        let tag = label.rsplit(':').next().ok_or("缺少 Chrome tabTag")?;
        return crate::chrome_browser::request(
            app,
            "cdp",
            json!({"tabTag":tag,"method":method,"params":params,"sessionId":session_id}),
        )
        .await;
    }
    native_cdp(app, method, params, session_id, cancel).await
}

#[cfg(windows)]
async fn native_cdp(
    app: &AppHandle,
    method: &str,
    params: Value,
    session_id: Option<&str>,
    cancel: Option<Arc<AtomicBool>>,
) -> Result<Value, String> {
    use webview2_com::{
        CallDevToolsProtocolMethodCompletedHandler,
        Microsoft::Web::WebView2::Win32::ICoreWebView2_11,
    };
    use windows::core::{Interface, HSTRING};
    let view = app.get_webview(&active_label(app)?).ok_or("浏览器未创建")?;
    let method = method.to_string();
    let params = params.to_string();
    let session_id = session_id.map(str::to_owned);
    let (tx, rx) = tokio::sync::oneshot::channel();
    view.with_webview(move |native| unsafe {
        if cancel.is_some_and(|flag| flag.load(Ordering::SeqCst)) {
            let _ = tx.send(Err("已停止".into()));
            return;
        }
        let sender = Arc::new(Mutex::new(Some(tx)));
        let callback_sender = sender.clone();
        let callback =
            CallDevToolsProtocolMethodCompletedHandler::create(Box::new(move |status, body| {
                if let Some(tx) = callback_sender.lock().unwrap().take() {
                    let _ = tx.send(
                        status
                            .map_err(|e| e.to_string())
                            .and_then(|_| serde_json::from_str(&body).map_err(|e| e.to_string())),
                    );
                }
                Ok(())
            }));
        let result = native.controller().CoreWebView2().and_then(|core| {
            if let Some(session_id) = session_id {
                core.cast::<ICoreWebView2_11>()?
                    .CallDevToolsProtocolMethodForSession(
                        &HSTRING::from(session_id),
                        &HSTRING::from(method),
                        &HSTRING::from(params),
                        &callback,
                    )
            } else {
                core.CallDevToolsProtocolMethod(
                    &HSTRING::from(method),
                    &HSTRING::from(params),
                    &callback,
                )
            }
        });
        if let Err(error) = result {
            if let Some(tx) = sender.lock().unwrap().take() {
                let _ = tx.send(Err(error.to_string()));
            }
        }
    })
    .map_err(|e| e.to_string())?;
    let value: Value = tokio::time::timeout(Duration::from_secs(3), rx)
        .await
        .map_err(|_| "网页响应超时")?
        .map_err(|e| e.to_string())??;
    if value.get("error").is_some() {
        return Err(value["error"].to_string());
    }
    Ok(value)
}

#[cfg(not(windows))]
async fn native_cdp(
    _: &AppHandle,
    _: &str,
    _: Value,
    _: Option<&str>,
    _: Option<Arc<AtomicBool>>,
) -> Result<Value, String> {
    Err("需要 Windows WebView2".into())
}

#[derive(Clone)]
struct Frame {
    id: String,
    parent: Option<String>,
    session: Option<String>,
    context: i64,
}

async fn evaluate(app: &AppHandle, frame: &Frame, expression: String) -> Result<Value, String> {
    let result = cdp(app, "Runtime.evaluate", json!({"expression":expression,"contextId":frame.context,"returnByValue":true,"awaitPromise":true}), frame.session.as_deref(), None).await?;
    if result.get("exceptionDetails").is_some() {
        return Err(result["exceptionDetails"]["exception"]["description"]
            .as_str()
            .unwrap_or("页面脚本执行失败")
            .into());
    }
    Ok(result["result"]["value"].clone())
}

#[derive(Clone)]
struct Observation {
    frames: Vec<Frame>,
    pages: Value,
    id: String,
    captured: std::time::Instant,
    screenshot: bool,
    full_page: bool,
    images: Vec<ScreenshotImage>,
    // A real run handoff permits one act on this owner/tab/snapshot only.
    jev_fallback: bool,
}

#[derive(Clone)]
struct ScreenshotImage {
    id: String,
    path: std::path::PathBuf,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    pixels: (u32, u32),
}
impl ScreenshotImage {
    fn point(&self, x: f64, y: f64) -> Result<(f64, f64), String> {
        if !x.is_finite() || !y.is_finite() || x < 0. || y < 0.
            || x >= self.pixels.0 as f64 || y >= self.pixels.1 as f64 {
            return Err("图片坐标越界，请使用该 imageId 的 pixelWidth/pixelHeight".into());
        }
        Ok((self.x + x * self.width / self.pixels.0 as f64,
            self.y + y * self.height / self.pixels.1 as f64))
    }
}
fn png_dimensions(bytes: &[u8]) -> Result<(u32, u32), String> {
    if bytes.len() < 24 || &bytes[..8] != b"\x89PNG\r\n\x1a\n" || &bytes[12..16] != b"IHDR" {
        return Err("截图不是有效的 PNG".into());
    }
    let w = u32::from_be_bytes(bytes[16..20].try_into().unwrap());
    let h = u32::from_be_bytes(bytes[20..24].try_into().unwrap());
    if w == 0 || h == 0 || w as u64 * h as u64 > 64_000_000 { return Err("截图像素超出安全预算，请裁剪或缩小截图".into()); }
    Ok((w, h))
}

fn screenshot_bytes(data:&str, edge:u32) -> Result<(Vec<u8>,(u32,u32)),String> {
    use base64::Engine;
    let bytes=base64::engine::general_purpose::STANDARD.decode(data).map_err(|e|e.to_string())?;
    let pixels=png_dimensions(&bytes)?;
    if edge==0 || pixels.0.max(pixels.1)<=edge {return Ok((bytes,pixels));}
    // CDP implementations do not all produce the same pixel dimensions for
    // clip.scale=1. Enforce the budget against the actual PNG, not guessed DPR.
    encode_screenshot(xcap::image::load_from_memory(&bytes).map_err(|e|e.to_string())?,edge)
}

fn encode_screenshot(image:xcap::image::DynamicImage, edge:u32) -> Result<(Vec<u8>,(u32,u32)),String> {
    let image=if edge>0 && image.width().max(image.height())>edge {
        image.resize(edge,edge,xcap::image::imageops::FilterType::Triangle)
    } else {image};
    let pixels=(image.width(),image.height());
    let mut output=std::io::Cursor::new(Vec::new());
    image.write_to(&mut output,xcap::image::ImageFormat::Png).map_err(|e|e.to_string())?;
    Ok((output.into_inner(),pixels))
}

async fn capture_viewport(app:&AppHandle, viewport:&Value, x:f64, y:f64, w:f64, h:f64) -> Result<xcap::image::DynamicImage,String> {
    use base64::Engine;
    // No CDP clip/scale: capture the existing surface without a temporary render
    // size/scale override. Crop and downsample locally, also for pixel preflight.
    let shot=cdp(app,"Page.captureScreenshot",json!({"format":"png","fromSurface":true,"captureBeyondViewport":false}),None,None).await?;
    let bytes=base64::engine::general_purpose::STANDARD.decode(shot["data"].as_str().ok_or("截图为空")?).map_err(|e|e.to_string())?;
    let pixels=png_dimensions(&bytes)?;
    let vw=viewport["width"].as_f64().filter(|v|*v>0.).ok_or("无效视口")?;
    let vh=viewport["height"].as_f64().filter(|v|*v>0.).ok_or("无效视口")?;
    let (sx,sy)=(pixels.0 as f64/vw,pixels.1 as f64/vh);
    let left=(x*sx).round().clamp(0.,pixels.0 as f64) as u32;
    let top=(y*sy).round().clamp(0.,pixels.1 as f64) as u32;
    let right=((x+w)*sx).round().clamp(0.,pixels.0 as f64) as u32;
    let bottom=((y+h)*sy).round().clamp(0.,pixels.1 as f64) as u32;
    if right<=left || bottom<=top {return Err("截图范围为空".into());}
    Ok(xcap::image::load_from_memory(&bytes).map_err(|e|e.to_string())?.crop_imm(left,top,right-left,bottom-top))
}

async fn observe(app: &AppHandle, scope: &str) -> Result<Observation, String> {
    fn collect(tree: &Value, session: Option<String>, frames: &mut Vec<Frame>) {
        if let Some(id) = tree["frame"]["id"].as_str() {
            frames.push(Frame {
                id: id.into(),
                parent: tree["frame"]["parentId"].as_str().map(str::to_owned),
                session: session.clone(),
                context: 0,
            });
        }
        if let Some(children) = tree["childFrames"].as_array() {
            for child in children {
                collect(child, session.clone(), frames);
            }
        }
    }
    let tree = cdp(app, "Page.getFrameTree", json!({}), None, None).await?;
    let mut frames = Vec::new();
    collect(&tree["frameTree"], None, &mut frames);
    let targets = cdp(app, "Target.getTargets", json!({}), None, None).await?;
    let mut gaps = Vec::new();
    if let Some(targets) = targets["targetInfos"].as_array() {
        app.state::<BrowserState>()
            .frame_sessions
            .lock()
            .unwrap()
            .retain(|key, _| {
                targets
                    .iter()
                    .any(|t| t["targetId"].as_str() == key.rsplit(':').next())
            });
        for target in targets.iter().filter(|t| t["type"] == "iframe").take(12) {
            let id = target["targetId"].as_str().ok_or("框架没有 ID")?;
            let cache_key = format!("{}:{id}", active_label(app)?);
            let cached = app
                .state::<BrowserState>()
                .frame_sessions
                .lock()
                .unwrap()
                .get(&cache_key)
                .cloned();
            let sid = if let Some(sid) = cached {
                sid
            } else {
                let attached = cdp(
                    app,
                    "Target.attachToTarget",
                    json!({"targetId":id,"flatten":true}),
                    None,
                    None,
                )
                .await?;
                let sid = attached["sessionId"]
                    .as_str()
                    .ok_or("无法连接框架")?
                    .to_string();
                app.state::<BrowserState>()
                    .frame_sessions
                    .lock()
                    .unwrap()
                    .insert(cache_key.clone(), sid.clone());
                sid
            };
            match cdp(app, "Page.getFrameTree", json!({}), Some(&sid), None).await {
                Ok(tree) => {
                    let mut candidates = Vec::new();
                    collect(&tree["frameTree"], Some(sid), &mut candidates);
                    // getTargets includes background tabs. Only this page's descendants belong in its observation.
                    if candidates.first().is_some_and(|child| {
                        child
                            .parent
                            .as_ref()
                            .is_some_and(|parent| frames.iter().any(|f| &f.id == parent))
                            || frames.iter().any(|f| f.id == child.id)
                    }) {
                        for child in candidates {
                            frames.retain(|f| f.id != child.id);
                            frames.push(child);
                        }
                    }
                }
                Err(e) => {
                    app.state::<BrowserState>()
                        .frame_sessions
                        .lock()
                        .unwrap()
                        .remove(&cache_key);
                    gaps.push(e);
                }
            }
        }
    }
    let mut pages = Vec::new();
    let mut ready = Vec::new();
    if frames.len() > 12 {
        gaps.push("框架超过12个，本次只读取前12个，不能视为完整页面".into());
    }
    for mut frame in frames.into_iter().take(12) {
        let result = async {
            let world = cdp(
                app,
                "Page.createIsolatedWorld",
                json!({"frameId":frame.id,"worldName":"nova-browser-control"}),
                frame.session.as_deref(),
                None,
            )
            .await?;
            frame.context = world["executionContextId"]
                .as_i64()
                .ok_or("无效页面上下文")?;
            let nonce = uuid::Uuid::new_v4().simple().to_string();
            evaluate(
                app,
                &frame,
                format!("{PAGE_SCRIPT}; __novaWebview.observe({},20000,{})", json!(nonce), json!(scope)),
            )
            .await
        }
        .await;
        match result {
            Ok(mut page) => {
                page["frame"] = json!(ready.len());
                pages.push(page);
                ready.push(frame);
            }
            Err(error) if frame.parent.is_none() && frame.session.is_none() => return Err(format!("主页面尚未就绪：{error}")),
            Err(error) => gaps.push(error),
        }
    }
    if ready.is_empty() {
        return Err("页面尚未就绪，请稍后重新观察".into());
    }
    Ok(Observation {
        frames: ready,
        pages: json!({"pages":pages,"coverageGaps":gaps,"domCoordinates":"item.point is frame-local viewport coordinates; item.documentRect is frame-local document coordinates. Prefer frame/ref for iframe targets."}),
        id: uuid::Uuid::new_v4().to_string(),
        captured: std::time::Instant::now(),
        screenshot: false,
        full_page: false,
        images: Vec::new(),
        jev_fallback: false,
    })
}

#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    ClickAt {
        x: f64,
        y: f64,
        #[serde(default)] button: Option<String>,
        #[serde(default)] click_count: Option<u8>,
    },
    Move {
        x: f64,
        y: f64,
    },
    Drag {
        x: f64,
        y: f64,
        to_x: f64,
        to_y: f64,
        #[serde(default)] duration_ms: Option<u64>,
    },
    Type {
        text: String,
    },
    ScrollAt {
        x: f64,
        y: f64,
        delta: i32,
        #[serde(default)] delta_x: Option<i32>,
    },
    Click {
        frame: usize,
        r#ref: String,
        #[serde(default)] button: Option<String>,
        #[serde(default)] click_count: Option<u8>,
    },
    Fill {
        frame: usize,
        r#ref: String,
        text: String,
    },
    Press {
        key: String,
    },
    Scroll {
        frame: usize,
        r#ref: Option<String>,
        delta: i32,
        #[serde(default)] delta_x: Option<i32>,
    },
    Wait {
        ms: u64,
    },
}

fn parse_action(text: &str) -> Result<Action, String> {
    let text = text.trim();
    let text = text
        .strip_prefix("```json")
        .or_else(|| text.strip_prefix("```"))
        .unwrap_or(text)
        .trim();
    let text = text.strip_suffix("```").unwrap_or(text).trim();
    serde_json::from_str(text).map_err(|e| format!("动作 JSON 无效：{e}。DOM 点击用 action={{\"action\":\"click\",\"frame\":0,\"ref\":\"最新引用\"}}；图片坐标点击用 action={{\"action\":\"click_at\",\"x\":100,\"y\":100}}，imageId 放在工具顶层且与 snapshotId 来自同次截图；按键仅用 action={{\"action\":\"press\",\"key\":\"Enter\"}}，不传 frame/ref。"))
}

fn validate_action(action: &Action) -> Result<(), String> {
    match action {
        Action::Click { button, click_count, .. } | Action::ClickAt { button, click_count, .. } => {
            if button.as_deref().is_some_and(|b| !matches!(b, "left"|"right"|"middle")) || click_count.is_some_and(|n| !(1..=2).contains(&n)) { return Err("button 或 click_count 无效".into()); }
        }
        Action::Type { text } | Action::Fill { text, .. } if text.len() > 16000 => return Err("输入过长".into()),
        Action::Wait { ms } if *ms > 2000 => return Err("wait.ms 必须为 0–2000".into()),
        Action::Scroll { delta, .. } | Action::ScrollAt { delta, .. } if delta.unsigned_abs() > 1200 => return Err("单次滚动不能超过1200像素".into()),
        Action::Press { key } if key_spec(key).is_none() => return Err("不支持的按键".into()),
        _ => (),
    }
    match action {
        Action::ClickAt { x,y,.. } | Action::Move { x,y } | Action::ScrollAt { x,y,.. } | Action::Drag { x,y,.. }
            if !x.is_finite() || !y.is_finite() || *x<0. || *y<0. => return Err("坐标无效".into()),
        _ => (),
    }
    if let Action::Drag { to_x,to_y,duration_ms,.. } = action {
        if !to_x.is_finite() || !to_y.is_finite() || *to_x<0. || *to_y<0. || duration_ms.is_some_and(|t| !(80..=1500).contains(&t)) { return Err("拖动终点或 duration_ms 无效".into()); }
    }
    if let Action::ScrollAt { delta_x:Some(x), .. } | Action::Scroll { delta_x:Some(x), .. } = action { if x.unsigned_abs()>1200 { return Err("delta_x 超过1200像素".into()); } }
    Ok(())
}
fn parse_actions(args: &Value, max_actions: usize) -> Result<Vec<Action>, String> {
    if !args["action"].is_null() && !args["actions"].is_null() { return Err("action 和 actions 不能同时提供：单步仅传 action，多步仅传 actions；删除另一个字段。本批次未执行。".into()); }
    let values = if let Some(values)=args["actions"].as_array() { values.clone() } else { vec![args["action"].clone()] };
    if values.is_empty() || values.len()>max_actions { return Err(format!("每批需要1–{max_actions}个确定动作")); }
    values.into_iter().map(|value| { let action=parse_action(&value.to_string())?; validate_action(&action)?; Ok(action) }).collect()
}

fn preflight(observation:&Observation, action:&Action, image_id:Option<&str>) -> Result<(),String> {
    match action {
        Action::Click{frame,r#ref,..}|Action::Fill{frame,r#ref,..}|Action::Scroll{frame,r#ref:Some(r#ref),..}=>{
            if *frame>=observation.frames.len() {return Err("frame 不属于当前快照".into());}
            if !observation.pages["pages"][*frame]["items"].as_array().is_some_and(|items|items.iter().any(|item|item["ref"].as_str()==Some(r#ref.as_str()))) {
                return Err("ref 不属于当前快照的指定 frame；ref 必须原样复制该 frame 的 items[].ref，不能用 snapshotId 加序号拼造。这不等于快照过期；先核对已有观察中的真实 ref，不要直接重试或无故重新截图".into());
            }
        }
        Action::Scroll{frame,..} if *frame!=0=>return Err("子框架滚动需要明确 ref".into()),
        Action::ClickAt{x,y,..}|Action::Move{x,y}|Action::Drag{x,y,..}|Action::ScrollAt{x,y,..}=>{
            if !observation.screenshot {return Err(format!("坐标操作需要截图：当前 snapshotId={} 没有图片。调用 screenshot 后同时使用它返回的新 snapshotId 和 imageId；不要将旧 imageId 与新 DOM 快照混用。", observation.id));}
            let (cx,cy)=image_point(observation,*x,*y,image_id)?;
            if !observation.images.iter().any(|image|cx>=image.x&&cy>=image.y&&cx<image.x+image.width&&cy<image.y+image.height) {return Err("坐标不在返回的图片中".into());}
            if let Action::Drag{to_x,to_y,..}=action {
                if observation.full_page {return Err("拖动需要 fullPage=false 的视口截图".into());}
                let (tx,ty)=image_point(observation,*to_x,*to_y,image_id)?;
                if !observation.images.iter().any(|image|tx>=image.x&&ty>=image.y&&tx<image.x+image.width&&ty<image.y+image.height) {return Err("拖动终点不在图片中".into());}
            }
        }
        _=>(),
    }
    Ok(())
}

fn jev_requires_run(enabled: bool, delegated: bool, observation: &Observation, actions: &[Action]) -> bool {
    enabled && !delegated
        && actions.iter().any(|a| matches!(a, Action::Click{..} | Action::Fill{..} | Action::Scroll{..}))
        && !(observation.jev_fallback && actions.len() == 1 && observation.captured.elapsed() <= Duration::from_secs(180))
}

async fn point(
    app: &AppHandle,
    observation: &Observation,
    index: usize,
    reference: &str,
    mode: &str,
) -> Result<Value, String> {
    let frame = observation
        .frames
        .get(index)
        .ok_or("模型选择了不存在的 frame")?;
    // Scroll ancestor frames before measuring the final point.
    let mut chain = Vec::new();
    let mut child = frame;
    while let Some(parent_id) = &child.parent {
        let parent = observation
            .frames
            .iter()
            .find(|f| &f.id == parent_id)
            .ok_or("父框架不可访问")?;
        let owner = cdp(
            app,
            "DOM.getFrameOwner",
            json!({"frameId":child.id}),
            parent.session.as_deref(),
            None,
        )
        .await?;
        let node = cdp(
            app,
            "DOM.resolveNode",
            json!({"backendNodeId":owner["backendNodeId"],"executionContextId":parent.context}),
            parent.session.as_deref(),
            None,
        )
        .await?;
        let object = node["object"]["objectId"]
            .as_str()
            .ok_or("框架元素不可访问")?
            .to_owned();
        chain.push((parent, object));
        child = parent;
        if chain.len() > 12 {
            return Err("框架嵌套过深".into());
        }
    }
    for (parent, object) in chain.iter().rev() {
        cdp(app, "Runtime.callFunctionOn", json!({"objectId":object,"functionDeclaration":"async function(){const r=this.getBoundingClientRect();if(r.bottom<=0||r.top>=innerHeight||r.right<=0||r.left>=innerWidth){this.scrollIntoView({block:'nearest',inline:'nearest',behavior:'instant'});await new Promise(r=>setTimeout(r,50));}}","awaitPromise":true}), parent.session.as_deref(), None).await?;
    }
    let mut value = evaluate(
        app,
        frame,
        format!("__novaWebview.prepare({},{})", json!(reference), json!(mode)),
    )
    .await?;
    value["localRect"] = value["rect"].clone();
    for (parent, object) in &chain {
        let result = cdp(app, "Runtime.callFunctionOn", json!({"objectId":object,"returnByValue":true,"arguments":[{"value":value}],"functionDeclaration":"function(p){const r=this.getBoundingClientRect();for(let e=this;e;e=e.parentElement||e.getRootNode()?.host){const t=getComputedStyle(e).transform;if(t!=='none'){const m=new DOMMatrixReadOnly(t);if(!m.is2D||m.a<=0||m.d<=0||Math.abs(m.b)>.00001||Math.abs(m.c)>.00001)throw Error('框架旋转/透视不能安全映射');}}const sx=r.width/this.offsetWidth,sy=r.height/this.offsetHeight,x=r.x+(this.clientLeft+p.x)*sx,y=r.y+(this.clientTop+p.y)*sy;let hit=this.ownerDocument.elementFromPoint(x,y);while(hit?.shadowRoot){const next=hit.shadowRoot.elementFromPoint(x,y);if(!next||next===hit)break;hit=next;}if(hit!==this)throw Error('框架被遮挡');return {x,y,rect:{x:r.x+(this.clientLeft+p.rect.x)*sx,y:r.y+(this.clientTop+p.rect.y)*sy,width:p.rect.width*sx,height:p.rect.height*sy}}}"}), parent.session.as_deref(), None).await?;
        if result.get("exceptionDetails").is_some() {
            return Err("框架坐标无法安全映射".into());
        }
        value["rect"] = result["result"]["value"]["rect"].clone();
        value["x"] = json!(result["result"]["value"]["x"]
            .as_f64()
            .ok_or("无效框架坐标")?);
        value["y"] = json!(result["result"]["value"]["y"]
            .as_f64()
            .ok_or("无效框架坐标")?);
    }
    for (parent, object) in &chain {
        let _ = cdp(app, "Runtime.releaseObject", json!({"objectId":object}), parent.session.as_deref(), None).await;
    }
    Ok(value)
}

// Retained outside the cancellable future. Timeouts must release held inputs,
// report uncertainty, and must never make a possibly executed click retryable.
#[derive(Default)]
struct InputProgress {
    attempted: bool,
    dom_preflight: bool,
    scroll_feedback: Option<Value>,
    completed: usize,
    held_mouse: Option<Value>,
    held_key: Option<Value>,
}

async fn mouse(app: &AppHandle, s: &Session, p: &Value, button: &str, count: u8, progress: &mut InputProgress) -> Result<(), String> {
    check(app, s)?;
    for n in 1..=count {
        check(app,s)?;
        progress.attempted = true;
        progress.held_mouse=Some(json!({"type":"mouseReleased","x":p["x"],"y":p["y"],"button":button,"clickCount":n}));
        let pressed = cdp(app,"Input.dispatchMouseEvent",json!({"type":"mousePressed","x":p["x"],"y":p["y"],"button":button,"clickCount":n}),None,Some(s.cancel.clone())).await;
        // Release even when a pressed reply failed; never retry the press.
        let released = cdp(app,"Input.dispatchMouseEvent",json!({"type":"mouseReleased","x":p["x"],"y":p["y"],"button":button,"clickCount":n}),None,None).await;
        if released.is_ok() {progress.held_mouse=None;}
        pressed?; released?;
    }
    Ok(())
}
fn key_spec(name: &str) -> Option<(&str,i32,i32)> {
    Some(match name {
        "Enter"=>("Enter",13,0),"Tab"=>("Tab",9,0),"Escape"=>("Escape",27,0),"Backspace"=>("Backspace",8,0),
        "ArrowDown"=>("ArrowDown",40,0),"ArrowUp"=>("ArrowUp",38,0),"ArrowLeft"=>("ArrowLeft",37,0),"ArrowRight"=>("ArrowRight",39,0),
        "Delete"=>("Delete",46,0),"Home"=>("Home",36,0),"End"=>("End",35,0),"PageDown"=>("PageDown",34,0),"PageUp"=>("PageUp",33,0),
        "Shift+Tab"=>("Tab",9,8),"Control+A"|"Ctrl+A"=>("a",65,2),"Control+Z"|"Ctrl+Z"=>("z",90,2),
        "Control+Shift+Z"|"Ctrl+Shift+Z"=>("Z",90,10),"Space"=>(" ",32,0),_=>return None,
    })
}
async fn key(app: &AppHandle, s: &Session, name: &str, progress: &mut InputProgress) -> Result<(), String> {
    let (key,code,modifiers)=key_spec(name).ok_or("不支持的按键")?;
    check(app,s)?; progress.attempted=true;
    progress.held_key=Some(json!({"type":"keyUp","key":key,"windowsVirtualKeyCode":code,"modifiers":modifiers}));
    let down=cdp(app,"Input.dispatchKeyEvent",json!({"type":"keyDown","key":key,"windowsVirtualKeyCode":code,"modifiers":modifiers}),None,Some(s.cancel.clone())).await;
    let up=cdp(app,"Input.dispatchKeyEvent",json!({"type":"keyUp","key":key,"windowsVirtualKeyCode":code,"modifiers":modifiers}),None,None).await;
    if up.is_ok() {progress.held_key=None;}
    down?;up?;Ok(())
}
async fn frame_has_focus(app:&AppHandle, observation:&Observation, index:usize) -> Result<bool,String> {
    let mut child=observation.frames.get(index).ok_or("frame 不存在")?;
    let mut depth=0;
    while let Some(parent_id)=&child.parent {
        depth+=1;if depth>12 {return Err("焦点框架嵌套过深".into());}
        let parent=observation.frames.iter().find(|f|&f.id==parent_id).ok_or("焦点父框架不可访问")?;
        let owner=cdp(app,"DOM.getFrameOwner",json!({"frameId":child.id}),parent.session.as_deref(),None).await?;
        let resolved=cdp(app,"DOM.resolveNode",json!({"backendNodeId":owner["backendNodeId"],"executionContextId":parent.context}),parent.session.as_deref(),None).await?;
        let object=resolved["object"]["objectId"].as_str().ok_or("焦点框架不可访问")?;
        let result=cdp(app,"Runtime.callFunctionOn",json!({"objectId":object,"returnByValue":true,"functionDeclaration":"function(){let e=this.ownerDocument.activeElement;while(e?.shadowRoot?.activeElement)e=e.shadowRoot.activeElement;return e===this;}"}),parent.session.as_deref(),None).await;
        let _=cdp(app,"Runtime.releaseObject",json!({"objectId":object}),parent.session.as_deref(),None).await;
        if result?["result"]["value"]!=true {return Ok(false);}
        child=parent;
    }
    Ok(true)
}
async fn require_focus(app:&AppHandle, observation:&Observation, index:usize, reference:&str) -> Result<(),String> {
    let frame=observation.frames.get(index).ok_or("frame 不存在")?;
    if !frame_has_focus(app,observation,index).await? {return Err("焦点已离开目标框架，停止填写".into());}
    let state=evaluate(app,frame,format!("__novaWebview.inputState({})",json!(reference))).await?;
    if state["matches"]!=true || state["editable"]!=true || state["password"]==true { return Err("目标焦点已改变或不可编辑，停止输入；请根据新观察继续".into()); }
    Ok(())
}

async fn apply(
    app: &AppHandle, s: &Session, observation: &Observation, action: &Action,
    image_id: Option<&str>, progress: &mut InputProgress,
) -> Result<(), String> {
    check(app,s)?;
    match action {
        Action::ClickAt {x,y,..} | Action::Move {x,y} | Action::Drag {x,y,..} | Action::ScrollAt {x,y,..} => {
            if observation.full_page && matches!(action,Action::Drag{..}) { return Err("拖动请使用 fullPage=false 的视口截图".into()); }
            let p=coordinate(app,observation,*x,*y,image_id).await?;
            if !matches!(action,Action::Move{..}) { guard_coordinate(app,observation,&p,*x,*y,image_id).await?; }
            if matches!(action,Action::ClickAt{..}|Action::ScrollAt{..}|Action::Drag{..}) {
                progress.attempted=true;
                cdp(app,"Input.dispatchMouseEvent",json!({"type":"mouseMoved","x":p["x"],"y":p["y"]}),None,Some(s.cancel.clone())).await?;
                if p["hoverTarget"]["ref"].is_string() {
                    evaluate(app,&observation.frames[0],format!("__novaWebview.verifyPoint({},{},{},{})",p["hoverTarget"]["ref"],p["x"],p["y"],p["hoverTarget"]["rect"])).await?;
                    if evaluate(app,&observation.frames[0],"__novaWebview.stamp()".into()).await?!=p["stamp"] {return Err("悬停期间视口已变化，停止点击".into());}
                } else {
                    guard_coordinate(app,observation,&p,*x,*y,image_id).await?;
                }
            }
            match action {
                Action::ClickAt {button,click_count,..} => mouse(app,s,&p,button.as_deref().unwrap_or("left"),click_count.unwrap_or(1),progress).await?,
                Action::Move{..} => { progress.attempted=true; cdp(app,"Input.dispatchMouseEvent",json!({"type":"mouseMoved","x":p["x"],"y":p["y"]}),None,Some(s.cancel.clone())).await?; },
                Action::ScrollAt{delta,delta_x,..} => { progress.attempted=true; cdp(app,"Input.dispatchMouseEvent",json!({"type":"mouseWheel","x":p["x"],"y":p["y"],"deltaX":delta_x.unwrap_or(0),"deltaY":delta}),None,Some(s.cancel.clone())).await?; },
                Action::Drag{to_x,to_y,duration_ms,..} => {
                    let end=coordinate(app,observation,*to_x,*to_y,image_id).await?;
                    guard_coordinate(app,observation,&end,*to_x,*to_y,image_id).await?;
                    let (x,y)=(p["x"].as_f64().ok_or("无效坐标")?,p["y"].as_f64().ok_or("无效坐标")?);
                    let (tx,ty)=(end["x"].as_f64().ok_or("无效终点")?,end["y"].as_f64().ok_or("无效终点")?);
                    let duration=duration_ms.unwrap_or(160); let steps=(duration/16).clamp(5,24);
                    let mut last=(x,y); progress.attempted=true;
                    let moved=async {
                        cdp(app,"Input.dispatchMouseEvent",json!({"type":"mouseMoved","x":x,"y":y}),None,Some(s.cancel.clone())).await?;
                        progress.held_mouse=Some(json!({"type":"mouseReleased","x":x,"y":y,"button":"left","clickCount":1}));
                        cdp(app,"Input.dispatchMouseEvent",json!({"type":"mousePressed","x":x,"y":y,"button":"left","buttons":1,"clickCount":1}),None,Some(s.cancel.clone())).await?;
                        for i in 1..=steps {
                            last=(x+(tx-x)*i as f64/steps as f64,y+(ty-y)*i as f64/steps as f64);
                            progress.held_mouse=Some(json!({"type":"mouseReleased","x":last.0,"y":last.1,"button":"left","clickCount":1}));
                            cdp(app,"Input.dispatchMouseEvent",json!({"type":"mouseMoved","x":last.0,"y":last.1,"button":"left","buttons":1}),None,Some(s.cancel.clone())).await?;
                            tokio::time::sleep(Duration::from_millis(duration/steps)).await;
                        }
                        Ok::<_,String>(())
                    }.await;
                    let released=cdp(app,"Input.dispatchMouseEvent",json!({"type":"mouseReleased","x":last.0,"y":last.1,"button":"left","clickCount":1}),None,None).await;
                    if released.is_ok() {progress.held_mouse=None;}
                    moved?; released?;
                }
                _=>unreachable!(),
            }
        }
        Action::Type{text} => {
            let mut focused=false;
            for (index,frame) in observation.frames.iter().enumerate() {
                let now=evaluate(app,frame,"__novaWebview.inputState()".into()).await?;
                if !frame_has_focus(app,observation,index).await? {continue;}
                if now["password"]==true { return Err("密码请手动输入".into()); }
                if now["editable"]==true {
                    if now["identity"]!=observation.pages["pages"][index]["focus"]["identity"] { return Err("输入焦点已变化，请重新观察后输入".into()); }
                    focused=true;
                }
            }
            if !focused { return Err("没有已确认的可编辑焦点；请先 click 或使用 fill".into()); }
            check(app,s)?; progress.attempted=true;
            cdp(app,"Input.insertText",json!({"text":text}),None,Some(s.cancel.clone())).await?;
        }
        Action::Click{frame,r#ref,..} | Action::Fill{frame,r#ref,..} => {
            progress.dom_preflight=true;
            let mode=if matches!(action,Action::Fill{..}) {"fill"} else {"click"};
            let p=point(app,observation,*frame,r#ref,mode).await?;
            // Hover is real input, then validate the same target again before pressing.
            progress.attempted=true;
            cdp(app,"Input.dispatchMouseEvent",json!({"type":"mouseMoved","x":p["x"],"y":p["y"]}),None,Some(s.cancel.clone())).await?;
            evaluate(app,&observation.frames[*frame],format!("__novaWebview.verifyPoint({},{},{},{},{})",json!(r#ref),p["localX"],p["localY"],p["localRect"],json!(mode))).await?;
            if observation.frames[*frame].parent.is_some() {
                let after=point(app,observation,*frame,r#ref,mode).await?;
                if ["x","y"].iter().any(|axis| (p[*axis].as_f64().unwrap_or(f64::NAN)-after[*axis].as_f64().unwrap_or(f64::NAN)).abs()>=0.5) {
                    return Err("框架在悬停后移动，已停止点击，请重新观察".into());
                }
            }
            progress.dom_preflight=false;
            if let Action::Fill{text,..}=action {
                if p["password"]==true { return Err("密码请手动输入".into()); }
                mouse(app,s,&p,"left",1,progress).await?;
                require_focus(app,observation,*frame,r#ref).await?;
                key(app,s,"Control+A",progress).await?;
                require_focus(app,observation,*frame,r#ref).await?;
                check(app,s)?; progress.attempted=true;
                cdp(app,"Input.insertText",json!({"text":text}),None,Some(s.cancel.clone())).await?;
                let verified=evaluate(app,&observation.frames[*frame],format!("__novaWebview.verifyValue({},{})",json!(r#ref),json!(text))).await?;
                if verified["matches"]!=true {return Err("字段实际值与请求不一致（可能被页面校验或长度限制修改）；已停止，请核对新观察".into());}
            } else if let Action::Click{button,click_count,..}=action {
                mouse(app,s,&p,button.as_deref().unwrap_or("left"),click_count.unwrap_or(1),progress).await?;
            }
        }
        Action::Press{key:name}=>key(app,s,name,progress).await?,
        Action::Scroll{frame,r#ref,delta,delta_x}=>{
            progress.dom_preflight=true;
            let mode=if delta_x.unwrap_or(0)!=0 {"scroll_x"} else {"scroll_y"};
            let p=if let Some(reference)=r#ref {point(app,observation,*frame,reference,mode).await?} else {
                if *frame!=0 {return Err("子框架滚动需要明确 ref".into());}
                evaluate(app,&observation.frames[0],format!("__novaWebview.prepare(null,{})",json!(mode))).await?
            };
            progress.dom_preflight=false;
            check(app,s)?; progress.attempted=true;
            cdp(app,"Input.dispatchMouseEvent",json!({"type":"mouseWheel","x":p["x"],"y":p["y"],"deltaX":delta_x.unwrap_or(0),"deltaY":delta}),None,Some(s.cancel.clone())).await?;
            progress.scroll_feedback=Some(evaluate(app,&observation.frames[*frame],
                format!("__novaWebview.scrollFeedback({},{})",json!(r#ref),p["scroll"])).await?);
        }
        Action::Wait{ms}=>tokio::time::sleep(Duration::from_millis(*ms)).await,
    }
    check(app,s)
}

fn image_point(observation: &Observation, x:f64, y:f64, image_id:Option<&str>) -> Result<(f64,f64),String> {
    match image_id {
        Some(id)=>observation.images.iter().find(|image| image.id==id).ok_or("imageId 不属于当前快照")?.point(x,y),
        None=>Ok((x,y)), // Backwards-compatible CSS coordinates; new clients should supply imageId.
    }
}
async fn guard_coordinate(app:&AppHandle, observation:&Observation, point:&Value, x:f64, y:f64, image_id:Option<&str>) -> Result<(),String> {
    let (cx,cy)=image_point(observation,x,y,image_id)?;
    let image=observation.images.iter().find(|i| image_id.map_or(cx>=i.x&&cy>=i.y&&cx<i.x+i.width&&cy<i.y+i.height, |id| i.id==id)).ok_or("坐标不在已返回的截图分片内，请重新截图")?;
    let (px,py)=((cx-image.x)*image.pixels.0 as f64/image.width,(cy-image.y)*image.pixels.1 as f64/image.height);
    let sx=image.width/image.pixels.0 as f64;let sy=image.height/image.pixels.1 as f64;
    let vx=point["x"].as_f64().ok_or("缺少视口落点")?;let vy=point["y"].as_f64().ok_or("缺少视口落点")?;
    let vw=observation.pages["pages"][0]["viewport"]["width"].as_f64().ok_or("缺少视口尺寸")?;
    let vh=observation.pages["pages"][0]["viewport"]["height"].as_f64().ok_or("缺少视口尺寸")?;
    // Compare only actually visible source pixels. A downscaled full-page tile
    // must not turn a 24px guard into a huge offscreen screenshot.
    let left=(px-(24./sx).ceil()).floor().max((px-vx/sx).ceil()).max(0.) as u32;
    let top=(py-(24./sy).ceil()).floor().max((py-vy/sy).ceil()).max(0.) as u32;
    let right=(px+(24./sx).ceil()+1.).ceil().min((px+(vw-vx)/sx).floor()).min(image.pixels.0 as f64) as u32;
    let bottom=(py+(24./sy).ceil()+1.).ceil().min((py+(vh-vy)/sy).floor()).min(image.pixels.1 as f64) as u32;
    if right<=left || bottom<=top {return Err("截图在落点附近分辨率不足，请截取局部高分辨率图片".into());}
    let (width,height)=(right-left,bottom-top);
    let expected=xcap::image::open(&image.path).map_err(|e|e.to_string())?.to_rgba8();
    let expected=xcap::image::imageops::crop_imm(&expected,left,top,width,height).to_image();
    let actual=capture_viewport(app,&observation.pages["pages"][0]["viewport"],
        vx+(left as f64-px)*sx,vy+(top as f64-py)*sy,width as f64*sx,height as f64*sy).await?.to_rgba8();
    if crate::visual_guard::patch_changed(&expected,&actual) { return Err("落点附近画面已变化，未点击；请根据新截图重新定位".into()); }
    if evaluate(app,&observation.frames[0],"__novaWebview.stamp()".into()).await?!=point["stamp"] {return Err("落点校验期间视口已变化，停止点击".into());}
    Ok(())
}

async fn coordinate(
    app: &AppHandle,
    observation: &Observation,
    x: f64,
    y: f64,
    image_id: Option<&str>,
) -> Result<Value, String> {
    if !observation.screenshot {
        return Err(
            "坐标操作需要截图：请 screenshot，或使用 DOM frame/ref；inspect 不能授权坐标操作"
                .into(),
        );
    }
    if observation.captured.elapsed() > Duration::from_secs(180) {
        return Err(format!(
            "截图已过期：已过去 {} 秒，有效期180秒；请 screenshot，不是 DOM 动画错误",
            observation.captured.elapsed().as_secs()
        ));
    }
    let (x,y)=image_point(observation,x,y,image_id)?;
    evaluate(
        app,
        &observation.frames[0],
        format!(
            "__novaWebview.coordinate({},{},{},{})",
            json!(x),
            json!(y),
            observation.pages["pages"][0]["stamp"],
            observation.full_page
        ),
    )
    .await
}

async fn snapshot(
    app: &AppHandle,
    with_image: bool,
    args: &Value,
) -> Result<(Observation, Value), String> {
    let mut region=args.get("region").filter(|v|!v.is_null()).cloned();
    if region.is_some() && args["ref"].is_string() {return Err("region 与 ref 裁剪不能同时提供".into());}
    if !with_image && (region.is_some() || args["ref"].is_string()) {return Err("region/ref 裁剪仅适用于 screenshot".into());}
    if let Some(reference)=args["ref"].as_str() {
        let previous=app.state::<BrowserState>().observations.lock().unwrap().get(&active_label(app)?).cloned().ok_or("目标裁剪需要最新 inspect/screenshot")?;
        if args["snapshotId"].as_str()!=Some(&previous.id) { return Err("目标裁剪 snapshotId 已失效".into()); }
        let p=point(app,&previous,args["frame"].as_u64().unwrap_or(0) as usize,reference,"capture").await?;
        region=Some(p["rect"].clone());
    }
    let scope=args["scope"].as_str().unwrap_or("all");
    if !matches!(scope,"all"|"viewport") {return Err("scope 必须为 all 或 viewport".into());}
    let mut observation = observe(app,scope).await?;
    let auto_visual=!with_image && args["visual"]!="none" && observation.pages["pages"].as_array().is_some_and(|pages|pages.iter().any(|p|p["visualSuggested"]==true));
    let with_image=with_image || auto_visual;
    let dir = state_dir(app).join("browser-shots");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let document_path = dir.join(format!("{}.json", observation.id));
    std::fs::write(
        &document_path,
        serde_json::to_vec(&observation.pages).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let mut result = observation.pages.clone();
    let mut remaining_items = args["maxItems"].as_u64().unwrap_or(60).clamp(1, 2000) as usize;
    let mut remaining_text = args["maxTextChars"]
        .as_u64()
        .unwrap_or(3000)
        .clamp(100, 100000) as usize;
    let query = args["query"].as_str().unwrap_or("").trim().to_lowercase();
    if let Some(pages) = result["pages"].as_array_mut() {
        for page in pages {
            let original_text = page["text"].as_str().unwrap_or_default();
            let matching_text = if query.is_empty() {
                original_text.to_string()
            } else {
                let lower = original_text.to_lowercase();
                lower
                    .find(&query)
                    .map(|at| {
                        let start = lower[..at]
                            .char_indices()
                            .rev()
                            .nth(120)
                            .map_or(0, |(index, _)| index);
                        lower[start..].to_string()
                    })
                    .unwrap_or_default()
            };
            let text: String = matching_text.chars().take(remaining_text).collect();
            remaining_text -= text.chars().count();
            let text_trimmed = text.len() < original_text.len();
            page["text"] = json!(text);
            let items = page["items"].as_array().cloned().unwrap_or_default();
            let mut matches: Vec<Value> = items
                .into_iter()
                .filter(|item| {
                    query.is_empty()
                        || format!("{} {} {}", item["name"], item["region"], item["href"])
                            .to_lowercase()
                            .contains(&query)
                })
                .collect();
            // Full document stays in documentPath; prioritize actionable visible controls in the inline summary.
            matches.sort_by_key(|item| (item["inView"] != true, item["blockedBy"].is_string()));
            let total = matches.len();
            let kept: Vec<Value> = matches.into_iter().take(remaining_items).collect();
            remaining_items -= kept.len();
            page["inlineTruncated"] = json!(text_trimmed || total > kept.len());
            page["returnedTextChars"] = json!(text.chars().count());
            page["nextRead"] = if text_trimmed || total > kept.len() {
                json!({"documentPath":document_path,"operation":"inspect","scope":"all",
                    "notice":"这里只是摘要；完整已加载数据在 documentPath。按 tables.rows 核对 Top N；不足时读取完整文档或按 query 定位表格，不能据摘要断言全部/不存在/只有这些。"})
            } else { Value::Null };
            page["matchingItems"] = json!(total);
            page["items"] = json!(kept);
            if let Some(headings) = page["headings"].as_array_mut() {
                headings.truncate(60);
            }
        }
    }
    result["snapshotId"] = json!(observation.id);
    result["visualReason"] = if auto_visual {json!("页面有大面积 Canvas，自动附可操作视口截图；图内控件不可由 DOM 枚举")} else {Value::Null};
    result["documentPath"] = json!(document_path);
    result["scope"]=json!(if scope=="viewport" {"当前各框架视口内的 DOM 目标；文本仍含已加载文档。屏外/虚拟化内容需 scope=all 或滚动后观察。"} else {"整页已加载DOM，包含屏幕外和内部滚动区域；documentPath保留完整文本、元素和引用。inlineTruncated仅代表工具回复摘要被截短；truncated/coverageGaps表示采集本身不完整。"});
    if with_image {
        let edge=args["maxEdge"].as_u64().unwrap_or(1600);
        if edge!=0 && !(320..=3840).contains(&edge) {return Err("maxEdge 必须为0或320–3840".into());}
        observation.full_page = !auto_visual && region.is_none() && args["fullPage"] != false;
        let mut images = Vec::new();
        if observation.full_page {
            // Retain the root world for cleanup even if capture is cancelled or times out.
            app.state::<BrowserState>()
                .observations
                .lock()
                .unwrap()
                .insert(active_label(app)?, observation.clone());
            evaluate(
                app,
                &observation.frames[0],
                "__novaWebview.captureLayout(true)".into(),
            )
            .await?;
            let metrics = cdp(app, "Page.getLayoutMetrics", json!({}), None, None).await?;
            let size = metrics
                .get("cssContentSize")
                .unwrap_or(&metrics["contentSize"]);
            let width = size["width"]
                .as_f64()
                .filter(|v| v.is_finite() && *v > 0.)
                .ok_or("无效文档宽度")?
                .ceil();
            let height = size["height"]
                .as_f64()
                .filter(|v| v.is_finite() && *v > 0.)
                .ok_or("无效文档高度")?
                .ceil();
            let columns = (width / 2048.).ceil() as u64;
            let rows = (height / 4096.).ceil() as u64;
            let total = columns.checked_mul(rows).ok_or("文档尺寸过大")?;
            let offset = args["tileOffset"].as_u64().unwrap_or(0);
            if offset >= total {
                return Err("tileOffset超出整页截图范围".into());
            }
            // ponytail: at most 4 tiles / 32M CSS pixels per call; nextTile makes huge pages resumable, never silently cropped.
            for tile in offset..total.min(offset + 4) {
                let x = (tile % columns) as f64 * 2048.;
                let y = (tile / columns) as f64 * 4096.;
                let w = (width - x).min(2048.);
                let h = (height - y).min(4096.);
                let scale=if edge==0 {1.} else {(edge as f64/w.max(h)).min(1.)};
                let shot=cdp(app,"Page.captureScreenshot",json!({"format":"png","fromSurface":true,"captureBeyondViewport":true,"clip":{"x":x,"y":y,"width":w,"height":h,"scale":scale}}),None,None).await?;
                let path = dir.join(format!("{}-{tile}.png", observation.id));
                let (bytes,pixels)=screenshot_bytes(shot["data"].as_str().ok_or("截图为空")?,edge as u32)?;
                std::fs::write(&path,bytes).map_err(|e|e.to_string())?;
                let image_id=format!("{}-{tile}",observation.id);
                observation.images.push(ScreenshotImage{id:image_id.clone(),path:path.clone(),x,y,width:w,height:h,pixels});
                images.push(json!({"imageId":image_id,"path":path,"tile":tile,"x":x,"y":y,"width":w,"height":h,"pixelWidth":pixels.0,"pixelHeight":pixels.1}));
            }
            let next = offset + images.len() as u64;
            result["documentSize"] = json!({"width":width,"height":height});
            result["totalTiles"] = json!(total);
            result["screenshotComplete"] = json!(offset == 0 && next == total);
            result["nextTile"] = if next < total {
                json!(next)
            } else {
                Value::Null
            };
            result["coordinateSpace"]=json!("Prefer act(imageId) with image-pixel x/y: tile offsets and PNG dimensions are mapped automatically. Without imageId use legacy CSS document pixels. click_at/move/scroll_at can auto-scroll; prefer DOM ref offscreen and viewport screenshots for Canvas; drag requires fullPage=false.");
            result["screenshotScope"]=json!("主文档整页；内部滚动区域/iframe的屏幕外内容用整页DOM读取。未加载图片或虚拟数据可能仍需定向滚动。");
        } else {
            let viewport=&observation.pages["pages"][0]["viewport"];
            let vw=viewport["width"].as_f64().ok_or("无效视口")?;let vh=viewport["height"].as_f64().ok_or("无效视口")?;
            let (x,y,w,h)=if let Some(region)=region {
                let get=|key:&str|region[key].as_f64().filter(|v|v.is_finite()).ok_or("region 必须包含有限的 x/y/width/height");
                let (x,y,w,h)=(get("x")?,get("y")?,get("width")?,get("height")?);
                if w<=0. || h<=0. || x>=vw || y>=vh || x+w<=0. || y+h<=0. {return Err("region 不在当前视口中".into());}
                (x.max(0.),y.max(0.),(x+w).min(vw)-x.max(0.),(y+h).min(vh)-y.max(0.))
            } else {(0.,0.,vw,vh)};
            let path=dir.join(format!("{}.png",observation.id));
            let (bytes,pixels)=encode_screenshot(capture_viewport(app,viewport,x,y,w,h).await?,edge as u32)?;
            std::fs::write(&path,bytes).map_err(|e|e.to_string())?;
            let image_id=format!("{}-0",observation.id);
            observation.images.push(ScreenshotImage{id:image_id.clone(),path:path.clone(),x,y,width:w,height:h,pixels});
            images.push(json!({"imageId":image_id,"path":path,"x":x,"y":y,"width":w,"height":h,"pixelWidth":pixels.0,"pixelHeight":pixels.1}));
            result["coordinateSpace"]=json!("Provide imageId with act and use image-pixel x/y/to_x/to_y; crop offsets and DPI are mapped automatically. Without imageId, legacy CSS viewport coordinates apply.");
            let after=evaluate(app,&observation.frames[0],"__novaWebview.stamp()".into()).await?;
            if after!=observation.pages["pages"][0]["stamp"] {return Err("截图期间视口变化，请重新截图".into());}
        }
        result["path"] = images[0]["path"].clone();
        result["images"] = json!(images);
        result["fullPage"] = json!(observation.full_page);
        observation.screenshot = true;
    }
    observation.captured = std::time::Instant::now();
    app.state::<BrowserState>()
        .observations
        .lock()
        .unwrap()
        .insert(active_label(app)?, observation.clone());
    Ok((observation, result))
}

async fn control(
    app: &AppHandle,
    id: &str,
    operation: &str,
    args: &Value,
) -> Result<Value, String> {
    control_session(app, session(app, id)?, operation, args).await
}

async fn control_session(
    app: &AppHandle,
    mut s: Session,
    operation: &str,
    args: &Value,
) -> Result<Value, String> {
    let state = app.state::<BrowserState>();
    let _guard = state.gate.try_lock().map_err(|_| "浏览器正在执行任务")?;
    let started = std::time::Instant::now();
    if args["feedback"].as_str().is_some_and(|v|!matches!(v,"none"|"inspect"|"screenshot")) {return Err("feedback 无效".into());}
    if args["scope"].as_str().is_some_and(|v|!matches!(v,"all"|"viewport")) {return Err("scope 无效".into());}
    let settle_ms = match args.get("settleMs").filter(|v|!v.is_null()) {
        None => 1500,
        Some(v) => v.as_u64().filter(|ms| *ms <= 4000).ok_or("settleMs 必须为 0–4000")?,
    };
    let chrome = s.browser_id == "chrome";
    s.cancel = if chrome {
        crate::chrome_browser::begin(app, s.active_tab.rsplit(':').next().unwrap())
    } else {
        Arc::new(AtomicBool::new(false))
    };
    check(app, &s)?;
    if !chrome {
        update_session(app, &s.thread_id, |current| {
            current.cancel = s.cancel.clone();
            current.busy = true;
            current.status = "正在操作网页".into();
        });
        emit(app);
    }
    let mut completed_action = None;
    let mut progress=InputProgress::default();
    let task=CONTROL_TAB.scope(s.active_tab.clone(),async {
        if operation!="act" {return Ok(snapshot(app,operation=="screenshot",args).await?.1);}
        let observation=state.observations.lock().unwrap().get(&s.active_tab).cloned().ok_or("请先 inspect 或 screenshot")?;
        if args["snapshotId"].as_str()!=Some(&observation.id) { return Err("观察已失效：snapshotId 不是最新观察或已执行；使用最近返回的 snapshotId，不要重放动作".into()); }
        let actions=parse_actions(args, if chrome { 16 } else { 8 })?;
        if jev_requires_run(app.state::<AppState>().settings.lock().unwrap().jev_enabled, crate::jev_run::executing_browser_action(), &observation, &actions) {
            return Ok(json!({"status":"not_executed","reason":"jev_run_required",
                "inputAttempted":false,"completedActions":0,"snapshotId":observation.id,
                "basedOnSnapshotId":observation.id,"verification":"unverified",
                "next":"JEV 已启用，DOM click/fill/scroll 必须委托 run。本批次未执行，snapshotId 仍有效；用同一目标和 snapshotId 调用 run，提供 plan.task、authorization、expectedText 及所需 inputs，不需要预列 steps。真实 handoff 返回的快照仅允许一次单步 act 兜底，之后恢复 run。视觉操作仍由主模型决定。"}));
        }
        let all_dom=actions.iter().all(|a|matches!(a,Action::Click{..}|Action::Fill{..}|Action::Scroll{r#ref:Some(_),..}));
        if !all_dom && observation.captured.elapsed()>Duration::from_secs(180) {return Err("观察已过期，请重新观察后继续".into());}
        // Static validation for the WHOLE batch; revalidate the live node/focus
        // before every individual input. Never retarget by matching its label.
        for action in &actions {
            preflight(&observation,action,args["imageId"].as_str())?;
        }
        if chrome {
            // Background tabs throttle animation/timer sampling, making stable DOM targets time out as "moving".
            // Activate the explicitly bound tab before live checks; stale geometry/identity is still checked by apply.
            crate::chrome_browser::request(app,"select_tab",json!({"tabTag":s.active_tab.rsplit(':').next().ok_or("缺少 Chrome tabTag")?})).await?;
        }
        state.observations.lock().unwrap().remove(&s.active_tab);
        let mut failure=None;
        let mut action_timings=Vec::new();
        for action in &actions {
            let began=std::time::Instant::now();
            let applied=apply(app,&s,&observation,action,args["imageId"].as_str(),&mut progress).await;
            action_timings.push(began.elapsed().as_millis());
            match applied {
                Ok(())=>progress.completed+=1,
                Err(error)=>{failure=Some(error);break;},
            }
        }
        let mut result=json!({"status":if failure.is_none(){"executed"}else if progress.attempted{"needs_review"}else{"not_executed"},
            "scrollFeedback":progress.scroll_feedback,
            "canReobserve":failure.is_some() && progress.dom_preflight && progress.completed==0,
            "reason":failure,"inputAttempted":progress.attempted,"completedActions":progress.completed,"actionTimingsMs":action_timings,"basedOnSnapshotId":observation.id,"verification":"unverified"});
        result["actionMs"] = json!(started.elapsed().as_millis());
        result["next"] = json!("根据返回的最新状态验证并继续；fill 一次完成聚焦和填写。executed/needs_review 不要直接重放。坐标操作需截图，DOM 操作使用最新 frame/ref。");
        completed_action = Some(result.clone());
        // A click/Enter often starts a navigation; feeding back the old DOM costs the model another
        // inspect round-trip. Wait (bounded) for the tab to finish loading before observing.
        if chrome && failure.is_none() && settle_ms > 0 && args["feedback"] != "none" && !s.cancel.load(Ordering::SeqCst)
            && actions.iter().any(|a| matches!(a, Action::Click{..} | Action::ClickAt{..} | Action::Press{..})) {
            let tag = s.active_tab.rsplit(':').next().unwrap_or_default();
            result["settle"] = wait_for_tab_load(app, tag, Duration::from_millis(settle_ms)).await;
            completed_action = Some(result.clone());
        }
        if (args["feedback"] != "none" || failure.is_some()) && !s.cancel.load(Ordering::SeqCst) {
            let mut feedback_args = args.clone();
            feedback_args["fullPage"] = json!(false);
            if let Some(args)=feedback_args.as_object_mut() {for key in ["ref","frame","region"] {args.remove(key);} }
            if args["feedback"]=="inspect" {feedback_args["visual"]=json!("none");}
            let visual=args["feedback"]=="screenshot" || (args["feedback"].is_null() && observation.screenshot);
            // Feedback failure must never turn a completed mutation into a retryable action failure.
            match tokio::time::timeout(Duration::from_secs(10), snapshot(app,visual,&feedback_args)).await {
                Ok(Ok((_, feedback))) => result.as_object_mut().unwrap().extend(feedback.as_object().unwrap().clone()),
                Ok(Err(error)) => result["observationError"] = json!(error),
                Err(_) => result["observationError"] = json!("动作后观察10秒超时；动作状态如上，请观察确认，不要重放"),
            }
        }
        Ok(result)
    });
    let mut result = tokio::select! {
        r=tokio::time::timeout(Duration::from_secs(15),task)=>r.map_err(|_|"浏览器操作15秒超时，已停止；请重新观察确认结果".to_string()).and_then(|r|r),
        _=async {while !s.cancel.load(Ordering::SeqCst) {tokio::time::sleep(Duration::from_millis(30)).await;}}=>Err("已停止浏览器操作，请检查页面确认结果".into()),
    };
    if let (Err(error), Some(mut completed)) = (&result, completed_action) {
        completed["observationError"] = json!(error);
        result = Ok(completed);
    }
    s.cancel.store(true, Ordering::SeqCst);
    if operation=="act" {
        if let Err(error)=&result {
            result=Ok(json!({"status":if progress.attempted {"needs_review"} else {"not_executed"},
                "reason":error,"completedActions":progress.completed,"inputAttempted":progress.attempted,
                "basedOnSnapshotId":args["snapshotId"],
                "verification":"unverified","next":"先重新观察确认状态；不要重放可能已经执行的动作"}));
        }
        let mut cleanup_errors=Vec::new();
        // Best-effort cleanup independently of cancellation. Never replay the
        // original down/press; uncertain releases remain needs_review.
        for (method,params) in [("Input.dispatchMouseEvent",progress.held_mouse.take()),("Input.dispatchKeyEvent",progress.held_key.take())] {
            if let Some(params)=params {
                if let Err(error)=CONTROL_TAB.scope(s.active_tab.clone(),cdp(app,method,params,None,None)).await {cleanup_errors.push(error);}
            }
        }
        if !cleanup_errors.is_empty() {
            if let Ok(value)=&mut result {value["status"]=json!("needs_review");value["inputReleaseErrors"]=json!(cleanup_errors);}
        }
    }
    if operation == "screenshot" && args["fullPage"] != false {
        let captured = state
            .observations
            .lock()
            .unwrap()
            .get(&s.active_tab)
            .cloned();
        if let Some(captured) = captured.filter(|o| o.full_page) {
            let restored = CONTROL_TAB
                .scope(s.active_tab.clone(), async {
                    // A viewport capture resets WebView2's full-page scrollbar/layout override.
                    let reset = cdp(
                        app,
                        "Page.captureScreenshot",
                        json!({"format":"png","captureBeyondViewport":false}),
                        None,
                        None,
                    )
                    .await;
                    let style = evaluate(
                        app,
                        &captured.frames[0],
                        "__novaWebview.captureLayout(false)".into(),
                    )
                    .await;
                    reset?;
                    style?;
                    Ok::<_, String>(())
                })
                .await;
            if let Err(error) = restored {
                if result.is_ok() {
                    result = Err(format!("整页截图后恢复页面布局失败，请重新观察：{error}"));
                }
            }
        }
    }
    if !chrome {
        update_session(app, &s.thread_id, |current| {
            if Arc::ptr_eq(&current.cancel, &s.cancel) {
                current.busy = false;
                current.status = "就绪".into();
            }
        });
        emit(app);
    }
    if let Ok(value) = &mut result {
        value["durationMs"] = json!(started.elapsed().as_millis());
        if value["snapshotId"].is_string() {
            value["jev"] = crate::jev::availability(&app.state::<AppState>().settings.lock().unwrap());
        }
    }
    result
}

pub(crate) fn current_context(root: &Path) -> Result<(&'static AppHandle, String), String> {
    let app = APP.get().ok_or("电脑/网页工具仅在 Nova 桌面应用内可用")?;
    let state = app.state::<AppState>();
    let thread_id = state
        .active_thread
        .lock()
        .unwrap()
        .clone()
        .ok_or("请打开一个会话")?;
    let cwd = state
        .store
        .lock()
        .unwrap()
        .get(&thread_id)
        .map(|t| t.cwd.clone())
        .ok_or("会话不存在")?;
    if std::fs::canonicalize(root)
        .ok()
        .zip(std::fs::canonicalize(&cwd).ok())
        .is_none_or(|(a, b)| a != b)
    {
        return Err("工具工作目录与前台会话不同，请切到对应会话后使用电脑/网页工具".into());
    }
    Ok((app, thread_id))
}

pub(crate) async fn execute(root: &Path, args: &Value) -> Result<Value, String> {
    let (app, thread_id) = current_context(root)?;
    let operation = args["operation"].as_str().unwrap_or_default();
    if operation == "open" {
        let url = normalized_url(args["url"].as_str().unwrap_or_default())?;
        let _ = app.emit_to("main", "native-browser:open", json!({"threadId":thread_id}));
        let value = mount(app, thread_id).await?;
        let gate = app.state::<BrowserState>();
        let _guard = gate
            .gate
            .try_lock()
            .map_err(|_| "浏览器正在执行任务，请先停止")?;
        app.get_webview(&active_label(app)?)
            .ok_or("浏览器未创建")?
            .navigate(url)
            .map_err(|e| e.to_string())?;
        return Ok(
            json!({"browserId":value["browserId"],"status":"opening","jev":crate::jev::availability(&jev_settings()?),"next":"页面加载完成后 inspect；JEV 启用时优先 run 委托文本 DOM 子目标"}),
        );
    }
    let id = args["browserId"]
        .as_str()
        .ok_or("缺少 open 返回的 browserId")?;
    let s = session(app, id)?;
    if s.thread_id != thread_id {
        return Err("浏览器属于其它会话".into());
    }
    if operation == "run" {
        return Box::pin(crate::jev_run::browser(root, args, &thread_id, "webview")).await;
    }
    if operation == "advise" {
        let settings = jev_settings()?;
        if settings.jev_enabled { jev_observation(root, args, &thread_id, "webview")?; }
        let mut result = crate::jev::advise(settings, args).await?;
        result["basedOnSnapshotId"] = args["snapshotId"].clone();
        return Ok(result);
    }
    if crate::tool_experience::is_operation(args) {
        let observed = if operation == "experience_search" { None } else {
            let pages = jev_observation(root, args, &thread_id, "webview")?;
            let url = tauri::Url::parse(pages["pages"][0]["url"].as_str().ok_or("观察缺少网站 URL")?).map_err(|e| e.to_string())?;
            Some(url.origin().ascii_serialization())
        };
        let args = args.clone();
        return tokio::task::spawn_blocking(move || crate::tool_experience::execute(
            &crate::lyra::config::nova_root().join("tool-experiences"), "webview", &thread_id, &args, observed.as_deref()))
            .await.map_err(|e| e.to_string())?;
    }
    match operation {
        "stop" => {
            s.cancel.store(true, Ordering::SeqCst);
            app.state::<BrowserState>().observations.lock().unwrap().remove(&s.active_tab);
            Ok(json!({"stopped":true}))
        }
        "tabs" => Ok(json!(s)),
        "downloads" => downloads(app, &thread_id, args).await,
        "new_tab" | "select_tab" | "close_tab" => {
            change_tab(app, &thread_id, operation, args.clone()).await
        }
        "inspect" | "screenshot" | "act" => control(app, id, operation, args).await,
        _ => Err("未知 webview 操作".into()),
    }
}

pub(crate) fn tool_owner(root: &Path, owner: &str) -> Result<String, String> {
    if owner.trim().is_empty() || owner.len() > 4096 {
        return Err("缺少有效工具客户端标识，请重启工具客户端".into());
    }
    let root = std::fs::canonicalize(root).map_err(|e| e.to_string())?;
    if !root.is_dir() {
        return Err("工具工作目录不存在".into());
    }
    Ok(format!("{owner}:{}", root.display()))
}

pub(crate) fn jev_settings() -> Result<crate::settings::Settings, String> {
    let app = APP.get().ok_or("JEV 仅在 Nova 桌面应用内可用")?;
    Ok(app.state::<AppState>().settings.lock().unwrap().clone())
}

fn jev_observation_key(root: &Path, args: &Value, owner: &str, tool: &str) -> Result<String, String> {
    let app = APP.get().ok_or("仅 Nova 内可用")?;
    let key = if tool == "webview" {
        let (_, thread_id) = current_context(root)?;
        if thread_id != owner { return Err("会话已切换，停止 JEV 连续决策".into()); }
        let s = session(app, args["browserId"].as_str().ok_or("缺少 browserId")?)?;
        check(app, &s)?;
        s.active_tab
    } else {
        let owner = tool_owner(root, owner)?;
        let tag = args["tabTag"].as_str().ok_or("缺少 tabTag")?;
        format!("chrome:{owner}:{tag}")
    };
    Ok(key)
}

pub(crate) fn jev_observation(root: &Path, args: &Value, owner: &str, tool: &str) -> Result<Value, String> {
    browser_observation(root, args, owner, tool, None)
}

pub(crate) fn jev_fallback(root: &Path, args: &Value, owner: &str, tool: &str, allow: bool) -> Result<Value, String> {
    browser_observation(root, args, owner, tool, Some(allow))
}

fn browser_observation(root: &Path, args: &Value, owner: &str, tool: &str, fallback: Option<bool>) -> Result<Value, String> {
    let app = APP.get().ok_or("仅 Nova 内可用")?;
    let key = jev_observation_key(root, args, owner, tool)?;
    let state = app.state::<BrowserState>();
    let mut observations = state.observations.lock().unwrap();
    let observation = observations.get_mut(&key)
        .filter(|o| args["snapshotId"].as_str() == Some(&o.id) && o.captured.elapsed() <= Duration::from_secs(180))
        .ok_or("观察已失效，交回主模型重新观察")?;
    if let Some(allow) = fallback { observation.jev_fallback = allow; }
    Ok(observation.pages.clone())
}

pub(crate) async fn execute_chrome(root: &Path, args: &Value, owner: &str) -> Result<Value, String> {
    let app = APP.get().ok_or("网页工具仅在 Nova 桌面应用内可用")?;
    let thread_id = tool_owner(root, owner)?;
    let operation = args["operation"].as_str().unwrap_or_default();
    if operation == "run" {
        return Box::pin(crate::jev_run::browser(root, args, owner, "chrome")).await;
    }
    if operation == "advise" {
        let settings = jev_settings()?;
        if settings.jev_enabled {
            let tag = args["tabTag"].as_str().ok_or("JEV 辅助判断需 tabTag")?;
            let state = app.state::<BrowserState>();
            let observations = state.observations.lock().unwrap();
            observations.get(&format!("chrome:{thread_id}:{tag}"))
                .filter(|o| args["snapshotId"].as_str() == Some(&o.id) && o.captured.elapsed() <= Duration::from_secs(180))
                .ok_or("JEV 辅助判断需本会话该标签最新观察（180秒内）")?;
        }
        let mut result = crate::jev::advise(settings, args).await?;
        result["basedOnSnapshotId"] = args["snapshotId"].clone();
        return Ok(result);
    }
    if crate::tool_experience::is_operation(args) {
        let observed = if operation == "experience_search" { None } else {
            let tag = args["tabTag"].as_str().ok_or("经验保存/反馈需tabTag")?;
            let state = app.state::<BrowserState>();
            let observations = state.observations.lock().unwrap();
            let observation = observations.get(&format!("chrome:{thread_id}:{tag}"))
                .filter(|o| args["snapshotId"].as_str() == Some(&o.id) && o.captured.elapsed() <= Duration::from_secs(180))
                .ok_or("保存/反馈经验前需本会话该标签最新inspect/screenshot（180秒内）")?;
            let url = tauri::Url::parse(observation.pages["pages"][0]["url"].as_str().ok_or("观察缺少网站URL")?).map_err(|e| e.to_string())?;
            Some(url.origin().ascii_serialization())
        };
        let args = args.clone();
        return tokio::task::spawn_blocking(move || crate::tool_experience::execute(
            &crate::lyra::config::nova_root().join("tool-experiences"), "chrome", &thread_id, &args, observed.as_deref()))
            .await.map_err(|e| e.to_string())?;
    }
    let connection = crate::chrome_browser::connect(app).await?;
    if operation == "downloads" {
        return crate::chrome_browser::request(app, operation, args.clone()).await
            .map_err(|e| format!("{e}；下载查询需要 Nova Chrome 0.1.6，请确认扩展已更新并重新加载"));
    }
    if matches!(operation, "connect" | "status") {
        let mut connection = connection;
        connection["jev"] = crate::jev::availability(&jev_settings()?);
        if connection["connected"] == true {
            match crate::chrome_browser::request(app, "status", json!({})).await {
                Ok(capabilities) => connection["incognitoAllowed"] = capabilities["incognitoAllowed"].clone(),
                Err(error) => connection["capabilityError"] = json!(format!("{error}；请重新加载Nova Chrome扩展")),
            }
        }
        return Ok(connection);
    }
    let observe = match args["observe"].as_str() {
        None => "inspect",
        Some(mode @ ("inspect" | "screenshot" | "none")) => mode,
        Some(_) => return Err("observe 必须为 inspect/screenshot/none".into()),
    };
    // Anything that changes the real screen or focus shares one lease with jianlai; the other
    // tool's stale desktop snapshot is retired explicitly instead of failing mid-batch later.
    let mutating = matches!(operation, "open" | "new_tab" | "select_tab" | "close_tab" | "goto" | "back" | "forward" | "reload" | "act");
    let _lease = if mutating {
        match crate::jianlai::lease_input("chrome") {
            Ok(lease) => Some(lease),
            Err(error) if operation == "act" => return Ok(json!({"status":"not_executed","completedActions":0,"inputAttempted":false,
                "reason":error,"basedOnSnapshotId":args["snapshotId"],"verification":"unverified","browser":"chrome","tabTag":args["tabTag"]})),
            Err(error) => return Err(error),
        }
    } else { None };
    let retire_desktop = |value: &mut Value| {
        if crate::jianlai::invalidate_desktop(&format!("chrome {operation} 已改变屏幕内容")) {
            value["desktopSnapshotInvalidated"] = json!(true);
            value["desktopNotice"] = json!("剑来旧快照已作废；需要剑来时先重新截图");
        }
    };
    if matches!(operation, "tabs" | "open" | "new_tab") {
        let mut params = args.clone();
        if let Some(url) = args["url"].as_str() {
            params["url"] = json!(normalized_url(url)?.to_string());
        }
        strip_observe_keys(&mut params);
        let mut value = crate::chrome_browser::request(app, operation, params).await?;
        let mut observe_args = args.clone();
        let mut target = None;
        if operation == "tabs" {
            // `query` binds deterministically when exactly one tab matches; no guessing the active tab.
            if let Some(q) = args["query"].as_str().map(str::trim).filter(|q| !q.is_empty()) {
                let q = q.to_lowercase();
                let all = value["tabs"].as_array().cloned().unwrap_or_default();
                let matched: Vec<Value> = all.iter().filter(|t| ["title", "url"].iter()
                    .any(|k| t[*k].as_str().is_some_and(|s| s.to_lowercase().contains(&q)))).cloned().collect();
                let controllable: Vec<&Value> = matched.iter().filter(|t| t["controllable"] != false).collect();
                value["totalTabs"] = json!(all.len());
                value["query"] = json!(q);
                match controllable.as_slice() {
                    [one] => { value["tabTag"] = one["tag"].clone(); target = one["tag"].as_str().map(str::to_owned); }
                    [] => value["next"] = json!("没有可操作标签匹配 query；换关键词，或 open(url) 新开标签"),
                    _ => value["next"] = json!("query 匹配到多个标签，请从 tabs 中选定 tabTag 后 inspect"),
                }
                value["tabs"] = json!(matched);
                if let Some(a) = observe_args.as_object_mut() { a.remove("query"); }
            }
        } else {
            retire_desktop(&mut value);
            if args["url"].is_string() { target = value["tabTag"].as_str().map(str::to_owned); }
        }
        if let (Some(tag), false) = (target, observe == "none") {
            let observed = observe_tab(app, &thread_id, &tag, &observe_args, observe, Duration::from_secs(8)).await;
            value.as_object_mut().unwrap().extend(observed.as_object().cloned().unwrap_or_default());
            value["tabTag"] = json!(tag);
            value["browser"] = json!("chrome");
        }
        return Ok(value);
    }
    let tag = args["tabTag"]
        .as_str()
        .filter(|tag| {
            tag.len() <= 80
                && tag.starts_with('C')
                && tag.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
        .ok_or("缺少有效 tabTag；请先 chrome.tabs（可带 query 唯一匹配直接绑定），不会默认操作当前激活标签")?;
    if operation == "stop" {
        crate::chrome_browser::stop(app, tag);
        app.state::<BrowserState>().observations.lock().unwrap().remove(&format!("chrome:{thread_id}:{tag}"));
    }
    if matches!(
        operation,
        "select_tab" | "close_tab" | "goto" | "back" | "forward" | "reload" | "stop"
    ) {
        let mut params = args.clone();
        if operation == "goto" {
            params["url"] =
                json!(normalized_url(args["url"].as_str().unwrap_or_default())?.to_string());
        }
        strip_observe_keys(&mut params);
        let mut value = crate::chrome_browser::request(app, operation, params).await?;
        if matches!(
            operation,
            "close_tab" | "goto" | "back" | "forward" | "reload" | "stop"
        ) {
            app.state::<BrowserState>()
                .observations
                .lock()
                .unwrap()
                .retain(|key, _| !key.ends_with(&format!(":{tag}")));
        }
        if mutating { retire_desktop(&mut value); }
        // Navigation returns the loaded page directly: no separate inspect round-trip.
        if matches!(operation, "select_tab" | "goto" | "back" | "forward" | "reload") && observe != "none" {
            let observed = observe_tab(app, &thread_id, tag, args, observe, Duration::from_secs(8)).await;
            value.as_object_mut().unwrap().extend(observed.as_object().cloned().unwrap_or_default());
            value["tabTag"] = json!(tag);
            value["browser"] = json!("chrome");
        }
        return Ok(value);
    }
    if !matches!(operation, "inspect" | "screenshot" | "act") {
        return Err("未知 chrome 操作".into());
    }
    let mut value = control_session(app, chrome_session(&thread_id, tag), operation, args).await?;
    if operation == "act" && value["status"] != "not_executed" { retire_desktop(&mut value); }
    value["tabTag"] = json!(tag);
    value["browser"] = json!("chrome");
    Ok(value)
}

// Observation-only parameters ride along with navigation calls; the extension must not see them.
fn strip_observe_keys(params: &mut Value) {
    if let Some(object) = params.as_object_mut() {
        for key in ["observe", "query", "scope", "maxItems", "maxTextChars", "maxEdge", "visual", "settleMs"] {
            object.remove(key);
        }
    }
}

fn chrome_session(thread_id: &str, tag: &str) -> Session {
    Session {
        browser_id: "chrome".into(),
        thread_id: thread_id.into(),
        url: String::new(),
        visible: true,
        busy: false,
        status: String::new(),
        active_tab: format!("chrome:{thread_id}:{tag}"),
        tabs: Vec::new(),
        bounds: None,
        cancel: Arc::new(AtomicBool::new(false)),
    }
}

/// Poll the extension's tab inventory until the tab finished loading (status complete, no
/// pendingUrl). Bounded: a page that never settles still returns so the caller observes what is
/// there; older extensions without `loading` report complete immediately.
async fn wait_for_tab_load(app: &AppHandle, tag: &str, budget: Duration) -> Value {
    let started = std::time::Instant::now();
    // Let the navigation commit before trusting the previous page's "complete".
    tokio::time::sleep(Duration::from_millis(120)).await;
    let mut state = json!({"status":"unknown"});
    loop {
        match crate::chrome_browser::request(app, "tabs", json!({})).await {
            Ok(value) => {
                let Some(tab) = value["tabs"].as_array().and_then(|tabs| tabs.iter().find(|t| t["tag"] == tag)).cloned() else {
                    state = json!({"status":"closed"});
                    break;
                };
                let loading = tab["loading"] == true;
                state = json!({"status": if loading {"loading"} else {"complete"}, "url": tab["url"], "title": tab["title"]});
                if !loading { break; }
            }
            Err(error) => { state = json!({"status":"unknown","error":error}); break; }
        }
        if started.elapsed() >= budget { break; }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    state["waitedMs"] = json!(started.elapsed().as_millis());
    state
}

/// Observe a tab right after a navigation-style operation so the caller gets snapshotId/DOM in
/// the same reply. Load waits and "not ready" retries share one bounded budget; observation
/// failure is reported alongside the completed operation, never as an operation failure.
async fn observe_tab(app: &AppHandle, thread_id: &str, tag: &str, args: &Value, mode: &str, budget: Duration) -> Value {
    let started = std::time::Instant::now();
    let mut result = json!({"load": wait_for_tab_load(app, tag, budget).await});
    if result["load"]["status"] == "closed" {
        result["observationError"] = json!("标签已关闭，无法观察");
        return result;
    }
    let mut observe_args = json!({"operation":mode,"tabTag":tag,"fullPage":false});
    observe_args["scope"] = args.get("scope").filter(|v| !v.is_null()).cloned().unwrap_or(json!("viewport"));
    for key in ["query", "maxItems", "maxTextChars", "maxEdge", "visual"] {
        if let Some(value) = args.get(key).filter(|v| !v.is_null()) { observe_args[key] = value.clone(); }
    }
    loop {
        match control_session(app, chrome_session(thread_id, tag), mode, &observe_args).await {
            Ok(observed) => {
                result.as_object_mut().unwrap().extend(observed.as_object().cloned().unwrap_or_default());
                result["observed"] = json!(mode);
                break;
            }
            Err(error) if error.contains("尚未就绪") && started.elapsed() < budget => tokio::time::sleep(Duration::from_millis(250)).await,
            Err(error) => {
                result["observationError"] = json!(format!("{error}；操作本身已完成，页面可能仍在加载，稍后 inspect"));
                break;
            }
        }
    }
    result
}

fn state_dir(app: &AppHandle) -> std::path::PathBuf {
    app.state::<AppState>().config_dir.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn jev_policy_requires_delegation_and_bounds_handoff_to_one_fresh_action() {
        let mut observation = Observation { frames: Vec::new(), pages: json!({}), id: "current".into(),
            captured: std::time::Instant::now(), screenshot: true, full_page: false,
            images: Vec::new(), jev_fallback: false };
        let actions = |items: Value| parse_actions(&json!({"actions":items}), 16).unwrap();
        for dom in [json!({"action":"click","frame":0,"ref":"a"}),
            json!({"action":"fill","frame":0,"ref":"a","text":"x"}),
            json!({"action":"scroll","frame":0,"delta":400}),
            json!({"action":"scroll","frame":0,"ref":"a","delta":400})] {
            let single = actions(json!([dom]));
            assert!(jev_requires_run(true, false, &observation, &single));
            assert!(!jev_requires_run(false, false, &observation, &single));
            assert!(!jev_requires_run(true, true, &observation, &single));
            for extra in [json!({"action":"click_at","x":10,"y":20}),
                json!({"action":"press","key":"Enter"}), json!({"action":"wait","ms":1})] {
                assert!(jev_requires_run(true, false, &observation, &actions(json!([extra,dom]))));
            }
            observation.jev_fallback = true;
            assert!(!jev_requires_run(true, false, &observation, &single));
            assert!(jev_requires_run(true, false, &observation, &actions(json!([dom,dom]))));
            observation.captured -= Duration::from_secs(181);
            assert!(jev_requires_run(true, false, &observation, &single));
            observation.captured = std::time::Instant::now();
            observation.jev_fallback = false;
        }
        assert!(!jev_requires_run(true, false, &observation, &actions(json!([
            {"action":"click_at","x":10,"y":20},{"action":"type","text":"x"},
            {"action":"press","key":"Enter"}]))));
    }

    #[test]
    fn background_tool_owners_are_stable_and_isolated() {
        let root = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let owner = tool_owner(root.path(), "client-a").unwrap();
        assert_eq!(owner, tool_owner(&root.path().join("."), "client-a").unwrap());
        assert_ne!(owner, tool_owner(root.path(), "client-b").unwrap());
        assert_ne!(owner, tool_owner(other.path(), "client-a").unwrap());
        assert!(tool_owner(root.path(), "").is_err());
        assert!(tool_owner(&root.path().join("missing"), "client-a").is_err());
    }

    #[test]
    fn pixel_crop_mapping_and_action_batches_are_bounded() {
        let image=ScreenshotImage{id:"image".into(),path:"unused.png".into(),x:100.,y:300.,width:600.,height:400.,pixels:(1200,800)};
        assert_eq!(image.point(400.,200.).unwrap(),(300.,400.));
        assert!(image.point(1200.,0.).is_err());assert!(image.point(-1.,0.).is_err());assert!(image.point(f64::NAN,0.).is_err());
        let small=ScreenshotImage{pixels:(300,200),..image};assert_eq!(small.point(100.,50.).unwrap(),(300.,400.));
        assert!(parse_actions(&json!({"actions":[{"action":"fill","frame":0,"ref":"a","text":"one"},{"action":"click","frame":0,"ref":"b","button":"right","click_count":2}]}), 8).is_ok());
        for value in [json!({"actions":[]}),json!({"actions":[{"action":"click_at","x":1,"y":2,"button":"bad"}]}),
            json!({"action":{"action":"drag","x":1,"y":2,"to_x":3,"to_y":4,"duration_ms":99999}}),
            json!({"action":{"action":"wait","ms":3000}}),json!({"actions":[{"action":"press","key":"Enter"},{"action":"press","key":"bad"}]}),
            json!({"action":{"action":"wait","ms":1},"actions":[{"action":"wait","ms":1}]})] {assert!(parse_actions(&value, 8).is_err(),"{value}");}
        assert!(parse_actions(&json!({"actions":vec![json!({"action":"wait","ms":0});16]}), 16).is_ok());
        assert!(parse_actions(&json!({"actions":vec![json!({"action":"wait","ms":0});17]}), 16).is_err());
        assert!(parse_actions(&json!({"actions":vec![json!({"action":"wait","ms":0});9]}), 8).is_err());
        let mut png=vec![0u8;24];png[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");png[12..16].copy_from_slice(b"IHDR");
        png[16..20].copy_from_slice(&1200u32.to_be_bytes());png[20..24].copy_from_slice(&800u32.to_be_bytes());
        assert_eq!(png_dimensions(&png).unwrap(),(1200,800));assert!(png_dimensions(&png[..23]).is_err());
        png[16..20].copy_from_slice(&u32::MAX.to_be_bytes());assert!(png_dimensions(&png).is_err());
    }
    #[test]
    fn screenshot_budget_uses_actual_png_dimensions() {
        use base64::Engine;
        let image = xcap::image::RgbaImage::from_pixel(2400, 1200, xcap::image::Rgba([30, 90, 150, 255]));
        let mut png = std::io::Cursor::new(Vec::new());
        xcap::image::DynamicImage::ImageRgba8(image)
            .write_to(&mut png, xcap::image::ImageFormat::Png).unwrap();
        let encoded = base64::engine::general_purpose::STANDARD.encode(png.get_ref());
        let (resized, pixels) = screenshot_bytes(&encoded, 1600).unwrap();
        assert_eq!(pixels, (1600, 800));
        assert_eq!(png_dimensions(&resized).unwrap(), pixels);
        let (unchanged, pixels) = screenshot_bytes(&encoded, 0).unwrap();
        assert_eq!(pixels, (2400, 1200));
        assert_eq!(unchanged, png.into_inner());
        assert!(screenshot_bytes("not-base64", 1600).is_err());
    }
    #[test]
    fn native_browser_rejects_unsafe_urls_and_unstructured_actions() {
        assert_eq!(normalized_url("localhost:5173").unwrap().scheme(), "http");
        for url in [
            "file:///C:/secret",
            "javascript:alert(1)",
            "https://user:pass@example.com",
        ] {
            assert!(normalized_url(url).is_err(), "{url}");
        }
        assert!(parse_action(r#"{"action":"click","frame":0,"ref":"v1:0"}"#).is_ok());
        assert!(
            parse_action(r#"{"action":"click","frame":0,"ref":"v1:0","selector":"body"}"#).is_err()
        );
        assert!(parse_action(r#"{"action":"eval","code":"alert(1)"}"#).is_err());
        let error = parse_action(r#"{"action":"click","x":100,"y":100}"#).unwrap_err();
        assert!(error.contains("click_at") && error.contains("imageId 放在工具顶层"));
        let error = parse_action(r#"{"action":"press","key":"Enter","frame":0}"#).unwrap_err();
        assert!(error.contains("不传 frame/ref"));
    }
}
