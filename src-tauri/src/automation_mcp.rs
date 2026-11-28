//! 外部 MCP 自动化服务（单工具 `nova_automation`）：
//! 1. **主进程 Streamable HTTP MCP 服务端**（`127.0.0.1:<port>/mcp`）：
//!    - 强制校验回环 `Host`/`Origin` 与随机 Bearer Token（恒时比对）；
//!    - 支持标准 MCP `initialize` / `ping` / `tools/list` / `tools/call` / `DELETE`；
//!    - 把外部传入的 `intent` + `scope`（`chrome` / `desktop` / `both`）派发到可见的受限 Lyra 会话，
//!      固定使用视觉模型且工具列表仅含 Chrome / 剑来，`desktop` 任务通过全局互斥锁串行保护。
//! 2. **Stdio MCP 桥接子模式**（`Nova --mcp-automation`）：
//!    - 绕过 GUI 单实例锁，通过 stdio 与 Claude Code / Cursor / Codex / agy 等客户端通信，
//!      并将 `tools/call` 经本机回环 HTTP 转发给正在运行的 Nova 主进程。

use crate::acp::EV_THREADS;
use crate::antigravity::sidecar::constant_time_eq;
use crate::lyra::bridge::automation_tools_for_scope;
use crate::settings::{Scene, Settings};
use crate::threads::{AgentKind, Item, Thread};
use crate::AppState;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot, Notify};

pub const MCP_AUTOMATION_CLI_FLAG: &str = "--mcp-automation";
pub const ENV_MCP_AUTOMATION_URL: &str = "NOVA_AUTOMATION_MCP_URL";
pub const ENV_MCP_AUTOMATION_TOKEN: &str = "NOVA_AUTOMATION_MCP_TOKEN";
pub const DISCOVERY_FILE_NAME: &str = "automation-mcp.json";
pub const INTERNAL_DISCOVERY_FILE_NAME: &str = "automation-mcp-internal.json";
pub const AUTOMATION_TOOL_NAME: &str = "nova_automation";

