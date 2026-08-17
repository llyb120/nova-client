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

/// 预热保留位：idle_children 里不属于任何线程的预热 bridge 用这个键存放，
/// 新线程首轮 prompt 直接认领，避免 Node 启动 + Agent.create 冷启动。
const PREWARM_KEY: &str = "__nova_prewarm__";

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

/// 回合控制通道：进程桥写 stdin，进程内桥写 mpsc（同样的 JSONL 控制行）。
#[derive(Clone)]
enum BridgeControl {
    Process(Arc<tokio::sync::Mutex<ChildStdin>>),
    InProcess(tokio::sync::mpsc::UnboundedSender<String>),
}

struct RunningBridge {
    control: BridgeControl,
    pid: Option<u32>,
    /// 进程内桥的注册代数（run_epoch），用于区分同一 session 的新旧回合；进程桥为 0。
    registration: u64,
    /// 进程内任务的 abort 句柄，等价于进程桥的 kill。
    abort: Option<tokio::task::AbortHandle>,
}

impl RunningBridge {
    fn identity(&self) -> (Option<u32>, u64) {
        (self.pid, self.registration)
    }
}

fn kill_running(bridge: &RunningBridge) {
    if let Some(pid) = bridge.pid {
        crate::acp::kill_process_tree(pid);
    }
    if let Some(abort) = &bridge.abort {
        abort.abort();
    }
}

struct IdleBridge {
    child: Child,
    stdin: Arc<tokio::sync::Mutex<ChildStdin>>,
    stdout: BufReader<tokio::process::ChildStdout>,
    stderr: Arc<Mutex<Vec<String>>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ReadEventsOutcome {
    Completed,
    Superseded,
}

/// 回合事件行来源：进程桥 stdout 或进程内 mpsc 通道，均为 JSONL。
enum EventSource<'a> {
    Process(&'a mut BufReader<tokio::process::ChildStdout>),
    Channel(tokio::sync::mpsc::UnboundedReceiver<String>),
}

impl EventSource<'_> {
    async fn next_line(&mut self) -> Result<Option<String>, String> {
        match self {
            EventSource::Process(reader) => {
                reader.lines().next_line().await.map_err(|e| e.to_string())
            }
            EventSource::Channel(rx) => Ok(rx.recv().await),
        }
    }
}

