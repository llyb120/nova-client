//! Native Codex stdio JSON-RPC client. No Node process sits between Nova and Codex.
pub(crate) mod mcp;
use base64::Engine;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::mpsc;
use tokio::time::Instant;

const RPC_TIMEOUT: Duration = Duration::from_secs(120);
const POLARIS_GUIDANCE: &str = "Nova provides the polaris tool through MCP server nova-tools. Call its exposed MCP tool directly, using the name in the tool schema; do not use Devin's mcp_call_tool wrapper. When edit locations are unknown or you need to read two or more unread files, call polaris first with the user's symbols, keywords or file paths. It defaults to a fast overview: 1–2 core hit lines plus necessary signatures/locations, without recursive dependency indexing, alias-call tracing, body-driven follow-up searches or Git history analysis. Set deep=true only when those deeper relationships are needed (slower). Only actual shown lines count as read; use exact path and line for necessary body reads, without rediscovering the same keywords with shell searches. Keep searches bounded and honor .gitignore. Use Codex's built-in file and shell tools for edits and verification.";

pub struct Options {
    pub ponytail: bool,
    /// The existing optional Polaris MCP server; not the Codex transport.
    pub polaris: Option<Value>,
}

pub(super) fn rtk_guidance() -> String {
    let Ok(exe) = std::env::current_exe() else {
        return String::new();
    };
    let path = exe.to_string_lossy();
    let path = if cfg!(windows) {
        path.trim_start_matches(r"\\?\").replace('\\', "/")
    } else {
        path.into_owned()
    };
    // A GUI-subsystem exe needs a pipeline for PowerShell to wait and capture output.
    // Dot-sourcing keeps LASTEXITCODE in the caller; -Stream avoids buffering all output.
    let powershell = format!(". {{ & '{}' @args | Out-String -Stream }} __rtk", path.replace('\'', "''"));
    let bash = format!("'{}' __rtk", path.replace('\'', "'\\''"));
    // ponytail: instruction-based for Codex and ACP agents; enforcing every command
    // requires a backend pre-execution rewrite hook when that API is available.
    format!(
        "Nova includes RTK for compact shell output; no separate rtk installation is needed. \
         Invoke it through your native shell tool using the prefix for that shell:\n\
         PowerShell: {powershell}\nBash/sh: {bash}\n\
         For supported commands whose output you are reading, use this prefix, e.g. \
         `<prefix> git status`, `<prefix> git diff`, `<prefix> git log -5`, `<prefix> cargo test`. \
         Use `<prefix> --help` to check supported commands when needed. \
         Keep unsupported commands, shell builtins, and commands whose exact/raw output is \
         needed (including output parsed by scripts or pipelines) native. Preserve arguments, \
         working directory, shell syntax, and approval/sandbox restrictions. \
         Do not blindly rerun a failed command that may have side effects."
    )
}

// Defense in depth after removing native executors. Codex hooks fail open on
// execution errors; the completion notification guard also aborts on such errors.
const DENY_TOOLS: &str = "echo Nova complete_text forbids all tools >&2; exit 2";
// Fixed UTF-16LE script emits native permissionDecision=deny JSON with exit 0.
// An outer pwsh -Command translates an external exit 2 into 1 (fail-open), so
// Windows must use structured denial instead. No caller/model text is interpolated.
const DENY_TOOLS_WINDOWS: &str = "powershell.exe -NoLogo -NoProfile -NonInteractive -EncodedCommand WwBDAG8AbgBzAG8AbABlAF0AOgA6AE8AdQB0AC4AVwByAGkAdABlACgAJwB7ACIAaABvAG8AawBTAHAAZQBjAGkAZgBpAGMATwB1AHQAcAB1AHQAIgA6AHsAIgBoAG8AbwBrAEUAdgBlAG4AdABOAGEAbQBlACIAOgAiAFAAcgBlAFQAbwBvAGwAVQBzAGUAIgAsACIAcABlAHIAbQBpAHMAcwBpAG8AbgBEAGUAYwBpAHMAaQBvAG4AIgA6ACIAZABlAG4AeQAiACwAIgBwAGUAcgBtAGkAcwBzAGkAbwBuAEQAZQBjAGkAcwBpAG8AbgBSAGUAYQBzAG8AbgAiADoAIgBOAG8AdgBhACAAYwBvAG0AcABsAGUAdABlAF8AdABlAHgAdAAgAGYAbwByAGIAaQBkAHMAIABhAGwAbAAgAHQAbwBvAGwAcwAiAH0AfQAnACkA";

fn restrict_completion(command: &mut Command) {
    let hooks = format!(
        "hooks.PreToolUse=[{{hooks=[{{type=\"command\",command={},commandWindows={},timeout=5}}]}}]",
        json!(DENY_TOOLS), json!(DENY_TOOLS_WINDOWS)
    );
    command.args(["--strict-config", "-c", &hooks, "-c", "features.hooks=true",
        "-c", "web_search=\"disabled\"", "-c", "notify=[]",
        "-c", "tools.experimental_request_user_input.enabled=false", "-c", "tools.update_plan.enabled=false"]);
    // Remove native executors, not merely their approval paths. These feature
    // names are declared by Codex 0.161.0; strict-config fails on incompatible CLIs.
    for feature in ["shell_tool", "unified_exec", "multi_agent", "code_mode", "code_mode_host",
        "view_image", "browser_use", "computer_use", "apps", "plugins", "image_generation", "goals",
        "js_repl", "memory_tool", "multi_agent_v2", "browser_use_external", "imagegenext",
        "request_permissions", "sleep_tool", "tool_search", "skill_search"] {
        command.args(["-c", &format!("features.{feature}=false")]);
    }
    // Do not run inherited lifecycle hooks. hooks/list below also rejects hooks
    // loaded from hooks.json/plugins/managed configuration that survived overrides.
    for event in ["PermissionRequest", "PostToolUse", "PreCompact", "PostCompact",
        "SessionStart", "SessionEnd", "UserPromptSubmit", "SubagentStart", "SubagentStop", "Stop", "Interrupt"] {
        command.args(["-c", &format!("hooks.{event}=[]")]);
    }
}

fn check_completion_hooks(response: &Value) -> Result<Value, String> {
    let entries = response["data"].as_array().ok_or("Codex 未提供 hooks/list 能力")?;
    let mut state = serde_json::Map::new();
    for entry in entries {
        if !entry["errors"].as_array().is_some_and(Vec::is_empty) {
            return Err("Codex 一次性生成的工具拦截 hook 加载失败".into());
        }
        for hook in entry["hooks"].as_array().ok_or("Codex 未返回 hook 列表")? {
            if hook["enabled"] == false { continue; }
            if hook["eventName"] != "preToolUse" || hook["handlerType"] != "command"
                || hook["source"] != "sessionFlags"
                || hook["async"] == true || !hook["matcher"].is_null()
                || ![DENY_TOOLS, DENY_TOOLS_WINDOWS].iter().any(|command| hook["command"] == *command) {
                return Err("Codex 一次性生成拒绝未受控或未信任的 hook".into());
            }
            let key = hook["key"].as_str().ok_or("Codex hook 缺少 key")?;
            let hash = hook["currentHash"].as_str().filter(|hash| hash.starts_with("sha256:") && hash.len() == 71)
                .ok_or("Codex hook 缺少有效 currentHash")?;
            // Only trust our exact command from our CLI layer. Never persist trust
            // or approve any inherited hook. Codex initially lists CLI hooks as untrusted.
            state.insert(key.into(), json!({"enabled":true,"trusted_hash":hash}));
        }
    }
    if state.is_empty() { return Err("Codex 一次性生成需要启用原生 PreToolUse 拦截 hook（Codex 0.161.0+）".into()); }
    Ok(json!({"PreToolUse":[{"hooks":[{"type":"command","command":DENY_TOOLS,
        "commandWindows":DENY_TOOLS_WINDOWS,"timeout":5}]}],"state":state}))
}

fn thread_options(request: &Value, options: &Options) -> Value {
    let title = matches!(request["action"].as_str(), Some("title" | "complete_text"));
    let read_only = title || request["mode"] == "plan";
    let mut guidance = Vec::new();
    let rtk = rtk_guidance();
    let mut mcp = if title { None } else { options.polaris.clone() };
    if let Some(mcp) = mcp.as_mut() {
        mcp["env"]["NOVA_TOOLS_CWD"] = request["cwd"].clone();
        mcp["env"]["NOVA_TOOLS_READ_ONLY"] = json!(if read_only { "1" } else { "0" });
        // 自动模式：线程 id / 场景 / 是否可按场景转交，供 nova-tools 暴露场景转交工具。
        if let Some(env) = request["novaToolEnv"].as_object() {
            for (name, value) in env { mcp["env"][name] = value.clone(); }
        }
        if mcp["env"]["NOVA_FAST_CONTEXT"] != "0" { guidance.push(POLARIS_GUIDANCE); }
    }
    if !title {
        if !rtk.is_empty() {
            guidance.push(&rtk);
        }
        if options.ponytail {
            guidance.push(crate::lyra::PONYTAIL_RULES);
        }
        if read_only {
            guidance.push("Current Nova mode is plan/read-only: analyze and propose a plan; do not modify files.");
        }
    }
    let mut value = json!({
        "cwd": request["cwd"], "model": optional_text(&request["model"]),
        "sandbox": if read_only { "read-only" } else { "danger-full-access" },
        "approvalPolicy": "never", "approvalsReviewer": "user", "developerInstructions": guidance.join("\n\n"),
        "config": {"mcp_servers.nova-tools": mcp.unwrap_or_else(|| json!({"command":"codex", "enabled":false}))}
    });
    if title {
        value["ephemeral"] = json!(true);
    }
    value
}

fn optional_text(value: &Value) -> Option<&str> {
    value.as_str().filter(|s| !s.is_empty())
}

/// Windows npm shims need a shell. Resolve the native binary instead.
pub fn executable(program: &str) -> Result<PathBuf, String> {
    let resolved = crate::acp::resolve_program_on_path(program).unwrap_or_else(|| program.into());
    if !cfg!(windows)
        || resolved
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"))
    {
        return Ok(resolved);
    }
    let target = if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "x86_64"
    };
    let package = if cfg!(target_arch = "aarch64") {
        "codex-win32-arm64"
    } else {
        "codex-win32-x64"
    };
    let mut roots = Vec::new();
    if let Some(parent) = resolved.parent() {
        roots.push(parent.to_path_buf());
    }
    if let Some(dir) = std::env::var_os("APPDATA") {
        roots.push(PathBuf::from(dir).join("npm"));
    }
    if let Some(dir) = std::env::var_os("npm_config_prefix") {
        roots.push(dir.into());
    }
    if let Some(path) = std::env::var_os("PATH") {
        roots.extend(std::env::split_paths(&path));
    }
    for root in roots {
        for prefix in [
            format!("node_modules/@openai/codex/node_modules/@openai/{package}"),
            format!("node_modules/@openai/{package}"),
            "node_modules/@openai/codex".into(),
        ] {
            let binary = root
                .join(prefix)
                .join(format!("vendor/{target}-pc-windows-msvc/bin/codex.exe"));
            if binary.is_file() {
                return Ok(binary);
            }
        }
    }
    Err(format!(
        "无法从 {program} 定位 Codex 原生程序，请配置 codex.exe 路径"
    ))
}

