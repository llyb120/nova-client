use crate::acp::{
    EV_LOG, EV_NOTIFY_OPEN, EV_OPTIONS, EV_PERMISSION, EV_PERMISSION_RESOLVED, EV_THREADS, EV_TURN,
    EV_UPDATE,
};
use crate::model_cache;
use crate::nova_data_dir;
use crate::settings::Settings;
use crate::threads::{now_ms, AgentKind, Item, PromptImage, Thread, ToolCall};
use crate::AppState;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Child;
use tokio::sync::{mpsc, oneshot, Mutex as TokioMutex};
use tokio::time::{timeout, Duration};

const LOG_CAP: usize = 800;
const TOOL_OUTPUT_MAX_LINES: usize = 500;
const TOOL_OUTPUT_MAX_CHARS: usize = 10_000;
const TOOL_OUTPUT_OMISSION_PREFIX: &str = "[输出过长，已省略前面内容，仅保留最后";

#[cfg(windows)]
fn path_has_separator(path: &str) -> bool {
    path.contains('\\') || path.contains('/')
}

#[cfg(windows)]
fn find_on_path(name: &str) -> Option<PathBuf> {
    let candidates: Vec<String> = if Path::new(name).extension().is_some() {
        vec![name.to_string()]
    } else {
        ["", ".exe", ".cmd", ".bat"]
            .iter()
            .map(|ext| format!("{name}{ext}"))
            .collect()
    };
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths).find_map(|dir| {
            candidates
                .iter()
                .map(|candidate| dir.join(candidate))
                .find(|path| path.is_file())
        })
    })
}

#[cfg(windows)]
fn codex_npm_shim_script(path: &Path) -> Option<PathBuf> {
    let name = path.file_name()?.to_string_lossy().to_ascii_lowercase();
    if name != "codex" && name != "codex.cmd" && name != "codex.ps1" {
        return None;
    }
    let script = path
        .parent()?
        .join("node_modules")
        .join("@openai")
        .join("codex")
        .join("bin")
        .join("codex.js");
    script.is_file().then_some(script)
}

#[cfg(windows)]
fn codex_npm_package_root(path: &Path) -> Option<PathBuf> {
    let name = path.file_name()?.to_string_lossy().to_ascii_lowercase();
    if name != "codex" && name != "codex.cmd" && name != "codex.ps1" {
        return None;
    }
    let root = path
        .parent()?
        .join("node_modules")
        .join("@openai")
        .join("codex");
    root.join("package.json").is_file().then_some(root)
}

#[cfg(windows)]
fn codex_native_binary_from_npm_root(root: &Path) -> Option<PathBuf> {
    let arch_pkg = if cfg!(target_arch = "aarch64") {
        "codex-win32-arm64"
    } else {
        "codex-win32-x64"
    };
    let target = if cfg!(target_arch = "aarch64") {
        "aarch64-pc-windows-msvc"
    } else {
        "x86_64-pc-windows-msvc"
    };
    [
        root.join("node_modules")
            .join("@openai")
            .join(arch_pkg)
            .join("vendor")
            .join(target)
            .join("bin")
            .join("codex.exe"),
        root.join("vendor")
            .join(target)
            .join("bin")
            .join("codex.exe"),
    ]
    .into_iter()
    .find(|path| path.is_file())
}

#[cfg(windows)]
fn resolve_codex_native_binary(codex_path: &str) -> Option<PathBuf> {
    let shim = if path_has_separator(codex_path) {
        PathBuf::from(codex_path)
    } else {
        find_on_path(codex_path)?
    };
    if shim
        .file_name()
        .map(|name| name.to_string_lossy().eq_ignore_ascii_case("codex.exe"))
        .unwrap_or(false)
    {
        return shim.is_file().then_some(shim);
    }
    codex_npm_package_root(&shim).and_then(|root| codex_native_binary_from_npm_root(&root))
}

#[cfg(windows)]
fn resolve_codex_npm_shim(codex_path: &str) -> Option<(PathBuf, PathBuf)> {
    let shim = if path_has_separator(codex_path) {
        PathBuf::from(codex_path)
    } else {
        find_on_path(codex_path)?
    };
    let script = codex_npm_shim_script(&shim)?;
    let node = shim
        .parent()
        .map(|dir| dir.join("node.exe"))
        .filter(|path| path.is_file())
        .or_else(|| find_on_path("node.exe"))
        .unwrap_or_else(|| PathBuf::from("node"));
    Some((node, script))
}

struct PendingCodexPermission {
    rpc_id: Value,
    method: String,
}

struct CodexTurnOutcome {
    stop_reason: String,
    usage: Option<Value>,
    /// 轮次失败时的错误文案（来自 turn.error 或 error 通知），用于展示给用户
    error: Option<String>,
}

pub struct CodexConn {
    stdin_tx: mpsc::UnboundedSender<String>,
    pending: StdMutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>,
    next_id: AtomicU64,
    pub alive: AtomicBool,
    child: StdMutex<Option<Child>>,
}

impl CodexConn {
    fn send_raw(&self, msg: Value) -> Result<(), String> {
        self.stdin_tx
            .send(msg.to_string())
            .map_err(|_| "codex app-server 不可写（已退出？）".to_string())
    }

    pub async fn request(
        &self,
        method: &str,
        params: Value,
        wait: Option<Duration>,
    ) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        self.send_raw(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        }))?;
        let recv = async {
            rx.await
                .map_err(|_| "codex app-server 连接已断开".to_string())
                .and_then(|r| r)
        };
        match wait {
            Some(d) => timeout(d, recv)
                .await
                .map_err(|_| format!("{method} 等待超时"))?,
            None => recv.await,
        }
    }

    pub fn respond_ok(&self, id: Value, result: Value) {
        let _ = self.send_raw(json!({ "jsonrpc": "2.0", "id": id, "result": result }));
    }

    pub fn respond_err(&self, id: Value, code: i64, message: String) {
        let _ = self.send_raw(json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": code, "message": message }
        }));
    }

    pub fn kill(&self) {
        self.alive.store(false, Ordering::SeqCst);
        if let Some(mut child) = self.child.lock().unwrap().take() {
            // 同 acp：杀整棵进程树，避免经垫片拉起的 node 及其 spawn 的 shell 变孤儿。
            if let Some(pid) = child.id() {
                crate::acp::kill_process_tree(pid);
            }
            let _ = child.start_kill();
        }
    }
}

/// codex app-server 空闲整树回收的超时：app-server 会为每个 thread 拉起一套 MCP 服务器
/// 且用完不退，长时间驻留只增不减；连接空闲达到该时长即整树回收释放内存。
/// 下次发消息自动重启进程并经 thread/resume 恢复上下文，仅首条消息慢几秒。
const IDLE_KILL_CODEX_MS: u128 = 10 * 60 * 1000;
/// 会话数回收阈值：当前连接累计创建/恢复过这么多 thread（≈ 驻留的 MCP 服务器套数）后，
/// 只要出现一段短暂安静期（CODEX_QUIET_MS）就整树回收，防止连续使用下内存无限堆积。
const CODEX_THREADS_RECYCLE_AT: u64 = 6;
const CODEX_QUIET_MS: u128 = 2 * 60 * 1000;

pub struct CodexManager {
    pub app: AppHandle,
    /// 额度租借实例使用的独立凭证环境；普通全局实例为空。
    launch_env: HashMap<String, String>,
    permission_scope: String,
    conn: TokioMutex<Option<Arc<CodexConn>>>,
    routes: StdMutex<HashMap<String, String>>,
    running_threads: StdMutex<HashSet<String>>,
    turn_started: StdMutex<HashMap<String, std::time::Instant>>,
    active_turns: StdMutex<HashMap<String, String>>,
    turn_waiters: StdMutex<HashMap<String, oneshot::Sender<Result<CodexTurnOutcome, String>>>>,
    turn_usage: StdMutex<HashMap<String, Value>>,
    /// 轮次进行中收到的 error 通知文案（turnId -> 最近一条），轮次失败时作为兜底展示
    turn_errors: StdMutex<HashMap<String, String>>,
    turn_reasoning_items: StdMutex<HashMap<String, String>>,
    pending_permissions: StdMutex<HashMap<String, PendingCodexPermission>>,
    prewarmed: StdMutex<HashMap<String, String>>,
    prewarming: StdMutex<HashSet<String>>,
    thread_locks: StdMutex<HashMap<String, Arc<TokioMutex<()>>>>,
    item_ids: StdMutex<HashMap<String, u64>>,
    tool_outputs: StdMutex<HashMap<String, String>>,
    /// 正在进行手动压缩（thread/compact/start）的会话；压缩完成/超时后据此解除忙碌态
    manual_compacting: StdMutex<HashSet<String>>,
    model_options: StdMutex<Option<Value>>,
    /// 本进程内是否已对磁盘缓存做过一次后台重拉
    model_options_revalidated: AtomicBool,
    model_options_refreshing: AtomicBool,
    logs: StdMutex<VecDeque<String>>,
    pub agent_info: StdMutex<Option<Value>>,
    /// 连接最近一次活跃（建连/复用/轮次结束）时刻，供空闲回收器判定
    conn_last_active: StdMutex<Option<std::time::Instant>>,
    /// 当前连接累计创建/恢复过的 thread 数（≈ app-server 里驻留的 MCP 服务器套数）
    threads_spawned: StdMutex<u64>,
}

impl CodexManager {
    pub fn new(app: AppHandle) -> Arc<Self> {
        Self::new_with_env(app, HashMap::new(), String::new())
    }

