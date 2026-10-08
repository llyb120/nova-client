//! Codex 式 turn 循环：规范历史 → 上下文视图 → 流式请求 → 工具派发 → 追加结果，直到模型不再调用工具
//! 且没有待消化的 steer。只读工具在参数完整时（`ToolCallDone`）即提前派发；有副作用的工具按出现顺序
//! 串行，且其后的调用不再提前派发，保证“先改后读”看到新内容。provider 失败最多重试 5 次（指数退避 +
//! 抖动），上下文超限先按 Reasonix 折叠再重试一次。失败的 assistant 占位从不入史，重试无需回滚。

use crate::lyra::config::Resolved;
use crate::lyra::context::{coalesce_user_runs, ContextWindow, Receipt, Trigger, SUMMARY_OUTPUT_MAX_TOKENS};
use crate::lyra::history::{now_ms, user_message, History};
use crate::lyra::prompt::{is_context_window_error, is_retryable_provider_error, ShellConfig};
use crate::lyra::provider::{is_retryable_stream_error, stream_chat, StreamEvent};
use crate::lyra::session::{self, AgentHandle};
use crate::lyra::tools::{execute, Tool, ToolOutcome};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

pub enum TurnEvent {
    MessageStart,
    TextDelta(String),
    ThinkingDelta(String),
    ToolStart { id: String, name: String, args: Value },
    /// `outcome` 是入史的 toolResult 消息。
    ToolEnd { id: String, outcome: Value },
    MessageEnd { usage: Value },
    Retry { attempt: usize, error: String, context_recovery: bool },
    Compacted(Receipt),
}

#[derive(Default, Debug)]
pub struct TurnOutcome {
    pub cancelled: bool,
    pub error: Option<String>,
}

/// 一个 agent 的全部运行态（根会话与子 agent 共用）。
pub struct Session {
    pub http: reqwest::Client,
    pub model: Resolved,
    /// 折叠摘要用的模型（轻量模型或主模型）。
    pub summarizer: Resolved,
    pub system_prompt: String,
    pub tools: Vec<Tool>,
    pub cwd: PathBuf,
    pub session_id: String,
    pub archive_dir: Option<PathBuf>,
    pub shell: Option<ShellConfig>,
    pub history: History,
    pub context: ContextWindow,
    pub cancelled: Arc<AtomicBool>,
    pub steer: Arc<Mutex<VecDeque<Value>>>,
    /// 多 agent 控制面；子 agent 为 None（深度上限 1）。
    pub agents: Option<AgentHandle>,
}

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
type Sink<'a> = &'a mut (dyn FnMut(TurnEvent) + Send);

const MAX_RETRIES: usize = 5;
const CANCELLED_TEXT: &str = "已取消";

/// 参数完整即可提前派发的只读工具。
fn early_safe(name: &str) -> bool {
    matches!(name, "read" | "polaris")
}

/// 可与相邻同类调用并发执行的工具（只读、不改会话状态）。
fn parallel_safe(name: &str) -> bool {
    early_safe(name) || matches!(name, "context_budget" | "list_agents" | "wait_agent")
}

/// 操作同一块屏幕的工具跨 agent 互斥。
fn screen_tool(name: &str) -> bool {
    matches!(name, "jianlai" | "chrome" | "webview")
}

fn screen_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

/// 上下文机制自带的工具：`recall` 读回折叠原文，`context_budget` 报告剩余空间。
pub fn context_tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "recall",
            description: "Read back messages that compaction folded out of your context. The fold summary indexes them as #n; pass those numbers in `positions` to read the originals, or pass `query` to search the folded part. Budgeted per fold, so ask only for what you need.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "positions": { "type": "array", "items": { "type": "integer", "minimum": 0 }, "description": "#n positions from the fold index" },
                    "query": { "type": "string", "description": "text to search for in the folded messages" }
                }
            }),
        },
        Tool {
            name: "context_budget",
            description: "Report how many tokens of room remain before this conversation is automatically compacted.".into(),
            parameters: json!({ "type": "object", "properties": {} }),
        },
    ]
}

pub fn text_outcome(text: impl Into<String>, is_error: bool) -> ToolOutcome {
    ToolOutcome { content: vec![json!({ "type": "text", "text": text.into() })], details: None, is_error }
}

// ponytail: 抖动取系统时钟纳秒，不为此引入随机数依赖。
fn backoff(attempt: usize) -> Duration {
    let base = 200u64 << (attempt.saturating_sub(1)).min(6);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let jitter = 0.9 + (nanos % 1000) as f64 / 5000.0;
    Duration::from_millis((base as f64 * jitter) as u64)
}