#[derive(Default)]
struct Images(Vec<PathBuf>);

impl Images {
    fn input(&mut self, request: &Value) -> Result<Value, String> {
        let mut parts = Vec::new();
        for part in request["parts"].as_array().into_iter().flatten() {
            match part["type"].as_str() {
                Some("text") => parts.push(json!({"type":"text", "text":part["text"]})),
                Some("local_image") => {
                    parts.push(json!({"type":"localImage", "path":part["path"]}))
                }
                Some("image_data") => {
                    let data = base64::engine::general_purpose::STANDARD
                        .decode(part["data"].as_str().unwrap_or_default())
                        .map_err(|e| format!("无效图片数据：{e}"))?;
                    let dir =
                        std::env::temp_dir().join(format!("nova-codex-{}", uuid::Uuid::new_v4()));
                    std::fs::create_dir(&dir).map_err(|e| e.to_string())?;
                    self.0.push(dir.clone());
                    let ext = Path::new(part["name"].as_str().unwrap_or(""))
                        .extension()
                        .and_then(|v| v.to_str())
                        .filter(|v| v.len() <= 10 && v.bytes().all(|c| c.is_ascii_alphanumeric()))
                        .unwrap_or_else(|| match part["mime"].as_str() {
                            Some("image/jpeg") => "jpg",
                            Some("image/webp") => "webp",
                            Some("image/gif") => "gif",
                            _ => "png",
                        });
                    let path = dir.join(format!("image.{ext}"));
                    std::fs::write(&path, data).map_err(|e| e.to_string())?;
                    parts.push(json!({"type":"localImage", "path":path}));
                }
                _ => {}
            }
        }
        Ok(json!(parts))
    }
}

