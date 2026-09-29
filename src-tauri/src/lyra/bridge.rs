//! stdio JSONL 协议（与 alkaid-bridge 兼容）：首行为请求，prompt 期间后续行为
//! cancel/steer；事件 ready/item/timing/done/{ok:false}。Reasonix 会话生命周期在此串联。

use crate::lyra::config::{self, Resolved, Roots};
use crate::lyra::context::ContextWindow;
use crate::lyra::history::{estimate_text_tokens, user_message, History};
use crate::lyra::prompt::{
    self, build_system_prompt, expand_skill_command, format_skills_prompt, image_media_type,
    load_agent_instructions, load_skills, merge_usage, SystemPromptOptions,
};
use crate::lyra::provider::{stream_chat, StreamEvent};
use crate::lyra::rollout::Rollout;
use crate::lyra::session;
use crate::lyra::tools::tool_set;
use crate::lyra::turn::{context_tools, run_turn, Session, TurnEvent};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn send(value: &Value) {
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    let _ = writeln!(lock, "{}", serde_json::to_string(value).unwrap_or_default());
    let _ = lock.flush();
}

fn send_error(error: impl Into<String>) {
    send(&json!({ "ok": false, "error": error.into() }));
}

/// 事件出口：stdio 子进程写 stdout，进程内运行写 mpsc 通道。
type Emit = Arc<dyn Fn(&Value) + Send + Sync>;

fn stdout_emit() -> Emit {
    Arc::new(|value: &Value| send(value))
}

fn stable_hash(value: impl AsRef<[u8]>) -> String {
    let digest = Sha256::digest(value.as_ref());
    format!("{digest:x}")[..16].to_string()
}

fn new_session_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("lyra-{:x}-{:x}", nanos, std::process::id())
}

/// provider 最后一轮结束与控制通道收取需要对齐：provider 最后一轮结束与控制通道收取
/// steer 之间存在竞争，必须等到控制通道经历一个安静窗口后才能判定任务结束。
async fn settle_pending_input(command_busy: &AtomicBool, command_revision: &AtomicU64) {
    loop {
        let observed = command_revision.load(Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(25)).await;
        if !command_busy.load(Ordering::SeqCst)
            && command_revision.load(Ordering::SeqCst) == observed
        {
            return;
        }
    }
}

/// 把协议 parts 转为 (文本, 图片)（local_image 读文件转 base64）。
fn prompt_input(parts: &[Value]) -> (String, Vec<Value>) {
    let mut texts = Vec::new();
    let mut images = Vec::new();
    for part in parts {
        match part.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(text) = part.get("text").and_then(Value::as_str) {
                    texts.push(text.to_string());
                }
            }
            Some("image_data") => {
                images.push(json!({
                    "type": "image",
                    "data": part.get("data").and_then(Value::as_str).unwrap_or_default(),
                    "mimeType": part.get("mime").and_then(Value::as_str).unwrap_or("image/png"),
                }));
            }
            Some("local_image") => {
                let path = part.get("path").and_then(Value::as_str).unwrap_or_default();
                if let Some(mime) = image_media_type(path) {
                    if let Ok(data) = std::fs::read(path) {
                        use base64::Engine;
                        images.push(json!({
                            "type": "image",
                            "data": base64::engine::general_purpose::STANDARD.encode(data),
                            "mimeType": mime,
                        }));
                        continue;
                    }
                }
                texts.push(format!("Attached file: {path}"));
            }
            _ => {}
        }
    }
    (texts.join("\n\n"), images)
}

