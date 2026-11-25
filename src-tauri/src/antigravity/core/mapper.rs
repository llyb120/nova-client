//! ACP `session/update` 到 Nova 事件/条目的纯 Rust 映射器：
//! - 覆盖全部 `sessionUpdate` 变体；
//! - `tool_call_update` 按字段合并，严禁覆盖既有 `title`/`rawInput`/`locations`；
//! - `reconcile_usage` 兼容 ACP 1.5（净输入 + 含思考输出）与 1.4.3（含缓存输入 + 不含思考输出）两套口径，
//!   并将 `cachedReadTokens`/`cachedWriteTokens` 重命名为 Nova `Thread::push_turn` 识别的键名；
//! - 将 `agy` 混入正文流的 `⚠ ` 内联诊断通告分流为独立系统消息。

use super::quota::is_usage_limit_reply;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};

/// 纯 Rust 工具调用快照（与 `threads::ToolCall` 1:1 对应，不依赖 `crate::threads`）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MappedToolCall {
    pub tool_call_id: String,
    pub title: String,
    pub kind: String,
    pub status: String,
    pub content: Vec<Value>,
    pub locations: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_input: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_output: Option<Value>,
}

/// 映射器产出的纯 Rust 动作，由薄胶水层 `runtime.rs` 写入 `Thread` 并发射 `EV_*`。
#[derive(Clone, Debug, PartialEq)]
pub enum MappedAction {
    AssistantDelta(String),
    ThoughtDelta(String),
    ToolUpsert(MappedToolCall),
    /// `Some(entries)` 更新计划列表；`None` 清空计划。
    Plan(Option<Vec<Value>>),
    LiveUsage {
        context_tokens: u64,
        context_window: Option<u64>,
        cost: Option<Value>,
    },
    SystemNotice {
        level: String,
        text: String,
    },
    ModeChanged {
        wire_mode: String,
    },
    ConfigOptionsUpdated {
        config_options: Vec<Value>,
    },
    SessionInfoUpdated {
        title: Option<String>,
    },
    Log(String),
}

#[derive(Default, Debug)]
pub struct TurnMapper {
    tools: HashMap<String, MappedToolCall>,
    active_tools: HashSet<String>,
    running_commands: HashSet<String>,
    last_context_used: Option<u64>,
    context_window: Option<u64>,
    suppress_replay: bool,
    cancelled: bool,
    assistant_text: String,
    had_thought: bool,
    inline_unauthenticated: Option<String>,
    pending_notice_line: String,
    in_code_fence: bool,
    fallback_tool_seq: u64,
}

impl TurnMapper {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_suppress_replay(&mut self, suppress: bool) {
        self.suppress_replay = suppress;
    }

    /// 标记本轮已取消：此后收到的任何尾部 `session/update` 均被幂等忽略，防止取消后继续刷屏。
    pub fn mark_cancelled(&mut self) {
        self.cancelled = true;
        self.pending_notice_line.clear();
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled
    }

    pub fn active_tools(&self) -> &HashSet<String> {
        &self.active_tools
    }

    pub fn has_running_command(&self) -> bool {
        !self.running_commands.is_empty()
    }

    pub fn tool(&self, tool_call_id: &str) -> Option<&MappedToolCall> {
        self.tools.get(tool_call_id)
    }

    pub fn last_context_used(&self) -> Option<u64> {
        self.last_context_used
    }

    pub fn context_window(&self) -> Option<u64> {
        self.context_window
    }

    pub fn assistant_text(&self) -> &str {
        &self.assistant_text
    }

    pub fn take_inline_unauthenticated(&mut self) -> Option<String> {
        self.inline_unauthenticated.take()
    }

    pub fn has_substantive_output(&self) -> bool {
        !self.tools.is_empty() || self.had_thought || !self.assistant_text.trim().is_empty()
    }

    /// 刷出跨 chunk 缓冲的尾部行（应在收到 `session/prompt` 响应后调用）。
    pub fn flush_pending(&mut self) -> Vec<MappedAction> {
        if self.cancelled || self.pending_notice_line.is_empty() {
            self.pending_notice_line.clear();
            return Vec::new();
        }
        let pending = std::mem::take(&mut self.pending_notice_line);
        let trimmed = pending.trim();
        if !self.in_code_fence {
            if is_inline_unauthenticated_line(trimmed) {
                self.inline_unauthenticated = Some(trimmed.to_string());
                return Vec::new();
            }
            if let Some(level) = classify_inline_warning_line(trimmed) {
                return vec![MappedAction::SystemNotice {
                    level: level.to_string(),
                    text: trimmed.to_string(),
                }];
            }
        }
        self.assistant_text.push_str(&pending);
        vec![MappedAction::AssistantDelta(pending)]
    }

