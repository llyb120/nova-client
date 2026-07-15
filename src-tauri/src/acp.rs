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
use tokio::time::{sleep, timeout, Duration};

pub const EV_UPDATE: &str = "acp:update";
pub const EV_TURN: &str = "acp:turn";
pub const EV_PERMISSION: &str = "acp:permission";
pub const EV_PERMISSION_RESOLVED: &str = "acp:permission-resolved";
pub const EV_STATUS: &str = "acp:status";
pub const EV_LOG: &str = "acp:log";
pub const EV_THREADS: &str = "threads:changed";
pub const EV_TITLE_GENERATED: &str = "threads:title-generated";
pub const EV_OPTIONS: &str = "acp:options";
pub const EV_COMMANDS: &str = "acp:commands";
pub const EV_NOTIFY_OPEN: &str = "acp:notify-open";

const LOG_CAP: usize = 800;
const TOOL_OUTPUT_LIMIT: usize = 64 * 1024;

/// 连接池的两个「非用户会话」保留键：
/// - SHARED：Devin 等多路复用后端的唯一共享连接（所有线程共用一条连接、一个进程）。
/// - AUX：CodeBuddy 专用的辅助连接，只跑标题生成 / 模型·命令探测这类临时会话，
///   与用户线程的连接完全隔离，避免抢占某个线程连接的「单活跃 session」。
const SHARED_KEY: &str = "__shared__";
const AUX_KEY: &str = "__aux__";
const OPENCODE_BASE_MODEL_META: &str = "nova.ai/opencodeBaseModel";
const OPENCODE_VARIANT_META: &str = "nova.ai/opencodeVariant";

#[cfg(windows)]
const CURSOR_WINDOWS_HIDE_PATCH: &str = include_str!("../cursor-windows-hide.cjs");

#[cfg(windows)]
static CURSOR_WINDOWS_HIDE_PATCH_PATH: std::sync::OnceLock<Result<PathBuf, String>> =
    std::sync::OnceLock::new();

pub struct PendingPermission {
    pub rpc_id: Value,
    pub session_id: String,
    /// 收到该权限请求的连接：respond 时用它路由回正确的进程（多连接下不能再假设「当前连接」）。
    pub conn: Arc<AcpConn>,
}

/// 已挂载到 devin 进程上的 session → 线程路由与已应用的配置
struct Route {
    thread_id: String,
    applied_model: Option<String>,
    applied_mode: Option<String>,
}

fn connection_scoped_model_session_changed(
    kind: &AgentKind,
    active_sid: Option<&str>,
    sid: &str,
) -> bool {
    kind == &AgentKind::OpenCode && active_sid != Some(sid)
}

struct TitleJob {
    thread_id: String,
    fallback_title: String,
    output: String,
}

#[derive(Clone, Debug, Default)]
struct CursorToolDetails {
    tool_name: Option<String>,
    raw_input: Option<Value>,
    raw_output: Option<Value>,
}

pub struct AcpConn {
    /// 该连接在连接池中的键：SHARED / AUX / 或 CodeBuddy 的 thread_id。
    /// 连接关闭回调据此只清理属于自己的会话，不误伤其它并行连接。
    key: String,
    /// 该连接所属 agent 的展示名（Devin / CodeBuddy…）：用于错误提示，避免统一显示成 devin。
    label: &'static str,
    stdin_tx: mpsc::UnboundedSender<String>,
    pending: StdMutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>,
    next_id: AtomicU64,
    pub alive: AtomicBool,
    child: StdMutex<Option<Child>>,
}

impl AcpConn {
    fn send_raw(&self, msg: Value) -> Result<(), String> {
        self.stdin_tx
            .send(msg.to_string())
            .map_err(|_| format!("{} 进程不可写（已退出？）", self.label))
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
                .map_err(|_| format!("{} 连接已断开", self.label))
                .and_then(|r| r)
        };
        match wait {
            Some(d) => timeout(d, recv)
                .await
                .map_err(|_| format!("{method} 等待超时"))?,
            None => recv.await,
        }
    }

    pub fn notify(&self, method: &str, params: Value) {
        let _ = self.send_raw(json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params
        }));
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
            // 先杀整棵进程树：ACP agent 常经 cmd 垫片启动（cmd→node），且 agent 执行工具调用时
            // 会 spawn shell（powershell/bash）。仅 start_kill 只终止直接子进程，node 与其拉起的
            // shell 会变孤儿堆积，塞满 shell 通道。
            if let Some(pid) = child.id() {
                kill_process_tree(pid);
            }
            let _ = child.start_kill();
        }
    }
}

/// Windows：杀掉以 `pid` 为根的整棵进程树（含 agent 经 cmd 垫片拉起的 node、
/// 以及 agent 执行工具调用时 spawn 的 shell 等所有后代）。
///
/// 用 Win32 原生 API（Toolhelp 快照建父子表 + TerminateProcess，子进程先杀、根最后杀），
/// **不再外挂 taskkill.exe 子进程**。原因：GUI（windows 子系统）进程在「正在退出」时再去启动
/// 控制台子进程（taskkill 是 CUI 程序）会触发控制台初始化失败，弹出
/// 「taskkill.exe - 应用程序错误 0xc0000142」。原生调用无需拉起任何子进程，退出期也稳定，
/// 且同步完成，保证「退出/升级/取消」时清理干净、不残留孤儿。
#[cfg(windows)]
pub(crate) fn kill_process_tree(pid: u32) {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32First, Process32Next, PROCESSENTRY32, TH32CS_SNAPPROCESS,
    };

    unsafe {
        // 1) 快照全部进程，建 (pid, ppid) 表。失败则至少杀掉根进程。
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            terminate_pid(pid);
            return;
        }
        let mut pairs: Vec<(u32, u32)> = Vec::new();
        let mut entry: PROCESSENTRY32 = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32>() as u32;
        if Process32First(snapshot, &mut entry) != 0 {
            loop {
                pairs.push((entry.th32ProcessID, entry.th32ParentProcessID));
                if Process32Next(snapshot, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snapshot);

        // 2) 广度优先收集以 pid 为根的整棵树。
        let mut tree = vec![pid];
        let mut i = 0;
        while i < tree.len() {
            let cur = tree[i];
            for &(p, pp) in &pairs {
                if pp == cur && p != 0 && !tree.contains(&p) {
                    tree.push(p);
                }
            }
            i += 1;
        }

        // 3) 子进程先杀、根最后杀，尽量避免中间态再拉起新子进程。
        for &p in tree.iter().rev() {
            terminate_pid(p);
        }
        // TerminateProcess 只是发终止请求。退出/升级路径会紧接着拉起新进程，
        // 这里同步等一小段时间，避免旧 agent/shell 仍处于 terminating 状态时被误认为残留。
        for &p in &tree {
            wait_pid_exit(p, 800);
        }
    }
}

/// 强制结束单个进程（找不到/已退出则忽略）。
#[cfg(windows)]
unsafe fn terminate_pid(pid: u32) {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};
    let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
    if !handle.is_null() {
        TerminateProcess(handle, 1);
        CloseHandle(handle);
    }
}

#[cfg(windows)]
fn wait_pid_exit(pid: u32, timeout_ms: u32) {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, WaitForSingleObject};
    const SYNCHRONIZE: u32 = 0x0010_0000;
    let handle = unsafe { OpenProcess(SYNCHRONIZE, 0, pid) };
    if !handle.is_null() {
        unsafe {
            let _ = WaitForSingleObject(handle, timeout_ms);
            CloseHandle(handle);
        }
    }
}

#[cfg(not(windows))]
pub(crate) fn kill_process_tree(pid: u32) {
    // spawn 时 process_group(0)：整组 SIGKILL（根 pid == pgid）
    #[cfg(unix)]
    unsafe {
        let _ = libc::kill(-(pid as i32), libc::SIGKILL);
    }

    fn collect_children(parent: u32, out: &mut Vec<u32>) {
        let Ok(output) = std::process::Command::new("pgrep")
            .args(["-P", &parent.to_string()])
            .output()
        else {
            return;
        };
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            if let Ok(child) = line.trim().parse::<u32>() {
                if child != 0 && !out.contains(&child) {
                    collect_children(child, out);
                    out.push(child);
                }
            }
        }
    }

    let mut tree = Vec::new();
    collect_children(pid, &mut tree);
    tree.push(pid);
    for &p in tree.iter().rev() {
        #[cfg(unix)]
        unsafe {
            let _ = libc::kill(p as i32, libc::SIGKILL);
        }
        #[cfg(not(unix))]
        let _ = p;
    }
}

/// 把 agent 连接进程挂进全局 Job 对象（KILL_ON_JOB_CLOSE）作兜底：
/// Nova 无论正常退出、崩溃还是被任务管理器强杀，内核都会随句柄关闭自动终结整棵
/// agent 进程树——包括经 cmd 垫片间接拉起、垫片先退导致 taskkill/快照法漏杀的后代
/// （此前的孤儿 codebuddy/node 进程即由此产生）。主动 kill 路径仍走 kill_process_tree，
/// Job 只兜「应用整个生命周期结束」这一层，且各连接互不影响。
#[cfg(windows)]
pub(crate) fn assign_to_agent_job(child: &tokio::process::Child) {
    use std::sync::OnceLock;
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    // HANDLE 本质是指针，不跨进程传递，仅进程内单例持有，Send/Sync 安全
    struct JobHandle(isize);
    unsafe impl Send for JobHandle {}
    unsafe impl Sync for JobHandle {}

    static JOB: OnceLock<Option<JobHandle>> = OnceLock::new();
    let job = JOB.get_or_init(|| unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            return None;
        }
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let ok = SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const std::ffi::c_void,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        );
        if ok == 0 {
            // 配置失败就不挂 Job（句柄随进程退出回收），行为退回原状
            return None;
        }
        Some(JobHandle(job as isize))
    });
    if let (Some(job), Some(handle)) = (job.as_ref(), child.raw_handle()) {
        unsafe {
            AssignProcessToJobObject(job.0 as _, handle as _);
        }
    }
}

#[cfg(not(windows))]
pub(crate) fn assign_to_agent_job(_child: &tokio::process::Child) {}

pub struct AcpManager {
    pub app: AppHandle,
    /// 该实例对应的 agent 类型。Devin 与 CodeBuddy 都走标准 ACP 协议，
    /// 仅启动命令、事件标签、权限 key 前缀不同，故复用同一套实现、各起一个实例。
    pub kind: AgentKind,
    /// 额度租借实例使用的独立凭证环境；普通全局实例为空。
    launch_env: HashMap<String, String>,
    /// 额度租借实例的权限请求作用域，避免不同进程的递增 RPC id 发生碰撞。
    permission_scope: String,
    /// 连接池：conn_key → 该键的连接槽（TokioMutex<Option<conn>>，语义同「旧单连接」但按键分裂）。
    /// - Devin：只用 SHARED 一个键（多路复用，一条连接跑所有 session）。
    /// - CodeBuddy：每个用户线程一个键（thread_id）独占一条连接（一个进程），从而真正并行；
    ///   另有 AUX 键专跑标题/探测。不同键各自的槽互不阻塞，可并发建连接。
    slots: StdMutex<HashMap<String, Arc<TokioMutex<Option<Arc<AcpConn>>>>>>,
    /// 存活连接计数：spawn 成功 +1、连接关闭 -1；用于 connected() 与断连广播（归零才广播）。
    alive_conns: AtomicU64,
    routes: StdMutex<HashMap<String, Route>>,
    /// 正在 session/load 回放、需要抑制 update 的会话
    loading_sessions: StdMutex<HashSet<String>>,
    running_threads: StdMutex<HashSet<String>>,
    /// 每线程取消代次。CodeBuddy/Cursor 的 run_prompt 可能在串行闸门上等待；cancel
    /// 递增代次后，仍在等待的旧轮次拿到闸门时必须退出，不能再次 set_running(true)。
    cancel_epochs: StdMutex<HashMap<String, u64>>,
    /// 轮次开始时间，用于结束时计算耗时
    turn_started: StdMutex<HashMap<String, std::time::Instant>>,
    /// 诊断：session/prompt 发出时刻 → 用于测量「首响应延迟」(session_id)
    prompt_sent_at: StdMutex<HashMap<String, std::time::Instant>>,
    pending_permissions: StdMutex<HashMap<String, PendingPermission>>,
    /// 预热好的空 session：cwd → sessionId（仅 Devin，走 SHARED 连接）
    prewarmed: StdMutex<HashMap<String, String>>,
    prewarming: StdMutex<HashSet<String>>,
    /// 串行化同一线程上的 session 建立操作
    thread_locks: StdMutex<HashMap<String, Arc<TokioMutex<()>>>>,
    /// 连接级串行闸门（按 conn_key 分裂）：CodeBuddy 的 ACP server 无法在单条 stdio 连接上并发
    /// 处理请求（并发 prompt 会把响应串台、prompt 运行中并发 session/new 会卡死整轮）；OpenCode
    /// 的模型选择则实际落在共享连接上，不串行会让不同 session 的模型互相覆盖。故对这两类后端的
    /// 「同一条连接」会话级操作用同一把闸门排队；不同连接（不同 thread）仍可并行。
    /// Devin 等支持多路复用，返回 None、不进闸门。
    serial_gates: StdMutex<HashMap<String, Arc<TokioMutex<()>>>>,
    /// CodeBuddy：记录每条连接当前活跃的 sessionId，prompt 前按需 session/load 重新激活。
    /// OpenCode：记录共享连接上最近应用模型的 sessionId，切换 session 时强制重下发线程模型。
    active_session: StdMutex<HashMap<String, String>>,
    /// devin 返回的可用模型/模式（来自 session/new 响应）
    model_options: StdMutex<Option<Value>>,
    /// Cursor 专用：经 `cursor-agent models` CLI 拉到的完整模型目录（含 thinking/effort/fast
    /// 变体）。ACP 只返回每个模型的默认变体，CLI 目录用于补齐全部可选组合。None = 尚未拉取。
    cursor_catalog: StdMutex<Option<Vec<(String, String)>>>,
    /// Cursor ACP 的标准模型列表。它包含 CLI 目录偶尔缺少的模型，合并时作为补充来源。
    cursor_acp_models: StdMutex<Vec<Value>>,
    /// Cursor ACP 把 MCP 调用压扁为 `MCP: tool`。按 session/toolCallId 缓存从 Cursor
    /// 当前会话存储恢复出的真实工具名与参数，供后续 in_progress/completed 更新复用。
    cursor_tool_details: StdMutex<HashMap<String, CursorToolDetails>>,
    /// Cursor 自带的 Node 运行时，用其只读 SQLite API 读取同一 ACP 会话库。
    cursor_node_path: StdMutex<Option<PathBuf>>,
    /// Cursor 目录拉取进行中标记（防并发重复拉取）
    cursor_catalog_fetching: StdMutex<bool>,
    /// 本进程内是否已对磁盘缓存做过一次后台重拉（避免每次 get 都打 agent）
    model_options_revalidated: AtomicBool,
    /// 后台重拉进行中
    model_options_refreshing: AtomicBool,
    available_commands: StdMutex<Option<Value>>,
    title_jobs: StdMutex<HashMap<String, TitleJob>>,
    logs: StdMutex<VecDeque<String>>,
    pub agent_info: StdMutex<Option<Value>>,
    /// CodeBuddy 专用：每条连接进程的启动工作目录（conn_key → cwd）。CodeBuddy 的 ACP server 不认
    /// session/new 的 cwd，只按进程工作目录执行命令，故按会话项目目录启动进程；目录变化时需重启连接。
    conn_cwd: StdMutex<HashMap<String, String>>,
    /// CodeBuddy / ClaudeCode：各连接最近一次活跃（建连/复用/轮次结束）时刻，供空闲回收器判定。
    /// CodeBuddy 每线程一条连接=一个 node 进程，数字员工心跳每轮都开新线程，用完的进程
    /// 若不回收会随运行时间无限堆积（表现为内存持续增长）。ClaudeCode 则是共享连接下的
    /// 适配器按 session 堆积 claude 子进程（连带 LSP/MCP），同样需要按空闲回收。
    conn_last_active: StdMutex<HashMap<String, std::time::Instant>>,
    /// ClaudeCode 专用：共享连接累计创建/恢复过的 session 数（≈ 适配器里驻留的 claude
    /// 子进程套数）。达到阈值后在下一段安静期整树回收，防止长时间使用下内存无限堆积。
    conn_sessions: StdMutex<HashMap<String, u64>>,
}

/// CodeBuddy 空闲连接的回收超时：员工后台会话一轮跑完基本不会立刻复用，短超时快速回收；
/// 用户交互会话给足续聊窗口（回收后下次发送自动重启进程并经 session/load 恢复，仅慢几秒）。
const IDLE_KILL_EMPLOYEE_MS: u128 = 2 * 60 * 1000;
const IDLE_KILL_USER_MS: u128 = 15 * 60 * 1000;
/// AUX（标题生成/模型探测）连接的空闲回收超时
const IDLE_KILL_AUX_MS: u128 = 5 * 60 * 1000;
/// ClaudeCode 共享连接的空闲回收超时。claude-code-acp 适配器会为每个 session 拉起一个
/// 独立的 claude 进程，连同它自带的 LSP（如 gopls）与整套 MCP 服务器，单套常驻数百 MB，
/// 且 session 用完进程也不退。整条连接空闲达到该时长即整树回收；下次发消息自动重启进程
/// 并经 session/load 恢复上下文，仅首条消息慢几秒。
const IDLE_KILL_SHARED_MS: u128 = 10 * 60 * 1000;
/// ClaudeCode 共享连接的「会话数回收阈值」：连接累计创建/恢复过这么多 session（≈ 适配器
/// 里驻留的 claude 子进程套数）后，不必等满空闲超时——只要出现一段短暂安静
/// （IDLE_KILL_EMPLOYEE_MS）就整树回收，防止长时间连续使用下内存无限堆积。
const SHARED_SESSIONS_RECYCLE_AT: u64 = 6;

impl AcpManager {
    pub fn new(app: AppHandle, kind: AgentKind) -> Arc<Self> {
        Self::new_with_env(app, kind, HashMap::new(), String::new())
    }

    pub fn new_with_env(
        app: AppHandle,
        kind: AgentKind,
        launch_env: HashMap<String, String>,
        permission_scope: String,
    ) -> Arc<Self> {
        let mgr = Arc::new(AcpManager {
            app,
            kind,
            launch_env,
            permission_scope,
            slots: StdMutex::new(HashMap::new()),
            alive_conns: AtomicU64::new(0),
            routes: StdMutex::new(HashMap::new()),
            loading_sessions: StdMutex::new(HashSet::new()),
            running_threads: StdMutex::new(HashSet::new()),
            cancel_epochs: StdMutex::new(HashMap::new()),
            turn_started: StdMutex::new(HashMap::new()),
            prompt_sent_at: StdMutex::new(HashMap::new()),
            pending_permissions: StdMutex::new(HashMap::new()),
            prewarmed: StdMutex::new(HashMap::new()),
            prewarming: StdMutex::new(HashSet::new()),
            thread_locks: StdMutex::new(HashMap::new()),
            serial_gates: StdMutex::new(HashMap::new()),
            active_session: StdMutex::new(HashMap::new()),
            model_options: StdMutex::new(None),
            cursor_catalog: StdMutex::new(None),
            cursor_acp_models: StdMutex::new(Vec::new()),
            cursor_tool_details: StdMutex::new(HashMap::new()),
            cursor_node_path: StdMutex::new(None),
            cursor_catalog_fetching: StdMutex::new(false),
            model_options_revalidated: AtomicBool::new(false),
            model_options_refreshing: AtomicBool::new(false),
            available_commands: StdMutex::new(None),
            title_jobs: StdMutex::new(HashMap::new()),
            logs: StdMutex::new(VecDeque::new()),
            agent_info: StdMutex::new(None),
            conn_cwd: StdMutex::new(HashMap::new()),
            conn_last_active: StdMutex::new(HashMap::new()),
            conn_sessions: StdMutex::new(HashMap::new()),
        });
        // 空闲回收器：
        // - CodeBuddy：每线程一条连接=一个 node 进程（数字员工心跳每轮开新线程，保留下来的
        //   会话此前会让进程永久驻留），按键逐条回收。
        // - ClaudeCode：共享一条连接，但 claude-code-acp 适配器会为每个 session 常驻一个
        //   claude 子进程（自带 gopls 等 LSP + 整套 MCP 服务器，单套数百 MB），只能整树回收。
        // - OpenCode：共享一条连接（bun 运行时常驻上百 MB），空闲整树回收，
        //   下次发消息自动重连并经 session/load 恢复。
        // - Devin 不回收：进程本身只有几十 MB，且预热 session、剩余额度查询都依赖常驻连接。
        // - Cursor 不回收：cursor-agent 的 session/load 有已知缺陷（恢复会失败），
        //   杀进程等于丢上下文，只能常驻。
        if matches!(
            mgr.kind,
            AgentKind::CodeBuddy | AgentKind::ClaudeCode | AgentKind::OpenCode
        ) {
            let weak = Arc::downgrade(&mgr);
            tauri::async_runtime::spawn(async move {
                loop {
                    sleep(Duration::from_secs(30)).await;
                    let Some(m) = weak.upgrade() else { break };
                    m.reap_idle_conns().await;
                }
            });
        }
        mgr
    }

    pub fn is_running(&self, thread_id: &str) -> bool {
        self.running_threads.lock().unwrap().contains(thread_id)
    }

    fn cancel_epoch(&self, thread_id: &str) -> u64 {
        self.cancel_epochs
            .lock()
            .unwrap()
            .get(thread_id)
            .copied()
            .unwrap_or(0)
    }

    fn bump_cancel_epoch(&self, thread_id: &str) {
        let mut epochs = self.cancel_epochs.lock().unwrap();
        let epoch = epochs.entry(thread_id.to_string()).or_insert(0);
        *epoch = epoch.wrapping_add(1);
    }

    /// ACP session 不挂 Nova MCP；数字员工工具统一走 CLI。
    fn mcp_servers_for_thread(&self, _thread_id: Option<&str>) -> Value {
        json!([])
    }

    /// 用户线程对应的连接键：CodeBuddy 每线程独占一条连接以实现并行；Cursor 的完整模型
    /// 变体只能在进程启动时通过 `--model` 生效，也必须按线程独占连接；其它后端共用 SHARED。
    fn conn_key_for_thread(&self, thread_id: &str) -> String {
        match self.kind {
            AgentKind::CodeBuddy | AgentKind::Cursor => thread_id.to_string(),
            _ => SHARED_KEY.to_string(),
        }
    }

    /// 辅助操作（标题生成 / 模型·命令探测）所用连接键：CodeBuddy 用独立 AUX 连接，
    /// 与用户线程连接隔离；其它后端复用 SHARED。
    fn aux_key(&self) -> String {
        match self.kind {
            AgentKind::CodeBuddy | AgentKind::Cursor => AUX_KEY.to_string(),
            _ => SHARED_KEY.to_string(),
        }
    }

    /// 取（或创建）某个键的连接槽。槽 = TokioMutex<Option<conn>>，语义等同旧的单连接字段，
    /// 只是按键分裂：不同键各自的槽互不阻塞，可并发建连接/跑会话。
    fn slot(&self, key: &str) -> Arc<TokioMutex<Option<Arc<AcpConn>>>> {
        self.slots
            .lock()
            .unwrap()
            .entry(key.to_string())
            .or_insert_with(|| Arc::new(TokioMutex::new(None)))
            .clone()
    }

    fn slot_opt(&self, key: &str) -> Option<Arc<TokioMutex<Option<Arc<AcpConn>>>>> {
        self.slots.lock().unwrap().get(key).cloned()
    }

    pub fn get_logs(&self) -> Vec<String> {
        self.logs.lock().unwrap().iter().cloned().collect()
    }

    pub fn get_model_options(&self) -> Option<Value> {
        self.model_options.lock().unwrap().clone()
    }

    /// 启动时从磁盘缓存灌入内存（不广播；前端经 get_model_options 立刻拿到）。
    pub fn seed_model_options(&self, v: Value) {
        *self.model_options.lock().unwrap() = Some(v);
    }

    fn persist_model_options(&self, v: &Value) {
        model_cache::save(&nova_data_dir(&self.app), self.kind.as_str(), v);
    }

    /// 向 agent 重拉最新列表；旧缓存继续服务前端，拉到后再覆盖，避免首屏空窗。
    pub async fn refresh_model_options(self: &Arc<Self>) -> Result<Value, String> {
        match self.fetch_model_options_from_agent().await {
            Ok(v) => {
                self.model_options_revalidated.store(true, Ordering::SeqCst);
                Ok(v)
            }
            Err(e) => Err(e),
        }
    }

    /// 已有缓存时后台重拉一次（本进程每后端最多一次，避免反复打 agent）。
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

