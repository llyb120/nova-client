//! OpenAI 兼容 provider：chat/completions 与 responses 两种协议的流式调用、
//! 消息互转、reasoning 参数、缓存优化注入（prompt_cache_key / service_tier /
//! prompt_cache_retention / 会话亲和头）。

use crate::http_stream::SseDecoder;
use crate::lyra::config::ResolvedModel;
use crate::lyra::prompt::{clamp_prompt_cache_key, clamp_tool_output_text};
use crate::lyra::tools::Tool;
use base64::Engine;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

#[derive(Debug)]
pub enum StreamEvent {
    TextDelta(String),
    ThinkingDelta(String),
    /// 工具调用参数的流式增量：index 为本条消息内工具调用序号，
    /// args 为该调用迄今累积的参数 JSON 片段（供投机执行预解析）。
    ToolArgsDelta {
        index: usize,
        name: String,
        args: String,
    },
}

#[derive(Debug, Default)]
pub struct StreamResult {
    pub content: Vec<Value>,
    pub usage: Value,
    pub stop_reason: String,
    pub error_message: Option<String>,
}

impl StreamResult {
    fn empty() -> Self {
        StreamResult {
            content: Vec::new(),
            usage: Value::Null,
            stop_reason: String::new(),
            error_message: None,
        }
    }
}

fn content_text_parts(content: &[Value]) -> String {
    content
        .iter()
        .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n")
}

fn tool_result_text(message: &Value) -> String {
    let text = message
        .get("content")
        .and_then(Value::as_array)
        .map(|parts| content_text_parts(parts))
        .unwrap_or_default();
    clamp_tool_output_text(&text)
}

fn tool_result_images(message: &Value, model: &ResolvedModel) -> Vec<Value> {
    if !model.supports_images {
        return Vec::new();
    }
    let mut images: Vec<Value> = message
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|part| part.get("type").and_then(Value::as_str) == Some("image"))
        .cloned()
        .collect();
    if let Some(path) = message
        .pointer("/details/imagePath")
        .and_then(Value::as_str)
    {
        if let Ok(data) = std::fs::read(path) {
            images.push(json!({
                "type": "image",
                "mimeType": "image/png",
                "data": base64::engine::general_purpose::STANDARD.encode(data),
            }));
        }
    }
    images
}

// ---------- chat/completions ----------

fn completions_messages(
    system_prompt: &str,
    messages: &[Value],
    model: &ResolvedModel,
) -> Vec<Value> {
    let mut out = vec![json!({ "role": "system", "content": system_prompt })];
    for message in messages {
        match message.get("role").and_then(Value::as_str) {
            Some("user") => {
                let mut parts = Vec::new();
                if let Some(content) = message.get("content") {
                    if let Some(text) = content.as_str() {
                        parts.push(json!({ "type": "text", "text": text }));
                    } else if let Some(blocks) = content.as_array() {
                        for block in blocks {
                            match block.get("type").and_then(Value::as_str) {
                                Some("text") => parts.push(json!({
                                    "type": "text",
                                    "text": block.get("text").and_then(Value::as_str).unwrap_or_default()
                                })),
                                Some("image") if model.supports_images => parts.push(json!({
                                    "type": "image_url",
                                    "image_url": { "url": format!("data:{};base64,{}",
                                        block.get("mimeType").and_then(Value::as_str).unwrap_or("image/png"),
                                        block.get("data").and_then(Value::as_str).unwrap_or_default()) }
                                })),
                                Some("image") | Some("image_url") => parts.push(json!({
                                    "type": "text",
                                    "text": "[历史图片已省略：当前模型不支持图片输入]"
                                })),
                                _ => {}
                            }
                        }
                    }
                }
                out.push(json!({ "role": "user", "content": parts }));
            }
            Some("assistant") => {
                let content = message
                    .get("content")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let text = content_text_parts(&content);
                let tool_calls: Vec<Value> = content
                    .iter()
                    .filter(|part| part.get("type").and_then(Value::as_str) == Some("toolCall"))
                    .map(|part| {
                        json!({
                            "id": part.get("id").and_then(Value::as_str).unwrap_or_default(),
                            "type": "function",
                            "function": {
                                "name": part.get("name").and_then(Value::as_str).unwrap_or_default(),
                                "arguments": serde_json::to_string(
                                    part.get("arguments").unwrap_or(&Value::Object(Map::new()))
                                ).unwrap_or_else(|_| "{}".into()),
                            }
                        })
                    })
                    .collect();
                let mut item = Map::new();
                item.insert("role".into(), json!("assistant"));
                item.insert(
                    "content".into(),
                    if text.is_empty() {
                        Value::Null
                    } else {
                        json!(text)
                    },
                );
                if model.requires_reasoning_content {
                    let thinking: String = content
                        .iter()
                        .filter(|part| part.get("type").and_then(Value::as_str) == Some("thinking"))
                        .filter_map(|part| part.get("thinking").and_then(Value::as_str))
                        .collect::<Vec<_>>()
                        .join("\n");
                    // DeepSeek/Console Go 的 thinking 模式要求每条带 tool_calls 的 assistant
                    // 历史都显式回传 reasoning_content；运行时合成工具调用可能只有决策轨迹。
                    if !thinking.is_empty() || !tool_calls.is_empty() {
                        item.insert("reasoning_content".into(), json!(thinking));
                    }
                }
                if !tool_calls.is_empty() {
                    item.insert("tool_calls".into(), Value::Array(tool_calls));
                }
                out.push(Value::Object(item));
            }
            Some("toolResult") => {
                out.push(json!({
                    "role": "tool",
                    "tool_call_id": message.get("toolCallId").and_then(Value::as_str).unwrap_or_default(),
                    "content": tool_result_text(message),
                }));
                let images = tool_result_images(message, model);
                if !images.is_empty() {
                    let mut parts = vec![
                        json!({ "type": "text", "text": "Screenshot returned by the browser tool." }),
                    ];
                    parts.extend(images.into_iter().map(|image| json!({
                        "type": "image_url",
                        "image_url": { "url": format!("data:{};base64,{}",
                            image.get("mimeType").and_then(Value::as_str).unwrap_or("image/png"),
                            image.get("data").and_then(Value::as_str).unwrap_or_default()) }
                    })));
                    out.push(json!({ "role": "user", "content": parts }));
                }
            }
            _ => {}
        }
    }
    out
}

fn apply_reasoning_completions(body: &mut Value, model: &ResolvedModel, level: Option<&str>) {
    if !model.reasoning {
        return;
    }
    let enabled = !matches!(level, Some("off"));
    match model.thinking_format.as_deref() {
        Some("deepseek") | Some("zai") => {
            body["thinking"] = json!({ "type": if enabled { "enabled" } else { "disabled" } });
            // deepseek 等在 thinking.enabled 之外同时发送 reasoning_effort（如 max）。
            if enabled && model.supports_reasoning_effort {
                if let Some(level) = level {
                    body["reasoning_effort"] = json!(level);
                }
            }
        }
        // thinking_format 由用户在 options 显式指定，代表中转站接受的风格：
        // kimi → 只发 reasoning_effort；qwen → enable_thinking 且中转普遍同时收 effort，
        // 端点只认 thinking_budget 时配 supportsReasoningEffort: false 关掉。
        Some("qwen") => {
            body["enable_thinking"] = json!(enabled);
            if enabled && model.supports_reasoning_effort {
                if let Some(level) = level {
                    body["reasoning_effort"] = json!(level);
                }
            }
        }
        _ => {
            if enabled {
                if let Some(level) = level {
                    body["reasoning_effort"] = json!(level);
                }
            }
        }
    }
    // 与 thinking_format 无关：显式配置即下发 thinking.clear_thinking（Some(false) =
    // GLM Preserved Thinking）。已有 thinking 对象时合并，没有则新建。
    if let Some(clear) = model.clear_thinking.filter(|_| enabled) {
        if let Some(object) = body.as_object_mut() {
            let thinking = object
                .entry("thinking")
                .or_insert_with(|| json!({}))
                .as_object_mut();
            if let Some(thinking) = thinking {
                thinking.insert("clear_thinking".into(), json!(clear));
            }
        }
    }
}

