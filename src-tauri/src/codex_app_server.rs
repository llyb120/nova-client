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
const POLARIS_GUIDANCE: &str = "Nova provides the polaris tool through MCP server nova-tools. Call its exposed MCP tool directly, using the name in the tool schema; do not use Devin's mcp_call_tool wrapper. When edit locations are unknown or you need to read two or more unread files, call polaris first with the user's symbols, keywords or file paths. It returns complete code units, dependencies and coverage gaps. Do not re-read covered ranges or rediscover the same keywords with shell searches; read only gaps / next_reads by exact path and line. Keep searches bounded and honor .gitignore. Use Codex's built-in file and shell tools for edits and verification.";

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
    let powershell = format!("& '{}' __rtk", path.replace('\'', "''"));
    let bash = format!("'{}' __rtk", path.replace('\'', "'\\''"));
    // ponytail: instruction-based like upstream RTK's Codex integration; enforcing every
    // command requires a Codex pre-execution rewrite hook when that API is available.
    format!(
        "Nova includes RTK for compact shell output; no separate rtk installation is needed. \
         Invoke it through Codex's native shell tool using the prefix for that shell:\n\
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

fn thread_options(request: &Value, options: &Options) -> Value {
    let title = request["action"] == "title";
    let read_only = title || request["mode"] == "plan";
    let mut guidance = Vec::new();
    let rtk = rtk_guidance();
    let mut mcp = if title { None } else { options.polaris.clone() };
    if let Some(mcp) = mcp.as_mut() {
        mcp["env"]["NOVA_TOOLS_CWD"] = request["cwd"].clone();
        mcp["env"]["NOVA_TOOLS_READ_ONLY"] = json!(if read_only { "1" } else { "0" });
        guidance.push(POLARIS_GUIDANCE);
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
        "approvalPolicy": "never", "developerInstructions": guidance.join("\n\n"),
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
                        .unwrap_or("png");
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
            if turn["status"] == "interrupted" {
                return Err("Codex title generation interrupted".into());
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

pub async fn run(
    mut command: Command,
    request: Value,
    options: Options,
    mut controls: mpsc::UnboundedReceiver<String>,
    emit: impl Fn(Value),
) -> Result<Value, String> {
    let title = request["action"] == "title";
    let fork = request["action"] == "fork";
    if !title && !fork && request["action"] != "prompt" {
        return Err("Unknown Codex action".into());
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
    let result = async {
        client.rpc("initialize", json!({"clientInfo":{"name":"nova_app_server","title":"Nova","version":env!("CARGO_PKG_VERSION")},"capabilities":{"experimentalApi":true}})).await?;
        loop {
            if turn.completed.is_some() { return turn.result(title); }
            if turn.id.is_some() {
                while let Some(value) = queued.pop_front() { control(&mut client, &mut turn, &mut images, value).await?; }
            }
            let deadline = client.pending.values().map(|p| p.deadline).min().unwrap_or_else(|| Instant::now() + RPC_TIMEOUT);
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
                        } else { turn.notification(method, &message["params"], title, &emit)?; }
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
                            if fork { client.rpc("thread/read", json!({"threadId":request["sessionId"],"includeTurns":true})).await?; }
                            else {
                                let mut params = thread_options(&request, &options);
                                let resume = request["sessionId"].as_str().is_some_and(|s| !s.is_empty());
                                if resume { params["threadId"] = request["sessionId"].clone(); }
                                client.rpc(if resume { "thread/resume" } else { "thread/start" }, params).await?;
                            }
                        }
                        "thread/start" | "thread/resume" => {
                            turn.thread = Some(response["thread"]["id"].as_str().ok_or("Codex 未返回 thread id")?.to_string());
                            if !title { emit(json!({"type":"ready","sessionId":turn.thread})); }
                            let input = if title { json!([{"type":"text","text":request["prompt"]}]) } else { images.input(&request)? };
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
                value = controls.recv(), if controls_open => {
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
