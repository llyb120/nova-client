//! Antigravity ACP 薄 Tauri 胶水层。
//!
//! 职责边界：
//! - 通过 `SessionPool` 管理每会话独占的 `SessionBinding`（严格 1 活跃会话 = 1 独立 `agy acp` 子进程 + 1 独占 MCP 回环 IPC）；
//! - 接入三级恢复链（`session/resume` → `session/load(抑制重放)` → `session/new + handoff`）、
//!   启动 2s 瞬态 auth 重试、热切模型/思考强度（半切换态自动丢弃重建）、
//!   stderr 429 重试风暴快速中止与双档 Prompt 看门狗；
//! - 将 `core::mapper::MappedAction` 写入 `AppState.store` 并发射现有 `EV_UPDATE` / `EV_TURN` / `EV_OPTIONS` / `EV_LOG`；
//! - 负责模型列表探测（23 模型）与 `~/.nova/model-options/antigravity.json` 缓存；
//! - 取消时先发 `session/cancel`，宽限 1.5s 未收敛则强杀进程树并幂等收尾。

use super::accounts::{AccountManager, AntigravityAccountsSnapshot};
use super::bridge::start_session_bridge;
use super::core::connection::{AcpConnection, ConnectionOptions, InboundEvent, CANCEL_GRACE_MS};
use super::core::mapper::{MappedAction, MappedToolCall, TurnMapper};
use super::core::process::{prepare_env_patch, LaunchConfig};
use super::core::protocol::{
    escape_leading_slash, extract_antigravity_tier, extract_nova_model_options,
    find_preferred_wire_mode, truncate_step_text, AcpError, AcpErrorKind, AgentCapabilities,
    ElicitationRequestParams, PromptResult, SessionNewParams, SessionPromptParams,
    ACP_STEP_TEXT_BYTES,
};
use super::core::quota::{
    parse_agy_log_line, parse_antigravity_usage_output, AgyLogSignal, AntigravityQuotaSnapshot,
    ResourceExhaustionTracker,
};
use super::core::session::{
    is_transient_startup_auth_error, open_or_recover_session, switch_model_and_effort,
    ModelSwitchError, SessionOpenMode, STARTUP_TRANSIENT_AUTH_RETRY_DELAY,
};
use super::core::session_pool::SessionPool;
use super::core::system_prompt::build_session_meta;
use super::core::watchdog::PromptWatchdog;
use super::sidecar::SessionIpcServer;
use crate::acp::{
    assign_to_agent_job, kill_process_tree, resolve_program_on_path, EV_ANTIGRAVITY_ACCOUNTS,
    EV_ANTIGRAVITY_QUOTA, EV_LOG, EV_OPTIONS, EV_PERMISSION, EV_PERMISSION_RESOLVED, EV_THREADS,
    EV_TURN, EV_UPDATE,
};
use crate::agent_config;
use crate::model_cache;
use crate::threads::{
    compact_tool_value, compact_tool_values, file_uri_to_local_path, now_ms,
    render_handoff_context, save_attachment_to_temp, AgentKind, Item, PromptImage, ToolCall,
};
use crate::{nova_data_dir, AppState};
use base64::Engine;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::{mpsc, oneshot, Notify};

/// 客户端标识：按实测保持中性，避免触发服务端门控或指纹异常。
const CLIENT_NAME: &str = "nova";
const CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");
/// 后台空闲热进程回收检查周期（s）。
const IDLE_SWEEP_INTERVAL_SECS: u64 = 60;
const QUOTA_CACHE_TTL_MS: i64 = 60_000;
const QUOTA_DEDUP_MS: i64 = 3_000;
const QUOTA_FETCH_TIMEOUT: Duration = Duration::from_secs(20);
/// OAuth Access Token 临近过期回收窗口（5 分钟，与 `agy` 启动主动刷新阈值一致）。
const TOKEN_RECYCLE_MARGIN_MS: i64 = 5 * 60 * 1000;
const INLINE_AUTH_RETRY_DELAY_CLEAN: Duration = Duration::from_millis(500);
const INLINE_AUTH_RETRY_DELAY_MID_TURN: Duration = Duration::from_millis(800);
const MID_TURN_AUTH_CONTINUE_PROMPT: &str =
    "请从刚才因认证令牌刷新中断的位置继续完成当前任务，不要重复已完成的步骤。";

fn should_recycle_binding_for_token_expiry(
    loaded_expiry_ms: Option<i64>,
    latest_keyring_expiry_ms: Option<i64>,
    now_ms: i64,
) -> bool {
    let Some(exp) = loaded_expiry_ms else {
        return false;
    };
    let _ = latest_keyring_expiry_ms;
    now_ms >= exp.saturating_sub(TOKEN_RECYCLE_MARGIN_MS)
}

fn is_permanent_auth_error(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("invalid_grant")
        || lower.contains("api_key")
        || lower.contains("api key")
        || lower.contains("not logged into")
        || lower.contains("validation_required")
}

type ReadyCli = (Arc<AcpConnection>, mpsc::UnboundedReceiver<InboundEvent>, AgentCapabilities);
type Claim<T> = oneshot::Sender<oneshot::Sender<T>>;

// Replacing the ticket drops the receiver's peer and cancels even an in-flight initialize.
fn warm_ticket<T: Send + 'static>(prepare: impl std::future::Future<Output = Result<T, AcpError>> + Send + 'static, ttl: Duration) -> Claim<T> {
    let (claim, request) = oneshot::channel::<oneshot::Sender<T>>();
    tokio::spawn(async move {
        let pending = async { tokio::try_join!(prepare, async { request.await.map_err(|_| AcpError::aborted("草稿已切换")) }) };
        if let Ok(Ok((ready, reply))) = tokio::time::timeout(ttl, pending).await {
            let _ = reply.send(ready);
        }
    });
    claim
}

async fn initialize_cli(cfg: LaunchConfig) -> Result<ReadyCli, AcpError> {
    let (conn, rx) = AcpConnection::spawn_with_child_hook(cfg, ConnectionOptions::default(), assign_to_agent_job).await?;
    match conn.initialize(CLIENT_NAME, CLIENT_VERSION).await {
        Ok(init) => Ok((Arc::new(conn), rx, init.capabilities)),
        Err(err) => { conn.terminate(500).await; Err(err) }
    }
}

struct SessionBinding {
    connection: Arc<AcpConnection>,
    inbound_rx: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<InboundEvent>>>,
    /// 每会话独占的 MCP 回环 socket/pipe 服务端，随 binding 生死自动清理。
    _mcp_bridge: SessionIpcServer,
    polaris_enabled: bool,
    session_id: String,
    cwd: String,
    account_id: Option<String>,
    app_data_dir: String,
    loaded_token_expiry_ms: Option<i64>,
    capabilities: AgentCapabilities,
    config_options: Vec<Value>,
    active_model: Option<String>,
    active_effort: Option<String>,
    active_wire_mode: Option<String>,
}

struct ActiveTurn {
    run_epoch: u64,
    connection: Arc<AcpConnection>,
    session_id: String,
    cancel_notify: Arc<Notify>,
    cancelled: Arc<AtomicBool>,
}

struct PendingElicitation {
    thread_id: String,
    connection: Arc<AcpConnection>,
    rpc_id: Value,
    params: ElicitationRequestParams,
    watchdog: PromptWatchdog,
}

/// 单轮流式条目归属状态：合并连续正文/思考 chunk 为 `delta`，按 `toolCallId` 复用工具卡片。
#[derive(Default)]
struct StreamItemState {
    current_assistant_id: Option<u64>,
    current_thought_id: Option<u64>,
    tool_item_ids: HashMap<String, u64>,
}

impl StreamItemState {
    fn close_text_blocks(&mut self) {
        self.current_assistant_id = None;
        self.current_thought_id = None;
    }
}

/// agy emits no second prompt response for an autonomous background continuation.
/// Its final text is followed by a usage update; tool requests can follow that same update.
/// Fallback timers prevent hanging indefinitely if agy omits a trailing `usage_update` or exits quietly.
#[derive(Default)]
struct CommandTail {
    answer: bool,
    settle_at: Option<tokio::time::Instant>,
    fallback_settle_at: Option<tokio::time::Instant>,
}

const COMMAND_TAIL_ANSWER_FALLBACK: Duration = Duration::from_secs(15);
const COMMAND_TAIL_IDLE_FALLBACK: Duration = Duration::from_secs(45);
const COMMAND_TAIL_ERROR_NOTICE_FALLBACK: Duration = Duration::from_secs(3);

impl CommandTail {
    fn observe(&mut self, update: &Value, mapper: &TurnMapper) {
        let idle = !mapper.has_running_command() && mapper.active_tools().is_empty();
        match update["sessionUpdate"].as_str() {
            Some("agent_message_chunk") if idle => {
                let non_empty = update
                    .pointer("/content/text")
                    .and_then(Value::as_str)
                    .is_some_and(|s| !s.trim().is_empty());
                if non_empty {
                    self.answer = true;
                    self.settle_at = None;
                    self.fallback_settle_at =
                        Some(tokio::time::Instant::now() + COMMAND_TAIL_ANSWER_FALLBACK);
                }
            }
            Some("usage_update") if self.answer && idle => {
                // shortcut: agy 1.5 has no autonomous end marker; allow a trailing tool batch to arrive.
                self.settle_at = Some(tokio::time::Instant::now() + Duration::from_secs(1));
            }
            Some("notice") if idle => {
                let is_err = update["level"]
                    .as_str()
                    .is_some_and(|l| matches!(l.to_ascii_lowercase().as_str(), "error" | "fatal"));
                if is_err {
                    self.fallback_settle_at =
                        Some(tokio::time::Instant::now() + COMMAND_TAIL_ERROR_NOTICE_FALLBACK);
                }
            }
            Some("tool_call_update") if idle => {
                // 所有后台命令与工具均已结束：若模型静默无后续输出，45s 后兜底收敛。
                self.answer = false;
                self.settle_at = None;
                self.fallback_settle_at =
                    Some(tokio::time::Instant::now() + COMMAND_TAIL_IDLE_FALLBACK);
            }
            Some("agent_thought_chunk" | "tool_call" | "tool_call_update") => {
                *self = Self::default();
            }
            _ => {}
        }
    }

    fn next_settle_at(&self) -> Option<tokio::time::Instant> {
        match (self.settle_at, self.fallback_settle_at) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }
}

#[derive(Default)]
struct TurnDiagnostics {
    saw_stream_headers: bool,
    last_issue: Option<String>,
    retry_notices_sent: u32,
}

/// RAII 守卫：保证 turn 退出（含异常早退）时释放 `SessionPool` 的 `in_use` 标记。
struct BindingInUseGuard<'a> {
    runtime: &'a AntigravityRuntime,
    thread_id: String,
}

impl<'a> BindingInUseGuard<'a> {
    fn acquire(runtime: &'a AntigravityRuntime, thread_id: &str) -> Self {
        runtime
            .pool
            .lock()
            .unwrap()
            .mark_in_use(thread_id, true, tokio::time::Instant::now());
        Self {
            runtime,
            thread_id: thread_id.to_string(),
        }
    }
}

impl Drop for BindingInUseGuard<'_> {
    fn drop(&mut self) {
        self.runtime.pool.lock().unwrap().mark_in_use(
            &self.thread_id,
            false,
            tokio::time::Instant::now(),
        );
    }
}

#[derive(serde::Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AccountSwitchOutcome {
    pub snapshot: AntigravityAccountsSnapshot,
    pub options: Option<Value>,
    pub warning: Option<String>,
}

pub struct AntigravityRuntime {
    app: AppHandle,
    accounts: AccountManager,
    pool: Mutex<SessionPool<Arc<tokio::sync::Mutex<SessionBinding>>>>,
    active_turns: Mutex<HashMap<String, ActiveTurn>>,
    running: Mutex<HashSet<String>>,
    turn_started: Mutex<HashMap<String, Instant>>,
    model_options: Mutex<Option<Value>>,
    draft_cli: Mutex<Option<(LaunchConfig, Option<i64>, Claim<ReadyCli>)>>,
    model_options_refreshing: AtomicBool,
    probe_result: tokio::sync::Mutex<Option<Result<Value, String>>>,
    quota_cache: Mutex<Option<AntigravityQuotaSnapshot>>,
    quota_fetch_lock: tokio::sync::Mutex<()>,
    quota_refreshing: AtomicBool,
    account_gate: tokio::sync::RwLock<()>,
    model_options_revalidated: AtomicBool,
    next_run_epoch: AtomicU64,
    run_epochs: Mutex<HashMap<String, u64>>,
    pending_elicitations: Mutex<HashMap<String, PendingElicitation>>,
    next_elicitation_seq: AtomicU64,
    shutting_down: AtomicBool,
}

