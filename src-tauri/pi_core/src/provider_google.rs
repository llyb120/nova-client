//! Google Generative AI provider conversion (`google-generative-ai`), ported
//! from pi-ai `google-generative-ai.js` + `google-shared.js`. Deterministic
//! request building and chunk folding; the HTTP transport lives in `nova_lib`.
//!
//! Scope: standard API-key auth, contents/parts with text/inlineData,
//! functionCall/functionResponse, thought parts with signatures, thinking
//! config, and the Gemini streaming chunk shape. Vertex AI and Cloud Code
//! Assist specifics are out of scope for the Vega native path.

use serde_json::{json, Value};

use crate::agent::StreamTurn;
use crate::transform::transform_messages;

fn normalize_tool_call_id(id: &str) -> String {
    let sanitized: String = id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .collect();
    sanitized.chars().take(64).collect()
}

fn requires_tool_call_id(model_id: &str) -> bool {
    model_id.starts_with("claude-") || model_id.starts_with("gpt-oss-")
}

/// Convert pi tool definitions to Gemini `tools` (functionDeclarations).
pub fn pi_tools_to_google(tools: &[Value]) -> Value {
    let declarations: Vec<Value> = tools
        .iter()
        .map(|tool| {
            json!({
                "name": tool.get("name").and_then(Value::as_str).unwrap_or(""),
                "description": tool.get("description").and_then(Value::as_str).unwrap_or(""),
                "parametersJsonSchema": tool.get("parameters").cloned().unwrap_or(json!({ "type": "object", "properties": {} })),
            })
        })
        .collect();
    json!([{ "functionDeclarations": declarations }])
}

fn convert_google_messages(
    model: &Value,
    system_prompt: &str,
    transformed: &[Value],
) -> (Option<String>, Vec<Value>) {
    let model_id = model.get("id").and_then(Value::as_str).unwrap_or("");
    let provider = model.get("provider").and_then(Value::as_str).unwrap_or("");
    let include_id = requires_tool_call_id(model_id);

    let mut contents: Vec<Value> = Vec::new();
    for msg in transformed {
        let role = msg.get("role").and_then(Value::as_str).unwrap_or("");
        match role {
            "user" => {
                let content = msg.get("content");
                match content {
                    Some(Value::String(s)) => {
                        contents.push(json!({ "role": "user", "parts": [{ "text": s }] }));
                    }
                    Some(Value::Array(parts)) => {
                        let gemini_parts: Vec<Value> = parts
                            .iter()
                            .filter_map(|item| match item.get("type").and_then(Value::as_str) {
                                Some("text") => {
                                    Some(json!({ "text": item.get("text").and_then(Value::as_str).unwrap_or("") }))
                                }
                                Some("image") => Some(json!({
                                    "inlineData": {
                                        "mimeType": item.get("mimeType").and_then(Value::as_str).unwrap_or("image/png"),
                                        "data": item.get("data").and_then(Value::as_str).unwrap_or(""),
                                    },
                                })),
                                _ => None,
                            })
                            .collect();
                        if !gemini_parts.is_empty() {
                            contents.push(json!({ "role": "user", "parts": gemini_parts }));
                        }
                    }
                    _ => {}
                }
            }
            "assistant" => {
                let content = msg.get("content").and_then(Value::as_array);
                let is_same = msg.get("provider").and_then(Value::as_str) == Some(provider)
                    && msg.get("model").and_then(Value::as_str) == Some(model_id);
                let mut parts: Vec<Value> = Vec::new();
                if let Some(content) = content {
                    for block in content {
                        match block.get("type").and_then(Value::as_str) {
                            Some("text") => {
                                let text = block.get("text").and_then(Value::as_str).unwrap_or("");
                                if text.trim().is_empty() {
                                    continue;
                                }
                                parts.push(json!({ "text": text }));
                            }
                            Some("thinking") => {
                                let thinking =
                                    block.get("thinking").and_then(Value::as_str).unwrap_or("");
                                if thinking.trim().is_empty() {
                                    continue;
                                }
                                if is_same {
                                    let mut part = json!({ "thought": true, "text": thinking });
                                    if let Some(sig) =
                                        block.get("thinkingSignature").and_then(Value::as_str)
                                    {
                                        if !sig.is_empty() {
                                            part["thoughtSignature"] = json!(sig);
                                        }
                                    }
                                    parts.push(part);
                                } else {
                                    parts.push(json!({ "text": thinking }));
                                }
                            }
                            Some("toolCall") => {
                                let mut fc = json!({
                                    "name": block.get("name").and_then(Value::as_str).unwrap_or(""),
                                    "args": block.get("arguments").cloned().unwrap_or(json!({})),
                                });
                                if include_id {
                                    fc["id"] = json!(block.get("id").and_then(Value::as_str).unwrap_or(""));
                                }
                                parts.push(json!({ "functionCall": fc }));
                            }
                            _ => {}
                        }
                    }
                }
                if !parts.is_empty() {
                    contents.push(json!({ "role": "model", "parts": parts }));
                }
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
                let is_error = msg.get("isError").and_then(Value::as_bool).unwrap_or(false);
                let tool_name = msg.get("toolName").and_then(Value::as_str).unwrap_or("");
                let response_value = if text_result.is_empty() {
                    String::new()
                } else {
                    text_result
                };
                let mut fr = json!({
                    "name": tool_name,
                    "response": if is_error { json!({ "error": response_value }) } else { json!({ "output": response_value }) },
                });
                if include_id {
                    fr["id"] = json!(msg.get("toolCallId").and_then(Value::as_str).unwrap_or(""));
                }
                let function_response_part = json!({ "functionResponse": fr });
                // Merge consecutive function responses into one user turn.
                if let Some(last) = contents.last_mut() {
                    let is_user = last.get("role").and_then(Value::as_str) == Some("user");
                    let has_fr = last
                        .get("parts")
                        .and_then(Value::as_array)
                        .map(|parts| parts.iter().any(|p| p.get("functionResponse").is_some()))
                        .unwrap_or(false);
                    if is_user && has_fr {
                        last["parts"].as_array_mut().unwrap().push(function_response_part);
                        continue;
                    }
                }
                contents.push(json!({ "role": "user", "parts": [function_response_part] }));
            }
            _ => {}
        }
    }
    let system_instruction = if system_prompt.is_empty() {
        None
    } else {
        Some(system_prompt.to_string())
    };
    (system_instruction, contents)
}

