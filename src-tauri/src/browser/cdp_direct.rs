//! Direct native CDP WebSocket engine for Headless Chrome (zero extension overhead!).
//! Manages the headless Chrome lifecycle, multiplexed CDP WebSocket, event-driven navigation
//! waits, virtual cursor execution, iframe mapping, and live Page.startScreencast sidebar frames.
use crate::browser::protocol::{key_spec, normalized_url, upload_files, RawActResult, RawView, Step, TabInfo, ViewOptions, PAGE_RUNTIME};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
    process::{Child, Command},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::{mpsc, oneshot, Notify};
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};

const RETRY_AFTER: Duration = Duration::from_secs(15);
const TABS_PER_SESSION: usize = 6;
const IDLE_RECLAIM: Duration = Duration::from_secs(15 * 60);
const DEVTOOLS_LIMIT: usize = 500;
const EV_OPENED: &str = "headless:opened";
pub(crate) const EV_FRAME: &str = "headless:frame";

#[derive(Clone, Debug)]
struct TargetEntry {
    target_id: String,
    tag: String,
    title: String,
    url: String,
    opener_id: Option<String>,
    created_at: Instant,
}

#[derive(Clone, Debug)]
struct FrameRoute {
    frame_id: String,
    session_id: Option<String>,
    parent_chain: Vec<(String, Option<String>)>, // (child_frame_id, parent_session_id)
}

#[derive(Default)]
struct DevtoolsLog {
    since: i64,
    console: VecDeque<Value>,
    network: HashMap<String, Value>,
    network_order: VecDeque<String>,
}

#[derive(Default)]
struct SessionMeta {
    main_frame_id: String,
    /// `(cdp_session_or_empty, frame_id) -> executionContextId`
    contexts: HashMap<(String, String), i64>,
    loading: bool,
    last_nav: Option<Instant>,
    cursor: Option<(f64, f64)>,
    cursor_active: bool,
    /// Child OOPIF targets attached under this tab: `target_id -> session_id`
    child_sessions: HashMap<String, String>,
    /// Last observed element IDs belonging to sub-frames: `element_id -> FrameRoute`
    frame_routes: HashMap<u32, FrameRoute>,
    overlay_frame: Option<FrameRoute>,
    scroll_frame: Option<FrameRoute>,
    devtools: Option<DevtoolsLog>,
}

struct ScreencastState {
    session_id: String,
    tab_tag: String,
    thread_id: Option<String>,
    last_heartbeat: Instant,
    last_emit: Instant,
    latest_b64: Option<String>,
}

struct CdpInner {
    tx: mpsc::UnboundedSender<String>,
    next_id: AtomicU64,
    alive: AtomicBool,
    pending: Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>,
    /// `target_id -> TargetEntry`
    targets: Mutex<HashMap<String, TargetEntry>>,
    /// `tag -> target_id`
    tag_to_target: Mutex<HashMap<String, String>>,
    next_tag: AtomicU64,
    /// `target_id -> cdp_session_id`
    target_sessions: Mutex<HashMap<String, String>>,
    /// `cdp_session_id -> SessionMeta`
    sessions: Mutex<HashMap<String, SessionMeta>>,
    /// Active tab tag (last focused/opened)
    active_tag: Mutex<Option<String>>,
    load_notify: Notify,
    screencast: Mutex<Option<ScreencastState>>,
    app: Option<AppHandle>,
}

#[derive(Clone)]
pub(crate) struct CdpClient {
    inner: Arc<CdpInner>,
}

impl CdpClient {
    pub(crate) async fn connect(ws_url: &str, app: Option<AppHandle>) -> Result<Self, String> {
        let req = ws_url.into_client_request().map_err(|e| e.to_string())?;
        let parsed = tauri::Url::parse(ws_url).map_err(|e| e.to_string())?;
        let host = parsed.host_str().unwrap_or("127.0.0.1");
        let port = parsed.port().ok_or("CDP WebSocket 缺少端口")?;
        let stream = tokio::net::TcpStream::connect((host, port))
            .await
            .map_err(|e| format!("连接无头浏览器 CDP 失败：{e}"))?;
        let _ = stream.set_nodelay(true);
        let (ws, _) = tokio_tungstenite::client_async(req, stream)
            .await
            .map_err(|e| format!("无头浏览器 CDP 握手失败：{e}"))?;
        let (mut sink, mut source) = ws.split();
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();

        let inner = Arc::new(CdpInner {
            tx,
            next_id: AtomicU64::new(1),
            alive: AtomicBool::new(true),
            pending: Mutex::new(HashMap::new()),
            targets: Mutex::new(HashMap::new()),
            tag_to_target: Mutex::new(HashMap::new()),
            next_tag: AtomicU64::new(1),
            target_sessions: Mutex::new(HashMap::new()),
            sessions: Mutex::new(HashMap::new()),
            active_tag: Mutex::new(None),
            load_notify: Notify::new(),
            screencast: Mutex::new(None),
            app,
        });

        // Writer task
        let write_inner = inner.clone();
        tauri::async_runtime::spawn(async move {
            while let Some(msg) = rx.recv().await {
                if sink.send(Message::Text(msg.into())).await.is_err() {
                    break;
                }
            }
            write_inner.alive.store(false, Ordering::SeqCst);
        });

        // Reader & event router task
        let read_inner = inner.clone();
        tauri::async_runtime::spawn(async move {
            while let Some(Ok(msg)) = source.next().await {
                let text = match msg {
                    Message::Text(t) => t,
                    Message::Close(_) => break,
                    _ => continue,
                };
                let Ok(val) = serde_json::from_str::<Value>(text.as_str()) else { continue };
                if let Some(id) = val.get("id").and_then(Value::as_u64) {
                    if let Some(sender) = read_inner.pending.lock().unwrap().remove(&id) {
                        let reply = if let Some(err) = val.get("error") {
                            let msg = err["message"].as_str().unwrap_or("CDP 错误");
                            Err(msg.to_string())
                        } else {
                            Ok(val.get("result").cloned().unwrap_or(json!({})))
                        };
                        let _ = sender.send(reply);
                    }
                } else if let Some(method) = val.get("method").and_then(Value::as_str) {
                    let session_id = val.get("sessionId").and_then(Value::as_str);
                    let params = val.get("params").unwrap_or(&Value::Null);
                    handle_event(&read_inner, session_id, method, params);
                }
            }
            read_inner.alive.store(false, Ordering::SeqCst);
            let mut pending = read_inner.pending.lock().unwrap();
            for (_, tx) in pending.drain() {
                let _ = tx.send(Err("无头浏览器 CDP 连接已断开".into()));
            }
        });

        let client = Self { inner };
        // Start passive target discovery so tab list is always up-to-date with 0 polling.
        client.call("Target.setDiscoverTargets", json!({"discover": true}), None).await?;
        if let Ok(list) = client.call("Target.getTargets", json!({}), None).await {
            if let Some(infos) = list["targetInfos"].as_array() {
                for info in infos {
                    upsert_target(&client.inner, info);
                }
            }
        }
        Ok(client)
    }

    pub(crate) fn is_alive(&self) -> bool {
        self.inner.alive.load(Ordering::SeqCst)
    }

    pub(crate) async fn call(&self, method: &str, params: Value, session_id: Option<&str>) -> Result<Value, String> {
        if !self.is_alive() {
            return Err("无头浏览器已断开".into());
        }
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.inner.pending.lock().unwrap().insert(id, tx);
        let mut payload = json!({ "id": id, "method": method, "params": params });
        if let Some(sid) = session_id {
            payload["sessionId"] = json!(sid);
        }
        if self.inner.tx.send(payload.to_string()).is_err() {
            self.inner.pending.lock().unwrap().remove(&id);
            return Err("无头浏览器连接已关闭".into());
        }
        match tokio::time::timeout(Duration::from_secs(12), rx).await {
            Ok(Ok(res)) => res,
            Ok(Err(_)) => Err("无头浏览器响应中断".into()),
            Err(_) => {
                self.inner.pending.lock().unwrap().remove(&id);
                Err(format!("CDP {method} 超时"))
            }
        }
    }

    fn resolve_target_id(&self, tag: &str) -> Result<String, String> {
        self.inner
            .tag_to_target
            .lock()
            .unwrap()
            .get(tag)
            .cloned()
            .ok_or_else(|| format!("标签 {tag} 不存在或已关闭"))
    }

    pub(crate) fn list_tabs(&self) -> Vec<TabInfo> {
        let active = self.inner.active_tag.lock().unwrap().clone();
        let targets = self.inner.targets.lock().unwrap();
        let sessions = self.inner.sessions.lock().unwrap();
        let target_sessions = self.inner.target_sessions.lock().unwrap();
        let id_to_tag: HashMap<&str, &str> = targets.iter().map(|(id, t)| (id.as_str(), t.tag.as_str())).collect();
        let mut list: Vec<(&TargetEntry, TabInfo)> = targets
            .values()
            .map(|t| {
                let loading = target_sessions
                    .get(&t.target_id)
                    .and_then(|sid| sessions.get(sid))
                    .is_some_and(|s| s.loading);
                let controllable = t.url.starts_with("http://") || t.url.starts_with("https://") || t.url == "about:blank";
                let info = TabInfo {
                    tag: t.tag.clone(),
                    title: t.title.clone(),
                    url: t.url.clone(),
                    active: active.as_deref() == Some(t.tag.as_str()),
                    incognito: false,
                    loading,
                    controllable,
                    opener_tag: t.opener_id.as_deref().and_then(|o| id_to_tag.get(o).copied()).map(str::to_string),
                };
                (t, info)
            })
            .collect();
        list.sort_by_key(|(t, _)| t.created_at);
        list.into_iter().map(|(_, info)| info).collect()
    }

    async fn ensure_session(&self, tag: &str) -> Result<String, String> {
        let target_id = self.resolve_target_id(tag)?;
        if let Some(sid) = self.inner.target_sessions.lock().unwrap().get(&target_id).cloned() {
            return Ok(sid);
        }
        let attached = self
            .call("Target.attachToTarget", json!({"targetId": target_id, "flatten": true}), None)
            .await?;
        let sid = attached["sessionId"]
            .as_str()
            .ok_or("无法附加到无头标签")?
            .to_string();
        let _ = self.call("Page.enable", json!({}), Some(&sid)).await;
        let _ = self
            .call(
                "Page.addScriptToEvaluateOnNewDocument",
                json!({"source": PAGE_RUNTIME, "worldName": "nova"}),
                Some(&sid),
            )
            .await;
        let tree = self.call("Page.getFrameTree", json!({}), Some(&sid)).await?;
        let main_frame_id = tree["frameTree"]["frame"]["id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        {
            let mut ts = self.inner.target_sessions.lock().unwrap();
            ts.insert(target_id.clone(), sid.clone());
            let mut sm = self.inner.sessions.lock().unwrap();
            sm.insert(
                sid.clone(),
                SessionMeta {
                    main_frame_id,
                    ..Default::default()
                },
            );
        }
        Ok(sid)
    }

