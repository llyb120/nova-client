use crate::model_cache;
use crate::nova_data_dir;
use crate::settings::Settings;
use crate::threads::{now_ms, AgentKind, Item, PromptImage, Thread, ToolCall};
use crate::AppState;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
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

/// Devin 多路复用后端的唯一共享连接（所有线程共用一条连接、一个进程）。
const SHARED_KEY: &str = "__shared__";

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

fn permission_request_key(permission_scope: &str, id: &Value) -> String {
    format!("{permission_scope}perm-{id}")
}

struct TitleJob {
    thread_id: String,
    fallback_title: String,
    output: String,
}

pub struct AcpConn {
    /// 该连接在连接池中的键（Devin 固定为 SHARED）。
    key: String,
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
/// agent 进程树——包括经 cmd 垫片间接拉起、垫片先退导致 taskkill/快照法漏杀的后代。
/// 主动 kill 路径仍走 kill_process_tree，
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
    /// 保留 agent 类型供现有路由和事件载荷使用；ACP 实现仅支持 Devin。
    pub kind: AgentKind,
    /// 额度租借实例使用的独立凭证环境；普通全局实例为空。
    launch_env: HashMap<String, String>,
    /// 额度租借实例的权限请求作用域，避免不同进程的递增 RPC id 发生碰撞。
    permission_scope: String,
    /// Devin 只使用 SHARED 键，一条连接多路复用所有 session。
    slots: StdMutex<HashMap<String, Arc<TokioMutex<Option<Arc<AcpConn>>>>>>,
    /// 存活连接计数：spawn 成功 +1、连接关闭 -1；用于 connected() 与断连广播（归零才广播）。
    alive_conns: AtomicU64,
    routes: StdMutex<HashMap<String, Route>>,
    /// 正在 session/load 回放、需要抑制 update 的会话
    loading_sessions: StdMutex<HashSet<String>>,
    running_threads: StdMutex<HashSet<String>>,
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
    /// devin 返回的可用模型/模式（来自 session/new 响应）
    model_options: StdMutex<Option<Value>>,
    /// 本进程内是否已对磁盘缓存做过一次后台重拉（避免每次 get 都打 agent）
    model_options_revalidated: AtomicBool,
    /// 后台重拉进行中
    model_options_refreshing: AtomicBool,
    available_commands: StdMutex<Option<Value>>,
    title_jobs: StdMutex<HashMap<String, TitleJob>>,
    logs: StdMutex<VecDeque<String>>,
    pub agent_info: StdMutex<Option<Value>>,
}

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
            turn_started: StdMutex::new(HashMap::new()),
            prompt_sent_at: StdMutex::new(HashMap::new()),
            pending_permissions: StdMutex::new(HashMap::new()),
            prewarmed: StdMutex::new(HashMap::new()),
            prewarming: StdMutex::new(HashSet::new()),
            thread_locks: StdMutex::new(HashMap::new()),
            model_options: StdMutex::new(None),
            model_options_revalidated: AtomicBool::new(false),
            model_options_refreshing: AtomicBool::new(false),
            available_commands: StdMutex::new(None),
            title_jobs: StdMutex::new(HashMap::new()),
            logs: StdMutex::new(VecDeque::new()),
            agent_info: StdMutex::new(None),
        });
        mgr
    }

    pub fn is_running(&self, thread_id: &str) -> bool {
        self.running_threads.lock().unwrap().contains(thread_id)
    }

    /// ACP session 不挂 Nova MCP；数字员工工具统一走 CLI。
    fn mcp_servers_for_thread(&self, _thread_id: Option<&str>) -> Value {
        json!([])
    }

    fn conn_key_for_thread(&self, _thread_id: &str) -> String {
        SHARED_KEY.to_string()
    }

    fn aux_key(&self) -> String {
        SHARED_KEY.to_string()
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
            "build" | "bypass" => "bypass".into(),
            "plan" => "plan".into(),
            other => other.into(),
        }
    }

    /// 该后端在设置里配置的代理地址（空 = 不代理）
    fn proxy_of<'a>(&self, settings: &'a Settings) -> &'a str {
        &settings.devin_proxy
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
        let values: Vec<String> = options
            .iter()
            .filter_map(|o| o.get("value").and_then(|v| v.as_str()).map(str::to_string))
            .collect();
        (!values.is_empty()).then_some(values)
    }

    pub fn get_commands(&self) -> Option<Value> {
        self.available_commands.lock().unwrap().clone()
    }

    pub async fn fetch_commands(self: &Arc<Self>) -> Result<Value, String> {
        if let Some(v) = self.get_commands() {
            return Ok(v);
        }
        // 独占连接的后端用 AUX 探测斜杠命令，不占用任何用户线程连接。
        let aux = self.aux_key();
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
        for _ in 0..40 {
            if let Some(v) = self.get_commands() {
                return Ok(v);
            }
            sleep(Duration::from_millis(100)).await;
        }
        Ok(json!([]))
    }

    pub async fn fetch_model_options(self: &Arc<Self>) -> Result<Value, String> {
        if let Some(v) = self.get_model_options() {
            return Ok(v);
        }
        self.fetch_model_options_from_agent().await
    }

    /// 无视内存缓存，向 agent 开探测 session 拉最新模型列表（供启动后台刷新）。
    async fn fetch_model_options_from_agent(self: &Arc<Self>) -> Result<Value, String> {
        // 独占连接的后端用 AUX 探测模型列表，不占用任何用户线程连接。
        let aux = self.aux_key();
        // 拿闸门期间可能已被别的路径填好缓存，复查一次（非强制刷新场景）
        // 强制刷新仍继续往下打 agent，用新结果覆盖。
        let cwd = std::env::temp_dir().join("Nova-model-options");
        std::fs::create_dir_all(&cwd)
            .map_err(|e| format!("创建 {} 模型探测目录失败：{e}", self.kind.label()))?;
        let conn = self.ensure_conn_for(&aux, None).await?;
        let resp = conn
            .request(
                "session/new",
                json!({ "cwd": cwd.to_string_lossy(), "mcpServers": [] }),
                Some(Duration::from_secs(180)),
            )
            .await
            .map_err(|e| format!("拉取 {} 模型列表失败：{e}", self.kind.label()))?;
        self.capture_options(&resp);
        self.get_model_options()
            .ok_or_else(|| format!("{} 未返回模型列表", self.kind.label()))
    }

    pub async fn connected(&self) -> bool {
        self.alive_conns.load(Ordering::SeqCst) > 0
    }

    fn push_log(&self, line: String) {
        {
            let mut logs = self.logs.lock().unwrap();
            if logs.len() >= LOG_CAP {
                logs.pop_front();
            }
            logs.push_back(line.clone());
        }
        let _ = self.app.emit(EV_LOG, line);
    }

    /// 杀掉 Devin 共享连接并清空全局路由。
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
        self.prewarmed.lock().unwrap().clear();
        self.prewarming.lock().unwrap().clear();
        self.pending_permissions.lock().unwrap().clear();
        self.title_jobs.lock().unwrap().clear();
        self.prompt_sent_at.lock().unwrap().clear();
    }

    /// 只清理「属于某条连接键」的会话状态（路由 / 活跃会话 / 回放标记 / 计时），
    /// 供切目录重启、连接关闭、单键 kill 复用，避免误伤其它并行连接的会话。
    fn clear_sessions_of_key(&self, conn_key: &str) {
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

    /// 确保某个连接键对应的 ACP 进程存活，按需启动并完成 initialize 握手。
    /// 不同 conn_key 使用各自的连接槽，可并发建连接、并行跑会话。
    ///
    /// Devin 通过 session/new 的 cwd 选择工作目录，无需按目录重启进程。
    async fn ensure_conn_for(
        self: &Arc<Self>,
        conn_key: &str,
        want_cwd: Option<&str>,
    ) -> Result<Arc<AcpConn>, String> {
        let slot = self.slot(conn_key);
        let mut guard = slot.lock().await;
        if let Some(c) = guard.as_ref() {
            if c.alive.load(Ordering::SeqCst) {
                return Ok(c.clone());
            }
        }
        let settings = {
            let state = self.app.state::<AppState>();
            let s = state.settings.lock().unwrap().clone();
            s
        };
        let conn = self.spawn_conn(&settings, conn_key, want_cwd).await?;
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
        _want_cwd: Option<&str>,
    ) -> Result<Arc<AcpConn>, String> {
        let program = settings.devin_path.clone();
        let args_str = settings.acp_args.clone();
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
        // 微型 GUI helper 统一覆盖各后端绕过父进程 flags 的 cmd/powershell/pwsh 孙进程。
        #[cfg(windows)]
        if self.app.state::<AppState>().windows_shell_shim_enabled {
            if let Err(e) = crate::windows_shell_shim::apply(&self.app, &mut cmd, &self.launch_env)
            {
                self.push_log(format!("[windows-shell-shim] {e}"));
            }
        }

        // 把 ~/.nova/skills 用软链接/目录联接同步到各后端全局 skills 目录
        crate::skills::sync_skills_from_home();

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

        let client_capabilities = json!({
            "fs": { "readTextFile": false, "writeTextFile": false }
        });
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
                // 自动放行（挑一个 allow 选项作答）的两种情形：
                // - 数字员工：无人值守，授权请求没人应答会让本轮永久挂起（is_running 卡死、
                //   员工「永远在忙」、无法再次「立即执行」）；
                // - 统一 Build 模式：语义就是放开全部权限。若后端仍上报授权请求，
                //   这里代答 allow；Plan 等其他模式照旧弹给用户审批。
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
                // Devin 保持无前缀的 perm- key，以兼容历史权限请求。
                let key = permission_request_key(&self.permission_scope, &id);
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
                            Some((*id, text.clone()))
                        }
                        Some(Item::Thought { id, text: t, .. }) if is_thought => {
                            t.push_str(&text);
                            Some((*id, text.clone()))
                        }
                        _ => None,
                    };
                    thread.updated_at = now_ms();
                    match appended {
                        Some((item_id, appended)) => {
                            self.emit_update(
                                &thread_id,
                                json!({ "t": "delta", "itemId": item_id, "text": appended }),
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
                    // Devin 自发切模式时同步到统一
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
        self.app
            .state::<AppState>()
            .sleep_inhibitor
            .set_running(thread_id, running);
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
        // ACP 标准模型放在 `models`(SessionModelState)，Cognition/Devin 扩展放在
        // `configOptions`。统一收敛成前端期望的 configOptions 形状：
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

        // 无标准 modes 字段时，从 configOptions 里 id=="mode" 的选项合成 SessionModeState。
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
        if !has_config && !has_models && !has_modes {
            return;
        }
        // configOptions 优先用扩展字段；没有则从标准 models 转换。
        let config_options = if has_config {
            result.get("configOptions").cloned().unwrap_or(Value::Null)
        } else {
            models_to_config_options(result.get("models"))
        };
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

    /// 预热：提前为某个项目目录创建空 session，消除首条消息的建会话延迟
    pub async fn prewarm(self: &Arc<Self>, cwd: String) {
        // 拿闸门前先粗筛：已预热好/正在预热就别再排队
        {
            let warmed = self.prewarmed.lock().unwrap();
            let warming = self.prewarming.lock().unwrap();
            if warmed.contains_key(&cwd) || warming.contains(&cwd) {
                return;
            }
        }
        // 这里只有共享连接后端会走到；Devin 等返回 None、保持多路复用。
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
        // Devin 的所有线程共用 SHARED 连接（多路复用）。
        let key = self.conn_key_for_thread(thread_id);
        let conn = self.ensure_conn_for(&key, Some(&cwd)).await?;

        let sid = match existing {
            Some(sid) if self.routes.lock().unwrap().contains_key(&sid) => sid,
            Some(sid) => {
                // 进程重启过：尝试 session/load 恢复上下文。瞬时网络错先重试，避免误丢上下文。
                self.loading_sessions.lock().unwrap().insert(sid.clone());
                self.routes.lock().unwrap().insert(
                    sid.clone(),
                    Route {
                        thread_id: thread_id.to_string(),
                        applied_model: None,
                        applied_mode: None,
                    },
                );
                let load_attempts: u32 = 2;
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
                        // session/load 成功，继续复用该会话。
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
                                applied_model: None,
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

        self.apply_session_config(&conn, &key, &sid, model, mode)
            .await;
        Ok(sid)
    }

    /// 把模型标记为「已处理」（避免同一会话每轮 prompt 反复处理），并在会话里推一条警告。
    /// 用于所选模型不在可用列表等不应下发的场景。
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
        _conn_key: &str,
        sid: &str,
        model: Option<String>,
        mode: Option<String>,
    ) {
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
        // 防「发送无反应」：所选模型若不在当前可用列表里，强行下发会让 agent 在 prompt 阶段直接
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
                let method = "session/set_config_option";
                let params = json!({ "sessionId": sid, "configId": "model", "value": model });
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

    /// 在 Devin 实例上异步生成标题。model 非空时下发为标题会话模型，
    /// "" 表示使用会话默认模型。
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
        // 标题生成另开 session 发一轮 prompt；独占连接的后端走 AUX（与用户线程连接隔离），
        // 用 AUX 闸门把标题/探测这类辅助任务串行化，不占用也不阻塞任何用户会话的并行。
        let aux = self.aux_key();
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
        // 独占连接的后端在 AUX 上记录标题会话活跃位，不影响用户线程连接。
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
        let key = self.conn_key_for_thread(thread_id);
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
        // 连云端建会话时可能偶发 ECONNRESET / PING timed out / Connection stalled /
        // Internal error；进程也可能瞬时退出。同连接退避重试，进程死了则拉起新进程再试，
        // 避免把瞬时抖动打成「创建会话失败」打断会话。
        let max_attempts: u32 = 3;
        let mut last_err = String::new();
        let mut resp = None;
        let mut conn = conn.clone();
        for attempt in 1..=max_attempts {
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
                applied_model: None,
                applied_mode: None,
            },
        );
        Ok(sid)
    }

    /// 在指定线程上执行一轮对话
    pub async fn run_prompt(
        self: &Arc<Self>,
        thread_id: String,
        text: String,
        images: Vec<PromptImage>,
    ) {
        // 新会话的 Paper Trail / 跨 agent 接力上下文，在真实用户输入前隐式注入。
        let handoff = {
            let state = self.app.state::<AppState>();
            let mut store = state.store.lock().unwrap();
            let mut consumed_marker = false;
            let ctx = store.get_mut(&thread_id).and_then(|t| {
                let had_handoff = t.handoff_from.is_some();
                if let Some(ctx) = t.take_prompt_context(self.kind.label()) {
                    consumed_marker = had_handoff;
                    return Some(ctx);
                }
                None
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
            sid.is_none()
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
        // ensure_session 返回后进程可能恰好退出；交给下面的重建分支恢复。
        let mut conn = self.conn_for_key(&conn_key).await;
        let mut prompt = Self::build_user_prompt_blocks(text, images, include_runtime_guidance);
        if let Some(ctx) = handoff {
            prompt.insert(0, json!({ "type": "text", "text": ctx }));
        }
        let items_at_prompt = {
            let state = self.app.state::<AppState>();
            let store = state.store.lock().unwrap();
            store.get(thread_id).map(|t| t.items.len()).unwrap_or(0)
        };
        let max_attempts: u32 = 3;
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

    /// 进程崩溃后清掉线程上的 ACP session，下次 ensure_session 会新建。
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

        // Devin 共享连接上多 session 复用。
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
    }
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
    let inherited = std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
        .unwrap_or_default();
    inherited
        .into_iter()
        .chain(crate::path_env::windows_registry_paths())
        .find_map(|dir| {
            exts.iter()
                .map(|ext| dir.join(format!("{name}.{ext}")))
                .find(|p| p.is_file())
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

/// ACP 后端连云端时偶发的瞬时网络错。

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
        // 建会话/prompt 时偶发的云端内部错，常为瞬时
        || lower.contains("internal error")
        || lower == "internalerror"
}

fn is_process_exit_error(err: &str) -> bool {
    err.contains("进程已退出") || err.contains("进程不可写") || err.contains("连接已断开")
}

fn prompt_conn_needs_rebuild(conn_alive: Option<bool>, last_err: &str) -> bool {
    conn_alive != Some(true) || is_process_exit_error(last_err)
}

/// 瞬时错误退避：1s → 2s → 4s → 8s（封顶 8s）。
fn retriable_backoff_ms(attempt: u32) -> u64 {
    1000u64 * (1u64 << (attempt.saturating_sub(1).min(3)))
}

/// 是否「放开全部权限」语义的 Devin 模式。
fn is_full_permission_mode(mode: &str) -> bool {
    matches!(mode, "build" | "bypass")
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
