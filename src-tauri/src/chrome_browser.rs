//! Chrome extension transport. The browser engine remains in native_browser; no second action implementation.
use serde_json::{json, Value};
use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tauri::{AppHandle, Manager};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{oneshot, Notify},
};

const EXTENSION_ID: &str = "mdicifjlbgnkkdkcbfdnebeghhlamjbm";
type Pending = HashMap<String, oneshot::Sender<Result<Value, String>>>;
pub(crate) struct ChromeBridge {
    start: tokio::sync::Mutex<()>,
    config: Mutex<Option<(u16, String, PathBuf)>>,
    /// 所有在轮询的扩展（Chrome、Edge 等可同时装同一扩展）：clientId -> (最后轮询时间, 浏览器)。
    clients: Mutex<HashMap<String, (Instant, &'static str)>>,
    /// 接收 chrome 工具普通命令的扩展；有 Google Chrome 时优先它，否则沿用当前，避免标签 tag 在浏览器间漂移。
    primary: Mutex<Option<String>>,
    /// 按 clientId 分开的命令队列，每个扩展只取发给自己的命令。
    queues: Mutex<HashMap<String, VecDeque<Value>>>,
    queued: Notify,
    pending: Mutex<Pending>,
    cancellations: Mutex<HashMap<String, Arc<AtomicBool>>>,
    /// Opens miniapp windows for page-launcher clicks; unset in transport-only tests.
    app: std::sync::OnceLock<AppHandle>,
}
impl ChromeBridge {
    fn new() -> Self {
        Self {
            start: tokio::sync::Mutex::new(()),
            config: Mutex::new(None),
            clients: Mutex::new(HashMap::new()),
            primary: Mutex::new(None),
            queues: Mutex::new(HashMap::new()),
            queued: Notify::new(),
            pending: Mutex::new(HashMap::new()),
            cancellations: Mutex::new(HashMap::new()),
            app: std::sync::OnceLock::new(),
        }
    }
    /// 45 秒内轮询过的扩展，按 clientId 排序；顺带清掉断开扩展的队列。
    fn live_clients(&self) -> Vec<(String, &'static str)> {
        let mut clients = self.clients.lock().unwrap();
        clients.retain(|_, (seen, _)| seen.elapsed() < Duration::from_secs(45));
        self.queues.lock().unwrap().retain(|id, _| clients.contains_key(id));
        let mut live: Vec<_> = clients.iter().map(|(id, (_, browser))| (id.clone(), *browser)).collect();
        live.sort();
        live
    }
    fn primary(&self) -> Option<(String, &'static str)> {
        let live = self.live_clients();
        let chrome = live.iter().find(|(_, browser)| *browser == "Google Chrome");
        let mut primary = self.primary.lock().unwrap();
        let chosen = match primary.as_ref().and_then(|id| live.iter().find(|(c, _)| c == id)) {
            Some(current) if current.1 == "Google Chrome" || chrome.is_none() => Some(current),
            _ => chrome.or(live.first()),
        }
        .cloned();
        *primary = chosen.as_ref().map(|(id, _)| id.clone());
        chosen
    }
    fn connected(&self) -> bool {
        self.primary().is_some()
    }
    async fn request(self: &Arc<Self>, operation: &str, args: Value) -> Result<Value, String> {
        let Some((client, _)) = self.primary() else {
            return Err(
                "Chrome 未连接；安装独立 Nova Chrome 扩展后自动连接，在扩展中授权标签页".into(),
            );
        };
        self.request_to(&client, operation, args).await
    }
    /// 取出发给该扩展、仍在等待且未过期的下一条命令。
    fn take(&self, client: &str) -> Option<Value> {
        let mut queues = self.queues.lock().unwrap();
        let queue = queues.get_mut(client)?;
        while let Some(command) = queue.pop_front() {
            if command["id"].as_str().is_some_and(|id| self.pending.lock().unwrap().contains_key(id))
                && command["expiresAt"].as_i64().unwrap_or(0) > chrono::Utc::now().timestamp_millis()
            {
                return Some(command);
            }
        }
        None
    }
    async fn request_to(self: &Arc<Self>, client: &str, operation: &str, args: Value) -> Result<Value, String> {
        let id = uuid::Uuid::new_v4().to_string();
        let (send, receive) = oneshot::channel();
        self.pending.lock().unwrap().insert(id.clone(), send);
        let _pending = PendingCall(self.clone(), id.clone());
        // A busy renderer can delay CDP while processing a large SPA; this is a
        // deadline, not a sleep, and commands are still sent exactly once.
        let timeout = Duration::from_secs(if matches!(operation, "cdp" | "devtools") { 10 } else { 3 });
        let expires = chrono::Utc::now().timestamp_millis() + timeout.as_millis() as i64;
        {
            let mut queues = self.queues.lock().unwrap();
            let queue = queues.entry(client.to_string()).or_default();
            if queue.len() >= 128 {
                return Err("Chrome 命令队列已满".into());
            }
            queue.push_back(json!({"id":id,"operation":operation,"args":args,"expiresAt":expires}));
        }
        self.queued.notify_waiters();
        tokio::time::timeout(timeout, receive)
            .await
            .map_err(|_| "Chrome 响应超时；结果可能已执行，请先观察，不要重放动作")?
            .map_err(|_| "Chrome 连接中断，请重新观察")?
    }
}
struct PendingCall(Arc<ChromeBridge>, String);
impl Drop for PendingCall {
    fn drop(&mut self) {
        self.0.pending.lock().unwrap().remove(&self.1);
    }
}
pub(crate) fn init(app: &AppHandle) {
    let bridge = Arc::new(ChromeBridge::new());
    let _ = bridge.app.set(app.clone());
    app.manage(bridge);
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(error) = connect(&app).await {
            eprintln!("Chrome bridge: {error}");
        }
    });
}
fn bridge(app: &AppHandle) -> Arc<ChromeBridge> {
    app.state::<Arc<ChromeBridge>>().inner().clone()
}
pub(crate) fn tool_definition() -> Value {
    serde_json::from_str(include_str!("../../scripts/chrome-tool.json"))
        .expect("chrome tool schema")
}

