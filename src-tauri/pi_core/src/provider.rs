//! Provider message conversion for the OpenAI Chat Completions protocol
//! (`openai-completions`), the dominant Vega provider API.
//!
//! The conversions here are deterministic and unit-tested without a live
//! provider: building the request payload from pi messages/tools, and folding
//! streamed SSE chunks into a `StreamTurn`. The actual HTTP transport lives in
//! `nova_lib` (the excluded, unverifiable LLM boundary).

use std::collections::BTreeMap;

use serde_json::{json, Map, Value};

use crate::agent::StreamTurn;

/// Convert one pi message to an OpenAI Chat Completions message.
pub fn pi_message_to_openai(message: &Value) -> Option<Value> {
    match message.get("role").and_then(Value::as_str) {
        Some("user") => {
            let content = message.get("content").and_then(Value::as_array);
            let mut text_parts: Vec<String> = Vec::new();
            let mut parts: Vec<Value> = Vec::new();
            let mut has_non_text = false;
            if let Some(content) = content {
                for part in content {
                    match part.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            let text = part.get("text").and_then(Value::as_str).unwrap_or("");
                            text_parts.push(text.to_string());
                            parts.push(json!({ "type": "text", "text": text }));
                        }
                        Some("image") => {
                            has_non_text = true;
                            let data = part.get("data").and_then(Value::as_str).unwrap_or("");
                            let mime = part.get("mimeType").and_then(Value::as_str).unwrap_or("image/png");
                            parts.push(json!({
                                "type": "image_url",
                                "image_url": { "url": format!("data:{mime};base64,{data}") },
                            }));
                        }
                        _ => {}
                    }
                }
            }
            let content_value = if has_non_text {
                Value::Array(parts)
            } else {
                Value::String(text_parts.join("\n\n"))
            };
            Some(json!({ "role": "user", "content": content_value }))
        }
        Some("assistant") => {
            let content = message.get("content").and_then(Value::as_array);
            let mut text_parts: Vec<String> = Vec::new();
            let mut tool_calls: Vec<Value> = Vec::new();
            if let Some(content) = content {
                for part in content {
                    match part.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            text_parts.push(part.get("text").and_then(Value::as_str).unwrap_or("").to_string());
                        }
                        Some("toolCall") => {
                            let arguments = part.get("arguments").cloned().unwrap_or(json!({}));
                            tool_calls.push(json!({
                                "id": part.get("id").and_then(Value::as_str).unwrap_or(""),
                                "type": "function",
                                "function": {
                                    "name": part.get("name").and_then(Value::as_str).unwrap_or(""),
                                    "arguments": arguments.to_string(),
                                },
                            }));
                        }
                        _ => {}
                    }
                }
            }
            let mut map = Map::new();
            map.insert("role".to_string(), json!("assistant"));
            let text = text_parts.join("");
            map.insert("content".to_string(), if text.is_empty() { Value::Null } else { Value::String(text) });
            if !tool_calls.is_empty() {
                map.insert("tool_calls".to_string(), Value::Array(tool_calls));
            }
            Some(Value::Object(map))
        }
        Some("toolResult") => {
            let content = message.get("content").and_then(Value::as_array);
            let text = content
                .map(|parts| {
                    parts
                        .iter()
                        .filter_map(|part| part.get("text").and_then(Value::as_str))
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();
            Some(json!({
                "role": "tool",
                "tool_call_id": message.get("toolCallId").and_then(Value::as_str).unwrap_or(""),
                "content": text,
            }))
        }
        _ => None,
    }
}

/// Convert pi tool definitions to OpenAI function tools.
pub fn pi_tools_to_openai(tools: &[Value]) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| {
            json!({
                "type": "function",
                "function": {
                    "name": tool.get("name").and_then(Value::as_str).unwrap_or(""),
                    "description": tool.get("description").and_then(Value::as_str).unwrap_or(""),
                    "parameters": tool.get("parameters").cloned().unwrap_or(json!({ "type": "object", "properties": {} })),
                },
            })
        })
        .collect()
}

/// Build an OpenAI Chat Completions request body (streaming).
pub fn build_openai_chat_request(
    model_id: &str,
    system_prompt: &str,
    messages: &[Value],
    tools: &[Value],
) -> Value {
    let mut chat_messages: Vec<Value> = Vec::new();
    if !system_prompt.is_empty() {
        chat_messages.push(json!({ "role": "system", "content": system_prompt }));
    }
    for message in messages {
        if let Some(converted) = pi_message_to_openai(message) {
            chat_messages.push(converted);
        }
    }
    let openai_tools = pi_tools_to_openai(tools);
    let mut body = json!({
        "model": model_id,
        "messages": chat_messages,
        "stream": true,
        "stream_options": { "include_usage": true },
    });
    if !openai_tools.is_empty() {
        body["tools"] = Value::Array(openai_tools);
    }
    body
}

