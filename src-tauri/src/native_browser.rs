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
tokio::task_local! { static CONTROL_TAB: String; }
tokio::task_local! { static INPUT_TRACE: Arc<Mutex<InputTrace>>; }
#[derive(Default)]
struct InputTrace { sent: bool, calls: usize, mouse: Option<Value>, key: Option<Value> }

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
#[path = "browser_pixels.rs"]
mod browser_pixels;
use browser_pixels::{Crop, ImageMap};
#[path = "visual_guard.rs"]
mod visual_guard;

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
            if !matches!(url.scheme(), "http" | "https" | "about") {
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
    view.with_webview(move |native| unsafe {
        let Ok(core) = native.controller().CoreWebView2() else {
            return;
        };
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
                    matches!(u.scheme(), "http" | "https") || u.as_str() == "about:blank"
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
            Ok(json!({"stopped":true}))
        }
        "status" => Ok(json!(s)),
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
    let _=INPUT_TRACE.try_with(|trace| {
        let mut trace=trace.lock().unwrap(); trace.calls+=1;
        match (method,params["type"].as_str().unwrap_or("")) {
            ("Input.dispatchMouseEvent","mousePressed") => { trace.sent=true; trace.mouse=Some(params.clone()); },
            ("Input.dispatchMouseEvent","mouseMoved") => { if let Some(p)=trace.mouse.as_mut() {p["x"]=params["x"].clone();p["y"]=params["y"].clone();} },
            ("Input.dispatchMouseEvent","mouseWheel") | ("Input.insertText",_) => trace.sent=true,
            ("Input.dispatchKeyEvent","keyDown"|"rawKeyDown") => {trace.sent=true;trace.key=Some(params.clone());},
            _ => (),
        }
    });
    let label = active_label(app)?;
    let result=if label.starts_with("chrome:") {
        let tag = label.rsplit(':').next().ok_or("缺少 Chrome tabTag")?;
        crate::chrome_browser::request(app,"cdp",json!({"tabTag":tag,"method":method,"params":params,"sessionId":session_id})).await
    } else { native_cdp(app, method, params.clone(), session_id, cancel).await };
    if result.is_ok() {
        let _=INPUT_TRACE.try_with(|trace| {
            let mut trace=trace.lock().unwrap();
            if method=="Input.dispatchMouseEvent" && params["type"]=="mouseReleased" {trace.mouse=None;}
            if method=="Input.dispatchKeyEvent" && params["type"]=="keyUp" {trace.key=None;}
        });
    }
    result
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
    images: Vec<ImageMap>,
}

async fn observe(app: &AppHandle, query: &str) -> Result<Observation, String> {
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
                format!("{PAGE_SCRIPT};__novaWebview.observe({},20000,{})", json!(nonce),json!(query)),
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
            Err(error) if frame.parent.is_none() => return Err(format!("主页面尚未就绪：{error}")),
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
    })
}