    /// 给前端 IPC：有缓存立刻返回并后台刷新；无缓存则加入/等待正在进行的刷新。
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
            sleep(Duration::from_millis(100)).await;
        }
        if let Some(v) = self.get_model_options() {
            return Ok(v);
        }
        self.fetch_model_options().await
    }

    /// 统一模式 → 该后端真实模式 id。
    /// 界面只暴露两种模式：build（放开全部权限执行，等价原 Bypass Permissions）与
    /// plan（只规划不执行）。旧数据里的 bypass 视同 build；其余值（历史会话存的
    /// 后端原生模式，如 accept-edits / ask）原样透传，交由可用列表校验兜底。
    fn backend_mode_id(&self, mode: &str) -> String {
        match mode {
            "build" | "bypass" => match self.kind {
                AgentKind::CodeBuddy | AgentKind::ClaudeCode => "bypassPermissions".into(),
                AgentKind::Cursor => "agent".into(),
                AgentKind::OpenCode => "build".into(),
                // Devin（Cognition ACP）：bypass 即全自动
                _ => "bypass".into(),
            },
            "plan" => "plan".into(),
            other => other.into(),
        }
    }

    /// 该后端在设置里配置的代理地址（空 = 不代理）
    fn proxy_of<'a>(&self, settings: &'a Settings) -> &'a str {
        match self.kind {
            AgentKind::CodeBuddy => &settings.codebuddy_proxy,
            AgentKind::ClaudeCode => &settings.claudecode_proxy,
            AgentKind::Cursor => &settings.cursor_proxy,
            AgentKind::OpenCode => &settings.opencode_proxy,
            _ => &settings.devin_proxy,
        }
    }

    /// 为一条 cursor-agent 连接准备隔离的配置目录。
    ///
    /// Nova 的 Cursor 用户线程各自运行独立进程，但 Cursor 默认让所有进程共享
    /// `~/.cursor/cli-config.json` 与同目录的 `acp-config.json`。并发使用不同模型时，进程会
    /// 互相覆盖 selectedModel / effort / fast；新进程也可能恰好读到另一线程刚写入的参数，
    /// 于是选择 xhigh 却实际发出 high/medium，或意外带上 fast。
    ///
    /// Cursor 官方支持用 `CURSOR_CONFIG_DIR` 改写配置目录。这里从全局 cli-config 复制登录、
    /// 网络等设置到本进程专属目录，再写入本线程模型参数；不复制全局 acp-config，避免其中
    /// 残留的 selectedModelVariantId 再次覆盖 `--model`。因此：
    /// 1. 始终关闭 Max Mode 计费开关（模型名/effort 里的 Max 不是该开关）；
    /// 2. 若本次以明确 CLI 模型启动，把 `selectedModel` / `modelParameters` 写成与扁平 id
    ///    一致的参数（effort 与 fast 都严格跟随所选模型变体）；
    /// 3. 每次进程启动都从全局配置重新复制，及时带上登录态/网络设置的变化。
    /// 只做本地文件操作，不发出任何模型请求。失败时返回 None，让 Cursor 回退到默认目录。
    fn prepare_cursor_config_dir(
        &self,
        conn_key: &str,
        startup_model: Option<&str>,
    ) -> Option<std::path::PathBuf> {
        let Some(home) = std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(std::path::PathBuf::from)
        else {
            return None;
        };
        let source = home.join(".cursor").join("cli-config.json");
        let Ok(raw) = std::fs::read_to_string(&source) else {
            return None;
        };
        let Ok(mut cfg) = serde_json::from_str::<Value>(&raw) else {
            return None;
        };
        let mut notes: Vec<String> = Vec::new();
        if clear_cursor_max_mode(&mut cfg) {
            notes.push("关闭 Max Mode".into());
        }
        if let Some(model) = startup_model.filter(|m| is_cursor_cli_model_id(m)) {
            if sync_cursor_cli_model_selection(&mut cfg, model) {
                notes.push(format!("同步模型参数 → {model}"));
            }
        }
        // conn_key 只会是 UUID / __aux__ / __shared__，直接作为目录名可保持同一连接重启时复用。
        // 再按进程 id 分层，避免上次异常退出留下的 acp-config 污染本次运行。
        let config_dir = nova_data_dir(&self.app)
            .join("cursor-config")
            .join(std::process::id().to_string())
            .join(conn_key);
        if let Err(e) = std::fs::create_dir_all(&config_dir) {
            self.push_log(format!("[nova] 创建 Cursor 隔离配置目录失败：{e}"));
            return None;
        }
        if let Err(e) =
            crate::agent_config::sync_cursor_config_dir(&nova_data_dir(&self.app), &config_dir)
        {
            self.push_log(format!("[nova] 同步 Cursor 全局 Rule 失败：{e}"));
        }
        // 同一线程切换模型后会复用这个目录；Cursor ACP 持久化的旧变体优先级可能高于
        // `--model`，所以每次重启都清掉，仅保留下面重新生成的 cli-config。
        let _ = std::fs::remove_file(config_dir.join("acp-config.json"));
        let path = config_dir.join("cli-config.json");
        match write_cursor_cli_config(&path, &cfg) {
            Ok(()) => {
                self.push_log(format!(
                    "[nova] 已准备 Cursor 隔离配置 {}{}",
                    config_dir.display(),
                    if notes.is_empty() {
                        String::new()
                    } else {
                        format!("（{}）", notes.join("；"))
                    }
                ));
                Some(config_dir)
            }
            Err(e) => {
                self.push_log(format!("[nova] 写入 Cursor 隔离配置失败：{e}"));
                None
            }
        }
    }

    /// 最近一次捕获到的可用模式 id 列表（modes.availableModes）。None = 尚未捕获，不做校验。
    fn known_mode_ids(&self) -> Option<Vec<String>> {
        let guard = self.model_options.lock().unwrap();
        let modes = guard
            .as_ref()?
            .get("modes")?
            .get("availableModes")?
            .as_array()?;
        let ids: Vec<String> = modes
            .iter()
            .filter_map(|m| m.get("id").and_then(|v| v.as_str()).map(str::to_string))
            .collect();
        (!ids.is_empty()).then_some(ids)
    }

    /// 最近一次捕获到的可选模型 value 列表（configOptions 里 id=="model" 的 options）。
    /// 返回 None 表示尚未捕获到模型列表，调用方应跳过校验、按原样下发。
    fn known_model_values(&self) -> Option<Vec<String>> {
        let guard = self.model_options.lock().unwrap();
        let cfg = guard.as_ref()?.get("configOptions")?.as_array()?;
        let model = cfg
            .iter()
            .find(|o| o.get("id").and_then(|v| v.as_str()) == Some("model"))?;
        let options = model.get("options")?.as_array()?;
        let mut values: Vec<String> = options
            .iter()
            .filter_map(|o| o.get("value").and_then(|v| v.as_str()).map(str::to_string))
            .collect();
        if self.kind == AgentKind::Cursor {
            values.extend(
                self.cursor_acp_models
                    .lock()
                    .unwrap()
                    .iter()
                    .filter_map(|o| o.get("value").and_then(|v| v.as_str()).map(str::to_string)),
            );
        }
        (!values.is_empty()).then_some(values)
    }

    fn cursor_startup_model_for_thread(&self, thread_id: &str) -> Option<String> {
        if self.kind != AgentKind::Cursor {
            return None;
        }
        let state = self.app.state::<AppState>();
        let store = state.store.lock().unwrap();
        store
            .get(thread_id)
            .and_then(|thread| thread.model.as_deref())
            .filter(|model| is_cursor_cli_model_id(model))
            .map(str::to_string)
    }

    fn cursor_session_store_path(&self, thread_id: &str, session_id: &str) -> Option<PathBuf> {
        if self.kind != AgentKind::Cursor || session_id.is_empty() {
            return None;
        }
        let config_dir = self
            .launch_env
            .get("CURSOR_CONFIG_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                nova_data_dir(&self.app)
                    .join("cursor-config")
                    .join(std::process::id().to_string())
                    .join(thread_id)
            });
        Some(
            config_dir
                .join("acp-sessions")
                .join(session_id)
                .join("store.db"),
        )
    }

    fn enrich_cursor_tool_update(
        &self,
        thread_id: &str,
        session_id: &str,
        update: &Value,
    ) -> Value {
        let tool_call_id = update["toolCallId"].as_str().unwrap_or_default();
        if tool_call_id.is_empty() {
            return update.clone();
        }
        let cache_key = format!("{session_id}\n{tool_call_id}");
        let generic_mcp = is_generic_cursor_mcp_title(update["title"].as_str().unwrap_or_default());
        let terminal = matches!(update["status"].as_str(), Some("completed" | "failed"));
        let mut details = {
            let mut cache = self.cursor_tool_details.lock().unwrap();
            if !generic_mcp && !cache.contains_key(&cache_key) {
                return update.clone();
            }
            cache.remove(&cache_key).unwrap_or_default()
        };
        if details.tool_name.is_none() || (terminal && details.raw_output.is_none()) {
            let node_path = self.cursor_node_path.lock().unwrap().clone();
            if let (Some(node_path), Some(store_path)) = (
                node_path,
                self.cursor_session_store_path(thread_id, session_id),
            ) {
                if let Some(found) = read_cursor_tool_details(&node_path, &store_path, tool_call_id)
                {
                    details.merge(found);
                }
            }
        }

        let mut enriched = update.clone();
        apply_cursor_tool_details(&mut enriched, &details, terminal);
        if !terminal {
            self.cursor_tool_details
                .lock()
                .unwrap()
                .insert(cache_key, details);
        }
        enriched
    }

    /// 从缓存的 model_options 里查找指定 id 的 config option。
    fn cached_config_option(&self, id: &str) -> Option<Value> {
        let guard = self.model_options.lock().unwrap();
        let cfg = guard
            .as_ref()?
            .get("configOptions")
            .and_then(|v| v.as_array())?;
        cfg.iter()
            .find(|o| o.get("id").and_then(|v| v.as_str()) == Some(id))
            .cloned()
    }

    /// 某个 config option 的 options 列表里是否包含指定 value。
    fn config_option_has_value(option: &Value, value: &str) -> bool {
        option
            .get("options")
            .and_then(|v| v.as_array())
            .map(|opts| {
                opts.iter()
                    .any(|o| o.get("value").and_then(|v| v.as_str()) == Some(value))
            })
            .unwrap_or(false)
    }

    /// 对 Cursor ACP 使用参数化模型选择器：把 CLI 短 id 拆成基座模型 + fast 开关，
    /// 通过 `session/set_config_option` 分别设置。只有当前端 model_options 缓存里同时
    /// 存在 `model` 和 `fast` 两个 config option 时才走这条路径；否则返回 None，
    /// 让调用方回退到原来的 set_model / set_config_option。
    /// 返回 Some(true) 表示设置成功，Some(false) 表示参数化路径存在但执行失败，
    /// None 表示当前不支持参数化设置。
    async fn apply_cursor_parameterized_model(
        &self,
        conn: &Arc<AcpConn>,
        sid: &str,
        model: &str,
    ) -> Option<bool> {
        if self.kind != AgentKind::Cursor {
            return None;
        }
        let (base, _effort, fast) = cursor_variant_dims(model);
        let fast_str = if fast { "true" } else { "false" };

        let model_opt = self.cached_config_option("model")?;
        let fast_opt = self.cached_config_option("fast")?;

        let model_config_id = model_opt.get("id").and_then(|v| v.as_str())?;
        let fast_config_id = fast_opt.get("id").and_then(|v| v.as_str())?;

        if !Self::config_option_has_value(&model_opt, &base)
            || !Self::config_option_has_value(&fast_opt, fast_str)
        {
            return None;
        }

        let set_model = conn
            .request(
                "session/set_config_option",
                json!({
                    "sessionId": sid,
                    "configId": model_config_id,
                    "value": base
                }),
                Some(Duration::from_secs(30)),
            )
            .await;
        if let Err(e) = set_model {
            self.push_log(format!("[nova] Cursor 设置模型基座失败：{e}"));
            return Some(false);
        }

        let set_fast = conn
            .request(
                "session/set_config_option",
                json!({
                    "sessionId": sid,
                    "configId": fast_config_id,
                    "value": fast_str
                }),
                Some(Duration::from_secs(30)),
            )
            .await;
        match set_fast {
            Ok(_) => {
                if let Some(route) = self.routes.lock().unwrap().get_mut(sid) {
                    route.applied_model = Some(model.to_string());
                }
                self.push_log(format!(
                    "[nova] Cursor 已切换到 {model}（基座={base} fast={fast_str}）"
                ));
                Some(true)
            }
            Err(e) => {
                self.push_log(format!("[nova] Cursor 设置 fast 开关失败：{e}"));
                Some(false)
            }
        }
    }

    pub fn get_commands(&self) -> Option<Value> {
        self.available_commands.lock().unwrap().clone()
    }

    pub async fn fetch_commands(self: &Arc<Self>) -> Result<Value, String> {
        if let Some(v) = self.get_commands() {
            return Ok(v);
        }
        // CodeBuddy：探测斜杠命令也要开 session/new，走独立 AUX 连接，不占用任何用户线程连接
        let aux = self.aux_key();
        let _gate = self.serial_gate(&aux).await;
        if let Some(v) = self.get_commands() {
            return Ok(v);
        }
        let cwd = std::env::temp_dir().join("Nova-command-options");
        std::fs::create_dir_all(&cwd)
            .map_err(|e| format!("创建 {} 命令探测目录失败：{e}", self.kind.label()))?;
        let conn = self.ensure_conn_for(&aux, None).await?;
        let resp = conn
            .request(
                "session/new",
                json!({ "cwd": cwd.to_string_lossy(), "mcpServers": [] }),
                Some(Duration::from_secs(180)),
            )
            .await
            .map_err(|e| format!("拉取 {} 斜杠命令失败：{e}", self.kind.label()))?;
        self.capture_options(&resp);
        self.note_session_spawned(&aux);
        if let Some(sid) = resp["sessionId"].as_str() {
            self.mark_active_session(&aux, sid);
        }
        for _ in 0..40 {
            if let Some(v) = self.get_commands() {
                return Ok(v);
            }
            sleep(Duration::from_millis(100)).await;
        }
        Ok(json!([]))
    }

    pub async fn fetch_model_options(self: &Arc<Self>) -> Result<Value, String> {
        // Cursor：完整 thinking/effort/fast 变体来自 CLI 目录。若缓存里还只有 ACP 默认变体，
        // 继续等待目录就绪，避免前端首屏只能选默认档。
        if let Some(v) = self.get_model_options() {
            if self.kind != AgentKind::Cursor || self.cursor_catalog.lock().unwrap().is_some() {
                return Ok(v);
            }
            self.spawn_cursor_catalog_fetch();
            for _ in 0..150 {
                if self.cursor_catalog.lock().unwrap().is_some() {
                    break;
                }
                sleep(Duration::from_millis(100)).await;
            }
            return Ok(self.get_model_options().unwrap_or(v));
        }
        self.fetch_model_options_from_agent().await
    }

    /// 无视内存缓存，向 agent 开探测 session 拉最新模型列表（供启动后台刷新）。
    async fn fetch_model_options_from_agent(self: &Arc<Self>) -> Result<Value, String> {
        // CodeBuddy：探测模型列表也要开 session/new，走独立 AUX 连接，不占用任何用户线程连接
        let aux = self.aux_key();
        let _gate = self.serial_gate(&aux).await;
        // 拿闸门期间可能已被别的路径填好缓存，复查一次（非强制刷新场景）
        // 强制刷新仍继续往下打 agent，用新结果覆盖。
        let cwd = std::env::temp_dir().join("Nova-model-options");
        std::fs::create_dir_all(&cwd)
            .map_err(|e| format!("创建 {} 模型探测目录失败：{e}", self.kind.label()))?;
        let conn = self.ensure_conn_for(&aux, None).await?;
        let mut resp = conn
            .request(
                "session/new",
                json!({ "cwd": cwd.to_string_lossy(), "mcpServers": [] }),
                Some(Duration::from_secs(180)),
            )
            .await
            .map_err(|e| format!("拉取 {} 模型列表失败：{e}", self.kind.label()))?;
        if self.kind == AgentKind::OpenCode {
            resp = self.expand_opencode_model_variants(&conn, resp).await;
        }
        self.capture_options(&resp);
        self.note_session_spawned(&aux);
        if let Some(sid) = resp["sessionId"].as_str() {
            self.mark_active_session(&aux, sid);
        }
        if self.kind == AgentKind::Cursor {
            // capture_options 已后台拉目录；这里最多等 15s，让首屏拿到完整变体列表。
            for _ in 0..150 {
                if self.cursor_catalog.lock().unwrap().is_some() {
                    break;
                }
                sleep(Duration::from_millis(100)).await;
            }
        }
        self.get_model_options()
            .ok_or_else(|| format!("{} 未返回模型列表", self.kind.label()))
    }

    pub async fn connected(&self) -> bool {
        self.alive_conns.load(Ordering::SeqCst) > 0
    }

    fn push_log(&self, line: String) {
        // 各 ACP 实例共用日志列表，加前缀便于在设置→日志里区分来源（Devin 不加前缀兼容历史）
        let line = match self.kind {
            AgentKind::CodeBuddy => format!("[codebuddy] {line}"),
            AgentKind::ClaudeCode => format!("[claudecode] {line}"),
            AgentKind::Cursor => format!("[cursor] {line}"),
            AgentKind::OpenCode => format!("[opencode] {line}"),
            _ => line,
        };
        {
            let mut logs = self.logs.lock().unwrap();
            if logs.len() >= LOG_CAP {
                logs.pop_front();
            }
            logs.push_back(line.clone());
        }
        let _ = self.app.emit(EV_LOG, line);
    }

    /// 杀掉全部连接（所有 CodeBuddy 线程进程 / Devin 共享进程）并清空全局路由。
    /// 用于「重启 agent」「改配置」「应用退出」等需要彻底重置的场景。
    pub async fn kill_conn(&self) {
        let slots: Vec<_> = self.slots.lock().unwrap().drain().map(|(_, v)| v).collect();
        for slot in slots {
            if let Some(conn) = slot.lock().await.take() {
                conn.kill();
            }
        }
        self.routes.lock().unwrap().clear();
        self.loading_sessions.lock().unwrap().clear();
        self.active_session.lock().unwrap().clear();
        self.conn_cwd.lock().unwrap().clear();
        self.conn_last_active.lock().unwrap().clear();
        self.conn_sessions.lock().unwrap().clear();
        self.serial_gates.lock().unwrap().clear();
        self.prewarmed.lock().unwrap().clear();
        self.prewarming.lock().unwrap().clear();
        self.pending_permissions.lock().unwrap().clear();
        self.title_jobs.lock().unwrap().clear();
        self.prompt_sent_at.lock().unwrap().clear();
    }

    /// 杀掉单个键对应的连接（CodeBuddy / Cursor 用于「停止/删除某线程」时释放它独占的进程），
    /// 并只清理属于该连接的会话路由，不影响其它并行连接。
    ///
    /// 注意：不移除 `serial_gates`。停止时旧轮次的 `run_prompt` 仍持有闸门，若此处删掉，
    /// 紧接着的重发会新建空闸门并立刻开跑，与旧轮次收尾竞态（误清 running / 报内部错误）。
    /// 闸门在 `forget_session_of_thread` 里随线程一起回收。
    async fn kill_conn_key(&self, key: &str) {
        if let Some(slot) = self.slot_opt(key) {
            if let Some(conn) = slot.lock().await.take() {
                conn.kill();
            }
        }
        self.slots.lock().unwrap().remove(key);
        self.conn_cwd.lock().unwrap().remove(key);
        self.conn_last_active.lock().unwrap().remove(key);
        self.conn_sessions.lock().unwrap().remove(key);
        self.clear_sessions_of_key(key);
    }

    /// 刷新某条连接的「最近活跃」时刻（CodeBuddy / ClaudeCode / OpenCode 维护，供空闲回收器判定）
    fn touch_conn(&self, key: &str) {
        if matches!(
            self.kind,
            AgentKind::CodeBuddy | AgentKind::ClaudeCode | AgentKind::OpenCode
        ) {
            self.conn_last_active
                .lock()
                .unwrap()
                .insert(key.to_string(), std::time::Instant::now());
        }
    }

    /// 记录某条连接上又创建/恢复了一个 session。仅 ClaudeCode 统计：其适配器会为每个
    /// session 常驻一个 claude 子进程，计数达到阈值即触发「会话数回收」。
    fn note_session_spawned(&self, conn_key: &str) {
        if self.kind == AgentKind::ClaudeCode {
            *self
                .conn_sessions
                .lock()
                .unwrap()
                .entry(conn_key.to_string())
                .or_insert(0) += 1;
        }
    }

    /// 某条连接的空闲回收超时：员工后台会话短（心跳下一轮会自动重连恢复），
    /// 用户交互会话长，AUX（标题/探测）居中；线程已被删除的连接立即回收。
    fn idle_timeout_for(&self, key: &str) -> u128 {
        if key == AUX_KEY {
            return IDLE_KILL_AUX_MS;
        }
        let Some(state) = self.app.try_state::<AppState>() else {
            return IDLE_KILL_USER_MS;
        };
        let store = state.store.lock().unwrap();
        match store.get(key) {
            Some(t) if t.employee_id.is_some() => IDLE_KILL_EMPLOYEE_MS,
            Some(_) => IDLE_KILL_USER_MS,
            // 线程已删除（正常已随删除回收，这里兜底）：立即回收
            None => 0,
        }
    }

    /// 空闲连接回收入口（由后台循环每 30s 调一次），按后端分派不同策略。
    async fn reap_idle_conns(self: &Arc<Self>) {
        match self.kind {
            AgentKind::CodeBuddy => self.reap_idle_codebuddy().await,
            AgentKind::ClaudeCode | AgentKind::OpenCode => self.reap_idle_shared().await,
            _ => {}
        }
    }

    /// ClaudeCode 共享连接回收：适配器进程本身不大，但它按 session 常驻的 claude 子进程
    /// （连带 LSP/MCP）只增不减，唯一的释放手段是整树杀掉共享连接。
    /// 条件：无任何运行中轮次、无预热/标题任务在飞，且（空闲超时 或 会话数达阈值后出现短暂
    /// 安静期）。下次发消息会自动重启进程并经 session/load 恢复上下文，仅首条消息慢几秒。
    async fn reap_idle_shared(self: &Arc<Self>) {
        if !self.running_threads.lock().unwrap().is_empty() {
            return;
        }
        if !self.prewarming.lock().unwrap().is_empty()
            || !self.title_jobs.lock().unwrap().is_empty()
            || !self.pending_permissions.lock().unwrap().is_empty()
        {
            return;
        }
        // 连接不存在或已死：没有可回收的进程树
        let alive = {
            let Some(slot) = self.slot_opt(SHARED_KEY) else {
                return;
            };
            let guard = slot.lock().await;
            guard
                .as_ref()
                .map(|c| c.alive.load(Ordering::SeqCst))
                .unwrap_or(false)
        };
        if !alive {
            return;
        }
        let idle_ms = {
            let m = self.conn_last_active.lock().unwrap();
            match m.get(SHARED_KEY) {
                Some(t) => t.elapsed().as_millis(),
                // 无活跃记录（异常残留）：按已超时处理
                None => u128::MAX,
            }
        };
        let sessions = self
            .conn_sessions
            .lock()
            .unwrap()
            .get(SHARED_KEY)
            .copied()
            .unwrap_or(0);
        let due = idle_ms >= IDLE_KILL_SHARED_MS
            || (sessions >= SHARED_SESSIONS_RECYCLE_AT && idle_ms >= IDLE_KILL_EMPLOYEE_MS);
        if !due {
            return;
        }
        // kill 前复查一次运行状态，收窄「检查到动手」之间的竞态窗口
        if !self.running_threads.lock().unwrap().is_empty() {
            return;
        }
        self.push_log(format!(
            "连接空闲 {} 分钟（累计 {sessions} 个 session），整树回收进程释放内存；下次发消息自动重连恢复",
            idle_ms / 60000
        ));
        self.kill_conn_key(SHARED_KEY).await;
    }

    /// CodeBuddy 空闲连接回收：把「不在运行中、闸门空闲、超时未活跃」的连接杀掉。
    /// 下次再用该线程会自动重启进程并经 session/load 恢复上下文（与手动停止后的自愈一致）。
    async fn reap_idle_codebuddy(self: &Arc<Self>) {
        let keys: Vec<String> = self.slots.lock().unwrap().keys().cloned().collect();
        let now = std::time::Instant::now();
        for key in keys {
            if key == SHARED_KEY {
                continue;
            }
            // 轮次运行中绝不回收（长任务不受超时影响）
            if self.running_threads.lock().unwrap().contains(&key) {
                continue;
            }
            let idle_ms = {
                let m = self.conn_last_active.lock().unwrap();
                match m.get(&key) {
                    Some(t) => now.duration_since(*t).as_millis(),
                    // 无活跃记录（异常残留）：按已超时处理
                    None => u128::MAX,
                }
            };
            if idle_ms < self.idle_timeout_for(&key) {
                continue;
            }
            // 持有该连接的串行闸门再回收：拿不到说明有辅助任务（标题生成/探测）或
            // 新一轮 prompt 正在使用这条连接，跳过本轮。闸门本身由 forget_session /
            // kill_conn 回收；此处只杀进程，保留闸门给可能的重连排队。
            let gate = self.serial_gates.lock().unwrap().get(&key).cloned();
            let _guard = match &gate {
                Some(g) => match g.clone().try_lock_owned() {
                    Ok(guard) => Some(guard),
                    Err(_) => continue,
                },
                None => None,
            };
            self.push_log(format!(
                "[codebuddy] 连接空闲 {} 分钟，回收进程（key={key}）",
                idle_ms / 60000
            ));
            self.kill_conn_key(&key).await;
        }
    }

    /// 只清理「属于某条连接键」的会话状态（路由 / 活跃会话 / 回放标记 / 计时），
    /// 供切目录重启、连接关闭、单键 kill 复用，避免误伤其它并行连接的会话。
    fn clear_sessions_of_key(&self, conn_key: &str) {
        self.active_session.lock().unwrap().remove(conn_key);
        let removed: Vec<String> = {
            let mut routes = self.routes.lock().unwrap();
            let keys: Vec<String> = routes
                .iter()
                .filter(|(_, r)| self.conn_key_for_thread(&r.thread_id) == conn_key)
                .map(|(sid, _)| sid.clone())
                .collect();
            for sid in &keys {
                routes.remove(sid);
            }
            keys
        };
        if !removed.is_empty() {
            let mut loading = self.loading_sessions.lock().unwrap();
            let mut sent = self.prompt_sent_at.lock().unwrap();
            for sid in &removed {
                loading.remove(sid);
                sent.remove(sid);
            }
        }
    }

    /// 手动重启：杀掉进程连接，并立即把所有运行中的轮次就地结束，
    /// 让卡死的任务在界面上马上停下（而不是干等到进程退出回调）。
    /// 下次发消息时会自动重连并经 session/load 恢复上下文。
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
            // kill_conn 后进程退出回调也会兜底结束轮次；这里抢先结束以即时反馈，
            // 已被结束的线程（is_running=false）跳过，避免重复提示。
            if self.is_running(&tid) {
                self.force_finish(
                    &tid,
                    "已重启 agent 进程，本轮已结束；下次发送会自动重连并恢复上下文。",
                )
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

    /// 取连接级串行闸门（按 conn_key）：CodeBuddy / Cursor / OpenCode 返回该连接对应的 Some(guard)
    /// （持有期间独占这条连接），其它 agent 返回 None（不串行、保持多路复用）。
    /// 不同 conn_key 各有各的闸门，因此不同线程（各自独占一条连接）之间互不阻塞，可真正并行。
    async fn serial_gate(&self, conn_key: &str) -> Option<tokio::sync::OwnedMutexGuard<()>> {
        if matches!(
            self.kind,
            AgentKind::CodeBuddy | AgentKind::Cursor | AgentKind::OpenCode
        ) {
            let gate = self
                .serial_gates
                .lock()
                .unwrap()
                .entry(conn_key.to_string())
                .or_insert_with(|| Arc::new(TokioMutex::new(())))
                .clone();
            Some(gate.lock_owned().await)
        } else {
            None
        }
    }

    /// 记录某条连接刚刚激活的 session（session/new 或 session/load 之后调用）。
    /// CodeBuddy 用它恢复单活跃会话；OpenCode 用它识别共享连接上的模型作用域切换。
    fn mark_active_session(&self, conn_key: &str, sid: &str) {
        if matches!(self.kind, AgentKind::CodeBuddy | AgentKind::OpenCode) {
            self.active_session
                .lock()
                .unwrap()
                .insert(conn_key.to_string(), sid.to_string());
        }
    }

    fn activate_model_session(&self, conn_key: &str, sid: &str) {
        let switched = {
            let mut active_sessions = self.active_session.lock().unwrap();
            if !connection_scoped_model_session_changed(
                &self.kind,
                active_sessions.get(conn_key).map(String::as_str),
                sid,
            ) {
                false
            } else {
                active_sessions.insert(conn_key.to_string(), sid.to_string());
                true
            }
        };
        if switched {
            if let Some(route) = self.routes.lock().unwrap().get_mut(sid) {
                route.applied_model = None;
            }
        }
    }

    /// CodeBuddy 专用：确保 `sid` 是其所在连接（conn_key）上的当前活跃 session。若期间有别的
    /// session/new 把它挤下了活跃位，就 session/load 重新激活——否则该会话再 prompt 会 end_turn
    /// 且不返回任何内容。session/load 会回放历史，故用 loading_sessions 抑制重复。
    /// 粒度 A 下每线程独占连接，一般不会被挤下；保留该逻辑以防万一（如复用/边界情况）。
    async fn ensure_active_session(
        self: &Arc<Self>,
        conn: &Arc<AcpConn>,
        conn_key: &str,
        sid: &str,
        cwd: &str,
    ) -> Result<(), String> {
        if self.kind != AgentKind::CodeBuddy {
            return Ok(());
        }
        if self
            .active_session
            .lock()
            .unwrap()
            .get(conn_key)
            .map(String::as_str)
            == Some(sid)
        {
            return Ok(());
        }
        self.loading_sessions
            .lock()
            .unwrap()
            .insert(sid.to_string());
        let thread_id = self
            .routes
            .lock()
            .unwrap()
            .get(sid)
            .map(|r| r.thread_id.clone());
        let r = conn
            .request(
                "session/load",
                json!({
                    "sessionId": sid,
                    "cwd": cwd,
                    "mcpServers": self.mcp_servers_for_thread(thread_id.as_deref())
                }),
                Some(Duration::from_secs(300)),
            )
            .await;
        self.loading_sessions.lock().unwrap().remove(sid);
        match r {
            Ok(_) => {
                self.mark_active_session(conn_key, sid);
                // 重新激活后会话侧 model/mode 可能回落默认，清掉已应用标记，让随后的
                // apply_session_config 重新下发，保证模型/模式与用户所选一致。
                if let Some(route) = self.routes.lock().unwrap().get_mut(sid) {
                    route.applied_model = None;
                    route.applied_mode = None;
                }
                self.push_log(format!("[codebuddy] 重新激活会话 {sid}"));
                Ok(())
            }
            Err(e) => Err(format!("重新激活会话失败：{e}")),
        }
    }

    /// 确保某个连接键对应的 ACP 进程存活，按需启动并完成 initialize 握手。
    /// 不同 conn_key 使用各自的连接槽，可并发建连接、并行跑会话。
    ///
    /// `want_cwd` 对 CodeBuddy / Cursor 有意义：两者都按「进程工作目录」跑命令。
    /// 该键已有连接但工作目录与本次不一致时，杀掉旧连接、按新目录重启。
    /// Devin/其它 agent 忽略该参数（走 session/new 的 cwd，无需重启）。
    async fn ensure_conn_for(
        self: &Arc<Self>,
        conn_key: &str,
        want_cwd: Option<&str>,
    ) -> Result<Arc<AcpConn>, String> {
        // CodeBuddy / Cursor 用户线程：cancel 已把 running 清掉并 kill 连接后，
        // 仍在飞的 drive_prompt/ensure_session 绝不能把进程重新拉起来
        // （否则界面已停、agent 还在跑，随后重发就会报内部错误）。
        if matches!(self.kind, AgentKind::CodeBuddy | AgentKind::Cursor)
            && conn_key != AUX_KEY
            && !self.is_running(conn_key)
        {
            return Err("任务已停止".into());
        }
        let slot = self.slot(conn_key);
        let mut guard = slot.lock().await;
        if let Some(c) = guard.as_ref() {
            if c.alive.load(Ordering::SeqCst) {
                let needs_respawn = matches!(self.kind, AgentKind::CodeBuddy | AgentKind::Cursor)
                    && want_cwd.is_some()
                    && self
                        .conn_cwd
                        .lock()
                        .unwrap()
                        .get(conn_key)
                        .map(String::as_str)
                        != want_cwd;
                if !needs_respawn {
                    self.touch_conn(conn_key);
                    return Ok(c.clone());
                }
                // 切工作目录：摘掉旧连接并杀掉（其关闭回调会因槽内即将换成新连接而判定
                // stale、跳过清理）；这里只清理本连接键的会话路由，随后重启。
                if let Some(old) = guard.take() {
                    old.kill();
                }
                self.clear_sessions_of_key(conn_key);
                if want_cwd.is_some()
                    && self
                        .conn_cwd
                        .lock()
                        .unwrap()
                        .get(conn_key)
                        .map(String::as_str)
                        != want_cwd
                {
                    self.push_log(format!(
                        "切换工作目录，重启连接 → {}",
                        want_cwd.unwrap_or("")
                    ));
                }
            }
        }
        // 再次确认：等槽锁期间可能已被 cancel
        if matches!(self.kind, AgentKind::CodeBuddy | AgentKind::Cursor)
            && conn_key != AUX_KEY
            && !self.is_running(conn_key)
        {
            return Err("任务已停止".into());
        }
        let settings = {
            let state = self.app.state::<AppState>();
            let s = state.settings.lock().unwrap().clone();
            s
        };
        let conn = self.spawn_conn(&settings, conn_key, want_cwd).await?;
        if let Some(cwd) = want_cwd {
            self.conn_cwd
                .lock()
                .unwrap()
                .insert(conn_key.to_string(), cwd.to_string());
        } else {
            self.conn_cwd.lock().unwrap().remove(conn_key);
        }
        self.touch_conn(conn_key);
        *guard = Some(conn.clone());
        Ok(conn)
    }

    /// 取某个连接键当前已建立且存活的连接（不新建）。
    async fn conn_for_key(&self, conn_key: &str) -> Option<Arc<AcpConn>> {
        let slot = self.slot_opt(conn_key)?;
        let guard = slot.lock().await;
        guard
            .as_ref()
            .filter(|c| c.alive.load(Ordering::SeqCst))
            .cloned()
    }

    async fn spawn_conn(
        self: &Arc<Self>,
        settings: &Settings,
        conn_key: &str,
        want_cwd: Option<&str>,
    ) -> Result<Arc<AcpConn>, String> {
        // 按实例类型选择启动命令：CodeBuddy 用 codebuddy_path/args，ClaudeCode 用 claudecode_path/args，
        // Cursor 用 cursor_path/args，OpenCode 用 opencode_path/args，其余 ACP agent（Devin）用 devin_path/acp_args
        let (program, mut args_str) = match self.kind {
            AgentKind::CodeBuddy => (
                settings.codebuddy_path.clone(),
                settings.codebuddy_args.clone(),
            ),
            AgentKind::ClaudeCode => (
                settings.claudecode_path.clone(),
                settings.claudecode_args.clone(),
            ),
            AgentKind::Cursor => (settings.cursor_path.clone(), settings.cursor_args.clone()),
            AgentKind::OpenCode => (
                settings.opencode_path.clone(),
                settings.opencode_args.clone(),
            ),
            _ => (settings.devin_path.clone(), settings.acp_args.clone()),
        };
        if self.kind == AgentKind::Cursor {
            *self.cursor_node_path.lock().unwrap() = cursor_node_path_for_program(&program);
        }
        // Cursor ACP 的 session/set_model 只接受 session/new 返回的少量默认变体；CLI 目录里的
        // 完整 thinking/effort/fast 组合必须用全局 `--model <id>` 在 `acp` 子命令之前启动。
        let cursor_startup_model = if self.kind == AgentKind::Cursor && conn_key != self.aux_key() {
            self.cursor_startup_model_for_thread(conn_key)
        } else {
            None
        };
        if let Some(model) = cursor_startup_model.as_deref() {
            args_str = format!("--model {model} {args_str}");
        }
        #[cfg(windows)]
        let mut cmd = build_acp_command(&program, &args_str);
        #[cfg(not(windows))]
        let mut cmd = {
            let mut c = tokio::process::Command::new(&program);
            c.args(args_str.split_whitespace());
            c
        };
        cmd.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        #[cfg(unix)]
        {
            // 独立进程组：退出时可 kill(-pid) 整组清理子孙
            cmd.process_group(0);
        }

        // 每个后端可单独配置代理：注入 HTTP(S)_PROXY 等环境变量到该子进程（空 = 不覆盖）
        apply_proxy_env(&mut cmd, self.proxy_of(settings));
        cmd.envs(&self.launch_env);
        #[cfg(windows)]
        if self.kind == AgentKind::Cursor {
            // Cursor 的 shell runner 未设置 windowsHide；仅对它的 Node 进程预加载默认值补丁。
            apply_cursor_windows_hide_patch(&self.app, &mut cmd, &self.launch_env)?;
        }

        // 把 ~/.nova/skills 用软链接/目录联接同步到各后端全局 skills 目录
        crate::skills::sync_skills_from_home();

        // Cursor：每条连接使用自己的配置目录，隔离不同线程的 effort / fast / ACP 变体缓存。
        // cursor-agent 启动时读入并缓存配置，必须在 spawn 前准备并注入环境变量。
        let cursor_config_dir = if self.kind == AgentKind::Cursor {
            self.launch_env
                .get("CURSOR_CONFIG_DIR")
                .map(std::path::PathBuf::from)
                .or_else(|| {
                    self.prepare_cursor_config_dir(conn_key, cursor_startup_model.as_deref())
                })
        } else {
            None
        };
        if let Some(dir) = cursor_config_dir.as_ref() {
            cmd.env("CURSOR_CONFIG_DIR", dir);
        }

        // CodeBuddy / Cursor：按会话项目目录启动进程。
        // CodeBuddy 的 ACP 不认 session/new cwd；Cursor 也按项目目录跑命令更稳。
        if matches!(self.kind, AgentKind::CodeBuddy | AgentKind::Cursor) {
            if let Some(cwd) = want_cwd {
                if std::path::Path::new(cwd).is_dir() {
                    cmd.current_dir(cwd);
                }
            }
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("无法启动 {}（{program}）：{e}", self.kind.label()))?;
        // 兜底：挂进 KILL_ON_JOB_CLOSE 的 Job，Nova 无论如何退出都不会残留 agent 孤儿进程
        assign_to_agent_job(&child);

        let stdin = child.stdin.take().ok_or("无法获取 agent stdin")?;
        let stdout = child.stdout.take().ok_or("无法获取 agent stdout")?;
        let stderr = child.stderr.take().ok_or("无法获取 agent stderr")?;

        let (stdin_tx, mut stdin_rx) = mpsc::unbounded_channel::<String>();
        let conn = Arc::new(AcpConn {
            key: conn_key.to_string(),
            label: self.kind.label(),
            stdin_tx,
            pending: StdMutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            alive: AtomicBool::new(true),
            child: StdMutex::new(Some(child)),
        });
        // 每创建一条连接 +1；对应的 stdout reader 结束时在 on_conn_closed 里 -1，恒定配对。
        self.alive_conns.fetch_add(1, Ordering::SeqCst);

        // stdin writer
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

        // stderr reader（devin 的日志走 stderr）
        {
            let mgr = self.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    mgr.push_log(line);
                }
            });
        }

        // stdout reader（JSON-RPC 消息流）
        {
            let mgr = self.clone();
            let conn2 = conn.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stdout).lines();
                loop {
                    match lines.next_line().await {
                        Ok(Some(line)) => {
                            let line = line.trim().to_string();
                            if line.is_empty() {
                                continue;
                            }
                            mgr.handle_line(&conn2, &line);
                        }
                        _ => break,
                    }
                }
                mgr.on_conn_closed(&conn2).await;
            });
        }

        // initialize 握手
        // Cursor ACP 只在客户端声明 parameterizedModelPicker 能力时，
        // 才会把 Composer 等模型的 Fast 开关暴露为独立的 `fast` config option；
        // 否则它只返回爆炸变体（如 `composer-2.5[fast=true]`），非 fast 变体无法选择。
        // 该字段放在 clientCapabilities._meta，其它后端会忽略。
        let client_capabilities = if self.kind == AgentKind::Cursor {
            json!({
                "fs": { "readTextFile": false, "writeTextFile": false },
                "_meta": { "parameterizedModelPicker": true }
            })
        } else {
            json!({
                "fs": { "readTextFile": false, "writeTextFile": false }
            })
        };
        let init = conn
            .request(
                "initialize",
                json!({
                    "protocolVersion": 1,
                    "clientInfo": {
                        "name": "nova",
                        "title": "Nova",
                        "version": env!("CARGO_PKG_VERSION")
                    },
                    // 不声明 fs 能力：我们不是编辑器、没有未保存缓冲区，
                    // 让 devin 走自己内部的文件读写管线——它对图片等二进制
                    // 文件有专门处理，经客户端 fs/read_text_file 读图片必然
                    // UTF-8 报错（导致带图会话前几个工具调用失败）
                    "clientCapabilities": client_capabilities
                }),
                Some(Duration::from_secs(60)),
            )
            .await;

        match init {
            Ok(result) => {
                *self.agent_info.lock().unwrap() = Some(result.clone());
                let _ = self.app.emit(
                    EV_STATUS,
                    json!({ "connected": true, "agent": result.get("agentInfo").cloned() }),
                );
                Ok(conn)
            }
            Err(e) => {
                conn.kill();
                Err(format!("{} ACP 初始化失败：{e}", self.kind.label()))
            }
        }
    }

    async fn on_conn_closed(&self, conn: &Arc<AcpConn>) {
        conn.alive.store(false, Ordering::SeqCst);
        // 连接存活计数 -1（与 spawn_conn 创建时的 +1 严格配对）
        self.alive_conns.fetch_sub(1, Ordering::SeqCst);
        // 让这条连接上所有等待中的请求立即失败
        let pending: Vec<_> = {
            let mut map = conn.pending.lock().unwrap();
            map.drain().collect()
        };
        for (_, tx) in pending {
            let _ = tx.send(Err(format!("{} 进程已退出", self.kind.label())));
        }
        let key = conn.key.clone();
        // stale 判定：若该键的槽已换成别的连接（如切目录重启被主动替换的旧连接），本回调只失败
        // pending、不做会话清理，避免误伤新连接。是自己才把槽置空并继续清理本连接的会话。
        let is_current = if let Some(slot) = self.slot_opt(&key) {
            let mut g = slot.lock().await;
            let same = g.as_ref().map(|c| Arc::ptr_eq(c, conn)).unwrap_or(false);
            if same {
                *g = None;
            }
            same
        } else {
            false
        };
        if !is_current {
            self.broadcast_if_all_closed();
            return;
        }
        // 只作废「属于本连接会话」的未决权限请求
        let removed_sessions: Vec<String> = {
            let routes = self.routes.lock().unwrap();
            routes
                .iter()
                .filter(|(_, r)| self.conn_key_for_thread(&r.thread_id) == key)
                .map(|(sid, _)| sid.clone())
                .collect()
        };
        let resolved_keys: Vec<String> = {
            let mut perms = self.pending_permissions.lock().unwrap();
            let keys: Vec<String> = perms
                .iter()
                .filter(|(_, p)| removed_sessions.contains(&p.session_id))
                .map(|(k, _)| k.clone())
                .collect();
            for k in &keys {
                perms.remove(k);
            }
            keys
        };
        for k in resolved_keys {
            let _ = self
                .app
                .emit(EV_PERMISSION_RESOLVED, json!({ "requestKey": k }));
        }
        // 清理本连接键的会话路由 / 活跃会话 / 回放标记 / 计时 / 会话计数
        self.clear_sessions_of_key(&key);
        self.conn_sessions.lock().unwrap().remove(&key);
        // 辅助（或 Devin 的共享）连接关闭时，跑在其上的标题任务作废、预热缓存清空
        if key == self.aux_key() {
            self.title_jobs.lock().unwrap().clear();
        }
        if key == SHARED_KEY {
            self.prewarmed.lock().unwrap().clear();
            self.prewarming.lock().unwrap().clear();
        }
        self.push_log(format!(
            "[nova] {} acp 连接已退出（key={key}）",
            self.kind.label()
        ));
        self.broadcast_if_all_closed();
    }

    /// 所有连接都已关闭时才广播「未连接」并清掉 agent 信息（多连接下不能因单条退出就报未连接）。
    fn broadcast_if_all_closed(&self) {
        if self.alive_conns.load(Ordering::SeqCst) == 0 {
            *self.agent_info.lock().unwrap() = None;
            let _ = self
                .app
                .emit(EV_STATUS, json!({ "connected": false, "agent": null }));
        }
    }

    fn handle_line(self: &Arc<Self>, conn: &Arc<AcpConn>, line: &str) {
        let Ok(msg) = serde_json::from_str::<Value>(line) else {
            self.push_log(format!("[nova] 无法解析的 stdout 行: {line}"));
            return;
        };
        let has_method = msg.get("method").is_some();
        let has_id = msg.get("id").is_some();

        if has_method && has_id {
            self.handle_server_request(conn, &msg);
        } else if has_method {
            let method = msg["method"].as_str().unwrap_or_default();
            match method {
                "session/update" => self.on_session_update(&msg["params"]),
                "_cognition.ai/output" => {
                    let p = &msg["params"];
                    self.push_log(format!(
                        "[{}] {}",
                        p["channel"].as_str().unwrap_or("devin"),
                        p["message"].as_str().unwrap_or_default()
                    ));
                }
                _ => {}
            }
        } else if has_id {
            // 响应
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

    fn handle_server_request(self: &Arc<Self>, conn: &Arc<AcpConn>, msg: &Value) {
        let method = msg["method"].as_str().unwrap_or_default().to_string();
        let id = msg["id"].clone();
        let params = msg["params"].clone();

        match method.as_str() {
            "session/request_permission" => {
                let session_id = params["sessionId"].as_str().unwrap_or_default().to_string();
                let thread_id = self
                    .routes
                    .lock()
                    .unwrap()
                    .get(&session_id)
                    .map(|r| r.thread_id.clone());
                let Some(thread_id) = thread_id else {
                    conn.respond_ok(id, json!({ "outcome": { "outcome": "cancelled" } }));
                    return;
                };
                let tool_call = params.get("toolCall").cloned().unwrap_or(Value::Null);
                let tool_call = if self.kind == AgentKind::Cursor {
                    self.enrich_cursor_tool_update(&thread_id, &session_id, &tool_call)
                } else {
                    tool_call
                };
                // 自动放行（挑一个 allow 选项作答）的两种情形：
                // - 数字员工：无人值守，授权请求没人应答会让本轮永久挂起（is_running 卡死、
                //   员工「永远在忙」、无法再次「立即执行」）；
                // - 统一 Build 模式：语义就是放开全部权限。Devin/Claude Code 的 bypass 在
                //   后端侧已不再上报授权，但 Cursor 的 agent 模式对敏感命令仍会请求确认
                //   （其 TUI 里 shift+tab 的 Run Everything 才等价免确认），这里代答 allow，
                //   让 Build 在所有后端行为一致。Plan 等其他模式照旧弹给用户审批。
                let (is_employee, is_build) = {
                    let state = self.app.state::<AppState>();
                    let store = state.store.lock().unwrap();
                    let t = store.get(&thread_id);
                    (
                        t.and_then(|t| t.employee_id.clone()).is_some(),
                        t.and_then(|t| t.mode.clone())
                            .map(|m| is_full_permission_mode(&m))
                            .unwrap_or(false),
                    )
                };
                if is_employee || is_build {
                    let allow = params
                        .get("options")
                        .and_then(|o| o.as_array())
                        .and_then(|arr| {
                            arr.iter()
                                .find(|o| {
                                    o["kind"]
                                        .as_str()
                                        .map(|k| k.starts_with("allow"))
                                        .unwrap_or(false)
                                })
                                .or_else(|| arr.first())
                                .and_then(|o| o["optionId"].as_str())
                        });
                    let outcome = match allow {
                        Some(oid) => json!({ "outcome": "selected", "optionId": oid }),
                        None => json!({ "outcome": "cancelled" }),
                    };
                    if is_build && !is_employee {
                        let title = tool_call
                            .get("title")
                            .and_then(|v| v.as_str())
                            .unwrap_or("工具调用");
                        self.push_log(format!("[nova] Build 模式自动批准授权：{title}"));
                    }
                    conn.respond_ok(id, json!({ "outcome": outcome }));
                    return;
                }
                // 权限 key 带实例前缀，便于 respond_permission 路由到正确的 manager。
                // Devin 保持无前缀（perm-）以兼容历史；CodeBuddy 用 cb-perm-；ClaudeCode 用 cc-perm-；
                // Cursor 用 cs-perm-；OpenCode 用 oc-perm-。
                let prefix = match self.kind {
                    AgentKind::CodeBuddy => "cb-",
                    AgentKind::ClaudeCode => "cc-",
                    AgentKind::Cursor => "cs-",
                    AgentKind::OpenCode => "oc-",
                    _ => "",
                };
                let key = format!("{}{prefix}perm-{}", self.permission_scope, id);
                self.pending_permissions.lock().unwrap().insert(
                    key.clone(),
                    PendingPermission {
                        rpc_id: id,
                        session_id,
                        conn: conn.clone(),
                    },
                );
                let _ = self.app.emit(
                    EV_PERMISSION,
                    json!({
                        "threadId": thread_id,
                        "agentKind": self.kind.as_str(),
                        "requestKey": key,
                        "toolCall": tool_call,
                        "options": params.get("options").cloned().unwrap_or(json!([])),
                    }),
                );
            }
            "fs/read_text_file" => {
                let conn = conn.clone();
                tokio::spawn(async move {
                    let path = params["path"].as_str().unwrap_or_default().to_string();
                    match tokio::fs::read_to_string(&path).await {
                        Ok(content) => {
                            let line = params["line"].as_u64();
                            let limit = params["limit"].as_u64();
                            let result = if line.is_some() || limit.is_some() {
                                let start = line.unwrap_or(1).saturating_sub(1) as usize;
                                let iter = content.lines().skip(start);
                                let v: Vec<&str> = match limit {
                                    Some(n) => iter.take(n as usize).collect(),
                                    None => iter.collect(),
                                };
                                v.join("\n")
                            } else {
                                content
                            };
                            conn.respond_ok(id, json!({ "content": result }));
                        }
                        Err(e) => conn.respond_err(id, -32603, format!("读取 {path} 失败: {e}")),
                    }
                });
            }
            "fs/write_text_file" => {
                let conn = conn.clone();
                tokio::spawn(async move {
                    let path = params["path"].as_str().unwrap_or_default().to_string();
                    let content = params["content"].as_str().unwrap_or_default().to_string();
                    if let Some(parent) = std::path::Path::new(&path).parent() {
                        let _ = tokio::fs::create_dir_all(parent).await;
                    }
                    match tokio::fs::write(&path, content).await {
                        Ok(_) => conn.respond_ok(id, json!({})),
                        Err(e) => conn.respond_err(id, -32603, format!("写入 {path} 失败: {e}")),
                    }
                });
            }
            _ => {
                conn.respond_err(id, -32601, format!("客户端不支持方法 {method}"));
            }
        }
    }

    fn emit_update(&self, thread_id: &str, op: Value) {
        // 只给前台正在查看的会话推流。后台会话的高频流式事件（delta/upsert）若也
        // 广播到 WebView，会被前端按 threadId 立刻丢弃，但仍已跨 IPC 全量反序列化成
        // JS 对象——多会话并发时，N 路 ~30/s 的增量在 WebView2 渲染进程里堆成海量瞬时
        // 垃圾，GC 追不上，进程内存飙升直至崩溃。增量已落库（thread.items），切回该
        // 会话时经 get_thread 快照 + 前端 reconcile 完整补齐，不丢内容。
        //
        // mode / proposed_plan / plan 是低频关键状态：agent 切到 Plan 后必须立刻反映到
        // 选择器并弹出「实施此计划」，不能被 active_thread 门控吞掉（否则会出现后端已
        // 进 Plan、UI 仍显示 Build、也没有实施按钮的卡死态）。
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

    fn on_session_update(self: &Arc<Self>, params: &Value) {
        let session_id = params["sessionId"].as_str().unwrap_or_default();
        let update = &params["update"];
        let kind = update["sessionUpdate"].as_str().unwrap_or_default();

        if kind == "available_commands_update" {
            self.capture_commands(update);
            return;
        }

        // 诊断：从 session/prompt 发出到首个响应（含 devin 推理时延）
        if let Some(t0) = self.prompt_sent_at.lock().unwrap().remove(session_id) {
            self.push_log(format!(
                "[nova][timing] 首响应延迟 {}ms (kind={})",
                t0.elapsed().as_millis(),
                kind
            ));
        }

        if self.loading_sessions.lock().unwrap().contains(session_id) {
            return; // session/load 回放阶段，本地已有历史
        }
        if self.capture_title_update(session_id, update) {
            return;
        }
        let thread_id = {
            let routes = self.routes.lock().unwrap();
            let Some(r) = routes.get(session_id) else {
                return;
            };
            r.thread_id.clone()
        };
        let enriched_update =
            if matches!(kind, "tool_call" | "tool_call_update") && self.kind == AgentKind::Cursor {
                Some(self.enrich_cursor_tool_update(&thread_id, session_id, update))
            } else {
                None
            };
        let update = enriched_update.as_ref().unwrap_or(update);
        let state = self.app.state::<AppState>();
        {
            let mut store = state.store.lock().unwrap();
            let Some(thread) = store.get_mut(&thread_id) else {
                return;
            };

            match kind {
                "agent_message_chunk" | "agent_thought_chunk" => {
                    for item in complete_pending_tools(thread, None) {
                        self.emit_update(&thread_id, json!({ "t": "upsert", "item": item }));
                    }
                    let text = extract_text(&update["content"]);
                    if text.is_empty() {
                        return;
                    }
                    let is_thought = kind == "agent_thought_chunk";
                    // devin 在工具调用间隙会泄漏内容恰为 "None" 的独立消息块（上游 bug），
                    // 仅在「将创建新条目」时丢弃，正常长文本中的 None 字样不受影响
                    if text.trim() == "None" {
                        let continues_last = match thread.items.last() {
                            Some(Item::Assistant { .. }) => !is_thought,
                            Some(Item::Thought { .. }) => is_thought,
                            _ => false,
                        };
                        if !continues_last {
                            return;
                        }
                    }
                    let appended = match thread.items.last_mut() {
                        Some(Item::Assistant { id, text: t, .. }) if !is_thought => {
                            t.push_str(&text);
                            Some(*id)
                        }
                        Some(Item::Thought { id, text: t, .. }) if is_thought => {
                            t.push_str(&text);
                            Some(*id)
                        }
                        _ => None,
                    };
                    thread.updated_at = now_ms();
                    match appended {
                        Some(item_id) => {
                            self.emit_update(
                                &thread_id,
                                json!({ "t": "delta", "itemId": item_id, "text": text }),
                            );
                        }
                        None => {
                            let id = thread.next_item_id();
                            let item = if is_thought {
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
                            thread.items.push(item.clone());
                            self.emit_update(&thread_id, json!({ "t": "upsert", "item": item }));
                        }
                    }
                }
                "tool_call" | "tool_call_update" => {
                    let tc_id = update["toolCallId"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string();
                    if tc_id.is_empty() {
                        return;
                    }
                    let mut found = false;
                    let mut completed = false;
                    let mut snapshot: Option<Item> = None;
                    for item in thread.items.iter_mut().rev() {
                        if let Item::Tool { call, .. } = item {
                            if call.tool_call_id == tc_id {
                                merge_tool_call(call, update);
                                completed = call.status == "completed" || call.status == "failed";
                                snapshot = Some(item.clone());
                                found = true;
                                break;
                            }
                        }
                    }
                    if !found {
                        for item in complete_pending_tools(thread, Some(&tc_id)) {
                            self.emit_update(&thread_id, json!({ "t": "upsert", "item": item }));
                        }
                        let call = tool_call_from_update(&tc_id, update);
                        let item = Item::Tool {
                            id: thread.next_item_id(),
                            ts: now_ms(),
                            call,
                        };
                        thread.items.push(item.clone());
                        snapshot = Some(item);
                    }
                    thread.updated_at = now_ms();
                    if let Some(item) = snapshot {
                        self.emit_update(&thread_id, json!({ "t": "upsert", "item": item }));
                    }
                    if completed {
                        store.save();
                    }
                }
                "plan" => {
                    let entries = update["entries"].clone();
                    thread.plan = Some(entries.clone());
                    thread.updated_at = now_ms();
                    self.emit_update(&thread_id, json!({ "t": "plan", "plan": entries }));
                }
                "current_mode_update" => {
                    // agent 自发切模式（Cursor SwitchMode / Devin 进 plan 等）：同步到统一
                    // Build/Plan，更新选择器，并让轮次结束时能弹出「实施此计划」。
                    // 以前若只改了后端 session、UI 事件被 active_thread 门控丢掉，就会出现
                    // 「已进 Plan 并停住，但前端仍显示 Build、也没有实施按钮」。
                    if let Some(mode) = update["currentModeId"].as_str() {
                        let reported = unify_mode_id(mode);
                        if let Some(r) = self.routes.lock().unwrap().get_mut(session_id) {
                            r.applied_mode = Some(reported.clone());
                        }
                        if thread.mode.as_deref() != Some(reported.as_str()) {
                            thread.mode = Some(reported.clone());
                            thread.updated_at = now_ms();
                            store.save();
                            self.emit_update(&thread_id, json!({ "t": "mode", "mode": reported }));
                        }
                    }
                }
                "available_commands_update" => {
                    self.capture_commands(update);
                }
                _ => {
                    // user_message_chunk（load 回放）等忽略
                }
            }
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
                self.emit_proposed_plan(thread_id, None);
                return;
            }
            thread.plan = None;
            thread.updated_at = now_ms();
            store.save();
            true
        };

        if changed {
            self.emit_update(thread_id, json!({ "t": "plan", "plan": [] }));
        }
        self.emit_proposed_plan(thread_id, None);
    }

    /// Plan 模式产出的正文：前端据此展示「实施此计划 / 继续规划」。
    fn emit_proposed_plan(&self, thread_id: &str, text: Option<String>) {
        self.emit_update(thread_id, json!({ "t": "proposed_plan", "text": text }));
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
        let _ = self.app.emit(
            EV_TURN,
            json!({ "threadId": thread_id, "running": running, "stopReason": stop_reason }),
        );
    }

    /// 轮次收尾：写入 turn item（耗时 + token 用量）并结束 running 状态
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
        self.maybe_emit_plan_action(thread_id, &stop_reason);
        self.set_running(thread_id, false, Some(stop_reason.clone()));
        self.notify_done(thread_id, &stop_reason);
    }

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
            if thread.mode.as_deref().map(unify_mode_id).as_deref() != Some("plan") {
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

    /// 任务结束的系统通知（窗口在前台时不打扰），点击跳转到对应会话
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
            "end_turn" | "max_turn_requests" => "任务已完成，点击查看结果",
            "cancelled" | "force_cancelled" => "任务已停止",
            _ => "任务已结束（出错）",
        };
        crate::sys_notify::notify_thread_done(&self.app, thread_id, &title, body, EV_NOTIFY_OPEN);
    }

    /// 缓存 session/new 返回的模型/模式选项并通知前端
    fn capture_options(self: &Arc<Self>, result: &Value) {
        // 标准 ACP（如 Claude Code）把可选模型放在 `models`(SessionModelState)；
        // Cognition/Devin 扩展放在 `configOptions`。统一收敛成前端期望的 configOptions 形状：
        // [{ id:"model", currentValue, options:[{value,name,description}] }]。
        fn models_to_config_options(models: Option<&Value>) -> Value {
            let Some(models) = models else {
                return Value::Null;
            };
            let Some(available) = models.get("availableModels").and_then(|v| v.as_array()) else {
                return Value::Null;
            };
            let options: Vec<Value> = available
                .iter()
                .filter_map(|m| {
                    // 标准字段是 modelId；对个别实现容错兼容 id / value。
                    let value = m
                        .get("modelId")
                        .and_then(|v| v.as_str())
                        .or_else(|| m.get("id").and_then(|v| v.as_str()))
                        .or_else(|| m.get("value").and_then(|v| v.as_str()))?;
                    let name = m.get("name").and_then(|v| v.as_str()).unwrap_or(value);
                    let mut opt = json!({ "value": value, "name": name });
                    if let Some(desc) = m.get("description").and_then(|v| v.as_str()) {
                        opt["description"] = json!(desc);
                    }
                    Some(opt)
                })
                .collect();
            if options.is_empty() {
                return Value::Null;
            }
            let current = models
                .get("currentModelId")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            json!([{ "id": "model", "name": "Model", "currentValue": current, "options": options }])
        }

        // 无标准 modes 字段时，从 configOptions 里 id=="mode" 的选项合成 SessionModeState
        // （OpenCode 走这条：它把会话模式作为 config option 下发，set 用标准 session/set_mode）。
        fn modes_from_config_options(config_options: &Value) -> Value {
            let Some(arr) = config_options.as_array() else {
                return Value::Null;
            };
            let Some(mode_opt) = arr
                .iter()
                .find(|o| o.get("id").and_then(|v| v.as_str()) == Some("mode"))
            else {
                return Value::Null;
            };
            let available: Vec<Value> = mode_opt
                .get("options")
                .and_then(|v| v.as_array())
                .map(|opts| {
                    opts.iter()
                        .filter_map(|opt| {
                            let id = opt.get("value").and_then(|v| v.as_str())?;
                            let name = opt.get("name").and_then(|v| v.as_str()).unwrap_or(id);
                            Some(json!({ "id": id, "name": name }))
                        })
                        .collect()
                })
                .unwrap_or_default();
            if available.is_empty() {
                return Value::Null;
            }
            json!({
                "currentModeId": mode_opt.get("currentValue").cloned().unwrap_or(Value::Null),
                "availableModes": available,
            })
        }

        let has_config = result.get("configOptions").is_some();
        let has_models = result.get("models").is_some();
        let has_modes = result.get("modes").is_some();
        // Cursor：即便 session/new 完全不带 models/configOptions，也要用 CLI 目录兜底，
        // 否则前端合并选择器看不到 Cursor，更选不到 thinking/effort 变体。
        if !has_config && !has_models && !has_modes && self.kind != AgentKind::Cursor {
            return;
        }
        // configOptions 优先用扩展字段；没有则从标准 models 转换（Claude Code 走这条）。
        let mut config_options = if has_config {
            result.get("configOptions").cloned().unwrap_or(Value::Null)
        } else {
            models_to_config_options(result.get("models"))
        };
        // OpenCode 只在切到某个模型后才返回该模型的 effort 选项。模型目录探测会把
        // provider/model/variant 展开成可直接选择的模型项；普通 session/new / prewarm
        // 仍只有基座模型列表，这里把已探测到的 variants 合回去，避免覆盖丰富目录。
        if self.kind == AgentKind::OpenCode && !opencode_catalog_is_expanded(&config_options) {
            if let Some(cached) = self.get_model_options() {
                merge_cached_opencode_variants(&mut config_options, &cached);
            }
        }
        // Cursor 的 ACP 只上报每个模型的默认变体，完整 thinking/effort/fast 枚举来自
        // `cursor-agent models`。两边合并：CLI 负责完整变体，ACP 补充 CLI 偶尔漏掉的模型。
        if self.kind == AgentKind::Cursor {
            if !config_options.is_array() {
                config_options = json!([]);
            }
            // 必须从本次 session/new 的原始选项读取，不能在模型合并函数里再锁
            // self.model_options：后台目录刷新本身已持有该锁，重复加锁会永久死锁。
            let fast_values = cursor_fast_values(&config_options);
            if let Some(arr) = config_options.as_array_mut() {
                let model_opt = arr
                    .iter_mut()
                    .find(|o| o.get("id").and_then(|v| v.as_str()) == Some("model"));
                let acp_options = model_opt
                    .as_ref()
                    .and_then(|opt| opt.get("options"))
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                *self.cursor_acp_models.lock().unwrap() = acp_options;
                // 本地 CLI 查询不发送 prompt、不产生模型调用费用。完成后再次合并并广播。
                self.spawn_cursor_catalog_fetch();
                // 目录未就绪时，若内存里已有较完整列表（磁盘缓存 / 上次合并结果），
                // 不要用「仅 Auto + ACP 默认变体」覆盖并广播——否则前端会把已选模型重置成 Auto。
                let catalog_ready = self.cursor_catalog.lock().unwrap().is_some();
                let existing_rich = Self::model_option_count(self.get_model_options().as_ref()) > 1;
                if !catalog_ready && existing_rich {
                    return;
                }
                let merged = self.cursor_merged_model_options(&fast_values);
                match model_opt {
                    Some(opt) => {
                        opt["options"] = merged;
                        opt["currentValue"] = json!("");
                    }
                    None => arr.push(json!({
                        "id": "model",
                        "name": "Model",
                        "currentValue": "",
                        "options": merged,
                    })),
                }
            }
        }
        let modes = match result.get("modes") {
            Some(m) if !m.is_null() => m.clone(),
            _ => modes_from_config_options(&config_options),
        };
        let v = json!({
            "configOptions": config_options,
            "modes": modes,
        });
        *self.model_options.lock().unwrap() = Some(v.clone());
        self.persist_model_options(&v);
        let _ = self.app.emit(
            EV_OPTIONS,
            json!({ "agentKind": self.kind.as_str(), "options": v }),
        );
    }

    /// OpenCode 的首次 session/new 只列基座模型；对探测 session 逐个切换模型后，
    /// 它会用标准 effort config option 返回该模型的全部 variants。每次切换只改本地
    /// session 状态，不发送 prompt，也不会产生模型调用费用。
    async fn expand_opencode_model_variants(
        &self,
        conn: &Arc<AcpConn>,
        mut response: Value,
    ) -> Value {
        let Some(session_id) = response
            .get("sessionId")
            .and_then(|v| v.as_str())
            .map(str::to_string)
        else {
            return response;
        };
        let base_models = opencode_model_choices(&response);
        if base_models.is_empty() {
            return response;
        }

        let cached_groups = self
            .get_model_options()
            .as_ref()
            .map(opencode_variant_groups)
            .unwrap_or_default();
        let mut expanded = Vec::new();
        let mut consecutive_failures = 0usize;

        for model in base_models {
            let Some(model_id) = model
                .get("value")
                .and_then(|v| v.as_str())
                .map(str::to_string)
            else {
                continue;
            };
            let fallback = || {
                cached_groups
                    .get(&model_id)
                    .cloned()
                    .unwrap_or_else(|| vec![model.clone()])
            };
            if consecutive_failures >= 2 {
                expanded.extend(fallback());
                continue;
            }

            match conn
                .request(
                    "session/set_config_option",
                    json!({
                        "sessionId": session_id,
                        "configId": "model",
                        "value": &model_id,
                    }),
                    Some(Duration::from_secs(5)),
                )
                .await
            {
                Ok(result) => {
                    consecutive_failures = 0;
                    expanded.extend(expand_opencode_model_choice(
                        &model,
                        &opencode_effort_choices(&result),
                    ));
                }
                Err(e) => {
                    consecutive_failures += 1;
                    self.push_log(format!(
                        "[nova] 获取 OpenCode 模型 variants 失败 {model_id}: {e}"
                    ));
                    expanded.extend(fallback());
                }
            }
        }

        if let Some(model_option) = config_option_mut(&mut response, "model") {
            model_option["options"] = Value::Array(expanded);
        }
        response
    }

    /// 已缓存模型选项里非空 value 的数量（用于判断是否比「仅 Auto」更完整）。
    fn model_option_count(opts: Option<&Value>) -> usize {
        let Some(opts) = opts else { return 0 };
        let Some(arr) = opts.get("configOptions").and_then(|c| c.as_array()) else {
            return 0;
        };
        let Some(model) = arr
            .iter()
            .find(|o| o.get("id").and_then(|x| x.as_str()) == Some("model"))
        else {
            return 0;
        };
        model
            .get("options")
            .and_then(|o| o.as_array())
            .map(|xs| {
                xs.iter()
                    .filter(|o| {
                        o.get("value")
                            .and_then(|v| v.as_str())
                            .map(|s| !s.is_empty())
                            .unwrap_or(false)
                    })
                    .count()
            })
            .unwrap_or(0)
    }

    fn capture_commands(&self, update: &Value) {
        let commands = update
            .get("commands")
            .cloned()
            .or_else(|| update.get("availableCommands").cloned())
            .unwrap_or_else(|| json!([]));
        *self.available_commands.lock().unwrap() = Some(commands.clone());
        let _ = self.app.emit(
            EV_COMMANDS,
            json!({ "agentKind": self.kind.as_str(), "commands": commands }),
        );
    }

    /// Cursor 模型选项：「Auto」+ CLI 完整变体 + ACP 独有模型。
    /// CLI 项保留原始短 id，实际在独立 ACP 进程启动时通过 `--model` 生效。
    /// 当 ACP 返回参数化选择器（model + fast）且 CLI 目录尚未就绪时，
    /// 从基础模型合成 fast/non-fast 短 id，保证前端仍能切换速度档。
    fn cursor_merged_model_options(&self, fast_values: &[String]) -> Value {
        let mut options = vec![json!({
            "value": "",
            "name": "Auto（Cursor 默认）"
        })];
        let mut cli_bases = HashSet::new();
        if let Some(catalog) = self.cursor_catalog.lock().unwrap().as_ref() {
            for (id, name) in catalog {
                cli_bases.insert(cursor_variant_dims(id).0);
                options.push(json!({ "value": id, "name": name }));
            }
        }

        let can_synthesize = !fast_values.is_empty()
            && self
                .cursor_catalog
                .lock()
                .unwrap()
                .as_ref()
                .map(|c| c.is_empty())
                .unwrap_or(true);

        for option in self.cursor_acp_models.lock().unwrap().iter() {
            let Some(value) = option.get("value").and_then(|v| v.as_str()) else {
                continue;
            };
            if value == "default" {
                continue;
            }
            let base = value.split_once('[').map(|(base, _)| base).unwrap_or(value);
            if cli_bases.contains(base) {
                continue;
            }
            // 参数化模式返回的基础模型：合成 fast/non-fast 短 id 变体。
            if can_synthesize && !value.contains('[') {
                let name = option
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or(value)
                    .to_string();
                for fast in fast_values {
                    let (id, display) = if fast == "true" {
                        (format!("{base}-fast"), format!("{name} Fast"))
                    } else {
                        (base.to_string(), name.clone())
                    };
                    options.push(json!({ "value": id, "name": display }));
                }
                continue;
            }
            options.push(option.clone());
        }
        json!(options)
    }

    /// 后台拉取一次 `cursor-agent models` 目录（本地 CLI 列表查询，不发送任何 prompt、
    /// 不产生模型调用费用）。拉到后合并进 model_options 缓存并重新广播给前端。
    fn spawn_cursor_catalog_fetch(self: &Arc<Self>) {
        if self.kind != AgentKind::Cursor {
            return;
        }
        {
            let mut fetching = self.cursor_catalog_fetching.lock().unwrap();
            if *fetching || self.cursor_catalog.lock().unwrap().is_some() {
                return;
            }
            *fetching = true;
        }
        let mgr = self.clone();
        tauri::async_runtime::spawn(async move {
            let (program, proxy) = {
                let state = mgr.app.state::<AppState>();
                let s = state.settings.lock().unwrap();
                (s.cursor_path.clone(), s.cursor_proxy.clone())
            };
            let result = run_cursor_models_cli(&program, &proxy).await;
            *mgr.cursor_catalog_fetching.lock().unwrap() = false;
            match result {
                Ok(catalog) if !catalog.is_empty() => {
                    mgr.push_log(format!(
                        "[nova] 模型目录就绪：{} 项（来自 cursor-agent models）",
                        catalog.len()
                    ));
                    *mgr.cursor_catalog.lock().unwrap() = Some(catalog);
                    mgr.refresh_cursor_model_options();
                }
                Ok(_) => {
                    // 记空目录，避免 fetch_model_options 反复空等 15s
                    *mgr.cursor_catalog.lock().unwrap() = Some(Vec::new());
                    mgr.push_log("[nova] cursor-agent models 未返回任何模型".into());
                }
                Err(e) => {
                    *mgr.cursor_catalog.lock().unwrap() = Some(Vec::new());
                    mgr.push_log(format!("[nova] 拉取模型目录失败：{e}"));
                }
            }
        });
    }

    /// 把 CLI 目录合并进已缓存的模型选项并广播
    fn refresh_cursor_model_options(&self) {
        let updated = {
            let mut guard = self.model_options.lock().unwrap();
            let Some(v) = guard.as_mut() else { return };
            let fast_values = cursor_fast_values(v);
            let Some(arr) = v.get_mut("configOptions").and_then(|c| c.as_array_mut()) else {
                return;
            };
            let Some(opt) = arr
                .iter_mut()
                .find(|o| o.get("id").and_then(|x| x.as_str()) == Some("model"))
            else {
                return;
            };
            opt["options"] = self.cursor_merged_model_options(&fast_values);
            v.clone()
        };
        self.persist_model_options(&updated);
        let _ = self.app.emit(
            EV_OPTIONS,
            json!({ "agentKind": self.kind.as_str(), "options": updated }),
        );
    }

    /// 预热：提前为某个项目目录创建空 session，消除首条消息的建会话延迟
    pub async fn prewarm(self: &Arc<Self>, cwd: String) {
        // CodeBuddy 单连接只允许一个活跃 session；Cursor 的完整模型需按线程启动独立进程，
        // 两者都不能用不带线程/模型信息的共享预热 session。
        if matches!(self.kind, AgentKind::CodeBuddy | AgentKind::Cursor) {
            return;
        }
        // 拿闸门前先粗筛：已预热好/正在预热就别再排队
        {
            let warmed = self.prewarmed.lock().unwrap();
            let warming = self.prewarming.lock().unwrap();
            if warmed.contains_key(&cwd) || warming.contains(&cwd) {
                return;
            }
        }
        // 这里只有非 CodeBuddy / Cursor 会走到。OpenCode 预热仍需与用户轮次共用连接闸门，
        // 避免 session/new 与模型切换或 prompt 重叠；Devin 等返回 None、保持多路复用。
        let _gate = self.serial_gate(SHARED_KEY).await;
        {
            let warmed = self.prewarmed.lock().unwrap();
            let mut warming = self.prewarming.lock().unwrap();
            if warmed.contains_key(&cwd) || !warming.insert(cwd.clone()) {
                return;
            }
        }
        let result = async {
            let conn = self.ensure_conn_for(SHARED_KEY, None).await?;
            let resp = conn
                .request(
                    "session/new",
                    json!({
                        "cwd": cwd,
                        "mcpServers": []
                    }),
                    Some(Duration::from_secs(180)),
                )
                .await?;
            self.capture_options(&resp);
            let sid = resp["sessionId"]
                .as_str()
                .map(|s| s.to_string())
                .ok_or_else(|| "session/new 未返回 sessionId".to_string())?;
            self.note_session_spawned(SHARED_KEY);
            // 统一记录活跃会话（对 Devin 为 no-op；CodeBuddy 不会走到预热）
            self.mark_active_session(SHARED_KEY, &sid);
            Ok::<String, String>(sid)
        }
        .await;
        self.prewarming.lock().unwrap().remove(&cwd);
        match result {
            Ok(sid) => {
                self.push_log(format!("[nova] 预热完成 {cwd} → {sid}"));
                self.prewarmed.lock().unwrap().insert(cwd, sid);
            }
            Err(e) => self.push_log(format!("[nova] 预热失败 {cwd}: {e}")),
        }
    }

    async fn take_prewarmed_session(&self, cwd: &str) -> Option<String> {
        for _ in 0..200 {
            if let Some(sid) = self.prewarmed.lock().unwrap().remove(cwd) {
                return Some(sid);
            }
            if !self.prewarming.lock().unwrap().contains(cwd) {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        None
    }

    /// 确保线程的 ACP session 就绪（按需建立/恢复），返回 sessionId
    async fn ensure_session(self: &Arc<Self>, thread_id: &str) -> Result<String, String> {
        let lock = self.thread_lock(thread_id);
        let _guard = lock.lock().await;

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
        // CodeBuddy：每个线程独占一条连接（key=thread_id）、按其项目目录启动进程 → 真正并行。
        // Devin：所有线程共用 SHARED 连接（多路复用）。
        let key = self.conn_key_for_thread(thread_id);
        let conn = self.ensure_conn_for(&key, Some(&cwd)).await?;

        let sid = match existing {
            Some(sid) if self.routes.lock().unwrap().contains_key(&sid) => sid,
            Some(sid) if self.kind == AgentKind::Cursor => {
                self.push_log(format!(
                    "[nova] Cursor 跳过 session/load，同步旧会话 {sid} 改为本地历史接力"
                ));
                let new_sid = self.new_session_for(&conn, &key, thread_id, &cwd).await?;
                let state = self.app.state::<AppState>();
                let mut store = state.store.lock().unwrap();
                if let Some(thread) = store.get_mut(thread_id) {
                    thread.acp_session_id = Some(new_sid.clone());
                }
                store.save();
                new_sid
            }
            Some(sid) => {
                // 进程重启过：尝试 session/load 恢复上下文。瞬时网络错先重试，避免误丢上下文。
                self.loading_sessions.lock().unwrap().insert(sid.clone());
                self.routes.lock().unwrap().insert(
                    sid.clone(),
                    Route {
                        thread_id: thread_id.to_string(),
                        applied_model: self.cursor_startup_model_for_thread(thread_id),
                        applied_mode: None,
                    },
                );
                let load_attempts: u32 = if self.kind == AgentKind::Cursor { 4 } else { 2 };
                let mut loaded: Result<Value, String> = Err("session/load 未执行".into());
                for attempt in 1..=load_attempts {
                    if !conn.alive.load(Ordering::SeqCst) {
                        loaded = Err(format!("{} 进程已退出", self.kind.label()));
                        break;
                    }
                    loaded = conn
                        .request(
                            "session/load",
                            json!({
                                "sessionId": sid,
                                "cwd": cwd,
                                "mcpServers": self.mcp_servers_for_thread(Some(thread_id))
                            }),
                            Some(Duration::from_secs(300)),
                        )
                        .await;
                    match &loaded {
                        Ok(_) => break,
                        Err(e) if attempt < load_attempts && is_retriable_rpc_error(e) => {
                            let delay_ms = retriable_backoff_ms(attempt);
                            self.push_log(format!(
                                "[nova] session/load 瞬时失败（第{attempt}/{load_attempts}次）：{e}，{delay_ms}ms 后重试"
                            ));
                            sleep(Duration::from_millis(delay_ms)).await;
                        }
                        Err(_) => break,
                    }
                }
                self.loading_sessions.lock().unwrap().remove(&sid);
                match loaded {
                    Ok(_) => {
                        // session/load 成功即把该会话设为其连接的活跃会话；
                        // 对 ClaudeCode 同样意味着适配器里多驻留了一个 claude 子进程
                        self.note_session_spawned(&key);
                        self.mark_active_session(&key, &sid);
                        sid
                    }
                    Err(e) => {
                        self.routes.lock().unwrap().remove(&sid);
                        self.push_log(format!("[nova] session/load 失败，转为新建会话：{e}"));
                        let new_sid = self.new_session_for(&conn, &key, thread_id, &cwd).await?;
                        let state = self.app.state::<AppState>();
                        let mut store = state.store.lock().unwrap();
                        if let Some(thread) = store.get_mut(thread_id) {
                            thread.acp_session_id = Some(new_sid.clone());
                            let item = thread.push_system(
                                "历史会话无法恢复，已在新会话中继续（上下文可能丢失）。".into(),
                                "warn",
                            );
                            self.emit_update(thread_id, json!({ "t": "upsert", "item": item }));
                        }
                        store.save();
                        new_sid
                    }
                }
            }
            None => {
                // 优先消费预热好的 session
                let sid = match self.take_prewarmed_session(&cwd).await {
                    Some(sid) => {
                        self.routes.lock().unwrap().insert(
                            sid.clone(),
                            Route {
                                thread_id: thread_id.to_string(),
                                applied_model: self.cursor_startup_model_for_thread(thread_id),
                                applied_mode: None,
                            },
                        );
                        sid
                    }
                    None => self.new_session_for(&conn, &key, thread_id, &cwd).await?,
                };
                {
                    let state = self.app.state::<AppState>();
                    let mut store = state.store.lock().unwrap();
                    if let Some(thread) = store.get_mut(thread_id) {
                        thread.acp_session_id = Some(sid.clone());
                    }
                    store.save();
                }
                sid
            }
        };

        // CodeBuddy：prompt 前确保本会话仍是其连接的活跃会话（期间可能被别的 session/new 挤下）
        self.ensure_active_session(&conn, &key, &sid, &cwd).await?;
        self.apply_session_config(&conn, &key, &sid, model, mode)
            .await;
        Ok(sid)
    }

    /// 把模型标记为「已处理」（避免同一会话每轮 prompt 反复处理），并在会话里推一条警告。
    /// 用于模型无法/不应下发的场景：不在可用列表、Cursor 拒绝切换、Max 变体被拦截等。
    fn mark_model_applied_with_warn(&self, sid: &str, model: &str, warn: String) {
        let thread_id = {
            let mut routes = self.routes.lock().unwrap();
            routes.get_mut(sid).map(|route| {
                route.applied_model = Some(model.to_string());
                route.thread_id.clone()
            })
        };
        let Some(thread_id) = thread_id else { return };
        let state = self.app.state::<AppState>();
        let mut store = state.store.lock().unwrap();
        if let Some(thread) = store.get_mut(&thread_id) {
            let item = thread.push_system(warn, "warn");
            self.emit_update(&thread_id, json!({ "t": "upsert", "item": item }));
        }
        store.save();
    }

    /// 按需把线程级模型/模式同步到 session（只在变化时发请求）
    async fn apply_session_config(
        &self,
        conn: &Arc<AcpConn>,
        conn_key: &str,
        sid: &str,
        model: Option<String>,
        mode: Option<String>,
    ) {
        self.activate_model_session(conn_key, sid);
        let (mut need_model, need_mode) = {
            let routes = self.routes.lock().unwrap();
            let Some(r) = routes.get(sid) else { return };
            (
                model.filter(|m| r.applied_model.as_ref() != Some(m)),
                mode.filter(|m| r.applied_mode.as_ref() != Some(m)),
            )
        };
        // 统一模式翻译：界面只暴露 build / plan 两种，这里翻成各后端的真实模式 id。
        // 翻译结果不在可用列表时：先找语义等价 fallback（Build→其它全权限 id）；
        // 没有 fallback 仍尝试下发，避免以前「直接标成已应用」导致 UI 显示 Build、
        // session 实际停在默认 Plan、也没有「实施」按钮。
        let mut mode_to_send = need_mode.clone().map(|m| self.backend_mode_id(&m));
        if let (Some(m), Some(target)) = (need_mode.clone(), mode_to_send.clone()) {
            if self
                .known_mode_ids()
                .is_some_and(|known| !known.contains(&target))
            {
                if let Some(alt) = self
                    .known_mode_ids()
                    .and_then(|known| pick_fallback_mode_id(&m, &known))
                {
                    self.push_log(format!(
                        "[nova] 模式「{m}」（→{target}）不在 {} 可用列表，改用「{alt}」",
                        self.kind.label()
                    ));
                    mode_to_send = Some(alt);
                } else {
                    self.push_log(format!(
                        "[nova] 模式「{m}」（→{target}）不在 {} 可用列表，仍尝试下发",
                        self.kind.label()
                    ));
                }
            }
        }
        // 防「发送无反应」：所选模型若不在该后端当前可用列表里（典型如 CodeBuddy 服务端
        // 调整了可用模型，但本地仍记着旧模型 id），强行下发会让 agent 在 prompt 阶段直接
        // 返回 refusal 且不产生任何内容。这里跳过下发、回退到会话默认模型，并提示用户。
        if let Some(m) = need_model.clone() {
            if self
                .known_model_values()
                .is_some_and(|known| !known.contains(&m))
            {
                need_model = None;
                self.mark_model_applied_with_warn(
                    sid,
                    &m,
                    format!("所选模型「{m}」当前不可用，已改用默认模型，可在下方重新选择模型。"),
                );
            }
        }
        if need_model.is_none() && need_mode.is_none() {
            return;
        }
        let t_cfg = std::time::Instant::now();
        // 模型与模式互相独立，并发下发以省一次往返，缩短首字前的等待。
        let model_fut = async {
            if let Some(model) = need_model {
                // Cursor ACP：若 initialize 时声明了 parameterizedModelPicker，
                // 优先把 CLI 短 id 拆成基座模型 + fast 开关，用 set_config_option 分别下发。
                // 这样选择 `composer-2.5` 会显式设置 fast=false，避免被 Cursor 默认成 fast。
                if self.kind == AgentKind::Cursor {
                    match self
                        .apply_cursor_parameterized_model(conn, sid, &model)
                        .await
                    {
                        Some(true) => return,
                        Some(false) => {
                            self.mark_model_applied_with_warn(
                                sid,
                                &model,
                                format!("Cursor 参数化模型设置失败「{model}」，本会话将按 Cursor CLI 的当前默认模型执行。"),
                            );
                            return;
                        }
                        None => {}
                    }
                }

                // 标准 ACP（Claude Code / OpenCode）用 session/set_model{modelId}；
                // Cognition/Devin、CodeBuddy 扩展用 session/set_config_option{configId:"model"}。
                let standard_acp = matches!(
                    self.kind,
                    AgentKind::ClaudeCode | AgentKind::Cursor | AgentKind::OpenCode
                );
                let (method, params) = if standard_acp {
                    (
                        "session/set_model",
                        json!({ "sessionId": sid, "modelId": model }),
                    )
                } else {
                    (
                        "session/set_config_option",
                        json!({ "sessionId": sid, "configId": "model", "value": model }),
                    )
                };
                let r = conn
                    .request(method, params, Some(Duration::from_secs(30)))
                    .await;
                match r {
                    Ok(_) => {
                        if let Some(route) = self.routes.lock().unwrap().get_mut(sid) {
                            route.applied_model = Some(model);
                        }
                    }
                    Err(e) => {
                        self.push_log(format!("[nova] 设置模型失败: {e}"));
                        // Cursor 的 ACP 在其模型接口异常时对任何 set_model 都报
                        // Invalid model value。标记已处理 + 会话内提示一次（而不是
                        // 每轮 prompt 重复报错），本会话按 Cursor CLI 默认模型执行。
                        if self.kind == AgentKind::Cursor {
                            self.mark_model_applied_with_warn(
                                sid,
                                &model,
                                format!(
                                    "Cursor 未接受模型切换「{model}」，本会话将按 Cursor CLI 的当前默认模型执行。"
                                ),
                            );
                        }
                    }
                }
            }
        };
        let mode_fut = async {
            if let (Some(mode), Some(mode_id)) = (need_mode, mode_to_send) {
                let r = conn
                    .request(
                        "session/set_mode",
                        json!({ "sessionId": sid, "modeId": mode_id }),
                        Some(Duration::from_secs(30)),
                    )
                    .await;
                match r {
                    Ok(_) => {
                        if let Some(route) = self.routes.lock().unwrap().get_mut(sid) {
                            route.applied_mode = Some(mode);
                        }
                    }
                    Err(e) => self.push_log(format!("[nova] 设置模式失败: {e}")),
                }
            }
        };
        tokio::join!(model_fut, mode_fut);
        self.push_log(format!(
            "[nova][timing] apply_session_config {}ms",
            t_cfg.elapsed().as_millis()
        ));
    }

    /// 在本后端实例上异步生成标题。model 非空时下发为标题会话模型（须为本后端的模型 id，
    /// ""=用本后端会话默认模型）。是否为 Cursor / 用哪个后端由上层 AppState::generate_title 路由决定。
    pub fn generate_title_async(
        self: &Arc<Self>,
        thread_id: String,
        prompt: String,
        fallback_title: String,
        model: String,
    ) {
        let prompt = prompt.trim().to_string();
        if prompt.is_empty() {
            return;
        }
        let mgr = self.clone();
        tauri::async_runtime::spawn(async move {
            if let Err(e) = mgr
                .generate_title(thread_id.clone(), prompt, fallback_title, model)
                .await
            {
                mgr.push_log(format!("[nova] 标题生成失败 {thread_id}: {e}"));
            }
        });
    }

    async fn generate_title(
        self: &Arc<Self>,
        thread_id: String,
        user_prompt: String,
        fallback_title: String,
        model: String,
    ) -> Result<(), String> {
        // CodeBuddy：标题生成另开 session 发一轮 prompt，走独立 AUX 连接（与用户线程连接隔离），
        // 用 AUX 闸门把标题/探测这类辅助任务串行化，不占用也不阻塞任何用户会话的并行。
        let aux = self.aux_key();
        let _gate = self.serial_gate(&aux).await;
        let conn = self.ensure_conn_for(&aux, None).await?;
        let cwd = std::env::temp_dir().join("Nova-title");
        std::fs::create_dir_all(&cwd).map_err(|e| format!("创建标题目录失败：{e}"))?;
        let resp = conn
            .request(
                "session/new",
                json!({ "cwd": cwd.to_string_lossy(), "mcpServers": [] }),
                Some(Duration::from_secs(180)),
            )
            .await
            .map_err(|e| format!("创建标题会话失败：{e}"))?;
        self.capture_options(&resp);
        let sid = resp["sessionId"]
            .as_str()
            .ok_or("session/new 未返回 sessionId")?
            .to_string();
        self.note_session_spawned(&aux);
        // CodeBuddy 标题会话占用独立 AUX 活跃位；OpenCode 与用户会话共用 SHARED，
        // 记录后可让下一轮用户 prompt 重新下发其线程模型。
        self.mark_active_session(&aux, &sid);
        // 标题用轻量模型生成：model 由上层按「标题后端」解析好后传入（已保证是本后端的模型 id），
        // 非空即下发；空则用本后端会话默认模型。
        if !model.is_empty() {
            conn.request(
                "session/set_config_option",
                json!({ "sessionId": sid, "configId": "model", "value": model }),
                Some(Duration::from_secs(30)),
            )
            .await
            .map_err(|e| format!("设置标题模型失败：{e}"))?;
        }
        self.title_jobs.lock().unwrap().insert(
            sid.clone(),
            TitleJob {
                thread_id,
                fallback_title,
                output: String::new(),
            },
        );
        let title_prompt = format!(
            "请为下面用户第一次提示词生成一个简短会话标题。\n只输出标题本身，不要解释，不要引号，不要句号。\n中文最多12个字，英文最多6个词。\n\n用户提示词：\n{}",
            user_prompt
        );
        let prompt = Self::build_prompt_blocks(&title_prompt, &[]);
        let result = conn
            .request(
                "session/prompt",
                json!({ "sessionId": sid, "prompt": prompt }),
                Some(Duration::from_secs(120)),
            )
            .await;
        self.complete_title_job(&sid);
        result.map(|_| ())
    }

    fn capture_title_update(&self, session_id: &str, update: &Value) -> bool {
        let mut jobs = self.title_jobs.lock().unwrap();
        let Some(job) = jobs.get_mut(session_id) else {
            return false;
        };
        if update["sessionUpdate"].as_str() == Some("agent_message_chunk") {
            let text = extract_text(&update["content"]);
            job.output.push_str(&text);
        }
        true
    }

    fn complete_title_job(&self, session_id: &str) {
        let Some(job) = self.title_jobs.lock().unwrap().remove(session_id) else {
            return;
        };
        let title = normalize_generated_title(&job.output, &job.fallback_title);
        if title == job.fallback_title {
            return;
        }
        let changed = {
            let state = self.app.state::<AppState>();
            let mut store = state.store.lock().unwrap();
            if let Some(thread) = store.get_mut(&job.thread_id) {
                if thread.title == "新会话" || thread.title == job.fallback_title {
                    thread.title = title;
                    true
                } else {
                    false
                }
            } else {
                false
            }
        };
        if changed {
            let state = self.app.state::<AppState>();
            state.store.lock().unwrap().save();
            let _ = self
                .app
                .emit(EV_TITLE_GENERATED, json!({ "threadId": job.thread_id }));
            let _ = self.app.emit(EV_THREADS, json!({}));
        }
    }

    /// 线程的模型/模式被修改后，若 session 已挂载则立即同步
    pub async fn sync_thread_config(self: &Arc<Self>, thread_id: &str) {
        let (sid, model, mode) = {
            let state = self.app.state::<AppState>();
            let store = state.store.lock().unwrap();
            let Some(thread) = store.get(thread_id) else {
                return;
            };
            let Some(sid) = thread.acp_session_id.clone() else {
                return;
            };
            (sid, thread.model.clone(), thread.mode.clone())
        };
        if !self.routes.lock().unwrap().contains_key(&sid) {
            return; // 未挂载，等下次 ensure_session 时应用
        }
        if self.kind == AgentKind::Cursor {
            let applied = self
                .routes
                .lock()
                .unwrap()
                .get(&sid)
                .and_then(|route| route.applied_model.clone());
            let target_is_cli = model.as_deref().is_some_and(is_cursor_cli_model_id);
            let applied_is_cli = applied.as_deref().is_some_and(is_cursor_cli_model_id);
            // CLI 完整变体只能在进程启动时指定。切入另一 CLI 变体，或从 CLI 变体切回
            // Auto，都重建本线程独占进程；下一条消息通过 handoff 注入已有上下文继续。
            if (target_is_cli && applied != model) || (model.is_none() && applied_is_cli) {
                let key = self.conn_key_for_thread(thread_id);
                self.kill_conn_key(&key).await;
                let state = self.app.state::<AppState>();
                let mut store = state.store.lock().unwrap();
                if let Some(thread) = store.get_mut(thread_id) {
                    thread.acp_session_id = None;
                    if !thread.items.is_empty() {
                        thread.handoff_from = Some(AgentKind::Cursor);
                    }
                }
                store.save();
                return;
            }
        }
        // CodeBuddy / OpenCode：配置切换不能与同连接正在跑的 prompt 重叠，用同一把连接闸门排队。
        // CodeBuddy 不同线程连接各自独立；OpenCode 所有线程共用 SHARED，借此避免模型串台。
        let key = self.conn_key_for_thread(thread_id);
        let _gate = self.serial_gate(&key).await;
        let Some(conn) = self.conn_for_key(&key).await else {
            return;
        };
        self.apply_session_config(&conn, &key, &sid, model, mode)
            .await;
    }

    async fn new_session_for(
        self: &Arc<Self>,
        conn: &Arc<AcpConn>,
        conn_key: &str,
        thread_id: &str,
        cwd: &str,
    ) -> Result<String, String> {
        // Cursor 等连云端建会话时偶发 ECONNRESET / PING timed out / Connection stalled /
        // Internal error；进程也可能瞬时退出。同连接退避重试，进程死了则拉起新进程再试，
        // 避免把瞬时抖动打成「创建会话失败」打断会话。
        let max_attempts: u32 = if self.kind == AgentKind::Cursor { 5 } else { 3 };
        let mut last_err = String::new();
        let mut resp = None;
        let mut conn = conn.clone();
        for attempt in 1..=max_attempts {
            if matches!(self.kind, AgentKind::CodeBuddy | AgentKind::Cursor)
                && conn_key != AUX_KEY
                && !self.is_running(conn_key)
            {
                return Err("任务已停止".into());
            }
            if !conn.alive.load(Ordering::SeqCst) {
                self.push_log(format!(
                    "[nova] session/new 前发现 {} 进程已退出，正在重启（第{attempt}/{max_attempts}次）",
                    self.kind.label()
                ));
                match self.ensure_conn_for(conn_key, Some(cwd)).await {
                    Ok(c) => conn = c,
                    Err(e) => {
                        last_err = e;
                        if attempt < max_attempts {
                            let delay_ms = retriable_backoff_ms(attempt);
                            sleep(Duration::from_millis(delay_ms)).await;
                            continue;
                        }
                        break;
                    }
                }
            }
            match conn
                .request(
                    "session/new",
                    json!({
                        "cwd": cwd,
                        "mcpServers": self.mcp_servers_for_thread(Some(thread_id))
                    }),
                    Some(Duration::from_secs(180)),
                )
                .await
            {
                Ok(r) => {
                    resp = Some(r);
                    break;
                }
                Err(e) => {
                    last_err = e;
                    let dead =
                        is_process_exit_error(&last_err) || !conn.alive.load(Ordering::SeqCst);
                    if attempt < max_attempts && (is_retriable_rpc_error(&last_err) || dead) {
                        let delay_ms = retriable_backoff_ms(attempt);
                        self.push_log(format!(
                            "[nova] session/new 瞬时失败（第{attempt}/{max_attempts}次）：{last_err}，{delay_ms}ms 后重试"
                        ));
                        if dead {
                            // 下一轮循环开头会 ensure_conn_for 拉起新进程
                            conn.alive.store(false, Ordering::SeqCst);
                        }
                        sleep(Duration::from_millis(delay_ms)).await;
                        continue;
                    }
                    break;
                }
            }
        }
        let resp = resp.ok_or_else(|| format!("创建会话失败：{last_err}"))?;
        self.capture_options(&resp);
        let sid = resp["sessionId"]
            .as_str()
            .ok_or("session/new 未返回 sessionId")?
            .to_string();
        self.routes.lock().unwrap().insert(
            sid.clone(),
            Route {
                thread_id: thread_id.to_string(),
                applied_model: self.cursor_startup_model_for_thread(thread_id),
                applied_mode: None,
            },
        );
        self.note_session_spawned(conn_key);
        // CodeBuddy 记录单活跃会话；OpenCode 记录共享连接最近影响模型作用域的会话。
        self.mark_active_session(conn_key, &sid);
        Ok(sid)
    }

    /// 在指定线程上执行一轮对话
    pub async fn run_prompt(
        self: &Arc<Self>,
        thread_id: String,
        text: String,
        images: Vec<PromptImage>,
    ) {
        // 必须在任何 await / running 置位之前记录：停止可能发生在本轮等待独占连接闸门时。
        let cancel_epoch = self.cancel_epoch(&thread_id);
        // 新会话的 Paper Trail / 跨 agent 接力上下文，在真实用户输入前隐式注入。
        let handoff = {
            let cursor_routes = (self.kind == AgentKind::Cursor).then(|| {
                self.routes
                    .lock()
                    .unwrap()
                    .keys()
                    .cloned()
                    .collect::<HashSet<_>>()
            });
            let state = self.app.state::<AppState>();
            let mut store = state.store.lock().unwrap();
            let mut consumed_marker = false;
            let ctx = store.get_mut(&thread_id).and_then(|t| {
                let had_handoff = t.handoff_from.is_some();
                if let Some(ctx) = t.take_prompt_context(self.kind.label()) {
                    consumed_marker = had_handoff;
                    return Some(ctx);
                }
                let needs_cursor_handoff = if self.kind == AgentKind::Cursor && !t.items.is_empty()
                {
                    match (cursor_routes.as_ref(), t.acp_session_id.as_deref()) {
                        (Some(routes), Some(sid)) => !routes.contains(sid),
                        (Some(_), None) => true,
                        _ => false,
                    }
                } else {
                    false
                };
                if needs_cursor_handoff {
                    t.handoff_from = Some(AgentKind::Cursor);
                    consumed_marker = true;
                    t.take_prompt_context(self.kind.label())
                } else {
                    None
                }
            });
            if consumed_marker {
                store.save();
            }
            ctx
        };
        // 1. 本地先落用户消息
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
        self.clear_plan(&thread_id);
        self.set_running(&thread_id, true, None);

        // CodeBuddy / Cursor：整轮独占「本线程自己的连接」，避免同一连接上的会话级操作重叠
        // 导致响应串台或卡死；也避免「停止后立刻重发」时旧轮次收尾误清新轮次的 running。
        // OpenCode：整轮独占共享连接，避免另一 session 在本轮中途切换连接级模型。Devin 等返回
        // None、不串行，保持多路复用。闸门在本函数作用域内持有，结束自动释放。
        let conn_key = self.conn_key_for_thread(&thread_id);
        let _gate = self.serial_gate(&conn_key).await;

        // 同线程排队 / 停止后重发：上一轮收尾可能已把 running 置为 false（或新轮次先置位后又
        // 被旧轮次 finish_turn 清掉），拿到闸门、真正开跑前重新置位并重置计时。
        if matches!(self.kind, AgentKind::CodeBuddy | AgentKind::Cursor) {
            if self.cancel_epoch(&thread_id) != cancel_epoch {
                // 本轮在等待闸门期间已被停止。cancel 已负责写结束状态和清 running；
                // 这里直接退出，禁止旧轮次“反弹”复活，导致用户必须再点一次停止。
                return;
            }
            self.set_running(&thread_id, true, None);
        }

        // 标题生成按设置里的「标题后端 + 标题模型」路由（默认 Devin），在目标后端另开 session
        // 发一轮轻量 prompt，与本轮主 prompt 互不干扰（各后端连接/闸门独立）。
        if let Some((prompt, fallback)) = title_job {
            self.app.state::<AppState>().generate_title(
                &self.kind,
                thread_id.clone(),
                prompt,
                fallback,
            );
        }

        // 记录开轮前的条目数：用于判断本轮 agent 是否真的产出了内容（助手消息/思考/工具）
        let items_before = {
            let state = self.app.state::<AppState>();
            let store = state.store.lock().unwrap();
            store.get(&thread_id).map(|t| t.items.len()).unwrap_or(0)
        };

        let outcome = self
            .drive_prompt(&thread_id, &text, &images, handoff.as_deref())
            .await;

        // 轮次已被强制结束（看门狗/重启 devin），丢弃迟到的结果
        if !self.is_running(&thread_id) {
            let state = self.app.state::<AppState>();
            let store = state.store.lock().unwrap();
            store.save();
            return;
        }

        let (stop_reason, usage) = match outcome {
            Ok((stop, usage)) => {
                // 兜底「发送无反应」：轮次正常结束但没有任何新内容（典型如模型 refusal、
                // 模型不可用/无权限）。给用户一条明确提示，避免界面看起来「毫无反应」。
                let produced = {
                    let state = self.app.state::<AppState>();
                    let store = state.store.lock().unwrap();
                    store
                        .get(&thread_id)
                        .map(|t| t.items.len() > items_before)
                        .unwrap_or(true)
                };
                if !produced {
                    let note = if stop == "refusal" {
                        "模型拒绝了本次请求，未返回任何内容。常见原因：所选模型当前不可用或无权限。请在下方切换其他模型后重试。"
                    } else {
                        "本轮没有返回任何内容。可尝试在下方切换模型或重新发送。"
                    };
                    let state = self.app.state::<AppState>();
                    let mut store = state.store.lock().unwrap();
                    if let Some(thread) = store.get_mut(&thread_id) {
                        let item = thread.push_system(note.to_string(), "warn");
                        self.emit_update(&thread_id, json!({ "t": "upsert", "item": item }));
                    }
                }
                (stop, usage)
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
        // 轮次刚结束的时刻作为该连接的空闲起点（供 CodeBuddy 空闲回收器计时）
        self.touch_conn(&conn_key);
        let _ = self.app.emit(EV_THREADS, json!({}));
    }

    /// 构建 session/prompt 的 content blocks（文本 + 附件）。
    fn build_prompt_blocks(text: &str, images: &[PromptImage]) -> Vec<Value> {
        let mut prompt: Vec<Value> = Vec::new();
        if !text.is_empty() {
            prompt.push(json!({ "type": "text", "text": text }));
        }
        // 图片可读成 image block；普通文件只作为 resource_link 传给 devin，不在客户端展开内容。
        let mut saved: Vec<String> = Vec::new();
        for img in images {
            if img.mime_type.starts_with("image/") {
                if let Some(data) = prompt_image_data(img) {
                    let mut block = json!({
                        "type": "image",
                        "mimeType": img.mime_type,
                        "data": data
                    });
                    if let Some(uri) = &img.uri {
                        block["uri"] = json!(uri);
                        if let Some(path) = file_uri_to_path(uri) {
                            saved.push(path);
                        }
                    } else if let Some(path) = save_prompt_image(img) {
                        block["uri"] = json!(format!("file:///{}", path.replace('\\', "/")));
                        saved.push(path);
                    }
                    prompt.push(block);
                    continue;
                }
            }
            // 非图片文件：本机 uri 直接用；漫游/分享只带 data 时先落临时文件再引用
            let file_uri = if let Some(uri) = &img.uri {
                Some(uri.clone())
            } else {
                crate::threads::save_attachment_to_temp(img)
                    .map(|p| format!("file:///{}", p.replace('\\', "/")))
            };
            if let Some(uri) = file_uri {
                let mut block = json!({
                    "type": "resource_link",
                    "uri": uri,
                    "name": img.name,
                    "mimeType": img.mime_type
                });
                if let Some(size) = attachment_size(img) {
                    block["size"] = json!(size);
                }
                prompt.push(block);
            }
        }
        if !saved.is_empty() {
            prompt.push(json!({
                "type": "text",
                "text": format!(
                    "（用户随消息附带了 {} 张图片，本地文件路径：\n{}\n若你已能直接看到图片内容，忽略本段。若看不到，可用读取工具打开上述文件；\
                     若读取后仍只得到 [Image N] 占位符而看不到实际画面，说明当前模型不支持图片输入——\
                     请如实告知用户并建议换用支持视觉的模型（如 Claude 系列），切勿凭空猜测图片内容。）",
                    saved.len(),
                    saved.join("\n")
                )
            }));
        }
        prompt
    }

    fn build_user_prompt_blocks(
        text: &str,
        images: &[PromptImage],
        include_runtime_guidance: bool,
    ) -> Vec<Value> {
        let mut prompt = Self::build_prompt_blocks(text, images);
        if include_runtime_guidance {
            if let Some(guidance) = devin_runtime_guidance() {
                prompt.insert(0, json!({ "type": "text", "text": guidance }));
            }
        }
        prompt
    }

    /// 运行中追加提示（引导）：向当前活跃 session 直接注入新的 session/prompt。
    /// devin 会把它合并进当前轮次（实测：注入请求与主请求在轮次结束时返回同一结果），
    /// 因此这里只落库用户消息并发出请求，轮次收尾仍由主 drive 负责。
    pub async fn steer_prompt(
        self: &Arc<Self>,
        thread_id: String,
        text: String,
        images: Vec<PromptImage>,
    ) {
        // 首条消息的 session 可能还在建立中，短暂等待
        let mut session_id: Option<String> = None;
        for _ in 0..20 {
            session_id = {
                let state = self.app.state::<AppState>();
                let store = state.store.lock().unwrap();
                store.get(&thread_id).and_then(|t| t.acp_session_id.clone())
            };
            if session_id.is_some() || !self.is_running(&thread_id) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        // 「停止 → 立刻重发」竞态：路由到这里时轮次还在跑，但此刻 cancel 已落地。
        // 再注入只会随被取消的轮次一起丢弃（消息上屏却永远没有回应，表现为发送失败），
        // 改走正常新轮次。
        if !self.is_running(&thread_id) {
            self.clone().run_prompt(thread_id, text, images).await;
            return;
        }
        let err = |msg: String| {
            let state = self.app.state::<AppState>();
            let mut store = state.store.lock().unwrap();
            if let Some(thread) = store.get_mut(&thread_id) {
                let item = thread.push_system(msg, "error");
                store.save();
                self.emit_update(&thread_id, json!({ "t": "upsert", "item": item }));
            }
        };
        let Some(session_id) = session_id else {
            err("引导消息发送失败：会话尚未建立".into());
            return;
        };
        let Some(conn) = self
            .conn_for_key(&self.conn_key_for_thread(&thread_id))
            .await
        else {
            err(format!("引导消息发送失败：{} 未连接", self.kind.label()));
            return;
        };
        // 落库并立刻显示用户消息
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
        let _ = self.app.emit(EV_THREADS, json!({}));
        let prompt = Self::build_prompt_blocks(&text, &images);
        let mgr = self.clone();
        let tid = thread_id.clone();
        // 该请求要到轮次结束才返回（与主 prompt 一同返回），结果由主 drive 收尾，这里只记录失败
        tauri::async_runtime::spawn(async move {
            if let Err(e) = conn
                .request(
                    "session/prompt",
                    json!({ "sessionId": session_id, "prompt": prompt }),
                    None,
                )
                .await
            {
                mgr.push_log(format!("[nova] 引导消息发送失败 {tid}: {e}"));
                // 注入随轮次一起夭折（如注入后用户立刻停止/连接被杀）：轮次已结束的话，
                // 这条消息不会再有任何回应，明确提示用户重发，避免看起来「发出去但没反应」。
                if !mgr.is_running(&tid) {
                    let state = mgr.app.state::<AppState>();
                    let mut store = state.store.lock().unwrap();
                    if let Some(thread) = store.get_mut(&tid) {
                        let item = thread.push_system(
                            "上一条消息随已停止的任务一起中断了，未被处理，请重新发送。".into(),
                            "warn",
                        );
                        store.save();
                        mgr.emit_update(&tid, json!({ "t": "upsert", "item": item }));
                    }
                }
            }
        });
    }

    async fn drive_prompt(
        self: &Arc<Self>,
        thread_id: &str,
        text: &str,
        images: &[PromptImage],
        handoff: Option<&str>,
    ) -> Result<(String, Option<Value>), String> {
        let include_runtime_guidance = {
            let state = self.app.state::<AppState>();
            let store = state.store.lock().unwrap();
            let sid = store.get(thread_id).and_then(|t| t.acp_session_id.clone());
            drop(store);
            match sid {
                Some(sid) if self.kind == AgentKind::Cursor => {
                    !self.routes.lock().unwrap().contains_key(&sid)
                }
                Some(_) => false,
                None => true,
            }
        };
        let t_ensure = std::time::Instant::now();
        let mut session_id = self.ensure_session(thread_id).await?;
        if !self.is_running(thread_id) {
            return Err("任务已停止".into());
        }
        self.push_log(format!(
            "[nova][timing] ensure_session {}ms (新会话={})",
            t_ensure.elapsed().as_millis(),
            include_runtime_guidance
        ));
        let conn_key = self.conn_key_for_thread(thread_id);
        // ensure_session 返回后进程可能恰好退出；Cursor 交给下面的重建分支恢复。
        let mut conn = self.conn_for_key(&conn_key).await;
        if conn.is_none() && self.kind != AgentKind::Cursor {
            return Err(format!("{} 未连接", self.kind.label()));
        }
        let mut prompt = Self::build_user_prompt_blocks(text, images, include_runtime_guidance);
        if let Some(ctx) = handoff {
            prompt.insert(0, json!({ "type": "text", "text": ctx }));
        }
        // Cursor 云端偶发 PING timed out / Connection stalled / Internal error。
        // 同 session 上透明重试；进程退出则清 session、拉起新进程再建会话后重发。
        // 若本轮已有流式输出再失败：不再重发同一 prompt（会重复），软收尾保留会话。
        let items_at_prompt = {
            let state = self.app.state::<AppState>();
            let store = state.store.lock().unwrap();
            store.get(thread_id).map(|t| t.items.len()).unwrap_or(0)
        };
        let max_attempts: u32 = if self.kind == AgentKind::Cursor { 5 } else { 3 };
        let mut last_err = if conn.is_none() {
            format!("{} 未连接", self.kind.label())
        } else {
            String::new()
        };
        for attempt in 1..=max_attempts {
            if !self.is_running(thread_id) {
                return Err("任务已停止".into());
            }
            let needs_rebuild = prompt_conn_needs_rebuild(
                conn.as_ref().map(|conn| conn.alive.load(Ordering::SeqCst)),
                &last_err,
            );
            if needs_rebuild && (attempt > 1 || conn.is_none()) {
                // 已有输出时不重建（上面 soft-finish 会先 return）；此处仅无输出场景。
                let produced = {
                    let state = self.app.state::<AppState>();
                    let store = state.store.lock().unwrap();
                    store
                        .get(thread_id)
                        .map(|t| t.items.len() > items_at_prompt)
                        .unwrap_or(false)
                };
                if produced {
                    break;
                }
                self.push_log(format!(
                    "[nova] {} 连接不可用，重建会话后重试 prompt（第{attempt}/{max_attempts}次）：{last_err}",
                    self.kind.label()
                ));
                if let Some(conn) = conn.as_ref() {
                    conn.kill();
                }
                self.clear_thread_session_for_respawn(thread_id);
                // Cursor session/load 不可靠：把已有历史注入 prompt，避免新会话空上下文。
                if handoff.is_none() && self.kind == AgentKind::Cursor {
                    let ctx = {
                        let state = self.app.state::<AppState>();
                        let mut store = state.store.lock().unwrap();
                        store.get_mut(thread_id).and_then(|thread| {
                            thread.handoff_from = Some(AgentKind::Cursor);
                            thread.take_prompt_context(self.kind.label())
                        })
                    };
                    if let Some(ctx) = ctx {
                        prompt =
                            Self::build_user_prompt_blocks(text, images, include_runtime_guidance);
                        prompt.insert(0, json!({ "type": "text", "text": ctx }));
                    }
                }
                session_id = self.ensure_session(thread_id).await?;
                conn = self.conn_for_key(&conn_key).await;
                if conn.is_none() {
                    last_err = format!("{} 未连接", self.kind.label());
                    if attempt < max_attempts {
                        let delay_ms = retriable_backoff_ms(attempt);
                        self.push_log(format!(
                            "[nova] 重建后连接仍不可用（第{attempt}/{max_attempts}次），{delay_ms}ms 后重试"
                        ));
                        sleep(Duration::from_millis(delay_ms)).await;
                        continue;
                    }
                    break;
                }
                last_err.clear();
            } else if conn
                .as_ref()
                .map(|conn| !conn.alive.load(Ordering::SeqCst))
                .unwrap_or(true)
            {
                // 首轮就发现进程已死：走与上面相同的重建路径（计入 attempt）
                last_err = format!("{} 进程已退出", self.kind.label());
                if attempt < max_attempts {
                    let delay_ms = retriable_backoff_ms(attempt);
                    self.push_log(format!(
                        "[nova] session/prompt 前进程已退出（第{attempt}/{max_attempts}次），{delay_ms}ms 后重建"
                    ));
                    sleep(Duration::from_millis(delay_ms)).await;
                    continue;
                }
                break;
            }
            let Some(conn) = conn.as_ref() else {
                last_err = format!("{} 未连接", self.kind.label());
                continue;
            };
            self.prompt_sent_at
                .lock()
                .unwrap()
                .insert(session_id.clone(), std::time::Instant::now());
            match conn
                .request(
                    "session/prompt",
                    json!({
                        "sessionId": session_id,
                        "prompt": prompt
                    }),
                    None,
                )
                .await
            {
                Ok(resp) => {
                    let stop = resp["stopReason"]
                        .as_str()
                        .unwrap_or("end_turn")
                        .to_string();
                    let usage = resp.get("usage").cloned().filter(|v| !v.is_null());
                    return Ok((stop, usage));
                }
                Err(e) => {
                    last_err = e;
                    let dead =
                        is_process_exit_error(&last_err) || !conn.alive.load(Ordering::SeqCst);
                    if !is_retriable_rpc_error(&last_err) && !dead {
                        break;
                    }
                    let produced = {
                        let state = self.app.state::<AppState>();
                        let store = state.store.lock().unwrap();
                        store
                            .get(thread_id)
                            .map(|t| t.items.len() > items_at_prompt)
                            .unwrap_or(false)
                    };
                    if produced {
                        // 已有部分输出：保留会话与已生成内容，不当硬错误打断。
                        self.push_log(format!(
                            "[nova] session/prompt 云端中断但已有输出，软收尾保留会话：{last_err}"
                        ));
                        {
                            let state = self.app.state::<AppState>();
                            let mut store = state.store.lock().unwrap();
                            if let Some(thread) = store.get_mut(thread_id) {
                                let item = thread.push_system(
                                    "云端连接短暂中断，本轮已保留已生成内容；可直接继续发送。"
                                        .into(),
                                    "warn",
                                );
                                self.emit_update(thread_id, json!({ "t": "upsert", "item": item }));
                            }
                            store.save();
                        }
                        return Ok(("end_turn".into(), None));
                    }
                    if attempt < max_attempts {
                        let delay_ms = retriable_backoff_ms(attempt);
                        self.push_log(format!(
                            "[nova] session/prompt 瞬时失败（第{attempt}/{max_attempts}次）：{last_err}，{delay_ms}ms 后重试"
                        ));
                        sleep(Duration::from_millis(delay_ms)).await;
                        continue;
                    }
                }
            }
        }
        Err(last_err)
    }

    /// 进程崩溃后清掉线程上的 ACP session，下次 ensure_session 会新建（Cursor 的
    /// session/load 不可靠，不能指望在旧 sessionId 上恢复）。
    fn clear_thread_session_for_respawn(&self, thread_id: &str) {
        let state = self.app.state::<AppState>();
        let mut store = state.store.lock().unwrap();
        if let Some(thread) = store.get_mut(thread_id) {
            if let Some(sid) = thread.acp_session_id.take() {
                self.routes.lock().unwrap().remove(&sid);
            }
        }
        store.save();
    }

    /// 强制本地结束一个轮次（devin 不响应 cancel 或网络卡死时的兜底）
    async fn force_finish(&self, thread_id: &str, msg: &str) {
        self.mark_plan_interrupted(thread_id, "cancelled", true);
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

    pub async fn cancel(self: &Arc<Self>, thread_id: &str) {
        if !self.is_running(thread_id) {
            return;
        }
        if matches!(self.kind, AgentKind::CodeBuddy | AgentKind::Cursor) {
            self.bump_cancel_epoch(thread_id);
        }

        let conn_key = self.conn_key_for_thread(thread_id);
        let session_id = {
            let state = self.app.state::<AppState>();
            let store = state.store.lock().unwrap();
            store.get(thread_id).and_then(|t| t.acp_session_id.clone())
        };

        // 该会话所有未决权限请求回 cancelled（用收到它的那条连接回复，多连接下不能假设「当前连接」）
        if let Some(sid) = &session_id {
            let to_cancel: Vec<(String, PendingPermission)> = {
                let mut perms = self.pending_permissions.lock().unwrap();
                let keys: Vec<String> = perms
                    .iter()
                    .filter(|(_, p)| &p.session_id == sid)
                    .map(|(k, _)| k.clone())
                    .collect();
                keys.into_iter()
                    .filter_map(|k| perms.remove(&k).map(|p| (k, p)))
                    .collect()
            };
            for (key, perm) in to_cancel {
                perm.conn.respond_ok(
                    perm.rpc_id,
                    json!({ "outcome": { "outcome": "cancelled" } }),
                );
                let _ = self
                    .app
                    .emit(EV_PERMISSION_RESOLVED, json!({ "requestKey": key }));
            }
        }

        // CodeBuddy / Cursor：该线程独占一条连接（一个进程），直接 kill 即可立即停手，
        // 且完全不影响其它并行会话。
        // - CodeBuddy：下次发送自动重连并经 session/load 恢复上下文。
        // - Cursor：session/cancel 不可靠（界面已停但 agent 仍在跑），随后重发会撞上
        //   仍活跃的 turn 报内部错误；session/load 也有缺陷，不能软取消后复用。
        //   必须硬杀进程；杀后清掉 session，下次发送走 handoff 注入上下文。
        if matches!(self.kind, AgentKind::CodeBuddy | AgentKind::Cursor) {
            self.force_finish(thread_id, "已停止当前任务。").await;
            self.kill_conn_key(&conn_key).await;
            if self.kind == AgentKind::Cursor {
                let state = self.app.state::<AppState>();
                let mut store = state.store.lock().unwrap();
                if let Some(thread) = store.get_mut(thread_id) {
                    thread.acp_session_id = None;
                    if !thread.items.is_empty() {
                        thread.handoff_from = Some(AgentKind::Cursor);
                    }
                }
                store.save();
            }
            // 不在这里排空串行闸门：kill 后旧 run_prompt 会很快失败并释放闸门；
            // 重发方的 run_prompt 会在同一把闸门上排队。若 cancel 再 acquire，
            // 可能插到「已重发的新轮次」后面，把停止操作卡住整轮。
            return;
        }

        // Devin / ClaudeCode / OpenCode：共享连接上多 session 复用。
        let Some(session_id) = session_id else {
            // 还没建立 session 就要停（如卡在 session/new）：直接本地结束
            if self.is_running(thread_id) {
                self.force_finish(thread_id, "已停止。").await;
            }
            return;
        };
        if let Some(conn) = self.conn_for_key(&conn_key).await {
            conn.notify("session/cancel", json!({ "sessionId": session_id }));
        }
        // 不再 kill 整条连接（旧行为仅在「没有其它会话」时硬杀进程）：
        // 硬杀后下一次发送必须冷启动——初始化、重建会话是整条链路里最容易
        // 失败的环节，表现为「停止后第一次发送失败、第二次才成功」。
        // 统一改为：协议级 session/cancel 尽力而为 + 本地立即结束 + 忘掉该 session
        // 的路由（对 cancel 支持不稳定，迟到的 update 会被忽略，停止在界面上
        // 立即生效）。连接保持热存活，下次发送直接复用；session 仍留在 agent 侧，
        // 可经 session/load 恢复上下文。
        self.force_finish(thread_id, "已停止当前任务。").await;
        self.forget_session_of_thread(thread_id);
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
        let outcome = if option_id.is_empty() {
            json!({ "outcome": "cancelled" })
        } else {
            json!({ "outcome": "selected", "optionId": option_id })
        };
        // 用「收到该请求的那条连接」回复，多连接下不能假设是某个当前连接。
        perm.conn
            .respond_ok(perm.rpc_id, json!({ "outcome": outcome }));
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

    pub fn forget_session_of_thread(self: &Arc<Self>, thread_id: &str) {
        self.routes
            .lock()
            .unwrap()
            .retain(|_, r| r.thread_id != thread_id);
        self.thread_locks.lock().unwrap().remove(thread_id);
        // CodeBuddy / Cursor：线程独占一条连接，线程删除/切换后端时一并回收。
        // Devin 等共用 SHARED 连接，绝不能在这里 kill。
        if matches!(self.kind, AgentKind::CodeBuddy | AgentKind::Cursor) {
            let me = self.clone();
            let key = self.conn_key_for_thread(thread_id);
            // 线程已废弃：闸门也可回收（kill_conn_key 有意保留闸门供「停止→重发」同步）。
            self.serial_gates.lock().unwrap().remove(&key);
            tauri::async_runtime::spawn(async move {
                me.kill_conn_key(&key).await;
            });
        }
    }
}

fn config_option<'a>(value: &'a Value, id: &str) -> Option<&'a Value> {
    let options = value
        .as_array()
        .or_else(|| value.get("configOptions").and_then(|v| v.as_array()))?;
    options
        .iter()
        .find(|option| option.get("id").and_then(|v| v.as_str()) == Some(id))
}

fn config_option_mut<'a>(value: &'a mut Value, id: &str) -> Option<&'a mut Value> {
    let options = if value.is_array() {
        value.as_array_mut()?
    } else {
        value.get_mut("configOptions")?.as_array_mut()?
    };
    options
        .iter_mut()
        .find(|option| option.get("id").and_then(|v| v.as_str()) == Some(id))
}

fn config_choices(value: &Value, id: &str) -> Vec<Value> {
    config_option(value, id)
        .and_then(|option| option.get("options"))
        .and_then(|options| options.as_array())
        .cloned()
        .unwrap_or_default()
}

fn opencode_model_choices(value: &Value) -> Vec<Value> {
    config_choices(value, "model")
}

fn opencode_effort_choices(value: &Value) -> Vec<Value> {
    config_choices(value, "effort")
}

fn set_json_meta(value: &mut Value, key: &str, meta_value: Value) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    let meta = object.entry("_meta").or_insert_with(|| json!({}));
    if !meta.is_object() {
        *meta = json!({});
    }
    if let Some(meta) = meta.as_object_mut() {
        meta.insert(key.to_string(), meta_value);
    }
}

fn opencode_variant_label(value: &str, name: &str) -> String {
    match value.to_ascii_lowercase().as_str() {
        "none" => "None".into(),
        "minimal" => "Minimal".into(),
        "low" => "Low".into(),
        "medium" => "Medium".into(),
        "high" => "High".into(),
        "xhigh" | "extra-high" | "extra_high" => "XHigh".into(),
        "max" => "Max".into(),
        _ if !name.trim().is_empty() => name.trim().to_string(),
        _ => value.to_string(),
    }
}

/// 把 OpenCode 的独立 effort 选项展开成可持久化、可直接传给 session/set_model 的
/// provider/model/variant。基座项保留为 Default，兼容旧会话里没有 variant 的模型值。
fn expand_opencode_model_choice(model: &Value, efforts: &[Value]) -> Vec<Value> {
    let Some(base_id) = model.get("value").and_then(|v| v.as_str()) else {
        return vec![];
    };
    let Some(base_name) = model.get("name").and_then(|v| v.as_str()) else {
        return vec![model.clone()];
    };
    if efforts.is_empty() {
        return vec![model.clone()];
    }

    let mut base = model.clone();
    base["name"] = json!(format!("{base_name} · Default"));
    set_json_meta(&mut base, OPENCODE_BASE_MODEL_META, json!(base_id));
    let mut choices = vec![base];

    for effort in efforts {
        let Some(variant) = effort.get("value").and_then(|v| v.as_str()) else {
            continue;
        };
        if variant.is_empty() || variant.eq_ignore_ascii_case("default") {
            continue;
        }
        let effort_name = effort
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or(variant);
        let mut choice = model.clone();
        choice["value"] = json!(format!("{base_id}/{variant}"));
        choice["name"] = json!(format!(
            "{base_name} · {}",
            opencode_variant_label(variant, effort_name)
        ));
        set_json_meta(&mut choice, OPENCODE_BASE_MODEL_META, json!(base_id));
        set_json_meta(&mut choice, OPENCODE_VARIANT_META, json!(variant));
        choices.push(choice);
    }
    choices
}

fn opencode_variant_groups(value: &Value) -> HashMap<String, Vec<Value>> {
    let mut groups: HashMap<String, Vec<Value>> = HashMap::new();
    for choice in opencode_model_choices(value) {
        let Some(base) = choice
            .get("_meta")
            .and_then(|meta| meta.get(OPENCODE_BASE_MODEL_META))
            .and_then(|v| v.as_str())
        else {
            continue;
        };
        groups.entry(base.to_string()).or_default().push(choice);
    }
    groups
}

fn opencode_catalog_is_expanded(config_options: &Value) -> bool {
    !opencode_variant_groups(config_options).is_empty()
}

fn merge_cached_opencode_variants(config_options: &mut Value, cached: &Value) {
    let groups = opencode_variant_groups(cached);
    if groups.is_empty() {
        return;
    }
    let Some(model_option) = config_option_mut(config_options, "model") else {
        return;
    };
    let Some(base_models) = model_option
        .get("options")
        .and_then(|options| options.as_array())
        .cloned()
    else {
        return;
    };

    let mut merged = Vec::new();
    for model in base_models {
        let Some(base_id) = model.get("value").and_then(|v| v.as_str()) else {
            continue;
        };
        if let Some(variants) = groups.get(base_id) {
            merged.extend(variants.clone());
        } else {
            merged.push(model);
        }
    }
    model_option["options"] = Value::Array(merged);
}

/// Windows：在 PATH 中按 exe/cmd/bat 顺序解析裸命令名为具体文件路径。
/// 带路径分隔符的输入视为具体文件；仅带扩展名的裸文件名仍需搜索 PATH。
/// 也被「后端可用性检查」复用：零成本判断某个 CLI 是否安装（不拉起进程）。
#[cfg(windows)]
pub(crate) fn resolve_program_on_path(name: &str) -> Option<std::path::PathBuf> {
    use std::path::{Path, PathBuf};
    if name.contains('\\') || name.contains('/') {
        let p = PathBuf::from(name);
        return p.is_file().then_some(p);
    }
    if Path::new(name).extension().is_some() {
        let p = PathBuf::from(name);
        if p.is_file() {
            return Some(p);
        }
        return std::env::var_os("PATH").and_then(|paths| {
            std::env::split_paths(&paths)
                .map(|dir| dir.join(name))
                .find(|p| p.is_file())
        });
    }
    let exts = ["exe", "cmd", "bat"];
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths).find_map(|dir| {
            exts.iter()
                .map(|ext| dir.join(format!("{name}.{ext}")))
                .find(|p| p.is_file())
        })
    })
}

