//! Provider 流式调用与 agent 循环 —— OpenAI Responses API 实现。
//!
//! 这是 Vega Rust 版的核心。pi-ai 的 `streamSimple` 对 `openai-responses` provider 走
//! OpenAI Responses API（`POST {base_url}/responses`，SSE）。此处实现该协议的子集：
//! 文本/推理增量、函数调用增量、usage、tool 循环。其他 provider 协议（anthropic、
//! google、openai-completions、azure）尚未移植，见 main.rs 的 action 分发。
use crate::config::ModelInfo;
use crate::tools::{clamp_tool_output_text, ToolResult, OPENAI_TOOL_OUTPUT_SAFE_MAX_CHARS};
use reqwest::Client;
use serde_json::{json, Map, Value};
use std::sync::Arc;
use tokio::sync::Mutex;

pub struct ProviderEvent {
    pub kind: ProviderEventKind,
}

pub enum ProviderEventKind {
    TextDelta(String),
    ThinkingDelta(String),
    ToolCallStart { id: String, name: String },
    ToolCallArgumentsDelta { id: String, delta: String },
    ToolCallEnd { id: String },
    MessageEnd { usage: Option<Value>, stop_reason: Option<String> },
    Error(String),
}

pub struct StreamOutput {
    pub text: String,
    pub thinking: String,
    pub tool_calls: Vec<ToolCall>,
    pub usage: Option<Value>,
    pub stop_reason: Option<String>,
    pub error: Option<String>,
}

#[derive(Clone)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

pub async fn stream_responses(
    client: &Client,
    model: &ModelInfo,
    api_key: Option<&str>,
    system_prompt: &str,
    messages: &[Value],
    tools_schema: &[Value],
    session_id: Option<&str>,
    thinking_level: Option<&str>,
) -> StreamOutput {
    let url = format!("{}/responses", model.base_url.trim_end_matches('/'));
    let mut input: Vec<Value> = Vec::new();
    if !system_prompt.is_empty() {
        input.push(json!({ "role": "system", "content": system_prompt }));
    }
    for m in messages {
        input.push(m.clone());
    }
    let mut body = json!({
        "model": model.id,
        "input": input,
        "stream": true,
    });
    if !tools_schema.is_empty() {
        body["tools"] = Value::Array(tools_schema.to_vec());
    }
    if let Some(level) = thinking_level {
        if model.reasoning {
            body["reasoning"] = json!({ "effort": level });
        }
    }
    if let Some(sid) = session_id {
        // prompt_cache_key compat for OpenAI-compatible proxies.
        body["prompt_cache_key"] = Value::String(sid.chars().take(64).collect());
    }
    let mut req = client.post(&url).json(&body);
    if let Some(key) = api_key {
        req = req.bearer_auth(key);
    }
    if let Some(headers) = &model.headers {
        if let Some(obj) = headers.as_object() {
            for (k, v) in obj {
                if let Some(s) = v.as_str() {
                    req = req.header(k, s);
                }
            }
        }
    }
    let response = match req.send().await {
        Ok(r) => r,
        Err(e) => return StreamOutput { text: String::new(), thinking: String::new(), tool_calls: Vec::new(), usage: None, stop_reason: None, error: Some(e.to_string()) },
    };
    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        return StreamOutput { text: String::new(), thinking: String::new(), tool_calls: Vec::new(), usage: None, stop_reason: None, error: Some(format!("provider HTTP {status}: {}", truncate(&text, 2000))) };
    }
    use futures_util::StreamExt;
    let stream = response.bytes_stream();
    parse_sse_stream(stream).await
}

/// SSE accumulator state — exposed so tests can drive the parser chunk-by-chunk.
pub struct SseState {
    pub buf: String,
    pub text: String,
    pub thinking: String,
    pub tool_calls: Vec<ToolCall>,
    pub tool_args: std::collections::HashMap<String, String>,
    pub usage: Option<Value>,
    pub stop_reason: Option<String>,
    pub error: Option<String>,
}

impl SseState {
    pub fn new() -> Self {
        SseState {
            buf: String::new(),
            text: String::new(),
            thinking: String::new(),
            tool_calls: Vec::new(),
            tool_args: std::collections::HashMap::new(),
            usage: None,
            stop_reason: None,
            error: None,
        }
    }

    pub fn finish(self) -> StreamOutput {
        StreamOutput {
            text: self.text,
            thinking: self.thinking,
            tool_calls: self.tool_calls,
            usage: self.usage,
            stop_reason: self.stop_reason,
            error: self.error,
        }
    }
}

