//! Agent 主循环：消息状态、流式调用、并行工具执行、steering（全量模式）、取消。

use crate::lyra::config::Resolved;
use crate::lyra::prompt::ShellConfig;
use crate::lyra::provider::{stream_chat, StreamEvent};
use crate::lyra::tools::{execute, Tool, ToolOutcome};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// provider 尚未返回 usage 时的保守估算：ASCII 约 4 字符/token，非 ASCII 约 1 字符/token。
pub(crate) fn estimate_text_tokens(text: &str) -> u64 {
    let (ascii, non_ascii) = text.chars().fold((0_u64, 0_u64), |(ascii, non_ascii), ch| {
        if ch.is_ascii() {
            (ascii + 1, non_ascii)
        } else {
            (ascii, non_ascii + 1)
        }
    });
    ascii.div_ceil(4).saturating_add(non_ascii)
}

fn estimate_request_tokens(system_prompt: &str, messages: &[Value], tools: &[Tool]) -> u64 {
    let mut tokens = estimate_text_tokens(system_prompt);
    tokens = tokens.saturating_add(estimate_text_tokens(
        &serde_json::to_string(messages).unwrap_or_default(),
    ));
    for tool in tools {
        tokens = tokens
            .saturating_add(estimate_text_tokens(tool.name))
            .saturating_add(estimate_text_tokens(&tool.description))
            .saturating_add(estimate_text_tokens(
                &serde_json::to_string(&tool.parameters).unwrap_or_default(),
            ));
    }
    tokens
}

#[derive(Debug)]
pub enum AgentEvent {
    MessageStart {
        input_estimate: u64,
    },
    TextDelta(String),
    ThinkingDelta(String),
    /// final_note 寄生的最终回复（不参与流式拼接，独立成一条消息项）。
    FinalNote(String),
    ToolStart {
        id: String,
        name: String,
        args: Value,
    },
    ToolEnd {
        id: String,
        outcome: Value,
    },
    MessageEnd {
        usage: Value,
    },
}

#[derive(Debug, Default)]
pub struct TurnOutcome {
    pub cancelled: bool,
    pub stop_reason: String,
    pub error: Option<String>,
}

/// 投机执行句柄：工具参数流式到达期间预执行的无副作用调用。
pub struct Speculative {
    /// 触发预执行时（挽救式解析出）的参数，最终参数严格相等才允许命中。
    pub args: Value,
    pub handle: tokio::task::JoinHandle<ToolOutcome>,
}