/// 非 Windows：在 PATH 中解析裸命令名（带路径分隔符的输入视为具体文件）。
#[cfg(not(windows))]
pub(crate) fn resolve_program_on_path(name: &str) -> Option<std::path::PathBuf> {
    use std::path::PathBuf;
    if name.contains('/') {
        let p = PathBuf::from(name);
        return p.is_file().then_some(p);
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths).find_map(|dir| {
            let p = dir.join(name);
            p.is_file().then_some(p)
        })
    })
}

#[cfg(windows)]
fn cursor_windows_hide_patch_path(app: &AppHandle) -> Result<PathBuf, String> {
    CURSOR_WINDOWS_HIDE_PATCH_PATH
        .get_or_init(|| {
            let dir = nova_data_dir(app).join("runtime");
            std::fs::create_dir_all(&dir).map_err(|e| format!("创建 Cursor 运行目录失败：{e}"))?;
            let path = dir.join("cursor-windows-hide.cjs");
            let current = std::fs::read_to_string(&path).ok();
            if current.as_deref() != Some(CURSOR_WINDOWS_HIDE_PATCH) {
                std::fs::write(&path, CURSOR_WINDOWS_HIDE_PATCH)
                    .map_err(|e| format!("写入 Cursor 无闪屏补丁失败：{e}"))?;
            }
            Ok(path)
        })
        .clone()
}