async fn bind_listener(
    ports: std::ops::RangeInclusive<u16>,
) -> Result<tokio::net::TcpListener, String> {
    let mut last_error = None;
    for port in ports.clone() {
        match tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
            Ok(listener) => return Ok(listener),
            Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => last_error = Some(error),
            Err(error) => return Err(format!("Chrome 本机端口 {port} 无法启动：{error}")),
        }
    }
    Err(format!(
        "Chrome 本机端口 {}–{} 均被占用：{}",
        ports.start(),
        ports.end(),
        last_error.unwrap()
    ))
}

fn browser_name(user_agent: &str) -> &'static str {
    if user_agent.contains("Edg/") { "Microsoft Edge" }
    else if user_agent.contains("OPR/") { "Opera" }
    else if user_agent.contains("Chrome/") { "Google Chrome" }
    else { "unknown" }
}

pub(crate) async fn connect(app: &AppHandle) -> Result<Value, String> {
    let state = bridge(app);
    let _start = state.start.lock().await;
    if state.config.lock().unwrap().is_none() {
        let directory = app
            .state::<crate::AppState>()
            .config_dir
            .join("chrome-extension");
        std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
        let path = directory.join("config.json");
        let ports = std::env::var("NOVA_CHROME_PORT")
            .ok()
            .and_then(|s| s.parse::<u16>().ok())
            .map(|port| port..=port)
            .unwrap_or(47653..=47662);
        // Pairing is automatic; each process owns its token, even when sharing a data directory.
        let token = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let listener = bind_listener(ports).await?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        std::fs::write(
            path,
            json!({"origin":format!("http://127.0.0.1:{port}"),"token":token}).to_string(),
        )
        .map_err(|e| e.to_string())?;
        *state.config.lock().unwrap() = Some((port, token, directory));
        let shared = state.clone();
        tauri::async_runtime::spawn(async move {
            // ponytail: bound local HTTP connections at 32; use an HTTP library if the bridge grows beyond two routes.
            let slots = Arc::new(tokio::sync::Semaphore::new(32));
            while let Ok((stream, _)) = listener.accept().await {
                let Ok(slot) = slots.clone().try_acquire_owned() else {
                    continue;
                };
                let shared = shared.clone();
                tauri::async_runtime::spawn(async move {
                    let _slot = slot;
                    let _ =
                        tokio::time::timeout(Duration::from_secs(25), serve(stream, shared)).await;
                });
            }
        });
    }
    let config = state.config.lock().unwrap();
    let (port, _, _) = config.as_ref().unwrap();
    Ok(
        json!({"connected":state.connected(),"browser":state.primary().map(|(_, b)| b),
            "browsers":state.live_clients().iter().map(|(_, b)| *b).collect::<Vec<_>>(),
            "origin":format!("http://127.0.0.1:{port}"),"extensionId":EXTENSION_ID,
            "next":"独立 Nova Chrome 扩展会自动连接本机 Nova，在扩展中勾选允许控制的标签；浏览器网页操作使用 chrome。多个浏览器同时装了扩展时，chrome 工具操作 browser 所示的那个（优先 Google Chrome），browsers 列出全部已连接浏览器。"}),
    )
}
pub(crate) async fn request(
    app: &AppHandle,
    operation: &str,
    args: Value,
) -> Result<Value, String> {
    bridge(app).request(operation, args).await
}
/// 每个已连接扩展各自的浏览器、版本与无痕权限；查询失败的扩展 version 为 null（视为过期）。
pub(crate) async fn extension_statuses(app: &AppHandle) -> Vec<Value> {
    client_statuses(&bridge(app)).await.into_iter().map(|(_, status)| status).collect()
}
async fn client_statuses(state: &Arc<ChromeBridge>) -> Vec<(String, Value)> {
    let mut statuses = Vec::new();
    for (client, browser) in state.live_clients() {
        let status = state.request_to(&client, "status", json!({})).await.unwrap_or(Value::Null);
        statuses.push((client, json!({"browser":browser,"version":status["version"],"incognitoAllowed":status["incognitoAllowed"] == true})));
    }
    statuses
}
pub(crate) fn begin(app: &AppHandle, tag: &str) -> Arc<AtomicBool> {
    let flag = Arc::new(AtomicBool::new(false));
    if let Some(old) = bridge(app)
        .cancellations
        .lock()
        .unwrap()
        .insert(tag.into(), flag.clone())
    {
        old.store(true, Ordering::SeqCst);
    }
    flag
}
pub(crate) fn stop(app: &AppHandle, tag: &str) {
    if let Some(flag) = bridge(app).cancellations.lock().unwrap().get(tag) {
        flag.store(true, Ordering::SeqCst);
    }
}