fn completions_body(
    model: &ResolvedModel,
    system_prompt: &str,
    messages: &[Value],
    tools: &[Tool],
    level: Option<&str>,
    session_id: Option<&str>,
) -> Value {
    let tool_defs: Vec<Value> = tools
        .iter()
        .map(|tool| {
            json!({
                "type": "function",
                "function": {
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.parameters,
                }
            })
        })
        .collect();
    let mut body = json!({
        "model": model.id,
        "messages": completions_messages(system_prompt, messages, model),
        "stream": true,
        "stream_options": { "include_usage": true },
    });
    body[model.max_tokens_field] = json!(model.max_output_tokens);
    if !tool_defs.is_empty() {
        body["tools"] = Value::Array(tool_defs);
    }
    // 用户在 options 里配置的非内置字段直接附加到请求体顶层（tool_stream 等）。
    if let Some(object) = body.as_object_mut() {
        for (key, value) in &model.extra_options {
            object.insert(key.clone(), value.clone());
        }
    }
    apply_reasoning_completions(&mut body, model, level);
    if let Some(key) = session_id.and_then(clamp_prompt_cache_key) {
        body["prompt_cache_key"] = json!(key);
    }
    if model.supports_long_cache_retention {
        body["prompt_cache_retention"] = json!("24h");
    }
    if let Some(tier) = &model.service_tier {
        body["service_tier"] = json!(tier);
    }
    if let Some(temperature) = model.temperature {
        body["temperature"] = json!(temperature);
    }
    if let Some(top_p) = model.top_p {
        body["top_p"] = json!(top_p);
    }
    body
}

// ---------- responses ----------

fn responses_input(messages: &[Value], model: &ResolvedModel) -> Vec<Value> {
    let mut out = Vec::new();
    for message in messages {
        match message.get("role").and_then(Value::as_str) {
            Some("user") => {
                let mut parts = Vec::new();
                if let Some(content) = message.get("content") {
                    if let Some(text) = content.as_str() {
                        parts.push(json!({ "type": "input_text", "text": text }));
                    } else if let Some(blocks) = content.as_array() {
                        for block in blocks {
                            match block.get("type").and_then(Value::as_str) {
                                Some("text") => parts.push(json!({
                                    "type": "input_text",
                                    "text": block.get("text").and_then(Value::as_str).unwrap_or_default()
                                })),
                                Some("image") if model.supports_images => parts.push(json!({
                                    "type": "input_image",
                                    "image_url": format!("data:{};base64,{}",
                                        block.get("mimeType").and_then(Value::as_str).unwrap_or("image/png"),
                                        block.get("data").and_then(Value::as_str).unwrap_or_default())
                                })),
                                Some("image") | Some("image_url") => parts.push(json!({
                                    "type": "input_text",
                                    "text": "[历史图片已省略：当前模型不支持图片输入]"
                                })),
                                _ => {}
                            }
                        }
                    }
                }
                if !parts.is_empty() {
                    out.push(json!({ "type": "message", "role": "user", "content": parts }));
                }
            }
            Some("assistant") => {
                let content = message
                    .get("content")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let text = content_text_parts(&content);
                if !text.is_empty() {
                    out.push(json!({
                        "type": "message", "role": "assistant",
                        "content": [{ "type": "output_text", "text": text }]
                    }));
                }
                for part in &content {
                    if part.get("type").and_then(Value::as_str) != Some("toolCall") {
                        continue;
                    }
                    out.push(json!({
                        "type": "function_call",
                        "call_id": part.get("id").and_then(Value::as_str).unwrap_or_default(),
                        "name": part.get("name").and_then(Value::as_str).unwrap_or_default(),
                        "arguments": serde_json::to_string(
                            part.get("arguments").unwrap_or(&Value::Object(Map::new()))
                        ).unwrap_or_else(|_| "{}".into()),
                    }));
                }
            }
            Some("toolResult") => {
                out.push(json!({
                    "type": "function_call_output",
                    "call_id": message.get("toolCallId").and_then(Value::as_str).unwrap_or_default(),
                    "output": tool_result_text(message),
                }));
                let images = tool_result_images(message, model);
                if !images.is_empty() {
                    let mut content = vec![json!({
                        "type": "input_text",
                        "text": "Screenshot returned by the browser tool."
                    })];
                    content.extend(images.into_iter().map(|image| json!({
                        "type": "input_image",
                        "image_url": format!("data:{};base64,{}",
                            image.get("mimeType").and_then(Value::as_str).unwrap_or("image/png"),
                            image.get("data").and_then(Value::as_str).unwrap_or_default())
                    })));
                    out.push(json!({ "type": "message", "role": "user", "content": content }));
                }
            }
            _ => {}
        }
    }
    out
}

fn responses_body(
    model: &ResolvedModel,
    system_prompt: &str,
    messages: &[Value],
    tools: &[Tool],
    level: Option<&str>,
    session_id: Option<&str>,
) -> Value {
    let tool_defs: Vec<Value> = tools
        .iter()
        .map(|tool| {
            json!({
                "type": "function",
                "name": tool.name,
                "description": tool.description,
                "parameters": tool.parameters,
            })
        })
        .collect();
    let mut body = json!({
        "model": model.id,
        "instructions": system_prompt,
        "input": responses_input(messages, model),
        "store": false,
        "stream": true,
        "max_output_tokens": model.max_output_tokens,
    });
    if !tool_defs.is_empty() {
        body["tools"] = Value::Array(tool_defs);
    }
    if model.reasoning && level != Some("off") {
        // OpenAI Responses 只有显式请求 summary 才会流式返回
        // response.reasoning_summary_text.delta；仅发送 effort 会有推理开销但前端无内容可展示。
        let mut reasoning = Map::new();
        reasoning.insert("summary".into(), json!("auto"));
        if let Some(level) = level {
            reasoning.insert("effort".into(), json!(level));
        }
        body["reasoning"] = Value::Object(reasoning);
    }
    if let Some(key) = session_id.and_then(clamp_prompt_cache_key) {
        body["prompt_cache_key"] = json!(key);
    }
    if model.supports_long_cache_retention {
        body["prompt_cache_retention"] = json!("24h");
    }
    if let Some(tier) = &model.service_tier {
        body["service_tier"] = json!(tier);
    }
    if let Some(temperature) = model.temperature {
        body["temperature"] = json!(temperature);
    }
    if let Some(top_p) = model.top_p {
        body["top_p"] = json!(top_p);
    }
    body
}

// ---------- HTTP + SSE ----------

// PI/OpenAI SDK 的请求可由 AbortSignal 打断；Lyra 使用 reqwest 时必须显式把取消和
// deadline 并入网络 future，否则代理接受连接后不发响应头/SSE 时会永久挂起。
const RESPONSE_HEADERS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
const SSE_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);
const CANCEL_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);

async fn wait_cancelled(cancel: &Arc<AtomicBool>) {
    while !cancel.load(Ordering::SeqCst) {
        tokio::time::sleep(CANCEL_POLL_INTERVAL).await;
    }
}

async fn send_request(
    request: reqwest::RequestBuilder,
    cancel: &Arc<AtomicBool>,
) -> Result<reqwest::Response, String> {
    tokio::select! {
        result = tokio::time::timeout(RESPONSE_HEADERS_TIMEOUT, request.send()) => {
            match result {
                Ok(result) => result.map_err(|e| format!("请求失败：{e}")),
                Err(_) => Err(format!(
                    "provider 响应头等待超过 {}s",
                    RESPONSE_HEADERS_TIMEOUT.as_secs()
                )),
            }
        }
        _ = wait_cancelled(cancel) => Err("provider 请求已取消".into()),
    }
}

