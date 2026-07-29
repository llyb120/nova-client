//! The agent loop state machine, ported from `runAgentLoop`/`runLoop` in
//! `pi-agent-core/dist/agent-loop.js`.
//!
//! This is a faithful-but-simplified port for differential testing: the LLM
//! boundary returns a complete assistant message (no streaming deltas), and
//! tool batches run sequentially. Both match node's observable event order when
//! the stream mock yields `start`+`done` and `toolExecution` is `"sequential"`.
//! Hooks that do not affect the deterministic core (`transformContext`,
//! `prepareNextTurn`, `shouldStopAfterTurn`, `beforeToolCall`/`afterToolCall`)
//! are omitted here.

use serde_json::{json, Value};

use super::messages::{create_error_tool_result, create_tool_result_message};

/// Mutable loop context: the running transcript plus the system prompt and
/// available tools.
pub struct LoopContext {
    pub system_prompt: String,
    pub messages: Vec<Value>,
    pub tools: Vec<Value>,
}

/// A tool executor: given a tool name and arguments, produce `(result, isError)`
/// where `result` is the `{ content, details, ... }` object.
pub type ToolFn<'a> = dyn FnMut(&str, &Value) -> (Value, bool) + 'a;

/// The LLM boundary: given the call index (0-based) and the LLM context
/// (`{ systemPrompt, messages, tools }`), produce the next assistant message.
pub type StreamFn<'a> = dyn FnMut(usize, &Value) -> Value + 'a;

/// Queued-message provider used for steering/follow-up injection.
pub type QueueFn<'a> = dyn FnMut() -> Vec<Value> + 'a;

pub struct LoopConfig<'a> {
    pub get_steering_messages: Option<Box<QueueFn<'a>>>,
    pub get_follow_up_messages: Option<Box<QueueFn<'a>>>,
    pub timestamp: u64,
}

fn drain(queue: &mut Option<Box<QueueFn>>) -> Vec<Value> {
    queue.as_mut().map(|queue| queue()).unwrap_or_default()
}

