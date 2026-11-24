//! ACP（Agent Client Protocol）JSON-RPC 2.0 协议类型、有界 NDJSON 帧读写与辅助函数。

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fmt;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt};

/// 单条 NDJSON 入站/出站帧硬上限（32 MiB）。
pub const MAX_FRAME_BYTES: usize = 32 * 1024 * 1024;
/// 本实现要求的 ACP 协议版本。
pub const ACP_PROTOCOL_VERSION: u64 = 1;
/// 单步用户文本安全字节预算（低于 harness 192,000 B 硬限）。
pub const ACP_STEP_TEXT_BYTES: usize = 188_000;
/// `agy` 已知会在 stdout 混入的浏览器提示噪声行，必须静默忽略。
pub const KNOWN_STDOUT_NOISE: &[&str] = &["Opening in existing browser session."];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AcpErrorKind {
    Aborted,
    Auth,
    Model,
    Protocol,
    Quota,
    Spawn,
    Timeout,
    ProcessExit,
    Transport,
    Refusal,
    InvalidInput,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcpError {
    pub kind: AcpErrorKind,
    pub message: String,
    pub code: Option<i64>,
    pub stderr_tail: Option<String>,
}

impl AcpError {
    pub fn new(kind: AcpErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            code: None,
            stderr_tail: None,
        }
    }

    pub fn with_stderr(mut self, stderr_tail: Option<String>) -> Self {
        self.stderr_tail = stderr_tail.filter(|s| !s.trim().is_empty());
        self
    }

    pub fn protocol(message: impl Into<String>) -> Self {
        Self::new(AcpErrorKind::Protocol, message)
    }

    pub fn timeout(message: impl Into<String>) -> Self {
        Self::new(AcpErrorKind::Timeout, message)
    }

    pub fn spawn(message: impl Into<String>) -> Self {
        Self::new(AcpErrorKind::Spawn, message)
    }

    pub fn aborted(message: impl Into<String>) -> Self {
        Self::new(AcpErrorKind::Aborted, message)
    }

    pub fn process_exit(message: impl Into<String>, stderr_tail: Option<String>) -> Self {
        Self::new(AcpErrorKind::ProcessExit, message).with_stderr(stderr_tail)
    }

    /// 按 JSON-RPC 错误码与错误文案做错误分类：
    /// 特别注意 OAuth 刷新遇到 DNS/TCP/TLS 抖动时虽含 token/oauth 字样，但本质是网络断连，
    /// 必须归类为 `Transport`，避免误导用户重新登录。
    pub fn from_rpc_error(code: i64, message: impl Into<String>, stderr_tail: Option<String>) -> Self {
        let message = message.into();
        let lower = message.to_ascii_lowercase();
        let is_network = [
            "econnreset",
            "econnrefused",
            "etimedout",
            "enotfound",
            "eai_again",
            "ehostunreach",
            "enetunreach",
            "dns",
            "tls",
            "ssl",
            "handshake",
            "network",
            "connection reset",
            "connection refused",
            "broken pipe",
            "socket hang up",
            "unexpected eof",
            "tcp",
            "lookup",
        ]
        .iter()
        .any(|needle| lower.contains(needle));

        let kind = if is_network {
            AcpErrorKind::Transport
        } else if lower.contains("resource_exhausted")
            || lower.contains("resource has been exhausted")
            || lower.contains("quota")
            || lower.contains("usage limit")
            || lower.contains("rate limit")
            || lower.contains("ratelimitexceeded")
            || lower.contains("credits balance")
            || lower.contains("429")
        {
            AcpErrorKind::Quota
        } else if code == -32000
            || lower.contains("unauthenticated")
            || lower.contains("unauthorized")
            || lower.contains("invalid_grant")
            || lower.contains("auth required")
            || lower.contains("authentication required")
            || lower.contains("not logged in")
            || lower.contains("login required")
        {
            AcpErrorKind::Auth
        } else if lower.contains("model")
            && (lower.contains("not found")
                || lower.contains("invalid")
                || lower.contains("unavailable")
                || lower.contains("unsupported"))
        {
            AcpErrorKind::Model
        } else if lower.contains("refusal") || lower.contains("safety") || lower.contains("blocked") {
            AcpErrorKind::Refusal
        } else if code == -32602 {
            AcpErrorKind::InvalidInput
        } else {
            AcpErrorKind::Protocol
        };

        Self {
            kind,
            message,
            code: Some(code),
            stderr_tail: stderr_tail.filter(|s| !s.trim().is_empty()),
        }
    }
}