#[cfg(windows)]
fn cursor_windows_hide_node_options(existing: Option<&str>, patch: &Path) -> String {
    let patch = patch
        .to_string_lossy()
        .replace('\\', "/")
        .replace('"', "\\\"");
    let require = format!("--require \"{patch}\"");
    match existing.map(str::trim).filter(|value| !value.is_empty()) {
        Some(existing) => format!("{existing} {require}"),
        None => require,
    }
}

#[cfg(windows)]
fn apply_cursor_windows_hide_patch(
    app: &AppHandle,
    cmd: &mut tokio::process::Command,
    launch_env: &HashMap<String, String>,
) -> Result<(), String> {
    let patch = cursor_windows_hide_patch_path(app)?;
    let existing = launch_env
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("NODE_OPTIONS"))
        .map(|(_, value)| value.clone())
        .or_else(|| std::env::var("NODE_OPTIONS").ok());
    cmd.env(
        "NODE_OPTIONS",
        cursor_windows_hide_node_options(existing.as_deref(), &patch),
    );
    Ok(())
}

#[cfg(all(test, windows))]
mod cursor_windows_hide_tests {
    use super::*;

    #[test]
    fn node_options_preserve_existing_flags_and_quote_patch_path() {
        let patch = Path::new(r"C:\Nova Data\cursor-windows-hide.cjs");
        assert_eq!(
            cursor_windows_hide_node_options(Some("--trace-warnings"), patch),
            r#"--trace-warnings --require "C:/Nova Data/cursor-windows-hide.cjs""#
        );
    }
}

