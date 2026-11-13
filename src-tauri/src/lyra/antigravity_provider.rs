//! Google Antigravity（反重力）反代：用 Google 账号的 OAuth 凭据，把 Nova 的 OpenAI-completions 请求
//! 翻译成 cloudcode-pa 的 `v1internal` 信封，再把上游 SSE 还原成 `StreamEvent`。
//!
//! 协议事实（端点、信封字段、UA 指纹、`-disabled` 占位模型）来自 CLIProxyAPI 的 Antigravity executor；
//! 上游按客户端指纹下发模型目录，UA 里的版本/CL 号需要跟着官方客户端走，否则拿不到较新的模型。
//! 凭据只存 refresh token：访问令牌每次请求现换，避免把 1 小时有效的令牌落到配置里。

use super::config::ResolvedModel;
use super::provider::{self, StreamEvent, StreamResult};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};

const BASE_URL: &str = "https://cloudcode-pa.googleapis.com";
const DAILY_URL: &str = "https://daily-cloudcode-pa.googleapis.com";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const USERINFO_URL: &str = "https://www.googleapis.com/oauth2/v2/userinfo?alt=json";
/// Antigravity 桌面客户端自带的 OAuth 客户端；反代只能沿用，无法自行注册。
const CLIENT_ID: &str = "1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com";
const CLIENT_SECRET: &str = "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";
/// 官方客户端注册的 loopback 回调；端口被占用时仍可粘贴回调地址。
const REDIRECT_URI: &str = "http://localhost:51121/oauth-callback";
// 混入 aistudio 会让 Google 以 invalid_scope 拒绝整次 Antigravity 授权。
const SCOPES: &str = "https://www.googleapis.com/auth/cloud-platform \
https://www.googleapis.com/auth/userinfo.email \
https://www.googleapis.com/auth/userinfo.profile \
https://www.googleapis.com/auth/cclog \
https://www.googleapis.com/auth/experimentsandconfigs";
/// ponytail: 版本与 CL 号抄自 CLIProxyAPI 的 fallback（2.29.0 / cl=1013060670）；上游改指纹时同步升级。
const USER_AGENT: &str = "antigravity/hub/2.29.0 (aidev_client; os_type=darwin; arch=arm64; cl=1013060670)";
const LOGIN_FILE: &str = "antigravity-credentials.json";
/// 流式读取连续无数据的上限；Carbon 会在自验证上长时间思考，比普通对话宽松。
const IDLE_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase", default)]
pub(super) struct Credentials {
    refresh_token: String,
    /// loadCodeAssist 返回的 cloudaicompanionProject，信封里必须带上。
    project: String,
    email: String,
    updated_at: i64,
}

fn login_path(root: &Path) -> PathBuf {
    root.join(LOGIN_FILE)
}

fn read_credentials(path: &Path) -> Result<Credentials, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|_| "未登录 Antigravity：请在 Lyra 设置里粘贴授权码或 refresh token".to_string())?;
    let credentials: Credentials = serde_json::from_str(&text).map_err(|_| "Antigravity 登录文件格式无效")?;
    if credentials.refresh_token.trim().is_empty() {
        return Err("Antigravity 登录缺少 refresh token，请重新登录".into());
    }
    Ok(credentials)
}

fn save_credentials(root: &Path, credentials: &Credentials) -> Result<(), String> {
    std::fs::create_dir_all(root).map_err(|error| format!("创建凭据目录失败：{error}"))?;
    let text = serde_json::to_string(credentials).map_err(|error| error.to_string())?;
    super::codebuddy_provider::write_private(&login_path(root), text.as_bytes())
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn form(fields: &[(&str, &str)]) -> String {
    // 值里只有 URL/令牌，仍需按表单规则转义，避免 `+`、`/` 被当成空格。
    fields
        .iter()
        .map(|(key, value)| format!("{key}={}", urlencode(value)))
        .collect::<Vec<_>>()
        .join("&")
}

fn urlencode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (byte as char).to_string(),
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

async fn post_form(http: &reqwest::Client, fields: &[(&str, &str)]) -> Result<Value, String> {
    let response = provider::send_request(
        http.post(TOKEN_URL)
            .header("content-type", "application/x-www-form-urlencoded")
            .body(form(fields)),
        &Arc::new(AtomicBool::new(false)),
    )
    .await?;
    let status = response.status();
    let body: Value = response.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        let error = body["error_description"].as_str().or_else(|| body["error"].as_str()).unwrap_or("未知错误");
        return Err(format!("Google 授权失败（{status}）：{error}"));
    }
    Ok(body)
}

/// 用 refresh token 换访问令牌；过期时间由 Google 决定，不落盘。
async fn access_token(http: &reqwest::Client, credentials: &Credentials) -> Result<String, String> {
    let body = post_form(
        http,
        &[
            ("client_id", CLIENT_ID),
            ("client_secret", CLIENT_SECRET),
            ("refresh_token", credentials.refresh_token.as_str()),
            ("grant_type", "refresh_token"),
        ],
    )
    .await
    .map_err(|error| format!("{error}；登录可能已失效，请重新授权"))?;
    body["access_token"]
        .as_str()
        .map(str::to_string)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| "Google 未返回访问令牌，请重新授权".into())
}

async fn user_email(http: &reqwest::Client, token: &str) -> Option<String> {
    let response = http.get(USERINFO_URL).bearer_auth(token).send().await.ok()?;
    let body: Value = response.json().await.ok()?;
    body["email"].as_str().map(str::to_string)
}