async fn respond(
    stream: &mut tokio::net::TcpStream,
    status: u16,
    body: Value,
) -> Result<(), String> {
    let text = body.to_string();
    let response=format!("HTTP/1.1 {status} Result\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\nAccess-Control-Allow-Origin: chrome-extension://{EXTENSION_ID}\r\nAccess-Control-Allow-Headers: Authorization, Content-Type\r\nAccess-Control-Allow-Methods: POST, OPTIONS\r\n\r\n{text}",text.len());
    stream
        .write_all(response.as_bytes())
        .await
        .map_err(|e| e.to_string())
}
async fn serve(mut stream: tokio::net::TcpStream, state: Arc<ChromeBridge>) -> Result<(), String> {
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 8192];
    let header_end = loop {
        if let Some(at) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            break at + 4;
        }
        if bytes.len() > 16384 {
            return respond(&mut stream, 431, json!({"error":"Headers too large"})).await;
        }
        let n = stream.read(&mut chunk).await.map_err(|e| e.to_string())?;
        if n == 0 {
            return Ok(());
        }
        bytes.extend_from_slice(&chunk[..n]);
    };
    let header = std::str::from_utf8(&bytes[..header_end]).map_err(|_| "Invalid headers")?;
    let mut lines = header.split("\r\n");
    let request = lines.next().unwrap_or_default();
    let mut headers = HashMap::new();
    for line in lines.filter(|line| !line.is_empty()) {
        let (name, value) = line.split_once(':').ok_or("Invalid header")?;
        if headers
            .insert(name.to_ascii_lowercase(), value.trim().to_string())
            .is_some()
        {
            return Err("Duplicate header".into());
        }
    }
    let origin = format!("chrome-extension://{EXTENSION_ID}");
    if headers.get("origin") != Some(&origin) {
        return respond(
            &mut stream,
            403,
            json!({"error":"Invalid extension origin"}),
        )
        .await;
    }
    if request.starts_with("OPTIONS ") {
        return respond(&mut stream, 200, json!({})).await;
    }
    let token = state
        .config
        .lock()
        .unwrap()
        .as_ref()
        .map(|(_, token, _)| token.clone())
        .ok_or("Bridge not configured")?;
    // Pair only after validating the fixed extension origin; ordinary websites cannot read the token.
    if request == "POST /pair HTTP/1.1" {
        return respond(&mut stream, 200, json!({"token":token})).await;
    }
    if headers.get("authorization") != Some(&format!("Bearer {token}")) {
        return respond(&mut stream, 403, json!({"error":"Unauthorized"})).await;
    }
    if !matches!(request, "POST /poll HTTP/1.1" | "POST /reply HTTP/1.1" | "POST /miniapp/triggers HTTP/1.1" | "POST /miniapp/launch HTTP/1.1") {
        return respond(&mut stream, 404, json!({})).await;
    }
    let request = request.to_owned(); // `bytes` grows below while the route is still needed.
    let poll = request == "POST /poll HTTP/1.1";
    let size = headers
        .get("content-length")
        .and_then(|s| s.parse::<usize>().ok())
        .ok_or("Content-Length required")?;
    if size > 32 * 1024 * 1024 || headers.contains_key("transfer-encoding") {
        return respond(
            &mut stream,
            413,
            json!({"error":"Body too large or unsupported encoding"}),
        )
        .await;
    }
    while bytes.len() < header_end + size {
        let n = stream.read(&mut chunk).await.map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("Incomplete request".into());
        }
        bytes.extend_from_slice(&chunk[..n]);
    }
    let body: Value =
        serde_json::from_slice(&bytes[header_end..header_end + size]).map_err(|e| e.to_string())?;
    let client = body["clientId"]
        .as_str()
        .filter(|s| uuid::Uuid::parse_str(s).is_ok())
        .ok_or("Invalid clientId")?;
    let accepted = {
        let mut clients = state.clients.lock().unwrap();
        if poll {
            let browser = browser_name(headers.get("user-agent").map_or("", String::as_str));
            clients.insert(client.into(), (Instant::now(), browser));
            true
        } else {
            // Page launchers may ask right after pairing, before the first poll registers the client.
            request.starts_with("POST /miniapp/") || clients.contains_key(client)
        }
    };
    if !accepted {
        return respond(&mut stream, 409, json!({"error":"Unknown Chrome client"})).await;
    }
    if request == "POST /miniapp/triggers HTTP/1.1" {
        return respond(&mut stream, 200, json!({"triggers": crate::miniapp_window::browser_triggers()})).await;
    }
    if request == "POST /miniapp/launch HTTP/1.1" {
        let result = match state.app.get() {
            Some(app) => crate::miniapp_window::browser_launch(app, &body).await,
            None => Err("Nova 尚未就绪".into()),
        };
        return match result {
            Ok(()) => respond(&mut stream, 200, json!({"ok": true})).await,
            Err(error) => respond(&mut stream, 400, json!({"error": error})).await,
        };
    }
    if poll {
        let command = tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                // 先登记唤醒再查队列，避免查完到等待之间入队的命令被漏掉。
                let queued = state.queued.notified();
                tokio::pin!(queued);
                queued.as_mut().enable();
                if let Some(command) = state.take(client) {
                    return command;
                }
                queued.await;
            }
        })
        .await
        .ok();
        respond(&mut stream, 200, json!({"command":command})).await
    } else {
        let sender = body["id"]
            .as_str()
            .and_then(|id| state.pending.lock().unwrap().remove(id));
        if let Some(sender) = sender {
            let _ = sender.send(if let Some(error) = body["error"].as_str() {
                Err(error.into())
            } else {
                Ok(body["result"].clone())
            });
        }
        respond(&mut stream, 200, json!({"ok":true})).await
    }
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
        return connect(&app).await;
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
        .map(|t| PathBuf::from(&t.cwd))
        .ok_or("会话不存在")?;
    let mut args = args;
    args["operation"] = json!(operation);
    crate::native_browser::execute_chrome(&root, &args, &format!("ui:{id}")).await
}