    async fn ensure_world(&self, tab_sid: &str, cdp_sid: &str, frame_id: &str) -> Result<i64, String> {
        let key = (cdp_sid.to_string(), frame_id.to_string());
        if let Some(ctx) = self
            .inner
            .sessions
            .lock()
            .unwrap()
            .get(tab_sid)
            .and_then(|m| m.contexts.get(&key).copied())
        {
            return Ok(ctx);
        }
        let created = self
            .call(
                "Page.createIsolatedWorld",
                json!({"frameId": frame_id, "worldName": "nova"}),
                Some(cdp_sid),
            )
            .await?;
        let ctx = created["executionContextId"]
            .as_i64()
            .ok_or("创建页面隔离环境失败")?;
        // Idempotent ensure: immediately injects runtime if page was loaded before attach.
        let _ = self
            .call(
                "Runtime.evaluate",
                json!({"expression": PAGE_RUNTIME, "contextId": ctx, "returnByValue": false}),
                Some(cdp_sid),
            )
            .await;
        // Restore virtual cursor immediately in top frame after navigation while session is active.
        let (saved_cursor, cursor_active) = self
            .inner
            .sessions
            .lock()
            .unwrap()
            .get(tab_sid)
            .filter(|m| m.main_frame_id == frame_id && cdp_sid == tab_sid)
            .map(|m| (m.cursor, m.cursor_active))
            .unwrap_or((None, false));
        if cursor_active || saved_cursor.is_some() {
            let expr = match (cursor_active, saved_cursor) {
                (true, Some((cx, cy))) => format!("__novaBrowser.cursor.show({cx},{cy})"),
                (true, None) => "__novaBrowser.cursor.show()".into(),
                (false, Some((cx, cy))) => format!("__novaBrowser.cursor.sync({cx},{cy})"),
                (false, None) => String::new(),
            };
            if !expr.is_empty() {
                let _ = self
                    .call("Runtime.evaluate", json!({"expression": expr, "contextId": ctx}), Some(cdp_sid))
                    .await;
            }
        }
        if let Some(meta) = self.inner.sessions.lock().unwrap().get_mut(tab_sid) {
            meta.contexts.insert(key, ctx);
        }
        Ok(ctx)
    }

    async fn eval_in_frame(&self, tab_sid: &str, cdp_sid: &str, frame_id: &str, expr: &str) -> Result<Value, String> {
        for attempt in 0..2 {
            let ctx = self.ensure_world(tab_sid, cdp_sid, frame_id).await?;
            let reply = self
                .call(
                    "Runtime.evaluate",
                    json!({
                        "expression": expr,
                        "contextId": ctx,
                        "returnByValue": true,
                        "awaitPromise": true
                    }),
                    Some(cdp_sid),
                )
                .await;
            match reply {
                Ok(val) => {
                    if let Some(exc) = val.get("exceptionDetails") {
                        let desc = exc["exception"]["description"]
                            .as_str()
                            .or_else(|| exc["text"].as_str())
                            .unwrap_or("页面脚本执行失败");
                        if attempt == 0 && (desc.contains("__novaBrowser") || desc.contains("Cannot find context")) {
                            if let Some(m) = self.inner.sessions.lock().unwrap().get_mut(tab_sid) {
                                m.contexts.remove(&(cdp_sid.to_string(), frame_id.to_string()));
                            }
                            continue;
                        }
                        return Err(desc.to_string());
                    }
                    return Ok(val["result"]["value"].clone());
                }
                Err(err)
                    if attempt == 0
                        && (err.contains("Cannot find context")
                            || err.contains("Execution context was destroyed")) =>
                {
                    if let Some(m) = self.inner.sessions.lock().unwrap().get_mut(tab_sid) {
                        m.contexts.remove(&(cdp_sid.to_string(), frame_id.to_string()));
                    }
                }
                Err(err) => return Err(err),
            }
        }
        Err("页面上下文重建失败".into())
    }

