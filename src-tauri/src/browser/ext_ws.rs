//! Persistent local WebSocket bridge for the Nova Chrome extension (v0.2.0).
//! Replaces legacy HTTP Connection:close long-polling with full-duplex WebSocket frames
//! and compound macro commands (1 round-trip per tool call instead of 15–30).
use crate::browser::protocol::PAGE_RUNTIME;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::{Duration, Instant},
};
use tauri::{AppHandle, Manager};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{mpsc, oneshot},
};
use tokio_tungstenite::tungstenite::{
    handshake::server::{ErrorResponse, Request, Response},
    http::StatusCode,
    Message,
};

pub(crate) const EXTENSION_ID: &str = "mdicifjlbgnkkdkcbfdnebeghhlamjbm";

const EXTENSION_FILES: [(&str, &str); 5] = [
    ("manifest.json", include_str!("../../../extensions/nova-chrome/manifest.json")),
    ("worker.js", include_str!("../../../extensions/nova-chrome/worker.js")),
    ("popup.html", include_str!("../../../extensions/nova-chrome/popup.html")),
    ("popup.js", include_str!("../../../extensions/nova-chrome/popup.js")),
    ("content.js", include_str!("../../../extensions/nova-chrome/content.js")),
];

struct WsClient {
    tx: mpsc::UnboundedSender<String>,
    browser: &'static str,
    connected_at: Instant,
}

pub(crate) struct ExtBridge {
    start: tokio::sync::Mutex<()>,
    port: Mutex<Option<u16>>,
    clients: Mutex<HashMap<String, WsClient>>,
    primary: Mutex<Option<String>>,
    pending: Mutex<HashMap<String, oneshot::Sender<Result<Value, String>>>>,
    prefer_headless: AtomicBool,
    session_headless: Mutex<HashMap<String, bool>>,
    app: OnceLock<AppHandle>,
}

impl ExtBridge {
    fn new() -> Self {
        Self {
            start: tokio::sync::Mutex::new(()),
            port: Mutex::new(None),
            clients: Mutex::new(HashMap::new()),
            primary: Mutex::new(None),
            pending: Mutex::new(HashMap::new()),
            prefer_headless: AtomicBool::new(false),
            session_headless: Mutex::new(HashMap::new()),
            app: OnceLock::new(),
        }
    }

    fn live_clients(&self) -> Vec<(String, &'static str)> {
        let clients = self.clients.lock().unwrap();
        let mut out: Vec<_> = clients.iter().map(|(id, c)| (id.clone(), c.browser)).collect();
        out.sort();
        out
    }

    fn primary_client(&self) -> Option<(String, &'static str, mpsc::UnboundedSender<String>)> {
        let clients = self.clients.lock().unwrap();
        if clients.is_empty() {
            return None;
        }
        let mut primary = self.primary.lock().unwrap();
        let chrome = clients.iter().find(|(_, c)| c.browser == "Google Chrome");
        let chosen_id = match primary.as_ref().and_then(|id| clients.get(id).map(|c| (id, c))) {
            Some((id, c)) if c.browser == "Google Chrome" || chrome.is_none() => id.clone(),
            _ => chrome
                .or_else(|| clients.iter().min_by_key(|(_, c)| c.connected_at))
                .map(|(id, _)| id.clone())?,
        };
        *primary = Some(chosen_id.clone());
        let c = clients.get(&chosen_id)?;
        Some((chosen_id, c.browser, c.tx.clone()))
    }

    fn session_prefer(&self, owner: &str) -> bool {
        let global = self.prefer_headless.load(Ordering::Relaxed);
        let mut sessions = self.session_headless.lock().unwrap();
        if sessions.len() >= 1024 {
            sessions.clear();
        }
        *sessions.entry(owner.to_string()).or_insert(global)
    }

    async fn call_client(
        &self,
        tx: &mpsc::UnboundedSender<String>,
        op: &str,
        args: Value,
        timeout: Duration,
    ) -> Result<Value, String> {
        let id = uuid::Uuid::new_v4().to_string();
        let (reply_tx, reply_rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id.clone(), reply_tx);
        let payload = json!({ "type": "cmd", "id": id, "op": op, "args": args });
        if tx.send(payload.to_string()).is_err() {
            self.pending.lock().unwrap().remove(&id);
            return Err("Chrome 扩展连接已断开".into());
        }
        match tokio::time::timeout(timeout, reply_rx).await {
            Ok(Ok(res)) => res,
            Ok(Err(_)) => Err("Chrome 扩展连接中断".into()),
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                Err(format!("Chrome 扩展执行 {op} 超时；请先查看页面状态，不要盲目重放"))
            }
        }
    }
}

