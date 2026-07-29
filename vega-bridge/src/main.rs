//! vega-bridge —— Vega (pi core) 编码 agent 桥，Rust 版。
//!
//! NDJSON 协议与 Node 版 `alkaid-bridge.mjs` 对齐：
//!   请求（stdin 首行）：{ action: "prompt"|"models"|"title"|"fork", ... }
//!   后续行（仅 prompt）：{ action: "cancel"|"steer"|"permission", ... }
//!   事件（stdout）：ready / timing / item / done / error / { ok: true, data }
mod config;
mod provider;
mod skills;
mod slim_memory;
mod smart_edit;
mod system_prompt;
mod tools;

use config::{load_alkaid_config, resolve_alkaid_model, LoadedConfig, ModelInfo};
use provider::{run_agent_loop, tool_schema, AgentLoopOutput, ProviderEvent, ProviderEventKind, ToolExecutor};
use serde_json::{json, Value};
use skills::Skill;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tokio::sync::Mutex;
use tools::ToolResult;

fn send(value: &Value) {
    let mut stdout = std::io::stdout();
    let _ = writeln!(stdout, "{}", value);
    let _ = stdout.flush();
}

fn send_error(message: &str) {
    send(&json!({ "ok": false, "error": message }));
}

fn data_root() -> PathBuf {
    config::alkaid_data_root()
}

fn session_path(session_id: &str) -> Option<PathBuf> {
    if !session_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') || session_id.is_empty() {
        return None;
    }
    Some(data_root().join("sessions").join(format!("{session_id}.json")))
}

fn slim_path(session_id: &str) -> Option<PathBuf> {
    session_path(session_id).map(|p| p.with_extension("slim.json"))
}

async fn load_messages(session_id: &str) -> Vec<Value> {
    let Some(path) = session_path(session_id) else { return Vec::new(); };
    match tokio::fs::read_to_string(&path).await {
        Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

async fn save_json_atomic(path: &Path, value: &Value) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    tokio::fs::write(&tmp, serde_json::to_vec(value)?).await?;
    tokio::fs::rename(&tmp, path).await
}

async fn save_messages(session_id: &str, messages: &[Value]) {
    if let Some(path) = session_path(session_id) {
        let _ = save_json_atomic(&path, &Value::Array(messages.to_vec())).await;
    }
}

async fn load_slim_memory(session_id: &str) -> Value {
    let Some(path) = slim_path(session_id) else { return slim_memory::create_slim_memory(); };
    match tokio::fs::read_to_string(&path).await {
        Ok(text) => {
            let mut parsed: Value = serde_json::from_str(&text).unwrap_or_else(|_| slim_memory::create_slim_memory());
            // Normalize fields defensively.
            if parsed.get("turns").and_then(Value::as_array).is_none() {
                parsed = slim_memory::create_slim_memory();
            }
            parsed
        }
        Err(_) => slim_memory::create_slim_memory(),
    }
}

async fn save_slim_memory(session_id: &str, memory: &Value) {
    if let Some(path) = slim_path(session_id) {
        let mut with_version = memory.clone();
        if let Some(obj) = with_version.as_object_mut() {
            obj.entry("version").or_insert(Value::from(3u64));
        }
        let _ = save_json_atomic(&path, &with_version).await;
    }
}

fn build_tool_schemas(read_only: bool) -> Vec<Value> {
    let read_files_params = json!({
        "type": "object",
        "properties": {
            "paths": { "type": "array", "minItems": 1, "items": {
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "offset": { "type": "integer", "minimum": 1 },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 2000 }
                }
            }}
        },
        "required": ["paths"]
    });
    let mut schemas = vec![
        tool_schema("read_files", "并行读取多个 UTF-8 文本文件（可带 offset/limit）。", read_files_params),
        tool_schema("read", "读取单个文件。", json!({
            "type": "object",
            "properties": { "path": { "type": "string" }, "offset": { "type": "integer", "minimum": 1 }, "limit": { "type": "integer", "minimum": 1 } },
            "required": ["path"]
        })),
        tool_schema("grep", "正则搜索文件内容。", json!({
            "type": "object",
            "properties": { "pattern": { "type": "string" }, "path": { "type": "string" }, "maxResults": { "type": "integer" } },
            "required": ["pattern"]
        })),
        tool_schema("ls", "列出目录。", json!({ "type": "object", "properties": { "path": { "type": "string" } } })),
        tool_schema("glob", "按 glob 模式查找文件。", json!({
            "type": "object",
            "properties": { "pattern": { "type": "string" }, "path": { "type": "string" } },
            "required": ["pattern"]
        })),
    ];
    if !read_only {
        let edit_files_params = json!({
            "type": "object",
            "properties": {
                "files": { "type": "array", "minItems": 1, "items": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "edits": { "type": "array", "minItems": 1, "items": {
                            "type": "object",
                            "properties": { "oldText": { "type": "string" }, "newText": { "type": "string" } },
                            "required": ["oldText", "newText"]
                        }}
                    },
                    "required": ["path", "edits"]
                }}
            },
            "required": ["files"]
        });
        schemas.push(tool_schema("edit_files", "并行智能编辑多个互不依赖的已有文件。", edit_files_params));
        schemas.push(tool_schema("edit", "编辑单个已有文件。", json!({
            "type": "object",
            "properties": { "path": { "type": "string" }, "oldText": { "type": "string" }, "newText": { "type": "string" } },
            "required": ["path", "oldText", "newText"]
        })));
        schemas.push(tool_schema("write", "写入单个文件。", json!({
            "type": "object",
            "properties": { "path": { "type": "string" }, "content": { "type": "string" } },
            "required": ["path", "content"]
        })));
        schemas.push(tool_schema("bash", "执行 shell 命令。", json!({
            "type": "object",
            "properties": { "command": { "type": "string" }, "timeout": { "type": "integer" } },
            "required": ["command"]
        })));
    }
    schemas
}