const EXTENSION_FILES: [(&str, &str); 5] = [
    ("manifest.json", include_str!("../../extensions/nova-chrome/manifest.json")),
    ("worker.js", include_str!("../../extensions/nova-chrome/worker.js")),
    ("popup.html", include_str!("../../extensions/nova-chrome/popup.html")),
    ("popup.js", include_str!("../../extensions/nova-chrome/popup.js")),
    ("content.js", include_str!("../../extensions/nova-chrome/content.js")),
];

fn find_chrome() -> Option<PathBuf> {
    #[cfg(windows)]
    let candidates: Vec<PathBuf> = ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"]
        .iter()
        .filter_map(|key| std::env::var_os(key))
        .map(|base| PathBuf::from(base).join(r"Google\Chrome\Application\chrome.exe"))
        .collect();
    #[cfg(target_os = "macos")]
    let candidates = vec![PathBuf::from("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome")];
    #[cfg(all(unix, not(target_os = "macos")))]
    let candidates: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
        .flat_map(|dir| ["google-chrome", "google-chrome-stable"].map(|name| dir.join(name)))
        .collect();
    candidates.into_iter().find(|path| path.is_file())
}

/// 检测 Chrome 依赖：Chrome 是否安装、扩展是否已连接及是否允许无痕；同时释放自带扩展到固定目录供「加载已解压的扩展程序」使用。
#[tauri::command]
pub async fn chrome_dependency_status(app: AppHandle) -> Result<Value, String> {
    let directory = app
        .state::<crate::AppState>()
        .config_dir
        .join("chrome-extension")
        .join("nova-chrome");
    std::fs::create_dir_all(&directory).map_err(|e| format!("释放 Chrome 扩展失败：{e}"))?;
    for (name, content) in EXTENSION_FILES {
        std::fs::write(directory.join(name), content).map_err(|e| format!("释放 Chrome 扩展失败：{e}"))?;
    }
    let connected = connect(&app).await.map(|v| v["connected"] == true).unwrap_or(false);
    let bundled_version = serde_json::from_str::<Value>(EXTENSION_FILES[0].1).ok().and_then(|m| m["version"].as_str().map(String::from));
    // 新文件已写入扩展目录：过期且认识 reload 的扩展直接自行重载升级，不动界面。
    // 重载后扩展以新 clientId 重连；从别的目录加载的重载后版本不变，仍留给剑来移除再加载。
    let mut upgraded = Vec::new();
    if connected {
        let state = bridge(&app);
        let before = state.live_clients().len();
        for (client, status) in client_statuses(&state).await {
            if status["version"].as_str() != bundled_version.as_deref()
                && state.request_to(&client, "reload", json!({})).await.is_ok()
            {
                state.clients.lock().unwrap().remove(&client);
                upgraded.push(status["browser"].clone());
            }
        }
        if !upgraded.is_empty() {
            let deadline = Instant::now() + Duration::from_secs(10);
            while state.live_clients().len() < before && Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(300)).await;
            }
        }
    }
    let connected = connect(&app).await.map(|v| v["connected"] == true).unwrap_or(false);
    let status = if connected { request(&app, "status", json!({})).await.ok() } else { None };
    let incognito_allowed = status.as_ref().map(|v| v["incognitoAllowed"] == true);
    // 旧版扩展不上报 version，也视为过期；任一已连接浏览器过期都需要更新。
    let installed_version = status.as_ref().and_then(|v| v["version"].as_str().map(String::from));
    let extensions: Vec<Value> = extension_statuses(&app).await.into_iter()
        .map(|mut e| { e["outdated"] = json!(e["version"].as_str() != bundled_version.as_deref()); e })
        .collect();
    let outdated = connected && (installed_version != bundled_version || extensions.iter().any(|e| e["outdated"] == true));
    Ok(json!({
        "chromePath": find_chrome().map(|p| p.to_string_lossy().to_string()),
        "connected": connected,
        "incognitoAllowed": incognito_allowed,
        "installedVersion": installed_version,
        "bundledVersion": bundled_version,
        "outdated": outdated,
        "upgraded": upgraded,
        "extensions": extensions,
        "extensionDir": directory.to_string_lossy(),
        "extensionId": EXTENSION_ID,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn browser_name_tells_edge_from_chrome() {
        assert_eq!(browser_name("Mozilla/5.0 Chrome/130.0.0.0 Safari/537.36 Edg/130.0.0.0"), "Microsoft Edge");
        assert_eq!(browser_name("Mozilla/5.0 Chrome/130.0.0.0 Safari/537.36"), "Google Chrome");
    }
    #[tokio::test]
    async fn cdp_waits_for_busy_renderer_without_resending() {
        let state = Arc::new(ChromeBridge::new());
        state.clients.lock().unwrap().insert("test".into(), (Instant::now(), "Google Chrome"));
        let shared = state.clone();
        let request = tokio::spawn(async move { shared.request("cdp", json!({"method":"Runtime.evaluate"})).await });
        let command = loop {
            if let Some(command) = state.take("test") { break command; }
            tokio::task::yield_now().await;
        };
        assert!(command["expiresAt"].as_i64().unwrap() - chrono::Utc::now().timestamp_millis() > 8000);
        tokio::time::sleep(Duration::from_millis(3100)).await;
        state.pending.lock().unwrap().remove(command["id"].as_str().unwrap()).unwrap().send(Ok(json!({"ok":true}))).unwrap();
        assert_eq!(request.await.unwrap().unwrap()["ok"], true);
        assert!(state.take("test").is_none());
    }
    #[test]
    fn primary_prefers_chrome_and_sticks_otherwise() {
        let state = ChromeBridge::new();
        let add = |id: &str, browser| { state.clients.lock().unwrap().insert(id.into(), (Instant::now(), browser)); };
        add("b-edge", "Microsoft Edge");
        assert_eq!(state.primary().unwrap().0, "b-edge");
        add("a-opera", "Opera");
        assert_eq!(state.primary().unwrap().0, "b-edge", "no Chrome: keep current to avoid tag drift");
        add("c-chrome", "Google Chrome");
        assert_eq!(state.primary().unwrap().0, "c-chrome");
        state.clients.lock().unwrap().get_mut("c-chrome").unwrap().0 = Instant::now() - Duration::from_secs(60);
        assert_ne!(state.primary().unwrap().0, "c-chrome", "stale client is dropped");
        assert_eq!(state.live_clients().len(), 2);
    }
    #[tokio::test]
    async fn occupied_port_falls_back_and_exhaustion_is_reported() {
        let occupied = bind_listener(0..=0).await.unwrap();
        let port = occupied.local_addr().unwrap().port();
        let error = bind_listener(port..=port).await.unwrap_err();
        assert!(error.contains("均被占用"));
        // Ephemeral ports can include 65535; keep the fallback range non-empty.
        if port < u16::MAX {
            let next = bind_listener(port..=u16::MAX).await.unwrap();
            assert_ne!(next.local_addr().unwrap().port(), port);
            assert!(next.local_addr().unwrap().ip().is_loopback());
        }
    }

    #[tokio::test]
    async fn authenticated_bridge_roundtrip_and_cancelled_commands_are_not_replayed() {
        let state = Arc::new(ChromeBridge::new());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        *state.config.lock().unwrap() = Some((port, "test-token".into(), PathBuf::new()));
        let shared = state.clone();
        let server = tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let state = shared.clone();
                tokio::spawn(async move {
                    let _ = serve(stream, state).await;
                });
            }
        });
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        let url = format!("http://127.0.0.1:{port}");
        assert_eq!(
            http.post(format!("{url}/pair"))
                .header("Origin", "https://example.com")
                .send()
                .await
                .unwrap()
                .status(),
            403
        );
        let paired: Value = http
            .post(format!("{url}/pair"))
            .header("Origin", format!("chrome-extension://{EXTENSION_ID}"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(paired["token"], "test-token");
        let client_id = uuid::Uuid::new_v4().to_string();
        let post = |path: &str, value: Value| {
            http.post(format!("{url}{path}"))
                .header("Origin", format!("chrome-extension://{EXTENSION_ID}"))
                .bearer_auth("test-token")
                .json(&value)
        };
        assert_eq!(
            http.post(format!("{url}/poll"))
                .json(&json!({}))
                .send()
                .await
                .unwrap()
                .status(),
            403
        );
        state.clients.lock().unwrap().insert(client_id.clone(), (Instant::now(), "Google Chrome"));
        // Simulate a caller future being cancelled after enqueueing, before the extension polls.
        let cancelled_state = state.clone();
        let cancelled = tokio::spawn(async move {
            cancelled_state
                .request(
                    "cdp",
                    json!({"tabTag":"C1-test","method":"Input.insertText"}),
                )
                .await
        });
        tokio::time::timeout(Duration::from_secs(1), async {
            while state.pending.lock().unwrap().is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        cancelled.abort();
        let _ = cancelled.await;
        assert!(state.pending.lock().unwrap().is_empty());
        let request_state = state.clone();
        let request = tokio::spawn(async move { request_state.request("tabs", json!({})).await });
        let poll: Value = post("/poll", json!({"clientId":client_id}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(
            poll["command"]["operation"], "tabs",
            "cancelled mutation must never reach Chrome"
        );
        let response=post("/reply",json!({"clientId":client_id,"id":poll["command"]["id"],"result":{"tabs":[{"tag":"C1-test"}]}})).send().await.unwrap();
        assert!(response.status().is_success());
        assert_eq!(request.await.unwrap().unwrap()["tabs"][0]["tag"], "C1-test");
        assert!(state.pending.lock().unwrap().is_empty());
        let triggers: Value = post("/miniapp/triggers", json!({"clientId":client_id})).send().await.unwrap().json().await.unwrap();
        assert!(triggers["triggers"].is_array());
        let launch = post("/miniapp/launch", json!({"clientId":client_id,"appId":"12345678-1234-4abc-8def-123456789abc","url":"https://a/"})).send().await.unwrap();
        assert_eq!(launch.status(), 400, "unconfigured apps never open from a page");
        // 第二个浏览器（如 Edge）可以同时连接，只取发给自己的命令；未轮询过的客户端不能回复。
        let edge = uuid::Uuid::new_v4().to_string();
        let edge_poll = tokio::spawn(post("/poll", json!({"clientId":edge})).header("User-Agent", "Chrome/130.0 Edg/130.0").send());
        tokio::time::timeout(Duration::from_secs(2), async {
            while state.live_clients().len() < 2 { tokio::task::yield_now().await; }
        }).await.unwrap();
        assert_eq!(state.primary().unwrap().0, client_id, "Chrome stays primary");
        let edge_state = state.clone();
        let edge_id = edge.clone();
        let edge_request = tokio::spawn(async move { edge_state.request_to(&edge_id, "status", json!({})).await });
        let polled: Value = edge_poll.await.unwrap().unwrap().json().await.unwrap();
        assert_eq!(polled["command"]["operation"], "status");
        post("/reply", json!({"clientId":edge,"id":polled["command"]["id"],"result":{"version":"0.1.7"}})).send().await.unwrap();
        assert_eq!(edge_request.await.unwrap().unwrap()["version"], "0.1.7");
        let other = post(
            "/reply",
            json!({"clientId":uuid::Uuid::new_v4().to_string()}),
        )
        .send()
        .await
        .unwrap();
        assert_eq!(other.status(), 409);
        server.abort();
    }
}
