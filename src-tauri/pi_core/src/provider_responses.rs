//! OpenAI Responses provider conversion (`openai-responses`), ported from
//! pi-ai `openai-responses.js` + `openai-responses-shared.js`. Deterministic
//! request building and SSE folding; the HTTP transport lives in `nova_lib`.
//!
//! Scope: standard API-key auth, developer/system role, input_text/input_image,
//! message/function_call/reasoning output items, function_call_output, and the
//! Responses SSE event set. Deferred tools, service-tier pricing, and copilot
//! headers are out of scope for the Vega native path.

use serde_json::{json, Value};

use crate::agent::StreamTurn;
use crate::transform::transform_messages;

fn normalize_id_part(part: &str) -> String {
    let sanitized: String = part
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .collect();
    let truncated: String = sanitized.chars().take(64).collect();
    truncated.trim_end_matches('_').to_string()
}

fn normalize_tool_call_id(id: &str) -> String {
    if !id.contains('|') {
        return normalize_id_part(id);
    }
    let mut parts = id.splitn(2, '|');
    let call_id = normalize_id_part(parts.next().unwrap_or(""));
    let item_id_raw = parts.next().unwrap_or("");
    let mut item_id = normalize_id_part(item_id_raw);
    if !item_id.starts_with("fc_") {
        item_id = normalize_id_part(&format!("fc_{item_id}"));
    }
    format!("{call_id}|{item_id}")
}

/// Convert pi tool definitions to OpenAI Responses `tools`.
pub fn pi_tools_to_responses(tools: &[Value]) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| {
            json!({
                "type": "function",
                "name": tool.get("name").and_then(Value::as_str).unwrap_or(""),
                "description": tool.get("description").and_then(Value::as_str).unwrap_or(""),
                "parameters": tool.get("parameters").cloned().unwrap_or(json!({ "type": "object", "properties": {} })),
                "strict": false,
            })
        })
        .collect()
}