async fn post_stream(
    http: &reqwest::Client,
    url: &str,
    model: &ResolvedModel,
    api_key: &str,
    session_id: Option<&str>,
    body: Value,
    cancel: &Arc<AtomicBool>,
) -> Result<reqwest::Response, String> {
    let mut request = http
        .post(url)
        .header("content-type", "application/json")
        .header("accept", "text/event-stream")
        .json(&body);
    if !api_key.is_empty() {
        request = request.bearer_auth(api_key);
    }
    for (key, value) in &model.headers {
        if let Some(text) = value.as_str() {
            request = request.header(key.as_str(), text);
        }
    }
    if model.session_affinity_headers {
        if let Some(session) = session_id {
            // openrouter 只发 x-session-id；其余发 session_id（openai 格式）
            // + x-client-request-id + x-session-affinity，提高代理层会话亲和/前缀缓存命中。
            if model.session_affinity_format == "openrouter" {
                request = request.header("x-session-id", session);
            } else {
                if model.session_affinity_format == "openai" {
                    request = request.header("session_id", session);
                }
                request = request
                    .header("x-client-request-id", session)
                    .header("x-session-affinity", session);
            }
        }
    }
    let response = send_request(request, cancel).await?;
    let status = response.status();
    if !status.is_success() {
        let text = response.text().await.unwrap_or_default();
        let message = text.trim();
        return Err(if message.is_empty() {
            format!("HTTP {status}")
        } else {
            format!(
                "HTTP {status}：{}",
                message.chars().take(500).collect::<String>()
            )
        });
    }
    Ok(response)
}

/// 逐行读取 SSE，回调每个 data 负载；[DONE] 或取消时结束。返回是否被取消。
async fn read_sse(
    response: &mut reqwest::Response,
    cancel: &Arc<AtomicBool>,
    mut on_data: impl FnMut(&str) -> Result<(), String>,
) -> Result<bool, String> {
    let mut decoder = SseDecoder::new();
    loop {
        let chunk = tokio::select! {
            result = tokio::time::timeout(SSE_IDLE_TIMEOUT, response.chunk()) => {
                match result {
                    Ok(result) => result.map_err(|e| format!("读取响应流失败：{e}"))?,
                    Err(_) => return Err(format!(
                        "provider SSE 连续 {}s 无数据",
                        SSE_IDLE_TIMEOUT.as_secs()
                    )),
                }
            }
            _ = wait_cancelled(cancel) => return Ok(true),
        };
        let (events, finished) = match chunk {
            Some(chunk) => (
                decoder
                    .push(&chunk)
                    .map_err(|e| format!("读取响应流失败：{e}"))?,
                false,
            ),
            None => (
                decoder
                    .finish()
                    .map_err(|e| format!("读取响应流失败：{e}"))?,
                true,
            ),
        };
        for data in events {
            if data == "[DONE]" {
                return Ok(false);
            }
            on_data(&data)?;
        }
        if finished {
            return Ok(false);
        }
    }
}

fn push_delta(
    result: &mut StreamResult,
    kind: &str,
    delta: &str,
    on_event: &mut (dyn FnMut(StreamEvent) + Send),
) {
    if delta.is_empty() {
        return;
    }
    match kind {
        "thinking" => {
            // 合并到当前 thinking 块
            if !matches!(result.content.last(), Some(b) if b.get("type").and_then(Value::as_str) == Some("thinking"))
            {
                result
                    .content
                    .push(json!({ "type": "thinking", "thinking": "" }));
            }
            if let Some(block) = result.content.last_mut() {
                let current = block
                    .get("thinking")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                block["thinking"] = json!(format!("{current}{delta}"));
            }
            on_event(StreamEvent::ThinkingDelta(delta.to_string()));
        }
        _ => {
            if !matches!(result.content.last(), Some(b) if b.get("type").and_then(Value::as_str) == Some("text"))
            {
                result.content.push(json!({ "type": "text", "text": "" }));
            }
            if let Some(block) = result.content.last_mut() {
                let current = block
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                block["text"] = json!(format!("{current}{delta}"));
            }
            on_event(StreamEvent::TextDelta(delta.to_string()));
        }
    }
}

struct ToolCallAccum {
    id: String,
    name: String,
    arguments: String,
}