fn cursor_node_path_for_program(program: &str) -> Option<PathBuf> {
    let launcher = resolve_program_on_path(program)?;
    let node_name = if cfg!(windows) { "node.exe" } else { "node" };
    if launcher
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case(node_name))
    {
        return Some(launcher);
    }
    let root = launcher.parent()?;
    let direct = root.join(node_name);
    if direct.is_file() {
        return Some(direct);
    }
    let bundled = std::fs::read_dir(root.join("versions"))
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path().join(node_name))
        .filter(|path| path.is_file())
        .max_by(|left, right| left.parent().cmp(&right.parent()));
    bundled.or_else(|| resolve_program_on_path("node"))
}

/// Windows：构造 ACP agent 启动命令。
/// - 解析到 .exe（如 devin.exe）：直接启动，与原有行为一致；
/// - 解析到 .cmd/.bat 垫片（如 npx.cmd）：经 `cmd /D /S /C` 启动，借 cmd 的 PATHEXT 解析，
///   用裸命令名而非带空格的完整路径，规避 cmd 的引号陷阱；
/// - 找不到：退回直接用原名，spawn 时给出清晰错误。
#[cfg(windows)]
fn build_acp_command(program: &str, args_str: &str) -> tokio::process::Command {
    let args: Vec<&str> = args_str.split_whitespace().collect();
    match resolve_program_on_path(program) {
        Some(p)
            if p.extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("exe"))
                .unwrap_or(false) =>
        {
            let mut cmd = tokio::process::Command::new(p);
            cmd.args(&args);
            cmd
        }
        Some(_) => {
            let mut cmd = tokio::process::Command::new("cmd");
            cmd.arg("/D").arg("/S").arg("/C").arg(program).args(&args);
            cmd
        }
        None => {
            let mut cmd = tokio::process::Command::new(program);
            cmd.args(&args);
            cmd
        }
    }
}