    /// 消费单条 `session/update` 的 `update` 对象，返回待应用的映射动作列表。
    pub fn on_update(&mut self, update: &Value) -> Vec<MappedAction> {
        if self.cancelled {
            return Vec::new();
        }
        let Some(kind) = update.get("sessionUpdate").and_then(Value::as_str) else {
            return Vec::new();
        };

        // `session/load` 重放窗口内仅放行配置与模式同步，丢弃历史消息重放防止 UI 重复气泡。
        if self.suppress_replay
            && !matches!(kind, "config_option_update" | "current_mode_update")
        {
            return Vec::new();
        }

        if matches!(kind, "tool_call" | "tool_call_update") {
            if let Some(id) = update["toolCallId"].as_str().filter(|id| !id.is_empty()) {
                if matches!(update["status"].as_str(), Some("completed" | "failed")) {
                    self.running_commands.remove(id);
                } else if update["name"] == "run_command" {
                    self.running_commands.insert(id.to_owned());
                }
            }
        }
        match kind {
            "agent_message_chunk" => self.on_message_chunk(update),
            "agent_thought_chunk" => {
                let text = format_content_block(update.get("content"));
                if text.is_empty() {
                    Vec::new()
                } else {
                    if !text.trim().is_empty() {
                        self.had_thought = true;
                    }
                    vec![MappedAction::ThoughtDelta(text)]
                }
            }
            "tool_call" => self.on_tool_call(update),
            "tool_call_update" => self.on_tool_call_update(update),
            "plan" | "plan_update" => {
                let entries = update
                    .get("entries")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                vec![MappedAction::Plan(Some(entries))]
            }
            "plan_removed" => vec![MappedAction::Plan(None)],
            "usage_update" => {
                let used = update.get("used").and_then(Value::as_u64).unwrap_or(0);
                let size = update.get("size").and_then(Value::as_u64).filter(|s| *s > 0);
                // 建会话初始空包（used == 0）必须忽略
                if used == 0 {
                    return Vec::new();
                }
                self.last_context_used = Some(used);
                if size.is_some() {
                    self.context_window = size;
                }
                let cost = update.get("cost").cloned().filter(|v| !v.is_null());
                vec![MappedAction::LiveUsage {
                    context_tokens: used,
                    context_window: self.context_window,
                    cost,
                }]
            }
            "current_mode_update" => {
                let mode = update
                    .get("currentModeId")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                if mode.is_empty() {
                    Vec::new()
                } else {
                    vec![MappedAction::ModeChanged { wire_mode: mode }]
                }
            }
            "config_option_update" => {
                let config_options = update
                    .get("configOptions")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                if config_options.is_empty() {
                    Vec::new()
                } else {
                    vec![MappedAction::ConfigOptionsUpdated { config_options }]
                }
            }
            "session_info_update" => {
                let title = update
                    .get("title")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string);
                vec![MappedAction::SessionInfoUpdated { title }]
            }
            "notice" => {
                let text = update
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                if text.is_empty() {
                    return Vec::new();
                }
                let raw_level = update
                    .get("level")
                    .and_then(Value::as_str)
                    .unwrap_or("info")
                    .to_ascii_lowercase();
                let level = match raw_level.as_str() {
                    "error" | "fatal" => "error",
                    "warn" | "warning" => "warn",
                    _ => "info",
                }
                .to_string();
                vec![MappedAction::SystemNotice { level, text }]
            }
            "subagent_update" => {
                let status = update
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("running");
                let mut actions = vec![MappedAction::Log(format!("subagent_update: status={status}"))];
                if status == "error" {
                    let msg = update
                        .get("message")
                        .or_else(|| update.get("error"))
                        .and_then(Value::as_str)
                        .unwrap_or("子代理执行失败");
                    actions.push(MappedAction::SystemNotice {
                        level: "warn".into(),
                        text: format!("Antigravity 子代理异常：{msg}"),
                    });
                }
                actions
            }
            "compaction_update" | "compaction_summary_chunk" => {
                vec![MappedAction::Log(format!("{kind}"))]
            }
            "available_commands_update"
            | "user_message_chunk"
            | "session_message"
            | "session_message_chunk" => {
                // 用户消息已由 Nova 在发送前落库，忽略 user_message_chunk 防止重复气泡。
                Vec::new()
            }
            other => vec![MappedAction::Log(format!("未识别的 sessionUpdate: {other}"))],
        }
    }

    fn on_message_chunk(&mut self, update: &Value) -> Vec<MappedAction> {
        let text = format_content_block(update.get("content"));
        if text.is_empty() {
            return Vec::new();
        }
        split_inline_notices_streaming(
            &text,
            &mut self.pending_notice_line,
            &mut self.in_code_fence,
            &mut self.assistant_text,
            &mut self.inline_unauthenticated,
        )
    }

    fn alloc_fallback_tool_id(&mut self) -> String {
        self.fallback_tool_seq += 1;
        format!("agy-tool-{}", self.fallback_tool_seq)
    }

    fn on_tool_call(&mut self, update: &Value) -> Vec<MappedAction> {
        let tool_call_id = update
            .get("toolCallId")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| self.alloc_fallback_tool_id());

        let raw_title = update
            .get("title")
            .or_else(|| update.get("name"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let raw_kind = update
            .get("kind")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("other")
            .to_string();
        let status = update
            .get("status")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("in_progress")
            .to_string();
        let content = update
            .get("content")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let locations = update
            .get("locations")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let raw_input = update.get("rawInput").cloned().filter(|v| !v.is_null());
        let raw_output = update.get("rawOutput").cloned().filter(|v| !v.is_null());

        let mut call = MappedToolCall {
            tool_call_id: tool_call_id.clone(),
            title: raw_title,
            kind: raw_kind,
            status,
            content,
            locations,
            raw_input,
            raw_output,
        };
        normalize_mcp_tool_display(&mut call);
        if call.title.trim().is_empty() {
            call.title = call.kind.clone();
        }

        self.track_tool_liveness(&tool_call_id, &call.status);
        self.tools.insert(tool_call_id, call.clone());
        vec![MappedAction::ToolUpsert(call)]
    }

    fn on_tool_call_update(&mut self, update: &Value) -> Vec<MappedAction> {
        let Some(tool_call_id) = update
            .get("toolCallId")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
        else {
            return Vec::new();
        };

        let mut call = self.tools.get(&tool_call_id).cloned().unwrap_or_else(|| {
            MappedToolCall {
                tool_call_id: tool_call_id.clone(),
                title: String::new(),
                kind: "other".into(),
                status: "in_progress".into(),
                content: Vec::new(),
                locations: Vec::new(),
                raw_input: None,
                raw_output: None,
            }
        });

        // 严禁整对象覆盖：仅在载荷显式提供非空值时合并对应字段。
        if let Some(title) = update
            .get("title")
            .or_else(|| update.get("name"))
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
        {
            call.title = title.to_string();
        }
        if let Some(kind) = update
            .get("kind")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
        {
            call.kind = kind.to_string();
        }
        if let Some(status) = update
            .get("status")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
        {
            call.status = status.to_string();
        }
        if let Some(content) = update
            .get("content")
            .and_then(Value::as_array)
            .filter(|arr| !arr.is_empty())
        {
            call.content = content.clone();
        }
        if let Some(locations) = update
            .get("locations")
            .and_then(Value::as_array)
            .filter(|arr| !arr.is_empty())
        {
            call.locations = locations.clone();
        }
        if let Some(raw_input) = update.get("rawInput").filter(|v| !v.is_null()) {
            call.raw_input = Some(raw_input.clone());
        }
        if let Some(raw_output) = update.get("rawOutput").filter(|v| !v.is_null()) {
            call.raw_output = Some(raw_output.clone());
        }

        normalize_mcp_tool_display(&mut call);
        if call.title.trim().is_empty() {
            call.title = call.kind.clone();
        }

        self.track_tool_liveness(&tool_call_id, &call.status);
        self.tools.insert(tool_call_id, call.clone());
        vec![MappedAction::ToolUpsert(call)]
    }

    fn track_tool_liveness(&mut self, tool_call_id: &str, status: &str) {
        if matches!(status, "completed" | "failed") {
            self.active_tools.remove(tool_call_id);
        } else {
            self.active_tools.insert(tool_call_id.to_string());
        }
    }

    /// 将 `PromptResponse.usage` + 实时 `usage_update.used` 转换为 Nova `Thread::push_turn`
    /// 直接消费的 JSON 结构，利用 `totalTokens` 自动对账 ACP 1.5 与 1.4.3 两套口径。
    pub fn reconcile_usage(&self, raw_usage: Option<&Value>) -> Option<Value> {
        let Some(raw) = raw_usage.filter(|v| v.is_object()) else {
            return self
                .last_context_used
                .map(|used| json!({ "contextTokens": used }));
        };

        let get_u64 = |keys: &[&str]| {
            keys.iter()
                .find_map(|k| raw.get(*k).and_then(Value::as_u64))
        };

        let raw_in = get_u64(&["inputTokens", "input_tokens"]);
        let cr = get_u64(&[
            "cachedReadTokens",
            "cacheReadTokens",
            "cached_read_tokens",
            "cache_read_tokens",
        ]);
        let cw = get_u64(&[
            "cachedWriteTokens",
            "cacheWriteTokens",
            "cached_write_tokens",
            "cache_write_tokens",
        ]);
        let raw_out = get_u64(&["outputTokens", "output_tokens"]);
        let thought = get_u64(&["thoughtTokens", "thought_tokens"]);
        let total = get_u64(&["totalTokens", "total_tokens"]).unwrap_or(0);

        if raw_in.is_none() && raw_out.is_none() && total == 0 && self.last_context_used.is_none() {
            return None;
        }

        let r_in = raw_in.unwrap_or(0);
        let r_cr = cr.unwrap_or(0);
        let r_cw = cw.unwrap_or(0);
        let r_out = raw_out.unwrap_or(0);
        let r_thought = thought.unwrap_or(0);

        let net_in_with_cache = r_in.saturating_add(r_cr).saturating_add(r_cw);
        let out_with_thought = r_out.saturating_add(r_thought);

        let (gross_in, total_out) = if total > 0 {
            if net_in_with_cache.saturating_add(r_out) == total {
                // ACP 1.5.0+：inputTokens 不含缓存，outputTokens 已含 thoughtTokens
                (net_in_with_cache, r_out)
            } else if r_thought > 0 && net_in_with_cache.saturating_add(out_with_thought) == total {
                // 净输入 + 未含 thought 的输出
                (net_in_with_cache, out_with_thought)
            } else if r_thought > 0 && r_in.saturating_add(out_with_thought) == total {
                // ACP 1.4.3：inputTokens 已含缓存，outputTokens 未含 thoughtTokens
                (r_in, out_with_thought)
            } else if r_in.saturating_add(r_out) == total {
                // ACP 1.4.3：inputTokens 已含缓存，outputTokens 已含（或无）thoughtTokens
                (r_in, r_out)
            } else {
                // 兜底：若 raw_in 小于缓存读/写之和，说明必为净输入口径；否则默认 1.5 净输入语义
                (net_in_with_cache, r_out)
            }
        } else {
            (net_in_with_cache, r_out)
        };

        let final_total = if total > 0 {
            total
        } else {
            gross_in.saturating_add(total_out)
        };

        let mut out_map = Map::new();
        out_map.insert("inputTokens".into(), json!(gross_in));
        out_map.insert("outputTokens".into(), json!(total_out));
        out_map.insert("totalTokens".into(), json!(final_total));
        if let Some(v) = cr {
            out_map.insert("cacheReadTokens".into(), json!(v));
        }
        if let Some(v) = cw {
            out_map.insert("cacheWriteTokens".into(), json!(v));
        }
        if let Some(used) = self.last_context_used {
            out_map.insert("contextTokens".into(), json!(used));
        }
        Some(Value::Object(out_map))
    }

    /// 归一化 `stopReason` 并检查伪正常配额耗尽响应（`totalTokens == 0` 或正文命中 `"Usage Limit Reached"`/`"quota"`）。
    /// 返回 `(nova_stop_reason, optional_system_notice(level, text))`。
    pub fn normalize_stop_reason(
        &self,
        raw_stop_reason: &str,
        reconciled_usage: Option<&Value>,
    ) -> (&'static str, Option<(&'static str, String)>) {
        if self.cancelled {
            return ("cancelled", None);
        }
        let total_tokens = reconciled_usage
            .and_then(|u| u.get("totalTokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        if is_usage_limit_reply(&self.assistant_text, total_tokens, raw_stop_reason) {
            return (
                "error",
                Some((
                    "error",
                    "Antigravity 配额已耗尽（Usage Limit Reached / Quota），请稍后重试或切换模型。"
                        .into(),
                )),
            );
        }
        match raw_stop_reason {
            "end_turn" => ("end_turn", None),
            "max_turn_requests" => ("max_turn_requests", None),
            "max_tokens" => (
                "max_tokens",
                Some(("warn", "已触达单轮输出 token 上限，回答可能未完整结束。".into())),
            ),
            "cancelled" => ("cancelled", None),
            "refusal" => ("refusal", None),
            _ => ("error", None),
        }
    }
}