pub(crate) fn init(app: &AppHandle) {
    let bridge = Arc::new(ExtBridge::new());
    let _ = bridge.app.set(app.clone());
    app.manage(bridge);
    if let Err(err) = release_extension(app) {
        eprintln!("Browser extension release: {err}");
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(err) = ensure_listening(&app).await {
            eprintln!("Browser WS bridge: {err}");
        }
    });
}

fn bridge(app: &AppHandle) -> Arc<ExtBridge> {
    app.state::<Arc<ExtBridge>>().inner().clone()
}

pub(crate) fn set_prefer_headless(app: &AppHandle, enabled: bool) {
    bridge(app).prefer_headless.store(enabled, Ordering::Relaxed);
}

pub(crate) fn prefers_headless(app: &AppHandle) -> bool {
    bridge(app).prefer_headless.load(Ordering::Relaxed)
}

pub(crate) fn prefers_headless_for(app: &AppHandle, owner: &str) -> bool {
    prefers_headless(app) && bridge(app).session_prefer(owner)
}

pub(crate) fn is_connected(app: &AppHandle) -> bool {
    bridge(app).primary_client().is_some()
}

pub(crate) fn primary_browser(app: &AppHandle) -> Option<&'static str> {
    bridge(app).primary_client().map(|(_, b, _)| b)
}

pub(crate) fn live_browsers(app: &AppHandle) -> Vec<String> {
    bridge(app).live_clients().into_iter().map(|(_, b)| b.to_string()).collect()
}

pub(crate) async fn request(app: &AppHandle, op: &str, args: Value) -> Result<Value, String> {
    let _ = ensure_listening(app).await;
    let state = bridge(app);
    let Some((_, _, tx)) = state.primary_client() else {
        return Err("Chrome 扩展未连接；请在 Chrome 中加载 Nova Chrome 扩展，或开启「无头浏览器」开关".into());
    };
    let timeout = Duration::from_secs(if matches!(op, "act" | "view" | "open" | "goto" | "reload") { 25 } else { 10 });
    state.call_client(&tx, op, args, timeout).await
}

pub(crate) async fn export_cookies(app: &AppHandle) -> Result<Vec<Value>, String> {
    let res = request(app, "cookies_export", json!({})).await?;
    Ok(res["cookies"].as_array().cloned().unwrap_or_default())
}

pub(crate) async fn import_cookies(app: &AppHandle, cookies: &[Value]) -> Result<usize, String> {
    if cookies.is_empty() {
        return Ok(0);
    }
    let existing = export_cookies(app).await.unwrap_or_default();
    let same: HashMap<String, String> = existing
        .iter()
        .map(|c| (crate::browser::cdp_direct::cookie_key(c), c["value"].as_str().unwrap_or_default().to_string()))
        .collect();
    let filtered: Vec<Value> = cookies
        .iter()
        .filter(|c| same.get(&crate::browser::cdp_direct::cookie_key(c)).map(String::as_str) != Some(c["value"].as_str().unwrap_or_default()))
        .filter_map(crate::browser::cdp_direct::cookie_param)
        .collect();
    if filtered.is_empty() {
        return Ok(0);
    }
    let res = request(app, "cookies_import", json!({ "cookies": filtered })).await?;
    Ok(res["count"].as_u64().unwrap_or(filtered.len() as u64) as usize)
}

pub(crate) async fn extension_statuses(app: &AppHandle) -> Vec<Value> {
    let state = bridge(app);
    let list: Vec<(String, &'static str, mpsc::UnboundedSender<String>)> = {
        state
            .clients
            .lock()
            .unwrap()
            .iter()
            .map(|(id, c)| (id.clone(), c.browser, c.tx.clone()))
            .collect()
    };
    let mut out = Vec::new();
    for (_, browser, tx) in list {
        let status = state
            .call_client(&tx, "status", json!({}), Duration::from_secs(3))
            .await
            .unwrap_or(Value::Null);
        out.push(json!({
            "browser": browser,
            "version": status["version"],
            "incognitoAllowed": status["incognitoAllowed"] == true,
        }));
    }
    out
}