pub struct Agent {
    pub model: Resolved,
    pub system_prompt: String,
    pub messages: Vec<Value>,
    pub tools: Vec<Tool>,
    pub cwd: PathBuf,
    pub session_id: String,
    pub archive_dir: Option<PathBuf>,
    pub shell: Option<ShellConfig>,
    pub cancelled: Arc<AtomicBool>,
    pub steering: Arc<Mutex<VecDeque<Value>>>,
    /// 投机缓存：本条消息内工具调用序号 → 预执行句柄（每条消息流开始前清空）。
    pub spec_cache: Arc<Mutex<HashMap<usize, Speculative>>>,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 投机执行：工具参数流式到达期间预执行无副作用工具。LYRA_SPECULATE=off 关闭（基准对照）。
fn speculate_enabled() -> bool {
    std::env::var("LYRA_SPECULATE").ok().as_deref() != Some("off")
}

/// final_note：最终总结寄生在最后一次工具调用上，省一个纯总结回合。LYRA_FINAL_NOTE=off 关闭。
fn final_note_enabled() -> bool {
    std::env::var("LYRA_FINAL_NOTE").ok().as_deref() != Some("off")
}

/// 流式参数片段的挽救式解析：原样解析失败后尝试补全未闭合的字符串/数组/对象括号。
/// 仅用于投机预执行——最终参数严格相等才命中缓存，误判的代价至多是一次预读。
fn salvage_json(fragment: &str) -> Option<Value> {
    let trimmed = fragment.trim();
    if trimmed.len() < 2 {
        return None;
    }
    if let Ok(value) = serde_json::from_str(trimmed) {
        return Some(value);
    }
    let body = trimmed.strip_suffix('\\').unwrap_or(trimmed);
    for suffix in ["}", "\"}", "]}", "\"]}", "\"}}", "]}}", "\"]}}"] {
        if let Ok(value) = serde_json::from_str::<Value>(&format!("{body}{suffix}")) {
            return Some(value);
        }
    }
    None
}

/// 无副作用工具白名单：参数齐备（以必需字段为准）才可投机预执行。
fn speculatable(name: &str, args: &Value) -> bool {
    match name {
        "read" => args.get("path").and_then(Value::as_str).is_some(),
        "find_symbols" => args.get("names").and_then(Value::as_array).is_some(),
        "fast_context" => args.get("keywords").and_then(Value::as_array).is_some(),
        _ => false,
    }
}

/// 投机上下文：流式回调内部不可借用 &mut Agent，预克隆所需状态。
struct SpecContext {
    cache: Arc<Mutex<HashMap<usize, Speculative>>>,
    cwd: PathBuf,
    shell: Option<ShellConfig>,
    archive_dir: Option<PathBuf>,
}

/// 流式参数增量驱动投机执行：参数可挽救解析且齐备时立即预执行，与剩余生成重叠。
/// 同序号参数演进时以最新为准，旧句柄中止。
fn maybe_speculate(spec: &SpecContext, index: usize, name: &str, fragment: &str) {
    if !speculate_enabled() {
        return;
    }
    let Some(mut args) = salvage_json(fragment) else {
        return;
    };
    // final_note 只属于 agent 收尾协议，不应让它造成投机参数与最终执行参数不一致。
    if let Some(map) = args.as_object_mut() {
        map.remove("final_note");
    }
    if !speculatable(name, &args) {
        return;
    }
    let mut cache = spec.cache.lock().unwrap();
    if cache.get(&index).is_some_and(|entry| entry.args == args) {
        return;
    }
    let root = spec.cwd.clone();
    let shell = spec.shell.clone();
    let archive_dir = spec.archive_dir.clone();
    let tool = name.to_string();
    let call_id = format!("spec-{index}");
    let exec_args = args.clone();
    let handle = tokio::spawn(async move {
        execute(
            &root,
            &tool,
            &exec_args,
            shell.as_ref(),
            archive_dir.as_deref(),
            &call_id,
        )
        .await
    });
    if let Some(old) = cache.insert(index, Speculative { args, handle }) {
        old.handle.abort();
    }
}

fn error_outcome(message: String) -> ToolOutcome {
    ToolOutcome {
        content: vec![json!({ "type": "text", "text": message })],
        details: None,
        is_error: true,
    }
}

impl Drop for Agent {
    fn drop(&mut self) {
        if let Ok(mut cache) = self.spec_cache.lock() {
            for (_, entry) in cache.drain() {
                entry.handle.abort();
            }
        }
    }
}

pub fn user_message(text: &str, images: &[Value]) -> Value {
    let mut parts = Vec::new();
    if !text.is_empty() {
        parts.push(json!({ "type": "text", "text": text }));
    }
    parts.extend(images.iter().cloned());
    json!({ "role": "user", "content": parts, "timestamp": now_ms() })
}

impl Agent {
    fn drain_steering(&mut self) {
        let queued: Vec<Value> = self.steering.lock().unwrap().drain(..).collect();
        self.messages.extend(queued);
    }

    /// 追加用户提示并跑到本轮结束（含工具循环与 steering 消化）。
    pub async fn prompt<F, M>(
        &mut self,
        http: &reqwest::Client,
        text: &str,
        images: Vec<Value>,
        on_event: &mut F,
        mid_turn: &mut M,
    ) -> Result<TurnOutcome, String>
    where
        F: FnMut(AgentEvent) + Send,
        M: FnMut(&mut Vec<Value>, &Value) + Send,
    {
        self.messages.push(user_message(text, &images));
        self.run_loop(http, on_event, mid_turn).await
    }

    /// 重试时不重复追加提示，直接从当前消息尾部继续。
    pub async fn continue_run<F, M>(
        &mut self,
        http: &reqwest::Client,
        on_event: &mut F,
        mid_turn: &mut M,
    ) -> Result<TurnOutcome, String>
    where
        F: FnMut(AgentEvent) + Send,
        M: FnMut(&mut Vec<Value>, &Value) + Send,
    {
        self.run_loop(http, on_event, mid_turn).await
    }