impl Session {
    fn last_user_index(&self) -> Option<usize> {
        self.history
            .items()
            .iter()
            .rposition(|m| m.get("role").and_then(Value::as_str) == Some("user"))
    }

    fn strips_reasoning(&self) -> bool {
        self.model.model.api.starts_with("openai") || self.model.model.api == "azure-openai-responses"
    }

    fn cancel(&mut self) -> TurnOutcome {
        self.history.close_pending_calls(CANCELLED_TEXT);
        TurnOutcome { cancelled: true, error: None }
    }
}

/// 跑一轮：`input` 为本轮用户消息（None 表示只消化 steer 继续）。返回 BoxFuture 以便子 agent 递归派生。
pub fn run_turn<'a>(s: &'a mut Session, input: Option<Value>, on_event: Sink<'a>) -> BoxFuture<'a, TurnOutcome> {
    Box::pin(async move {
        if let Some(input) = input {
            s.history.record(input);
        }
        let turn_start = s.last_user_index().unwrap_or(0);
        let mut retries = 0;
        let mut overflow_recovered = false;
        loop {
            if s.cancelled.load(Ordering::SeqCst) {
                return s.cancel();
            }
            let steered: Vec<Value> = s.steer.lock().unwrap().drain(..).collect();
            for message in steered {
                s.history.record(message);
            }
            if s.context.should_compact(s.history.items()) {
                compact(s, Trigger::Auto, on_event).await;
            }
            let view = s.context.view(s.history.items());
            let completed = s.context.view_index(s.history.items(), turn_start).unwrap_or(0);
            let messages = coalesce_user_runs(History::for_prompt(&view, completed, s.strips_reasoning()));

            on_event(TurnEvent::MessageStart);
            let mut early: Vec<(String, String, Value, tokio::task::JoinHandle<ToolOutcome>)> = Vec::new();
            let result = {
                let (cwd, shell, archive, cancelled) =
                    (s.cwd.clone(), s.shell.clone(), s.archive_dir.clone(), s.cancelled.clone());
                let mut blocked = false;
                let early = &mut early;
                let on_event = &mut *on_event;
                stream_chat(
                    &s.http,
                    &s.model.model,
                    &s.model.api_key,
                    s.model.thinking_level.as_deref(),
                    &s.system_prompt,
                    &messages,
                    &s.tools,
                    Some(&s.session_id),
                    &s.cancelled,
                    &mut |event| match event {
                        StreamEvent::TextDelta(delta) => on_event(TurnEvent::TextDelta(delta)),
                        StreamEvent::ThinkingDelta(delta) => on_event(TurnEvent::ThinkingDelta(delta)),
                        StreamEvent::ToolCallDone { id, name, arguments } => {
                            blocked |= !early_safe(&name) || id.is_empty();
                            if blocked {
                                return;
                            }
                            let (cwd, shell, archive, cancelled) =
                                (cwd.clone(), shell.clone(), archive.clone(), cancelled.clone());
                            let (task_name, task_args, task_id) = (name.clone(), arguments.clone(), id.clone());
                            let handle = tokio::spawn(async move {
                                execute(&cwd, &task_name, &task_args, shell.as_ref(), archive.as_deref(), &task_id, Some(&cancelled)).await
                            });
                            early.push((id, name, arguments, handle));
                        }
                        _ => {}
                    },
                )
                .await
            };
            if let Ok(result) = &result {
                if !result.usage.is_null() {
                    on_event(TurnEvent::MessageEnd { usage: result.usage.clone() });
                }
            }
            let result = match result {
                Ok(result) if result.stop_reason != "error" => result,
                failed => {
                    early.iter().for_each(|(.., handle)| handle.abort());
                    let error = match failed {
                        Ok(result) => result.error_message.unwrap_or_else(|| "provider error".into()),
                        Err(error) => error,
                    };
                    if s.cancelled.load(Ordering::SeqCst) {
                        return s.cancel();
                    }
                    if is_context_window_error(&error) && !overflow_recovered {
                        overflow_recovered = true;
                        if compact(s, Trigger::Overflow, on_event).await.is_some_and(|r| r.status == "applied") {
                            on_event(TurnEvent::Retry { attempt: retries + 1, error, context_recovery: true });
                            continue;
                        }
                    } else if retries < MAX_RETRIES
                        && (is_retryable_provider_error(&error) || is_retryable_stream_error(&error))
                    {
                        retries += 1;
                        on_event(TurnEvent::Retry { attempt: retries, error, context_recovery: false });
                        tokio::time::sleep(backoff(retries)).await;
                        continue;
                    }
                    return TurnOutcome { cancelled: false, error: Some(error) };
                }
            };
            retries = 0;
            let usage = &result.usage;
            let prompt_tokens = ["input", "cacheRead", "cacheWrite"]
                .iter()
                .map(|key| usage.get(key).and_then(Value::as_u64).unwrap_or(0))
                .sum();
            s.context.calibrate(&messages, prompt_tokens);

            let mut content = result.content;
            let mut calls = Vec::new();
            for (index, part) in content.iter_mut().enumerate() {
                if part.get("type").and_then(Value::as_str) != Some("toolCall") {
                    continue;
                }
                if part.get("id").and_then(Value::as_str).unwrap_or_default().is_empty() {
                    part["id"] = json!(format!("call-{index}"));
                }
                calls.push((
                    part["id"].as_str().unwrap_or_default().to_string(),
                    part.get("name").and_then(Value::as_str).unwrap_or_default().to_string(),
                    part.get("arguments").cloned().unwrap_or_else(|| json!({})),
                ));
            }
            let aborted = result.stop_reason == "aborted";
            if !(aborted && content.is_empty()) {
                s.history.record(json!({
                    "role": "assistant",
                    "content": content,
                    "api": s.model.model.api,
                    "provider": s.model.model.provider,
                    "model": s.model.model.id,
                    "usage": result.usage,
                    "stopReason": result.stop_reason,
                    "errorMessage": result.error_message,
                    "timestamp": now_ms(),
                }));
            }
            if aborted {
                early.iter().for_each(|(.., handle)| handle.abort());
                return s.cancel();
            }
            if calls.is_empty() {
                if s.steer.lock().unwrap().is_empty() {
                    return TurnOutcome::default();
                }
                continue;
            }
            // ponytail: 预算提醒按工具执行前的历史估算，最多晚一轮出现。
            let notice = s.context.budget_notice(s.history.items());
            let mut results = run_tools(s, &calls, early, on_event).await;
            if let (Some(notice), Some(last)) = (notice, results.last_mut()) {
                if let Some(parts) = last.get_mut("content").and_then(Value::as_array_mut) {
                    parts.push(json!({ "type": "text", "text": notice }));
                }
            }
            for message in results {
                s.history.record(message);
            }
        }
    })
}