/// 反向映射 MCP 工具名前缀：`mcp_<server>_<tool>` → `<tool>`。
/// 支持：
/// - `mcp_nova_chrome_tabs` → `chrome_tabs`
/// - `Run mcp_nova_code_map` / `Call mcp_nova_polaris`（实测 `agy 1.5.15` 会在工具标题前加 `Run `）→ `code_map` / `polaris`
/// - `mcp_github_list_issues` → `list_issues`
/// - 带参数的标题如 `mcp_nova_chrome_tabs (query="docs")` → `chrome_tabs (query="docs")`
pub fn unwrap_mcp_tool_name(raw: &str) -> String {
    let trimmed = raw.trim();
    // 实测 `agy 1.5.15` 推送的 MCP tool_call title 为 `"Run mcp_nova_<tool>"`
    let candidate = trimmed
        .strip_prefix("Run ")
        .or_else(|| trimmed.strip_prefix("Call "))
        .or_else(|| trimmed.strip_prefix("Calling "))
        .map(str::trim_start)
        .filter(|rest| rest.starts_with("mcp_"))
        .unwrap_or(trimmed);

    if let Some(rest) = candidate.strip_prefix("mcp_nova_") {
        return rest.to_string();
    }
    if let Some(after_mcp) = candidate.strip_prefix("mcp_") {
        if let Some(idx) = after_mcp.find('_') {
            let server = &after_mcp[..idx];
            let tool = &after_mcp[idx + 1..];
            if !server.is_empty()
                && !tool.is_empty()
                && server
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            {
                return tool.to_string();
            }
        }
    }
    trimmed.to_string()
}