pub const SERVER_NAME: &str = "nova-automation";
const DEFAULT_WAIT_SEC: u64 = 120;
const MAX_WAIT_SEC: u64 = 600;
const DESKTOP_LOCK_TIMEOUT: Duration = Duration::from_secs(3);
const PROGRESS_INTERVAL: Duration = Duration::from_secs(15);
const MAX_HTTP_HEADER_BYTES: usize = 32 * 1024;
const MAX_HTTP_BODY_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationMcpDiscovery {
    pub url: String,
    pub port: u16,
    pub token: String,
    pub pid: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationMcpStatus {
    pub enabled: bool,
    pub running: bool,
    pub port: u16,
    pub url: String,
    pub token: String,
    pub executable_path: String,
    pub error: Option<String>,
}

#[derive(Clone, Debug)]
pub struct BridgeCliConfig {
    pub url: Option<String>,
    pub token: Option<String>,
    pub data_dir: PathBuf,
    pub internal: bool,
}

#[derive(Default)]
struct RuntimeState {
    enabled: bool,
    configured_port: u16,
    bound_port: u16,
    url: String,
    token: String,
    error: Option<String>,
    shutdown_tx: Option<oneshot::Sender<()>>,
    task: Option<tauri::async_runtime::JoinHandle<()>>,
}

#[derive(Default)]
struct InternalRuntimeState {
    bound_port: u16,
    url: String,
    token: String,
    shutdown_tx: Option<oneshot::Sender<()>>,
    task: Option<tauri::async_runtime::JoinHandle<()>>,
}

struct AutomationSessionTrack {
    scope: String,
    last_start_item_id: u64,
    notify: Arc<Notify>,
}

pub struct AutomationMcpManager {
    app: AppHandle,
    config_dir: PathBuf,
    runtime: Mutex<RuntimeState>,
    internal: Mutex<InternalRuntimeState>,
    desktop_lock: Arc<tokio::sync::Mutex<()>>,
    sessions: Mutex<HashMap<String, AutomationSessionTrack>>,
}

impl AutomationMcpManager {
    pub fn new(app: AppHandle, config_dir: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            app,
            config_dir,
            runtime: Mutex::new(RuntimeState::default()),
            internal: Mutex::new(InternalRuntimeState::default()),
            desktop_lock: Arc::new(tokio::sync::Mutex::new(())),
            sessions: Mutex::new(HashMap::new()),
        })
    }

    pub fn sync_from_settings(self: &Arc<Self>, settings: &Settings) {
        if crate::server::is_headless() {
            return;
        }
        let want_enabled = settings.automation_mcp_enabled;
        let want_port = settings.automation_mcp_port;
        let want_token = settings.automation_mcp_token.trim().to_string();

        let (needs_restart, was_running) = {
            let guard = self.runtime.lock().unwrap();
            let running = guard.shutdown_tx.is_some();
            if !want_enabled {
                (false, running)
            } else {
                let same = running
                    && guard.enabled
                    && guard.configured_port == want_port
                    && guard.token == want_token;
                (!same, running)
            }
        };

        if !want_enabled {
            if was_running || !self.runtime.lock().unwrap().url.is_empty() {
                self.shutdown_external();
            } else {
                let mut guard = self.runtime.lock().unwrap();
                guard.enabled = false;
                guard.configured_port = want_port;
                guard.token = want_token;
                guard.error = None;
            }
            return;
        }

        if !needs_restart {
            return;
        }

        self.shutdown_external();
        if want_token.is_empty() {
            let mut guard = self.runtime.lock().unwrap();
            guard.enabled = true;
            guard.configured_port = want_port;
            guard.error = Some("缺少 MCP 鉴权 Token，请点击「重置 Token」后保存".into());
            return;
        }

        let this = self.clone();
        let (tx, rx) = oneshot::channel::<()>();
        let addr = format!("127.0.0.1:{want_port}");
        let listener = match std::net::TcpListener::bind(&addr) {
            Ok(l) => {
                if let Err(err) = l.set_nonblocking(true) {
                    let mut guard = self.runtime.lock().unwrap();
                    guard.enabled = true;
                    guard.configured_port = want_port;
                    guard.token = want_token;
                    guard.error = Some(format!("设置本地 MCP 服务非阻塞失败：{err}"));
                    return;
                }
                match TcpListener::from_std(l) {
                    Ok(listener) => listener,
                    Err(err) => {
                        let mut guard = self.runtime.lock().unwrap();
                        guard.enabled = true;
                        guard.configured_port = want_port;
                        guard.token = want_token;
                        guard.error = Some(format!("初始化本地 MCP 监听失败：{err}"));
                        return;
                    }
                }
            }
            Err(err) => {
                let mut guard = self.runtime.lock().unwrap();
                guard.enabled = true;
                guard.configured_port = want_port;
                guard.token = want_token;
                guard.error = Some(format!("绑定 127.0.0.1:{want_port} 失败：{err}"));
                return;
            }
        };

        let bound_port = listener.local_addr().map(|a| a.port()).unwrap_or(want_port);
        let url = format!("http://127.0.0.1:{bound_port}/mcp");
        let discovery = AutomationMcpDiscovery {
            url: url.clone(),
            port: bound_port,
            token: want_token.clone(),
            pid: std::process::id(),
        };
        if let Err(err) = write_discovery_file(&self.config_dir, &discovery) {
            eprintln!("[automation-mcp] 写入服务发现文件失败：{err}");
        }

        let token_for_task = want_token.clone();
        let task = tauri::async_runtime::spawn(async move {
            run_http_server(listener, this, token_for_task, rx).await;
        });

        let mut guard = self.runtime.lock().unwrap();
        guard.enabled = true;
        guard.configured_port = want_port;
        guard.bound_port = bound_port;
        guard.url = url;
        guard.token = want_token;
        guard.error = None;
        guard.shutdown_tx = Some(tx);
        guard.task = Some(task);
    }

    fn shutdown_external(&self) {
        let (tx, task) = {
            let mut guard = self.runtime.lock().unwrap();
            guard.enabled = false;
            guard.bound_port = 0;
            guard.url.clear();
            guard.error = None;
            (guard.shutdown_tx.take(), guard.task.take())
        };
        if let Some(tx) = tx {
            let _ = tx.send(());
        }
        if let Some(task) = task {
            task.abort();
        }
        remove_discovery_file(&self.config_dir);
    }

    fn shutdown_internal(&self) {
        let (tx, task) = {
            let mut guard = self.internal.lock().unwrap();
            guard.bound_port = 0;
            guard.url.clear();
            guard.token.clear();
            (guard.shutdown_tx.take(), guard.task.take())
        };
        if let Some(tx) = tx {
            let _ = tx.send(());
        }
        if let Some(task) = task {
            task.abort();
        }
        remove_internal_discovery_file(&self.config_dir);
    }

    pub fn shutdown(&self) {
        self.shutdown_external();
        self.shutdown_internal();
    }

    /// 确保内置 Antigravity（ACP 与终端 CLI）专用的回环自动化 MCP 端点常驻运行；
    /// 独立于用户可控的外部 MCP 开关，使用随机本地端口与随机会话令牌。
    pub fn ensure_internal_endpoint(self: &Arc<Self>) -> Result<AutomationMcpDiscovery, String> {
        if crate::server::is_headless() {
            return Err("无头服务模式不支持本地自动化 MCP".into());
        }
        let mut guard = self.internal.lock().unwrap();
        if guard.shutdown_tx.is_some() && !guard.url.is_empty() {
            return Ok(AutomationMcpDiscovery {
                url: guard.url.clone(),
                port: guard.bound_port,
                token: guard.token.clone(),
                pid: std::process::id(),
            });
        }

        let listener = std::net::TcpListener::bind("127.0.0.1:0")
            .map_err(|err| format!("绑定本地内置 MCP 服务失败：{err}"))?;
        listener
            .set_nonblocking(true)
            .map_err(|err| format!("设置本地内置 MCP 服务非阻塞失败：{err}"))?;
        let listener = TcpListener::from_std(listener)
            .map_err(|err| format!("初始化本地内置 MCP 监听失败：{err}"))?;

        let bound_port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
        if bound_port == 0 {
            return Err("无法获取本地内置 MCP 监听端口".into());
        }
        let url = format!("http://127.0.0.1:{bound_port}/mcp");
        let token = uuid::Uuid::new_v4().to_string();
        let discovery = AutomationMcpDiscovery {
            url: url.clone(),
            port: bound_port,
            token: token.clone(),
            pid: std::process::id(),
        };
        if let Err(err) = write_discovery_to(&internal_discovery_path(&self.config_dir), &discovery) {
            eprintln!("[automation-mcp] 写入内置服务发现文件失败：{err}");
        }

        let (tx, rx) = oneshot::channel::<()>();
        let this = self.clone();
        let token_for_task = token.clone();
        let task = tauri::async_runtime::spawn(async move {
            run_http_server(listener, this, token_for_task, rx).await;
        });

        guard.bound_port = bound_port;
        guard.url = url;
        guard.token = token;
        guard.shutdown_tx = Some(tx);
        guard.task = Some(task);
        Ok(discovery)
    }

    pub fn status(&self, settings: &Settings) -> AutomationMcpStatus {
        let guard = self.runtime.lock().unwrap();
        let running = guard.shutdown_tx.is_some();
        let port = if running {
            guard.bound_port
        } else {
            settings.automation_mcp_port
        };
        let url = if running && !guard.url.is_empty() {
            guard.url.clone()
        } else {
            format!("http://127.0.0.1:{}/mcp", settings.automation_mcp_port)
        };
        let exe = std::env::current_exe()
            .map(|p| dunce::canonicalize(&p).unwrap_or(p))
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| "Nova.exe".into());
        AutomationMcpStatus {
            enabled: settings.automation_mcp_enabled,
            running,
            port,
            url,
            token: settings.automation_mcp_token.clone(),
            executable_path: exe,
            error: guard.error.clone(),
        }
    }

    async fn handle_tool_call(self: &Arc<Self>, arguments: &Value) -> Value {
        let action = arguments
            .get("action")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("run");

        match action {
            "run" => self.action_run(arguments).await,
            "status" => self.action_status(arguments),
            "cancel" => self.action_cancel(arguments).await,
            "close" => self.action_close(arguments).await,
            other => tool_error_result(format!("不支持的 action：{other}（可选 run / status / cancel / close）")),
        }
    }

    async fn action_run(self: &Arc<Self>, arguments: &Value) -> Value {
        let intent = arguments
            .get("intent")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default()
            .to_string();
        if intent.is_empty() {
            return tool_error_result("action=run 时必须提供非空的 intent".into());
        }

        let raw_scope = arguments
            .get("scope")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let session_id_arg = arguments
            .get("session_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let wait_sec = arguments
            .get("wait_sec")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_WAIT_SEC)
            .clamp(1, MAX_WAIT_SEC);

        let (thread_id, effective_scope, start_item_id, notify) = match session_id_arg {
            Some(existing_id) => {
                match self.prepare_existing_session(&existing_id, raw_scope) {
                    Ok(v) => v,
                    Err(err) => return tool_error_result(err),
                }
            }
            None => {
                let scope = match normalize_scope(raw_scope.unwrap_or("both")) {
                    Ok(s) => s.to_string(),
                    Err(err) => return tool_error_result(err),
                };
                match self.create_new_session(&intent, &scope) {
                    Ok(v) => v,
                    Err(err) => return tool_error_result(err),
                }
            }
        };

        // 剑来桌面控制属于全局独占键鼠资源：同一时间仅允许一个含 desktop 的自动化轮次。
        let desktop_guard = if scope_uses_desktop(&effective_scope) {
            match tokio::time::timeout(DESKTOP_LOCK_TIMEOUT, self.desktop_lock.clone().lock_owned()).await {
                Ok(guard) => Some(guard),
                Err(_) => {
                    return tool_json_result(
                        json!({
                            "session_id": thread_id,
                            "scope": effective_scope,
                            "status": "busy",
                            "summary": "桌面（剑来）当前正被另一个自动化任务占用，请稍后重试。",
                        }),
                        true,
                    );
                }
            }
        } else {
            None
        };

        let lyra = self.app.state::<AppState>().lyra.clone();
        let notify_done = notify.clone();
        let tid_bg = thread_id.clone();
        tauri::async_runtime::spawn(async move {
            let _held_desktop = desktop_guard;
            lyra.run_prompt(tid_bg, intent, Vec::new()).await;
            notify_done.notify_waiters();
        });

        let wait_deadline = Duration::from_secs(wait_sec);
        let start_wait = Instant::now();
        loop {
            let still_running = {
                let state = self.app.state::<AppState>();
                state.lyra.is_running(&thread_id)
            };
            if !still_running {
                // 等待一小段时间可能刚由 spawn 启动，确认至少看到已执行完或新 item 已写入
                if start_wait.elapsed() >= Duration::from_millis(80)
                    || self.has_turn_output_since(&thread_id, start_item_id)
                {
                    break;
                }
            }
            let remaining = wait_deadline.saturating_sub(start_wait.elapsed());
            if remaining.is_zero() {
                break;
            }
            let step = remaining.min(Duration::from_millis(500));
            let _ = tokio::time::timeout(step, notify.notified()).await;
        }

        self.collect_session_report(&thread_id, &effective_scope, start_item_id)
    }

    fn action_status(self: &Arc<Self>, arguments: &Value) -> Value {
        let Some(thread_id) = arguments
            .get("session_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            return tool_error_result("action=status 时必须提供 session_id".into());
        };
        let (scope, start_item_id) = match self.lookup_session(thread_id) {
            Ok(v) => v,
            Err(err) => return tool_error_result(err),
        };
        self.collect_session_report(thread_id, &scope, start_item_id)
    }

    async fn action_cancel(self: &Arc<Self>, arguments: &Value) -> Value {
        let Some(thread_id) = arguments
            .get("session_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            return tool_error_result("action=cancel 时必须提供 session_id".into());
        };
        if let Err(err) = self.lookup_session(thread_id) {
            return tool_error_result(err);
        }
        let lyra = self.app.state::<AppState>().lyra.clone();
        lyra.cancel(thread_id).await;
        tool_json_result(
            json!({
                "session_id": thread_id,
                "status": "cancelled",
                "summary": "已请求中断当前自动化任务。",
            }),
            false,
        )
    }

    async fn action_close(self: &Arc<Self>, arguments: &Value) -> Value {
        let Some(thread_id) = arguments
            .get("session_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            return tool_error_result("action=close 时必须提供 session_id".into());
        };
        if let Err(err) = self.lookup_session(thread_id) {
            return tool_error_result(err);
        }
        let lyra = self.app.state::<AppState>().lyra.clone();
        if lyra.is_running(thread_id) {
            lyra.cancel(thread_id).await;
        }
        crate::browser::release_thread(&self.app, thread_id);
        lyra.forget_session_of_thread(thread_id);
        self.sessions.lock().unwrap().remove(thread_id);
        tool_json_result(
            json!({
                "session_id": thread_id,
                "status": "closed",
                "summary": "已关闭自动化会话并释放浏览器标签绑定。",
            }),
            false,
        )
    }

    fn prepare_existing_session(
        &self,
        thread_id: &str,
        requested_scope: Option<&str>,
    ) -> Result<(String, String, u64, Arc<Notify>), String> {
        let state = self.app.state::<AppState>();
        if state.lyra.is_running(thread_id) {
            return Err("该自动化会话当前正在执行中，请先等待完成（status）或中止（cancel）后再继续发送新意图".into());
        }
        let (stored_scope, last_item_id) = {
            let mut store = state.store.lock().unwrap();
            let thread = store
                .get_mut(thread_id)
                .ok_or_else(|| format!("自动化会话不存在：{thread_id}"))?;
            let scope = thread
                .automation_scope
                .clone()
                .ok_or("不能通过 MCP 接管普通用户会话，请省略 session_id 创建新的自动化会话")?;
            let effective = match requested_scope {
                Some(raw) => {
                    let norm = normalize_scope(raw)?;
                    if norm != scope {
                        thread.automation_scope = Some(norm.to_string());
                        store.save_thread(thread_id);
                    }
                    norm.to_string()
                }
                None => scope,
            };
            let max_id = store
                .get(thread_id)
                .and_then(|t| t.items.last().map(Item::id))
                .unwrap_or(0);
            (effective, max_id)
        };
        let notify = {
            let mut map = self.sessions.lock().unwrap();
            let entry = map
                .entry(thread_id.to_string())
                .or_insert_with(|| AutomationSessionTrack {
                    scope: stored_scope.clone(),
                    last_start_item_id: last_item_id,
                    notify: Arc::new(Notify::new()),
                });
            entry.scope = stored_scope.clone();
            entry.last_start_item_id = last_item_id;
            entry.notify.clone()
        };
        Ok((thread_id.to_string(), stored_scope, last_item_id, notify))
    }

    fn create_new_session(
        &self,
        intent: &str,
        scope: &str,
    ) -> Result<(String, String, u64, Arc<Notify>), String> {
        let state = self.app.state::<AppState>();
        let model = state
            .scene_models(Scene::Computer)
            .into_iter()
            .find(|(kind, _)| *kind == AgentKind::Lyra)
            .map(|(_, m)| m)
            .filter(|m| !m.trim().is_empty());

        let cwd = crate::remote::make_scratch_dir()?;
        let mut thread = Thread::new(cwd, AgentKind::Lyra, model, Some("build".into()), None, false);
        thread.title = format!("[MCP] {}", summarize_title(intent));
        thread.title_locked = true;
        thread.automation_scope = Some(scope.to_string());
        let thread_id = thread.id.clone();

        {
            let mut store = state.store.lock().unwrap();
            store.threads.push(thread);
            store.save_thread(&thread_id);
        }
        let _ = self.app.emit(EV_THREADS, json!({}));

        let notify = Arc::new(Notify::new());
        self.sessions.lock().unwrap().insert(
            thread_id.clone(),
            AutomationSessionTrack {
                scope: scope.to_string(),
                last_start_item_id: 0,
                notify: notify.clone(),
            },
        );
        Ok((thread_id, scope.to_string(), 0, notify))
    }

    fn lookup_session(&self, thread_id: &str) -> Result<(String, u64), String> {
        let tracked = self
            .sessions
            .lock()
            .unwrap()
            .get(thread_id)
            .map(|s| (s.scope.clone(), s.last_start_item_id));
        if let Some(info) = tracked {
            return Ok(info);
        }
        let state = self.app.state::<AppState>();
        let store = state.store.lock().unwrap();
        let thread = store
            .get(thread_id)
            .ok_or_else(|| format!("自动化会话不存在：{thread_id}"))?;
        let scope = thread
            .automation_scope
            .clone()
            .ok_or("该会话不是外部 MCP 自动化会话")?;
        Ok((scope, 0))
    }

    fn has_turn_output_since(&self, thread_id: &str, start_item_id: u64) -> bool {
        let state = self.app.state::<AppState>();
        let store = state.store.lock().unwrap();
        let Some(thread) = store.get(thread_id) else {
            return false;
        };
        thread
            .items
            .iter()
            .any(|item| item.id() > start_item_id && !matches!(item, Item::User { .. }))
    }

    fn collect_session_report(
        &self,
        thread_id: &str,
        scope: &str,
        start_item_id: u64,
    ) -> Value {
        let state = self.app.state::<AppState>();
        let running = state.lyra.is_running(thread_id);
        let store = state.store.lock().unwrap();
        let Some(thread) = store.get(thread_id) else {
            return tool_error_result(format!("会话已不存在：{thread_id}"));
        };

        let mut last_assistant: Option<String> = None;
        let mut last_error: Option<String> = None;
        let mut turn_stop: Option<String> = None;
        let mut tool_steps = 0usize;

        for item in thread.items.iter().filter(|i| i.id() > start_item_id) {
            match item {
                Item::Assistant { text, .. } if !text.trim().is_empty() => {
                    last_assistant = Some(text.trim().to_string());
                }
                Item::System { text, level, .. } if level == "error" && !text.trim().is_empty() => {
                    last_error = Some(text.trim().to_string());
                }
                Item::Tool { .. } => {
                    tool_steps += 1;
                }
                Item::Turn { stop_reason, .. } => {
                    turn_stop = Some(stop_reason.clone());
                }
                _ => {}
            }
        }

        if running {
            let summary = last_assistant
                .unwrap_or_else(|| format!("任务正在执行中（已执行 {tool_steps} 步操作），可用同一 session_id 调 action=status 查询结果。"));
            return tool_json_result(
                json!({
                    "session_id": thread_id,
                    "scope": scope,
                    "status": "running",
                    "tool_steps": tool_steps,
                    "summary": summary,
                }),
                false,
            );
        }

        if turn_stop.as_deref() == Some("cancelled") {
            return tool_json_result(
                json!({
                    "session_id": thread_id,
                    "scope": scope,
                    "status": "cancelled",
                    "tool_steps": tool_steps,
                    "summary": last_assistant.unwrap_or_else(|| "自动化任务已被取消。".into()),
                }),
                false,
            );
        }

        if turn_stop.as_deref() == Some("error") || last_error.is_some() {
            let msg = last_error
                .or(last_assistant)
                .unwrap_or_else(|| "自动化执行失败".into());
            return tool_json_result(
                json!({
                    "session_id": thread_id,
                    "scope": scope,
                    "status": "error",
                    "tool_steps": tool_steps,
                    "summary": msg,
                }),
                true,
            );
        }

        tool_json_result(
            json!({
                "session_id": thread_id,
                "scope": scope,
                "status": "completed",
                "tool_steps": tool_steps,
                "summary": last_assistant.unwrap_or_else(|| "自动化任务已结束（未返回额外文本）。".into()),
            }),
            false,
        )
    }
}