/// Map an OpenAI `finish_reason` to a pi `stopReason`.
pub fn map_finish_reason(finish_reason: Option<&str>) -> &'static str {
    match finish_reason {
        Some("length") => "length",
        Some("tool_calls") => "tool_calls",
        Some("content_filter") => "end_turn",
        _ => "end_turn",
    }
}

#[derive(Default)]
struct ToolCallAcc {
    id: String,
    name: String,
    arguments: String,
}

/// Folds OpenAI Chat Completions SSE chunks into a `StreamTurn`. Feed each
/// decoded `data:` JSON object to `add_chunk`, then call `finish`.
#[derive(Default)]
pub struct OpenAiChatAccumulator {
    text: String,
    tool_calls: BTreeMap<usize, ToolCallAcc>,
    finish_reason: Option<String>,
    usage: Option<Value>,
    model: String,
    provider: String,
    api: String,
    events: Vec<Value>,
    started: bool,
}

impl OpenAiChatAccumulator {
    pub fn new(model: &str, provider: &str, api: &str) -> Self {
        OpenAiChatAccumulator {
            model: model.to_string(),
            provider: provider.to_string(),
            api: api.to_string(),
            ..Default::default()
        }
    }

    fn current_partial(&self) -> Value {
        self.build_message("end_turn")
    }

    fn build_message(&self, stop_reason: &str) -> Value {
        let mut content: Vec<Value> = Vec::new();
        if !self.text.is_empty() {
            content.push(json!({ "type": "text", "text": self.text }));
        }
        for (_, acc) in &self.tool_calls {
            let arguments: Value = serde_json::from_str(&acc.arguments).unwrap_or(json!({}));
            content.push(json!({
                "type": "toolCall",
                "id": acc.id,
                "name": acc.name,
                "arguments": arguments,
            }));
        }
        json!({
            "role": "assistant",
            "content": content,
            "api": self.api,
            "provider": self.provider,
            "model": self.model,
            "usage": self.pi_usage(),
            "stopReason": stop_reason,
        })
    }

    fn pi_usage(&self) -> Value {
        let input = self.usage.as_ref().and_then(|u| u.get("prompt_tokens")).and_then(Value::as_u64).unwrap_or(0);
        let output = self.usage.as_ref().and_then(|u| u.get("completion_tokens")).and_then(Value::as_u64).unwrap_or(0);
        let cache_read = self
            .usage
            .as_ref()
            .and_then(|u| u.get("prompt_tokens_details"))
            .and_then(|d| d.get("cached_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let total = self.usage.as_ref().and_then(|u| u.get("total_tokens")).and_then(Value::as_u64).unwrap_or(input + output);
        json!({
            "input": input,
            "output": output,
            "cacheRead": cache_read,
            "cacheWrite": 0,
            "totalTokens": total,
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 },
        })
    }

    /// Process one decoded SSE `data:` object.
    pub fn add_chunk(&mut self, chunk: &Value) {
        if let Some(choices) = chunk.get("choices").and_then(Value::as_array) {
            for choice in choices {
                let delta = choice.get("delta");
                if let Some(content) = delta.and_then(|d| d.get("content")).and_then(Value::as_str) {
                    if !self.started {
                        self.started = true;
                        self.events.push(json!({ "type": "start", "partial": self.current_partial() }));
                    }
                    if !content.is_empty() {
                        self.text.push_str(content);
                        self.events.push(json!({
                            "type": "text_delta",
                            "delta": content,
                            "partial": self.current_partial(),
                        }));
                    }
                }
                if let Some(tool_calls) = delta.and_then(|d| d.get("tool_calls")).and_then(Value::as_array) {
                    if !self.started {
                        self.started = true;
                        self.events.push(json!({ "type": "start", "partial": self.current_partial() }));
                    }
                    for tool_call in tool_calls {
                        let index = tool_call.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                        let acc = self.tool_calls.entry(index).or_default();
                        if let Some(id) = tool_call.get("id").and_then(Value::as_str) {
                            acc.id = id.to_string();
                        }
                        if let Some(function) = tool_call.get("function") {
                            if let Some(name) = function.get("name").and_then(Value::as_str) {
                                acc.name.push_str(name);
                            }
                            if let Some(arguments) = function.get("arguments").and_then(Value::as_str) {
                                acc.arguments.push_str(arguments);
                            }
                        }
                    }
                }
                if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
                    self.finish_reason = Some(reason.to_string());
                }
            }
        }
        if let Some(usage) = chunk.get("usage") {
            if !usage.is_null() {
                self.usage = Some(usage.clone());
            }
        }
    }