/// 规范化 `mcp_<server>_<tool>` 或 `call_mcp_tool` 包装的展示标题与 kind。
fn normalize_mcp_tool_display(call: &mut MappedToolCall) {
    if call.title == "call_mcp_tool" {
        if let Some(input) = call.raw_input.as_ref().and_then(Value::as_object) {
            if let Some(tool_name) = input.get("tool").and_then(Value::as_str) {
                call.title = unwrap_mcp_tool_name(tool_name);
            }
            if let Some(args) = input.get("arguments").filter(|v| !v.is_null()) {
                call.raw_input = Some(args.clone());
            }
        }
    } else {
        call.title = unwrap_mcp_tool_name(&call.title);
    }

    if call.kind == "other" || call.kind.trim().is_empty() {
        let base_tool = call
            .title
            .split_whitespace()
            .next()
            .unwrap_or(call.title.as_str());
        if matches!(base_tool, "polaris" | "code_map") {
            call.kind = "search".into();
        } else if base_tool.starts_with("browser_") || base_tool.starts_with("chrome_") {
            call.kind = "fetch".into();
        }
    }
}

/// 按文件扩展名推断图片 MIME 类型。
pub fn infer_image_mime_type(path: &str) -> &'static str {
    let ext = path
        .rsplit('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    match ext.as_str() {
        "webp" => "image/webp",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        _ => "image/png",
    }
}