fn make_tool_executor(root: Arc<PathBuf>, shell: Arc<system_prompt::ShellConfig>) -> ToolExecutor {
    Arc::new(move |name: &str, args: &Value| {
        let root = root.clone();
        let shell = shell.clone();
        let name = name.to_string();
        let args = args.clone();
        Box::pin(async move {
            match name.as_str() {
                "read_files" => tools::tool_read_files(&args, &root).await,
                "read" => tools::tool_read(&args, &root).await,
                "grep" => tools::tool_grep(&args, &root).await,
                "ls" => tools::tool_ls(&args, &root).await,
                "glob" => tools::tool_glob(&args, &root).await,
                "bash" => tools::tool_bash(&args, &shell).await,
                "edit_files" => match tools::tool_edit_files(&args, &root).await {
                    Ok(r) => r,
                    Err(e) => ToolResult::error(e),
                },
                "edit" => match tools::tool_edit(&args, &root).await {
                    Ok(r) => r,
                    Err(e) => ToolResult::error(e),
                },
                "write" => match tools::tool_write(&args, &root).await {
                    Ok(r) => r,
                    Err(e) => ToolResult::error(e),
                },
                other if other.starts_with("mcp__") => ToolResult::error(format!("MCP 工具 {other} 尚未接入")),
                other => ToolResult::error(format!("未知工具 {other}")),
            }
        })
    })
}

fn started_tool_item(tool_call_id: &str, name: &str, args: &Value) -> Value {
    let file_change = matches!(name, "edit" | "write" | "edit_files");
    let mut item = json!({
        "id": tool_call_id,
        "type": "mcp_tool_call",
        "status": "in_progress",
        "arguments": args,
        "server": "Vega",
        "tool": name,
    });
    if name == "bash" {
        item["type"] = Value::String("command_execution".into());
        item["command"] = Value::String(args.get("command").and_then(Value::as_str).unwrap_or("").to_string());
    } else if file_change {
        item["type"] = Value::String("file_change".into());
        let files = if name == "edit_files" {
            args.get("files").and_then(Value::as_array).cloned().unwrap_or_default()
        } else {
            vec![args.clone()]
        };
        let changes: Vec<Value> = files.iter().filter_map(|f| f.get("path").and_then(Value::as_str).map(|p| json!({ "path": p, "kind": "update" }))).collect();
        item["changes"] = Value::Array(changes);
    } else if let Some(stripped) = name.strip_prefix("mcp__") {
        let parts: Vec<&str> = stripped.splitn(2, "__").collect();
        if parts.len() == 2 {
            item["server"] = Value::String(parts[0].into());
            item["tool"] = Value::String(parts[1].into());
        }
    }
    item
}

