//! Nova 进程内 Tool API：数字员工工具走 localhost HTTP，不再依赖 CLI 子进程写收件箱。
//!
//! 仅监听 127.0.0.1；启动时写入 `~/.nova/tool-api.json`（url + token）。

use crate::employees::{self, InboxCommand};
use crate::marks::MarkStore;
use crate::AppState;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;
use tauri::{AppHandle, Manager};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use uuid::Uuid;

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolApiInfo {
    pub url: String,
    pub token: String,
    pub port: u16,
}

/// 启动本地 Tool API，返回连接信息（并落盘）。
pub fn start(app: AppHandle, data_dir: PathBuf) -> ToolApiInfo {
    let token = Uuid::new_v4().to_string();
    let listener = match tauri::async_runtime::block_on(TcpListener::bind("127.0.0.1:0")) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("[nova] Tool API 绑定失败：{e}");
            return ToolApiInfo {
                url: String::new(),
                token,
                port: 0,
            };
        }
    };
    let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
    let info = ToolApiInfo {
        url: format!("http://127.0.0.1:{port}"),
        token: token.clone(),
        port,
    };
    write_info(&data_dir, &info);
    if let Some(st) = app.try_state::<AppState>() {
        *st.tool_api.lock().unwrap() = Some(info.clone());
    }

    let app2 = app.clone();
    let token2 = token;
    let data_dir2 = data_dir;
    tauri::async_runtime::spawn(async move {
        let state = Arc::new(ApiState {
            app: app2,
            token: token2,
            data_dir: data_dir2,
        });
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                continue;
            };
            let state = state.clone();
            tauri::async_runtime::spawn(async move {
                let mut buf = vec![0u8; 64 * 1024];
                let n = match socket.read(&mut buf).await {
                    Ok(0) | Err(_) => return,
                    Ok(n) => n,
                };
                let raw = String::from_utf8_lossy(&buf[..n]);
                let resp = handle_http(&state, &raw).await;
                let _ = socket.write_all(resp.as_bytes()).await;
            });
        }
    });

    eprintln!("[nova] Tool API 已启动：{}", info.url);
    info
}

fn write_info(data_dir: &PathBuf, info: &ToolApiInfo) {
    let _ = std::fs::create_dir_all(data_dir);
    if let Ok(json) = serde_json::to_string_pretty(info) {
        let _ = std::fs::write(data_dir.join("tool-api.json"), json);
    }
}

struct ApiState {
    app: AppHandle,
    token: String,
    data_dir: PathBuf,
}