/// 零外部依赖的标准 RFC 4648 Base64 编码（用于纯 Rust core 图片块构造）。
pub fn encode_base64_standard(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((triple >> 18) & 0x3F) as usize] as char);
        out.push(TABLE[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(triple & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

/// 构造 MCP `tools/call` 结果内容块：
/// - 文本结果走 `{ "type": "text", "text": ... }`；
/// - `raw_value` 中 `images[].path` 或 `imagePath` 对应的截图通过 `read_image` 读盘后走标准
///   `{ "type": "image", "data": "<base64>", "mimeType": "<mime>" }` 块，绝不把 base64 塞进文本块。
pub fn build_mcp_content_blocks<F>(
    text: String,
    raw_value: Option<&Value>,
    read_image: F,
) -> Vec<Value>
where
    F: Fn(&str) -> Option<(Vec<u8>, &'static str)>,
{
    let mut blocks = vec![json!({ "type": "text", "text": text })];
    let Some(val) = raw_value else {
        return blocks;
    };

    let mut seen_paths = HashSet::new();
    let mut paths: Vec<String> = Vec::new();
    if let Some(arr) = val.get("images").and_then(Value::as_array) {
        for item in arr {
            // `unchanged: true` means this exact screenshot path was already delivered in an earlier turn
            // of the same session; skip re-sending duplicate Base64 image blocks while keeping JSON metadata.
            if item.get("unchanged").and_then(Value::as_bool) == Some(true) {
                continue;
            }
            if let Some(p) = item
                .get("path")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                if seen_paths.insert(p.to_string()) {
                    paths.push(p.to_string());
                }
            }
        }
    }
    if let Some(p) = val
        .get("imagePath")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        if seen_paths.insert(p.to_string()) {
            paths.push(p.to_string());
        }
    }

    for path in paths {
        if let Some((bytes, mime)) = read_image(&path) {
            if !bytes.is_empty() {
                blocks.push(json!({
                    "type": "image",
                    "data": encode_base64_standard(&bytes),
                    "mimeType": mime,
                }));
            }
        }
    }
    blocks
}

/// 格式化 ACP `ContentBlock` 为可读文本。
fn format_content_block(block: Option<&Value>) -> String {
    let Some(block) = block else {
        return String::new();
    };
    if let Some(s) = block.as_str() {
        return s.to_string();
    }
    match block.get("type").and_then(Value::as_str) {
        Some("text") => block
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        Some("image") => {
            let mime = block
                .get("mimeType")
                .and_then(Value::as_str)
                .unwrap_or("image");
            format!("[图片: {mime}]")
        }
        Some("audio") => {
            let mime = block
                .get("mimeType")
                .and_then(Value::as_str)
                .unwrap_or("audio");
            format!("[音频: {mime}]")
        }
        Some("resource_link") => {
            let name = block
                .get("name")
                .or_else(|| block.get("uri"))
                .and_then(Value::as_str)
                .unwrap_or("资源");
            format!("[资源链接: {name}]")
        }
        Some("resource") => {
            if let Some(text) = block.pointer("/resource/text").and_then(Value::as_str) {
                text.to_string()
            } else {
                let uri = block
                    .pointer("/resource/uri")
                    .and_then(Value::as_str)
                    .unwrap_or("嵌入资源");
                format!("[资源: {uri}]")
            }
        }
        _ => block
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
    }
}

/// 拆分 `agy` 混入 `agent_message_chunk` 正文流的 `⚠` 内联诊断通告（支持跨 chunk 缓冲与围栏代码块保护）。
fn split_inline_notices_streaming(
    chunk: &str,
    pending_line: &mut String,
    in_code_fence: &mut bool,
    assistant_acc: &mut String,
    inline_unauthenticated: &mut Option<String>,
) -> Vec<MappedAction> {
    let combined = if pending_line.is_empty() {
        chunk.to_string()
    } else {
        let mut s = std::mem::take(pending_line);
        s.push_str(chunk);
        s
    };

    let mut actions = Vec::new();
    let mut normal_buf = String::new();
    for segment in combined.split_inclusive('\n') {
        let has_newline = segment.ends_with('\n');
        let trimmed = segment.trim();

        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            *in_code_fence = !*in_code_fence;
            normal_buf.push_str(segment);
            continue;
        }
        if *in_code_fence {
            normal_buf.push_str(segment);
            continue;
        }

        if let Some(level) = classify_inline_warning_line(trimmed) {
            if !has_newline {
                // 等待完整行到达后再发射 notice，避免截断诊断细节
                pending_line.push_str(segment);
                continue;
            }
            if !normal_buf.is_empty() {
                assistant_acc.push_str(&normal_buf);
                actions.push(MappedAction::AssistantDelta(std::mem::take(&mut normal_buf)));
            }
            if is_inline_unauthenticated_line(trimmed) {
                *inline_unauthenticated = Some(trimmed.to_string());
            } else {
                actions.push(MappedAction::SystemNotice {
                    level: level.to_string(),
                    text: trimmed.to_string(),
                });
            }
        } else if !has_newline && could_be_partial_warning_line(trimmed) {
            pending_line.push_str(segment);
        } else {
            normal_buf.push_str(segment);
        }
    }

    if !normal_buf.is_empty() {
        assistant_acc.push_str(&normal_buf);
        actions.push(MappedAction::AssistantDelta(normal_buf));
    }
    actions
}

fn could_be_partial_warning_line(trimmed: &str) -> bool {
    !trimmed.is_empty() && trimmed.starts_with('⚠') && trimmed.len() <= 256
}

pub fn is_inline_unauthenticated_line(trimmed: &str) -> bool {
    if !trimmed.starts_with('⚠') {
        return false;
    }
    let lower = trimmed.to_ascii_lowercase();
    lower.contains("unauthenticated")
        && (lower.contains("401") || lower.contains("invalid authentication credentials"))
}

fn classify_inline_warning_line(trimmed: &str) -> Option<&'static str> {
    if !trimmed.starts_with('⚠') {
        return None;
    }
    if trimmed.contains("Agent execution terminated due to error.")
        || trimmed.contains("Your AI credits balance is too low")
        || trimmed.contains("Model produced invalid output.")
        || trimmed.contains("RESOURCE_EXHAUSTED")
    {
        return Some("error");
    }
    if trimmed.contains("API error (attempt")
        || trimmed.contains("There was a network issue")
        || trimmed.contains("The stream was interrupted")
        || trimmed.starts_with("⚠ ")
    {
        return Some("warn");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg_chunk(text: &str) -> Value {
        json!({
            "sessionUpdate": "agent_message_chunk",
            "content": { "type": "text", "text": text }
        })
    }

    #[test]
    fn captures_inline_401_without_premature_notice_or_substantive_progress() {
        for suffix in ["\n", ""] {
            let mut mapper = TurnMapper::new();
            let mut actions = mapper.on_update(&msg_chunk("\n"));
            let line = format!("⚠ UNAUTHENTICATED (code 401): Request had invalid authentication credentials. Expected OAuth 2 access token.{suffix}");
            actions.extend(mapper.on_update(&msg_chunk(&line)));
            actions.extend(mapper.flush_pending());

            assert!(
                !actions
                    .iter()
                    .any(|a| matches!(a, MappedAction::SystemNotice { .. })),
                "should not emit premature SystemNotice for inline 401"
            );
            assert!(!mapper.has_substantive_output());
            let captured = mapper.take_inline_unauthenticated().unwrap();
            assert!(captured.contains("UNAUTHENTICATED (code 401)"));
            assert!(mapper.take_inline_unauthenticated().is_none());
        }
    }

    #[test]
    fn inline_401_after_tool_or_thought_marks_substantive_output() {
        let mut mapper_thought = TurnMapper::new();
        mapper_thought.on_update(&json!({
            "sessionUpdate": "agent_thought_chunk",
            "content": { "type": "text", "text": "Checking files..." }
        }));
        mapper_thought.on_update(&msg_chunk("⚠ UNAUTHENTICATED (code 401): Request had invalid authentication credentials.\n"));
        assert!(mapper_thought.has_substantive_output());
        assert!(mapper_thought.take_inline_unauthenticated().is_some());

        let mut mapper_tool = TurnMapper::new();
        mapper_tool.on_update(&json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "t1",
            "name": "read_file",
            "status": "completed"
        }));
        mapper_tool.on_update(&msg_chunk("⚠ UNAUTHENTICATED (code 401): Request had invalid authentication credentials."));
        mapper_tool.flush_pending();
        assert!(mapper_tool.has_substantive_output());
        assert!(mapper_tool.take_inline_unauthenticated().is_some());
    }

    #[test]
    fn non_auth_inline_warnings_still_emit_system_notice() {
        let mut mapper = TurnMapper::new();
        let mut actions = mapper.on_update(&msg_chunk("⚠ API error (attempt 1): transient failure\n"));
        actions.extend(mapper.on_update(&msg_chunk("⚠ RESOURCE_EXHAUSTED\n")));
        assert!(mapper.take_inline_unauthenticated().is_none());
        assert_eq!(actions.len(), 2);
        assert!(matches!(
            &actions[0],
            MappedAction::SystemNotice { level, text }
                if level == "warn" && text.contains("API error")
        ));
        assert!(matches!(
            &actions[1],
            MappedAction::SystemNotice { level, text }
                if level == "error" && text.contains("RESOURCE_EXHAUSTED")
        ));
    }
}
