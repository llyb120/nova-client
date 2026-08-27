//! stdio JSONL 协议（与 alkaid-bridge 兼容）：首行为请求，prompt 期间后续行为
//! cancel/steer；事件 ready/item/timing/done/{ok:false}。Reasonix 会话生命周期在此串联。

use crate::lyra::agent::{estimate_text_tokens, user_message, Agent, AgentEvent};
use crate::lyra::config::{self, Resolved, Roots};
use crate::lyra::prompt::{
    self, build_system_prompt, expand_skill_command, format_skills_prompt, image_media_type,
    is_context_window_error, is_retryable_provider_error, load_agent_instructions, load_skills,
    merge_usage, system_prompt_fingerprint, SystemPromptOptions, PROVIDER_RETRY_DELAYS_MS,
};
use crate::lyra::provider::{stream_chat, StreamEvent};
use crate::lyra::reasonix::{self, context_tokens_from_messages, load_legacy_messages, SlimMemory};
use crate::lyra::tools::tool_set;
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

fn send_timing(emit: &Emit, phase: &str, start: Instant) {
    emit(&json!({
        "type": "timing",
        "phase": phase,
        "elapsedMs": start.elapsed().as_millis() as u64,
    }));
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

fn truncate_at_restore(messages: Vec<Value>, restore_at: Option<&str>) -> Vec<Value> {
    let Some(restore_at) = restore_at else {
        return messages;
    };
    if restore_at.is_empty() {
        return messages;
    }
    match messages
        .iter()
        .position(|m| m.get("timestamp").map(|t| t.to_string()).as_deref() == Some(restore_at))
    {
        Some(index) => messages[..=index].to_vec(),
        None => messages,
    }
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

async fn summarize_with_model(
    http: &reqwest::Client,
    resolved: &Resolved,
    text: &str,
) -> Result<String, String> {
    let prompt = "把下面的工作过程压缩成一段简洁的延续摘要，覆盖目标、关键决定、已完成的修改、验证结果与后续事项。不要加入原文没有的信息。\n\n".to_string() + text;
    let messages = vec![user_message(&prompt, &[])];
    let mut result_text = String::new();
    let result = stream_chat(
        http,
        &resolved.model,
        &resolved.api_key,
        None,
        "你是代码助手。",
        &messages,
        &[],
        None,
        &Arc::new(AtomicBool::new(false)),
        &mut |event| {
            if let StreamEvent::TextDelta(delta) = event {
                result_text.push_str(&delta);
            }
        },
    )
    .await?;
    if result.stop_reason == "error" {
        return Err(result
            .error_message
            .unwrap_or_else(|| "summarize failed".into()));
    }
    Ok(result_text)
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
) -> Result<(), String> {
    let turn_started = Instant::now();
    let sessions_root = roots.sessions();
    let ctx = PromptContext {
        session_id: request
            .get("sessionId")
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(new_session_id),
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

    // ---- Reasonix：加载精简记忆 / 旧会话播种 / restoreAt 截断 ----
    let mut memory = SlimMemory::load(&sessions_root, &ctx.session_id);
    // 上一轮被强杀/panic/退出时轮末写盘没机会执行：增量 checkpoint 保留了中断轨迹。
    // slim 已有 pendingMessages（优雅取消/失败已保存）时以 slim 为准，不覆盖。
    if memory.pending_messages.is_empty() {
        if let Some(pending) = reasonix::load_pending_checkpoint(&sessions_root, &ctx.session_id) {
            memory.pending_messages = pending;
        }
    }
    if memory.turns.is_empty()
        && memory.digests.is_empty()
        && memory.pending_messages.is_empty()
        && request.get("sessionId").and_then(Value::as_str).is_some()
    {
        let legacy = truncate_at_restore(
            load_legacy_messages(&sessions_root, &ctx.session_id),
            request.get("restoreAt").and_then(Value::as_str),
        );
        if !legacy.is_empty() {
            memory.seed_from_messages(&legacy);
        }
    }

    let agent_instructions = load_agent_instructions(roots);
    let settings = crate::settings::Settings::load(&crate::lyra::config::nova_root());
    let memory_enabled = settings.experience_training_enabled;
    let auto_change_project = settings.auto_change_project_enabled;
    let ponytail = settings.ponytail_enabled;
    let shell = (!read_only).then(prompt::detect_shell);
    let skills_text = format_skills_prompt(&skills);
    let prompt_options = SystemPromptOptions {
        cwd: cwd_path.display().to_string(),
        read_only,
        fast_context,
        memory_enabled,
        auto_change_project,
        shell: shell.clone(),
        skills_text,
        custom_instructions: agent_instructions,
        ponytail,
    };
    let fingerprint = system_prompt_fingerprint(&prompt_options);
    let system_changed = memory.system_fingerprint != fingerprint;
    if system_changed {
        memory.system_fingerprint = fingerprint;
        memory.system_prompt_snapshot.clear();
        memory.system_prompt_hash.clear();
        memory.last_shape_rewrite_version = 0;
    }

    let context_window = resolved.model.context_window;
    let measured_tokens = memory
        .pending_messages
        .is_empty()
        .then(|| context_tokens_from_messages(&memory.full_messages))
        .unwrap_or_else(|| context_tokens_from_messages(&memory.pending_messages))
        .max(memory.context_tokens);
    let current_tier = reasonix::pressure_tier(measured_tokens, context_window);
    memory.context_tier = current_tier.into();
    let max_context_tokens = ((context_window as f64) * 0.75) as u64;
    let force_context_tokens = ((context_window as f64) * 0.9) as u64;
    let max_context_chars = force_context_tokens.saturating_mul(4) as usize;
    let mut use_full_context =
        reasonix::should_use_full_context(&memory, force_context_tokens, max_context_chars);
    memory.append_turn(&text);
    // 摘要容量判断基于重建后的 slim memory，而不是即将丢弃的
    // 原生 reasoning/tool trajectory usage。
    let rebuilt_context_tokens = estimate_text_tokens(&memory.format());
    let summarize_model = lightweight.clone().unwrap_or_else(|| resolved.clone());
    if memory
        .compact(rebuilt_context_tokens, max_context_tokens, |earlier| {
            let http = http.clone();
            let model = summarize_model.clone();
            async move { summarize_with_model(&http, &model, &earlier).await }
        })
        .await
    {
        memory.context_stage = "slim".into();
        memory.context_tokens = 0;
        memory.full_messages.clear();
        use_full_context = false;
    }
    if !use_full_context && memory.context_stage == "full" {
        memory.context_stage = "slim".into();
        memory.context_tokens = 0;
        memory.full_messages.clear();
        memory.rewrite_version += 1;
    }

    let resumed_pending_turn = !memory.pending_messages.is_empty();
    let mut native_messages: Vec<Value> = if resumed_pending_turn {
        reasonix::repair_interrupted_tool_pairs(&std::mem::take(&mut memory.pending_messages))
    } else if use_full_context {
        memory.full_messages.clone()
    } else {
        Vec::new()
    };
    let strips_completed_reasoning =
        resolved.model.api.starts_with("openai") || resolved.model.api == "azure-openai-responses";
    if !resumed_pending_turn && strips_completed_reasoning {
        native_messages = reasonix::strip_completed_openai_reasoning(&native_messages);
    }

    let active_turn_start = if resumed_pending_turn {
        0
    } else if native_messages.is_empty() {
        -1
    } else {
        native_messages.len() as i64
    };
    memory.pending_messages = native_messages.clone();

    // ---- Agent 构建 ----
    let system_prompt = if !memory.system_prompt_snapshot.is_empty() && !system_changed {
        memory.system_prompt_snapshot.clone()
    } else {
        build_system_prompt(&prompt_options)
    };
    let agent_tools = tool_set(read_only, fast_context, memory_enabled, auto_change_project);
    let system_prompt_hash = stable_hash(system_prompt.as_bytes());
    let tool_shape = serde_json::to_string(
        &agent_tools
            .iter()
            .map(|tool| {
                json!({
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.parameters,
                })
            })
            .collect::<Vec<_>>(),
    )
    .unwrap_or_default();
    let tool_schema_hash = stable_hash(tool_shape.as_bytes());
    let mut cache_miss_reasons = Vec::new();
    if !memory.system_prompt_hash.is_empty() && memory.system_prompt_hash != system_prompt_hash {
        cache_miss_reasons.push("system_changed");
    }
    if !memory.tool_schema_hash.is_empty() && memory.tool_schema_hash != tool_schema_hash {
        cache_miss_reasons.push("tools_changed");
    }
    if memory.last_shape_rewrite_version != memory.rewrite_version {
        cache_miss_reasons.push("history_rewritten");
    }
    memory.system_prompt_hash = system_prompt_hash.clone();
    memory.tool_schema_hash = tool_schema_hash.clone();
    memory.last_shape_rewrite_version = memory.rewrite_version;
    let history_hash = stable_hash(
        memory
            .without_current(!memory.pending_messages.is_empty())
            .format()
            .as_bytes(),
    );
    emit(&json!({
        "type": "timing",
        "phase": "context_shape",
        "elapsedMs": 0,
        "contextTier": memory.context_tier,
        "rewriteVersion": memory.rewrite_version,
        "cacheMissReasons": cache_miss_reasons,
        "systemPromptHash": system_prompt_hash,
        "toolSchemaHash": tool_schema_hash,
        "historyHash": history_hash,
    }));
    let archive_dir = Some(roots.data().join("tool-results").join(&ctx.session_id));
    let cancelled = Arc::new(AtomicBool::new(false));
    let steering = Arc::new(Mutex::new(std::collections::VecDeque::new()));
    let mut agent = Agent {
        model: resolved.clone(),
        system_prompt,
        messages: native_messages.clone(),
        tools: agent_tools,
        cwd: cwd_path.clone(),
        session_id: ctx.session_id.clone(),
        archive_dir,
        shell,
        cancelled: cancelled.clone(),
        steering: steering.clone(),
        spec_cache: Arc::new(Mutex::new(std::collections::HashMap::new())),
        checkpoint: None,
        watchdog: Some((
            crate::lyra::watchdog::IdleWatchdog::new(),
            Arc::new(crate::lyra::watchdog::DiagnosticLog::new(Some(
                sessions_root.clone(),
            ))),
        )),
    };
    // 后台单写者：Agent 热路径只更新最新 snapshot；300ms debounce 后由
    // spawn_blocking 完成序列化与原子写盘，不阻塞 provider/tool 执行。
    let checkpoint_writer = Arc::new(Mutex::new(Some(reasonix::PendingCheckpointWriter::spawn(
        sessions_root.clone(),
        ctx.session_id.clone(),
    ))));
    {
        let writer = checkpoint_writer.clone();
        agent.checkpoint = Some(Box::new(move |messages: &[Value]| {
            if let Some(writer) = writer.lock().unwrap().as_ref() {
                writer.checkpoint(messages);
            }
        }));
    }

    emit(&json!({ "type": "ready", "sessionId": ctx.session_id }));

    // ---- steer / cancel 控制行消费（行源由调用方提供：stdin 或进程内通道） ----
    let command_busy = Arc::new(AtomicBool::new(false));
    let command_revision = Arc::new(AtomicU64::new(0));
    let line_consumer = {
        let steering = steering.clone();
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
                        steering
                            .lock()
                            .unwrap()
                            .push_back(user_message(&text, &images));
                        command_revision.fetch_add(1, Ordering::SeqCst);
                        command_busy.store(false, Ordering::SeqCst);
                    }
                    _ => {}
                }
            }
        })
    };

    let prompt_text = if !resumed_pending_turn && !use_full_context {
        reasonix::message_with_slim_memory(&text, &memory)
    } else {
        text
    };

    // ---- Reasonix 中途维护闭包 ----
    let context_window_u = context_window;
    let rebase_memory = memory.clone();
    let mid_turn_state = Arc::new(Mutex::new((0_u64, 0_u64, String::new())));
    let mut mid_turn = {
        let mut last_rewrite_turn: usize = usize::MAX;
        let mid_turn_state = mid_turn_state.clone();
        move |messages: &mut Vec<Value>, assistant: &Value| {
            let measured = reasonix::context_tokens_from_messages(std::slice::from_ref(assistant));
            let measured = measured.max(reasonix::context_tokens_from_messages(messages));
            let tier = reasonix::pressure_tier(measured, context_window_u);
            if !matches!(tier, "normal" | "warn") {
                let started = Instant::now();
                let (next, changed) = reasonix::compact_native_tool_results(messages, tier);
                let mut rewritten = false;
                if changed {
                    *messages = next;
                    rewritten = true;
                }
                let turn_count = messages.len();
                if tier == "force" && turn_count > 0 && last_rewrite_turn != turn_count {
                    let (next, rebased) = reasonix::rebase_native_context(
                        messages,
                        active_turn_start,
                        &rebase_memory,
                    );
                    if rebased {
                        *messages = next;
                        rewritten = true;
                        last_rewrite_turn = turn_count;
                    }
                }
                if rewritten {
                    let mut state = mid_turn_state.lock().unwrap();
                    state.0 += 1;
                    state.1 = measured;
                    state.2 = tier.into();
                    send_timing(emit, "mid_turn_context_rewrite", started);
                }
            }
        }
    };

    // ---- 事件 → 协议 items ----
    let mut total_usage = json!({});
    let mut last_context_tokens = 0u64;
    let mut agent_message_index = 0u64;
    let mut current_text = String::new();
    let mut current_thinking = String::new();
    let mut started_tools: std::collections::HashMap<String, Value> =
        std::collections::HashMap::new();

    let mut on_event = |event: AgentEvent| match event {
        AgentEvent::MessageStart => {
            agent_message_index += 1;
            emit(&json!({ "type": "timing", "phase": "provider_turn", "elapsedMs": 0 }));
            current_text.clear();
            current_thinking.clear();
            // 快照刷新可能清掉前端的临时 liveUsage；下一次 request 开始时用此前
            // request 已返回的真实累计 usage 重发一次，不做任何 token 估算。
            if total_usage
                .as_object()
                .is_some_and(|usage| !usage.is_empty())
            {
                emit(&json!({ "type": "usage", "usage": total_usage, "estimated": false }));
            }
        }
        AgentEvent::TextDelta(delta) => {
            current_text.push_str(&delta);
            emit(&json!({
                "type": "item",
                "item": { "id": format!("agent_message-{agent_message_index}"), "type": "agent_message", "text": current_text.as_str() },
            }));
        }
        AgentEvent::ThinkingDelta(delta) => {
            current_thinking.push_str(&delta);
            emit(&json!({
                "type": "item",
                "item": { "id": format!("reasoning-{agent_message_index}"), "type": "reasoning", "text": current_thinking.as_str() },
            }));
        }

        AgentEvent::ToolStart { id, name, args } => {
            let item = started_tool_item(&id, &name, &args);
            started_tools.insert(id, item.clone());
            emit(&json!({ "type": "item", "item": item }));
        }
        AgentEvent::ToolEnd { id, outcome, .. } => {
            let started = started_tools
                .get(&id)
                .cloned()
                .unwrap_or_else(|| json!({ "id": id, "type": "mcp_tool_call", "server": "Lyra" }));
            let item = completed_tool_item(&started, &outcome);
            emit(&json!({ "type": "item", "item": item }));
            if outcome.get("specHit").and_then(Value::as_bool) == Some(true) {
                emit(&json!({ "type": "timing", "phase": "spec_hit", "elapsedMs": 0 }));
            }
            if let Some(cwd) = outcome
                .get("details")
                .and_then(|details| details.get("workingDirectory"))
                .and_then(Value::as_str)
            {
                emit(&json!({ "type": "working_directory_changed", "cwd": cwd }));
            }
            // 工具执行期间前端可能因运行态快照刷新而清空 liveUsage。在工具结束、
            // 下一次 provider request 之前重发上一 request 的真实累计值。
            if total_usage
                .as_object()
                .is_some_and(|usage| !usage.is_empty())
            {
                emit(&json!({ "type": "usage", "usage": total_usage, "estimated": false }));
            }
        }
        AgentEvent::MessageEnd { usage } => {
            // 费用字段按整轮累计；contextTokens 始终表示最后一次真实 provider 请求的输入上下文。
            let input = usage.get("input").and_then(Value::as_u64).unwrap_or(0);
            let cache_read = usage.get("cacheRead").and_then(Value::as_u64).unwrap_or(0);
            let cache_write = usage.get("cacheWrite").and_then(Value::as_u64).unwrap_or(0);
            last_context_tokens = input.saturating_add(cache_read).saturating_add(cache_write);
            merge_usage(&mut total_usage, &usage);
            total_usage["contextTokens"] = json!(last_context_tokens);
            emit(&json!({ "type": "usage", "usage": total_usage, "estimated": false }));
        }
    };

    // ---- 带重试的执行 ----
    let mut retries = 0usize;
    let mut pending = Some((prompt_text, images));
    let mut context_recovery_attempted = false;
    let outcome = loop {
        let attempt = if let Some((text, images)) = pending.take() {
            agent
                .prompt(http, &text, images, &mut on_event, &mut mid_turn)
                .await
        } else {
            agent.continue_run(http, &mut on_event, &mut mid_turn).await
        };
        let outcome = match attempt {
            Ok(outcome) => outcome,
            Err(error) => crate::lyra::agent::TurnOutcome {
                cancelled: false,
                stop_reason: "error".into(),
                error: Some(error),
            },
        };
        let provider_error = outcome.error.clone().filter(|e| !e.is_empty());
        if provider_error.is_none() || outcome.cancelled {
            if !outcome.cancelled && !cancelled.load(Ordering::SeqCst) {
                settle_pending_input(&command_busy, &command_revision).await;
                if !steering.lock().unwrap().is_empty() {
                    // steer 可能在 Agent 最后一次 drain 后才进入队列；保持同一 turn，
                    // 从现有轨迹继续，不能静默结束并遗留用户的新指令。
                    continue;
                }
            }
            break outcome;
        }
        let error = provider_error.unwrap();
        let context_overflow = is_context_window_error(&error);
        if context_overflow && !context_recovery_attempted && !cancelled.load(Ordering::SeqCst) {
            // 失败 assistant 只是 provider 占位，不能带入下一次请求。
            while agent
                .messages
                .last()
                .and_then(|m| m.get("role"))
                .and_then(Value::as_str)
                == Some("assistant")
            {
                agent.messages.pop();
            }
            let (compacted, tools_changed) =
                reasonix::compact_all_native_tool_results(&agent.messages);
            agent.messages = compacted;
            let (rebased, history_changed) =
                reasonix::rebase_native_context(&agent.messages, active_turn_start, &memory);
            if history_changed {
                agent.messages = rebased;
            }
            // 中断恢复时 active_turn_start=0，若工具结果也无法再压，重试不会改变请求形状。
            if !tools_changed && !history_changed {
                break outcome;
            }
            context_recovery_attempted = true;
            memory.context_stage = "slim".into();
            memory.context_tier = "force".into();
            memory.context_tokens = 0;
            memory.full_messages.clear();
            memory.pending_messages = agent.messages.clone();
            memory.rewrite_version += 1;
            let _ = memory.save(&sessions_root, &ctx.session_id);
            send_timing(emit, "context_overflow_recovery", turn_started);
            emit(&json!({
                "type": "ready",
                "sessionId": ctx.session_id,
                "retry": retries + 1,
                "contextRecovery": true,
            }));
            continue;
        }
        if retries >= PROVIDER_RETRY_DELAYS_MS.len()
            || !is_retryable_provider_error(&error)
            || cancelled.load(Ordering::SeqCst)
        {
            break outcome;
        }
        // 去掉失败的 assistant 占位消息后重试。
        while agent
            .messages
            .last()
            .and_then(|m| m.get("role"))
            .and_then(Value::as_str)
            == Some("assistant")
        {
            agent.messages.pop();
        }
        send_timing(emit, "provider_retry", turn_started);
        emit(&json!({
            "type": "ready",
            "sessionId": ctx.session_id,
            "retry": retries + 1,
        }));
        tokio::time::sleep(std::time::Duration::from_millis(
            PROVIDER_RETRY_DELAYS_MS[retries],
        ))
        .await;
        retries += 1;
        if cancelled.load(Ordering::SeqCst) {
            break crate::lyra::agent::TurnOutcome {
                cancelled: true,
                stop_reason: "aborted".into(),
                error: None,
            };
        }
    };
    drop(on_event);
    drop(mid_turn);
    let (mid_turn_rewrite_count, mid_turn_context_tokens, mid_turn_context_tier) = {
        let state = mid_turn_state.lock().unwrap();
        (state.0, state.1, state.2.clone())
    };
    if mid_turn_rewrite_count > 0 {
        memory.rewrite_version = memory
            .rewrite_version
            .saturating_add(mid_turn_rewrite_count);
        memory.context_tokens = mid_turn_context_tokens;
        memory.context_tier = mid_turn_context_tier;
    }
    line_consumer.abort();

    let failed = outcome
        .error
        .clone()
        .filter(|e| !e.is_empty() && outcome.stop_reason == "error");

    // ---- Reasonix：写回精简记忆 ----
    if !outcome.cancelled && failed.is_none() {
        let conclusions: Vec<Value> = agent
            .messages
            .iter()
            .filter(|m| m.get("role").and_then(Value::as_str) == Some("assistant"))
            .filter(|m| m.get("stopReason").and_then(Value::as_str) != Some("error"))
            .cloned()
            .collect();
        if let Some(last) = conclusions.last() {
            memory.set_latest_conclusion(last.get("content").unwrap_or(&Value::Null));
        }
        memory.pending_messages.clear();
        let measured = context_tokens_from_messages(&agent.messages);
        if memory.context_stage == "full" {
            let base: Vec<Value> = native_messages
                .iter()
                .take(if active_turn_start >= 0 {
                    active_turn_start as usize
                } else {
                    0
                })
                .cloned()
                .collect();
            let mut full: Vec<Value> = base;
            // 合并本会话产生的新消息（用户提示及之后）
            let prefix_len = active_turn_start.max(0) as usize;
            let mut new_messages = agent.messages[prefix_len.min(agent.messages.len())..].to_vec();
            full.append(&mut new_messages);
            let pressure = reasonix::pressure_tier(measured, context_window);
            let (compacted, changed) = reasonix::compact_native_tool_results(&full, pressure);
            memory.full_messages = compacted;
            memory.context_tokens = measured;
            memory.context_tier = pressure.into();
            if changed {
                memory.rewrite_version += 1;
            }
            if !reasonix::should_use_full_context(&memory, force_context_tokens, max_context_chars)
            {
                memory.context_stage = "slim".into();
                memory.context_tier = "force".into();
                memory.context_tokens = 0;
                memory.full_messages.clear();
                memory.rewrite_version += 1;
            }
        } else {
            // slim epoch 只保留用户原文、冻结摘要和结论；不能把已嵌入 slim memory 的
            // prompt 再保存为 fullMessages，否则下一轮会重复重放压缩历史。
            memory.context_tokens = measured;
            memory.full_messages.clear();
        }
    } else {
        // 取消或失败：中断轨迹作为 pendingMessages 保留，等待下一条提示恢复。
        memory.pending_messages = agent.messages.clone();
    }

    // 取消/失败先 flush 最新活动轨迹，保证恢复语义；正常完成可丢弃尚未写出的
    // 冗余 checkpoint，正式 slim memory 保存后再清理 sidecar。
    agent.checkpoint = None;
    let checkpoint_writer = checkpoint_writer.lock().unwrap().take();
    if let Some(writer) = checkpoint_writer {
        writer.close(outcome.cancelled || failed.is_some()).await;
    }
    if memory.system_prompt_snapshot.is_empty() && failed.is_none() {
        memory.system_prompt_snapshot = agent.system_prompt.clone();
    }
    memory.normalize();
    let memory_saved = match memory.save(&sessions_root, &ctx.session_id) {
        Ok(()) => true,
        Err(error) => {
            eprintln!("lyra: 保存会话失败：{error}");
            false
        }
    };
    // 只有正式会话已可靠落盘才删除 checkpoint；保存失败时保留 sidecar，避免取消轨迹丢失。
    if memory_saved {
        reasonix::clear_pending_checkpoint(&sessions_root, &ctx.session_id);
    }
    let input_tokens = total_usage
        .get("input")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let cache_read_tokens = total_usage
        .get("cacheRead")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let cache_write_tokens = total_usage
        .get("cacheWrite")
        .and_then(Value::as_u64)
        .unwrap_or(0);
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
        "rewriteVersion": memory.rewrite_version,
    }));

    if let Some(error) = failed {
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
    let mut config_value = config_value;
    if let Some(map) = config_value.as_object_mut() {
        map.remove("root");
        map.remove("env");
    }
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
            handle_prompt(&http, &request, &emit, fast_context, &roots, line_rx).await
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
            handle_prompt(http, request, &emit, fast_context, &roots, line_rx).await
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
    let http = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .pool_max_idle_per_host(4)
        .tcp_keepalive(std::time::Duration::from_secs(20))
        .build()
        .unwrap_or_default();
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
