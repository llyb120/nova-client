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

use pi_core::agent::{Agent, AgentState, PrepareNextTurnFn, StreamFn, StreamTurn, ToolFn};
use pi_core::alkaid_config::{merge_config, parse_jsonc, resolve_model};
use pi_core::bridge::ProtocolAccumulator;
use pi_core::prompt::{build_system_prompt, ShellConfig, ShellKind};
use pi_core::skills::{format_alkaid_skills_prompt, Skill};
use pi_core::skills_discovery::load_skills_from_dir;
use pi_core::tools::NativeTools;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::vega_provider::{stream_turn, ProviderConfig};

/// Configuration for a native Vega turn.
pub struct NativeTurnConfig {
    pub system_prompt: String,
    pub model: Value,
    pub tools: Vec<Value>,
    pub history: Vec<Value>,
    /// Prompt images in pi format (`{type:"image", data, mimeType}`).
    pub images: Vec<Value>,
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
/// A live native turn's control surface, registered in `SdkRuntime` so
/// `cancel`/`steer_prompt` can reach an in-process turn (the node bridge gets
/// these via its stdin command channel).
pub struct NativeRunController {
    /// Cancel flag; the stream function checks it between provider calls and
    /// races it against the in-flight transport.
    pub cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Steering user messages queued by `steer_prompt`, drained into the agent
    /// loop between tool rounds.
    pub steer_queue: std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<Value>>>,
}

/// Build a `StreamTurn` carrying an `aborted`-stop assistant message so the
/// loop terminates cleanly on cancel.
fn aborted_turn(provider: &ProviderConfig) -> StreamTurn {
    let message = json!({
        "role": "assistant",
        "content": [{ "type": "text", "text": "" }],
        "api": provider.api,
        "provider": provider.provider,
        "model": provider.model_id,
        "stopReason": "aborted",
        "errorMessage": "cancelled",
    });
    StreamTurn {
        events: vec![
            json!({ "type": "start", "partial": message }),
            json!({ "type": "done" }),
        ],
        result: message,
    }
}

///
/// `stream_fn` is the injected LLM transport; `tool_fn` executes tools natively.
/// The agent's emitted events are replayed through a `ProtocolAccumulator` to
/// produce the protocol item stream the backend expects. When `emit_item` is
/// provided, each newly produced protocol item is forwarded immediately
/// (progressive streaming); otherwise items are only collected.
pub fn run_native_turn(
    config: NativeTurnConfig,
    prompt_text: &str,
    stream_fn: &mut StreamFn,
    tool_fn: &mut ToolFn,
    prepare_next_turn: Option<Box<PrepareNextTurnFn<'static>>>,
    steer_queue: Option<std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<Value>>>>,
    mut emit_item: Option<&mut dyn FnMut(Value)>,
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
    agent.prepare_next_turn = prepare_next_turn;
    // Drain queued steering messages into the agent between tool rounds.
    agent.steer_queue = steer_queue;

    let events = agent.prompt(
        &Value::String(prompt_text.to_string()),
        &config.images,
        config.timestamp,
        stream_fn,
        tool_fn,
    );

    let mut accumulator = ProtocolAccumulator::new();
    for event in &events {
        let before = accumulator.items.len();
        accumulator.on_event(event);
        if let Some(emit) = emit_item.as_mut() {
            for item in accumulator.items[before..].iter().cloned() {
                emit(item);
            }
        }
    }
    let usage = accumulator.usage.clone();
    let items = accumulator.finish();

    NativeTurnOutput {
        items,
        usage,
        messages: agent.state.messages.clone(),
    }
}

/// Port of alkaid `governToolResult`: when a tool result's combined text exceeds
/// the context byte budget, archive the full text to disk (when an archive dir
/// is configured) and replace it with a head+tail elision carrying a pointer.
pub fn govern_tool_result(
    mut result: Value,
    tool_call_id: Option<&str>,
    tool_name: &str,
    archive_dir: Option<&Path>,
) -> Value {
    use pi_core::text::{govern_text, safe_archive_segment, TOOL_OUTPUT_CONTEXT_MAX_BYTES};
    let content = match result.get("content").and_then(Value::as_array) {
        Some(content) => content.clone(),
        None => return result,
    };
    let text = content
        .iter()
        .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n");
    let governed = match archive_dir {
        Some(dir) => {
            if text.len() > TOOL_OUTPUT_CONTEXT_MAX_BYTES {
                let _ = std::fs::create_dir_all(dir);
                let file_name = format!(
                    "{}-{}.txt",
                    safe_archive_segment(tool_call_id),
                    safe_archive_segment(Some(tool_name))
                );
                let path = dir.join(file_name);
                let archive_path = if std::fs::write(&path, &text).is_ok() {
                    Some(path.to_string_lossy().to_string())
                } else {
                    None
                };
                govern_text(&text, TOOL_OUTPUT_CONTEXT_MAX_BYTES, archive_path.as_deref())
            } else {
                None
            }
        }
        None => govern_text(&text, TOOL_OUTPUT_CONTEXT_MAX_BYTES, None),
    };
    let Some(governed) = governed else {
        return result;
    };
    // Rebuild content: governed text first, then the non-text parts.
    let mut new_content: Vec<Value> = vec![json!({ "type": "text", "text": governed.text })];
    for part in &content {
        if part.get("type").and_then(Value::as_str) != Some("text") {
            new_content.push(part.clone());
        }
    }
    result["content"] = Value::Array(new_content);
    let mut details = result
        .get("details")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    details.insert(
        "archivedToolOutput".to_string(),
        governed
            .archived_path
            .map(Value::String)
            .unwrap_or(Value::Null),
    );
    details.insert("originalBytes".to_string(), json!(governed.original_bytes));
    result["details"] = Value::Object(details);
    result
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
    prepare_next_turn: Option<Box<PrepareNextTurnFn<'static>>>,
    mcp_hub: Option<std::sync::Arc<crate::mcp::McpHub>>,
    controller: Option<std::sync::Arc<NativeRunController>>,
    archive_dir: Option<PathBuf>,
) -> Result<
    (
        tokio::sync::mpsc::UnboundedReceiver<Value>,
        tokio::task::JoinHandle<Result<NativeTurnOutput, String>>,
    ),
    String,
> {
    let (item_tx, item_rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
    let handle = tokio::runtime::Handle::current();
    let join = tokio::task::spawn_blocking(move || {
        let session_id = config.session_id.clone();
        let tool_handle = handle.clone();
        let cancel = controller.as_ref().map(|c| std::sync::Arc::clone(&c.cancel));
        let steer_queue = controller.as_ref().map(|c| std::sync::Arc::clone(&c.steer_queue));
        let mut stream_fn = move |_index: usize, llm_context: &Value| -> StreamTurn {
            // Honor cancel before starting a new provider call.
            if cancel
                .as_ref()
                .map(|c| c.load(std::sync::atomic::Ordering::SeqCst))
                .unwrap_or(false)
            {
                return aborted_turn(&provider);
            }
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
            let provider_for_abort = provider.clone();
            let provider_for_error = provider.clone();
            let session_id = session_id.clone();
            let cancel = cancel.clone();
            handle
                .block_on(async move {
                    let transport = stream_turn(
                        &client,
                        &provider_for_call,
                        &system_prompt,
                        &messages,
                        &tools,
                        session_id.as_deref(),
                    );
                    match cancel {
                        // Race the transport against the cancel flag so an
                        // in-flight request aborts promptly on cancel.
                        Some(flag) => {
                            tokio::select! {
                                result = transport => result,
                                _ = wait_for_cancel(flag) => Ok(aborted_turn(&provider_for_abort)),
                            }
                        }
                        None => transport.await,
                    }
                })
                .unwrap_or_else(|error| error_turn(&error, &provider_for_error))
        };
        let mut tool_fn = move |id: &str, name: &str, args: &Value| -> (Value, bool) {
            let (result, is_error) = if name.starts_with("mcp__") {
                if let Some(hub) = mcp_hub.as_ref() {
                    let hub = std::sync::Arc::clone(hub);
                    let args = args.clone();
                    let name = name.to_string();
                    tool_handle.block_on(async move { hub.call_tool(&name, &args).await })
                } else {
                    native_tools.execute(name, args)
                }
            } else {
                native_tools.execute(name, args)
            };
            // Port of alkaid's per-tool `governToolResult`: clamp oversized
            // outputs and archive the full text to disk.
            (
                govern_tool_result(result, Some(id), name, archive_dir.as_deref()),
                is_error,
            )
        };
        let mut emit = move |item: Value| {
            let _ = item_tx.send(item);
        };
        Ok::<NativeTurnOutput, String>(run_native_turn(
            config,
            &prompt_text,
            &mut stream_fn,
            &mut tool_fn,
            prepare_next_turn,
            steer_queue,
            Some(&mut emit),
        ))
    });
    Ok((item_rx, join))
}

/// Resolve when the cancel flag is set (polling). Used to race the transport.
async fn wait_for_cancel(flag: std::sync::Arc<std::sync::atomic::AtomicBool>) {
    loop {
        if flag.load(std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// Resolve just the `ProviderConfig` for a model selection (config load +
/// model resolution), without loading skills/tools. Used for the Reasonix
/// summary turn's lightweight model.
pub fn resolve_provider_config(
    data_dir: &Path,
    server_config: Option<&Value>,
    model_selection: &str,
) -> Result<ProviderConfig, String> {
    let config = load_alkaid_config(data_dir, server_config)?;
    let env: HashMap<String, String> = std::env::vars().collect();
    let resolved = resolve_model(&config, model_selection, &env)?;
    let model = resolved.get("model").cloned().unwrap_or(json!({}));
    let api_key = resolved.get("apiKey").and_then(Value::as_str).map(String::from);
    Ok(ProviderConfig {
        api: model.get("api").and_then(Value::as_str).unwrap_or("").to_string(),
        base_url: model.get("baseUrl").and_then(Value::as_str).unwrap_or("").to_string(),
        model_id: model.get("id").and_then(Value::as_str).unwrap_or("").to_string(),
        provider: model.get("provider").and_then(Value::as_str).unwrap_or("").to_string(),
        api_key,
    })
}

/// Run a lightweight, tool-free native turn and return the assistant's text.
/// Used by the Reasonix digest compaction to summarize older turns. The summary
/// model is `summary_provider`; the prompt is sent with no tools and no history
/// so the model produces a plain text digest.
pub async fn run_summary_turn_async(
    client: reqwest::Client,
    summary_provider: ProviderConfig,
    summary_prompt: String,
) -> Result<String, String> {
    let model = json!({
        "id": summary_provider.model_id,
        "provider": summary_provider.provider,
        "api": summary_provider.api,
    });
    let config = NativeTurnConfig {
        system_prompt: String::new(),
        model,
        tools: Vec::new(),
        history: Vec::new(),
        images: Vec::new(),
        session_id: None,
        timestamp: now_millis(),
        parallel_tools: false,
    };
    let native_tools = NativeTools::new(PathBuf::from("."));
    let (_item_rx, join) = run_native_turn_async(
        client,
        summary_provider,
        config,
        summary_prompt,
        native_tools,
        None,
        None,
        None,
        None,
    )
    .await?;
    let output = join
        .await
        .map_err(|error| format!("summary turn panicked: {error}"))??;
    // Concatenate the text of the final assistant message(s).
    let text = output
        .messages
        .iter()
        .filter(|m| m.get("role").and_then(Value::as_str) == Some("assistant"))
        .filter_map(|m| m.get("content").and_then(Value::as_array))
        .flatten()
        .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("");
    Ok(text)
}

/// Load and merge the Vega config: `config.jsonc` under `{data_dir}/alkaid`,
/// with the server-provided config as the baseline (port of `loadAlkaidConfig`).
pub fn load_alkaid_config(data_dir: &Path, server_config: Option<&Value>) -> Result<Value, String> {
    let config_path = data_dir.join("alkaid").join("config.jsonc");
    let local = match std::fs::read_to_string(&config_path) {
        Ok(text) => parse_jsonc(&text)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => json!({}),
        Err(error) => {
            return Err(format!(
                "读取 Vega 配置失败：{error}"
            ))
        }
    };
    let merged = match server_config {
        Some(server) => merge_config(server, &local),
        None => local,
    };
    if merged.get("provider").and_then(Value::as_object).map_or(true, |p| p.is_empty()) {
        return Err(format!("未找到 Vega 配置：{}", config_path.display()));
    }
    Ok(merged)
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

/// The tool definitions advertised to the LLM (name/description/parameters).
/// Mirrors the alkaid tool set: batch `read_files`/`edit_files` plus the
/// pi-coding-agent `read`/`edit`/`write`/`bash`/`grep`/`find`/`ls`.
pub fn native_tool_definitions(read_only: bool) -> Vec<Value> {
    let mut tools = vec![
        json!({
            "name": "read_files",
            "description": "并行读取多个 UTF-8 文本文件（可带 offset/limit）。",
            "parameters": {
                "type": "object",
                "properties": {
                    "paths": {
                        "type": "array",
                        "minItems": 1,
                        "items": {
                            "anyOf": [
                                { "type": "string" },
                                {
                                    "type": "object",
                                    "properties": {
                                        "path": { "type": "string" },
                                        "offset": { "type": "integer", "minimum": 1 },
                                        "limit": { "type": "integer", "minimum": 1, "maximum": 2000 }
                                    },
                                    "required": ["path"]
                                }
                            ]
                        }
                    }
                },
                "required": ["paths"]
            }
        }),
        json!({
            "name": "read",
            "description": "Read the contents of a file. Use offset/limit for large files.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "offset": { "type": "number" },
                    "limit": { "type": "number" }
                },
                "required": ["path"]
            }
        }),
    ];
    if read_only {
        tools.push(json!({
            "name": "grep",
            "description": "Search file contents for a pattern (respects .gitignore).",
            "parameters": {
                "type": "object",
                "properties": {
                    "pattern": { "type": "string" },
                    "path": { "type": "string" },
                    "glob": { "type": "string" },
                    "ignoreCase": { "type": "boolean" },
                    "literal": { "type": "boolean" }
                },
                "required": ["pattern"]
            }
        }));
        tools.push(json!({
            "name": "find",
            "description": "Find files by glob pattern.",
            "parameters": {
                "type": "object",
                "properties": { "pattern": { "type": "string" }, "path": { "type": "string" } },
                "required": ["pattern"]
            }
        }));
        tools.push(json!({
            "name": "ls",
            "description": "List directory contents.",
            "parameters": {
                "type": "object",
                "properties": { "path": { "type": "string" }, "limit": { "type": "number" } }
            }
        }));
        return tools;
    }
    tools.push(json!({
        "name": "edit_files",
        "description": "并行智能编辑多个互不依赖的已有文件（精确优先、锚点定位、歧义拒绝）。",
        "parameters": {
            "type": "object",
            "properties": {
                "files": {
                    "type": "array",
                    "minItems": 1,
                    "items": {
                        "type": "object",
                        "properties": {
                            "path": { "type": "string" },
                            "edits": {
                                "type": "array",
                                "minItems": 1,
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "oldText": { "type": "string" },
                                        "newText": { "type": "string" }
                                    },
                                    "required": ["oldText", "newText"]
                                }
                            }
                        },
                        "required": ["path", "edits"]
                    }
                }
            },
            "required": ["files"]
        }
    }));
    tools.push(json!({
        "name": "edit",
        "description": "Edit a single file using exact text replacement.",
        "parameters": {
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "edits": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "oldText": { "type": "string" },
                            "newText": { "type": "string" }
                        },
                        "required": ["oldText", "newText"]
                    }
                }
            },
            "required": ["path", "edits"]
        }
    }));
    tools.push(json!({
        "name": "write",
        "description": "Write content to a file. Creates the file if it doesn't exist, overwrites if it does.",
        "parameters": {
            "type": "object",
            "properties": { "path": { "type": "string" }, "content": { "type": "string" } },
            "required": ["path", "content"]
        }
    }));
    tools.push(json!({
        "name": "bash",
        "description": "Execute a bash command in the current working directory.",
        "parameters": {
            "type": "object",
            "properties": { "command": { "type": "string" } },
            "required": ["command"]
        }
    }));
    tools.push(json!({
        "name": "grep",
        "description": "Search file contents for a pattern (respects .gitignore).",
        "parameters": {
            "type": "object",
            "properties": {
                "pattern": { "type": "string" },
                "path": { "type": "string" },
                "glob": { "type": "string" },
                "ignoreCase": { "type": "boolean" },
                "literal": { "type": "boolean" }
            },
            "required": ["pattern"]
        }
    }));
    tools.push(json!({
        "name": "find",
        "description": "Find files by glob pattern.",
        "parameters": {
            "type": "object",
            "properties": { "pattern": { "type": "string" }, "path": { "type": "string" } },
            "required": ["pattern"]
        }
    }));
    tools.push(json!({
        "name": "ls",
        "description": "List directory contents.",
        "parameters": {
            "type": "object",
            "properties": { "path": { "type": "string" }, "limit": { "type": "number" } }
        }
    }));
    tools
}