fn completions_usage(usage: &Value) -> Value {
    let prompt = usage
        .get("prompt_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let completion = usage
        .get("completion_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let cached = usage
        .pointer("/prompt_tokens_details/cached_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    json!({
        "input": prompt.saturating_sub(cached),
        "output": completion,
        "cacheRead": cached,
        "cacheWrite": 0,
        "totalTokens": prompt + completion,
    })
}

fn responses_usage(usage: &Value) -> Value {
    let input = usage
        .get("input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let output = usage
        .get("output_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let cached = usage
        .pointer("/input_tokens_details/cached_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    json!({
        "input": input.saturating_sub(cached),
        "output": output,
        "cacheRead": cached,
        "cacheWrite": 0,
        "totalTokens": input + output,
    })
}

fn finalize_tool_calls(calls: Vec<ToolCallAccum>, result: &mut StreamResult) {
    for call in calls {
        if call.name.is_empty() {
            continue;
        }
        let arguments = serde_json::from_str(&call.arguments)
            .unwrap_or_else(|_| json!({ "__invalidJson": call.arguments }));
        result.content.push(json!({
            "type": "toolCall",
            "id": call.id,
            "name": call.name,
            "arguments": arguments,
        }));
    }
}

fn map_finish_reason(
    reason: Option<&str>,
    has_tool_calls: bool,
    has_content: bool,
) -> (String, Option<String>) {
    match reason {
        Some("tool_calls") => ("toolUse".into(), None),
        Some("stop") | None if has_tool_calls => ("toolUse".into(), None),
        Some("stop") | None => ("stop".into(), None),
        Some("length") | Some("max_tokens") => ("length".into(), None),
        // 未知 finish_reason（如网关返回的 "other"）：已产出内容时按 stop 收尾，
        // 空响应才报错并交给外层重试。
        Some(other) if has_tool_calls || has_content => (
            if has_tool_calls { "toolUse" } else { "stop" }.into(),
            None,
        ),
        Some(other) => (
            "error".into(),
            Some(format!("provider finish_reason: {other}")),
        ),
    }
}

async fn stream_completions(
    http: &reqwest::Client,
    model: &ResolvedModel,
    api_key: &str,
    body: Value,
    session_id: Option<&str>,
    cancel: &Arc<AtomicBool>,
    on_event: &mut (dyn FnMut(StreamEvent) + Send),
) -> Result<StreamResult, String> {
    let url = crate::lyra_complete::join_url(&model.base_url, "chat/completions");
    let mut response = post_stream(http, &url, model, api_key, session_id, body, cancel).await?;
    let mut result = StreamResult::empty();
    let mut calls: Vec<ToolCallAccum> = Vec::new();
    let mut finish_reason: Option<String> = None;
    let cancelled = read_sse(&mut response, cancel, |data| {
        let Ok(value) = serde_json::from_str::<Value>(data) else {
            return Ok(());
        };
        // 非流式回退：偶发代理把 stream 请求按整包 JSON 返回
        if value.get("choices").is_some()
            && value.pointer("/choices/0/delta").is_none()
            && value.pointer("/choices/0/message").is_some()
        {
            let message = &value["choices"][0]["message"];
            if let Some(text) = message.get("content").and_then(Value::as_str) {
                push_delta(&mut result, "text", text, on_event);
            }
            if let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array) {
                for call in tool_calls {
                    calls.push(ToolCallAccum {
                        id: call
                            .get("id")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .into(),
                        name: call
                            .pointer("/function/name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .into(),
                        arguments: call
                            .pointer("/function/arguments")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .into(),
                    });
                }
            }
            if let Some(usage) = value.get("usage") {
                result.usage = completions_usage(usage);
            }
            finish_reason = value
                .pointer("/choices/0/finish_reason")
                .and_then(Value::as_str)
                .map(str::to_string);
            return Ok(());
        }
        if let Some(usage) = value.get("usage").filter(|u| u.is_object()) {
            result.usage = completions_usage(usage);
        }
        let delta = value.pointer("/choices/0/delta");
        if let Some(delta) = delta {
            if let Some(text) = delta.get("content").and_then(Value::as_str) {
                push_delta(&mut result, "text", text, on_event);
            }
            for field in ["reasoning_content", "reasoning", "reasoning_text"] {
                if let Some(thinking) = delta.get(field).and_then(Value::as_str) {
                    push_delta(&mut result, "thinking", thinking, on_event);
                    break;
                }
            }
            if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
                for call in tool_calls {
                    let index = call.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                    while calls.len() <= index {
                        calls.push(ToolCallAccum {
                            id: String::new(),
                            name: String::new(),
                            arguments: String::new(),
                        });
                    }
                    let accum = &mut calls[index];
                    if let Some(id) = call.get("id").and_then(Value::as_str) {
                        accum.id.push_str(id);
                    }
                    if let Some(name) = call.pointer("/function/name").and_then(Value::as_str) {
                        accum.name.push_str(name);
                    }
                    if let Some(args) = call.pointer("/function/arguments").and_then(Value::as_str)
                    {
                        accum.arguments.push_str(args);
                        on_event(StreamEvent::ToolArgsDelta {
                            index,
                            name: accum.name.clone(),
                            args: accum.arguments.clone(),
                        });
                    }
                }
            }
        }
        if let Some(reason) = value
            .pointer("/choices/0/finish_reason")
            .and_then(Value::as_str)
        {
            finish_reason = Some(reason.to_string());
        }
        if let Some(error) = value.get("error") {
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .or_else(|| error.as_str())
                .unwrap_or("provider stream error");
            return Err(format!("provider 错误：{message}"));
        }
        Ok(())
    })
    .await?;
    if cancelled {
        result.stop_reason = "aborted".into();
        return Ok(result);
    }
    let has_tool_calls = calls.iter().any(|call| !call.name.is_empty());
    let has_content = result.content.iter().any(|part| {
        matches!(
            part.get("type").and_then(Value::as_str),
            Some("text") | Some("thinking")
        )
    });
    finalize_tool_calls(calls, &mut result);
    let (stop, error) = map_finish_reason(finish_reason.as_deref(), has_tool_calls, has_content);
    result.stop_reason = stop;
    result.error_message = error;
    Ok(result)
}

async fn stream_responses(
    http: &reqwest::Client,
    model: &ResolvedModel,
    api_key: &str,
    body: Value,
    session_id: Option<&str>,
    cancel: &Arc<AtomicBool>,
    on_event: &mut (dyn FnMut(StreamEvent) + Send),
) -> Result<StreamResult, String> {
    let url = crate::lyra_complete::join_url(&model.base_url, "responses");
    let mut response = post_stream(http, &url, model, api_key, session_id, body, cancel).await?;
    let mut result = StreamResult::empty();
    let mut calls: Vec<ToolCallAccum> = Vec::new();
    let mut open_call: Option<usize> = None;
    let cancelled = read_sse(&mut response, cancel, |data| {
        let Ok(value) = serde_json::from_str::<Value>(data) else {
            return Ok(());
        };
        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match kind {
            "response.output_text.delta" | "response.text.delta" => {
                if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                    push_delta(&mut result, "text", delta, on_event);
                }
            }
            "response.reasoning_summary_text.delta"
            | "response.reasoning_text.delta"
            | "response.reasoning.delta" => {
                let delta = value
                    .get("delta")
                    .and_then(Value::as_str)
                    .or_else(|| value.pointer("/delta/text").and_then(Value::as_str));
                if let Some(delta) = delta {
                    push_delta(&mut result, "thinking", delta, on_event);
                }
            }
            "response.output_item.added" => {
                let item = value.get("item").cloned().unwrap_or(Value::Null);
                if item.get("type").and_then(Value::as_str) == Some("function_call") {
                    calls.push(ToolCallAccum {
                        id: item
                            .get("call_id")
                            .and_then(Value::as_str)
                            .or_else(|| item.get("id").and_then(Value::as_str))
                            .unwrap_or_default()
                            .into(),
                        name: item
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .into(),
                        arguments: item
                            .get("arguments")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .into(),
                    });
                    open_call = Some(calls.len() - 1);
                }
            }
            "response.function_call_arguments.delta" => {
                if let Some(index) = open_call {
                    if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                        calls[index].arguments.push_str(delta);
                        on_event(StreamEvent::ToolArgsDelta {
                            index,
                            name: calls[index].name.clone(),
                            args: calls[index].arguments.clone(),
                        });
                    }
                }
            }
            "response.output_item.done" => {
                let item = value.get("item").cloned().unwrap_or(Value::Null);
                if item.get("type").and_then(Value::as_str) == Some("function_call") {
                    if let Some(index) = open_call {
                        if let Some(arguments) = item.get("arguments").and_then(Value::as_str) {
                            if !arguments.is_empty() {
                                calls[index].arguments = arguments.to_string();
                            }
                        }
                        if calls[index].name.is_empty() {
                            if let Some(name) = item.get("name").and_then(Value::as_str) {
                                calls[index].name = name.to_string();
                            }
                        }
                    }
                    open_call = None;
                }
            }
            "response.completed" | "response.incomplete" => {
                if let Some(usage) = value.pointer("/response/usage") {
                    result.usage = responses_usage(usage);
                }
                if kind == "response.incomplete" {
                    result.stop_reason = "length".into();
                }
            }
            "response.failed" | "error" => {
                let message = value
                    .pointer("/response/error/message")
                    .or_else(|| value.pointer("/error/message"))
                    .or_else(|| value.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("provider stream error");
                return Err(format!("provider 错误：{message}"));
            }
            _ => {
                // 非流式整包回退
                if value.get("output").is_some() || value.get("output_text").is_some() {
                    parse_responses_object(&value, &mut result, &mut calls);
                }
            }
        }
        Ok(())
    })
    .await?;
    if cancelled {
        result.stop_reason = "aborted".into();
        return Ok(result);
    }
    let has_tool_calls = calls.iter().any(|call| !call.name.is_empty());
    finalize_tool_calls(calls, &mut result);
    if result.stop_reason.is_empty() {
        result.stop_reason = if has_tool_calls { "toolUse" } else { "stop" }.into();
    }
    Ok(result)
}

fn parse_responses_object(
    value: &Value,
    result: &mut StreamResult,
    calls: &mut Vec<ToolCallAccum>,
) {
    let items: Vec<Value> = if let Some(output) = value.get("output").and_then(Value::as_array) {
        output.clone()
    } else {
        vec![value.clone()]
    };
    for item in items {
        match item.get("type").and_then(Value::as_str) {
            Some("message") => {
                if let Some(parts) = item.get("content").and_then(Value::as_array) {
                    for part in parts {
                        if matches!(
                            part.get("type").and_then(Value::as_str),
                            Some("output_text") | Some("text")
                        ) {
                            if let Some(text) = part.get("text").and_then(Value::as_str) {
                                result.content.push(json!({ "type": "text", "text": text }));
                            }
                        }
                    }
                }
            }
            Some("function_call") => {
                calls.push(ToolCallAccum {
                    id: item
                        .get("call_id")
                        .and_then(Value::as_str)
                        .or_else(|| item.get("id").and_then(Value::as_str))
                        .unwrap_or_default()
                        .into(),
                    name: item
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .into(),
                    arguments: item
                        .get("arguments")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .into(),
                });
            }
            _ => {}
        }
    }
    if let Some(usage) = value.get("usage") {
        result.usage = responses_usage(usage);
    }
}

const STREAM_RETRY_DELAYS_MS: [u64; 2] = [250, 750];

fn is_retryable_stream_error(error: &str) -> bool {
    let message = error.to_ascii_lowercase();
    [
        "error decoding response body",
        "读取响应流失败",
        "unexpected eof",
        "connection reset",
        "connection closed",
        "connection error",
        "broken pipe",
        "incomplete message",
        "stream error",
        "request failed",
        "请求失败",
        "timeout",
        "timed out",
        "响应头等待超过",
        // SSE 空闲由 bridge 做“无新用户消息的继续”重试，避免这里与外层叠加。
        "http 429",
        "too many requests",
        "rate limit",
        "http 500",
        "http 502",
        "http 503",
        "http 504",
        "http2",
        "http/2",
    ]
    .iter()
    .any(|fragment| message.contains(fragment))
}

