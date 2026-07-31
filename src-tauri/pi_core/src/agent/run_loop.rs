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
pub type ToolFn<'a> = dyn FnMut(&str, &str, &Value) -> (Value, bool) + 'a;

/// One LLM turn as seen by the loop: the raw provider stream events (matching
/// node's `start`/`*_delta`/`done`/`error` shapes) plus the finalized assistant
/// message that `response.result()` resolves to.
pub struct StreamTurn {
    pub events: Vec<Value>,
    pub result: Value,
}

/// The LLM boundary: given the call index (0-based) and the LLM context
/// (`{ systemPrompt, messages, tools }`), produce the next turn's stream.
pub type StreamFn<'a> = dyn FnMut(usize, &Value) -> StreamTurn + 'a;

/// Queued-message provider used for steering/follow-up injection.
pub type QueueFn<'a> = dyn FnMut() -> Vec<Value> + 'a;

/// Optional mid-turn context-maintenance hook (port of the JS Agent's
/// `prepareNextTurnWithContext`). Called after a batch of tool results is
/// appended to `context.messages` and before the next provider request, with
/// the assistant `message` and the `tool_results`. It may mutate
/// `context.messages` (e.g. compact tool results or rebase to slim memory).
/// `None` (the default) leaves the loop unchanged.
pub type PrepareNextTurnFn<'a> = dyn FnMut(&Value, &[Value], &mut LoopContext) + Send + 'a;

pub struct LoopConfig<'a> {
    pub get_steering_messages: Option<Box<QueueFn<'a>>>,
    pub get_follow_up_messages: Option<Box<QueueFn<'a>>>,
    /// Mid-turn context-maintenance hook; see [`PrepareNextTurnFn`].
    pub prepare_next_turn: Option<Box<PrepareNextTurnFn<'a>>>,
    pub timestamp: u64,
    /// When true, a batch of tool calls emits all `tool_execution_start` events,
    /// runs the tools, emits all `tool_execution_end` events, then emits the
    /// `toolResult` messages in order (node `executeToolCallsParallel`).
    pub parallel: bool,
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

/// Port of `streamAssistantResponse`: drive the provider stream, emitting
/// `message_start` on `start`, `message_update` for each delta, and
/// `message_end` on `done`/`error`, while maintaining the running partial
/// message in the transcript. Returns the finalized assistant message.
fn stream_assistant_response(
    context: &mut LoopContext,
    emit: &mut dyn FnMut(Value),
    stream_fn: &mut StreamFn,
    call_index: usize,
    llm_context: &Value,
) -> Value {
    let turn = stream_fn(call_index, llm_context);
    let mut added_partial = false;

    for event in &turn.events {
        match event.get("type").and_then(Value::as_str) {
            Some("start") => {
                let partial = event.get("partial").cloned().unwrap_or_else(|| json!({}));
                context.messages.push(partial.clone());
                added_partial = true;
                emit(json!({ "type": "message_start", "message": partial }));
            }
            Some(
                "text_start"
                | "text_delta"
                | "text_end"
                | "thinking_start"
                | "thinking_delta"
                | "thinking_end"
                | "toolcall_start"
                | "toolcall_delta"
                | "toolcall_end",
            ) => {
                if added_partial {
                    if let Some(partial) = event.get("partial") {
                        let last = context.messages.len() - 1;
                        context.messages[last] = partial.clone();
                        emit(json!({
                            "type": "message_update",
                            "assistantMessageEvent": event,
                            "message": partial,
                        }));
                    }
                }
            }
            Some("done") | Some("error") => {
                let final_message = turn.result.clone();
                if added_partial {
                    let last = context.messages.len() - 1;
                    context.messages[last] = final_message.clone();
                } else {
                    context.messages.push(final_message.clone());
                    emit(json!({ "type": "message_start", "message": final_message }));
                }
                emit(json!({ "type": "message_end", "message": final_message }));
                return final_message;
            }
            _ => {}
        }
    }

    // Stream ended without an explicit done/error event.
    let final_message = turn.result.clone();
    if added_partial {
        let last = context.messages.len() - 1;
        context.messages[last] = final_message.clone();
    } else {
        context.messages.push(final_message.clone());
        emit(json!({ "type": "message_start", "message": final_message }));
    }
    emit(json!({ "type": "message_end", "message": final_message }));
    final_message
}

