use crate::acp::{
    apply_proxy_env, resolve_program_on_path, EV_LOG, EV_OPTIONS, EV_THREADS, EV_TURN, EV_UPDATE,
};
use crate::codex_radar;
use crate::model_cache;
use crate::sdk_adapters::SdkAdapter;
use crate::threads::{
    file_uri_to_local_path, now_ms, save_attachment_to_temp, AgentKind, CodexUsageSnapshot, Item,
    PromptImage, ToolCall,
};
use crate::{nova_data_dir, AppState};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};

fn is_codex_model_resume_warning(value: &Value) -> bool {
    if value.get("type").and_then(Value::as_str) != Some("error") {
        return false;
    }
    value
        .get("message")
        .and_then(Value::as_str)
        .is_some_and(|message| {
            message.starts_with("This session was recorded with model `")
                && message.contains("` but is resuming with `")
                && message.contains("`. Consider switching back to `")
                && message.ends_with("` as it may affect Codex performance.")
        })
}

struct RunningBridge {
    stdin: Arc<tokio::sync::Mutex<ChildStdin>>,
    pid: Option<u32>,
}

struct IdleBridge {
    child: Child,
    stdin: Arc<tokio::sync::Mutex<ChildStdin>>,
    stdout: BufReader<tokio::process::ChildStdout>,
    stderr: Arc<Mutex<Vec<String>>>,
}

pub struct SdkManager {
    app: AppHandle,
    adapter: Arc<dyn SdkAdapter>,
    launch_env: HashMap<String, String>,
    running_children: Mutex<HashMap<String, RunningBridge>>,
    idle_children: Mutex<HashMap<String, IdleBridge>>,
    running: Mutex<HashSet<String>>,
    turn_started: Mutex<HashMap<String, Instant>>,
    pending_permissions: Mutex<HashMap<String, (String, String)>>,
    model_options: Mutex<Option<Value>>,
    model_options_refreshing: AtomicBool,
    model_options_revalidated: AtomicBool,
    /// 仅 Alkaid 使用：nova-server 下发的配置保存在内存，请求 bridge 时随首包传入。
    alkaid_server_config: Mutex<Option<Value>>,
    alkaid_config_generation: AtomicU64,
    next_run_epoch: AtomicU64,
    run_epochs: Mutex<HashMap<String, u64>>,
}