/// loadCodeAssist 决定信封里的 project 字段；没有它上游会 400。
async fn discover_project(http: &reqwest::Client, token: &str) -> Result<String, String> {
    let response = provider::send_request(
        http.post(format!("{DAILY_URL}/v1internal:loadCodeAssist"))
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .header("user-agent", USER_AGENT)
            .json(&json!({ "metadata": { "ideType": "ANTIGRAVITY" } })),
        &Arc::new(AtomicBool::new(false)),
    )
    .await?;
    let status = response.status();
    let body: Value = response.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        return Err(format!("读取 Google Cloud 项目失败（{status}）"));
    }
    for key in ["cloudaicompanionProject", "projectId", "project"] {
        let value = &body[key];
        if let Some(text) = value.as_str().filter(|text| !text.trim().is_empty()) {
            return Ok(text.trim().to_string());
        }
        if let Some(id) = value["id"].as_str().filter(|id| !id.trim().is_empty()) {
            return Ok(id.trim().to_string());
        }
    }
    Err("Google 账号没有返回 Cloud 项目，无法使用 Antigravity".into())
}

struct PendingLogin {
    state: String,
    callback: Arc<Mutex<Option<String>>>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    created: Instant,
}

static PENDING: tokio::sync::Mutex<Option<PendingLogin>> = tokio::sync::Mutex::const_new(None);
const LOGIN_TIMEOUT: Duration = Duration::from_secs(300);

/// 登录第一步：先监听本机回调，再打开授权地址；监听失败时保留手动粘贴。
pub(super) async fn login_start(_site: &str) -> Result<Value, String> {
    let mut pending = PENDING.lock().await;
    if let Some(login) = pending.take() {
        for task in login.tasks {
            task.abort();
            let _ = task.await;
        }
    }
    let state = uuid::Uuid::new_v4().simple().to_string();
    let callback = Arc::new(Mutex::new(None));
    let mut tasks = Vec::new();
    // localhost 可能优先解析为 IPv6；两个地址都只监听 loopback，不向局域网开放。
    for address in ["127.0.0.1:51121", "[::1]:51121"] {
        if let Ok(listener) = tokio::net::TcpListener::bind(address).await {
            let state = state.clone();
            let callback = callback.clone();
            tasks.push(tokio::spawn(async move {
                let _ = tokio::time::timeout(LOGIN_TIMEOUT, serve_callback(listener, state, callback)).await;
            }));
        }
    }
    let callback_listening = !tasks.is_empty();
    *pending = Some(PendingLogin { state: state.clone(), callback, tasks, created: Instant::now() });
    let url = format!(
        "{AUTH_URL}?client_id={}&response_type=code&redirect_uri={}&scope={}&access_type=offline&prompt=consent&state={state}",
        urlencode(CLIENT_ID),
        urlencode(REDIRECT_URI),
        urlencode(SCOPES),
    );
    Ok(json!({ "state": state, "url": url, "callbackListening": callback_listening }))
}

async fn serve_callback(listener: tokio::net::TcpListener, state: String, slot: Arc<Mutex<Option<String>>>) {
    while let Ok((mut socket, _)) = listener.accept().await {
        let mut line = Vec::new();
        let read = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::io::BufReader::new((&mut socket).take(8192)).read_until(b'\n', &mut line).await
        }).await;
        if !matches!(read, Ok(Ok(_))) { continue; }
        let request = String::from_utf8_lossy(&line);
        let mut parts = request.split_whitespace();
        let callback = if parts.next() == Some("GET") && line.ends_with(b"\r\n") {
            parts.next().and_then(|path| reqwest::Url::parse(&format!("http://localhost:51121{path}")).ok())
                .filter(|url| url.path() == "/oauth-callback"
                    && url.query_pairs().any(|(key, value)| key == "state" && value == state)
                    && url.query_pairs().any(|(key, value)| (key == "code" || key == "error") && !value.is_empty()))
        } else { None };
        let accepted = callback.is_some();
        let body = if accepted { "已收到 Google 授权回调，请返回 Nova 查看登录结果。" }
            else { "登录回调无效，请返回 Nova 重新登录。" };
        let response = format!("HTTP/1.1 {}\r\nContent-Type: text/plain; charset=utf-8\r\nCache-Control: no-store\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            if accepted { "200 OK" } else { "400 Bad Request" }, body.len());
        let _ = tokio::time::timeout(Duration::from_secs(5), socket.write_all(response.as_bytes())).await;
        if let Some(url) = callback {
            *slot.lock().unwrap() = Some(url.into());
            return;
        }
    }
}

