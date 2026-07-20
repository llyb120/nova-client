use crate::acp::{
    apply_proxy_env, resolve_program_on_path, EV_OPTIONS, EV_PERMISSION, EV_PERMISSION_RESOLVED,
    EV_THREADS, EV_TURN, EV_UPDATE,
};
use crate::codex_radar;
use crate::model_cache;
use crate::threads::{
    file_uri_to_local_path, now_ms, save_attachment_to_temp, Item, PromptImage, ToolCall,
};
use crate::AppState;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::time::{sleep, Duration};

pub const MODEL_CACHE_KEY: &str = "opencode-sdk";

struct RunningBridge {
    child: Child,
    stdin: Arc<tokio::sync::Mutex<ChildStdin>>,
}

struct PendingPermission {
    thread_id: String,
    request_id: String,
}

pub struct OpenCodeSdkManager {
    app: AppHandle,
    launch_env: HashMap<String, String>,
    running_children: Mutex<HashMap<String, RunningBridge>>,
    pending_permissions: Mutex<HashMap<String, PendingPermission>>,
    running: Mutex<HashSet<String>>,
    turn_started: Mutex<HashMap<String, Instant>>,
    model_options: Mutex<Option<Value>>,
    model_options_refreshing: AtomicBool,
    model_options_revalidated: AtomicBool,
}

impl OpenCodeSdkManager {
    pub fn new(app: AppHandle) -> Arc<Self> {
        Self::new_with_env(app, HashMap::new())
    }

    pub fn new_with_env(app: AppHandle, launch_env: HashMap<String, String>) -> Arc<Self> {
        Arc::new(Self {
            app,
            launch_env,
            running_children: Mutex::new(HashMap::new()),
            pending_permissions: Mutex::new(HashMap::new()),
            running: Mutex::new(HashSet::new()),
            turn_started: Mutex::new(HashMap::new()),
            model_options: Mutex::new(None),
            model_options_refreshing: AtomicBool::new(false),
            model_options_revalidated: AtomicBool::new(false),
        })
    }

    pub fn is_running(&self, thread_id: &str) -> bool {
        self.running.lock().unwrap().contains(thread_id)
    }

    pub fn has_pending_permission(&self, request_key: &str) -> bool {
        self.pending_permissions
            .lock()
            .unwrap()
            .contains_key(request_key)
    }

    pub fn get_model_options(&self) -> Option<Value> {
        self.model_options.lock().unwrap().clone()
    }

    pub fn seed_model_options(&self, value: Value) {
        *self.model_options.lock().unwrap() = Some(value);
    }

    pub fn spawn_revalidate_model_options(self: &Arc<Self>) {
        if self.model_options_revalidated.load(Ordering::SeqCst)
            || self
                .model_options_refreshing
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
        {
            return;
        }
        let manager = self.clone();
        tauri::async_runtime::spawn(async move {
            let _ = manager.refresh_model_options().await;
            manager
                .model_options_refreshing
                .store(false, Ordering::SeqCst);
        });
    }

    async fn refresh_model_options(&self) -> Result<Value, String> {
        let cwd = current_dir()?;
        let value = provider_options(
            self.run_bridge(&cwd, json!({ "action": "providers" }))
                .await?,
        )?;
        *self.model_options.lock().unwrap() = Some(value.clone());
        model_cache::save(&crate::nova_data_dir(&self.app), MODEL_CACHE_KEY, &value);
        self.model_options_revalidated.store(true, Ordering::SeqCst);
        let _ = self.app.emit(
            EV_OPTIONS,
            json!({ "agentKind": "opencode", "options": value }),
        );
        Ok(value)
    }

    pub async fn ensure_model_options(self: &Arc<Self>) -> Result<Value, String> {
        if let Some(value) = self.get_model_options() {
            self.spawn_revalidate_model_options();
            return Ok(value);
        }
        self.spawn_revalidate_model_options();
        for _ in 0..600 {
            if let Some(value) = self.get_model_options() {
                return Ok(value);
            }
            if !self.model_options_refreshing.load(Ordering::SeqCst) {
                break;
            }
            sleep(Duration::from_millis(100)).await;
        }
        if let Some(value) = self.get_model_options() {
            return Ok(value);
        }
        self.refresh_model_options().await
    }

