use crate::acp::{
    EV_LOG, EV_OPTIONS, EV_THREADS, EV_TURN, EV_UPDATE,
};
use crate::model_cache;
use crate::threads::{
    file_uri_to_local_path, now_ms, save_attachment_to_temp, AgentKind, Item,
    PromptImage, ToolCall,
};
use crate::{nova_data_dir, AppState};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tauri::{AppHandle, Emitter, Manager};

struct RunningBridge {
    control: tokio::sync::mpsc::UnboundedSender<String>,
    /// 注册代数（run_epoch），用于区分同一 session 的新旧回合。
    registration: u64,
    abort: tokio::task::AbortHandle,
}

impl RunningBridge {
    fn identity(&self) -> u64 {
        self.registration
    }
}

fn kill_running(bridge: &RunningBridge) {
    bridge.abort.abort();
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ReadEventsOutcome {
    Completed,
    Superseded,
}

pub struct LyraRuntime {
    app: AppHandle,
    running_children: Mutex<HashMap<String, RunningBridge>>,
    running: Mutex<HashSet<String>>,
    turn_started: Mutex<HashMap<String, Instant>>,
    pending_permissions: Mutex<HashMap<String, (String, String)>>,
    model_options: Mutex<Option<Value>>,
    model_options_refreshing: AtomicBool,
    model_options_revalidated: AtomicBool,
    /// 本地 Lyra 配置变化代数，避免旧模型列表覆盖新配置结果。
    config_generation: AtomicU64,
    next_run_epoch: AtomicU64,
    run_epochs: Mutex<HashMap<String, u64>>,
}

impl LyraRuntime {
    pub fn new(app: AppHandle) -> Arc<Self> {
        Arc::new(Self {
            app,
            running_children: Mutex::new(HashMap::new()),
            running: Mutex::new(HashSet::new()),
            turn_started: Mutex::new(HashMap::new()),
            pending_permissions: Mutex::new(HashMap::new()),
            model_options: Mutex::new(None),
            model_options_refreshing: AtomicBool::new(false),
            model_options_revalidated: AtomicBool::new(false),
            config_generation: AtomicU64::new(0),
            next_run_epoch: AtomicU64::new(1),
            run_epochs: Mutex::new(HashMap::new()),
        })
    }

    /// 按当前设置复用 provider 连接池；代理修改后下一次请求立即使用新配置。
    fn native_http(&self) -> reqwest::Client {
        crate::lyra::provider::http_client()
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
        let (cwd, model, mode, reasoning_effort, context, session_id) = {
            let state = self.app.state::<AppState>();
            let mut store = state.store.lock().unwrap();
            let Some(thread) = store.get_mut(&thread_id) else {
                return;
            };
            if thread.agent_kind != AgentKind::Lyra {
                drop(store);
                self.push_system(&thread_id, "该模型尚未迁移到 Lyra，未执行请求".into(), "error");
                self.set_running(&thread_id, false, Some("error"));
                return;
            }
            let context = thread.take_prompt_context("Lyra");
            let session_id = thread.acp_session_id.clone();
            let item = thread.push_user(text.clone(), images.clone());
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
                session_id,
            );
            store.save_thread(&thread_id);
            values
        };
        if let Some((prompt, fallback)) = title_job {
            self.app.state::<AppState>().generate_title(
                &AgentKind::Lyra,
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
        let mut parts = Vec::new();
        if let Some(context) = context {
            parts.push(json!({ "type": "text", "text": context }));
        }
        parts.extend(prompt_parts(&text, &images));
        let lightweight_model = self.app.state::<AppState>().lyra_lightweight_model();
        let request = json!({
            "action": "prompt",
            "threadId": thread_id,
            "cwd": cwd,
            "sessionId": session_id,
            "model": model,
            "mode": mode,
            "reasoningEffort": reasoning_effort,
            "lightweightModel": lightweight_model,
            "parts": parts
        });
        let outcome = self
            .run_prompt_inprocess(&thread_id, request, run_epoch)
            .await;
        if !self.is_current_run(&thread_id, run_epoch) {
            return;
        }
        let succeeded = outcome.is_ok();
        if let Err(error) = outcome {
            self.push_system(
                &thread_id,
                format!("Lyra 请求失败：{error}"),
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
            .map(|bridge| (bridge.control.clone(), bridge.identity()));
        let target = bridge.as_ref().map(|(_, identity)| *identity);
        if let Some((control, _)) = bridge {
            let _ = write_control(&control, &json!({ "action": "cancel" })).await;
            for _ in 0..20 {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                let target_is_running = self
                    .running_children
                    .lock()
                    .unwrap()
                    .get(thread_id)
                    .is_some_and(|bridge| Some(bridge.identity()) == target);
                if !target_is_running {
                    break;
                }
            }
        }
        let bridge = {
            let mut running = self.running_children.lock().unwrap();
            running
                .get(thread_id)
                .is_some_and(|bridge| Some(bridge.identity()) == target)
                .then(|| running.remove(thread_id))
                .flatten()
        };
        if let Some(bridge) = bridge {
            kill_running(&bridge);
            // 进程内（Lyra）bridge 的中断轨迹（pendingMessages）只在轮次收尾时写盘；
            // 宽限期内没能自行结束（典型：卡在长时间工具执行里，工具不响应取消），
            // 被强制 abort 意味着这次中断内容没来得及持久化。回退到 Nova 侧接力上下文，
            // 把已流出的中断轮注入下一条提示，避免续聊时上下文退回上次结论。
            let state = self.app.state::<AppState>();
            let mut store = state.store.lock().unwrap();
            if let Some(thread) = store.get_mut(thread_id) {
                thread.handoff_from = Some(AgentKind::Lyra);
            }
            store.save_thread(&thread_id);
        }
        self.finish_turn(thread_id, "cancelled", None);
    }

    /// 原生 steer 直接把用户消息排入当前 Agent run；空闲时开启新轮次。
    pub async fn steer_prompt(
        self: &Arc<Self>,
        thread_id: String,
        text: String,
        images: Vec<PromptImage>,
    ) {
        if self.is_running(&thread_id) {
            self.native_steer_prompt(&thread_id, text, images).await;
            return;
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
        let mut control = None;
        for _ in 0..20 {
            control = self
                .running_children
                .lock()
                .unwrap()
                .get(thread_id)
                .map(|bridge| bridge.control.clone());
            if control.is_some() || !self.is_running(thread_id) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        let Some(control) = control else {
            if !self.is_running(thread_id) {
                self.clone()
                    .run_prompt(thread_id.to_string(), text, images)
                    .await;
            } else {
                self.push_system(thread_id, "引导失败：运行通道尚未就绪。".into(), "error");
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
            store.save_thread(thread_id);
        }

        let parts = prompt_parts(&text, &images);
        if let Err(error) =
            write_control(&control, &json!({ "action": "steer", "parts": parts })).await
        {
            self.push_system(thread_id, format!("引导发送失败：{error}"), "error");
        }
    }

    pub fn forget_session_of_thread(&self, thread_id: &str) {
        if let Some(bridge) = self.running_children.lock().unwrap().remove(thread_id) {
            kill_running(&bridge);
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
            .ok_or_else(|| "Lyra fork 未返回新会话 ID".into())
    }

    pub fn shutdown(&self) {
        for bridge in std::mem::take(&mut *self.running_children.lock().unwrap()).into_values() {
            kill_running(&bridge);
        }
    }

    pub fn seed_model_options(&self, value: Value) {
        *self.model_options.lock().unwrap() = Some(value);
    }

    /// 后端启动配置（API Key / 可执行文件 / 代理）变化后，缓存的模型列表就过期了，
    /// 必须立刻用新配置重拉，成功后由 EV_OPTIONS 推给前端。否则用户填完 API Key
    /// 也只能看到空列表，直到下次重启应用。
    /// 不先清内存：旧缓存继续服务前端，拉到新列表后再覆盖。
    pub fn refresh_model_options_soon(self: &Arc<Self>) {
        self.model_options_revalidated
            .store(false, Ordering::SeqCst);
        // 旧配置的探测可能仍在飞行中，等它退出 refreshing 闸门再用新配置重拉。
        let manager = self.clone();
        tauri::async_runtime::spawn(async move {
            while manager.model_options_refreshing.load(Ordering::SeqCst) {
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
            manager.spawn_revalidate_model_options();
        });
    }

    /// 本地 `config.jsonc` 发生变化时调用。当前正在执行的任务不打断，
    /// 但会让下一轮请求和模型列表使用新配置。
    pub fn notify_config_changed(self: &Arc<Self>) {
        self.invalidate_lyra_config();
    }

    fn invalidate_lyra_config(self: &Arc<Self>) {
        crate::lyra_complete::invalidate_config_cache();
        self.config_generation.fetch_add(1, Ordering::SeqCst);
        *self.model_options.lock().unwrap() = None;
        let _ = self.app.emit(
            EV_OPTIONS,
            json!({
                "agentKind": "lyra",
                "options": self.pending_model_options(),
            }),
        );
        self.refresh_model_options_soon();
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
            if let Err(error) = manager.refresh_model_options().await {
                let _ = manager.app.emit(
                    EV_LOG,
                    format!("[Lyra] 拉取模型列表失败：{error}"),
                );
            }
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
        Ok(self.pending_model_options())
    }

    /// 真实列表还没拉到时给前端的占位。带 pending 标记，前端才知道这不是最终结果，
    /// 下次打开选择器可以再问一次，而不是把空列表当成已加载缓存住。
    fn pending_model_options(&self) -> Value {
        json!({
            "configOptions": [{ "id": "model", "name": "Model", "currentValue": "", "options": [] }],
            "modes": null,
            "pending": true,
        })
    }

    async fn refresh_model_options(&self) -> Result<Value, String> {
        let generation = self.config_generation.load(Ordering::SeqCst);
        let cwd = std::env::current_dir()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let value = self
            .run_bridge(&cwd, json!({ "action": "models", "cwd": cwd }))
            .await?;
        if generation != self.config_generation.load(Ordering::SeqCst) {
            return Err("Lyra 配置已更新，丢弃旧模型列表".into());
        }
        *self.model_options.lock().unwrap() = Some(value.clone());
        model_cache::save(&crate::nova_data_dir(&self.app), "lyra", &value);
        let _ = self.app.emit(
            EV_OPTIONS,
            json!({ "agentKind": "lyra", "options": value }),
        );
        self.model_options_revalidated.store(true, Ordering::SeqCst);
        Ok(value)
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
            let cwd = std::env::current_dir()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
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
                    store.save_thread(&thread_id);
                    let _ = manager.app.emit(EV_THREADS, json!({}));
                }
            }
        });
    }

    /// Lyra 一次性全长无工具生成，不使用行内补全的短输出限制。
    pub async fn generate_text_once(&self, model: &str, prompt: String) -> Result<String, String> {
        let output = crate::lyra::run_oneshot(
            &self.native_http(),
            &json!({ "action": "generate", "model": model, "prompt": prompt }),
            None,
        ).await?;
        output.as_str().map(str::to_owned)
            .ok_or_else(|| "Lyra 文本生成返回了非文本结果".to_string())
    }

    /// 无工具设计轮次：完整历史按原生 role 顺序发送，不创建工具会话。
    pub(crate) async fn generate_design_once(
        &self,
        model: &str,
        prompt: String,
        history: &[crate::miniapp_design::DesignMessage],
        images: &[Value],
    ) -> Result<String, String> {
        let output = crate::lyra::run_oneshot(
            &self.native_http(),
            &json!({ "action": "generate", "model": model, "prompt": prompt, "history": history, "images": images }),
            None,
        ).await?;
        output.as_str().map(str::to_owned)
            .ok_or_else(|| "Lyra 文本生成返回了非文本结果".to_string())
    }

    /// 一次性行内补全：Rust 直连 provider HTTP（不冷启 Node）。
    /// 仅 openai-completions / openai-responses；其它协议再回退 bridge。
    pub async fn complete_once(
        &self,
        cwd: &str,
        model: &str,
        prompt: String,
    ) -> Result<String, String> {
        let data_dir = nova_data_dir(&self.app);
        match crate::lyra_complete::complete_direct(
            &self.native_http(),
            &data_dir,
            &HashMap::new(),
            model,
            &prompt,
        )
        .await
        {
            Ok(text) => return Ok(text),
            Err(error)
                if error.contains("补全暂不支持直连协议")
                    || error.contains("provider 缺少 api") =>
            {
                // anthropic / google 等仍走 bridge
            }
            Err(error) => return Err(error),
        }
        let request = json!({
            "action": "complete",
            "cwd": cwd,
            "model": model,
            "prompt": prompt,
        });
        let output = self.run_bridge(cwd, request).await?;
        Ok(output.as_str().unwrap_or_default().trim().to_string())
    }

    async fn run_bridge(&self, _cwd: &str, request: Value) -> Result<Value, String> {
        crate::lyra::run_oneshot(&self.native_http(), &request, None).await
    }

    /// 原生会话控制在同进程 tokio 任务中执行，事件/控制行走 mpsc 通道。
    async fn run_prompt_inprocess(
        &self,
        thread_id: &str,
        mut request: Value,
        run_epoch: u64,
    ) -> Result<(), String> {
        let context_tools = {
            let state = self.app.state::<AppState>();
            let enabled = state.settings.lock().unwrap().context_tools_enabled();
            enabled
        };
        // Lyra 子 agent 的 Stage 只用于展示，不经主轮次的短生命周期事件通道。
        request["novaThreadId"] = json!(thread_id);
        let session = crate::lyra::spawn_prompt(
            None,
            request,
            context_tools,
            None,
            Some(self.app.clone()),
        );
        let abort = session.task.abort_handle();
        self.running_children.lock().unwrap().insert(
            thread_id.to_string(),
            RunningBridge {
                control: session.control.clone(),
                registration: run_epoch,
                abort: abort.clone(),
            },
        );
        let crate::lyra::InProcessSession { events, task, .. } = session;
        let mut source = events;
        let event_result = self
            .read_events(thread_id, run_epoch, &mut source)
            .await;
        // 仅当注册项仍属于本回合时才移除（新回合可能已重新注册）。
        {
            let mut running = self.running_children.lock().unwrap();
            if running
                .get(thread_id)
                .is_some_and(|bridge| bridge.identity() == run_epoch)
            {
                running.remove(thread_id);
            }
        }
        let result = event_result.map(|_| ());
        // 错误均已作为 {ok:false} 事件流出；这里只打捞 panic 信息，interrupt 的 abort 属正常。
        match tokio::time::timeout(std::time::Duration::from_secs(5), task).await {
            Ok(Err(join_error)) if join_error.is_panic() => Err(format!(
                "{}；原生任务 panic：{join_error}",
                result
                    .err()
                    .unwrap_or_else(|| "原生任务异常结束".to_string())
            )),
            Err(_) => {
                abort.abort();
                result
            }
            _ => result,
        }
    }

    async fn read_events(
        &self,
        thread_id: &str,
        run_epoch: u64,
        source: &mut tokio::sync::mpsc::UnboundedReceiver<String>,
    ) -> Result<ReadEventsOutcome, String> {
        let mut item_ids = HashMap::new();
        while let Some(line) = source.recv().await {
            let event: Value = serde_json::from_str(&line).map_err(|e| {
                format!("解析 Lyra 事件失败：{e}；输出：{line}")
            })?;
            if event.get("ok").and_then(Value::as_bool) == Some(false) {
                return Err(event["error"]
                    .as_str()
                    .unwrap_or("Lyra 任务执行失败")
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
                return Ok(ReadEventsOutcome::Superseded);
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
                        format!("[Lyra][timing] {phase} {elapsed_ms}ms{cancelled_runs}"),
                    );
                    // provider 出错后 turn 会退避重试（每次可能等满 180s），只写日志时界面像卡死。
                    if phase == "provider_retry" {
                        let error = event.get("error").and_then(Value::as_str).unwrap_or("未知错误");
                        self.push_system(
                            thread_id,
                            format!("Lyra 请求出错，正在自动重试：{error}"),
                            "warn",
                        );
                    }
                }
                Some("item") => self.apply_item(thread_id, &event["item"], &mut item_ids),
                Some("working_directory_changed") => {
                    if let Some(cwd) = event.get("cwd").and_then(Value::as_str) {
                        self.change_working_directory(thread_id, cwd);
                    }
                }
                Some("plan") => self.apply_plan(thread_id, &event["plan"]),
                Some("permission") => self.emit_permission(thread_id, &event["permission"]),
                Some("usage") => {
                    // 本轮进行中的累计用量（bridge 可选上报）：只推给前端实时展示，不落库；
                    // Turn 落库时前端清零该值，避免与轮次用量重复计。
                    if let Some(raw) = event.get("usage") {
                        if let Some(usage) = normalize_usage(Some(raw)) {
                            let _ =
                                self.emit_op(thread_id, json!({ "t": "usage", "usage": usage }));
                        }
                    }
                }
                Some("done") => {
                    let usage = event.get("usage").cloned();
                    let stop_reason = if event.get("cancelled").and_then(Value::as_bool) == Some(true) {
                        "cancelled"
                    } else {
                        "end_turn"
                    };
                    self.finish_turn_if_current(thread_id, run_epoch, stop_reason, usage);
                    return Ok(ReadEventsOutcome::Completed);
                }
                _ => {}
            }
        }
        Err("Lyra 事件通道意外关闭".into())
    }

    fn change_working_directory(&self, thread_id: &str, cwd: &str) {
        let path = std::path::PathBuf::from(cwd);
        let canonical = dunce::canonicalize(&path).unwrap_or(path);
        if !canonical.is_dir() {
            return;
        }
        let cwd = display_working_directory(&canonical);
        let state = self.app.state::<AppState>();
        {
            let mut store = state.store.lock().unwrap();
            if let Some(thread) = store.get_mut(thread_id) {
                thread.cwd = cwd.clone();
            }
            store.save_thread(thread_id);
        }
        // touch 同时覆盖“已有则切换到最前、没有则创建”，并持久化 projects.json。
        state.projects.lock().unwrap().touch(&cwd);
        let _ = self.app.emit("projects:changed", json!({}));
        let _ = self.app.emit(
            "thread:cwd-changed",
            json!({ "threadId": thread_id, "cwd": cwd }),
        );
        let _ = self.app.emit(EV_THREADS, json!({}));
    }

    fn save_session_id(&self, thread_id: &str, session_id: &str) {
        let state = self.app.state::<AppState>();
        let mut store = state.store.lock().unwrap();
        if let Some(thread) = store.get_mut(thread_id) {
            thread.acp_session_id = Some(session_id.to_string());
        }
        store.save_thread(thread_id);
    }

    pub(crate) fn apply_item(&self, thread_id: &str, value: &Value, ids: &mut HashMap<String, u64>) {
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
                    // 签名或空增量不代表可展示的思考；也不能清空已有摘要。
                    .filter(|text| !text.trim().is_empty())
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
            Some(_) => Some(Item::Tool {
                id,
                ts: now_ms(),
                call: map_tool_call(value),
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
                let mut item = item;
                if let (Item::Tool { ts: started_at, .. }, Item::Tool { ts, call, .. }) =
                    (&*slot, &mut item)
                {
                    *ts = *started_at;
                    if matches!(call.status.as_str(), "completed" | "failed") {
                        set_tool_duration(call, now_ms().saturating_sub(*started_at) as u64);
                    }
                }
                update = match text_snapshot_change(slot, &item) {
                    TextSnapshotChange::Delta(delta) => {
                        Some(json!({ "t": "delta", "itemId": id, "text": delta }))
                    }
                    TextSnapshotChange::Unchanged => None,
                    TextSnapshotChange::Replace => Some(json!({ "t": "upsert", "item": item })),
                };
                *slot = item.clone();
            }
        } else {
            ids.insert(remote_id.into(), id);
            thread.items.push(item.clone());
        }
        thread.updated_at = now_ms();
        if let Some(op) = update {
            let _ = self.emit_op(thread_id, op);
        }
        store.save_thread(thread_id);
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
        store.save_thread(thread_id);
    }

    fn emit_permission(&self, thread_id: &str, permission: &Value) {
        let Some(request_id) = permission.get("id").and_then(Value::as_str) else {
            return;
        };
        let request_key = format!("lyr-perm-{thread_id}-{request_id}");
        self.pending_permissions.lock().unwrap().insert(
            request_key.clone(),
            (thread_id.to_string(), request_id.to_string()),
        );
        let _ = self.app.emit(crate::acp::EV_PERMISSION, json!({
            "threadId": thread_id,
            "agentKind": "lyra",
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
        let control = self
            .running_children
            .lock()
            .unwrap()
            .get(&thread_id)
            .map(|bridge| bridge.control.clone())
            .ok_or("Lyra 会话已结束")?;
        write_control(&control, &json!({ "action": "permission", "requestId": request_id, "reply": if option_id == "reject" { "reject" } else { "once" } })).await?;
        let _ = self.app.emit(
            crate::acp::EV_PERMISSION_RESOLVED,
            json!({ "requestKey": request_key }),
        );
        Ok(())
    }

    pub(crate) fn push_system(&self, thread_id: &str, text: String, level: &str) {
        let state = self.app.state::<AppState>();
        let mut store = state.store.lock().unwrap();
        if let Some(thread) = store.get_mut(thread_id) {
            let item = thread.push_system(text, level);
            let _ = self.emit_update(thread_id, &item);
        }
        store.save_thread(thread_id);
    }

    pub(crate) fn set_running(&self, thread_id: &str, running: bool, stop_reason: Option<&str>) {
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

    pub(crate) fn finish_turn(&self, thread_id: &str, stop_reason: &str, usage: Option<Value>) {
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
            let usage = normalize_usage(usage.as_ref());
            for item in complete_pending_tools(thread) {
                let _ = self.emit_update(thread_id, &item);
            }
            let item = thread.push_turn(duration, usage.as_ref(), stop_reason);
            let _ = self.emit_update(thread_id, &item);
        }
        store.save_thread(thread_id);
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

    pub(crate) fn emit_update(&self, thread_id: &str, item: &Item) -> Result<(), tauri::Error> {
        self.emit_op(thread_id, json!({ "t": "upsert", "item": item }))
    }

    fn emit_op(&self, thread_id: &str, op: Value) -> Result<(), tauri::Error> {
        // 同 acp.rs：只给前台正在查看的会话推流。后台会话的高频增量（delta/upsert/usage）
        // 广播到 WebView 只会被前端按 threadId 丢弃，却仍跨 IPC 全量反序列化，多会话并发
        // 时把 WebView2 渲染进程拖垮。增量已落库，切回时经 get_thread 快照补齐。
        // plan 是低频关键状态，不走 emit_op 的 emit 路径仍始终推送。
        let always = op
            .get("t")
            .and_then(|v| v.as_str())
            .is_some_and(|t| t == "plan");
        if !always {
            let state = self.app.state::<AppState>();
            let active = state.active_thread.lock().unwrap();
            if active.as_deref() != Some(thread_id) {
                return Ok(());
            }
        }
        self.app
            .emit(EV_UPDATE, json!({ "threadId": thread_id, "op": op }))
    }
}

impl Drop for LyraRuntime {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn normalize_usage(usage: Option<&Value>) -> Option<Value> {
    let usage = usage?;
    let input = usage.get("input").and_then(Value::as_u64)?;
    let output = usage.get("output").and_then(Value::as_u64)?;
    let cache_read = usage.get("cacheRead").and_then(Value::as_u64);
    let cache_write = usage.get("cacheWrite").and_then(Value::as_u64);
    let input = input.saturating_add(cache_read.unwrap_or(0).saturating_add(cache_write.unwrap_or(0)));
    let mut normalized = json!({
        "inputTokens": input,
        "outputTokens": output,
        "totalTokens": input.saturating_add(output),
    });
    if let Some(value) = cache_read { normalized["cacheReadTokens"] = value.into(); }
    if let Some(value) = cache_write { normalized["cacheWriteTokens"] = value.into(); }
    if let Some(value) = usage.get("contextTokens").and_then(Value::as_u64) {
        normalized["contextTokens"] = value.into();
    }
    Some(normalized)
}

fn map_tool_call(value: &Value) -> ToolCall {
    let item_type = value.get("type").and_then(Value::as_str).unwrap_or("tool");
    let status = value
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("completed")
        .to_string();
    let arguments = value.get("arguments").cloned();
    let result = value.get("result").or_else(|| value.get("error")).cloned();
    if item_type == "command_execution" {
        let command = value
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or("bash");
        let output = value
            .get("aggregated_output")
            .and_then(Value::as_str)
            .unwrap_or_default();
        return ToolCall {
            tool_call_id: value["id"].as_str().unwrap_or("tool").into(),
            title: command.into(),
            kind: "execute".into(),
            status,
            content: text_content(output),
            locations: Vec::new(),
            raw_input: arguments.or_else(|| Some(json!({ "command": command }))),
            raw_output: result,
        };
    }
    if item_type == "file_change" {
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
        let title = if changes.len() == 1 {
            format!("修改 {}", changes[0]["path"].as_str().unwrap_or("文件"))
        } else {
            format!("修改 {} 个文件", changes.len())
        };
        return ToolCall {
            tool_call_id: value["id"].as_str().unwrap_or("tool").into(),
            title,
            kind: "edit".into(),
            status,
            content: Vec::new(),
            locations,
            raw_input: arguments,
            raw_output: result,
        };
    }
    let server = value
        .get("server")
        .and_then(Value::as_str)
        .unwrap_or("Lyra");
    let tool = value.get("tool").and_then(Value::as_str).unwrap_or("tool");
    let detail = tool_detail(tool, value.get("arguments"));
    let output = value
        .get("error")
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| value.get("result").and_then(result_text));
    ToolCall {
        tool_call_id: value["id"].as_str().unwrap_or("tool").into(),
        title: format!(
            "{server} / {tool}{}",
            detail
                .map(|detail| format!(" · {detail}"))
                .unwrap_or_default()
        ),
        kind: match tool {
            "bash" | "shell" => "execute",
            "read" | "load_skill" => "read",
            "edit" | "write" => "edit",
            "grep" | "find" | "ls" | "glob" => "search",
            _ => "other",
        }
        .into(),
        status,
        content: output
            .map(|output| text_content(&output))
            .unwrap_or_default(),
        locations: argument_paths(value.get("arguments")),
        raw_input: arguments,
        raw_output: result,
    }
}

fn tool_detail(tool: &str, arguments: Option<&Value>) -> Option<String> {
    let arguments = arguments?;
    let value = match tool {
        "read" | "edit" | "write" | "ls" => arguments.get("path")?.as_str()?.to_string(),
        "grep" | "find" => arguments.get("pattern")?.as_str()?.to_string(),
        "load_skill" => arguments.get("name")?.as_str()?.to_string(),
        _ => return None,
    };
    Some(value.chars().take(160).collect())
}

fn argument_paths(arguments: Option<&Value>) -> Vec<Value> {
    let Some(arguments) = arguments else {
        return Vec::new();
    };
    if let Some(path) = arguments.get("path").and_then(Value::as_str) {
        return vec![json!({ "path": path })];
    }
    arguments
        .get("paths")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|path| {
            path.as_str()
                .or_else(|| path.get("path").and_then(Value::as_str))
                .map(|path| json!({ "path": path }))
        })
        .collect()
}

fn result_text(result: &Value) -> Option<String> {
    let text = result
        .get("content")?
        .as_array()?
        .iter()
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n");
    (!text.is_empty()).then_some(text)
}

fn text_content(text: &str) -> Vec<Value> {
    if text.trim().is_empty() {
        Vec::new()
    } else {
        vec![json!({ "type": "content", "content": { "type": "text", "text": text } })]
    }
}

fn prompt_parts(text: &str, images: &[PromptImage]) -> Vec<Value> {
    let mut parts = Vec::new();
    if !text.is_empty() {
        parts.push(json!({ "type": "text", "text": text }));
    }
    for image in images {
        if image.mime_type.starts_with("image/") {
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

async fn write_control(
    control: &tokio::sync::mpsc::UnboundedSender<String>,
    line: &Value,
) -> Result<(), String> {
    control
        .send(line.to_string())
        .map_err(|_| "会话控制通道已关闭".to_string())
}

pub(crate) fn display_working_directory(path: &std::path::Path) -> String {
    let value = path.to_string_lossy();
    if let Some(rest) = value.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = value.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        value.into_owned()
    }
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

fn set_tool_duration(call: &mut ToolCall, duration_ms: u64) {
    let output = call.raw_output.get_or_insert_with(|| json!({}));
    if let Some(object) = output.as_object_mut() {
        object.insert("durationMs".into(), json!(duration_ms));
    }
}

fn complete_pending_tools(thread: &mut crate::threads::Thread) -> Vec<Item> {
    let mut changed = Vec::new();
    let finished_at = now_ms();
    for item in &mut thread.items {
        let Item::Tool { ts, call, .. } = item else {
            continue;
        };
        if call.status == "pending" || call.status == "in_progress" {
            call.status = "completed".to_string();
            set_tool_duration(call, finished_at.saturating_sub(*ts) as u64);
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

/// Lyra 事件流输出累计文本快照。把纯追加部分转换成前端 delta，
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

#[cfg(test)]
mod tests {
    use super::{
        complete_pending_tools, derive_title, display_working_directory, normalize_title,
        text_snapshot_change, write_control, TextSnapshotChange,
    };
    use super::{map_tool_call, normalize_usage, prompt_parts};
    use crate::threads::{now_ms, AgentKind, Item, Thread, ToolCall};
    use serde_json::json;

    #[test]
    fn maps_tool_items() {
        let tool = map_tool_call(&json!({
            "id": "call-1", "type": "mcp_tool_call", "server": "Lyra", "tool": "read",
            "arguments": { "path": "src/main.rs" }, "status": "completed",
            "result": { "content": [{ "type": "text", "text": "ok" }] }
        }));
        assert_eq!(tool.kind, "read");
        assert!(tool.title.starts_with("Lyra / read"));
        let bash = map_tool_call(&json!({
            "id": "call-2", "type": "command_execution", "status": "completed",
            "command": "ls", "aggregated_output": "a.rs"
        }));
        assert_eq!(bash.kind, "execute");
        assert_eq!(bash.raw_input.as_ref().unwrap()["command"], "ls");
        assert_eq!(bash.content[0]["content"]["text"], "a.rs");
        let edit = map_tool_call(&json!({
            "id": "call-3", "type": "file_change", "status": "completed",
            "changes": [{"path": "src/main.rs"}]
        }));
        assert_eq!(edit.kind, "edit");
        assert_eq!(edit.locations, vec![json!({"path": "src/main.rs"})]);
    }

    #[test]
    fn prompt_keeps_inline_images_and_file_uris() {
        let images: Vec<crate::threads::PromptImage> = serde_json::from_value(json!([
            {"name":"screen.png", "mimeType":"image/png", "data":"cG5n"},
            {"name":"photo.jpg", "mimeType":"image/jpeg", "uri":"file:///tmp/photo.jpg"}
        ])).unwrap();
        assert_eq!(prompt_parts("read these", &images), vec![
            json!({"type":"text", "text":"read these"}),
            json!({"type":"image_data", "name":"screen.png", "mime":"image/png", "data":"cG5n"}),
            json!({"type":"local_image", "path":"/tmp/photo.jpg"})
        ]);
    }

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
    fn lyra_usage_includes_pi_cached_input() {
        let raw = json!({
            "input": 100,
            "output": 20,
            "cacheRead": 300,
            "cacheWrite": 40,
            "contextTokens": 50
        });
        let usage = normalize_usage(Some(&raw));

        assert_eq!(
            usage,
            Some(json!({
                "inputTokens": 440,
                "outputTokens": 20,
                "totalTokens": 460,
                "cacheReadTokens": 300,
                "cacheWriteTokens": 40,
                "contextTokens": 50
            }))
        );
        for raw in [serde_json::Value::Null, json!({"input": 1}), json!({"input": -1, "output": 2})] {
            assert!(normalize_usage(Some(&raw)).is_none());
        }
        assert!(normalize_usage(None).is_none());
    }

    #[test]
    fn working_directory_hides_windows_verbatim_prefix() {
        assert_eq!(
            display_working_directory(std::path::Path::new(r"\\?\D:\code\nova-client")),
            r"D:\code\nova-client"
        );
        assert_eq!(
            display_working_directory(std::path::Path::new(r"\\?\UNC\server\share\repo")),
            r"\\server\share\repo"
        );
    }

    #[tokio::test]
    async fn native_control_forwards_messages_and_reports_closed_channel() {
        let (control, mut events) = tokio::sync::mpsc::unbounded_channel();
        for request in [json!({"action":"cancel"}), json!({"action":"steer","parts":[]})] {
            write_control(&control, &request).await.unwrap();
            assert_eq!(events.recv().await.unwrap(), request.to_string());
        }
        drop(events);
        assert!(write_control(&control, &json!({"action":"cancel"})).await.is_err());
    }

    #[test]
    fn normalizes_title_response() {
        assert_eq!(
            normalize_title("`修复标题路由。`", "fallback"),
            "修复标题路由"
        );
        assert_eq!(normalize_title("  ", "fallback"), "fallback");
    }

    #[test]
    fn turn_completion_finishes_pending_sdk_tools() {
        let mut thread = Thread::new(".".into(), AgentKind::Lyra, None, None, None, false);
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
    fn lyra_text_snapshots_become_deltas_when_they_only_append() {
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

}