    fn main_frame(&self, tab_sid: &str) -> Result<String, String> {
        self.inner
            .sessions
            .lock()
            .unwrap()
            .get(tab_sid)
            .map(|m| m.main_frame_id.clone())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| "主框架未就绪".into())
    }

    pub(crate) async fn open_tab(&self, url: &str) -> Result<TabInfo, String> {
        let href = if url.is_empty() || url == "about:blank" {
            "about:blank".to_string()
        } else {
            normalized_url(url)?.to_string()
        };
        let created = self
            .call("Target.createTarget", json!({"url": href}), None)
            .await?;
        let target_id = created["targetId"].as_str().ok_or("创建标签失败")?.to_string();
        // Register immediately even before Target.targetCreated arrives.
        let tag = upsert_target(
            &self.inner,
            &json!({"targetId": target_id, "type": "page", "url": href, "title": ""}),
        );
        *self.inner.active_tag.lock().unwrap() = Some(tag.clone());
        let sid = self.ensure_session(&tag).await?;
        if href != "about:blank" {
            self.wait_for_load(&sid, Duration::from_millis(3500)).await;
        }
        Ok(self
            .list_tabs()
            .into_iter()
            .find(|t| t.tag == tag)
            .unwrap_or(TabInfo {
                tag,
                title: String::new(),
                url: href,
                active: true,
                incognito: false,
                loading: false,
                controllable: true,
                opener_tag: None,
            }))
    }

    pub(crate) async fn close_tab(&self, tag: &str) -> Result<(), String> {
        let target_id = self.resolve_target_id(tag)?;
        let _ = self.call("Target.closeTarget", json!({"targetId": target_id}), None).await;
        remove_target(&self.inner, &target_id);
        Ok(())
    }

    pub(crate) async fn select_tab(&self, tag: &str) -> Result<(), String> {
        let target_id = self.resolve_target_id(tag)?;
        let _ = self.call("Target.activateTarget", json!({"targetId": target_id}), None).await;
        *self.inner.active_tag.lock().unwrap() = Some(tag.to_string());
        Ok(())
    }

    pub(crate) async fn goto(&self, tag: &str, url: &str) -> Result<(), String> {
        let href = normalized_url(url)?.to_string();
        let sid = self.ensure_session(tag).await?;
        if let Some(m) = self.inner.sessions.lock().unwrap().get_mut(&sid) {
            m.loading = true;
            m.contexts.clear();
        }
        self.call("Page.navigate", json!({"url": href}), Some(&sid)).await?;
        *self.inner.active_tag.lock().unwrap() = Some(tag.to_string());
        self.wait_for_load(&sid, Duration::from_millis(3500)).await;
        Ok(())
    }

    pub(crate) async fn navigate_history(&self, tag: &str, delta: i32) -> Result<(), String> {
        let sid = self.ensure_session(tag).await?;
        let history = self.call("Page.getNavigationHistory", json!({}), Some(&sid)).await?;
        let current = history["currentIndex"].as_i64().unwrap_or(0);
        let entries = history["entries"].as_array().ok_or("无导航历史")?;
        let next = (current + delta as i64) as usize;
        let entry_id = entries
            .get(next)
            .and_then(|e| e["id"].as_i64())
            .ok_or(if delta < 0 { "已到最早历史记录" } else { "已到最新历史记录" })?;
        if let Some(m) = self.inner.sessions.lock().unwrap().get_mut(&sid) {
            m.loading = true;
            m.contexts.clear();
        }
        self.call("Page.navigateToHistoryEntry", json!({"entryId": entry_id}), Some(&sid)).await?;
        self.wait_for_load(&sid, Duration::from_millis(3000)).await;
        Ok(())
    }

    pub(crate) async fn reload(&self, tag: &str) -> Result<(), String> {
        let sid = self.ensure_session(tag).await?;
        if let Some(m) = self.inner.sessions.lock().unwrap().get_mut(&sid) {
            m.loading = true;
            m.contexts.clear();
        }
        self.call("Page.reload", json!({}), Some(&sid)).await?;
        self.wait_for_load(&sid, Duration::from_millis(3500)).await;
        Ok(())
    }

    async fn wait_for_load(&self, sid: &str, budget: Duration) {
        let deadline = Instant::now() + budget;
        loop {
            let loading = self
                .inner
                .sessions
                .lock()
                .unwrap()
                .get(sid)
                .is_some_and(|m| m.loading);
            if !loading {
                // Quick readyState sanity check
                if let Ok(res) = self
                    .call(
                        "Runtime.evaluate",
                        json!({"expression": "document.readyState", "returnByValue": true}),
                        Some(sid),
                    )
                    .await
                {
                    let state = res["result"]["value"].as_str().unwrap_or("complete");
                    if state == "complete" || state == "interactive" {
                        break;
                    }
                } else {
                    break;
                }
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            let notified = self.inner.load_notify.notified();
            let _ = tokio::time::timeout(left.min(Duration::from_millis(120)), notified).await;
        }
        // Brief paint/SPA hydration settle (max 250ms)
        if let Ok(frame) = self.main_frame(sid) {
            let _ = self.eval_in_frame(sid, sid, &frame, "__novaBrowser.waitQuiet(100, 260)").await;
        }
    }

    /// Compute live offset `(ox, oy, sx, sy)` of a nested iframe chain in top-level CSS pixels.
    async fn iframe_transform(&self, tab_sid: &str, chain: &[(String, Option<String>)]) -> Result<(f64, f64, f64, f64), String> {
        let (mut ox, mut oy, mut sx, mut sy) = (0.0, 0.0, 1.0, 1.0);
        // Walk from innermost to outermost, accumulating linear scale & offset.
        for (child_frame_id, parent_session) in chain {
            let p_sid = parent_session.as_deref().unwrap_or(tab_sid);
            let owner = self
                .call("DOM.getFrameOwner", json!({"frameId": child_frame_id}), Some(p_sid))
                .await?;
            let backend = owner["backendNodeId"].as_i64().ok_or("框架元素不可访问")?;
            let resolved = self
                .call("DOM.resolveNode", json!({"backendNodeId": backend}), Some(p_sid))
                .await?;
            let obj_id = resolved["object"]["objectId"].as_str().ok_or("无法解析框架节点")?;
            let rect_res = self
                .call(
                    "Runtime.callFunctionOn",
                    json!({
                        "objectId": obj_id,
                        "returnByValue": true,
                        "functionDeclaration": "function(){const r=this.getBoundingClientRect();const sx=this.offsetWidth?r.width/this.offsetWidth:1;const sy=this.offsetHeight?r.height/this.offsetHeight:1;return [r.x+this.clientLeft*sx,r.y+this.clientTop*sy,sx,sy];}"
                    }),
                    Some(p_sid),
                )
                .await;
            let _ = self.call("Runtime.releaseObject", json!({"objectId": obj_id}), Some(p_sid)).await;
            let arr = rect_res?["result"]["value"].as_array().cloned().ok_or("框架坐标无效")?;
            let fx = arr.first().and_then(Value::as_f64).unwrap_or(0.0);
            let fy = arr.get(1).and_then(Value::as_f64).unwrap_or(0.0);
            let fsx = arr.get(2).and_then(Value::as_f64).unwrap_or(1.0);
            let fsy = arr.get(3).and_then(Value::as_f64).unwrap_or(1.0);
            ox = fx + ox * fsx;
            oy = fy + oy * fsy;
            sx *= fsx;
            sy *= fsy;
        }
        Ok((ox, oy, sx, sy))
    }

    pub(crate) async fn view(&self, tag: &str, opts: &ViewOptions) -> Result<RawView, String> {
        let sid = self.ensure_session(tag).await?;
        let main_frame = self.main_frame(&sid)?;
        let saved_cursor = {
            let mut sessions = self.inner.sessions.lock().unwrap();
            if let Some(m) = sessions.get_mut(&sid) {
                m.cursor_active = true;
                m.cursor
            } else {
                None
            }
        };
        let show_expr = match saved_cursor {
            Some((cx, cy)) => format!("__novaBrowser?.cursor?.show?.({cx},{cy})"),
            None => "__novaBrowser?.cursor?.show?.()".into(),
        };
        let _ = self.eval_in_frame(&sid, &sid, &main_frame, &show_expr).await;

        // Optional zoom_id resolution before fresh observe (uses existing live WeakRef).
        let mut zoom_css = opts.zoom_rect;
        if let Some(zid) = opts.zoom_id {
            if let Ok(resolved) = self.resolve_element(&sid, zid).await {
                if let Some(arr) = resolved["rect"].as_array() {
                    if arr.len() == 4 {
                        zoom_css = Some([
                            arr[0].as_f64().unwrap_or(0.0),
                            arr[1].as_f64().unwrap_or(0.0),
                            arr[2].as_f64().unwrap_or(0.0),
                            arr[3].as_f64().unwrap_or(0.0),
                        ]);
                    }
                }
            }
        }

        let obs_arg = json!({
            "scope": opts.scope.as_deref().unwrap_or("viewport"),
            "query": opts.query.as_deref().unwrap_or(""),
            "maxItems": opts.max_items.unwrap_or(100),
            "maxTextChars": opts.max_text_chars.unwrap_or(3000),
            "idOffset": 0
        });
        let main_val = self
            .eval_in_frame(&sid, &sid, &main_frame, &format!("__novaBrowser.observe({obs_arg})"))
            .await?;

        let mut items = main_val["items"].as_array().cloned().unwrap_or_default();
        let mut text = main_val["text"].as_str().unwrap_or_default().to_string();
        let mut overlay = (!main_val["overlay"].is_null()).then(|| main_val["overlay"].clone());
        let mut scroll_info = (!main_val["scrollInfo"].is_null()).then(|| main_val["scrollInfo"].clone());
        let mut frame_routes: HashMap<u32, FrameRoute> = HashMap::new();
        let mut overlay_frame: Option<FrameRoute> = None;
        let mut scroll_frame: Option<FrameRoute> = None;

        // Only probe child iframes if the main page actually has an <iframe> (0ms overhead otherwise!).
        if main_val["hasIframes"] == true {
            let _ = self
                .call(
                    "Target.setAutoAttach",
                    json!({"autoAttach": true, "waitForDebuggerOnStart": false, "flatten": true}),
                    Some(&sid),
                )
                .await;
            if let Ok(tree) = self.call("Page.getFrameTree", json!({}), Some(&sid)).await {
                let mut children: Vec<(String, Option<String>, Vec<(String, Option<String>)>)> = Vec::new();
                collect_child_frames(&tree["frameTree"], None, &[], &mut children);
                // Include OOPIF child sessions too
                let child_sids: Vec<String> = self
                    .inner
                    .sessions
                    .lock()
                    .unwrap()
                    .get(&sid)
                    .map(|m| m.child_sessions.values().cloned().collect())
                    .unwrap_or_default();
                for csid in child_sids {
                    if let Ok(ctree) = self.call("Page.getFrameTree", json!({}), Some(&csid)).await {
                        if let Some(fid) = ctree["frameTree"]["frame"]["id"].as_str() {
                            if !children.iter().any(|c| c.0 == fid) {
                                children.push((fid.to_string(), Some(csid.clone()), vec![(fid.to_string(), None)]));
                            }
                        }
                    }
                }
                let mut offset = items.iter().filter_map(|i| i["id"].as_u64()).max().unwrap_or(0) as u32;
                for (fid, cdp_sid, chain) in children.into_iter().take(6) {
                    let Ok((ox, oy, sx, sy)) = self.iframe_transform(&sid, &chain).await else { continue };
                    let target_sid = cdp_sid.as_deref().unwrap_or(&sid);
                    let f_arg = json!({
                        "scope": opts.scope.as_deref().unwrap_or("viewport"),
                        "query": opts.query.as_deref().unwrap_or(""),
                        "maxItems": 40,
                        "maxTextChars": 1000,
                        "idOffset": offset
                    });
                    if let Ok(fval) = self
                        .eval_in_frame(&sid, target_sid, &fid, &format!("__novaBrowser.observe({f_arg})"))
                        .await
                    {
                        let route_obj = FrameRoute {
                            frame_id: fid.clone(),
                            session_id: cdp_sid.clone(),
                            parent_chain: chain.clone(),
                        };
                        if let Some(fitems) = fval["items"].as_array() {
                            for mut item in fitems.iter().cloned() {
                                if let Some(id) = item["id"].as_u64().map(|n| n as u32) {
                                    offset = offset.max(id);
                                    let lx = item["x"].as_f64().unwrap_or(0.0);
                                    let ly = item["y"].as_f64().unwrap_or(0.0);
                                    item["x"] = json!((ox + lx * sx).round());
                                    item["y"] = json!((oy + ly * sy).round());
                                    if let Some(r) = item["rect"].as_array() {
                                        if r.len() == 4 {
                                            item["rect"] = json!([
                                                (ox + r[0].as_f64().unwrap_or(0.0) * sx).round(),
                                                (oy + r[1].as_f64().unwrap_or(0.0) * sy).round(),
                                                (r[2].as_f64().unwrap_or(0.0) * sx).round(),
                                                (r[3].as_f64().unwrap_or(0.0) * sy).round(),
                                            ]);
                                        }
                                    }
                                    frame_routes.insert(id, route_obj.clone());
                                    items.push(item);
                                }
                            }
                        }
                        if overlay.is_none() && !fval["overlay"].is_null() {
                            let mut fov = fval["overlay"].clone();
                            if let Some(r) = fov["rect"].as_array().filter(|a| a.len() == 4) {
                                fov["rect"] = json!([
                                    (ox + r[0].as_f64().unwrap_or(0.0) * sx).round(),
                                    (oy + r[1].as_f64().unwrap_or(0.0) * sy).round(),
                                    (r[2].as_f64().unwrap_or(0.0) * sx).round(),
                                    (r[3].as_f64().unwrap_or(0.0) * sy).round(),
                                ]);
                            }
                            if let Some(pt) = fov["outsidePoint"].as_array().filter(|a| a.len() == 2) {
                                fov["outsidePoint"] = json!([
                                    (ox + pt[0].as_f64().unwrap_or(0.0) * sx).round(),
                                    (oy + pt[1].as_f64().unwrap_or(0.0) * sy).round(),
                                ]);
                            }
                            overlay = Some(fov);
                            overlay_frame = Some(route_obj.clone());
                        }
                        let fvp_w = fval["viewport"].get(0).and_then(Value::as_f64).unwrap_or(0.0) * sx;
                        let fvp_h = fval["viewport"].get(1).and_then(Value::as_f64).unwrap_or(0.0) * sy;
                        let f_more = fval["scrollInfo"]["moreBelow"].as_i64().unwrap_or(0);
                        let cur_more = scroll_info.as_ref().and_then(|s| s["moreBelow"].as_i64()).unwrap_or(0);
                        if cur_more <= 80 && f_more > 80 && fvp_w >= 300.0 && fvp_h >= 200.0 {
                            if scroll_frame.is_none() || f_more > cur_more {
                                scroll_info = Some(fval["scrollInfo"].clone());
                                scroll_frame = Some(route_obj);
                            }
                        }
                        if let Some(ftxt) = fval["text"].as_str().filter(|s| !s.is_empty()) {
                            if !text.is_empty() {
                                text.push('\n');
                            }
                            text.push_str(ftxt);
                        }
                    }
                }
            }
        }

        if let Some(meta) = self.inner.sessions.lock().unwrap().get_mut(&sid) {
            meta.frame_routes = frame_routes;
            meta.overlay_frame = overlay_frame;
            meta.scroll_frame = scroll_frame;
        }

        // Capture JPEG screenshot without hiding the cursor so live viewers never see it blink.
        let (screenshot_b64, screenshot_error) = if opts.screenshot {
            match self
                .call(
                    "Page.captureScreenshot",
                    json!({"format": "jpeg", "quality": 80, "fromSurface": true}),
                    Some(&sid),
                )
                .await
            {
                Ok(shot) => (shot["data"].as_str().map(str::to_string), None),
                Err(err) => (None, Some(err)),
            }
        } else {
            (None, None)
        };

        let vp = main_val["viewport"].as_array();
        let viewport = [
            vp.and_then(|a| a.first()).and_then(Value::as_f64).unwrap_or(1280.0),
            vp.and_then(|a| a.get(1)).and_then(Value::as_f64).unwrap_or(800.0),
            vp.and_then(|a| a.get(2)).and_then(Value::as_f64).unwrap_or(1.0),
        ];
        let sc = main_val["scroll"].as_array();
        let scroll = [
            sc.and_then(|a| a.first()).and_then(Value::as_f64).unwrap_or(0.0),
            sc.and_then(|a| a.get(1)).and_then(Value::as_f64).unwrap_or(0.0),
            sc.and_then(|a| a.get(2)).and_then(Value::as_f64).unwrap_or(viewport[0]),
            sc.and_then(|a| a.get(3)).and_then(Value::as_f64).unwrap_or(viewport[1]),
        ];
        let cursor = main_val["cursor"].as_array().and_then(|a| {
            Some([a.first()?.as_f64()?, a.get(1)?.as_f64()?])
        });

        Ok(RawView {
            url: main_val["url"].as_str().unwrap_or_default().to_string(),
            title: main_val["title"].as_str().unwrap_or_default().to_string(),
            ready_state: main_val["readyState"].as_str().unwrap_or("complete").to_string(),
            viewport,
            scroll,
            scroll_info,
            cursor,
            focused: (!main_val["focused"].is_null()).then(|| main_val["focused"].clone()),
            overlay,
            items,
            text,
            observe_ms: main_val["observeMs"].as_u64().unwrap_or(0),
            screenshot_b64,
            screenshot_error,
            zoom_css,
        })
    }

    async fn resolve_element(&self, tab_sid: &str, id: u32) -> Result<Value, String> {
        self.resolve_element_for(tab_sid, id, "").await
    }

    async fn resolve_element_for(&self, tab_sid: &str, id: u32, action_kind: &str) -> Result<Value, String> {
        let route = self
            .inner
            .sessions
            .lock()
            .unwrap()
            .get(tab_sid)
            .and_then(|m| m.frame_routes.get(&id).cloned());
        let expr = format!("__novaBrowser.resolve({id},{})", json!(action_kind));
        if let Some(route) = route {
            let target_sid = route.session_id.as_deref().unwrap_or(tab_sid);
            let mut val = self
                .eval_in_frame(tab_sid, target_sid, &route.frame_id, &expr)
                .await?;
            let (ox, oy, sx, sy) = self.iframe_transform(tab_sid, &route.parent_chain).await?;
            let lx = val["x"].as_f64().unwrap_or(0.0);
            let ly = val["y"].as_f64().unwrap_or(0.0);
            val["x"] = json!(ox + lx * sx);
            val["y"] = json!(oy + ly * sy);
            if let Some(r) = val["rect"].as_array() {
                if r.len() == 4 {
                    val["rect"] = json!([
                        ox + r[0].as_f64().unwrap_or(0.0) * sx,
                        oy + r[1].as_f64().unwrap_or(0.0) * sy,
                        r[2].as_f64().unwrap_or(0.0) * sx,
                        r[3].as_f64().unwrap_or(0.0) * sy,
                    ]);
                }
            }
            Ok(val)
        } else {
            let main_frame = self.main_frame(tab_sid)?;
            self.eval_in_frame(tab_sid, tab_sid, &main_frame, &expr)
                .await
        }
    }

    async fn move_cursor(&self, tab_sid: &str, x: f64, y: f64, duration_ms: u64) -> Result<(), String> {
        let main_frame = self.main_frame(tab_sid)?;
        let _ = self
            .eval_in_frame(
                tab_sid,
                tab_sid,
                &main_frame,
                &format!("__novaBrowser.cursor.moveTo({x:.1},{y:.1},{duration_ms})"),
            )
            .await;
        if let Some(m) = self.inner.sessions.lock().unwrap().get_mut(tab_sid) {
            m.cursor = Some((x, y));
            m.cursor_active = true;
        }
        self.call(
            "Input.dispatchMouseEvent",
            json!({"type": "mouseMoved", "x": x, "y": y}),
            Some(tab_sid),
        )
        .await?;
        Ok(())
    }

    async fn click_point(&self, tab_sid: &str, x: f64, y: f64, button: &str, clicks: u8) -> Result<(), String> {
        let main_frame = self.main_frame(tab_sid)?;
        let _ = self
            .eval_in_frame(
                tab_sid,
                tab_sid,
                &main_frame,
                &format!("__novaBrowser.cursor.click({x:.1},{y:.1})"),
            )
            .await;
        for n in 1..=clicks.clamp(1, 2) {
            self.call(
                "Input.dispatchMouseEvent",
                json!({"type": "mousePressed", "x": x, "y": y, "button": button, "clickCount": n}),
                Some(tab_sid),
            )
            .await?;
            self.call(
                "Input.dispatchMouseEvent",
                json!({"type": "mouseReleased", "x": x, "y": y, "button": button, "clickCount": n}),
                Some(tab_sid),
            )
            .await?;
        }
        Ok(())
    }

    async fn press_key(&self, tab_sid: &str, name: &str, show_badge: bool) -> Result<(), String> {
        let (key, code, vk, modifiers, command) =
            key_spec(name).ok_or_else(|| format!("不支持的按键：{name}"))?;
        if let Ok(main_frame) = self.main_frame(tab_sid) {
            let expr = if show_badge {
                format!("__novaBrowser.cursor.badge({})", json!(name))
            } else {
                "__novaBrowser.cursor.markInput(260)".to_string()
            };
            let _ = self.eval_in_frame(tab_sid, tab_sid, &main_frame, &expr).await;
        }
        let mut down = json!({"type": "keyDown", "key": key, "code": code, "windowsVirtualKeyCode": vk, "modifiers": modifiers});
        if let Some(cmd) = command {
            down["commands"] = json!([cmd]);
        }
        let up = json!({"type": "keyUp", "key": key, "code": code, "windowsVirtualKeyCode": vk, "modifiers": modifiers});
        self.call("Input.dispatchKeyEvent", down, Some(tab_sid)).await?;
        self.call("Input.dispatchKeyEvent", up, Some(tab_sid)).await?;
        Ok(())
    }

    pub(crate) async fn dismiss_cursor(&self, tag: Option<&str>) {
        let sids: Vec<String> = match tag {
            Some(t) => self
                .resolve_target_id(t)
                .ok()
                .and_then(|tid| self.inner.target_sessions.lock().unwrap().get(&tid).cloned())
                .into_iter()
                .collect(),
            None => self.inner.sessions.lock().unwrap().keys().cloned().collect(),
        };
        for sid in sids {
            if let Some(m) = self.inner.sessions.lock().unwrap().get_mut(&sid) {
                m.cursor_active = false;
            }
            if let Ok(frame) = self.main_frame(&sid) {
                let _ = self.eval_in_frame(&sid, &sid, &frame, "__novaBrowser?.cursor?.dismiss?.(false)").await;
            }
        }
    }

    pub(crate) async fn act(&self, tag: &str, steps: &[Step], root: &Path, view_opts: Option<&ViewOptions>) -> Result<RawActResult, String> {
        let started = Instant::now();
        let sid = self.ensure_session(tag).await?;
        let main_frame = self.main_frame(&sid)?;
        let before_tags: HashSet<String> = self.list_tabs().into_iter().map(|t| t.tag).collect();

        let mut actions_log = Vec::with_capacity(steps.len());
        let mut completed = 0usize;
        let mut failure = None;
        let mut may_navigate = false;

        // Smooth human-like cursor glide (~155ms single step, ~110ms hint inside multi-step batch)
        let glide_ms = if steps.len() > 2 { 110 } else { 155 };

        for step in steps {
            let outcome: Result<String, String> = match step {
                Step::Click { id, x, y, button, clicks } => {
                    may_navigate = true;
                    let btn = button.as_deref().unwrap_or("left");
                    let cnt = clicks.unwrap_or(1);
                    let (tx, ty, desc) = if let Some(eid) = id {
                        let r = self.resolve_element_for(&sid, *eid, "click").await?;
                        let nm = r["name"].as_str().unwrap_or_default();
                        (r["x"].as_f64().unwrap_or(0.0), r["y"].as_f64().unwrap_or(0.0), format!("[{eid}] \"{nm}\""))
                    } else if let (Some(px), Some(py)) = (x, y) {
                        (*px, *py, "target".to_string())
                    } else {
                        return Err("click 需提供 id 或 x/y 坐标".into());
                    };
                    self.move_cursor(&sid, tx, ty, glide_ms).await?;
                    self.click_point(&sid, tx, ty, btn, cnt).await?;
                    Ok(format!("clicked {desc}"))
                }
                Step::Type { text, id, x, y, clear, submit } => {
                    let mut is_select = false;
                    let (target_sid, target_fid) = if let Some(eid) = id {
                        let route = self.inner.sessions.lock().unwrap().get(&sid).and_then(|m| m.frame_routes.get(eid).cloned());
                        match route {
                            Some(rt) => (rt.session_id.unwrap_or_else(|| sid.clone()), rt.frame_id),
                            None => (sid.clone(), main_frame.clone()),
                        }
                    } else {
                        (sid.clone(), main_frame.clone())
                    };
                    let target_desc = if let Some(eid) = id {
                        let r = self.resolve_element_for(&sid, *eid, "type").await?;
                        is_select = r["tag"].as_str() == Some("select");
                        let (tx, ty) = (r["x"].as_f64().unwrap_or(0.0), r["y"].as_f64().unwrap_or(0.0));
                        self.move_cursor(&sid, tx, ty, glide_ms).await?;
                        if !is_select {
                            self.click_point(&sid, tx, ty, "left", 1).await?;
                            tokio::time::sleep(Duration::from_millis(20)).await;
                        }
                        format!("[{eid}]")
                    } else if let (Some(px), Some(py)) = (x, y) {
                        self.move_cursor(&sid, *px, *py, glide_ms).await?;
                        self.click_point(&sid, *px, *py, "left", 1).await?;
                        tokio::time::sleep(Duration::from_millis(20)).await;
                        "target".to_string()
                    } else {
                        "focused".to_string()
                    };
                    if is_select {
                        let id_arg = id.map(|v| v.to_string()).unwrap_or_else(|| "null".into());
                        let sel = self
                            .eval_in_frame(&sid, &target_sid, &target_fid, &format!("__novaBrowser.selectOption({id_arg},{})", json!(text)))
                            .await?;
                        let chosen = sel["selected"].as_str().unwrap_or(text.as_str());
                        Ok(format!("selected \"{chosen}\" in {target_desc}"))
                    } else {
                        if clear.unwrap_or(true) {
                            #[cfg(target_os = "macos")]
                            let select_all = "Meta+A";
                            #[cfg(not(target_os = "macos"))]
                            let select_all = "Ctrl+A";
                            let _ = self.press_key(&sid, select_all, false).await;
                            let _ = self.press_key(&sid, "Backspace", false).await;
                            let _ = self.eval_in_frame(&sid, &target_sid, &target_fid, "__novaBrowser.ensureCleared()").await;
                        }
                        if !text.is_empty() {
                            self.call("Input.insertText", json!({"text": text}), Some(&sid)).await?;
                            let _ = self.eval_in_frame(&sid, &sid, &main_frame, "__novaBrowser.cursor.markInput()").await;
                        }
                        let readback = self
                            .eval_in_frame(&sid, &target_sid, &target_fid, "__novaBrowser.readTypedValue()")
                            .await
                            .ok()
                            .and_then(|v| v.as_str().map(str::to_string));
                        if submit == &Some(true) {
                            may_navigate = true;
                            tokio::time::sleep(Duration::from_millis(20)).await;
                            self.press_key(&sid, "Enter", true).await?;
                        }
                        let preview: String = text.chars().take(40).collect();
                        let val_note = readback
                            .filter(|v| !v.is_empty() && submit != &Some(true))
                            .map(|v| format!(" (val=\"{v}\")"))
                            .unwrap_or_default();
                        Ok(format!("typed \"{preview}\" into {target_desc}{val_note}{}", if submit == &Some(true) { " + Enter" } else { "" }))
                    }
                }
                Step::Hover { id, x, y } => {
                    let (tx, ty, desc) = if let Some(eid) = id {
                        let r = self.resolve_element(&sid, *eid).await?;
                        (r["x"].as_f64().unwrap_or(0.0), r["y"].as_f64().unwrap_or(0.0), format!("[{eid}]"))
                    } else if let (Some(px), Some(py)) = (x, y) {
                        (*px, *py, "target".to_string())
                    } else {
                        return Err("hover 需提供 id 或 x/y 坐标".into());
                    };
                    self.move_cursor(&sid, tx, ty, glide_ms).await?;
                    Ok(format!("hovered {desc}"))
                }
                Step::Drag { x1, y1, x2, y2, duration_ms } => {
                    let dur = duration_ms.unwrap_or(240).clamp(80, 1500);
                    let segs = (dur / 16).clamp(6, 28);
                    self.move_cursor(&sid, *x1, *y1, glide_ms).await?;
                    let _ = self.eval_in_frame(&sid, &sid, &main_frame, &format!("__novaBrowser.cursor.drag('start',{x1:.1},{y1:.1})")).await;
                    self.call("Input.dispatchMouseEvent", json!({"type":"mousePressed","x":x1,"y":y1,"button":"left","buttons":1,"clickCount":1}), Some(&sid)).await?;
                    for i in 1..=segs {
                        let t = i as f64 / segs as f64;
                        let ease = t * t * (3.0 - 2.0 * t);
                        let nx = x1 + (x2 - x1) * ease;
                        let ny = y1 + (y2 - y1) * ease;
                        let _ = self.eval_in_frame(&sid, &sid, &main_frame, &format!("__novaBrowser.cursor.drag('move',{nx:.1},{ny:.1})")).await;
                        self.call("Input.dispatchMouseEvent", json!({"type":"mouseMoved","x":nx,"y":ny,"button":"left","buttons":1}), Some(&sid)).await?;
                        tokio::time::sleep(Duration::from_millis(dur / segs)).await;
                    }
                    self.call("Input.dispatchMouseEvent", json!({"type":"mouseReleased","x":x2,"y":y2,"button":"left","clickCount":1}), Some(&sid)).await?;
                    let _ = self.eval_in_frame(&sid, &sid, &main_frame, &format!("__novaBrowser.cursor.drag('end',{x2:.1},{y2:.1})")).await;
                    if let Some(m) = self.inner.sessions.lock().unwrap().get_mut(&sid) {
                        m.cursor = Some((*x2, *y2));
                    }
                    Ok("dragged".into())
                }
                Step::Scroll { dy, dx, id, x, y } => {
                    let id_js = id.map(|v| v.to_string()).unwrap_or_else(|| "null".into());
                    let route = {
                        let sessions = self.inner.sessions.lock().unwrap();
                        let meta = sessions.get(&sid);
                        if let Some(eid) = id {
                            meta.and_then(|m| m.frame_routes.get(eid).cloned())
                        } else if x.is_none() && y.is_none() {
                            meta.and_then(|m| m.scroll_frame.clone())
                        } else {
                            None
                        }
                    };
                    let (target_sid, target_fid) = match route.as_ref() {
                        Some(r) => (r.session_id.as_deref().unwrap_or(&sid), r.frame_id.as_str()),
                        None => (sid.as_str(), main_frame.as_str()),
                    };
                    let info = self
                        .eval_in_frame(&sid, target_sid, target_fid, &format!("__novaBrowser.scrollTarget({id_js})"))
                        .await
                        .ok();
                    let (mut tx, mut ty) = if let (Some(px), Some(py)) = (x, y) {
                        (*px, *py)
                    } else if let Some(ref iv) = info {
                        (iv["x"].as_f64().unwrap_or(640.0), iv["y"].as_f64().unwrap_or(400.0))
                    } else if let Some(eid) = id {
                        let r = self.resolve_element(&sid, *eid).await?;
                        (r["x"].as_f64().unwrap_or(640.0), r["y"].as_f64().unwrap_or(400.0))
                    } else {
                        (640.0, 400.0)
                    };
                    if let (Some(r), true, true) = (route.as_ref(), info.is_some(), x.is_none() && y.is_none()) {
                        if let Ok((ox, oy, sx, sy)) = self.iframe_transform(&sid, &r.parent_chain).await {
                            tx = ox + tx * sx;
                            ty = oy + ty * sy;
                        }
                    }
                    let before_top = info.as_ref().and_then(|v| v["top"].as_i64()).unwrap_or(0);
                    let before_left = info.as_ref().and_then(|v| v["left"].as_i64()).unwrap_or(0);
                    self.move_cursor(&sid, tx, ty, 80).await?;
                    self.call(
                        "Input.dispatchMouseEvent",
                        json!({"type": "mouseWheel", "x": tx, "y": ty, "deltaX": dx, "deltaY": dy}),
                        Some(&sid),
                    )
                    .await?;
                    tokio::time::sleep(Duration::from_millis(60)).await;
                    let after = self
                        .eval_in_frame(
                            &sid,
                            target_sid,
                            target_fid,
                            &format!("__novaBrowser.verifyOrFallbackScroll({id_js},{dx},{dy},{before_top},{before_left})"),
                        )
                        .await
                        .ok();
                    if let Some(a) = after.filter(|v| v["afterTop"].is_i64()) {
                        Ok(format!(
                            "scrolled y: {} -> {}/{}",
                            a["beforeTop"].as_i64().unwrap_or(before_top),
                            a["afterTop"].as_i64().unwrap_or(before_top),
                            a["maxY"].as_i64().unwrap_or(0)
                        ))
                    } else {
                        Ok(format!("scrolled dx={dx} dy={dy}"))
                    }
                }
                Step::Press { key } => {
                    if key.eq_ignore_ascii_case("Enter") {
                        may_navigate = true;
                    }
                    self.press_key(&sid, key, true).await?;
                    Ok(format!("pressed {key}"))
                }
                Step::Dismiss => {
                    may_navigate = true;
                    let mut ov_route: Option<FrameRoute> = None;
                    let mut ov = self
                        .eval_in_frame(&sid, &sid, &main_frame, "__novaBrowser.overlayState()")
                        .await
                        .unwrap_or_else(|_| json!({"open": false}));
                    if ov["open"] != true {
                        let candidate = self
                            .inner
                            .sessions
                            .lock()
                            .unwrap()
                            .get(&sid)
                            .and_then(|m| m.overlay_frame.clone());
                        if let Some(r) = candidate {
                            let r_sid = r.session_id.as_deref().unwrap_or(&sid);
                            if let Ok(mut fov) = self.eval_in_frame(&sid, r_sid, &r.frame_id, "__novaBrowser.overlayState()").await {
                                if fov["open"] == true {
                                    if let Ok((ox, oy, sx, sy)) = self.iframe_transform(&sid, &r.parent_chain).await {
                                        if let Some(pt) = fov["outsidePoint"].as_array().filter(|a| a.len() == 2) {
                                            fov["outsidePoint"] = json!([
                                                ox + pt[0].as_f64().unwrap_or(0.0) * sx,
                                                oy + pt[1].as_f64().unwrap_or(0.0) * sy,
                                            ]);
                                        }
                                    }
                                    ov = fov;
                                    ov_route = Some(r);
                                }
                            }
                        }
                    }
                    let pt = if ov["open"] == true {
                        ov["outsidePoint"].as_array().filter(|a| a.len() == 2)
                    } else {
                        ov["safeBlankPoint"].as_array().filter(|a| a.len() == 2)
                    };
                    if let Some(pt) = pt {
                        let ox = pt[0].as_f64().unwrap_or(20.0);
                        let oy = pt[1].as_f64().unwrap_or(20.0);
                        self.move_cursor(&sid, ox, oy, glide_ms).await?;
                        self.click_point(&sid, ox, oy, "left", 1).await?;
                        tokio::time::sleep(Duration::from_millis(140)).await;
                        let (chk_sid, chk_fid) = match ov_route.as_ref() {
                            Some(r) => (r.session_id.as_deref().unwrap_or(&sid), r.frame_id.as_str()),
                            None => (sid.as_str(), main_frame.as_str()),
                        };
                        let after = self
                            .eval_in_frame(&sid, chk_sid, chk_fid, "__novaBrowser.overlayState()")
                            .await
                            .unwrap_or_else(|_| json!({"open": false}));
                        if ov["open"] == true && after["open"] == true {
                            self.press_key(&sid, "Escape", true).await?;
                            tokio::time::sleep(Duration::from_millis(80)).await;
                            Ok("dismissed overlay (clicked blank space + Escape)".into())
                        } else {
                            Ok("dismissed overlay by clicking blank space".into())
                        }
                    } else {
                        self.press_key(&sid, "Escape", true).await?;
                        tokio::time::sleep(Duration::from_millis(80)).await;
                        Ok("pressed Escape to dismiss overlay".into())
                    }
                }
                Step::Wait { ms } => {
                    let _ = self.eval_in_frame(&sid, &sid, &main_frame, "__novaBrowser?.cursor?.keepAlive?.()").await;
                    tokio::time::sleep(Duration::from_millis((*ms).min(10000))).await;
                    Ok(format!("waited {ms}ms"))
                }
                Step::Upload { selector, files } => {
                    let resolved_files = upload_files(root, files)?;
                    let ctx = self.ensure_world(&sid, &sid, &main_frame).await?;
                    let expr = format!(
                        "(()=>{{const ns=document.querySelectorAll({});if(ns.length!==1)throw Error('selector必须唯一匹配file input');const el=ns[0];if(!(el instanceof HTMLInputElement)||el.type!=='file'||el.disabled)throw Error('目标不是可用的file input');return el;}})()",
                        json!(selector)
                    );
                    let reply = self
                        .call("Runtime.evaluate", json!({"expression": expr, "contextId": ctx, "returnByValue": false}), Some(&sid))
                        .await?;
                    if let Some(exc) = reply.get("exceptionDetails") {
                        return Err(exc["exception"]["description"].as_str().unwrap_or("上传目标校验失败").into());
                    }
                    let obj = reply["result"]["objectId"].as_str().ok_or("未取得 file input")?;
                    let up = self.call("DOM.setFileInputFiles", json!({"objectId": obj, "files": resolved_files}), Some(&sid)).await;
                    let _ = self.call("Runtime.releaseObject", json!({"objectId": obj}), Some(&sid)).await;
                    up?;
                    Ok(format!("uploaded {} file(s)", resolved_files.len()))
                }
            };
            match outcome {
                Ok(msg) => {
                    completed += 1;
                    actions_log.push(msg);
                }
                Err(err) => {
                    failure = Some(err);
                    break;
                }
            }
            // Check if a popup/new tab was opened mid-batch
            let newly_opened: Vec<TabInfo> = self
                .list_tabs()
                .into_iter()
                .filter(|t| !before_tags.contains(&t.tag) && t.opener_tag.as_deref() == Some(tag))
                .collect();
            if !newly_opened.is_empty() && completed < steps.len() {
                break;
            }
        }

        // Smart event-driven settle (fast!)
        if failure.is_none() {
            if may_navigate {
                tokio::time::sleep(Duration::from_millis(50)).await;
                let is_loading = self.inner.sessions.lock().unwrap().get(&sid).is_some_and(|m| m.loading);
                if is_loading {
                    self.wait_for_load(&sid, Duration::from_millis(3000)).await;
                } else {
                    let _ = self.eval_in_frame(&sid, &sid, &main_frame, "__novaBrowser.waitQuiet(110, 800)").await;
                }
            } else {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        }

        let opened_tabs: Vec<TabInfo> = self
            .list_tabs()
            .into_iter()
            .filter(|t| !before_tags.contains(&t.tag) && t.opener_tag.as_deref() == Some(tag))
            .collect();

        // If a single new controllable tab was opened by the action, observe it automatically!
        let observe_tag = if opened_tabs.len() == 1 && opened_tabs[0].controllable {
            let new_tag = opened_tabs[0].tag.clone();
            if let Ok(nsid) = self.ensure_session(&new_tag).await {
                self.wait_for_load(&nsid, Duration::from_millis(3000)).await;
            }
            *self.inner.active_tag.lock().unwrap() = Some(new_tag.clone());
            new_tag
        } else {
            tag.to_string()
        };

        let view = if let Some(vopts) = view_opts {
            self.view(&observe_tag, vopts).await.ok()
        } else {
            None
        };

        Ok(RawActResult {
            ok: failure.is_none(),
            completed,
            error: failure,
            actions: actions_log,
            opened_tabs,
            view,
            action_ms: started.elapsed().as_millis() as u64,
        })
    }

    pub(crate) async fn eval(&self, tag: &str, code: &str, with_view: bool) -> Result<Value, String> {
        let sid = self.ensure_session(tag).await?;
        let main_frame = self.main_frame(&sid)?;
        let res = tokio::time::timeout(
            Duration::from_secs(6),
            self.eval_in_frame(&sid, &sid, &main_frame, &format!("__novaBrowser.evalCode({})", json!(code))),
        )
        .await
        .map_err(|_| "脚本执行超过 6 秒超时")??;
        let mut out = if res["ok"] == true {
            json!({"ok": true, "value": res["value"]})
        } else {
            json!({"ok": false, "error": res["error"].as_str().unwrap_or("脚本执行失败")})
        };
        if with_view {
            if let Ok(v) = self.view(tag, &ViewOptions::default()).await {
                out["rawView"] = serde_json::to_value(v).unwrap_or(Value::Null);
            }
        }
        Ok(out)
    }

    pub(crate) async fn debug(&self, tag: &str, kind: &str, query: Option<&str>, level: Option<&str>, limit: usize, request_id: Option<&str>, clear: bool) -> Result<Value, String> {
        let sid = self.ensure_session(tag).await?;
        let needs_enable = self
            .inner
            .sessions
            .lock()
            .unwrap()
            .get(&sid)
            .is_some_and(|m| m.devtools.is_none());
        if needs_enable {
            if let Some(m) = self.inner.sessions.lock().unwrap().get_mut(&sid) {
                m.devtools = Some(DevtoolsLog {
                    since: chrono::Utc::now().timestamp_millis(),
                    ..Default::default()
                });
            }
            for method in ["Runtime.enable", "Log.enable", "Network.enable"] {
                let _ = self.call(method, json!({}), Some(&sid)).await;
            }
        }
        if let Some(rid) = request_id {
            let body = self
                .call("Network.getResponseBody", json!({"requestId": rid}), Some(&sid))
                .await?;
            let text = body["body"].as_str().unwrap_or_default();
            let clipped: String = text.chars().take(65536).collect();
            return Ok(json!({
                "tab": tag,
                "requestId": rid,
                "base64Encoded": body["base64Encoded"] == true,
                "bodyLength": text.len(),
                "body": clipped,
            }));
        }
        let q = query.map(|s| s.to_lowercase());
        let mut sessions = self.inner.sessions.lock().unwrap();
        let log = sessions.get_mut(&sid).and_then(|m| m.devtools.as_mut()).ok_or("DevTools 未启用")?;
        let keep = |e: &Value| -> bool {
            let text_ok = q.as_ref().is_none_or(|q| {
                e["text"].as_str().is_some_and(|s| s.to_lowercase().contains(q))
                    || e["url"].as_str().is_some_and(|s| s.to_lowercase().contains(q))
            });
            let level_ok = level.is_none_or(|lvl| {
                e["level"].as_str() == Some(lvl)
                    || (lvl == "error" && (e["failed"] == true || e["status"].as_i64().is_some_and(|s| s >= 400)))
            });
            text_ok && level_ok
        };
        let mut out = json!({"tab": tag, "recordingSince": log.since});
        if kind != "network" {
            let items: Vec<Value> = log.console.iter().filter(|e| keep(e)).cloned().collect();
            let total = items.len();
            let tail = items.into_iter().rev().take(limit).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>();
            out["console"] = json!({"total": total, "items": tail});
        }
        if kind != "console" {
            let all_net: Vec<&Value> = log
                .network_order
                .iter()
                .filter_map(|id| log.network.get(id))
                .collect();
            let items: Vec<Value> = all_net.iter().copied().filter(|e| keep(e)).cloned().collect();
            let total = items.len();
            let tail = items.into_iter().rev().take(limit).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>();
            let mut net_obj = json!({"total": total, "items": tail});
            if q.is_some() && total == 0 && !all_net.is_empty() {
                net_obj["unfilteredTotal"] = json!(all_net.len());
                let samples: Vec<Value> = all_net
                    .iter()
                    .rev()
                    .filter(|e| matches!(e["type"].as_str(), Some("Fetch" | "XHR")))
                    .take(6)
                    .filter_map(|e| e["url"].as_str().map(|s| json!(s)))
                    .collect();
                if !samples.is_empty() {
                    net_obj["sampleUrls"] = json!(samples);
                }
            }
            out["network"] = net_obj;
        }
        if clear {
            log.console.clear();
            log.network.clear();
            log.network_order.clear();
        }
        Ok(out)
    }

    pub(crate) async fn export_cookies(&self) -> Result<Vec<Value>, String> {
        let res = self.call("Storage.getCookies", json!({}), None).await?;
        Ok(res["cookies"].as_array().cloned().unwrap_or_default())
    }

    pub(crate) async fn import_cookies(&self, cookies: &[Value]) -> Result<usize, String> {
        if cookies.is_empty() {
            return Ok(0);
        }
        let existing = self.export_cookies().await.unwrap_or_default();
        let same: HashMap<String, String> = existing
            .iter()
            .map(|c| (cookie_key(c), c["value"].as_str().unwrap_or_default().to_string()))
            .collect();
        let params: Vec<Value> = cookies
            .iter()
            .filter(|c| same.get(&cookie_key(c)).map(String::as_str) != Some(c["value"].as_str().unwrap_or_default()))
            .filter_map(cookie_param)
            .collect();
        for chunk in params.chunks(500) {
            self.call("Storage.setCookies", json!({"cookies": chunk}), None).await?;
        }
        Ok(params.len())
    }

    pub(crate) async fn export_storage(&self, tag: &str) -> Result<Vec<Vec<String>>, String> {
        let sid = self.ensure_session(tag).await?;
        let res = self
            .call(
                "Runtime.evaluate",
                json!({"expression": "JSON.stringify(Object.entries(localStorage))", "returnByValue": true}),
                Some(&sid),
            )
            .await?;
        let pairs: Vec<Vec<String>> =
            serde_json::from_str(res["result"]["value"].as_str().unwrap_or("[]")).unwrap_or_default();
        Ok(pairs.into_iter().filter(|p| p.len() == 2).collect())
    }

    /// Ensure smooth Page.startScreencast is running on `tag` for the sidebar preview.
    pub(crate) async fn ensure_screencast(&self, tag: &str, thread_id: Option<String>) -> Result<Option<String>, String> {
        let sid = self.ensure_session(tag).await?;
        let (need_start, old_sid, existing_frame) = {
            let mut sc = self.inner.screencast.lock().unwrap();
            match sc.as_mut() {
                Some(state) if state.session_id == sid => {
                    state.last_heartbeat = Instant::now();
                    state.thread_id = thread_id;
                    (false, None, state.latest_b64.clone())
                }
                other => {
                    let prev = other.as_ref().map(|s| s.session_id.clone());
                    *sc = Some(ScreencastState {
                        session_id: sid.clone(),
                        tab_tag: tag.to_string(),
                        thread_id,
                        last_heartbeat: Instant::now(),
                        last_emit: Instant::now() - Duration::from_secs(1),
                        latest_b64: None,
                    });
                    (true, prev, None)
                }
            }
        };
        if let Some(prev) = old_sid {
            let _ = self.call("Page.stopScreencast", json!({}), Some(&prev)).await;
        }
        if need_start {
            let _ = self
                .call(
                    "Page.startScreencast",
                    json!({"format": "jpeg", "quality": 72, "maxWidth": 1280, "maxHeight": 800, "everyNthFrame": 1}),
                    Some(&sid),
                )
                .await;
        }
        if existing_frame.is_some() {
            return Ok(existing_frame);
        }
        // Return an immediate frame on first open without waiting for next repaint.
        let shot = self
            .call(
                "Page.captureScreenshot",
                json!({"format": "jpeg", "quality": 72, "fromSurface": true}),
                Some(&sid),
            )
            .await?;
        let b64 = shot["data"].as_str().map(str::to_string);
        if let Some(ref data) = b64 {
            if let Some(state) = self.inner.screencast.lock().unwrap().as_mut() {
                if state.session_id == sid {
                    state.latest_b64 = Some(data.clone());
                }
            }
        }
        Ok(b64)
    }

    pub(crate) async fn stop_idle_screencast(&self) {
        let stop_sid = {
            let mut sc = self.inner.screencast.lock().unwrap();
            match sc.as_ref() {
                Some(s) if s.last_heartbeat.elapsed() > Duration::from_secs(7) => {
                    let sid = s.session_id.clone();
                    *sc = None;
                    Some(sid)
                }
                _ => None,
            }
        };
        if let Some(sid) = stop_sid {
            let _ = self.call("Page.stopScreencast", json!({}), Some(&sid)).await;
        }
    }
}

fn collect_child_frames(
    node: &Value,
    cdp_sid: Option<String>,
    parent_chain: &[(String, Option<String>)],
    out: &mut Vec<(String, Option<String>, Vec<(String, Option<String>)>)>,
) {
    if let Some(children) = node["childFrames"].as_array() {
        for child in children {
            if let Some(fid) = child["frame"]["id"].as_str() {
                let mut chain = vec![(fid.to_string(), cdp_sid.clone())];
                chain.extend_from_slice(parent_chain);
                out.push((fid.to_string(), cdp_sid.clone(), chain.clone()));
                collect_child_frames(child, cdp_sid.clone(), &chain, out);
            }
        }
    }
}

fn upsert_target(inner: &CdpInner, info: &Value) -> String {
    if info["type"].as_str() != Some("page") {
        return String::new();
    }
    let Some(target_id) = info["targetId"].as_str() else { return String::new() };
    let mut targets = inner.targets.lock().unwrap();
    if let Some(existing) = targets.get_mut(target_id) {
        if let Some(title) = info["title"].as_str() {
            existing.title = title.to_string();
        }
        if let Some(url) = info["url"].as_str() {
            existing.url = url.to_string();
        }
        return existing.tag.clone();
    }
    let n = inner.next_tag.fetch_add(1, Ordering::Relaxed);
    let tag = format!("H{n}");
    targets.insert(
        target_id.to_string(),
        TargetEntry {
            target_id: target_id.to_string(),
            tag: tag.clone(),
            title: info["title"].as_str().unwrap_or_default().to_string(),
            url: info["url"].as_str().unwrap_or_default().to_string(),
            opener_id: info["openerId"].as_str().map(str::to_string),
            created_at: Instant::now(),
        },
    );
    inner.tag_to_target.lock().unwrap().insert(tag.clone(), target_id.to_string());
    tag
}

fn remove_target(inner: &CdpInner, target_id: &str) {
    if let Some(removed) = inner.targets.lock().unwrap().remove(target_id) {
        inner.tag_to_target.lock().unwrap().remove(&removed.tag);
    }
    if let Some(sid) = inner.target_sessions.lock().unwrap().remove(target_id) {
        inner.sessions.lock().unwrap().remove(&sid);
    }
}

fn handle_event(inner: &CdpInner, session_id: Option<&str>, method: &str, params: &Value) {
    match method {
        "Target.targetCreated" | "Target.targetInfoChanged" => {
            upsert_target(inner, &params["targetInfo"]);
        }
        "Target.targetDestroyed" => {
            if let Some(tid) = params["targetId"].as_str() {
                remove_target(inner, tid);
            }
        }
        "Target.attachedToTarget" => {
            if let Some(sid) = session_id {
                if params["targetInfo"]["type"] == "iframe" {
                    if let (Some(tid), Some(csid)) = (params["targetInfo"]["targetId"].as_str(), params["sessionId"].as_str()) {
                        if let Some(m) = inner.sessions.lock().unwrap().get_mut(sid) {
                            m.child_sessions.insert(tid.to_string(), csid.to_string());
                        }
                    }
                }
            }
        }
        "Target.detachedFromTarget" => {
            if let (Some(sid), Some(csid)) = (session_id, params["sessionId"].as_str()) {
                if let Some(m) = inner.sessions.lock().unwrap().get_mut(sid) {
                    m.child_sessions.retain(|_, v| v != csid);
                }
            }
        }
        "Page.frameStartedLoading" => {
            if let Some(sid) = session_id {
                if let Some(m) = inner.sessions.lock().unwrap().get_mut(sid) {
                    if params["frameId"].as_str() == Some(m.main_frame_id.as_str()) {
                        m.loading = true;
                        m.last_nav = Some(Instant::now());
                    }
                }
            }
        }
        "Page.frameNavigated" => {
            if let Some(sid) = session_id {
                if let Some(m) = inner.sessions.lock().unwrap().get_mut(sid) {
                    let fid = params["frame"]["id"].as_str().unwrap_or_default();
                    if fid == m.main_frame_id {
                        m.contexts.clear();
                    } else {
                        m.contexts.retain(|(_, f), _| f != fid);
                    }
                }
            }
        }
        "Page.domContentEventFired" | "Page.loadEventFired" | "Page.frameStoppedLoading" => {
            if let Some(sid) = session_id {
                if let Some(m) = inner.sessions.lock().unwrap().get_mut(sid) {
                    if method != "Page.frameStoppedLoading" || params["frameId"].as_str() == Some(m.main_frame_id.as_str()) {
                        m.loading = false;
                    }
                }
                inner.load_notify.notify_waiters();
            }
        }
        "Page.screencastFrame" => {
            let Some(sid) = session_id else { return };
            let ack_id = params["sessionId"].as_i64().unwrap_or(0);
            // Always ACK immediately so Chrome keeps streaming smoothly.
            let ack_rpc = json!({
                "id": inner.next_id.fetch_add(1, Ordering::Relaxed),
                "method": "Page.screencastFrameAck",
                "params": { "sessionId": ack_id },
                "sessionId": sid
            });
            let _ = inner.tx.send(ack_rpc.to_string());
            let Some(data) = params["data"].as_str() else { return };
            let emit_info = {
                let mut sc = inner.screencast.lock().unwrap();
                let Some(state) = sc.as_mut().filter(|s| s.session_id == sid) else { return };
                state.latest_b64 = Some(data.to_string());
                if state.last_emit.elapsed() >= Duration::from_millis(50) {
                    state.last_emit = Instant::now();
                    Some((state.tab_tag.clone(), state.thread_id.clone()))
                } else {
                    None
                }
            };
            if let (Some(app), Some((tab_tag, thread_id))) = (&inner.app, emit_info) {
                let _ = app.emit(
                    EV_FRAME,
                    json!({
                        "threadId": thread_id,
                        "tabTag": tab_tag,
                        "image": format!("data:image/jpeg;base64,{data}"),
                    }),
                );
            }
        }
        "Runtime.consoleAPICalled" | "Runtime.exceptionThrown" | "Log.entryAdded"
        | "Network.requestWillBeSent" | "Network.responseReceived"
        | "Network.loadingFinished" | "Network.loadingFailed" => {
            let Some(sid) = session_id else { return };
            let mut sessions = inner.sessions.lock().unwrap();
            let Some(log) = sessions.get_mut(sid).and_then(|m| m.devtools.as_mut()) else { return };
            record_devtools(log, method, params);
        }
        _ => {}
    }
}

fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        format!("{}…", text.chars().take(max).collect::<String>())
    }
}