/// Process a single complete SSE `data:` line and mutate state.
pub fn process_sse_line(line: &str, state: &mut SseState) {
    let line = line.trim();
    if line.is_empty() { return; }
    let Some(data) = line.strip_prefix("data: ") else { return; };
    if data == "[DONE]" { return; }
    let Ok(event) = serde_json::from_str::<Value>(data) else { return; };
    let event_type = event.get("type").and_then(Value::as_str).unwrap_or("");
    match event_type {
        "response.output_text.delta" => {
            if let Some(d) = event.get("delta").and_then(Value::as_str) {
                state.text.push_str(d);
            }
        }
        "response.reasoning.delta" | "response.reasoning_summary_text.delta" => {
            if let Some(d) = event.get("delta").and_then(Value::as_str) {
                state.thinking.push_str(d);
            }
        }
        "response.output_item.added" => {
            if let Some(item) = event.get("item") {
                if item.get("type").and_then(Value::as_str) == Some("function_call") {
                    let id = item.get("call_id").or_else(|| item.get("id")).and_then(Value::as_str).unwrap_or("").to_string();
                    let name = item.get("name").and_then(Value::as_str).unwrap_or("").to_string();
                    if !id.is_empty() && !state.tool_calls.iter().any(|t| t.id == id) {
                        state.tool_calls.push(ToolCall { id, name, arguments: String::new() });
                    }
                }
            }
        }
        "response.function_call_arguments.delta" => {
            let id = event.get("item_id").or_else(|| event.get("call_id")).and_then(Value::as_str).unwrap_or("").to_string();
            if let Some(d) = event.get("delta").and_then(Value::as_str) {
                let entry = state.tool_args.entry(id.clone()).or_default();
                entry.push_str(d);
            }
        }
        "response.output_item.done" => {
            if let Some(item) = event.get("item") {
                if item.get("type").and_then(Value::as_str) == Some("function_call") {
                    let id = item.get("call_id").or_else(|| item.get("id")).and_then(Value::as_str).unwrap_or("").to_string();
                    let args = item.get("arguments").and_then(Value::as_str).map(str::to_string).unwrap_or_default();
                    if !id.is_empty() {
                        if let Some(tc) = state.tool_calls.iter_mut().find(|t| t.id == id) {
                            if !args.is_empty() { tc.arguments = args; }
                        } else {
                            let name = item.get("name").and_then(Value::as_str).unwrap_or("").to_string();
                            state.tool_calls.push(ToolCall { id, name, arguments: args });
                        }
                    }
                }
            }
        }
        "response.completed" | "response.done" => {
            if let Some(resp) = event.get("response").or(Some(&event)) {
                if let Some(u) = resp.get("usage") {
                    state.usage = Some(u.clone());
                }
                if let Some(sr) = resp.get("status").and_then(Value::as_str) {
                    state.stop_reason = Some(sr.to_string());
                }
            }
        }
        "error" => {
            let msg = event.get("message").and_then(Value::as_str).unwrap_or("provider error").to_string();
            state.error = Some(msg);
        }
        _ => {}
    }
}

/// Feed a chunk of bytes into the state; complete `\n`-terminated lines are processed.
pub fn feed_sse_chunk(bytes: &[u8], state: &mut SseState) {
    state.buf.push_str(&String::from_utf8_lossy(bytes));
    while let Some(idx) = state.buf.find('\n') {
        let line = state.buf[..idx].to_string();
        state.buf = state.buf[idx + 1..].to_string();
        process_sse_line(&line, state);
    }
}

/// Drive the SSE parser over an async byte stream (used by `stream_responses` and tests).
pub async fn parse_sse_stream<S>(mut stream: S) -> StreamOutput
where
    S: futures_util::Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Unpin,
{
    use futures_util::StreamExt;
    let mut state = SseState::new();
    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(c) => feed_sse_chunk(&c, &mut state),
            Err(e) => { state.error = Some(e.to_string()); break; }
        }
    }
    // Merge accumulated argument deltas into tool_calls.
    for tc in &mut state.tool_calls {
        if tc.arguments.is_empty() {
            if let Some(args) = state.tool_args.get(&tc.id) {
                tc.arguments = args.clone();
            }
        }
    }
    state.finish()
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n { s.to_string() } else { format!("{}…", &s[..n]) }
}

/// Build the OpenAI Responses tool schema for a named tool with a JSON-schema parameter object.
pub fn tool_schema(name: &str, description: &str, parameters: Value) -> Value {
    json!({
        "type": "function",
        "name": name,
        "description": description,
        "parameters": parameters,
        "strict": false
    })
}

