//! Shared WS transport for relay messages and remote sync. Business IDs/cursors stay unchanged.
use crate::http_stream::{decode_sse_json, SSE_IDLE_TIMEOUT_SECS, SSE_MAX_EVENT_BYTES};
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::timeout;
use tokio_tungstenite::{tungstenite::{client::IntoClientRequest, handshake::{derive_accept_key, client::generate_key}, protocol::{Message, Role, WebSocketConfig}, Error as WsError}, WebSocketStream};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const WRITE_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_PENDING: usize = 64;

type Reply = oneshot::Sender<Result<Value, String>>;
struct Request { frame: Value, reply: Reply }
#[derive(Clone)]
struct Connection { server: String, token: String, sender: mpsc::Sender<Request> }

pub struct RelaySocket {
    connection: Mutex<Option<Connection>>,
    legacy: Mutex<Option<(String, String)>>,
    changes: watch::Sender<()>,
}

impl RelaySocket {
    pub fn new() -> Arc<Self> {
        Arc::new(Self { connection: Mutex::new(None), legacy: Mutex::new(None), changes: watch::channel(()).0 })
    }

    pub fn subscribe(&self) -> watch::Receiver<()> { self.changes.subscribe() }

    #[cfg(test)]
    pub fn connected(&self, server: &str, token: &str) -> bool {
        self.connection.lock().unwrap().as_ref()
            .is_some_and(|c| c.server == server && c.token == token && !c.sender.is_closed())
    }

    pub fn legacy(&self, server: &str, token: &str) -> bool {
        self.legacy.lock().unwrap().as_ref().is_some_and(|(s, t)| s == server && t == token)
    }

    pub fn set_legacy(&self, server: &str, token: &str, enabled: bool) {
        let next = enabled.then(|| (server.to_string(), token.to_string()));
        let mut legacy = self.legacy.lock().unwrap();
        if *legacy != next {
            *legacy = next;
            self.changes.send_replace(());
        }
    }

    /// None permits HTTP only after an explicit legacy downgrade. A missing/reconnecting WS
    /// returns Some(Err): never replay an unconfirmed WS call over HTTP.
    pub async fn request(&self, server: &str, token: &str, mut frame: Value) -> Option<Result<Value, String>> {
        let sender = self.connection.lock().unwrap().as_ref()
            .filter(|c| c.server == server && c.token == token).map(|c| c.sender.clone());
        let Some(sender) = sender else {
            return if self.legacy(server, token) { None }
                else { Some(Err("中转站 WebSocket 未就绪，本次未发送".into())) };
        };
        frame["requestId"] = Value::String(uuid::Uuid::new_v4().to_string());
        let (reply, result) = oneshot::channel();
        Some(timeout(REQUEST_TIMEOUT, async {
            sender.send(Request { frame, reply }).await.map_err(|_| "WS 已断开；结果未确认".to_string())?;
            result.await.map_err(|_| "WS 已断开；结果未确认".to_string())?
        }).await.unwrap_or_else(|_| Err("WS 回执超时；结果未确认，不应重复执行".into())))
    }