fn record_devtools(log: &mut DevtoolsLog, method: &str, params: &Value) {
    let push_console = |log: &mut DevtoolsLog, entry: Value| {
        log.console.push_back(entry);
        while log.console.len() > DEVTOOLS_LIMIT {
            log.console.pop_front();
        }
    };
    match method {
        "Runtime.consoleAPICalled" => {
            let text = params["args"]
                .as_array()
                .map(|args| {
                    args.iter()
                        .map(|a| {
                            a["value"]
                                .as_str()
                                .map(str::to_string)
                                .or_else(|| a["description"].as_str().map(str::to_string))
                                .unwrap_or_else(|| a["value"].to_string())
                        })
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default();
            push_console(log, json!({"source": "console", "level": params["type"], "text": clip(&text, 2000), "time": params["timestamp"]}));
        }
        "Runtime.exceptionThrown" => {
            let d = &params["exceptionDetails"];
            let desc = d["exception"]["description"].as_str().or(d["text"].as_str()).unwrap_or("Exception");
            push_console(log, json!({"source": "exception", "level": "error", "text": clip(desc, 2000), "url": clip(d["url"].as_str().unwrap_or_default(), 2048)}));
        }
        "Log.entryAdded" => {
            let e = &params["entry"];
            push_console(log, json!({"source": e["source"], "level": e["level"], "text": clip(e["text"].as_str().unwrap_or_default(), 2000), "url": clip(e["url"].as_str().unwrap_or_default(), 2048)}));
        }
        "Network.requestWillBeSent" => {
            let Some(rid) = params["requestId"].as_str() else { return };
            if !log.network.contains_key(rid) {
                log.network_order.push_back(rid.to_string());
                while log.network_order.len() > DEVTOOLS_LIMIT {
                    if let Some(old) = log.network_order.pop_front() {
                        log.network.remove(&old);
                    }
                }
            }
            log.network.insert(
                rid.to_string(),
                json!({
                    "requestId": rid,
                    "method": params["request"]["method"],
                    "url": clip(params["request"]["url"].as_str().unwrap_or_default(), 2048),
                    "type": params["type"],
                    "startedAt": (params["wallTime"].as_f64().unwrap_or(0.0) * 1000.0).round() as i64,
                }),
            );
        }
        "Network.responseReceived" => {
            if let Some(req) = params["requestId"].as_str().and_then(|id| log.network.get_mut(id)) {
                req["status"] = params["response"]["status"].clone();
                req["statusText"] = params["response"]["statusText"].clone();
                req["mimeType"] = params["response"]["mimeType"].clone();
            }
        }
        "Network.loadingFinished" => {
            if let Some(req) = params["requestId"].as_str().and_then(|id| log.network.get_mut(id)) {
                req["finished"] = json!(true);
                req["encodedBytes"] = params["encodedDataLength"].clone();
            }
        }
        "Network.loadingFailed" => {
            if let Some(req) = params["requestId"].as_str().and_then(|id| log.network.get_mut(id)) {
                req["failed"] = json!(true);
                req["errorText"] = params["errorText"].clone();
            }
        }
        _ => {}
    }
}

// ==================== Headless Process Manager ====================

pub(crate) struct HeadlessManager {
    child: Mutex<Option<Child>>,
    client: Mutex<Option<CdpClient>>,
    start: tokio::sync::Mutex<()>,
    profile: PathBuf,
    failure: Mutex<Option<(String, Instant)>>,
    cookies_from_disk: Mutex<bool>,
    notice: Mutex<Option<String>>,
    threads: Mutex<HashMap<String, Vec<String>>>,
    tabs: Mutex<Vec<(String, String)>>,
    last_used: Mutex<Instant>,
}

pub(crate) fn init_headless(app: &AppHandle) {
    let profile = app.state::<crate::AppState>().config_dir.join("headless-browser");
    app.manage(HeadlessManager {
        child: Mutex::new(None),
        client: Mutex::new(None),
        start: tokio::sync::Mutex::new(()),
        profile,
        failure: Mutex::new(None),
        cookies_from_disk: Mutex::new(false),
        notice: Mutex::new(None),
        threads: Mutex::new(HashMap::new()),
        tabs: Mutex::new(Vec::new()),
        last_used: Mutex::new(Instant::now()),
    });
    let reclaimer = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            if let Some(client) = current_client(&reclaimer) {
                client.stop_idle_screencast().await;
            }
            if let Some(mgr) = reclaimer.try_state::<HeadlessManager>() {
                let idle = mgr.last_used.lock().unwrap().elapsed();
                if idle >= IDLE_RECLAIM && mgr.client.lock().unwrap().is_some() {
                    stop_headless(&reclaimer);
                }
            }
        }
    });
}