    pub fn new_with_env(
        app: AppHandle,
        launch_env: HashMap<String, String>,
        permission_scope: String,
    ) -> Arc<Self> {
        let mgr = Arc::new(CodexManager {
            app,
            launch_env,
            permission_scope,
            conn: TokioMutex::new(None),
            routes: StdMutex::new(HashMap::new()),
            running_threads: StdMutex::new(HashSet::new()),
            turn_started: StdMutex::new(HashMap::new()),
            active_turns: StdMutex::new(HashMap::new()),
            turn_waiters: StdMutex::new(HashMap::new()),
            turn_usage: StdMutex::new(HashMap::new()),
            turn_errors: StdMutex::new(HashMap::new()),
            turn_reasoning_items: StdMutex::new(HashMap::new()),
            pending_permissions: StdMutex::new(HashMap::new()),
            prewarmed: StdMutex::new(HashMap::new()),
            prewarming: StdMutex::new(HashSet::new()),
            thread_locks: StdMutex::new(HashMap::new()),
            item_ids: StdMutex::new(HashMap::new()),
            tool_outputs: StdMutex::new(HashMap::new()),
            manual_compacting: StdMutex::new(HashSet::new()),
            model_options: StdMutex::new(None),
            model_options_revalidated: AtomicBool::new(false),
            model_options_refreshing: AtomicBool::new(false),
            logs: StdMutex::new(VecDeque::new()),
            agent_info: StdMutex::new(None),
            conn_last_active: StdMutex::new(None),
            threads_spawned: StdMutex::new(0),
        });
        // 空闲回收器：app-server 为每个 thread 常驻一套 MCP 服务器进程（用完不退、只增不减），
        // 空闲时整树回收释放内存；下次发消息自动重连并经 thread/resume 恢复。
        {
            let weak = Arc::downgrade(&mgr);
            tauri::async_runtime::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    let Some(m) = weak.upgrade() else { break };
                    m.reap_if_idle().await;
                }
            });
        }
        mgr
    }

    /// 刷新连接「最近活跃」时刻（供空闲回收器判定）
    fn touch_conn(&self) {
        *self.conn_last_active.lock().unwrap() = Some(std::time::Instant::now());
    }

    /// 空闲整树回收：无运行中轮次/压缩/预热/未决审批，且（空闲超时 或 会话数达阈值后出现
    /// 短暂安静期）时，杀掉 app-server 进程树释放其驻留的全部 MCP 服务器。
    async fn reap_if_idle(self: &Arc<Self>) {
        if !self.running_threads.lock().unwrap().is_empty()
            || !self.prewarming.lock().unwrap().is_empty()
            || !self.manual_compacting.lock().unwrap().is_empty()
            || !self.pending_permissions.lock().unwrap().is_empty()
            || !self.turn_waiters.lock().unwrap().is_empty()
        {
            return;
        }
        if !self.connected().await {
            return;
        }
        let idle_ms = self
            .conn_last_active
            .lock()
            .unwrap()
            .map(|t| t.elapsed().as_millis())
            .unwrap_or(u128::MAX);
        let threads = *self.threads_spawned.lock().unwrap();
        let due = idle_ms >= IDLE_KILL_CODEX_MS
            || (threads >= CODEX_THREADS_RECYCLE_AT && idle_ms >= CODEX_QUIET_MS);
        if !due {
            return;
        }
        // kill 前复查一次运行状态，收窄「检查到动手」之间的竞态窗口
        if !self.running_threads.lock().unwrap().is_empty() {
            return;
        }
        self.push_log(format!(
            "连接空闲 {} 分钟（累计 {threads} 个会话），整树回收 app-server 释放内存；下次发消息自动重连恢复",
            idle_ms / 60000
        ));
        self.kill_conn().await;
    }

    pub fn is_running(&self, thread_id: &str) -> bool {
        self.running_threads.lock().unwrap().contains(thread_id)
    }

    /// 模式变化后调用：卸载该线程的远端挂载（未运行时），下次发消息经 thread/resume
    /// 重新应用 approvalPolicy/sandbox。运行中不动，避免当前轮次事件断流。
    pub fn remount_for_config(&self, thread_id: &str) {
        if self.is_running(thread_id) {
            return;
        }
        self.routes.lock().unwrap().retain(|_, v| v != thread_id);
    }

    pub fn get_model_options(&self) -> Option<Value> {
        self.model_options.lock().unwrap().clone()
    }

    pub fn seed_model_options(&self, v: Value) {
        *self.model_options.lock().unwrap() = Some(v);
    }

    fn persist_model_options(&self, v: &Value) {
        model_cache::save(&nova_data_dir(&self.app), "codex", v);
    }

    pub async fn refresh_model_options(self: &Arc<Self>) -> Result<Value, String> {
        // 不先清内存：旧缓存继续服务前端，拉到新列表后再覆盖。
        match self.fetch_model_options_from_agent().await {
            Ok(v) => {
                self.model_options_revalidated.store(true, Ordering::SeqCst);
                Ok(v)
            }
            Err(e) => Err(e),
        }
    }

    pub fn spawn_revalidate_model_options(self: &Arc<Self>) {
        if self.model_options_revalidated.load(Ordering::SeqCst) {
            return;
        }
        if self
            .model_options_refreshing
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }
        let mgr = Arc::clone(self);
        tauri::async_runtime::spawn(async move {
            let _ = mgr.refresh_model_options().await;
            mgr.model_options_refreshing.store(false, Ordering::SeqCst);
        });
    }

    pub async fn ensure_model_options(self: &Arc<Self>) -> Result<Value, String> {
        if let Some(v) = self.get_model_options() {
            self.spawn_revalidate_model_options();
            return Ok(v);
        }
        self.spawn_revalidate_model_options();
        for _ in 0..600 {
            if let Some(v) = self.get_model_options() {
                return Ok(v);
            }
            if !self.model_options_refreshing.load(Ordering::SeqCst) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        if let Some(v) = self.get_model_options() {
            return Ok(v);
        }
        self.fetch_model_options().await
    }

    pub fn get_logs(&self) -> Vec<String> {
        self.logs.lock().unwrap().iter().cloned().collect()
    }

    pub async fn connected(&self) -> bool {
        self.conn
            .lock()
            .await
            .as_ref()
            .map(|c| c.alive.load(Ordering::SeqCst))
            .unwrap_or(false)
    }

    fn push_log(&self, line: String) {
        let line = format!("[codex] {line}");
        {
            let mut logs = self.logs.lock().unwrap();
            if logs.len() >= LOG_CAP {
                logs.pop_front();
            }
            logs.push_back(line.clone());
        }
        let _ = self.app.emit(EV_LOG, line);
    }

    pub async fn kill_conn(&self) {
        let mut guard = self.conn.lock().await;
        if let Some(conn) = guard.take() {
            conn.kill();
        }
        *self.threads_spawned.lock().unwrap() = 0;
        self.routes.lock().unwrap().clear();
        self.active_turns.lock().unwrap().clear();
        self.prewarmed.lock().unwrap().clear();
        self.prewarming.lock().unwrap().clear();
        self.pending_permissions.lock().unwrap().clear();
        self.item_ids.lock().unwrap().clear();
        self.tool_outputs.lock().unwrap().clear();
        self.turn_errors.lock().unwrap().clear();
        self.turn_reasoning_items.lock().unwrap().clear();
        self.manual_compacting.lock().unwrap().clear();
        let waiters: Vec<_> = self.turn_waiters.lock().unwrap().drain().collect();
        for (_, tx) in waiters {
            let _ = tx.send(Err("codex app-server 已退出".into()));
        }
    }

    /// 手动重启：杀掉 app-server 连接，并立即结束所有运行中的轮次，
    /// 让卡死的任务在界面上马上停下。下次发消息时自动重连。
    pub async fn restart(self: &Arc<Self>) {
        let running: Vec<String> = self
            .running_threads
            .lock()
            .unwrap()
            .iter()
            .cloned()
            .collect();
        self.kill_conn().await;
        for tid in running {
            if self.is_running(&tid) {
                self.force_finish(&tid, "已重启 Codex 进程，本轮已结束；下次发送会自动重连。")
                    .await;
            }
        }
    }

    fn thread_lock(&self, thread_id: &str) -> Arc<TokioMutex<()>> {
        self.thread_locks
            .lock()
            .unwrap()
            .entry(thread_id.to_string())
            .or_insert_with(|| Arc::new(TokioMutex::new(())))
            .clone()
    }

    pub async fn ensure_conn(self: &Arc<Self>) -> Result<Arc<CodexConn>, String> {
        let mut guard = self.conn.lock().await;
        if let Some(c) = guard.as_ref() {
            if c.alive.load(Ordering::SeqCst) {
                self.touch_conn();
                return Ok(c.clone());
            }
        }
        let settings = {
            let state = self.app.state::<AppState>();
            let s = state.settings.lock().unwrap().clone();
            s
        };
        let conn = self.spawn_conn(&settings).await?;
        *self.threads_spawned.lock().unwrap() = 0;
        self.touch_conn();
        *guard = Some(conn.clone());
        Ok(conn)
    }

    async fn current_conn(&self) -> Option<Arc<CodexConn>> {
        let guard = self.conn.lock().await;
        guard
            .as_ref()
            .filter(|c| c.alive.load(Ordering::SeqCst))
            .cloned()
    }

    async fn spawn_conn(self: &Arc<Self>, settings: &Settings) -> Result<Arc<CodexConn>, String> {
        // 把 ~/.nova/skills 用软链接/目录联接同步到各后端全局 skills 目录
        crate::skills::sync_skills_from_home();

        let mut args: Vec<String> = settings
            .codex_args
            .split_whitespace()
            .map(str::to_string)
            .collect();
        #[cfg(windows)]
        apply_windows_sandbox_fallback(&mut args);
        let pxpipe_url =
            crate::pxpipe::prepare_codex_pxpipe(&mut args, settings, &settings.codex_proxy)?;
        #[cfg(windows)]
        let mut cmd = {
            if let Some(binary) = resolve_codex_native_binary(&settings.codex_path) {
                let mut cmd = tokio::process::Command::new(binary);
                cmd.args(&args);
                cmd
            } else if let Some((node, script)) = resolve_codex_npm_shim(&settings.codex_path) {
                let mut cmd = tokio::process::Command::new(node);
                cmd.arg(script).args(&args);
                cmd
            } else if settings.codex_path.to_ascii_lowercase().ends_with(".exe") {
                let mut cmd = tokio::process::Command::new(&settings.codex_path);
                cmd.args(&args);
                cmd
            } else {
                let mut cmd = tokio::process::Command::new("cmd");
                cmd.arg("/D")
                    .arg("/S")
                    .arg("/C")
                    .arg(&settings.codex_path)
                    .args(&args);
                cmd
            }
        };
        #[cfg(not(windows))]
        let mut cmd = {
            let mut cmd = tokio::process::Command::new(&settings.codex_path);
            cmd.args(&args);
            cmd
        };
        cmd.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        crate::acp::apply_proxy_env(&mut cmd, &settings.codex_proxy);
        cmd.envs(&self.launch_env);
        crate::pxpipe::apply_codex_pxpipe_env(&mut cmd, pxpipe_url.as_deref());
        // Windows 下继承 Nova 在启动时建立的隐藏控制台。这里不能使用 CREATE_NO_WINDOW，
        // 否则 Codex 本身没有控制台，其后续启动 PowerShell 时会重新创建可见 conhost。
        #[cfg(unix)]
        {
            cmd.process_group(0);
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("无法启动 codex（{}）：{e}", settings.codex_path))?;
        // 兜底：挂进 KILL_ON_JOB_CLOSE 的 Job，Nova 无论如何退出都不会残留 codex 孤儿进程
        crate::acp::assign_to_agent_job(&child);

        let stdin = child.stdin.take().ok_or("无法获取 codex stdin")?;
        let stdout = child.stdout.take().ok_or("无法获取 codex stdout")?;
        let stderr = child.stderr.take().ok_or("无法获取 codex stderr")?;

        let (stdin_tx, mut stdin_rx) = mpsc::unbounded_channel::<String>();
        let conn = Arc::new(CodexConn {
            stdin_tx,
            pending: StdMutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            alive: AtomicBool::new(true),
            child: StdMutex::new(Some(child)),
        });

        tokio::spawn(async move {
            let mut stdin = stdin;
            while let Some(line) = stdin_rx.recv().await {
                if stdin.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
                if stdin.write_all(b"\n").await.is_err() {
                    break;
                }
                let _ = stdin.flush().await;
            }
        });

        {
            let mgr = self.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    mgr.push_log(line);
                }
            });
        }

        {
            let mgr = self.clone();
            let conn2 = conn.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stdout).lines();
                loop {
                    match lines.next_line().await {
                        Ok(Some(line)) => {
                            let line = line.trim().to_string();
                            if !line.is_empty() {
                                mgr.handle_line(&conn2, &line);
                            }
                        }
                        _ => break,
                    }
                }
                mgr.on_conn_closed(&conn2);
            });
        }

        let init = conn
            .request(
                "initialize",
                json!({
                    "clientInfo": {
                        "name": "nova",
                        "title": "Nova",
                        "version": env!("CARGO_PKG_VERSION")
                    },
                    // experimentalApi：启用实验 API 字段（turn/start.collaborationMode
                    // 承载原生 Plan 模式，即 TUI 里的 /plan）
                    "capabilities": { "experimentalApi": true }
                }),
                Some(Duration::from_secs(60)),
            )
            .await;

        match init {
            Ok(result) => {
                *self.agent_info.lock().unwrap() = Some(result.clone());
                self.push_log(format!(
                    "app-server 已连接 {}",
                    result["userAgent"].as_str().unwrap_or("codex")
                ));
                Ok(conn)
            }
            Err(e) => {
                conn.kill();
                Err(format!("codex app-server 初始化失败：{e}"))
            }
        }
    }

    fn on_conn_closed(&self, conn: &Arc<CodexConn>) {
        conn.alive.store(false, Ordering::SeqCst);
        let pending: Vec<_> = {
            let mut map = conn.pending.lock().unwrap();
            map.drain().collect()
        };
        for (_, tx) in pending {
            let _ = tx.send(Err("codex app-server 已退出".into()));
        }
        let keys: Vec<String> = {
            let mut perms = self.pending_permissions.lock().unwrap();
            let keys = perms.keys().cloned().collect();
            perms.clear();
            keys
        };
        for key in keys {
            let _ = self
                .app
                .emit(EV_PERMISSION_RESOLVED, json!({ "requestKey": key }));
        }
        let waiters: Vec<_> = self.turn_waiters.lock().unwrap().drain().collect();
        for (_, tx) in waiters {
            let _ = tx.send(Err("codex app-server 已退出".into()));
        }
        self.routes.lock().unwrap().clear();
        self.active_turns.lock().unwrap().clear();
        self.prewarmed.lock().unwrap().clear();
        self.prewarming.lock().unwrap().clear();
        self.item_ids.lock().unwrap().clear();
        self.tool_outputs.lock().unwrap().clear();
        self.turn_errors.lock().unwrap().clear();
        self.turn_reasoning_items.lock().unwrap().clear();
        self.manual_compacting.lock().unwrap().clear();
        *self.threads_spawned.lock().unwrap() = 0;
        *self.agent_info.lock().unwrap() = None;
        self.push_log("app-server 进程已退出".into());
    }

    fn handle_line(self: &Arc<Self>, conn: &Arc<CodexConn>, line: &str) {
        let Ok(msg) = serde_json::from_str::<Value>(line) else {
            self.push_log(format!("无法解析 stdout 行: {line}"));
            return;
        };
        let has_method = msg.get("method").is_some();
        let has_id = msg.get("id").is_some();

        if has_method && has_id {
            self.handle_server_request(conn, &msg);
        } else if has_method {
            self.handle_notification(&msg);
        } else if has_id {
            let Some(id) = msg["id"].as_u64() else { return };
            let tx = conn.pending.lock().unwrap().remove(&id);
            if let Some(tx) = tx {
                if let Some(err) = msg.get("error") {
                    let text = err["message"].as_str().unwrap_or("未知错误").to_string();
                    let _ = tx.send(Err(text));
                } else {
                    let _ = tx.send(Ok(msg["result"].clone()));
                }
            }
        }
    }

    fn handle_server_request(self: &Arc<Self>, conn: &Arc<CodexConn>, msg: &Value) {
        let method = msg["method"].as_str().unwrap_or_default().to_string();
        let id = msg["id"].clone();
        let params = msg["params"].clone();

        match method.as_str() {
            "item/commandExecution/requestApproval"
            | "item/fileChange/requestApproval"
            | "execCommandApproval"
            | "applyPatchApproval" => self.emit_permission_request(conn, id, method, &params),
            _ => conn.respond_err(id, -32601, format!("客户端不支持方法 {method}")),
        }
    }

    fn emit_permission_request(
        self: &Arc<Self>,
        conn: &Arc<CodexConn>,
        id: Value,
        method: String,
        params: &Value,
    ) {
        let remote_thread_id = params["threadId"]
            .as_str()
            .or_else(|| params["conversationId"].as_str())
            .unwrap_or_default();
        let thread_id = self.routes.lock().unwrap().get(remote_thread_id).cloned();
        let Some(thread_id) = thread_id else {
            conn.respond_err(id, -32603, "无法定位 Codex 会话".into());
            return;
        };
        // 数字员工是无人值守的：授权请求没人应答会让本轮永久挂起（is_running 卡死、员工「永远在忙」、
        // 无法再次「立即执行」）。对员工会话直接自动放行（等价 bypass），让自主编排顺畅跑完并正常收尾。
        let is_employee = {
            let state = self.app.state::<AppState>();
            let store = state.store.lock().unwrap();
            store
                .get(&thread_id)
                .and_then(|t| t.employee_id.clone())
                .is_some()
        };
        if is_employee {
            let decision = if method == "execCommandApproval" || method == "applyPatchApproval" {
                "approved"
            } else {
                "accept"
            };
            conn.respond_ok(id, json!({ "decision": decision }));
            return;
        }
        let key = format!("{}codex-perm-{}", self.permission_scope, id);
        self.pending_permissions.lock().unwrap().insert(
            key.clone(),
            PendingCodexPermission {
                rpc_id: id,
                method: method.clone(),
            },
        );

        let command = params["command"].as_str().unwrap_or_default();
        let reason = params["reason"].as_str().unwrap_or_default();
        let grant_root = params["grantRoot"].as_str().unwrap_or_default();
        let (title, kind, raw) = if method.contains("fileChange") || method == "applyPatchApproval"
        {
            (
                "修改文件".to_string(),
                "edit".to_string(),
                json!({ "reason": reason, "grantRoot": grant_root, "raw": params }),
            )
        } else {
            (
                if command.is_empty() {
                    "执行命令".to_string()
                } else {
                    command.to_string()
                },
                "execute".to_string(),
                json!({ "command": command, "cwd": params["cwd"], "reason": reason, "raw": params }),
            )
        };
        let _ = self.app.emit(
            EV_PERMISSION,
            json!({
                "threadId": thread_id,
                "agentKind": "codex",
                "requestKey": key,
                "toolCall": {
                    "title": title,
                    "kind": kind,
                    "rawInput": raw
                },
                "options": [
                    { "optionId": "accept", "name": "允许", "kind": "allow" },
                    { "optionId": "acceptForSession", "name": "本会话允许", "kind": "allow" },
                    { "optionId": "decline", "name": "拒绝", "kind": "reject" }
                ]
            }),
        );
    }

    fn handle_notification(&self, msg: &Value) {
        let method = msg["method"].as_str().unwrap_or_default();
        let params = &msg["params"];
        match method {
            "turn/started" => self.on_turn_started(params),
            "turn/completed" => self.on_turn_completed(params),
            "thread/tokenUsage/updated" => self.on_token_usage(params),
            "item/started" => self.on_item(params, false),
            "item/completed" => self.on_item(params, true),
            "item/agentMessage/delta" => self.on_text_delta(params, false),
            "item/plan/delta" => self.on_text_delta(params, false),
            "item/reasoning/delta"
            | "item/reasoning/summaryDelta"
            | "item/reasoning/summaryTextDelta"
            | "item/reasoning/textDelta" => self.on_text_delta(params, true),
            "item/commandExecution/outputDelta" | "command/exec/outputDelta" => {
                self.on_tool_output_delta(params)
            }
            "item/fileChange/patchUpdated" => self.on_patch_updated(params),
            "turn/plan/updated" => self.on_plan_updated(params),
            "error" => self.on_error(params),
            "warning" => self.push_log(params.to_string()),
            _ => {}
        }
    }

    fn local_thread_id(&self, remote_thread_id: &str) -> Option<String> {
        self.routes.lock().unwrap().get(remote_thread_id).cloned()
    }

    fn on_turn_started(&self, params: &Value) {
        let remote_thread_id = params["threadId"].as_str().unwrap_or_default();
        let Some(thread_id) = self.local_thread_id(remote_thread_id) else {
            return;
        };
        if let Some(turn_id) = params["turn"]["id"].as_str() {
            self.active_turns
                .lock()
                .unwrap()
                .insert(thread_id.clone(), turn_id.to_string());
            self.set_turn_waiting_item(&thread_id);
        }
    }

    fn on_turn_completed(&self, params: &Value) {
        let remote_thread_id = params["threadId"].as_str().unwrap_or_default();
        let Some(thread_id) = self.local_thread_id(remote_thread_id) else {
            return;
        };
        let turn = &params["turn"];
        let turn_id = turn["id"].as_str().unwrap_or_default().to_string();
        self.active_turns.lock().unwrap().remove(&thread_id);
        // 手动压缩（thread/compact/start）会以一个“伪轮次”形式结束：
        // 它没有注册 turn_waiter，这里仅清理并解除忙碌态，不写入轮次统计。
        if self.manual_compacting.lock().unwrap().contains(&thread_id) {
            self.turn_usage.lock().unwrap().remove(&turn_id);
            self.remove_turn_reasoning_item(&thread_id, &turn_id);
            self.turn_errors.lock().unwrap().remove(&turn_id);
            self.finish_manual_compaction(&thread_id);
            return;
        }
        let status = turn["status"].as_str().unwrap_or("completed");
        let stop_reason = match status {
            "completed" => "end_turn",
            "interrupted" => "cancelled",
            "failed" => "error",
            other => other,
        }
        .to_string();
        let usage = self.turn_usage.lock().unwrap().remove(&turn_id);
        self.remove_turn_reasoning_item(&thread_id, &turn_id);
        let captured = self.turn_errors.lock().unwrap().remove(&turn_id);
        // 失败轮次：优先取 turn.error，其次取进行中捕获的 error 通知，最后兜底文案
        let error = if status == "failed" {
            Some(
                format_codex_error(&turn["error"])
                    .or(captured)
                    .unwrap_or_else(|| "Codex 任务执行失败".to_string()),
            )
        } else {
            None
        };
        if let Some(tx) = self.turn_waiters.lock().unwrap().remove(&turn_id) {
            let _ = tx.send(Ok(CodexTurnOutcome {
                stop_reason,
                usage,
                error,
            }));
        }
    }

    /// codex 的 error 通知：始终落日志；记录到当前轮次，轮次失败时展示给用户。
    /// willRetry 的瞬时错误（如重连）不单独打扰用户，仅在最终失败时呈现。
    fn on_error(&self, params: &Value) {
        self.push_log(params.to_string());
        let Some(turn_id) = params["turnId"].as_str() else {
            return;
        };
        if let Some(msg) = format_codex_error(&params["error"]) {
            self.turn_errors
                .lock()
                .unwrap()
                .insert(turn_id.to_string(), msg);
        }
    }

    fn on_token_usage(&self, params: &Value) {
        let Some(turn_id) = params["turnId"].as_str() else {
            return;
        };
        if let Some(last) = params["tokenUsage"].get("last") {
            self.turn_usage
                .lock()
                .unwrap()
                .insert(turn_id.to_string(), last.clone());
        }
    }

    fn on_item(&self, params: &Value, completed: bool) {
        let remote_thread_id = params["threadId"].as_str().unwrap_or_default();
        let Some(thread_id) = self.local_thread_id(remote_thread_id) else {
            return;
        };
        self.upsert_codex_item(&thread_id, &params["item"], true, completed);
    }

    fn on_text_delta(&self, params: &Value, thought: bool) {
        let remote_thread_id = params["threadId"]
            .as_str()
            .or_else(|| params["conversationId"].as_str())
            .unwrap_or_default();
        let item_id = params["itemId"]
            .as_str()
            .or_else(|| params["item_id"].as_str())
            .unwrap_or_default();
        let delta = text_delta(params);
        if delta.is_empty() {
            return;
        }
        let Some(thread_id) = self.local_thread_id(remote_thread_id) else {
            return;
        };
        if thought {
            self.set_thinking_item(&thread_id, item_id);
            return;
        }
        self.remove_current_turn_reasoning_item(&thread_id);
        self.append_text_item(&thread_id, item_id, delta, thought);
    }

    fn on_tool_output_delta(&self, params: &Value) {
        let remote_thread_id = params["threadId"].as_str().unwrap_or_default();
        let item_id = params["itemId"].as_str().unwrap_or_default();
        let delta = params["delta"].as_str().unwrap_or_default().to_string();
        if delta.is_empty() {
            return;
        }
        let Some(thread_id) = self.local_thread_id(remote_thread_id) else {
            return;
        };
        self.append_tool_output(&thread_id, item_id, &delta);
    }

    fn on_patch_updated(&self, params: &Value) {
        let remote_thread_id = params["threadId"].as_str().unwrap_or_default();
        let item_id = params["itemId"].as_str().unwrap_or_default();
        let Some(thread_id) = self.local_thread_id(remote_thread_id) else {
            return;
        };
        let item = json!({
            "type": "fileChange",
            "id": item_id,
            "changes": params["changes"].clone(),
            "status": "inProgress"
        });
        self.upsert_codex_item(&thread_id, &item, true, true);
    }

    fn on_plan_updated(&self, params: &Value) {
        let remote_thread_id = params["threadId"].as_str().unwrap_or_default();
        let Some(thread_id) = self.local_thread_id(remote_thread_id) else {
            return;
        };
        let plan: Vec<Value> = params["plan"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|step| {
                json!({
                    "content": step["step"].as_str().unwrap_or_default(),
                    "status": normalize_plan_status(step["status"].as_str().unwrap_or("pending"))
                })
            })
            .collect();
        {
            let state = self.app.state::<AppState>();
            let mut store = state.store.lock().unwrap();
            if let Some(thread) = store.get_mut(&thread_id) {
                thread.plan = Some(json!(plan));
                thread.updated_at = now_ms();
            }
            store.save();
        }
        self.emit_update(&thread_id, json!({ "t": "plan", "plan": plan }));
    }

    fn emit_update(&self, thread_id: &str, op: Value) {
        // 只给前台正在查看的会话推流：后台会话的高频增量若也广播到 WebView，会被前端
        // 按 threadId 立刻丢弃，却仍已跨 IPC 全量反序列化，多会话并发时把 WebView2 渲染
        // 进程内存堆爆。增量已落库，切回时经 get_thread 快照补齐。详见 acp.rs::emit_update。
        // mode / proposed_plan / plan 低频关键状态始终推送，避免 Plan 收尾按钮丢失。
        let always = op
            .get("t")
            .and_then(|v| v.as_str())
            .is_some_and(|t| matches!(t, "mode" | "proposed_plan" | "plan"));
        if !always {
            let state = self.app.state::<AppState>();
            let active = state.active_thread.lock().unwrap();
            if active.as_deref() != Some(thread_id) {
                return;
            }
        }
        let _ = self
            .app
            .emit(EV_UPDATE, json!({ "threadId": thread_id, "op": op }));
    }

    fn append_text_item(&self, thread_id: &str, remote_item_id: &str, text: String, thought: bool) {
        let state = self.app.state::<AppState>();
        let mut store = state.store.lock().unwrap();
        let Some(thread) = store.get_mut(thread_id) else {
            return;
        };
        let local_id = self.item_ids.lock().unwrap().get(remote_item_id).cloned();
        if let Some(local_id) = local_id {
            for item in thread.items.iter_mut().rev() {
                match item {
                    Item::Assistant { id, text: t, .. } if *id == local_id && !thought => {
                        t.push_str(&text);
                        thread.updated_at = now_ms();
                        self.emit_update(
                            thread_id,
                            json!({ "t": "delta", "itemId": local_id, "text": text }),
                        );
                        return;
                    }
                    Item::Thought { id, text: t, .. } if *id == local_id && thought => {
                        t.push_str(&text);
                        thread.updated_at = now_ms();
                        self.emit_update(
                            thread_id,
                            json!({ "t": "delta", "itemId": local_id, "text": text }),
                        );
                        return;
                    }
                    _ => {}
                }
            }
        }

        for item in complete_pending_tools(thread, None) {
            self.emit_update(thread_id, json!({ "t": "upsert", "item": item }));
        }
        let id = thread.next_item_id();
        let item = if thought {
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
        };
        self.item_ids
            .lock()
            .unwrap()
            .insert(remote_item_id.to_string(), id);
        thread.items.push(item.clone());
        thread.updated_at = now_ms();
        self.emit_update(thread_id, json!({ "t": "upsert", "item": item }));
    }

    fn append_tool_output(&self, thread_id: &str, remote_item_id: &str, delta: &str) {
        let mut outputs = self.tool_outputs.lock().unwrap();
        let output = outputs.entry(remote_item_id.to_string()).or_default();
        output.push_str(delta);
        *output = limit_display_text(output);
        let text = output.clone();
        drop(outputs);

        let state = self.app.state::<AppState>();
        let mut store = state.store.lock().unwrap();
        let Some(thread) = store.get_mut(thread_id) else {
            return;
        };
        let Some(local_id) = self.item_ids.lock().unwrap().get(remote_item_id).cloned() else {
            return;
        };
        for item in thread.items.iter_mut().rev() {
            if let Item::Tool { id, call, .. } = item {
                if *id != local_id {
                    continue;
                }
                call.content = vec![json!({
                    "type": "content",
                    "content": { "type": "text", "text": text }
                })];
                let snapshot = item.clone();
                self.emit_update(thread_id, json!({ "t": "upsert", "item": snapshot }));
                return;
            }
        }
    }

    fn upsert_codex_item(
        &self,
        thread_id: &str,
        remote_item: &Value,
        save_on_complete: bool,
        completed: bool,
    ) {
        let item_type = remote_item["type"].as_str().unwrap_or_default();
        if item_type == "userMessage" || item_type == "hookPrompt" {
            return;
        }
        let remote_id = remote_item["id"].as_str().unwrap_or_default();
        if remote_id.is_empty() {
            return;
        }

        match item_type {
            "agentMessage" => {
                let text = remote_item["text"].as_str().unwrap_or_default().to_string();
                if !text.is_empty() {
                    self.remove_current_turn_reasoning_item(thread_id);
                    self.set_text_item(thread_id, remote_id, text, false);
                }
            }
            "reasoning" => {
                if completed {
                    self.remove_text_item(thread_id, remote_id, true);
                } else {
                    self.set_thinking_item(thread_id, remote_id);
                }
            }
            "plan" => {
                // Plan 模式的 proposed plan：以正文展示（不是思考折叠），
                // item/completed 的 text 为权威终稿；流式靠 item/plan/delta。
                let text = remote_item["text"].as_str().unwrap_or_default().to_string();
                if !text.is_empty() {
                    self.remove_current_turn_reasoning_item(thread_id);
                    self.set_text_item(thread_id, remote_id, text.clone(), false);
                }
                if completed {
                    let final_text = if !text.is_empty() {
                        Some(text)
                    } else {
                        self.local_text_item(thread_id, remote_id, false)
                    };
                    if let Some(final_text) = final_text.filter(|t| !t.trim().is_empty()) {
                        self.emit_proposed_plan(thread_id, Some(final_text));
                    }
                }
            }
            "commandExecution" | "fileChange" | "mcpToolCall" | "dynamicToolCall" | "webSearch"
            | "imageGeneration" => {
                if !completed {
                    self.remove_current_turn_reasoning_item(thread_id);
                }
                let item = self.tool_item_from_codex(thread_id, remote_item);
                self.upsert_tool_item(thread_id, remote_id, item, save_on_complete);
                if completed {
                    self.set_turn_waiting_item(thread_id);
                }
            }
            // 上下文压缩（自动触发或手动 thread/compact/start）：以分隔条形式展示进度
            "contextCompaction" => {
                if !completed {
                    self.remove_current_turn_reasoning_item(thread_id);
                }
                self.set_compaction_item(thread_id, remote_id, completed);
            }
            _ => {}
        }
    }

    /// 读取已落库的 assistant/thought 正文（plan 终稿为空时回退用）
    fn local_text_item(&self, thread_id: &str, remote_item_id: &str, thought: bool) -> Option<String> {
        let local_id = self.item_ids.lock().unwrap().get(remote_item_id).cloned()?;
        let state = self.app.state::<AppState>();
        let store = state.store.lock().unwrap();
        let thread = store.get(thread_id)?;
        for item in thread.items.iter().rev() {
            match item {
                Item::Assistant { id, text, .. } if *id == local_id && !thought => {
                    return Some(text.clone());
                }
                Item::Thought { id, text, .. } if *id == local_id && thought => {
                    return Some(text.clone());
                }
                _ => {}
            }
        }
        None
    }

    fn set_text_item(&self, thread_id: &str, remote_item_id: &str, text: String, thought: bool) {
        let state = self.app.state::<AppState>();
        let mut store = state.store.lock().unwrap();
        let Some(thread) = store.get_mut(thread_id) else {
            return;
        };
        let local_id = self.item_ids.lock().unwrap().get(remote_item_id).cloned();
        if let Some(local_id) = local_id {
            for item in thread.items.iter_mut().rev() {
                match item {
                    Item::Assistant { id, text: t, .. } if *id == local_id && !thought => {
                        *t = text;
                        let snapshot = item.clone();
                        self.emit_update(thread_id, json!({ "t": "upsert", "item": snapshot }));
                        return;
                    }
                    Item::Thought { id, text: t, .. } if *id == local_id && thought => {
                        *t = text;
                        let snapshot = item.clone();
                        self.emit_update(thread_id, json!({ "t": "upsert", "item": snapshot }));
                        return;
                    }
                    _ => {}
                }
            }
        }

        for item in complete_pending_tools(thread, None) {
            self.emit_update(thread_id, json!({ "t": "upsert", "item": item }));
        }
        let id = thread.next_item_id();
        let item = if thought {
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
        };
        self.item_ids
            .lock()
            .unwrap()
            .insert(remote_item_id.to_string(), id);
        thread.items.push(item.clone());
        thread.updated_at = now_ms();
        self.emit_update(thread_id, json!({ "t": "upsert", "item": item }));
    }

    fn set_thinking_item(&self, thread_id: &str, remote_item_id: &str) {
        if remote_item_id.is_empty() {
            return;
        }
        if let Some(turn_id) = self.active_turns.lock().unwrap().get(thread_id).cloned() {
            let previous = self
                .turn_reasoning_items
                .lock()
                .unwrap()
                .insert(turn_id, remote_item_id.to_string());
            if let Some(previous) = previous.filter(|id| id != remote_item_id) {
                self.remove_text_item(thread_id, &previous, true);
            }
        }
        self.set_text_item(thread_id, remote_item_id, "思考中…".to_string(), true);
    }

    fn set_turn_waiting_item(&self, thread_id: &str) {
        let turn_id = self.active_turns.lock().unwrap().get(thread_id).cloned();
        if let Some(turn_id) = turn_id {
            let remote_item_id = format!("turn-waiting-{turn_id}");
            self.set_thinking_item(thread_id, &remote_item_id);
        }
    }

    fn remove_current_turn_reasoning_item(&self, thread_id: &str) {
        let turn_id = self.active_turns.lock().unwrap().get(thread_id).cloned();
        if let Some(turn_id) = turn_id {
            self.remove_turn_reasoning_item(thread_id, &turn_id);
        }
    }

    fn remove_turn_reasoning_item(&self, thread_id: &str, turn_id: &str) {
        let remote_item_id = self.turn_reasoning_items.lock().unwrap().remove(turn_id);
        if let Some(remote_item_id) = remote_item_id {
            self.remove_text_item(thread_id, &remote_item_id, true);
        }
    }

    fn remove_text_item(&self, thread_id: &str, remote_item_id: &str, thought: bool) {
        let local_id = self.item_ids.lock().unwrap().remove(remote_item_id);
        let Some(local_id) = local_id else {
            return;
        };
        let state = self.app.state::<AppState>();
        let mut store = state.store.lock().unwrap();
        let Some(thread) = store.get_mut(thread_id) else {
            return;
        };
        let before = thread.items.len();
        thread.items.retain(|item| match item {
            Item::Assistant { id, .. } if *id == local_id && !thought => false,
            Item::Thought { id, .. } if *id == local_id && thought => false,
            _ => true,
        });
        if thread.items.len() != before {
            thread.updated_at = now_ms();
            self.emit_update(thread_id, json!({ "t": "remove", "itemId": local_id }));
        }
    }

    fn upsert_tool_item(
        &self,
        thread_id: &str,
        remote_item_id: &str,
        new_item: Item,
        save_on_complete: bool,
    ) {
        let state = self.app.state::<AppState>();
        let mut store = state.store.lock().unwrap();
        let Some(thread) = store.get_mut(thread_id) else {
            return;
        };
        let local_id = self.item_ids.lock().unwrap().get(remote_item_id).cloned();
        if let Some(local_id) = local_id {
            for item in thread.items.iter_mut().rev() {
                if item.id() == local_id {
                    *item = new_item.clone();
                    thread.updated_at = now_ms();
                    self.emit_update(thread_id, json!({ "t": "upsert", "item": new_item }));
                    if save_on_complete {
                        store.save();
                    }
                    return;
                }
            }
        }

        for item in complete_pending_tools(thread, Some(remote_item_id)) {
            self.emit_update(thread_id, json!({ "t": "upsert", "item": item }));
        }
        self.item_ids
            .lock()
            .unwrap()
            .insert(remote_item_id.to_string(), new_item.id());
        thread.items.push(new_item.clone());
        thread.updated_at = now_ms();
        self.emit_update(thread_id, json!({ "t": "upsert", "item": new_item }));
    }

    fn tool_item_from_codex(&self, thread_id: &str, item: &Value) -> Item {
        let id = item["id"].as_str().unwrap_or_default();
        let kind = item["type"].as_str().unwrap_or("dynamicToolCall");
        let call = match kind {
            "commandExecution" => {
                let command = item["command"].as_str().unwrap_or("执行命令").to_string();
                let output = item["aggregatedOutput"].as_str().unwrap_or_default();
                let content = if output.is_empty() {
                    Vec::new()
                } else {
                    let output = limit_display_text(output);
                    vec![json!({
                        "type": "content",
                        "content": { "type": "text", "text": output }
                    })]
                };
                ToolCall {
                    tool_call_id: id.to_string(),
                    title: command.clone(),
                    kind: "execute".into(),
                    status: normalize_tool_status(item["status"].as_str().unwrap_or("inProgress")),
                    content,
                    locations: Vec::new(),
                    raw_input: Some(json!({
                        "command": command,
                        "cwd": item["cwd"],
                        "source": item["source"],
                        "commandActions": item["commandActions"]
                    })),
                    raw_output: Some(json!({
                        "exitCode": item["exitCode"],
                        "durationMs": item["durationMs"]
                    })),
                }
            }
            "fileChange" => {
                let changes = item["changes"].as_array().cloned().unwrap_or_default();
                let title = if changes.len() == 1 {
                    format!("修改 {}", changes[0]["path"].as_str().unwrap_or("文件"))
                } else {
                    format!("修改 {} 个文件", changes.len())
                };
                let content: Vec<Value> = changes
                    .iter()
                    .map(|c| {
                        json!({
                            "type": "content",
                            "content": {
                                "type": "text",
                                "text": limit_display_text(&format!("{}\n{}", c["path"].as_str().unwrap_or(""), c["diff"].as_str().unwrap_or("")))
                            }
                        })
                    })
                    .collect();
                ToolCall {
                    tool_call_id: id.to_string(),
                    title,
                    kind: "edit".into(),
                    status: normalize_tool_status(item["status"].as_str().unwrap_or("inProgress")),
                    content,
                    locations: changes
                        .iter()
                        .filter_map(|c| c["path"].as_str().map(|p| json!({ "path": p })))
                        .collect(),
                    raw_input: None,
                    raw_output: Some(json!({ "changes": changes })),
                }
            }
            "mcpToolCall" => ToolCall {
                tool_call_id: id.to_string(),
                title: format!(
                    "{}.{}",
                    item["server"].as_str().unwrap_or("mcp"),
                    item["tool"].as_str().unwrap_or("tool")
                ),
                kind: "execute".into(),
                status: normalize_tool_status(item["status"].as_str().unwrap_or("inProgress")),
                content: codex_result_content(item.get("result"), item.get("error")),
                locations: Vec::new(),
                raw_input: item.get("arguments").filter(|v| !v.is_null()).cloned(),
                raw_output: item
                    .get("result")
                    .filter(|v| !v.is_null())
                    .cloned()
                    .or_else(|| item.get("error").cloned()),
            },
            "webSearch" => ToolCall {
                tool_call_id: id.to_string(),
                title: item["query"].as_str().unwrap_or("网页搜索").to_string(),
                kind: "search".into(),
                status: "completed".into(),
                content: Vec::new(),
                locations: Vec::new(),
                raw_input: Some(item.clone()),
                raw_output: None,
            },
            "imageGeneration" => ToolCall {
                tool_call_id: id.to_string(),
                title: "生成图片".into(),
                kind: "execute".into(),
                status: normalize_tool_status(item["status"].as_str().unwrap_or("inProgress")),
                content: codex_result_content(item.get("result"), None),
                locations: Vec::new(),
                raw_input: Some(json!({ "revisedPrompt": item["revisedPrompt"] })),
                raw_output: Some(item.clone()),
            },
            _ => ToolCall {
                tool_call_id: id.to_string(),
                title: format!(
                    "{}{}",
                    item["namespace"].as_str().unwrap_or(""),
                    item["tool"].as_str().unwrap_or("工具调用")
                ),
                kind: "execute".into(),
                status: normalize_tool_status(item["status"].as_str().unwrap_or("inProgress")),
                content: codex_result_content(item.get("contentItems"), None),
                locations: Vec::new(),
                raw_input: Some(item["arguments"].clone()),
                raw_output: Some(item.clone()),
            },
        };
        let local_id = self
            .item_ids
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .unwrap_or_else(|| {
                let state = self.app.state::<AppState>();
                let store = state.store.lock().unwrap();
                store.get(thread_id).map(|t| t.next_item_id()).unwrap_or(1)
            });
        Item::Tool {
            id: local_id,
            ts: now_ms(),
            call,
        }
    }

    /// 模型列表里的默认模型 id（collaborationMode.settings.model 必填，线程未选模型时用它）
    async fn default_model_id(self: &Arc<Self>) -> Option<String> {
        let opts = match self.get_model_options() {
            Some(v) => v,
            None => self.fetch_model_options().await.ok()?,
        };
        let cfg = opts
            .get("configOptions")?
            .as_array()?
            .iter()
            .find(|o| o.get("id").and_then(|v| v.as_str()) == Some("model"))?;
        let options = cfg.get("options")?.as_array()?;
        let pick = options
            .iter()
            .find(|o| {
                o.get("_meta")
                    .and_then(|m| m.get("codex.ai/default"))
                    .and_then(|v| v.as_bool())
                    == Some(true)
            })
            .or_else(|| options.first())?;
        let value = pick.get("value")?.as_str()?;
        // 选项 value 可能是组合形式 `<model>:<effort>`
        let (base, _) = split_model_effort(Some(value));
        base
    }

    pub async fn fetch_model_options(self: &Arc<Self>) -> Result<Value, String> {
        if let Some(v) = self.get_model_options() {
            return Ok(v);
        }
        self.fetch_model_options_from_agent().await
    }

    async fn fetch_model_options_from_agent(self: &Arc<Self>) -> Result<Value, String> {
        let conn = self.ensure_conn().await?;
        let mut all: Vec<Value> = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let resp = conn
                .request(
                    "model/list",
                    json!({ "cursor": cursor, "limit": 100, "includeHidden": false }),
                    Some(Duration::from_secs(60)),
                )
                .await?;
            if let Some(data) = resp["data"].as_array() {
                all.extend(data.iter().cloned());
            }
            cursor = resp["nextCursor"].as_str().map(|s| s.to_string());
            if cursor.is_none() {
                break;
            }
        }
        // 思考强度不再单列下拉，而是与模型组合成单一选项（如「GPT-5.1-Codex · 高」），
        // 选项 value 形如 `<model>:<effort>`，发送轮次时再拆出 effort 传给 codex。
        let mut options: Vec<Value> = Vec::new();
        for m in all {
            let value = m["id"]
                .as_str()
                .or_else(|| m["model"].as_str())
                .unwrap_or("");
            if value.is_empty() {
                continue;
            }
            let display = m["displayName"].as_str().unwrap_or(value);
            let supports_images = m["inputModalities"]
                .as_array()
                .map(|xs| xs.iter().any(|v| v.as_str() == Some("image")))
                .unwrap_or(false);
            let is_default_model = m["isDefault"].as_bool().unwrap_or(false);
            let default_effort = m["defaultReasoningEffort"].as_str().unwrap_or("");
            let description = m["description"].as_str().unwrap_or("");
            let efforts: Vec<(String, String)> = m["supportedReasoningEfforts"]
                .as_array()
                .map(|xs| {
                    xs.iter()
                        .filter_map(|e| {
                            let v = e["reasoningEffort"].as_str()?;
                            Some((
                                v.to_string(),
                                e["description"].as_str().unwrap_or("").to_string(),
                            ))
                        })
                        .collect()
                })
                .unwrap_or_default();
            if efforts.is_empty() {
                options.push(json!({
                    "value": value,
                    "name": display,
                    "_meta": {
                        "codex.ai/supportsImages": supports_images,
                        "codex.ai/default": is_default_model,
                        "codex.ai/description": description
                    }
                }));
            } else {
                for (effort, edesc) in efforts {
                    let desc = if edesc.is_empty() {
                        description.to_string()
                    } else {
                        edesc
                    };
                    options.push(json!({
                        "value": format!("{value}:{effort}"),
                        "name": format!("{display} · {}", effort_label(&effort)),
                        "_meta": {
                            "codex.ai/supportsImages": supports_images,
                            "codex.ai/default": is_default_model && effort == default_effort,
                            "codex.ai/effort": effort,
                            "codex.ai/description": desc
                        }
                    }));
                }
            }
        }
        let v = json!({
            "configOptions": [{
                "id": "model",
                "name": "模型",
                "options": options
            }],
            "modes": {
                "currentModeId": "build",
                "availableModes": codex_modes()
            }
        });
        *self.model_options.lock().unwrap() = Some(v.clone());
        self.persist_model_options(&v);
        let _ = self.app.emit(
            EV_OPTIONS,
            json!({ "agentKind": "codex", "options": v.clone() }),
        );
        Ok(v)
    }

    pub async fn prewarm(
        self: &Arc<Self>,
        cwd: String,
        model: Option<String>,
        mode: Option<String>,
    ) {
        let key = codex_warm_key(&cwd, model.as_deref(), mode.as_deref());
        {
            let warmed = self.prewarmed.lock().unwrap();
            let mut warming = self.prewarming.lock().unwrap();
            if warmed.contains_key(&key) || !warming.insert(key.clone()) {
                return;
            }
        }
        let result = async {
            let conn = self.ensure_conn().await?;
            self.start_remote_thread(&conn, &cwd, model.as_deref(), mode.as_deref())
                .await
        }
        .await;
        self.prewarming.lock().unwrap().remove(&key);
        match result {
            Ok(remote_id) => {
                self.push_log(format!("预热完成 {cwd} → {remote_id}"));
                self.prewarmed.lock().unwrap().insert(key, remote_id);
            }
            Err(e) => self.push_log(format!("预热失败 {cwd}: {e}")),
        }
    }

    async fn take_prewarmed_thread(&self, key: &str) -> Option<String> {
        for _ in 0..200 {
            if let Some(remote_id) = self.prewarmed.lock().unwrap().remove(key) {
                return Some(remote_id);
            }
            if !self.prewarming.lock().unwrap().contains(key) {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        None
    }

    async fn ensure_thread(self: &Arc<Self>, thread_id: &str) -> Result<String, String> {
        let lock = self.thread_lock(thread_id);
        let _guard = lock.lock().await;
        let conn = self.ensure_conn().await?;
        let (cwd, existing, model, mode) = {
            let state = self.app.state::<AppState>();
            let store = state.store.lock().unwrap();
            let thread = store.get(thread_id).ok_or("线程不存在")?;
            (
                thread.cwd.clone(),
                thread.acp_session_id.clone(),
                thread.model.clone(),
                thread.mode.clone(),
            )
        };

        let remote_id = match existing {
            Some(remote_id) if self.routes.lock().unwrap().contains_key(&remote_id) => remote_id,
            Some(remote_id) => {
                let resumed = conn
                    .request(
                        "thread/resume",
                        codex_thread_params(
                            Some(&remote_id),
                            &cwd,
                            model.as_deref(),
                            mode.as_deref(),
                        ),
                        Some(Duration::from_secs(120)),
                    )
                    .await;
                match resumed {
                    Ok(resp) => {
                        let id = resp["thread"]["id"]
                            .as_str()
                            .unwrap_or(remote_id.as_str())
                            .to_string();
                        // 恢复的 thread 同样会在 app-server 里驻留一套 MCP，计入回收阈值
                        *self.threads_spawned.lock().unwrap() += 1;
                        self.routes
                            .lock()
                            .unwrap()
                            .insert(id.clone(), thread_id.to_string());
                        id
                    }
                    Err(e) => {
                        self.push_log(format!("thread/resume 失败，转为新建会话：{e}"));
                        let id = self
                            .start_thread(&conn, thread_id, &cwd, model.as_deref(), mode.as_deref())
                            .await?;
                        let state = self.app.state::<AppState>();
                        let mut store = state.store.lock().unwrap();
                        if let Some(thread) = store.get_mut(thread_id) {
                            thread.acp_session_id = Some(id.clone());
                            let item = thread.push_system(
                                "Codex 历史会话无法恢复，已在新会话中继续。".into(),
                                "warn",
                            );
                            self.emit_update(thread_id, json!({ "t": "upsert", "item": item }));
                            store.save();
                        }
                        id
                    }
                }
            }
            None => {
                let key = codex_warm_key(&cwd, model.as_deref(), mode.as_deref());
                let id = match self.take_prewarmed_thread(&key).await {
                    Some(id) => {
                        self.routes
                            .lock()
                            .unwrap()
                            .insert(id.clone(), thread_id.to_string());
                        id
                    }
                    None => {
                        self.start_thread(&conn, thread_id, &cwd, model.as_deref(), mode.as_deref())
                            .await?
                    }
                };
                let state = self.app.state::<AppState>();
                let mut store = state.store.lock().unwrap();
                if let Some(thread) = store.get_mut(thread_id) {
                    thread.acp_session_id = Some(id.clone());
                    store.save();
                }
                id
            }
        };
        Ok(remote_id)
    }

    async fn start_thread(
        self: &Arc<Self>,
        conn: &Arc<CodexConn>,
        thread_id: &str,
        cwd: &str,
        model: Option<&str>,
        mode: Option<&str>,
    ) -> Result<String, String> {
        let id = self.start_remote_thread(conn, cwd, model, mode).await?;
        self.routes
            .lock()
            .unwrap()
            .insert(id.clone(), thread_id.to_string());
        Ok(id)
    }

    async fn start_remote_thread(
        self: &Arc<Self>,
        conn: &Arc<CodexConn>,
        cwd: &str,
        model: Option<&str>,
        mode: Option<&str>,
    ) -> Result<String, String> {
        let resp = conn
            .request(
                "thread/start",
                codex_thread_params(None, cwd, model, mode),
                Some(Duration::from_secs(120)),
            )
            .await
            .map_err(|e| format!("创建 Codex 会话失败：{e}"))?;
        let id = resp["thread"]["id"]
            .as_str()
            .ok_or("thread/start 未返回 thread.id")?
            .to_string();
        *self.threads_spawned.lock().unwrap() += 1;
        Ok(id)
    }

    pub async fn run_prompt(
        self: &Arc<Self>,
        thread_id: String,
        text: String,
        images: Vec<PromptImage>,
    ) {
        // 上下文接力：跨 agent 切换后的首条消息，把历史注入新 agent
        let handoff = {
            let state = self.app.state::<AppState>();
            let mut store = state.store.lock().unwrap();
            let ctx = store
                .get_mut(&thread_id)
                .and_then(|t| t.take_handoff_context("Codex"));
            if ctx.is_some() {
                store.save();
            }
            ctx
        };
        let mut title_job: Option<(String, String)> = None;
        {
            let state = self.app.state::<AppState>();
            let mut store = state.store.lock().unwrap();
            let Some(thread) = store.get_mut(&thread_id) else {
                return;
            };
            let item = thread.push_user(text.clone(), images.clone());
            if thread.title == "新会话" {
                let fallback = derive_title(&text, !images.is_empty());
                thread.title = fallback.clone();
                title_job = Some((text.clone(), fallback));
                let _ = self.app.emit(EV_THREADS, json!({}));
            }
            store.save();
            self.emit_update(&thread_id, json!({ "t": "upsert", "item": item }));
        }
        if let Some((prompt, fallback)) = title_job {
            let state = self.app.state::<AppState>();
            state.generate_title(&AgentKind::Codex, thread_id.clone(), prompt, fallback);
        }
        self.clear_plan(&thread_id);
        self.set_running(&thread_id, true, None);

        let outcome = self
            .drive_prompt(&thread_id, &text, &images, handoff.as_deref())
            .await;
        if !self.is_running(&thread_id) {
            return;
        }

        let (stop_reason, usage) = match outcome {
            Ok(o) => {
                // 轮次失败（turn.status=failed / error 通知）此前只落日志，用户看不到；这里补上
                if let Some(err) = o.error {
                    let state = self.app.state::<AppState>();
                    let mut store = state.store.lock().unwrap();
                    if let Some(thread) = store.get_mut(&thread_id) {
                        let item = thread.push_system(err, "error");
                        self.emit_update(&thread_id, json!({ "t": "upsert", "item": item }));
                    }
                }
                (o.stop_reason, o.usage)
            }
            Err(e) => {
                let state = self.app.state::<AppState>();
                let mut store = state.store.lock().unwrap();
                if let Some(thread) = store.get_mut(&thread_id) {
                    let item = thread.push_system(e, "error");
                    self.emit_update(&thread_id, json!({ "t": "upsert", "item": item }));
                }
                ("error".to_string(), None)
            }
        };
        self.finish_turn(&thread_id, stop_reason, usage);
        let _ = self.app.emit(EV_THREADS, json!({}));
    }

    async fn drive_prompt(
        self: &Arc<Self>,
        thread_id: &str,
        text: &str,
        images: &[PromptImage],
        handoff: Option<&str>,
    ) -> Result<CodexTurnOutcome, String> {
        let remote_thread_id = self.ensure_thread(thread_id).await?;
        let conn = self.current_conn().await.ok_or("codex 未连接")?;
        let (model, stored_effort, mode) = {
            let state = self.app.state::<AppState>();
            let store = state.store.lock().unwrap();
            let t = store.get(thread_id);
            (
                t.and_then(|t| t.model.clone()),
                t.and_then(|t| t.reasoning_effort.clone()),
                t.and_then(|t| t.mode.clone()),
            )
        };
        // 组合选项 `<model>:<effort>` 拆出 effort；旧会话回退到单独存的 reasoning_effort
        let (base_model, model_effort) = split_model_effort(model.as_deref());
        let effort = model_effort.or(stored_effort);
        let mut input = build_user_input(text, images);
        if let Some(ctx) = handoff {
            input.insert(
                0,
                json!({ "type": "text", "text": ctx, "text_elements": [] }),
            );
        }
        // Codex 原生 Plan 模式（TUI 的 /plan）：turn/start.collaborationMode（experimental API，
        // initialize 已声明 experimentalApi）。settings.model 必填字符串：优先线程所选模型，
        // 否则用模型列表里的默认模型；developer_instructions=null 表示用 codex 内置 Plan 指令。
        // 万一拿不到模型 id，退回注入规划指令的兜底方案，保证 Plan 语义不丢。
        let mut collaboration_mode: Option<Value> = None;
        if mode.as_deref() == Some("plan") {
            let cm_model = match base_model.clone() {
                Some(m) => Some(m),
                None => self.default_model_id().await,
            };
            match cm_model {
                Some(m) => {
                    let mut settings = json!({ "model": m, "developer_instructions": null });
                    if let Some(e) = effort.clone().filter(|e| !e.is_empty()) {
                        settings["reasoning_effort"] = json!(e);
                    }
                    collaboration_mode = Some(json!({ "mode": "plan", "settings": settings }));
                }
                None => {
                    self.push_log("[nova] 未取到默认模型，Plan 模式退回指令注入".into());
                    input.insert(
                        0,
                        json!({ "type": "text", "text": PLAN_MODE_DIRECTIVE, "text_elements": [] }),
                    );
                }
            }
        }
        let mut params = json!({
            "threadId": remote_thread_id,
            "input": input,
            "model": base_model
        });
        if let Some(effort) = effort.filter(|e| !e.is_empty()) {
            params["effort"] = json!(effort);
        }
        if let Some(cm) = collaboration_mode {
            params["collaborationMode"] = cm;
        }
        let resp = conn
            .request("turn/start", params, Some(Duration::from_secs(60)))
            .await?;
        let turn_id = resp["turn"]["id"]
            .as_str()
            .ok_or("turn/start 未返回 turn.id")?
            .to_string();
        self.active_turns
            .lock()
            .unwrap()
            .insert(thread_id.to_string(), turn_id.clone());
        self.set_turn_waiting_item(thread_id);
        let (tx, rx) = oneshot::channel();
        self.turn_waiters
            .lock()
            .unwrap()
            .insert(turn_id.clone(), tx);
        rx.await.map_err(|_| "Codex 轮次等待失败".to_string())?
    }

    pub async fn steer_prompt(
        self: &Arc<Self>,
        thread_id: String,
        text: String,
        images: Vec<PromptImage>,
    ) {
        let (remote_thread_id, turn_id) = {
            let state = self.app.state::<AppState>();
            let store = state.store.lock().unwrap();
            let remote = store.get(&thread_id).and_then(|t| t.acp_session_id.clone());
            let turn = self.active_turns.lock().unwrap().get(&thread_id).cloned();
            (remote, turn)
        };
        let err = |msg: String| {
            let state = self.app.state::<AppState>();
            let mut store = state.store.lock().unwrap();
            if let Some(thread) = store.get_mut(&thread_id) {
                let item = thread.push_system(msg, "error");
                store.save();
                self.emit_update(&thread_id, json!({ "t": "upsert", "item": item }));
            }
        };
        let Some(remote_thread_id) = remote_thread_id else {
            err("引导消息发送失败：Codex 会话尚未建立".into());
            return;
        };
        let Some(turn_id) = turn_id else {
            err("引导消息发送失败：当前 Codex 轮次尚未建立".into());
            return;
        };
        let Some(conn) = self.current_conn().await else {
            err("引导消息发送失败：Codex 未连接".into());
            return;
        };
        {
            let state = self.app.state::<AppState>();
            let mut store = state.store.lock().unwrap();
            let Some(thread) = store.get_mut(&thread_id) else {
                return;
            };
            let item = thread.push_user(text.clone(), images.clone());
            store.save();
            self.emit_update(&thread_id, json!({ "t": "upsert", "item": item }));
        }
        self.mark_plan_interrupted(&thread_id, "interrupted", false);
        self.emit_proposed_plan(&thread_id, None);
        self.set_turn_waiting_item(&thread_id);
        let _ = self.app.emit(EV_THREADS, json!({}));
        let input = build_user_input(&text, &images);
        let mgr = self.clone();
        tauri::async_runtime::spawn(async move {
            if let Err(e) = conn
                .request(
                    "turn/steer",
                    json!({ "threadId": remote_thread_id, "expectedTurnId": turn_id, "input": input }),
                    Some(Duration::from_secs(60)),
                )
                .await
            {
                mgr.push_log(format!("引导消息发送失败 {thread_id}: {e}"));
                let state = mgr.app.state::<AppState>();
                let mut store = state.store.lock().unwrap();
                if let Some(thread) = store.get_mut(&thread_id) {
                    let item = thread.push_system(format!("引导消息发送失败：{e}"), "error");
                    store.save();
                    mgr.emit_update(&thread_id, json!({ "t": "upsert", "item": item }));
                }
            }
        });
    }

    pub async fn cancel(self: &Arc<Self>, thread_id: &str) {
        if !self.is_running(thread_id) {
            return;
        }
        let (remote_thread_id, turn_id) = {
            let state = self.app.state::<AppState>();
            let store = state.store.lock().unwrap();
            let remote = store.get(thread_id).and_then(|t| t.acp_session_id.clone());
            let turn = self.active_turns.lock().unwrap().get(thread_id).cloned();
            (remote, turn)
        };
        // 先本地立即结束（running 马上清掉，停止在界面上立即生效，且用户随后的
        // 「编辑重发」不会被 truncate 的运行中校验拒绝）；turn/interrupt 转入后台
        // 尽力而为——codex 忙于工具执行时可能拖到 10s 才应答，不能让停止等它。
        self.force_finish(thread_id, "已停止当前 Codex 任务。")
            .await;
        if let (Some(remote_thread_id), Some(turn_id)) = (remote_thread_id, turn_id) {
            let mgr = self.clone();
            tauri::async_runtime::spawn(async move {
                if let Some(conn) = mgr.current_conn().await {
                    let _ = conn
                        .request(
                            "turn/interrupt",
                            json!({ "threadId": remote_thread_id, "turnId": turn_id }),
                            Some(Duration::from_secs(10)),
                        )
                        .await;
                }
            });
        }
    }

    async fn force_finish(&self, thread_id: &str, msg: &str) {
        self.mark_plan_interrupted(thread_id, "cancelled", true);
        // 清掉本轮残留的「思考中…」占位，避免停止后界面仍显示在思考。
        self.remove_current_turn_reasoning_item(thread_id);
        {
            let state = self.app.state::<AppState>();
            let mut store = state.store.lock().unwrap();
            if let Some(thread) = store.get_mut(thread_id) {
                let item = thread.push_system(msg.to_string(), "warn");
                self.emit_update(thread_id, json!({ "t": "upsert", "item": item }));
            }
            store.save();
        }
        self.finish_turn(thread_id, "force_cancelled".into(), None);
        let _ = self.app.emit(EV_THREADS, json!({}));
    }

    pub async fn respond_permission(
        &self,
        request_key: &str,
        option_id: &str,
    ) -> Result<(), String> {
        let perm = self
            .pending_permissions
            .lock()
            .unwrap()
            .remove(request_key)
            .ok_or("该权限请求已失效")?;
        let conn = self.current_conn().await.ok_or("codex 未连接")?;
        let decision = match option_id {
            "accept" => "accept",
            "acceptForSession" => "acceptForSession",
            "decline" => "decline",
            _ => "cancel",
        };
        let result = if perm.method == "execCommandApproval" {
            json!({ "decision": if decision == "accept" { "approved" } else if decision == "acceptForSession" { "approved_for_session" } else if decision == "decline" { "denied" } else { "abort" } })
        } else if perm.method == "applyPatchApproval" {
            json!({ "decision": if decision == "accept" { "approved" } else if decision == "acceptForSession" { "approved_for_session" } else if decision == "decline" { "denied" } else { "abort" } })
        } else {
            json!({ "decision": decision })
        };
        conn.respond_ok(perm.rpc_id, result);
        let _ = self
            .app
            .emit(EV_PERMISSION_RESOLVED, json!({ "requestKey": request_key }));
        Ok(())
    }

    pub fn has_pending_permission(&self, request_key: &str) -> bool {
        self.pending_permissions
            .lock()
            .unwrap()
            .contains_key(request_key)
    }

    fn set_running(&self, thread_id: &str, running: bool, stop_reason: Option<String>) {
        {
            let mut set = self.running_threads.lock().unwrap();
            if running {
                set.insert(thread_id.to_string());
                self.turn_started
                    .lock()
                    .unwrap()
                    .insert(thread_id.to_string(), std::time::Instant::now());
            } else {
                set.remove(thread_id);
            }
        }
        // 轮次开始/结束都刷新连接活跃时刻（结束时刻即空闲计时起点）
        self.touch_conn();
        let _ = self.app.emit(
            EV_TURN,
            json!({ "threadId": thread_id, "running": running, "stopReason": stop_reason }),
        );
    }

    fn finish_turn(&self, thread_id: &str, stop_reason: String, usage: Option<Value>) {
        let duration_ms = self
            .turn_started
            .lock()
            .unwrap()
            .remove(thread_id)
            .map(|t| t.elapsed().as_millis() as u64)
            .unwrap_or(0);
        {
            let state = self.app.state::<AppState>();
            let mut store = state.store.lock().unwrap();
            if let Some(thread) = store.get_mut(thread_id) {
                for item in complete_pending_tools(thread, None) {
                    self.emit_update(thread_id, json!({ "t": "upsert", "item": item }));
                }
                let item = thread.push_turn(duration_ms, usage.as_ref(), &stop_reason);
                self.emit_update(thread_id, json!({ "t": "upsert", "item": item }));
            }
            store.save();
        }
        // Plan 模式收尾：若本轮没有单独的 plan item（或仅有助手正文），仍给出「实施此计划」
        self.maybe_emit_plan_action(thread_id, &stop_reason);
        self.set_running(thread_id, false, Some(stop_reason.clone()));
        self.notify_done(thread_id, &stop_reason);
    }

    /// Plan 模式轮次正常结束时，用本轮最后一条助手正文作为「实施此计划」的依据。
    /// 已通过 plan item 推送过的也会再推一次（同文案），保证按钮在 running 结束后可见。
    fn maybe_emit_plan_action(&self, thread_id: &str, stop_reason: &str) {
        if !matches!(stop_reason, "end_turn" | "max_turn_requests") {
            return;
        }
        let text = {
            let state = self.app.state::<AppState>();
            let store = state.store.lock().unwrap();
            let Some(thread) = store.get(thread_id) else {
                return;
            };
            if thread.mode.as_deref() != Some("plan") {
                return;
            }
            let mut last_assistant: Option<String> = None;
            for item in thread.items.iter().rev() {
                match item {
                    Item::Turn { .. } => continue,
                    Item::User { .. } => break,
                    Item::Assistant { text, .. } if !text.trim().is_empty() => {
                        last_assistant = Some(text.clone());
                        break;
                    }
                    _ => {}
                }
            }
            last_assistant
        };
        if let Some(text) = text {
            self.emit_proposed_plan(thread_id, Some(text));
        }
    }

    fn mark_plan_interrupted(&self, thread_id: &str, status: &str, include_pending: bool) {
        let plan = {
            let state = self.app.state::<AppState>();
            let mut store = state.store.lock().unwrap();
            let Some(thread) = store.get_mut(thread_id) else {
                return;
            };
            let Some(plan) = thread.plan.as_mut() else {
                return;
            };
            let Some(entries) = plan.as_array_mut() else {
                return;
            };
            let mut changed = false;
            for entry in entries {
                let current = entry["status"].as_str().unwrap_or_default();
                if current == "in_progress" || (include_pending && current == "pending") {
                    entry["status"] = json!(status);
                    changed = true;
                }
            }
            if !changed {
                return;
            }
            thread.updated_at = now_ms();
            let plan = plan.clone();
            store.save();
            plan
        };
        self.emit_update(thread_id, json!({ "t": "plan", "plan": plan }));
    }

    fn clear_plan(&self, thread_id: &str) {
        let changed = {
            let state = self.app.state::<AppState>();
            let mut store = state.store.lock().unwrap();
            let Some(thread) = store.get_mut(thread_id) else {
                return;
            };
            if thread.plan.is_none() {
                false
            } else {
                thread.plan = None;
                thread.updated_at = now_ms();
                store.save();
                true
            }
        };
        if changed {
            self.emit_update(thread_id, json!({ "t": "plan", "plan": [] }));
        }
        // 无论 checklist 是否存在，都清掉「实施此计划」提示
        self.emit_proposed_plan(thread_id, None);
    }

    /// Plan 模式产出的 proposed plan 正文：前端据此展示「实施此计划 / 继续规划」。
    fn emit_proposed_plan(&self, thread_id: &str, text: Option<String>) {
        self.emit_update(
            thread_id,
            json!({ "t": "proposed_plan", "text": text }),
        );
    }

    fn notify_done(&self, thread_id: &str, stop_reason: &str) {
        let title = {
            let state = self.app.state::<AppState>();
            let store = state.store.lock().unwrap();
            match store.get(thread_id) {
                // 数字员工的后台会话完成不弹系统提醒（巡查/开发都算），避免打扰。
                Some(t) if t.employee_id.is_some() => return,
                Some(t) => t.title.clone(),
                None => return,
            }
        };
        let body = match stop_reason {
            "end_turn" | "max_turn_requests" => "Codex 任务已完成，点击查看结果",
            "cancelled" | "force_cancelled" => "Codex 任务已停止",
            _ => "Codex 任务已结束（出错）",
        };
        crate::sys_notify::notify_thread_done(&self.app, thread_id, &title, body, EV_NOTIFY_OPEN);
    }

    /// 触发手动上下文压缩：调用 codex 原生 thread/compact/start。
    /// 压缩在 codex 内部完成（把历史浓缩为摘要，后续轮次仅基于摘要继续），
    /// 进度通过 contextCompaction item 通知，完成/超时后解除忙碌态。
    pub async fn compact(self: &Arc<Self>, thread_id: String) {
        if self.is_running(&thread_id) {
            return;
        }
        self.manual_compacting
            .lock()
            .unwrap()
            .insert(thread_id.clone());
        self.set_running(&thread_id, true, None);
        let result = async {
            let remote_thread_id = self.ensure_thread(&thread_id).await?;
            let conn = self.current_conn().await.ok_or("codex 未连接")?;
            conn.request(
                "thread/compact/start",
                json!({ "threadId": remote_thread_id }),
                Some(Duration::from_secs(60)),
            )
            .await
        }
        .await;
        match result {
            Ok(_) => {
                // 兜底：极端情况下长时间收不到压缩完成通知时自行解除忙碌态，避免卡住
                let mgr = self.clone();
                let tid = thread_id.clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(Duration::from_secs(180)).await;
                    mgr.finish_manual_compaction(&tid);
                });
            }
            Err(e) => {
                self.manual_compacting.lock().unwrap().remove(&thread_id);
                {
                    let state = self.app.state::<AppState>();
                    let mut store = state.store.lock().unwrap();
                    if let Some(thread) = store.get_mut(&thread_id) {
                        let item = thread.push_system(format!("压缩上下文失败：{e}"), "error");
                        self.emit_update(&thread_id, json!({ "t": "upsert", "item": item }));
                        store.save();
                    }
                }
                self.set_running(&thread_id, false, Some("error".into()));
                let _ = self.app.emit(EV_THREADS, json!({}));
            }
        }
    }

    /// 手动压缩收尾：解除忙碌态（幂等；压缩完成通知与伪轮次结束通知谁先到都安全）
    fn finish_manual_compaction(&self, thread_id: &str) {
        let was_manual = self.manual_compacting.lock().unwrap().remove(thread_id);
        if was_manual {
            self.set_running(thread_id, false, Some("compacted".into()));
            let _ = self.app.emit(EV_THREADS, json!({}));
        }
    }

    /// 压缩进度分隔条：进行中 level=compacting「正在压缩上下文…」，完成 level=compacted「已压缩」。
    /// 复用 System item + remote→local 映射，started/completed 命中同一条原地更新。
    fn set_compaction_item(&self, thread_id: &str, remote_item_id: &str, done: bool) {
        let (level, text) = if done {
            ("compacted", "上下文已压缩，后续将基于摘要继续")
        } else {
            ("compacting", "正在压缩上下文…")
        };
        {
            let state = self.app.state::<AppState>();
            let mut store = state.store.lock().unwrap();
            let Some(thread) = store.get_mut(thread_id) else {
                return;
            };
            let local_id = self.item_ids.lock().unwrap().get(remote_item_id).cloned();
            let mut updated = false;
            if let Some(local_id) = local_id {
                for item in thread.items.iter_mut().rev() {
                    if let Item::System {
                        id,
                        text: t,
                        level: l,
                        ..
                    } = item
                    {
                        if *id == local_id {
                            *t = text.to_string();
                            *l = level.to_string();
                            let snapshot = item.clone();
                            thread.updated_at = now_ms();
                            self.emit_update(thread_id, json!({ "t": "upsert", "item": snapshot }));
                            updated = true;
                            break;
                        }
                    }
                }
            }
            if !updated {
                for item in complete_pending_tools(thread, None) {
                    self.emit_update(thread_id, json!({ "t": "upsert", "item": item }));
                }
                let item = thread.push_system(text.to_string(), level);
                self.item_ids
                    .lock()
                    .unwrap()
                    .insert(remote_item_id.to_string(), item.id());
                self.emit_update(thread_id, json!({ "t": "upsert", "item": item }));
            }
            store.save();
        }
        if done {
            self.finish_manual_compaction(thread_id);
        }
    }

    pub fn forget_session_of_thread(&self, thread_id: &str) {
        self.routes.lock().unwrap().retain(|_, t| t != thread_id);
        self.thread_locks.lock().unwrap().remove(thread_id);
        self.active_turns.lock().unwrap().remove(thread_id);
    }
}