pub(crate) async fn upgrade_outdated_extensions(app: &AppHandle) -> Vec<Value> {
    let state = bridge(app);
    let target_ver = bundled_version();
    let list: Vec<(String, &'static str, mpsc::UnboundedSender<String>)> = {
        state
            .clients
            .lock()
            .unwrap()
            .iter()
            .map(|(id, c)| (id.clone(), c.browser, c.tx.clone()))
            .collect()
    };
    let mut upgraded = Vec::new();
    for (id, browser, tx) in list {
        let status = state.call_client(&tx, "status", json!({}), Duration::from_secs(3)).await.unwrap_or(Value::Null);
        if status["version"].as_str() != target_ver.as_deref() {
            if state.call_client(&tx, "reload_ext", json!({}), Duration::from_secs(3)).await.is_ok() {
                state.clients.lock().unwrap().remove(&id);
                upgraded.push(json!(browser));
            }
        }
    }
    if !upgraded.is_empty() {
        tokio::time::sleep(Duration::from_millis(1200)).await;
    }
    upgraded
}

pub(crate) async fn ensure_listening(app: &AppHandle) -> Result<u16, String> {
    let state = bridge(app);
    let _lock = state.start.lock().await;
    if let Some(p) = *state.port.lock().unwrap() {
        return Ok(p);
    }
    let ports = std::env::var("NOVA_CHROME_PORT")
        .ok()
        .and_then(|s| s.parse::<u16>().ok())
        .map(|p| p..=p)
        .unwrap_or(47653..=47662);
    let listener = bind_listener(ports).await?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    *state.port.lock().unwrap() = Some(port);

    let shared = state.clone();
    tauri::async_runtime::spawn(async move {
        let slots = Arc::new(tokio::sync::Semaphore::new(64));
        while let Ok((stream, _)) = listener.accept().await {
            let Ok(slot) = slots.clone().try_acquire_owned() else { continue };
            let shared = shared.clone();
            tauri::async_runtime::spawn(async move {
                let _slot = slot;
                let _ = stream.set_nodelay(true);
                let _ = handle_connection(stream, shared).await;
            });
        }
    });
    Ok(port)
}

async fn bind_listener(ports: std::ops::RangeInclusive<u16>) -> Result<tokio::net::TcpListener, String> {
    let mut last_err = None;
    for port in ports.clone() {
        match tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
            Ok(l) => return Ok(l),
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => last_err = Some(e),
            Err(e) => return Err(format!("浏览器端口 {port} 启动失败：{e}")),
        }
    }
    Err(format!("浏览器端口 {}–{} 均被占用：{}", ports.start(), ports.end(), last_err.unwrap()))
}

fn browser_name(ua: &str) -> &'static str {
    if ua.contains("Edg/") { "Microsoft Edge" }
    else if ua.contains("OPR/") { "Opera" }
    else if ua.contains("Chrome/") { "Google Chrome" }
    else { "Chrome" }
}

async fn handle_connection(stream: tokio::net::TcpStream, state: Arc<ExtBridge>) -> Result<(), String> {
    let mut peek_buf = [0u8; 16];
    let n = tokio::time::timeout(Duration::from_secs(5), stream.peek(&mut peek_buf))
        .await
        .map_err(|_| "peek timeout")?
        .map_err(|e| e.to_string())?;
    if n == 0 {
        return Ok(());
    }
    if peek_buf[..n].starts_with(b"GET /ws") {
        serve_ws(stream, state).await
    } else {
        serve_legacy_http(stream).await
    }
}