/// 按顺序执行：相邻的可并发调用一起跑（提前派发的直接取结果），其余逐个串行。
async fn run_tools(
    s: &mut Session,
    calls: &[(String, String, Value)],
    mut early: Vec<(String, String, Value, tokio::task::JoinHandle<ToolOutcome>)>,
    on_event: Sink<'_>,
) -> Vec<Value> {
    for (id, name, args) in calls {
        on_event(TurnEvent::ToolStart { id: id.clone(), name: name.clone(), args: args.clone() });
    }
    let mut results = Vec::new();
    let mut i = 0;
    while i < calls.len() {
        if s.cancelled.load(Ordering::SeqCst) {
            for (id, name, _) in &calls[i..] {
                results.push(finish(id, name, text_outcome(CANCELLED_TEXT, true), on_event));
            }
            break;
        }
        let run = if parallel_safe(&calls[i].1) {
            calls[i..].iter().take_while(|(_, name, _)| parallel_safe(name)).count()
        } else {
            1
        };
        let batch = &calls[i..i + run];
        let outcomes: Vec<ToolOutcome> = if run == 1 && calls[i].1 == "recall" {
            vec![recall(s, &calls[i].2)]
        } else {
            let handles: Vec<_> = batch
                .iter()
                .map(|(id, name, args)| {
                    early
                        .iter()
                        .position(|(eid, ename, eargs, _)| eid == id && ename == name && eargs == args)
                        .map(|at| early.swap_remove(at).3)
                })
                .collect();
            let shared: &Session = s;
            futures_util::future::join_all(batch.iter().zip(handles).map(|((id, name, args), handle)| async move {
                match handle {
                    Some(handle) => handle
                        .await
                        .unwrap_or_else(|error| text_outcome(format!("工具执行失败：{error}"), true)),
                    None => call_tool(shared, id, name, args).await,
                }
            }))
            .await
        };
        for ((id, name, _), outcome) in batch.iter().zip(outcomes) {
            if name == "change_working_directory" && !outcome.is_error {
                if let Some(cwd) = outcome
                    .details
                    .as_ref()
                    .and_then(|d| d.get("workingDirectory"))
                    .and_then(Value::as_str)
                {
                    s.cwd = PathBuf::from(cwd);
                }
            }
            results.push(finish(id, name, outcome, on_event));
        }
        i += run;
    }
    early.iter().for_each(|(.., handle)| handle.abort());
    results
}