#[cfg(windows)]
fn apply_windows_sandbox_fallback(args: &mut Vec<String>) {
    if args.iter().any(|arg| arg.contains("windows.sandbox")) {
        return;
    }
    let insert_at = args
        .iter()
        .position(|arg| arg == "app-server")
        .map(|i| i + 1)
        .unwrap_or(0);
    args.insert(insert_at, "windows.sandbox=\"unelevated\"".into());
    args.insert(insert_at, "-c".into());
}

/// Plan 模式兜底指令：正常走 turn/start.collaborationMode 原生 Plan（TUI 的 /plan），
/// 仅在拿不到必填的模型 id 时退回注入本指令 + 只读沙箱。
const PLAN_MODE_DIRECTIVE: &str = "【Plan 模式】本轮只做分析与规划：阅读代码、梳理现状，\
输出实施计划（目标、改动点、涉及文件、步骤顺序、风险与验证方式）。\
不要修改任何文件，不要执行有副作用的命令；等用户切换到 Build 模式后再实施。";

fn codex_thread_params(
    thread_id: Option<&str>,
    cwd: &str,
    model: Option<&str>,
    mode: Option<&str>,
) -> Value {
    let (approval_policy, sandbox) = codex_policy(mode);
    let mut params = json!({
        "cwd": cwd,
        "approvalPolicy": approval_policy,
        "sandbox": sandbox,
        "approvalsReviewer": "user"
    });
    if let Some(thread_id) = thread_id {
        params["threadId"] = json!(thread_id);
    } else {
        params["ephemeral"] = json!(false);
    }
    // model 可能形如 `<id>:<effort>`，thread/start 只认模型本身，effort 在 turn/start 传
    if let (Some(base), _) = split_model_effort(model) {
        params["model"] = json!(base);
    }
    params
}