fn tool_calls_of(message: &Value) -> Vec<Value> {
    message
        .get("content")
        .and_then(Value::as_array)
        .map(|parts| {
            parts
                .iter()
                .filter(|part| part.get("type").and_then(Value::as_str) == Some("toolCall"))
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

fn has_tool(context: &LoopContext, name: &str) -> bool {
    context
        .tools
        .iter()
        .any(|tool| tool.get("name").and_then(Value::as_str) == Some(name))
}

/// Port of `defaultConvertToLlm`: keep only messages that map to LLM roles.
fn default_convert_to_llm(messages: &[Value]) -> Vec<Value> {
    messages
        .iter()
        .filter(|message| {
            matches!(
                message.get("role").and_then(Value::as_str),
                Some("user") | Some("assistant") | Some("toolResult")
            )
        })
        .cloned()
        .collect()
}

/// Execute one tool call, emitting `tool_execution_start`/`end` and the
/// `toolResult` message pair. Mirrors the sequential path of node
/// `executeToolCalls`.
fn execute_one_tool(
    context: &LoopContext,
    tool_call: &Value,
    tool_fn: &mut ToolFn,
    emit: &mut dyn FnMut(Value),
    timestamp: u64,
) -> Value {
    let id = tool_call.get("id").and_then(Value::as_str).unwrap_or("");
    let name = tool_call.get("name").and_then(Value::as_str).unwrap_or("");
    let args = tool_call.get("arguments").cloned().unwrap_or(json!({}));

    emit(json!({ "type": "tool_execution_start", "toolCallId": id, "toolName": name, "args": args }));

    let (result, is_error) = if has_tool(context, name) {
        tool_fn(name, &args)
    } else {
        (create_error_tool_result(&format!("Tool {name} not found")), true)
    };

    emit(json!({ "type": "tool_execution_end", "toolCallId": id, "toolName": name, "result": result, "isError": is_error }));

    let message = create_tool_result_message(id, name, &result, is_error, timestamp);
    emit(json!({ "type": "message_start", "message": message }));
    emit(json!({ "type": "message_end", "message": message }));
    message
}

/// Port of `runAgentLoop`: append the prompts, emit the opening events, then
/// drive the loop. Returns the messages appended during this run.
pub fn run_agent_loop(
    prompts: &[Value],
    context: &mut LoopContext,
    config: &mut LoopConfig,
    emit: &mut dyn FnMut(Value),
    stream_fn: &mut StreamFn,
    tool_fn: &mut ToolFn,
) -> Vec<Value> {
    let mut new_messages: Vec<Value> = prompts.to_vec();
    context.messages.extend(prompts.iter().cloned());

    emit(json!({ "type": "agent_start" }));
    emit(json!({ "type": "turn_start" }));
    for prompt in prompts {
        emit(json!({ "type": "message_start", "message": prompt }));
        emit(json!({ "type": "message_end", "message": prompt }));
    }

    run_loop_body(context, &mut new_messages, config, emit, stream_fn, tool_fn);
    new_messages
}

/// Port of node `runLoop`: the two-level loop (inner tool/steering loop nested
/// in the outer follow-up loop). Emits `agent_end` on every exit path.
fn run_loop_body(
    context: &mut LoopContext,
    new_messages: &mut Vec<Value>,
    config: &mut LoopConfig,
    emit: &mut dyn FnMut(Value),
    stream_fn: &mut StreamFn,
    tool_fn: &mut ToolFn,
) {
    let mut first_turn = true;
    let mut pending = drain(&mut config.get_steering_messages);
    let mut call_index = 0usize;

    loop {
        let mut has_more_tool_calls = true;
        while has_more_tool_calls || !pending.is_empty() {
            if first_turn {
                first_turn = false;
            } else {
                emit(json!({ "type": "turn_start" }));
            }

            if !pending.is_empty() {
                for message in pending.drain(..) {
                    emit(json!({ "type": "message_start", "message": message }));
                    emit(json!({ "type": "message_end", "message": message }));
                    context.messages.push(message.clone());
                    new_messages.push(message);
                }
            }

            let llm_context = json!({
                "systemPrompt": context.system_prompt,
                "messages": default_convert_to_llm(&context.messages),
                "tools": context.tools,
            });
            let message = stream_fn(call_index, &llm_context);
            call_index += 1;
            context.messages.push(message.clone());
            new_messages.push(message.clone());
            emit(json!({ "type": "message_start", "message": message }));
            emit(json!({ "type": "message_end", "message": message }));

            let stop_reason = message.get("stopReason").and_then(Value::as_str).unwrap_or("");
            if stop_reason == "error" || stop_reason == "aborted" {
                emit(json!({ "type": "turn_end", "message": message, "toolResults": [] }));
                emit(json!({ "type": "agent_end", "messages": new_messages }));
                return;
            }

            let tool_calls = tool_calls_of(&message);
            let mut tool_results: Vec<Value> = Vec::new();
            has_more_tool_calls = false;
            if !tool_calls.is_empty() {
                for tool_call in &tool_calls {
                    let result_message =
                        execute_one_tool(context, tool_call, tool_fn, emit, config.timestamp);
                    tool_results.push(result_message);
                }
                has_more_tool_calls = true;
                for result in &tool_results {
                    context.messages.push(result.clone());
                    new_messages.push(result.clone());
                }
            }

            emit(json!({ "type": "turn_end", "message": message, "toolResults": tool_results }));

            pending = drain(&mut config.get_steering_messages);
        }

        let follow_ups = drain(&mut config.get_follow_up_messages);
        if !follow_ups.is_empty() {
            pending = follow_ups;
            continue;
        }
        break;
    }

    emit(json!({ "type": "agent_end", "messages": new_messages }));
}