async fn stream_chat_once(
    http: &reqwest::Client,
    model: &ResolvedModel,
    api_key: &str,
    thinking_level: Option<&str>,
    system_prompt: &str,
    messages: &[Value],
    tools: &[Tool],
    session_id: Option<&str>,
    cancel: &Arc<AtomicBool>,
    on_event: &mut (dyn FnMut(StreamEvent) + Send),
) -> Result<StreamResult, String> {
    match model.api.as_str() {
        "openai-completions" => {
            let body = completions_body(
                model,
                system_prompt,
                messages,
                tools,
                thinking_level,
                session_id,
            );
            stream_completions(http, model, api_key, body, session_id, cancel, on_event).await
        }
        "openai-responses" => {
            let body = responses_body(
                model,
                system_prompt,
                messages,
                tools,
                thinking_level,
                session_id,
            );
            stream_responses(http, model, api_key, body, session_id, cancel, on_event).await
        }
        "anthropic-messages" => {
            let body = anthropic_body(model, system_prompt, messages, tools, thinking_level);
            stream_anthropic(http, model, api_key, body, cancel, on_event).await
        }
        other => Err(format!("Lyra 暂不支持协议 {other}")),
    }
}

/// 模型级 proxy 配置的 HTTP 客户端：按代理地址缓存并复用连接池。
/// 无协议前缀按 http 代理处理；URL 无法解析时退化为直连（与全局 lyra-proxy 一致）。
pub(crate) fn client_for_proxy(proxy: &str) -> reqwest::Client {
    static CLIENTS: OnceLock<Mutex<HashMap<String, reqwest::Client>>> = OnceLock::new();
    let clients = CLIENTS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut clients = clients.lock().unwrap();
    if let Some(client) = clients.get(proxy) {
        return client.clone();
    }
    let url = if proxy.contains("://") {
        proxy.to_string()
    } else {
        format!("http://{proxy}")
    };
    let mut builder = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .pool_max_idle_per_host(4)
        .tcp_keepalive(std::time::Duration::from_secs(20));
    if let Ok(parsed) = reqwest::Proxy::all(&url) {
        builder = builder.proxy(parsed);
    }
    let client = builder.build().unwrap_or_default();
    clients.insert(proxy.to_string(), client.clone());
    client
}

/// 一次流式模型调用。网络/响应体解码错误会在尚未向调用方发送任何增量时静默重试；
/// 已经发送增量后不重试，避免 UI、工具参数或会话内容重复。取消时返回 stop_reason = aborted。
pub async fn stream_chat(
    http: &reqwest::Client,
    model: &ResolvedModel,
    api_key: &str,
    thinking_level: Option<&str>,
    system_prompt: &str,
    messages: &[Value],
    tools: &[Tool],
    session_id: Option<&str>,
    cancel: &Arc<AtomicBool>,
    on_event: &mut (dyn FnMut(StreamEvent) + Send),
) -> Result<StreamResult, String> {
    // 模型在 config.jsonc 声明了 options.proxy 时改用按代理地址缓存的客户端；
    // 未声明则沿用调用方客户端（已含全局 lyra-proxy 设置）。
    let proxied;
    let http = match model.proxy.as_deref() {
        Some(proxy) => {
            proxied = client_for_proxy(proxy);
            &proxied
        }
        None => http,
    };
    let mut retry = 0;
    loop {
        let mut emitted = false;
        let result = stream_chat_once(
            http,
            model,
            api_key,
            thinking_level,
            system_prompt,
            messages,
            tools,
            session_id,
            cancel,
            &mut |event| {
                emitted = true;
                on_event(event);
            },
        )
        .await;

        match result {
            Err(_) if cancel.load(Ordering::SeqCst) => {
                let mut result = StreamResult::empty();
                result.stop_reason = "aborted".into();
                return Ok(result);
            }
            Err(error)
                if !emitted
                    && retry < STREAM_RETRY_DELAYS_MS.len()
                    && is_retryable_stream_error(&error) =>
            {
                let delay = STREAM_RETRY_DELAYS_MS[retry];
                retry += 1;
                tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                if cancel.load(Ordering::SeqCst) {
                    let mut result = StreamResult::empty();
                    result.stop_reason = "aborted".into();
                    return Ok(result);
                }
            }
            outcome => return outcome,
        }
    }
}

// ---------- anthropic-messages ----------

fn anthropic_thinking_budget(level: Option<&str>, max_output_tokens: u64) -> Option<u64> {
    let budget = match level.unwrap_or("medium") {
        "off" | "none" => return None,
        "minimal" => 1024,
        "low" => 4096,
        "medium" => 16384,
        "high" => 32768,
        "xhigh" | "max" => 65536,
        _ => 16384,
    };
    // Anthropic 要求 max_tokens > budget_tokens。
    let cap = max_output_tokens.saturating_sub(1024).max(1024);
    Some(budget.min(cap))
}

fn anthropic_messages(messages: &[Value], model: &ResolvedModel) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for message in messages {
        match message.get("role").and_then(Value::as_str) {
            Some("user") => {
                let mut parts = Vec::new();
                if let Some(content) = message.get("content") {
                    if let Some(text) = content.as_str() {
                        parts.push(json!({ "type": "text", "text": text }));
                    } else if let Some(blocks) = content.as_array() {
                        for block in blocks {
                            match block.get("type").and_then(Value::as_str) {
                                Some("text") => parts.push(json!({
                                    "type": "text",
                                    "text": block.get("text").and_then(Value::as_str).unwrap_or_default()
                                })),
                                Some("image") => parts.push(json!({
                                    "type": "image",
                                    "source": {
                                        "type": "base64",
                                        "media_type": block.get("mimeType").and_then(Value::as_str).unwrap_or("image/png"),
                                        "data": block.get("data").and_then(Value::as_str).unwrap_or_default(),
                                    }
                                })),
                                _ => {}
                            }
                        }
                    }
                }
                if !parts.is_empty() {
                    out.push(json!({ "role": "user", "content": parts }));
                }
            }
            Some("assistant") => {
                let content = message
                    .get("content")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let mut parts = Vec::new();
                for block in &content {
                    match block.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            let text = block
                                .get("text")
                                .and_then(Value::as_str)
                                .unwrap_or_default();
                            if !text.is_empty() {
                                parts.push(json!({ "type": "text", "text": text }));
                            }
                        }
                        Some("toolCall") => parts.push(json!({
                            "type": "tool_use",
                            "id": block.get("id").and_then(Value::as_str).unwrap_or_default(),
                            "name": block.get("name").and_then(Value::as_str).unwrap_or_default(),
                            "input": block.get("arguments").cloned().unwrap_or_else(|| json!({})),
                        })),
                        // thinking 缺签名无法回传，丢弃（Anthropic 允许历史里不带 thinking）
                        _ => {}
                    }
                }
                if !parts.is_empty() {
                    out.push(json!({ "role": "assistant", "content": parts }));
                }
            }
            Some("toolResult") => {
                let is_error = message.get("isError").and_then(Value::as_bool) == Some(true);
                out.push(json!({
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": message.get("toolCallId").and_then(Value::as_str).unwrap_or_default(),
                        "content": [{ "type": "text", "text": tool_result_text(message) }],
                        "is_error": is_error,
                    }],
                }));
                let images = tool_result_images(message, model);
                if !images.is_empty() {
                    let mut content = vec![json!({
                        "type": "text",
                        "text": "Screenshot returned by the browser tool."
                    })];
                    content.extend(images.into_iter().map(|image| json!({
                        "type": "image",
                        "source": {
                            "type": "base64",
                            "media_type": image.get("mimeType").and_then(Value::as_str).unwrap_or("image/png"),
                            "data": image.get("data").and_then(Value::as_str).unwrap_or_default(),
                        }
                    })));
                    out.push(json!({ "role": "user", "content": content }));
                }
            }
            _ => {}
        }
    }
    out
}