    pub async fn fetch_commands(&self) -> Result<Vec<Value>, String> {
        let result = self
            .run_bridge(&current_dir()?, json!({ "action": "commands" }))
            .await?;
        Ok(result
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|command| {
                let name = command.get("name")?.as_str()?;
                Some(json!({
                    "name": name,
                    "description": command.get("description").and_then(Value::as_str).unwrap_or(""),
                    "kind": "command",
                    "input": format!("/{name} ")
                }))
            })
            .collect())
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
        let mut title_job: Option<(String, String)> = None;
        let (
            cwd,
            mut model,
            mode,
            reasoning_effort,
            context,
            session_id,
            user_item_id,
            cached_auto_model,
        ) = {
            let state = self.app.state::<AppState>();
            let mut store = state.store.lock().unwrap();
            let Some(thread) = store.get_mut(&thread_id) else {
                return;
            };
            let context = thread.take_prompt_context("OpenCode+");
            let item = thread.push_user(text.clone(), images.clone());
            let user_item_id = item.id();
            if thread.title == "新会话" {
                let fallback = derive_title(&text, !images.is_empty());
                thread.title = fallback.clone();
                title_job = Some((text.clone(), fallback));
                let _ = self.app.emit(EV_THREADS, json!({}));
            }
            let _ = self.emit_update(&thread_id, &item);
            let values = (
                thread.cwd.clone(),
                thread.model.clone(),
                thread.mode.clone(),
                thread.reasoning_effort.clone(),
                context,
                thread.acp_session_id.clone(),
                user_item_id,
                thread
                    .model
                    .as_deref()
                    .and_then(|selection| thread.cached_auto_model(selection)),
            );
            store.save();
            values
        };
        if let Some((prompt, fallback)) = title_job {
            self.app.state::<AppState>().generate_title(
                &crate::threads::AgentKind::OpenCode,
                thread_id.clone(),
                prompt,
                fallback,
            );
        }
        self.set_running(&thread_id, true, None);

        if model.as_deref().is_some_and(codex_radar::is_auto_model) {
            if let Some(cached) = cached_auto_model {
                model = Some(cached);
            } else {
                let selection = model.clone().unwrap_or_default();
                self.push_system(
                    &thread_id,
                    format!(
                        "正在查询 Codex 雷达，为本会话选择{}第一名…",
                        codex_radar::selection_label(&selection)
                    ),
                    "info",
                );
                let options = match self.ensure_model_options().await {
                    Ok(options) => options,
                    Err(error) => {
                        self.push_system(&thread_id, format!("Auto 路由失败：{error}"), "error");
                        self.finish_turn(&thread_id, "error");
                        return;
                    }
                };
                match codex_radar::resolve_auto_model(&selection, &options, true).await {
                    Ok(resolved) => {
                        model = Some(resolved.value.clone());
                        let state = self.app.state::<AppState>();
                        let mut store = state.store.lock().unwrap();
                        if let Some(thread) = store.get_mut(&thread_id) {
                            thread.auto_route_selection = Some(selection);
                            thread.auto_routed_model = Some(resolved.value);
                            thread.auto_routed_label = Some(resolved.label.clone());
                            let item = thread.push_system(
                                format!("Auto 路由完成，实际使用模型：{}", resolved.label),
                                "info",
                            );
                            let _ = self.emit_update(&thread_id, &item);
                        }
                        store.save();
                    }
                    Err(error) => {
                        self.push_system(&thread_id, format!("Auto 路由失败：{error}"), "error");
                        self.finish_turn(&thread_id, "error");
                        return;
                    }
                }
            }
        }