/// 给子进程注入代理环境变量（HTTP_PROXY / HTTPS_PROXY / ALL_PROXY 及小写变体）。
/// proxy 为空则不覆盖；无协议前缀时按 http 代理处理。
pub(crate) fn apply_proxy_env(cmd: &mut tokio::process::Command, proxy: &str) {
    let proxy = proxy.trim();
    if proxy.is_empty() {
        return;
    }
    let proxy = if proxy.contains("://") {
        proxy.to_string()
    } else {
        format!("http://{proxy}")
    };
    for key in [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
    ] {
        cmd.env(key, &proxy);
    }
}

/// 运行 `<cursor_path> models` 拉取完整模型目录（纯本地 CLI 列表查询，无任何 prompt/费用）。
/// 输出形如 `gpt-5.2 - GPT-5.2`、`composer-2.5-fast - Composer 2.5 Fast (default)`。
async fn run_cursor_models_cli(
    program: &str,
    proxy: &str,
) -> Result<Vec<(String, String)>, String> {
    #[cfg(windows)]
    let mut cmd = build_acp_command(program, "models");
    #[cfg(not(windows))]
    let mut cmd = {
        let mut c = tokio::process::Command::new(program);
        c.arg("models");
        c
    };
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    apply_proxy_env(&mut cmd, proxy);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    let out = tokio::time::timeout(Duration::from_secs(60), cmd.output())
        .await
        .map_err(|_| "cursor-agent models 超时（60s）".to_string())?
        .map_err(|e| format!("启动 cursor-agent models 失败：{e}"))?;
    if !out.status.success() {
        return Err(format!(
            "cursor-agent models 退出码 {:?}",
            out.status.code()
        ));
    }
    Ok(parse_cursor_models(&String::from_utf8_lossy(&out.stdout)))
}

