use super::tests::{assert_error, call, describe, run, uncancelled, value};
use super::*;
use std::sync::Mutex;
use tokio::net::{TcpListener, TcpStream};
use tokio::task::{JoinHandle, JoinSet};

struct HttpFixture {
    url: String,
    records: Arc<Mutex<Vec<Value>>>,
    task: JoinHandle<()>,
    env_key: String,
}
impl Drop for HttpFixture {
    fn drop(&mut self) {
        self.task.abort();
        std::env::remove_var(&self.env_key);
    }
}
impl HttpFixture {
    async fn new(mode: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        let records = Arc::new(Mutex::new(Vec::new()));
        let log = records.clone();
        let task = tokio::spawn(async move {
            let mut children = JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let (stream, _) = accepted.unwrap();
                        let log = log.clone();
                        children.spawn(async move { serve(stream, mode, log).await });
                    },
                    Some(result) = children.join_next(), if !children.is_empty() => { result.unwrap(); },
                }
            }
        });
        let env_key = format!("LYRA_HTTP_TEST_{}", uuid::Uuid::new_v4().simple());
        std::env::set_var(&env_key, "private-http-token");
        Self {
            url,
            records,
            task,
            env_key,
        }
    }
    fn config(&self, timeout: u64) -> McpConfig {
        config_from_root(&json!({"mcpServers":{"test":{"enabled":true,"url":self.url,
            "headers":{"Authorization":format!("Bearer {{env:{}}}", self.env_key)},
            "request_timeout_ms":timeout,"startup_timeout_ms":1500,"readOnlyTools":["lookup"]}}}))
        .unwrap()
    }
    fn session(&self, timeout: u64) -> McpSession {
        McpSession::new(self.config(timeout), PathBuf::from("."), false).unwrap()
    }
    fn count(&self, method: &str) -> usize {
        self.records
            .lock()
            .unwrap()
            .iter()
            .filter(|v| v["method"] == method)
            .count()
    }
    async fn wait_for(&self, method: &str) {
        tokio::time::timeout(Duration::from_secs(3), async {
            while self.count(method) == 0 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
    }
}

async fn response(stream: &mut TcpStream, status: &str, extra: &str, mime: &str, body: &str) {
    let head = format!("HTTP/1.1 {status}\r\nConnection: close\r\nContent-Type: {mime}\r\nContent-Length: {}\r\n{extra}\r\n", body.len());
    let _ = stream.write_all(head.as_bytes()).await;
    let _ = stream.write_all(body.as_bytes()).await;
}

async fn serve(stream: TcpStream, mode: &str, records: Arc<Mutex<Vec<Value>>>) {
    let mut stream = BufReader::new(stream);
    let mut line = String::new();
    stream.read_line(&mut line).await.unwrap();
    assert!(line.starts_with("POST /mcp "), "{line}");
    let mut headers = BTreeMap::new();
    loop {
        line.clear();
        stream.read_line(&mut line).await.unwrap();
        if line == "\r\n" {
            break;
        }
        let (name, val) = line.trim_end().split_once(':').unwrap();
        headers.insert(name.to_ascii_lowercase(), val.trim().to_owned());
    }
    let length: usize = headers["content-length"].parse().unwrap();
    assert!(length < MAX_WIRE_BYTES);
    let mut body = vec![0; length];
    stream.read_exact(&mut body).await.unwrap();
    let m: Value = serde_json::from_slice(&body).unwrap();
    records.lock().unwrap().push(json!({"method":m["method"],"id":m["id"],"session":headers.get("mcp-session-id"),
        "version":headers.get("mcp-protocol-version"),"authorized":headers.get("authorization").is_some_and(|v| v == "Bearer private-http-token"),
        "validReply":m["id"] == "from-server" && m["error"]["code"] == -32601 && m.get("result").is_none()}));
    let stream = stream.get_mut();
    if m["method"] == "initialize" {
        if mode == "redirect" {
            response(
                stream,
                "307 Temporary Redirect",
                "Location: /must-not-follow\r\n",
                "text/plain",
                "",
            )
            .await;
            return;
        }
        response(stream, "200 OK", "Mcp-Session-Id: mock-session\r\n", "application/json", &json!({"jsonrpc":"2.0","id":m["id"],
            "result":{"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"mock","version":"1"}}}).to_string()).await;
        return;
    }
    if m["method"] == "notifications/initialized" || m.get("method").is_none() {
        response(stream, "202 Accepted", "", "application/json", "").await;
        return;
    }
    if m["method"] == "tools/call" {
        if mode == "hang" {
            tokio::time::sleep(Duration::from_secs(10)).await;
            return;
        }
        if mode == "expired" {
            response(stream, "404 Not Found", "", "text/plain", "expired").await;
            return;
        }
        if mode == "rpc_error" {
            response(stream, "200 OK", "", "application/json", &json!({"jsonrpc":"2.0","id":m["id"],"error":{"code":-32000,"message":"private-http-token failure"}}).to_string()).await;
            return;
        }
        if mode == "oversize" {
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n").await.unwrap();
            let bytes = vec![b'x'; MAX_WIRE_BYTES + 1];
            let _ = stream.write_all(&bytes).await;
            return;
        }
    }
    let result = if m["method"] == "tools/list" {
        if m["params"]["cursor"] == "second" {
            json!({"tools":[{"name":"next","inputSchema":{"type":"object"}}]})
        } else {
            json!({"tools":[{"name":"lookup","description":"HTTP lookup","inputSchema":{"type":"object","description":"HTTP_SCHEMA_ONLY"}}],"nextCursor":"second"})
        }
    } else {
        json!({"content":[{"type":"text","text":"private-http-token hello 中文"}],"isError":mode == "tool_error"})
    };
    let reply = json!({"jsonrpc":"2.0","id":m["id"],"result":result});
    if mode != "json"
        && mode != "rpc_error"
        && mode != "expired"
        && mode != "hang"
        && mode != "tool_error"
    {
        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream; charset=utf-8\r\nConnection: close\r\n\r\n").await.unwrap();
        if mode == "server_request" && m["method"] == "tools/list" {
            let request = b"data: {\"jsonrpc\":\"2.0\",\"id\":\"from-server\",\"method\":\"sampling/createMessage\"}\n\n";
            stream.write_all(request).await.unwrap();
        }
        // Multi-line data, BOM, comments, split UTF-8 and split CRLF; leave stream open AFTER result.
        let pretty = serde_json::to_string_pretty(&reply).unwrap();
        let event = format!(
            "\u{feff}: heartbeat\r\nevent: message\r\n{}\r\n",
            pretty
                .lines()
                .map(|s| format!("data: {s}\r\n"))
                .collect::<String>()
        );
        for chunk in event.as_bytes().chunks(7) {
            if stream.write_all(chunk).await.is_err() {
                return;
            }
            tokio::task::yield_now().await;
        }
        tokio::time::sleep(Duration::from_secs(10)).await;
    } else {
        response(stream, "200 OK", "", "application/json", &reply.to_string()).await;
    }
}

#[tokio::test]
async fn streamable_http_json_sse_sessions_paginate_and_govern() {
    for mode in ["json", "sse", "server_request"] {
        let mock = HttpFixture::new(mode).await;
        let mut session = mock.session(1000);
        let first = run(&mut session, json!({"op":"list","server":"test"})).await;
        assert!(!first.is_error, "{}", value(&first));
        assert!(!json!(first.content)
            .to_string()
            .contains("HTTP_SCHEMA_ONLY"));
        let second = run(
            &mut session,
            json!({"op":"list","server":"test","cursor":value(&first)["nextCursor"]}),
        )
        .await;
        assert_eq!(value(&second)["tools"][0]["name"], "next");
        assert!(!describe(&mut session, "lookup").await.is_error);
        let out = call(&mut session, "lookup").await;
        assert!(!out.is_error, "{}", value(&out));
        assert_eq!(value(&out)["content"][0]["text"], "[REDACTED] hello 中文");
        let log = mock.records.lock().unwrap();
        assert_eq!(
            log.iter().filter(|v| v["method"] == "initialize").count(),
            1
        );
        assert!(log.iter().all(|v| v["authorized"] == true));
        assert!(log
            .iter()
            .skip(1)
            .all(|v| v["session"] == "mock-session" && v["version"] == "2025-06-18"));
        if mode == "server_request" {
            assert!(log.iter().any(|v| v["validReply"] == true));
        }
    }
}

#[tokio::test]
async fn http_timeouts_cancellation_error_and_wire_caps_do_not_replay() {
    for (mode, code) in [
        ("hang", "timeout"),
        ("expired", "http_status"),
        ("oversize", "protocol_error"),
        ("rpc_error", "rpc_error"),
    ] {
        let mock = HttpFixture::new(mode).await;
        let mut session = mock.session(400);
        assert!(!describe(&mut session, "lookup").await.is_error);
        let out = call(&mut session, "lookup").await;
        assert_error(&out, code);
        assert!(!json!(out.content)
            .to_string()
            .contains("private-http-token"));
        let rpc = mode == "rpc_error";
        // JSON-RPC errors keep the HTTP session and describe grant; transport errors drop both.
        assert_error(&call(&mut session, "lookup").await, if rpc { "rpc_error" } else { "not_described" });
        assert_eq!(mock.count("tools/call"), 1 + usize::from(rpc));
        assert_eq!(mock.count("initialize"), 1);
    }
    let mock = HttpFixture::new("hang").await;
    let mut session = mock.session(2000);
    assert!(!describe(&mut session, "lookup").await.is_error);
    let cancel = uncancelled();
    let signal = async {
        mock.wait_for("tools/call").await;
        cancel.store(true, Ordering::SeqCst);
    };
    let (out, _) = tokio::join!(
        session.execute(
            json!({"op":"call","server":"test","tool":"lookup"}),
            &cancel,
            |out| out
        ),
        signal
    );
    assert_error(&out, "cancelled");
    assert_error(&call(&mut session, "lookup").await, "not_described");
    assert_eq!(mock.count("tools/call"), 1);
    let mock = HttpFixture::new("redirect").await;
    assert_error(
        &describe(&mut mock.session(1000), "lookup").await,
        "http_status",
    );
    assert_eq!(mock.count("initialize"), 1);
    let mock = HttpFixture::new("tool_error").await;
    let mut session = mock.session(1000);
    assert!(!describe(&mut session, "lookup").await.is_error);
    assert!(call(&mut session, "lookup").await.is_error);
}