/// 登录第二步：优先用粘贴的回调/query/refresh token，否则取本机自动收到的回调。
pub(super) async fn login_poll(root: &Path, state: &str, pasted: Option<&str>) -> Result<bool, String> {
    let pasted = pasted.unwrap_or("").trim();
    let pasted = {
        let mut pending = PENDING.lock().await;
        let login = pending.as_mut().filter(|login| login.state == state && login.created.elapsed() < LOGIN_TIMEOUT)
            .ok_or("登录已失效，请重新登录")?;
        let pasted = if pasted.is_empty() { login.callback.lock().unwrap().take() } else { Some(pasted.to_string()) };
        if pasted.is_some() {
            for task in &login.tasks { task.abort(); }
        }
        pasted
    };
    let Some(pasted) = pasted else { return Ok(false) };
    let http = provider::local_api_client();
    let refresh_token = if pasted.starts_with("1//") {
        pasted.to_string()
    } else {
        let (code, callback_state) = parse_callback(&pasted)?;
        if !state.is_empty() && !callback_state.is_empty() && callback_state != state {
            return Err("授权回调的 state 与本次登录不匹配，请重新登录".into());
        }
        let body = post_form(
            &http,
            &[
                ("client_id", CLIENT_ID),
                ("client_secret", CLIENT_SECRET),
                ("code", code.as_str()),
                ("grant_type", "authorization_code"),
                ("redirect_uri", REDIRECT_URI),
            ],
        )
        .await?;
        body["refresh_token"]
            .as_str()
            .filter(|token| !token.is_empty())
            .ok_or("Google 未返回 refresh token（可能已授权过，请到账号里撤销后重试）")?
            .to_string()
    };
    let mut credentials = Credentials { refresh_token, project: String::new(), email: String::new(), updated_at: now_ms() };
    let token = access_token(&http, &credentials).await?;
    credentials.project = discover_project(&http, &token).await?;
    credentials.email = user_email(&http, &token).await.unwrap_or_default();
    save_credentials(root, &credentials)?;
    Ok(true)
}

/// 从跳转地址或裸 query 里取 code 与 state；错误回调也必须反馈，不能一直等到超时。
fn parse_callback(pasted: &str) -> Result<(String, String), String> {
    let query = pasted.split_once('?').map_or(pasted, |(_, rest)| rest).split('#').next().unwrap_or("");
    let mut url = reqwest::Url::parse(REDIRECT_URI).unwrap();
    url.set_query(Some(query));
    let mut code = String::new();
    let mut state = String::new();
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "code" => code = value.into_owned(),
            "state" => state = value.into_owned(),
            "error" if !value.is_empty() => return Err(format!("Google 授权失败：{value}")),
            _ => {}
        }
    }
    if code.is_empty() {
        return Err("授权地址里没有 code：请复制浏览器地址栏完整地址，或直接粘贴 refresh token".into());
    }
    Ok((code, state))
}

/// provider 片段：`resolve_local` 直接拿去当一个 provider 用。
///
/// 这里只读文件不换令牌：`load` 是同步入口，在 tokio 运行时里 block_on 会直接 panic。
/// apiKey 因此存 refresh token，由 `models`/`stream` 现换成访问令牌（带进程内缓存）。
/// 注意不能设置 `localAuth`：那个字段会路由到 `local_auth::credentials`，它只认 CLI 登录来源。
pub(super) fn load(root: &Path, _env: &std::collections::HashMap<String, String>) -> Result<Value, String> {
    let credentials = read_credentials(&login_path(root))?;
    Ok(json!({
        "name": "Google Antigravity",
        "api": "antigravity",
        "options": {
            "baseURL": BASE_URL,
            "apiKey": credentials.refresh_token,
            "project": credentials.project,
            "account": credentials.email,
            "headers": { "user-agent": USER_AGENT },
        },
        "models": {},
    }))
}

/// refresh token → 访问令牌，进程内缓存到过期前 5 分钟。
async fn bearer(http: &reqwest::Client, refresh_token: &str) -> Result<String, String> {
    use std::sync::Mutex;
    use std::time::Instant;
    static CACHE: Mutex<Vec<(String, String, Instant)>> = Mutex::new(Vec::new());
    if let Ok(cache) = CACHE.lock() {
        if let Some((_, token, expiry)) = cache.iter().find(|(key, _, _)| key == refresh_token) {
            if *expiry > Instant::now() {
                return Ok(token.clone());
            }
        }
    }
    let credentials = Credentials { refresh_token: refresh_token.to_string(), ..Credentials::default() };
    let token = access_token(http, &credentials).await?;
    if let Ok(mut cache) = CACHE.lock() {
        cache.retain(|(key, _, _)| key != refresh_token);
        cache.push((refresh_token.to_string(), token.clone(), Instant::now() + Duration::from_secs(55 * 60)));
    }
    Ok(token)
}

/// 账户级模型目录：上游按权益下发，无权模型以 `-disabled` 占位出现，必须过滤。
pub(super) async fn models(options: &Value, _force: bool) -> Result<Map<String, Value>, String> {
    let refresh = options["apiKey"].as_str().unwrap_or("").trim();
    if refresh.is_empty() {
        return Err("Antigravity 未登录".into());
    }
    let http = provider::local_api_client();
    let token = bearer(&http, refresh).await?;
    let base = options["baseURL"].as_str().filter(|url| !url.trim().is_empty()).unwrap_or(BASE_URL);
    let response = provider::send_request(
        http
            .post(format!("{}/v1internal:fetchAvailableModels", base.trim_end_matches('/')))
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .header("user-agent", USER_AGENT)
            .json(&json!({ "project": options["project"].as_str().unwrap_or("") })),
        &Arc::new(AtomicBool::new(false)),
    )
    .await?;
    let status = response.status();
    let body: Value = response.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        return Err(format!("Antigravity 模型目录读取失败（{status}）"));
    }
    Ok(catalog_models(&body))
}

