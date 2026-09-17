//! Chrome extension transport. The browser engine remains in native_browser; no second action implementation.
use serde_json::{json, Value};
use std::{
    collections::HashMap,
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
    sync::{mpsc, oneshot},
};

const EXTENSION_ID: &str = "mdicifjlbgnkkdkcbfdnebeghhlamjbm";
type Pending = HashMap<String, oneshot::Sender<Result<Value, String>>>;
pub(crate) struct ChromeBridge {
    start: tokio::sync::Mutex<()>,
    config: Mutex<Option<(u16, String, PathBuf)>>,
    client: Mutex<Option<(String, Instant)>>,
    sender: mpsc::Sender<Value>,
    receiver: tokio::sync::Mutex<mpsc::Receiver<Value>>,
    pending: Mutex<Pending>,
    cancellations: Mutex<HashMap<String, Arc<AtomicBool>>>,
}
impl ChromeBridge {
    fn new() -> Self {
        let (sender, receiver) = mpsc::channel(128);
        Self {
            start: tokio::sync::Mutex::new(()),
            config: Mutex::new(None),
            client: Mutex::new(None),
            sender,
            receiver: tokio::sync::Mutex::new(receiver),
            pending: Mutex::new(HashMap::new()),
            cancellations: Mutex::new(HashMap::new()),
        }
    }
    fn connected(&self) -> bool {
        self.client
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|(_, seen)| seen.elapsed() < Duration::from_secs(45))
    }
    async fn request(self: &Arc<Self>, operation: &str, args: Value) -> Result<Value, String> {
        if !self.connected() {
            return Err(
                "Chrome 未连接；安装独立 Nova Chrome 扩展后自动连接，在扩展中授权标签页".into(),
            );
        }
        let id = uuid::Uuid::new_v4().to_string();
        let (send, receive) = oneshot::channel();
        self.pending.lock().unwrap().insert(id.clone(), send);
        let _pending = PendingCall(self.clone(), id.clone());
        let expires = chrono::Utc::now().timestamp_millis() + 3000;
        self.sender
            .try_send(json!({"id":id,"operation":operation,"args":args,"expiresAt":expires}))
            .map_err(|_| "Chrome 命令队列已满")?;
        tokio::time::timeout(Duration::from_secs(3), receive)
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
    app.manage(Arc::new(ChromeBridge::new()));
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
        json!({"connected":state.connected(),"origin":format!("http://127.0.0.1:{port}"),"extensionId":EXTENSION_ID,"next":"独立 Nova Chrome 扩展会自动连接本机 Nova，在扩展中勾选允许控制的标签。chrome 和右侧 webview 可同时使用。"}),
    )
}
pub(crate) async fn request(
    app: &AppHandle,
    operation: &str,
    args: Value,
) -> Result<Value, String> {
    bridge(app).request(operation, args).await
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
    if !matches!(request, "POST /poll HTTP/1.1" | "POST /reply HTTP/1.1") {
        return respond(&mut stream, 404, json!({})).await;
    }
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
        let mut current = state.client.lock().unwrap();
        if current
            .as_ref()
            .is_some_and(|(id, seen)| id != client && seen.elapsed() < Duration::from_secs(45))
        {
            false
        } else if poll {
            *current = Some((client.into(), Instant::now()));
            true
        } else {
            current.as_ref().is_some_and(|(id, _)| id == client)
        }
    };
    if !accepted {
        return respond(
            &mut stream,
            409,
            json!({"error":"Another Chrome client is connected"}),
        )
        .await;
    }
    if poll {
        let command = tokio::time::timeout(Duration::from_secs(20), async {
            let mut receiver = state.receiver.lock().await;
            while let Some(command) = receiver.recv().await {
                if command["id"]
                    .as_str()
                    .is_some_and(|id| state.pending.lock().unwrap().contains_key(id))
                    && command["expiresAt"].as_i64().unwrap_or(0)
                        > chrono::Utc::now().timestamp_millis()
                {
                    return Some(command);
                }
            }
            None
        })
        .await
        .ok()
        .flatten();
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
    crate::native_browser::execute_chrome(&root, &args).await
}

#[cfg(test)]
mod tests {
    use super::*;
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
        *state.client.lock().unwrap() = Some((client_id.clone(), Instant::now()));
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
        let other = post(
            "/poll",
            json!({"clientId":uuid::Uuid::new_v4().to_string()}),
        )
        .send()
        .await
        .unwrap();
        assert_eq!(other.status(), 409);
        server.abort();
    }
}