pub(crate) fn current_client(app: &AppHandle) -> Option<CdpClient> {
    let mgr = app.try_state::<HeadlessManager>()?;
    let guard = mgr.client.lock().unwrap();
    guard.as_ref().filter(|c| c.is_alive()).cloned()
}

pub(crate) fn touch(app: &AppHandle) {
    if let Some(mgr) = app.try_state::<HeadlessManager>() {
        *mgr.last_used.lock().unwrap() = Instant::now();
    }
}

pub(crate) fn stop_headless(app: &AppHandle) {
    let Some(mgr) = app.try_state::<HeadlessManager>() else { return };
    *mgr.client.lock().unwrap() = None;
    if let Some(mut child) = mgr.child.lock().unwrap().take() {
        let _ = child.kill();
        let _ = child.wait();
    }
    mgr.tabs.lock().unwrap().clear();
    *mgr.failure.lock().unwrap() = None;
    *mgr.notice.lock().unwrap() = None;
}

pub(crate) fn bind_thread(app: &AppHandle, thread_id: &str, owner: &str) {
    let Some(mgr) = app.try_state::<HeadlessManager>() else { return };
    let mut threads = mgr.threads.lock().unwrap();
    let roots = threads.entry(thread_id.to_string()).or_default();
    if !roots.iter().any(|r| r == owner) {
        roots.push(owner.to_string());
    }
}