/// 拆分模型选项 value：组合形式 `<model>:<effort>` -> (模型, 思考强度)
fn split_model_effort(model: Option<&str>) -> (Option<String>, Option<String>) {
    let Some(m) = model.filter(|m| !m.is_empty()) else {
        return (None, None);
    };
    match m.rsplit_once(':') {
        Some((id, effort)) if !id.is_empty() && !effort.is_empty() => {
            (Some(id.to_string()), Some(effort.to_string()))
        }
        _ => (Some(m.to_string()), None),
    }
}

fn codex_warm_key(cwd: &str, model: Option<&str>, mode: Option<&str>) -> String {
    let (base_model, _) = split_model_effort(model);
    format!(
        "{}\n{}\n{}",
        cwd,
        base_model.unwrap_or_default(),
        mode.unwrap_or_default()
    )
}

/// 思考强度标签（英文）
fn effort_label(effort: &str) -> String {
    match effort {
        "minimal" => "Minimal",
        "low" => "Low",
        "medium" => "Medium",
        "high" => "High",
        "xhigh" => "XHigh",
        other => other,
    }
    .to_string()
}

/// 统一模式 → Codex 审批/沙箱策略。
/// 界面只暴露 build（放开全部权限，等价原 bypass）/ plan（只读规划）两种；
/// ask / accept-edits 等是历史会话存的旧值，保留原语义；未设模式默认 workspace-write。
fn codex_policy(mode: Option<&str>) -> (&'static str, &'static str) {
    match mode.unwrap_or("") {
        "ask" => ("on-request", "read-only"),
        "plan" => ("on-request", "read-only"),
        "build" | "bypass" => ("never", "danger-full-access"),
        _ => ("on-request", "workspace-write"),
    }
}