        let mut parts = Vec::new();
        if let Some(context) = context {
            parts.push(json!({ "type": "text", "text": context }));
        }
        if !text.is_empty() {
            parts.push(json!({ "type": "text", "text": text }));
        }
        for image in images {
            if let Some(part) = prompt_attachment_part(image) {
                parts.push(part);
            }
        }
        let selected = model.as_deref().and_then(split_model_variant);
        let model = selected.as_ref().map(
            |(provider_id, model_id, _)| json!({ "providerID": provider_id, "modelID": model_id }),
        );
        let variant = selected
            .and_then(|(_, _, variant)| variant.map(str::to_string))
            .or(reasoning_effort);
        let request = json!({
            "action": "prompt",
            "sessionId": session_id,
            "model": model,
            "variant": variant,
            "mode": mode,
            "agent": mode.filter(|value| value == "plan"),
            "parts": parts
        });
        let request = with_command(request, &text);
        let outcome = self
            .run_prompt_bridge(&thread_id, &cwd, request, user_item_id)
            .await;
        if !self.is_running(&thread_id) {
            return;
        }
        let succeeded = outcome.is_ok();
        match outcome {
            Ok(()) => {}
            Err(error) => {
                self.push_system(&thread_id, format!("OpenCode+ 请求失败：{error}"), "error")
            }
        }
        self.finish_turn(&thread_id, if succeeded { "end_turn" } else { "error" });
    }

    pub async fn cancel(&self, thread_id: &str) {
        if self.is_running(thread_id) {
            self.push_system(thread_id, "已停止当前任务。".into(), "warn");
            self.finish_turn(thread_id, "cancelled");
        }
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
        if let Some(mut bridge) = self.running_children.lock().unwrap().remove(thread_id) {
            kill_child(&mut bridge.child);
        }
    }

    pub async fn fork_session(
        &self,
        cwd: &str,
        session_id: &str,
        position: &str,
    ) -> Result<String, String> {
        let value = self
            .run_bridge(
                cwd,
                json!({
                    "action": "fork",
                    "sessionId": session_id,
                    "position": position,
                }),
            )
            .await?;
        value
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| "OpenCode fork 未返回新会话 ID".into())
    }

    pub fn forget_session_of_thread(&self, thread_id: &str) {
        if let Some(mut bridge) = self.running_children.lock().unwrap().remove(thread_id) {
            kill_child(&mut bridge.child);
        }
        self.clear_permissions(thread_id);
    }

    pub fn shutdown(&self) {
        for mut bridge in std::mem::take(&mut *self.running_children.lock().unwrap()).into_values()
        {
            kill_child(&mut bridge.child);
        }
    }

    pub fn generate_title_async(
        self: &Arc<Self>,
        thread_id: String,
        prompt: String,
        fallback: String,
        model: String,
    ) {
        let manager = self.clone();
        tauri::async_runtime::spawn(async move {
            let selected = split_model_variant(&model);
            let request = json!({
                "action": "title",
                "model": selected.map(|(provider, model, _)| json!({ "providerID": provider, "modelID": model })),
                "variant": selected.and_then(|(_, _, variant)| variant),
                "prompt": format!(
                    "请为下面用户第一次提示词生成一个简短会话标题。\n只输出标题本身，不要解释，不要引号，不要句号。\n中文最多12个字，英文最多6个词。\n\n用户提示词：\n{}",
                    prompt.trim()
                )
            });
            let Ok(output) = manager
                .run_bridge(&current_dir().unwrap_or_default(), request)
                .await
            else {
                return;
            };
            let title = normalize_title(output.as_str().unwrap_or(""), &fallback);
            if title == fallback {
                return;
            }
            let state = manager.app.state::<AppState>();
            let mut store = state.store.lock().unwrap();
            if let Some(thread) = store.get_mut(&thread_id) {
                if thread.title == "新会话" || thread.title == fallback {
                    thread.title = title;
                    store.save();
                    let _ = manager.app.emit(EV_THREADS, json!({}));
                }
            }
        });
    }

    async fn run_prompt_bridge(
        &self,
        thread_id: &str,
        cwd: &str,
        request: Value,
        user_item_id: u64,
    ) -> Result<(), String> {
        let mut child = self.spawn_bridge(cwd)?;
        let stdin = child.stdin.take().ok_or("OpenCode+ bridge stdin 不可用")?;
        let stdin = Arc::new(tokio::sync::Mutex::new(stdin));
        write_line(&stdin, &request).await?;
        let stdout = child
            .stdout
            .take()
            .ok_or("OpenCode+ bridge stdout 不可用")?;
        self.running_children
            .lock()
            .unwrap()
            .insert(thread_id.to_string(), RunningBridge { child, stdin });
        let result = self
            .read_prompt_events(thread_id, user_item_id, stdout)
            .await;
        if let Some(mut bridge) = self.running_children.lock().unwrap().remove(thread_id) {
            if result.is_err() {
                kill_child(&mut bridge.child);
            } else {
                let _ = bridge.child.try_wait();
            }
        }
        self.clear_permissions(thread_id);
        result
    }

    async fn run_bridge(&self, cwd: &str, request: Value) -> Result<Value, String> {
        let mut child = self.spawn_bridge(cwd)?;
        write_request(&mut child, &request).await?;
        let output = child.wait_with_output().await.map_err(|e| e.to_string())?;
        parse_bridge_output(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    async fn read_prompt_events(
        &self,
        thread_id: &str,
        user_item_id: u64,
        stdout: tokio::process::ChildStdout,
    ) -> Result<(), String> {
        let mut lines = BufReader::new(stdout).lines();
        let mut part_items: HashMap<String, u64> = HashMap::new();
        while let Some(line) = lines.next_line().await.map_err(|e| e.to_string())? {
            let event: Value = serde_json::from_str(&line)
                .map_err(|e| format!("解析 OpenCode+ 事件失败：{e}；输出：{line}"))?;
            if event.get("ok").and_then(Value::as_bool) == Some(false) {
                return Err(event["error"]
                    .as_str()
                    .unwrap_or("OpenCode+ bridge 执行失败")
                    .into());
            }
            match event.get("type").and_then(Value::as_str) {
                Some("ready") => {
                    if let Some(session_id) = event.get("sessionId").and_then(Value::as_str) {
                        self.save_session_id(thread_id, session_id);
                    }
                }
                Some("part") => self.apply_part(thread_id, &event["part"], &mut part_items),
                Some("checkpoint") => self.save_checkpoint(thread_id, user_item_id, &event),
                Some("permission") => self.handle_permission(thread_id, &event["permission"]),
                Some("error") => {
                    return Err(event["error"]
                        .as_str()
                        .unwrap_or("OpenCode+ 会话失败")
                        .into())
                }
                Some("done") => return Ok(()),
                _ => {}
            }
        }
        Err("OpenCode+ bridge 意外退出".into())
    }

    fn spawn_bridge(&self, cwd: &str) -> Result<Child, String> {
        let (opencode_path, proxy) = {
            let state = self.app.state::<AppState>();
            let settings = state.settings.lock().unwrap();
            (
                settings.opencode_path.clone(),
                settings.opencode_proxy.clone(),
            )
        };
        let node = resolve_program_on_path("node")
            .ok_or("未找到 Node.js，OpenCode+ 需要 Node.js 运行官方 SDK")?;
        let bridge = bridge_path(&self.app)?;
        let mut command = Command::new(node);
        command
            .arg(bridge)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if !self.launch_env.is_empty() {
            crate::credential_roaming::isolate_borrowed_command(&mut command);
            command.envs(&self.launch_env);
        }
        if let Some(dir) = resolve_program_on_path(&opencode_path)
            .and_then(|path| path.parent().map(PathBuf::from))
        {
            let mut paths = vec![dir];
            paths.extend(std::env::split_paths(
                &std::env::var_os("PATH").unwrap_or_default(),
            ));
            if let Ok(path) = std::env::join_paths(paths) {
                command.env("PATH", path);
            }
        }
        apply_proxy_env(&mut command, &proxy);
        #[cfg(windows)]
        command.creation_flags(0x0800_0000);
        command
            .spawn()
            .map_err(|e| format!("启动 OpenCode+ Node bridge 失败：{e}"))
    }

    fn save_session_id(&self, thread_id: &str, session_id: &str) {
        let state = self.app.state::<AppState>();
        let mut store = state.store.lock().unwrap();
        if let Some(thread) = store.get_mut(thread_id) {
            thread.acp_session_id = Some(session_id.to_string());
        }
        store.save();
    }

    fn save_checkpoint(&self, thread_id: &str, user_item_id: u64, event: &Value) {
        let Some(session_id) = event.get("sessionId").and_then(Value::as_str) else {
            return;
        };
        let Some(position) = event.get("position").and_then(Value::as_str) else {
            return;
        };
        let state = self.app.state::<AppState>();
        let mut store = state.store.lock().unwrap();
        if let Some(thread) = store.get_mut(thread_id) {
            thread.record_provider_checkpoint(
                user_item_id,
                session_id.to_string(),
                position.to_string(),
            );
        }
        store.save();
    }

    fn apply_part(&self, thread_id: &str, part: &Value, part_items: &mut HashMap<String, u64>) {
        let Some(part_id) = part.get("id").and_then(Value::as_str) else {
            return;
        };
        let state = self.app.state::<AppState>();
        let mut store = state.store.lock().unwrap();
        let Some(thread) = store.get_mut(thread_id) else {
            return;
        };
        let existing = part_items.get(part_id).copied();
        let item = match part.get("type").and_then(Value::as_str) {
            Some("text") => text_item(
                existing.unwrap_or_else(|| thread.next_item_id()),
                part,
                false,
            ),
            Some("reasoning") => text_item(
                existing.unwrap_or_else(|| thread.next_item_id()),
                part,
                true,
            ),
            Some("tool") => Some(Item::Tool {
                id: existing.unwrap_or_else(|| thread.next_item_id()),
                ts: now_ms(),
                call: tool_call(part),
            }),
            _ => None,
        };
        let Some(item) = item else {
            return;
        };
        let item_id = item.id();
        if existing.is_some() {
            if let Some(slot) = thread
                .items
                .iter_mut()
                .find(|candidate| candidate.id() == item_id)
            {
                *slot = item.clone();
            }
        } else {
            part_items.insert(part_id.to_string(), item_id);
            thread.items.push(item.clone());
        }
        thread.updated_at = now_ms();
        let _ = self.emit_update(thread_id, &item);
        store.save();
    }

    fn handle_permission(&self, thread_id: &str, permission: &Value) {
        let Some(request_id) = permission.get("id").and_then(Value::as_str) else {
            return;
        };
        let request_key = format!("ocp-perm-{thread_id}-{request_id}");
        self.pending_permissions.lock().unwrap().insert(
            request_key.clone(),
            PendingPermission {
                thread_id: thread_id.to_string(),
                request_id: request_id.to_string(),
            },
        );
        let title = permission
            .get("permission")
            .and_then(Value::as_str)
            .unwrap_or("工具调用");
        let _ = self.app.emit(
            EV_PERMISSION,
            json!({
                "threadId": thread_id,
                "agentKind": "opencode",
                "requestKey": request_key,
                "toolCall": {
                    "title": title,
                    "kind": "other",
                    "rawInput": permission.get("metadata").cloned().unwrap_or(Value::Null)
                },
                "options": [
                    { "optionId": "once", "name": "允许一次", "kind": "allow_once" },
                    { "optionId": "always", "name": "始终允许", "kind": "allow_always" },
                    { "optionId": "reject", "name": "拒绝", "kind": "reject_once" }
                ]
            }),
        );
    }

    pub async fn respond_permission(
        &self,
        request_key: &str,
        option_id: &str,
    ) -> Result<(), String> {
        let permission = self
            .pending_permissions
            .lock()
            .unwrap()
            .remove(request_key)
            .ok_or("该权限请求已失效")?;
        let stdin = self
            .running_children
            .lock()
            .unwrap()
            .get(&permission.thread_id)
            .map(|bridge| bridge.stdin.clone())
            .ok_or("OpenCode+ 会话已结束")?;
        let reply = match option_id {
            "always" => "always",
            "reject" | "" => "reject",
            _ => "once",
        };
        write_line(
            &stdin,
            &json!({ "action": "permission", "requestId": permission.request_id, "reply": reply }),
        )
        .await?;
        let _ = self
            .app
            .emit(EV_PERMISSION_RESOLVED, json!({ "requestKey": request_key }));
        Ok(())
    }

    fn clear_permissions(&self, thread_id: &str) {
        let keys = {
            let mut pending = self.pending_permissions.lock().unwrap();
            let keys = pending
                .iter()
                .filter(|(_, permission)| permission.thread_id == thread_id)
                .map(|(key, _)| key.clone())
                .collect::<Vec<_>>();
            pending.retain(|_, permission| permission.thread_id != thread_id);
            keys
        };
        for request_key in keys {
            let _ = self
                .app
                .emit(EV_PERMISSION_RESOLVED, json!({ "requestKey": request_key }));
        }
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
        self.app
            .state::<AppState>()
            .sleep_inhibitor
            .set_running(thread_id, running);
        if running {
            self.running.lock().unwrap().insert(thread_id.to_string());
            self.turn_started
                .lock()
                .unwrap()
                .insert(thread_id.to_string(), Instant::now());
        } else {
            self.running.lock().unwrap().remove(thread_id);
        }
        let _ = self.app.emit(
            EV_TURN,
            json!({
                "threadId": thread_id, "running": running, "stopReason": stop_reason
            }),
        );
        let _ = self.app.emit(EV_THREADS, json!({}));
    }

    fn finish_turn(&self, thread_id: &str, stop_reason: &str) {
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
        let state = self.app.state::<AppState>();
        let mut store = state.store.lock().unwrap();
        if let Some(thread) = store.get_mut(thread_id) {
            let item = thread.push_turn(duration, None, stop_reason);
            let _ = self.emit_update(thread_id, &item);
        }
        store.save();
        drop(store);
        self.set_running(thread_id, false, Some(stop_reason));
    }

    fn emit_update(&self, thread_id: &str, item: &Item) -> Result<(), tauri::Error> {
        self.app.emit(
            EV_UPDATE,
            json!({
                "threadId": thread_id, "op": { "t": "upsert", "item": item }
            }),
        )
    }
}

fn prompt_attachment_part(image: PromptImage) -> Option<Value> {
    if image.mime_type.starts_with("image/") {
        let url = image
            .data
            .map(|data| format!("data:{};base64,{data}", image.mime_type))
            .or(image.uri)?;
        return Some(json!({
            "type": "file", "mime": image.mime_type, "filename": image.name, "url": url
        }));
    }
    let path = save_attachment_to_temp(&image)
        .or_else(|| image.uri.as_deref().and_then(file_uri_to_local_path))?;
    Some(json!({ "type": "text", "text": format!("Attached file: {path}") }))
}

impl Drop for OpenCodeSdkManager {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn bridge_path(app: &AppHandle) -> Result<PathBuf, String> {
    const BRIDGE: &[u8] = include_bytes!("../resources/opencode-bridge.cjs");
    let dir = crate::nova_data_dir(app).join("runtime");
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建 OpenCode+ 运行目录失败：{e}"))?;
    let path = dir.join("opencode-bridge.cjs");
    if std::fs::read(&path).ok().as_deref() != Some(BRIDGE) {
        std::fs::write(&path, BRIDGE).map_err(|e| format!("释放 OpenCode+ bridge 失败：{e}"))?;
    }
    Ok(path)
}

async fn write_request(child: &mut Child, request: &Value) -> Result<(), String> {
    let mut stdin = child.stdin.take().ok_or("OpenCode+ bridge stdin 不可用")?;
    stdin
        .write_all(request.to_string().as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    stdin.shutdown().await.map_err(|e| e.to_string())
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

fn parse_bridge_output(output: String) -> Result<Value, String> {
    let value: Value = serde_json::from_str(&output)
        .map_err(|e| format!("解析 OpenCode+ bridge 响应失败：{e}；输出：{output}"))?;
    if value.get("ok").and_then(Value::as_bool) == Some(true) {
        Ok(value.get("data").cloned().unwrap_or(Value::Null))
    } else {
        Err(value
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("OpenCode+ bridge 执行失败")
            .to_string())
    }
}

fn kill_child(child: &mut Child) {
    if let Some(pid) = child.id() {
        crate::acp::kill_process_tree(pid);
    }
    let _ = child.start_kill();
}

fn current_dir() -> Result<String, String> {
    std::env::current_dir()
        .map(|path| path.to_string_lossy().into_owned())
        .map_err(|e| format!("读取当前目录失败：{e}"))
}

fn split_model_variant(model: &str) -> Option<(&str, &str, Option<&str>)> {
    let (provider, rest) = model.split_once('/')?;
    if provider.is_empty() || rest.is_empty() {
        return None;
    }
    let (model_id, variant) = rest
        .rsplit_once("/variant/")
        .map(|(id, variant)| (id, Some(variant)))
        .unwrap_or((rest, None));
    (!model_id.is_empty() && variant.is_none_or(|value| !value.is_empty()))
        .then_some((provider, model_id, variant))
}

fn with_command(mut request: Value, text: &str) -> Value {
    let Some(command) = text.trim().strip_prefix('/') else {
        return request;
    };
    let (name, arguments) = command
        .split_once(char::is_whitespace)
        .unwrap_or((command, ""));
    if name.is_empty() {
        return request;
    }
    request["command"] = json!(name);
    request["arguments"] = json!(arguments.trim());
    request
}

fn normalize_title(output: &str, fallback: &str) -> String {
    let title = output
        .trim()
        .trim_matches(['"', '\'', '`'])
        .trim_end_matches(['。', '.', '！', '!', '？', '?'])
        .trim();
    if title.is_empty() {
        fallback.to_string()
    } else {
        title.chars().take(60).collect()
    }
}

fn provider_options(value: Value) -> Result<Value, String> {
    let providers = value
        .get("all")
        .and_then(Value::as_array)
        .or_else(|| value.as_array())
        .ok_or("OpenCode+ provider 响应格式无效")?;
    let mut options = Vec::new();
    for provider in providers {
        let Some(provider_id) = provider.get("id").and_then(Value::as_str) else {
            continue;
        };
        let provider_name = provider
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or(provider_id);
        if let Some(models) = provider.get("models").and_then(Value::as_object) {
            for (model_id, model) in models {
                let name = model
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or(model_id);
                let base_name = format!("{provider_name} / {name}");
                options.push(json!({
                    "value": format!("{provider_id}/{model_id}"),
                    "name": format!("{base_name} · Default"),
                    "_meta": { "codex.ai/supportsImages": supports_images(model) }
                }));
                for variant in model
                    .get("variants")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                {
                    options.push(json!({
                        "value": format!("{provider_id}/{model_id}/variant/{variant}"),
                        "name": format!("{base_name} · {}", variant_label(variant)),
                        "_meta": { "codex.ai/supportsImages": supports_images(model) }
                    }));
                }
            }
        }
    }
    Ok(json!({
        "configOptions": [{ "id": "model", "name": "Model", "options": options }],
        "modes": { "currentModeId": "build", "availableModes": [
            { "id": "build", "name": "Build" }, { "id": "plan", "name": "Plan" }
        ] }
    }))
}

fn supports_images(model: &Value) -> bool {
    model
        .pointer("/capabilities/input/image")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn variant_label(variant: &str) -> &str {
    match variant.to_ascii_lowercase().as_str() {
        "none" => "None",
        "minimal" => "Minimal",
        "low" => "Low",
        "medium" => "Medium",
        "high" => "High",
        "xhigh" | "extra-high" | "extra_high" => "XHigh",
        "max" => "Max",
        _ => variant,
    }
}

fn text_item(id: u64, part: &Value, thought: bool) -> Option<Item> {
    let text = part.get("text").and_then(Value::as_str)?.to_string();
    Some(if thought {
        Item::Thought {
            id,
            text,
            ts: now_ms(),
        }
    } else {
        Item::Assistant {
            id,
            text,
            ts: now_ms(),
        }
    })
}

fn tool_call(part: &Value) -> ToolCall {
    let state = part.get("state").unwrap_or(&Value::Null);
    let status = state
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("pending");
    let output = state.get("output").or_else(|| state.get("error")).cloned();
    let output_text = output.as_ref().map(|value| {
        value
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| value.to_string())
    });
    let tool = part.get("tool").and_then(Value::as_str).unwrap_or("Tool");
    let input = state.get("input").cloned();
    let path = input.as_ref().and_then(|value| {
        ["filePath", "file_path", "path"]
            .iter()
            .find_map(|key| value.get(key).and_then(Value::as_str))
    });
    let is_todo = matches!(
        tool.to_ascii_lowercase().as_str(),
        "todowrite" | "todo_write" | "todo"
    );
    let todo_text = is_todo.then(|| {
        input
            .as_ref()
            .and_then(|value| value.get("todos"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|todo| {
                let content = todo.get("content").and_then(Value::as_str)?;
                let marker = match todo.get("status").and_then(Value::as_str) {
                    Some("completed") => "x",
                    Some("in_progress" | "inProgress") => ">",
                    Some("cancelled") => "-",
                    _ => " ",
                };
                Some(format!("[{marker}] {content}"))
            })
            .collect::<Vec<_>>()
            .join("\n")
    });
    let detail = input
        .as_ref()
        .and_then(|value| match tool.to_ascii_lowercase().as_str() {
            "bash" | "shell" => value.get("command"),
            "grep" | "search" => value.get("pattern").or_else(|| value.get("query")),
            "glob" => value.get("pattern"),
            _ => None,
        })
        .and_then(Value::as_str)
        .map(compact_tool_detail)
        .filter(|value| !value.is_empty());
    ToolCall {
        tool_call_id: part
            .get("callID")
            .or_else(|| part.get("id"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        title: if is_todo {
            "Todo list".into()
        } else if let Some(path) = path {
            format!("{tool} {path}")
        } else if let Some(detail) = detail {
            format!("{tool} {detail}")
        } else {
            tool.to_string()
        },
        kind: match tool.to_ascii_lowercase().as_str() {
            "read" => "read",
            "edit" | "write" | "patch" | "apply_patch" => "edit",
            "grep" | "glob" | "search" => "search",
            "bash" | "shell" => "execute",
            _ if is_todo => "think",
            _ => "other",
        }
        .into(),
        status: match status {
            "completed" => "completed",
            "error" => "failed",
            other => other,
        }
        .into(),
        content: output_text
            .or(todo_text)
            .map(|text| {
                vec![json!({ "type": "content", "content": { "type": "text", "text": text } })]
            })
            .unwrap_or_default(),
        locations: path
            .map(|path| vec![json!({ "path": path })])
            .unwrap_or_default(),
        raw_input: input,
        raw_output: output,
    }
}

fn compact_tool_detail(value: &str) -> String {
    let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = value.chars();
    let detail = chars.by_ref().take(160).collect::<String>();
    if chars.next().is_some() {
        format!("{detail}…")
    } else {
        detail
    }
}

fn derive_title(text: &str, has_images: bool) -> String {
    let title: String = text
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .chars()
        .take(40)
        .collect();
    if !title.is_empty() {
        title
    } else if has_images {
        "[图片]".into()
    } else {
        "新会话".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_fallback_uses_first_prompt_line_or_image() {
        assert_eq!(
            derive_title("  修复标题生成\n更多内容", false),
            "修复标题生成"
        );
        assert_eq!(derive_title("", true), "[图片]");
        assert_eq!(derive_title("", false), "新会话");
    }

    #[test]
    fn ordinary_attachments_become_readable_paths() {
        let part = prompt_attachment_part(PromptImage {
            name: "质量公式分析视图_backup.xlsx".into(),
            mime_type: "application/octet-stream".into(),
            data: None,
            uri: Some("file:///C:/Users/1/Desktop/%E8%B4%A8%E9%87%8F.xlsx".into()),
            size: None,
        })
        .unwrap();
        assert_eq!(part["type"], "text");
        assert!(part["text"].as_str().unwrap().contains("质量.xlsx"));

        let image = prompt_attachment_part(PromptImage {
            name: "chart.png".into(),
            mime_type: "image/png".into(),
            data: Some("base64".into()),
            uri: None,
            size: None,
        })
        .unwrap();
        assert_eq!(image["type"], "file");
        assert_eq!(image["mime"], "image/png");
    }

    #[test]
    fn splits_sdk_model_identifier() {
        assert_eq!(
            split_model_variant("anthropic/claude-sonnet-4/variant/high"),
            Some(("anthropic", "claude-sonnet-4", Some("high")))
        );
        assert_eq!(
            split_model_variant("openrouter/anthropic/claude-sonnet-4/variant/xhigh"),
            Some(("openrouter", "anthropic/claude-sonnet-4", Some("xhigh")))
        );
        assert_eq!(split_model_variant("claude-sonnet-4"), None);
    }

    #[test]
    fn maps_provider_models_to_nova_options() {
        let value = provider_options(json!({
            "all": [{ "id": "openai", "name": "OpenAI", "models": {
                "gpt-5": {
                    "name": "GPT-5",
                    "variants": ["low", "high"],
                    "capabilities": { "input": { "image": true } }
                }
            } }]
        }))
        .unwrap();
        assert_eq!(
            value["configOptions"][0]["options"][0]["value"],
            "openai/gpt-5"
        );
        assert_eq!(
            value["configOptions"][0]["options"][2]["value"],
            "openai/gpt-5/variant/high"
        );
        assert_eq!(
            value["configOptions"][0]["options"][0]["_meta"]["codex.ai/supportsImages"],
            true
        );
    }

    #[test]
    fn recognizes_sdk_slash_commands() {
        let request = with_command(json!({ "action": "prompt" }), "/review staged changes");
        assert_eq!(request["command"], "review");
        assert_eq!(request["arguments"], "staged changes");
        assert!(
            with_command(json!({ "action": "prompt" }), "explain /review")["command"].is_null()
        );
    }

    #[test]
    fn maps_read_and_todo_tools_for_display() {
        let read = tool_call(&json!({
            "id": "part", "callID": "call", "type": "tool", "tool": "read",
            "state": { "status": "running", "input": { "filePath": "src/main.rs" } }
        }));
        assert_eq!(read.title, "read src/main.rs");
        assert_eq!(read.kind, "read");
        assert_eq!(read.locations[0]["path"], "src/main.rs");

        let todo = tool_call(&json!({
            "id": "todo", "type": "tool", "tool": "todowrite",
            "state": { "status": "completed", "input": { "todos": [
                { "content": "Fix streaming", "status": "in_progress" }
            ] } }
        }));
        assert_eq!(todo.title, "Todo list");
        assert_eq!(todo.kind, "think");
        assert!(todo.raw_input.unwrap()["todos"].is_array());

        let bash = tool_call(&json!({
            "id": "bash", "type": "tool", "tool": "bash",
            "state": { "status": "running", "input": { "command": "python inspect_excel.py report.xlsx" } }
        }));
        assert_eq!(bash.title, "bash python inspect_excel.py report.xlsx");
        assert_eq!(bash.kind, "execute");
    }
}