fn owned_by(roots: &[String], owner: &str) -> bool {
    let sep = std::path::MAIN_SEPARATOR;
    roots.iter().any(|root| owner == root || owner.strip_prefix(root.as_str()).is_some_and(|rest| rest.starts_with(sep)))
}

fn thread_of(mgr: &HeadlessManager, owner: &str) -> Option<String> {
    mgr.threads
        .lock()
        .unwrap()
        .iter()
        .find(|(_, roots)| owned_by(roots, owner))
        .map(|(id, _)| id.clone())
}

pub(crate) fn claim_tab(app: &AppHandle, owner: &str, tag: &str) {
    let Some(mgr) = app.try_state::<HeadlessManager>() else { return };
    let (added, evicted) = {
        let mut tabs = mgr.tabs.lock().unwrap();
        if tabs.iter().any(|(t, o)| t == tag && o != owner) {
            return;
        }
        let added = !tabs.iter().any(|(t, _)| t == tag);
        if added {
            tabs.push((tag.to_string(), owner.to_string()));
        }
        let mine: Vec<String> = tabs.iter().filter(|(_, o)| o == owner).map(|(t, _)| t.clone()).collect();
        let extra = mine.len().saturating_sub(TABS_PER_SESSION);
        let evicted: Vec<String> = mine.into_iter().take(extra).collect();
        if !evicted.is_empty() {
            tabs.retain(|(t, _)| !evicted.contains(t));
        }
        (added, evicted)
    };
    if added {
        if let Some(thread_id) = thread_of(&mgr, owner) {
            let _ = app.emit(EV_OPENED, json!({ "threadId": thread_id }));
        }
    }
    if !evicted.is_empty() {
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            if let Some(client) = current_client(&app) {
                for tag in evicted {
                    let _ = client.close_tab(&tag).await;
                }
            }
        });
    }
}