pub struct SdkManager {
    app: AppHandle,
    adapter: Arc<dyn SdkAdapter>,
    launch_env: HashMap<String, String>,
    /// 补全直连 HTTP 复用连接池，避免每次冷建 TLS。
    http: reqwest::Client,
    running_children: Mutex<HashMap<String, RunningBridge>>,
    idle_children: Mutex<HashMap<String, IdleBridge>>,
    /// 最新一次预热请求（后到覆盖先到）；持有 prewarm_gate 的循环负责逐个消化。
    prewarm_pending: Mutex<Option<Value>>,
    prewarm_gate: tokio::sync::Mutex<()>,
    running: Mutex<HashSet<String>>,
    turn_started: Mutex<HashMap<String, Instant>>,
    pending_permissions: Mutex<HashMap<String, (String, String)>>,
    model_options: Mutex<Option<Value>>,
    model_options_refreshing: AtomicBool,
    model_options_revalidated: AtomicBool,
    /// 本地 Vega 配置变化代数，避免旧模型列表覆盖新配置结果。
    alkaid_config_generation: AtomicU64,
    /// 进程内原生 agent（Lyra）的 HTTP 连接池：按 vega_proxy 设置构建一次并复用。
    native_http: Mutex<Option<reqwest::Client>>,
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
        let http = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(5))
            .pool_max_idle_per_host(4)
            .tcp_keepalive(std::time::Duration::from_secs(20))
            .build()
            .unwrap_or_default();
        Arc::new(Self {
            app,
            adapter: Arc::new(adapter),
            launch_env,
            http,
            running_children: Mutex::new(HashMap::new()),
            idle_children: Mutex::new(HashMap::new()),
            prewarm_pending: Mutex::new(None),
            prewarm_gate: tokio::sync::Mutex::new(()),
            running: Mutex::new(HashSet::new()),
            turn_started: Mutex::new(HashMap::new()),
            pending_permissions: Mutex::new(HashMap::new()),
            model_options: Mutex::new(None),
            model_options_refreshing: AtomicBool::new(false),
            model_options_revalidated: AtomicBool::new(false),
            alkaid_config_generation: AtomicU64::new(0),
            native_http: Mutex::new(None),
            next_run_epoch: AtomicU64::new(1),
            run_epochs: Mutex::new(HashMap::new()),
        })
    }

    /// 主运行时用进程内原生 agent：adapter 声明支持且非借用额度隔离运行时。
    /// 进程内原生 agent（Lyra）：主运行时与借用额度运行时都在本进程 tokio 任务中运行。
    /// 借用额度只换数据根（不同凭证），无需进程隔离。
    fn use_inprocess(&self) -> bool {
        self.adapter.runs_inprocess()
    }

    /// 借用额度运行时的隔离数据根（launch_env 中的 NOVA_DATA_DIR）；主运行时为 None。
    fn borrowed_root(&self) -> Option<PathBuf> {
        self.launch_env.get("NOVA_DATA_DIR").map(PathBuf::from)
    }

    /// 进程内原生 agent 的 HTTP 连接池：按 vega_proxy 设置构建一次复用，避免每轮冷建 TLS。
    fn native_http(&self) -> reqwest::Client {
        if let Some(client) = self.native_http.lock().unwrap().as_ref() {
            return client.clone();
        }
        let proxy = {
            let state = self.app.state::<AppState>();
            let proxy = state.settings.lock().unwrap().vega_proxy.clone();
            proxy
        };
        let mut builder = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .pool_max_idle_per_host(4)
            .tcp_keepalive(std::time::Duration::from_secs(20));
        let proxy = proxy.trim();
        if !proxy.is_empty() {
            let url = if proxy.contains("://") {
                proxy.to_string()
            } else {
                format!("http://{proxy}")
            };
            if let Ok(proxy) = reqwest::Proxy::all(&url) {
                builder = builder.proxy(proxy);
            }
        }
        let client = builder.build().unwrap_or_default();
        *self.native_http.lock().unwrap() = Some(client.clone());
        client
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
        let lightweight_model = if matches!(
            self.adapter.agent_kind(),
            AgentKind::Alkaid | AgentKind::Lyra
        ) {
            let app_state = self.app.state::<AppState>();
            let settings = app_state.settings.lock().unwrap();
            (AgentKind::from_str(&settings.lightweight_model_agent) == Some(AgentKind::Alkaid))
                .then(|| settings.lightweight_model.trim().to_string())
                .filter(|model| !model.is_empty())
        } else {
            None
        };
        let mut request = json!({
            "action": "prompt",
            "threadId": thread_id,
            "cwd": cwd,
            "sessionId": session_id,
            "restoreAt": native_restore.as_ref().map(|restore| &restore.position),
            "model": model,
            "mode": mode,
            "reasoningEffort": reasoning_effort,
            "lightweightModel": lightweight_model,
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
            // Cursor super context keeps its own completed-history lifecycle. Treat an SDK error
            // as an unfinished same-agent handoff so the next prompt also receives the failed user
            // request and every assistant/tool item that reached Thread before the bridge exited.
            let state = self.app.state::<AppState>();
            let cursor_super_context = self.adapter.agent_kind() == AgentKind::Cursor
                && state.settings.lock().unwrap().cursor_context_mode == "super";
            if cursor_super_context {
                let mut store = state.store.lock().unwrap();
                if let Some(thread) = store.get_mut(&thread_id) {
                    thread.handoff_from = Some(AgentKind::Cursor);
                }
                store.save();
            }
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
            for _ in 0..self.adapter.cancel_grace_attempts() {
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
            let in_process = matches!(bridge.control, BridgeControl::InProcess(_));
            kill_running(&bridge);
            // 进程内（Lyra）bridge 的中断轨迹（pendingMessages）只在轮次收尾时写盘；
            // 宽限期内没能自行结束（典型：卡在长时间工具执行里，工具不响应取消），
            // 被强制 abort 意味着这次中断内容没来得及持久化。回退到 Nova 侧接力上下文，
            // 把已流出的中断轮注入下一条提示，避免续聊时上下文退回上次结论。
            if in_process {
                let state = self.app.state::<AppState>();
                let mut store = state.store.lock().unwrap();
                if let Some(thread) = store.get_mut(thread_id) {
                    thread.handoff_from = Some(self.adapter.agent_kind());
                }
                store.save();
            }
        }
        self.finish_turn(thread_id, "cancelled", None);
    }

    /// 支持原生 steer 的 SDK 直接把用户消息排入当前 Agent run；其他 SDK
    /// （如 Cursor：复用 live Agent session，但无原生 steer）仍静默结束当前流，再开新 turn。
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
        if let Err(error) =
            write_control(&control, &json!({ "action": "steer", "parts": parts })).await
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
            .map(|bridge| (bridge.control.clone(), bridge.pid, bridge.abort.clone()));
        if let Some((control, pid, abort)) = bridge {
            // Bridge cancellation and persistence are best-effort only. The replacement prompt
            // receives the already-streamed transcript from Thread, so do not wait for SDK cleanup.
            if let Some(pid) = pid {
                crate::acp::kill_process_tree(pid);
            } else if let Some(abort) = abort {
                abort.abort();
            } else {
                let _ = write_control(&control, &json!({ "action": "cancel" })).await;
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
            // Force the replacement prompt through Nova's handoff renderer. This captures the
            // interrupted user message plus all assistant/tool events already streamed to Thread,
            // independently of the cancelled provider run or its asynchronous memory file.
            thread.handoff_from = Some(self.adapter.agent_kind());
        }
        store.save();
    }

    pub fn forget_session_of_thread(&self, thread_id: &str) {
        if let Some(bridge) = self.running_children.lock().unwrap().remove(thread_id) {
            kill_running(&bridge);
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
            kill_running(&bridge);
        }
        for mut bridge in std::mem::take(&mut *self.idle_children.lock().unwrap()).into_values() {
            kill_child(&mut bridge.child);
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

    /// 本地 `config.jsonc` 发生变化时调用。当前正在执行的 bridge 不打断，
    /// 但会让下一轮请求、模型列表和预热实例使用新配置。
    pub fn notify_alkaid_config_changed(self: &Arc<Self>) {
        if !matches!(
            self.adapter.agent_kind(),
            AgentKind::Alkaid | AgentKind::Lyra
        ) {
            return;
        }
        self.invalidate_alkaid_config();
    }

    fn invalidate_alkaid_config(self: &Arc<Self>) {
        crate::alkaid_complete::invalidate_config_cache();
        self.alkaid_config_generation.fetch_add(1, Ordering::SeqCst);
        *self.model_options.lock().unwrap() = None;
        let _ = self.app.emit(
            EV_OPTIONS,
            json!({
                "agentKind": self.adapter.agent_kind().as_str(),
                "options": self.pending_model_options(),
            }),
        );
        for mut bridge in std::mem::take(&mut *self.idle_children.lock().unwrap()).into_values() {
            kill_child(&mut bridge.child);
        }
        self.refresh_model_options_soon();
    }

    /// 出借 Vega 额度：跑一次 bridge 导出本地 config.jsonc 的生效配置，
    /// 并把 {env:NAME} 密钥占位符解析成字面量，
    /// 借用方无需出借方的环境变量即可直接使用该配置。
    pub async fn export_quota_credentials(&self) -> Result<String, String> {
        if self.adapter.agent_kind() != AgentKind::Alkaid {
            return Err("仅 Vega 支持导出共享凭证".into());
        }
        let cwd = std::env::current_dir()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default();
        let value = self.run_bridge(&cwd, json!({ "action": "export" })).await?;
        value
            .as_str()
            .filter(|config| !config.trim().is_empty())
            .map(str::to_string)
            .ok_or_else(|| "Vega 凭证导出结果无效".into())
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
                    format!("[{}] 拉取模型列表失败：{error}", manager.adapter.label()),
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

    fn empty_model_options(&self) -> Value {
        self.adapter.empty_model_options()
    }

    /// 真实列表还没拉到时给前端的占位。带 pending 标记，前端才知道这不是最终结果，
    /// 下次打开选择器可以再问一次，而不是把空列表当成已加载缓存住。
    fn pending_model_options(&self) -> Value {
        let mut value = self.empty_model_options();
        if let Some(object) = value.as_object_mut() {
            object.insert("pending".into(), Value::Bool(true));
        }
        value
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
        model_cache::save(&crate::nova_data_dir(&self.app), kind.as_str(), &value);
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

    /// 一次性行内补全：Rust 直连 provider HTTP（不冷启 Node）。
    /// 仅 openai-completions / openai-responses；其它协议再回退 bridge。
    pub async fn complete_once(
        &self,
        cwd: &str,
        model: &str,
        prompt: String,
    ) -> Result<String, String> {
        let data_dir = nova_data_dir(&self.app);
        match crate::alkaid_complete::complete_direct(
            &self.http,
            &data_dir,
            &self.launch_env,
            model,
            &prompt,
        )
        .await
        {
            Ok(text) => return Ok(text),
            Err(error)
                if error.contains("补全暂不支持直连协议")
                    || error.contains("Vega provider 缺少 api") =>
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

    async fn run_bridge(&self, cwd: &str, request: Value) -> Result<Value, String> {
        if self.use_inprocess() {
            return crate::lyra::run_oneshot(&self.native_http(), &request, self.borrowed_root())
                .await;
        }
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
        if self.use_inprocess() {
            return self
                .run_prompt_inprocess(thread_id, request, user_item_id, run_epoch)
                .await;
        }
        let cached_bridge = self
            .adapter
            .keeps_bridge_alive()
            .then(|| {
                let mut idle = self.idle_children.lock().unwrap();
                idle.remove(thread_id)
                    // 新线程首轮：认领草稿页空闲期预热好的 bridge（进程 + Agent 已就绪）。
                    .or_else(|| idle.remove(PREWARM_KEY))
            })
            .flatten();
        let reused_cached_bridge = cached_bridge.is_some();
        let mut bridge = match cached_bridge {
            Some(bridge) => bridge,
            None => self.spawn_idle_bridge(cwd)?,
        };
        let mut pid = bridge.child.id();
        self.running_children.lock().unwrap().insert(
            thread_id.to_string(),
            RunningBridge {
                control: BridgeControl::Process(bridge.stdin.clone()),
                pid,
                registration: 0,
                abort: None,
            },
        );
        if let Err(first_error) = write_line(&bridge.stdin, &request).await {
            {
                let mut running = self.running_children.lock().unwrap();
                if running
                    .get(thread_id)
                    .is_some_and(|running_bridge| running_bridge.pid == pid)
                {
                    running.remove(thread_id);
                }
            }
            kill_child(&mut bridge.child);
            // A kept-alive Cursor bridge can exit between turns (or while an interrupted turn is
            // being replaced). Treat that cached pipe as stale and retry once with a fresh bridge.
            if !reused_cached_bridge || !self.is_current_run(thread_id, run_epoch) {
                return Err(first_error);
            }
            bridge = self.spawn_idle_bridge(cwd)?;
            pid = bridge.child.id();
            self.running_children.lock().unwrap().insert(
                thread_id.to_string(),
                RunningBridge {
                    control: BridgeControl::Process(bridge.stdin.clone()),
                    pid,
                    registration: 0,
                    abort: None,
                },
            );
            if let Err(error) = write_line(&bridge.stdin, &request).await {
                let mut running = self.running_children.lock().unwrap();
                if running
                    .get(thread_id)
                    .is_some_and(|running_bridge| running_bridge.pid == pid)
                {
                    running.remove(thread_id);
                }
                drop(running);
                kill_child(&mut bridge.child);
                return Err(error);
            }
        }
        let mut source = EventSource::Process(&mut bridge.stdout);
        let event_result = self
            .read_events(thread_id, user_item_id, run_epoch, &mut source)
            .await;
        let completed = matches!(&event_result, Ok(ReadEventsOutcome::Completed));
        let still_owned = {
            let mut running = self.running_children.lock().unwrap();
            if running
                .get(thread_id)
                .is_some_and(|running_bridge| running_bridge.pid == pid)
            {
                running.remove(thread_id);
                true
            } else {
                false
            }
        };
        let result = event_result.map(|_| ()).map_err(|error| {
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
            && completed
            && still_owned
            && bridge.child.try_wait().ok().flatten().is_none();
        if reusable {
            // `done` clears this run's epoch before event reading returns. Holding the epoch lock
            // while caching closes the window where a newer run could start and miss this bridge.
            let epochs = self.run_epochs.lock().unwrap();
            if epochs.contains_key(thread_id) {
                drop(epochs);
                kill_child(&mut bridge.child);
            } else {
                self.idle_children
                    .lock()
                    .unwrap()
                    .insert(thread_id.to_string(), bridge);
            }
        } else if result.is_err() || !still_owned || !completed {
            // Superseded runs can return successfully after observing their invalidated epoch, but
            // their bridge is being cancelled and must never be cached for the replacement turn.
            kill_child(&mut bridge.child);
        }
        result
    }

    /// 进程内运行 Lyra：不起子进程，会话在同进程 tokio 任务中执行，
    /// 事件/控制行走 mpsc 通道，事件处理与进程桥完全同一路径。
    async fn run_prompt_inprocess(
        &self,
        thread_id: &str,
        request: Value,
        user_item_id: u64,
        run_epoch: u64,
    ) -> Result<(), String> {
        let context_tools = {
            let state = self.app.state::<AppState>();
            let enabled = state.settings.lock().unwrap().context_tools_enabled();
            enabled
        };
        let session = crate::lyra::spawn_prompt(
            self.native_http(),
            request,
            context_tools,
            self.borrowed_root(),
        );
        let abort = session.task.abort_handle();
        self.running_children.lock().unwrap().insert(
            thread_id.to_string(),
            RunningBridge {
                control: BridgeControl::InProcess(session.control.clone()),
                pid: None,
                registration: run_epoch,
                abort: Some(abort.clone()),
            },
        );
        let crate::lyra::InProcessSession { events, task, .. } = session;
        let mut source = EventSource::Channel(events);
        let event_result = self
            .read_events(thread_id, user_item_id, run_epoch, &mut source)
            .await;
        // 仅当注册项仍属于本回合时才移除（新回合可能已重新注册）。
        {
            let mut running = self.running_children.lock().unwrap();
            if running
                .get(thread_id)
                .is_some_and(|bridge| bridge.identity() == (None, run_epoch))
            {
                running.remove(thread_id);
            }
        }
        let result = event_result.map(|_| ());
        // 错误均已作为 {ok:false} 事件流出；这里只打捞 panic 信息，interrupt 的 abort 属正常。
        match tokio::time::timeout(std::time::Duration::from_secs(5), task).await {
            Ok(Err(join_error)) if join_error.is_panic() => Err(format!(
                "{}；Lyra 任务 panic：{join_error}",
                result
                    .err()
                    .unwrap_or_else(|| "Lyra 任务异常结束".to_string())
            )),
            Err(_) => {
                abort.abort();
                result
            }
            _ => result,
        }
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

    /// 草稿页空闲期预热：提前拉起 bridge（Node 进程）并完成 SDK Agent.create，
    /// 把首轮关键路径上的进程启动与 Agent 创建开销提前到用户还在选项目/模型时。
    /// 最新请求生效：模型/cwd/模式连续变化时，飞行中的循环会接着处理最新一条。
    pub fn prewarm_idle(self: &Arc<Self>, cwd: String, model: String, mode: String) {
        if !self.adapter.supports_idle_prewarm() {
            return;
        }
        // Cursor 用空字符串表示 Auto；bridge 会把它转换为 { id: "auto" }。这里不能把空值
        // 当成“未选模型”跳过，否则最常用的 Auto 首轮永远无法预热，Agent.create 会阻塞首字。
        let request = json!({
            "action": "prewarm",
            "cwd": cwd,
            "model": model,
            "mode": mode,
        });
        *self.prewarm_pending.lock().unwrap() = Some(request);
        let manager = self.clone();
        tauri::async_runtime::spawn(async move {
            manager.prewarm_idle_loop().await;
        });
    }

    async fn prewarm_idle_loop(&self) {
        // 已有一个循环在飞时直接退出：它每轮都会取走最新的 pending 请求。
        let Ok(_gate) = self.prewarm_gate.try_lock() else {
            return;
        };
        loop {
            let Some(request) = self.prewarm_pending.lock().unwrap().take() else {
                break;
            };
            let cwd = request["cwd"].as_str().unwrap_or_default().to_string();
            let label = self.adapter.label();
            match self.prewarm_once(&cwd, &request).await {
                Ok((ready, elapsed_ms)) => {
                    let _ = self.app.emit(
                        EV_LOG,
                        format!("[{label}][timing] prewarm {elapsed_ms}ms ready={ready}"),
                    );
                }
                Err(error) => {
                    let _ = self
                        .app
                        .emit(EV_LOG, format!("[{label}] 预热失败：{error}"));
                }
            }
        }
    }

    async fn prewarm_once(&self, cwd: &str, request: &Value) -> Result<(bool, u64), String> {
        let mut bridge = match self.idle_children.lock().unwrap().remove(PREWARM_KEY) {
            Some(bridge) => bridge,
            None => self.spawn_idle_bridge(cwd)?,
        };
        if let Err(error) = write_line(&bridge.stdin, request).await {
            kill_child(&mut bridge.child);
            return Err(error);
        }
        let read = async {
            let stdout = &mut bridge.stdout;
            let mut lines = stdout.lines();
            while let Some(line) = lines.next_line().await.map_err(|e| e.to_string())? {
                let event: Value = serde_json::from_str(&line)
                    .map_err(|e| format!("解析预热事件失败：{e}；输出：{line}"))?;
                if event.get("ok").and_then(Value::as_bool) == Some(false) {
                    return Err(event["error"]
                        .as_str()
                        .unwrap_or("SDK bridge 预热失败")
                        .to_string());
                }
                if event.get("type").and_then(Value::as_str) == Some("prewarmed") {
                    let ready = event.get("ready").and_then(Value::as_bool).unwrap_or(false);
                    let elapsed_ms = event.get("elapsedMs").and_then(Value::as_u64).unwrap_or(0);
                    return Ok((ready, elapsed_ms));
                }
            }
            Err("bridge 在预热完成前退出".to_string())
        };
        let outcome = tokio::time::timeout(std::time::Duration::from_secs(45), read).await;
        match outcome {
            Ok(Ok(result)) => {
                self.idle_children
                    .lock()
                    .unwrap()
                    .insert(PREWARM_KEY.to_string(), bridge);
                Ok(result)
            }
            Ok(Err(error)) => {
                kill_child(&mut bridge.child);
                Err(error)
            }
            Err(_) => {
                kill_child(&mut bridge.child);
                Err("预热超时".to_string())
            }
        }
    }

    async fn read_events(
        &self,
        thread_id: &str,
        user_item_id: u64,
        run_epoch: u64,
        source: &mut EventSource<'_>,
    ) -> Result<ReadEventsOutcome, String> {
        let mut item_ids = HashMap::new();
        while let Some(line) = source.next_line().await? {
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
                        format!(
                            "[{}][timing] {phase} {elapsed_ms}ms{cancelled_runs}",
                            self.adapter.label()
                        ),
                    );
                }
                Some("item") => self.apply_item(thread_id, &event["item"], &mut item_ids),
                Some("working_directory_changed") => {
                    if let Some(cwd) = event.get("cwd").and_then(Value::as_str) {
                        self.change_working_directory(thread_id, cwd);
                    }
                }
                Some("plan") => self.apply_plan(thread_id, &event["plan"]),
                Some("checkpoint") => self.save_checkpoint(thread_id, user_item_id, &event),
                Some("permission") => self.emit_permission(thread_id, &event["permission"]),
                Some("usage") => {
                    // 本轮进行中的累计用量（bridge 可选上报）：只推给前端实时展示，不落库；
                    // Turn 落库时前端清零该值，避免与轮次用量重复计。
                    if let Some(raw) = event.get("usage") {
                        let (usage, _) = self.adapter.normalize_usage(Some(raw), None, None);
                        if let Some(usage) = usage {
                            let _ =
                                self.emit_op(thread_id, json!({ "t": "usage", "usage": usage }));
                        }
                    }
                }
                Some("done") => {
                    let usage = event.get("usage").cloned();
                    let stop_reason = if self.adapter.done_is_cancelled(&event) {
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
        // Rust 原生 agent（如 Lyra）：直接以应用自身子命令启动，不经 Node bridge。
        let native_subcommand = self.adapter.native_subcommand();
        let mut command = if let Some(subcommand) = native_subcommand {
            let exe =
                std::env::current_exe().map_err(|e| format!("定位应用可执行文件失败：{e}"))?;
            let mut command = Command::new(exe);
            command.arg(subcommand);
            command
        } else {
            let node = resolve_program_on_path("node").ok_or_else(|| {
                format!(
                    "未找到 Node.js，{} 需要 Node.js 运行官方 SDK",
                    self.adapter.label()
                )
            })?;
            let bridge = bridge_path(&self.app, self.adapter.as_ref())?;
            let mut command = Command::new(node);
            command.arg(bridge);
            command
        };
        command
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env(launch.path_env, &program)
            // Node bridges also persist app-owned state. Pin them to the same profile-specific
            // root as Rust so debug builds never fall back to the release ~/.nova directory.
            .env("NOVA_DATA_DIR", nova_data_dir(&self.app));
        if self.adapter.agent_kind() == AgentKind::Alkaid {
            let exe = std::env::current_exe()
                .map_err(|e| format!("定位 Vega 内嵌 RTK 可执行文件失败：{e}"))?;
            command.env("NOVA_RTK_EXE", exe);
        }
        {
            let state = self.app.state::<AppState>();
            let settings = state.settings.lock().unwrap();
            command
                .env(
                    "NOVA_CONTEXT_SERVICE_ENDPOINT",
                    state.context_service.endpoint(),
                )
                .env("NOVA_CONTEXT_SERVICE_TOKEN", state.context_service.token())
                .env(
                    "NOVA_CONTEXT_RETRIEVAL_MODE",
                    settings.context_retrieval_mode.as_str(),
                );
        }
        if !self.launch_env.is_empty() {
            crate::credential_roaming::isolate_borrowed_command(&mut command);
            command.envs(&self.launch_env);
        }
        apply_proxy_env(&mut command, &launch.proxy);
        command.envs(launch.extra_env);
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
        command.spawn().map_err(|e| {
            if native_subcommand.is_some() {
                format!("启动 {} 原生进程失败：{e}", self.adapter.label())
            } else {
                format!("启动 {} Node bridge 失败：{e}", self.adapter.label())
            }
        })
    }

    fn change_working_directory(&self, thread_id: &str, cwd: &str) {
        let path = std::path::PathBuf::from(cwd);
        let canonical = std::fs::canonicalize(&path).unwrap_or(path);
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
            store.save();
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
                let mut item = item;
                if let (Item::Tool { ts: started_at, .. }, Item::Tool { ts, call, .. }) =
                    (&*slot, &mut item)
                {
                    *ts = *started_at;
                    if matches!(call.status.as_str(), "completed" | "failed") {
                        set_tool_duration(call, now_ms().saturating_sub(*started_at) as u64);
                    }
                }
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
        let control = self
            .running_children
            .lock()
            .unwrap()
            .get(&thread_id)
            .map(|bridge| bridge.control.clone())
            .ok_or_else(|| format!("{} 会话已结束", self.adapter.label()))?;
        write_control(&control, &json!({ "action": "permission", "requestId": request_id, "reply": if option_id == "reject" { "reject" } else { "once" } })).await?;
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
    crate::nova_tools_napi_asset::materialize(&dir)?;
    let path = dir.join(name);
    if std::fs::read(&path).ok().as_deref() != Some(bridge) {
        std::fs::write(&path, bridge)
            .map_err(|e| format!("释放 {} bridge 失败：{e}", adapter.label()))?;
    }
    for (sidecar_name, sidecar) in adapter.bridge_sidecars() {
        let sidecar_path = dir.join(sidecar_name);
        if std::fs::read(&sidecar_path).ok().as_deref() != Some(sidecar) {
            std::fs::write(&sidecar_path, sidecar).map_err(|e| {
                format!("释放 {} sidecar {sidecar_name} 失败：{e}", adapter.label())
            })?;
        }
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

async fn write_control(control: &BridgeControl, line: &Value) -> Result<(), String> {
    match control {
        BridgeControl::Process(stdin) => write_line(stdin, line).await,
        BridgeControl::InProcess(tx) => tx
            .send(line.to_string())
            .map_err(|_| "Lyra 会话控制通道已关闭".to_string()),
    }
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

fn display_working_directory(path: &std::path::Path) -> String {
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
        codex_todo_plan, complete_pending_tools, derive_title, display_working_directory,
        is_codex_model_resume_warning, normalize_title, parse_bridge_output, resolve_codex_model,
        text_snapshot_change, tool_call, TextSnapshotChange,
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
            cache_read_tokens: 200,
            cache_write_tokens: 0,
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
            Some(json!({
                "inputTokens": 600,
                "outputTokens": 80,
                "totalTokens": 680,
                "cacheReadTokens": 300,
                "cacheWriteTokens": 0
            }))
        );
        let snapshot = snapshot.unwrap();
        assert_eq!(snapshot.input_tokens, 1_600);
        assert_eq!(snapshot.cache_read_tokens, 500);
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
            ..Default::default()
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
            Some(json!({
                "inputTokens": 440,
                "outputTokens": 20,
                "totalTokens": 460,
                "cacheReadTokens": 300,
                "cacheWriteTokens": 40
            }))
        );
    }

    #[test]
    fn cursor_usage_maps_disjoint_bridge_usage_and_includes_cached_input() {
        let raw = json!({
            "inputTokens": 100,
            "outputTokens": 20,
            "cacheReadTokens": 300,
            "cacheWriteTokens": 40
        });
        let (usage, _) = CursorAdapter.normalize_usage(Some(&raw), None, None);

        assert_eq!(
            usage,
            Some(json!({
                "inputTokens": 440,
                "outputTokens": 20,
                "totalTokens": 460,
                "cacheReadTokens": 300,
                "cacheWriteTokens": 40
            }))
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
                Some(json!({
                    "inputTokens": 440,
                    "outputTokens": 20,
                    "totalTokens": 460,
                    "cacheReadTokens": 300,
                    "cacheWriteTokens": 40
                }))
            );
        }

        let partial = json!({ "input_tokens": 100 });
        let (usage, _) = ClaudeAdapter.normalize_usage(Some(&partial), None, None);
        assert!(usage.is_none());
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

        let read_files = tool_call(&json!({
            "id": "read-files", "type": "mcp_tool_call", "server": "Cursor", "tool": "mcp",
            "arguments": {
                "args": { "paths": [
                    { "path": "src/a.ts", "offset": 10, "limit": 20 },
                    "src/b.ts"
                ] },
                "providerIdentifier": "custom-user-tools",
                "toolName": "read_files"
            }, "status": "completed"
        }));
        assert_eq!(read_files.title, "Cursor / read_files · src/a.ts");
        assert_eq!(read_files.kind, "read");
        assert_eq!(read_files.locations[0]["path"], "src/a.ts");
        assert_eq!(read_files.locations[1]["path"], "src/b.ts");
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
            let raw_arguments = value.get("arguments");
            let envelope = raw_arguments
                .filter(|arguments| arguments.get("toolName").and_then(Value::as_str).is_some());
            let tool = envelope
                .and_then(|arguments| arguments.get("toolName"))
                .and_then(Value::as_str)
                .or_else(|| value.get("tool").and_then(Value::as_str))
                .unwrap_or("tool");
            let arguments = envelope
                .and_then(|arguments| arguments.get("args"))
                .or(raw_arguments);
            let detail = match tool {
                "shell" => arguments.and_then(|args| args.get("command")),
                "read" | "edit" | "write" | "delete" | "ls" => {
                    arguments.and_then(|args| args.get("path"))
                }
                "glob" => arguments.and_then(|args| args.get("globPattern")),
                "grep" => arguments.and_then(|args| args.get("pattern")),
                "semSearch" => arguments.and_then(|args| args.get("query")),
                "read_files" => arguments
                    .and_then(|args| args.get("paths"))
                    .and_then(Value::as_array)
                    .and_then(|paths| paths.first())
                    .and_then(|path| {
                        if path.is_string() {
                            Some(path)
                        } else {
                            path.get("path")
                        }
                    }),
                _ => None,
            }
            .and_then(Value::as_str)
            .map(compact_tool_detail)
            .filter(|detail| !detail.is_empty());
            let locations = arguments
                .and_then(|args| args.get("paths"))
                .and_then(Value::as_array)
                .map(|paths| {
                    paths
                        .iter()
                        .filter_map(|path| {
                            path.as_str()
                                .or_else(|| path.get("path").and_then(Value::as_str))
                        })
                        .map(|path| json!({ "path": path }))
                        .collect::<Vec<_>>()
                })
                .filter(|locations| !locations.is_empty())
                .or_else(|| {
                    arguments
                        .and_then(|args| args.get("path"))
                        .and_then(Value::as_str)
                        .map(|path| vec![json!({ "path": path })])
                })
                .unwrap_or_default();
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
                    "read" | "read_files" => "read",
                    "edit" | "write" | "edit_files" => "edit",
                    "delete" => "delete",
                    "glob" | "grep" | "semSearch" | "ls" => "search",
                    "createPlan" | "updateTodos" => "think",
                    _ => "other",
                },
                locations,
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