/// Convert transformed pi messages to OpenAI Responses `input` items.
fn convert_responses_messages(
    model: &Value,
    system_prompt: &str,
    transformed: &[Value],
) -> Vec<Value> {
    let mut messages: Vec<Value> = Vec::new();
    let model_id = model.get("id").and_then(Value::as_str).unwrap_or("");
    let provider = model.get("provider").and_then(Value::as_str).unwrap_or("");
    let reasoning = model.get("reasoning").and_then(Value::as_bool).unwrap_or(false);
    let supports_developer = model
        .get("compat")
        .and_then(|c| c.get("supportsDeveloperRole"))
        .and_then(Value::as_bool)
        .unwrap_or(true);

    if !system_prompt.is_empty() {
        let role = if reasoning && supports_developer { "developer" } else { "system" };
        messages.push(json!({ "role": role, "content": system_prompt }));
    }

    let mut msg_index = 0usize;
    for msg in transformed {
        let role = msg.get("role").and_then(Value::as_str).unwrap_or("");
        match role {
            "user" => {
                let content = msg.get("content");
                match content {
                    Some(Value::String(s)) => {
                        messages.push(json!({
                            "role": "user",
                            "content": [{ "type": "input_text", "text": s }],
                        }));
                    }
                    Some(Value::Array(parts)) => {
                        let content: Vec<Value> = parts
                            .iter()
                            .filter_map(|item| match item.get("type").and_then(Value::as_str) {
                                Some("text") => Some(json!({
                                    "type": "input_text",
                                    "text": item.get("text").and_then(Value::as_str).unwrap_or(""),
                                })),
                                Some("image") => Some(json!({
                                    "type": "input_image",
                                    "detail": "auto",
                                    "image_url": format!(
                                        "data:{};base64,{}",
                                        item.get("mimeType").and_then(Value::as_str).unwrap_or("image/png"),
                                        item.get("data").and_then(Value::as_str).unwrap_or(""),
                                    ),
                                })),
                                _ => None,
                            })
                            .collect();
                        if !content.is_empty() {
                            messages.push(json!({ "role": "user", "content": content }));
                        }
                    }
                    _ => {}
                }
            }
            "assistant" => {
                let content = msg.get("content").and_then(Value::as_array);
                let mut output: Vec<Value> = Vec::new();
                let mut text_block_index = 0usize;
                if let Some(content) = content {
                    for block in content {
                        match block.get("type").and_then(Value::as_str) {
                            Some("thinking") => {
                                if let Some(sig) =
                                    block.get("thinkingSignature").and_then(Value::as_str)
                                {
                                    if let Ok(item) = serde_json::from_str::<Value>(sig) {
                                        output.push(item);
                                    }
                                }
                            }
                            Some("text") => {
                                let text = block.get("text").and_then(Value::as_str).unwrap_or("");
                                let msg_id = if text_block_index == 0 {
                                    format!("msg_pi_{msg_index}")
                                } else {
                                    format!("msg_pi_{msg_index}_{text_block_index}")
                                };
                                text_block_index += 1;
                                output.push(json!({
                                    "type": "message",
                                    "role": "assistant",
                                    "content": [{ "type": "output_text", "text": text, "annotations": [] }],
                                    "status": "completed",
                                    "id": msg_id,
                                }));
                            }
                            Some("toolCall") => {
                                let id = block.get("id").and_then(Value::as_str).unwrap_or("");
                                let mut split = id.splitn(2, '|');
                                let call_id = split.next().unwrap_or("");
                                let item_id = split.next();
                                output.push(json!({
                                    "type": "function_call",
                                    "id": item_id,
                                    "call_id": call_id,
                                    "name": block.get("name").and_then(Value::as_str).unwrap_or(""),
                                    "arguments": block.get("arguments").map(|a| a.to_string()).unwrap_or_else(|| "{}".to_string()),
                                }));
                            }
                            _ => {}
                        }
                    }
                }
                messages.extend(output);
            }
            "toolResult" => {
                let content = msg.get("content").and_then(Value::as_array);
                let text_result = content
                    .map(|parts| {
                        parts
                            .iter()
                            .filter(|c| c.get("type").and_then(Value::as_str) == Some("text"))
                            .filter_map(|c| c.get("text").and_then(Value::as_str))
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default();
                let tool_call_id = msg.get("toolCallId").and_then(Value::as_str).unwrap_or("");
                let call_id = tool_call_id.splitn(2, '|').next().unwrap_or("");
                let output = if text_result.is_empty() {
                    "(no tool output)".to_string()
                } else {
                    text_result
                };
                messages.push(json!({
                    "type": "function_call_output",
                    "call_id": call_id,
                    "output": output,
                }));
            }
            _ => {}
        }
        msg_index += 1;
    }
    let _ = (model_id, provider);
    messages
}

/// Build an OpenAI Responses request body (streaming).
pub fn build_openai_responses_request(
    model: &Value,
    system_prompt: &str,
    messages: &[Value],
    tools: &[Value],
    session_id: Option<&str>,
) -> Value {
    let model_id = model.get("id").and_then(Value::as_str).unwrap_or("");
    let provider = model.get("provider").and_then(Value::as_str).unwrap_or("");
    let api = model.get("api").and_then(Value::as_str).unwrap_or("");
    let supports_image = model
        .get("input")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().any(|v| v.as_str() == Some("image")))
        .unwrap_or(true);

    let transformed = transform_messages(
        messages,
        provider,
        api,
        model_id,
        supports_image,
        normalize_tool_call_id,
    );
    let input = convert_responses_messages(model, system_prompt, &transformed);
    let responses_tools = pi_tools_to_responses(tools);

    let mut body = json!({
        "model": model_id,
        "input": input,
        "stream": true,
        "store": false,
    });
    if let Some(sid) = session_id {
        body["prompt_cache_key"] = json!(sid.chars().take(64).collect::<String>());
    }
    if !responses_tools.is_empty() {
        body["tools"] = Value::Array(responses_tools);
    }
    if model.get("reasoning").and_then(Value::as_bool).unwrap_or(false) {
        let effort = model
            .get("thinkingLevel")
            .and_then(Value::as_str)
            .map(|level| match level {
                "minimal" => "low",
                other => other,
            })
            .unwrap_or("medium");
        body["reasoning"] = json!({ "effort": effort, "summary": "auto" });
        body["include"] = json!(["reasoning.encrypted_content"]);
    }
    body
}

fn map_responses_stop_reason(status: Option<&str>) -> &'static str {
    match status {
        Some("completed") => "stop",
        Some("incomplete") => "length",
        Some("failed") | Some("cancelled") => "error",
        _ => "stop",
    }
}