/// fetchAvailableModels 的响应 → Nova 的模型表。抽出来方便离线测试。
fn catalog_models(body: &Value) -> Map<String, Value> {
    let mut out = Map::new();
    let Some(models) = body["models"].as_object() else { return out };
    for (id, info) in models {
        if id.ends_with("-disabled") {
            continue;
        }
        let context = info["maxTokens"].as_u64().filter(|value| *value > 0).unwrap_or(1_048_576);
        let output = info["maxOutputTokens"].as_u64().filter(|value| *value > 0).unwrap_or(65_536);
        let name = info["displayName"].as_str().filter(|name| !name.trim().is_empty()).unwrap_or(id);
        // 目录里的 thinkingLevel 是有档位的思考模型的标记；没有它就别在界面上摆思考强度。
        let reasoning = info.get("thinkingLevel").is_some_and(|level| !level.is_null())
            || info["supportsThinking"].as_bool() == Some(true);
        out.insert(
            id.clone(),
            json!({
                "name": name,
                "reasoning": reasoning,
                // Antigravity 目录里全是 Gemini，图片输入是标配；不写的话 Nova 会当纯文本模型。
                "modalities": { "input": ["text", "image"] },
                "limit": { "context": context, "output": output },
            }),
        );
    }
    out
}

/// 裸 `gemini-4-carbon` 上游会 400：Carbon 必须带思考档位后缀，缺省按 medium（官方默认）。
fn resolve_model_id(id: &str) -> String {
    let id = id.trim();
    if id == "gemini-4-carbon" { "gemini-4-carbon-medium".into() } else { id.to_string() }
}

pub(super) async fn stream(
    model: &ResolvedModel,
    api_key: &str,
    body: Value,
    cancel: &Arc<AtomicBool>,
    on_event: &mut (dyn FnMut(StreamEvent) + Send),
) -> Result<StreamResult, String> {
    let project = model.extra_options.get("project").and_then(Value::as_str).unwrap_or("");
    let payload = envelope(&resolve_model_id(&model.id), project, &body)?;
    let base = if model.base_url.trim().is_empty() { BASE_URL } else { model.base_url.trim_end_matches('/') };
    let http = provider::local_api_client();
    let token = bearer(&http, api_key).await?;
    let mut request = http
        .post(format!("{base}/v1internal:streamGenerateContent?alt=sse"))
        .header("content-type", "application/json")
        .header("accept", "text/event-stream")
        .header("authorization", format!("Bearer {token}"))
        .json(&payload);
    for (key, value) in &model.headers {
        if let Some(text) = value.as_str() {
            request = request.header(key.as_str(), text);
        }
    }
    let response = provider::send_request(request, cancel).await?;
    let status = response.status();
    if !status.is_success() {
        let text = response.text().await.unwrap_or_default();
        return Err(format!(
            "Antigravity 上游 HTTP {}：{}",
            status.as_u16(),
            text.trim().chars().take(300).collect::<String>()
        ));
    }
    let mut state = StreamState::default();
    let mut buffer = String::new();
    let mut response = response;
    loop {
        let chunk = tokio::select! {
            chunk = tokio::time::timeout(IDLE_TIMEOUT, response.chunk()) => chunk
                .map_err(|_| "Antigravity 响应连续 5 分钟无数据")?
                .map_err(|_| "Antigravity 响应流读取失败")?,
            _ = provider::wait_cancelled(cancel) => return Err("Antigravity 请求已取消".into()),
        };
        let Some(chunk) = chunk else { break };
        if chunk.is_empty() {
            continue;
        }
        on_event(StreamEvent::Activity);
        buffer.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(end) = buffer.find("\n\n") {
            let event: String = buffer.drain(..end + 2).collect();
            for line in event.lines() {
                let Some(data) = line.strip_prefix("data:") else { continue };
                let data = data.trim();
                if data.is_empty() || data == "[DONE]" {
                    continue;
                }
                let Ok(payload) = serde_json::from_str::<Value>(data) else { continue };
                state.apply(&payload, on_event);
            }
        }
    }
    // 末尾没有空行分隔的残帧同样要处理，否则最后一句话会丢。
    for line in buffer.lines() {
        if let Some(data) = line.strip_prefix("data:") {
            if let Ok(payload) = serde_json::from_str::<Value>(data.trim()) {
                state.apply(&payload, on_event);
            }
        }
    }
    Ok(state.finish())
}

fn push_content(contents: &mut Vec<Value>, role: &str, parts: Vec<Value>) {
    if parts.is_empty() {
        return;
    }
    // Gemini 要求一轮 functionCall 后紧跟的那一轮包含全部 functionResponse：并行工具调用、
    // 工具结果后面追加的截图都会产生连续同角色轮次，合并成一才不会被上游拒。
    if let Some(last) = contents.last_mut() {
        if last["role"].as_str() == Some(role) {
            if let Some(target) = last["parts"].as_array_mut() {
                target.extend(parts);
                return;
            }
        }
    }
    contents.push(json!({ "role": role, "parts": parts }));
}

