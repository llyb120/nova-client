//! Async LLM provider transport for the native Vega path.
//!
//! This is the one boundary excluded from deterministic parity ("排除大模型"):
//! it talks to a live provider over HTTP/SSE. The deterministic request building
//! and SSE folding live in `pi_core::provider` (unit-tested); this module adds
//! the thin async HTTP layer using `reqwest` and the shared `SseDecoder`.
//!
//! Currently implements `openai-completions` (Chat Completions),
//! `anthropic-messages`, `openai-responses`, and `google-generative-ai`.
//!
//! The transport collects the full stream and returns a `StreamTurn`; the agent
//! loop then replays the events. Progressive per-token UI streaming would
//! require restructuring the (synchronous, parity-tested) loop and is deferred.

use pi_core::agent::StreamTurn;
use pi_core::payload::{
    clamp_openai_payload_tool_outputs, inject_openai_prompt_cache_key,
};
use pi_core::provider::{build_openai_chat_request, OpenAiChatAccumulator};
use pi_core::provider_anthropic::{build_anthropic_request, AnthropicAccumulator};
use pi_core::provider_google::{build_google_request, GoogleAccumulator};
use pi_core::provider_responses::{build_openai_responses_request, ResponsesAccumulator};
use pi_core::text::OPENAI_TOOL_OUTPUT_SAFE_MAX_CHARS;
use serde_json::{json, Value};

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
    /// Per-million-token cost rates (input/output/cacheRead/cacheWrite).
    pub cost: Value,
    /// Model output token limit (`limit.output`).
    pub max_tokens: u64,
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
    // Port of alkaid's provider retry: transient/network errors are retried with
    // backoff delays [1000, 3000]ms (up to 2 retries), matching
    // `runAlkaidPromptWithRetry` + `DEFAULT_PROVIDER_RETRY_DELAYS_MS`.
    const RETRY_DELAYS_MS: [u64; 2] = [1000, 3000];
    let mut attempt = 0;
    loop {
        match stream_turn_once(client, config, system_prompt, messages, tools, session_id).await {
            Ok(turn) => return Ok(turn),
            Err(error) => {
                if attempt < RETRY_DELAYS_MS.len() && is_retryable_provider_error(&error) {
                    tokio::time::sleep(std::time::Duration::from_millis(RETRY_DELAYS_MS[attempt]))
                        .await;
                    attempt += 1;
                    continue;
                }
                return Err(error);
            }
        }
    }
}

/// Port of `isRetryableAlkaidProviderError`: substring match on the error text.
pub fn is_retryable_provider_error(error: &str) -> bool {
    const FRAGMENTS: &[&str] = &[
        "terminated",
        "fetch failed",
        "connection error",
        "socket hang up",
        "econnreset",
        "etimedout",
        "econnaborted",
        "epipe",
        "request timed out",
        "und_err_socket",
        "premature close",
        "other side closed",
        "network connection lost",
        "stream ended before a terminal response event",
        "stream ended without finish_reason",
        "idle timeout",
        "429",
        "too many requests",
        "rate limit",
    ];
    let message = error.to_lowercase();
    FRAGMENTS.iter().any(|fragment| message.contains(fragment))
}

async fn stream_turn_once(
    client: &reqwest::Client,
    config: &ProviderConfig,
    system_prompt: &str,
    messages: &[Value],
    tools: &[Value],
    session_id: Option<&str>,
) -> Result<StreamTurn, String> {
    let mut turn = match config.api.as_str() {
        "openai-completions" => {
            stream_openai_chat(client, config, system_prompt, messages, tools, session_id).await
        }
        "anthropic-messages" => {
            stream_anthropic(client, config, system_prompt, messages, tools, session_id).await
        }
        "openai-responses" => {
            stream_openai_responses(client, config, system_prompt, messages, tools, session_id)
                .await
        }
        "google-generative-ai" => {
            stream_google(client, config, system_prompt, messages, tools).await
        }
        other => Err(format!(
            "native Vega transport does not yet implement provider api: {other}"
        )),
    }?;
    // Attach token cost (port of pi-ai `calculateCost`) to the final usage.
    attach_cost(&mut turn.result, &config.cost);
    Ok(turn)
}

