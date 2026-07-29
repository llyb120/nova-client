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

use pi_core::agent::{Agent, AgentState, StreamFn, ToolFn};
use pi_core::bridge::ProtocolAccumulator;
use serde_json::Value;

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