impl Drop for Images {
    fn drop(&mut self) {
        for dir in &self.0 {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

/// Covers normal shutdown, errors, cancellation and task abortion.
struct ServerChild(Child);
impl Drop for ServerChild {
    fn drop(&mut self) {
        if let Some(pid) = self.0.id() {
            crate::acp::kill_process_tree(pid);
            let _ = self.0.start_kill();
        }
    }
}

async fn write(stdin: &mut ChildStdin, message: Value) -> Result<(), String> {
    stdin
        .write_all(format!("{message}\n").as_bytes())
        .await
        .map_err(|e| format!("Codex app-server 写入失败：{e}"))
}

struct Pending {
    method: &'static str,
    deadline: Instant,
}

struct Client {
    stdin: ChildStdin,
    pending: HashMap<u64, Pending>,
    next_id: u64,
}

impl Client {
    async fn rpc(&mut self, method: &'static str, params: Value) -> Result<(), String> {
        self.next_id += 1;
        write(
            &mut self.stdin,
            json!({"id":self.next_id,"method":method,"params":params}),
        )
        .await?;
        self.pending.insert(
            self.next_id,
            Pending {
                method,
                deadline: Instant::now() + RPC_TIMEOUT,
            },
        );
        Ok(())
    }
}

fn normalized_status(value: &Value) -> Value {
    if value == "inProgress" {
        json!("in_progress")
    } else {
        value.clone()
    }
}

fn normalize_item(item: &Value) -> Option<Value> {
    let mut value = item.clone();
    let kind = match item["type"].as_str()? {
        "agentMessage" | "plan" => "agent_message",
        "reasoning" => {
            let chunks = item["summary"]
                .as_array()
                .filter(|v| !v.is_empty())
                .or_else(|| item["content"].as_array());
            value["text"] = json!(chunks
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("\n"));
            "reasoning"
        }
        "commandExecution" => {
            value["aggregated_output"] =
                json!(item["aggregatedOutput"].as_str().unwrap_or_default());
            value["exit_code"] = item["exitCode"].clone();
            "command_execution"
        }
        "fileChange" => {
            if let Some(changes) = value["changes"].as_array_mut() {
                for change in changes {
                    if change["kind"].is_object() {
                        change["kind"] = change["kind"]["type"].clone();
                    }
                }
            }
            "file_change"
        }
        "mcpToolCall" => "mcp_tool_call",
        "dynamicToolCall" => {
            value["server"] = json!("Codex");
            value["result"] = json!({"content":item["contentItems"]});
            "mcp_tool_call"
        }
        "webSearch" => "web_search",
        _ => return None,
    };
    value["type"] = json!(kind);
    value["status"] = normalized_status(&item["status"]);
    Some(value)
}

#[derive(Default)]
struct Turn {
    thread: Option<String>,
    id: Option<String>,
    items: HashMap<String, Value>,
    order: Vec<String>,
    permissions: HashMap<String, Value>,
    usage: Value,
    completed: Option<Value>,
}

impl Turn {
    fn item(&mut self, item: Value, title: bool, emit: &impl Fn(Value)) {
        let Some(id) = item["id"].as_str() else {
            return;
        };
        if !self.items.contains_key(id) {
            self.order.push(id.to_string());
        }
        if !title {
            if let Some(value) = normalize_item(&item) {
                emit(json!({"type":"item","item":value}));
            }
        }
        self.items.insert(id.to_string(), item);
    }

    fn notification(
        &mut self,
        method: &str,
        p: &Value,
        title: bool,
        emit: &impl Fn(Value),
    ) -> Result<(), String> {
        if p["threadId"]
            .as_str()
            .is_some_and(|id| self.thread.as_deref() != Some(id))
        {
            return Ok(());
        }
        if p["turnId"]
            .as_str()
            .is_some_and(|id| self.id.as_deref().is_some_and(|current| current != id))
        {
            return Ok(());
        }
        let item_id = p["itemId"].as_str().unwrap_or_default();
        match method {
            "turn/started" => self.id = p["turn"]["id"].as_str().map(str::to_string),
            "item/started" | "item/completed" => self.item(p["item"].clone(), title, emit),
            "item/agentMessage/delta" | "item/plan/delta" => {
                let mut item = self
                    .items
                    .get(item_id)
                    .cloned()
                    .unwrap_or_else(|| json!({"id":item_id,"type":"agentMessage","text":""}));
                item["text"] = json!(format!(
                    "{}{}",
                    item["text"].as_str().unwrap_or_default(),
                    p["delta"].as_str().unwrap_or_default()
                ));
                self.item(item, title, emit);
            }
            "item/reasoning/summaryTextDelta" | "item/reasoning/textDelta" => {
                let mut item = self.items.get(item_id).cloned().unwrap_or_else(
                    || json!({"id":item_id,"type":"reasoning","summary":[],"content":[]}),
                );
                let (field, index) = if method.contains("summary") {
                    ("summary", "summaryIndex")
                } else {
                    ("content", "contentIndex")
                };
                let index = p[index].as_u64().unwrap_or(0) as usize;
                if index > 10000 {
                    return Err("Codex reasoning index 超出范围".into());
                }
                let mut chunks = item[field].as_array().cloned().unwrap_or_default();
                chunks.resize(chunks.len().max(index + 1), json!(""));
                chunks[index] = json!(format!(
                    "{}{}",
                    chunks[index].as_str().unwrap_or_default(),
                    p["delta"].as_str().unwrap_or_default()
                ));
                item[field] = json!(chunks);
                self.item(item, title, emit);
            }
            "item/commandExecution/outputDelta" => {
                if let Some(mut item) = self.items.get(item_id).cloned() {
                    item["aggregatedOutput"] = json!(format!(
                        "{}{}",
                        item["aggregatedOutput"].as_str().unwrap_or_default(),
                        p["delta"].as_str().unwrap_or_default()
                    ));
                    self.item(item, title, emit);
                }
            }
            "turn/plan/updated" if !title => emit(
                json!({"type":"plan","plan":p["plan"].as_array().into_iter().flatten().map(|step| json!({"content":step["step"],"status":normalized_status(&step["status"]),"priority":"medium"})).collect::<Vec<_>>()}),
            ),
            "thread/tokenUsage/updated" => {
                let total = &p["tokenUsage"]["total"];
                self.usage = json!({"input_tokens":total["inputTokens"],"output_tokens":total["outputTokens"],"cached_input_tokens":total["cachedInputTokens"],"cache_creation_input_tokens":total["cacheWriteInputTokens"].as_u64().unwrap_or(0)});
            }
            "turn/completed" => self.completed = Some(p["turn"].clone()),
            "error" if p["willRetry"] != true => {
                return Err(p["error"]["message"]
                    .as_str()
                    .unwrap_or("Codex turn failed")
                    .into())
            }
            _ => {}
        }
        Ok(())
    }

    fn result(&self, title: bool) -> Result<Value, String> {
        let turn = self.completed.as_ref().ok_or("Codex 未返回完成事件")?;
        if turn["status"] == "failed" {
            return Err(turn["error"]["message"]
                .as_str()
                .unwrap_or("Codex turn failed")
                .into());
        }
        if title {
            if turn["status"] != "completed" {
                return Err("Codex text generation did not complete".into());
            }
            let messages: Vec<_> = self
                .order
                .iter()
                .filter_map(|id| self.items.get(id))
                .filter(|item| item["type"] == "agentMessage")
                .collect();
            let final_text = messages
                .iter()
                .filter(|item| item["phase"] == "final_answer")
                .filter_map(|item| item["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n");
            return Ok(json!(if final_text.is_empty() {
                messages
                    .last()
                    .and_then(|item| item["text"].as_str())
                    .unwrap_or_default()
                    .to_string()
            } else {
                final_text
            }));
        }
        Ok(json!({"type":"done","usage":self.usage,"cancelled":turn["status"] == "interrupted"}))
    }
}

async fn control(
    client: &mut Client,
    turn: &mut Turn,
    images: &mut Images,
    value: Value,
) -> Result<(), String> {
    match value["action"].as_str() {
        Some("cancel") => client.rpc("turn/interrupt", json!({"threadId":turn.thread,"turnId":turn.id})).await?,
        Some("steer") => client.rpc("turn/steer", json!({"threadId":turn.thread,"expectedTurnId":turn.id,"input":images.input(&value)?})).await?,
        Some("permission") => {
            if let Some(id) = turn.permissions.remove(value["requestId"].as_str().unwrap_or_default()) {
                write(&mut client.stdin, json!({"id":id,"result":{"decision":if value["reply"] == "reject" {"decline"} else {"accept"}}})).await?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// `complete_text`: fresh ephemeral, tool-denied text/image-input completion.
/// Request: {action, cwd, model?, reasoningEffort?, parts:[text|image_data|local_image]}.
/// Returns a JSON string (not a title-normalized value); never resumes a session.
/// The caller should use codex_launch(cwd, true) and an isolated temporary cwd.
pub async fn run(
    mut command: Command,
    request: Value,
    options: Options,
    mut controls: mpsc::UnboundedReceiver<String>,
    emit: impl Fn(Value),
) -> Result<Value, String> {
    let complete = request["action"] == "complete_text";
    let title = request["action"] == "title" || complete;
    let fork = request["action"] == "fork";
    if !title && !fork && request["action"] != "prompt" {
        return Err("Unknown Codex action".into());
    }
    if complete {
        if optional_text(&request["sessionId"]).is_some() {
            return Err("Codex complete_text cannot resume a session".into());
        }
        if !request["parts"].as_array().is_some_and(|parts| !parts.is_empty() && parts.iter().all(|part| match part["type"].as_str() {
            Some("text") => part["text"].is_string(),
            Some("image_data") => part["data"].is_string(),
            Some("local_image") => part["path"].is_string(),
            _ => false,
        })) {
            return Err("Codex complete_text requires valid text/image input parts".into());
        }
        restrict_completion(&mut command);
    }
    if fork && request["retainedTurns"].as_u64().unwrap_or(0) == 0 {
        return Err("retainedTurns must be a positive integer".into());
    }
    command
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x0800_0000);
    #[cfg(unix)]
    command.process_group(0);
    let mut child = ServerChild(
        command
            .spawn()
            .map_err(|e| format!("启动 Codex app-server 失败：{e}"))?,
    );
    let mut lines =
        BufReader::new(child.0.stdout.take().ok_or("Codex stdout unavailable")?).lines();
    let mut stderr =
        BufReader::new(child.0.stderr.take().ok_or("Codex stderr unavailable")?).lines();
    let mut client = Client {
        stdin: child.0.stdin.take().ok_or("Codex stdin unavailable")?,
        pending: HashMap::new(),
        next_id: 0,
    };
    let mut images = Images::default();
    let mut turn = Turn::default();
    let mut queued = VecDeque::new();
    let mut controls_open = true;
    let mut stderr_open = true;
    let mut errors = VecDeque::new();
    let mut completion_config = json!({});
    let completion_deadline = Instant::now() + Duration::from_millis(request["timeoutMs"].as_u64().unwrap_or(60_000));
    let result = async {
        client.rpc("initialize", json!({"clientInfo":{"name":"nova_app_server","title":"Nova","version":env!("CARGO_PKG_VERSION")},"capabilities":{"experimentalApi":true}})).await?;
        loop {
            if turn.completed.is_some() {
                let result = turn.result(title)?;
                if complete && result.as_str().is_none_or(|text| text.trim().is_empty()) {
                    return Err("Codex complete_text returned empty text".into());
                }
                return Ok(result);
            }
            if turn.id.is_some() {
                while let Some(value) = queued.pop_front() { control(&mut client, &mut turn, &mut images, value).await?; }
            }
            let mut deadline = client.pending.values().map(|p| p.deadline).min().unwrap_or_else(|| Instant::now() + RPC_TIMEOUT);
            if complete { deadline = deadline.min(completion_deadline); }
            tokio::select! {
                line = lines.next_line() => {
                    let line = line.map_err(|e| e.to_string())?.ok_or("Codex app-server 在请求完成前退出")?;
                    if line.trim().is_empty() { continue; }
                    let message: Value = serde_json::from_str(&line).map_err(|e| format!("Codex JSON-RPC 无效：{e}"))?;
                    if let Some(method) = message["method"].as_str() {
                        if let Some(id) = message.get("id") {
                            if matches!(method, "item/commandExecution/requestApproval" | "item/fileChange/requestApproval") {
                                if title { write(&mut client.stdin, json!({"id":id,"result":{"decision":"decline"}})).await?; }
                                else {
                                    // Keep integer and string server IDs distinct from each other and client IDs.
                                    let key = id.to_string();
                                    turn.permissions.insert(key.clone(), id.clone());
                                    emit(json!({"type":"permission","permission":{"id":key,"permission":message["params"]["reason"].as_str().unwrap_or(method),"metadata":message["params"]}}));
                                }
                            } else { write(&mut client.stdin, json!({"id":id,"error":{"code":-32601,"message":format!("Nova does not support {method}")}})).await?; }
                        } else {
                            // ponytail: upstream hook errors fail open. This notification abort
                            // is best-effort, not an atomic barrier; strict fault-safe no-tools
                            // requires Codex to expose a fail-closed hook/tool policy.
                            if complete && method == "hook/completed" && message["params"]["run"]["status"] != "blocked" {
                                return Err("Codex 工具拦截 hook 未成功拒绝，终止生成".into());
                            }
                            turn.notification(method, &message["params"], title, &emit)?;
                        }
                        continue;
                    }
                    let Some(pending) = message["id"].as_u64().and_then(|id| client.pending.remove(&id)) else { continue; };
                    if let Some(error) = message.get("error").filter(|v| !v.is_null()) {
                        let error = error["message"].as_str().unwrap_or("Codex RPC failed");
                        if pending.method == "turn/steer" {
                            emit(json!({"type":"item","item":{"id":format!("steer-error-{}",client.next_id),"type":"error","message":format!("会话引导失败：{error}")}}));
                            continue;
                        }
                        return Err(format!("{}: {error}", pending.method));
                    }
                    let response = &message["result"];
                    match pending.method {
                        "initialize" => {
                            write(&mut client.stdin, json!({"method":"initialized","params":{}})).await?;
                            if complete { client.rpc("hooks/list", json!({"cwds":[request["cwd"]]})).await?; }
                            else if fork { client.rpc("thread/read", json!({"threadId":request["sessionId"],"includeTurns":true})).await?; }
                            else {
                                let mut params = thread_options(&request, &options);
                                let resume = request["sessionId"].as_str().is_some_and(|s| !s.is_empty());
                                if resume { params["threadId"] = request["sessionId"].clone(); }
                                client.rpc(if resume { "thread/resume" } else { "thread/start" }, params).await?;
                            }
                        }
                        "hooks/list" => {
                            completion_config["hooks"] = check_completion_hooks(response)?;
                            client.rpc("config/read", json!({"cwd":request["cwd"],"includeLayers":false})).await?;
                        }
                        "config/read" => {
                            // Disable every configured MCP server, not just Nova's; avoid
                            // starting user MCP subprocesses before a tool call is attempted.
                            if let Some(servers) = response["config"]["mcp_servers"].as_object() {
                                // Overlay only enabled: config/read includes null optionals
                                // which cannot round-trip through Codex's TOML overrides.
                                completion_config["mcp_servers"] = json!(servers.keys().map(|name|
                                    (name.clone(), json!({"enabled":false}))).collect::<serde_json::Map<_, _>>());
                            }
                            let mut params = thread_options(&request, &options);
                            params["config"].as_object_mut().unwrap().extend(completion_config.as_object().unwrap().clone());
                            client.rpc("thread/start", params).await?;
                        }
                        "thread/start" | "thread/resume" => {
                            turn.thread = Some(response["thread"]["id"].as_str().ok_or("Codex 未返回 thread id")?.to_string());
                            if !title { emit(json!({"type":"ready","sessionId":turn.thread})); }
                            let input = if request["action"] == "title" { json!([{"type":"text","text":request["prompt"]}]) } else { images.input(&request)? };
                            client.rpc("turn/start", json!({"threadId":turn.thread,"input":input,"cwd":request["cwd"],"model":optional_text(&request["model"]),"effort":optional_text(&request["reasoningEffort"]),"approvalPolicy":"never","sandboxPolicy":{"type":if title || request["mode"] == "plan" { "readOnly" } else { "dangerFullAccess" }}})).await?;
                        }
                        "turn/start" => turn.id = Some(response["turn"]["id"].as_str().ok_or("Codex 未返回 turn id")?.to_string()),
                        "thread/read" => {
                            let index = request["retainedTurns"].as_u64().unwrap() - 1;
                            let last = response["thread"]["turns"].as_array().and_then(|turns| usize::try_from(index).ok().and_then(|i| turns.get(i))).and_then(|t| t["id"].as_str()).ok_or("Codex 会话没有指定的保留轮次")?;
                            let mut params = thread_options(&request, &options);
                            params["threadId"] = request["sessionId"].clone();
                            params["lastTurnId"] = json!(last);
                            client.rpc("thread/fork", params).await?;
                        }
                        "thread/fork" => return response["thread"]["id"].as_str().map(|id| json!(id)).ok_or_else(|| "Codex fork 未返回会话 ID".into()),
                        _ => {}
                    }
                }
                value = controls.recv(), if controls_open && !complete => {
                    if let Some(line) = value {
                        let value: Value = serde_json::from_str(&line).map_err(|e| e.to_string())?;
                        if value["action"] == "permission" || turn.id.is_some() { control(&mut client, &mut turn, &mut images, value).await?; }
                        else { queued.push_back(value); }
                    } else { controls_open = false; }
                }
                line = stderr.next_line(), if stderr_open => {
                    match line {
                        Ok(Some(line)) => { errors.push_back(line); if errors.len() > 20 { errors.pop_front(); } }
                        _ => stderr_open = false,
                    }
                }
                _ = tokio::time::sleep_until(deadline) => {
                    if complete && Instant::now() >= completion_deadline { return Err("Codex complete_text timed out after 60s".into()); }
                    if let Some(p) = client.pending.values().find(|p| p.deadline <= Instant::now()) { return Err(format!("Codex app-server request timed out: {}",p.method)); }
                }
            }
        }
    }.await;
    // EOF first lets Codex close MCP children and persist the interrupted turn.
    drop(client);
    if tokio::time::timeout(Duration::from_millis(1500), child.0.wait())
        .await
        .is_err()
    {
        if let Some(pid) = child.0.id() {
            crate::acp::kill_process_tree(pid);
        }
        let _ = child.0.start_kill();
        let _ = child.0.wait().await;
    }
    result.map_err(|error: String| {
        if errors.is_empty() {
            error
        } else {
            format!(
                "{error}；stderr：{}",
                errors.into_iter().collect::<Vec<_>>().join("\n")
            )
        }
    })
}

pub fn spawn_prompt(
    command: Command,
    request: Value,
    options: Options,
) -> crate::lyra::InProcessSession {
    let (control, controls) = mpsc::unbounded_channel();
    let (tx, events) = mpsc::unbounded_channel();
    let task = tokio::spawn(async move {
        let emit = |value: Value| {
            let _ = tx.send(value.to_string());
        };
        match run(command, request, options, controls, &emit).await {
            Ok(done) => emit(done),
            Err(error) => emit(json!({"ok":false,"error":error})),
        }
    });
    crate::lyra::InProcessSession {
        control,
        events,
        task,
    }
}

#[cfg(test)]
mod tests;