    async fn run_loop<F, M>(
        &mut self,
        http: &reqwest::Client,
        on_event: &mut F,
        mid_turn: &mut M,
    ) -> Result<TurnOutcome, String>
    where
        F: FnMut(AgentEvent) + Send,
        M: FnMut(&mut Vec<Value>, &Value) + Send,
    {
        let mut outcome = TurnOutcome::default();
        loop {
            if self.cancelled.load(Ordering::SeqCst) {
                outcome.cancelled = true;
                outcome.stop_reason = "aborted".into();
                return Ok(outcome);
            }
            let input_estimate =
                estimate_request_tokens(&self.system_prompt, &self.messages, &self.tools);
            on_event(AgentEvent::MessageStart { input_estimate });
            // 投机缓存按消息重置：序号是消息内的，上一轮未命中的句柄中止。
            {
                let mut cache = self.spec_cache.lock().unwrap();
                for (_, entry) in cache.drain() {
                    entry.handle.abort();
                }
            }
            let spec = SpecContext {
                cache: self.spec_cache.clone(),
                cwd: self.cwd.clone(),
                shell: self.shell.clone(),
                archive_dir: self.archive_dir.clone(),
            };
            let stream_error: Option<String> = None;
            let result = stream_chat(
                http,
                &self.model.model,
                &self.model.api_key,
                self.model.thinking_level.as_deref(),
                &self.system_prompt,
                &self.messages,
                &self.tools,
                Some(&self.session_id),
                &self.cancelled,
                &mut |event| {
                    if stream_error.is_some() {
                        return;
                    }
                    match event {
                        StreamEvent::TextDelta(delta) => on_event(AgentEvent::TextDelta(delta)),
                        StreamEvent::ThinkingDelta(delta) => {
                            on_event(AgentEvent::ThinkingDelta(delta))
                        }
                        StreamEvent::ToolArgsDelta { index, name, args } => {
                            maybe_speculate(&spec, index, &name, &args);
                        }
                    }
                },
            )
            .await;
            let result = match result {
                Ok(result) => result,
                Err(error) => {
                    outcome.stop_reason = "error".into();
                    outcome.error = Some(error.clone());
                    // 追加一条 error 占位 assistant 消息，保持轨迹完整（与 PI 一致）。
                    self.messages.push(json!({
                        "role": "assistant",
                        "content": [],
                        "api": self.model.model.api,
                        "provider": self.model.model.provider,
                        "model": self.model.model.id,
                        "usage": null,
                        "stopReason": "error",
                        "errorMessage": error,
                        "timestamp": now_ms(),
                    }));
                    return Ok(outcome);
                }
            };
            let assistant = json!({
                "role": "assistant",
                "content": result.content,
                "api": self.model.model.api,
                "provider": self.model.model.provider,
                "model": self.model.model.id,
                "usage": result.usage,
                "stopReason": result.stop_reason,
                "errorMessage": result.error_message,
                "timestamp": now_ms(),
            });
            self.messages.push(assistant);
            on_event(AgentEvent::MessageEnd {
                usage: result.usage.clone(),
            });
            let message = self.messages.last().cloned().unwrap_or(Value::Null);
            outcome.stop_reason = result.stop_reason.clone();
            outcome.error = result.error_message.clone();

            if result.stop_reason == "aborted" {
                outcome.cancelled = true;
                return Ok(outcome);
            }
            if result.stop_reason == "error" {
                return Ok(outcome);
            }

            let tool_calls: Vec<Value> = result
                .content
                .iter()
                .filter(|part| part.get("type").and_then(Value::as_str) == Some("toolCall"))
                .cloned()
                .collect();
            if !tool_calls.is_empty() {
                let notes = self.execute_tools(tool_calls, on_event).await;
                // final_note：模型声明这是最后一次工具调用，总结直接落为最终回复，
                // 跳过"收结果 → 再生成一轮纯总结"的回合。
                if final_note_enabled() && !notes.is_empty() {
                    outcome.stop_reason = "stop".into();
                    let note = notes.join("\n\n");
                    on_event(AgentEvent::FinalNote(note.clone()));
                    self.messages.push(json!({
                        "role": "assistant",
                        "content": [{ "type": "text", "text": note }],
                        "api": self.model.model.api,
                        "provider": self.model.model.provider,
                        "model": self.model.model.id,
                        "usage": Value::Null,
                        "stopReason": "stop",
                        "timestamp": now_ms(),
                    }));
                }
                // Reasonix：每个模型/工具回合后、下一次 provider 请求前维护上下文。
                mid_turn(&mut self.messages, &message);
            }
            // steeringMode=all：工具结果后、最终回复后都把排队提示注入下一轮。
            self.drain_steering();
            let has_more_work = !self.messages.is_empty()
                && self
                    .messages
                    .last()
                    .and_then(|m| m.get("role"))
                    .and_then(Value::as_str)
                    != Some("assistant");
            if !has_more_work {
                return Ok(outcome);
            }
        }
    }