/// 把 OpenAI-completions 形状的请求体翻译成 Antigravity 信封。
fn envelope(model_id: &str, project: &str, body: &Value) -> Result<Value, String> {
    let mut contents: Vec<Value> = Vec::new();
    let mut system: Vec<String> = Vec::new();
    let mut tool_names: Map<String, Value> = Map::new();
    for message in body["messages"].as_array().cloned().unwrap_or_default() {
        match message["role"].as_str().unwrap_or("") {
            "system" => {
                let text = text_of(&message["content"]);
                if !text.trim().is_empty() {
                    system.push(text);
                }
            }
            "user" => {
                let parts = parts_of(&message["content"]);
                push_content(&mut contents, "user", parts);
            }
            "assistant" => {
                let mut parts = Vec::new();
                let text = text_of(&message["content"]);
                if !text.trim().is_empty() {
                    parts.push(json!({ "text": text }));
                }
                for call in message["tool_calls"].as_array().cloned().unwrap_or_default() {
                    let name = call["function"]["name"].as_str().unwrap_or("");
                    if name.is_empty() {
                        continue;
                    }
                    let id = call["id"].as_str().unwrap_or("");
                    if !id.is_empty() {
                        tool_names.insert(id.to_string(), json!(name));
                    }
                    // 历史参数是 JSON 字符串；上游要的是对象，坏掉的参数直接丢该次调用。
                    let arguments = match call["function"]["arguments"].as_str() {
                        Some(raw) => serde_json::from_str::<Value>(raw).unwrap_or(Value::Null),
                        None => call["function"]["arguments"].clone(),
                    };
                    if arguments.is_object() {
                        parts.push(json!({ "functionCall": { "name": name, "args": arguments } }));
                    }
                }
                push_content(&mut contents, "model", parts);
            }
            "tool" => {
                let id = message["tool_call_id"].as_str().unwrap_or("");
                let name = tool_names.get(id).and_then(Value::as_str).unwrap_or("tool").to_string();
                let output = text_of(&message["content"]);
                push_content(&mut contents, "user", vec![json!({ "functionResponse": { "name": name, "response": { "result": output } } })]);
            }
            _ => {}
        }
    }
    if contents.is_empty() {
        return Err("Antigravity 请求没有可发送的内容".into());
    }
    let mut request = json!({ "contents": contents });
    if !system.is_empty() {
        request["systemInstruction"] = json!({ "parts": [{ "text": system.join("\n\n") }] });
    }
    let declarations: Vec<Value> = body["tools"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|tool| {
            let function = &tool["function"];
            let name = function["name"].as_str()?;
            let mut declaration = json!({ "name": name });
            if let Some(description) = function["description"].as_str().filter(|text| !text.is_empty()) {
                declaration["description"] = json!(description);
            }
            let parameters = &function["parameters"];
            if parameters.is_object() {
                declaration["parameters"] = sanitize_schema(parameters.clone());
            }
            Some(declaration)
        })
        .collect();
    if !declarations.is_empty() {
        request["tools"] = json!([{ "functionDeclarations": declarations }]);
    }
    let mut generation = json!({});
    if let Some(max) = body["max_tokens"].as_u64().or_else(|| body["max_completion_tokens"].as_u64()) {
        generation["maxOutputTokens"] = json!(max);
    }
    if !generation.as_object().is_some_and(Map::is_empty) {
        request["generationConfig"] = generation;
    }
    Ok(json!({
        "model": model_id,
        "userAgent": "antigravity",
        "requestType": "agent",
        "project": project,
        "requestId": format!("agent-{}", uuid::Uuid::new_v4()),
        "request": request,
    }))
}

/// Gemini 的 JSON Schema 只认一部分关键字：去掉会 400 的字段，其余原样保留。
fn sanitize_schema(mut schema: Value) -> Value {
    if let Some(object) = schema.as_object_mut() {
        for key in ["$schema", "additionalProperties", "strict", "title", "$id", "default"] {
            object.remove(key);
        }
        // Gemini 的 enum 是 repeated string；数字枚举也须字符串传输，参数 type 保持不变。
        if let Some(values) = object.get_mut("enum").and_then(Value::as_array_mut) {
            for value in values {
                if !value.is_string() { *value = Value::String(value.to_string()); }
            }
        }
        for key in ["properties", "$defs", "definitions"] {
            if let Some(map) = object.get_mut(key).and_then(Value::as_object_mut) {
                for value in map.values_mut() {
                    *value = sanitize_schema(value.clone());
                }
            }
        }
        for key in ["items", "anyOf", "oneOf", "allOf"] {
            if let Some(array) = object.get_mut(key).and_then(Value::as_array_mut) {
                for value in array.iter_mut() {
                    *value = sanitize_schema(value.clone());
                }
            } else if let Some(value) = object.get_mut(key) {
                if value.is_object() {
                    *value = sanitize_schema(value.clone());
                }
            }
        }
    }
    schema
}

