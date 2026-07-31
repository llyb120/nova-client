//! Anthropic Messages provider conversion (`anthropic-messages`), ported from
//! pi-ai `anthropic-messages.js`. Deterministic request building and SSE
//! folding; the HTTP transport lives in `nova_lib`.
//!
//! Scope: standard API-key auth (not OAuth/Claude Code stealth), cache control
//! on system prompt + last user message, thinking/redacted-thinking blocks,
//! tool_use conversion, and the full SSE event set. Deferred tools and copilot
//! dynamic headers are out of scope for the Vega native path.

use serde_json::{json, Map, Value};

use crate::agent::StreamTurn;
use crate::transform::transform_messages;

fn normalize_tool_call_id(id: &str) -> String {
    let sanitized: String = id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .collect();
    sanitized.chars().take(64).collect()
}

/// Convert pi tool definitions to Anthropic `tools`.
pub fn pi_tools_to_anthropic(tools: &[Value]) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| {
            let parameters = tool.get("parameters").cloned().unwrap_or(json!({}));
            let properties = parameters.get("properties").cloned().unwrap_or(json!({}));
            let required = parameters.get("required").cloned().unwrap_or(json!([]));
            json!({
                "name": tool.get("name").and_then(Value::as_str).unwrap_or(""),
                "description": tool.get("description").and_then(Value::as_str).unwrap_or(""),
                "input_schema": {
                    "type": "object",
                    "properties": properties,
                    "required": required,
                },
            })
        })
        .collect()
}

fn convert_content_blocks(content: &[Value]) -> Value {
    let has_images = content
        .iter()
        .any(|c| c.get("type").and_then(Value::as_str) == Some("image"));
    if !has_images {
        let text: Vec<&str> = content
            .iter()
            .filter_map(|c| c.get("text").and_then(Value::as_str))
            .collect();
        return Value::String(text.join("\n"));
    }
    let mut blocks: Vec<Value> = Vec::new();
    for block in content {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                blocks.push(json!({ "type": "text", "text": block.get("text").and_then(Value::as_str).unwrap_or("") }));
            }
            Some("image") => {
                blocks.push(json!({
                    "type": "image",
                    "source": {
                        "type": "base64",
                        "media_type": block.get("mimeType").and_then(Value::as_str).unwrap_or("image/png"),
                        "data": block.get("data").and_then(Value::as_str).unwrap_or(""),
                    },
                }));
            }
            _ => {}
        }
    }
    if !blocks.iter().any(|b| b.get("type").and_then(Value::as_str) == Some("text")) {
        blocks.insert(0, json!({ "type": "text", "text": "(see attached image)" }));
    }
    Value::Array(blocks)
}