/// Folds OpenAI Responses SSE events into a `StreamTurn`.
#[derive(Default)]
pub struct ResponsesAccumulator {
    text: String,
    thinking: String,
    thinking_signature: Option<String>,
    tool_calls: Vec<ToolCallSlot>,
    stop_reason: String,
    usage: Option<Value>,
    model: String,
    provider: String,
    api: String,
    events: Vec<Value>,
    started: bool,
    saw_terminal: bool,
    has_tool_call: bool,
}

#[derive(Default, Clone)]
struct ToolCallSlot {
    call_id: String,
    item_id: String,
    name: String,
    partial_json: String,
}

impl ResponsesAccumulator {
    pub fn new(model: &str, provider: &str, api: &str) -> Self {
        ResponsesAccumulator {
            stop_reason: "stop".to_string(),
            model: model.to_string(),
            provider: provider.to_string(),
            api: api.to_string(),
            ..Default::default()
        }
    }

    fn build_message(&self, stop_reason: &str) -> Value {
        let mut content: Vec<Value> = Vec::new();
        if !self.thinking.is_empty() {
            let mut block = json!({ "type": "thinking", "thinking": self.thinking });
            if let Some(sig) = &self.thinking_signature {
                block["thinkingSignature"] = json!(sig);
            }
            content.push(block);
        }
        if !self.text.is_empty() {
            content.push(json!({ "type": "text", "text": self.text }));
        }
        for tc in &self.tool_calls {
            let arguments = serde_json::from_str::<Value>(&tc.partial_json).unwrap_or(json!({}));
            let id = if tc.item_id.is_empty() {
                tc.call_id.clone()
            } else {
                format!("{}|{}", tc.call_id, tc.item_id)
            };
            content.push(json!({
                "type": "toolCall",
                "id": id,
                "name": tc.name,
                "arguments": arguments,
            }));
        }
        json!({
            "role": "assistant",
            "content": content,
            "api": self.api,
            "provider": self.provider,
            "model": self.model,
            "stopReason": stop_reason,
            "usage": self.usage.clone().unwrap_or(json!({
                "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0,
            })),
        })
    }

    fn current_partial(&self) -> Value {
        self.build_message(&self.stop_reason)
    }

    fn ensure_started(&mut self) {
        if !self.started {
            self.started = true;
            self.events.push(json!({ "type": "start", "partial": self.current_partial() }));
        }
    }