/// Everything `sdk_runtime` needs to run a native Vega turn.
pub struct NativeVegaSetup {
    pub provider: ProviderConfig,
    pub turn_config: NativeTurnConfig,
    pub native_tools: NativeTools,
    /// Discovered skills, retained so the caller can expand `/skill:<name>`.
    pub skills: Vec<Skill>,
}

/// Port of `loadAlkaidAgentInstructions`: read `AGENTS.md`, empty when absent.
fn load_agent_instructions(path: &Path) -> String {
    match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => format!("[读取 Vega AGENTS.md 失败：{error}]"),
    }
}

/// Detect the shell config (port of `detectAlkaidShellConfig` +
/// `resolveAlkaidShellConfig`): PowerShell on Windows when available, otherwise
/// Bash, with `NOVA_SHELL_SHIM_*` overrides applied.
fn detect_shell_config() -> ShellConfig {
    let is_windows = cfg!(target_os = "windows");
    if is_windows {
        let shim = std::env::var("NOVA_SHELL_SHIM_POWERSHELL").ok().filter(|s| !s.is_empty());
        if let Some(shell) = shim.or_else(find_windows_powershell) {
            return ShellConfig {
                shell,
                kind: ShellKind::Powershell,
            };
        }
    }
    let shim = std::env::var("NOVA_SHELL_SHIM_BASH").ok().filter(|s| !s.is_empty());
    let shell = shim
        .or_else(|| std::env::var("SHELL").ok().filter(|s| !s.is_empty()))
        .unwrap_or_else(|| "/bin/bash".to_string());
    ShellConfig {
        shell,
        kind: ShellKind::Bash,
    }
}