/// A finalized tool call: identity plus result, used to order parallel batches.
struct FinalizedTool {
    id: String,
    name: String,
    result: Value,
    is_error: bool,
}

fn emit_tool_start(emit: &mut dyn FnMut(Value), tool_call: &Value) {
    let id = tool_call.get("id").and_then(Value::as_str).unwrap_or("");
    let name = tool_call.get("name").and_then(Value::as_str).unwrap_or("");
    let args = tool_call.get("arguments").cloned().unwrap_or(json!({}));
    emit(json!({ "type": "tool_execution_start", "toolCallId": id, "toolName": name, "args": args }));
}

/// Run a tool call without emitting events (the node `prepareToolCall` +
/// `executePreparedToolCall` core, minus hooks).
fn run_tool_call(context: &LoopContext, tool_call: &Value, tool_fn: &mut ToolFn) -> FinalizedTool {
    let id = tool_call.get("id").and_then(Value::as_str).unwrap_or("").to_string();
    let name = tool_call.get("name").and_then(Value::as_str).unwrap_or("").to_string();
    let args = tool_call.get("arguments").cloned().unwrap_or(json!({}));
    let (result, is_error) = if has_tool(context, &name) {
        tool_fn(&id, &name, &args)
    } else {
        (create_error_tool_result(&format!("Tool {name} not found")), true)
    };
    FinalizedTool { id, name, result, is_error }
}

fn emit_tool_end(emit: &mut dyn FnMut(Value), finalized: &FinalizedTool) {
    emit(json!({ "type": "tool_execution_end", "toolCallId": finalized.id, "toolName": finalized.name, "result": finalized.result, "isError": finalized.is_error }));
}

/// Build the `toolResult` message and emit its `message_start`/`message_end`.
fn emit_tool_result_message(
    emit: &mut dyn FnMut(Value),
    finalized: &FinalizedTool,
    timestamp: u64,
) -> Value {
    let message = create_tool_result_message(
        &finalized.id,
        &finalized.name,
        &finalized.result,
        finalized.is_error,
        timestamp,
    );
    emit(json!({ "type": "message_start", "message": message }));
    emit(json!({ "type": "message_end", "message": message }));
    message
}

/// Execute one tool call sequentially: start, run, end, then result message.
fn execute_one_tool(
    context: &LoopContext,
    tool_call: &Value,
    tool_fn: &mut ToolFn,
    emit: &mut dyn FnMut(Value),
    timestamp: u64,
) -> Value {
    emit_tool_start(emit, tool_call);
    let finalized = run_tool_call(context, tool_call, tool_fn);
    emit_tool_end(emit, &finalized);
    emit_tool_result_message(emit, &finalized, timestamp)
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
            let message =
                stream_assistant_response(context, emit, stream_fn, call_index, &llm_context);
            call_index += 1;
            new_messages.push(message.clone());

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
                if config.parallel {
                    // Phase 1: emit every start first (node emits all starts in
                    // the prepare loop before any execution settles).
                    for tool_call in &tool_calls {
                        emit_tool_start(emit, tool_call);
                    }
                    // Phase 2: run each tool and emit its end.
                    let mut finalized: Vec<FinalizedTool> = Vec::new();
                    for tool_call in &tool_calls {
                        let done = run_tool_call(context, tool_call, tool_fn);
                        emit_tool_end(emit, &done);
                        finalized.push(done);
                    }
                    // Phase 3: result messages in call order.
                    for done in &finalized {
                        tool_results.push(emit_tool_result_message(emit, done, config.timestamp));
                    }
                } else {
                    for tool_call in &tool_calls {
                        let result_message =
                            execute_one_tool(context, tool_call, tool_fn, emit, config.timestamp);
                        tool_results.push(result_message);
                    }
                }
                has_more_tool_calls = true;
                for result in &tool_results {
                    context.messages.push(result.clone());
                    new_messages.push(result.clone());
                }
            }

            // Mid-turn context maintenance (Reasonix): compact/rebase the
            // transcript between tool rounds, before the next provider request.
            if !tool_results.is_empty() {
                if let Some(hook) = config.prepare_next_turn.as_mut() {
                    hook(&message, &tool_results, context);
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