/// 解析 `cursor-agent models` 的输出为 (id, 显示名) 列表。
/// 跳过表头/提示行与 `auto`（由「Auto（Cursor 默认）」占位项承担）；
/// 并对齐 Devin 目录的呈现方式做规整：
/// - 显示名统一为「基础名 [None|Low|Medium|High|XHigh|Max] [Fast] [尾注]」——思考强度与
///   Fast 模式一律按 id 显式标出（CLI 原始名时有时无、词序不一，如 "Opus 4.8 1M
///   Thinking" 实为 high 档、"Fable 5 1M Extra High Thinking" 强度词插在中间、
///   gpt-5.5-extra-high 的原始名只写了 "High"）；
/// - 排序按「基础模型（首次出现序）→ 常规/Fast → 思考强度升序」聚拢，同族变体相邻。
///
/// 少数家族的 **id 后缀档位** 与 **CLI 营销名档位** 整体错开一档（如 grok：
/// id `grok-4.5-medium/high/xhigh` 的营销名却是 "Low/Medium/(顶档无后缀)"，
/// Cursor GUI 里也只有三档、根本没有 "XHigh"）。若同一基座下 ≥2 个变体的
/// 「营销名档位 − id 档位」是同一个非零常量，就判定该家族整体偏移，显示名按营销
/// 档位（= id 档位 + 偏移）标注，避免造出 Cursor 里不存在的档位。id 本身不改：
/// 实际启动仍用真实 CLI id（`--model grok-4.5-xhigh`）。
fn parse_cursor_models(out: &str) -> Vec<(String, String)> {
    struct Raw {
        id: String,
        name: String,
        base_key: String,
        id_effort: Option<usize>,
        name_effort: Option<usize>,
        fast: bool,
    }
    let mut raws: Vec<Raw> = Vec::new();
    let mut base_order: Vec<String> = Vec::new();
    for line in out.lines() {
        // 去掉可能的 ANSI 颜色码
        let line = strip_ansi(line);
        let line = line.trim();
        let Some((id, name)) = line.split_once(" - ") else {
            continue;
        };
        let id = id.trim();
        if id.is_empty() || id.contains(' ') || id == "auto" {
            continue;
        }
        // 去掉 CLI 的状态尾注（默认/当前项），模型属性尾注（如 "(NO ZDR)"）保留
        let mut name = name.trim();
        loop {
            let stripped = name
                .trim_end_matches("(default)")
                .trim()
                .trim_end_matches("(current)")
                .trim();
            if stripped == name {
                break;
            }
            name = stripped;
        }
        let (base_key, id_effort, fast) = cursor_variant_dims(id);
        if !base_order.iter().any(|b| b == &base_key) {
            base_order.push(base_key.clone());
        }
        let name_effort = cursor_name_effort_rank(name, &base_key);
        raws.push(Raw {
            id: id.to_string(),
            name: name.to_string(),
            base_key,
            id_effort,
            name_effort,
            fast,
        });
    }

    // 侦测家族级档位偏移：某基座下同时带 id 档位与营销名档位的变体，若差值一致且非零，
    // 视为整族刻度偏移（如 grok 恒 -1）。单个变体的差值可能只是营销名漏词（如 gpt-5.5
    // 把 "Extra High" 写成 "High"），故要求 ≥2 个变体佐证，避免误判。
    let mut diffs: HashMap<String, Vec<isize>> = HashMap::new();
    for r in &raws {
        if let (Some(ie), Some(ne)) = (r.id_effort, r.name_effort) {
            diffs
                .entry(r.base_key.clone())
                .or_default()
                .push(ne as isize - ie as isize);
        }
    }
    let offsets: HashMap<String, isize> = diffs
        .into_iter()
        .filter_map(|(base, ds)| {
            let first = *ds.first()?;
            (ds.len() >= 2 && first != 0 && ds.iter().all(|d| *d == first)).then_some((base, first))
        })
        .collect();

    let max_rank = (CURSOR_EFFORT_LABELS.len() - 1) as isize;
    let mut entries: Vec<(usize, bool, usize, String, String)> = Vec::new();
    for r in &raws {
        let base_idx = base_order.iter().position(|b| b == &r.base_key).unwrap();
        // 偏移家族按营销档位显示（id 档位 + 偏移，夹取到合法范围）；其余仍以 id 档位为准。
        let shown_effort = match (r.id_effort, offsets.get(&r.base_key)) {
            (Some(rank), Some(off)) => Some((rank as isize + off).clamp(0, max_rank) as usize),
            _ => r.id_effort,
        };
        let display = cursor_display_name(&r.name, &r.base_key, shown_effort, r.fast);
        // 无强度段即 CLI 缺省档，按 Medium 档参与排序
        entries.push((
            base_idx,
            r.fast,
            shown_effort.unwrap_or(2),
            r.id.clone(),
            display,
        ));
    }
    entries.sort_by_key(|e| (e.0, e.1, e.2));
    entries
        .into_iter()
        .map(|(_, _, _, id, display)| (id, display))
        .collect()
}

/// 从 CLI 显示名里解析思考强度档位（供家族档位偏移侦测）。排除括号尾注、Fast、以及
/// 属于基座名的段（如 `gpt-5.1-codex-max` 里的 Max）。支持 "Extra High" 两段写法（= XHigh）。
/// 返回 None 表示名里没有强度词。
fn cursor_name_effort_rank(name: &str, base_key: &str) -> Option<usize> {
    let core = match name.find('(') {
        Some(i) => &name[..i],
        None => name,
    };
    let tokens: Vec<&str> = core
        .split_whitespace()
        .filter(|t| !t.eq_ignore_ascii_case("fast"))
        .collect();
    let keep: Vec<&str> = base_key.split('-').collect();
    let kept = |t: &str| keep.iter().any(|s| s.eq_ignore_ascii_case(t));
    for i in 0..tokens.len().saturating_sub(1) {
        if !kept(tokens[i])
            && !kept(tokens[i + 1])
            && tokens[i].eq_ignore_ascii_case("extra")
            && tokens[i + 1].eq_ignore_ascii_case("high")
        {
            return Some(4);
        }
    }
    tokens.iter().find_map(|t| {
        if kept(t) {
            return None;
        }
        CURSOR_EFFORT_LABELS
            .iter()
            .position(|l| t.eq_ignore_ascii_case(l))
    })
}

/// 思考强度档位显示名（与排序档位一致）。
const CURSOR_EFFORT_LABELS: [&str; 6] = ["None", "Low", "Medium", "High", "XHigh", "Max"];

/// 从模型 id 的段里提取（同族键, 思考强度档位, 是否 Fast）。
/// 同族键 = 去掉强度/速度段后的 id（比显示名可靠：CLI 里同族名字时有时无 "1M" 等修饰），
/// 如 gpt-5.3-codex-xhigh-fast → ("gpt-5.3-codex", Some(4), true)。
/// "extra"+"high" 相邻两段等价 xhigh（如 gpt-5.5-extra-high）。
fn cursor_variant_dims(id: &str) -> (String, Option<usize>, bool) {
    let segs: Vec<&str> = id.split('-').collect();
    let fast = segs.iter().any(|seg| seg.eq_ignore_ascii_case("fast"));
    // 从右往左只取最后一个强度段。这样 gpt-5.1-codex-max-low 的 `max` 仍属于模型名，
    // `low` 才是强度；claude-opus-4-8-max 则把末尾 `max` 识别为强度。
    let mut effort_at: Option<(usize, usize, usize)> = None;
    for i in (0..segs.len()).rev() {
        let seg = segs[i];
        if seg.eq_ignore_ascii_case("high") && i > 0 && segs[i - 1].eq_ignore_ascii_case("extra") {
            effort_at = Some((i - 1, 2, 4));
            break;
        }
        if let Some(rank) = ["none", "low", "medium", "high", "xhigh", "max"]
            .iter()
            .position(|kind| seg.eq_ignore_ascii_case(kind))
        {
            effort_at = Some((i, 1, rank));
            break;
        }
    }
    let mut base_segs: Vec<&str> = Vec::new();
    for (i, seg) in segs.iter().enumerate() {
        let is_effort = effort_at
            .map(|(start, len, _)| i >= start && i < start + len)
            .unwrap_or(false);
        if !is_effort && !seg.eq_ignore_ascii_case("fast") {
            base_segs.push(seg);
        }
    }
    let effort = effort_at.map(|(_, _, rank)| rank);
    (base_segs.join("-"), effort, fast)
}

/// 把 CLI 显示名规整为「基础名 [强度] [Fast] [尾注]」（Devin 目录风格）。
/// 基础名 = 原始名去掉散落的强度/速度词；强度/Fast 以 id 解析结果为准显式补回，
/// 例："Fable 5 1M Extra High Thinking (NO ZDR)" + xhigh
///   → "Fable 5 1M Thinking XHigh (NO ZDR)"。
/// 无强度段的缺省档（如 "Codex 5.3"）不猜档位、保持原名。
///
/// CLI 显示名里的强度词可能与 id 档位不一致（如 `grok-4.5-medium` 的 CLI 名写
/// "Low"），有强度档时清掉全部强度词再按 id 补回，避免 "Low Medium" 叠词。
/// `base_key` 里若含看似强度的段（如 `gpt-5.1-codex-max` 的 max），予以保留。
fn cursor_display_name(name: &str, base_key: &str, effort: Option<usize>, fast: bool) -> String {
    let (core, note) = match name.find('(') {
        Some(i) => (name[..i].trim(), Some(name[i..].trim())),
        None => (name, None),
    };
    let mut base_tokens: Vec<&str> = core
        .split_whitespace()
        .filter(|token| !token.eq_ignore_ascii_case("fast"))
        .collect();
    if effort.is_some() {
        let keep: Vec<&str> = base_key.split('-').collect();
        strip_effort_tokens(&mut base_tokens, &keep);
    }
    let mut display = base_tokens.join(" ");
    if let Some(e) = effort {
        display.push(' ');
        display.push_str(CURSOR_EFFORT_LABELS[e.min(CURSOR_EFFORT_LABELS.len() - 1)]);
    }
    if fast {
        display.push_str(" Fast");
    }
    if let Some(n) = note {
        display.push(' ');
        display.push_str(n);
    }
    display.trim().to_string()
}

/// 从显示名 token 里去掉思考强度词（含 "Extra High" 两段写法）。
/// `keep` 为同族 id 段：其中出现的词（如模型名里的 Max）不删。
fn strip_effort_tokens(tokens: &mut Vec<&str>, keep: &[&str]) {
    let kept = |token: &str| keep.iter().any(|seg| seg.eq_ignore_ascii_case(token));
    loop {
        if let Some(i) = (0..tokens.len().saturating_sub(1)).rev().find(|&i| {
            !kept(tokens[i])
                && !kept(tokens[i + 1])
                && tokens[i].eq_ignore_ascii_case("extra")
                && tokens[i + 1].eq_ignore_ascii_case("high")
        }) {
            tokens.drain(i..=i + 1);
            continue;
        }
        if let Some(i) = tokens.iter().rposition(|token| {
            !kept(token)
                && CURSOR_EFFORT_LABELS
                    .iter()
                    .any(|label| token.eq_ignore_ascii_case(label))
        }) {
            tokens.remove(i);
            continue;
        }
        break;
    }
}

/// CLI 完整目录使用短 id；ACP 标准目录使用 `model[...]` 参数化 id。
fn is_cursor_cli_model_id(model: &str) -> bool {
    !model.is_empty() && !model.contains('[')
}