fn aggregate_tool_text(message: &Value) -> String {
    message
        .get("content")
        .and_then(Value::as_array)
        .map(|parts| {
            parts
                .iter()
                .filter(|p| p.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|p| p.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

/// 与 alkaid bridge 相同的工具事件 → UI item 映射。
fn started_tool_item(id: &str, name: &str, args: &Value) -> Value {
    match name {
        "bash" => json!({
            "id": id, "type": "command_execution", "status": "in_progress", "tool": name,
            "command": args.get("command").and_then(Value::as_str).unwrap_or_default(),
            "aggregatedOutput": "",
        }),
        "edit" | "write" => {
            let path = args.get("path").and_then(Value::as_str).unwrap_or_default();
            json!({ "id": id, "type": "file_change", "status": "in_progress", "tool": name, "arguments": args, "changes": [{ "path": path, "kind": "update" }] })
        }
        _ => json!({
            "id": id, "type": "mcp_tool_call", "status": "in_progress",
            "server": "Lyra", "tool": name, "arguments": args, "result": null,
        }),
    }
}

fn completed_tool_item(started: &Value, outcome: &Value) -> Value {
    let mut item = started.clone();
    item["status"] = json!("completed");
    match item.get("type").and_then(Value::as_str) {
        Some("command_execution") => {
            item["aggregated_output"] = json!(aggregate_tool_text(outcome));
        }
        Some("mcp_tool_call") => {
            item["result"] = json!({ "content": outcome.get("content").cloned().unwrap_or(Value::Array(vec![])) });
        }
        // file_change 失败时附上错误内容：edit 定位失败等原因对 UI/调用方可见。
        Some("file_change") => {
            if outcome.get("isError").and_then(Value::as_bool) == Some(true) {
                item["result"] = json!({ "content": outcome.get("content").cloned().unwrap_or(Value::Array(vec![])) });
            }
        }
        _ => {}
    }
    if let Some(details) = outcome.get("details").filter(|d| !d.is_null()) {
        item["details"] = details.clone();
    }
    // 工具失败（edit 定位失败等）对 UI/调用方可见；缺省视为成功。
    if outcome.get("isError").and_then(Value::as_bool) == Some(true) {
        item["isError"] = json!(true);
    }
    item
}

struct PromptContext {
    session_id: String,
    cwd: String,
    mode: String,
}

async fn handle_prompt(
    http: &reqwest::Client,
    request: &Value,
    emit: &Emit,
    fast_context: bool,
    roots: &Roots,
    mut line_rx: tokio::sync::mpsc::UnboundedReceiver<Value>,
    _app: Option<tauri::AppHandle>,
) -> Result<(), String> {
    let turn_started = Instant::now();
    let sessions_root = roots.sessions();
    let requested_id = request
        .get("sessionId")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let ctx = PromptContext {
        session_id: requested_id.clone().unwrap_or_else(new_session_id),
        cwd: request
            .get("cwd")
            .and_then(Value::as_str)
            .unwrap_or(".")
            .to_string(),
        mode: request
            .get("mode")
            .and_then(Value::as_str)
            .unwrap_or("build")
            .to_string(),
    };
    let cwd_path = std::path::PathBuf::from(&ctx.cwd);
    let cwd_path = cwd_path
        .canonicalize()
        .unwrap_or_else(|_| std::path::PathBuf::from(&ctx.cwd));

    let env = config::process_env();
    let config_value = roots.load_config(request.get("alkaidServerConfig").cloned())?;
    let resolved = config::resolve_model(
        &config_value,
        request.get("model").and_then(Value::as_str),
        &env,
    )?;
    let thinking_level = resolved.thinking_level.clone().or_else(|| {
        request
            .get("reasoningEffort")
            .and_then(Value::as_str)
            .map(str::to_string)
    });
    let resolved = Resolved {
        thinking_level,
        ..resolved
    };
    let lightweight = request
        .get("lightweightModel")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .and_then(|selection| config::resolve_model(&config_value, Some(selection), &env).ok());
    let read_only = ctx.mode == "plan";

    let (mut text, images) = prompt_input(
        request
            .get("parts")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .as_slice(),
    );
    let skills = load_skills(roots);
    text = expand_skill_command(&text, &skills);

    let agent_instructions = load_agent_instructions(roots);
    let settings = crate::settings::Settings::load(&crate::lyra::config::nova_root());
    let auto_change_project = settings.auto_change_project_enabled;
    let shell = (!read_only).then(prompt::detect_shell);
    let prompt_options = SystemPromptOptions {
        cwd: cwd_path.display().to_string(),
        read_only,
        fast_context,
        auto_change_project,
        shell: shell.clone(),
        skills_text: format_skills_prompt(&skills),
        custom_instructions: agent_instructions,
        ponytail: settings.ponytail_enabled,
    };
    let system_prompt = build_system_prompt(&prompt_options);
    let mut tools = tool_set(read_only, fast_context, auto_change_project);
    tools.extend(context_tools());
    tools.extend(session::agent_tools());
    let tool_shape = serde_json::to_string(
        &tools
            .iter()
            .map(|tool| json!({ "name": tool.name, "description": tool.description, "parameters": tool.parameters }))
            .collect::<Vec<_>>(),
    )
    .unwrap_or_default();

    // ---- 会话：jsonl 规范历史 + 上下文投影账本 ----
    let (rollout, items) = Rollout::open(
        &sessions_root,
        &ctx.session_id,
        requested_id.is_some(),
        request.get("restoreAt").and_then(Value::as_str),
    )?;
    let overhead = estimate_text_tokens(&system_prompt) + estimate_text_tokens(&tool_shape);
    let context = ContextWindow::load(&sessions_root, &ctx.session_id, resolved.model.context_window, overhead);
    emit(&json!({
        "type": "timing",
        "phase": "context_shape",
        "elapsedMs": 0,
        "rewriteVersion": context.generation(),
        "systemPromptHash": stable_hash(system_prompt.as_bytes()),
        "toolSchemaHash": stable_hash(tool_shape.as_bytes()),
    }));
    let cancelled = Arc::new(AtomicBool::new(false));
    let steer = Arc::new(Mutex::new(std::collections::VecDeque::new()));
    let mut session = Session {
        http: http.clone(),
        summarizer: lightweight.unwrap_or_else(|| resolved.clone()),
        model: resolved,
        system_prompt,
        tools,
        cwd: cwd_path,
        session_id: ctx.session_id.clone(),
        archive_dir: Some(roots.data().join("tool-results").join(&ctx.session_id)),
        shell,
        history: History::new(items, Some(rollout)),
        context,
        cancelled: cancelled.clone(),
        steer: steer.clone(),
        agents: Some(session::root_handle(&ctx.session_id)),
    };

    emit(&json!({ "type": "ready", "sessionId": ctx.session_id }));

    // ---- steer / cancel 控制行消费（行源由调用方提供：stdin 或进程内通道） ----
    let command_busy = Arc::new(AtomicBool::new(false));
    let command_revision = Arc::new(AtomicU64::new(0));
    let line_consumer = {
        let steer = steer.clone();
        let cancelled = cancelled.clone();
        let command_busy = command_busy.clone();
        let command_revision = command_revision.clone();
        tokio::spawn(async move {
            while let Some(value) = line_rx.recv().await {
                match value.get("action").and_then(Value::as_str) {
                    Some("cancel") => {
                        cancelled.store(true, Ordering::SeqCst);
                    }
                    Some("steer") => {
                        command_busy.store(true, Ordering::SeqCst);
                        let (text, images) = prompt_input(
                            value
                                .get("parts")
                                .and_then(Value::as_array)
                                .cloned()
                                .unwrap_or_default()
                                .as_slice(),
                        );
                        steer.lock().unwrap().push_back(user_message(&text, &images));
                        command_revision.fetch_add(1, Ordering::SeqCst);
                        command_busy.store(false, Ordering::SeqCst);
                    }
                    _ => {}
                }
            }
        })
    };

    // ---- 事件 → 协议 items ----
    let mut total_usage = json!({});
    let mut agent_message_index = 0u64;
    let mut current_text = String::new();
    let mut current_thinking = String::new();
    let mut started_tools: std::collections::HashMap<String, Value> =
        std::collections::HashMap::new();

    let mut on_event = |event: TurnEvent| match event {
        TurnEvent::MessageStart => {
            agent_message_index += 1;
            emit(&json!({ "type": "timing", "phase": "provider_turn", "elapsedMs": 0 }));
            current_text.clear();
            current_thinking.clear();
            // 快照刷新可能清掉前端的临时 liveUsage；下一次 request 开始时用此前
            // request 已返回的真实累计 usage 重发一次，不做任何 token 估算。
            if total_usage.as_object().is_some_and(|usage| !usage.is_empty()) {
                emit(&json!({ "type": "usage", "usage": total_usage, "estimated": false }));
            }
        }
        TurnEvent::TextDelta(delta) => {
            current_text.push_str(&delta);
            emit(&json!({
                "type": "item",
                "item": { "id": format!("agent_message-{agent_message_index}"), "type": "agent_message", "text": current_text.as_str() },
            }));
        }
        TurnEvent::ThinkingDelta(delta) => {
            current_thinking.push_str(&delta);
            emit(&json!({
                "type": "item",
                "item": { "id": format!("reasoning-{agent_message_index}"), "type": "reasoning", "text": current_thinking.as_str() },
            }));
        }
        TurnEvent::ToolStart { id, name, args } => {
            let item = started_tool_item(&id, &name, &args);
            started_tools.insert(id, item.clone());
            emit(&json!({ "type": "item", "item": item }));
        }
        TurnEvent::ToolEnd { id, outcome } => {
            let started = started_tools
                .get(&id)
                .cloned()
                .unwrap_or_else(|| json!({ "id": id, "type": "mcp_tool_call", "server": "Lyra" }));
            emit(&json!({ "type": "item", "item": completed_tool_item(&started, &outcome) }));
            if let Some(cwd) = outcome
                .get("details")
                .and_then(|details| details.get("workingDirectory"))
                .and_then(Value::as_str)
            {
                emit(&json!({ "type": "working_directory_changed", "cwd": cwd }));
            }
            // 工具执行期间前端可能因运行态快照刷新而清空 liveUsage。在工具结束、
            // 下一次 provider request 之前重发上一 request 的真实累计值。
            if total_usage.as_object().is_some_and(|usage| !usage.is_empty()) {
                emit(&json!({ "type": "usage", "usage": total_usage, "estimated": false }));
            }
        }
        TurnEvent::MessageEnd { usage } => {
            // 费用字段按整轮累计；contextTokens 始终表示最后一次真实 provider 请求的输入上下文。
            let context_tokens: u64 = ["input", "cacheRead", "cacheWrite"]
                .iter()
                .map(|key| usage.get(key).and_then(Value::as_u64).unwrap_or(0))
                .sum();
            merge_usage(&mut total_usage, &usage);
            total_usage["contextTokens"] = json!(context_tokens);
            emit(&json!({ "type": "usage", "usage": total_usage, "estimated": false }));
        }
        TurnEvent::Retry { attempt, error, context_recovery } => {
            emit(&json!({
                "type": "timing",
                "phase": if context_recovery { "context_overflow_recovery" } else { "provider_retry" },
                "elapsedMs": turn_started.elapsed().as_millis() as u64,
                "error": error,
            }));
            let mut ready = json!({ "type": "ready", "sessionId": ctx.session_id, "retry": attempt });
            if context_recovery {
                ready["contextRecovery"] = json!(true);
            }
            emit(&ready);
        }
        TurnEvent::Compacted(receipt) => {
            emit(&json!({
                "type": "timing",
                "phase": "context_compaction",
                "elapsedMs": turn_started.elapsed().as_millis() as u64,
                "receipt": receipt,
            }));
        }
    };

    let mut input = Some(user_message(&text, &images));
    let outcome = loop {
        let outcome = run_turn(&mut session, input.take(), &mut on_event).await;
        if outcome.cancelled || outcome.error.is_some() || cancelled.load(Ordering::SeqCst) {
            break outcome;
        }
        // steer 可能在 turn 最后一次 drain 后才进入队列：等控制通道安静后再判定结束，
        // 否则用户的新指令会被静默遗留。
        settle_pending_input(&command_busy, &command_revision).await;
        if steer.lock().unwrap().is_empty() {
            break outcome;
        }
    };
    drop(on_event);
    line_consumer.abort();

    let usage_of = |key: &str| total_usage.get(key).and_then(Value::as_u64).unwrap_or(0);
    let (input_tokens, cache_read_tokens, cache_write_tokens) =
        (usage_of("input"), usage_of("cacheRead"), usage_of("cacheWrite"));
    let cache_denominator = if input_tokens >= cache_read_tokens + cache_write_tokens {
        input_tokens
    } else {
        input_tokens + cache_read_tokens + cache_write_tokens
    };
    emit(&json!({
        "type": "timing",
        "phase": "cache_shape",
        "elapsedMs": turn_started.elapsed().as_millis() as u64,
        "inputTokens": input_tokens,
        "cacheReadTokens": cache_read_tokens,
        "cacheWriteTokens": cache_write_tokens,
        "cacheHitRate": if cache_denominator > 0 { cache_read_tokens as f64 / cache_denominator as f64 } else { 0.0 },
        "rewriteVersion": session.context.generation(),
    }));

    if let Some(error) = outcome.error.filter(|_| !outcome.cancelled) {
        emit(&json!({ "ok": false, "error": format!("Lyra provider 请求失败：{error}") }));
        return Ok(());
    }
    emit(&json!({
        "type": "done",
        "usage": if total_usage.as_object().map(|o| o.is_empty()).unwrap_or(true) { Value::Null } else { total_usage },
        "cancelled": outcome.cancelled,
    }));
    Ok(())
}

fn models_data(request: &Value, roots: &Roots) -> Result<Value, String> {
    let config_value = roots.load_config(request.get("alkaidServerConfig").cloned())?;
    // 沿用旧 bridge的 configOptions 形状：前端与漫游/雷达都只认 id=="model" 的包裹结构，
    // 直接返回扁平选项列表会导致选择器永远为空。
    let current = config::default_model(&config_value)?; // 与 JS 一样先校验存在可用模型
    Ok(json!({
        "configOptions": [{
            "id": "model",
            "name": "Model",
            "currentValue": current,
            "options": config::model_options(&config_value),
        }],
        "modes": Value::Null,
    }))
}

async fn title_data(
    http: &reqwest::Client,
    request: &Value,
    roots: &Roots,
) -> Result<Value, String> {
    let env = config::process_env();
    let config_value = roots.load_config(request.get("alkaidServerConfig").cloned())?;
    let resolved = config::resolve_model(
        &config_value,
        request.get("model").and_then(Value::as_str),
        &env,
    )?;
    let prompt = request
        .get("prompt")
        .and_then(Value::as_str)
        .ok_or_else(|| "title 请求缺少 prompt".to_string())?;
    let messages = vec![user_message(prompt, &[])];
    let mut text = String::new();
    let result = stream_chat(
        http,
        &resolved.model,
        &resolved.api_key,
        Some("off"),
        "你是代码助手。",
        &messages,
        &[],
        Some(&new_session_id()),
        &Arc::new(AtomicBool::new(false)),
        &mut |event| {
            if let StreamEvent::TextDelta(delta) = event {
                text.push_str(&delta);
            }
        },
    )
    .await?;
    if result.stop_reason == "error" {
        return Err(result
            .error_message
            .unwrap_or_else(|| "title 生成失败".into()));
    }
    Ok(Value::String(text.trim().to_string()))
}

async fn complete_data(
    http: &reqwest::Client,
    request: &Value,
    roots: &Roots,
) -> Result<Value, String> {
    let prompt = request
        .get("prompt")
        .and_then(Value::as_str)
        .ok_or_else(|| "complete 请求缺少 prompt".to_string())?;
    let data = crate::lyra_complete::complete_direct(
        http,
        roots.nova(),
        &config::process_env(),
        request.get("model").and_then(Value::as_str).unwrap_or(""),
        prompt,
    )
    .await
    .map_err(|e| format!("{e:?}"))?;
    Ok(Value::String(data))
}

fn export_data(request: &Value, roots: &Roots) -> Result<Value, String> {
    let config_value = roots.load_config(request.get("alkaidServerConfig").cloned())?;
    let selected = request["model"].as_str().ok_or("共享导出缺少模型")?;
    let selections: Vec<String> = serde_json::from_value(request["sharedModels"].clone())
        .map_err(|_| "共享导出缺少模型白名单")?;
    let config_value = config::shared_config(&config_value, &selections, selected)?;
    let resolved = config::resolve_config_env(&config_value, &config::process_env())?;
    Ok(Value::String(
        serde_json::to_string_pretty(&resolved).map_err(|e| e.to_string())?,
    ))
}

/// 一次性请求（models/title/complete/export）：进程内直接调用，返回 data 载荷。
/// borrowed_root：借用额度运行时的隔离数据根（凭证即其中的 alkaid/config.jsonc）；
/// None 表示主运行时，使用全局数据根。
pub async fn run_oneshot(
    http: &reqwest::Client,
    request: &Value,
    borrowed_root: Option<PathBuf>,
) -> Result<Value, String> {
    let roots = borrowed_root
        .map(Roots::borrowed)
        .unwrap_or_else(Roots::global);
    match request.get("action").and_then(Value::as_str) {
        Some("models") => models_data(request, &roots),
        Some("title") => title_data(http, request, &roots).await,
        Some("complete") => complete_data(http, request, &roots).await,
        Some("export") => export_data(request, &roots),
        Some(other) => Err(format!("Lyra 不支持的 action：{other}")),
        None => Err("Lyra 请求缺少 action".into()),
    }
}

/// 进程内 prompt 会话：事件以 JSONL 字符串流入 events，控制行（cancel/steer）写入 control。
/// borrowed_root 同 run_oneshot：借用额度只换数据根（不同凭证），不做进程隔离。
pub struct InProcessSession {
    pub control: tokio::sync::mpsc::UnboundedSender<String>,
    pub events: tokio::sync::mpsc::UnboundedReceiver<String>,
    pub task: tokio::task::JoinHandle<()>,
}

pub fn spawn_prompt(
    http: reqwest::Client,
    request: Value,
    fast_context: bool,
    borrowed_root: Option<PathBuf>,
    app: Option<tauri::AppHandle>,
) -> InProcessSession {
    let roots = borrowed_root
        .map(Roots::borrowed)
        .unwrap_or_else(Roots::global);
    let (control_tx, mut control_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let (event_tx, events) = tokio::sync::mpsc::unbounded_channel::<String>();
    let task = tokio::spawn(async move {
        let emit: Emit = Arc::new(move |value: &Value| {
            let _ = event_tx.send(value.to_string());
        });
        let (line_tx, line_rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
        let pump = tokio::spawn(async move {
            while let Some(line) = control_rx.recv().await {
                if let Ok(value) = serde_json::from_str::<Value>(&line) {
                    if line_tx.send(value).is_err() {
                        break;
                    }
                }
            }
        });
        if let Err(error) =
            handle_prompt(&http, &request, &emit, fast_context, &roots, line_rx, app).await
        {
            emit(&json!({ "ok": false, "error": error }));
        }
        pump.abort();
    });
    InProcessSession {
        control: control_tx,
        events,
        task,
    }
}

async fn dispatch(http: &reqwest::Client, request: &Value) -> Result<(), String> {
    let roots = Roots::global();
    match request.get("action").and_then(Value::as_str) {
        Some("prompt") => {
            let emit = stdout_emit();
            let fast_context = std::env::var("NOVA_FAST_CONTEXT").ok().as_deref() != Some("0");
            let (line_tx, line_rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
            tokio::spawn(async move {
                use tokio::io::AsyncBufReadExt;
                let stdin = tokio::io::stdin();
                let mut lines = tokio::io::BufReader::new(stdin).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if let Ok(value) = serde_json::from_str::<Value>(&line) {
                        if line_tx.send(value).is_err() {
                            break;
                        }
                    }
                }
            });
            handle_prompt(http, request, &emit, fast_context, &roots, line_rx, None).await
        }
        Some("models") => {
            let data = models_data(request, &roots)?;
            send(&json!({ "ok": true, "data": data }));
            Ok(())
        }
        Some("title") => {
            let data = title_data(http, request, &roots).await?;
            send(&json!({ "ok": true, "data": data }));
            Ok(())
        }
        Some("complete") => {
            let data = complete_data(http, request, &roots).await?;
            send(&json!({ "ok": true, "data": data }));
            Ok(())
        }
        Some("export") => {
            let data = export_data(request, &roots)?;
            send(&json!({ "ok": true, "data": data }));
            Ok(())
        }
        Some(other) => Err(format!("Lyra 不支持的 action：{other}")),
        None => Err("Lyra 请求缺少 action".into()),
    }
}

pub async fn run() -> i32 {
    use tokio::io::AsyncBufReadExt;
    let settings = crate::settings::Settings::load(&config::nova_root());
    let http = super::provider::client_for_proxy(settings.lyra_proxy.trim());
    let stdin = tokio::io::stdin();
    let mut lines = tokio::io::BufReader::new(stdin).lines();
    let first = match lines.next_line().await {
        Ok(Some(line)) => line,
        _ => return 0, // 无请求直接退出（prewarm 等场景）
    };
    let request: Value = match serde_json::from_str(&first) {
        Ok(value) => value,
        Err(e) => {
            send_error(format!("Lyra 请求解析失败：{e}"));
            return 1;
        }
    };
    match dispatch(&http, &request).await {
        Ok(()) => 0,
        Err(error) => {
            send_error(error);
            1
        }
    }
}

#[cfg(test)]
mod tests {

    /// 借用额度运行时：进程内按隔离数据根加载凭证配置（不起子进程、不读全局配置）。
    #[tokio::test]
    async fn borrowed_root_loads_isolated_config() {
        let root =
            std::env::temp_dir().join(format!("nova-lyra-borrowed-test-{}", uuid::Uuid::new_v4()));
        let config_dir = root.join("alkaid");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(
            config_dir.join("config.jsonc"),
            r#"{
                "model": "borrowed/test-model",
                "provider": {
                    "borrowed": {
                        "name": "Borrowed",
                        "api": "openai-completions",
                        "options": { "baseURL": "http://127.0.0.1:9", "apiKey": "borrowed-key" },
                        "models": { "test-model": { "name": "Test Model" } }
                    }
                }
            }"#,
        )
        .unwrap();
        let data = super::run_oneshot(
            &reqwest::Client::new(),
            &serde_json::json!({ "action": "models" }),
            Some(root.clone()),
        )
        .await
        .expect("models");
        let options = data
            .pointer("/configOptions/0/options")
            .and_then(|v| v.as_array())
            .unwrap();
        assert!(
            options
                .iter()
                .any(|o| o.get("value").and_then(|v| v.as_str()) == Some("borrowed/test-model")),
            "未从借用数据根加载模型：{data}"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn pending_input_settlement_waits_for_late_command() {
        use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
        use std::sync::Arc;

        let busy = Arc::new(AtomicBool::new(false));
        let revision = Arc::new(AtomicU64::new(0));
        let writer_busy = busy.clone();
        let writer_revision = revision.clone();
        let writer = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            writer_busy.store(true, Ordering::SeqCst);
            writer_revision.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            writer_busy.store(false, Ordering::SeqCst);
        });

        super::settle_pending_input(&busy, &revision).await;
        writer.await.unwrap();
        assert_eq!(revision.load(Ordering::SeqCst), 1);
        assert!(!busy.load(Ordering::SeqCst));
    }
    /// 端到端：spawn_prompt 事件流完整；带 sessionId 的第二次提示从 jsonl 恢复并把上一轮发给模型。
    #[tokio::test]
    async fn second_prompt_resumes_history_from_rollout() {
        use crate::lyra::turn::tests::{capturing_server, text_reply};
        let (url, bodies) = capturing_server(vec![text_reply("one"), text_reply("two")]).await;
        let root = std::env::temp_dir().join(format!("nova-lyra-resume-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("alkaid")).unwrap();
        let config = serde_json::json!({
            "model": "local/m",
            "provider": { "local": { "name": "Local", "api": "openai-completions",
                "options": { "baseURL": url, "apiKey": "k" }, "models": { "m": { "name": "M" } } } }
        });
        std::fs::write(root.join("alkaid").join("config.jsonc"), config.to_string()).unwrap();
        let prompt = |text: &str, session_id: Option<&str>| {
            let mut request = serde_json::json!({ "action": "prompt", "cwd": root, "mode": "plan",
                "parts": [{ "type": "text", "text": text }] });
            if let Some(id) = session_id {
                request["sessionId"] = serde_json::json!(id);
            }
            let http = reqwest::Client::builder().no_proxy().build().unwrap();
            let mut session = super::spawn_prompt(http, request, false, Some(root.clone()), None);
            async move {
                let mut events = Vec::new();
                while let Some(line) = session.events.recv().await {
                    events.push(serde_json::from_str::<serde_json::Value>(&line).unwrap());
                }
                events
            }
        };
        let first = prompt("hi", None).await;
        let session_id = first.iter().find_map(|e| e["sessionId"].as_str()).unwrap().to_string();
        assert!(first.iter().any(|e| e["item"]["text"] == "one"), "{first:?}");
        assert!(first.iter().any(|e| e["type"] == "done"), "{first:?}");
        let second = prompt("again", Some(&session_id)).await;
        assert!(second.iter().any(|e| e["type"] == "done"), "{second:?}");
        let resumed = &bodies.lock().unwrap()[1];
        assert!(resumed.contains("hi") && resumed.contains("one") && resumed.contains("again"), "{resumed}");
        let _ = std::fs::remove_dir_all(root);
    }

    /// 手动端到端验证（需真实 provider 配置）：进程内 spawn_prompt 全事件流 + run_oneshot。
    #[tokio::test]
    #[ignore = "需要真实 provider 配置，手动验证用"]
    async fn inprocess_prompt_and_oneshot_smoke() {
        let http = reqwest::Client::new();
        let models = super::run_oneshot(&http, &serde_json::json!({ "action": "models" }), None)
            .await
            .expect("models");
        assert!(models.get("configOptions").is_some());

        let session = super::spawn_prompt(
            http,
            serde_json::json!({
                "action": "prompt",
                "cwd": std::env::current_dir().unwrap(),
                "mode": "build",
                "parts": [{ "type": "text", "text": "运行 bash 工具执行 echo lyra-inprocess，然后简短汇报" }],
            }),
            true,
            None,
            None,
        );
        let abort = session.task.abort_handle();
        let mut joined = String::new();
        let mut events = session.events;
        while let Some(line) = events.recv().await {
            joined.push_str(&line);
            joined.push('\n');
        }
        let _ = session.task.await;
        drop(abort);
        assert!(joined.contains("\"ready\""), "缺少 ready：{joined}");
        assert!(joined.contains("\"done\""), "缺少 done：{joined}");
        assert!(!joined.contains("\"ok\":false"), "出现错误事件：{joined}");
        assert!(
            joined.contains("lyra-inprocess"),
            "未执行 bash 工具：{joined}"
        );
        // request 级 usage 必须在工具结束前到达；否则 UI 只能在整个 turn 完成后变化。
        let first_usage = joined
            .find("\"type\":\"usage\"")
            .expect("缺少 request usage");
        let tool_completed = joined
            .find("\"status\":\"completed\"")
            .expect("缺少工具完成事件");
        assert!(
            first_usage < tool_completed,
            "request usage 未在工具执行前上报：{joined}"
        );
    }
}