pub fn normalize_scope(raw: &str) -> Result<&'static str, String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "chrome" | "browser" => Ok("chrome"),
        "desktop" | "jianlai" => Ok("desktop"),
        "both" | "all" | "" => Ok("both"),
        other => Err(format!("不支持的 scope：{other}（可选 chrome / desktop / both）")),
    }
}

fn scope_uses_desktop(scope: &str) -> bool {
    matches!(scope, "desktop" | "both")
}

fn summarize_title(intent: &str) -> String {
    let single_line = intent.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out = String::new();
    for (i, ch) in single_line.chars().enumerate() {
        if i >= 24 {
            out.push('…');
            break;
        }
        out.push(ch);
    }
    if out.is_empty() {
        "自动化会话".into()
    } else {
        out
    }
}

pub fn nova_automation_tool_definition() -> Value {
    json!({
        "name": AUTOMATION_TOOL_NAME,
        "description": "将浏览器（Chrome）或桌面（剑来）操作意图委托给 Nova 独立自动化会话执行，并返回最终结论；适合外部 Agent 通过自然语言意图驱动复杂界面交互，同时保持多轮会话上下文。",
        "inputSchema": {
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["run", "status", "cancel", "close"],
                    "default": "run",
                    "description": "操作类型：run=新建或延续会话执行意图（默认），status=查询运行中会话进度与结论，cancel=中止运行中的任务，close=结束并释放会话资源"
                },
                "intent": {
                    "type": "string",
                    "description": "自然语言任务意图、约束条件与验收标准（action=run 时必填）"
                },
                "scope": {
                    "type": "string",
                    "enum": ["chrome", "desktop", "both"],
                    "default": "both",
                    "description": "允许使用的自动化能力边界：chrome=仅浏览器，desktop=仅剑来桌面操控，both=两者皆可（默认）"
                },
                "session_id": {
                    "type": "string",
                    "description": "会话 ID：action=run 时省略则新建可见会话，传入则在同一会话内继续多轮操控；action=status/cancel/close 时必填"
                },
                "wait_sec": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 600,
                    "default": 120,
                    "description": "本次调用最多等待的秒数（默认 120）；超时后会话仍在后台执行，可用同一 session_id 调 action=status 轮询"
                }
            }
        },
        "_meta": {
            "antigravity/alwaysLoad": true
        }
    })
}