impl SdkManager {
    pub fn new<A: SdkAdapter + 'static>(app: AppHandle, adapter: A) -> Arc<Self> {
        Self::new_with_env(app, adapter, HashMap::new())
    }

    pub fn new_with_env<A: SdkAdapter + 'static>(
        app: AppHandle,
        adapter: A,
        launch_env: HashMap<String, String>,
    ) -> Arc<Self> {
        Arc::new(Self {
            app,
            adapter: Arc::new(adapter),
            launch_env,
            running_children: Mutex::new(HashMap::new()),
            idle_children: Mutex::new(HashMap::new()),
            running: Mutex::new(HashSet::new()),
            turn_started: Mutex::new(HashMap::new()),
            pending_permissions: Mutex::new(HashMap::new()),
            model_options: Mutex::new(None),
            model_options_refreshing: AtomicBool::new(false),
            model_options_revalidated: AtomicBool::new(false),
            alkaid_server_config: Mutex::new(None),
            alkaid_config_generation: AtomicU64::new(0),
            next_run_epoch: AtomicU64::new(1),
            run_epochs: Mutex::new(HashMap::new()),
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
            mut reasoning_effort,
            context,
            session_id,
            native_restore,
            user_item_id,
            cached_auto_model,
        ) = {
            let state = self.app.state::<AppState>();
            let mut store = state.store.lock().unwrap();
            let Some(thread) = store.get_mut(&thread_id) else {
                return;
            };
            let context = thread.take_prompt_context(self.adapter.label());
            let native_restore = thread.pending_native_restore.take();
            let session_id = native_restore
                .as_ref()
                .map(|restore| restore.session_id.clone())
                .or_else(|| thread.acp_session_id.clone());
            if self.adapter.uses_codex_model_routing() && session_id.is_none() {
                thread.codex_usage_snapshot = Some(CodexUsageSnapshot::default());
            }
            let item = thread.push_user(text.clone(), images.clone());
            let user_item_id = item.id();
            if self.adapter.generates_title() && thread.title == "新会话" {
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
                session_id,
                native_restore,
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
                &self.adapter.agent_kind(),
                thread_id.clone(),
                prompt,
                fallback,
            );
        }
        let run_epoch = self.next_run_epoch.fetch_add(1, Ordering::Relaxed);
        self.run_epochs
            .lock()
            .unwrap()
            .insert(thread_id.clone(), run_epoch);
        self.set_running(&thread_id, true, None);
        if self.adapter.uses_codex_model_routing()
            && model.as_deref().is_some_and(codex_radar::is_auto_model)
        {
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
                let manager = self.app.state::<AppState>().codex.clone();
                let options_result = manager.ensure_model_options().await;
                if !self.is_current_run(&thread_id, run_epoch) {
                    return;
                }
                let options = match options_result {
                    Ok(options) => options,
                    Err(error) => {
                        self.push_system(&thread_id, format!("Auto 路由失败：{error}"), "error");
                        self.finish_turn_if_current(&thread_id, run_epoch, "error", None);
                        return;
                    }
                };
                let resolved = codex_radar::resolve_auto_model(&selection, &options, false).await;
                if !self.is_current_run(&thread_id, run_epoch) {
                    return;
                }
                match resolved {
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
                        self.finish_turn_if_current(&thread_id, run_epoch, "error", None);
                        return;
                    }
                }
            }
        }
        if self.adapter.uses_codex_model_routing() {
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
                            thread.clear_auto_route();
                            thread.model = None;
                            thread.reasoning_effort = None;
                        }
                        store.save();
                    }
                }
            }
        }
        let mut parts = Vec::new();
        if native_restore.is_none() {
            if let Some(context) = context.as_ref() {
                parts.push(json!({ "type": "text", "text": context }));
            }
        }
        parts.extend(prompt_parts(self.adapter.as_ref(), &text, &images));
        let mut request = json!({
            "action": "prompt",
            "threadId": thread_id,
            "cwd": cwd,
            "sessionId": session_id,
            "restoreAt": native_restore.as_ref().map(|restore| &restore.position),
            "model": model,
            "mode": mode,
            "reasoningEffort": reasoning_effort,
            "parts": parts
        });
        let mut outcome = self
            .run_prompt_bridge(&thread_id, &cwd, request.clone(), user_item_id, run_epoch)
            .await;
        if !self.is_current_run(&thread_id, run_epoch) {
            return;
        }
        if outcome.is_err() && native_restore.is_some() {
            self.forget_session_of_thread(&thread_id);
            self.clear_session_id(&thread_id);
            let mut fallback_parts = Vec::new();
            if let Some(context) = context {
                fallback_parts.push(json!({ "type": "text", "text": context }));
            }
            fallback_parts.extend(request["parts"].as_array().cloned().unwrap_or_default());
            request["sessionId"] = Value::Null;
            request["restoreAt"] = Value::Null;
            request["parts"] = Value::Array(fallback_parts);
            outcome = self
                .run_prompt_bridge(&thread_id, &cwd, request, user_item_id, run_epoch)
                .await;
        }
        if !self.is_current_run(&thread_id, run_epoch) {
            return;
        }
        let succeeded = outcome.is_ok();
        if let Err(error) = outcome {
            self.push_system(
                &thread_id,
                format!("{} 请求失败：{error}", self.adapter.label()),
                "error",
            );
        }
        self.finish_turn_if_current(
            &thread_id,
            run_epoch,
            if succeeded { "end_turn" } else { "error" },
            None,
        );
    }

    pub async fn cancel(&self, thread_id: &str) {
        if self.is_running(thread_id) {
            self.push_system(thread_id, "已停止当前任务。".into(), "warn");
        }
        let bridge = self
            .running_children
            .lock()
            .unwrap()
            .get(thread_id)
            .map(|bridge| (bridge.stdin.clone(), bridge.pid));
        let target_pid = bridge.as_ref().and_then(|(_, pid)| *pid);
        if let Some((stdin, _)) = bridge {
            let _ = write_line(&stdin, &json!({ "action": "cancel" })).await;
            for _ in 0..self.adapter.cancel_grace_attempts() {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                let target_is_running = self
                    .running_children
                    .lock()
                    .unwrap()
                    .get(thread_id)
                    .is_some_and(|bridge| bridge.pid == target_pid);
                if !target_is_running {
                    break;
                }
            }
        }
        let bridge = {
            let mut running = self.running_children.lock().unwrap();
            running
                .get(thread_id)
                .is_some_and(|bridge| bridge.pid == target_pid)
                .then(|| running.remove(thread_id))
                .flatten()
        };
        if let Some(bridge) = bridge {
            if let Some(pid) = bridge.pid {
                crate::acp::kill_process_tree(pid);
            }
        }
        self.finish_turn(thread_id, "cancelled", None);
    }

    /// 支持原生 steer 的 SDK 直接把用户消息排入当前 Agent run；其他 SDK
    /// （如 Cursor：每轮 Agent.create + slim memory）仍静默结束当前流，再开新 turn。
    pub async fn steer_prompt(
        self: &Arc<Self>,
        thread_id: String,
        text: String,
        images: Vec<PromptImage>,
    ) {
        if self.is_running(&thread_id) && self.adapter.supports_native_steer() {
            self.native_steer_prompt(&thread_id, text, images).await;
            return;
        }
        if self.is_running(&thread_id) {
            self.interrupt_for_steer(&thread_id).await;
        }
        self.clone().run_prompt(thread_id, text, images).await;
    }

    async fn native_steer_prompt(
        self: &Arc<Self>,
        thread_id: &str,
        text: String,
        images: Vec<PromptImage>,
    ) {
        // set_running 早于 bridge 注册，极快的补充提示可能命中这个短窗口；稍候 bridge 就绪。
        let mut stdin = None;
        for _ in 0..20 {
            stdin = self
                .running_children
                .lock()
                .unwrap()
                .get(thread_id)
                .map(|bridge| bridge.stdin.clone());
            if stdin.is_some() || !self.is_running(thread_id) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        let Some(stdin) = stdin else {
            if !self.is_running(thread_id) {
                self.clone()
                    .run_prompt(thread_id.to_string(), text, images)
                    .await;
            } else {
                self.push_system(
                    thread_id,
                    "Vega 引导失败：运行通道尚未就绪。".into(),
                    "error",
                );
            }
            return;
        };

        // 先落 UI transcript，保证 bridge 注入后产生的新输出一定排在引导消息之后。
        {
            let state = self.app.state::<AppState>();
            let mut store = state.store.lock().unwrap();
            let Some(thread) = store.get_mut(thread_id) else {
                return;
            };
            let item = thread.push_user(text.clone(), images.clone());
            let _ = self.emit_update(thread_id, &item);
            store.save();
        }

        let parts = prompt_parts(self.adapter.as_ref(), &text, &images);
        if let Err(error) = write_line(&stdin, &json!({ "action": "steer", "parts": parts })).await
        {
            self.push_system(thread_id, format!("Vega 引导发送失败：{error}"), "error");
        }
    }

    async fn interrupt_for_steer(&self, thread_id: &str) {
        // 先让旧轮次失效。即使旧 bridge 随后返回迟到事件，也不能结束新轮次。
        self.run_epochs.lock().unwrap().remove(thread_id);
        let bridge = self
            .running_children
            .lock()
            .unwrap()
            .get(thread_id)
            .map(|bridge| (bridge.stdin.clone(), bridge.pid));
        if let Some((stdin, pid)) = bridge {
            let _ = write_line(&stdin, &json!({ "action": "cancel" })).await;
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            if let Some(pid) = pid {
                crate::acp::kill_process_tree(pid);
            }
            self.running_children.lock().unwrap().remove(thread_id);
        }
        self.running.lock().unwrap().remove(thread_id);

        // 工具卡片不能永远停留在进行中，但不插入 cancelled turn 或系统提示。
        let state = self.app.state::<AppState>();
        let mut store = state.store.lock().unwrap();
        if let Some(thread) = store.get_mut(thread_id) {
            for item in complete_pending_tools(thread) {
                let _ = self.emit_update(thread_id, &item);
            }
        }
        store.save();
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

    pub async fn fork_session(
        &self,
        cwd: &str,
        session_id: &str,
        retained_turns: usize,
    ) -> Result<String, String> {
        let value = self
            .run_bridge(
                cwd,
                json!({
                    "action": "fork",
                    "cwd": cwd,
                    "sessionId": session_id,
                    "retainedTurns": retained_turns,
                }),
            )
            .await?;
        value
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| "Codex fork 未返回新会话 ID".into())
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

    pub fn seed_model_options(&self, value: Value) {
        *self.model_options.lock().unwrap() = Some(value);
    }

    /// 应用 nova-server 定向下发的 Alkaid 配置。配置只驻留内存；当前运行轮次不打断，
    /// 后续 bridge 首包携带它并由 JS 侧以本地 config.jsonc 覆盖合并。
    pub fn set_alkaid_server_config(self: &Arc<Self>, config: Option<Value>) {
        if self.adapter.agent_kind() != AgentKind::Alkaid {
            return;
        }
        {
            let mut current = self.alkaid_server_config.lock().unwrap();
            if *current == config {
                return;
            }
            *current = config;
        }
        self.alkaid_config_generation.fetch_add(1, Ordering::SeqCst);
        *self.model_options.lock().unwrap() = None;
        self.model_options_revalidated
            .store(false, Ordering::SeqCst);
        let _ = self.app.emit(
            EV_OPTIONS,
            json!({
                "agentKind": AgentKind::Alkaid.as_str(),
                "options": self.empty_model_options(),
            }),
        );
        for mut bridge in std::mem::take(&mut *self.idle_children.lock().unwrap()).into_values() {
            kill_child(&mut bridge.child);
        }
        // 若旧配置的模型探测仍在进行，等它退出 refreshing 闸门后再用新配置重拉。
        let manager = self.clone();
        tauri::async_runtime::spawn(async move {
            while manager.model_options_refreshing.load(Ordering::SeqCst) {
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
            manager.spawn_revalidate_model_options();
        });
    }

    fn with_alkaid_server_config(&self, mut request: Value) -> Value {
        if self.adapter.agent_kind() == AgentKind::Alkaid {
            if let Some(config) = self.alkaid_server_config.lock().unwrap().clone() {
                request["alkaidServerConfig"] = config;
            }
        }
        request
    }

    /// 返回当前缓存的模型列表，供同步的远程快照构建逻辑使用。
    pub fn get_model_options(&self) -> Option<Value> {
        self.model_options.lock().unwrap().clone()
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

    pub async fn ensure_model_options(self: &Arc<Self>) -> Result<Value, String> {
        if let Some(value) = self.model_options.lock().unwrap().clone() {
            self.spawn_revalidate_model_options();
            return Ok(value);
        }
        self.spawn_revalidate_model_options();
        Ok(self.empty_model_options())
    }

    fn empty_model_options(&self) -> Value {
        self.adapter.empty_model_options()
    }

    async fn refresh_model_options(&self) -> Result<Value, String> {
        let generation = self.alkaid_config_generation.load(Ordering::SeqCst);
        let cwd = std::env::current_dir()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let value = self
            .run_bridge(&cwd, json!({ "action": "models", "cwd": cwd }))
            .await?;
        if generation != self.alkaid_config_generation.load(Ordering::SeqCst) {
            return Err("Vega 配置已更新，丢弃旧模型列表".into());
        }
        *self.model_options.lock().unwrap() = Some(value.clone());
        let kind = self.adapter.agent_kind();
        // 服务端配置只允许驻留内存；启用时不把合并后的模型列表写入本地缓存。
        if kind != AgentKind::Alkaid || self.alkaid_server_config.lock().unwrap().is_none() {
            model_cache::save(&crate::nova_data_dir(&self.app), kind.as_str(), &value);
        }
        self.model_options_revalidated.store(true, Ordering::SeqCst);
        let _ = self.app.emit(
            EV_OPTIONS,
            json!({ "agentKind": kind.as_str(), "options": value }),
        );
        Ok(value)
    }

    pub fn generate_title_async(
        self: &Arc<Self>,
        thread_id: String,
        prompt: String,
        fallback: String,
        model: String,
    ) {
        if !self.adapter.generates_title() {
            return;
        }
        let manager = self.clone();
        tauri::async_runtime::spawn(async move {
            let cwd = std::env::current_dir()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let model = if manager.adapter.uses_codex_model_routing() {
                split_codex_effort(&model)
                    .map(|(model, _)| model)
                    .unwrap_or(&model)
            } else {
                &model
            };
            let request = json!({
                "action": "title",
                "cwd": cwd,
                "model": model,
                "prompt": format!(
                    "请为下面用户第一次提示词生成一个简短会话标题。\n只输出标题本身，不要解释，不要引号，不要句号。\n中文最多12个字，英文最多6个词。\n\n用户提示词：\n{}",
                    prompt.trim()
                )
            });
            let Ok(output) = manager.run_bridge(&cwd, request).await else {
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

    async fn run_bridge(&self, cwd: &str, request: Value) -> Result<Value, String> {
        let request = self.with_alkaid_server_config(request);
        let mut child = self.spawn_bridge(cwd)?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| format!("{} bridge stdin 不可用", self.adapter.label()))?;
        stdin
            .write_all(format!("{request}\n").as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        drop(stdin);
        let output = child.wait_with_output().await.map_err(|e| e.to_string())?;
        parse_bridge_output(
            &String::from_utf8_lossy(&output.stdout),
            self.adapter.label(),
        )
    }

    async fn run_prompt_bridge(
        &self,
        thread_id: &str,
        cwd: &str,
        request: Value,
        user_item_id: u64,
        run_epoch: u64,
    ) -> Result<(), String> {
        let request = self.with_alkaid_server_config(request);
        let mut bridge = self
            .adapter
            .keeps_bridge_alive()
            .then(|| self.idle_children.lock().unwrap().remove(thread_id))
            .flatten()
            .map(Ok)
            .unwrap_or_else(|| self.spawn_idle_bridge(cwd))?;
        let pid = bridge.child.id();
        self.running_children.lock().unwrap().insert(
            thread_id.to_string(),
            RunningBridge {
                stdin: bridge.stdin.clone(),
                pid,
            },
        );
        if let Err(error) = write_line(&bridge.stdin, &request).await {
            self.running_children.lock().unwrap().remove(thread_id);
            kill_child(&mut bridge.child);
            return Err(error);
        }
        let result = self
            .read_events(thread_id, user_item_id, run_epoch, &mut bridge.stdout)
            .await;
        {
            let mut running = self.running_children.lock().unwrap();
            if running
                .get(thread_id)
                .is_some_and(|bridge| bridge.pid == pid)
            {
                running.remove(thread_id);
            }
        }
        let result = result.map_err(|error| {
            let status = bridge
                .child
                .try_wait()
                .ok()
                .flatten()
                .map(|status| status.to_string());
            let stderr = bridge.stderr.lock().unwrap().join("\n");
            if status.is_none() && stderr.is_empty() {
                return error;
            }
            format!(
                "{error}{}{}",
                status
                    .map(|value| format!("；退出状态：{value}"))
                    .unwrap_or_default(),
                (!stderr.is_empty())
                    .then(|| format!("；stderr：{stderr}"))
                    .unwrap_or_default()
            )
        });
        let reusable = self.adapter.keeps_bridge_alive()
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
            .ok_or_else(|| format!("{} bridge stdin 不可用", self.adapter.label()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| format!("{} bridge stdout 不可用", self.adapter.label()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| format!("{} bridge stderr 不可用", self.adapter.label()))?;
        let stderr_lines = Arc::new(Mutex::new(Vec::new()));
        let captured = stderr_lines.clone();
        tauri::async_runtime::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if line.contains("ExperimentalWarning: SQLite is an experimental feature")
                    || line.contains("node --trace-warnings")
                {
                    continue;
                }
                let mut captured = captured.lock().unwrap();
                captured.push(line);
                if captured.len() > 20 {
                    captured.remove(0);
                }
            }
        });
        Ok(IdleBridge {
            child,
            stdin: Arc::new(tokio::sync::Mutex::new(stdin)),
            stdout: BufReader::new(stdout),
            stderr: stderr_lines,
        })
    }

    async fn read_events(
        &self,
        thread_id: &str,
        user_item_id: u64,
        run_epoch: u64,
        stdout: &mut BufReader<tokio::process::ChildStdout>,
    ) -> Result<(), String> {
        let mut lines = stdout.lines();
        let mut item_ids = HashMap::new();
        while let Some(line) = lines.next_line().await.map_err(|e| e.to_string())? {
            let event: Value = serde_json::from_str(&line).map_err(|e| {
                format!("解析 {} 事件失败：{e}；输出：{line}", self.adapter.label())
            })?;
            if event.get("ok").and_then(Value::as_bool) == Some(false) {
                return Err(event["error"]
                    .as_str()
                    .unwrap_or("SDK bridge 执行失败")
                    .into());
            }
            let event_type = event.get("type").and_then(Value::as_str);
            // 静默换轮期间仍接收 session id，确保新轮能够续接原上下文。
            if event_type == Some("ready") {
                if let Some(session_id) = event.get("sessionId").and_then(Value::as_str) {
                    self.save_session_id(thread_id, session_id);
                }
            }
            if !self.is_current_run(thread_id, run_epoch) {
                return Ok(());
            }
            match event_type {
                Some("ready") => {}
                Some("timing") => {
                    let phase = event
                        .get("phase")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown");
                    let elapsed_ms = event.get("elapsedMs").and_then(Value::as_u64).unwrap_or(0);
                    let cancelled_runs = event
                        .get("cancelledRuns")
                        .and_then(Value::as_u64)
                        .map(|count| format!(" cancelled_runs={count}"))
                        .unwrap_or_default();
                    let _ = self.app.emit(
                        EV_LOG,
                        format!(
                            "[{}][timing] {phase} {elapsed_ms}ms{cancelled_runs}",
                            self.adapter.label()
                        ),
                    );
                }
                Some("item") => self.apply_item(thread_id, &event["item"], &mut item_ids),
                Some("plan") => self.apply_plan(thread_id, &event["plan"]),
                Some("checkpoint") => self.save_checkpoint(thread_id, user_item_id, &event),
                Some("permission") => self.emit_permission(thread_id, &event["permission"]),
                Some("done") => {
                    let usage = event.get("usage").cloned();
                    let stop_reason = if self.adapter.done_is_cancelled(&event) {
                        "cancelled"
                    } else {
                        "end_turn"
                    };
                    self.finish_turn_if_current(thread_id, run_epoch, stop_reason, usage);
                    return Ok(());
                }
                _ => {}
            }
        }
        Err(format!("{} bridge 意外退出", self.adapter.label()))
    }

    fn spawn_bridge(&self, cwd: &str) -> Result<Child, String> {
        let launch = {
            let state = self.app.state::<AppState>();
            let settings = state.settings.lock().unwrap();
            self.adapter.launch_config(&settings)
        };
        let program = resolve_program_on_path(&launch.program)
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or(launch.program);
        let node = resolve_program_on_path("node").ok_or_else(|| {
            format!(
                "未找到 Node.js，{} 需要 Node.js 运行官方 SDK",
                self.adapter.label()
            )
        })?;
        let bridge = bridge_path(&self.app, self.adapter.as_ref())?;
        let mut command = Command::new(node);
        command
            .arg(bridge)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env(launch.path_env, &program)
            // Node bridges also persist app-owned state. Pin them to the same profile-specific
            // root as Rust so debug builds never fall back to the release ~/.nova directory.
            .env("NOVA_DATA_DIR", nova_data_dir(&self.app));
        if !self.launch_env.is_empty() {
            crate::credential_roaming::isolate_borrowed_command(&mut command);
            command.envs(&self.launch_env);
        }
        apply_proxy_env(&mut command, &launch.proxy);
        if self.launch_env.is_empty() {
            if let Some((name, value)) = launch.api_key {
                command.env(name, value);
            }
        }
        // SDK 后端统一由这个 Node bridge 启动；必须在 bridge 进程环境中注入 shim，
        // 才能覆盖 Cursor SDK 等后续创建的 cmd / powershell / pwsh 孙进程。
        #[cfg(windows)]
        if self.app.state::<AppState>().windows_shell_shim_enabled {
            crate::windows_shell_shim::apply(&self.app, &mut command, &self.launch_env)
                .map_err(|e| format!("应用 Windows shell shim 失败：{e}"))?;
        }
        #[cfg(windows)]
        command.creation_flags(0x0800_0000);
        command
            .spawn()
            .map_err(|e| format!("启动 {} Node bridge 失败：{e}", self.adapter.label()))
    }

    fn save_session_id(&self, thread_id: &str, session_id: &str) {
        let state = self.app.state::<AppState>();
        let mut store = state.store.lock().unwrap();
        if let Some(thread) = store.get_mut(thread_id) {
            if self.adapter.uses_codex_model_routing() {
                if let Some(snapshot) = thread.codex_usage_snapshot.as_mut() {
                    if snapshot
                        .session_id
                        .as_deref()
                        .is_some_and(|id| id != session_id)
                    {
                        *snapshot = CodexUsageSnapshot::default();
                    }
                    snapshot.session_id = Some(session_id.to_string());
                }
            }
            thread.acp_session_id = Some(session_id.to_string());
        }
        store.save();
    }

    fn clear_session_id(&self, thread_id: &str) {
        let state = self.app.state::<AppState>();
        let mut store = state.store.lock().unwrap();
        if let Some(thread) = store.get_mut(thread_id) {
            thread.acp_session_id = None;
            if self.adapter.uses_codex_model_routing() {
                thread.codex_usage_snapshot = None;
            }
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

    fn apply_item(&self, thread_id: &str, value: &Value, ids: &mut HashMap<String, u64>) {
        if self.adapter.uses_codex_model_routing() && is_codex_model_resume_warning(value) {
            return;
        }
        let Some(remote_id) = value.get("id").and_then(Value::as_str) else {
            return;
        };
        let plan = self
            .adapter
            .uses_codex_model_routing()
            .then(|| codex_todo_plan(value))
            .flatten();
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
                call: self
                    .adapter
                    .map_tool_call(value)
                    .unwrap_or_else(|| tool_call(value)),
            }),
            Some(_) => Some(Item::Tool {
                id,
                ts: now_ms(),
                call: self
                    .adapter
                    .map_tool_call(value)
                    .unwrap_or_else(|| tool_call(value)),
            }),
            None => None,
        };
        let Some(item) = item else {
            return;
        };
        let mut update = Some(json!({ "t": "upsert", "item": item }));
        if existing.is_some() {
            if let Some(slot) = thread
                .items
                .iter_mut()
                .find(|candidate| candidate.id() == id)
            {
                update = if self.adapter.uses_text_deltas() {
                    match text_snapshot_change(slot, &item) {
                        TextSnapshotChange::Delta(delta) => {
                            Some(json!({ "t": "delta", "itemId": id, "text": delta }))
                        }
                        TextSnapshotChange::Unchanged => None,
                        TextSnapshotChange::Replace => Some(json!({ "t": "upsert", "item": item })),
                    }
                } else {
                    Some(json!({ "t": "upsert", "item": item }))
                };
                *slot = item.clone();
            }
        } else {
            ids.insert(remote_id.into(), id);
            thread.items.push(item.clone());
        }
        if let Some(plan) = &plan {
            thread.plan = Some(plan.clone());
        }
        thread.updated_at = now_ms();
        if let Some(op) = update {
            let _ = self.emit_op(thread_id, op);
        }
        if let Some(plan) = plan {
            let _ = self.emit_op(thread_id, json!({ "t": "plan", "plan": plan }));
        }
        store.save();
    }

    fn apply_plan(&self, thread_id: &str, plan: &Value) {
        let Some(entries) = plan.as_array() else {
            return;
        };
        let plan = Value::Array(entries.clone());
        let state = self.app.state::<AppState>();
        let mut store = state.store.lock().unwrap();
        let Some(thread) = store.get_mut(thread_id) else {
            return;
        };
        thread.plan = Some(plan.clone());
        thread.updated_at = now_ms();
        let _ = self.emit_op(thread_id, json!({ "t": "plan", "plan": plan }));
        store.save();
    }

    fn emit_permission(&self, thread_id: &str, permission: &Value) {
        let Some(request_id) = permission.get("id").and_then(Value::as_str) else {
            return;
        };
        let prefix = self.adapter.permission_prefix();
        let agent_kind = self.adapter.agent_kind();
        let request_key = format!("{prefix}-perm-{thread_id}-{request_id}");
        self.pending_permissions.lock().unwrap().insert(
            request_key.clone(),
            (thread_id.to_string(), request_id.to_string()),
        );
        let _ = self.app.emit(crate::acp::EV_PERMISSION, json!({
            "threadId": thread_id,
            "agentKind": agent_kind.as_str(),
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
            .ok_or_else(|| format!("{} 会话已结束", self.adapter.label()))?;
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
        self.app
            .state::<AppState>()
            .sleep_inhibitor
            .set_running(thread_id, running);
        if running {
            self.running.lock().unwrap().insert(thread_id.into());
            self.turn_started
                .lock()
                .unwrap()
                .entry(thread_id.into())
                .or_insert_with(Instant::now);
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
        self.run_epochs.lock().unwrap().remove(thread_id);
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
            let (usage, snapshot) = self.adapter.normalize_usage(
                usage.as_ref(),
                thread.codex_usage_snapshot.as_ref(),
                thread.acp_session_id.as_deref(),
            );
            if let Some(snapshot) = snapshot {
                thread.codex_usage_snapshot = Some(snapshot);
            }
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

    fn is_current_run(&self, thread_id: &str, run_epoch: u64) -> bool {
        self.run_epochs
            .lock()
            .unwrap()
            .get(thread_id)
            .is_some_and(|current| *current == run_epoch)
    }

    fn finish_turn_if_current(
        &self,
        thread_id: &str,
        run_epoch: u64,
        stop_reason: &str,
        usage: Option<Value>,
    ) {
        if self.is_current_run(thread_id, run_epoch) {
            self.finish_turn(thread_id, stop_reason, usage);
        }
    }

    fn emit_update(&self, thread_id: &str, item: &Item) -> Result<(), tauri::Error> {
        self.emit_op(thread_id, json!({ "t": "upsert", "item": item }))
    }

    fn emit_op(&self, thread_id: &str, op: Value) -> Result<(), tauri::Error> {
        self.app
            .emit(EV_UPDATE, json!({ "threadId": thread_id, "op": op }))
    }
}

impl Drop for SdkManager {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn bridge_path(app: &AppHandle, adapter: &dyn SdkAdapter) -> Result<PathBuf, String> {
    let (name, bridge) = adapter.bridge();
    let dir = crate::nova_data_dir(app).join("runtime");
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("创建 {} 运行目录失败：{e}", adapter.label()))?;
    let path = dir.join(name);
    if std::fs::read(&path).ok().as_deref() != Some(bridge) {
        std::fs::write(&path, bridge)
            .map_err(|e| format!("释放 {} bridge 失败：{e}", adapter.label()))?;
    }
    Ok(path)
}

fn prompt_parts(adapter: &dyn SdkAdapter, text: &str, images: &[PromptImage]) -> Vec<Value> {
    let mut parts = Vec::new();
    if !text.is_empty() {
        parts.push(json!({ "type": "text", "text": text }));
    }
    for image in images {
        if adapter.accepts_data_image(&image.mime_type) {
            if let Some(data) = &image.data {
                parts.push(json!({
                    "type": "image_data", "name": image.name, "mime": image.mime_type, "data": data
                }));
                continue;
            }
        } else if let Some(path) = save_attachment_to_temp(image) {
            parts.push(json!({ "type": "local_image", "path": path }));
            continue;
        }
        if let Some(uri) = &image.uri {
            let path = file_uri_to_local_path(uri).unwrap_or_else(|| uri.clone());
            parts.push(json!({ "type": "local_image", "path": path }));
        }
    }
    parts
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

fn parse_bridge_output(output: &str, label: &str) -> Result<Value, String> {
    let line = output
        .lines()
        .find(|line| !line.trim().is_empty())
        .ok_or_else(|| format!("{label} bridge 未返回结果"))?;
    let response: Value = serde_json::from_str(line)
        .map_err(|e| format!("解析 {} bridge 响应失败：{e}；输出：{line}", label))?;
    if response.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(response["error"]
            .as_str()
            .unwrap_or("SDK bridge 执行失败")
            .into());
    }
    response
        .get("data")
        .cloned()
        .ok_or_else(|| format!("{label} bridge 响应缺少 data"))
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

#[derive(Debug, PartialEq, Eq)]
enum TextSnapshotChange<'a> {
    Delta(&'a str),
    Unchanged,
    Replace,
}

/// Codex SDK 的 `item.updated` 携带累计文本快照。把纯追加部分转换成前端 delta，
/// 同文快照不重复刷新；若服务端改写了既有文本，则回退到整条 upsert。
fn text_snapshot_change<'a>(previous: &Item, next: &'a Item) -> TextSnapshotChange<'a> {
    let texts = match (previous, next) {
        (Item::Assistant { text: previous, .. }, Item::Assistant { text: next, .. })
        | (Item::Thought { text: previous, .. }, Item::Thought { text: next, .. }) => {
            Some((previous.as_str(), next.as_str()))
        }
        _ => None,
    };
    let Some((previous, next)) = texts else {
        return TextSnapshotChange::Replace;
    };
    if previous == next {
        return TextSnapshotChange::Unchanged;
    }
    next.strip_prefix(previous)
        .map(TextSnapshotChange::Delta)
        .unwrap_or(TextSnapshotChange::Replace)
}

/// Codex SDK 用 `todo_list` 快照表达计划进度；转换成 Nova 各后端共用的计划结构。
fn codex_todo_plan(value: &Value) -> Option<Value> {
    if value.get("type").and_then(Value::as_str) != Some("todo_list") {
        return None;
    }
    let entries = value
        .get("items")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let content = item.get("text")?.as_str()?.trim();
            if content.is_empty() {
                return None;
            }
            let status = if item.get("completed").and_then(Value::as_bool) == Some(true) {
                "completed"
            } else {
                "pending"
            };
            Some(json!({ "content": content, "status": status }))
        })
        .collect::<Vec<_>>();
    Some(json!(entries))
}

#[cfg(test)]
mod tests {
    use super::{
        codex_todo_plan, complete_pending_tools, derive_title, is_codex_model_resume_warning,
        normalize_title, parse_bridge_output, resolve_codex_model, text_snapshot_change, tool_call,
        TextSnapshotChange,
    };
    use crate::sdk_adapters::{
        AlkaidAdapter, ClaudeAdapter, CodeBuddyAdapter, CodexAdapter, CursorAdapter, SdkAdapter,
    };
    use crate::threads::{now_ms, AgentKind, CodexUsageSnapshot, Item, Thread, ToolCall};
    use serde_json::json;

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
    fn codex_model_resume_warning_is_nonfatal() {
        assert!(is_codex_model_resume_warning(&json!({
            "type": "error",
            "message": "This session was recorded with model `gpt-5.6-terra` but is resuming with `gpt-5.6-sol`. Consider switching back to `gpt-5.6-terra` as it may affect Codex performance."
        })));
        assert!(!is_codex_model_resume_warning(&json!({
            "type": "error",
            "message": "Codex transport failed"
        })));
    }

    #[test]
    fn codex_usage_is_the_delta_between_cumulative_snapshots() {
        let baseline = CodexUsageSnapshot {
            session_id: Some("thread-1".into()),
            input_tokens: 1_000,
            output_tokens: 100,
        };
        let (usage, snapshot) = CodexAdapter.normalize_usage(
            Some(&json!({
                "input_tokens": 1_600,
                "cached_input_tokens": 500,
                "output_tokens": 180,
                "reasoning_output_tokens": 40
            })),
            Some(&baseline),
            Some("thread-1"),
        );

        assert_eq!(
            usage,
            Some(json!({ "inputTokens": 600, "outputTokens": 80, "totalTokens": 680 }))
        );
        assert_eq!(snapshot.unwrap().input_tokens, 1_600);
    }

    #[test]
    fn codex_usage_without_a_matching_baseline_is_not_counted() {
        let raw = json!({ "input_tokens": 1_600, "output_tokens": 180 });
        let (missing, snapshot) = CodexAdapter.normalize_usage(Some(&raw), None, Some("thread-1"));
        assert!(missing.is_none());
        assert_eq!(snapshot.unwrap().output_tokens, 180);

        let other_session = CodexUsageSnapshot {
            session_id: Some("thread-0".into()),
            input_tokens: 1_000,
            output_tokens: 100,
        };
        let (mismatched, _) =
            CodexAdapter.normalize_usage(Some(&raw), Some(&other_session), Some("thread-1"));
        assert!(mismatched.is_none());
    }

    #[test]
    fn alkaid_usage_includes_pi_cached_input() {
        let raw = json!({
            "input": 100,
            "output": 20,
            "cacheRead": 300,
            "cacheWrite": 40
        });
        let (usage, _) = AlkaidAdapter.normalize_usage(Some(&raw), None, None);

        assert_eq!(
            usage,
            Some(json!({ "inputTokens": 440, "outputTokens": 20, "totalTokens": 460 }))
        );
    }

    #[test]
    fn cursor_usage_maps_camel_case_and_includes_cached_input() {
        let raw = json!({
            "inputTokens": 100,
            "outputTokens": 20,
            "cacheReadTokens": 300,
            "cacheWriteTokens": 40
        });
        let (usage, _) = CursorAdapter.normalize_usage(Some(&raw), None, None);

        assert_eq!(
            usage,
            Some(json!({ "inputTokens": 440, "outputTokens": 20, "totalTokens": 460 }))
        );
    }

    #[test]
    fn claude_style_usage_includes_cached_input_and_rejects_partial_data() {
        let raw = json!({
            "input_tokens": 100,
            "output_tokens": 20,
            "cache_read_input_tokens": 300,
            "cache_creation_input_tokens": 40
        });
        for adapter in [&ClaudeAdapter as &dyn SdkAdapter, &CodeBuddyAdapter] {
            let (usage, _) = adapter.normalize_usage(Some(&raw), None, None);
            assert_eq!(
                usage,
                Some(json!({ "inputTokens": 440, "outputTokens": 20, "totalTokens": 460 }))
            );
        }

        let partial = json!({ "input_tokens": 100 });
        let (usage, _) = ClaudeAdapter.normalize_usage(Some(&partial), None, None);
        assert!(usage.is_none());
    }

    #[test]
    fn parses_and_normalizes_title_response() {
        let output =
            parse_bridge_output(r#"{"ok":true,"data":"`修复标题路由。`"}"#, "Codex+").unwrap();
        assert_eq!(
            normalize_title(output.as_str().unwrap(), "fallback"),
            "修复标题路由"
        );
        assert_eq!(normalize_title("  ", "fallback"), "fallback");
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

    #[test]
    fn codex_text_snapshots_become_deltas_when_they_only_append() {
        let previous = Item::Assistant {
            id: 1,
            text: "你好".into(),
            ts: 1,
        };
        let appended = Item::Assistant {
            id: 1,
            text: "你好，世界".into(),
            ts: 2,
        };
        let unchanged = appended.clone();
        let rewritten = Item::Assistant {
            id: 1,
            text: "您好，世界".into(),
            ts: 3,
        };

        assert_eq!(
            text_snapshot_change(&previous, &appended),
            TextSnapshotChange::Delta("，世界")
        );
        assert_eq!(
            text_snapshot_change(&appended, &unchanged),
            TextSnapshotChange::Unchanged
        );
        assert_eq!(
            text_snapshot_change(&appended, &rewritten),
            TextSnapshotChange::Replace
        );
    }

    #[test]
    fn codex_todo_list_becomes_the_shared_plan_shape() {
        let plan = codex_todo_plan(&json!({
            "id": "todo-1",
            "type": "todo_list",
            "items": [
                { "text": " inspect repository ", "completed": true },
                { "text": "Implement fix", "completed": false },
                { "text": "  ", "completed": false }
            ]
        }));
        assert_eq!(
            plan,
            Some(json!([
                { "content": "inspect repository", "status": "completed" },
                { "content": "Implement fix", "status": "pending" }
            ]))
        );
        assert_eq!(
            codex_todo_plan(&json!({ "id": "tool", "type": "web_search" })),
            None
        );
    }

    #[test]
    fn cursor_tools_show_the_specific_operation() {
        let shell = tool_call(&json!({
            "id": "shell", "type": "mcp_tool_call", "server": "Cursor", "tool": "shell",
            "arguments": { "command": "python inspect_excel.py 1.xlsx" }, "status": "in_progress"
        }));
        assert_eq!(
            shell.title,
            "Cursor / shell · python inspect_excel.py 1.xlsx"
        );
        assert_eq!(shell.kind, "execute");

        let read = tool_call(&json!({
            "id": "read", "type": "mcp_tool_call", "server": "Cursor", "tool": "read",
            "arguments": { "path": "C:/Users/1/Desktop/1.xlsx" }, "status": "completed"
        }));
        assert_eq!(read.title, "Cursor / read · C:/Users/1/Desktop/1.xlsx");
        assert_eq!(read.kind, "read");
        assert_eq!(read.locations[0]["path"], "C:/Users/1/Desktop/1.xlsx");
    }

    #[test]
    fn sdk_tools_preserve_available_details() {
        let command = tool_call(&json!({
            "id": "command", "type": "command_execution", "command": "git status",
            "aggregated_output": " M src/main.rs\n", "exit_code": 0, "status": "completed"
        }));
        assert_eq!(command.raw_input.unwrap()["command"], "git status");
        assert_eq!(command.raw_output.as_ref().unwrap()["exitCode"], 0);
        assert_eq!(command.content[0]["content"]["text"], " M src/main.rs\n");

        let files = tool_call(&json!({
            "id": "files", "type": "file_change", "status": "completed", "changes": [
                { "path": "src/main.rs", "kind": "update" },
                { "path": "src/new.rs", "kind": "add" }
            ]
        }));
        assert_eq!(files.title, "修改 2 个文件");
        assert_eq!(files.locations[0]["path"], "src/main.rs");
        assert_eq!(
            files.content[0]["content"]["text"],
            "更新 src/main.rs\n新增 src/new.rs"
        );

        let mcp = tool_call(&json!({
            "id": "mcp", "type": "mcp_tool_call", "server": "files", "tool": "read",
            "arguments": { "path": "README.md" }, "status": "completed",
            "result": {
                "content": [{ "type": "text", "text": "hello" }],
                "structured_content": { "lines": 1 }
            }
        }));
        assert_eq!(mcp.content[0]["content"]["text"], "hello");
        assert_eq!(mcp.raw_output.unwrap()["structured_content"]["lines"], 1);

        let future = tool_call(&json!({
            "id": "future", "type": "image_generation", "status": "completed",
            "result": { "path": "out.png" }
        }));
        assert_eq!(future.title, "image generation");
        assert_eq!(future.raw_output.unwrap()["result"]["path"], "out.png");
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

fn text_content(text: String) -> Vec<Value> {
    if text.trim().is_empty() {
        Vec::new()
    } else {
        vec![json!({ "type": "content", "content": { "type": "text", "text": text } })]
    }
}

fn display_file_change(kind: &str, path: &str) -> String {
    let action = match kind {
        "add" => "新增",
        "delete" => "删除",
        "update" => "更新",
        _ => kind,
    };
    format!("{action} {path}")
}

fn mcp_result_text(result: &Value) -> Option<String> {
    let content = result.get("content")?.as_array()?;
    let text = content
        .iter()
        .filter_map(|block| {
            (block.get("type").and_then(Value::as_str) == Some("text"))
                .then(|| block.get("text").and_then(Value::as_str))
                .flatten()
        })
        .collect::<Vec<_>>()
        .join("\n");
    (!text.is_empty()).then_some(text)
}

fn tool_call(value: &Value) -> ToolCall {
    let kind = value.get("type").and_then(Value::as_str).unwrap_or("tool");
    let status = value
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("completed");
    let (title, tool_kind, locations, raw_input, raw_output, content) = match kind {
        "command_execution" => {
            let output = value
                .get("aggregated_output")
                .and_then(Value::as_str)
                .unwrap_or_default();
            (
                value
                    .get("command")
                    .and_then(Value::as_str)
                    .unwrap_or("Command")
                    .to_string(),
                "execute",
                Vec::new(),
                Some(json!({ "command": value.get("command").cloned().unwrap_or(Value::Null) })),
                Some(json!({
                    "aggregatedOutput": value.get("aggregated_output").cloned().unwrap_or(Value::Null),
                    "exitCode": value.get("exit_code").cloned().unwrap_or(Value::Null)
                })),
                text_content(output.to_string()),
            )
        }
        "file_change" => {
            let changes = value
                .get("changes")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let locations = changes
                .iter()
                .filter_map(|change| change.get("path").and_then(Value::as_str))
                .map(|path| json!({ "path": path }))
                .collect();
            let details = changes
                .iter()
                .filter_map(|change| {
                    Some(display_file_change(
                        change.get("kind")?.as_str()?,
                        change.get("path")?.as_str()?,
                    ))
                })
                .collect::<Vec<_>>()
                .join("\n");
            let title = if changes.len() == 1 {
                changes[0]
                    .get("path")
                    .and_then(Value::as_str)
                    .map(|path| format!("修改 {path}"))
                    .unwrap_or_else(|| "修改文件".into())
            } else {
                format!("修改 {} 个文件", changes.len())
            };
            (
                title,
                "edit",
                locations,
                Some(json!({ "changes": changes })),
                None,
                text_content(details),
            )
        }
        "mcp_tool_call" => {
            let server = value.get("server").and_then(Value::as_str).unwrap_or("MCP");
            let tool = value.get("tool").and_then(Value::as_str).unwrap_or("tool");
            let arguments = value.get("arguments");
            let detail = match tool {
                "shell" => arguments.and_then(|args| args.get("command")),
                "read" | "edit" | "write" | "delete" | "ls" => {
                    arguments.and_then(|args| args.get("path"))
                }
                "glob" => arguments.and_then(|args| args.get("globPattern")),
                "grep" => arguments.and_then(|args| args.get("pattern")),
                "semSearch" => arguments.and_then(|args| args.get("query")),
                _ => None,
            }
            .and_then(Value::as_str)
            .map(compact_tool_detail)
            .filter(|detail| !detail.is_empty());
            let path = arguments
                .and_then(|args| args.get("path"))
                .and_then(Value::as_str);
            let result = value.get("result").or_else(|| value.get("error")).cloned();
            let output = value
                .get("error")
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| value.get("result").and_then(mcp_result_text));
            (
                format!(
                    "{server} / {tool}{}",
                    detail
                        .map(|detail| format!(" · {detail}"))
                        .unwrap_or_default()
                ),
                match tool {
                    "shell" => "execute",
                    "read" => "read",
                    "edit" | "write" => "edit",
                    "delete" => "delete",
                    "glob" | "grep" | "semSearch" | "ls" => "search",
                    "createPlan" | "updateTodos" => "think",
                    _ => "other",
                },
                path.map(|path| vec![json!({ "path": path })])
                    .unwrap_or_default(),
                arguments.cloned(),
                result,
                output.map(text_content).unwrap_or_default(),
            )
        }
        "web_search" => (
            value
                .get("query")
                .and_then(Value::as_str)
                .map(|query| format!("搜索 {query}"))
                .unwrap_or_else(|| "网页搜索".into()),
            "search",
            Vec::new(),
            Some(json!({ "query": value.get("query").cloned().unwrap_or(Value::Null) })),
            None,
            Vec::new(),
        ),
        "todo_list" => {
            let items = value
                .get("items")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let text = items
                .iter()
                .filter_map(|item| {
                    let text = item.get("text")?.as_str()?;
                    let mark = if item.get("completed").and_then(Value::as_bool) == Some(true) {
                        "x"
                    } else {
                        " "
                    };
                    Some(format!("[{mark}] {text}"))
                })
                .collect::<Vec<_>>()
                .join("\n");
            (
                "Todo list".into(),
                "think",
                Vec::new(),
                Some(json!({ "items": items })),
                None,
                text_content(text),
            )
        }
        _ => (
            kind.replace('_', " "),
            "other",
            Vec::new(),
            None,
            Some(value.clone()),
            Vec::new(),
        ),
    };
    ToolCall {
        tool_call_id: value.get("id").and_then(Value::as_str).unwrap_or("").into(),
        title,
        kind: tool_kind.into(),
        status: match status {
            "failed" => "failed",
            "in_progress" => "in_progress",
            _ => "completed",
        }
        .into(),
        content,
        locations,
        raw_input,
        raw_output,
    }
}