impl fmt::Display for AcpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)?;
        if let Some(code) = self.code {
            write!(f, " (code={code})")?;
        }
        if let Some(tail) = &self.stderr_tail {
            if !tail.trim().is_empty() {
                write!(f, "\n[stderr]\n{tail}")?;
            }
        }
        Ok(())
    }
}

impl std::error::Error for AcpError {}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(default)]
    pub data: Option<Value>,
}

/// 从子进程 stdout 解析出的一条 JSON-RPC 2.0 消息。
#[derive(Clone, Debug, PartialEq)]
pub enum IncomingMessage {
    Response {
        id: Value,
        result: Option<Value>,
        error: Option<JsonRpcError>,
    },
    Request {
        id: Value,
        method: String,
        params: Value,
    },
    Notification {
        method: String,
        params: Value,
    },
}

/// 编码一条出站 NDJSON 帧（自动追加 `\n` 并强制 32 MiB 上限）。
pub fn encode_ndjson_frame(value: &Value) -> Result<Vec<u8>, AcpError> {
    encode_ndjson_frame_with_limit(value, MAX_FRAME_BYTES)
}

pub fn encode_ndjson_frame_with_limit(value: &Value, max_bytes: usize) -> Result<Vec<u8>, AcpError> {
    let mut bytes = serde_json::to_vec(value)
        .map_err(|e| AcpError::protocol(format!("序列化 JSON-RPC 帧失败：{e}")))?;
    if bytes.len() + 1 > max_bytes {
        return Err(AcpError::protocol(format!(
            "出站 NDJSON 帧超过上限（{} > {} 字节）",
            bytes.len() + 1,
            max_bytes
        )));
    }
    bytes.push(b'\n');
    Ok(bytes)
}

/// 从异步缓冲流读取单行有界 NDJSON 帧；返回 `Ok(None)` 表示 EOF。
/// 超过 `max_bytes` 立即返回 `AcpErrorKind::Protocol`，防止内存被畸形超长行打爆。
pub async fn read_ndjson_frame<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    max_bytes: usize,
) -> Result<Option<Vec<u8>>, AcpError> {
    let mut buf = Vec::new();
    let limit = (max_bytes as u64).saturating_add(1);
    let read = reader
        .take(limit)
        .read_until(b'\n', &mut buf)
        .await
        .map_err(|e| AcpError::new(AcpErrorKind::Transport, format!("读取 ACP stdout 失败：{e}")))?;
    if read == 0 {
        return Ok(None);
    }
    if buf.len() > max_bytes {
        return Err(AcpError::protocol(format!(
            "入站 NDJSON 帧超过 {} 字节上限",
            max_bytes
        )));
    }
    Ok(Some(buf))
}

/// 解析单行 NDJSON 字节切片：
/// - 剔除尾部 `\n` / `\r`（兼容 CRLF）；
/// - 忽略空行与已知 stdout 噪声行（如 `"Opening in existing browser session."`）；
/// - 严格校验合法 JSON 与 `"jsonrpc": "2.0"` 结构。
pub fn parse_ndjson_line(raw: &[u8]) -> Result<Option<IncomingMessage>, AcpError> {
    parse_ndjson_line_with_limit(raw, MAX_FRAME_BYTES)
}