fn codex_modes() -> Vec<Value> {
    vec![
        json!({ "id": "build", "name": "Build" }),
        json!({ "id": "plan", "name": "Plan" }),
    ]
}

fn build_user_input(text: &str, images: &[PromptImage]) -> Vec<Value> {
    let mut input = Vec::new();
    if !text.is_empty() {
        input.push(json!({ "type": "text", "text": text, "text_elements": [] }));
    }
    let mut saved_paths = Vec::new();
    for img in images {
        if img.mime_type.starts_with("image/") {
            if let Some(path) = prompt_image_path(img) {
                input.push(json!({ "type": "localImage", "path": path }));
                saved_paths.push(path);
            }
        } else {
            // 非图片文件：本机 uri 直接给路径；漫游/分享只带 data 时先落临时文件
            let path = if let Some(uri) = &img.uri {
                file_uri_to_path(uri)
            } else {
                crate::threads::save_attachment_to_temp(img)
            };
            if let Some(path) = path {
                input.push(json!({
                    "type": "text",
                    "text": format!("附件：{}（本地路径：{}）", img.name, path),
                    "text_elements": []
                }));
            }
        }
    }
    if !saved_paths.is_empty() {
        input.push(json!({
            "type": "text",
            "text": format!("用户随消息附带了图片，本地路径：\n{}", saved_paths.join("\n")),
            "text_elements": []
        }));
    }
    input
}

