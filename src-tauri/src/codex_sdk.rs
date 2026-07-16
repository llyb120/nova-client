use crate::acp::{apply_proxy_env, resolve_program_on_path, EV_THREADS, EV_TURN, EV_UPDATE};
use crate::threads::{now_ms, Item, PromptImage, ToolCall};
use crate::AppState;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SdkBackend {
    Codex,
    CodeBuddy,
    Claude,
    Cursor,
}

impl SdkBackend {
    fn label(self) -> &'static str {
        match self {
            Self::Codex => "Codex+",
            Self::CodeBuddy => "CodeBuddy+",
            Self::Claude => "Claude Code+",
            Self::Cursor => "Cursor+",
        }
    }

    fn bridge(self) -> (&'static str, &'static [u8]) {
        match self {
            Self::Codex => (
                "codex-bridge.mjs",
                include_bytes!("../resources/codex-bridge.mjs"),
            ),
            Self::CodeBuddy => (
                "codebuddy-bridge.cjs",
                include_bytes!("../resources/codebuddy-bridge.cjs"),
            ),
            Self::Claude => (
                "claude-bridge.mjs",
                include_bytes!("../resources/claude-bridge.mjs"),
            ),
            Self::Cursor => (
                "cursor-bridge.mjs",
                include_bytes!("../resources/cursor-bridge.mjs"),
            ),
        }
    }
}

struct RunningBridge {
    stdin: Arc<tokio::sync::Mutex<ChildStdin>>,
    pid: Option<u32>,
}

struct IdleBridge {
    child: Child,
    stdin: Arc<tokio::sync::Mutex<ChildStdin>>,
    stdout: BufReader<tokio::process::ChildStdout>,
}

pub struct CodexSdkManager {
    app: AppHandle,
    backend: SdkBackend,
    running_children: Mutex<HashMap<String, RunningBridge>>,
    idle_children: Mutex<HashMap<String, IdleBridge>>,
    running: Mutex<HashSet<String>>,
    turn_started: Mutex<HashMap<String, Instant>>,
    pending_permissions: Mutex<HashMap<String, (String, String)>>,
}

impl CodexSdkManager {
    pub fn new(app: AppHandle, backend: SdkBackend) -> Arc<Self> {
        Arc::new(Self {
            app,
            backend,
            running_children: Mutex::new(HashMap::new()),
            idle_children: Mutex::new(HashMap::new()),
            running: Mutex::new(HashSet::new()),
            turn_started: Mutex::new(HashMap::new()),
            pending_permissions: Mutex::new(HashMap::new()),
        })
    }

    pub fn is_running(&self, thread_id: &str) -> bool {
        self.running.lock().unwrap().contains(thread_id)
    }

    pub async fn run_prompt(
        self: Arc<Self>,
        thread_id: String,
        text: String,
        images: Vec<PromptImage>,
    ) {
        if self.is_running(&thread_id) {
            return;
        }
        let (cwd, mut model, mode, mut reasoning_effort, context, session_id) = {
            let state = self.app.state::<AppState>();
            let mut store = state.store.lock().unwrap();
            let Some(thread) = store.get_mut(&thread_id) else {
                return;
            };
            let context = thread.take_prompt_context(self.backend.label());
            let item = thread.push_user(text.clone(), images.clone());
            let _ = self.emit_update(&thread_id, &item);
            let values = (
                thread.cwd.clone(),
                thread.model.clone(),
                thread.mode.clone(),
                thread.reasoning_effort.clone(),
                context,
                thread.acp_session_id.clone(),
            );
            store.save();
            values
        };
        if self.backend == SdkBackend::Codex {
            let state = self.app.state::<AppState>();
            if let (Some(selected), Some(options)) =
                (model.as_deref(), state.codex.get_model_options())
            {
                match resolve_codex_model(&options, selected, reasoning_effort.as_deref()) {
                    Some((resolved_model, resolved_effort)) => {
                        model = Some(resolved_model);
                        reasoning_effort = resolved_effort;
                    }
                    None => {
                        model = None;
                        reasoning_effort = None;
                        let mut store = state.store.lock().unwrap();
                        if let Some(thread) = store.get_mut(&thread_id) {
                            thread.model = None;
                            thread.reasoning_effort = None;
                        }
                        store.save();
                    }
                }
            }
        }
        self.set_running(&thread_id, true, None);

        let mut parts = Vec::new();
        if let Some(context) = context {
            parts.push(json!({ "type": "text", "text": context }));
        }
        if !text.is_empty() {
            parts.push(json!({ "type": "text", "text": text }));
        }
        for image in images {
            if let Some(data) = image.data {
                parts.push(json!({
                    "type": "image_data", "name": image.name, "mime": image.mime_type, "data": data
                }));
            } else if let Some(uri) = image.uri {
                let path = uri.strip_prefix("file://").unwrap_or(&uri);
                parts.push(json!({ "type": "local_image", "path": path }));
            }
        }
        let request = json!({
            "action": "prompt",
            "cwd": cwd,
            "sessionId": session_id,
            "model": model,
            "mode": mode,
            "reasoningEffort": reasoning_effort,
            "parts": parts
        });
        let outcome = self.run_prompt_bridge(&thread_id, &cwd, request).await;
        if !self.is_running(&thread_id) {
            return;
        }
        let succeeded = outcome.is_ok();
        if let Err(error) = outcome {
            self.push_system(
                &thread_id,
                format!("{} 请求失败：{error}", self.backend.label()),
                "error",
            );
        }
        self.finish_turn(
            &thread_id,
            if succeeded { "end_turn" } else { "error" },
            None,
        );
    }