fn prompt_parts_to_input(parts: &Value) -> (String, Vec<Value>) {
    let mut text_parts: Vec<String> = Vec::new();
    let mut images: Vec<Value> = Vec::new();
    if let Some(arr) = parts.as_array() {
        for part in arr {
            match part.get("type").and_then(Value::as_str) {
                Some("text") => {
                    if let Some(t) = part.get("text").and_then(Value::as_str) {
                        text_parts.push(t.to_string());
                    }
                }
                Some("image_data") => {
                    if let (Some(data), Some(mime)) = (part.get("data").and_then(Value::as_str), part.get("mime").and_then(Value::as_str)) {
                        images.push(json!({ "type": "input_image", "image_url": format!("data:{mime};base64,{data}") }));
                    }
                }
                _ => {}
            }
        }
    }
    (text_parts.join("\n\n"), images)
}

async fn handle_models(request: &Value) -> Value {
    let server_config = request.get("alkaidServerConfig").cloned();
    let config = match load_alkaid_config(None, server_config.as_ref()) {
        Ok(c) => c,
        Err(e) => { send_error(&e); return Value::Null; }
    };
    config::model_options_json(&config.value)
}

async fn handle_title(request: &Value) {
    let server_config = request.get("alkaidServerConfig").cloned();
    let config = match load_alkaid_config(None, server_config.as_ref()) { Ok(c) => c, Err(e) => { send_error(&e); return; } };
    let model_sel = request.get("model").and_then(Value::as_str);
    let resolved = match resolve_alkaid_model(&config.value, model_sel) { Ok(r) => r, Err(e) => { send_error(&e); return; } };
    let cwd = request.get("cwd").and_then(Value::as_str).map(PathBuf::from).unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    let prompt = request.get("prompt").and_then(Value::as_str).unwrap_or("");
    let client = reqwest::Client::new();
    let messages = vec![provider::user_message(prompt, &[])];
    let out = provider::stream_responses(&client, &resolved.model, resolved.api_key.as_deref(), "", &messages, &[], None, resolved.thinking_level.as_deref()).await;
    if let Some(e) = out.error {
        send_error(&e);
        return;
    }
    send(&json!({ "ok": true, "data": out.text }));
}