pub(crate) fn release_thread(app: &AppHandle, thread_id: &str) {
    let Some(mgr) = app.try_state::<HeadlessManager>() else { return };
    let Some(roots) = mgr.threads.lock().unwrap().remove(thread_id) else { return };
    let closing: Vec<String> = {
        let mut tabs = mgr.tabs.lock().unwrap();
        let mut out = Vec::new();
        tabs.retain(|(tag, owner)| {
            if owned_by(&roots, owner) {
                out.push(tag.clone());
                false
            } else {
                true
            }
        });
        out
    };
    if !closing.is_empty() {
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            if let Some(client) = current_client(&app) {
                for tag in closing {
                    let _ = client.close_tab(&tag).await;
                }
            }
        });
    }
}

pub(crate) fn session_tags(app: &AppHandle, thread_id: &str) -> HashSet<String> {
    let Some(mgr) = app.try_state::<HeadlessManager>() else { return HashSet::new() };
    let Some(roots) = mgr.threads.lock().unwrap().get(thread_id).cloned() else { return HashSet::new() };
    let tags = mgr
        .tabs
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, owner)| owned_by(&roots, owner))
        .map(|(tag, _)| tag.clone())
        .collect();
    tags
}

pub(crate) async fn dismiss_headless_cursor(app: &AppHandle, tag: Option<&str>) {
    if let Some(client) = current_client(app) {
        client.dismiss_cursor(tag).await;
    }
}

pub(crate) async fn ensure_headless(app: &AppHandle) -> Result<CdpClient, String> {
    touch(app);
    if let Some(c) = current_client(app) {
        return Ok(c);
    }
    let mgr = app.state::<HeadlessManager>();
    let _lock = mgr.start.lock().await;
    if let Some(c) = current_client(app) {
        return Ok(c);
    }
    if let Some((err, at)) = mgr.failure.lock().unwrap().as_ref() {
        if at.elapsed() < RETRY_AFTER {
            return Err(err.clone());
        }
    }
    // Kill any leftover dead process
    if let Some(mut old) = mgr.child.lock().unwrap().take() {
        let _ = old.kill();
        let _ = old.wait();
    }
    match launch_headless(app, &mgr).await {
        Ok(client) => {
            *mgr.client.lock().unwrap() = Some(client.clone());
            *mgr.failure.lock().unwrap() = None;
            // Background login-cookie sync from real Chrome if real extension is connected.
            let disk_cookies = *mgr.cookies_from_disk.lock().unwrap();
            let sync_app = app.clone();
            let sync_client = client.clone();
            tauri::async_runtime::spawn(async move {
                let res = sync_cookies_from_real(&sync_app, &sync_client).await;
                let notice = match res {
                    Ok(_) => None,
                    Err(_) if disk_cookies => None,
                    Err(err) => Some(format!("未同步登录态：{err}；无头浏览器可能未登录")),
                };
                if let Some(m) = sync_app.try_state::<HeadlessManager>() {
                    *m.notice.lock().unwrap() = notice;
                }
            });
            Ok(client)
        }
        Err(err) => {
            stop_headless(app);
            let msg = format!("{err}；可在新会话页关闭「无头浏览器」开关改用真实浏览器");
            *mgr.failure.lock().unwrap() = Some((msg.clone(), Instant::now()));
            Err(msg)
        }
    }
}

async fn sync_cookies_from_real(app: &AppHandle, headless: &CdpClient) -> Result<usize, String> {
    let cookies = crate::browser::ext_ws::export_cookies(app).await?;
    headless.import_cookies(&cookies).await
}

async fn launch_headless(app: &AppHandle, mgr: &HeadlessManager) -> Result<CdpClient, String> {
    let chrome = crate::browser::ext_ws::find_chrome()
        .ok_or("未找到 Google Chrome：无头浏览器需要本机安装 Chrome")?;
    let _ = std::fs::remove_file(mgr.profile.join("DevToolsActivePort"));
    let (copied, cookies_from_disk, warnings) = seed_profile(&mgr.profile);
    *mgr.cookies_from_disk.lock().unwrap() = cookies_from_disk;
    for w in &warnings {
        eprintln!("[headless] {w}");
    }
    let mut cmd = Command::new(&chrome);
    cmd.arg("--headless=new")
        .arg(format!("--user-data-dir={}", mgr.profile.display()))
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .arg("--window-size=1280,800")
        .arg("--force-device-scale-factor=1")
        .arg("--remote-debugging-port=0")
        .arg("about:blank");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let child = cmd.spawn().map_err(|e| format!("启动无头 Chrome 失败：{e}"))?;
    *mgr.child.lock().unwrap() = Some(child);

    let started = Instant::now();
    let (port, path) = wait_debug_port(&mgr.profile, started).await?;
    let ws_url = format!("ws://127.0.0.1:{port}{path}");
    let client = CdpClient::connect(&ws_url, Some(app.clone())).await?;
    eprintln!("[headless] 原生 CDP WebSocket 已直连（复制 {copied} 项登录态文件）");
    Ok(client)
}