async fn handle_http(state: &ApiState, raw: &str) -> String {
    let Some((method, path, headers, body)) = parse_request(raw) else {
        return http_response(400, r#"{"ok":false,"error":"bad request"}"#);
    };
    let auth = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("authorization"))
        .map(|(_, v)| v.as_str())
        .unwrap_or("");
    let token_ok = auth == format!("Bearer {}", state.token)
        || headers
            .iter()
            .any(|(k, v)| k.eq_ignore_ascii_case("x-nova-token") && v == &state.token);
    if !token_ok {
        return http_response(401, r#"{"ok":false,"error":"unauthorized"}"#);
    }

    match (method.as_str(), path.as_str()) {
        ("GET", "/v1/health") => http_response(200, r#"{"ok":true}"#),
        ("POST", "/v1/command") => {
            let cmd: InboxCommand = match serde_json::from_str(body) {
                Ok(c) => c,
                Err(e) => {
                    return http_response(
                        400,
                        &json!({"ok":false,"error":format!("invalid command: {e}")}).to_string(),
                    );
                }
            };
            employees::dispatch_inbox_command(&state.app, cmd).await;
            http_response(200, r#"{"ok":true}"#)
        }
        ("POST", "/v1/kb-search") => {
            let v: Value = serde_json::from_str(body).unwrap_or(json!({}));
            let employee = v.get("employee").and_then(|x| x.as_str()).unwrap_or("");
            let query = v.get("query").and_then(|x| x.as_str()).unwrap_or("");
            let k = v.get("k").and_then(|x| x.as_u64()).unwrap_or(6) as usize;
            let text = employees::tool_kb_search(&state.app, employee, query, k);
            http_response(200, &json!({"ok":true,"text":text}).to_string())
        }
        ("POST", "/v1/ledger-list") | ("GET", "/v1/ledger-list") => {
            let scope = if method == "GET" {
                path_query(raw, "scope").unwrap_or_default()
            } else {
                let v: Value = serde_json::from_str(body).unwrap_or(json!({}));
                v.get("scope")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string()
            };
            let text = {
                let store = MarkStore::load(&state.data_dir);
                let marks = store.list(if scope.is_empty() {
                    None
                } else {
                    Some(scope.as_str())
                });
                format!(
                    "门锁账本（scope={scope}）：\n{}",
                    crate::marks::render_digest(&marks)
                )
            };
            http_response(200, &json!({"ok":true,"text":text}).to_string())
        }
        ("POST", "/v1/notices/respond") => {
            let v: Value = serde_json::from_str(body).unwrap_or(json!({}));
            let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("");
            let employee = v.get("employee").and_then(|x| x.as_str()).unwrap_or("");
            let text = v
                .get("text")
                .and_then(|x| x.as_str())
                .map(|s| s.to_string());
            let reject = v.get("reject").and_then(|x| x.as_bool()).unwrap_or(false);
            let emp = employees::find_employee(&state.app, employee);
            let by = match emp {
                Some(e) => crate::notice::ActorRef::employee(&e.id, &e.name),
                None => crate::notice::ActorRef::user(),
            };
            match crate::notice::respond_notice(
                &state.app,
                id,
                by,
                crate::notice::RespondParams {
                    choice_id: v
                        .get("choiceId")
                        .and_then(|x| x.as_str())
                        .map(|s| s.to_string()),
                    text,
                    reject,
                },
            ) {
                Ok(_) => http_response(200, r#"{"ok":true}"#),
                Err(e) => http_response(400, &json!({"ok":false,"error":e}).to_string()),
            }
        }
        _ => http_response(404, r#"{"ok":false,"error":"not found"}"#),
    }
}

fn path_query(raw: &str, key: &str) -> Option<String> {
    let line = raw.lines().next()?;
    let path = line.split_whitespace().nth(1)?;
    let q = path.split('?').nth(1)?;
    for pair in q.split('&') {
        let mut it = pair.splitn(2, '=');
        let k = it.next()?;
        let v = it.next().unwrap_or("");
        if k == key {
            return Some(urlencoding_decode(v));
        }
    }
    None
}

fn urlencoding_decode(s: &str) -> String {
    // 极简：只处理 %XX 与 +
    let mut out = String::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => {
                out.push(' ');
                i += 1;
            }
            b'%' if i + 2 < b.len() => {
                let hex = &s[i + 1..i + 3];
                if let Ok(v) = u8::from_str_radix(hex, 16) {
                    out.push(v as char);
                    i += 3;
                } else {
                    out.push('%');
                    i += 1;
                }
            }
            c => {
                out.push(c as char);
                i += 1;
            }
        }
    }
    out
}

fn parse_request(raw: &str) -> Option<(String, String, Vec<(String, String)>, &str)> {
    let (head, body) = raw
        .split_once("\r\n\r\n")
        .or_else(|| raw.split_once("\n\n"))?;
    let mut lines = head.lines();
    let req = lines.next()?;
    let mut parts = req.split_whitespace();
    let method = parts.next()?.to_string();
    let path_full = parts.next()?.to_string();
    let path = path_full
        .split('?')
        .next()
        .unwrap_or(&path_full)
        .to_string();
    let mut headers = Vec::new();
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    Some((method, path, headers, body))
}

fn http_response(status: u16, body: &str) -> String {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        _ => "Error",
    };
    format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}