async fn handle_prompt(request: Value) {
    let server_config = request.get("alkaidServerConfig").cloned();
    let config = match load_alkaid_config(None, server_config.as_ref()) { Ok(c) => c, Err(e) => { send_error(&e); return; } };
    let model_sel = request.get("model").and_then(Value::as_str);
    let resolved = match resolve_alkaid_model(&config.value, model_sel) { Ok(r) => r, Err(e) => { send_error(&e); return; } };
    let cwd = request.get("cwd").and_then(Value::as_str).map(PathBuf::from).unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    let read_only = request.get("mode").and_then(Value::as_str) == Some("plan");
    let session_id = request.get("sessionId").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let (input_text, images) = prompt_parts_to_input(request.get("parts").unwrap_or(&Value::Null));

    let skills = skills::load_skills_from_dir(&skills::skills_root());
    let shell = if read_only { None } else { Some(system_prompt::resolve_shell_config(system_prompt::detect_shell_config())) };
    let agent_instructions = std::fs::read_to_string(data_root().join("AGENTS.md")).unwrap_or_default();
    let custom = [agent_instructions.trim().to_string(), request.get("systemPrompt").and_then(Value::as_str).unwrap_or("").trim().to_string()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join("\n\n");
    let system_prompt = system_prompt::build_system_prompt(&cwd, &skills, read_only, shell.as_ref(), &custom);

    // Slim memory / context (reasonix-style, simplified).
    let mut memory = load_slim_memory(&session_id).await;
    if memory.get("digests").and_then(Value::as_array).is_some_and(|a| a.is_empty())
        && memory.get("turns").and_then(Value::as_array).is_some_and(|a| a.is_empty())
        && request.get("sessionId").and_then(Value::as_str).is_some()
    {
        let prior = load_messages(&session_id).await;
        slim_memory::seed_slim_memory_from_messages(&mut memory, &Value::Array(prior));
    }
    slim_memory::append_slim_turn(&mut memory, &input_text);

    let context_window = resolved.model.context_window.max(2_000);
    let max_context_tokens = (context_window as f64 * 0.75).floor() as u64;
    let use_full = slim_memory::should_use_full_context(&memory, (context_window as f64 * 0.9) as u64, u64::MAX);
    let native_messages: Vec<Value> = if use_full {
        memory.get("fullMessages").and_then(Value::as_array).cloned().unwrap_or_default()
    } else {
        Vec::new()
    };
    let expanded_text = skills::expand_skill_command(&input_text, &skills).await;
    let prompt_text = if use_full {
        expanded_text.clone()
    } else {
        let context = slim_memory::format_slim_memory(&slim_memory::memory_without_current(&memory, false));
        format!("{context}\n\nUser:\n{expanded_text}")
    };

    // Persist pending turn.
    let mut pending = native_messages.clone();
    pending.push(provider::user_message(&prompt_text, &images));
    memory["pendingMessages"] = Value::Array(pending.clone());
    save_slim_memory(&session_id, &memory).await;

    send(&json!({ "type": "ready", "sessionId": session_id }));

    let root = Arc::new(cwd.clone());
    let shell_arc = Arc::new(shell.unwrap_or(system_prompt::ShellConfig { shell: "bash".into(), kind: "bash".into() }));
    let tool_executor = make_tool_executor(root, shell_arc);
    let schemas = build_tool_schemas(read_only);
    let messages = Arc::new(Mutex::new(native_messages.clone()));
    let cancel = Arc::new(AtomicBool::new(false));

    // Spawn a reader for cancel/steer lines.
    let cancel_clone = cancel.clone();
    let messages_clone = messages.clone();
    let skills_clone: Vec<Skill> = skills.clone();
    let _reader = tokio::spawn(async move {
        let mut stdin = tokio::io::BufReader::new(tokio::io::stdin());
        use tokio::io::AsyncBufReadExt;
        loop {
            let mut line = String::new();
            match stdin.read_line(&mut line).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let line = line.trim();
                    if line.is_empty() { continue; }
                    let Ok(cmd) = serde_json::from_str::<Value>(line) else { continue };
                    match cmd.get("action").and_then(Value::as_str) {
                        Some("cancel") => { cancel_clone.store(true, std::sync::atomic::Ordering::Relaxed); break; }
                        Some("steer") => {
                            let (text, images) = prompt_parts_to_input(cmd.get("parts").unwrap_or(&Value::Null));
                            let expanded = skills::expand_skill_command(&text, &skills_clone).await;
                            let msg = provider::user_message(&expanded, &images);
                            let mut m = messages_clone.lock().await;
                            m.push(msg);
                        }
                        _ => {}
                    }
                }
            }
        }
    });

    let assistant_id = format!("assistant-{}", uuid::Uuid::new_v4());
    let thinking_id = format!("thinking-{}", uuid::Uuid::new_v4());
    let text_for_event = Arc::new(Mutex::new(String::new()));
    let thinking_for_event = Arc::new(Mutex::new(String::new()));
    let tool_items: Arc<Mutex<std::collections::HashMap<String, Value>>> = Arc::new(Mutex::new(std::collections::HashMap::new()));

    let on_event = {
        let text_state = text_for_event.clone();
        let thinking_state = thinking_for_event.clone();
        let tool_items = tool_items.clone();
        move |event: ProviderEvent| {
            match event.kind {
                ProviderEventKind::TextDelta(delta) => {
                    let mut t = text_state.blocking_lock();
                    t.push_str(&delta);
                    send(&json!({ "type": "item", "item": { "id": assistant_id, "type": "agent_message", "text": &*t } }));
                }
                ProviderEventKind::ThinkingDelta(delta) => {
                    let mut t = thinking_state.blocking_lock();
                    t.push_str(&delta);
                    send(&json!({ "type": "item", "item": { "id": thinking_id, "type": "reasoning", "text": &*t } }));
                }
                ProviderEventKind::ToolCallStart { id, name } => {
                    // args unknown here; will be enriched on end via transcript.
                    let item = started_tool_item(&id, &name, &Value::Null);
                    tool_items.blocking_lock().insert(id.clone(), item.clone());
                    send(&json!({ "type": "item", "item": item }));
                }
                ProviderEventKind::ToolCallEnd { id } => {
                    if let Some(mut item) = tool_items.blocking_lock().get_mut(&id) {
                        item["status"] = Value::String("completed".into());
                    }
                }
                ProviderEventKind::MessageEnd { .. } | ProviderEventKind::ToolCallArgumentsDelta { .. } | ProviderEventKind::Error(_) => {}
            }
        }
    };

    let client = reqwest::Client::new();
    let out: AgentLoopOutput = run_agent_loop(
        &client,
        &resolved.model,
        resolved.api_key.as_deref(),
        &system_prompt,
        messages.clone(),
        schemas,
        Some(&session_id),
        resolved.thinking_level.as_deref(),
        tool_executor,
        on_event,
        cancel.clone(),
    ).await;

    // Emit final tool items with outputs from the transcript.
    {
        let items = tool_items.lock().await;
        let m = messages.lock().await;
        for msg in m.iter() {
            if msg.get("role").and_then(Value::as_str) == Some("assistant") {
                if let Some(content) = msg.get("content").and_then(Value::as_array) {
                    for block in content {
                        if block.get("type").and_then(Value::as_str) == Some("function_call") {
                            let id = block.get("call_id").or_else(|| block.get("id")).and_then(Value::as_str).unwrap_or("");
                            if let Some(mut item) = items.get(id).cloned() {
                                item["status"] = Value::String("completed".into());
                                item["arguments"] = block.get("arguments").cloned().unwrap_or(Value::Null);
                                send(&json!({ "type": "item", "item": item }));
                            }
                        }
                    }
                }
            }
        }
    }

    if let Some(e) = &out.error {
        // Persist failed turn.
        memory["pendingMessages"] = Value::Array(out.messages.clone());
        save_slim_memory(&session_id, &memory).await;
        send_error(e);
        return;
    }

    // Update slim memory conclusion.
    if !out.cancelled {
        slim_memory::set_latest_conclusion(&mut memory, &json!(out.text));
        memory["pendingMessages"] = Value::Array(vec![]);
        memory["fullMessages"] = Value::Array(out.messages.clone());
        memory["contextTokens"] = Value::from(slim_memory::context_tokens_from_messages(&Value::Array(out.messages.clone())));
    } else {
        memory["pendingMessages"] = Value::Array(out.messages.clone());
    }
    save_slim_memory(&session_id, &memory).await;
    save_messages(&session_id, &out.messages).await;

    let usage = out.usage.map(|u| normalize_usage(&u));
    send(&json!({ "type": "done", "cancelled": out.cancelled, "usage": usage }));
}