fn text_of(content: &Value) -> String {
    if let Some(text) = content.as_str() {
        return text.to_string();
    }
    content
        .as_array()
        .map(|blocks| {
            blocks
                .iter()
                .filter(|block| block["type"] == "text")
                .filter_map(|block| block["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

fn parts_of(content: &Value) -> Vec<Value> {
    if let Some(text) = content.as_str().filter(|text| !text.is_empty()) {
        return vec![json!({ "text": text })];
    }
    content
        .as_array()
        .map(|blocks| {
            blocks
                .iter()
                .filter_map(|block| match block["type"].as_str() {
                    Some("text") => Some(json!({ "text": block["text"].as_str().unwrap_or("") })),
                    Some("image_url") | Some("image") => {
                        let url = block["image_url"]["url"].as_str().or_else(|| block["url"].as_str()).unwrap_or("");
                        inline_image(url)
                    }
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

fn inline_image(url: &str) -> Option<Value> {
    let (meta, data) = url.strip_prefix("data:")?.split_once(";base64,")?;
    Some(json!({ "inlineData": { "mimeType": meta, "data": data } }))
}

/// 上游 SSE 一帧：既可能是裸 Gemini chunk，也可能包在 `{"response": {...}}` 里。
#[derive(Default)]
struct StreamState {
    text: String,
    thinking: String,
    calls: Vec<(String, String, Value)>,
    usage: Value,
    stop: String,
}

impl StreamState {
    fn apply(&mut self, payload: &Value, on_event: &mut (dyn FnMut(StreamEvent) + Send)) {
        let chunk = if payload.get("response").is_some_and(Value::is_object) { &payload["response"] } else { payload };
        if let Some(usage) = chunk.get("usageMetadata").filter(|usage| usage.is_object()) {
            self.usage = usage.clone();
        }
        let Some(candidate) = chunk["candidates"].as_array().and_then(|list| list.first()) else { return };
        for part in candidate["content"]["parts"].as_array().cloned().unwrap_or_default() {
            if let Some(text) = part["text"].as_str().filter(|text| !text.is_empty()) {
                if part["thought"].as_bool() == Some(true) {
                    self.thinking.push_str(text);
                    on_event(StreamEvent::ThinkingDelta(text.to_string()));
                } else {
                    self.text.push_str(text);
                    on_event(StreamEvent::TextDelta(text.to_string()));
                }
            }
            if let Some(call) = part.get("functionCall").filter(|call| call["name"].is_string()) {
                let name = call["name"].as_str().unwrap_or("").to_string();
                let arguments = call["args"].clone();
                let id = format!("antigravity-{}-{name}", self.calls.len());
                self.calls.push((id.clone(), name.clone(), arguments.clone()));
                on_event(StreamEvent::ToolCallDone { id, name, arguments });
            }
        }
        if let Some(reason) = candidate["finishReason"].as_str() {
            self.stop = reason.to_string();
        }
    }

    fn finish(self) -> StreamResult {
        let mut content = Vec::new();
        if !self.thinking.is_empty() {
            content.push(json!({ "type": "thinking", "thinking": self.thinking }));
        }
        if !self.text.is_empty() {
            content.push(json!({ "type": "text", "text": self.text }));
        }
        for (id, name, arguments) in &self.calls {
            content.push(json!({ "type": "toolCall", "id": id, "name": name, "arguments": arguments }));
        }
        let count = |key: &str| self.usage[key].as_u64().unwrap_or(0);
        let input = count("promptTokenCount");
        let mut output = count("candidatesTokenCount");
        output = output.saturating_add(count("thoughtsTokenCount"));
        let stop_reason = if !self.calls.is_empty() {
            "tool_use"
        } else if self.stop == "MAX_TOKENS" {
            "max_tokens"
        } else {
            "end_turn"
        };
        StreamResult {
            content,
            usage: json!({ "input": input, "output": output, "cacheRead": count("cachedContentTokenCount"), "cacheWrite": 0 }),
            stop_reason: stop_reason.into(),
            error_message: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn login_url_requests_only_supported_antigravity_scopes() {
        let login = login_start("").await.unwrap();
        let url = reqwest::Url::parse(login["url"].as_str().unwrap()).unwrap();
        let scope = url.query_pairs().find(|(key, _)| key == "scope").unwrap().1;
        assert_eq!(scope.split_whitespace().collect::<Vec<_>>(), [
            "https://www.googleapis.com/auth/cloud-platform",
            "https://www.googleapis.com/auth/userinfo.email",
            "https://www.googleapis.com/auth/userinfo.profile",
            "https://www.googleapis.com/auth/cclog",
            "https://www.googleapis.com/auth/experimentsandconfigs",
        ]);
        let root = tempfile::tempdir().unwrap();
        let state = login["state"].as_str().unwrap();
        assert!(!login_poll(root.path(), state, None).await.unwrap());
        assert!(login_poll(root.path(), "other", None).await.unwrap_err().contains("失效"));
        let slot = PENDING.lock().await.as_ref().unwrap().callback.clone();
        *slot.lock().unwrap() = Some(format!("{REDIRECT_URI}?error=access_denied&state={state}"));
        assert!(login_poll(root.path(), state, None).await.unwrap_err().contains("access_denied"));
        assert!(!login_path(root.path()).exists(), "OAuth errors must not save credentials");
        let retry = login_start("").await.unwrap();
        assert_ne!(retry["state"], login["state"]);
        let state = retry["state"].as_str().unwrap();
        assert!(!login_poll(root.path(), state, None).await.unwrap());
        assert!(login_poll(root.path(), state, Some("code=abc&state=other")).await.unwrap_err().contains("不匹配"));
    }

    fn body() -> Value {
        json!({
            "model": "gemini-4-carbon-high",
            "max_tokens": 8192,
            "messages": [
                { "role": "system", "content": "你是助手" },
                { "role": "user", "content": "看看 src/main.rs" },
                { "role": "assistant", "content": null, "tool_calls": [
                    { "id": "c1", "type": "function", "function": { "name": "read_file", "arguments": "{\"path\":\"src/main.rs\"}" } }
                ]},
                { "role": "tool", "tool_call_id": "c1", "content": "fn main() {}" }
            ],
            "tools": [{ "type": "function", "function": { "name": "read_file", "description": "读文件",
                "parameters": { "type": "object", "properties": { "path": { "type": "string", "title": "路径" } }, "additionalProperties": false } } }]
        })
    }

    #[test]
    fn envelope_wraps_completions_into_antigravity_request() {
        let payload = envelope("gemini-4-carbon-high", "proj-1", &body()).unwrap();
        assert_eq!(payload["model"], "gemini-4-carbon-high");
        assert_eq!(payload["project"], "proj-1");
        assert_eq!(payload["requestType"], "agent");
        assert_eq!(payload["userAgent"], "antigravity");
        assert!(payload["requestId"].as_str().unwrap().starts_with("agent-"));
        assert_eq!(payload["request"]["systemInstruction"]["parts"][0]["text"], "你是助手");
        assert_eq!(payload["request"]["contents"][0]["role"], "user");
        // 工具调用 → functionCall，工具结果 → functionResponse，名字要按 id 回填。
        assert_eq!(payload["request"]["contents"][1]["parts"][0]["functionCall"]["name"], "read_file");
        assert_eq!(payload["request"]["contents"][1]["parts"][0]["functionCall"]["args"]["path"], "src/main.rs");
        assert_eq!(payload["request"]["contents"][2]["parts"][0]["functionResponse"]["name"], "read_file");
        assert_eq!(payload["request"]["contents"][2]["parts"][0]["functionResponse"]["response"]["result"], "fn main() {}");
        assert_eq!(payload["request"]["generationConfig"]["maxOutputTokens"], 8192);
        let schema = &payload["request"]["tools"][0]["functionDeclarations"][0]["parameters"];
        assert!(schema.get("additionalProperties").is_none());
        assert!(schema["properties"]["path"].get("title").is_none());
    }

    #[test]
    fn tool_enums_use_strings_on_the_wire_without_changing_parameter_types() {
        let parameters = json!({ "type": "object", "required": ["maxEdge"], "properties": {
            "maxEdge": { "anyOf": [
                { "type": "integer", "enum": [0] },
                { "type": "integer", "minimum": 640, "maximum": 3840 }
            ] },
            "modes": { "type": "array", "items": { "type": "string", "enum": ["desktop", "window"] } },
            "enabled": { "type": "boolean", "enum": [true, false] },
            "scale": { "type": "number", "enum": [0.5, 1] },
            "levels": { "type": "array", "items": { "type": "integer", "enum": [1, 2] } }
        } });
        let mut body = body();
        body["tools"][0]["function"]["parameters"] = parameters.clone();
        let payload = envelope("gemini-4-carbon-high", "proj-1", &body).unwrap();
        let schema = &payload["request"]["tools"][0]["functionDeclarations"][0]["parameters"];
        let mut expected = parameters.clone();
        expected["properties"]["maxEdge"]["anyOf"][0]["enum"] = json!(["0"]);
        expected["properties"]["enabled"]["enum"] = json!(["true", "false"]);
        expected["properties"]["scale"]["enum"] = json!(["0.5", "1"]);
        expected["properties"]["levels"]["items"]["enum"] = json!(["1", "2"]);
        assert_eq!(schema, &expected, "只转换枚举传输形式，不改参数类型、范围、required 或字符串枚举");
        assert_eq!(sanitize_schema(schema.clone()), *schema, "转换可重复应用");
        assert_eq!(body["tools"][0]["function"]["parameters"], parameters, "原始工具 schema 不变");
        assert!(crate::automation_schema::check(&parameters, &json!({ "maxEdge": 0 }), "参数").is_ok());
        assert!(crate::automation_schema::check(&parameters, &json!({ "maxEdge": "0" }), "参数").is_err());
        assert!(crate::automation_schema::check(&parameters, &json!({ "maxEdge": 1 }), "参数").is_err());
    }

    #[test]
    fn bare_carbon_model_defaults_to_medium_level() {
        assert_eq!(resolve_model_id("gemini-4-carbon"), "gemini-4-carbon-medium");
        assert_eq!(resolve_model_id(" gemini-4-carbon-high "), "gemini-4-carbon-high");
        assert_eq!(resolve_model_id("gemini-4-argon"), "gemini-4-argon");
    }

    #[test]
    fn parallel_tool_results_and_screenshots_merge_into_one_user_turn() {
        // 并行工具调用 → 连续两条 tool 消息；最后一张截图是另一条 user 消息。
        let body = json!({ "messages": [
            { "role": "user", "content": "看看这两张图" },
            { "role": "assistant", "content": null, "tool_calls": [
                { "id": "a", "function": { "name": "shot", "arguments": "{}" } },
                { "id": "b", "function": { "name": "shot", "arguments": "{}" } }
            ]},
            { "role": "tool", "tool_call_id": "a", "content": "ok-a" },
            { "role": "tool", "tool_call_id": "b", "content": "ok-b" },
            { "role": "user", "content": [{ "type": "image_url", "image_url": { "url": "data:image/png;base64,AAAA" } }] }
        ]});
        let payload = envelope("gemini-4-carbon-high", "p", &body).unwrap();
        let contents = payload["request"]["contents"].as_array().unwrap();
        assert_eq!(contents.len(), 3, "{contents:?}");
        assert_eq!(contents[1]["parts"].as_array().unwrap().len(), 2, "两个 functionResponse 必须同轮");
        assert_eq!(contents[2]["parts"].as_array().unwrap().len(), 3, "截图跟在工具结果后面同一轮");
        assert_eq!(contents[2]["parts"][2]["inlineData"]["mimeType"], "image/png");
    }

    #[test]
    fn empty_conversation_is_rejected_before_hitting_upstream() {
        assert!(envelope("gemini-4-carbon-high", "", &json!({ "messages": [] })).is_err());
    }

    #[test]
    fn sse_chunks_emit_text_thinking_and_tools() {
        let mut events = Vec::new();
        {
            let mut on_event = |event| events.push(event);
            let mut state = StreamState::default();
            state.apply(&json!({ "response": { "candidates": [{ "content": { "parts": [
                { "text": "想一下", "thought": true }, { "text": "答案是" }
            ]}}]}}), &mut on_event);
            state.apply(&json!({ "candidates": [{ "content": { "parts": [
                { "functionCall": { "name": "read_file", "args": { "path": "a.rs" } } }
            ]}, "finishReason": "STOP" }],
                "usageMetadata": { "promptTokenCount": 10, "candidatesTokenCount": 5, "thoughtsTokenCount": 3 } }), &mut on_event);
            let result = state.finish();
            assert_eq!(result.stop_reason, "tool_use");
            assert_eq!(result.usage["input"], 10);
            assert_eq!(result.usage["output"], 8);
            assert_eq!(result.content[0]["type"], "thinking");
            assert_eq!(result.content[1]["text"], "答案是");
            assert_eq!(result.content[2]["name"], "read_file");
        }
        assert!(events.iter().any(|event| matches!(event, StreamEvent::ThinkingDelta(text) if text == "想一下")));
        assert!(events.iter().any(|event| matches!(event, StreamEvent::TextDelta(text) if text == "答案是")));
        assert!(events.iter().any(|event| matches!(event, StreamEvent::ToolCallDone { name, .. } if name == "read_file")));
    }

    #[test]
    fn disabled_models_are_dropped_from_the_account_catalog() {
        let models = catalog_models(&json!({ "models": {
            "gemini-4-carbon-high": { "displayName": "Gemini 4 Carbon (High)", "maxTokens": 1048576, "maxOutputTokens": 262144, "thinkingLevel": 3 },
            "gemini-4-carbon-disabled": { "displayName": "Gemini 4 Carbon" },
            "gemini-3.8-flash": { "maxTokens": 1048576, "maxOutputTokens": 65535 }
        }}));
        assert!(!models.contains_key("gemini-4-carbon-disabled"));
        assert_eq!(models["gemini-4-carbon-high"]["limit"]["output"], 262144);
        assert_eq!(models["gemini-4-carbon-high"]["name"], "Gemini 4 Carbon (High)");
        assert_eq!(models["gemini-4-carbon-high"]["reasoning"], true);
        assert_eq!(models["gemini-3.8-flash"]["reasoning"], false);
        assert_eq!(models["gemini-3.8-flash"]["name"], "gemini-3.8-flash");
        assert_eq!(models["gemini-4-carbon-high"]["modalities"]["input"], json!(["text", "image"]));
    }

    #[test]
    fn callback_parsing_accepts_pasted_url_or_bare_query() {
        for pasted in [
            "http://localhost:51121/oauth-callback?code=abc%2F1&state=xyz",
            "http://localhost:51121/oauth-callback?state=xyz&code=abc%2F1#ignored",
            "code=abc%2F1&state=xyz",
        ] {
            assert_eq!(parse_callback(pasted).unwrap(), ("abc/1".into(), "xyz".into()));
        }
        assert_eq!(parse_callback("code=abc%2B1%3D%26&state=s").unwrap().0, "abc+1=&");
        assert!(parse_callback("http://localhost:51121/oauth-callback").is_err());
        assert!(parse_callback("http://localhost:51121/oauth-callback?error=access_denied").unwrap_err().contains("access_denied"));
    }

    #[tokio::test]
    async fn loopback_callback_checks_state_and_keeps_waiting_after_invalid_request() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let slot = Arc::new(Mutex::new(None));
        let task = tokio::spawn(serve_callback(listener, "expected".into(), slot.clone()));
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        for path in [
            "/oauth-callback?code=abc&state=other",
            "/oauth-callback?code=abc",
            "/other?code=abc&state=expected",
        ] {
            let response = http.get(format!("http://{address}{path}")).send().await.unwrap();
            assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
            assert!(slot.lock().unwrap().is_none());
        }
        let response = http.get(format!("http://{address}/oauth-callback?code=abc%2F1&state=expected")).send().await.unwrap();
        assert!(response.status().is_success());
        assert!(response.text().await.unwrap().contains("已收到"));
        tokio::time::timeout(Duration::from_secs(5), task).await.unwrap().unwrap();
        assert_eq!(parse_callback(slot.lock().unwrap().as_deref().unwrap()).unwrap(), ("abc/1".into(), "expected".into()));
    }

    #[tokio::test]
    async fn loopback_callback_forwards_oauth_errors() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let slot = Arc::new(Mutex::new(None));
        let task = tokio::spawn(serve_callback(listener, "expected".into(), slot.clone()));
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        let response = http.get(format!("http://{address}/oauth-callback?error=access_denied&state=expected")).send().await.unwrap();
        assert!(response.status().is_success());
        tokio::time::timeout(Duration::from_secs(5), task).await.unwrap().unwrap();
        assert!(parse_callback(slot.lock().unwrap().as_deref().unwrap()).unwrap_err().contains("access_denied"));
    }
}