/// Convert a ToolResult into a Responses `function_call_output` input item.
pub fn tool_output_item(call_id: &str, result: &ToolResult) -> Value {
    let text: String = result.content.iter().filter_map(|p| p.get("text").and_then(Value::as_str).map(str::to_string)).collect::<Vec<_>>().join("\n");
    let clamped = clamp_tool_output_text(&text, OPENAI_TOOL_OUTPUT_SAFE_MAX_CHARS);
    json!({ "type": "function_call_output", "call_id": call_id, "output": clamped })
}

/// Append an assistant message capturing text + tool calls to the transcript.
pub fn assistant_message(text: &str, thinking: &str, tool_calls: &[ToolCall]) -> Value {
    let mut content: Vec<Value> = Vec::new();
    if !thinking.is_empty() {
        content.push(json!({ "type": "reasoning", "text": thinking }));
    }
    if !text.is_empty() {
        content.push(json!({ "type": "output_text", "text": text }));
    }
    for tc in tool_calls {
        content.push(json!({ "type": "function_call", "call_id": tc.id, "name": tc.name, "arguments": tc.arguments }));
    }
    json!({ "role": "assistant", "content": content })
}

pub fn user_message(text: &str, images: &[Value]) -> Value {
    let mut content: Vec<Value> = Vec::new();
    if !text.is_empty() {
        content.push(json!({ "type": "input_text", "text": text }));
    }
    for img in images {
        content.push(img.clone());
    }
    json!({ "role": "user", "content": content })
}

pub type ToolExecutor = Arc<dyn Fn(&str, &Value) -> std::pin::Pin<Box<dyn std::future::Future<Output = ToolResult> + Send>> + Send + Sync>;

pub struct AgentLoopOutput {
    pub text: String,
    pub thinking: String,
    pub usage: Option<Value>,
    pub cancelled: bool,
    pub error: Option<String>,
    pub messages: Vec<Value>,
}

/// Drive the agent loop: stream → execute tool calls → append results → repeat until no tool calls.
pub async fn run_agent_loop(
    client: &Client,
    model: &ModelInfo,
    api_key: Option<&str>,
    system_prompt: &str,
    messages: Arc<Mutex<Vec<Value>>>,
    tools_schema: Vec<Value>,
    session_id: Option<&str>,
    thinking_level: Option<&str>,
    tool_executor: ToolExecutor,
    on_event: impl Fn(ProviderEvent) + Send + Sync,
    cancel: Arc<std::sync::atomic::AtomicBool>,
) -> AgentLoopOutput {
    let mut final_text = String::new();
    let mut final_thinking = String::new();
    let mut usage: Option<Value> = None;
    let mut error: Option<String> = None;
    let mut iterations = 0u32;
    const MAX_ITERATIONS: u32 = 40;
    loop {
        iterations += 1;
        if iterations > MAX_ITERATIONS {
            error = Some("Vega agent 超过最大迭代次数".into());
            break;
        }
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            break;
        }
        let snapshot = messages.lock().await.clone();
        let out = stream_responses(client, model, api_key, system_prompt, &snapshot, &tools_schema, session_id, thinking_level).await;
        if let Some(e) = out.error {
            error = Some(e);
            break;
        }
        if !out.text.is_empty() {
            final_text = out.text.clone();
            on_event(ProviderEvent { kind: ProviderEventKind::TextDelta(out.text.clone()) });
        }
        if !out.thinking.is_empty() {
            final_thinking = out.thinking.clone();
            on_event(ProviderEvent { kind: ProviderEventKind::ThinkingDelta(out.thinking.clone()) });
        }
        if let Some(u) = &out.usage {
            usage = Some(u.clone());
            on_event(ProviderEvent { kind: ProviderEventKind::MessageEnd { usage: Some(u.clone()), stop_reason: out.stop_reason.clone() } });
        }
        // Append assistant message.
        {
            let mut m = messages.lock().await;
            m.push(assistant_message(&out.text, &out.thinking, &out.tool_calls));
        }
        if out.tool_calls.is_empty() {
            break;
        }
        // Execute tool calls in parallel.
        let mut handles: Vec<tokio::task::JoinHandle<(String, ToolResult)>> = Vec::new();
        for tc in &out.tool_calls {
            on_event(ProviderEvent { kind: ProviderEventKind::ToolCallStart { id: tc.id.clone(), name: tc.name.clone() } });
            let args_value = serde_json::from_str::<Value>(&tc.arguments).unwrap_or(Value::Null);
            let exec = tool_executor.clone();
            let id = tc.id.clone();
            let name = tc.name.clone();
            let args = args_value.clone();
            let exec2 = exec.clone();
            let handle = tokio::spawn(async move {
                let result = exec2(&name, &args).await;
                (id, result)
            });
            handles.push(handle);
        }
        let mut results: Vec<(String, ToolResult)> = Vec::new();
        for h in handles {
            if let Ok((id, r)) = h.await {
                on_event(ProviderEvent { kind: ProviderEventKind::ToolCallEnd { id: id.clone() } });
                results.push((id, r));
            }
        }
        {
            let mut m = messages.lock().await;
            for (id, r) in &results {
                m.push(tool_output_item(id, r));
            }
        }
    }
    let messages_final = messages.lock().await.clone();
    AgentLoopOutput { text: final_text, thinking: final_thinking, usage, cancelled: cancel.load(std::sync::atomic::Ordering::Relaxed), error, messages: messages_final }
}