fn normalize_automation_tool_name(raw: &str) -> &str {
    let trimmed = raw.trim();
    trimmed
        .strip_prefix("mcp_nova-automation_")
        .or_else(|| trimmed.strip_prefix("mcp_nova_automation_"))
        .unwrap_or(trimmed)
}

fn tool_json_result(payload: Value, is_error: bool) -> Value {
    let text = serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string());
    json!({
        "content": [{ "type": "text", "text": text }],
        "isError": is_error,
    })
}

fn tool_error_result(message: String) -> Value {
    json!({
        "content": [{ "type": "text", "text": message }],
        "isError": true,
    })
}

/// 验证本地自动化工具白名单只放行 Chrome / 剑来，仅供单元测试与内部断言检查。
pub(crate) fn list_allowed_tool_names_for_scope(scope: &str) -> Vec<&'static str> {
    automation_tools_for_scope(scope)
        .into_iter()
        .map(|t| t.name)
        .collect()
}

// ============================================================================
// 本地 Streamable HTTP Server (127.0.0.1:<port>/mcp)
// ============================================================================

async fn run_http_server(
    listener: TcpListener,
    manager: Arc<AutomationMcpManager>,
    token: String,
    mut shutdown_rx: oneshot::Receiver<()>,
) {
    loop {
        tokio::select! {
            _ = &mut shutdown_rx => break,
            accept_res = listener.accept() => {
                let Ok((stream, _peer)) = accept_res else {
                    break;
                };
                let manager = manager.clone();
                let token = token.clone();
                tauri::async_runtime::spawn(async move {
                    let _ = handle_http_connection(stream, manager, token).await;
                });
            }
        }
    }
}

async fn handle_http_connection(
    mut stream: TcpStream,
    manager: Arc<AutomationMcpManager>,
    token: String,
) -> Result<(), String> {
    let Some(req) = read_http_request(&mut stream).await? else {
        return Ok(());
    };
    let resp = dispatch_http_request(req, &token, |arguments| {
        let manager = manager.clone();
        Box::pin(async move { manager.handle_tool_call(&arguments).await })
    })
    .await;
    write_http_response(&mut stream, &resp).await
}

struct ParsedHttpRequest {
    method: String,
    path: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

struct HttpResponseSpec {
    status: u16,
    reason: &'static str,
    extra_headers: Vec<(String, String)>,
    body: Vec<u8>,
    content_type: &'static str,
}

async fn read_http_request(stream: &mut TcpStream) -> Result<Option<ParsedHttpRequest>, String> {
    let mut buf = Vec::with_capacity(2048);
    let mut header_end = None;
    let mut chunk = [0u8; 2048];
    while header_end.is_none() {
        let n = tokio::time::timeout(Duration::from_secs(10), stream.read(&mut chunk))
            .await
            .map_err(|_| "读取 HTTP 请求头超时".to_string())?
            .map_err(|e| e.to_string())?;
        if n == 0 {
            if buf.is_empty() {
                return Ok(None);
            }
            return Err("不完整的 HTTP 请求".into());
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.len() > MAX_HTTP_HEADER_BYTES {
            let _ = write_http_response(
                stream,
                &HttpResponseSpec {
                    status: 431,
                    reason: "Request Header Fields Too Large",
                    extra_headers: vec![],
                    body: b"Header too large".to_vec(),
                    content_type: "text/plain; charset=utf-8",
                },
            )
            .await;
            return Ok(None);
        }
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            header_end = Some(pos + 4);
        }
    }

    let header_end = header_end.unwrap();
    let header_text = String::from_utf8_lossy(&buf[..header_end - 4]);
    let mut lines = header_text.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_ascii_uppercase();
    let raw_target = parts.next().unwrap_or_default();
    let path = raw_target.split('?').next().unwrap_or("/").to_string();

    let mut headers = HashMap::new();
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }

    let mut body = buf[header_end..].to_vec();
    let is_chunked = headers
        .get("transfer-encoding")
        .is_some_and(|v| v.to_ascii_lowercase().contains("chunked"));

    if is_chunked {
        body = read_chunked_body(stream, body).await?;
    } else if let Some(len) = headers
        .get("content-length")
        .and_then(|v| v.parse::<usize>().ok())
    {
        if len > MAX_HTTP_BODY_BYTES {
            let _ = write_http_response(
                stream,
                &HttpResponseSpec {
                    status: 413,
                    reason: "Payload Too Large",
                    extra_headers: vec![],
                    body: b"Payload too large".to_vec(),
                    content_type: "text/plain; charset=utf-8",
                },
            )
            .await;
            return Ok(None);
        }
        while body.len() < len {
            let need = (len - body.len()).min(8192);
            let mut tmp = vec![0u8; need];
            let n = tokio::time::timeout(Duration::from_secs(30), stream.read(&mut tmp))
                .await
                .map_err(|_| "读取 HTTP 请求体超时".to_string())?
                .map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            body.extend_from_slice(&tmp[..n]);
        }
        body.truncate(len);
    }

    Ok(Some(ParsedHttpRequest {
        method,
        path,
        headers,
        body,
    }))
}

async fn read_chunked_body(stream: &mut TcpStream, mut buffered: Vec<u8>) -> Result<Vec<u8>, String> {
    let mut decoded = Vec::new();
    loop {
        let crlf = loop {
            if let Some(pos) = buffered.windows(2).position(|w| w == b"\r\n") {
                break pos;
            }
            let mut tmp = [0u8; 2048];
            let n = stream.read(&mut tmp).await.map_err(|e| e.to_string())?;
            if n == 0 {
                return Err("chunked 请求体提前结束".into());
            }
            buffered.extend_from_slice(&tmp[..n]);
            if buffered.len() > MAX_HTTP_BODY_BYTES {
                return Err("chunked 请求体过大".into());
            }
        };
        let size_line = String::from_utf8_lossy(&buffered[..crlf]).to_string();
        buffered.drain(..crlf + 2);
        let size_hex = size_line.split(';').next().unwrap_or_default().trim();
        let size = usize::from_str_radix(size_hex, 16)
            .map_err(|_| "无效的 chunk 长度".to_string())?;
        if size == 0 {
            return Ok(decoded);
        }
        if decoded.len().saturating_add(size) > MAX_HTTP_BODY_BYTES {
            return Err("请求体超过大小限制".into());
        }
        while buffered.len() < size + 2 {
            let mut tmp = [0u8; 4096];
            let n = stream.read(&mut tmp).await.map_err(|e| e.to_string())?;
            if n == 0 {
                return Err("chunked 数据提前结束".into());
            }
            buffered.extend_from_slice(&tmp[..n]);
        }
        decoded.extend_from_slice(&buffered[..size]);
        buffered.drain(..size + 2);
    }
}

fn is_loopback_host(host_header: &str) -> bool {
    let trimmed = host_header.trim();
    let host = if let Some(rest) = trimmed.strip_prefix('[') {
        rest.split(']').next().unwrap_or("")
    } else {
        trimmed.split(':').next().unwrap_or("")
    };
    matches!(
        host.to_ascii_lowercase().as_str(),
        "127.0.0.1" | "localhost" | "::1"
    )
}

fn is_loopback_origin(origin: &str) -> bool {
    let lower = origin.trim().to_ascii_lowercase();
    let rest = lower
        .strip_prefix("http://")
        .or_else(|| lower.strip_prefix("https://"))
        .unwrap_or("");
    let authority = rest.split('/').next().unwrap_or("");
    !authority.is_empty() && is_loopback_host(authority)
}