    pub async fn cancel(&self, thread_id: &str) {
        let stdin = self
            .running_children
            .lock()
            .unwrap()
            .get(thread_id)
            .map(|bridge| bridge.stdin.clone());
        if let Some(stdin) = stdin {
            let _ = write_line(&stdin, &json!({ "action": "cancel" })).await;
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        if let Some(bridge) = self.running_children.lock().unwrap().remove(thread_id) {
            if let Some(pid) = bridge.pid {
                crate::acp::kill_process_tree(pid);
            }
        }
        self.finish_turn(thread_id, "cancelled", None);
    }

    pub fn forget_session_of_thread(&self, thread_id: &str) {
        if let Some(bridge) = self.running_children.lock().unwrap().remove(thread_id) {
            if let Some(pid) = bridge.pid {
                crate::acp::kill_process_tree(pid);
            }
        }
        if let Some(mut bridge) = self.idle_children.lock().unwrap().remove(thread_id) {
            kill_child(&mut bridge.child);
        }
    }

    pub fn shutdown(&self) {
        for bridge in std::mem::take(&mut *self.running_children.lock().unwrap()).into_values() {
            if let Some(pid) = bridge.pid {
                crate::acp::kill_process_tree(pid);
            }
        }
        for mut bridge in std::mem::take(&mut *self.idle_children.lock().unwrap()).into_values() {
            kill_child(&mut bridge.child);
        }
    }

    async fn run_prompt_bridge(
        &self,
        thread_id: &str,
        cwd: &str,
        request: Value,
    ) -> Result<(), String> {
        let mut bridge = match self.backend {
            SdkBackend::Cursor => self.idle_children.lock().unwrap().remove(thread_id),
            _ => None,
        }
        .map(Ok)
        .unwrap_or_else(|| self.spawn_idle_bridge(cwd))?;
        write_line(&bridge.stdin, &request).await?;
        self.running_children.lock().unwrap().insert(
            thread_id.to_string(),
            RunningBridge {
                stdin: bridge.stdin.clone(),
                pid: bridge.child.id(),
            },
        );
        let result = self.read_events(thread_id, &mut bridge.stdout).await;
        self.running_children.lock().unwrap().remove(thread_id);
        let reusable = self.backend == SdkBackend::Cursor
            && result.is_ok()
            && bridge.child.try_wait().ok().flatten().is_none();
        if reusable {
            self.idle_children
                .lock()
                .unwrap()
                .insert(thread_id.to_string(), bridge);
        } else if result.is_err() {
            kill_child(&mut bridge.child);
        }
        result
    }

    fn spawn_idle_bridge(&self, cwd: &str) -> Result<IdleBridge, String> {
        let mut child = self.spawn_bridge(cwd)?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| format!("{} bridge stdin 不可用", self.backend.label()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| format!("{} bridge stdout 不可用", self.backend.label()))?;
        Ok(IdleBridge {
            child,
            stdin: Arc::new(tokio::sync::Mutex::new(stdin)),
            stdout: BufReader::new(stdout),
        })
    }

    async fn read_events(
        &self,
        thread_id: &str,
        stdout: &mut BufReader<tokio::process::ChildStdout>,
    ) -> Result<(), String> {
        let mut lines = stdout.lines();
        let mut item_ids = HashMap::new();
        while let Some(line) = lines.next_line().await.map_err(|e| e.to_string())? {
            let event: Value = serde_json::from_str(&line).map_err(|e| {
                format!("解析 {} 事件失败：{e}；输出：{line}", self.backend.label())
            })?;
            if event.get("ok").and_then(Value::as_bool) == Some(false) {
                return Err(event["error"]
                    .as_str()
                    .unwrap_or("SDK bridge 执行失败")
                    .into());
            }
            match event.get("type").and_then(Value::as_str) {
                Some("ready") => {
                    if let Some(session_id) = event.get("sessionId").and_then(Value::as_str) {
                        self.save_session_id(thread_id, session_id);
                    }
                }
                Some("item") => self.apply_item(thread_id, &event["item"], &mut item_ids),
                Some("permission") => self.emit_permission(thread_id, &event["permission"]),
                Some("done") => {
                    let usage = event.get("usage").cloned();
                    self.finish_turn(thread_id, "end_turn", usage);
                    return Ok(());
                }
                _ => {}
            }
        }
        Err(format!("{} bridge 意外退出", self.backend.label()))
    }

    fn spawn_bridge(&self, cwd: &str) -> Result<Child, String> {
        let (program, proxy, path_env, api_key) = {
            let state = self.app.state::<AppState>();
            let settings = state.settings.lock().unwrap();
            match self.backend {
                SdkBackend::Codex => (
                    settings.codex_path.clone(),
                    settings.codex_proxy.clone(),
                    "NOVA_CODEX_PATH",
                    None,
                ),
                SdkBackend::CodeBuddy => (
                    settings.codebuddy_path.clone(),
                    settings.codebuddy_proxy.clone(),
                    "NOVA_CODEBUDDY_PATH",
                    None,
                ),
                SdkBackend::Claude => (
                    "claude".into(),
                    settings.claudecode_proxy.clone(),
                    "NOVA_CLAUDE_PATH",
                    (!settings.claudecode_sdk_api_key.is_empty())
                        .then(|| ("ANTHROPIC_API_KEY", settings.claudecode_sdk_api_key.clone())),
                ),
                SdkBackend::Cursor => (
                    settings.cursor_path.clone(),
                    settings.cursor_proxy.clone(),
                    "NOVA_CURSOR_PATH",
                    (!settings.cursor_sdk_api_key.is_empty())
                        .then(|| ("CURSOR_API_KEY", settings.cursor_sdk_api_key.clone())),
                ),
            }
        };
        let program = resolve_program_on_path(&program)
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or(program);
        let node = resolve_program_on_path("node").ok_or_else(|| {
            format!(
                "未找到 Node.js，{} 需要 Node.js 运行官方 SDK",
                self.backend.label()
            )
        })?;
        let bridge = bridge_path(&self.app, self.backend)?;
        let mut command = Command::new(node);
        command
            .arg(bridge)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .env(path_env, &program);
        apply_proxy_env(&mut command, &proxy);
        if let Some((name, value)) = api_key {
            command.env(name, value);
        }
        #[cfg(windows)]
        command.creation_flags(0x0800_0000);
        command
            .spawn()
            .map_err(|e| format!("启动 {} Node bridge 失败：{e}", self.backend.label()))
    }

    fn save_session_id(&self, thread_id: &str, session_id: &str) {
        let state = self.app.state::<AppState>();
        let mut store = state.store.lock().unwrap();
        if let Some(thread) = store.get_mut(thread_id) {
            thread.acp_session_id = Some(session_id.to_string());
        }
        store.save();
    }

    fn apply_item(&self, thread_id: &str, value: &Value, ids: &mut HashMap<String, u64>) {
        let Some(remote_id) = value.get("id").and_then(Value::as_str) else {
            return;
        };
        let state = self.app.state::<AppState>();
        let mut store = state.store.lock().unwrap();
        let Some(thread) = store.get_mut(thread_id) else {
            return;
        };
        let existing = ids.get(remote_id).copied();
        let id = existing.unwrap_or_else(|| thread.next_item_id());
        let item = match value.get("type").and_then(Value::as_str) {
            Some("agent_message") => {
                value
                    .get("text")
                    .and_then(Value::as_str)
                    .map(|text| Item::Assistant {
                        id,
                        text: text.into(),
                        ts: now_ms(),
                    })
            }
            Some("reasoning") => {
                value
                    .get("text")
                    .and_then(Value::as_str)
                    .map(|text| Item::Thought {
                        id,
                        text: text.into(),
                        ts: now_ms(),
                    })
            }
            Some("error") => {
                value
                    .get("message")
                    .and_then(Value::as_str)
                    .map(|text| Item::System {
                        id,
                        text: text.into(),
                        level: "error".into(),
                        ts: now_ms(),
                    })
            }
            Some("command_execution")
            | Some("file_change")
            | Some("mcp_tool_call")
            | Some("web_search")
            | Some("todo_list") => Some(Item::Tool {
                id,
                ts: now_ms(),
                call: tool_call(value),
            }),
            _ => None,
        };
        let Some(item) = item else {
            return;
        };
        if existing.is_some() {
            if let Some(slot) = thread
                .items
                .iter_mut()
                .find(|candidate| candidate.id() == id)
            {
                *slot = item.clone();
            }
        } else {
            ids.insert(remote_id.into(), id);
            thread.items.push(item.clone());
        }
        thread.updated_at = now_ms();
        let _ = self.emit_update(thread_id, &item);
        store.save();
    }

    fn emit_permission(&self, thread_id: &str, permission: &Value) {
        let Some(request_id) = permission.get("id").and_then(Value::as_str) else {
            return;
        };
        let (prefix, agent_kind) = match self.backend {
            SdkBackend::Claude => ("clp", "claudecode"),
            _ => ("cbp", "codebuddy"),
        };
        let request_key = format!("{prefix}-perm-{thread_id}-{request_id}");
        self.pending_permissions.lock().unwrap().insert(
            request_key.clone(),
            (thread_id.to_string(), request_id.to_string()),
        );
        let _ = self.app.emit(crate::acp::EV_PERMISSION, json!({
            "threadId": thread_id,
            "agentKind": agent_kind,
            "requestKey": request_key,
            "toolCall": {
                "title": permission.get("permission").and_then(Value::as_str).unwrap_or("工具调用"),
                "kind": "other",
                "rawInput": permission.get("metadata").cloned().unwrap_or(Value::Null)
            },
            "options": [
                { "optionId": "once", "name": "允许一次", "kind": "allow_once" },
                { "optionId": "reject", "name": "拒绝", "kind": "reject_once" }
            ]
        }));
    }

    pub async fn respond_permission(
        &self,
        request_key: &str,
        option_id: &str,
    ) -> Result<(), String> {
        let (thread_id, request_id) = self
            .pending_permissions
            .lock()
            .unwrap()
            .remove(request_key)
            .ok_or("该权限请求已失效")?;
        let stdin = self
            .running_children
            .lock()
            .unwrap()
            .get(&thread_id)
            .map(|bridge| bridge.stdin.clone())
            .ok_or_else(|| format!("{} 会话已结束", self.backend.label()))?;
        write_line(&stdin, &json!({ "action": "permission", "requestId": request_id, "reply": if option_id == "reject" { "reject" } else { "once" } })).await?;
        let _ = self.app.emit(
            crate::acp::EV_PERMISSION_RESOLVED,
            json!({ "requestKey": request_key }),
        );
        Ok(())
    }

    fn push_system(&self, thread_id: &str, text: String, level: &str) {
        let state = self.app.state::<AppState>();
        let mut store = state.store.lock().unwrap();
        if let Some(thread) = store.get_mut(thread_id) {
            let item = thread.push_system(text, level);
            let _ = self.emit_update(thread_id, &item);
        }
        store.save();
    }

    fn set_running(&self, thread_id: &str, running: bool, stop_reason: Option<&str>) {
        if running {
            self.running.lock().unwrap().insert(thread_id.into());
            self.turn_started
                .lock()
                .unwrap()
                .insert(thread_id.into(), Instant::now());
        } else {
            self.running.lock().unwrap().remove(thread_id);
        }
        let _ = self.app.emit(
            EV_TURN,
            json!({ "threadId": thread_id, "running": running, "stopReason": stop_reason }),
        );
        let _ = self.app.emit(EV_THREADS, json!({}));
    }

    fn finish_turn(&self, thread_id: &str, stop_reason: &str, usage: Option<Value>) {
        if !self.is_running(thread_id) {
            return;
        }
        let duration = self
            .turn_started
            .lock()
            .unwrap()
            .remove(thread_id)
            .map(|started| started.elapsed().as_millis() as u64)
            .unwrap_or(0);
        let usage = usage.map(|value| {
            json!({
                "inputTokens": value.get("input_tokens").and_then(Value::as_u64),
                "outputTokens": value.get("output_tokens").and_then(Value::as_u64),
                "totalTokens": value.get("input_tokens").and_then(Value::as_u64).unwrap_or(0)
                    + value.get("output_tokens").and_then(Value::as_u64).unwrap_or(0)
            })
        });
        let state = self.app.state::<AppState>();
        let mut store = state.store.lock().unwrap();
        if let Some(thread) = store.get_mut(thread_id) {
            for item in complete_pending_tools(thread) {
                let _ = self.emit_update(thread_id, &item);
            }
            let item = thread.push_turn(duration, usage.as_ref(), stop_reason);
            let _ = self.emit_update(thread_id, &item);
        }
        store.save();
        drop(store);
        self.set_running(thread_id, false, Some(stop_reason));
    }

    fn emit_update(&self, thread_id: &str, item: &Item) -> Result<(), tauri::Error> {
        self.app.emit(
            EV_UPDATE,
            json!({ "threadId": thread_id, "op": { "t": "upsert", "item": item } }),
        )
    }
}

impl Drop for CodexSdkManager {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn bridge_path(app: &AppHandle, backend: SdkBackend) -> Result<PathBuf, String> {
    let (name, bridge) = backend.bridge();
    let dir = crate::nova_data_dir(app).join("runtime");
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("创建 {} 运行目录失败：{e}", backend.label()))?;
    let path = dir.join(name);
    if std::fs::read(&path).ok().as_deref() != Some(bridge) {
        std::fs::write(&path, bridge)
            .map_err(|e| format!("释放 {} bridge 失败：{e}", backend.label()))?;
    }
    Ok(path)
}

async fn write_line(
    stdin: &Arc<tokio::sync::Mutex<ChildStdin>>,
    request: &Value,
) -> Result<(), String> {
    let mut stdin = stdin.lock().await;
    stdin
        .write_all(format!("{request}\n").as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    stdin.flush().await.map_err(|e| e.to_string())
}

fn kill_child(child: &mut Child) {
    if let Some(pid) = child.id() {
        crate::acp::kill_process_tree(pid);
    }
    let _ = child.start_kill();
}

fn resolve_codex_model(
    options: &Value,
    selected: &str,
    reasoning_effort: Option<&str>,
) -> Option<(String, Option<String>)> {
    let models = options["configOptions"]
        .as_array()
        .and_then(|configs| configs.iter().find(|config| config["id"] == "model"))
        .and_then(|config| config["options"].as_array())?;
    let exact = models
        .iter()
        .find(|option| option["value"].as_str() == Some(selected));
    if let Some(option) = exact {
        let effort = option["_meta"]["codex.ai/effort"]
            .as_str()
            .or_else(|| split_codex_effort(selected).map(|(_, effort)| effort))
            .or(reasoning_effort);
        let model = effort
            .and_then(|effort| selected.strip_suffix(&format!(":{effort}")))
            .unwrap_or(selected);
        return Some((model.to_string(), effort.map(str::to_string)));
    }
    if let Some(effort) = reasoning_effort {
        let combined = format!("{selected}:{effort}");
        if models
            .iter()
            .any(|option| option["value"].as_str() == Some(&combined))
        {
            return Some((selected.to_string(), Some(effort.to_string())));
        }
    }
    models
        .iter()
        .any(|option| {
            option["value"].as_str().is_some_and(|value| {
                value
                    .strip_prefix(selected)
                    .is_some_and(|suffix| suffix.starts_with(':'))
            })
        })
        .then(|| (selected.to_string(), None))
}

fn split_codex_effort(value: &str) -> Option<(&str, &str)> {
    const EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max", "ultra"];
    let (model, effort) = value.rsplit_once(':')?;
    EFFORTS.contains(&effort).then_some((model, effort))
}

fn complete_pending_tools(thread: &mut crate::threads::Thread) -> Vec<Item> {
    let mut changed = Vec::new();
    for item in &mut thread.items {
        let Item::Tool { call, .. } = item else {
            continue;
        };
        if call.status == "pending" || call.status == "in_progress" {
            call.status = "completed".to_string();
            changed.push(item.clone());
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::{complete_pending_tools, resolve_codex_model};
    use crate::threads::{now_ms, AgentKind, Item, Thread, ToolCall};
    use serde_json::json;

    #[test]
    fn codex_model_resolution_splits_combined_values() {
        let options = json!({
            "configOptions": [{
                "id": "model",
                "options": [
                    { "value": "gpt-5.6-sol:low", "_meta": { "codex.ai/effort": "low" } },
                    { "value": "gpt-5.6-sol:medium", "_meta": { "codex.ai/effort": "medium" } },
                    { "value": "gpt-5.6-sol:high", "_meta": { "codex.ai/effort": "high" } },
                    { "value": "gpt-5.6-sol:xhigh", "_meta": { "codex.ai/effort": "xhigh" } },
                    { "value": "gpt-5.6-sol:max", "_meta": { "codex.ai/effort": "max" } },
                    { "value": "gpt-5.6-sol:ultra", "_meta": { "codex.ai/effort": "ultra" } },
                    { "value": "gpt-5.6-terra:max" }
                ]
            }]
        });
        for effort in ["low", "medium", "high", "xhigh", "max", "ultra"] {
            assert_eq!(
                resolve_codex_model(&options, &format!("gpt-5.6-sol:{effort}"), None),
                Some(("gpt-5.6-sol".into(), Some(effort.into())))
            );
        }
        assert_eq!(
            resolve_codex_model(&options, "gpt-5.6-sol", Some("high")),
            Some(("gpt-5.6-sol".into(), Some("high".into())))
        );
        assert_eq!(
            resolve_codex_model(&options, "gpt-5.6-terra:max", None),
            Some(("gpt-5.6-terra".into(), Some("max".into())))
        );
        assert_eq!(resolve_codex_model(&options, "gpt-5.4-minilow", None), None);
    }

    #[test]
    fn turn_completion_finishes_pending_sdk_tools() {
        let mut thread = Thread::new(".".into(), AgentKind::Cursor, None, None, None, false);
        thread.items.push(Item::Tool {
            id: 1,
            ts: now_ms(),
            call: ToolCall {
                tool_call_id: "tool".into(),
                title: "glob".into(),
                kind: "other".into(),
                status: "in_progress".into(),
                content: Vec::new(),
                locations: Vec::new(),
                raw_input: None,
                raw_output: None,
            },
        });

        let changed = complete_pending_tools(&mut thread);
        assert_eq!(changed.len(), 1);
        let Item::Tool { call, .. } = &thread.items[0] else {
            panic!("expected tool item");
        };
        assert_eq!(call.status, "completed");
    }
}

fn tool_call(value: &Value) -> ToolCall {
    let kind = value.get("type").and_then(Value::as_str).unwrap_or("tool");
    let status = value
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("completed");
    let (title, raw_input, output) = match kind {
        "command_execution" => (
            value
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or("Command")
                .to_string(),
            value.get("command").cloned(),
            value.get("aggregated_output").cloned(),
        ),
        "file_change" => ("File changes".into(), value.get("changes").cloned(), None),
        "mcp_tool_call" => (
            format!(
                "{} / {}",
                value.get("server").and_then(Value::as_str).unwrap_or("MCP"),
                value.get("tool").and_then(Value::as_str).unwrap_or("tool")
            ),
            value.get("arguments").cloned(),
            value.get("result").or_else(|| value.get("error")).cloned(),
        ),
        "web_search" => ("Web search".into(), value.get("query").cloned(), None),
        "todo_list" => ("Todo list".into(), value.get("items").cloned(), None),
        _ => ("Tool".into(), None, None),
    };
    let output_text = output.as_ref().map(|v| {
        v.as_str()
            .map(str::to_string)
            .unwrap_or_else(|| v.to_string())
    });
    ToolCall {
        tool_call_id: value.get("id").and_then(Value::as_str).unwrap_or("").into(),
        title,
        kind: "other".into(),
        status: match status {
            "failed" => "failed",
            "in_progress" => "in_progress",
            _ => "completed",
        }
        .into(),
        content: output_text
            .map(|text| {
                vec![json!({ "type": "content", "content": { "type": "text", "text": text } })]
            })
            .unwrap_or_default(),
        locations: Vec::new(),
        raw_input,
        raw_output: output,
    }
}