fn normalize_usage(usage: &Value) -> Value {
    let input = usage.get("input").and_then(Value::as_u64).or_else(|| usage.get("input_tokens").and_then(Value::as_u64)).unwrap_or(0);
    let output = usage.get("output").and_then(Value::as_u64).or_else(|| usage.get("output_tokens").and_then(Value::as_u64)).unwrap_or(0);
    let cache_read = usage.get("cacheRead").and_then(Value::as_u64).or_else(|| usage.get("cached_input_tokens").and_then(Value::as_u64));
    let cache_write = usage.get("cacheWrite").and_then(Value::as_u64);
    let mut out = json!({ "input": input, "output": output, "totalTokens": input + output });
    if let Some(c) = cache_read { out["cacheRead"] = Value::from(c); }
    if let Some(c) = cache_write { out["cacheWrite"] = Value::from(c); }
    out
}

#[tokio::main]
async fn main() {
    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();
    let first = match lines.next() {
        Some(Ok(l)) => l,
        _ => { send_error("Vega bridge 缺少请求"); return; }
    };
    let request: Value = match serde_json::from_str(&first) {
        Ok(v) => v,
        Err(e) => { send_error(&format!("解析请求失败：{e}")); return; }
    };
    match request.get("action").and_then(Value::as_str) {
        Some("models") => {
            let data = handle_models(&request).await;
            if !data.is_null() {
                send(&json!({ "ok": true, "data": data }));
            }
        }
        Some("title") => handle_title(&request).await,
        Some("prompt") => handle_prompt(request).await,
        Some("fork") => send_error("Vega fork 暂未实现"),
        other => send_error(&format!("Vega bridge 不支持 action: {:?}", other)),
    }
}