    pub async fn run<S, F, R>(&self, mut socket: WebSocketStream<S>, server: &str, token: &str, mut on_event: F, on_ready: R) -> Result<(), String>
    where S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
          F: FnMut(Value), R: FnOnce() {
        let first = timeout(WRITE_TIMEOUT, socket.next()).await.map_err(|_| "WS ready 超时")?
            .ok_or("WS 在 ready 前关闭")?.map_err(|e| e.to_string())?;
        let ready = decode_message(first)?;
        if ready["op"] != "ready" || ready["version"] != 2 { return Err("无效的 WS ready".into()); }
        self.set_legacy(server, token, false);
        on_event(ready);
        let (sender, mut requests) = mpsc::channel::<Request>(MAX_PENDING);
        let _guard = ConnectionGuard { owner: self, sender: sender.clone() };
        *self.connection.lock().unwrap() = Some(Connection { server: server.into(), token: token.into(), sender });
        self.changes.send_replace(());
        on_ready();
        let mut pending: HashMap<String, Reply> = HashMap::new();
        let idle = tokio::time::sleep(Duration::from_secs(SSE_IDLE_TIMEOUT_SECS));
        tokio::pin!(idle);
        loop {
            tokio::select! {
                _ = &mut idle => return Err("WS 接收空闲超时".into()),
                message = socket.next() => {
                    let message = message.ok_or("WS 已关闭")?.map_err(|e| e.to_string())?;
                    idle.as_mut().reset(tokio::time::Instant::now() + Duration::from_secs(SSE_IDLE_TIMEOUT_SECS));
                    match message {
                        Message::Ping(_) => {
                            // tungstenite queued the corresponding Pong while reading.
                            timeout(WRITE_TIMEOUT, socket.flush()).await.map_err(|_| "WS Pong 超时")?.map_err(|e| e.to_string())?;
                        }
                        Message::Pong(_) => {},
                        Message::Close(_) => return Err("WS 已关闭".into()),
                        message => {
                            let value = decode_message(message)?;
                            if value["op"] == "ack" {
                                if let Some(reply) = value["requestId"].as_str().and_then(|id| pending.remove(id)) {
                                    let result = match value["error"].as_str().filter(|s| !s.is_empty()) {
                                        Some(error) => Err(error.to_string()),
                                        None => Ok(value),
                                    };
                                    let _ = reply.send(result);
                                }
                            } else { on_event(value); }
                        }
                    }
                }
                Some(request) = requests.recv() => {
                    pending.retain(|_, reply| !reply.is_closed());
                    if request.reply.is_closed() { continue; }
                    if pending.len() >= MAX_PENDING {
                        let _ = request.reply.send(Err("WS 在途请求过多，本次未发送".into()));
                        continue;
                    }
                    let message = match encode_message(&request.frame) {
                        Ok(message) => message,
                        Err(error) => { let _ = request.reply.send(Err(error)); continue; }
                    };
                    let id = request.frame["requestId"].as_str().unwrap().to_string();
                    pending.insert(id, request.reply);
                    timeout(WRITE_TIMEOUT, socket.send(message)).await.map_err(|_| "WS 发送超时；结果未确认")?.map_err(|e| e.to_string())?;
                }
            }
        }
    }
}

struct ConnectionGuard<'a> { owner: &'a RelaySocket, sender: mpsc::Sender<Request> }
impl Drop for ConnectionGuard<'_> {
    fn drop(&mut self) {
        let mut connection = self.owner.connection.lock().unwrap();
        if connection.as_ref().is_some_and(|c| c.sender.same_channel(&self.sender)) {
            *connection = None;
            self.owner.changes.send_replace(());
        }
    }
}

fn encode_message(frame: &Value) -> Result<Message, String> {
    let raw = serde_json::to_string(frame).map_err(|e| e.to_string())?;
    if raw.len() > SSE_MAX_EVENT_BYTES { return Err("WS 消息超过 32 MiB，本次未发送".into()); }
    if raw.len() >= 1024 {
        Ok(Message::Binary(crate::relay::gzip_json(frame)?.into()))
    } else { Ok(Message::Text(raw.into())) }
}

fn decode_message(message: Message) -> Result<Value, String> {
    match message {
        Message::Text(text) if text.len() <= SSE_MAX_EVENT_BYTES => decode_sse_json(&text),
        _ => Err("无效的 WS 事件".into()),
    }
}