/// Convert transformed pi messages to Anthropic `messages`.
fn convert_messages(transformed: &[Value], cache_control: Option<&Value>) -> Vec<Value> {
    let mut params: Vec<Value> = Vec::new();
    let mut i = 0;
    while i < transformed.len() {
        let msg = &transformed[i];
        let role = msg.get("role").and_then(Value::as_str).unwrap_or("");
        match role {
            "user" => {
                let content = msg.get("content");
                match content {
                    Some(Value::String(s)) => {
                        if !s.trim().is_empty() {
                            params.push(json!({ "role": "user", "content": s }));
                        }
                    }
                    Some(Value::Array(parts)) => {
                        let blocks: Vec<Value> = parts
                            .iter()
                            .filter_map(|item| match item.get("type").and_then(Value::as_str) {
                                Some("text") => {
                                    let text = item.get("text").and_then(Value::as_str).unwrap_or("");
                                    if text.trim().is_empty() {
                                        None
                                    } else {
                                        Some(json!({ "type": "text", "text": text }))
                                    }
                                }
                                Some("image") => Some(json!({
                                    "type": "image",
                                    "source": {
                                        "type": "base64",
                                        "media_type": item.get("mimeType").and_then(Value::as_str).unwrap_or("image/png"),
                                        "data": item.get("data").and_then(Value::as_str).unwrap_or(""),
                                    },
                                })),
                                _ => None,
                            })
                            .collect();
                        if !blocks.is_empty() {
                            params.push(json!({ "role": "user", "content": blocks }));
                        }
                    }
                    _ => {}
                }
            }
            "assistant" => {
                let content = msg.get("content").and_then(Value::as_array);
                let mut blocks: Vec<Value> = Vec::new();
                if let Some(content) = content {
                    for block in content {
                        match block.get("type").and_then(Value::as_str) {
                            Some("text") => {
                                let text = block.get("text").and_then(Value::as_str).unwrap_or("");
                                if !text.trim().is_empty() {
                                    blocks.push(json!({ "type": "text", "text": text }));
                                }
                            }
                            Some("thinking") => {
                                let redacted =
                                    block.get("redacted").and_then(Value::as_bool).unwrap_or(false);
                                if redacted {
                                    blocks.push(json!({
                                        "type": "redacted_thinking",
                                        "data": block.get("thinkingSignature").and_then(Value::as_str).unwrap_or(""),
                                    }));
                                    continue;
                                }
                                let thinking =
                                    block.get("thinking").and_then(Value::as_str).unwrap_or("");
                                let signature = block
                                    .get("thinkingSignature")
                                    .and_then(Value::as_str)
                                    .unwrap_or("");
                                if thinking.trim().is_empty() && signature.trim().is_empty() {
                                    continue;
                                }
                                if signature.trim().is_empty() {
                                    blocks.push(json!({ "type": "text", "text": thinking }));
                                } else {
                                    blocks.push(json!({
                                        "type": "thinking",
                                        "thinking": thinking,
                                        "signature": signature,
                                    }));
                                }
                            }
                            Some("toolCall") => {
                                blocks.push(json!({
                                    "type": "tool_use",
                                    "id": block.get("id").and_then(Value::as_str).unwrap_or(""),
                                    "name": block.get("name").and_then(Value::as_str).unwrap_or(""),
                                    "input": block.get("arguments").cloned().unwrap_or(json!({})),
                                }));
                            }
                            _ => {}
                        }
                    }
                }
                if !blocks.is_empty() {
                    params.push(json!({ "role": "assistant", "content": blocks }));
                }
            }
            "toolResult" => {
                // Merge consecutive toolResult messages into one user turn.
                let mut tool_results: Vec<Value> = Vec::new();
                while i < transformed.len()
                    && transformed[i].get("role").and_then(Value::as_str) == Some("toolResult")
                {
                    let tr = &transformed[i];
                    let content = tr.get("content").and_then(Value::as_array);
                    let converted = content
                        .map(|parts| convert_content_blocks(parts))
                        .unwrap_or(Value::String(String::new()));
                    let is_error = tr.get("isError").and_then(Value::as_bool).unwrap_or(false);
                    tool_results.push(json!({
                        "type": "tool_result",
                        "tool_use_id": tr.get("toolCallId").and_then(Value::as_str).unwrap_or(""),
                        "content": converted,
                        "is_error": is_error,
                    }));
                    i += 1;
                }
                i -= 1; // outer loop will increment
                params.push(json!({ "role": "user", "content": tool_results }));
            }
            _ => {}
        }
        i += 1;
    }

    // Cache control on the last user message.
    if let Some(cc) = cache_control {
        if let Some(last) = params.last_mut() {
            if last.get("role").and_then(Value::as_str) == Some("user") {
                match last.get("content").cloned() {
                    Some(Value::Array(mut blocks)) => {
                        if let Some(last_block) = blocks.last_mut() {
                            let t = last_block.get("type").and_then(Value::as_str).unwrap_or("");
                            if matches!(t, "text" | "image" | "tool_result") {
                                last_block["cache_control"] = cc.clone();
                            }
                        }
                        last["content"] = Value::Array(blocks);
                    }
                    Some(Value::String(s)) => {
                        last["content"] = json!([{ "type": "text", "text": s, "cache_control": cc }]);
                    }
                    _ => {}
                }
            }
        }
    }
    params
}