impl AntigravityRuntime {
    pub fn new(app: AppHandle) -> Arc<Self> {
        let accounts = AccountManager::new(nova_data_dir(&app));
        let runtime = Arc::new(Self {
            app,
            accounts,
            pool: Mutex::new(SessionPool::default()),
            active_turns: Mutex::new(HashMap::new()),
            running: Mutex::new(HashSet::new()),
            turn_started: Mutex::new(HashMap::new()),
            model_options: Mutex::new(None),
            draft_cli: Mutex::new(None),
            model_options_refreshing: AtomicBool::new(false),
            probe_result: tokio::sync::Mutex::new(None),
            quota_cache: Mutex::new(None),
            quota_fetch_lock: tokio::sync::Mutex::new(()),
            quota_refreshing: AtomicBool::new(false),
            account_gate: tokio::sync::RwLock::new(()),
            model_options_revalidated: AtomicBool::new(false),
            next_run_epoch: AtomicU64::new(1),
            run_epochs: Mutex::new(HashMap::new()),
            pending_elicitations: Mutex::new(HashMap::new()),
            next_elicitation_seq: AtomicU64::new(1),
            shutting_down: AtomicBool::new(false),
        });
        // 后台空闲回收协程：每 60s 清理超过 30 分钟未使用的空闲热进程（保留 acpSessionId 以便续聊）。
        let weak = Arc::downgrade(&runtime);
        tauri::async_runtime::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(IDLE_SWEEP_INTERVAL_SECS));
            ticker.tick().await;
            loop {
                ticker.tick().await;
                let Some(rt) = weak.upgrade() else {
                    break;
                };
                if rt.shutting_down.load(Ordering::SeqCst) {
                    break;
                }
                let evicted = rt
                    .pool
                    .lock()
                    .unwrap()
                    .evict_idle(tokio::time::Instant::now());
                for (_, binding) in evicted {
                    spawn_terminate_binding(binding, CANCEL_GRACE_MS);
                }
            }
        });
        runtime
    }

    pub fn warm_draft(&self, directory: &std::path::Path) -> Result<(), String> {
        let Ok(_account) = self.account_gate.try_read() else { return Ok(()); };
        if !directory.is_absolute() || !directory.is_dir() { return Err("工作目录无效".into()); }
        let (_, app_data_dir, token_expiry_ms) = self.accounts.active_account_context_with_expiry();
        let cfg = self.launch_acp_config_with_dir(
            PathBuf::from(canonical_cwd_string(directory)),
            app_data_dir,
        )?;
        let now = now_ms();
        let mut slot = self.draft_cli.lock().unwrap();
        if !slot.as_ref().is_some_and(|(key, warm_expiry, claim)| {
            key.same_environment(&cfg)
                && !claim.is_closed()
                && !should_recycle_binding_for_token_expiry(*warm_expiry, token_expiry_ms, now)
        }) {
            *slot = Some((
                cfg.clone(),
                token_expiry_ms,
                warm_ticket(initialize_cli(cfg), Duration::from_secs(120)),
            ));
        }
        Ok(())
    }

    /// 打开一个尚未持有常驻 `agy` 进程的 Antigravity 会话时后台预热 CLI，消除首轮冷启动等待。
    pub fn warm_thread_if_cold(&self, thread_id: &str, directory: &std::path::Path) {
        if self.shutting_down.load(Ordering::SeqCst) || self.is_running(thread_id) {
            return;
        }
        if self.pool.lock().unwrap().contains(thread_id) {
            return;
        }
        let _ = self.warm_draft(directory);
    }

    pub(super) fn tool_cancel_flag(&self, thread_id: &str) -> Option<Arc<AtomicBool>> {
        self.active_turns.lock().unwrap().get(thread_id).map(|turn| turn.cancelled.clone())
    }

    pub fn is_running(&self, thread_id: &str) -> bool {
        self.running.lock().unwrap().contains(thread_id)
    }

    pub fn seed_model_options(&self, value: Value) {
        *self.model_options.lock().unwrap() = Some(value);
    }

    pub fn get_model_options(&self) -> Option<Value> {
        self.model_options.lock().unwrap().clone()
    }

    pub fn refresh_model_options_soon(self: &Arc<Self>) {
        self.model_options_revalidated
            .store(false, Ordering::SeqCst);
        let manager = self.clone();
        tauri::async_runtime::spawn(async move {
            while manager.model_options_refreshing.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            manager.spawn_revalidate_model_options();
        });
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
                let _ = manager
                    .app
                    .emit(EV_LOG, format!("[Antigravity] 拉取模型列表失败：{error}"));
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

    fn pending_model_options(&self) -> Value {
        json!({
            "configOptions": [{ "id": "model", "name": "Model", "currentValue": "", "options": [] }],
            "modes": Value::Null,
            "pending": true,
        })
    }

    fn resolve_cli_program(&self) -> Result<String, String> {
        let configured = {
            let state = self.app.state::<AppState>();
            let s = state.settings.lock().unwrap();
            let trimmed = s.antigravity_path.trim();
            if trimmed.is_empty() {
                "agy".to_string()
            } else {
                trimmed.to_string()
            }
        };
        resolve_program_on_path(&configured)
            .map(|p| p.to_string_lossy().into_owned())
            .ok_or_else(|| {
                format!(
                    "未找到 Antigravity CLI（{configured}），请在设置中检查路径或确认 agy 已在 PATH 中"
                )
            })
    }

    fn launch_config(&self, cwd: PathBuf) -> Result<LaunchConfig, String> {
        let (_, app_data_dir) = self.accounts.active_account_context();
        self.launch_config_with_dir(cwd, app_data_dir)
    }

    fn launch_acp_config(&self, cwd: PathBuf) -> Result<LaunchConfig, String> {
        let (_, app_data_dir) = self.accounts.active_account_context();
        self.launch_acp_config_with_dir(cwd, app_data_dir)
    }

    fn launch_acp_config_with_dir(
        &self,
        cwd: PathBuf,
        app_data_dir: String,
    ) -> Result<LaunchConfig, String> {
        let mut cfg = self.launch_config_with_dir(cwd, app_data_dir)?;
        if let Some(log_path) = allocate_acp_log_file(&nova_data_dir(&self.app)) {
            cfg = cfg.with_log_file(log_path);
        }
        Ok(cfg)
    }

    fn launch_config_with_dir(
        &self,
        cwd: PathBuf,
        app_data_dir: String,
    ) -> Result<LaunchConfig, String> {
        let mut cfg = LaunchConfig::for_agy_with_dir(self.resolve_cli_program()?, cwd, app_data_dir);
        #[cfg(windows)]
        {
            // 若用户已在环境或自定义环境变量中显式设置代理，尊重显式配置；
            // 否则在 Windows 系统代理启用时导出给 Go HTTP 客户端。
            let has_explicit_proxy = ["HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy"]
                .iter()
                .any(|k| std::env::var(k).is_ok_and(|v| !v.trim().is_empty()));
            if !has_explicit_proxy {
                let (enabled, server, bypass, _) = crate::path_env::windows_system_proxy();
                if enabled != 0 {
                    cfg.env_overrides = super::proxy::proxy_env(
                        &server,
                        &bypass,
                        std::env::var("NO_PROXY").unwrap_or_default(),
                    );
                }
            }
        }
        Ok(cfg)
    }

    fn emit_accounts(&self, snapshot: &AntigravityAccountsSnapshot) {
        let _ = self.app.emit(EV_ANTIGRAVITY_ACCOUNTS, snapshot);
    }

    fn clear_quota_cache(&self) {
        *self.quota_cache.lock().unwrap() = None;
        let _ = self.app.emit(EV_ANTIGRAVITY_QUOTA, Value::Null);
    }

    pub async fn get_quota(&self, force: bool) -> Result<AntigravityQuotaSnapshot, String> {
        let _account = self.account_gate.read().await;
        let (current_account_id, _) = self.accounts.active_account_context();
        if !force {
            if let Some(cached) = self.quota_cache.lock().unwrap().clone() {
                if cached.account_id == current_account_id
                    && now_ms().saturating_sub(cached.fetched_at) < QUOTA_CACHE_TTL_MS
                {
                    return Ok(cached);
                }
            }
        }

        let _fetch_guard = self.quota_fetch_lock.lock().await;
        let dedup_ms = if force { QUOTA_DEDUP_MS } else { QUOTA_CACHE_TTL_MS };
        if let Some(cached) = self.quota_cache.lock().unwrap().clone() {
            if cached.account_id == current_account_id
                && now_ms().saturating_sub(cached.fetched_at) < dedup_ms
            {
                return Ok(cached);
            }
        }

        let program = self.resolve_cli_program()?;
        let data_dir = nova_data_dir(&self.app);
        let cwd = if std::fs::create_dir_all(&data_dir).is_ok() {
            data_dir
        } else {
            std::env::temp_dir()
        };

        #[cfg(windows)]
        let env_overrides = {
            let (enabled, server, bypass, _) = crate::path_env::windows_system_proxy();
            if enabled != 0 {
                super::proxy::proxy_env(
                    &server,
                    &bypass,
                    std::env::var("NO_PROXY").unwrap_or_default(),
                )
            } else {
                HashMap::new()
            }
        };
        #[cfg(not(windows))]
        let env_overrides = HashMap::new();

        let base_env: HashMap<String, String> = std::env::vars().collect();
        let env_patch = prepare_env_patch(&base_env, &env_overrides);

        let mut cmd = tokio::process::Command::new(&program);
        cmd.args(["--print", "/usage", "--output-format", "json"])
            .current_dir(&cwd)
            .envs(env_patch)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);

        #[cfg(unix)]
        cmd.process_group(0);
        #[cfg(windows)]
        cmd.creation_flags(0x0800_0000);

        let child = cmd
            .spawn()
            .map_err(|e| format!("启动 Antigravity CLI 查询用量失败：{e}"))?;
        assign_to_agent_job(&child);
        let pid = child.id();

        let output = match tokio::time::timeout(QUOTA_FETCH_TIMEOUT, child.wait_with_output()).await
        {
            Ok(Ok(out)) => out,
            Ok(Err(e)) => return Err(format!("读取 Antigravity 用量输出失败：{e}")),
            Err(_) => {
                if let Some(pid) = pid {
                    #[cfg(windows)]
                    let _ = tokio::task::spawn_blocking(move || kill_process_tree(pid)).await;
                    #[cfg(unix)]
                    super::core::process::kill_process_group_force(pid);
                }
                return Err("获取 Antigravity 用量超时".into());
            }
        };

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let redacted = super::core::process::redact_stderr(&stderr);
            let detail = redacted
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .map(str::trim)
                .unwrap_or("命令执行失败");
            return Err(format!("获取 Antigravity 用量失败：{detail}"));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let snapshot = parse_antigravity_usage_output(&stdout, current_account_id, now_ms())?;
        *self.quota_cache.lock().unwrap() = Some(snapshot.clone());
        let _ = self.app.emit(EV_ANTIGRAVITY_QUOTA, &snapshot);
        Ok(snapshot)
    }

    pub fn spawn_refresh_quota(self: &Arc<Self>, force: bool) {
        if self.shutting_down.load(Ordering::SeqCst)
            || self
                .quota_refreshing
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
        {
            return;
        }
        let manager = self.clone();
        tauri::async_runtime::spawn(async move {
            struct ResetFlag<'a>(&'a AtomicBool);
            impl Drop for ResetFlag<'_> {
                fn drop(&mut self) {
                    self.0.store(false, Ordering::SeqCst);
                }
            }
            let _reset = ResetFlag(&manager.quota_refreshing);
            let _ = manager.get_quota(force).await;
        });
    }

    async fn drain_live_processes(&self) {
        self.draft_cli.lock().unwrap().take();
        let idle = self.pool.lock().unwrap().drain_all();
        for (_, binding) in idle {
            let conn = binding.lock().await.connection.clone();
            conn.terminate(CANCEL_GRACE_MS).await;
        }
        self.model_options_revalidated.store(false, Ordering::SeqCst);
        self.publish_model_options(self.pending_model_options());
    }

    pub fn list_accounts(&self) -> Result<AntigravityAccountsSnapshot, String> {
        self.accounts.list_and_sync()
    }

    pub(crate) fn prepare_terminal_account_home(&self, account_id: &str) -> Result<PathBuf, String> {
        self.accounts.prepare_terminal_home(account_id)
    }

    pub fn rename_account(
        &self,
        account_id: &str,
        label: &str,
    ) -> Result<AntigravityAccountsSnapshot, String> {
        let snapshot = self.accounts.rename_account(account_id, label)?;
        self.emit_accounts(&snapshot);
        Ok(snapshot)
    }

    pub fn delete_account(&self, account_id: &str) -> Result<AntigravityAccountsSnapshot, String> {
        let snapshot = self.accounts.delete_account(account_id)?;
        self.emit_accounts(&snapshot);
        Ok(snapshot)
    }

    /// 一键切换到指定已保存账号：先回写当前活跃令牌、替换系统凭据库，再清理热进程并重新探测可用模型。
    pub async fn switch_account(&self, account_id: &str) -> Result<AccountSwitchOutcome, String> {
        let _exclusive = self
            .account_gate
            .try_write()
            .map_err(|_| "Antigravity 正在执行请求、探测模型或换号，请在结束后重试")?;
        let snapshot = self.accounts.activate_account(account_id)?;
        self.clear_quota_cache();
        self.emit_accounts(&snapshot);
        self.drain_live_processes().await;
        let (options, warning) = match self.probe_model_options().await {
            Ok(opts) => (Some(opts), None),
            Err(err) => (
                None,
                Some(format!(
                    "已切换账号，但验证连接失败（可能凭据已过期，可点击「重新登录」）：{}",
                    super::core::process::redact_stderr(&err)
                )),
            ),
        };
        let latest = self.accounts.list_and_sync().unwrap_or(snapshot);
        self.emit_accounts(&latest);
        Ok(AccountSwitchOutcome {
            snapshot: latest,
            options,
            warning,
        })
    }

    /// 打开官方 `agy acp --login` 窗口新增账号或为指定账号重新授权；取消/失败时自动回滚原活跃账号。
    pub async fn login_account(
        &self,
        target_account_id: Option<&str>,
    ) -> Result<AccountSwitchOutcome, String> {
        let _exclusive = self
            .account_gate
            .try_write()
            .map_err(|_| "Antigravity 正在执行请求、探测模型或换号，请在结束后重试")?;
        if !cfg!(windows) {
            return Err("请先在本机 agy 终端用 /logout、/login 换号，再点「同步终端登录」".into());
        }
        #[cfg(windows)]
        {
            use windows_sys::Win32::System::Threading::CREATE_NEW_CONSOLE;
            let cwd = nova_data_dir(&self.app).join("antigravity-login");
            std::fs::create_dir_all(&cwd).map_err(|_| "无法创建 agy 登录工作目录")?;
            let cfg = self.launch_config(cwd)?;
            if !std::path::Path::new(&cfg.program)
                .extension()
                .is_some_and(|s| s.eq_ignore_ascii_case("exe"))
            {
                return Err("打开登录窗口需要 agy.exe。也可以在终端换号后点「同步终端登录」。".into());
            }

            let backup = self.accounts.prepare_interactive_login()?;
            self.drain_live_processes().await;

            let spawn_res = tokio::process::Command::new(&cfg.program)
                .args(["acp", "--app_data_dir=antigravity-acp-login", "--login"])
                .current_dir(&cfg.cwd)
                .envs(cfg.env_overrides)
                .creation_flags(CREATE_NEW_CONSOLE)
                .kill_on_drop(true)
                .spawn();

            let mut child = match spawn_res {
                Ok(child) => child,
                Err(_) => {
                    self.accounts.rollback_interactive_login(backup);
                    if let Ok(restored) = self.accounts.list_and_sync() {
                        self.emit_accounts(&restored);
                    }
                    return Err("未能打开 agy 登录窗口，已恢复原账号。".into());
                }
            };

            let exited_ok = match child.wait().await {
                Ok(status) => status.success(),
                Err(_) => false,
            };
            if !exited_ok {
                self.accounts.rollback_interactive_login(backup);
                if let Ok(restored) = self.accounts.list_and_sync() {
                    self.emit_accounts(&restored);
                }
                return Err("登录未完成或已取消，已保留原账号。".into());
            }

            let snapshot = match self.accounts.commit_interactive_login(target_account_id) {
                Ok(snap) => snap,
                Err(err) => {
                    self.accounts.rollback_interactive_login(backup);
                    if let Ok(restored) = self.accounts.list_and_sync() {
                        self.emit_accounts(&restored);
                    }
                    return Err(format!("{err}，已恢复原账号。"));
                }
            };
            self.clear_quota_cache();
            self.emit_accounts(&snapshot);

            let (options, warning) = match self.probe_model_options().await {
                Ok(opts) => (Some(opts), None),
                Err(err) => (
                    None,
                    Some(format!(
                        "账号已保存，但探测模型列表失败：{}",
                        super::core::process::redact_stderr(&err)
                    )),
                ),
            };
            let latest = self.accounts.list_and_sync().unwrap_or(snapshot);
            self.emit_accounts(&latest);
            return Ok(AccountSwitchOutcome {
                snapshot: latest,
                options,
                warning,
            });
        }
        #[cfg(not(windows))]
        {
            let _ = target_account_id;
            unreachable!()
        }
    }

    /// 同步本机 `agy` 终端中的当前登录，清理旧热进程并重新探测可用模型。
    pub async fn sync_active_account(&self) -> Result<AccountSwitchOutcome, String> {
        let _exclusive = self
            .account_gate
            .try_write()
            .map_err(|_| "Antigravity 正在执行请求、探测模型或换号，请在结束后重试")?;
        self.drain_live_processes().await;
        let snapshot = self.accounts.force_sync_current()?;
        self.clear_quota_cache();
        self.emit_accounts(&snapshot);
        let options = self.probe_model_options().await.map_err(|e| {
            format!(
                "无法确认当前登录可用：{}",
                super::core::process::redact_stderr(&e)
            )
        })?;
        let latest = self.accounts.list_and_sync().unwrap_or(snapshot);
        self.emit_accounts(&latest);
        Ok(AccountSwitchOutcome {
            snapshot: latest,
            options: Some(options),
            warning: None,
        })
    }

    /// Keep Nova threads and saved session IDs. Only the processes that cached the old login are replaced.
    pub async fn reconnect_account(&self, login: bool) -> Result<Value, String> {
        let outcome = if login {
            self.login_account(None).await?
        } else {
            self.sync_active_account().await?
        };
        if let Some(opts) = outcome.options {
            Ok(opts)
        } else {
            Err(outcome
                .warning
                .unwrap_or_else(|| "无法确认当前登录可用".into()))
        }
    }

    pub async fn refresh_model_options(&self) -> Result<Value, String> {
        let _account = self.account_gate.try_read().map_err(|_| "正在重新连接 Antigravity 账号")?;
        let mut slot = match self.probe_result.try_lock() {
            Ok(slot) => slot,
            Err(_) => {
                let slot = self.probe_result.lock().await;
                return slot.clone().unwrap_or_else(|| Err("模型探测已中断，请重试".into()));
            }
        };
        *slot = None;
        let result = self.probe_model_options().await.map_err(|e| super::core::process::redact_stderr(&e));
        *slot = Some(result.clone());
        result
    }

    async fn probe_model_options(&self) -> Result<Value, String> {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let cfg = self.launch_acp_config(cwd.clone())?;
        let (conn, _rx) = AcpConnection::spawn_with_child_hook(
            cfg,
            ConnectionOptions::default(),
            assign_to_agent_job,
        )
        .await
        .map_err(|e| e.to_string())?;

        let result = async {
            conn.initialize(CLIENT_NAME, CLIENT_VERSION)
                .await
                .map_err(|e| e.to_string())?;
            let canonical_cwd = canonical_cwd_string(&cwd);
            let params = SessionNewParams {
                cwd: canonical_cwd,
                mcp_servers: Vec::new(),
                meta: None,
            };
            let opened = match conn.session_new(params.clone()).await {
                Ok(ok) => ok,
                Err(ref err) if is_transient_startup_auth_error(&conn, err) => {
                    tokio::time::sleep(STARTUP_TRANSIENT_AUTH_RETRY_DELAY).await;
                    conn.session_new(params).await.map_err(|e| e.to_string())?
                }
                Err(err) => return Err(err.to_string()),
            };
            let tier = extract_antigravity_tier(&opened.raw);
            let opts = extract_nova_model_options(&opened.config_options, opened.models.as_ref())
                .ok_or_else(|| "Antigravity 未返回可用模型列表".to_string())?;
            Ok((opts, tier))
        }
        .await;

        conn.terminate(500).await;
        if let Some(pid) = conn.pid() {
            let _ = tokio::task::spawn_blocking(move || kill_process_tree(pid)).await;
        }

        let (options, tier) = result?;
        if let Some((tier_name, tier_id)) = tier {
            if let Some(snap) = self.accounts.update_active_tier(tier_name, tier_id) {
                self.emit_accounts(&snap);
            }
        }
        self.publish_model_options(options.clone());
        self.model_options_revalidated.store(true, Ordering::SeqCst);
        Ok(options)
    }

    fn publish_model_options(&self, options: Value) {
        *self.model_options.lock().unwrap() = Some(options.clone());
        model_cache::save(&nova_data_dir(&self.app), "antigravity", &options);
        let _ = self.app.emit(
            EV_OPTIONS,
            json!({ "agentKind": "antigravity", "options": options }),
        );
    }

    /// 同步占位：在 `send_prompt` 返回前即登记为运行中，
    /// 防止并发请求或紧随上轮结束的 `list_threads` 看到假空闲而重复派发/静默丢弃提示词。
    pub fn try_reserve_run(&self, thread_id: &str) -> Option<u64> {
        let mut running = self.running.lock().unwrap();
        if !running.insert(thread_id.to_string()) {
            return None;
        }
        let run_epoch = self.next_run_epoch.fetch_add(1, Ordering::Relaxed);
        self.run_epochs
            .lock()
            .unwrap()
            .insert(thread_id.to_string(), run_epoch);
        self.turn_started
            .lock()
            .unwrap()
            .insert(thread_id.to_string(), Instant::now());
        Some(run_epoch)
    }

    fn release_reserved_run(&self, thread_id: &str, run_epoch: u64) {
        let removed = {
            let mut epochs = self.run_epochs.lock().unwrap();
            if epochs.get(thread_id).copied() == Some(run_epoch) {
                epochs.remove(thread_id);
                true
            } else {
                false
            }
        };
        if removed {
            self.turn_started.lock().unwrap().remove(thread_id);
            self.running.lock().unwrap().remove(thread_id);
        }
    }

    pub async fn run_prompt(
        self: Arc<Self>,
        thread_id: String,
        text: String,
        images: Vec<PromptImage>,
    ) {
        let Some(run_epoch) = self.try_reserve_run(&thread_id) else {
            return;
        };
        self.run_prompt_reserved(thread_id, run_epoch, text, images)
            .await;
    }

    pub async fn run_prompt_reserved(
        self: Arc<Self>,
        thread_id: String,
        run_epoch: u64,
        text: String,
        images: Vec<PromptImage>,
    ) {
        let account = self.account_gate.try_read();
        let mut title_job: Option<(String, String)> = None;
        let (cwd, model, reasoning_effort, saved_session_id, saved_account_id, initial_context) = {
            let state = self.app.state::<AppState>();
            let mut store = state.store.lock().unwrap();
            let Some(thread) = store.get_mut(&thread_id) else {
                drop(store);
                self.release_reserved_run(&thread_id, run_epoch);
                return;
            };
            if thread.agent_kind != AgentKind::Antigravity {
                drop(store);
                self.release_reserved_run(&thread_id, run_epoch);
                self.push_system(
                    &thread_id,
                    "该会话未绑定到 Antigravity 后端，未执行请求".into(),
                    "error",
                );
                self.set_running(&thread_id, false, Some("error"));
                return;
            }
            // Do not consume a staged handoff for a request rejected during account selection.
            let context = if account.is_ok() { thread.take_prompt_context("Antigravity") } else { None };
            let session_id = thread.acp_session_id.clone();
            let account_id = thread.acp_account_id.clone();
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
                thread.reasoning_effort.clone(),
                session_id,
                account_id,
                context,
            );
            store.save_thread(&thread_id);
            values
        };

        if let Some((prompt, fallback)) = title_job {
            self.app.state::<AppState>().generate_title(
                &AgentKind::Antigravity,
                thread_id.clone(),
                prompt,
                fallback,
            );
        }

        if !self.is_current_run(&thread_id, run_epoch) {
            return;
        }
        self.set_running(&thread_id, true, None);

        let result = match account {
            Ok(_account) => self.execute_turn(
                &thread_id,
                run_epoch,
                &cwd,
                model.as_deref(),
                reasoning_effort.as_deref(),
                saved_session_id.as_deref(),
                saved_account_id.as_deref(),
                initial_context,
                &text,
                &images,
            ).await,
            Err(_) => Err(AcpError::spawn("正在切换 Antigravity 账号，本条请求未发送，请在登录完成后重试")),
        };

        self.active_turns.lock().unwrap().retain(|id, turn| {
            if id == &thread_id && turn.run_epoch == run_epoch {
                turn.cancelled.store(true, Ordering::SeqCst);
                false
            } else { true }
        });

        if !self.is_current_run(&thread_id, run_epoch) {
            return;
        }

        match result {
            Ok((stop_reason, usage, notice)) => {
                if let Some((level, msg)) = notice {
                    self.push_system(&thread_id, msg, level);
                }
                self.finish_turn_if_current(&thread_id, run_epoch, stop_reason, usage);
                if stop_reason != "cancelled" {
                    self.spawn_refresh_quota(true);
                }
            }
            Err(err) => {
                let stop_reason = if err.kind == AcpErrorKind::Aborted {
                    "cancelled"
                } else {
                    let prefix = match err.kind {
                        AcpErrorKind::Quota => "Antigravity 配额受限：",
                        AcpErrorKind::Auth => "Antigravity 鉴权失败（请先在终端登录 agy）：",
                        AcpErrorKind::Transport => "Antigravity 网络连接异常：",
                        _ => "Antigravity 请求失败：",
                    };
                    self.push_system(&thread_id, format!("{prefix}{err}"), "error");
                    "error"
                };
                self.finish_turn_if_current(&thread_id, run_epoch, stop_reason, None);
                if stop_reason != "cancelled" {
                    self.spawn_refresh_quota(true);
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn execute_turn(
        &self,
        thread_id: &str,
        run_epoch: u64,
        cwd: &str,
        requested_model: Option<&str>,
        requested_effort: Option<&str>,
        saved_session_id: Option<&str>,
        saved_account_id: Option<&str>,
        mut extra_context: Option<String>,
        text: &str,
        images: &[PromptImage],
    ) -> Result<(&'static str, Option<Value>, Option<(&'static str, String)>), AcpError> {
        let canonical_cwd = canonical_cwd_string(std::path::Path::new(cwd));
        let mut binding_arc: Option<Arc<tokio::sync::Mutex<SessionBinding>>> = None;
        let mut continuation_prompt: Option<&'static str> = None;
        let mut item_state = StreamItemState::default();

        for attempt in 0..=1 {
            if binding_arc.is_none() {
                // 1. 获取或新建独占 `agy acp` 绑定（严格 1 活跃会话 = 1 子进程 + 1 MCP socket）
                let mut acquired = if attempt == 0 {
                    self.ensure_binding(
                        thread_id,
                        &canonical_cwd,
                        saved_session_id,
                        saved_account_id,
                        extra_context.is_some(),
                        &mut extra_context,
                    )
                    .await?
                } else {
                    let (latest_sid, latest_acc) = self.read_saved_session_owner(thread_id);
                    if latest_sid.is_some() {
                        extra_context = None;
                    }
                    self.ensure_binding(
                        thread_id,
                        &canonical_cwd,
                        latest_sid.as_deref(),
                        latest_acc.as_deref(),
                        false,
                        &mut extra_context,
                    )
                    .await?
                };

                // 2. 热切模型与思考强度（优先 session/set_config_option，回退 session/set_model；
                //    两步切换若进入半切换态，则丢弃 binding 重建一次避免状态漂移）。
                let switch_res = self
                    .sync_binding_model_and_effort(&acquired, requested_model, requested_effort)
                    .await;
                if let Err(ModelSwitchError::HalfSwitched { error }) = switch_res {
                    let _ = self.app.emit(
                        EV_LOG,
                        format!("[Antigravity] 模型/思考强度半切换失败（{error}），正在重建会话绑定"),
                    );
                    self.drop_and_terminate_binding(thread_id, &acquired);
                    let (latest_sid, latest_acc) = self.read_saved_session_owner(thread_id);
                    acquired = self
                        .ensure_binding(
                            thread_id,
                            &canonical_cwd,
                            latest_sid.as_deref(),
                            latest_acc.as_deref(),
                            false,
                            &mut extra_context,
                        )
                        .await?;
                    if let Err(err2) = self
                        .sync_binding_model_and_effort(&acquired, requested_model, requested_effort)
                        .await
                    {
                        let _ = self.app.emit(
                            EV_LOG,
                            format!("[Antigravity] 重建后切换模型/思考强度失败（继续使用默认）：{err2}"),
                        );
                    }
                } else if let Err(ModelSwitchError::Failed(err)) = switch_res {
                    let _ = self.app.emit(
                        EV_LOG,
                        format!("[Antigravity] 切换模型/思考强度失败（继续当前配置）：{err}"),
                    );
                }

                binding_arc = Some(acquired);
            }

            let cur_binding = binding_arc.as_ref().unwrap().clone();
            let in_use_guard = BindingInUseGuard::acquire(self, thread_id);

            let (connection, inbound_rx, session_id, supports_images) = {
                let binding = cur_binding.lock().await;
                (
                    binding.connection.clone(),
                    binding.inbound_rx.clone(),
                    binding.session_id.clone(),
                    binding.capabilities.prompt.image,
                )
            };

            // 3. 构造 ACP prompt 载荷（斜杠转义、188KB 单步上限、图片块）
            let prompt_blocks = if let Some(cont) = continuation_prompt {
                build_prompt_blocks(None, cont, &[], supports_images)
            } else {
                build_prompt_blocks(extra_context.as_deref(), text, images, supports_images)
            };

            let cancel_notify = Arc::new(Notify::new());
            let cancelled = Arc::new(AtomicBool::new(false));
            self.active_turns.lock().unwrap().insert(
                thread_id.to_string(),
                ActiveTurn {
                    run_epoch,
                    connection: connection.clone(),
                    session_id: session_id.clone(),
                    cancel_notify: cancel_notify.clone(),
                    cancelled: cancelled.clone(),
                },
            );

            if !self.is_current_run(thread_id, run_epoch) {
                return Err(AcpError::aborted("已取消当前任务"));
            }
            let mut mapper = TurnMapper::new();
            let watchdog = PromptWatchdog::default_timeouts();
            let mut quota_tracker = ResourceExhaustionTracker::new();
            let mut diagnostics = TurnDiagnostics::default();

            // 4. 并发执行 session/prompt、消费 session/update、stderr 与 agy 日志重试监测
            let prompt_fut = connection.session_prompt(SessionPromptParams {
                session_id: session_id.clone(),
                prompt: prompt_blocks,
            });
            tokio::pin!(prompt_fut);

            let mut rx_guard = inbound_rx.lock().await;
            let mut first_reply_notice = Some(tokio::time::Instant::now() + Duration::from_secs(60));
            let mut mid_turn_notice: Option<tokio::time::Instant> = None;
            let mut mid_turn_notice_shown = false;
            let mut response_boundary = false;
            let mut prompt_reply: Option<PromptResult> = None;
            let mut following_command = false;
            let mut tail_state = CommandTail::default();
            let prompt_outcome: Result<PromptResult, AcpError> = loop {
                if response_boundary && !following_command {
                    if let Some(reply) = prompt_reply.take() {
                        if reply.stop_reason != "end_turn" || !mapper.has_running_command() || cancelled.load(Ordering::SeqCst) {
                            break Ok(reply);
                        }
                        prompt_reply = Some(reply);
                        following_command = true;
                        item_state.close_text_blocks();
                        self.push_system(thread_id, "Antigravity 后台命令仍在运行，正在等待结果及后续回复。".into(), "info");
                    }
                }
                let settle_deadline = tail_state.next_settle_at();
                tokio::select! {
                    biased;
                    _ = cancel_notify.notified() => {
                        mapper.mark_cancelled();
                        if prompt_reply.is_some() { break Err(AcpError::aborted("已取消当前任务")); }
                        match tokio::time::timeout(Duration::from_millis(CANCEL_GRACE_MS), &mut prompt_fut).await {
                            Ok(res) => {
                                while rx_guard.try_recv().is_ok() {}
                                break res;
                            }
                            Err(_) => break Err(AcpError::aborted("已取消当前任务")),
                        }
                    }
                    res = &mut prompt_fut, if prompt_reply.is_none() => {
                        match res {
                            Ok(reply) => prompt_reply = Some(reply),
                            Err(err) => break Err(err),
                        }
                    }
                    timeout_info = watchdog.wait_for_timeout() => {
                        let mins = (timeout_info.timeout.as_secs() / 60).max(1);
                        let phase = if timeout_info.awaiting_first_progress {
                            "未收到首条执行进展"
                        } else if timeout_info.had_active_work {
                            "未收到正在执行的工具或权限请求的进展"
                        } else {
                            "未收到新的执行进展"
                        };
                        let detail = if let Some(ref issue) = diagnostics.last_issue {
                            format!("（最近状态：{issue}）")
                        } else if timeout_info.awaiting_first_progress && !diagnostics.saw_stream_headers {
                            "（未收到上游流响应头，多为代理或上游长连接挂起）".into()
                        } else if !timeout_info.had_active_work && diagnostics.saw_stream_headers {
                            "（上游流中途静默中断）".into()
                        } else {
                            String::new()
                        };
                        if let Some(log_path) = connection.log_file() {
                            let _ = self.app.emit(
                                EV_LOG,
                                format!(
                                    "[Antigravity] 轮次超时（{mins}m，{phase}），ACP 日志：{}",
                                    log_path.display()
                                ),
                            );
                        }
                        self.forget_stalled_session(thread_id, run_epoch, &session_id);
                        let pid = connection.pid();
                        let _ = connection.session_cancel(&session_id).await;
                        connection.terminate(CANCEL_GRACE_MS).await;
                        if let Some(pid) = pid {
                            let _ = tokio::task::spawn_blocking(move || kill_process_tree(pid)).await;
                        }
                        break Err(AcpError::timeout(format!(
                            "Antigravity 连续 {mins} 分钟{phase}{detail}，已自动中止本轮。下次发送会新建 CLI 会话并接回本地记录；本次请求不会自动重发。"
                        ))
                        .with_stderr(connection.diagnostic_tail()));
                    }
                    maybe_ev = rx_guard.recv(), if !response_boundary || following_command => {
                        let ev = match maybe_ev {
                            Some(InboundEvent::PromptResponse) => { response_boundary = true; continue; }
                            Some(InboundEvent::SessionUpdate { session_id: sid, .. }) if sid != session_id => continue,
                            Some(InboundEvent::ProcessExited { stderr_tail, .. }) => break Err(AcpError::process_exit("Antigravity 子进程已断开", stderr_tail.or_else(|| connection.log_tail()))),
                            Some(InboundEvent::ProtocolError(err)) => break Err(err),
                            Some(ev) => ev,
                            None => break Err(AcpError::process_exit("ACP 事件通道已关闭", connection.stderr_tail().or_else(|| connection.log_tail()))),
                        };
                        if let Some(quota_msg) = self.handle_inbound_event(
                            thread_id,
                            run_epoch,
                            &session_id,
                            &connection,
                            &ev,
                            &mut mapper,
                            &mut item_state,
                            &watchdog,
                            &mut quota_tracker,
                            &mut diagnostics,
                        ) {
                            let pid = connection.pid();
                            let _ = connection.session_cancel(&session_id).await;
                            connection.terminate(CANCEL_GRACE_MS).await;
                            if let Some(pid) = pid {
                                let _ = tokio::task::spawn_blocking(move || kill_process_tree(pid)).await;
                            }
                            break Err(AcpError::new(AcpErrorKind::Quota, quota_msg));
                        }
                        let had_progress = match &ev {
                            InboundEvent::SessionUpdate { update, .. } => {
                                let kind = update["sessionUpdate"].as_str().unwrap_or_default();
                                matches!(
                                    kind,
                                    "agent_message_chunk"
                                        | "agent_thought_chunk"
                                        | "tool_call"
                                        | "tool_call_update"
                                        | "plan"
                                        | "plan_update"
                                        | "compaction_update"
                                        | "compaction_summary_chunk"
                                        | "subagent_update"
                                )
                            }
                            InboundEvent::PermissionAutoApproved { .. }
                            | InboundEvent::PermissionRejected { .. }
                            | InboundEvent::PermissionRequest { .. }
                            | InboundEvent::ElicitationRequest { .. } => true,
                            _ => false,
                        };
                        if !watchdog.awaiting_first_progress() {
                            first_reply_notice = None;
                            if watchdog.has_active_work() {
                                mid_turn_notice = None;
                                mid_turn_notice_shown = false;
                            } else if had_progress {
                                mid_turn_notice_shown = false;
                                mid_turn_notice =
                                    Some(tokio::time::Instant::now() + Duration::from_secs(90));
                            }
                        }
                        if following_command {
                            if let InboundEvent::SessionUpdate { update, .. } = &ev { tail_state.observe(update, &mapper); }
                        }
                    }
                    _ = async { if let Some(at) = settle_deadline { tokio::time::sleep_until(at).await; } }, if settle_deadline.is_some() => {
                        break Ok(prompt_reply.take().expect("autonomous reply"));
                    }
                    _ = async { if let Some(at) = first_reply_notice { tokio::time::sleep_until(at).await; } }, if first_reply_notice.is_some() => {
                        first_reply_notice = None;
                        if !following_command && watchdog.awaiting_first_progress() && diagnostics.retry_notices_sent == 0 {
                            self.push_system(
                                thread_id,
                                "请求已发往 Antigravity，1 分钟内尚无执行进展。仍在等待（满 3 分钟仍无进展将自动中止并切换新会话），可手动停止；不会自动重发。".into(),
                                "info",
                            );
                        }
                    }
                    _ = async { if let Some(at) = mid_turn_notice { tokio::time::sleep_until(at).await; } }, if mid_turn_notice.is_some() => {
                        mid_turn_notice = None;
                        if !following_command && !watchdog.awaiting_first_progress() && !watchdog.has_active_work() && !mid_turn_notice_shown {
                            mid_turn_notice_shown = true;
                            self.push_system(
                                thread_id,
                                "Antigravity 已超过 90 秒无新输出，仍在等待上游响应（满 5 分钟无进展将自动中止），可随时手动停止。".into(),
                                "info",
                            );
                        }
                    }
                }
            };
            for action in mapper.flush_pending() {
                self.apply_mapped_action(thread_id, action, &mut item_state);
            }
            drop(rx_guard);
            drop(in_use_guard);

            // Even an early cancel can leave a late RPC boundary or background update in this receiver.
            if cancelled.load(Ordering::SeqCst) || mapper.is_cancelled() {
                self.drop_binding_if_same(thread_id, &cur_binding);
                let pid = connection.pid();
                connection.terminate(0).await;
                if let Some(pid) = pid {
                    let _ = tokio::task::spawn_blocking(move || kill_process_tree(pid)).await;
                }
                return Ok(("cancelled", None, None));
            }
            match prompt_outcome {
                Ok(res) => {
                    let reconciled = mapper.reconcile_usage(res.usage.as_ref());
                    if let Some(raw_401) = mapper.take_inline_unauthenticated() {
                        if attempt == 0 {
                            if !mapper.has_substantive_output() {
                                let _ = self.app.emit(
                                    EV_LOG,
                                    "[Antigravity] 检测到瞬态 401（UNAUTHENTICATED），正在重建会话并自动重试 1 次",
                                );
                                self.drop_and_terminate_binding(thread_id, &cur_binding);
                                binding_arc = None;
                                if !wait_auth_retry_delay(
                                    INLINE_AUTH_RETRY_DELAY_CLEAN,
                                    &cancel_notify,
                                    &cancelled,
                                )
                                .await
                                    || !self.is_current_run(thread_id, run_epoch)
                                {
                                    return Ok(("cancelled", None, None));
                                }
                                continue;
                            } else {
                                item_state.close_text_blocks();
                                let _ = self.app.emit(
                                    EV_LOG,
                                    "[Antigravity] 轮次中途遇到瞬态 401（UNAUTHENTICATED），等待令牌刷新后自动继续",
                                );
                                self.push_system(
                                    thread_id,
                                    "Antigravity 令牌到期触发瞬态 401，正在自动继续当前任务…".into(),
                                    "info",
                                );
                                if !wait_auth_retry_delay(
                                    INLINE_AUTH_RETRY_DELAY_MID_TURN,
                                    &cancel_notify,
                                    &cancelled,
                                )
                                .await
                                    || !self.is_current_run(thread_id, run_epoch)
                                {
                                    return Ok(("cancelled", None, None));
                                }
                                if connection.is_closed() {
                                    self.drop_and_terminate_binding(thread_id, &cur_binding);
                                    binding_arc = None;
                                }
                                continuation_prompt = Some(MID_TURN_AUTH_CONTINUE_PROMPT);
                                continue;
                            }
                        }
                        self.drop_and_terminate_binding(thread_id, &cur_binding);
                        return Ok((
                            "error",
                            reconciled,
                            Some((
                                "error",
                                format!("Antigravity 鉴权失败（HTTP 401），已自动重试一次仍未恢复，请检查网络代理或重新登录 Antigravity：{raw_401}"),
                            )),
                        ));
                    }
                    let (stop_reason, notice) =
                        mapper.normalize_stop_reason(&res.stop_reason, reconciled.as_ref());
                    return Ok((stop_reason, reconciled, notice));
                }
                Err(err) => {
                    if err.kind == AcpErrorKind::Aborted {
                        return Ok(("cancelled", None, None));
                    }
                    if err.kind == AcpErrorKind::Auth {
                        self.drop_and_terminate_binding(thread_id, &cur_binding);
                        binding_arc = None;
                        if attempt == 0
                            && !mapper.has_substantive_output()
                            && !is_permanent_auth_error(&err.message)
                        {
                            let _ = self.app.emit(
                                EV_LOG,
                                format!("[Antigravity] 检测到瞬态鉴权失败（{err}），正在重建会话并自动重试 1 次"),
                            );
                            if !wait_auth_retry_delay(
                                INLINE_AUTH_RETRY_DELAY_CLEAN,
                                &cancel_notify,
                                &cancelled,
                            )
                            .await
                                || !self.is_current_run(thread_id, run_epoch)
                            {
                                return Ok(("cancelled", None, None));
                            }
                            continue;
                        }
                        return Err(err);
                    }
                    if matches!(
                        err.kind,
                        AcpErrorKind::ProcessExit
                            | AcpErrorKind::Protocol
                            | AcpErrorKind::Timeout
                            | AcpErrorKind::Transport
                            | AcpErrorKind::Quota
                    ) {
                        self.drop_binding_if_same(thread_id, &cur_binding);
                    }
                    return Err(err);
                }
            }
        }
        Err(AcpError::spawn("Antigravity 重试状态异常"))
    }

    async fn sync_binding_model_and_effort(
        &self,
        binding_arc: &Arc<tokio::sync::Mutex<SessionBinding>>,
        requested_model: Option<&str>,
        requested_effort: Option<&str>,
    ) -> Result<(), ModelSwitchError> {
        let mut binding = binding_arc.lock().await;
        let model_diff = requested_model
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .is_some_and(|m| binding.active_model.as_deref() != Some(m));
        let effort_diff = requested_effort
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .is_some_and(|e| binding.active_effort.as_deref() != Some(e));

        if !model_diff && !effort_diff {
            return Ok(());
        }

        let switched = switch_model_and_effort(
            &binding.connection,
            &binding.session_id,
            &binding.config_options,
            if model_diff { requested_model } else { None },
            if effort_diff { requested_effort } else { None },
        )
        .await?;

        if let Some(m) = switched.applied_model {
            binding.active_model = Some(m);
        }
        if let Some(e) = switched.applied_effort {
            binding.active_effort = Some(e);
        }
        if let Some(updated_opts) = switched.updated_config_options {
            binding.config_options = updated_opts;
        }
        Ok(())
    }

    async fn ensure_binding(
        &self,
        thread_id: &str,
        canonical_cwd: &str,
        saved_session_id: Option<&str>,
        saved_account_id: Option<&str>,
        has_handoff: bool,
        extra_context: &mut Option<String>,
    ) -> Result<Arc<tokio::sync::Mutex<SessionBinding>>, AcpError> {
        let polaris_enabled = self.app.state::<AppState>().settings.lock().unwrap().context_tools_enabled();
        let (current_account_id, current_app_data_dir, current_token_expiry_ms) =
            self.accounts.active_account_context_with_expiry();
        let existing = self
            .pool
            .lock()
            .unwrap()
            .touch(thread_id, tokio::time::Instant::now())
            .cloned();
        if let Some(binding_arc) = existing {
            let (reusable, recycle_reason) = {
                let b = binding_arc.lock().await;
                let expiring = should_recycle_binding_for_token_expiry(
                    b.loaded_token_expiry_ms,
                    current_token_expiry_ms,
                    now_ms(),
                );
                let ok = !b.connection.is_closed()
                    && b.polaris_enabled == polaris_enabled
                    && b.cwd == canonical_cwd
                    && b.account_id == current_account_id
                    && b.app_data_dir == current_app_data_dir
                    && !has_handoff
                    && !expiring;
                (ok, expiring)
            };
            if reusable {
                return Ok(binding_arc);
            }
            if recycle_reason {
                let _ = self.app.emit(
                    EV_LOG,
                    "[Antigravity] 热进程加载的 OAuth 令牌临近过期，主动重建连接以刷新令牌",
                );
            }
            self.forget_session_of_thread(thread_id);
        }

        // 切号防串：只有旧 session 明确属于当前活跃账号时才允许 session/resume / session/load；
        // 跨账号或未标记归属的历史会话绝不拿旧 sessionId 去探测当前账号，直接新建会话并用 Nova 本地历史恢复。
        let allow_resume = can_reuse_saved_session(
            saved_session_id,
            saved_account_id,
            current_account_id.as_deref(),
        );
        let switched_account = saved_session_id.is_some() && !allow_resume;
        let effective_saved_session_id = if allow_resume { saved_session_id } else { None };

        let cfg = self
            .launch_acp_config_with_dir(PathBuf::from(canonical_cwd), current_app_data_dir.clone())
            .map_err(AcpError::spawn)?;
        // 预热的 draft_cli 仅完成了 initialize，尚未创建会话，新建/恢复/接力都可直接复用以缩短冷启耗时。
        let now = now_ms();
        let claim = self
            .draft_cli
            .lock()
            .unwrap()
            .take_if(|(key, warm_expiry, _)| {
                key.same_environment(&cfg)
                    && !should_recycle_binding_for_token_expiry(
                        *warm_expiry,
                        current_token_expiry_ms,
                        now,
                    )
            })
            .map(|(_, _, claim)| claim);
        let ready = if let Some(claim) = claim {
            let (reply, rx) = oneshot::channel();
            if claim.send(reply).is_ok() { rx.await.ok() } else { None }
        } else { None };
        let (conn, mut rx, capabilities) = match ready {
            Some(cli) if !cli.0.is_closed() => cli,
            _ => initialize_cli(cfg).await?,
        };

        // 为本会话启动独占的本机回环 IPC 服务端（Unix 0600 socket / Windows named pipe）
        let app_data_dir = nova_data_dir(&self.app);
        let (mcp_bridge, mcp_server_spec) =
            match start_session_bridge(&self.app, &app_data_dir, thread_id, canonical_cwd, conn.pid(), polaris_enabled) {
                Ok(pair) => pair,
                Err(err) => {
                    conn.terminate(500).await;
                    return Err(AcpError::spawn(format!(
                        "启动 Antigravity MCP 工具桥失败：{err}"
                    )));
                }
            };

        let global_instructions =
            agent_config::global_instructions_for_agent(&app_data_dir, "antigravity");
        let session_meta = build_session_meta(
            &capabilities,
            true,
            polaris_enabled,
            global_instructions.as_deref(),
        );

        let opened_session = match open_or_recover_session(
            &conn,
            &mut rx,
            &capabilities,
            canonical_cwd,
            vec![mcp_server_spec],
            session_meta,
            effective_saved_session_id,
            has_handoff,
        )
        .await
        {
            Ok(ok) => ok,
            Err(err) => {
                conn.terminate(500).await;
                return Err(err);
            }
        };

        let (_, _, refreshed_expiry_ms) = self.accounts.active_account_context_with_expiry();
        let loaded_token_expiry_ms = refreshed_expiry_ms.or(current_token_expiry_ms);

        if matches!(opened_session.mode, SessionOpenMode::Created | SessionOpenMode::CreatedAfterFallback) {
            if extra_context.is_none() {
                *extra_context = self.build_fallback_handoff(thread_id);
                if extra_context.is_some() {
                    let notice = if switched_account {
                        "已切换 Antigravity 账号，已在新账号独立目录下新建会话并用 Nova 本地历史续聊。"
                    } else {
                        "已新建 Antigravity 会话，用此对话的本地历史续聊。"
                    };
                    self.push_system(thread_id, notice.into(), "info");
                }
            }
        } else if opened_session.mode == SessionOpenMode::Loaded
            && opened_session.discarded_replay_events > 0
        {
            let _ = self.app.emit(
                EV_LOG,
                format!(
                    "[Antigravity] session/load 恢复成功，已静默丢弃 {} 条历史重放事件",
                    opened_session.discarded_replay_events
                ),
            );
        }

        let mut opened = opened_session.result;
        if let Some(replayed_opts) = opened_session.replayed_config_options {
            if opened.config_options.is_empty() {
                opened.config_options = replayed_opts;
            }
        }

        // 立即持久化 acpSessionId 与归属账号 ID，支持同号恢复并防止跨号探测
        self.save_session_id(thread_id, &opened.session_id, current_account_id.as_deref());

        if let Some((tier_name, tier_id)) = extract_antigravity_tier(&opened.raw) {
            if let Some(snap) = self.accounts.update_active_tier(tier_name, tier_id) {
                self.emit_accounts(&snap);
            }
        }

        if let Some(opts) =
            extract_nova_model_options(&opened.config_options, opened.models.as_ref())
        {
            self.publish_model_options(opts);
            self.model_options_revalidated.store(true, Ordering::SeqCst);
        }

        // 决策 B：默认协商 accept-edits（永不请求 plan 模式）
        let mut active_wire_mode = opened_session.replayed_wire_mode.or_else(|| {
            opened
                .modes
                .as_ref()
                .and_then(|m| m.get("currentModeId"))
                .and_then(Value::as_str)
                .map(str::to_string)
        });
        if let Some((target_mode, config_id)) =
            find_preferred_wire_mode(opened.modes.as_ref(), &opened.config_options)
        {
            let mode_res = match config_id {
                Some(cid) => conn
                    .session_set_config_option(&opened.session_id, &cid, &target_mode)
                    .await
                    .map(|_| ()),
                None => conn.session_set_mode(&opened.session_id, &target_mode).await,
            };
            match mode_res {
                Ok(()) => active_wire_mode = Some(target_mode),
                Err(err) => {
                    let _ = self.app.emit(
                        EV_LOG,
                        format!("[Antigravity] 协商执行模式 {target_mode} 失败（已忽略）：{err}"),
                    );
                }
            }
        }

        let active_model = opened
            .models
            .as_ref()
            .and_then(|m| m.get("currentModelId"))
            .and_then(Value::as_str)
            .or_else(|| {
                opened
                    .config_options
                    .iter()
                    .find(|opt| {
                        opt.get("category").and_then(Value::as_str) == Some("model")
                            || opt.get("id").and_then(Value::as_str) == Some("model")
                    })
                    .and_then(|opt| opt.get("currentValue").and_then(Value::as_str))
            })
            .map(str::to_string);

        let active_effort = opened
            .config_options
            .iter()
            .find(|opt| {
                let id = opt.get("id").and_then(Value::as_str).unwrap_or_default();
                let cat = opt.get("category").and_then(Value::as_str).unwrap_or_default();
                cat == "thought_level"
                    || id == "thought_level"
                    || id == "thinking_level"
                    || id == "reasoning_effort"
            })
            .and_then(|opt| opt.get("currentValue").and_then(Value::as_str))
            .map(str::to_string);

        let binding = Arc::new(tokio::sync::Mutex::new(SessionBinding {
            connection: conn,
            inbound_rx: Arc::new(tokio::sync::Mutex::new(rx)),
            _mcp_bridge: mcp_bridge,
            polaris_enabled,
            session_id: opened.session_id,
            cwd: canonical_cwd.to_string(),
            account_id: current_account_id,
            app_data_dir: current_app_data_dir,
            loaded_token_expiry_ms,
            capabilities,
            config_options: opened.config_options,
            active_model,
            active_effort,
            active_wire_mode,
        }));

        let evicted = self.pool.lock().unwrap().insert(
            thread_id.to_string(),
            binding.clone(),
            tokio::time::Instant::now(),
        );
        for (_, old_binding) in evicted {
            spawn_terminate_binding(old_binding, CANCEL_GRACE_MS);
        }

        Ok(binding)
    }

    fn build_fallback_handoff(&self, thread_id: &str) -> Option<String> {
        let state = self.app.state::<AppState>();
        let store = state.store.lock().unwrap();
        history_before_prompt(store.get(thread_id)?)
    }

    fn read_saved_session_owner(&self, thread_id: &str) -> (Option<String>, Option<String>) {
        let state = self.app.state::<AppState>();
        let store = state.store.lock().unwrap();
        match store.get(thread_id) {
            Some(t) => (t.acp_session_id.clone(), t.acp_account_id.clone()),
            None => (None, None),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_inbound_event(
        &self,
        thread_id: &str,
        run_epoch: u64,
        session_id: &str,
        connection: &Arc<AcpConnection>,
        event: &InboundEvent,
        mapper: &mut TurnMapper,
        item_state: &mut StreamItemState,
        watchdog: &PromptWatchdog,
        quota_tracker: &mut ResourceExhaustionTracker,
        diagnostics: &mut TurnDiagnostics,
    ) -> Option<String> {
        if !self.is_current_run(thread_id, run_epoch) || mapper.is_cancelled() {
            return None;
        }
        match event {
            InboundEvent::PromptResponse => {}
            InboundEvent::SessionUpdate { update, .. } => {
                if watchdog.observe_update(update) {
                    diagnostics.last_issue = None;
                }
                let actions = mapper.on_update(update);
                for action in actions {
                    if matches!(
                        action,
                        MappedAction::AssistantDelta(_)
                            | MappedAction::ThoughtDelta(_)
                            | MappedAction::ToolUpsert(_)
                    ) {
                        quota_tracker.note_progress();
                    }
                    if let MappedAction::ToolUpsert(ref call) = action {
                        if matches!(call.status.as_str(), "completed" | "failed") {
                            watchdog.tool_finished(&call.tool_call_id);
                        } else {
                            watchdog.tool_started(call.tool_call_id.clone());
                        }
                    }
                    self.apply_mapped_action(thread_id, action, item_state);
                }
            }
            InboundEvent::StderrLine(line) => {
                if let Some(quota_msg) = quota_tracker.observe_stderr_line(line) {
                    let _ = self
                        .app
                        .emit(EV_LOG, format!("[Antigravity] 触发 429 配额风暴保护：{quota_msg}"));
                    return Some(quota_msg);
                }
            }
            InboundEvent::LogLine(line) => {
                if let Some(quota_msg) = quota_tracker.observe_stderr_line(line) {
                    let _ = self
                        .app
                        .emit(EV_LOG, format!("[Antigravity] 日志触发 429 配额风暴保护：{quota_msg}"));
                    return Some(quota_msg);
                }
                if let Some(signal) = parse_agy_log_line(line) {
                    match signal {
                        AgyLogSignal::StreamHeadersReceived => {
                            diagnostics.saw_stream_headers = true;
                        }
                        AgyLogSignal::RetryAttempt {
                            attempt,
                            reason,
                            retry_in_ms,
                            is_quota,
                        } => {
                            diagnostics.last_issue = Some(reason.clone());
                            let delay_desc = retry_in_ms
                                .map(|ms| format!("约 {:.1}s 后", (ms as f64 / 1000.0).max(0.1)))
                                .unwrap_or_else(|| "稍后".into());
                            let _ = self.app.emit(
                                EV_LOG,
                                format!(
                                    "[Antigravity] CLI 重试（第 {attempt} 次，{delay_desc}）：{reason}"
                                ),
                            );
                            if !is_quota {
                                watchdog.note_retry_backoff(Duration::from_millis(
                                    retry_in_ms.unwrap_or(4_000),
                                ));
                                if diagnostics.retry_notices_sent < 2 {
                                    diagnostics.retry_notices_sent += 1;
                                    self.push_system(
                                        thread_id,
                                        format!(
                                            "Antigravity 遇到网络波动（{reason}），CLI 正在自动重试（第 {attempt} 次，{delay_desc}）…"
                                        ),
                                        "info",
                                    );
                                }
                            }
                        }
                        AgyLogSignal::NetworkFailure(reason) => {
                            diagnostics.last_issue = Some(reason.clone());
                            let _ = self.app.emit(
                                EV_LOG,
                                format!("[Antigravity] CLI 日志报告网络异常：{reason}"),
                            );
                        }
                    }
                }
            }
            InboundEvent::PermissionAutoApproved {
                tool_call_id,
                title,
                option_id,
                ..
            } => {
                watchdog.record_activity();
                let _ = self.app.emit(
                    EV_LOG,
                    format!(
                        "[Antigravity] 自动放行权限请求：{title} (toolCallId={tool_call_id}, optionId={option_id})"
                    ),
                );
            }
            InboundEvent::PermissionRejected {
                tool_call_id,
                reason,
                ..
            } => {
                watchdog.record_activity();
                let _ = self.app.emit(
                    EV_LOG,
                    format!(
                        "[Antigravity] 已拒绝异常权限请求（toolCallId={tool_call_id}）：{reason}"
                    ),
                );
            }
            InboundEvent::PermissionRequest { .. } => {
                watchdog.record_activity();
            }
            InboundEvent::ElicitationRequest { rpc_id, params } => {
                if !params.session_id.is_empty() && params.session_id != session_id {
                    let conn = connection.clone();
                    let id = rpc_id.clone();
                    tauri::async_runtime::spawn(async move {
                        let _ = conn
                            .respond_elicitation(id, json!({ "action": "cancel" }))
                            .await;
                    });
                    return None;
                }
                watchdog.permission_started();
                let seq = self.next_elicitation_seq.fetch_add(1, Ordering::Relaxed);
                let request_key = format!("agy-elicit-{thread_id}-{seq}");
                let title = format_elicitation_title(&params.message);
                let questions = params.to_nova_questions();
                let tool_call = json!({
                    "toolCallId": params.tool_call_id,
                    "title": title,
                    "kind": "other",
                    "rawInput": params.raw_schema,
                });
                self.pending_elicitations.lock().unwrap().insert(
                    request_key.clone(),
                    PendingElicitation {
                        thread_id: thread_id.to_string(),
                        connection: connection.clone(),
                        rpc_id: rpc_id.clone(),
                        params: params.clone(),
                        watchdog: watchdog.clone(),
                    },
                );
                let _ = self.app.emit(
                    EV_PERMISSION,
                    json!({
                        "threadId": thread_id,
                        "agentKind": "antigravity",
                        "requestKey": request_key,
                        "toolCall": tool_call,
                        "options": [],
                        "questions": questions,
                    }),
                );
            }
            InboundEvent::ProtocolError(err) => {
                let _ = self
                    .app
                    .emit(EV_LOG, format!("[Antigravity] 协议错误：{err}"));
            }
            InboundEvent::ProcessExited {
                exit_code,
                stderr_tail,
            } => {
                let _ = self.app.emit(
                    EV_LOG,
                    format!(
                        "[Antigravity] 子进程退出（code={exit_code:?}）{}",
                        stderr_tail
                            .as_deref()
                            .map(|t| format!("：{t}"))
                            .unwrap_or_default()
                    ),
                );
            }
        }
        None
    }

    fn apply_mapped_action(
        &self,
        thread_id: &str,
        action: MappedAction,
        item_state: &mut StreamItemState,
    ) {
        match action {
            MappedAction::AssistantDelta(delta) => {
                if item_state.current_assistant_id.is_none() && delta.trim().is_empty() {
                    return;
                }
                item_state.current_thought_id = None;
                let state = self.app.state::<AppState>();
                let mut store = state.store.lock().unwrap();
                let Some(thread) = store.get_mut(thread_id) else {
                    return;
                };
                if let Some(id) = item_state.current_assistant_id {
                    if let Some(Item::Assistant { text, .. }) =
                        thread.items.iter_mut().find(|it| it.id() == id)
                    {
                        text.push_str(&delta);
                        thread.updated_at = now_ms();
                        let _ = self.emit_op(
                            thread_id,
                            json!({ "t": "delta", "itemId": id, "text": delta }),
                        );
                        store.save_thread(thread_id);
                        return;
                    }
                }
                let id = thread.next_item_id();
                let item = Item::Assistant {
                    id,
                    text: delta,
                    ts: now_ms(),
                };
                thread.items.push(item.clone());
                thread.updated_at = now_ms();
                item_state.current_assistant_id = Some(id);
                let _ = self.emit_update(thread_id, &item);
                store.save_thread(thread_id);
            }
            MappedAction::ThoughtDelta(delta) => {
                item_state.current_assistant_id = None;
                let state = self.app.state::<AppState>();
                let mut store = state.store.lock().unwrap();
                let Some(thread) = store.get_mut(thread_id) else {
                    return;
                };
                if let Some(id) = item_state.current_thought_id {
                    if let Some(Item::Thought { text, .. }) =
                        thread.items.iter_mut().find(|it| it.id() == id)
                    {
                        text.push_str(&delta);
                        thread.updated_at = now_ms();
                        let _ = self.emit_op(
                            thread_id,
                            json!({ "t": "delta", "itemId": id, "text": delta }),
                        );
                        store.save_thread(thread_id);
                        return;
                    }
                }
                let id = thread.next_item_id();
                let item = Item::Thought {
                    id,
                    text: delta,
                    ts: now_ms(),
                };
                thread.items.push(item.clone());
                thread.updated_at = now_ms();
                item_state.current_thought_id = Some(id);
                let _ = self.emit_update(thread_id, &item);
                store.save_thread(thread_id);
            }
            MappedAction::ToolUpsert(mapped) => {
                item_state.close_text_blocks();
                let mut call = to_thread_tool_call(mapped);
                let state = self.app.state::<AppState>();
                let mut store = state.store.lock().unwrap();
                let Some(thread) = store.get_mut(thread_id) else {
                    return;
                };
                let existing_id = item_state.tool_item_ids.get(&call.tool_call_id).copied();
                let item = if let Some(id) = existing_id {
                    if let Some(slot) = thread.items.iter_mut().find(|it| it.id() == id) {
                        let started_at = match slot {
                            Item::Tool { ts, .. } => *ts,
                            _ => now_ms(),
                        };
                        if matches!(call.status.as_str(), "completed" | "failed") {
                            set_tool_duration(
                                &mut call,
                                now_ms().saturating_sub(started_at) as u64,
                            );
                        }
                        let next = Item::Tool {
                            id,
                            ts: started_at,
                            call,
                        };
                        *slot = next.clone();
                        next
                    } else {
                        let next = Item::Tool {
                            id,
                            ts: now_ms(),
                            call,
                        };
                        thread.items.push(next.clone());
                        next
                    }
                } else {
                    let id = thread.next_item_id();
                    item_state
                        .tool_item_ids
                        .insert(call.tool_call_id.clone(), id);
                    let next = Item::Tool {
                        id,
                        ts: now_ms(),
                        call,
                    };
                    thread.items.push(next.clone());
                    next
                };
                thread.updated_at = now_ms();
                let _ = self.emit_update(thread_id, &item);
                store.save_thread(thread_id);
            }
            MappedAction::Plan(entries) => {
                let state = self.app.state::<AppState>();
                let mut store = state.store.lock().unwrap();
                let Some(thread) = store.get_mut(thread_id) else {
                    return;
                };
                let plan_val = match entries {
                    Some(list) => {
                        let arr = Value::Array(list);
                        thread.plan = Some(arr.clone());
                        arr
                    }
                    None => {
                        thread.plan = None;
                        Value::Null
                    }
                };
                thread.updated_at = now_ms();
                let _ = self.emit_op(thread_id, json!({ "t": "plan", "plan": plan_val }));
                store.save_thread(thread_id);
            }
            MappedAction::LiveUsage { context_tokens, .. } => {
                let _ = self.emit_op(
                    thread_id,
                    json!({
                        "t": "usage",
                        "usage": { "contextTokens": context_tokens }
                    }),
                );
            }
            MappedAction::SystemNotice { level, text } => {
                item_state.close_text_blocks();
                self.push_system(thread_id, text, &level);
            }
            MappedAction::ModeChanged { wire_mode } => {
                let _ = self.app.emit(
                    EV_LOG,
                    format!("[Antigravity] 会话模式变更为 {wire_mode}（前端保持 Build）"),
                );
            }
            MappedAction::ConfigOptionsUpdated { config_options } => {
                // 仅当未持有更完整的 23 模型清单或条目数不少于当前缓存时覆盖
                let current_count = self
                    .model_options
                    .lock()
                    .unwrap()
                    .as_ref()
                    .and_then(|v| v.pointer("/configOptions/0/options"))
                    .and_then(Value::as_array)
                    .map(|a| a.len())
                    .unwrap_or(0);
                if let Some(opts) = extract_nova_model_options(&config_options, None) {
                    let next_count = opts
                        .pointer("/configOptions/0/options")
                        .and_then(Value::as_array)
                        .map(|a| a.len())
                        .unwrap_or(0);
                    if next_count >= current_count {
                        self.publish_model_options(opts);
                    }
                }
            }
            MappedAction::SessionInfoUpdated { title } => {
                if let Some(title) = title {
                    let state = self.app.state::<AppState>();
                    let mut store = state.store.lock().unwrap();
                    if let Some(thread) = store.get_mut(thread_id) {
                        if thread.title == "新会话" {
                            thread.title = title;
                            store.save_thread(thread_id);
                            let _ = self.app.emit(EV_THREADS, json!({}));
                        }
                    }
                }
            }
            MappedAction::Log(msg) => {
                let _ = self.app.emit(EV_LOG, format!("[Antigravity] {msg}"));
            }
        }
    }

    pub async fn respond_permission(
        &self,
        request_key: &str,
        option_id: &str,
    ) -> Result<(), String> {
        let pending = self
            .pending_elicitations
            .lock()
            .unwrap()
            .remove(request_key)
            .ok_or_else(|| "该确认请求已失效".to_string())?;
        let result = pending.params.build_elicitation_result(option_id);
        let send_res = pending
            .connection
            .respond_elicitation(pending.rpc_id, result)
            .await
            .map_err(|e| e.to_string());
        pending.watchdog.permission_finished();
        let _ = self.app.emit(
            EV_PERMISSION_RESOLVED,
            json!({ "requestKey": request_key }),
        );
        send_res
    }

    fn clear_thread_elicitations(&self, thread_id: &str, send_cancel: bool) {
        let removed: Vec<(String, PendingElicitation)> = {
            let mut guard = self.pending_elicitations.lock().unwrap();
            let keys: Vec<String> = guard
                .iter()
                .filter(|(_, p)| p.thread_id == thread_id)
                .map(|(k, _)| k.clone())
                .collect();
            keys.into_iter()
                .filter_map(|k| guard.remove(&k).map(|p| (k, p)))
                .collect()
        };
        for (key, pending) in removed {
            pending.watchdog.permission_finished();
            let _ = self
                .app
                .emit(EV_PERMISSION_RESOLVED, json!({ "requestKey": key }));
            if send_cancel && !pending.connection.is_closed() {
                let conn = pending.connection;
                let rpc_id = pending.rpc_id;
                tauri::async_runtime::spawn(async move {
                    let _ = conn
                        .respond_elicitation(rpc_id, json!({ "action": "cancel" }))
                        .await;
                });
            }
        }
    }

    pub async fn cancel(&self, thread_id: &str) {
        let Some(epoch) = self.run_epochs.lock().unwrap().get(thread_id).copied() else { return };
        self.clear_thread_elicitations(thread_id, true);
        if self.is_running(thread_id) {
            self.push_system(thread_id, "已停止当前任务。".into(), "warn");
        }
        let active = self
            .active_turns
            .lock()
            .unwrap()
            .get(thread_id)
            .map(|t| {
                (
                    t.run_epoch,
                    t.connection.clone(),
                    t.session_id.clone(),
                    t.cancel_notify.clone(),
                    t.cancelled.clone(),
                )
            });

        if let Some((target_epoch, conn, session_id, notify, cancelled)) = active {
            cancelled.store(true, Ordering::SeqCst);
            let _ = conn.session_cancel(&session_id).await;
            notify.notify_one();

            // 等待最多 CANCEL_GRACE_MS（1.5s）让 session/prompt 优雅收敛
            let steps = (CANCEL_GRACE_MS / 50).max(1);
            for _ in 0..steps {
                tokio::time::sleep(Duration::from_millis(50)).await;
                let still_target = self
                    .active_turns
                    .lock()
                    .unwrap()
                    .get(thread_id)
                    .is_some_and(|t| t.run_epoch == target_epoch);
                if !still_target {
                    break;
                }
            }

            let still_stuck = self
                .active_turns
                .lock()
                .unwrap()
                .get(thread_id)
                .is_some_and(|t| t.run_epoch == target_epoch);
            if still_stuck {
                let pid = conn.pid();
                conn.terminate(0).await;
                if let Some(pid) = pid {
                    let _ = tokio::task::spawn_blocking(move || kill_process_tree(pid)).await;
                }
                let _ = self.pool.lock().unwrap().remove(thread_id);
            }
        }
        self.finish_turn_if_current(thread_id, epoch, "cancelled", None);
    }

    pub fn forget_session_of_thread(&self, thread_id: &str) {
        self.clear_thread_elicitations(thread_id, true);
        if let Some(binding) = self.pool.lock().unwrap().remove(thread_id) {
            spawn_terminate_binding(binding, CANCEL_GRACE_MS);
        }
    }

    /// 重置并清理全部在跑/常驻 `agy` 子进程（用于设置变更热重载，不关闭后台巡检）。
    pub fn restart(&self) {
        self.terminate_all_processes();
    }

    pub fn shutdown(&self) {
        self.shutting_down.store(true, Ordering::SeqCst);
        self.terminate_all_processes();
    }

    fn terminate_all_processes(&self) {
        self.draft_cli.lock().unwrap().take();
        let pending_elicitations =
            std::mem::take(&mut *self.pending_elicitations.lock().unwrap());
        for (key, item) in pending_elicitations {
            item.watchdog.permission_finished();
            let _ = self
                .app
                .emit(EV_PERMISSION_RESOLVED, json!({ "requestKey": key }));
        }
        let active: Vec<_> = std::mem::take(&mut *self.active_turns.lock().unwrap())
            .into_values()
            .collect();
        for turn in active {
            turn.cancelled.store(true, Ordering::SeqCst);
            turn.cancel_notify.notify_one();
            if let Some(pid) = turn.connection.pid() {
                std::thread::spawn(move || kill_process_tree(pid));
            }
        }
        let bindings = self.pool.lock().unwrap().drain_all();
        for (_, binding) in bindings {
            if let Ok(guard) = binding.try_lock() {
                if let Some(pid) = guard.connection.pid() {
                    std::thread::spawn(move || kill_process_tree(pid));
                }
            } else {
                spawn_terminate_binding(Arc::clone(&binding), 500);
            };
        }
    }

    fn forget_stalled_session(&self, thread_id: &str, run_epoch: u64, session_id: &str) {
        let state = self.app.state::<AppState>();
        let mut store = state.store.lock().unwrap();
        if !self.is_current_run(thread_id, run_epoch) {
            return;
        }
        if let Some(thread) = store.get_mut(thread_id) {
            if detach_stalled_session(thread, session_id) {
                store.save_thread(thread_id);
            }
        }
    }

    fn drop_binding_if_same(
        &self,
        thread_id: &str,
        target: &Arc<tokio::sync::Mutex<SessionBinding>>,
    ) {
        let mut pool = self.pool.lock().unwrap();
        if pool
            .get(thread_id)
            .is_some_and(|current| Arc::ptr_eq(current, target))
        {
            let _ = pool.remove(thread_id);
        }
    }

    fn drop_and_terminate_binding(
        &self,
        thread_id: &str,
        target: &Arc<tokio::sync::Mutex<SessionBinding>>,
    ) {
        let removed = {
            let mut pool = self.pool.lock().unwrap();
            if pool
                .get(thread_id)
                .is_some_and(|current| Arc::ptr_eq(current, target))
            {
                pool.remove(thread_id)
            } else {
                None
            }
        };
        if let Some(binding) = removed {
            spawn_terminate_binding(binding, 500);
        }
    }

    fn save_session_id(&self, thread_id: &str, session_id: &str, account_id: Option<&str>) {
        let state = self.app.state::<AppState>();
        let mut store = state.store.lock().unwrap();
        if let Some(thread) = store.get_mut(thread_id) {
            thread.acp_session_id = Some(session_id.to_string());
            thread.acp_account_id = account_id.map(str::to_string);
        }
        store.save_thread(thread_id);
    }

    fn push_system(&self, thread_id: &str, text: String, level: &str) {
        let state = self.app.state::<AppState>();
        let mut store = state.store.lock().unwrap();
        if let Some(thread) = store.get_mut(thread_id) {
            let item = thread.push_system(text, level);
            let _ = self.emit_update(thread_id, &item);
        }
        store.save_thread(thread_id);
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
        self.clear_thread_elicitations(thread_id, false);
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
            for item in settle_unconfirmed_tools(thread) {
                let _ = self.emit_update(thread_id, &item);
            }
            let item = thread.push_turn(duration, usage.as_ref(), stop_reason, false);
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

    fn emit_update(&self, thread_id: &str, item: &Item) -> Result<(), tauri::Error> {
        self.emit_op(thread_id, json!({ "t": "upsert", "item": item }))
    }

    fn emit_op(&self, thread_id: &str, op: Value) -> Result<(), tauri::Error> {
        // 与 LyraRuntime 一致：高频文本/工具增量仅推给前台当前会话，plan/usage/turn 始终推送。
        let always = matches!(op["t"].as_str(), Some("plan" | "usage"))
            || op["item"]["type"] == "turn";
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

impl Drop for AntigravityRuntime {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn spawn_terminate_binding(binding: Arc<tokio::sync::Mutex<SessionBinding>>, grace_ms: u64) {
    tauri::async_runtime::spawn(async move {
        let (conn, sid, supports_close) = {
            let guard = binding.lock().await;
            (
                guard.connection.clone(),
                guard.session_id.clone(),
                guard.capabilities.session.close,
            )
        };
        if supports_close && !conn.is_closed() {
            let _ = conn.session_close(&sid).await;
        }
        let pid = conn.pid();
        conn.terminate(grace_ms).await;
        if let Some(pid) = pid {
            let _ = tokio::task::spawn_blocking(move || kill_process_tree(pid)).await;
        }
        // `binding` 离开作用域时 `_mcp_bridge` 自动关闭并清理 socket 文件
        drop(binding);
    });
}

fn canonical_cwd_string(path: &std::path::Path) -> String {
    let resolved = dunce::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    crate::lyra::runtime::display_working_directory(&resolved)
}

fn build_prompt_blocks(
    extra_context: Option<&str>,
    text: &str,
    images: &[PromptImage],
    supports_images: bool,
) -> Vec<Value> {
    let mut blocks = Vec::new();
    let mut combined_text = String::new();
    if let Some(ctx) = extra_context.filter(|s| !s.trim().is_empty()) {
        combined_text.push_str(ctx);
        if !text.is_empty() {
            combined_text.push_str("\n\n");
            combined_text.push_str(text);
        }
    } else if !text.is_empty() {
        combined_text.push_str(&escape_leading_slash(text));
    }

    let mut attachment_notes = Vec::new();
    let mut image_blocks = Vec::new();
    for img in images {
        let is_image = img.mime_type.starts_with("image/");
        if is_image && supports_images {
            if let Some(data) = img.data.as_ref().filter(|d| !d.is_empty()) {
                image_blocks.push(json!({
                    "type": "image",
                    "data": data,
                    "mimeType": img.mime_type,
                }));
                continue;
            }
            if let Some(uri) = img.uri.as_deref() {
                if let Some(path) = file_uri_to_local_path(uri) {
                    if let Ok(bytes) = std::fs::read(&path) {
                        let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
                        image_blocks.push(json!({
                            "type": "image",
                            "data": b64,
                            "mimeType": img.mime_type,
                        }));
                        continue;
                    }
                }
            }
        }
        // 非图片附件或服务端不支持内嵌图片时，落盘/传路径提示
        if let Some(uri) = img.uri.as_deref() {
            let path = file_uri_to_local_path(uri).unwrap_or_else(|| uri.to_string());
            attachment_notes.push(format!("[附件: {path}]"));
        } else if let Some(path) = save_attachment_to_temp(img) {
            attachment_notes.push(format!("[附件: {path}]"));
        }
    }

    if !attachment_notes.is_empty() {
        if !combined_text.is_empty() {
            combined_text.push('\n');
        }
        combined_text.push_str(&attachment_notes.join("\n"));
    }

    if !combined_text.is_empty() {
        blocks.push(json!({
            "type": "text",
            "text": truncate_step_text(&combined_text, ACP_STEP_TEXT_BYTES),
        }));
    }
    blocks.extend(image_blocks);
    blocks
}

fn to_thread_tool_call(mapped: MappedToolCall) -> ToolCall {
    ToolCall {
        tool_call_id: mapped.tool_call_id,
        title: mapped.title,
        kind: mapped.kind,
        status: mapped.status,
        parallel: false,
        content: compact_tool_values(&mapped.content),
        locations: mapped.locations,
        raw_input: mapped.raw_input.as_ref().map(compact_tool_value),
        raw_output: mapped.raw_output.as_ref().map(compact_tool_value),
    }
}

fn set_tool_duration(call: &mut ToolCall, duration_ms: u64) {
    let output = call.raw_output.get_or_insert_with(|| json!({}));
    if let Some(object) = output.as_object_mut() {
        object.insert("durationMs".into(), json!(duration_ms));
    }
}

fn settle_unconfirmed_tools(thread: &mut crate::threads::Thread) -> Vec<Item> {
    let mut changed = Vec::new();
    let finished_at = now_ms();
    for item in thread.items.iter_mut().rev().take_while(|it| !matches!(it, Item::User { .. } | Item::Turn { .. })) {
        let Item::Tool { ts, call, .. } = item else {
            continue;
        };
        if call.status == "pending" || call.status == "in_progress" {
            call.status = "failed".to_string();
            call.content.push(json!({"type":"content", "content":{"type":"text", "text":"本轮已结束，未收到该工具的最终状态，执行结果未确认。"}}));
            set_tool_duration(call, finished_at.saturating_sub(*ts) as u64);
            changed.push(item.clone());
        }
    }
    changed
}

#[cfg(test)]
#[tokio::test]
async fn draft_ticket_can_be_claimed_before_ready_and_releases_abandoned_preparation() {
    let (go, gate) = oneshot::channel();
    let claim = warm_ticket(async { Ok(gate.await.unwrap()) }, Duration::from_secs(5));
    let (tx, rx) = oneshot::channel();
    claim.send(tx).unwrap();
    go.send(42).unwrap();
    assert_eq!(rx.await.unwrap(), 42);
    let (mut go, gate) = oneshot::channel::<()>();
    drop(warm_ticket(async { Ok(gate.await.unwrap()) }, Duration::from_secs(5)));
    tokio::time::timeout(Duration::from_secs(1), go.closed()).await.unwrap();
}

#[cfg(test)]
#[test]
fn background_command_tail_requires_the_result_and_the_followup_answer() {
    let mut mapper = TurnMapper::new();
    let mut tail = CommandTail::default();
    let start = |id: &str, name: &str| json!({"sessionUpdate":"tool_call", "name":name, "toolCallId":id, "status":"in_progress"});
    let end = |id: &str, status: &str| json!({"sessionUpdate":"tool_call_update", "toolCallId":id, "status":status});
    let answer = json!({"sessionUpdate":"agent_message_chunk", "content":{"type":"text", "text":"result"}});
    let usage = json!({"sessionUpdate":"usage_update", "used":104});
    mapper.on_update(&start("a", "read_file"));
    assert!(!mapper.has_running_command());
    mapper.on_update(&end("a", "completed"));
    mapper.on_update(&start("b", "run_command"));
    assert!(mapper.has_running_command()); // wire-order end_turn here
    let mut apply = |update: Value| {
        mapper.on_update(&update);
        tail.observe(&update, &mapper);
        tail.settle_at.is_some()
    };
    assert!(!apply(answer.clone())); // preliminary answer while command still running
    assert!(!apply(usage.clone()));
    assert!(!apply(end("b", "failed")));
    assert!(!apply(usage.clone())); // a tool result is not the model's final answer
    assert!(!apply(answer.clone()));
    assert!(apply(usage.clone()));
    assert!(!apply(start("c", "run_command"))); // commentary and tool batch can share a model step
    assert!(!apply(end("c", "completed")));
    assert!(!apply(answer));
    assert!(apply(usage));
}

#[cfg(test)]
#[test]
fn unconfirmed_tool_is_not_success_and_previous_turn_is_untouched() {
    let mut thread = crate::threads::Thread::new(".".into(), AgentKind::Antigravity, None, None, None, false);
    let tool = |id, status: &str| -> Item { serde_json::from_value(json!({
        "type":"tool", "id":id, "ts":0, "toolCallId":format!("t{id}"), "title":"cmd",
        "kind":"execute", "status":status, "content":[], "locations":[]
    })).unwrap() };
    thread.items.push(tool(1, "pending"));
    thread.push_user("继续".into(), vec![]);
    thread.items.extend([tool(3, "in_progress"), tool(4, "completed")]);
    let changed = settle_unconfirmed_tools(&mut thread);
    assert!(matches!(&changed[..], [Item::Tool { id: 3, call, .. }] if call.status == "failed"));
    assert!(settle_unconfirmed_tools(&mut thread).is_empty());
}

/// 仅当旧 `session_id` 明确归属于当前活跃账号时才允许 `session/resume` / `session/load`。
/// 切号后或旧历史会话未标记归属账号时一律返回 `false`，避免跨账号探测旧 sessionId，
/// 改为在新账号独立数据目录下新建会话并用 Nova 本地历史续聊。
fn can_reuse_saved_session(
    saved_session_id: Option<&str>,
    saved_account_id: Option<&str>,
    current_account_id: Option<&str>,
) -> bool {
    if saved_session_id.is_none() {
        return false;
    }
    match (saved_account_id, current_account_id) {
        (Some(saved), Some(current)) => saved == current,
        (None, None) => true,
        _ => false,
    }
}

fn allocate_acp_log_file(nova_data_dir: &std::path::Path) -> Option<PathBuf> {
    const MAX_LOG_FILES: usize = 30;
    let dir = nova_data_dir.join("antigravity-logs");
    std::fs::create_dir_all(&dir).ok()?;
    if let Ok(entries) = std::fs::read_dir(&dir) {
        let mut files: Vec<(std::time::SystemTime, PathBuf)> = entries
            .flatten()
            .filter_map(|e| {
                let path = e.path();
                if path.extension().and_then(|s| s.to_str()) != Some("log") {
                    return None;
                }
                let modified = e
                    .metadata()
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .unwrap_or(std::time::UNIX_EPOCH);
                Some((modified, path))
            })
            .collect();
        if files.len() >= MAX_LOG_FILES {
            files.sort_by_key(|(ts, _)| *ts);
            let remove_count = files.len().saturating_sub(MAX_LOG_FILES - 1);
            for (_, old_path) in files.into_iter().take(remove_count) {
                let _ = std::fs::remove_file(old_path);
            }
        }
    }
    let short = uuid::Uuid::new_v4().simple().to_string();
    Some(dir.join(format!("acp-{}-{}.log", now_ms(), &short[..8])))
}

fn detach_stalled_session(thread: &mut crate::threads::Thread, session_id: &str) -> bool {
    if thread.acp_session_id.as_deref() != Some(session_id) {
        return false;
    }
    thread.acp_session_id = None;
    thread.acp_account_id = None;
    true
}

#[cfg(test)]
#[test]
fn stalled_session_is_detached_and_next_prompt_handoff_preserves_history() {
    let mut thread =
        crate::threads::Thread::new(".".into(), AgentKind::Antigravity, None, None, None, false);
    thread.acp_session_id = Some("stalled".into());
    thread.acp_account_id = Some("account".into());
    thread.push_user("原任务".into(), vec![]);
    assert!(!detach_stalled_session(&mut thread, "older"));
    assert_eq!(thread.acp_account_id.as_deref(), Some("account"));
    assert!(detach_stalled_session(&mut thread, "stalled"));
    assert!(!can_reuse_saved_session(
        thread.acp_session_id.as_deref(),
        thread.acp_account_id.as_deref(),
        Some("account")
    ));
    thread.push_user("继续".into(), vec![]);
    let history = history_before_prompt(&thread).unwrap();
    assert!(history.contains("原任务") && !history.contains("用户：\n继续"));
}

#[cfg(test)]
#[test]
fn background_command_tail_has_fallback_when_usage_update_is_omitted() {
    let mut mapper = TurnMapper::new();
    let mut tail = CommandTail::default();
    let start = |id: &str, name: &str| {
        json!({"sessionUpdate":"tool_call", "name":name, "toolCallId":id, "status":"in_progress"})
    };
    let end = |id: &str, status: &str| {
        json!({"sessionUpdate":"tool_call_update", "toolCallId":id, "status":status})
    };
    let answer = json!({"sessionUpdate":"agent_message_chunk", "content":{"type":"text", "text":"done"}});
    let thought = json!({"sessionUpdate":"agent_thought_chunk", "content":{"type":"text", "text":"thinking"}});

    mapper.on_update(&start("b", "run_command"));
    mapper.on_update(&end("b", "completed"));
    tail.observe(&end("b", "completed"), &mapper);
    assert!(tail.settle_at.is_none());
    assert!(tail.next_settle_at().is_some()); // 静默 45s 兜底

    // 开始思考会取消静默兜底
    tail.observe(&thought, &mapper);
    assert!(tail.next_settle_at().is_none());

    // 收到正文但即使没有后续 usage_update，也有 15s 兜底收敛
    tail.observe(&answer, &mapper);
    assert!(tail.settle_at.is_none());
    assert!(tail.next_settle_at().is_some());
}

fn history_before_prompt(thread: &crate::threads::Thread) -> Option<String> {
    // The live prompt is appended separately; system notices can follow it in the store.
    let current = thread.items.iter().rposition(|it| matches!(it, Item::User { .. }))?;
    render_handoff_context(&thread.items[..current], thread.plan.as_ref(), "Antigravity", "Antigravity")
}

#[cfg(test)]
#[test]
fn cross_account_session_never_resumes_old_session_id() {
    assert!(can_reuse_saved_session(Some("s1"), Some("acc-a"), Some("acc-a")));
    assert!(!can_reuse_saved_session(Some("s1"), Some("acc-a"), Some("acc-b")));
    assert!(!can_reuse_saved_session(Some("s1"), None, Some("acc-a")));
    assert!(!can_reuse_saved_session(Some("s1"), Some("acc-a"), None));
    assert!(can_reuse_saved_session(Some("s1"), None, None));
    assert!(!can_reuse_saved_session(None, Some("acc-a"), Some("acc-a")));
}

#[cfg(test)]
#[test]
fn account_handoff_preserves_progress_without_repeating_the_live_prompt() {
    let mut thread = crate::threads::Thread::new(".".into(), AgentKind::Antigravity, None, None, None, false);
    thread.push_user("整理文档".into(), vec![]);
    assert!(history_before_prompt(&thread).is_none());
    thread.items.push(Item::Assistant { id: 2, ts: 0, text: "已完成第一章".into() });
    thread.push_user("现在检查第二章".into(), vec![]);
    thread.push_system("重建连接".into(), "info");
    let history = history_before_prompt(&thread).unwrap();
    assert!(history.contains("已完成第一章"));
    assert!(!history.contains("现在检查第二章"));
    let prompt = build_prompt_blocks(Some(&history), "现在检查第二章", &[], false);
    assert_eq!(prompt[0]["text"].as_str().unwrap().matches("现在检查第二章").count(), 1);
}

fn format_elicitation_title(message: &str) -> String {
    let trimmed = message.trim();
    if trimmed.is_empty() || is_generic_elicitation_message(trimmed) {
        "提问确认".to_string()
    } else {
        trimmed.to_string()
    }
}

fn is_generic_elicitation_message(trimmed: &str) -> bool {
    if trimmed.eq_ignore_ascii_case("The agent has a question.") {
        return true;
    }
    let lower = trimmed.to_ascii_lowercase();
    if let Some(rest) = lower.strip_prefix("the agent has ") {
        if let Some(count) = rest.strip_suffix(" questions.") {
            return !count.is_empty() && count.bytes().all(|b| b.is_ascii_digit());
        }
    }
    false
}

async fn wait_auth_retry_delay(
    delay: Duration,
    cancel_notify: &Notify,
    cancelled: &AtomicBool,
) -> bool {
    tokio::select! {
        biased;
        _ = cancel_notify.notified() => false,
        _ = tokio::time::sleep(delay) => !cancelled.load(Ordering::SeqCst),
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
#[test]
fn pre_expiry_binding_recycle_and_permanent_auth_classification() {
    let now = 1_795_597_000_000i64;
    assert!(!should_recycle_binding_for_token_expiry(None, Some(now + 10_000), now));
    assert!(!should_recycle_binding_for_token_expiry(
        Some(now + TOKEN_RECYCLE_MARGIN_MS + 1),
        None,
        now
    ));
    assert!(should_recycle_binding_for_token_expiry(
        Some(now + TOKEN_RECYCLE_MARGIN_MS),
        None,
        now
    ));
    assert!(should_recycle_binding_for_token_expiry(
        Some(now - 1_000),
        Some(now + 3_600_000),
        now
    ));

    assert!(is_permanent_auth_error("oauth2: invalid_grant (Token has been expired or revoked)"));
    assert!(is_permanent_auth_error("invalid api_key"));
    assert!(is_permanent_auth_error("API key is missing"));
    assert!(is_permanent_auth_error("You are not logged into Antigravity."));
    assert!(is_permanent_auth_error("PERMISSION_DENIED: VALIDATION_REQUIRED"));
    assert!(!is_permanent_auth_error("UNAUTHENTICATED (code 401): Request had invalid authentication credentials."));
    assert!(!is_permanent_auth_error("Authentication required (auth_required)"));
}