    /// Feed one decoded Responses SSE event object.
    pub fn add_event(&mut self, event: &Value) {
        let event_type = event.get("type").and_then(Value::as_str).unwrap_or("");
        match event_type {
            "response.created" => {
                self.ensure_started();
            }
            "response.output_item.added" => {
                self.ensure_started();
                let item = event.get("item");
                let item_type = item.and_then(|i| i.get("type")).and_then(Value::as_str).unwrap_or("");
                if item_type == "function_call" {
                    let call_id = item.and_then(|i| i.get("call_id")).and_then(Value::as_str).unwrap_or("").to_string();
                    let item_id = item.and_then(|i| i.get("id")).and_then(Value::as_str).unwrap_or("").to_string();
                    let name = item.and_then(|i| i.get("name")).and_then(Value::as_str).unwrap_or("").to_string();
                    let args = item.and_then(|i| i.get("arguments")).and_then(Value::as_str).unwrap_or("").to_string();
                    self.tool_calls.push(ToolCallSlot {
                        call_id,
                        item_id,
                        name,
                        partial_json: args,
                    });
                    self.has_tool_call = true;
                }
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                let delta = event.get("delta").and_then(Value::as_str).unwrap_or("");
                self.thinking.push_str(delta);
                self.events.push(json!({
                    "type": "message_update",
                    "assistantMessageEvent": { "type": "thinking_delta", "delta": delta },
                    "partial": self.current_partial(),
                }));
            }
            "response.output_text.delta" | "response.refusal.delta" => {
                let delta = event.get("delta").and_then(Value::as_str).unwrap_or("");
                self.text.push_str(delta);
                self.events.push(json!({
                    "type": "message_update",
                    "assistantMessageEvent": { "type": "text_delta", "delta": delta },
                    "partial": self.current_partial(),
                }));
            }
            "response.function_call_arguments.delta" => {
                let delta = event.get("delta").and_then(Value::as_str).unwrap_or("");
                if let Some(slot) = self.tool_calls.last_mut() {
                    slot.partial_json.push_str(delta);
                }
            }
            "response.function_call_arguments.done" => {
                let arguments = event.get("arguments").and_then(Value::as_str).unwrap_or("");
                if let Some(slot) = self.tool_calls.last_mut() {
                    slot.partial_json = arguments.to_string();
                }
            }
            "response.output_item.done" => {
                let item = event.get("item");
                let item_type = item.and_then(|i| i.get("type")).and_then(Value::as_str).unwrap_or("");
                if item_type == "reasoning" {
                    // Capture the encrypted reasoning item as the thinking signature.
                    if let Some(item) = item {
                        self.thinking_signature = Some(item.to_string());
                        let summary = item
                            .get("summary")
                            .and_then(Value::as_array)
                            .map(|parts| {
                                parts
                                    .iter()
                                    .filter_map(|p| p.get("text").and_then(Value::as_str))
                                    .collect::<Vec<_>>()
                                    .join("\n\n")
                            })
                            .unwrap_or_default();
                        if !summary.is_empty() {
                            self.thinking = summary;
                        }
                    }
                }
            }
            "response.completed" | "response.incomplete" => {
                self.saw_terminal = true;
                let response = event.get("response");
                let status = response.and_then(|r| r.get("status")).and_then(Value::as_str);
                self.stop_reason = map_responses_stop_reason(status).to_string();
                if let Some(usage) = response.and_then(|r| r.get("usage")) {
                    self.merge_usage(usage);
                }
            }
            "response.failed" => {
                self.saw_terminal = true;
                self.stop_reason = "error".to_string();
            }
            _ => {}
        }
    }

    fn merge_usage(&mut self, usage: &Value) {
        let input_tokens = usage.get("input_tokens").and_then(Value::as_u64).unwrap_or(0);
        let output_tokens = usage.get("output_tokens").and_then(Value::as_u64).unwrap_or(0);
        let total_tokens = usage.get("total_tokens").and_then(Value::as_u64).unwrap_or(0);
        let cached = usage
            .pointer("/input_tokens_details/cached_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let cache_write = usage
            .pointer("/input_tokens_details/cache_write_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let input = input_tokens.saturating_sub(cached).saturating_sub(cache_write);
        self.usage = Some(json!({
            "input": input,
            "output": output_tokens,
            "cacheRead": cached,
            "cacheWrite": cache_write,
            "totalTokens": total_tokens,
        }));
    }

    /// Finalize into a `StreamTurn`.
    pub fn finish(mut self) -> StreamTurn {
        self.ensure_started();
        if self.has_tool_call && self.stop_reason == "stop" {
            self.stop_reason = "toolUse".to_string();
        }
        let result = self.build_message(&self.stop_reason);
        self.events.push(json!({ "type": "done" }));
        StreamTurn {
            events: self.events,
            result,
        }
    }
}