async fn serve_ws(stream: tokio::net::TcpStream, state: Arc<ExtBridge>) -> Result<(), String> {
    let expected_origin = format!("chrome-extension://{EXTENSION_ID}");
    let mut client_id = String::new();
    let mut browser: &'static str = "Google Chrome";

    let ws = tokio_tungstenite::accept_hdr_async(stream, |req: &Request, res: Response| {
        let origin = req
            .headers()
            .get("origin")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        if origin != expected_origin {
            return Err(ErrorResponse::new(Some("Invalid extension origin".into())));
        }
        let ua = req
            .headers()
            .get("user-agent")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        browser = browser_name(ua);
        if let Some(q) = req.uri().query() {
            for pair in q.split('&') {
                if let Some(v) = pair.strip_prefix("clientId=") {
                    client_id = v.to_string();
                }
            }
        }
        if client_id.is_empty() {
            client_id = uuid::Uuid::new_v4().to_string();
        }
        Ok(res)
    })
    .await
    .map_err(|e| e.to_string())?;

    let (mut sink, mut source) = ws.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();

    // Send hello with bundled version + single-source-of-truth page_runtime.js.
    let hello = json!({
        "type": "hello",
        "version": bundled_version(),
        "runtime": PAGE_RUNTIME,
    });
    let _ = tx.send(hello.to_string());

    state.clients.lock().unwrap().insert(
        client_id.clone(),
        WsClient { tx: tx.clone(), browser, connected_at: Instant::now() },
    );

    let writer = tauri::async_runtime::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if sink.send(Message::Text(msg.into())).await.is_err() {
                break;
            }
        }
    });

    while let Some(Ok(msg)) = source.next().await {
        let text = match msg {
            Message::Text(t) => t,
            Message::Close(_) => break,
            _ => continue,
        };
        let Ok(val) = serde_json::from_str::<Value>(text.as_str()) else { continue };
        match val["type"].as_str().unwrap_or_default() {
            "ping" => {
                let _ = tx.send(r#"{"type":"pong"}"#.to_string());
            }
            "reply" => {
                if let Some(id) = val["id"].as_str() {
                    if let Some(sender) = state.pending.lock().unwrap().remove(id) {
                        let res = if let Some(err) = val["error"].as_str() {
                            Err(err.to_string())
                        } else {
                            Ok(val["result"].clone())
                        };
                        let _ = sender.send(res);
                    }
                }
            }
            "miniapp_triggers" => {
                if let Some(id) = val["id"].as_str() {
                    let reply = json!({
                        "type": "host_reply",
                        "id": id,
                        "result": { "triggers": crate::miniapp_window::browser_triggers() }
                    });
                    let _ = tx.send(reply.to_string());
                }
            }
            "miniapp_launch" => {
                if let Some(id) = val["id"].as_str().map(str::to_string) {
                    let tx = tx.clone();
                    let app = state.app.get().cloned();
                    let body = val.clone();
                    tauri::async_runtime::spawn(async move {
                        let outcome = match app {
                            Some(ref app) => crate::miniapp_window::browser_launch(app, &body).await,
                            None => Err("Nova 尚未就绪".into()),
                        };
                        let reply = match outcome {
                            Ok(()) => json!({"type": "host_reply", "id": id, "result": {"ok": true}}),
                            Err(err) => json!({"type": "host_reply", "id": id, "error": err}),
                        };
                        let _ = tx.send(reply.to_string());
                    });
                }
            }
            _ => {}
        }
    }

    writer.abort();
    state.clients.lock().unwrap().remove(&client_id);
    Ok(())
}

/// Tiny upgrade shim for already-installed 0.1.x extensions: answers POST /pair & /poll with
/// bundled_version ("0.2.0") so older extensions self-reload automatically into WebSocket mode.
async fn serve_legacy_http(mut stream: tokio::net::TcpStream) -> Result<(), String> {
    let mut buf = vec![0u8; 4096];
    let n = tokio::time::timeout(Duration::from_secs(3), stream.read(&mut buf))
        .await
        .map_err(|_| "http read timeout")?
        .map_err(|e| e.to_string())?;
    let head = std::str::from_utf8(&buf[..n]).unwrap_or_default();
    let expected = format!("chrome-extension://{EXTENSION_ID}");
    let valid_origin = head
        .lines()
        .any(|l| l.to_ascii_lowercase().starts_with("origin:") && l.contains(&expected));
    let (status, body) = if !valid_origin {
        (StatusCode::FORBIDDEN, json!({"error": "Invalid origin"}))
    } else if head.starts_with("OPTIONS ") {
        (StatusCode::OK, json!({}))
    } else if head.starts_with("POST /pair ") {
        (StatusCode::OK, json!({"token": "upgrade-token-000000000000000000000000"}))
    } else if head.starts_with("POST /poll ") {
        (StatusCode::OK, json!({"command": Value::Null, "version": bundled_version()}))
    } else {
        (StatusCode::NOT_FOUND, json!({}))
    };
    let text = body.to_string();
    let resp = format!(
        "HTTP/1.1 {} Result\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\nAccess-Control-Allow-Origin: {}\r\nAccess-Control-Allow-Headers: Authorization, Content-Type\r\nAccess-Control-Allow-Methods: POST, OPTIONS\r\n\r\n{}",
        status.as_u16(),
        text.len(),
        expected,
        text
    );
    let _ = stream.write_all(resp.as_bytes()).await;
    Ok(())
}