/// 从 Cursor 的 configOptions 中读取参数化 Fast 选择器支持的值。
/// 只接受协议当前使用的 "true" / "false"，避免把未知值合成成前端模型 id。
fn cursor_fast_values(options: &Value) -> Vec<String> {
    options
        .as_array()
        .or_else(|| options.get("configOptions").and_then(|v| v.as_array()))
        .and_then(|arr| {
            arr.iter()
                .find(|o| o.get("id").and_then(|v| v.as_str()) == Some("fast"))
        })
        .and_then(|fast_opt| fast_opt.get("options").and_then(|v| v.as_array()))
        .map(|opts| {
            opts.iter()
                .filter_map(|o| {
                    let value = o.get("value").and_then(|v| v.as_str())?;
                    matches!(value, "true" | "false").then(|| value.to_string())
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 关闭 cli-config 里的 Max Mode 计费开关。返回是否有改动。
fn clear_cursor_max_mode(cfg: &mut Value) -> bool {
    let mut changed = false;
    if cfg.get("maxMode").and_then(|v| v.as_bool()) == Some(true) {
        cfg["maxMode"] = json!(false);
        changed = true;
    }
    if cfg
        .get("model")
        .and_then(|m| m.get("maxMode"))
        .and_then(|v| v.as_bool())
        == Some(true)
    {
        cfg["model"]["maxMode"] = json!(false);
        changed = true;
    }
    changed
}

/// 从扁平 CLI 模型 id 生成 cursor-agent 参数化选择（基座 id + parameters）。
/// `fast` 始终显式写出，避免配置里残留的 `fast=true` 盖过 `--model`。
fn cursor_selection_from_flat_id(model: &str) -> (String, Vec<Value>) {
    let (base, effort, fast) = cursor_variant_dims(model);
    let mut params = Vec::new();
    if let Some(rank) = effort {
        let value = ["none", "low", "medium", "high", "xhigh", "max"]
            [rank.min(CURSOR_EFFORT_LABELS.len() - 1)];
        params.push(json!({ "id": "effort", "value": value }));
    }
    params.push(json!({
        "id": "fast",
        "value": if fast { "true" } else { "false" }
    }));
    (base, params)
}

/// 把扁平 `--model` id 写回 cli-config 的 selectedModel / modelParameters / model.modelId。
/// 返回是否有实质改动。
fn sync_cursor_cli_model_selection(cfg: &mut Value, model: &str) -> bool {
    let (base, params) = cursor_selection_from_flat_id(model);
    let params_value = Value::Array(params);
    let mut changed = false;

    let selected = json!({
        "modelId": base,
        "parameters": params_value.clone(),
    });
    if cfg.get("selectedModel") != Some(&selected) {
        cfg["selectedModel"] = selected;
        changed = true;
    }

    let needs_params = match cfg.get("modelParameters").and_then(|v| v.as_object()) {
        Some(map) => map.get(&base) != Some(&params_value),
        None => true,
    };
    if needs_params {
        if !cfg
            .get("modelParameters")
            .map(|v| v.is_object())
            .unwrap_or(false)
        {
            cfg["modelParameters"] = json!({});
        }
        cfg["modelParameters"][&base] = params_value.clone();
        changed = true;
    }

    if cfg
        .get("model")
        .and_then(|m| m.get("modelId"))
        .and_then(|v| v.as_str())
        != Some(base.as_str())
    {
        if !cfg.get("model").map(|v| v.is_object()).unwrap_or(false) {
            cfg["model"] = json!({});
        }
        cfg["model"]["modelId"] = json!(base);
        cfg["model"]["displayModelId"] = json!(base);
        changed = true;
    }
    if !cfg.get("model").map(|v| v.is_object()).unwrap_or(false) {
        cfg["model"] = json!({});
    }
    if cfg["model"].get("parameters") != Some(&params_value) {
        cfg["model"]["parameters"] = params_value.clone();
        changed = true;
    }
    for stale in [
        "displayName",
        "selectedModelVariantId",
        "variantId",
        "effort",
        "fast",
    ] {
        if cfg["model"].get(stale).is_some() {
            cfg["model"].as_object_mut().unwrap().remove(stale);
            changed = true;
        }
    }

    changed
}

/// 写入 cli-config.json；文件可能带只读属性，先摘只读再还原。
fn write_cursor_cli_config(path: &std::path::Path, cfg: &Value) -> Result<(), String> {
    let was_readonly = std::fs::metadata(path)
        .map(|m| m.permissions().readonly())
        .unwrap_or(false);
    if was_readonly {
        if let Ok(meta) = std::fs::metadata(path) {
            let mut perms = meta.permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            perms.set_readonly(false);
            let _ = std::fs::set_permissions(path, perms);
        }
    }
    let result = serde_json::to_string_pretty(cfg)
        .map_err(|e| e.to_string())
        .and_then(|s| std::fs::write(path, s).map_err(|e| e.to_string()));
    if was_readonly {
        if let Ok(meta) = std::fs::metadata(path) {
            let mut perms = meta.permissions();
            perms.set_readonly(true);
            let _ = std::fs::set_permissions(path, perms);
        }
    }
    result
}

/// cursor-agent 等后端连云端时偶发的瞬时网络错。
/// 典型：`RetriableError: [unavailable] PING timed out`、`RetriableError: Connection stalled`、
/// `RetriableError: [aborted] read ECONNRESET`、裸 `Internal error`。
/// 重试通常能成功，不应清 session / 杀进程（进程已死的情况由调用方单独处理）。
fn is_retriable_rpc_error(err: &str) -> bool {
    let lower = err.to_ascii_lowercase();
    lower.contains("retriable")
        || lower.contains("ping timed out")
        || lower.contains("connection stalled")
        || lower.contains("[unavailable]")
        || lower.contains("econnreset")
        || lower.contains("etimedout")
        || lower.contains("econnrefused")
        || lower.contains("eai_again")
        || lower.contains("enotfound")
        || lower.contains("socket hang up")
        || lower.contains("und_err_")
        || (lower.contains("network") && lower.contains("abort"))
        || lower.contains("[aborted]")
        || lower.contains("stall_detector")
        // Cursor ACP 建会话/prompt 时偶发的云端内部错，常为瞬时
        || lower.contains("internal error")
        || lower == "internalerror"
}

fn is_process_exit_error(err: &str) -> bool {
    err.contains("进程已退出") || err.contains("进程不可写") || err.contains("连接已断开")
}

fn prompt_conn_needs_rebuild(conn_alive: Option<bool>, last_err: &str) -> bool {
    conn_alive != Some(true) || is_process_exit_error(last_err)
}

/// 瞬时错误退避：1s → 2s → 4s → 8s（封顶 8s）。Cursor 云端 PING/stall 恢复常需数秒。
fn retriable_backoff_ms(attempt: u32) -> u64 {
    1000u64 * (1u64 << (attempt.saturating_sub(1).min(3)))
}

/// 是否「放开全部权限」语义的模式：统一模式 build，以及各后端历史等价值
/// （bypass=Devin、bypassPermissions=CodeBuddy/ClaudeCode、agent=Cursor、
/// dontAsk/fullAccess=CodeBuddy）。这些模式下后端仍上报的授权请求由 Nova 代答 allow。
fn is_full_permission_mode(mode: &str) -> bool {
    matches!(
        mode,
        "build" | "bypass" | "bypassPermissions" | "agent" | "dontAsk" | "fullAccess"
    )
}

/// 后端原生模式 id → 统一模式 id（build / plan）。与 frontend `normalizeUnifiedMode` 对齐。
/// accept-edits / ask 等非统一值原样返回，由调用方决定是否透传。
fn unify_mode_id(mode: &str) -> String {
    if mode.eq_ignore_ascii_case("plan") {
        "plan".into()
    } else if is_full_permission_mode(mode) {
        "build".into()
    } else {
        mode.into()
    }
}

/// 目标统一模式在后端可用列表里找不到首选 id 时，挑一个语义等价的替代。
fn pick_fallback_mode_id(unified: &str, known: &[String]) -> Option<String> {
    let want_plan = unify_mode_id(unified) == "plan";
    known.iter().find_map(|id| {
        let u = unify_mode_id(id);
        if want_plan {
            (u == "plan").then(|| id.clone())
        } else if is_full_permission_mode(unified) || u == "build" {
            is_full_permission_mode(id).then(|| id.clone())
        } else {
            None
        }
    })
}

/// 去除 ANSI 转义序列（`ESC [ ... <字母>`）
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for c2 in chars.by_ref() {
                    if c2.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

#[cfg(windows)]
fn devin_runtime_guidance() -> Option<&'static str> {
    Some(
        "Runtime note for this local Devin session: shell commands run on Windows through PowerShell. Use PowerShell syntax for command execution. Prefer `Get-ChildItem -Force` over `ls -la`, `Get-Content` over `cat` when flags are needed, `$env:NAME` for environment variables, and Windows-compatible paths. Do not assume Git Bash or bash syntax unless you explicitly run `bash -lc \"...\"`.",
    )
}

#[cfg(not(windows))]
fn devin_runtime_guidance() -> Option<&'static str> {
    None
}

/// 把粘贴的图片写到临时目录，返回绝对路径（失败时返回 None，仅靠内嵌 base64）
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
    let dir = std::env::temp_dir().join("Nova-images");
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(format!("{}.{ext}", uuid::Uuid::new_v4()));
    std::fs::write(&path, bytes).ok()?;
    Some(path.to_string_lossy().to_string())
}

fn prompt_image_data(img: &PromptImage) -> Option<String> {
    if let Some(data) = &img.data {
        return Some(data.clone());
    }
    let uri = img.uri.as_ref()?;
    let path = file_uri_to_path(uri)?;
    let bytes = std::fs::read(path).ok()?;
    use base64::Engine;
    Some(base64::engine::general_purpose::STANDARD.encode(bytes))
}

fn attachment_size(img: &PromptImage) -> Option<u64> {
    img.size.or_else(|| {
        img.uri
            .as_ref()
            .and_then(|uri| file_uri_to_path(uri))
            .and_then(|path| std::fs::metadata(path).ok())
            .map(|m| m.len())
    })
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

fn extract_text(content: &Value) -> String {
    match content["type"].as_str() {
        Some("text") => content["text"].as_str().unwrap_or_default().to_string(),
        Some(other) => format!("[{other}]"),
        None => String::new(),
    }
}

fn tool_call_from_update(tc_id: &str, update: &Value) -> ToolCall {
    ToolCall {
        tool_call_id: tc_id.to_string(),
        title: update["title"].as_str().unwrap_or("(工具调用)").to_string(),
        kind: update["kind"].as_str().unwrap_or("other").to_string(),
        status: update["status"].as_str().unwrap_or("pending").to_string(),
        content: update["content"]
            .as_array()
            .map(|values| compact_tool_values(values))
            .unwrap_or_default(),
        locations: update["locations"].as_array().cloned().unwrap_or_default(),
        raw_input: update
            .get("rawInput")
            .filter(|v| !v.is_null())
            .map(compact_tool_value),
        raw_output: update
            .get("rawOutput")
            .filter(|v| !v.is_null())
            .map(compact_tool_value),
    }
}

impl CursorToolDetails {
    fn merge(&mut self, other: CursorToolDetails) {
        if other.tool_name.is_some() {
            self.tool_name = other.tool_name;
        }
        if other.raw_input.is_some() {
            self.raw_input = other.raw_input;
        }
        if other.raw_output.is_some() {
            self.raw_output = other.raw_output;
        }
    }

    fn is_empty(&self) -> bool {
        self.tool_name.is_none() && self.raw_input.is_none() && self.raw_output.is_none()
    }
}

fn is_generic_cursor_mcp_title(title: &str) -> bool {
    matches!(
        title.trim().to_ascii_lowercase().as_str(),
        "mcp: tool" | "mcp tool"
    )
}

fn cursor_tool_title(tool_name: &str) -> String {
    format!(
        "MCP: {}",
        tool_name.strip_prefix("mcp_").unwrap_or(tool_name)
    )
}

fn apply_cursor_tool_details(update: &mut Value, details: &CursorToolDetails, terminal: bool) {
    let Some(map) = update.as_object_mut() else {
        return;
    };
    if let Some(tool_name) = details.tool_name.as_deref() {
        map.insert("title".into(), Value::String(cursor_tool_title(tool_name)));
    }
    if let Some(raw_input) = details.raw_input.as_ref() {
        map.insert("rawInput".into(), compact_tool_value(raw_input));
    }
    if terminal {
        if let Some(raw_output) = details.raw_output.as_ref() {
            map.insert("rawOutput".into(), compact_tool_value(raw_output));
        }
    }
}

const CURSOR_TOOL_DETAILS_SCRIPT: &str = r#"
const { DatabaseSync } = require('node:sqlite');
const db = new DatabaseSync(process.argv[1], { readOnly: true });
const id = process.argv[2];
const escapedId = JSON.stringify(id).slice(1, -1);
const rows = db.prepare(
  'SELECT data FROM blobs WHERE instr(data, CAST(? AS BLOB)) > 0 OR instr(data, CAST(? AS BLOB)) > 0'
).all(id, escapedId);
const details = {};
for (const row of rows) {
  let message;
  try { message = JSON.parse(Buffer.from(row.data).toString('utf8')); } catch { continue; }
  if (!Array.isArray(message.content)) continue;
  for (const block of message.content) {
    if (block?.toolCallId !== id) continue;
    if (typeof block.toolName === 'string') details.toolName = block.toolName;
    if (block.type === 'tool-call' && block.args !== undefined) details.rawInput = block.args;
    if (block.type === 'tool-result') {
      if (block.result !== undefined) details.rawOutput = block.result;
      else if (block.experimental_content !== undefined) details.rawOutput = block.experimental_content;
    }
  }
}
process.stdout.write(JSON.stringify(details));
"#;

fn read_cursor_tool_details(
    node_path: &Path,
    store_path: &Path,
    tool_call_id: &str,
) -> Option<CursorToolDetails> {
    let mut command = std::process::Command::new(node_path);
    command
        .arg("-e")
        .arg(CURSOR_TOOL_DETAILS_SCRIPT)
        .arg(store_path)
        .arg(tool_call_id)
        .env("NODE_NO_WARNINGS", "1")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let output = command.output().ok()?;
    if !output.status.success() {
        return None;
    }
    let value = serde_json::from_slice::<Value>(&output.stdout).ok()?;
    let details = CursorToolDetails {
        tool_name: value
            .get("toolName")
            .and_then(Value::as_str)
            .map(str::to_string),
        raw_input: value.get("rawInput").filter(|v| !v.is_null()).cloned(),
        raw_output: value.get("rawOutput").filter(|v| !v.is_null()).cloned(),
    };
    (!details.is_empty()).then_some(details)
}

fn merge_tool_call(call: &mut ToolCall, update: &Value) {
    if let Some(title) = update["title"].as_str() {
        call.title = title.to_string();
    }
    if let Some(kind) = update["kind"].as_str() {
        call.kind = kind.to_string();
    }
    if let Some(status) = update["status"].as_str() {
        call.status = status.to_string();
    }
    if let Some(content) = update["content"].as_array() {
        call.content = compact_tool_values(content);
    }
    if let Some(locations) = update["locations"].as_array() {
        call.locations = locations.clone();
    }
    if let Some(v) = update.get("rawInput").filter(|v| !v.is_null()) {
        call.raw_input = Some(compact_tool_value(v));
    }
    if let Some(v) = update.get("rawOutput").filter(|v| !v.is_null()) {
        call.raw_output = Some(compact_tool_value(v));
    }
}

fn compact_tool_values(values: &[Value]) -> Vec<Value> {
    values.iter().map(compact_tool_value).collect()
}

fn compact_tool_value(value: &Value) -> Value {
    match value {
        Value::String(s) => Value::String(limit_display_text(s)),
        Value::Array(items) => Value::Array(items.iter().map(compact_tool_value).collect()),
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                out.insert(k.clone(), compact_tool_value(v));
            }
            Value::Object(out)
        }
        _ => value.clone(),
    }
}

fn limit_display_text(text: &str) -> String {
    if text.len() <= TOOL_OUTPUT_LIMIT {
        return text.to_string();
    }
    let mut start = text.len().saturating_sub(TOOL_OUTPUT_LIMIT);
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    format!(
        "[输出过长，已省略前面内容，仅保留最后 {}KB]\n{}",
        TOOL_OUTPUT_LIMIT / 1024,
        &text[start..]
    )
}

fn normalize_generated_title(raw: &str, fallback: &str) -> String {
    let mut title = raw
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
        .trim()
        .trim_matches(|c| matches!(c, '"' | '\'' | '`' | '“' | '”' | '‘' | '’'))
        .trim()
        .trim_end_matches(&['.', '。', '!', '！', '?', '？'][..])
        .trim()
        .to_string();
    if title.is_empty() {
        return fallback.to_string();
    }
    if title.chars().count() > 30 {
        title = title.chars().take(30).collect();
    }
    title
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

#[cfg(test)]
mod opencode_models_tests {
    use super::*;

    #[test]
    fn switching_opencode_sessions_requires_model_reapply() {
        let gpt_sid = "gpt-session";
        let ds_sid = "ds-session";
        assert!(connection_scoped_model_session_changed(
            &AgentKind::OpenCode,
            Some(gpt_sid),
            ds_sid,
        ));
        assert!(!connection_scoped_model_session_changed(
            &AgentKind::OpenCode,
            Some(ds_sid),
            ds_sid,
        ));
        assert!(!connection_scoped_model_session_changed(
            &AgentKind::Devin,
            Some(gpt_sid),
            ds_sid,
        ));
    }

    #[test]
    fn expands_effort_variants_into_selectable_model_ids() {
        let model = json!({
            "value": "codex/gpt-5.4",
            "name": "Codex Custom/GPT-5.4"
        });
        let efforts = vec![
            json!({ "value": "low", "name": "Low" }),
            json!({ "value": "medium", "name": "Medium" }),
            json!({ "value": "xhigh", "name": "Xhigh" }),
        ];

        let choices = expand_opencode_model_choice(&model, &efforts);
        let values: Vec<&str> = choices
            .iter()
            .filter_map(|choice| choice.get("value").and_then(|v| v.as_str()))
            .collect();
        assert_eq!(
            values,
            vec![
                "codex/gpt-5.4",
                "codex/gpt-5.4/low",
                "codex/gpt-5.4/medium",
                "codex/gpt-5.4/xhigh",
            ]
        );
        assert_eq!(choices[0]["name"], "Codex Custom/GPT-5.4 · Default");
        assert_eq!(choices[3]["name"], "Codex Custom/GPT-5.4 · XHigh");
        assert_eq!(choices[3]["_meta"]["nova.ai/opencodeVariant"], "xhigh");
    }

    #[test]
    fn plain_session_options_keep_cached_opencode_variants() {
        let cached_choices = expand_opencode_model_choice(
            &json!({ "value": "codex/gpt-5.4", "name": "GPT-5.4" }),
            &[
                json!({ "value": "low", "name": "Low" }),
                json!({ "value": "high", "name": "High" }),
            ],
        );
        let cached = json!({
            "configOptions": [{
                "id": "model",
                "options": cached_choices
            }]
        });
        let mut fresh = json!([{
            "id": "model",
            "options": [
                { "value": "codex/gpt-5.4", "name": "GPT-5.4" },
                { "value": "opencode/big-pickle", "name": "Big Pickle" }
            ]
        }]);

        merge_cached_opencode_variants(&mut fresh, &cached);
        let values: Vec<&str> = config_option(&fresh, "model").unwrap()["options"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|choice| choice.get("value").and_then(|v| v.as_str()))
            .collect();
        assert_eq!(
            values,
            vec![
                "codex/gpt-5.4",
                "codex/gpt-5.4/low",
                "codex/gpt-5.4/high",
                "opencode/big-pickle",
            ]
        );
        assert!(opencode_catalog_is_expanded(&fresh));
    }
}

#[cfg(test)]
mod cursor_models_tests {
    use super::*;

    #[test]
    fn cursor_mcp_tool_details_replace_generic_payload() {
        let details = CursorToolDetails {
            tool_name: Some("mcp_codegraph_codegraph_status".into()),
            raw_input: Some(json!({ "projectPath": "D:/project/nova-client" })),
            raw_output: Some(json!("healthy")),
        };
        let mut update = json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "tool_test_123",
            "title": "MCP: tool",
            "status": "completed",
            "rawInput": {},
            "rawOutput": { "success": true }
        });
        apply_cursor_tool_details(&mut update, &details, true);

        assert_eq!(update["title"], "MCP: codegraph_codegraph_status");
        assert_eq!(
            update["rawInput"],
            json!({ "projectPath": "D:/project/nova-client" })
        );
        assert_eq!(update["rawOutput"], "healthy");
    }

    #[test]
    fn parse_lists_effort_and_fast_variants() {
        let out = "\
Available models\n\
auto - Auto (default)\n\
gpt-5.3-codex - Codex 5.3\n\
gpt-5.3-codex-high - Codex 5.3 High\n\
gpt-5.3-codex-xhigh-fast - Codex 5.3 Extra High Fast\n\
claude-opus-4-8-thinking-max - Opus 4.8 1M Max Thinking\n\
gpt-5.1-codex-max-low - Codex 5.1 Max Low\n\
grok-4.5-medium - Cursor Grok 4.5 Low\n\
grok-4.5-high - Cursor Grok 4.5 Medium\n\
grok-4.5-xhigh - Cursor Grok 4.5\n\
";
        let parsed = parse_cursor_models(out);
        let ids: Vec<&str> = parsed.iter().map(|(id, _)| id.as_str()).collect();
        assert!(!ids.contains(&"auto"));
        assert!(ids.contains(&"gpt-5.3-codex"));
        assert!(ids.contains(&"gpt-5.3-codex-high"));
        assert!(ids.contains(&"gpt-5.3-codex-xhigh-fast"));
        assert!(ids.contains(&"claude-opus-4-8-thinking-max"));
        assert!(ids.contains(&"gpt-5.1-codex-max-low"));
        let high = parsed
            .iter()
            .find(|(id, _)| id == "gpt-5.3-codex-high")
            .unwrap();
        assert!(high.1.contains("High"), "{}", high.1);
        let xfast = parsed
            .iter()
            .find(|(id, _)| id == "gpt-5.3-codex-xhigh-fast")
            .unwrap();
        assert!(
            xfast.1.contains("XHigh") && xfast.1.contains("Fast"),
            "{}",
            xfast.1
        );
        let max_low = parsed
            .iter()
            .find(|(id, _)| id == "gpt-5.1-codex-max-low")
            .unwrap();
        // `max` 属于模型名，`low` 才是强度
        assert_eq!(
            cursor_variant_dims("gpt-5.1-codex-max-low").0,
            "gpt-5.1-codex-max"
        );
        assert_eq!(max_low.1, "Codex 5.1 Max Low");
        // grok 家族 id 后缀档位（medium/high/xhigh）比 CLI 营销档位（Low/Medium/顶档）整体高一档，
        // ≥2 个变体佐证同一 -1 偏移，显示名按营销档位对齐，避免造出 Cursor 里不存在的 "XHigh"。
        let grok_med = parsed
            .iter()
            .find(|(id, _)| id == "grok-4.5-medium")
            .unwrap();
        assert_eq!(grok_med.1, "Cursor Grok 4.5 Low");
        let grok_high = parsed.iter().find(|(id, _)| id == "grok-4.5-high").unwrap();
        assert_eq!(grok_high.1, "Cursor Grok 4.5 Medium");
        // id 仍是真实 CLI id（启动时 --model 直传），只有显示名跟随营销档位。
        let grok_x = parsed
            .iter()
            .find(|(id, _)| id == "grok-4.5-xhigh")
            .unwrap();
        assert_eq!(grok_x.1, "Cursor Grok 4.5 High");
    }

    #[test]
    fn cli_model_ids_are_short_not_parameterized() {
        assert!(is_cursor_cli_model_id("gpt-5.3-codex-high"));
        assert!(!is_cursor_cli_model_id("gpt-5.3-codex[effort=high]"));
        assert!(!is_cursor_cli_model_id(""));
    }

    #[test]
    fn reads_fast_values_from_current_session_options() {
        let config_options = json!([
            {
                "id": "fast",
                "options": [
                    { "value": "false", "name": "Off" },
                    { "value": "true", "name": "Fast" },
                    { "value": "future", "name": "Unknown" }
                ]
            }
        ]);
        assert_eq!(cursor_fast_values(&config_options), vec!["false", "true"]);
        assert_eq!(
            cursor_fast_values(&json!({ "configOptions": config_options })),
            vec!["false", "true"]
        );
    }

    #[test]
    fn flat_id_selection_follows_effort_and_fast_variant() {
        for (model, expected_base, expected_params) in [
            (
                "grok-4.5-medium",
                "grok-4.5",
                vec![
                    json!({ "id": "effort", "value": "medium" }),
                    json!({ "id": "fast", "value": "false" }),
                ],
            ),
            (
                "gpt-5.3-codex-high",
                "gpt-5.3-codex",
                vec![
                    json!({ "id": "effort", "value": "high" }),
                    json!({ "id": "fast", "value": "false" }),
                ],
            ),
            (
                "claude-opus-4-8-thinking-xhigh-fast",
                "claude-opus-4-8-thinking",
                vec![
                    json!({ "id": "effort", "value": "xhigh" }),
                    json!({ "id": "fast", "value": "true" }),
                ],
            ),
            (
                "composer-2.5-fast",
                "composer-2.5",
                vec![json!({ "id": "fast", "value": "true" })],
            ),
        ] {
            let (base, params) = cursor_selection_from_flat_id(model);
            assert_eq!(base, expected_base, "model={model}");
            assert_eq!(params, expected_params, "model={model}");
        }
    }

    #[test]
    fn sync_cli_config_overrides_persisted_fast_true() {
        let mut cfg = json!({
            "maxMode": true,
            "model": {
                "modelId": "grok-4.5",
                "displayModelId": "grok-4.5",
                "displayName": "Cursor Grok 4.5 High Fast",
                "selectedModelVariantId": "grok-4.5-high-fast",
                "parameters": [
                    { "id": "effort", "value": "high" },
                    { "id": "fast", "value": "true" }
                ],
                "maxMode": true
            },
            "modelParameters": {
                "grok-4.5": [
                    { "id": "effort", "value": "high" },
                    { "id": "fast", "value": "true" }
                ]
            },
            "selectedModel": {
                "modelId": "grok-4.5",
                "parameters": [
                    { "id": "effort", "value": "high" },
                    { "id": "fast", "value": "true" }
                ]
            }
        });
        assert!(clear_cursor_max_mode(&mut cfg));
        assert_eq!(cfg["maxMode"], json!(false));
        assert_eq!(cfg["model"]["maxMode"], json!(false));
        assert!(sync_cursor_cli_model_selection(&mut cfg, "grok-4.5-xhigh"));
        assert_eq!(
            cfg["selectedModel"],
            json!({
                "modelId": "grok-4.5",
                "parameters": [
                    { "id": "effort", "value": "xhigh" },
                    { "id": "fast", "value": "false" }
                ]
            })
        );
        assert_eq!(
            cfg["modelParameters"]["grok-4.5"],
            json!([
                { "id": "effort", "value": "xhigh" },
                { "id": "fast", "value": "false" }
            ])
        );
        assert_eq!(
            cfg["model"]["parameters"],
            json!([
                { "id": "effort", "value": "xhigh" },
                { "id": "fast", "value": "false" }
            ])
        );
        assert!(cfg["model"].get("displayName").is_none());
        assert!(cfg["model"].get("selectedModelVariantId").is_none());
        // 已对齐时再写应无改动
        assert!(!sync_cursor_cli_model_selection(&mut cfg, "grok-4.5-xhigh"));
    }

    #[test]
    fn detects_cursor_retriable_network_errors() {
        assert!(is_retriable_rpc_error(
            "RetriableError: [aborted] read ECONNRESET"
        ));
        assert!(is_retriable_rpc_error(
            "RetriableError: [unavailable] PING timed out"
        ));
        assert!(is_retriable_rpc_error(
            "Error: RetriableError: Connection stalled"
        ));
        assert!(is_retriable_rpc_error("connect ETIMEDOUT"));
        assert!(is_retriable_rpc_error("socket hang up"));
        assert!(is_retriable_rpc_error("stall_detector"));
        assert!(is_retriable_rpc_error("Internal error"));
        assert!(is_retriable_rpc_error("创建会话失败：Internal error"));
        assert!(!is_retriable_rpc_error("模型不存在"));
        assert!(!is_retriable_rpc_error("session/new 未返回 sessionId"));
        assert!(is_process_exit_error("Cursor 进程已退出"));
        assert!(is_process_exit_error("Cursor 进程不可写（已退出？）"));
        assert!(is_process_exit_error("Cursor 连接已断开"));
        assert!(!is_process_exit_error("Internal error"));
    }

    #[test]
    fn rebuilds_prompt_connection_when_cursor_slot_disappears() {
        assert!(prompt_conn_needs_rebuild(None, "Cursor 未连接"));
        assert!(prompt_conn_needs_rebuild(Some(false), ""));
        assert!(prompt_conn_needs_rebuild(Some(true), "Cursor 连接已断开"));
        assert!(!prompt_conn_needs_rebuild(Some(true), "Internal error"));
    }

    #[test]
    fn retriable_backoff_grows_then_caps() {
        assert_eq!(retriable_backoff_ms(1), 1000);
        assert_eq!(retriable_backoff_ms(2), 2000);
        assert_eq!(retriable_backoff_ms(3), 4000);
        assert_eq!(retriable_backoff_ms(4), 8000);
        assert_eq!(retriable_backoff_ms(5), 8000);
    }

    #[test]
    fn unify_mode_maps_backend_ids_to_build_or_plan() {
        assert_eq!(unify_mode_id("plan"), "plan");
        assert_eq!(unify_mode_id("Plan"), "plan");
        assert_eq!(unify_mode_id("build"), "build");
        assert_eq!(unify_mode_id("bypass"), "build");
        assert_eq!(unify_mode_id("bypassPermissions"), "build");
        assert_eq!(unify_mode_id("agent"), "build");
        assert_eq!(unify_mode_id("dontAsk"), "build");
        assert_eq!(unify_mode_id("fullAccess"), "build");
        // 非统一历史值原样保留
        assert_eq!(unify_mode_id("ask"), "ask");
    }

    #[test]
    fn pick_fallback_prefers_full_permission_for_build() {
        let known = vec![
            "plan".into(),
            "acceptEdits".into(),
            "bypassPermissions".into(),
        ];
        assert_eq!(
            pick_fallback_mode_id("build", &known).as_deref(),
            Some("bypassPermissions")
        );
        assert_eq!(
            pick_fallback_mode_id("plan", &known).as_deref(),
            Some("plan")
        );
        let plan_only = vec!["plan".into(), "ask".into()];
        assert_eq!(pick_fallback_mode_id("build", &plan_only), None);
    }
}