/// Build a Google Generative AI `generateContent` request body (streaming).
pub fn build_google_request(
    model: &Value,
    system_prompt: &str,
    messages: &[Value],
    tools: &[Value],
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
    let (system_instruction, contents) = convert_google_messages(model, system_prompt, &transformed);

    let mut config = json!({});
    if let Some(si) = system_instruction {
        config["systemInstruction"] = json!(si);
    }
    if !tools.is_empty() {
        config["tools"] = pi_tools_to_google(tools);
    }
    if model.get("reasoning").and_then(Value::as_bool).unwrap_or(false) {
        let mut thinking_config = json!({ "includeThoughts": true });
        if let Some(level) = model.get("thinkingLevel").and_then(Value::as_str) {
            let mapped = match level {
                "minimal" => "MINIMAL",
                "low" => "LOW",
                "medium" => "MEDIUM",
                "high" => "HIGH",
                _ => "HIGH",
            };
            thinking_config["thinkingLevel"] = json!(mapped);
        }
        config["thinkingConfig"] = thinking_config;
    }
    json!({
        "model": model_id,
        "contents": contents,
        "config": config,
    })
}

/// Map a Gemini `finishReason` string to a pi `stopReason`.
pub fn map_google_stop_reason(reason: &str) -> &'static str {
    match reason {
        "STOP" => "stop",
        "MAX_TOKENS" => "length",
        _ => "error",
    }
}