async fn dispatch_http_request<F>(
    req: ParsedHttpRequest,
    expected_token: &str,
    on_tool_call: F,
) -> HttpResponseSpec
where
    F: FnOnce(Value) -> std::pin::Pin<Box<dyn std::future::Future<Output = Value> + Send>>,
{
    if let Some(host) = req.headers.get("host") {
        if !is_loopback_host(host) {
            return HttpResponseSpec {
                status: 403,
                reason: "Forbidden",
                extra_headers: vec![],
                body: b"Forbidden host".to_vec(),
                content_type: "text/plain; charset=utf-8",
            };
        }
    }
    if let Some(origin) = req.headers.get("origin") {
        if !is_loopback_origin(origin) {
            return HttpResponseSpec {
                status: 403,
                reason: "Forbidden",
                extra_headers: vec![],
                body: b"Forbidden origin".to_vec(),
                content_type: "text/plain; charset=utf-8",
            };
        }
    }
    if req.method == "OPTIONS" {
        return HttpResponseSpec {
            status: 204,
            reason: "No Content",
            extra_headers: vec![
                ("Access-Control-Allow-Origin".into(), "*".into()),
                ("Access-Control-Allow-Methods".into(), "POST, DELETE, OPTIONS".into()),
                ("Access-Control-Allow-Headers".into(), "Content-Type, Authorization, Mcp-Session-Id".into()),
            ],
            body: Vec::new(),
            content_type: "text/plain; charset=utf-8",
        };
    }
    if req.path != "/mcp" && req.path != "/mcp/" {
        return HttpResponseSpec {
            status: 404,
            reason: "Not Found",
            extra_headers: vec![],
            body: b"Not Found".to_vec(),
            content_type: "text/plain; charset=utf-8",
        };
    }

    let provided_token = req
        .headers
        .get("authorization")
        .and_then(|v| v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("bearer ")))
        .map(str::trim)
        .unwrap_or("");
    if expected_token.is_empty()
        || !constant_time_eq(provided_token.as_bytes(), expected_token.as_bytes())
    {
        return HttpResponseSpec {
            status: 401,
            reason: "Unauthorized",
            extra_headers: vec![("WWW-Authenticate".into(), "Bearer".into())],
            body: b"Unauthorized".to_vec(),
            content_type: "text/plain; charset=utf-8",
        };
    }

    if req.method == "DELETE" {
        return HttpResponseSpec {
            status: 204,
            reason: "No Content",
            extra_headers: vec![],
            body: Vec::new(),
            content_type: "text/plain; charset=utf-8",
        };
    }
    if req.method != "POST" {
        return HttpResponseSpec {
            status: 405,
            reason: "Method Not Allowed",
            extra_headers: vec![("Allow".into(), "POST, DELETE, OPTIONS".into())],
            body: b"Method Not Allowed".to_vec(),
            content_type: "text/plain; charset=utf-8",
        };
    }

    let msg: Value = match serde_json::from_slice(&req.body) {
        Ok(v) => v,
        Err(err) => {
            return json_rpc_http_response(
                json!({
                    "jsonrpc": "2.0",
                    "id": Value::Null,
                    "error": { "code": -32700, "message": format!("Parse error: {err}") }
                }),
                vec![],
            );
        }
    };

    let id = msg.get("id").filter(|v| !v.is_null()).cloned();
    if id.is_none() {
        return HttpResponseSpec {
            status: 202,
            reason: "Accepted",
            extra_headers: vec![],
            body: Vec::new(),
            content_type: "application/json",
        };
    }
    let req_id = id.unwrap();
    let method = msg.get("method").and_then(Value::as_str).unwrap_or_default();
    let params = msg.get("params").cloned().unwrap_or(Value::Null);

    match method {
        "initialize" => {
            let protocol_version = params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .unwrap_or("2025-03-26");
            let session_header = req
                .headers
                .get("mcp-session-id")
                .cloned()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            json_rpc_http_response(
                json!({
                    "jsonrpc": "2.0",
                    "id": req_id,
                    "result": {
                        "protocolVersion": protocol_version,
                        "capabilities": {
                            "tools": { "listChanged": false }
                        },
                        "serverInfo": {
                            "name": SERVER_NAME,
                            "version": env!("CARGO_PKG_VERSION")
                        }
                    }
                }),
                vec![("Mcp-Session-Id".into(), session_header)],
            )
        }
        "ping" => json_rpc_http_response(
            json!({ "jsonrpc": "2.0", "id": req_id, "result": {} }),
            vec![],
        ),
        "tools/list" => json_rpc_http_response(
            json!({
                "jsonrpc": "2.0",
                "id": req_id,
                "result": {
                    "tools": [nova_automation_tool_definition()]
                }
            }),
            vec![],
        ),
        "tools/call" => {
            let tool_name = params.get("name").and_then(Value::as_str).unwrap_or_default();
            if normalize_automation_tool_name(tool_name) != AUTOMATION_TOOL_NAME {
                return json_rpc_http_response(
                    json!({
                        "jsonrpc": "2.0",
                        "id": req_id,
                        "result": tool_error_result(format!("未知工具：{tool_name}"))
                    }),
                    vec![],
                );
            }
            let arguments = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
            let result = on_tool_call(arguments).await;
            json_rpc_http_response(
                json!({
                    "jsonrpc": "2.0",
                    "id": req_id,
                    "result": result
                }),
                vec![],
            )
        }
        other => json_rpc_http_response(
            json!({
                "jsonrpc": "2.0",
                "id": req_id,
                "error": { "code": -32601, "message": format!("Method not found: {other}") }
            }),
            vec![],
        ),
    }
}

fn json_rpc_http_response(body: Value, extra_headers: Vec<(String, String)>) -> HttpResponseSpec {
    HttpResponseSpec {
        status: 200,
        reason: "OK",
        extra_headers,
        body: serde_json::to_vec(&body).unwrap_or_default(),
        content_type: "application/json; charset=utf-8",
    }
}

async fn write_http_response(stream: &mut TcpStream, resp: &HttpResponseSpec) -> Result<(), String> {
    let mut head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n",
        resp.status,
        resp.reason,
        resp.content_type,
        resp.body.len()
    );
    for (k, v) in &resp.extra_headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    stream
        .write_all(head.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    if !resp.body.is_empty() {
        stream
            .write_all(&resp.body)
            .await
            .map_err(|e| e.to_string())?;
    }
    stream.flush().await.map_err(|e| e.to_string())
}

// ============================================================================
// 服务发现文件读写 & Stdio 桥接子模式 (`Nova --mcp-automation`)
// ============================================================================

fn discovery_path(data_dir: &Path) -> PathBuf {
    data_dir.join(DISCOVERY_FILE_NAME)
}

fn internal_discovery_path(data_dir: &Path) -> PathBuf {
    data_dir.join("runtime").join(INTERNAL_DISCOVERY_FILE_NAME)
}

fn write_discovery_to(path: &Path, info: &AutomationMcpDiscovery) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let data = serde_json::to_vec_pretty(info).map_err(|e| e.to_string())?;
    std::fs::write(path, data).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

fn write_discovery_file(data_dir: &Path, info: &AutomationMcpDiscovery) -> Result<(), String> {
    write_discovery_to(&discovery_path(data_dir), info)
}

fn remove_discovery_file(data_dir: &Path) {
    let _ = std::fs::remove_file(discovery_path(data_dir));
}

fn remove_internal_discovery_file(data_dir: &Path) {
    let _ = std::fs::remove_file(internal_discovery_path(data_dir));
}

pub fn read_discovery_file(data_dir: &Path) -> Result<AutomationMcpDiscovery, String> {
    let path = discovery_path(data_dir);
    let raw = std::fs::read_to_string(&path).map_err(|_| {
        "未检测到运行中的 Nova 外部 MCP 服务：请先启动 Nova 客户端，并在「设置 → 外部 MCP 自动化」中开启服务。".to_string()
    })?;
    serde_json::from_str::<AutomationMcpDiscovery>(&raw)
        .map_err(|e| format!("读取 {} 失败：{e}", path.display()))
}

pub fn read_internal_discovery_file(data_dir: &Path) -> Result<AutomationMcpDiscovery, String> {
    let path = internal_discovery_path(data_dir);
    let raw = std::fs::read_to_string(&path).map_err(|_| {
        "未检测到运行中的 Nova 自动化 MCP 服务，请确认 Nova 客户端正在运行。".to_string()
    })?;
    serde_json::from_str::<AutomationMcpDiscovery>(&raw)
        .map_err(|e| format!("读取 {} 失败：{e}", path.display()))
}

pub fn current_executable_path() -> Result<PathBuf, String> {
    let exe = std::env::current_exe()
        .map_err(|e| format!("无法获取当前 Nova 可执行文件路径：{e}"))?;
    Ok(dunce::canonicalize(&exe).unwrap_or(exe))
}