pub(crate) fn find_chrome() -> Option<PathBuf> {
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

pub(crate) fn bundled_version() -> Option<String> {
    serde_json::from_str::<Value>(EXTENSION_FILES[0].1)
        .ok()
        .and_then(|m| m["version"].as_str().map(String::from))
}

pub(crate) fn release_extension(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .state::<crate::AppState>()
        .config_dir
        .join("chrome-extension")
        .join("nova-chrome");
    std::fs::create_dir_all(&dir).map_err(|e| format!("释放 Chrome 扩展失败：{e}"))?;
    for (name, content) in EXTENSION_FILES {
        std::fs::write(dir.join(name), content).map_err(|e| format!("释放 Chrome 扩展失败：{e}"))?;
    }
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    #[tokio::test]
    async fn ws_bridge_authenticates_origin_delivers_runtime_and_upgrades_legacy_clients() {
        let state = Arc::new(ExtBridge::new());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let shared = state.clone();
        let server = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else { break };
                let s = shared.clone();
                tokio::spawn(async move {
                    let _ = handle_connection(stream, s).await;
                });
            }
        });

        // 1. Legacy 0.1.x HTTP /poll shim returns bundled 0.2.0 version for auto-reload.
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        let bad = http
            .post(format!("http://127.0.0.1:{port}/pair"))
            .header("Origin", "https://evil.example")
            .send()
            .await
            .unwrap();
        assert_eq!(bad.status(), 403);
        let poll: Value = http
            .post(format!("http://127.0.0.1:{port}/poll"))
            .header("Origin", format!("chrome-extension://{EXTENSION_ID}"))
            .json(&json!({}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(poll["version"].as_str(), bundled_version().as_deref());

        // 2. WebSocket handshake rejects foreign origin and accepts fixed extension origin.
        let mut bad_req = format!("ws://127.0.0.1:{port}/ws?clientId=bad").into_client_request().unwrap();
        bad_req.headers_mut().insert("Origin", "https://evil.example".parse().unwrap());
        let bad_stream = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        assert!(tokio_tungstenite::client_async(bad_req, bad_stream).await.is_err());

        let mut req = format!("ws://127.0.0.1:{port}/ws?clientId=ext-test-1").into_client_request().unwrap();
        req.headers_mut().insert("Origin", format!("chrome-extension://{EXTENSION_ID}").parse().unwrap());
        req.headers_mut().insert("User-Agent", "Mozilla/5.0 Chrome/130.0.0.0".parse().unwrap());
        let stream = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let (mut ws, _) = tokio_tungstenite::client_async(req, stream).await.unwrap();

        // First message must be `hello` carrying bundled version and single-source-of-truth runtime.
        let hello_txt = ws.next().await.unwrap().unwrap().into_text().unwrap();
        let hello: Value = serde_json::from_str(&hello_txt).unwrap();
        assert_eq!(hello["type"], "hello");
        assert_eq!(hello["version"].as_str(), bundled_version().as_deref());
        assert!(hello["runtime"].as_str().unwrap().contains("__novaBrowser"));

        // 3. Round-trip command over persistent WebSocket.
        let (_, _, tx) = state.primary_client().unwrap();
        let call_state = state.clone();
        let call = tokio::spawn(async move {
            call_state.call_client(&tx, "tabs", json!({}), Duration::from_secs(3)).await
        });
        let cmd_txt = ws.next().await.unwrap().unwrap().into_text().unwrap();
        let cmd: Value = serde_json::from_str(&cmd_txt).unwrap();
        assert_eq!(cmd["type"], "cmd");
        assert_eq!(cmd["op"], "tabs");
        ws.send(Message::Text(json!({"type":"reply","id":cmd["id"],"result":{"tabs":[{"tag":"C1"}]}}).to_string().into())).await.unwrap();
        let reply = call.await.unwrap().unwrap();
        assert_eq!(reply["tabs"][0]["tag"], "C1");

        server.abort();
    }
}