#[allow(dead_code)]
fn _unused_map() -> Map<String, Value> { Map::new() }

#[cfg(test)]
mod tests {
    use super::*;

    /// Feed a sequence of byte chunks into the SSE parser and return the final StreamOutput.
    fn parse_chunks(chunks: &[&[u8]]) -> StreamOutput {
        let mut state = SseState::new();
        for c in chunks {
            feed_sse_chunk(c, &mut state);
        }
        // Flush any trailing line without a newline.
        if !state.buf.is_empty() {
            let remaining = std::mem::take(&mut state.buf);
            process_sse_line(&remaining, &mut state);
        }
        for tc in &mut state.tool_calls {
            if tc.arguments.is_empty() {
                if let Some(args) = state.tool_args.get(&tc.id) {
                    tc.arguments = args.clone();
                }
            }
        }
        state.finish()
    }

    fn sse_line(event_type: &str, extra: &str) -> String {
        format!("data: {{\"type\":\"{event_type}\"{extra}}}\n")
    }

    #[test]
    fn sse_text_deltas_concatenate() {
        let stream = format!(
            "{}{}{}",
            sse_line("response.output_text.delta", ",\"delta\":\"Hello\""),
            sse_line("response.output_text.delta", ",\"delta\":\" World\""),
            sse_line("response.completed", ",\"response\":{\"status\":\"completed\",\"usage\":{\"input\":10,\"output\":5}}"),
        );
        let out = parse_chunks(&[stream.as_bytes()]);
        assert_eq!(out.text, "Hello World");
        assert!(out.thinking.is_empty());
        assert!(out.tool_calls.is_empty());
        assert_eq!(out.stop_reason.as_deref(), Some("completed"));
        let usage = out.usage.unwrap();
        assert_eq!(usage["input"], 10);
        assert_eq!(usage["output"], 5);
        assert!(out.error.is_none());
    }

    #[test]
    fn sse_reasoning_deltas_concatenate() {
        let stream = format!(
            "{}{}",
            sse_line("response.reasoning.delta", ",\"delta\":\"thinking\""),
            sse_line("response.reasoning_summary_text.delta", ",\"delta\":\" more\""),
        );
        let out = parse_chunks(&[stream.as_bytes()]);
        assert_eq!(out.thinking, "thinking more");
        assert!(out.text.is_empty());
    }