pub fn build_acp_mcp_server_spec(
    exe_path: &Path,
    data_dir: &Path,
    endpoint: &AutomationMcpDiscovery,
) -> Value {
    json!({
        "name": SERVER_NAME,
        "command": exe_path.to_string_lossy().into_owned(),
        "args": [
            MCP_AUTOMATION_CLI_FLAG,
            "--internal",
            "--data-dir",
            data_dir.to_string_lossy().into_owned(),
        ],
        "env": [
            { "name": ENV_MCP_AUTOMATION_URL, "value": endpoint.url },
            { "name": ENV_MCP_AUTOMATION_TOKEN, "value": endpoint.token },
        ]
    })
}

fn atomic_write_bytes(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let Some(parent) = path.parent() else {
        return std::fs::write(path, bytes).map_err(|e| e.to_string());
    };
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let tmp = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("tmp"),
        uuid::Uuid::new_v4().simple()
    ));
    if let Err(err) = std::fs::write(&tmp, bytes) {
        let _ = std::fs::remove_file(&tmp);
        return Err(err.to_string());
    }
    if std::fs::rename(&tmp, path).is_err() {
        let _ = std::fs::remove_file(&tmp);
        std::fs::write(path, bytes).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// 将 `nova-automation` 合并写入隔离账号目录下的 `~/.gemini/config/mcp_config.json`，
/// 保留用户已配置的其他 MCP 服务器条目；不将鉴权令牌明文落盘到配置文件。
pub fn sync_agy_profile_mcp_config(
    profile_home: &Path,
    exe_path: &Path,
    data_dir: &Path,
) -> Result<PathBuf, String> {
    let config_path = profile_home.join(".gemini").join("config").join("mcp_config.json");
    let mut root = match std::fs::read_to_string(&config_path) {
        Ok(raw) if !raw.trim().is_empty() => serde_json::from_str::<Value>(&raw)
            .ok()
            .filter(|v| v.is_object())
            .unwrap_or_else(|| json!({})),
        _ => json!({}),
    };
    let root_obj = root.as_object_mut().expect("root is JSON object");
    if !root_obj.get("mcpServers").is_some_and(Value::is_object) {
        root_obj.insert("mcpServers".into(), json!({}));
    }
    let servers = root_obj
        .get_mut("mcpServers")
        .and_then(Value::as_object_mut)
        .expect("mcpServers is JSON object");
    servers.insert(
        SERVER_NAME.to_string(),
        json!({
            "command": exe_path.to_string_lossy().into_owned(),
            "args": [
                MCP_AUTOMATION_CLI_FLAG,
                "--internal",
                "--data-dir",
                data_dir.to_string_lossy().into_owned(),
            ],
            "disabled": false,
        }),
    );
    let mut encoded = serde_json::to_vec_pretty(&root).map_err(|e| e.to_string())?;
    encoded.push(b'\n');
    atomic_write_bytes(&config_path, &encoded)?;
    Ok(config_path)
}

/// 在隔离账号目录下写入轻量规则插件，引导终端 `agy` 将网页与桌面任务委托给 `nova_automation` MCP。
pub fn sync_agy_profile_automation_rule(profile_home: &Path) -> Result<PathBuf, String> {
    let plugin_dir = profile_home
        .join(".gemini")
        .join("config")
        .join("plugins")
        .join("nova-automation-guide");
    let plugin_manifest = plugin_dir.join("plugin.json");
    let rules_file = plugin_dir.join("rules").join("AGENTS.md");
    atomic_write_bytes(
        &plugin_manifest,
        br#"{"name":"nova-automation-guide"}
"#,
    )?;
    let rule_text = concat!(
        "# Nova Chrome & Desktop Automation\n\n",
        "When the user asks to inspect, browse, or operate web pages (Chrome) or native desktop applications (Jianlai / Windows / macOS UI), always use the `nova_automation` MCP tool (`mcp_nova-automation_nova_automation` / `nova_automation`) instead of Antigravity CLI's built-in browser tools.\n\n",
        "- Set `scope` to `\"chrome\"` for web/Chrome tasks, `\"desktop\"` for native desktop apps/dialogs (Jianlai), or `\"both\"` when both are needed.\n",
        "- Pass a clear natural-language `intent` with acceptance criteria.\n",
        "- Reuse the returned `session_id` on follow-up turns to keep browser tabs and desktop context continuous, and use `action: \"status\"` if a run exceeds `wait_sec`.\n",
    );
    atomic_write_bytes(&rules_file, rule_text.as_bytes())?;
    Ok(plugin_dir)
}

pub fn parse_cli_args(args: &[String]) -> Option<Result<BridgeCliConfig, String>> {
    if !args.iter().any(|a| a == MCP_AUTOMATION_CLI_FLAG) {
        return None;
    }
    let mut url = std::env::var(ENV_MCP_AUTOMATION_URL)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let mut token = std::env::var(ENV_MCP_AUTOMATION_TOKEN)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let mut data_dir = crate::server::data_dir_override().unwrap_or_else(default_nova_data_dir);
    let mut internal = false;

    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--internal" => {
                internal = true;
            }
            "--url" => {
                if let Some(v) = args.get(i + 1) {
                    url = Some(v.trim().to_string());
                    i += 2;
                    continue;
                }
            }
            "--token" => {
                if let Some(v) = args.get(i + 1) {
                    token = Some(v.trim().to_string());
                    i += 2;
                    continue;
                }
            }
            "--data-dir" => {
                if let Some(v) = args.get(i + 1) {
                    if !v.trim().is_empty() {
                        data_dir = PathBuf::from(v.trim());
                    }
                    i += 2;
                    continue;
                }
            }
            _ => {}
        }
        i += 1;
    }

    Some(Ok(BridgeCliConfig {
        url,
        token,
        data_dir,
        internal,
    }))
}

fn default_nova_data_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(crate::nova_data_dir_name())
}

pub fn maybe_run_cli() -> bool {
    let args: Vec<String> = std::env::args().collect();
    let Some(parsed) = parse_cli_args(&args) else {
        return false;
    };
    let code = match parsed {
        Ok(cfg) => {
            let rt = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(err) => {
                    eprintln!("[nova-automation-mcp] 无法创建异步运行时：{err}");
                    std::process::exit(1);
                }
            };
            rt.block_on(async move {
                let stdin = BufReader::new(tokio::io::stdin());
                let stdout = tokio::io::stdout();
                match serve_stdio(stdin, stdout, cfg).await {
                    Ok(()) => 0,
                    Err(err) => {
                        eprintln!("[nova-automation-mcp] {err}");
                        1
                    }
                }
            })
        }
        Err(err) => {
            eprintln!("[nova-automation-mcp] 参数错误：{err}");
            2
        }
    };
    std::process::exit(code);
}

fn request_id_key(id: &Value) -> String {
    match id {
        Value::String(s) => format!("s:{s}"),
        Value::Number(n) => format!("n:{n}"),
        other => other.to_string(),
    }
}

async fn write_mcp_line<W: AsyncWrite + Unpin>(writer: &mut W, value: &Value) -> Result<(), String> {
    let line = serde_json::to_string(value).map_err(|e| e.to_string())?;
    writer
        .write_all(line.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    writer
        .write_all(b"\n")
        .await
        .map_err(|e| e.to_string())?;
    writer.flush().await.map_err(|e| e.to_string())
}

fn resolve_target(cfg: &BridgeCliConfig) -> Result<(String, String), String> {
    match (&cfg.url, &cfg.token) {
        (Some(url), Some(token)) if !url.is_empty() && !token.is_empty() => {
            Ok((url.clone(), token.clone()))
        }
        _ if cfg.internal => {
            let disc = read_internal_discovery_file(&cfg.data_dir)
                .or_else(|_| read_discovery_file(&cfg.data_dir))
                .map_err(|_| {
                    "未检测到运行中的 Nova 自动化 MCP 服务，请确认 Nova 客户端正在运行。".to_string()
                })?;
            let url = cfg.url.clone().unwrap_or(disc.url);
            let token = cfg.token.clone().unwrap_or(disc.token);
            Ok((url, token))
        }
        _ => {
            let disc = read_discovery_file(&cfg.data_dir)?;
            let url = cfg.url.clone().unwrap_or(disc.url);
            let token = cfg.token.clone().unwrap_or(disc.token);
            Ok((url, token))
        }
    }
}

async fn forward_tool_call_http(
    http: &reqwest::Client,
    cfg: &BridgeCliConfig,
    tool_name: &str,
    arguments: &Value,
) -> Value {
    let normalized_name = normalize_automation_tool_name(tool_name);
    if normalized_name != AUTOMATION_TOOL_NAME {
        return tool_error_result(format!("未知工具：{tool_name}"));
    }
    let (url, token) = match resolve_target(cfg) {
        Ok(v) => v,
        Err(err) => return tool_error_result(err),
    };
    let rpc = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": normalized_name,
            "arguments": arguments
        }
    });
    let resp = match http
        .post(&url)
        .bearer_auth(&token)
        .json(&rpc)
        .timeout(Duration::from_secs(MAX_WAIT_SEC + 30))
        .send()
        .await
    {
        Ok(r) => r,
        Err(err) => {
            return tool_error_result(format!(
                "无法连接本地 Nova MCP 服务（{url}）：{err}。请确认 Nova 客户端正在运行并已启用「外部 MCP 自动化」。"
            ));
        }
    };
    if !resp.status().is_success() {
        return tool_error_result(format!(
            "Nova MCP 服务返回 HTTP {}，请检查 Token 是否一致或重新启用服务。",
            resp.status()
        ));
    }
    let body: Value = match resp.json().await {
        Ok(v) => v,
        Err(err) => return tool_error_result(format!("解析 Nova MCP 响应失败：{err}")),
    };
    if let Some(err_obj) = body.get("error") {
        return tool_error_result(
            err_obj
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("Nova MCP 返回协议错误")
                .to_string(),
        );
    }
    body.get("result")
        .cloned()
        .unwrap_or_else(|| tool_error_result("Nova MCP 响应缺少 result 字段".into()))
}

