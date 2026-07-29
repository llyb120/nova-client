//! Native Vega agent runtime — the Rust replacement for the node alkaid bridge.
//!
//! This wires `pi_core` (the ported, parity-tested deterministic agent core)
//! into the protocol the Tauri backend forwards to the UI. The LLM provider
//! transport is injected as a `StreamFn` closure — the single boundary excluded
//! from deterministic parity ("排除大模型"). Everything else (system prompt,
//! tools, the agent loop, steering, and protocol item assembly) runs natively
//! with no node dependency.
//!
//! Integration point: `sdk_runtime` can call `run_native_turn` in place of
//! spawning `alkaid-bridge.mjs`, feeding the returned `items` through the same
//! event path the `AlkaidAdapter` already parses.
//!
//! This module is compiled and wired into `nova_lib` but not yet invoked; the
//! final call site lands with the provider transport.
#![allow(dead_code)]

use pi_core::agent::{Agent, AgentState, StreamFn, StreamTurn, ToolFn};
use pi_core::bridge::ProtocolAccumulator;
use serde_json::{json, Value};

use crate::vega_provider::{stream_turn, ProviderConfig};

/// Configuration for a native Vega turn.
pub struct NativeTurnConfig {
    pub system_prompt: String,
    pub model: Value,
    pub tools: Vec<Value>,
    pub history: Vec<Value>,
    pub session_id: Option<String>,
    /// Stands in for `Date.now()`; pass a fixed value for reproducible runs.
    pub timestamp: u64,
    /// Mirrors alkaid's `toolExecution: "parallel"`.
    pub parallel_tools: bool,
}

/// The assembled result of a native turn.
pub struct NativeTurnOutput {
    /// Protocol items (`{ "type": "item", "item": ... }`) in emission order.
    pub items: Vec<Value>,
    /// Merged token usage across the turn's assistant messages.
    pub usage: Option<Value>,
    /// The final transcript (prior history plus new messages).
    pub messages: Vec<Value>,
}

/// Run one native Vega turn.
///
/// `stream_fn` is the injected LLM transport; `tool_fn` executes tools natively.
/// The agent's emitted events are replayed through a `ProtocolAccumulator` to
/// produce the protocol item stream the backend expects.
pub fn run_native_turn(
    config: NativeTurnConfig,
    prompt_text: &str,
    stream_fn: &mut StreamFn,
    tool_fn: &mut ToolFn,
) -> NativeTurnOutput {
    let state = AgentState::new(
        &config.system_prompt,
        config.model,
        config.tools,
        config.history,
    );
    let mut agent = Agent::new(state, "all", "one-at-a-time");
    agent.parallel_tools = config.parallel_tools;
    agent.session_id = config.session_id;

    let events = agent.prompt(
        &Value::String(prompt_text.to_string()),
        &[],
        config.timestamp,
        stream_fn,
        tool_fn,
    );

    let mut accumulator = ProtocolAccumulator::new();
    for event in &events {
        accumulator.on_event(event);
    }
    let usage = accumulator.usage.clone();
    let items = accumulator.finish();

    NativeTurnOutput {
        items,
        usage,
        messages: agent.state.messages.clone(),
    }
}

/// Build a `StreamTurn` carrying a provider error as an `error`-stop assistant
/// message, so the loop terminates and surfaces the message like node does.
fn error_turn(error: &str, provider: &ProviderConfig) -> StreamTurn {
    let message = json!({
        "role": "assistant",
        "content": [{ "type": "text", "text": "" }],
        "api": provider.api,
        "provider": provider.provider,
        "model": provider.model_id,
        "stopReason": "error",
        "errorMessage": error,
    });
    StreamTurn {
        events: vec![
            json!({ "type": "start", "partial": message }),
            json!({ "type": "done" }),
        ],
        result: message,
    }
}

/// Run one native Vega turn against a live provider.
///
/// The synchronous, parity-tested agent loop runs on a blocking thread; its
/// `StreamFn` blocks on the async provider transport via the current tokio
/// handle (legal inside `spawn_blocking`). `native_tools` supplies the `ToolFn`.
///
/// This is the integration seam for replacing the node bridge. It compiles and
/// is wired in, but must be verified against a real provider before the node
/// path is removed (the transport is the excluded, unverified LLM boundary).
pub async fn run_native_turn_async(
    client: reqwest::Client,
    provider: ProviderConfig,
    config: NativeTurnConfig,
    prompt_text: String,
    native_tools: pi_core::tools::NativeTools,
) -> Result<NativeTurnOutput, String> {
    let handle = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || {
        let session_id = config.session_id.clone();
        let mut stream_fn = move |_index: usize, llm_context: &Value| -> StreamTurn {
            let messages = llm_context
                .get("messages")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let system_prompt = llm_context
                .get("systemPrompt")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let tools = llm_context
                .get("tools")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let client = client.clone();
            let provider_for_call = provider.clone();
            let provider_for_error = provider.clone();
            let session_id = session_id.clone();
            handle
                .block_on(async move {
                    stream_turn(
                        &client,
                        &provider_for_call,
                        &system_prompt,
                        &messages,
                        &tools,
                        session_id.as_deref(),
                    )
                    .await
                })
                .unwrap_or_else(|error| error_turn(&error, &provider_for_error))
        };
        let mut tool_fn = move |name: &str, args: &Value| -> (Value, bool) {
            native_tools.execute(name, args)
        };
        Ok::<NativeTurnOutput, String>(run_native_turn(
            config,
            &prompt_text,
            &mut stream_fn,
            &mut tool_fn,
        ))
    })
    .await
    .map_err(|error| format!("native Vega turn panicked: {error}"))?
}
