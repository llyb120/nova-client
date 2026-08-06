//! Agent 主循环：消息状态、流式调用、并行工具执行、steering（全量模式）、取消。

use crate::lyra::config::Resolved;
use crate::lyra::prompt::ShellConfig;
use crate::lyra::provider::{stream_chat, StreamEvent};
use crate::lyra::tools::{execute, Tool, ToolOutcome};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Debug)]
pub enum AgentEvent {
    MessageStart,
    TextDelta(String),
    ThinkingDelta(String),
    ToolStart { id: String, name: String, args: Value },
    ToolEnd { id: String, outcome: Value },
    MessageEnd { usage: Value },
}

#[derive(Debug, Default)]
pub struct TurnOutcome {
    pub cancelled: bool,
    pub stop_reason: String,
    pub error: Option<String>,
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
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
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
            on_event(AgentEvent::MessageStart);
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
                self.execute_tools(tool_calls, on_event).await;
                // Reasonix：每个模型/工具回合后、下一次 provider 请求前维护上下文。
                mid_turn(&mut self.messages, &message);
            }
            // steeringMode=all：工具结果后、最终回复后都把排队提示注入下一轮。
            self.drain_steering();
            let has_more_work = !self.messages.is_empty()
                && self.messages.last().and_then(|m| m.get("role")).and_then(Value::as_str)
                    != Some("assistant");
            if !has_more_work {
                return Ok(outcome);
            }
        }
    }

    async fn execute_tools<F>(&mut self, tool_calls: Vec<Value>, on_event: &mut F)
    where
        F: FnMut(AgentEvent) + Send,
    {
        struct Prepared {
            id: String,
            name: String,
            args: Value,
        }
        let mut prepared = Vec::new();
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
            let args = call.get("arguments").cloned().unwrap_or_else(|| json!({}));
            on_event(AgentEvent::ToolStart {
                id: id.clone(),
                name: name.clone(),
                args: args.clone(),
            });
            prepared.push(Prepared { id, name, args });
        }
        let futures: Vec<_> = prepared
            .iter()
            .map(|call| {
                execute(
                    &self.cwd,
                    &call.name,
                    &call.args,
                    self.shell.as_ref(),
                    self.archive_dir.as_deref(),
                    &call.id,
                )
            })
            .collect();
        let results = futures_util::future::join_all(futures).await;
        for (call, outcome) in prepared.into_iter().zip(results) {
            let ToolOutcome {
                content,
                details,
                is_error,
            } = outcome;
            let result_message = json!({
                "role": "toolResult",
                "toolCallId": call.id,
                "toolName": call.name,
                "content": content,
                "details": details,
                "isError": is_error,
                "timestamp": now_ms(),
            });
            on_event(AgentEvent::ToolEnd {
                id: call.id.clone(),
                outcome: result_message.clone(),
            });
            self.messages.push(result_message);
        }
    }
}