fn find_windows_powershell() -> Option<String> {
    for root in [std::env::var("SystemRoot").ok(), std::env::var("windir").ok()]
        .into_iter()
        .flatten()
    {
        let candidate = Path::new(&root)
            .join("System32")
            .join("WindowsPowerShell")
            .join("v1.0")
            .join("powershell.exe");
        if candidate.exists() {
            return Some(candidate.to_string_lossy().to_string());
        }
    }
    None
}

/// Resolve the Vega config into a runnable native setup (port of the
/// `createAlkaidAgent` configuration step, minus skill loading and MCP).
///
/// Known gaps for production parity: skills are not loaded from disk (the
/// system prompt's skills section is empty) and MCP servers are not connected.
/// These are documented remaining steps before the node bridge is retired.
pub fn prepare_native_turn(
    data_dir: &Path,
    cwd: &str,
    server_config: Option<&Value>,
    model_selection: &str,
    history: Vec<Value>,
    session_id: Option<String>,
    read_only: bool,
) -> Result<NativeVegaSetup, String> {
    let config = load_alkaid_config(data_dir, server_config)?;
    let env: HashMap<String, String> = std::env::vars().collect();
    let resolved = resolve_model(&config, model_selection, &env)?;
    let model = resolved.get("model").cloned().unwrap_or(json!({}));
    let api_key = resolved.get("apiKey").and_then(Value::as_str).map(String::from);

    let provider = ProviderConfig {
        api: model.get("api").and_then(Value::as_str).unwrap_or("").to_string(),
        base_url: model.get("baseUrl").and_then(Value::as_str).unwrap_or("").to_string(),
        model_id: model.get("id").and_then(Value::as_str).unwrap_or("").to_string(),
        provider: model.get("provider").and_then(Value::as_str).unwrap_or("").to_string(),
        api_key,
    };

    let shell_config = if read_only {
        None
    } else {
        Some(detect_shell_config())
    };
    // Skills are discovered from `{data_dir}/alkaid/skills` (port of
    // `loadAlkaidSkills`); AGENTS.md supplies custom instructions.
    let skills_root = data_dir.join("alkaid").join("skills");
    let skills = load_skills_from_dir(&skills_root);
    let skills_prompt = format_alkaid_skills_prompt(&skills);
    let agent_instructions = load_agent_instructions(&data_dir.join("alkaid").join("AGENTS.md"));
    let custom_instructions = [agent_instructions.trim().to_string()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    let system_prompt = build_system_prompt(
        cwd,
        read_only,
        shell_config.as_ref(),
        &skills_prompt,
        &custom_instructions,
    );
    let tool_definitions = native_tool_definitions(read_only);
    let native_tools = NativeTools::new(PathBuf::from(cwd));

    let turn_config = NativeTurnConfig {
        system_prompt,
        model,
        tools: tool_definitions,
        history,
        images: Vec::new(),
        session_id,
        timestamp: now_millis(),
        parallel_tools: true,
    };

    Ok(NativeVegaSetup {
        provider,
        turn_config,
        native_tools,
        skills,
    })
}