    /// Finalize into a `StreamTurn`.
    pub fn finish(mut self) -> StreamTurn {
        if !self.started {
            self.events.push(json!({ "type": "start", "partial": self.current_partial() }));
        }
        let stop_reason = map_finish_reason(self.finish_reason.as_deref());
        let result = self.build_message(stop_reason);
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
    fn converts_user_text_message() {
        let message = json!({ "role": "user", "content": [{ "type": "text", "text": "hello" }] });
        assert_eq!(
            pi_message_to_openai(&message).unwrap(),
            json!({ "role": "user", "content": "hello" })
        );
    }

    #[test]
    fn converts_assistant_tool_call() {
        let message = json!({
            "role": "assistant",
            "content": [{ "type": "toolCall", "id": "c1", "name": "bash", "arguments": { "command": "ls" } }],
        });
        let converted = pi_message_to_openai(&message).unwrap();
        assert_eq!(converted["role"], json!("assistant"));
        assert!(converted["content"].is_null());
        assert_eq!(converted["tool_calls"][0]["function"]["name"], json!("bash"));
        let expected_args = json!({ "command": "ls" }).to_string();
        assert_eq!(
            converted["tool_calls"][0]["function"]["arguments"],
            json!(expected_args)
        );
    }

    #[test]
    fn converts_tool_result() {
        let message = json!({ "role": "toolResult", "toolCallId": "c1", "content": [{ "type": "text", "text": "ok" }] });
        assert_eq!(
            pi_message_to_openai(&message).unwrap(),
            json!({ "role": "tool", "tool_call_id": "c1", "content": "ok" })
        );
    }

    #[test]
    fn builds_request_with_system_and_tools() {
        let messages = vec![json!({ "role": "user", "content": [{ "type": "text", "text": "hi" }] })];
        let tools = vec![json!({ "name": "bash", "description": "run", "parameters": { "type": "object" } })];
        let request = build_openai_chat_request("gpt-x", "You are Vega.", &messages, &tools);
        assert_eq!(request["model"], json!("gpt-x"));
        assert_eq!(request["messages"][0], json!({ "role": "system", "content": "You are Vega." }));
        assert_eq!(request["messages"][1]["role"], json!("user"));
        assert_eq!(request["stream"], json!(true));
        assert_eq!(request["tools"][0]["function"]["name"], json!("bash"));
    }

    #[test]
    fn accumulates_text_stream() {
        let mut acc = OpenAiChatAccumulator::new("gpt-x", "openai", "openai-completions");
        acc.add_chunk(&json!({ "choices": [{ "delta": { "role": "assistant", "content": "" }, "finish_reason": null }] }));
        acc.add_chunk(&json!({ "choices": [{ "delta": { "content": "Hello" }, "finish_reason": null }] }));
        acc.add_chunk(&json!({ "choices": [{ "delta": { "content": " world" }, "finish_reason": null }] }));
        acc.add_chunk(&json!({ "choices": [{ "delta": {}, "finish_reason": "stop" }], "usage": { "prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15 } }));
        let turn = acc.finish();
        assert_eq!(turn.result["content"][0]["text"], json!("Hello world"));
        assert_eq!(turn.result["stopReason"], json!("end_turn"));
        assert_eq!(turn.result["usage"]["input"], json!(10));
        assert_eq!(turn.result["usage"]["output"], json!(5));
        // events: start + 2 text_delta + done
        let types: Vec<&str> = turn.events.iter().filter_map(|e| e["type"].as_str()).collect();
        assert_eq!(types, vec!["start", "text_delta", "text_delta", "done"]);
    }

    #[test]
    fn accumulates_tool_call_stream() {
        let mut acc = OpenAiChatAccumulator::new("gpt-x", "openai", "openai-completions");
        acc.add_chunk(&json!({ "choices": [{ "delta": { "tool_calls": [{ "index": 0, "id": "call_1", "function": { "name": "bash", "arguments": "" } }] }, "finish_reason": null }] }));
        acc.add_chunk(&json!({ "choices": [{ "delta": { "tool_calls": [{ "index": 0, "function": { "arguments": "{\"command\":" } }] }, "finish_reason": null }] }));
        acc.add_chunk(&json!({ "choices": [{ "delta": { "tool_calls": [{ "index": 0, "function": { "arguments": "\"ls\"}" } }] }, "finish_reason": null }] }));
        acc.add_chunk(&json!({ "choices": [{ "delta": {}, "finish_reason": "tool_calls" }] }));
        let turn = acc.finish();
        assert_eq!(turn.result["stopReason"], json!("tool_calls"));
        let tool_call = &turn.result["content"][0];
        assert_eq!(tool_call["type"], json!("toolCall"));
        assert_eq!(tool_call["name"], json!("bash"));
        assert_eq!(tool_call["arguments"], json!({ "command": "ls" }));
    }

    #[test]
    fn maps_finish_reasons() {
        assert_eq!(map_finish_reason(Some("stop")), "end_turn");
        assert_eq!(map_finish_reason(Some("length")), "length");
        assert_eq!(map_finish_reason(Some("tool_calls")), "tool_calls");
        assert_eq!(map_finish_reason(None), "end_turn");
    }
}