fn anthropic_body(
    model: &ResolvedModel,
    system_prompt: &str,
    messages: &[Value],
    tools: &[Tool],
    thinking_level: Option<&str>,
) -> Value {
    let mut body = json!({
        "model": model.id,
        "max_tokens": model.max_output_tokens,
        "messages": anthropic_messages(messages, model),
        "stream": true,
    });
    if !system_prompt.is_empty() {
        // 系统提示做短暂缓存，降低多轮重复计费
        body["system"] = json!([{
            "type": "text",
            "text": system_prompt,
            "cache_control": { "type": "ephemeral" },
        }]);
    }
    if !tools.is_empty() {
        let mut tool_defs: Vec<Value> = tools
            .iter()
            .map(|tool| {
                json!({
                    "name": tool.name,
                    "description": tool.description,
                    "input_schema": tool.parameters,
                })
            })
            .collect();
        if let Some(last) = tool_defs.last_mut() {
            last["cache_control"] = json!({ "type": "ephemeral" });
        }
        body["tools"] = Value::Array(tool_defs);
    }
    if model.reasoning {
        if let Some(budget) = anthropic_thinking_budget(thinking_level, model.max_output_tokens) {
            body["thinking"] = json!({ "type": "enabled", "budget_tokens": budget });
        }
    }
    body
}

fn anthropic_usage(usage: &Value, out: &mut Value) {
    let get = |key: &str| usage.get(key).and_then(Value::as_u64);
    if let Some(input) = get("input_tokens") {
        out["input"] = json!(input);
    }
    if let Some(output) = get("output_tokens") {
        out["output"] = json!(output);
    }
    if let Some(cache_read) = get("cache_read_input_tokens") {
        out["cacheRead"] = json!(cache_read);
    }
    if let Some(cache_write) = get("cache_creation_input_tokens") {
        out["cacheWrite"] = json!(cache_write);
    }
    let total = ["input", "output", "cacheRead", "cacheWrite"]
        .iter()
        .map(|key| out.get(key).and_then(Value::as_u64).unwrap_or(0))
        .sum::<u64>();
    out["totalTokens"] = json!(total);
}

fn map_anthropic_stop(
    reason: Option<&str>,
    has_tool_calls: bool,
    has_content: bool,
) -> (String, Option<String>) {
    match reason {
        Some("tool_use") => ("toolUse".into(), None),
        Some("end_turn") | Some("stop_sequence") => ("stop".into(), None),
        Some("max_tokens") => ("length".into(), None),
        None if has_tool_calls => ("toolUse".into(), None),
        None => ("stop".into(), None),
        // 未知 stop_reason：已产出内容时按 stop/toolUse 收尾，空响应才报错重试。
        Some(other) if has_tool_calls || has_content => (
            if has_tool_calls { "toolUse" } else { "stop" }.into(),
            None,
        ),
        Some(other) => (
            "error".into(),
            Some(format!("provider stop_reason: {other}")),
        ),
    }
}