pub async fn serve_stdio<R, W>(
    mut reader: R,
    mut writer: W,
    cfg: BridgeCliConfig,
) -> Result<(), String>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let http = reqwest::Client::builder()
        .no_proxy()
        .build()
        .map_err(|e| format!("初始化本地 HTTP 客户端失败：{e}"))?;

    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Value>();
    let mut active_tx = Some(out_tx);
    let in_flight: Arc<Mutex<HashMap<String, oneshot::Sender<()>>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let mut raw_line = Vec::<u8>::new();
    let mut stdin_eof = false;

    loop {
        if stdin_eof {
            match out_rx.recv().await {
                Some(frame) => write_mcp_line(&mut writer, &frame).await?,
                None => break,
            }
            continue;
        }

        tokio::select! {
            biased;
            maybe_frame = out_rx.recv() => {
                if let Some(frame) = maybe_frame {
                    write_mcp_line(&mut writer, &frame).await?;
                }
            }
            read_res = reader.read_until(b'\n', &mut raw_line) => {
                let n = read_res.map_err(|e| format!("读取 MCP stdin 失败：{e}"))?;
                if n == 0 && raw_line.is_empty() {
                    stdin_eof = true;
                    active_tx.take();
                    continue;
                }
                let line_bytes = std::mem::take(&mut raw_line);
                let line = String::from_utf8_lossy(&line_bytes);
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    if n == 0 {
                        stdin_eof = true;
                        active_tx.take();
                    }
                    continue;
                }

                let msg: Value = match serde_json::from_str(trimmed) {
                    Ok(v) => v,
                    Err(e) => {
                        write_mcp_line(&mut writer, &json!({
                            "jsonrpc": "2.0",
                            "id": Value::Null,
                            "error": { "code": -32700, "message": format!("Parse error: {e}") }
                        })).await?;
                        if n == 0 {
                            stdin_eof = true;
                            active_tx.take();
                        }
                        continue;
                    }
                };

                let method = msg.get("method").and_then(Value::as_str).unwrap_or_default();
                let id = msg.get("id").filter(|v| !v.is_null()).cloned();
                let params = msg.get("params").cloned().unwrap_or(Value::Null);

                if id.is_none() {
                    if method == "notifications/cancelled" {
                        if let Some(cancel_id) = params.get("requestId").filter(|v| !v.is_null()) {
                            let key = request_id_key(cancel_id);
                            if let Some(tx) = in_flight.lock().unwrap().remove(&key) {
                                let _ = tx.send(());
                            }
                        }
                    }
                    if n == 0 {
                        stdin_eof = true;
                        active_tx.take();
                    }
                    continue;
                }
                let req_id = id.unwrap();

                match method {
                    "initialize" => {
                        let protocol_version = params
                            .get("protocolVersion")
                            .and_then(Value::as_str)
                            .filter(|s| !s.is_empty())
                            .unwrap_or("2025-03-26");
                        write_mcp_line(&mut writer, &json!({
                            "jsonrpc": "2.0",
                            "id": req_id,
                            "result": {
                                "protocolVersion": protocol_version,
                                "capabilities": { "tools": { "listChanged": false } },
                                "serverInfo": {
                                    "name": SERVER_NAME,
                                    "version": env!("CARGO_PKG_VERSION")
                                }
                            }
                        })).await?;
                    }
                    "ping" => {
                        write_mcp_line(&mut writer, &json!({ "jsonrpc": "2.0", "id": req_id, "result": {} })).await?;
                    }
                    "tools/list" => {
                        write_mcp_line(&mut writer, &json!({
                            "jsonrpc": "2.0",
                            "id": req_id,
                            "result": { "tools": [nova_automation_tool_definition()] }
                        })).await?;
                    }
                    "tools/call" => {
                        let tool_name = params.get("name").and_then(Value::as_str).unwrap_or_default().to_string();
                        let arguments = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
                        let progress_token = params
                            .get("_meta")
                            .and_then(|m| m.get("progressToken"))
                            .filter(|v| !v.is_null())
                            .cloned();

                        let key = request_id_key(&req_id);
                        let (cancel_tx, mut cancel_rx) = oneshot::channel::<()>();
                        in_flight.lock().unwrap().insert(key.clone(), cancel_tx);

                        let Some(tx) = active_tx.as_ref().cloned() else { continue; };
                        let cfg = cfg.clone();
                        let http = http.clone();
                        let in_flight = in_flight.clone();
                        tokio::spawn(async move {
                            let mut ticker = tokio::time::interval(PROGRESS_INTERVAL);
                            ticker.tick().await;
                            let mut step = 0u64;
                            let mut call_fut = std::pin::pin!(forward_tool_call_http(&http, &cfg, &tool_name, &arguments));
                            let result_opt = loop {
                                tokio::select! {
                                    res = &mut call_fut => break Some(res),
                                    _ = &mut cancel_rx => {
                                        if let Some(sid) = arguments.get("session_id").and_then(Value::as_str).filter(|s| !s.is_empty()) {
                                            let cancel_args = json!({ "action": "cancel", "session_id": sid });
                                            let _ = forward_tool_call_http(&http, &cfg, AUTOMATION_TOOL_NAME, &cancel_args).await;
                                        }
                                        break None;
                                    }
                                    _ = ticker.tick() => {
                                        if let Some(pt) = &progress_token {
                                            step += 1;
                                            let _ = tx.send(json!({
                                                "jsonrpc": "2.0",
                                                "method": "notifications/progress",
                                                "params": {
                                                    "progressToken": pt,
                                                    "progress": step,
                                                    "message": "Nova 正在执行浏览器/桌面自动化任务…"
                                                }
                                            }));
                                        }
                                    }
                                }
                            };
                            in_flight.lock().unwrap().remove(&key);
                            if let Some(result) = result_opt {
                                let _ = tx.send(json!({
                                    "jsonrpc": "2.0",
                                    "id": req_id,
                                    "result": result
                                }));
                            }
                        });
                    }
                    other => {
                        write_mcp_line(&mut writer, &json!({
                            "jsonrpc": "2.0",
                            "id": req_id,
                            "error": { "code": -32601, "message": format!("Method not found: {other}") }
                        })).await?;
                    }
                }

                if n == 0 {
                    stdin_eof = true;
                    active_tx.take();
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automation_tool_whitelist_excludes_non_automation_tools() {
        let chrome = list_allowed_tool_names_for_scope("chrome");
        assert!(!chrome.is_empty());
        assert!(chrome.iter().all(|name| crate::browser::tools::is_tool(name)));
        assert!(!chrome.iter().any(|name| crate::jianlai_tools::is_tool(name)));

        let desktop = list_allowed_tool_names_for_scope("desktop");
        assert!(!desktop.is_empty());
        assert!(desktop.iter().all(|name| crate::jianlai_tools::is_tool(name)));
        assert!(!desktop.iter().any(|name| crate::browser::tools::is_tool(name)));

        let both = list_allowed_tool_names_for_scope("both");
        assert_eq!(both.len(), chrome.len() + desktop.len());
        for forbidden in ["bash", "read", "edit", "write", "polaris", "employee", "vault", "switch_scene"] {
            assert!(!both.contains(&forbidden), "不应暴露非自动化工具 {forbidden}");
        }
    }

    #[tokio::test]
    async fn http_mcp_auth_and_protocol_endpoints() {
        let token = "secret-token-123";
        let unauth = dispatch_http_request(
            ParsedHttpRequest {
                method: "POST".into(),
                path: "/mcp".into(),
                headers: HashMap::from([("host".into(), "127.0.0.1:24680".into())]),
                body: b"{}".to_vec(),
            },
            token,
            |_| Box::pin(async { json!({}) }),
        )
        .await;
        assert_eq!(unauth.status, 401);

        let bad_host = dispatch_http_request(
            ParsedHttpRequest {
                method: "POST".into(),
                path: "/mcp".into(),
                headers: HashMap::from([
                    ("host".into(), "evil.example.com".into()),
                    ("authorization".into(), format!("Bearer {token}")),
                ]),
                body: b"{}".to_vec(),
            },
            token,
            |_| Box::pin(async { json!({}) }),
        )
        .await;
        assert_eq!(bad_host.status, 403);

        let get_res = dispatch_http_request(
            ParsedHttpRequest {
                method: "GET".into(),
                path: "/mcp".into(),
                headers: HashMap::from([("authorization".into(), format!("Bearer {token}"))]),
                body: Vec::new(),
            },
            token,
            |_| Box::pin(async { json!({}) }),
        )
        .await;
        assert_eq!(get_res.status, 405);

        let init_res = dispatch_http_request(
            ParsedHttpRequest {
                method: "POST".into(),
                path: "/mcp".into(),
                headers: HashMap::from([("authorization".into(), format!("Bearer {token}"))]),
                body: br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}"#.to_vec(),
            },
            token,
            |_| Box::pin(async { json!({}) }),
        )
        .await;
        assert_eq!(init_res.status, 200);
        assert!(init_res.extra_headers.iter().any(|(k, _)| k == "Mcp-Session-Id"));

        let list_res = dispatch_http_request(
            ParsedHttpRequest {
                method: "POST".into(),
                path: "/mcp".into(),
                headers: HashMap::from([("authorization".into(), format!("Bearer {token}"))]),
                body: br#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#.to_vec(),
            },
            token,
            |_| Box::pin(async { json!({}) }),
        )
        .await;
        let parsed: Value = serde_json::from_slice(&list_res.body).unwrap();
        let tools = parsed["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], AUTOMATION_TOOL_NAME);
    }

    #[tokio::test]
    async fn stdio_mcp_handles_protocol_and_offline_server() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = BridgeCliConfig {
            url: None,
            token: None,
            data_dir: dir.path().to_path_buf(),
            internal: false,
        };
        let input = concat!(
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-03-26\"}}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"ping\"}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/list\"}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":4,\"method\":\"tools/call\",\"params\":{\"name\":\"nova_automation\",\"arguments\":{\"intent\":\"test\"}}}\n"
        );
        let mut out = Vec::new();
        serve_stdio(input.as_bytes(), &mut out, cfg).await.unwrap();
        let replies: Vec<Value> = std::str::from_utf8(&out)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect();
        assert_eq!(replies.len(), 4);
        assert_eq!(replies[0]["result"]["serverInfo"]["name"], SERVER_NAME);
        assert_eq!(replies[1]["id"], 2);
        assert_eq!(replies[2]["result"]["tools"][0]["name"], AUTOMATION_TOOL_NAME);
        assert_eq!(replies[2]["result"]["tools"][0]["_meta"]["antigravity/alwaysLoad"], true);
        assert_eq!(replies[3]["result"]["isError"], true);
        assert!(replies[3]["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("未检测到运行中的 Nova"));
    }

    #[test]
    fn internal_cli_args_discovery_and_acp_spec() {
        let dir = tempfile::tempdir().unwrap();
        let args = vec![
            "Nova.exe".to_string(),
            MCP_AUTOMATION_CLI_FLAG.to_string(),
            "--internal".to_string(),
            "--data-dir".to_string(),
            dir.path().to_string_lossy().into_owned(),
        ];
        let cfg = parse_cli_args(&args).unwrap().unwrap();
        assert!(cfg.internal);
        assert_eq!(cfg.data_dir, dir.path());

        let internal_info = AutomationMcpDiscovery {
            url: "http://127.0.0.1:41234/mcp".into(),
            port: 41234,
            token: "tok-internal".into(),
            pid: 99,
        };
        write_discovery_to(&internal_discovery_path(dir.path()), &internal_info).unwrap();
        let (url, token) = resolve_target(&cfg).unwrap();
        assert_eq!(url, "http://127.0.0.1:41234/mcp");
        assert_eq!(token, "tok-internal");

        remove_internal_discovery_file(dir.path());
        let external_info = AutomationMcpDiscovery {
            url: "http://127.0.0.1:24680/mcp".into(),
            port: 24680,
            token: "tok-external".into(),
            pid: 100,
        };
        write_discovery_file(dir.path(), &external_info).unwrap();
        let (url, token) = resolve_target(&cfg).unwrap();
        assert_eq!(url, "http://127.0.0.1:24680/mcp");
        assert_eq!(token, "tok-external");

        let spec = build_acp_mcp_server_spec(Path::new("C:\\Nova\\Nova.exe"), dir.path(), &internal_info);
        assert_eq!(spec["name"], SERVER_NAME);
        assert_eq!(spec["command"], "C:\\Nova\\Nova.exe");
        assert_eq!(
            spec["args"],
            json!([
                MCP_AUTOMATION_CLI_FLAG,
                "--internal",
                "--data-dir",
                dir.path().to_string_lossy().into_owned(),
            ])
        );
        assert_eq!(
            spec["env"],
            json!([
                { "name": ENV_MCP_AUTOMATION_URL, "value": "http://127.0.0.1:41234/mcp" },
                { "name": ENV_MCP_AUTOMATION_TOKEN, "value": "tok-internal" },
            ])
        );
    }

    #[test]
    fn sync_agy_profile_mcp_config_preserves_existing_servers_and_writes_rule() {
        let profile = tempfile::tempdir().unwrap();
        let data_dir = tempfile::tempdir().unwrap();
        let config_dir = profile.path().join(".gemini").join("config");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(
            config_dir.join("mcp_config.json"),
            r#"{"otherTopLevel":true,"mcpServers":{"custom":{"command":"echo","args":["hi"],"disabled":false},"nova-automation":{"command":"old"}}}"#,
        )
        .unwrap();

        let written = sync_agy_profile_mcp_config(
            profile.path(),
            Path::new("C:\\App\\Nova.exe"),
            data_dir.path(),
        )
        .unwrap();
        let parsed: Value = serde_json::from_str(&std::fs::read_to_string(&written).unwrap()).unwrap();
        assert_eq!(parsed["otherTopLevel"], true);
        assert_eq!(parsed["mcpServers"]["custom"]["command"], "echo");
        assert_eq!(parsed["mcpServers"][SERVER_NAME]["command"], "C:\\App\\Nova.exe");
        assert_eq!(parsed["mcpServers"][SERVER_NAME]["disabled"], false);
        assert_eq!(
            parsed["mcpServers"][SERVER_NAME]["args"],
            json!([
                MCP_AUTOMATION_CLI_FLAG,
                "--internal",
                "--data-dir",
                data_dir.path().to_string_lossy().into_owned(),
            ])
        );
        assert!(parsed["mcpServers"][SERVER_NAME].get("env").is_none());

        let plugin_dir = sync_agy_profile_automation_rule(profile.path()).unwrap();
        assert!(plugin_dir.join("plugin.json").is_file());
        let rule = std::fs::read_to_string(plugin_dir.join("rules").join("AGENTS.md")).unwrap();
        assert!(rule.contains("nova_automation"));
        assert!(rule.contains("chrome"));
        assert!(rule.contains("desktop"));
    }

    #[test]
    fn normalize_automation_tool_name_handles_agy_prefixes() {
        assert_eq!(normalize_automation_tool_name("nova_automation"), "nova_automation");
        assert_eq!(
            normalize_automation_tool_name("mcp_nova-automation_nova_automation"),
            "nova_automation"
        );
        assert_eq!(
            normalize_automation_tool_name("mcp_nova_automation_nova_automation"),
            "nova_automation"
        );
        assert_eq!(normalize_automation_tool_name("other_tool"), "other_tool");
    }
}