    #[test]
    fn sse_function_call_with_argument_deltas() {
        let stream = format!(
            "{}{}{}{}",
            sse_line("response.output_item.added", ",\"item\":{\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"read\"}"),
            sse_line("response.function_call_arguments.delta", ",\"item_id\":\"call_1\",\"delta\":\"{\\\"path\\\":\\\"a\""),
            sse_line("response.function_call_arguments.delta", ",\"item_id\":\"call_1\",\"delta\":\".rs\\\"}\""),
            sse_line("response.output_item.done", ",\"item\":{\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"read\",\"arguments\":\"{\\\"path\\\":\\\"a.rs\\\"}\"}"),
        );
        let out = parse_chunks(&[stream.as_bytes()]);
        assert_eq!(out.tool_calls.len(), 1);
        let tc = &out.tool_calls[0];
        assert_eq!(tc.id, "call_1");
        assert_eq!(tc.name, "read");
        // output_item.done carries the final arguments — should override accumulated deltas.
        assert_eq!(tc.arguments, r#"{"path":"a.rs"}"#);
    }

    #[test]
    fn sse_function_call_arguments_from_deltas_only() {
        // No output_item.done; arguments must be reconstructed from deltas.
        let stream = format!(
            "{}{}{}",
            sse_line("response.output_item.added", ",\"item\":{\"type\":\"function_call\",\"call_id\":\"call_2\",\"name\":\"bash\"}"),
            sse_line("response.function_call_arguments.delta", ",\"item_id\":\"call_2\",\"delta\":\"{\\\"command\\\":\\\"ls\\\"}\""),
            sse_line("response.completed", ",\"response\":{\"status\":\"completed\"}"),
        );
        let out = parse_chunks(&[stream.as_bytes()]);
        assert_eq!(out.tool_calls.len(), 1);
        assert_eq!(out.tool_calls[0].id, "call_2");
        assert_eq!(out.tool_calls[0].arguments, r#"{"command":"ls"}"#);
    }

    #[test]
    fn sse_split_across_chunks_reassembles_lines() {
        // A single SSE line split across multiple byte chunks must still parse.
        let full = sse_line("response.output_text.delta", ",\"delta\":\"split\"");
        let mid = full.len() / 2;
        let out = parse_chunks(&[&full.as_bytes()[..mid], &full.as_bytes()[mid..]]);
        assert_eq!(out.text, "split");
    }

    #[test]
    fn sse_done_marker_ignored() {
        let stream = format!(
            "{}data: [DONE]\n",
            sse_line("response.output_text.delta", ",\"delta\":\"ok\""),
        );
        let out = parse_chunks(&[stream.as_bytes()]);
        assert_eq!(out.text, "ok");
        assert!(out.error.is_none());
    }

    #[test]
    fn sse_error_event_captured() {
        let stream = sse_line("error", ",\"message\":\"rate limited\"");
        let out = parse_chunks(&[stream.as_bytes()]);
        assert_eq!(out.error.as_deref(), Some("rate limited"));
    }

    #[test]
    fn sse_multiple_function_calls_preserve_order() {
        let stream = format!(
            "{}{}{}{}",
            sse_line("response.output_item.added", ",\"item\":{\"type\":\"function_call\",\"call_id\":\"c1\",\"name\":\"read\"}"),
            sse_line("response.output_item.added", ",\"item\":{\"type\":\"function_call\",\"call_id\":\"c2\",\"name\":\"grep\"}"),
            sse_line("response.output_item.done", ",\"item\":{\"type\":\"function_call\",\"call_id\":\"c1\",\"name\":\"read\",\"arguments\":\"{}\"}"),
            sse_line("response.output_item.done", ",\"item\":{\"type\":\"function_call\",\"call_id\":\"c2\",\"name\":\"grep\",\"arguments\":\"{}\"}"),
        );
        let out = parse_chunks(&[stream.as_bytes()]);
        assert_eq!(out.tool_calls.len(), 2);
        assert_eq!(out.tool_calls[0].id, "c1");
        assert_eq!(out.tool_calls[0].name, "read");
        assert_eq!(out.tool_calls[1].id, "c2");
        assert_eq!(out.tool_calls[1].name, "grep");
    }

    #[test]
    fn tool_schema_shape() {
        let s = tool_schema("read", "desc", json!({"type":"object"}));
        assert_eq!(s["type"], "function");
        assert_eq!(s["name"], "read");
        assert_eq!(s["description"], "desc");
        assert_eq!(s["strict"], false);
    }

    #[test]
    fn assistant_message_serializes_text_and_tool_calls() {
        let msg = assistant_message("hi", "thinking", &[ToolCall {
            id: "c1".into(),
            name: "read".into(),
            arguments: "{}".into(),
        }]);
        let content = msg["content"].as_array().unwrap();
        // reasoning first, then text, then function_call.
        assert_eq!(content[0]["type"], "reasoning");
        assert_eq!(content[0]["text"], "thinking");
        assert_eq!(content[1]["type"], "output_text");
        assert_eq!(content[1]["text"], "hi");
        assert_eq!(content[2]["type"], "function_call");
        assert_eq!(content[2]["call_id"], "c1");
        assert_eq!(content[2]["name"], "read");
        assert_eq!(content[2]["arguments"], "{}");
        assert_eq!(msg["role"], "assistant");
    }

    #[test]
    fn user_message_with_images() {
        let img = json!({"type":"input_image","image_url":"data:image/png;base64,abc"});
        let msg = user_message("hello", &[img.clone()]);
        let content = msg["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "input_text");
        assert_eq!(content[0]["text"], "hello");
        assert_eq!(content[1], img);
        assert_eq!(msg["role"], "user");
    }

    #[test]
    fn tool_output_item_clamps_large_text() {
        let big = "x".repeat(OPENAI_TOOL_OUTPUT_SAFE_MAX_CHARS + 1000);
        let result = ToolResult::text(big);
        let item = tool_output_item("c1", &result);
        let output = item["output"].as_str().unwrap();
        assert!(output.contains("truncated"));
        assert!(output.len() <= OPENAI_TOOL_OUTPUT_SAFE_MAX_CHARS);
    }
}