/// Keep reqwest's system proxy and TLS validation; tunnel plain WS through HTTP proxies
/// because forwarding Upgrade as an ordinary HTTP request can strip the upgrade headers.
pub async fn upgrade(request: reqwest::RequestBuilder) -> Result<Option<WebSocketStream<reqwest::Upgraded>>, String> {
    let key = generate_key();
    let (client, request) = request.version(reqwest::Version::HTTP_11)
        .header("Connection", "Upgrade").header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13").header("Sec-WebSocket-Key", &key)
        .header("Accept-Encoding", "identity").build_split();
    let request = request.map_err(|e| e.to_string())?;
    let uri = request.url().as_str().parse().map_err(|e| format!("无效的 WS 地址：{e}"))?;
    let mut config = WebSocketConfig::default();
    config.max_message_size = Some(SSE_MAX_EVENT_BYTES);
    config.max_frame_size = Some(SSE_MAX_EVENT_BYTES);
    if request.url().scheme() == "http"
        && hyper_util::client::proxy::matcher::Matcher::from_system().intercept(&uri)
            .is_some_and(|proxy| matches!(proxy.uri().scheme_str(), Some("http" | "https"))) {
        return upgrade_via_proxy(client, request, config).await;
    }
    let response = timeout(WRITE_TIMEOUT, client.execute(request)).await
        .map_err(|_| "建立 WS 超时")?.map_err(|e| e.to_string())?;
    if matches!(response.status().as_u16(), 404 | 405 | 426 | 501) { return Ok(None); }
    if response.status() != reqwest::StatusCode::SWITCHING_PROTOCOLS { return Err(format!("WS 握手返回 HTTP {}", response.status())); }
    let header = |name| response.headers().get(name).and_then(|h| h.to_str().ok()).unwrap_or("");
    if !header("Upgrade").eq_ignore_ascii_case("websocket")
        || !header("Connection").split(',').any(|v| v.trim().eq_ignore_ascii_case("upgrade"))
        || header("Sec-WebSocket-Accept") != derive_accept_key(key.as_bytes()) {
        return Err("WS 握手校验失败".into());
    }
    let stream = response.upgrade().await.map_err(|e| e.to_string())?;
    Ok(Some(WebSocketStream::from_raw_socket(stream, Role::Client, Some(config)).await))
}