/// Folds Gemini streaming chunks into a `StreamTurn`. Feed each decoded chunk
/// JSON object to `add_chunk`, then call `finish`.
#[derive(Default)]
pub struct GoogleAccumulator {
    text: String,
    thinking: String,
    thinking_signature: Option<String>,
    tool_calls: Vec<GoogleToolCall>,
    stop_reason: String,
    usage: Option<Value>,
    model: String,
    provider: String,
    api: String,
    events: Vec<Value>,
    started: bool,
    // Track the current block kind to detect transitions.
    current_kind: Option<String>,
    has_tool_call: bool,
}

#[derive(Default, Clone)]
struct GoogleToolCall {
    id: String,
    name: String,
    arguments: Value,
}

impl GoogleAccumulator {
    pub fn new(model: &str, provider: &str, api: &str) -> Self {
        GoogleAccumulator {
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
            content.push(json!({
                "type": "toolCall",
                "id": tc.id,
                "name": tc.name,
                "arguments": tc.arguments,
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

    /// Feed one decoded Gemini streaming chunk.
    pub fn add_chunk(&mut self, chunk: &Value) {
        self.ensure_started();
        let candidate = chunk.get("candidates").and_then(Value::as_array).and_then(|a| a.first());
        if let Some(candidate) = candidate {
            if let Some(parts) = candidate.pointer("/content/parts").and_then(Value::as_array) {
                for part in parts {
                    if let Some(text) = part.get("text").and_then(Value::as_str) {
                        let is_thinking =
                            part.get("thought").and_then(Value::as_bool).unwrap_or(false);
                        let kind = if is_thinking { "thinking" } else { "text" };
                        // Detect block transition.
                        if self.current_kind.as_deref() != Some(kind) {
                            self.current_kind = Some(kind.to_string());
                        }
                        if is_thinking {
                            self.thinking.push_str(text);
                            if let Some(sig) = part.get("thoughtSignature").and_then(Value::as_str) {
                                if !sig.is_empty() {
                                    self.thinking_signature = Some(sig.to_string());
                                }
                            }
                            self.events.push(json!({
                                "type": "message_update",
                                "assistantMessageEvent": { "type": "thinking_delta", "delta": text },
                                "partial": self.current_partial(),
                            }));
                        } else {
                            self.text.push_str(text);
                            self.events.push(json!({
                                "type": "message_update",
                                "assistantMessageEvent": { "type": "text_delta", "delta": text },
                                "partial": self.current_partial(),
                            }));
                        }
                    }
                    if let Some(fc) = part.get("functionCall") {
                        self.current_kind = None;
                        let name = fc.get("name").and_then(Value::as_str).unwrap_or("").to_string();
                        let id = fc
                            .get("id")
                            .and_then(Value::as_str)
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| format!("{}_{}", name, self.tool_calls.len()));
                        let arguments = fc.get("args").cloned().unwrap_or(json!({}));
                        self.tool_calls.push(GoogleToolCall {
                            id,
                            name,
                            arguments,
                        });
                        self.has_tool_call = true;
                    }
                }
            }
            if let Some(reason) = candidate.get("finishReason").and_then(Value::as_str) {
                self.stop_reason = map_google_stop_reason(reason).to_string();
            }
        }
        if let Some(usage) = chunk.get("usageMetadata") {
            self.merge_usage(usage);
        }
    }

    fn merge_usage(&mut self, usage: &Value) {
        let prompt = usage.get("promptTokenCount").and_then(Value::as_u64).unwrap_or(0);
        let candidates = usage.get("candidatesTokenCount").and_then(Value::as_u64).unwrap_or(0);
        let cached = usage.get("cachedContentTokenCount").and_then(Value::as_u64).unwrap_or(0);
        let thoughts = usage.get("thoughtsTokenCount").and_then(Value::as_u64).unwrap_or(0);
        let total = usage.get("totalTokenCount").and_then(Value::as_u64).unwrap_or(0);
        self.usage = Some(json!({
            "input": prompt.saturating_sub(cached),
            "output": candidates + thoughts,
            "cacheRead": cached,
            "cacheWrite": 0,
            "reasoning": thoughts,
            "totalTokens": total,
        }));
    }

    /// Finalize into a `StreamTurn`.
    pub fn finish(mut self) -> StreamTurn {
        self.ensure_started();
        if self.has_tool_call {
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