fn finish(id: &str, name: &str, outcome: ToolOutcome, on_event: Sink<'_>) -> Value {
    let message = json!({
        "role": "toolResult",
        "toolCallId": id,
        "toolName": name,
        "content": outcome.content,
        "details": outcome.details,
        "isError": outcome.is_error,
        "timestamp": now_ms(),
    });
    on_event(TurnEvent::ToolEnd { id: id.to_string(), outcome: message.clone() });
    message
}

async fn call_tool(s: &Session, id: &str, name: &str, args: &Value) -> ToolOutcome {
    if name == "context_budget" {
        return text_outcome(s.context.budget_report(s.history.items()), false);
    }
    if session::is_agent_tool(name) {
        return match &s.agents {
            Some(agents) => session::call(agents, s, name, args).await,
            None => text_outcome(format!("{name} 仅在主 agent 中可用"), true),
        };
    }
    let _screen = if screen_tool(name) { Some(screen_lock().lock().await) } else { None };
    execute(&s.cwd, name, args, s.shell.as_ref(), s.archive_dir.as_deref(), id, Some(&s.cancelled)).await
}

fn recall(s: &mut Session, args: &Value) -> ToolOutcome {
    let positions: Vec<usize> = args
        .get("positions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_u64)
        .map(|p| p as usize)
        .collect();
    let query = args.get("query").and_then(Value::as_str);
    match s.context.recall(s.history.items(), &positions, query) {
        Ok(text) => text_outcome(text, false),
        Err(error) => text_outcome(error, true),
    }
}

/// 折叠一次；摘要走无工具的流式请求，用轻量模型（若配置）且关闭思考。
async fn compact(s: &mut Session, trigger: Trigger, on_event: Sink<'_>) -> Option<Receipt> {
    let active = s.last_user_index();
    let http = s.http.clone();
    let mut model = s.summarizer.clone();
    if model.model.max_output_tokens == 0 || model.model.max_output_tokens > SUMMARY_OUTPUT_MAX_TOKENS {
        model.model.max_output_tokens = SUMMARY_OUTPUT_MAX_TOKENS;
    }
    let cancelled = s.cancelled.clone();
    let summarize = move |system: String, transcript: String| async move {
        let messages = [user_message(&transcript, &[])];
        let mut text = String::new();
        let result = stream_chat(
            &http,
            &model.model,
            &model.api_key,
            Some("off"),
            &system,
            &messages,
            &[],
            None,
            &cancelled,
            &mut |event| {
                if let StreamEvent::TextDelta(delta) = event {
                    text.push_str(&delta);
                }
            },
        )
        .await?;
        match result.stop_reason.as_str() {
            "stop" | "toolUse" if !text.trim().is_empty() => Ok(text),
            reason => Err(result.error_message.unwrap_or_else(|| format!("摘要未完成（stop_reason={reason}）"))),
        }
    };
    match s.context.compact(s.history.items(), active, trigger, summarize).await {
        Ok(receipt) => {
            if receipt.status != "noop" {
                on_event(TurnEvent::Compacted(receipt.clone()));
            }
            Some(receipt)
        }
        Err(error) => {
            eprintln!("lyra: 上下文折叠失败：{error}");
            None
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::lyra::config::ResolvedModel;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// 依次回放脚本化响应的本地 HTTP 服务；`None` 表示 HTTP 503。
    pub(crate) async fn scripted_server(responses: Vec<Option<Vec<Value>>>) -> String {
        capturing_server(responses).await.0
    }

    /// 同 `scripted_server`，另返回收到的请求体（按到达顺序）。
    pub(crate) async fn capturing_server(responses: Vec<Option<Vec<Value>>>) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let captured = bodies.clone();
        tokio::spawn(async move {
            for chunks in responses {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let mut buf = [0u8; 8192];
                let body_start = loop {
                    let n = socket.read(&mut buf).await.unwrap();
                    request.extend_from_slice(&buf[..n]);
                    if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                        break end + 4;
                    }
                    if n == 0 {
                        break request.len();
                    }
                };
                let length = String::from_utf8_lossy(&request[..body_start])
                    .lines()
                    .find_map(|line| line.to_ascii_lowercase().strip_prefix("content-length: ").and_then(|v| v.trim().parse().ok()))
                    .unwrap_or(0);
                while request.len() < body_start + length {
                    let n = socket.read(&mut buf).await.unwrap();
                    if n == 0 {
                        break;
                    }
                    request.extend_from_slice(&buf[..n]);
                }
                captured.lock().unwrap().push(String::from_utf8_lossy(&request[body_start..]).into_owned());
                let reply = match chunks {
                    Some(chunks) => {
                        let body: String = chunks.iter().map(|c| format!("data: {c}\n\n")).collect::<String>() + "data: [DONE]\n\n";
                        format!("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len())
                    }
                    None => "HTTP/1.1 503 Service Unavailable\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".into(),
                };
                socket.write_all(reply.as_bytes()).await.unwrap();
            }
        });
        (format!("http://{addr}/v1"), bodies)
    }

    pub(crate) fn text_reply(text: &str) -> Option<Vec<Value>> {
        Some(vec![json!({"choices":[{"delta":{"content":text},"finish_reason":"stop"}],
            "usage":{"prompt_tokens":10,"completion_tokens":2,"total_tokens":12}})])
    }

    pub(crate) fn test_session(base_url: String, cwd: PathBuf) -> Session {
        let model = Resolved {
            model: ResolvedModel {
                provider: "p".into(),
                id: "m".into(),
                api: "openai-completions".into(),
                base_url,
                headers: serde_json::Map::new(),
                reasoning: false,
                thinking_format: None,
                max_tokens_field: "max_tokens",
                context_window: 0,
                max_output_tokens: 1000,
                service_tier: None,
                temperature: None,
                top_p: None,
                supports_images: false,
                requires_reasoning_content: false,
                session_affinity_headers: false,
                session_affinity_format: "openai".into(),
                supports_long_cache_retention: false,
                supports_reasoning_effort: false,
                clear_thinking: None,
                extra_options: serde_json::Map::new(),
                proxy: None,
            },
            api_key: "k".into(),
            thinking_level: None,
        };
        Session {
            http: reqwest::Client::builder().no_proxy().build().unwrap(),
            summarizer: model.clone(),
            model,
            system_prompt: "sys".into(),
            tools: context_tools(),
            cwd,
            session_id: "t".into(),
            archive_dir: None,
            shell: None,
            history: History::default(),
            context: ContextWindow::ephemeral(0, 0),
            cancelled: Arc::new(AtomicBool::new(false)),
            steer: Arc::new(Mutex::new(VecDeque::new())),
            agents: None,
        }
    }

    #[tokio::test]
    async fn retries_then_runs_tools_in_order_and_finishes() {
        let dir = std::env::temp_dir().join(format!("lyra-turn-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "alpha\n").unwrap();
        // read a.txt（可提前派发）→ write b.txt（副作用，阻断后续提前派发）→ read b.txt（必须看到新内容）。
        let call = |index: usize, id: &str, name: &str, args: Value| {
            json!({"choices":[{"delta":{"tool_calls":[{"index":index,"id":id,
                "function":{"name":name,"arguments":args.to_string()}}]},"finish_reason":null}]})
        };
        let mut tool_turn = vec![
            call(0, "c0", "read", json!({"path":"a.txt"})),
            call(1, "c1", "write", json!({"path":"b.txt","content":"bravo\n"})),
            call(2, "c2", "read", json!({"path":"b.txt"})),
        ];
        tool_turn.push(json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]}));
        let url = scripted_server(vec![None, Some(tool_turn), text_reply("done")]).await;
        let mut s = test_session(url, dir.clone());
        let mut retries = Vec::new();
        let mut ended = Vec::new();
        let outcome = run_turn(&mut s, Some(user_message("go", &[])), &mut |event| match event {
            TurnEvent::Retry { attempt, .. } => retries.push(attempt),
            TurnEvent::ToolEnd { id, .. } => ended.push(id),
            _ => {}
        })
        .await;
        assert!(outcome.error.is_none() && !outcome.cancelled, "{outcome:?}");
        assert_eq!(retries, vec![1]);
        assert_eq!(ended, vec!["c0", "c1", "c2"]);
        let roles: Vec<&str> = s.history.items().iter().map(|m| m["role"].as_str().unwrap()).collect();
        assert_eq!(roles, ["user", "assistant", "toolResult", "toolResult", "toolResult", "assistant"]);
        let text = |i: usize| s.history.items()[i]["content"].to_string();
        assert!(text(2).contains("alpha"));
        assert!(text(4).contains("bravo"), "read after write must see the new file: {}", text(4));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