async fn upgrade_via_proxy(client: reqwest::Client, request: reqwest::Request, config: WebSocketConfig) -> Result<Option<WebSocketStream<reqwest::Upgraded>>, String> {
    // The relay Authorization header belongs inside the tunnel, never on CONNECT.
    let response = timeout(WRITE_TIMEOUT, client.request(reqwest::Method::CONNECT, request.url().origin().ascii_serialization())
        .version(reqwest::Version::HTTP_11).send()).await
        .map_err(|_| "WS 代理 CONNECT 超时")?.map_err(|e| e.to_string())?;
    if !response.status().is_success() { return Err(format!("WS 代理 CONNECT 返回 HTTP {}", response.status())); }
    let stream = response.upgrade().await.map_err(|e| e.to_string())?;
    let mut url = request.url().clone();
    url.set_scheme("ws").map_err(|_| "无效的 WS 地址")?;
    let mut handshake = url.as_str().into_client_request().map_err(|e| e.to_string())?;
    handshake.headers_mut().extend(request.headers().clone());
    match timeout(WRITE_TIMEOUT, tokio_tungstenite::client_async_with_config(handshake, stream, Some(config))).await
        .map_err(|_| "WS 代理隧道握手超时")? {
        Ok((socket, _)) => Ok(Some(socket)),
        Err(WsError::Http(response)) if matches!(response.status().as_u16(), 404 | 405 | 426 | 501) => Ok(None),
        Err(error) => Err(format!("WS 代理隧道握手失败：{error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn incoming(message: Message) -> Value {
        match message {
            Message::Text(text) => serde_json::from_str(&text).unwrap(),
            Message::Binary(bytes) => serde_json::from_reader(flate2::read::GzDecoder::new(&bytes[..])).unwrap(),
            _ => panic!("unexpected message"),
        }
    }

    #[tokio::test]
    async fn http_fallback_requires_explicit_matching_legacy_server() {
        let socket = RelaySocket::new();
        assert!(socket.request("server", "token", json!({})).await.unwrap().is_err());
        socket.set_legacy("server", "token", true);
        assert!(socket.request("server", "token", json!({})).await.is_none());
        assert!(socket.request("other-server", "token", json!({})).await.unwrap().is_err());
        assert!(socket.request("server", "other-token", json!({})).await.unwrap().is_err());
        socket.set_legacy("server", "token", false);
        assert!(socket.request("server", "token", json!({})).await.unwrap().is_err());
    }

    #[tokio::test]
    async fn http_proxy_tunnels_upgrade_without_sending_relay_auth_on_connect() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut headers = Vec::new();
            while !headers.ends_with(b"\r\n\r\n") {
                headers.push(stream.read_u8().await.unwrap());
                assert!(headers.len() < 4096);
            }
            let headers = String::from_utf8(headers).unwrap();
            assert!(headers.starts_with("CONNECT relay.invalid:8320 HTTP/1.1\r\n"));
            assert!(!headers.to_ascii_lowercase().contains("authorization:"));
            stream.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n").await.unwrap();
            let mut ws = tokio_tungstenite::accept_hdr_async(stream, |request: &tokio_tungstenite::tungstenite::handshake::server::Request, response| {
                assert_eq!(request.uri(), "/v2/ws?since=7");
                assert_eq!(request.headers()["Authorization"], "Bearer test");
                assert_eq!(request.headers()["X-Relay-Device"], "test-device");
                Ok(response)
            }).await.unwrap();
            ws.send(Message::Text(json!({"version":2,"op":"ready"}).to_string().into())).await.unwrap();
        });
        let client = reqwest::Client::builder().proxy(reqwest::Proxy::http(proxy).unwrap()).build().unwrap();
        let request = client.get("http://relay.invalid:8320/v2/ws?since=7")
            .bearer_auth("test").header("X-Relay-Device", "test-device").build().unwrap();
        let mut ws = timeout(Duration::from_secs(3), upgrade_via_proxy(client, request, WebSocketConfig::default())).await.unwrap().unwrap().unwrap();
        assert_eq!(decode_message(ws.next().await.unwrap().unwrap()).unwrap()["op"], "ready");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn upgrade_multiplexes_acks_events_compression_and_ping() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (stop_tx, stop_rx) = oneshot::channel();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_hdr_async(stream, |request: &tokio_tungstenite::tungstenite::handshake::server::Request, response| {
                assert_eq!(request.uri().path(), "/v2/ws");
                assert_eq!(request.headers()["Authorization"], "Bearer test");
                assert_eq!(request.uri().query(), Some("since=7"));
                Ok(response)
            }).await.unwrap();
            ws.send(Message::Text(json!({"version":2,"op":"ready", "lastSeq":7}).to_string().into())).await.unwrap();
            ws.send(Message::Ping(vec![1,2,3].into())).await.unwrap();
            let mut requests = Vec::new();
            let mut pong = false;
            while requests.len() < 2 || !pong {
                match ws.next().await.unwrap().unwrap() {
                    Message::Pong(bytes) => { assert_eq!(&bytes[..], &[1,2,3]); pong = true; }
                    msg => requests.push(incoming(msg)),
                }
            }
            ws.send(Message::Text(json!({"version":2,"op":"remote.device", "response":{"commands":[]}}).to_string().into())).await.unwrap();
            for request in requests.into_iter().rev() {
                ws.send(Message::Text(json!({"version":2,"op":"ack", "requestId":request["requestId"], "response":request["data"]}).to_string().into())).await.unwrap();
            }
            let _ = stop_rx.await;
        });
        let socket = RelaySocket::new();
        let stream = upgrade(reqwest::Client::new().get(format!("{base}/v2/ws?since=7")).bearer_auth("test")).await.unwrap().unwrap();
        let (ready_tx, ready_rx) = oneshot::channel();
        let (events_tx, mut events_rx) = mpsc::unbounded_channel();
        let runner = { let socket = socket.clone(); let base = base.clone(); tokio::spawn(async move {
            socket.run(stream, &base, "test", |event| { events_tx.send(event).unwrap(); }, || { let _ = ready_tx.send(()); }).await
        }) };
        ready_rx.await.unwrap();
        assert!(socket.request(&base, "other-token", json!({})).await.unwrap().is_err());
        let first = json!({"version":2,"op":"send", "data":"one"});
        let second = json!({"version":2,"op":"remote.sync", "data":"中文".repeat(1024)});
        let (a, b) = tokio::join!(socket.request(&base, "test", first.clone()), socket.request(&base, "test", second.clone()));
        assert_eq!(a.unwrap().unwrap()["response"], first["data"]);
        assert_eq!(b.unwrap().unwrap()["response"], second["data"]);
        assert_eq!(events_rx.recv().await.unwrap()["op"], "ready");
        assert_eq!(events_rx.recv().await.unwrap()["op"], "remote.device");
        stop_tx.send(()).unwrap();
        assert!(timeout(Duration::from_secs(3), runner).await.unwrap().unwrap().is_err());
        assert!(!socket.connected(&base, "test"));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn disconnect_reports_unknown_outcome_without_http_replay() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            ws.send(Message::Text(json!({"version":2,"op":"ready"}).to_string().into())).await.unwrap();
            let request = incoming(ws.next().await.unwrap().unwrap());
            assert_eq!(request["messageId"], "business-id");
            // Peer closes after executing, before its ACK reaches us.
        });
        let socket = RelaySocket::new();
        let stream = upgrade(reqwest::Client::new().get(format!("{base}/v2/ws"))).await.unwrap().unwrap();
        let (ready_tx, ready_rx) = oneshot::channel();
        let runner = { let socket = socket.clone(); let base = base.clone(); tokio::spawn(async move {
            socket.run(stream, &base, "test", |_| {}, || { let _ = ready_tx.send(()); }).await
        }) };
        ready_rx.await.unwrap();
        let result = socket.request(&base, "test", json!({"version":2,"op":"send","messageId":"business-id"})).await;
        assert!(result.unwrap().unwrap_err().contains("结果未确认"));
        assert!(runner.await.unwrap().is_err());
        assert!(!socket.connected(&base, "test"));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn legacy_fallback_does_not_hide_auth_or_invalid_handshake() {
        for (status, headers, fallback) in [
            ("404 Not Found", "", true), ("405 Method Not Allowed", "", true),
            ("401 Unauthorized", "", false), ("200 OK", "", false),
            ("101 Switching Protocols", "Upgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: wrong\r\n", false),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut bytes = vec![0; 4096];
                let _ = stream.read(&mut bytes).await.unwrap();
                stream.write_all(format!("HTTP/1.1 {status}\r\n{headers}Content-Length: 0\r\n\r\n").as_bytes()).await.unwrap();
            });
            let result = upgrade(reqwest::Client::new().get(format!("http://{addr}/v2/ws"))).await;
            if fallback { assert!(result.unwrap().is_none()); } else { assert!(result.is_err()); }
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn abort_clears_shared_connection_and_fails_pending_requests() {
        let (client, server) = tokio::io::duplex(4096);
        let socket = RelaySocket::new();
        let (ready_tx, ready_rx) = oneshot::channel();
        let runner = { let socket = socket.clone(); tokio::spawn(async move {
            let ws = WebSocketStream::from_raw_socket(client, Role::Client, None).await;
            socket.run(ws, "server", "token", |_| {}, || { let _ = ready_tx.send(()); }).await
        }) };
        let mut ws = WebSocketStream::from_raw_socket(server, Role::Server, None).await;
        ws.send(Message::Text(json!({"version":2,"op":"ready"}).to_string().into())).await.unwrap();
        ready_rx.await.unwrap();
        let pending = { let socket = socket.clone(); tokio::spawn(async move { socket.request("server", "token", json!({"version":2,"op":"send"})).await }) };
        let _ = ws.next().await.unwrap().unwrap();
        runner.abort();
        let _ = runner.await;
        assert!(!socket.connected("server", "token"));
        assert!(pending.await.unwrap().unwrap().is_err());
    }
}