/// Port of pi-ai `calculateCost`: fill `usage.cost` from per-million-token rates.
pub fn attach_cost(message: &mut Value, cost_rates: &Value) {
    let rate = |key: &str| cost_rates.get(key).and_then(Value::as_f64).unwrap_or(0.0);
    let (input, output, cache_read, cache_write) = {
        let usage = message.get("usage");
        (
            usage.and_then(|u| u.get("input")).and_then(Value::as_f64).unwrap_or(0.0),
            usage.and_then(|u| u.get("output")).and_then(Value::as_f64).unwrap_or(0.0),
            usage.and_then(|u| u.get("cacheRead")).and_then(Value::as_f64).unwrap_or(0.0),
            usage.and_then(|u| u.get("cacheWrite")).and_then(Value::as_f64).unwrap_or(0.0),
        )
    };
    let input_cost = input / 1_000_000.0 * rate("input");
    let output_cost = output / 1_000_000.0 * rate("output");
    let cache_read_cost = cache_read / 1_000_000.0 * rate("cacheRead");
    let cache_write_cost = cache_write / 1_000_000.0 * rate("cacheWrite");
    let total = input_cost + output_cost + cache_read_cost + cache_write_cost;
    message["usage"]["cost"] = json!({
        "input": input_cost,
        "output": output_cost,
        "cacheRead": cache_read_cost,
        "cacheWrite": cache_write_cost,
        "total": total,
    });
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

/// Anthropic Messages transport: POST `{baseUrl}/v1/messages`, SSE with a
/// `type` field per event. Auth via `x-api-key` + `anthropic-version`.
async fn stream_anthropic(
    client: &reqwest::Client,
    config: &ProviderConfig,
    system_prompt: &str,
    messages: &[Value],
    tools: &[Value],
    session_id: Option<&str>,
) -> Result<StreamTurn, String> {
    let model = serde_json::json!({
        "id": config.model_id,
        "provider": config.provider,
        "api": config.api,
    });
    let body = build_anthropic_request(&model, system_prompt, messages, tools, session_id, config.max_tokens);

    let url = format!("{}/v1/messages", config.base_url.trim_end_matches('/'));
    let mut request = client
        .post(&url)
        .header("anthropic-version", "2023-06-01")
        .json(&body);
    if let Some(key) = &config.api_key {
        request = request.header("x-api-key", key);
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
        AnthropicAccumulator::new(&config.model_id, &config.provider, &config.api);

    let feed = |accumulator: &mut AnthropicAccumulator, events: Vec<String>| {
        for event in events {
            let trimmed = event.trim();
            if trimmed.is_empty() || trimmed == "[DONE]" {
                continue;
            }
            if let Ok(json) = decode_sse_json(trimmed) {
                accumulator.add_event(&json);
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

/// OpenAI Responses transport: POST `{baseUrl}/responses`, SSE with a `type`
/// field per event. Bearer auth.
async fn stream_openai_responses(
    client: &reqwest::Client,
    config: &ProviderConfig,
    system_prompt: &str,
    messages: &[Value],
    tools: &[Value],
    session_id: Option<&str>,
) -> Result<StreamTurn, String> {
    let model = serde_json::json!({
        "id": config.model_id,
        "provider": config.provider,
        "api": config.api,
    });
    let body = build_openai_responses_request(&model, system_prompt, messages, tools, session_id, config.max_tokens);

    let url = format!("{}/responses", config.base_url.trim_end_matches('/'));
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
        ResponsesAccumulator::new(&config.model_id, &config.provider, &config.api);

    let feed = |accumulator: &mut ResponsesAccumulator, events: Vec<String>| {
        for event in events {
            let trimmed = event.trim();
            if trimmed.is_empty() || trimmed == "[DONE]" {
                continue;
            }
            if let Ok(json) = decode_sse_json(trimmed) {
                accumulator.add_event(&json);
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

/// Google Generative AI transport: POST
/// `{baseUrl}/v1beta/models/{model}:streamGenerateContent?alt=sse`, SSE chunks.
/// Auth via `x-goog-api-key`.
async fn stream_google(
    client: &reqwest::Client,
    config: &ProviderConfig,
    system_prompt: &str,
    messages: &[Value],
    tools: &[Value],
) -> Result<StreamTurn, String> {
    let model = serde_json::json!({
        "id": config.model_id,
        "provider": config.provider,
        "api": config.api,
    });
    let body = build_google_request(&model, system_prompt, messages, tools, config.max_tokens);
    // The request builder returns { model, contents, config }; the HTTP body is
    // { contents, ...config } with systemInstruction/tools/thinkingConfig lifted.
    let contents = body.get("contents").cloned().unwrap_or(serde_json::json!([]));
    let config_obj = body.get("config").cloned().unwrap_or(serde_json::json!({}));
    let mut http_body = serde_json::json!({ "contents": contents });
    if let (Some(obj), Some(cfg)) = (http_body.as_object_mut(), config_obj.as_object()) {
        for (key, value) in cfg {
            obj.insert(key.clone(), value.clone());
        }
    }

    let base = config.base_url.trim_end_matches('/');
    let url = if base.contains("/v1beta") || base.contains("/v1") {
        format!("{}/models/{}:streamGenerateContent?alt=sse", base, config.model_id)
    } else {
        format!("{}/v1beta/models/{}:streamGenerateContent?alt=sse", base, config.model_id)
    };
    let mut request = client.post(&url).json(&http_body);
    if let Some(key) = &config.api_key {
        request = request.header("x-goog-api-key", key);
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
        GoogleAccumulator::new(&config.model_id, &config.provider, &config.api);

    let feed = |accumulator: &mut GoogleAccumulator, events: Vec<String>| {
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

#[cfg(test)]
mod tests {
    use super::is_retryable_provider_error;

    #[test]
    fn classifies_retryable_errors() {
        assert!(is_retryable_provider_error("request failed: 429 Too Many Requests"));
        assert!(is_retryable_provider_error("connection reset (ECONNRESET)"));
        assert!(is_retryable_provider_error("stream ended before a terminal response event"));
        assert!(is_retryable_provider_error("Rate limit exceeded"));
        assert!(!is_retryable_provider_error("Vega provider error 401: unauthorized"));
        assert!(!is_retryable_provider_error("invalid api key"));
    }
}