fn prompt_image_path(img: &PromptImage) -> Option<String> {
    if let Some(uri) = &img.uri {
        if let Some(path) = file_uri_to_path(uri) {
            return Some(path);
        }
    }
    save_prompt_image(img)
}

fn save_prompt_image(img: &PromptImage) -> Option<String> {
    use base64::Engine;
    let data = img.data.as_ref()?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data.as_bytes())
        .ok()?;
    let ext = match img.mime_type.as_str() {
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/bmp" => "bmp",
        _ => "png",
    };
    let dir = std::env::temp_dir().join("Nova-codex-images");
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(format!("{}.{ext}", uuid::Uuid::new_v4()));
    std::fs::write(&path, bytes).ok()?;
    Some(path.to_string_lossy().to_string())
}

fn file_uri_to_path(uri: &str) -> Option<String> {
    let raw = uri.strip_prefix("file://")?;
    let decoded = percent_decode(raw);
    #[cfg(windows)]
    {
        Some(decoded.trim_start_matches('/').replace('/', "\\"))
    }
    #[cfg(not(windows))]
    {
        Some(decoded)
    }
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(hex) = std::str::from_utf8(&bytes[i + 1..i + 3]) {
                if let Ok(value) = u8::from_str_radix(hex, 16) {
                    out.push(value);
                    i += 3;
                    continue;
                }
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

fn text_delta(params: &Value) -> String {
    for key in [
        "delta",
        "textDelta",
        "text_delta",
        "summaryDelta",
        "summary_delta",
        "summaryTextDelta",
        "summary_text_delta",
        "text",
    ] {
        if let Some(s) = params[key].as_str().filter(|s| !s.is_empty()) {
            return s.to_string();
        }
    }
    String::new()
}

/// 把 codex 的错误对象（{ message, additionalDetails, codexErrorInfo }）整理成可读文案
fn format_codex_error(err: &Value) -> Option<String> {
    let message = err["message"].as_str()?;
    let mut text = message.to_string();
    if let Some(details) = err["additionalDetails"].as_str() {
        if !details.is_empty() && details != message {
            text.push('\n');
            text.push_str(details);
        }
    }
    Some(text)
}

fn codex_result_content(result: Option<&Value>, error: Option<&Value>) -> Vec<Value> {
    let value = error.or(result);
    let Some(value) = value else {
        return Vec::new();
    };
    if value.is_null() {
        return Vec::new();
    }
    let text = value.as_str().map(String::from).unwrap_or_else(|| {
        serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
    });
    let text = limit_display_text(&text);
    if text.is_empty() {
        Vec::new()
    } else {
        vec![json!({ "type": "content", "content": { "type": "text", "text": text } })]
    }
}

fn limit_display_text(text: &str) -> String {
    let text = strip_omission_notice(text);
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let exceeds_line_limit = lines.len() > TOOL_OUTPUT_MAX_LINES;
    let exceeds_char_limit = text.chars().count() > TOOL_OUTPUT_MAX_CHARS;
    if !exceeds_line_limit && !exceeds_char_limit {
        return text.to_string();
    }

    let tail = if exceeds_line_limit {
        lines[lines.len() - TOOL_OUTPUT_MAX_LINES..].concat()
    } else {
        text.to_string()
    };
    let tail_char_count = tail.chars().count();
    let tail = if tail_char_count > TOOL_OUTPUT_MAX_CHARS {
        let start = tail
            .char_indices()
            .nth(tail_char_count - TOOL_OUTPUT_MAX_CHARS)
            .map(|(index, _)| index)
            .unwrap_or(0);
        &tail[start..]
    } else {
        &tail
    };

    format!(
        "{} {}行或{}字符（取较小者）]\n{}",
        TOOL_OUTPUT_OMISSION_PREFIX, TOOL_OUTPUT_MAX_LINES, TOOL_OUTPUT_MAX_CHARS, tail
    )
}

fn strip_omission_notice(text: &str) -> &str {
    let Some(rest) = text.strip_prefix(TOOL_OUTPUT_OMISSION_PREFIX) else {
        return text;
    };
    rest.split_once('\n').map(|(_, tail)| tail).unwrap_or(text)
}

#[cfg(test)]
mod display_text_tests {
    use super::*;

    fn omission_payload(text: &str) -> &str {
        text.split_once('\n').map(|(_, tail)| tail).unwrap()
    }

    #[test]
    fn limits_tool_output_to_last_500_lines() {
        let input = (0..501)
            .map(|index| format!("line {index}\n"))
            .collect::<String>();

        let output = limit_display_text(&input);
        let payload = omission_payload(&output);

        assert!(output.starts_with(TOOL_OUTPUT_OMISSION_PREFIX));
        assert_eq!(payload.lines().count(), TOOL_OUTPUT_MAX_LINES);
        assert!(!payload.starts_with("line 0\n"));
        assert!(payload.ends_with("line 500\n"));
    }

    #[test]
    fn limits_tool_output_to_last_10000_unicode_characters() {
        let input = format!("开头{}", "界".repeat(TOOL_OUTPUT_MAX_CHARS));

        let output = limit_display_text(&input);
        let payload = omission_payload(&output);

        assert_eq!(payload.chars().count(), TOOL_OUTPUT_MAX_CHARS);
        assert_eq!(payload, "界".repeat(TOOL_OUTPUT_MAX_CHARS));
    }

    #[test]
    fn applies_the_smaller_of_the_line_and_character_limits() {
        let input = (0..501)
            .map(|_| format!("{}\n", "字".repeat(30)))
            .collect::<String>();

        let output = limit_display_text(&input);
        let payload = omission_payload(&output);

        assert_eq!(payload.chars().count(), TOOL_OUTPUT_MAX_CHARS);
        assert!(payload.lines().count() <= TOOL_OUTPUT_MAX_LINES);
        assert!(input.ends_with(payload));
    }
}

fn normalize_tool_status(status: &str) -> String {
    match status {
        "inProgress" => "in_progress",
        "completed" => "completed",
        "failed" | "declined" => "failed",
        _ => "pending",
    }
    .to_string()
}

fn normalize_plan_status(status: &str) -> &'static str {
    match status {
        "inProgress" => "in_progress",
        "completed" => "completed",
        "cancelled" | "interrupted" => "interrupted",
        _ => "pending",
    }
}

fn complete_pending_tools(thread: &mut Thread, except_tool_call_id: Option<&str>) -> Vec<Item> {
    let mut changed = Vec::new();
    for item in &mut thread.items {
        let Item::Tool { call, .. } = item else {
            continue;
        };
        if except_tool_call_id == Some(call.tool_call_id.as_str()) {
            continue;
        }
        if call.status == "pending" || call.status == "in_progress" {
            call.status = "completed".to_string();
            changed.push(item.clone());
        }
    }
    changed
}

fn derive_title(text: &str, has_images: bool) -> String {
    let first_line = text.lines().next().unwrap_or("").trim();
    let title: String = first_line.chars().take(40).collect();
    if title.is_empty() {
        if has_images {
            "[图片]".into()
        } else {
            "新会话".into()
        }
    } else {
        title
    }
}