pub fn parse_ndjson_line_with_limit(
    raw: &[u8],
    max_bytes: usize,
) -> Result<Option<IncomingMessage>, AcpError> {
    if raw.len() > max_bytes {
        return Err(AcpError::protocol(format!(
            "入站 NDJSON 帧超过 {} 字节上限",
            max_bytes
        )));
    }
    let text = std::str::from_utf8(raw)
        .map_err(|_| AcpError::protocol("入站 ACP 帧不是有效 UTF-8"))?;
    let line = text.trim_end_matches('\n').trim_end_matches('\r').trim();
    if line.is_empty() {
        return Ok(None);
    }
    if KNOWN_STDOUT_NOISE.iter().any(|noise| line == *noise) {
        return Ok(None);
    }
    let value: Value = serde_json::from_str(line)
        .map_err(|e| AcpError::protocol(format!("入站 ACP 帧不是有效 JSON：{e}")))?;
    let obj = value
        .as_object()
        .ok_or_else(|| AcpError::protocol("入站 ACP 帧必须是 JSON 对象"))?;
    if obj.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(AcpError::protocol("入站 ACP 帧缺少 jsonrpc=\"2.0\""));
    }

    if let Some(method_val) = obj.get("method") {
        let method = method_val
            .as_str()
            .filter(|m| !m.is_empty())
            .ok_or_else(|| AcpError::protocol("JSON-RPC method 字段无效"))?
            .to_string();
        if obj.contains_key("result") || obj.contains_key("error") {
            return Err(AcpError::protocol(
                "JSON-RPC 请求/通知不应同时包含 result 或 error",
            ));
        }
        let params = obj.get("params").cloned().unwrap_or(Value::Null);
        if let Some(id) = obj.get("id") {
            if !id.is_string() && !id.is_number() {
                return Err(AcpError::protocol("JSON-RPC 请求 id 必须是字符串或数字"));
            }
            return Ok(Some(IncomingMessage::Request {
                id: id.clone(),
                method,
                params,
            }));
        }
        return Ok(Some(IncomingMessage::Notification { method, params }));
    }

    let id = obj
        .get("id")
        .cloned()
        .ok_or_else(|| AcpError::protocol("JSON-RPC 响应缺少 id"))?;
    let has_result = obj.contains_key("result");
    let has_error = obj.contains_key("error");
    if has_result == has_error {
        return Err(AcpError::protocol(
            "JSON-RPC 响应必须且只能包含 result 或 error 之一",
        ));
    }
    if let Some(err_val) = obj.get("error") {
        let code = err_val
            .get("code")
            .and_then(Value::as_i64)
            .ok_or_else(|| AcpError::protocol("JSON-RPC error 缺少整数 code"))?;
        let message = err_val
            .get("message")
            .and_then(Value::as_str)
            .ok_or_else(|| AcpError::protocol("JSON-RPC error 缺少字符串 message"))?
            .to_string();
        let data = err_val.get("data").cloned();
        return Ok(Some(IncomingMessage::Response {
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message,
                data,
            }),
        }));
    }
    Ok(Some(IncomingMessage::Response {
        id,
        result: obj.get("result").cloned(),
        error: None,
    }))
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentInfo {
    pub name: String,
    pub version: Option<String>,
    pub title: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PromptCapabilities {
    pub image: bool,
    pub audio: bool,
    pub embedded_context: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionCapabilities {
    pub resume: bool,
    pub close: bool,
    pub list: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentCapabilities {
    pub load_session: bool,
    pub prompt: PromptCapabilities,
    pub session: SessionCapabilities,
    pub mcp_http: bool,
    pub supports_system_prompt_append: bool,
    pub supports_disabled_tools: bool,
}

#[derive(Clone, Debug)]
pub struct InitializeResult {
    pub protocol_version: u64,
    pub agent_info: Option<AgentInfo>,
    pub capabilities: AgentCapabilities,
    pub auth_methods: Vec<Value>,
    pub raw: Value,
}

/// 校验并解析 `initialize` 响应（强校验 `protocolVersion == 1`）。
pub fn parse_initialize_result(result: Value) -> Result<InitializeResult, AcpError> {
    let version = result
        .get("protocolVersion")
        .and_then(Value::as_u64)
        .ok_or_else(|| AcpError::protocol("initialize 响应缺少整数 protocolVersion"))?;
    if version != ACP_PROTOCOL_VERSION {
        return Err(AcpError::protocol(format!(
            "不支持的 ACP protocolVersion：{version}（仅支持 {ACP_PROTOCOL_VERSION}）"
        )));
    }
    let agent_info = result.get("agentInfo").and_then(|info| {
        Some(AgentInfo {
            name: info.get("name")?.as_str()?.to_string(),
            version: info.get("version").and_then(Value::as_str).map(str::to_string),
            title: info.get("title").and_then(Value::as_str).map(str::to_string),
        })
    });
    let caps = result.get("agentCapabilities");
    let prompt_caps = caps.and_then(|c| c.get("promptCapabilities"));
    let session_caps = caps.and_then(|c| c.get("sessionCapabilities"));
    let mcp_caps = caps.and_then(|c| c.get("mcpCapabilities"));
    let supports_system_prompt_append = result
        .pointer("/_meta/antigravity/systemPrompt/append")
        .and_then(Value::as_bool)
        .or_else(|| {
            caps.and_then(|c| c.pointer("/_meta/antigravity/systemPrompt/append"))
                .and_then(Value::as_bool)
        })
        .unwrap_or(false);
    let supports_disabled_tools = result
        .pointer("/_meta/antigravity/disabledTools")
        .or_else(|| caps.and_then(|c| c.pointer("/_meta/antigravity/disabledTools")))
        .is_some_and(|v| !v.is_null() && v != &Value::Bool(false));

    let capabilities = AgentCapabilities {
        load_session: caps
            .and_then(|c| c.get("loadSession"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
        prompt: PromptCapabilities {
            image: prompt_caps
                .and_then(|p| p.get("image"))
                .and_then(Value::as_bool)
                .unwrap_or(false),
            audio: prompt_caps
                .and_then(|p| p.get("audio"))
                .and_then(Value::as_bool)
                .unwrap_or(false),
            embedded_context: prompt_caps
                .and_then(|p| p.get("embeddedContext"))
                .and_then(Value::as_bool)
                .unwrap_or(false),
        },
        session: SessionCapabilities {
            resume: session_caps.and_then(|s| s.get("resume")).is_some_and(|v| !v.is_null() && v != &Value::Bool(false)),
            close: session_caps.and_then(|s| s.get("close")).is_some_and(|v| !v.is_null() && v != &Value::Bool(false)),
            list: session_caps.and_then(|s| s.get("list")).is_some_and(|v| !v.is_null() && v != &Value::Bool(false)),
        },
        mcp_http: mcp_caps
            .and_then(|m| m.get("http"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
        supports_system_prompt_append,
        supports_disabled_tools,
    };

    let auth_methods = result
        .get("authMethods")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    Ok(InitializeResult {
        protocol_version: version,
        agent_info,
        capabilities,
        auth_methods,
        raw: result,
    })
}

#[derive(Clone, Debug)]
pub struct SessionNewParams {
    pub cwd: String,
    pub mcp_servers: Vec<Value>,
    pub meta: Option<Value>,
}

impl SessionNewParams {
    pub fn to_json(&self) -> Value {
        let mut obj = serde_json::Map::new();
        obj.insert("cwd".into(), Value::String(self.cwd.clone()));
        obj.insert("mcpServers".into(), Value::Array(self.mcp_servers.clone()));
        if let Some(meta) = &self.meta {
            obj.insert("_meta".into(), meta.clone());
        }
        Value::Object(obj)
    }
}

#[derive(Clone, Debug)]
pub struct SessionResumeParams {
    pub session_id: String,
    pub cwd: String,
    pub mcp_servers: Vec<Value>,
    pub meta: Option<Value>,
}

impl SessionResumeParams {
    pub fn to_json(&self) -> Value {
        let mut obj = serde_json::Map::new();
        obj.insert("sessionId".into(), Value::String(self.session_id.clone()));
        obj.insert("cwd".into(), Value::String(self.cwd.clone()));
        obj.insert("mcpServers".into(), Value::Array(self.mcp_servers.clone()));
        if let Some(meta) = &self.meta {
            obj.insert("_meta".into(), meta.clone());
        }
        Value::Object(obj)
    }
}

#[derive(Clone, Debug)]
pub struct SessionOpenResult {
    pub session_id: String,
    pub config_options: Vec<Value>,
    pub models: Option<Value>,
    pub modes: Option<Value>,
    pub raw: Value,
}

pub fn parse_session_open_result(
    result: Value,
    fallback_session_id: Option<&str>,
) -> Result<SessionOpenResult, AcpError> {
    let session_id = result
        .get("sessionId")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| fallback_session_id.map(str::to_string))
        .ok_or_else(|| AcpError::protocol("session/new 响应缺少 sessionId"))?;
    let config_options = result
        .get("configOptions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let models = result.get("models").cloned().filter(|v| !v.is_null());
    let modes = result.get("modes").cloned().filter(|v| !v.is_null());
    Ok(SessionOpenResult {
        session_id,
        config_options,
        models,
        modes,
        raw: result,
    })
}

#[derive(Clone, Debug)]
pub struct SessionPromptParams {
    pub session_id: String,
    pub prompt: Vec<Value>,
}

impl SessionPromptParams {
    pub fn to_json(&self) -> Value {
        json!({
            "sessionId": self.session_id,
            "prompt": self.prompt,
        })
    }
}

#[derive(Clone, Debug)]
pub struct PromptResult {
    pub stop_reason: String,
    pub usage: Option<Value>,
    pub raw: Value,
}

pub fn parse_prompt_result(result: Value) -> Result<PromptResult, AcpError> {
    let stop_reason = result
        .get("stopReason")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("end_turn")
        .to_string();
    let usage = result.get("usage").cloned().filter(|v| !v.is_null());
    Ok(PromptResult {
        stop_reason,
        usage,
        raw: result,
    })
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionOption {
    pub option_id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub kind: String,
}

#[derive(Clone, Debug)]
pub struct PermissionRequestParams {
    pub session_id: String,
    pub tool_call: Value,
    pub options: Vec<PermissionOption>,
}

pub fn parse_permission_request(params: &Value) -> Result<PermissionRequestParams, AcpError> {
    let session_id = params
        .get("sessionId")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let tool_call = params.get("toolCall").cloned().unwrap_or(Value::Null);
    let options = params
        .get("options")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|item| {
                    let option_id = item.get("optionId").and_then(Value::as_str)?.to_string();
                    let name = item
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let kind = item
                        .get("kind")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    Some(PermissionOption {
                        option_id,
                        name,
                        kind,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(PermissionRequestParams {
        session_id,
        tool_call,
        options,
    })
}

/// 自动放行选项优先级：`allow_once` > `allow_always` > `allow` > 任意 `allow*`。
/// 没有任何 `allow*` 选项时返回 `None`（调用方必须回复 `cancelled`）。
pub fn select_auto_allow_option(options: &[PermissionOption]) -> Option<String> {
    let mut best: Option<(u8, usize, &str)> = None;
    for (idx, opt) in options.iter().enumerate() {
        if opt.option_id.is_empty() {
            continue;
        }
        let kind = opt.kind.trim().to_ascii_lowercase();
        let rank = match kind.as_str() {
            "allow_once" => 0,
            "allow_always" => 1,
            "allow" => 2,
            other if other.starts_with("allow") => 3,
            _ => continue,
        };
        let replace = match best {
            None => true,
            Some((best_rank, best_idx, _)) => rank < best_rank || (rank == best_rank && idx < best_idx),
        };
        if replace {
            best = Some((rank, idx, opt.option_id.as_str()));
        }
    }
    best.map(|(_, _, id)| id.to_string())
}

/// 权限身份一致性注册表：校验同一 `toolCallId` 在 `tool_call` 与 `request_permission`
/// 之间未被篡改工具归属，防止异常冲突下盲目放行。
#[derive(Default, Debug)]
pub struct PermissionIdentityRegistry {
    entries: HashMap<String, (String, String)>,
}

impl PermissionIdentityRegistry {
    pub fn record_tool_call(&mut self, tool_call_id: &str, name_or_title: &str, kind: &str) {
        if tool_call_id.is_empty() {
            return;
        }
        let norm_name = name_or_title.trim().to_ascii_lowercase();
        let norm_kind = kind.trim().to_ascii_lowercase();
        if !norm_name.is_empty() || !norm_kind.is_empty() {
            self.entries
                .entry(tool_call_id.to_string())
                .or_insert((norm_name, norm_kind));
        }
    }

    /// 校验通过返回 `Ok(())`；发现同一 `toolCallId` 身份前后冲突返回 `Err(reason)`。
    pub fn verify_permission(&mut self, tool_call: &Value) -> Result<(), String> {
        let Some(tool_call_id) = tool_call
            .get("toolCallId")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        else {
            return Ok(());
        };
        let incoming_name = tool_call
            .get("name")
            .or_else(|| tool_call.get("title"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        let incoming_kind = tool_call
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();

        if let Some((known_name, known_kind)) = self.entries.get(tool_call_id) {
            if !known_kind.is_empty()
                && !incoming_kind.is_empty()
                && known_kind != &incoming_kind
            {
                return Err(format!(
                    "toolCallId={tool_call_id} 的 kind 冲突（原 {known_kind}，现 {incoming_kind}）"
                ));
            }
            if !known_name.is_empty()
                && !incoming_name.is_empty()
                && known_name != &incoming_name
                && !incoming_name.contains(known_name.as_str())
                && !known_name.contains(incoming_name.as_str())
            {
                return Err(format!(
                    "toolCallId={tool_call_id} 的工具名称冲突（原 {known_name}，现 {incoming_name}）"
                ));
            }
        } else if !incoming_name.is_empty() || !incoming_kind.is_empty() {
            self.entries
                .insert(tool_call_id.to_string(), (incoming_name, incoming_kind));
        }
        Ok(())
    }
}

/// 防止用户输入以 `/` 开头被 `agy` 截获为内置斜杠命令（`/plan`、`/logout` 等）：
/// 在首个 `/` 前插入零宽空格 `\u{200B}`（`agy` 在斜杠判定后会剥除它）。
pub fn escape_leading_slash(text: &str) -> String {
    let trimmed_start = text.trim_start();
    if trimmed_start.starts_with('/') {
        let leading_len = text.len() - trimmed_start.len();
        let mut out = String::with_capacity(text.len() + 3);
        out.push_str(&text[..leading_len]);
        out.push('\u{200B}');
        out.push_str(trimmed_start);
        out
    } else {
        text.to_string()
    }
}

/// 单步文本字节数硬上限裁剪（保证 UTF-8 字符边界安全且总字节数 `<= max_bytes`）。
pub fn truncate_step_text(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    const NOTICE: &str = "\n\n[输入超过单步长度上限，后续内容已截断]";
    if max_bytes <= NOTICE.len() {
        let mut end = max_bytes.min(text.len());
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        return text[..end].to_string();
    }
    let mut end = max_bytes - NOTICE.len();
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = String::with_capacity(end + NOTICE.len());
    out.push_str(&text[..end]);
    out.push_str(NOTICE);
    out
}

/// 将 `session/new` 或 `config_option_update` 返回的模型配置转为 Nova 标准的 `ModelOptions` JSON。
///
/// 实测 `agy 1.5.15` 的 `session/new` 同时返回：
/// - `models.availableModels`（23 个含思考档位的完整模型，如 `gemini-4-neon-high`、`claude-opus-5-5-high`）
/// - `configOptions`（9 个基础模型 + `thought_level` 思考档位）
/// 当 `models.availableModels` 提供更丰富的完整列表时优先展示完整列表，同时保留 `thought_level` 选项。
pub fn extract_nova_model_options(
    config_options: &[Value],
    models: Option<&Value>,
) -> Option<Value> {
    let mut cfg_model_entry: Option<(String, String, Vec<Value>)> = None;
    let mut thought_entries: Vec<Value> = Vec::new();

    // 1. 解析 ACP configOptions（category == "model" / "thought_level"）
    for opt in config_options {
        let id = opt.get("id").and_then(Value::as_str).unwrap_or_default();
        let category = opt.get("category").and_then(Value::as_str).unwrap_or_default();
        let is_model = category == "model" || id == "model";
        let is_thought = category == "thought_level"
            || id == "thought_level"
            || id == "thinking_level"
            || id == "reasoning_effort";
        if !is_model && !is_thought {
            continue;
        }
        let choices = flatten_config_choices(opt.get("options"));
        if choices.is_empty() {
            continue;
        }
        let current = opt
            .get("currentValue")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| {
                choices
                    .first()
                    .and_then(|c| c.get("value").and_then(Value::as_str))
                    .map(str::to_string)
            })
            .unwrap_or_default();
        if is_model {
            let name = opt
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("Model")
                .to_string();
            cfg_model_entry = Some((name, current, choices));
        } else {
            thought_entries.push(json!({
                "id": "thought_level",
                "name": opt.get("name").and_then(Value::as_str).unwrap_or("Reasoning Effort"),
                "currentValue": current,
                "options": choices,
            }));
        }
    }

    // 2. 解析 `models.availableModels`（如 `agy 1.5.15` 的 23 个完整模型）
    let available_models_entry: Option<(String, Vec<Value>)> = models.and_then(|models_obj| {
        let available = models_obj.get("availableModels").and_then(Value::as_array)?;
        let choices: Vec<Value> = available
            .iter()
            .filter_map(|m| {
                let value = m
                    .get("modelId")
                    .or_else(|| m.get("id"))
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())?;
                let name = m
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .unwrap_or(value);
                let mut entry = serde_json::Map::new();
                entry.insert("value".into(), Value::String(value.to_string()));
                entry.insert("name".into(), Value::String(name.to_string()));
                if let Some(desc) = m.get("description").and_then(Value::as_str) {
                    if !desc.is_empty() {
                        entry.insert("description".into(), Value::String(desc.to_string()));
                    }
                }
                if let Some(meta) = m.get("_meta").filter(|v| v.is_object()) {
                    entry.insert("_meta".into(), meta.clone());
                }
                Some(Value::Object(entry))
            })
            .collect();
        if choices.is_empty() {
            return None;
        }
        let current = models_obj
            .get("currentModelId")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .or_else(|| {
                choices
                    .first()
                    .and_then(|c| c.get("value").and_then(Value::as_str))
                    .map(str::to_string)
            })
            .unwrap_or_default();
        Some((current, choices))
    });

    // 3. 选择更完整的模型清单：当 availableModels 条目数多于 configOptions 时优先展示全部模型（23 个）
    let mut nova_config_options: Vec<Value> = Vec::new();
    match (cfg_model_entry, available_models_entry) {
        (Some((name, cfg_cur, cfg_choices)), Some((avail_cur, avail_choices))) => {
            if avail_choices.len() > cfg_choices.len() {
                nova_config_options.push(json!({
                    "id": "model",
                    "name": name,
                    "currentValue": avail_cur,
                    "options": avail_choices,
                }));
            } else {
                nova_config_options.push(json!({
                    "id": "model",
                    "name": name,
                    "currentValue": cfg_cur,
                    "options": cfg_choices,
                }));
            }
        }
        (Some((name, cfg_cur, cfg_choices)), None) => {
            nova_config_options.push(json!({
                "id": "model",
                "name": name,
                "currentValue": cfg_cur,
                "options": cfg_choices,
            }));
        }
        (None, Some((avail_cur, avail_choices))) => {
            nova_config_options.push(json!({
                "id": "model",
                "name": "Model",
                "currentValue": avail_cur,
                "options": avail_choices,
            }));
        }
        (None, None) => return None,
    }
    nova_config_options.extend(thought_entries);

    Some(json!({
        "configOptions": nova_config_options,
        "modes": Value::Null,
    }))
}

/// 在 ACP `configOptions` 中定位真实的模型配置 id（用于 `session/set_config_option`）。
pub fn find_model_config_id(config_options: &[Value]) -> Option<String> {
    config_options.iter().find_map(|opt| {
        let id = opt.get("id").and_then(Value::as_str)?;
        let category = opt.get("category").and_then(Value::as_str).unwrap_or_default();
        (category == "model" || id == "model").then(|| id.to_string())
    })
}

/// 在 ACP `configOptions` 中定位思考强度配置 id（用于 `session/set_config_option`）。
pub fn find_thought_level_config_id(config_options: &[Value]) -> Option<String> {
    config_options.iter().find_map(|opt| {
        let id = opt.get("id").and_then(Value::as_str)?;
        let category = opt.get("category").and_then(Value::as_str).unwrap_or_default();
        (category == "thought_level"
            || id == "thought_level"
            || id == "thinking_level"
            || id == "reasoning_effort")
            .then(|| id.to_string())
    })
}

/// 将用户选择的模型与思考强度映射为 `configOptions` 可接受的 `(base_model, thought_level)`。
/// 兼容：
/// - 直接命中 `configOptions[model]` 的值（如 `"gemini-4-neon"`、`"gpt-oss-120b-medium"`）；
/// - 冒号拼装格式 `<base>:<effort>`（如 `"gemini-4-neon:xhigh"`）；
/// - `agy 1.5.15` 23 模型完整 ID `<base>-<effort>`（如 `"gemini-4-neon-xhigh"` → `("gemini-4-neon", "xhigh")`）。
pub fn resolve_model_and_effort_for_config(
    config_options: &[Value],
    requested_model: Option<&str>,
    requested_effort: Option<&str>,
) -> (Option<String>, Option<String>) {
    let model_values: Vec<String> = config_options
        .iter()
        .find(|opt| {
            let id = opt.get("id").and_then(Value::as_str).unwrap_or_default();
            let cat = opt.get("category").and_then(Value::as_str).unwrap_or_default();
            cat == "model" || id == "model"
        })
        .map(|opt| {
            flatten_config_choices(opt.get("options"))
                .into_iter()
                .filter_map(|c| c.get("value").and_then(Value::as_str).map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    let effort_values: Vec<String> = config_options
        .iter()
        .find(|opt| {
            let id = opt.get("id").and_then(Value::as_str).unwrap_or_default();
            let cat = opt.get("category").and_then(Value::as_str).unwrap_or_default();
            cat == "thought_level"
                || id == "thought_level"
                || id == "thinking_level"
                || id == "reasoning_effort"
        })
        .map(|opt| {
            flatten_config_choices(opt.get("options"))
                .into_iter()
                .filter_map(|c| c.get("value").and_then(Value::as_str).map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    let explicit_effort = requested_effort
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let Some(raw_model) = requested_model.map(str::trim).filter(|s| !s.is_empty()) else {
        return (None, explicit_effort);
    };

    // 1. 精确命中 configOptions[model]
    if model_values.iter().any(|v| v == raw_model) {
        return (Some(raw_model.to_string()), explicit_effort);
    }

    // 2. 匹配 `<base>:<effort>`（Nova 内部折叠格式）
    if let Some((base, eff)) = raw_model.rsplit_once(':') {
        let eff_ok = effort_values.is_empty() || effort_values.iter().any(|e| e == eff);
        if (model_values.is_empty() || model_values.iter().any(|v| v == base)) && eff_ok {
            return (
                Some(base.to_string()),
                explicit_effort.or_else(|| Some(eff.to_string())),
            );
        }
    }

    // 3. 匹配 `<base>-<effort>`（agy 1.5.15 `availableModels` 中的 23 个组合 ID）
    if let Some((base, eff)) = raw_model.rsplit_once('-') {
        if model_values.iter().any(|v| v == base) && effort_values.iter().any(|e| e == eff) {
            return (
                Some(base.to_string()),
                explicit_effort.or_else(|| Some(eff.to_string())),
            );
        }
    }

    (Some(raw_model.to_string()), explicit_effort)
}

/// 决策 B：查找并协商推荐执行模式 `accept-edits`（绝不切到 `plan`）。
/// 若会话支持 `accept-edits` 且当前值尚未处于该模式，返回 `Some((mode_id, optional_config_id))`。
pub fn find_preferred_wire_mode(
    modes: Option<&Value>,
    config_options: &[Value],
) -> Option<(String, Option<String>)> {
    for opt in config_options {
        let id = opt.get("id").and_then(Value::as_str).unwrap_or_default();
        let category = opt.get("category").and_then(Value::as_str).unwrap_or_default();
        if category == "mode" || id == "mode" {
            let current = opt.get("currentValue").and_then(Value::as_str).unwrap_or_default();
            if current == "accept-edits" {
                return None;
            }
            let choices = flatten_config_choices(opt.get("options"));
            if choices
                .iter()
                .any(|c| c.get("value").and_then(Value::as_str) == Some("accept-edits"))
            {
                return Some(("accept-edits".into(), Some(id.to_string())));
            }
        }
    }
    if let Some(modes_obj) = modes {
        let current = modes_obj
            .get("currentModeId")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if current == "accept-edits" {
            return None;
        }
        let has_accept_edits = modes_obj
            .get("availableModes")
            .and_then(Value::as_array)
            .is_some_and(|arr| {
                arr.iter()
                    .any(|m| m.get("id").and_then(Value::as_str) == Some("accept-edits"))
            });
        if has_accept_edits {
            return Some(("accept-edits".into(), None));
        }
    }
    None
}

fn flatten_config_choices(raw: Option<&Value>) -> Vec<Value> {
    let Some(arr) = raw.and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in arr {
        if let Some(group_options) = entry.get("options").and_then(Value::as_array) {
            for child in group_options {
                if let Some(choice) = normalize_config_choice(child) {
                    out.push(choice);
                }
            }
        } else if let Some(choice) = normalize_config_choice(entry) {
            out.push(choice);
        }
    }
    out
}

fn normalize_config_choice(entry: &Value) -> Option<Value> {
    let value = entry
        .get("value")
        .or_else(|| entry.get("id"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())?;
    let name = entry
        .get("name")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or(value);
    let mut map = serde_json::Map::new();
    map.insert("value".into(), Value::String(value.to_string()));
    map.insert("name".into(), Value::String(name.to_string()));
    if let Some(desc) = entry.get("description").and_then(Value::as_str) {
        if !desc.is_empty() {
            map.insert("description".into(), Value::String(desc.to_string()));
        }
    }
    if let Some(meta) = entry.get("_meta").filter(|v| v.is_object()) {
        map.insert("_meta".into(), meta.clone());
    }
    Some(Value::Object(map))
}