async fn stream_anthropic(
    http: &reqwest::Client,
    model: &ResolvedModel,
    api_key: &str,
    body: Value,
    cancel: &Arc<AtomicBool>,
    on_event: &mut (dyn FnMut(StreamEvent) + Send),
) -> Result<StreamResult, String> {
    // Anthropic 约定 baseURL 不含版本段时追加 /v1（与官方 SDK 一致）。
    let base = model.base_url.trim_end_matches('/');
    let url = if base.ends_with("/v1") {
        format!("{base}/messages")
    } else {
        format!("{base}/v1/messages")
    };
    let oauth = api_key.starts_with("sk-ant-oat");
    let thinking_enabled = body.get("thinking").is_some();
    let mut request = http
        .post(&url)
        .header("content-type", "application/json")
        .header("accept", "text/event-stream")
        .header("anthropic-version", "2023-06-01")
        .json(&body);
    if !api_key.is_empty() {
        request = if oauth {
            request.bearer_auth(api_key)
        } else {
            request.header("x-api-key", api_key)
        };
    }
    // interleaved thinking / OAuth beta 头
    let mut betas: Vec<&str> = Vec::new();
    if thinking_enabled {
        betas.push("interleaved-thinking-2025-05-14");
    }
    if oauth {
        betas.push("oauth-2025-04-20");
    }
    if !betas.is_empty() {
        request = request.header("anthropic-beta", betas.join(","));
    }
    for (key, value) in &model.headers {
        if let Some(text) = value.as_str() {
            request = request.header(key.as_str(), text);
        }
    }
    let mut response = send_request(request, cancel).await?;
    let status = response.status();
    if !status.is_success() {
        let text = response.text().await.unwrap_or_default();
        let message = text.trim();
        return Err(if message.is_empty() {
            format!("HTTP {status}")
        } else {
            format!(
                "HTTP {status}：{}",
                message.chars().take(500).collect::<String>()
            )
        });
    }

    let mut result = StreamResult::empty();
    // 按 content block 索引聚合 tool_use 入参
    let mut blocks: std::collections::HashMap<u64, (String, String, String)> =
        std::collections::HashMap::new();
    // block 索引 → 本条消息内工具调用序号（与最终 content 中 toolCall 顺序一致）
    let mut block_ordinals: std::collections::HashMap<u64, usize> =
        std::collections::HashMap::new();
    let mut stop_reason: Option<String> = None;
    let cancelled = read_sse(&mut response, cancel, |data| {
        let Ok(value) = serde_json::from_str::<Value>(data) else {
            return Ok(());
        };
        match value.get("type").and_then(Value::as_str) {
            Some("message_start") => {
                if let Some(usage) = value.pointer("/message/usage") {
                    anthropic_usage(usage, &mut result.usage);
                }
            }
            Some("content_block_start") => {
                let index = value.get("index").and_then(Value::as_u64).unwrap_or(0);
                let block = &value["content_block"];
                if block.get("type").and_then(Value::as_str) == Some("tool_use") {
                    block_ordinals.insert(index, block_ordinals.len());
                    blocks.insert(
                        index,
                        (
                            block
                                .get("id")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                            block
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                            String::new(),
                        ),
                    );
                }
            }
            Some("content_block_delta") => {
                let delta = &value["delta"];
                match delta.get("type").and_then(Value::as_str) {
                    Some("text_delta") => {
                        if let Some(text) = delta.get("text").and_then(Value::as_str) {
                            push_delta(&mut result, "text", text, on_event);
                        }
                    }
                    Some("thinking_delta") => {
                        if let Some(text) = delta.get("thinking").and_then(Value::as_str) {
                            push_delta(&mut result, "thinking", text, on_event);
                        }
                    }
                    Some("input_json_delta") => {
                        let index = value.get("index").and_then(Value::as_u64).unwrap_or(0);
                        if let Some(partial) = delta.get("partial_json").and_then(Value::as_str) {
                            if let Some(entry) = blocks.get_mut(&index) {
                                entry.2.push_str(partial);
                                if let Some(ordinal) = block_ordinals.get(&index) {
                                    on_event(StreamEvent::ToolArgsDelta {
                                        index: *ordinal,
                                        name: entry.1.clone(),
                                        args: entry.2.clone(),
                                    });
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            Some("content_block_stop") => {
                let index = value.get("index").and_then(Value::as_u64).unwrap_or(0);
                if let Some((id, name, arguments)) = blocks.remove(&index) {
                    let args = if arguments.is_empty() {
                        json!({})
                    } else {
                        serde_json::from_str(&arguments)
                            .unwrap_or_else(|_| json!({ "__invalidJson": arguments }))
                    };
                    result.content.push(json!({
                        "type": "toolCall",
                        "id": id,
                        "name": name,
                        "arguments": args,
                    }));
                }
            }
            Some("message_delta") => {
                if let Some(reason) = value.pointer("/delta/stop_reason").and_then(Value::as_str) {
                    stop_reason = Some(reason.to_string());
                }
                if let Some(usage) = value.get("usage") {
                    anthropic_usage(usage, &mut result.usage);
                }
            }
            Some("error") => {
                let message = value
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("provider stream error");
                return Err(format!("Anthropic 流错误：{message}"));
            }
            _ => {}
        }
        Ok(())
    })
    .await?;

    if cancelled {
        result.stop_reason = "aborted".into();
        return Ok(result);
    }
    // 流意外结束时仍落盘未闭合的 tool_use
    let mut pending: Vec<_> = blocks.into_iter().collect();
    pending.sort_by_key(|(index, _)| *index);
    for (_, (id, name, arguments)) in pending {
        let args = serde_json::from_str(&arguments).unwrap_or_else(|_| json!({}));
        result.content.push(json!({
            "type": "toolCall",
            "id": id,
            "name": name,
            "arguments": args,
        }));
    }
    let has_tool_calls = result
        .content
        .iter()
        .any(|part| part.get("type").and_then(Value::as_str) == Some("toolCall"));
    let has_content = result
        .content
        .iter()
        .any(|part| part.get("type").and_then(Value::as_str) == Some("text"));
    let (stop, error) = map_anthropic_stop(stop_reason.as_deref(), has_tool_calls, has_content);
    result.stop_reason = stop;
    result.error_message = error;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lyra::config::ResolvedModel;
    use serde_json::Map;

    fn test_model(api: &str) -> ResolvedModel {
        ResolvedModel {
            provider: "p".into(),
            id: "m".into(),
            api: api.into(),
            base_url: "https://api.openai.com/v1".into(),
            headers: Map::new(),
            reasoning: true,
            thinking_format: None,
            max_tokens_field: "max_completion_tokens",
            context_window: 128_000,
            max_output_tokens: 32_000,
            service_tier: Some("flex".into()),
            temperature: Some(1.0),
            top_p: Some(0.95),
            supports_images: true,
            requires_reasoning_content: false,
            session_affinity_headers: false,
            session_affinity_format: "openai".into(),
            supports_long_cache_retention: true,
            supports_reasoning_effort: true,
            clear_thinking: None,
            extra_options: Map::new(),
            proxy: None,
        }
    }

    #[test]
    fn client_for_proxy_builds_caches_and_tolerates_invalid() {
        let _ = client_for_proxy("http://127.0.0.1:10808");
        let _ = client_for_proxy("http://127.0.0.1:10808"); // 命中缓存
        let _ = client_for_proxy("127.0.0.1:10809"); // 无协议前缀按 http 处理
        let _ = client_for_proxy("not a url"); // 无效代理退化为直连，不 panic
    }

    #[test]
    fn unknown_finish_reason_degrades_to_stop_when_content_exists() {
        assert_eq!(map_finish_reason(Some("other"), false, true).0, "stop");
        assert_eq!(map_finish_reason(Some("other"), true, true).0, "toolUse");
        let (stop, error) = map_finish_reason(Some("other"), false, false);
        assert_eq!(stop, "error");
        assert_eq!(error.as_deref(), Some("provider finish_reason: other"));
        assert_eq!(map_finish_reason(Some("stop"), false, false).0, "stop");
        assert_eq!(map_finish_reason(None, true, false).0, "toolUse");
    }

    #[test]
    fn transient_stream_decode_errors_are_retryable() {
        assert!(is_retryable_stream_error(
            "读取响应流失败：error decoding response body"
        ));
        assert!(is_retryable_stream_error("HTTP 503 Service Unavailable"));
        assert!(is_retryable_stream_error("connection reset by peer"));
        assert!(
            !is_retryable_stream_error("provider SSE 连续 90s 无数据"),
            "SSE 空闲应交给外层 continuation 重试"
        );
        assert!(!is_retryable_stream_error("HTTP 401 Unauthorized"));
        assert!(!is_retryable_stream_error("provider 错误：invalid request"));
    }

    #[test]
    fn completions_payload_injects_cache_fields() {
        let model = test_model("openai-completions");
        let body = completions_body(
            &model,
            "系统",
            &[json!({ "role": "user", "content": [{ "type": "text", "text": "你好" }] })],
            &[],
            Some("high"),
            Some("session-1"),
        );
        assert_eq!(body["prompt_cache_key"], "session-1");
        assert_eq!(body["prompt_cache_retention"], "24h");
        assert_eq!(body["service_tier"], "flex");
        assert_eq!(body["temperature"], 1.0);
        assert_eq!(body["top_p"], 0.95);
        assert_eq!(body["reasoning_effort"], "high");
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][1]["content"][0]["text"], "你好");
    }

    #[test]
    fn responses_payload_requests_reasoning_summary() {
        let model = test_model("openai-responses");
        let body = responses_body(
            &model,
            "系统",
            &[json!({ "role": "user", "content": [{ "type": "text", "text": "你好" }] })],
            &[],
            Some("high"),
            None,
        );
        assert_eq!(body["reasoning"]["effort"], "high");
        assert_eq!(body["reasoning"]["summary"], "auto");
    }

    #[test]
    fn responses_payload_still_requests_summary_without_effort() {
        let model = test_model("openai-responses");
        let body = responses_body(&model, "系统", &[], &[], None, None);
        assert!(body["reasoning"].get("effort").is_none());
        assert_eq!(body["reasoning"]["summary"], "auto");
    }

    #[test]
    fn responses_payload_keeps_reasoning_off() {
        let model = test_model("openai-responses");
        let body = responses_body(&model, "系统", &[], &[], Some("off"), None);
        assert!(body.get("reasoning").is_none());
    }

    #[test]
    fn text_only_model_downgrades_legacy_images() {
        let mut model = test_model("openai-completions");
        model.supports_images = false;
        let messages = vec![json!({ "role": "user", "content": [
            { "type": "text", "text": "看图" },
            { "type": "image", "mimeType": "image/png", "data": "QUJD" },
            { "type": "image_url", "image_url": { "url": "data:image/png;base64,QUJD" } }
        ]})];
        let out = completions_messages("sys", &messages, &model);
        assert_eq!(out[1]["content"][1]["type"], "text");
        assert_eq!(out[1]["content"][2]["type"], "text");
    }

    #[test]
    fn completions_messages_convert_tool_flow() {
        let model = test_model("openai-completions");
        let messages = vec![
            json!({ "role": "user", "content": [{ "type": "text", "text": "任务" }] }),
            json!({ "role": "assistant", "content": [
                { "type": "text", "text": "我来读一下" },
                { "type": "toolCall", "id": "call-1", "name": "read", "arguments": { "path": "a.rs" } }
            ] }),
            json!({ "role": "toolResult", "toolCallId": "call-1", "toolName": "read",
                    "content": [{ "type": "text", "text": "文件内容" }] }),
        ];
        let out = completions_messages("sys", &messages, &model);
        assert_eq!(out[2]["role"], "assistant");
        assert_eq!(out[2]["tool_calls"][0]["id"], "call-1");
        assert_eq!(
            out[2]["tool_calls"][0]["function"]["arguments"],
            "{\"path\":\"a.rs\"}"
        );
        assert_eq!(out[3]["role"], "tool");
        assert_eq!(out[3]["content"], "文件内容");
    }

    #[test]
    fn multimodal_tool_results_keep_browser_screenshots() {
        let model = test_model("openai-completions");
        let image_path = std::env::temp_dir().join(format!(
            "nova-browser-provider-test-{}.png",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&image_path, b"ABC").unwrap();
        let messages = vec![json!({
            "role": "toolResult", "toolCallId": "shot", "toolName": "browser",
            "content": [{ "type": "text", "text": "当前页面" }],
            "details": { "imagePath": image_path }
        })];
        let out = completions_messages("sys", &messages, &model);
        let _ = std::fs::remove_file(image_path);
        assert_eq!(out[1]["role"], "tool");
        assert_eq!(out[2]["content"][0]["type"], "text");
        assert_eq!(out[2]["content"][1]["type"], "image_url");
        assert_eq!(
            out[2]["content"][1]["image_url"]["url"],
            "data:image/png;base64,QUJD"
        );
    }

    #[test]
    fn responses_input_flattens_tool_calls() {
        let messages = vec![
            json!({ "role": "assistant", "content": [
                { "type": "thinking", "thinking": "想一下" },
                { "type": "text", "text": "读文件" },
                { "type": "toolCall", "id": "call-9", "name": "bash", "arguments": { "command": "ls" } }
            ] }),
            json!({ "role": "toolResult", "toolCallId": "call-9", "toolName": "bash",
                    "content": [{ "type": "text", "text": "ok" }] }),
        ];
        let model = test_model("openai-responses");
        let out = responses_input(&messages, &model);
        assert_eq!(out[0]["type"], "message");
        assert_eq!(out[0]["content"][0]["type"], "output_text");
        assert_eq!(out[1]["type"], "function_call");
        assert_eq!(out[1]["call_id"], "call-9");
        assert_eq!(out[2]["type"], "function_call_output");
    }

    #[test]
    fn usage_splits_cached_tokens() {
        let usage = completions_usage(&json!({
            "prompt_tokens": 1000, "completion_tokens": 50,
            "prompt_tokens_details": { "cached_tokens": 800 }
        }));
        assert_eq!(usage["input"], 200);
        assert_eq!(usage["cacheRead"], 800);
        assert_eq!(usage["totalTokens"], 1050);
    }

    #[test]
    fn anthropic_body_converts_tool_flow_and_cache() {
        let model = test_model("anthropic-messages");
        let tool = Tool {
            name: "read",
            description: "读文件".into(),
            parameters: json!({ "type": "object" }),
        };
        let messages = vec![
            json!({ "role": "user", "content": [
                { "type": "text", "text": "看这张图" },
                { "type": "image", "mimeType": "image/png", "data": "QUJD" }
            ] }),
            json!({ "role": "assistant", "content": [
                { "type": "thinking", "thinking": "想一下" },
                { "type": "text", "text": "读文件" },
                { "type": "toolCall", "id": "toolu-1", "name": "read", "arguments": { "path": "a.rs" } }
            ] }),
            json!({ "role": "toolResult", "toolCallId": "toolu-1", "toolName": "read",
                    "isError": false, "content": [{ "type": "text", "text": "内容" }] }),
        ];
        let body = anthropic_body(
            &model,
            "系统提示",
            &messages,
            std::slice::from_ref(&tool),
            Some("high"),
        );
        assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
        assert_eq!(body["tools"][0]["name"], "read");
        assert_eq!(body["tools"][0]["cache_control"]["type"], "ephemeral");
        assert_eq!(body["thinking"]["budget_tokens"], 32768.min(32_000 - 1024));
        let out = &body["messages"];
        assert_eq!(out[0]["content"][1]["source"]["media_type"], "image/png");
        // thinking 无签名，历史里丢弃
        assert_eq!(out[1]["content"][0]["type"], "text");
        assert_eq!(out[1]["content"][1]["type"], "tool_use");
        assert_eq!(out[1]["content"][1]["input"]["path"], "a.rs");
        assert_eq!(out[2]["content"][0]["type"], "tool_result");
        assert_eq!(out[2]["content"][0]["tool_use_id"], "toolu-1");
    }

    #[test]
    fn zai_thinking_sends_clear_thinking_and_suppresses_reasoning_content() {
        let mut model = test_model("openai-completions");
        model.thinking_format = Some("zai".into());
        model.clear_thinking = Some(true);
        let mut body = json!({});
        apply_reasoning_completions(&mut body, &model, Some("high"));
        assert_eq!(body["thinking"], json!({ "type": "enabled", "clear_thinking": true }));
        // GLM-5.3 思考深度只认 reasoning_effort。
        assert_eq!(body["reasoning_effort"], json!("high"));

        // Some(false)：Preserved Thinking。
        model.clear_thinking = Some(false);
        let mut body = json!({});
        apply_reasoning_completions(&mut body, &model, Some("max"));
        assert_eq!(
            body["thinking"],
            json!({ "type": "enabled", "clear_thinking": false })
        );

        // None：交给端点默认值，不造多余字段。
        model.clear_thinking = None;
        let mut body = json!({});
        apply_reasoning_completions(&mut body, &model, Some("off"));
        assert_eq!(body["thinking"], json!({ "type": "disabled" }));
    }

    #[test]
    fn clear_thinking_applies_regardless_of_thinking_format() {
        // 无 thinking_format：只新建 thinking 对象带 clear_thinking，reasoning_effort 照旧。
        let mut model = test_model("openai-completions");
        model.clear_thinking = Some(true);
        let mut body = json!({});
        apply_reasoning_completions(&mut body, &model, Some("high"));
        assert_eq!(body["thinking"], json!({ "clear_thinking": true }));
        assert_eq!(body["reasoning_effort"], json!("high"));

        model.thinking_format = Some("qwen".into());
        let mut body = json!({});
        apply_reasoning_completions(&mut body, &model, Some("high"));
        assert_eq!(body["enable_thinking"], json!(true));
        assert_eq!(body["thinking"], json!({ "clear_thinking": true }));
    }

    #[test]
    fn qwen_and_kimi_styles_send_levels() {
        // qwen 中转：enable_thinking 之外默认也发档位，可显式关掉。
        let mut model = test_model("openai-completions");
        model.thinking_format = Some("qwen".into());
        let mut body = json!({});
        apply_reasoning_completions(&mut body, &model, Some("high"));
        assert_eq!(body["enable_thinking"], json!(true));
        assert_eq!(body["reasoning_effort"], json!("high"));

        model.supports_reasoning_effort = false;
        let mut body = json!({});
        apply_reasoning_completions(&mut body, &model, Some("high"));
        assert!(body.get("reasoning_effort").is_none());

        // kimi 风格：只发 reasoning_effort，不造 thinking / enable_thinking。
        model.thinking_format = Some("kimi".into());
        let mut body = json!({});
        apply_reasoning_completions(&mut body, &model, Some("max"));
        assert_eq!(body["reasoning_effort"], json!("max"));
        assert!(body.get("thinking").is_none());
        assert!(body.get("enable_thinking").is_none());
    }

    #[test]
    fn anthropic_thinking_budget_caps_below_max_tokens() {
        assert_eq!(anthropic_thinking_budget(Some("off"), 32_000), None);
        assert_eq!(
            anthropic_thinking_budget(Some("medium"), 32_000),
            Some(16384)
        );
        assert_eq!(anthropic_thinking_budget(Some("xhigh"), 8_000), Some(6_976));
        assert_eq!(anthropic_thinking_budget(None, 32_000), Some(16384));
    }

    #[test]
    fn anthropic_stop_and_usage_mapping() {
        assert_eq!(map_anthropic_stop(Some("tool_use"), false, false).0, "toolUse");
        assert_eq!(map_anthropic_stop(Some("end_turn"), false, false).0, "stop");
        assert_eq!(map_anthropic_stop(Some("max_tokens"), false, false).0, "length");
        assert_eq!(map_anthropic_stop(None, true, false).0, "toolUse");
        // 未知 stop_reason：有内容时收尾，空响应才报错（可重试）。
        assert_eq!(map_anthropic_stop(Some("other"), false, true).0, "stop");
        assert_eq!(map_anthropic_stop(Some("other"), true, true).0, "toolUse");
        let (stop, error) = map_anthropic_stop(Some("refusal"), false, false);
        assert_eq!(stop, "error");
        assert_eq!(error.as_deref(), Some("provider stop_reason: refusal"));
        let mut usage = json!({});
        anthropic_usage(
            &json!({ "input_tokens": 100, "cache_read_input_tokens": 900, "cache_creation_input_tokens": 50 }),
            &mut usage,
        );
        anthropic_usage(&json!({ "output_tokens": 12 }), &mut usage);
        assert_eq!(usage["input"], 100);
        assert_eq!(usage["cacheRead"], 900);
        assert_eq!(usage["cacheWrite"], 50);
        assert_eq!(usage["output"], 12);
        assert_eq!(usage["totalTokens"], 1062);
    }
}