#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    ClickAt {
        x: f64,
        y: f64,
        #[serde(rename = "imageId")]
        image_id: Option<String>,
    },
    Move {
        x: f64,
        y: f64,
        #[serde(rename = "imageId")]
        image_id: Option<String>,
    },
    DoubleClickAt {
        x: f64,
        y: f64,
        #[serde(rename = "imageId")]
        image_id: Option<String>,
    },
    WaitFor { frame: usize, r#ref: String, state: String, text: Option<String>, ms: Option<u64> },
    Drag {
        #[serde(rename = "imageId")]
        image_id: Option<String>,
        x: f64,
        y: f64,
        to_x: f64,
        to_y: f64,
    },
    Type {
        text: String,
    },
    ScrollAt {
        x: f64,
        y: f64,
        delta: i32,
        #[serde(default)]
        delta_x: i32,
        #[serde(rename = "imageId")]
        image_id: Option<String>,
    },
    Click {
        frame: usize,
        r#ref: String,
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
    serde_json::from_str(text).map_err(|e| format!("动作 JSON 无效：{e}"))
}

async fn point(
    app: &AppHandle,
    observation: &Observation,
    index: usize,
    reference: &str,
    for_scroll: bool,
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
    let result = async {
        for (parent, object) in chain.iter().rev() {
            let moved = cdp(app, "Runtime.callFunctionOn", json!({"objectId":object,"functionDeclaration":"async function(){const r=this.getBoundingClientRect();if(r.left<0||r.top<0||r.right>innerWidth||r.bottom>innerHeight){this.scrollIntoView({block:'nearest',inline:'nearest',behavior:'instant'});await new Promise(r=>setTimeout(r,32));}}","awaitPromise":true}), parent.session.as_deref(), None).await?;
            if moved.get("exceptionDetails").is_some() { return Err("框架坐标无法安全映射".into()); }
        }
        let local = evaluate(app, frame, format!("__novaWebview.prepare({},{})", json!(reference),for_scroll)).await?;
        let map = |initial: Value| async {
            let mut value = initial;
            for (parent, object) in &chain {
                let result = cdp(app, "Runtime.callFunctionOn", json!({"objectId":object,"returnByValue":true,"awaitPromise":true,"arguments":[{"value":value}],"functionDeclaration":"function(p){return __novaWebview.frameOwner(this,p)}"}), parent.session.as_deref(), None).await?;
                if result.get("exceptionDetails").is_some() { return Err("框架坐标无法安全映射".to_string()); }
                value["x"] = result["result"]["value"]["x"].clone();
                value["y"] = result["result"]["value"]["y"].clone();
                if value["x"].as_f64().is_none() || value["y"].as_f64().is_none() { return Err("框架坐标无法安全映射".into()); }
            }
            Ok(value)
        };
        let value = map(local.clone()).await?;
        cdp(app, "Input.dispatchMouseEvent", json!({"type":"mouseMoved","x":value["x"],"y":value["y"]}), None, None).await?;
        // Hover handlers can insert an overlay or move the control. Do not click stale geometry.
        evaluate(app, frame, format!("__novaWebview.validate({},{})", json!(reference), local)).await?;
        let checked = map(local).await?;
        if ["x","y"].iter().any(|k| (checked[k].as_f64().unwrap()-value[k].as_f64().unwrap()).abs()>0.5) {
            return Err("目标在准备期间已变化：父框架移动".into());
        }
        Ok(value)
    }.await;
    for (parent, object) in &chain {
        let _ = cdp(app, "Runtime.releaseObject", json!({"objectId":object}), parent.session.as_deref(), None).await;
    }
    result
}

async fn mouse(app: &AppHandle, s: &Session, p: &Value) -> Result<(), String> {
    check(app, s)?;
    for event in ["mousePressed", "mouseReleased"] {
        cdp(
            app,
            "Input.dispatchMouseEvent",
            json!({"type":event,"x":p["x"],"y":p["y"],"button":"left","clickCount":1}),
            None,
            (event == "mousePressed").then(|| s.cancel.clone()),
        )
        .await?;
    }
    Ok(())
}

async fn key(app: &AppHandle, s: &Session, name: &str) -> Result<(), String> {
    let (key, code, modifiers) = match name {
        "Enter" => ("Enter", 13, 0),
        "Tab" => ("Tab", 9, 0),
        "Escape" => ("Escape", 27, 0),
        "Backspace" => ("Backspace", 8, 0),
        "ArrowDown" => ("ArrowDown", 40, 0),
        "ArrowUp" => ("ArrowUp", 38, 0),
        "ArrowLeft" => ("ArrowLeft", 37, 0),
        "ArrowRight" => ("ArrowRight", 39, 0),
        "Delete" => ("Delete", 46, 0),
        "Home" => ("Home", 36, 0),
        "End" => ("End", 35, 0),
        "PageDown" => ("PageDown", 34, 0),
        "PageUp" => ("PageUp", 33, 0),
        "Shift+Tab" => ("Tab", 9, 8),
        "Control+A" => ("a", 65, 2),
        _ => return Err("不支持的按键".into()),
    };
    check(app, s)?;
    for event in ["keyDown", "keyUp"] {
        cdp(
            app,
            "Input.dispatchKeyEvent",
            json!({"type":event,"key":key,"windowsVirtualKeyCode":code,"modifiers":modifiers}),
            None,
            (event == "keyDown").then(|| s.cancel.clone()),
        )
        .await?;
    }
    Ok(())
}

async fn apply(
    app: &AppHandle,
    s: &Session,
    observation: &Observation,
    action: &Action,
) -> Result<(), String> {
    check(app, s)?;
    match action {
        Action::ClickAt { x, y, image_id }
        | Action::DoubleClickAt { x, y, image_id }
        | Action::Move { x, y, image_id }
        | Action::Drag { x, y, image_id, .. }
        | Action::ScrollAt { x, y, image_id, .. } => {
            if observation.full_page && matches!(action, Action::Drag { .. }) {
                return Err("拖动请使用 fullPage=false 的视口截图，避免拖动过程中滚动页面".into());
            }
            let (x, y) = image_point(observation, image_id.as_deref(), *x, *y)?;
            let p = coordinate(app, observation, x, y).await?;
            if !matches!(action,Action::Move{..}) {check_image_pixels(app,observation,x,y,&p).await?;}
            cdp(app,"Input.dispatchMouseEvent",json!({"type":"mouseMoved","x":p["x"],"y":p["y"]}),None,Some(s.cancel.clone())).await?;
            evaluate(app,&observation.frames[0],"__novaWebview.validateCoordinate()".into()).await?;
            match action {
                Action::ClickAt { .. } => mouse(app, s, &p).await?,
                Action::DoubleClickAt { .. } => {
                    mouse(app, s, &p).await?;
                    for event in ["mousePressed","mouseReleased"] {
                        cdp(app,"Input.dispatchMouseEvent",json!({"type":event,"x":p["x"],"y":p["y"],"button":"left","clickCount":2}),None,(event=="mousePressed").then(||s.cancel.clone())).await?;
                    }
                }
                Action::Move { .. } => (),
                Action::ScrollAt { delta, delta_x, .. } => {
                    if delta.unsigned_abs() > 1200 || delta_x.unsigned_abs() > 1200 {
                        return Err("单次滚动不能超过1200像素".into());
                    }
                    cdp(
                        app,
                        "Input.dispatchMouseEvent",
                        json!({"type":"mouseWheel","x":p["x"],"y":p["y"],"deltaX":delta_x,"deltaY":delta}),
                        None,
                        Some(s.cancel.clone()),
                    )
                    .await?;
                }
                Action::Drag { to_x, to_y, .. } => {
                    let (to_x,to_y) = image_point(observation,image_id.as_deref(),*to_x,*to_y)?;
                    let end = coordinate(app, observation, to_x, to_y).await?;
                    check_image_pixels(app,observation,to_x,to_y,&end).await?;
                    let (px,py)=(p["x"].as_f64().ok_or("无效拖动坐标")?,p["y"].as_f64().ok_or("无效拖动坐标")?);
                    let (tx,ty)=(end["x"].as_f64().ok_or("无效拖动坐标")?,end["y"].as_f64().ok_or("无效拖动坐标")?);
                    cdp(app,"Input.dispatchMouseEvent",json!({"type":"mousePressed","x":px,"y":py,"button":"left","buttons":1,"clickCount":1}),None,Some(s.cancel.clone())).await?;
                    // Intermediate trusted pointer events trigger canvas drag thresholds and drawing paths.
                    let mut last=(px,py);
                    let moved=async {
                        for step in 1..=8 {
                            last=(px+(tx-px)*step as f64/8.,py+(ty-py)*step as f64/8.);
                            cdp(app,"Input.dispatchMouseEvent",json!({"type":"mouseMoved","x":last.0,"y":last.1,"button":"left","buttons":1}),None,Some(s.cancel.clone())).await?;
                            tokio::time::sleep(Duration::from_millis(8)).await;
                        }
                        Ok::<_,String>(())
                    }.await;
                    let released=cdp(app,"Input.dispatchMouseEvent",json!({"type":"mouseReleased","x":last.0,"y":last.1,"button":"left","buttons":0,"clickCount":1}),None,None).await;
                    moved?; released?;
                }
                _ => unreachable!(),
            }
        }
        Action::Type { text } => {
            if text.len() > 16000 {
                return Err("输入过长".into());
            }
            for frame in &observation.frames {
                let password=evaluate(app,frame,"(()=>{let e=document.activeElement;while(e?.shadowRoot?.activeElement)e=e.shadowRoot.activeElement;return e?.type==='password'})()".into()).await?;
                if password == true {
                    return Err("密码请手动输入".into());
                }
            }
            check(app, s)?;
            cdp(
                app,
                "Input.insertText",
                json!({"text":text}),
                None,
                Some(s.cancel.clone()),
            )
            .await?;
        }
        Action::Click { frame, r#ref } | Action::Fill { frame, r#ref, .. } => {
            let p = point(app, observation, *frame, r#ref, false).await?;
            if let Action::Fill { text, .. } = action {
                if p["editable"] != true || p["password"] == true || text.len() > 16000 {
                    return Err("目标不是可填写字段或需要手动输入密码".into());
                }
                mouse(app, s, &p).await?;
                let target=&observation.frames[*frame];
                evaluate(app,target,format!("__novaWebview.focused({})",json!(r#ref))).await?;
                key(app, s, "Control+A").await?;
                evaluate(app,target,format!("__novaWebview.focused({})",json!(r#ref))).await?;
                check(app, s)?;
                cdp(
                    app,
                    "Input.insertText",
                    json!({"text":text}),
                    None,
                    Some(s.cancel.clone()),
                )
                .await?;
            } else {
                mouse(app, s, &p).await?;
            }
        }
        Action::Press { key: name } => key(app, s, name).await?,
        Action::Scroll {
            frame,
            r#ref,
            delta,
        } => {
            if delta.unsigned_abs() > 1200 {
                return Err("单次滚动不能超过 1200 像素".into());
            }
            if *frame != 0 && r#ref.is_none() { return Err("iframe 滚动需要明确滚动容器的 frame/ref".into()); }
            let p = if let Some(reference) = r#ref {
                point(app, observation, *frame, reference, true).await?
            } else {
                json!({"x":observation.pages["pages"][0]["viewport"]["width"].as_f64().unwrap_or(400.0)/2.0,"y":observation.pages["pages"][0]["viewport"]["height"].as_f64().unwrap_or(400.0)/2.0})
            };
            check(app, s)?;
            cdp(
                app,
                "Input.dispatchMouseEvent",
                json!({"type":"mouseWheel","x":p["x"],"y":p["y"],"deltaX":0,"deltaY":delta}),
                None,
                Some(s.cancel.clone()),
            )
            .await?;
        }
        Action::WaitFor { frame, r#ref, state, text, ms } => {
            let target=observation.frames.get(*frame).ok_or("模型选择了不存在的 frame")?;
            let ms=ms.unwrap_or(1500);
            if ms>2000 || !["visible","hidden","enabled","text"].contains(&state.as_str()) { return Err("无效的等待条件".into()); }
            if state=="text" && text.as_deref().is_none_or(|v|v.is_empty()) { return Err("等待文本不能为空".into()); }
            evaluate(app,target,format!("__novaWebview.waitFor({},{},{},{})",json!(r#ref),json!(state),json!(text.as_deref().unwrap_or("")),ms)).await?;
        }
        Action::Wait { ms } => tokio::time::sleep(Duration::from_millis((*ms).min(2000))).await,
    }
    check(app, s)
}

fn image_point(observation: &Observation, id: Option<&str>, x: f64, y: f64) -> Result<(f64,f64),String> {
    match id {
        Some(id) => observation.images.iter().find(|m|m.id==id).ok_or("imageId 不属于当前截图")?.point(x,y),
        None if x.is_finite() && y.is_finite() => Ok((x,y)),
        None => Err("坐标必须是有限数字".into()),
    }
}

async fn coordinate(
    app: &AppHandle,
    observation: &Observation,
    x: f64,
    y: f64,
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

async fn check_image_pixels(app:&AppHandle, observation:&Observation, x:f64, y:f64, point:&Value) -> Result<(),String> {
    use base64::Engine;
    let map=observation.images.iter().find(|m|x>=m.rect.x && y>=m.rect.y && x<m.rect.x+m.rect.width && y<m.rect.y+m.rect.height)
        .ok_or("坐标没有对应的已观察截图区域，请重新截图")?;
    let (px,py)=(point["x"].as_f64().ok_or("无效坐标")?,point["y"].as_f64().ok_or("无效坐标")?);
    let viewport=&observation.pages["pages"][0]["viewport"];
    let left=24f64.min(x-map.rect.x).min(px).floor();
    let top=24f64.min(y-map.rect.y).min(py).floor();
    let right=24f64.min(map.rect.x+map.rect.width-x).min(viewport["width"].as_f64().unwrap_or(0.)-px).floor();
    let bottom=24f64.min(map.rect.y+map.rect.height-y).min(viewport["height"].as_f64().unwrap_or(0.)-py).floor();
    if left+right<2. || top+bottom<2. {return Err("落点贴近截图边缘，请局部重截后定位".into());}
    let origin=evaluate(app,&observation.frames[0],"({x:scrollX,y:scrollY})".into()).await?;
    let shot=cdp(app,"Page.captureScreenshot",json!({"format":"png","fromSurface":true,"captureBeyondViewport":false,"optimizeForSpeed":true,
        "clip":{"x":origin["x"].as_f64().unwrap_or(0.)+px-left,"y":origin["y"].as_f64().unwrap_or(0.)+py-top,"width":left+right,"height":top+bottom,"scale":1}}),None,None).await?;
    let bytes=base64::engine::general_purpose::STANDARD.decode(shot["data"].as_str().ok_or("落点截图为空")?).map_err(|e|e.to_string())?;
    let current=xcap::image::load_from_memory(&bytes).map_err(|e|e.to_string())?.to_rgba8();
    let original=xcap::image::open(&map.path).map_err(|e|e.to_string())?.to_rgba8();
    let sx=map.pixels.0 as f64/map.rect.width;let sy=map.pixels.1 as f64/map.rect.height;
    let bx=((x-left-map.rect.x)*sx).round().max(0.) as u32;
    let by=((y-top-map.rect.y)*sy).round().max(0.) as u32;
    let bw=((left+right)*sx).round().max(1.) as u32;let bh=((top+bottom)*sy).round().max(1.) as u32;
    if bx+bw>original.width() || by+bh>original.height() {return Err("落点截图比例已变化，请重新截图".into());}
    let previous=xcap::image::imageops::crop_imm(&original,bx,by,bw,bh).to_image();
    let resize=|image:xcap::image::RgbaImage| xcap::image::DynamicImage::ImageRgba8(image).resize_exact(32,32,xcap::image::imageops::FilterType::Triangle).into_rgba8();
    let (previous,current)=(resize(previous),resize(current));
    let gx=(left/(left+right)*32.).floor().min(31.) as u32;
    let gy=(top/(top+bottom)*32.).floor().min(31.) as u32;
    if !visual_guard::matches(&previous,&current,gx,gy) {return Err("落点附近画面已变化（Canvas 重绘、滚动或遮挡）；未点击，请使用新截图重新定位".into());}
    Ok(())
}

fn screenshot_map(path:std::path::PathBuf,id:String,rect:Crop) -> Result<ImageMap,String> {
    let pixels=xcap::image::ImageReader::open(&path).map_err(|e|e.to_string())?.into_dimensions().map_err(|e|e.to_string())?;
    if pixels.0==0 || pixels.1==0 || (pixels.0 as u64)*(pixels.1 as u64)>64*1024*1024 { return Err("截图像素过大，请使用视口或局部 region".into()); }
    Ok(ImageMap{id,path,rect,pixels})
}

async fn snapshot(
    app: &AppHandle,
    with_image: bool,
    args: &Value,
) -> Result<(Observation, Value), String> {
    if args["fullPage"] == true && !args["region"].is_null() {
        return Err("局部 region 只能与 fullPage=false 一起使用".into());
    }
    let mut observation = observe(app,args["query"].as_str().unwrap_or("")).await?;
    let visual_required = observation.pages["pages"].as_array().is_some_and(|pages|pages.iter().any(|p|p["visualRequired"]==true));
    let with_image = with_image || (visual_required && args["includeVisual"] != false);
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
            page["matchingItems"] = json!(total);
            page["items"] = json!(kept);
            if let Some(headings) = page["headings"].as_array_mut() {
                headings.truncate(60);
            }
        }
    }
    result["snapshotId"] = json!(observation.id);
    result["documentPath"] = json!(document_path);
    result["scope"]=json!("整页已加载DOM，包含屏幕外和内部滚动区域；documentPath保留完整已采集文本、元素和引用（query存在时仅采集匹配元素）。inlineTruncated仅代表工具回复摘要被截短；truncated/coverageGaps表示采集本身不完整。");
    result["visualRequired"] = json!(visual_required);
    if with_image {
        use base64::Engine;
        // Canvas observations automatically include the viewport, never a huge full-document image.
        observation.full_page = args["region"].is_null() && args["fullPage"] != false && args["operation"] != "inspect";
        if visual_required && args["operation"] != "screenshot" && args["fullPage"].is_null() { observation.full_page=false; }
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
                let shot=cdp(app,"Page.captureScreenshot",json!({"format":"png","fromSurface":true,"captureBeyondViewport":true,"clip":{"x":x,"y":y,"width":w,"height":h,"scale":1}}),None,None).await?;
                let path = dir.join(format!("{}-{tile}.png", observation.id));
                std::fs::write(
                    &path,
                    base64::engine::general_purpose::STANDARD
                        .decode(shot["data"].as_str().ok_or("截图为空")?)
                        .map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string())?;
                let map = screenshot_map(path, format!("{}:{tile}",observation.id), Crop{x,y,width:w,height:h})?;
                let mut meta=map.metadata(); meta["tile"]=json!(tile);
                images.push(meta); observation.images.push(map);
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
            result["coordinateSpace"]=json!("CSS document pixels. Image coordinate -> tile.x + pixelX*tile.width/imagePixelWidth, tile.y + pixelY*tile.height/imagePixelHeight. click_at/move/scroll_at auto-scroll to the document point. Prefer DOM ref for offscreen elements; drag requires fullPage=false.");
            result["screenshotScope"]=json!("主文档整页；内部滚动区域/iframe的屏幕外内容用整页DOM读取。未加载图片或虚拟数据可能仍需定向滚动。");
        } else {
            let viewport = &observation.pages["pages"][0]["viewport"];
            let (width,height)=(viewport["width"].as_f64().ok_or("无效视口")?,viewport["height"].as_f64().ok_or("无效视口")?);
            let crop:Crop=if args["region"].is_null() { Crop{x:0.,y:0.,width,height} } else { serde_json::from_value(args["region"].clone()).map_err(|e|format!("无效 region：{e}"))? };
            crop.validate(width,height)?;
            let mut params=json!({"format":"png","fromSurface":true,"captureBeyondViewport":false,"optimizeForSpeed":true});
            if !args["region"].is_null() {
                params["clip"]=json!({"x":crop.x+viewport["scrollX"].as_f64().unwrap_or(0.),"y":crop.y+viewport["scrollY"].as_f64().unwrap_or(0.),"width":crop.width,"height":crop.height,"scale":1});
            }
            let shot=cdp(app,"Page.captureScreenshot",params,None,None).await?;
            let path=dir.join(format!("{}.png",observation.id));
            std::fs::write(&path,base64::engine::general_purpose::STANDARD.decode(shot["data"].as_str().ok_or("截图为空")?).map_err(|e|e.to_string())?).map_err(|e|e.to_string())?;
            let map=screenshot_map(path,format!("{}:0",observation.id),crop)?;
            images.push(map.metadata()); observation.images.push(map);
            result["coordinateSpace"] =
                json!("Recommended: action.imageId + coordinates in that image's actual pixels; the tool maps crop offset and DPI. Without imageId: CSS viewport pixels (legacy).");
        }
        let after=evaluate(app,&observation.frames[0],"__novaWebview.stamp()".into()).await?;
        if after != observation.pages["pages"][0]["stamp"] { return Err("截图期间视口已变化，请重新观察".into()); }
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
    let trace=Arc::new(Mutex::new(InputTrace::default()));
    let task=INPUT_TRACE.scope(trace.clone(),CONTROL_TAB.scope(s.active_tab.clone(),async {
        if operation!="act" {let mut request=args.clone();request["operation"]=json!(operation);return Ok(snapshot(app,operation=="screenshot",&request).await?.1);}
        let observation=state.observations.lock().unwrap().get(&s.active_tab).cloned().ok_or("请先 inspect 或 screenshot")?;
        if args["snapshotId"].as_str()!=Some(&observation.id) { return Err("观察已失效：snapshotId 不是最新观察或已执行；使用最近返回的 snapshotId，不要重放动作".into()); }
        let action=parse_action(&args["action"].to_string())?;
        let dom_target = matches!(&action, Action::Click { .. } | Action::Fill { .. } | Action::Scroll { r#ref: Some(_), .. });
        if !dom_target && observation.captured.elapsed()>Duration::from_secs(180) {
            return Err(format!("观察已过期：已过去 {} 秒，有效期180秒；重新观察后继续，不是 DOM 动画错误", observation.captured.elapsed().as_secs()));
        }
        state.observations.lock().unwrap().remove(&s.active_tab);
        let mut result = match apply(app,&s,&observation,&action).await {
            Err(error) => {
                // Status follows actual dispatch, not substrings in an error (focus may fail AFTER a click).
                let not_executed=!trace.lock().unwrap().sent;
                json!({"status":if not_executed {"not_executed"} else {"needs_review"},"reason":error.lines().next().unwrap_or(&error)})
            },
            Ok(()) => json!({"status":"executed"}),
        };
        result["actionMs"] = json!(started.elapsed().as_millis());
        result["next"] = json!("根据返回的最新状态验证并继续；fill 一次完成聚焦和填写。executed/needs_review 不要直接重放。坐标操作需截图，DOM 操作使用最新 frame/ref。");
        completed_action = Some(result.clone());
        if args["feedback"] != "none" && !s.cancel.load(Ordering::SeqCst) {
            let mut feedback_args = args.clone();
            feedback_args["fullPage"] = json!(false);
            feedback_args["region"] = Value::Null;
            // Feedback failure must never turn a completed mutation into a retryable action failure.
            match tokio::time::timeout(Duration::from_secs(3), snapshot(app,args["feedback"]=="screenshot",&feedback_args)).await {
                Ok(Ok((_, feedback))) => result.as_object_mut().unwrap().extend(feedback.as_object().unwrap().clone()),
                Ok(Err(error)) => result["observationError"] = json!(error),
                Err(_) => result["observationError"] = json!("动作后观察3秒超时；动作状态如上，请观察确认，不要重放"),
            }
        }
        Ok(result)
    }));
    let mut result = tokio::select! {
        r=tokio::time::timeout(Duration::from_secs(15),task)=>r.map_err(|_|"浏览器操作15秒超时，已停止；请重新观察确认结果".to_string()).and_then(|r|r),
        _=async {while !s.cancel.load(Ordering::SeqCst) {tokio::time::sleep(Duration::from_millis(30)).await;}}=>Err("已停止浏览器操作，请检查页面确认结果".into()),
    };
    if let (Err(error), Some(mut completed)) = (&result, completed_action) {
        completed["observationError"] = json!(error);
        result = Ok(completed);
    }
    if result.is_err() && trace.lock().unwrap().sent {
        result=Ok(json!({"status":"needs_review","reason":result.as_ref().err(),"verification":"unverified","next":"输入可能已经发送；重新观察，禁止自动重放"}));
    }
    // Cancellation may drop a future between down/up. Release only what THIS operation held.
    let (pending_mouse,pending_key)={let mut t=trace.lock().unwrap();(t.mouse.take(),t.key.take())};
    let cleanup=CONTROL_TAB.scope(s.active_tab.clone(),async {
        let mut errors=Vec::new();
        if let Some(mut p)=pending_mouse {p["type"]=json!("mouseReleased");p["buttons"]=json!(0);if let Err(e)=cdp(app,"Input.dispatchMouseEvent",p,None,None).await {errors.push(e);}}
        if let Some(mut p)=pending_key {p["type"]=json!("keyUp");if let Err(e)=cdp(app,"Input.dispatchKeyEvent",p,None,None).await {errors.push(e);}}
        errors
    }).await;
    if let Ok(value)=&mut result {
        value["cdpCalls"]=json!(trace.lock().unwrap().calls);
        if !cleanup.is_empty() {value["releaseErrors"]=json!(cleanup);value["status"]=json!("needs_review");}
    }
    s.cancel.store(true, Ordering::SeqCst);
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
            json!({"browserId":value["browserId"],"status":"opening","next":"页面加载完成后 inspect/screenshot，再用 act 操作"}),
        );
    }
    let id = args["browserId"]
        .as_str()
        .ok_or("缺少 open 返回的 browserId")?;
    let s = session(app, id)?;
    if s.thread_id != thread_id {
        return Err("浏览器属于其它会话".into());
    }
    match operation {
        "stop" => {
            s.cancel.store(true, Ordering::SeqCst);
            Ok(json!({"stopped":true}))
        }
        "tabs" => Ok(json!(s)),
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

pub(crate) async fn execute_chrome(root: &Path, args: &Value, owner: &str) -> Result<Value, String> {
    let app = APP.get().ok_or("网页工具仅在 Nova 桌面应用内可用")?;
    let thread_id = tool_owner(root, owner)?;
    let operation = args["operation"].as_str().unwrap_or_default();
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
    if matches!(operation, "connect" | "status") {
        return Ok(connection);
    }
    if matches!(operation, "tabs" | "open" | "new_tab") {
        let mut params = args.clone();
        if let Some(url) = args["url"].as_str() {
            params["url"] = json!(normalized_url(url)?.to_string());
        }
        return crate::chrome_browser::request(app, operation, params).await;
    }
    let tag = args["tabTag"]
        .as_str()
        .filter(|tag| {
            tag.len() <= 80
                && tag.starts_with('C')
                && tag.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
        .ok_or("缺少有效 tabTag；请先 chrome.tabs，不会默认操作当前激活标签")?;
    if operation == "stop" {
        crate::chrome_browser::stop(app, tag);
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
        let value = crate::chrome_browser::request(app, operation, params).await?;
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
        return Ok(value);
    }
    if !matches!(operation, "inspect" | "screenshot" | "act") {
        return Err("未知 chrome 操作".into());
    }
    let s = Session {
        browser_id: "chrome".into(),
        thread_id: thread_id.clone(),
        url: String::new(),
        visible: true,
        busy: false,
        status: String::new(),
        active_tab: format!("chrome:{thread_id}:{tag}"),
        tabs: Vec::new(),
        bounds: None,
        cancel: Arc::new(AtomicBool::new(false)),
    };
    let mut value = control_session(app, s, operation, args).await?;
    value["tabTag"] = json!(tag);
    value["browser"] = json!("chrome");
    Ok(value)
}

fn state_dir(app: &AppHandle) -> std::path::PathBuf {
    app.state::<AppState>().config_dir.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
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
    }
}