/// Build an Anthropic Messages request body (streaming).
pub fn build_anthropic_request(
    model: &Value,
    system_prompt: &str,
    messages: &[Value],
    tools: &[Value],
    session_id: Option<&str>,
    max_tokens: u64,
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

    let cache_control = json!({ "type": "ephemeral" });
    let anthropic_messages = convert_messages(&transformed, Some(&cache_control));
    let anthropic_tools = pi_tools_to_anthropic(tools);

    let mut body = Map::new();
    body.insert("model".to_string(), json!(model_id));
    body.insert("stream".to_string(), json!(true));
    body.insert(
        "max_tokens".to_string(),
        model
            .get("maxTokens")
            .cloned()
            .unwrap_or_else(|| json!(max_tokens)),
    );
    if !system_prompt.is_empty() {
        body.insert(
            "system".to_string(),
            json!([{ "type": "text", "text": system_prompt, "cache_control": cache_control }]),
        );
    }
    body.insert("messages".to_string(), Value::Array(anthropic_messages));
    if !anthropic_tools.is_empty() {
        body.insert("tools".to_string(), Value::Array(anthropic_tools));
    }
    // Thinking config for reasoning models.
    if model.get("reasoning").and_then(Value::as_bool).unwrap_or(false) {
        let force_adaptive = model
            .get("compat")
            .and_then(|c| c.get("forceAdaptiveThinking"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let level = model
            .get("thinkingLevel")
            .and_then(Value::as_str)
            .unwrap_or("high");
        if force_adaptive {
            let effort = match level {
                "minimal" | "low" => "low",
                "medium" => "medium",
                _ => "high",
            };
            body.insert("thinking".to_string(), json!({ "type": "adaptive", "display": "summarized" }));
            body.insert("output_config".to_string(), json!({ "effort": effort }));
        } else {
            let budget = match level {
                "minimal" => 1024,
                "low" => 2048,
                "medium" => 8192,
                _ => 32768,
            };
            body.insert(
                "thinking".to_string(),
                json!({ "type": "enabled", "budget_tokens": budget, "display": "summarized" }),
            );
        }
    }
    if let Some(sid) = session_id {
        body.insert("metadata".to_string(), json!({ "user_id": sid }));
    }
    Value::Object(body)
}

/// Map an Anthropic `stop_reason` to a pi `stopReason`.
pub fn map_anthropic_stop_reason(reason: &str) -> (&'static str, Option<String>) {
    match reason {
        "end_turn" => ("stop", None),
        "max_tokens" => ("length", None),
        "tool_use" => ("toolUse", None),
        "refusal" => ("error", Some("The model refused to complete the request".to_string())),
        "pause_turn" | "stop_sequence" => ("stop", None),
        "sensitive" => ("error", None),
        _ => ("error", Some(format!("Unhandled stop reason: {reason}"))),
    }
}

/// Folds Anthropic Messages SSE events into a `StreamTurn`. Feed each decoded
/// event JSON object (with a `type` field) to `add_event`, then call `finish`.
#[derive(Default)]
pub struct AnthropicAccumulator {
    text: String,
    thinking: String,
    thinking_signature: String,
    tool_calls: Vec<ToolCallAcc>,
    /// Maps an SSE block index to a `tool_calls` position.
    tool_index: Vec<Option<usize>>,
    stop_reason: String,
    error_message: Option<String>,
    usage: Option<Value>,
    model: String,
    provider: String,
    api: String,
    events: Vec<Value>,
    started: bool,
    // Per-block scratch keyed by event `index`.
    block_kinds: Vec<String>,
}

#[derive(Default, Clone)]
struct ToolCallAcc {
    id: String,
    name: String,
    partial_json: String,
}

impl AnthropicAccumulator {
    pub fn new(model: &str, provider: &str, api: &str) -> Self {
        AnthropicAccumulator {
            stop_reason: "stop".to_string(),
            model: model.to_string(),
            provider: provider.to_string(),
            api: api.to_string(),
            ..Default::default()
        }
    }

    fn build_message(&self, stop_reason: &str) -> Value {
        let mut content: Vec<Value> = Vec::new();
        if !self.thinking.is_empty() || !self.thinking_signature.is_empty() {
            content.push(json!({
                "type": "thinking",
                "thinking": self.thinking,
                "thinkingSignature": self.thinking_signature,
            }));
        }
        if !self.text.is_empty() {
            content.push(json!({ "type": "text", "text": self.text }));
        }
        for tc in &self.tool_calls {
            let arguments = serde_json::from_str::<Value>(&tc.partial_json).unwrap_or(json!({}));
            content.push(json!({
                "type": "toolCall",
                "id": tc.id,
                "name": tc.name,
                "arguments": arguments,
            }));
        }
        let mut message = json!({
            "role": "assistant",
            "content": content,
            "api": self.api,
            "provider": self.provider,
            "model": self.model,
            "stopReason": stop_reason,
            "usage": self.usage.clone().unwrap_or(json!({
                "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0,
            })),
        });
        if let Some(err) = &self.error_message {
            message["errorMessage"] = json!(err);
        }
        message
    }

    fn current_partial(&self) -> Value {
        self.build_message(&self.stop_reason)
    }

    /// Feed one decoded Anthropic SSE event object.
    pub fn add_event(&mut self, event: &Value) {
        let event_type = event.get("type").and_then(Value::as_str).unwrap_or("");
        match event_type {
            "message_start" => {
                if !self.started {
                    self.started = true;
                    self.events.push(json!({ "type": "start", "partial": self.current_partial() }));
                }
                if let Some(usage) = event.pointer("/message/usage") {
                    self.merge_usage(usage);
                }
            }
            "content_block_start" => {
                let index = event.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                let block_type = event
                    .pointer("/content_block/type")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                while self.block_kinds.len() <= index {
                    self.block_kinds.push(String::new());
                }
                match block_type {
                    "text" => self.block_kinds[index] = "text".to_string(),
                    "thinking" => self.block_kinds[index] = "thinking".to_string(),
                    "redacted_thinking" => {
                        self.block_kinds[index] = "thinking".to_string();
                        let data = event
                            .pointer("/content_block/data")
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        self.thinking = "[Reasoning redacted]".to_string();
                        self.thinking_signature = data.to_string();
                    }
                    "tool_use" => {
                        self.block_kinds[index] = "tool_use".to_string();
                        let id = event
                            .pointer("/content_block/id")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        let name = event
                            .pointer("/content_block/name")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        let pos = self.tool_calls.len();
                        self.tool_calls.push(ToolCallAcc {
                            id,
                            name,
                            partial_json: String::new(),
                        });
                        while self.tool_index.len() <= index {
                            self.tool_index.push(None);
                        }
                        self.tool_index[index] = Some(pos);
                    }
                    _ => {}
                }
            }
            "content_block_delta" => {
                let index = event.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                let delta_type = event.pointer("/delta/type").and_then(Value::as_str).unwrap_or("");
                match delta_type {
                    "text_delta" => {
                        let delta = event.pointer("/delta/text").and_then(Value::as_str).unwrap_or("");
                        self.text.push_str(delta);
                        self.events.push(json!({
                            "type": "message_update",
                            "assistantMessageEvent": { "type": "text_delta", "delta": delta },
                            "partial": self.current_partial(),
                        }));
                    }
                    "thinking_delta" => {
                        let delta =
                            event.pointer("/delta/thinking").and_then(Value::as_str).unwrap_or("");
                        self.thinking.push_str(delta);
                        self.events.push(json!({
                            "type": "message_update",
                            "assistantMessageEvent": { "type": "thinking_delta", "delta": delta },
                            "partial": self.current_partial(),
                        }));
                    }
                    "input_json_delta" => {
                        let delta = event
                            .pointer("/delta/partial_json")
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        if let Some(Some(pos)) = self.tool_index.get(index) {
                            self.tool_calls[*pos].partial_json.push_str(delta);
                        }
                    }
                    "signature_delta" => {
                        let signature =
                            event.pointer("/delta/signature").and_then(Value::as_str).unwrap_or("");
                        self.thinking_signature.push_str(signature);
                    }
                    _ => {}
                }
            }
            "message_delta" => {
                if let Some(reason) = event.pointer("/delta/stop_reason").and_then(Value::as_str) {
                    let (mapped, err) = map_anthropic_stop_reason(reason);
                    self.stop_reason = mapped.to_string();
                    if let Some(err) = err {
                        self.error_message = Some(err);
                    }
                }
                if let Some(usage) = event.get("usage") {
                    self.merge_usage(usage);
                }
            }
            _ => {}
        }
    }

    fn merge_usage(&mut self, usage: &Value) {
        // Only override fields present (non-null) in this event, preserving
        // earlier values (e.g. input_tokens from message_start when a proxy
        // omits it in message_delta) — matching the node accumulator.
        let prev_input = self.usage.as_ref().and_then(|u| u.get("input")).and_then(Value::as_u64).unwrap_or(0);
        let prev_output = self.usage.as_ref().and_then(|u| u.get("output")).and_then(Value::as_u64).unwrap_or(0);
        let prev_cache_read = self.usage.as_ref().and_then(|u| u.get("cacheRead")).and_then(Value::as_u64).unwrap_or(0);
        let prev_cache_write = self.usage.as_ref().and_then(|u| u.get("cacheWrite")).and_then(Value::as_u64).unwrap_or(0);
        let input = usage.get("input_tokens").and_then(Value::as_u64).unwrap_or(prev_input);
        let output = usage.get("output_tokens").and_then(Value::as_u64).unwrap_or(prev_output);
        let cache_read = usage.get("cache_read_input_tokens").and_then(Value::as_u64).unwrap_or(prev_cache_read);
        let cache_write = usage.get("cache_creation_input_tokens").and_then(Value::as_u64).unwrap_or(prev_cache_write);
        let total = input + output + cache_read + cache_write;
        self.usage = Some(json!({
            "input": input,
            "output": output,
            "cacheRead": cache_read,
            "cacheWrite": cache_write,
            "totalTokens": total,
        }));
    }

    /// Finalize into a `StreamTurn`.
    pub fn finish(mut self) -> StreamTurn {
        if !self.started {
            self.started = true;
            self.events.push(json!({ "type": "start", "partial": self.current_partial() }));
        }
        let result = self.build_message(&self.stop_reason);
        self.events.push(json!({ "type": "done" }));
        StreamTurn {
            events: self.events,
            result,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_text_tool_and_usage() {
        let mut acc = AnthropicAccumulator::new("claude-x", "anthropic", "anthropic-messages");
        acc.add_event(&json!({ "type": "message_start", "message": { "usage": { "input_tokens": 10, "output_tokens": 1 } } }));
        acc.add_event(&json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "text" } }));
        acc.add_event(&json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": "Hello" } }));
        acc.add_event(&json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": " world" } }));
        acc.add_event(&json!({ "type": "content_block_stop", "index": 0 }));
        acc.add_event(&json!({ "type": "content_block_start", "index": 1, "content_block": { "type": "tool_use", "id": "tu_1", "name": "bash" } }));
        acc.add_event(&json!({ "type": "content_block_delta", "index": 1, "delta": { "type": "input_json_delta", "partial_json": "{\"command\":" } }));
        acc.add_event(&json!({ "type": "content_block_delta", "index": 1, "delta": { "type": "input_json_delta", "partial_json": "\"ls\"}" } }));
        acc.add_event(&json!({ "type": "content_block_stop", "index": 1 }));
        acc.add_event(&json!({ "type": "message_delta", "delta": { "stop_reason": "tool_use" }, "usage": { "output_tokens": 5 } }));
        acc.add_event(&json!({ "type": "message_stop" }));
        let turn = acc.finish();

        assert_eq!(turn.result["stopReason"], "toolUse");
        let content = turn.result["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "Hello world");
        assert_eq!(content[1]["type"], "toolCall");
        assert_eq!(content[1]["name"], "bash");
        assert_eq!(content[1]["arguments"]["command"], "ls");
        // usage: input 10, output 5 (message_delta overrides), total = 10+5.
        assert_eq!(turn.result["usage"]["input"], 10);
        assert_eq!(turn.result["usage"]["output"], 5);
        assert_eq!(turn.result["usage"]["totalTokens"], 15);
        // Stream events include a start, text deltas, and a done.
        assert!(turn.events.iter().any(|e| e["type"] == "start"));
        assert!(turn.events.iter().any(|e| e["type"] == "done"));
    }

    #[test]
    fn maps_stop_reasons() {
        assert_eq!(map_anthropic_stop_reason("end_turn"), ("stop", None));
        assert_eq!(map_anthropic_stop_reason("max_tokens"), ("length", None));
        assert_eq!(map_anthropic_stop_reason("tool_use"), ("toolUse", None));
        assert_eq!(map_anthropic_stop_reason("refusal").0, "error");
    }

    #[test]
    fn request_has_system_cache_and_tools() {
        let model = json!({ "id": "claude-x", "provider": "anthropic", "api": "anthropic-messages", "reasoning": false });
        let tools = vec![json!({ "name": "bash", "description": "run", "parameters": { "type": "object", "properties": { "command": { "type": "string" } }, "required": ["command"] } })];
        let body = build_anthropic_request(&model, "You are Vega.", &[], &tools, None, 4096);
        assert_eq!(body["model"], "claude-x");
        assert_eq!(body["max_tokens"], 4096);
        assert_eq!(body["system"][0]["text"], "You are Vega.");
        assert!(body["system"][0]["cache_control"].is_object());
        assert_eq!(body["tools"][0]["name"], "bash");
        assert_eq!(body["tools"][0]["input_schema"]["required"][0], "command");
    }
}