    /// 执行一轮工具调用：剥离 final_note →（投机缓存命中则复用）并行执行。
    /// 返回本轮收集到的 final_note。
    async fn execute_tools<F>(&mut self, tool_calls: Vec<Value>, on_event: &mut F) -> Vec<String>
    where
        F: FnMut(AgentEvent) + Send,
    {
        struct Prepared {
            index: usize,
            id: String,
            name: String,
            /// 剥离 final_note 后的参数（投机缓存按它匹配）。
            raw_args: Value,
            args: Value,
        }
        let mut prepared = Vec::new();
        let mut notes = Vec::new();
        for (index, call) in tool_calls.into_iter().enumerate() {
            let id = call
                .get("id")
                .and_then(Value::as_str)
                .map(str::to_string)
                .filter(|id| !id.is_empty())
                .unwrap_or_else(|| format!("call-{index}"));
            let name = call
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let mut args = call.get("arguments").cloned().unwrap_or_else(|| json!({}));
            if final_note_enabled() {
                if let Some(note) = args
                    .get("final_note")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|note| !note.is_empty())
                {
                    notes.push(note.to_string());
                }
                if let Some(map) = args.as_object_mut() {
                    map.remove("final_note");
                }
            }
            let raw_args = args.clone();
            on_event(AgentEvent::ToolStart {
                id: id.clone(),
                name: name.clone(),
                args: args.clone(),
            });
            prepared.push(Prepared {
                index,
                id,
                name,
                raw_args,
                args,
            });
        }
        // 收取本轮投机缓存：参数严格相等才命中，未命中与残留句柄一律中止。
        let mut speculated: Vec<Option<Speculative>> = Vec::new();
        {
            let mut cache = self.spec_cache.lock().unwrap();
            for call in &prepared {
                speculated.push(cache.remove(&call.index));
            }
            for (_, entry) in cache.drain() {
                entry.handle.abort();
            }
        }
        let spec_hits: Vec<bool> = prepared
            .iter()
            .zip(speculated.iter())
            .map(|(call, spec)| {
                spec.as_ref()
                    .is_some_and(|entry| entry.args == call.raw_args)
            })
            .collect();
        type ExecFuture<'a> = Pin<Box<dyn Future<Output = ToolOutcome> + Send + 'a>>;
        let futures: Vec<ExecFuture> = prepared
            .iter()
            .zip(speculated)
            .map(|(call, spec)| -> ExecFuture {
                match spec {
                    Some(entry) if entry.args == call.raw_args => Box::pin(async move {
                        entry
                            .handle
                            .await
                            .unwrap_or_else(|e| error_outcome(format!("投机执行句柄失败：{e}")))
                    }),
                    Some(entry) => {
                        entry.handle.abort();
                        Box::pin(execute(
                            &self.cwd,
                            &call.name,
                            &call.args,
                            self.shell.as_ref(),
                            self.archive_dir.as_deref(),
                            &call.id,
                        ))
                    }
                    None => Box::pin(execute(
                        &self.cwd,
                        &call.name,
                        &call.args,
                        self.shell.as_ref(),
                        self.archive_dir.as_deref(),
                        &call.id,
                    )),
                }
            })
            .collect();
        let results = futures_util::future::join_all(futures).await;
        for ((call, spec_hit), outcome) in prepared.into_iter().zip(spec_hits).zip(results) {
            let ToolOutcome {
                content,
                details,
                is_error,
            } = outcome;
            let mut result_message = json!({
                "role": "toolResult",
                "toolCallId": call.id,
                "toolName": call.name,
                "content": content,
                "details": details,
                "isError": is_error,
                "timestamp": now_ms(),
            });
            if spec_hit {
                result_message["specHit"] = json!(true);
            }
            on_event(AgentEvent::ToolEnd {
                id: call.id.clone(),
                outcome: result_message.clone(),
            });
            self.messages.push(result_message);
        }
        notes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn salvage_json_closes_partial_object_and_array() {
        let object = salvage_json(r#"{"path":"src/lib.rs"}"#).unwrap();
        assert_eq!(object["path"], "src/lib.rs");
        let array = salvage_json(r#"{"names":["Agent""#).unwrap();
        assert_eq!(array["names"][0], "Agent");
    }
}