#[cfg(test)]
mod parity_tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    fn workspace_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    }

    fn fixtures_dir() -> PathBuf {
        workspace_root().join("tests").join("parity").join("fixtures")
    }

    fn parity_helper() -> PathBuf {
        workspace_root().join("tests").join("parity").join("parity-node.mjs")
    }

    fn repo_root() -> PathBuf {
        workspace_root().parent().unwrap_or(&workspace_root()).to_path_buf()
    }

    /// Run the Node parity helper with the given arguments and return its stdout JSON.
    fn run_node(action: &str, args: &[&str]) -> serde_json::Value {
        let helper = parity_helper();
        let mut cmd = Command::new("node");
        cmd.arg(&helper).arg(action);
        for a in args {
            cmd.arg(a);
        }
        // The helper resolves scripts/ relative to its own location, so cwd doesn't
        // matter for module resolution. But env vars are needed for config {env:} refs.
        cmd.env("ZHIPU_API_KEY", "test");
        cmd.env("OPENAI_API_KEY", "test");
        let output = cmd.output().expect("failed to run node parity helper");
        if !output.status.success() {
            panic!("node parity helper {} failed: {}", action, String::from_utf8_lossy(&output.stderr));
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let line = stdout.lines().next().unwrap_or("");
        serde_json::from_str(line).unwrap_or_else(|e| panic!("invalid JSON from node helper: {e}\nraw: {line}"))
    }

    /// Run the Rust vega-bridge binary with the given stdin JSON and return stdout.
    fn run_rust_bridge(stdin_json: &str) -> String {
        let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("vega-bridge"));
        // current_exe during tests points to the test binary, not vega-bridge.
        // Build the path to the debug/release vega-bridge binary.
        let target_dir = workspace_root().join("target").join("debug").join("vega-bridge.exe");
        let target_dir_alt = workspace_root().join("target").join("release").join("vega-bridge.exe");
        let bin = if target_dir.is_file() {
            target_dir
        } else if target_dir_alt.is_file() {
            target_dir_alt
        } else {
            // Fallback: use cargo to run.
            let mut cmd = Command::new("cargo");
            cmd.arg("run").arg("--quiet").arg("--bin").arg("vega-bridge");
            cmd.stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
            let mut child = cmd.spawn().expect("failed to cargo run vega-bridge");
            {
                use std::io::Write;
                let stdin = child.stdin.as_mut().unwrap();
                stdin.write_all(stdin_json.as_bytes()).unwrap();
            }
            let output = child.wait_with_output().unwrap();
            return String::from_utf8_lossy(&output.stdout).to_string();
        };
        let mut cmd = Command::new(&bin);
        cmd.stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
        let mut child = cmd.spawn().expect("failed to spawn vega-bridge");
        {
            use std::io::Write;
            let stdin = child.stdin.as_mut().unwrap();
            stdin.write_all(stdin_json.as_bytes()).unwrap();
        }
        let output = child.wait_with_output().unwrap();
        String::from_utf8_lossy(&output.stdout).to_string()
    }

    /// Normalize a JSON value for comparison (sort object keys, strip volatile fields).
    fn normalize_for_compare(value: &serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::Object(map) => {
                let mut sorted = serde_json::Map::new();
                for (k, v) in map.iter() {
                    sorted.insert(k.clone(), normalize_for_compare(v));
                }
                serde_json::Value::Object(sorted)
            }
            serde_json::Value::Array(arr) => {
                serde_json::Value::Array(arr.iter().map(normalize_for_compare).collect())
            }
            other => other.clone(),
        }
    }

    fn assert_json_eq(node: &serde_json::Value, rust: &serde_json::Value, context: &str) {
        let n = normalize_for_compare(node);
        let r = normalize_for_compare(rust);
        assert_eq!(n, r, "parity mismatch ({context})\nNode: {n}\nRust: {r}");
    }

    // ── smart_edit parity ──────────────────────────────────────────────────

    #[test]
    fn parity_smart_edit() {
        let cases_path = fixtures_dir().join("smart-edit-cases.json");
        let node_results = run_node("smart-edit", &[cases_path.to_str().unwrap()]);
        let cases: Vec<serde_json::Value> = serde_json::from_str(
            &std::fs::read_to_string(&cases_path).unwrap()
        ).unwrap();

        let node_arr = node_results.as_array().expect("node smart-edit returned array");
        assert_eq!(node_arr.len(), cases.len(), "same number of cases");

        for (i, case) in cases.iter().enumerate() {
            let id = case["id"].as_str().unwrap_or("");
            let node_result = &node_arr[i];
            let content = case["content"].as_str().unwrap();
            let path = case["path"].as_str().unwrap();
            let edits: Vec<(String, String)> = case["edits"]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| (
                    e["oldText"].as_str().unwrap().to_string(),
                    e["newText"].as_str().unwrap().to_string(),
                ))
                .collect();

            match smart_edit::apply_smart_edits(content, &edits, path) {
                Ok(rust_result) => {
                    assert!(node_result["ok"].as_bool() == Some(true), "Node rejected but Rust accepted: {id}");
                    // Compare content.
                    assert_eq!(rust_result.content, node_result["content"].as_str().unwrap(),
                        "content mismatch for {id}");
                    // Compare matches (mode + line, ignoring editIndex order which both sort by index).
                    let node_matches = node_result["matches"].as_array().unwrap();
                    assert_eq!(rust_result.matches.len(), node_matches.len(),
                        "match count mismatch for {id}");
                    for (j, rm) in rust_result.matches.iter().enumerate() {
                        let nm = &node_matches[j];
                        assert_eq!(rm["mode"], nm["mode"], "mode mismatch for {id} match {j}");
                        assert_eq!(rm["line"], nm["line"], "line mismatch for {id} match {j}");
                    }
                }
                Err(rust_err) => {
                    assert!(node_result["ok"].as_bool() == Some(false),
                        "Rust rejected but Node accepted: {id}");
                    // Both should reject; compare error messages loosely.
                    let node_err = node_result["error"].as_str().unwrap_or("");
                    assert!(!node_err.is_empty(), "Node error message non-empty for {id}");
                    assert!(!rust_err.is_empty(), "Rust error message non-empty for {id}");
                }
            }
        }
    }

    // ── strip_skill_frontmatter parity ─────────────────────────────────────

    #[test]
    fn parity_strip_skill_frontmatter() {
        let skill_md = fixtures_dir().join("skills").join("my-skill").join("SKILL.md");
        let node_result = run_node("strip-frontmatter", &[skill_md.to_str().unwrap()]);
        let content = std::fs::read_to_string(&skill_md).unwrap();
        let rust_result = skills::strip_skill_frontmatter(&content);
        // Node returns the body after the closing "---" (slice(end+2) with relative end).
        // Rust does the same (lines[end+1..] with absolute end = same line).
        let node_str = node_result.as_str().unwrap_or("");
        assert_eq!(rust_result, node_str, "strip_skill_frontmatter parity mismatch");
    }

    // ── expand_skill_command parity ────────────────────────────────────────

    #[test]
    fn parity_expand_skill_command() {
        let skills_dir = fixtures_dir().join("skills");
        // Load skills with Rust.
        let rust_skills = skills::load_skills_from_dir(&skills_dir);
        assert_eq!(rust_skills.len(), 1, "Rust should find 1 skill");
        // Node expand-skill returns the expanded block. Compare the body content
        // (location paths differ by OS/absolute path, so compare structurally).
        let node_result = run_node("expand-skill", &[skills_dir.to_str().unwrap(), "my-skill"]);
        let node_str = node_result.as_str().unwrap_or("");
        // Run Rust expand.
        let rt = tokio::runtime::Runtime::new().unwrap();
        let rust_str = rt.block_on(skills::expand_skill_command("/skill:my-skill", &rust_skills));

        // Both should contain the skill body text.
        assert!(node_str.contains("This is the skill body."), "Node expand missing body: {node_str}");
        assert!(rust_str.contains("This is the skill body."), "Rust expand missing body: {rust_str}");
        // Both should have the <skill> wrapper.
        assert!(node_str.contains("<skill name=\"my-skill\""), "Node expand missing skill tag");
        assert!(rust_str.contains("<skill name=\"my-skill\""), "Rust expand missing skill tag");
        assert!(node_str.contains("</skill>"), "Node expand missing closing tag");
        assert!(rust_str.contains("</skill>"), "Rust expand missing closing tag");
        // Both should reference the skill directory.
        assert!(node_str.contains("References are relative to"), "Node missing reference line");
        assert!(rust_str.contains("References are relative to"), "Rust missing reference line");
    }

    // ── config models parity ───────────────────────────────────────────────

    #[test]
    fn parity_config_models() {
        let config_path = fixtures_dir().join("config.jsonc");
        let node_result = run_node("models", &[config_path.to_str().unwrap()]);

        // Run Rust config loading + model options.
        let root = fixtures_dir();
        let config = config::load_alkaid_config(Some(root.clone()), None).unwrap();
        let rust_result = config::model_options_json(&config.value);

        // Compare the structure: currentValue, options (value + name + supportsImages).
        assert_eq!(
            node_result["configOptions"][0]["currentValue"],
            rust_result["configOptions"][0]["currentValue"],
            "currentValue mismatch"
        );

        let node_options = node_result["configOptions"][0]["options"].as_array().unwrap();
        let rust_options = rust_result["configOptions"][0]["options"].as_array().unwrap();
        assert_eq!(node_options.len(), rust_options.len(), "option count mismatch");

        for (i, (no, ro)) in node_options.iter().zip(rust_options.iter()).enumerate() {
            assert_eq!(no["value"], ro["value"], "option {i} value mismatch");
            assert_eq!(no["name"], ro["name"], "option {i} name mismatch");
            // supportsImages meta — Node uses _meta, Rust uses _meta.
            let node_img = no["_meta"]["codex.ai/supportsImages"].as_bool();
            let rust_img = ro["_meta"]["codex.ai/supportsImages"].as_bool();
            assert_eq!(node_img, rust_img, "option {i} supportsImages mismatch");
        }
    }

    // ── SSE parsing parity ─────────────────────────────────────────────────

    #[test]
    fn parity_sse_parse() {
        let sse_path = fixtures_dir().join("sse-recording.txt");
        let node_result = run_node("sse-parse", &[sse_path.to_str().unwrap()]);

        // Run Rust SSE parser on the same recording.
        let bytes = std::fs::read(&sse_path).unwrap();
        let mut state = provider::SseState::new();
        provider::feed_sse_chunk(&bytes, &mut state);
        // Flush trailing line without newline.
        if !state.buf.is_empty() {
            let remaining = std::mem::take(&mut state.buf);
            provider::process_sse_line(&remaining, &mut state);
        }
        for tc in &mut state.tool_calls {
            if tc.arguments.is_empty() {
                if let Some(args) = state.tool_args.get(&tc.id) {
                    tc.arguments = args.clone();
                }
            }
        }

        // Compare text.
        assert_eq!(state.text, node_result["text"].as_str().unwrap(), "text parity mismatch");
        // Compare thinking.
        assert_eq!(state.thinking, node_result["thinking"].as_str().unwrap(), "thinking parity mismatch");
        // Compare stop reason.
        assert_eq!(
            state.stop_reason.as_deref(),
            node_result["stopReason"].as_str(),
            "stopReason parity mismatch"
        );
        // Compare error.
        assert_eq!(
            state.error.as_deref(),
            node_result.get("error").and_then(|v| v.as_str()),
            "error parity mismatch"
        );
        // Compare usage.
        assert_eq!(
            state.usage.as_ref(),
            node_result.get("usage"),
            "usage parity mismatch"
        );
        // Compare tool calls.
        let node_calls = node_result["toolCalls"].as_array().unwrap();
        assert_eq!(state.tool_calls.len(), node_calls.len(), "tool call count mismatch");
        for (i, (rc, nc)) in state.tool_calls.iter().zip(node_calls.iter()).enumerate() {
            assert_eq!(rc.id, nc["id"].as_str().unwrap(), "tool call {i} id mismatch");
            assert_eq!(rc.name, nc["name"].as_str().unwrap(), "tool call {i} name mismatch");
            assert_eq!(rc.arguments, nc["arguments"].as_str().unwrap(), "tool call {i} arguments mismatch");
        }
    }
}
