//! Async LLM provider transport for the native Vega path.
//!
//! This is the one boundary excluded from deterministic parity ("排除大模型"):
//! it talks to a live provider over HTTP/SSE. The deterministic request building
//! and SSE folding live in `pi_core::provider` (unit-tested); this module adds
//! the thin async HTTP layer using `reqwest` and the shared `SseDecoder`.
//!
//! Currently implements `openai-completions` (Chat Completions), the dominant
//! Vega provider API. Other protocols (`openai-responses`, `anthropic-messages`,
//! `google-generative-ai`) are extension points returning an explicit error.
//!
//! The transport collects the full stream and returns a `StreamTurn`; the agent
//! loop then replays the events. Progressive per-token UI streaming would
//! require restructuring the (synchronous, parity-tested) loop and is deferred.

use pi_core::agent::StreamTurn;
use pi_core::payload::{
    clamp_openai_payload_tool_outputs, inject_openai_prompt_cache_key,
};
use pi_core::provider::{build_openai_chat_request, OpenAiChatAccumulator};
use pi_core::text::OPENAI_TOOL_OUTPUT_SAFE_MAX_CHARS;
use serde_json::Value;

use crate::http_stream::{decode_sse_json, SseDecoder};

/// Provider endpoint configuration resolved from the Vega config.
#[derive(Clone)]
pub struct ProviderConfig {
    /// Protocol: `openai-completions`, `openai-responses`, `anthropic-messages`,
    /// `google-generative-ai`.
    pub api: String,
    pub base_url: String,
    pub model_id: String,
    pub provider: String,
    pub api_key: Option<String>,
}

/// Run one provider turn and return the collected stream.
pub async fn stream_turn(
    client: &reqwest::Client,
    config: &ProviderConfig,
    system_prompt: &str,
    messages: &[Value],
    tools: &[Value],
    session_id: Option<&str>,
) -> Result<StreamTurn, String> {
    match config.api.as_str() {
        "openai-completions" => {
            stream_openai_chat(client, config, system_prompt, messages, tools, session_id).await
        }
        other => Err(format!(
            "native Vega transport does not yet implement provider api: {other}"
        )),
    }
}

async fn stream_openai_chat(
    client: &reqwest::Client,
    config: &ProviderConfig,
    system_prompt: &str,
    messages: &[Value],
    tools: &[Value],
    session_id: Option<&str>,
) -> Result<StreamTurn, String> {
    let mut body = build_openai_chat_request(&config.model_id, system_prompt, messages, tools);
    // Mirror alkaid's onPayload transforms for OpenAI payloads.
    if let Some(with_cache) = inject_openai_prompt_cache_key(&body, session_id) {
        body = with_cache;
    }
    if let Some(clamped) = clamp_openai_payload_tool_outputs(&body, OPENAI_TOOL_OUTPUT_SAFE_MAX_CHARS)
    {
        body = clamped;
    }

    let url = format!(
        "{}/chat/completions",
        config.base_url.trim_end_matches('/')
    );
    let mut request = client.post(&url).json(&body);
    if let Some(key) = &config.api_key {
        request = request.bearer_auth(key);
    }
    let mut response = request
        .send()
        .await
        .map_err(|error| format!("Vega provider request failed: {error}"))?;

    let status = response.status();
    if !status.is_success() {
        let text = response.text().await.unwrap_or_default();
        return Err(format!("Vega provider error {status}: {text}"));
    }

    let mut decoder = SseDecoder::new();
    let mut accumulator =
        OpenAiChatAccumulator::new(&config.model_id, &config.provider, &config.api);

    let feed = |accumulator: &mut OpenAiChatAccumulator, events: Vec<String>| {
        for event in events {
            let trimmed = event.trim();
            if trimmed.is_empty() || trimmed == "[DONE]" {
                continue;
            }
            if let Ok(json) = decode_sse_json(trimmed) {
                accumulator.add_chunk(&json);
            }
        }
    };

    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| format!("Vega provider stream error: {error}"))?
    {
        let events = decoder.push(&chunk).map_err(|error| error.to_string())?;
        feed(&mut accumulator, events);
    }
    let events = decoder.finish().map_err(|error| error.to_string())?;
    feed(&mut accumulator, events);

    Ok(accumulator.finish())
}