async fn wait_debug_port(profile: &Path, started: Instant) -> Result<(u16, String), String> {
    let file = profile.join("DevToolsActivePort");
    loop {
        if let Ok(text) = std::fs::read_to_string(&file) {
            let mut lines = text.lines();
            if let (Some(port), Some(path)) = (lines.next(), lines.next()) {
                if let Ok(port) = port.trim().parse::<u16>() {
                    return Ok((port, path.trim().to_string()));
                }
            }
        }
        if started.elapsed() >= Duration::from_secs(12) {
            return Err("读不到无头 Chrome 的调试端口".into());
        }
        tokio::time::sleep(Duration::from_millis(80)).await;
    }
}

pub(crate) fn notice(app: &AppHandle) -> Option<String> {
    app.try_state::<HeadlessManager>()
        .and_then(|m| m.notice.lock().unwrap().clone())
}

pub(crate) fn prune_dead_tabs(app: &AppHandle, live_tags: &HashSet<String>) {
    if let Some(mgr) = app.try_state::<HeadlessManager>() {
        mgr.tabs.lock().unwrap().retain(|(tag, _)| live_tags.contains(tag));
    }
}

pub(crate) fn cookie_key(cookie: &Value) -> String {
    format!(
        "{}\u{1}{}\u{1}{}",
        cookie["name"].as_str().unwrap_or_default(),
        cookie["domain"].as_str().unwrap_or_default(),
        cookie["path"].as_str().unwrap_or_default()
    )
}

pub(crate) fn cookie_param(cookie: &Value) -> Option<Value> {
    if cookie.get("partitionKey").is_some_and(|k| !k.is_null()) {
        return None;
    }
    let name = cookie["name"].as_str()?;
    let value = cookie["value"].as_str()?;
    let domain = cookie["domain"].as_str().filter(|d| !d.is_empty())?;
    let secure = cookie["secure"] == true;
    let path = cookie["path"].as_str().filter(|p| !p.is_empty()).unwrap_or("/");
    let mut param = json!({
        "name": name, "value": value, "path": path,
        "secure": secure, "httpOnly": cookie["httpOnly"] == true,
    });
    if domain.starts_with('.') {
        param["domain"] = json!(domain);
    } else {
        let scheme = if secure { "https" } else { "http" };
        param["url"] = json!(format!("{scheme}://{domain}{path}"));
    }
    if let Some(ss) = cookie["sameSite"].as_str().filter(|v| matches!(*v, "Strict" | "Lax" | "None")) {
        param["sameSite"] = json!(ss);
    }
    if let Some(exp) = cookie["expires"].as_f64().filter(|e| *e > 0.0) {
        param["expires"] = json!(exp);
    }
    Some(param)
}

fn seed_profile(target: &Path) -> (usize, bool, Vec<String>) {
    let Some(source) = chrome_user_data_dir() else {
        return (0, false, vec!["未找到本机 Chrome 用户目录：无头浏览器将不带本机登录态".into()]);
    };
    let mut copied = 0;
    let mut cookies = false;
    let mut warnings = Vec::new();
    let entries: [&[&str]; 11] = [
        &["Local State"],
        &["Default", "Cookies"],
        &["Default", "Cookies-journal"],
        &["Default", "Cookies-wal"],
        &["Default", "Network", "Cookies"],
        &["Default", "Network", "Cookies-journal"],
        &["Default", "Network", "Cookies-wal"],
        &["Default", "Local Storage"],
        &["Default", "Session Storage"],
        &["Default", "IndexedDB"],
        &["Default", "Web Data"],
    ];
    for parts in entries {
        let from = parts.iter().fold(source.clone(), |p, part| p.join(part));
        if !from.exists() {
            continue;
        }
        let to = parts.iter().fold(target.to_path_buf(), |p, part| p.join(part));
        match copy_path(&from, &to) {
            Ok(true) => {
                copied += 1;
                if parts.last() == Some(&"Cookies") {
                    cookies = true;
                }
            }
            Ok(false) => {}
            Err(err) => warnings.push(format!("复制 {} 失败：{err}", parts.join("/"))),
        }
    }
    (copied, cookies, warnings)
}

fn copy_path(from: &Path, to: &Path) -> Result<bool, std::io::Error> {
    if from.is_dir() {
        std::fs::create_dir_all(to)?;
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            if entry.file_name() == "LOCK" {
                continue;
            }
            copy_path(&entry.path(), &to.join(entry.file_name()))?;
        }
        Ok(true)
    } else if from.is_file() {
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(from, to)?;
        Ok(true)
    } else {
        Ok(false)
    }
}

fn chrome_user_data_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("LOCALAPPDATA")
            .map(|b| PathBuf::from(b).join("Google").join("Chrome").join("User Data"))
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME")
            .map(|h| PathBuf::from(h).join("Library/Application Support/Google/Chrome"))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config/google-chrome"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser::annotate::{process_screenshot, SomBox};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn real_headless_chrome_pointer_and_som_roundtrip() {
        let Some(chrome) = crate::browser::ext_ws::find_chrome() else {
            return;
        };
        let html = r#"<!doctype html><html lang="zh-CN"><meta charset="utf-8"><title>Nova E2E</title>
<style>body{font-family:system-ui,sans-serif;margin:0;padding:28px;background:#f8fafc;color:#0f172a}
.card{background:#fff;border:1px solid #e2e8f0;border-radius:14px;padding:24px;max-width:760px}
.row{display:flex;gap:12px;align-items:center;margin-top:16px}
input{padding:9px 12px;border:1.5px solid #cbd5e1;border-radius:8px;font-size:15px;width:240px}
button{padding:9px 18px;border-radius:8px;border:0;background:#4f46e5;color:#fff;font-weight:600;cursor:pointer}
button.sec{background:#e2e8f0;color:#0f172a}
#log{margin-top:16px;padding:12px;background:#f1f5f9;border-radius:8px;font-family:monospace}
canvas{display:block;margin-top:16px;border:1px solid #cbd5e1;border-radius:8px}</style>
<div class="card"><h1 style="margin:0 0 8px">Nova 虚拟指针 + SoM 实测</h1>
<div class="row"><input id="q" placeholder="搜索项目或订单" value="old value"><button id="go">立即搜索</button><button id="sec" class="sec">高级筛选</button></div>
<div id="log">idle</div><canvas id="cv" width="480" height="140"></canvas></div>
<script>
const log=document.getElementById('log');
document.getElementById('go').onclick=()=>{log.textContent='SUBMIT:'+document.getElementById('q').value};
const c=document.getElementById('cv').getContext('2d');
c.fillStyle='#eef2ff';c.fillRect(0,0,480,140);c.fillStyle='#4f46e5';c.fillRect(160,40,160,56);
c.fillStyle='#fff';c.font='bold 15px sans-serif';c.fillText('Canvas Button',188,74);
</script></html>"#;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            loop {
                let Ok((mut s, _)) = listener.accept().await else { break };
                let body = html.to_string();
                tokio::spawn(async move {
                    let mut buf = [0u8; 1024];
                    let _ = s.read(&mut buf).await;
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = s.write_all(resp.as_bytes()).await;
                });
            }
        });

        let dir = tempfile::tempdir().unwrap();
        let mut cmd = Command::new(&chrome);
        cmd.arg("--headless=new")
            .arg(format!("--user-data-dir={}", dir.path().display()))
            .arg("--no-first-run")
            .arg("--no-default-browser-check")
            .arg("--window-size=1280,800")
            .arg("--force-device-scale-factor=1")
            .arg("--remote-debugging-port=0")
            .arg("about:blank");
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000);
        }
        let mut child = cmd.spawn().unwrap();
        let (ws_port, ws_path) = wait_debug_port(dir.path(), Instant::now()).await.unwrap();
        let client = CdpClient::connect(&format!("ws://127.0.0.1:{ws_port}{ws_path}"), None).await.unwrap();

        let tab = client.open_tab(&format!("http://127.0.0.1:{port}/")).await.unwrap();
        let v1 = client.view(&tab.tag, &ViewOptions::default()).await.unwrap();
        assert!(v1.items.len() >= 4, "expected input + buttons + canvas: {:?}", v1.items);
        // Session started operating: virtual cursor must be immediately visible and stay visible (no blinking).
        let cursor_check = "(() => { const h = document.getElementById('__nova-cursor-host'); if (!h) return 'missing'; const cs = getComputedStyle(h); return cs.visibility + '|' + cs.opacity; })()";
        let state_v1 = client.eval(&tab.tag, cursor_check, false).await.unwrap();
        assert_eq!(state_v1["value"], "visible|1", "cursor must appear immediately on first view: {state_v1}");

        let input_id = v1.items.iter().find(|i| i["role"] == "input").unwrap()["id"].as_u64().unwrap() as u32;
        let go_id = v1.items.iter().find(|i| i["name"].as_str().unwrap_or("").contains("立即搜索")).unwrap()["id"].as_u64().unwrap() as u32;

        let act = client
            .act(
                &tab.tag,
                &[
                    Step::Type { text: "Nova V2 指针引擎".into(), id: Some(input_id), x: None, y: None, clear: Some(true), submit: Some(false) },
                    Step::Click { id: Some(go_id), x: None, y: None, button: None, clicks: None },
                ],
                dir.path(),
                Some(&ViewOptions::default()),
            )
            .await
            .unwrap();
        assert!(act.ok, "act failed: {:?}", act.error);
        assert_eq!(act.completed, 2);
        let v2 = act.view.unwrap();
        assert!(v2.text.contains("SUBMIT:Nova V2 指针引擎"), "text={}", v2.text);
        assert!(v2.cursor.is_some(), "virtual cursor position must be reported");
        let state_v2 = client.eval(&tab.tag, cursor_check, false).await.unwrap();
        assert_eq!(state_v2["value"], "visible|1", "cursor must stay continuously visible after act & screenshot: {state_v2}");

        // Navigating during an active session must immediately restore the cursor on the new document.
        client.goto(&tab.tag, &format!("http://127.0.0.1:{port}/?page=2")).await.unwrap();
        let state_nav = client.eval(&tab.tag, cursor_check, false).await.unwrap();
        assert_eq!(state_nav["value"], "visible|1", "cursor must restore automatically after navigation: {state_nav}");

        // Session ends -> cursor fades out and cleanly unmounts with zero DOM residue.
        client.dismiss_cursor(None).await;
        tokio::time::sleep(Duration::from_millis(320)).await;
        let state_end = client.eval(&tab.tag, cursor_check, false).await.unwrap();
        assert_eq!(state_end["value"], "missing", "cursor must unmount cleanly on session end: {state_end}");

        // Render annotated SoM screenshot using annotate.rs
        let boxes: Vec<SomBox> = v2.items.iter().filter_map(|i| {
            let r = i["rect"].as_array()?;
            Some(SomBox { id: i["id"].as_u64()? as u32, rect: [r[0].as_f64()?, r[1].as_f64()?, r[2].as_f64()?, r[3].as_f64()?] })
        }).collect();
        let out_shot = std::env::var_os("NOVA_E2E_SHOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| dir.path().join("annotated"));
        let map = process_screenshot(v2.screenshot_b64.as_deref().unwrap(), (v2.viewport[0], v2.viewport[1]), None, &boxes, true, &out_shot).unwrap();
        assert_eq!(map.pixel_width as f64, v2.viewport[0]);
        assert_eq!(map.pixel_height as f64, v2.viewport[1]);
        assert!(map.path.exists());

        let _ = child.kill();
        let _ = child.wait();
        server.abort();
    }
}
