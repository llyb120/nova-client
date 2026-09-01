use crate::model_cache;
use crate::nova_data_dir;
use crate::settings::Settings;
use crate::threads::{now_ms, AgentKind, Item, PromptImage, Thread, ToolCall};
use crate::AppState;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Child;
use tokio::sync::{mpsc, oneshot, Mutex as TokioMutex};
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout, Duration};
use tokio_stream::StreamExt;

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

/// ACP 后端（Devin / CodeBuddy）支持 `/browser` 进入持续浏览器调试模式；
/// 工具通过注入 nova-tools MCP 提供。
fn acp_supports_browser_debug(kind: &AgentKind) -> bool {
    matches!(kind, AgentKind::Devin | AgentKind::CodeBuddy)
}

/// 模型探测、命令探测和标题生成共用的辅助连接。
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

fn thread_connection_key(thread_id: &str) -> String {
    format!("thread:{thread_id}")
}

struct TitleJob {
    thread_id: String,
    fallback_title: String,
    output: String,
}

/// CodeBuddy 官方 one-shot 预热进程；激活后句柄和日志任务转交给 AcpConn。
struct CodeBuddyPrewarm {
    id: String,
    cwd: String,
    child: Child,
    endpoint_rx: oneshot::Receiver<Result<String, String>>,
    log_tasks: Vec<JoinHandle<()>>,
}

impl CodeBuddyPrewarm {
    fn kill(mut self) {
        self.log_tasks.drain(..).for_each(|task| task.abort());
        if let Some(pid) = self.child.id() {
            kill_process_tree(pid);
        }
        let _ = self.child.start_kill();
    }
}

/// ACP 连接的出站传输：Devin 走 stdio 行协议；CodeBuddy 走官方 Streamable HTTP。
/// 每个 POST 自己返回该请求的 SSE 响应流，独立 GET SSE 只接收异步通知。
enum AcpTransport {
    Stdio(mpsc::UnboundedSender<String>),
    Http {
        client: reqwest::Client,
        url: String,
        connection_id: String,
        session_token: String,
        inbound: mpsc::UnboundedSender<String>,
    },
}

#[derive(Default)]
struct SseDecoder {
    buffer: Vec<u8>,
    data_lines: Vec<String>,
}

impl SseDecoder {
    fn push(&mut self, chunk: &[u8]) -> Vec<String> {
        self.buffer.extend_from_slice(chunk);
        self.drain_lines(false)
    }

    fn finish(&mut self) -> Vec<String> {
        self.drain_lines(true)
    }

    fn drain_lines(&mut self, finish: bool) -> Vec<String> {
        let mut payloads = Vec::new();
        while let Some(pos) = self.buffer.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.buffer.drain(..=pos).collect();
            self.accept_line(&line, &mut payloads);
        }
        if finish {
            if !self.buffer.is_empty() {
                let line = std::mem::take(&mut self.buffer);
                self.accept_line(&line, &mut payloads);
            }
            self.emit_payload(&mut payloads);
        }
        payloads
    }

    fn accept_line(&mut self, line: &[u8], payloads: &mut Vec<String>) {
        let line = String::from_utf8_lossy(line);
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            self.emit_payload(payloads);
        } else if let Some(data) = line.strip_prefix("data:") {
            self.data_lines
                .push(data.strip_prefix(' ').unwrap_or(data).to_string());
        }
    }

    fn emit_payload(&mut self, payloads: &mut Vec<String>) {
        if !self.data_lines.is_empty() {
            payloads.push(self.data_lines.join("\n"));
            self.data_lines.clear();
        }
    }
}

fn json_rpc_request_id(message: &Value) -> Option<u64> {
    message
        .get("method")
        .and_then(|_| message.get("id"))
        .and_then(Value::as_u64)
}

/// CodeBuddy 同时返回标准 `models` 和旧扩展 `configOptions`；两边的动态模型可能
/// 各自不完整。以标准字段的当前模型和元数据为准，并补入扩展字段中独有的模型。
fn merge_model_config_option(config_options: Value, model_config_options: Value) -> Value {
    let Some(mut model) = model_config_options
        .as_array()
        .and_then(|options| options.first())
        .cloned()
    else {
        return config_options;
    };
    let mut options = config_options.as_array().cloned().unwrap_or_default();
    if let Some(existing) = options
        .iter_mut()
        .find(|option| option.get("id").and_then(Value::as_str) == Some("model"))
    {
        if let (Some(standard), Some(extension)) = (
            model.get_mut("options").and_then(Value::as_array_mut),
            existing.get("options").and_then(Value::as_array),
        ) {
            let mut seen: HashSet<String> = standard
                .iter()
                .filter_map(|option| {
                    option
                        .get("value")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .collect();
            standard.extend(
                extension
                    .iter()
                    .filter(|option| {
                        option
                            .get("value")
                            .and_then(Value::as_str)
                            .is_some_and(|value| seen.insert(value.to_owned()))
                    })
                    .cloned(),
            );
        }
        *existing = model;
    } else {
        options.push(model);
    }
    Value::Array(options)
}

fn publishes_model_options(launch_env: &HashMap<String, String>) -> bool {
    !launch_env.contains_key("NOVA_QUOTA_BORROWED")
}

/// 把 codebuddy 上报的当前模型并入可选列表（置顶，若原本不在列表里）。
/// 应对云端清单异步就绪导致的「当前模型不在可选清单」残缺快照。
fn merge_current_model_option(options: Vec<Value>, current: &str) -> Vec<Value> {
    if current.is_empty()
        || options
            .iter()
            .any(|o| o.get("value").and_then(Value::as_str) == Some(current))
    {
        return options;
    }
    let mut merged = vec![json!({ "value": current, "name": current })];
    merged.extend(options);
    merged
}

fn http_rpc_error(id: u64, message: String) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": -32000, "message": message }
    })
    .to_string()
}

pub struct AcpConn {
    /// 该连接在连接池中的键；用户线程独立，辅助任务使用 SHARED。
    key: String,
    read_only: bool,
    label: &'static str,
    transport: AcpTransport,
    pending: StdMutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>,
    next_id: AtomicU64,
    pub alive: AtomicBool,
    child: StdMutex<Option<Child>>,
    /// HTTP 传输的 stdout/stderr 日志任务；kill 时中止，保证 on_conn_closed 能即时触发。
    log_tasks: StdMutex<Vec<JoinHandle<()>>>,
    /// 热连接 LRU 的最近使用戳（单调计数，越大越新）。回收按它挑最旧的非运行中线程连接。
    last_used: AtomicU64,
}

impl AcpConn {
    fn send_raw(&self, msg: Value) -> Result<(), String> {
        match &self.transport {
            AcpTransport::Stdio(tx) => tx
                .send(msg.to_string())
                .map_err(|_| format!("{} 进程不可写（已退出？）", self.label)),
            AcpTransport::Http {
                client,
                url,
                connection_id,
                session_token,
                inbound,
            } => {
                // CodeBuddy 使用 Streamable HTTP：请求结果不走独立 GET，而在本次 POST
                // 的 text/event-stream 响应里返回。Accept 必须同时声明两种类型，否则
                // 服务端会返回 406。POST 可能覆盖整个 prompt 生命周期，不能设置总超时。
                // JSON-RPC 响应也有 id，但它是对服务端请求的回答，不会再收到响应。
                // 只跟踪同时含 method + id 的客户端请求，避免把权限答复误配给 pending。
                let request_id = json_rpc_request_id(&msg);
                let req = client
                    .post(url)
                    .header("X-CodeBuddy-Request", "1")
                    .header("acp-connection-id", connection_id)
                    .header("acp-session-token", session_token)
                    .header("Accept", "application/json, text/event-stream")
                    .json(&msg);
                let inbound = inbound.clone();
                tauri::async_runtime::spawn(async move {
                    let result = async {
                        let response = req
                            .send()
                            .await
                            .map_err(|e| format!("CodeBuddy ACP POST 失败：{e}"))?;
                        let status = response.status();
                        if !status.is_success() {
                            let body = timeout(Duration::from_secs(5), response.text())
                                .await
                                .ok()
                                .and_then(Result::ok)
                                .unwrap_or_default();
                            return Err(format!(
                                "CodeBuddy ACP POST HTTP {status}: {}",
                                body.chars().take(500).collect::<String>()
                            ));
                        }

                        let mut stream = response.bytes_stream();
                        let mut decoder = SseDecoder::default();
                        let mut received_response = request_id.is_none();
                        while let Some(chunk) = stream.next().await {
                            let chunk = chunk
                                .map_err(|e| format!("读取 CodeBuddy ACP POST 响应失败：{e}"))?;
                            for payload in decoder.push(&chunk) {
                                if request_id.is_some_and(|id| {
                                    serde_json::from_str::<Value>(&payload)
                                        .ok()
                                        .and_then(|value| value.get("id").and_then(Value::as_u64))
                                        == Some(id)
                                }) {
                                    received_response = true;
                                }
                                let _ = inbound.send(payload);
                            }
                        }
                        for payload in decoder.finish() {
                            if request_id.is_some_and(|id| {
                                serde_json::from_str::<Value>(&payload)
                                    .ok()
                                    .and_then(|value| value.get("id").and_then(Value::as_u64))
                                    == Some(id)
                            }) {
                                received_response = true;
                            }
                            let _ = inbound.send(payload);
                        }
                        if received_response {
                            Ok(())
                        } else {
                            Err("CodeBuddy ACP POST 响应流未返回对应 JSON-RPC 结果".into())
                        }
                    }
                    .await;
                    if let (Err(error), Some(id)) = (result, request_id) {
                        let _ = inbound.send(http_rpc_error(id, error));
                    }
                });
                Ok(())
            }
        }
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
        if let Err(error) = self.send_raw(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        })) {
            self.pending.lock().unwrap().remove(&id);
            return Err(error);
        }
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
        // HTTP 传输：显式断连释放服务端 connection；进程句柄在下方统一回收。
        if let AcpTransport::Http {
            client,
            url,
            connection_id,
            session_token,
            ..
        } = &self.transport
        {
            let req = client
                .delete(url)
                .header("X-CodeBuddy-Request", "1")
                .header("acp-connection-id", connection_id)
                .header("acp-session-token", session_token);
            tauri::async_runtime::spawn(async move {
                let _ = req.send().await;
            });
        }
        // HTTP 传输：reader 任务结束于「连接关闭」语义，kill 时要显式中止日志任务，
        // 否则 kill_process_tree 后任务被 AbortOnDrop 丢弃、conn 强引用随任务消失，
        // 存活计数永不归零、断连状态不广播。
        self.log_tasks
            .lock()
            .unwrap()
            .drain(..)
            .for_each(|t| t.abort());
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

struct SteerTurnState {
    pending: u32,
    deferred_finish: Option<(String, Option<Value>)>,
}

pub struct AcpManager {
    pub app: AppHandle,
    /// 保留 agent 类型供现有路由和事件载荷使用；ACP 实现仅支持 Devin。
    pub kind: AgentKind,
    /// 额度租借实例使用的独立凭证环境；普通全局实例为空。
    launch_env: HashMap<String, String>,
    /// 额度租借实例的权限请求作用域，避免不同进程的递增 RPC id 发生碰撞。
    permission_scope: String,
    /// 用户线程各用独立连接；模型/命令/标题等辅助任务共用 SHARED。
    slots: StdMutex<HashMap<String, Arc<TokioMutex<Option<Arc<AcpConn>>>>>>,
    /// 热连接 LRU 单调时钟：每次连接被取用时 +1 写入 conn.last_used。
    lru_clock: AtomicU64,
    /// CodeBuddy 官方 one-shot 预热槽：草稿页启动，首个匹配目录的用户连接消费。
    codebuddy_prewarm: TokioMutex<Option<CodeBuddyPrewarm>>,
    /// 存活连接计数：spawn 成功 +1、连接关闭 -1；用于 connected() 与断连广播（归零才广播）。
    alive_conns: AtomicU64,
    routes: StdMutex<HashMap<String, Route>>,
    /// 正在 session/load 回放、需要抑制 update 的会话
    loading_sessions: StdMutex<HashSet<String>>,
    running_threads: StdMutex<HashSet<String>>,
    /// Devin 会把运行中引导作为并发 session/prompt 合入当前轮次。主请求可能先返回，
    /// 此时必须把轮次收尾延后到最后一个引导请求结束，避免 UI 提前显示“已停止”。
    steer_turns: StdMutex<HashMap<String, SteerTurnState>>,
    /// 轮次开始时间，用于结束时计算耗时
    turn_started: StdMutex<HashMap<String, std::time::Instant>>,
    /// 诊断：session/prompt 发出时刻 → 用于测量「首响应延迟」(session_id)
    prompt_sent_at: StdMutex<HashMap<String, std::time::Instant>>,
    pending_permissions: StdMutex<HashMap<String, PendingPermission>>,
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
            lru_clock: AtomicU64::new(0),
            codebuddy_prewarm: TokioMutex::new(None),
            alive_conns: AtomicU64::new(0),
            routes: StdMutex::new(HashMap::new()),
            loading_sessions: StdMutex::new(HashSet::new()),
            running_threads: StdMutex::new(HashSet::new()),
            steer_turns: StdMutex::new(HashMap::new()),
            turn_started: StdMutex::new(HashMap::new()),
            prompt_sent_at: StdMutex::new(HashMap::new()),
            pending_permissions: StdMutex::new(HashMap::new()),
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

    /// 用户线程一线程一连接。CodeBuddy 官方预热契约也是「一进程一会话」；共享同一
    /// Streamable HTTP connection 会让后发 session/prompt 中断前一个会话。
    fn conn_key_for_thread(&self, thread_id: &str) -> String {
        thread_connection_key(thread_id)
    }

    fn thread_is_read_only(&self, conn_key: &str) -> bool {
        // CodeBuddy 共享连接且 MCP 按会话注入，只读与否不影响进程。
        if self.kind == AgentKind::CodeBuddy {
            return false;
        }
        let Some(thread_id) = conn_key.strip_prefix("thread:") else {
            return false;
        };
        let state = self.app.state::<AppState>();
        let store = state.store.lock().unwrap();
        store
            .get(thread_id)
            .and_then(|thread| thread.mode.as_deref())
            .map(unify_mode_id)
            .as_deref()
            == Some("plan")
    }

    fn aux_key(&self) -> String {
        SHARED_KEY.to_string()
    }

    /// 取（或创建）某个键的连接槽。槽 = TokioMutex<Option<conn>>，语义等同旧的单连接字段，
    /// 只是按键分裂：不同键各自的槽互不阻塞，可并发建连接/跑会话。
    ///
    /// 辅助探测调用会并发打到 SHARED 槽，所以槽内必须记录「建连中」状态让后来者排队复用，
    /// 避免每个并发调用各自拉起一个 `codebuddy --serve` 常驻进程。
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

    /// 单个 `codebuddy --serve` 进程常驻 200MB+，每个会话各持一条会随会话数线性堆内存，
    /// 拖垮长时间运行后的切换。热连接按 LRU 封顶：CodeBuddy 一进程一会话、最吃内存，只留
    /// 最近 1 条；其余后端留 3 条。被回收线程下次发送时经 session/load 自动恢复上下文。
    /// 共享槽（探测/标题等辅助任务）不参与回收。
    fn hot_thread_conn_cap(&self) -> usize {
        match self.kind {
            AgentKind::CodeBuddy => 1,
            _ => 3,
        }
    }

    fn touch_conn(&self, conn: &Arc<AcpConn>) {
        conn.last_used
            .store(self.lru_clock.fetch_add(1, Ordering::SeqCst) + 1, Ordering::SeqCst);
    }

    /// 超出热连接上限时回收最旧的「非运行中」线程连接；正在跑轮的线程永不回收。
    /// 只清槽并 kill 进程：路由/权限/回放等会话状态由 SSE reader 触发的 on_conn_closed 统一清理，
    /// 旧会话下次发送时走「routes 缺失 → session/load」的既有恢复路径。
    fn evict_lru_thread_conns(self: &Arc<Self>, keep_key: &str) {
        let cap = self.hot_thread_conn_cap();
        let mut victims: Vec<(String, Arc<TokioMutex<Option<Arc<AcpConn>>>>)> = Vec::new();
        {
            let slots = self.slots.lock().unwrap();
            let mut candidates: Vec<(u64, String, Arc<TokioMutex<Option<Arc<AcpConn>>>>)> = slots
                .iter()
                .filter(|(key, _)| {
                    key.starts_with("thread:")
                        && key.as_str() != keep_key
                        && !self.is_running(&key["thread:".len()..])
                })
                .filter_map(|(key, slot)| {
                    slot.try_lock().ok().and_then(|g| g.clone()).map(|conn| {
                        (conn.last_used.load(Ordering::SeqCst), key.clone(), slot.clone())
                    })
                })
                .collect();
            for key in lru_evict_keys(
                candidates.iter().map(|(used, key, _)| (*used, key.as_str())),
                keep_key,
                cap,
            ) {
                if let Some((_, key, slot)) = candidates.iter().find(|(_, k, _)| k == key) {
                    victims.push((key.clone(), slot.clone()));
                }
            }
            candidates.clear();
        }
        if victims.is_empty() {
            return;
        }
        self.push_log(format!(
            "[nova] {} 热连接超过上限（{cap}），回收最久未用的 {} 条；旧会话再次发送时自动恢复",
            self.kind.label(),
            victims.len()
        ));
        for (key, slot) in victims {
            let mgr = self.clone();
            tauri::async_runtime::spawn(async move {
                if let Some(conn) = slot.lock().await.take() {
                    conn.kill();
                }
                mgr.slots.lock().unwrap().remove(&key);
            });
        }
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
        match self.kind {
            AgentKind::CodeBuddy => &settings.codebuddy_proxy,
            _ => &settings.devin_proxy,
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

    /// 杀掉全部 Devin 连接并清空全局路由。
    /// 用于「重启 agent」「改配置」「应用退出」等需要彻底重置的场景。
    pub async fn kill_conn(&self) {
        if let Some(prewarm) = self.codebuddy_prewarm.lock().await.take() {
            prewarm.kill();
        }
        let slots: Vec<_> = self.slots.lock().unwrap().drain().map(|(_, v)| v).collect();
        for slot in slots {
            if let Some(conn) = slot.lock().await.take() {
                conn.kill();
            }
        }
        self.routes.lock().unwrap().clear();
        self.loading_sessions.lock().unwrap().clear();
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
        // CodeBuddy 共享槽会被多个探测调用并发争抢，必须持锁完成「检查→建连」全程，
        // 让第二个调用在锁内看到 first-caller 已写回的存活连接直接复用；
        // Devin 每线程独占槽，此锁只会串行同一线程自己的建连，不影响并发会话。
        let mut guard = slot.lock().await;
        if let Some(c) = guard.as_ref() {
            if c.alive.load(Ordering::SeqCst) {
                self.touch_conn(c);
                return Ok(c.clone());
            }
        }
        let settings = {
            let state = self.app.state::<AppState>();
            let s = state.settings.lock().unwrap().clone();
            s
        };
        let conn = self.spawn_conn(&settings, conn_key, want_cwd).await?;
        self.touch_conn(&conn);
        *guard = Some(conn.clone());
        if conn_key.starts_with("thread:") {
            self.evict_lru_thread_conns(conn_key);
        }
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
        // CodeBuddy 官方 HTTP 传输：`codebuddy --serve` 起本地 HTTP 服务，
        // JSON-RPC 经 POST /api/v1/acp 下发、响应与事件经 SSE 订阅回收。
        if self.kind == AgentKind::CodeBuddy {
            self.push_log(format!(
                "[nova] CodeBuddy 正在启动 HTTP 传输（key={conn_key}）"
            ));
            return self
                .spawn_codebuddy_http_conn(settings, conn_key, want_cwd)
                .await;
        }
        // Devin 走自己的可执行文件与 acp_args。
        let (program, args_str) = (settings.devin_path.clone(), settings.acp_args.clone());
        #[cfg(windows)]
        let mut cmd = build_acp_command(&program, &args_str);
        #[cfg(not(windows))]
        let mut cmd = {
            let mut c = tokio::process::Command::new(&program);
            c.args(args_str.split_whitespace());
            c
        };
        // Devin 的项目级 MCP 配置需要绑定到线程连接的启动目录；CodeBuddy 在
        // session/new 时按标准 ACP mcpServers 注入，进程无需按目录分裂。
        if self.kind == AgentKind::Devin {
            if let Some(cwd) = want_cwd.filter(|_| conn_key.starts_with("thread:")) {
                let launch_dir = prepare_devin_nova_tools_config(
                    &self.app,
                    conn_key,
                    cwd,
                    settings.context_retrieval_mode.as_str(),
                    self.thread_is_read_only(conn_key),
                )?;
                cmd.current_dir(&launch_dir);
                self.push_log(format!(
                    "[nova] Devin 使用本地 nova-tools MCP 配置：{}",
                    launch_dir.display()
                ));
            }
        }
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
        if self.kind == AgentKind::CodeBuddy {
            // Windows 上 CodeBuddy 的 Bash 工具默认走 Git Bash；显式指定 PowerShell，
            // 避免依赖 Git Bash 安装，同时与 Nova 自身的 PowerShell 工具约定一致。
            cmd.env("CODEBUDDY_CODE_SHELL", "powershell");
        }
        {
            let state = self.app.state::<AppState>();
            cmd.env(
                "NOVA_CONTEXT_SERVICE_ENDPOINT",
                state.context_service.endpoint(),
            )
            .env("NOVA_CONTEXT_SERVICE_TOKEN", state.context_service.token())
            .env(
                "NOVA_CONTEXT_RETRIEVAL_MODE",
                settings.context_retrieval_mode.as_str(),
            );
        }
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
            read_only: self.thread_is_read_only(conn_key),
            label: self.kind.label(),
            transport: AcpTransport::Stdio(stdin_tx),
            pending: StdMutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            alive: AtomicBool::new(true),
            child: StdMutex::new(Some(child)),
            log_tasks: StdMutex::new(Vec::new()),
            last_used: AtomicU64::new(0),
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

    /// CodeBuddy 官方 HTTP 传输：优先消费匹配目录的 one-shot 预热进程，否则冷启动
    /// `codebuddy --serve`。POST /api/v1/acp 下发 JSON-RPC，SSE 接收响应与事件。
    /// 鉴权关闭（仅绑环回地址）；服务 stdout 打印的入口行同时作启动就绪信号。
    async fn spawn_codebuddy_http_conn(
        self: &Arc<Self>,
        settings: &Settings,
        conn_key: &str,
        want_cwd: Option<&str>,
    ) -> Result<Arc<AcpConn>, String> {
        crate::skills::sync_skills_from_home();

        if let Some(cwd) = want_cwd {
            let (prewarm, stale) = {
                let mut slot = self.codebuddy_prewarm.lock().await;
                if slot.as_ref().is_some_and(|prewarm| prewarm.cwd == cwd) {
                    (slot.take(), None)
                } else {
                    (None, slot.take())
                }
            };
            if let Some(stale) = stale {
                stale.kill();
            }
            if let Some(prewarm) = prewarm {
                match self
                    .activate_codebuddy_prewarm(settings, conn_key, prewarm)
                    .await
                {
                    Ok(conn) => return Ok(conn),
                    Err(error) => self.push_log(format!(
                        "[nova] CodeBuddy 预热激活失败，回退冷启动：{error}"
                    )),
                }
            }
        }

        let (program, mut cmd) = codebuddy_command(
            &settings.codebuddy_path,
            &[
                "--serve",
                "--port",
                "0",
                "--host",
                "127.0.0.1",
                "--auth",
                "none",
            ],
        );
        if let Some(cwd) = want_cwd {
            cmd.current_dir(cwd);
        }
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        #[cfg(unix)]
        {
            cmd.process_group(0);
        }
        apply_proxy_env(&mut cmd, self.proxy_of(settings));
        cmd.envs(&self.launch_env);
        #[cfg(windows)]
        {
            // Windows 上 CodeBuddy 的 Bash 工具默认走 Git Bash；显式指定 PowerShell，
            // 避免依赖 Git Bash 安装，同时与 Nova 自身的 PowerShell 工具约定一致。
            cmd.env("CODEBUDDY_CODE_SHELL", "powershell");
        }
        {
            let state = self.app.state::<AppState>();
            cmd.env(
                "NOVA_CONTEXT_SERVICE_ENDPOINT",
                state.context_service.endpoint(),
            )
            .env("NOVA_CONTEXT_SERVICE_TOKEN", state.context_service.token())
            .env(
                "NOVA_CONTEXT_RETRIEVAL_MODE",
                settings.context_retrieval_mode.as_str(),
            );
        }
        #[cfg(windows)]
        if self.app.state::<AppState>().windows_shell_shim_enabled {
            if let Err(e) = crate::windows_shell_shim::apply(&self.app, &mut cmd, &self.launch_env)
            {
                self.push_log(format!("[windows-shell-shim] {e}"));
            }
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("无法启动 CodeBuddy（{program}）：{e}"))?;
        assign_to_agent_job(&child);
        let stdout = child.stdout.take().ok_or("无法获取 CodeBuddy stdout")?;
        let stderr = child.stderr.take().ok_or("无法获取 CodeBuddy stderr")?;

        // 服务启动后在 stdout 打印 "Endpoint  http://127.0.0.1:<port>"（见官方文档）；
        // 逐行扫描提取端口，兼作进程就绪信号，避免按端口探测的空转等待。
        // 日志与就绪检测分离：stdout 一旦被 BufReader 持有就会一直读，不能同时再被
        // 另一个任务持有；因此把「endpoint 行已见」经 oneshot 上报后就绪，日志照常走 push_log。
        let (endpoint_tx, endpoint_rx) = oneshot::channel::<Result<String, String>>();
        let stdout_task = {
            let mgr = self.clone();
            tokio::spawn(async move {
                let mut tx = Some(endpoint_tx);
                let mut lines = BufReader::new(stdout).lines();
                loop {
                    match lines.next_line().await {
                        Ok(Some(line)) => {
                            let trimmed = line.trim();
                            // endpoint 只在启动初期出现一次；take 后 tx 为 None 不再重复解析。
                            if tx.is_some() {
                                if let Some(ep) = extract_codebuddy_endpoint(trimmed) {
                                    if let Some(tx) = tx.take() {
                                        let _ = tx.send(Ok(ep));
                                    }
                                }
                            }
                            mgr.push_log(format!("[codebuddy] {trimmed}"));
                        }
                        _ => {
                            if let Some(tx) = tx.take() {
                                let _ =
                                    tx.send(Err("CodeBuddy 服务启动失败（stdout 已关闭）".into()));
                            }
                            break;
                        }
                    }
                }
            })
        };
        let stderr_task = {
            let mgr = self.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    mgr.push_log(format!("[codebuddy] {}", line.trim()));
                }
            })
        };

        let endpoint = match timeout(Duration::from_secs(90), endpoint_rx).await {
            Ok(Ok(Ok(ep))) => {
                self.push_log(format!("[nova] CodeBuddy 服务入口已就绪：{ep}"));
                ep
            }
            Ok(Ok(Err(e))) => {
                let _ = child.start_kill();
                return Err(e);
            }
            Ok(Err(_)) => {
                let _ = child.start_kill();
                return Err("CodeBuddy 服务启动通道异常关闭".into());
            }
            Err(_) => {
                let _ = child.start_kill();
                return Err("等待 CodeBuddy 服务就绪超时（90s）".into());
            }
        };

        self.finish_codebuddy_http_conn(conn_key, endpoint, child, vec![stdout_task, stderr_task])
            .await
    }

    async fn activate_codebuddy_prewarm(
        self: &Arc<Self>,
        settings: &Settings,
        conn_key: &str,
        mut prewarm: CodeBuddyPrewarm,
    ) -> Result<Arc<AcpConn>, String> {
        let helper = resolve_sibling_program(&settings.codebuddy_path, "cbc-prewarm");
        let cwd = prewarm.cwd.clone();
        let id = prewarm.id.clone();
        let (context_endpoint, context_token) = {
            let state = self.app.state::<AppState>();
            (
                state.context_service.endpoint().to_string(),
                state.context_service.token().to_string(),
            )
        };
        let retrieval_mode = settings.context_retrieval_mode.as_str().to_string();
        let proxy = self.proxy_of(settings).trim();
        let proxy = if proxy.is_empty() {
            None
        } else if proxy.contains("://") {
            Some(proxy.to_string())
        } else {
            Some(format!("http://{proxy}"))
        };
        let mut args = vec![
            "activate".to_string(),
            id.clone(),
            "--cwd".into(),
            cwd.clone(),
            "--env".into(),
            format!("NOVA_CONTEXT_SERVICE_ENDPOINT={context_endpoint}"),
            "--env".into(),
            format!("NOVA_CONTEXT_SERVICE_TOKEN={context_token}"),
            "--env".into(),
            format!("NOVA_CONTEXT_RETRIEVAL_MODE={retrieval_mode}"),
        ];
        let activation_env = codebuddy_activation_env(&self.launch_env);
        #[cfg(windows)]
        let activation_env = {
            let mut env = activation_env;
            env.insert("CODEBUDDY_CODE_SHELL".into(), "powershell".into());
            env
        };
        for (key, value) in activation_env {
            args.extend(["--env".into(), format!("{key}={value}")]);
        }
        if let Some(proxy) = proxy {
            for key in ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY"] {
                args.extend(["--env".into(), format!("{key}={proxy}")]);
            }
        }
        args.extend(
            [
                "--",
                "--serve",
                "--port",
                "0",
                "--host",
                "127.0.0.1",
                "--auth",
                "none",
            ]
            .into_iter()
            .map(str::to_string),
        );
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let (_, mut cmd) = codebuddy_command(&helper, &arg_refs);
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        let output = match timeout(Duration::from_secs(10), cmd.output()).await {
            Ok(Ok(output)) => output,
            Ok(Err(error)) => {
                prewarm.kill();
                return Err(format!("无法执行 cbc-prewarm activate：{error}"));
            }
            Err(_) => {
                prewarm.kill();
                return Err("cbc-prewarm activate 超时（10s）".into());
            }
        };
        if !output.status.success() {
            prewarm.kill();
            return Err(format!(
                "cbc-prewarm activate 失败：{}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        let endpoint = match timeout(Duration::from_secs(180), &mut prewarm.endpoint_rx).await {
            Ok(Ok(Ok(endpoint))) => endpoint,
            Ok(Ok(Err(error))) => {
                prewarm.kill();
                return Err(error);
            }
            Ok(Err(_)) => {
                prewarm.kill();
                return Err("CodeBuddy 预热就绪通道已关闭".into());
            }
            Err(_) => {
                prewarm.kill();
                return Err("等待 CodeBuddy 预热服务就绪超时（180s）".into());
            }
        };
        self.push_log(format!("[nova] CodeBuddy 已消费预热进程 {id}：{endpoint}"));
        self.finish_codebuddy_http_conn(conn_key, endpoint, prewarm.child, prewarm.log_tasks)
            .await
    }

    async fn finish_codebuddy_http_conn(
        self: &Arc<Self>,
        conn_key: &str,
        endpoint: String,
        child: Child,
        log_tasks: Vec<JoinHandle<()>>,
    ) -> Result<Arc<AcpConn>, String> {
        let client = reqwest::Client::builder()
            .no_proxy()
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| format!("创建 CodeBuddy HTTP 客户端失败：{e}"))?;
        let acp_url = format!("{endpoint}/api/v1/acp");

        // 1) 建立 ACP 连接
        let conn_resp = client
            .post(format!("{acp_url}/connect"))
            .header("X-CodeBuddy-Request", "1")
            .json(&json!({}))
            .timeout(Duration::from_secs(15))
            .send()
            .await
            .map_err(|e| format!("CodeBuddy ACP 连接建立失败：{e}"))?;
        let conn_status = conn_resp.status();
        let conn_body: Value = conn_resp
            .json()
            .await
            .map_err(|e| format!("CodeBuddy ACP 连接响应解析失败：{e}"))?;
        self.push_log(format!(
            "[nova] CodeBuddy ACP connect HTTP {conn_status}: {}",
            conn_body.to_string().chars().take(300).collect::<String>()
        ));
        let connection_id = conn_body
            .pointer("/data/connectionId")
            .or_else(|| conn_body.get("connectionId"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| format!("CodeBuddy ACP 连接响应缺少 connectionId：{conn_body}"))?;
        let session_token = conn_body
            .pointer("/data/sessionToken")
            .or_else(|| conn_body.get("sessionToken"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| format!("CodeBuddy ACP 连接响应缺少 sessionToken：{conn_body}"))?;

        let (inbound_tx, mut inbound_rx) = mpsc::unbounded_channel::<String>();
        let conn = Arc::new(AcpConn {
            key: conn_key.to_string(),
            read_only: false,
            label: self.kind.label(),
            transport: AcpTransport::Http {
                client: client.clone(),
                url: acp_url.clone(),
                connection_id: connection_id.clone(),
                session_token: session_token.clone(),
                inbound: inbound_tx,
            },
            pending: StdMutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            alive: AtomicBool::new(true),
            child: StdMutex::new(Some(child)),
            log_tasks: StdMutex::new(log_tasks),
            last_used: AtomicU64::new(0),
        });
        // POST SSE 与独立 GET SSE 可并行到达；统一串到��有 ACP 消息路由。
        // 使用 Weak 避免 receiver 与 conn.transport.inbound 形成引用环。
        {
            let mgr = self.clone();
            let weak_conn = Arc::downgrade(&conn);
            tokio::spawn(async move {
                while let Some(payload) = inbound_rx.recv().await {
                    let Some(conn) = weak_conn.upgrade() else {
                        break;
                    };
                    mgr.handle_line(&conn, &payload);
                }
            });
        }
        // 每创建一条连接 +1；SSE reader 结束时在 on_conn_closed 里 -1，恒定配对。
        self.alive_conns.fetch_add(1, Ordering::SeqCst);

        // 2) 独立 SSE 只订阅不属于某个 POST 的异步通知；请求响应由各自 POST 流接收。
        let sse_resp = client
            .get(&acp_url)
            .header("X-CodeBuddy-Request", "1")
            .header("acp-connection-id", &connection_id)
            .header("acp-session-token", &session_token)
            .header("Accept", "text/event-stream")
            // SSE 是长连接，不能用 request timeout；建连超时已由 connect_timeout 封顶。
            .send()
            .await
            .map_err(|e| format!("CodeBuddy ACP 事件流订阅失败：{e}"))?;
        let sse_status = sse_resp.status();
        let sse_content_type = sse_resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        self.push_log(format!(
            "[nova] CodeBuddy ACP SSE HTTP {sse_status} content-type={sse_content_type}"
        ));
        if !sse_status.is_success() {
            conn.kill();
            return Err(format!("CodeBuddy ACP 事件流订阅被拒：{sse_status}"));
        }
        {
            let mgr = self.clone();
            let conn2 = conn.clone();
            tokio::spawn(async move {
                let mut stream = sse_resp.bytes_stream();
                let mut decoder = SseDecoder::default();
                while let Some(chunk) = stream.next().await {
                    let Ok(chunk) = chunk else { break };
                    for payload in decoder.push(&chunk) {
                        mgr.handle_line(&conn2, &payload);
                    }
                }
                for payload in decoder.finish() {
                    mgr.handle_line(&conn2, &payload);
                }
                mgr.push_log("[nova] CodeBuddy ACP 事件流已断开".into());
                mgr.on_conn_closed(&conn2).await;
            });
        }

        // 3) initialize 握手（与 stdio 传输同一套载荷）
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
                    "clientCapabilities": { "fs": { "readTextFile": false, "writeTextFile": false } }
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
                self.push_log(format!("[nova] CodeBuddy HTTP 服务已连接：{endpoint}"));
                Ok(conn)
            }
            Err(e) => {
                conn.kill();
                Err(format!("CodeBuddy ACP 初始化失败：{e}"))
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
        // 辅助连接关闭时，跑在其上的标题任务作废。
        if key == self.aux_key() {
            self.title_jobs.lock().unwrap().clear();
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
            // HTTP 传输下 stdout 行就是 SSE payload，把无法解析的帧如实记录便于诊断。
            self.push_log(format!(
                "[nova] 无法解析的 {} 消息: {line}",
                self.kind.label()
            ));
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
                // 统一 Build 模式会放开全部权限。若后端仍上报授权请求，
                // 这里代答 allow；Plan 等其他模式照旧弹给用户审批。
                let is_build = {
                    let state = self.app.state::<AppState>();
                    let store = state.store.lock().unwrap();
                    store
                        .get(&thread_id)
                        .and_then(|t| t.mode.clone())
                        .map(|m| is_full_permission_mode(&m))
                        .unwrap_or(false)
                };
                if is_build {
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
                    if is_build {
                        let title = tool_call
                            .get("title")
                            .and_then(|v| v.as_str())
                            .unwrap_or("工具调用");
                        self.push_log(format!("[nova] Build 模式自动批准授权：{title}"));
                    }
                    conn.respond_ok(id, json!({ "outcome": outcome }));
                    return;
                }
                let key = self.permission_key(conn, &id);
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
                        if let Item::Tool { ts, call, .. } = item {
                            if call.tool_call_id == tc_id {
                                merge_tool_call(call, update);
                                completed = call.status == "completed" || call.status == "failed";
                                if completed {
                                    set_tool_duration(call, now_ms().saturating_sub(*ts) as u64);
                                }
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
                        store.save_thread(&thread_id);
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
                            store.save_thread(&thread_id);
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
            store.save_thread(thread_id);
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
            store.save_thread(thread_id);
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
        self.steer_turns.lock().unwrap().remove(thread_id);
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
            store.save_thread(thread_id);
        }
        self.maybe_emit_plan_action(thread_id, &stop_reason);
        self.set_running(thread_id, false, Some(stop_reason.clone()));
        self.notify_done(thread_id, &stop_reason);
    }

    /// 若还有已经受理的 Devin 引导请求，则由最后一个引导请求负责收尾。
    fn finish_turn_after_steers(&self, thread_id: &str, stop_reason: String, usage: Option<Value>) {
        {
            let mut turns = self.steer_turns.lock().unwrap();
            if let Some(state) = turns.get_mut(thread_id) {
                if state.pending > 0 {
                    state.deferred_finish = Some((stop_reason, usage));
                    return;
                }
            }
        }
        self.finish_turn(thread_id, stop_reason, usage);
    }

    fn reserve_steer(&self, thread_id: &str) {
        let mut turns = self.steer_turns.lock().unwrap();
        let state = turns
            .entry(thread_id.to_string())
            .or_insert(SteerTurnState {
                pending: 0,
                deferred_finish: None,
            });
        state.pending = state.pending.saturating_add(1);
    }

    fn complete_steer(&self, thread_id: &str) {
        let deferred = {
            let mut turns = self.steer_turns.lock().unwrap();
            let Some(state) = turns.get_mut(thread_id) else {
                return;
            };
            state.pending = state.pending.saturating_sub(1);
            if state.pending == 0 {
                turns
                    .remove(thread_id)
                    .and_then(|state| state.deferred_finish)
            } else {
                None
            }
        };
        if let Some((stop_reason, usage)) = deferred {
            // cancel/force-finish 可能已在引导 RPC 返回前结束轮次。
            if self.is_running(thread_id) {
                self.finish_turn(thread_id, stop_reason, usage);
                let _ = self.app.emit(EV_THREADS, json!({}));
            }
        }
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
                Some(t) => t.title.clone(),
                None => return,
            }
        };
        // 大熊座训练/演进会话静默完成，不弹系统通知。
        if title.starts_with("经验训练") || title.starts_with("世代演进") {
            return;
        }
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
            let current = models
                .get("currentModelId")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            if options.is_empty() && current.is_empty() {
                return Value::Null;
            }
            // CodeBuddy 云端模型清单异步就绪：session/new 若在就绪前返回，availableModels 会缺
            // 云端动态模型（如 hy4-preview），但 currentModelId 已是它。把当前模型并入列表（置顶），
            // 否则选择器里看不到、用户一选会话就被解析回列表第一项。
            let options = merge_current_model_option(options, current);
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
        // CodeBuddy 同时提供标准 models 和兼容旧客户端的 configOptions，两边可能分别缺少
        // 刚下发的动态模型，因此以标准项为主合并扩展项；Devin 继续优先其扩展元数据。
        let extension_config_options = result.get("configOptions").cloned().unwrap_or(Value::Null);
        let standard_config_options = models_to_config_options(result.get("models"));
        let config_options =
            if self.kind == AgentKind::CodeBuddy && !standard_config_options.is_null() {
                merge_model_config_option(extension_config_options, standard_config_options)
            } else if has_config {
                extension_config_options
            } else {
                standard_config_options
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
        // 额度借用实例属于另一个账号；只保留自己的进程内列表，不能覆盖主账号缓存/UI。
        if publishes_model_options(&self.launch_env) {
            self.persist_model_options(&v);
            let _ = self.app.emit(
                EV_OPTIONS,
                json!({ "agentKind": self.kind.as_str(), "options": v }),
            );
        }
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

    /// CodeBuddy 官方预热：后台完成 bundle / DI / 配置 / MCP discovery，首条消息时
    /// one-shot 激活为当前项目的 `--serve --port 0` 进程。Devin 保持原行为，不预热。
    pub async fn prewarm(self: &Arc<Self>, cwd: String) {
        if self.kind != AgentKind::CodeBuddy {
            return;
        }
        // 持槽锁完成替换，保证用户快速切换 A→B 项目时旧 A 的迟到结果不会覆盖 B。
        let mut slot = self.codebuddy_prewarm.lock().await;
        if slot.as_ref().is_some_and(|prewarm| prewarm.cwd == cwd) {
            return;
        }
        let settings = {
            let state = self.app.state::<AppState>();
            let settings = state.settings.lock().unwrap().clone();
            settings
        };
        match self.spawn_codebuddy_prewarm(&settings, cwd.clone()).await {
            Ok(prewarm) => {
                if let Some(old) = slot.replace(prewarm) {
                    old.kill();
                }
                self.push_log(format!("[nova] CodeBuddy 预热已就绪：{cwd}"));
            }
            Err(error) => self.push_log(format!("[nova] CodeBuddy 预热失败：{error}")),
        }
    }

    async fn spawn_codebuddy_prewarm(
        self: &Arc<Self>,
        settings: &Settings,
        cwd: String,
    ) -> Result<CodeBuddyPrewarm, String> {
        let id = format!("nova-{}", uuid::Uuid::new_v4().simple());
        let (program, mut cmd) = codebuddy_command(
            &settings.codebuddy_path,
            &["--prewarm", "--prewarm-id", id.as_str()],
        );
        cmd.current_dir(&cwd)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        cmd.creation_flags(0x0800_0000);
        #[cfg(unix)]
        cmd.process_group(0);
        apply_proxy_env(&mut cmd, self.proxy_of(settings));
        cmd.envs(&self.launch_env);
        #[cfg(windows)]
        cmd.env("CODEBUDDY_CODE_SHELL", "powershell");
        {
            let state = self.app.state::<AppState>();
            cmd.env(
                "NOVA_CONTEXT_SERVICE_ENDPOINT",
                state.context_service.endpoint(),
            )
            .env("NOVA_CONTEXT_SERVICE_TOKEN", state.context_service.token())
            .env(
                "NOVA_CONTEXT_RETRIEVAL_MODE",
                settings.context_retrieval_mode.as_str(),
            );
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("无法启动 CodeBuddy 预热进程（{program}）：{e}"))?;
        assign_to_agent_job(&child);
        let stdout = child
            .stdout
            .take()
            .ok_or("无法获取 CodeBuddy 预热 stdout")?;
        let stderr = child
            .stderr
            .take()
            .ok_or("无法获取 CodeBuddy 预热 stderr")?;
        let (endpoint_tx, endpoint_rx) = oneshot::channel();
        let stdout_task = {
            let mgr = self.clone();
            tokio::spawn(async move {
                let mut endpoint_tx = Some(endpoint_tx);
                let mut lines = BufReader::new(stdout).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let trimmed = line.trim();
                    if let Some(endpoint) = extract_codebuddy_endpoint(trimmed) {
                        if let Some(tx) = endpoint_tx.take() {
                            let _ = tx.send(Ok(endpoint));
                        }
                    }
                    mgr.push_log(format!("[codebuddy-prewarm] {trimmed}"));
                }
                if let Some(tx) = endpoint_tx {
                    let _ = tx.send(Err("CodeBuddy 预热进程在激活前退出".into()));
                }
            })
        };
        let stderr_task = {
            let mgr = self.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    mgr.push_log(format!("[codebuddy-prewarm] {}", line.trim()));
                }
            })
        };
        // 官方客户端的 ping 是毫秒级 IPC；总等待严格封顶 30s，绝不无限阻塞调用线程。
        let helper = resolve_sibling_program(&settings.codebuddy_path, "cbc-prewarm");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        while tokio::time::Instant::now() < deadline {
            let (_, mut ping) = codebuddy_command(&helper, &["ping", id.as_str()]);
            ping.stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true);
            #[cfg(windows)]
            ping.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
            if timeout(Duration::from_secs(3), ping.status())
                .await
                .ok()
                .and_then(Result::ok)
                .is_some_and(|status| status.success())
            {
                return Ok(CodeBuddyPrewarm {
                    id,
                    cwd,
                    child,
                    endpoint_rx,
                    log_tasks: vec![stdout_task, stderr_task],
                });
            }
            if child.try_wait().ok().flatten().is_some() {
                stdout_task.abort();
                stderr_task.abort();
                return Err("CodeBuddy 预热进程提前退出".into());
            }
            sleep(Duration::from_millis(200)).await;
        }
        stdout_task.abort();
        stderr_task.abort();
        if let Some(pid) = child.id() {
            kill_process_tree(pid);
        }
        let _ = child.start_kill();
        Err("等待 CodeBuddy 预热 IPC 就绪超时（30s）".into())
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
        // 每个用户线程使用独立连接：Devin 隔离项目 MCP；CodeBuddy 避免新 prompt 中断旧会话。
        let key = self.conn_key_for_thread(thread_id);
        let conn = self.ensure_conn_for(&key, Some(&cwd)).await?;

        let read_only = mode.as_deref().map(unify_mode_id).as_deref() == Some("plan");
        let mcp_servers = self.session_mcp_servers(&cwd, read_only, thread_id)?;
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
                                "mcpServers": mcp_servers.clone()
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
                        let new_sid = self
                            .new_session_for(&conn, &key, thread_id, &cwd, &mcp_servers)
                            .await?;
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
                        store.save_thread(thread_id);
                        new_sid
                    }
                }
            }
            None => {
                let sid = self
                    .new_session_for(&conn, &key, thread_id, &cwd, &mcp_servers)
                    .await?;
                {
                    let state = self.app.state::<AppState>();
                    let mut store = state.store.lock().unwrap();
                    if let Some(thread) = store.get_mut(thread_id) {
                        thread.acp_session_id = Some(sid.clone());
                    }
                    store.save_thread(thread_id);
                }
                sid
            }
        };

        self.apply_session_config(&conn, &key, &sid, model, mode)
            .await;
        Ok(sid)
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
        let (need_model, need_mode) = {
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
                    // 工作流会话：剥掉模型可能重复生成的前缀，统一保证单个 [WF] 前缀。
                    let title = if job.fallback_title.starts_with("[WF]") {
                        format!("[WF] {}", title.trim_start_matches("[WF]").trim_start())
                    } else {
                        title
                    };
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

    /// Devin 的模式读写权限绑定进程级 MCP 配置，变化时要重启连接刷新；
    /// CodeBuddy 无此耦合，模式变化经 session/set_mode 即时生效。
    fn restart_conn_on_mode_change(&self) -> bool {
        self.kind != AgentKind::CodeBuddy
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
        let want_read_only = mode.as_deref().map(unify_mode_id).as_deref() == Some("plan");
        if self.restart_conn_on_mode_change() && conn.read_only != want_read_only {
            self.routes.lock().unwrap().remove(&sid);
            if let Some(slot) = self.slot_opt(&key) {
                if let Some(conn) = slot.lock().await.take() {
                    conn.kill();
                }
            }
            self.slots.lock().unwrap().remove(&key);
            self.push_log(format!(
                "[nova] Devin 模式读写权限变化，已重启线程连接以刷新 nova-tools（thread={thread_id}）"
            ));
            return;
        }
        self.apply_session_config(&conn, &key, &sid, model, mode)
            .await;
    }

    async fn new_session_for(
        self: &Arc<Self>,
        conn: &Arc<AcpConn>,
        conn_key: &str,
        thread_id: &str,
        cwd: &str,
        mcp_servers: &Value,
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
                        "mcpServers": mcp_servers
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
                store.save_thread(&thread_id);
            }
            ctx
        };
        // `/browser` 进入持续浏览器调试模式：本轮起 nova-tools MCP 附带 browser 工具，
        // 模式跨轮次保留直到 /browser-exit。
        let browser_command =
            acp_supports_browser_debug(&self.kind) && crate::lyra::is_browser_command(&text);
        let browser_exit_command =
            acp_supports_browser_debug(&self.kind) && crate::lyra::is_browser_exit_command(&text);
        if browser_command || browser_exit_command {
            let state = self.app.state::<AppState>();
            let mut store = state.store.lock().unwrap();
            if let Some(thread) = store.get_mut(&thread_id) {
                thread.browser_debug_mode = browser_command;
            }
            store.save_thread(&thread_id);
            let _ = self.app.emit(EV_THREADS, json!({}));
        }
        // Browser 准备与 CodeBuddy/Devin 的连接、session 创建并行，避免首次工具调用再串行
        // 安装运行时、启动录制进程和等待执行端口。
        if browser_command {
            let app = self.app.clone();
            tauri::async_runtime::spawn(async move {
                let bridge = crate::browser::ensure_mcp_bridge(&app);
                let recorder = crate::browser::ensure_exec_port(&app);
                let (bridge_result, recorder_result) = tokio::join!(bridge, recorder);
                if let Err(error) = bridge_result {
                    eprintln!("[browser] 启动 MCP 中转失败：{error}");
                }
                if let Err(error) = recorder_result {
                    eprintln!("[browser] 预热 Playwright 失败：{error}");
                }
            });
        }
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
            store.save_thread(&thread_id);
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

        let text = if acp_supports_browser_debug(&self.kind) {
            crate::lyra::expand_browser_command(&text)
                .or_else(|| crate::lyra::expand_browser_exit_command(&text))
                .unwrap_or(text)
        } else {
            text
        };
        let outcome = self
            .drive_prompt(&thread_id, &text, &images, handoff.as_deref())
            .await;

        // 轮次已被强制结束（看门狗/重启 devin），丢弃迟到的结果
        if !self.is_running(&thread_id) {
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
        self.finish_turn_after_steers(&thread_id, stop_reason, usage);
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
        &self,
        thread_id: &str,
        text: &str,
        images: &[PromptImage],
        include_runtime_guidance: bool,
    ) -> Vec<Value> {
        let mut prompt = Self::build_prompt_blocks(text, images);
        let mut guidance = Vec::new();
        if include_runtime_guidance {
            let (context_tools, read_only) = {
                let state = self.app.state::<AppState>();
                let context_tools = state.settings.lock().unwrap().context_tools_enabled();
                let read_only = {
                    let store = state.store.lock().unwrap();
                    store
                        .get(thread_id)
                        .and_then(|t| t.mode.as_deref())
                        .map(unify_mode_id)
                        .as_deref()
                        == Some("plan")
                };
                (context_tools, read_only)
            };
            guidance.push(nova_tools_prompt_guidance(context_tools, read_only));
        }
        // CodeBuddy 的工作目录契约逐轮附在用户文本末尾。其 CLI 会把同一条 ACP
        // prompt 中最后一个 text block 当成最新指令；若 guidance 放在最前面，模型可能
        // 把它当成当前消息而忽略随后真正的用户请求。
        let runtime_guidance = match self.kind {
            AgentKind::CodeBuddy => {
                let cwd = {
                    let state = self.app.state::<AppState>();
                    let store = state.store.lock().unwrap();
                    store
                        .get(thread_id)
                        .map(|thread| thread.cwd.clone())
                        .unwrap_or_default()
                };
                codebuddy_runtime_guidance(&cwd)
            }
            AgentKind::Devin if include_runtime_guidance => {
                let context_tools = self
                    .app
                    .state::<AppState>()
                    .settings
                    .lock()
                    .unwrap()
                    .context_tools_enabled();
                devin_runtime_guidance(context_tools)
            }
            _ => None,
        };
        if let Some(runtime) = runtime_guidance {
            guidance.push(runtime);
        }
        if !guidance.is_empty() {
            let guidance = guidance.join("\n\n");
            if self.kind == AgentKind::CodeBuddy {
                if let Some(block) = prompt
                    .iter_mut()
                    .find(|block| block.get("type").and_then(Value::as_str) == Some("text"))
                {
                    let text = block.get("text").and_then(Value::as_str).unwrap_or_default();
                    block["text"] = json!(format!("{text}\n\n<system-reminder>\n{guidance}\n</system-reminder>"));
                } else {
                    prompt.push(json!({ "type": "text", "text": guidance }));
                }
            } else {
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
        // 先登记引导，再做 session/连接准备；否则主 prompt 可能正好在结论阶段返回，
        // 抢先发出 running:false，而引导 RPC 实际仍在继续。
        if self.is_running(&thread_id) {
            self.reserve_steer(&thread_id);
        } else {
            self.clone().run_prompt(thread_id, text, images).await;
            return;
        }

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
            self.complete_steer(&thread_id);
            self.clone().run_prompt(thread_id, text, images).await;
            return;
        }
        let err = |msg: String| {
            let state = self.app.state::<AppState>();
            let mut store = state.store.lock().unwrap();
            if let Some(thread) = store.get_mut(&thread_id) {
                let item = thread.push_system(msg, "error");
                store.save_thread(&thread_id);
                self.emit_update(&thread_id, json!({ "t": "upsert", "item": item }));
            }
        };
        let Some(session_id) = session_id else {
            self.complete_steer(&thread_id);
            err("引导消息发送失败：会话尚未建立".into());
            return;
        };
        let Some(conn) = self
            .conn_for_key(&self.conn_key_for_thread(&thread_id))
            .await
        else {
            self.complete_steer(&thread_id);
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
            store.save_thread(&thread_id);
            self.emit_update(&thread_id, json!({ "t": "upsert", "item": item }));
        }
        self.mark_plan_interrupted(&thread_id, "interrupted", false);
        self.emit_proposed_plan(&thread_id, None);
        let _ = self.app.emit(EV_THREADS, json!({}));
        // 浏览器调试模式下，引导消息同样要带模式上下文。
        let text = if acp_supports_browser_debug(&self.kind) {
            let browser_debug_mode = {
                let state = self.app.state::<AppState>();
                let store = state.store.lock().unwrap();
                store
                    .get(&thread_id)
                    .map(|thread| thread.browser_debug_mode)
                    .unwrap_or(false)
            };
            let expanded = crate::lyra::expand_browser_command(&text)
                .or_else(|| crate::lyra::expand_browser_exit_command(&text))
                .unwrap_or(text);
            if browser_debug_mode
                && !crate::lyra::is_browser_command(&expanded)
                && !crate::lyra::is_browser_exit_command(&expanded)
            {
                format!(
                    "{expanded}\n\n（当前处于浏览器调试模式，可使用 browser 工具打开页面、查看 Console/错误并截图联合作业。）"
                )
            } else {
                expanded
            }
        } else {
            text
        };
        let prompt = Self::build_prompt_blocks(&text, &images);
        let mgr = self.clone();
        let tid = thread_id.clone();
        // 该请求要到轮次结束才返回（与主 prompt 一同返回），结果由主 drive 收尾，这里只记录失败
        tauri::async_runtime::spawn(async move {
            let result = conn
                .request(
                    "session/prompt",
                    json!({ "sessionId": session_id, "prompt": prompt }),
                    None,
                )
                .await;
            // 必须先释放引导占位；若主请求已经返回，这一步会完成被延后的轮次收尾。
            mgr.complete_steer(&tid);
            if let Err(e) = result {
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
                        store.save_thread(&tid);
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
        let mut prompt =
            self.build_user_prompt_blocks(thread_id, text, images, include_runtime_guidance);
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
                            store.save_thread(thread_id);
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
        store.save_thread(thread_id);
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
            store.save_thread(thread_id);
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

    /// 该连接需要自动代答的权限请求作用域：Devin 与 CodeBuddy 的递增 RPC id
    /// 都在同一前端路由表里，CodeBuddy 加 cbp- 前缀避免键碰撞。
    fn permission_scope_prefix(&self) -> String {
        if self.permission_scope.is_empty() && self.kind == AgentKind::CodeBuddy {
            "cbp-".to_string()
        } else {
            self.permission_scope.clone()
        }
    }

    /// Devin 保持无前缀的 perm- key；CodeBuddy 每线程一条连接、RPC id 各自递增，
    /// 必须把连接键纳入作用域，避免两个并发会话的权限请求互相覆盖。
    fn permission_key(&self, conn: &AcpConn, id: &Value) -> String {
        let scope = if self.kind == AgentKind::CodeBuddy {
            format!("{}{}-", self.permission_scope_prefix(), conn.key)
        } else {
            self.permission_scope_prefix()
        };
        permission_request_key(&scope, id)
    }

    pub fn has_pending_permission(&self, request_key: &str) -> bool {
        self.pending_permissions
            .lock()
            .unwrap()
            .contains_key(request_key)
    }

    /// 杀光全部连接（供「保存设置重启 agent」复用；模型列表下次经
    /// session/new 重新捕获）。
    pub fn shutdown(self: &Arc<Self>) {
        let mgr = self.clone();
        tauri::async_runtime::spawn(async move {
            mgr.kill_conn().await;
        });
    }

    /// session/new 与 session/load 携带的 MCP server 列表。
    /// Devin 靠进程启动目录的本地 MCP 配置；CodeBuddy 按 ACP mcpServers 注入 polaris / browser。
    fn session_mcp_servers(
        &self,
        cwd: &str,
        read_only: bool,
        thread_id: &str,
    ) -> Result<Value, String> {
        if self.kind != AgentKind::CodeBuddy {
            return Ok(json!([]));
        }
        let state = self.app.state::<AppState>();
        let browser_debug = {
            let store = state.store.lock().unwrap();
            store
                .get(thread_id)
                .map(|thread| thread.browser_debug_mode)
                .unwrap_or(false)
        };
        let context_mode = {
            let settings = state.settings.lock().unwrap();
            settings.context_retrieval_mode.as_str().to_string()
        };
        // browser 独立于上下文检索；关闭 polaris 时仍需挂载只包含 browser 的 nova-tools。
        if !browser_debug && !state.settings.lock().unwrap().context_tools_enabled() {
            return Ok(json!([]));
        }
        let server = codebuddy_nova_tools_mcp_server(
            &self.app,
            cwd,
            &context_mode,
            read_only,
            state.context_service.endpoint(),
            state.context_service.token(),
            browser_debug,
            &state.config_dir,
        )?;
        self.push_log(format!(
            "[nova] CodeBuddy 已为 {cwd} 注入 nova-tools{}",
            if browser_debug { "/browser" } else { "" }
        ));
        Ok(json!([server]))
    }

    pub fn forget_session_of_thread(self: &Arc<Self>, thread_id: &str) {
        self.routes
            .lock()
            .unwrap()
            .retain(|_, r| r.thread_id != thread_id);
        self.thread_locks.lock().unwrap().remove(thread_id);
        let key = self.conn_key_for_thread(thread_id);
        let mgr = self.clone();
        tauri::async_runtime::spawn(async move {
            if let Some(slot) = mgr.slot_opt(&key) {
                if let Some(conn) = slot.lock().await.take() {
                    conn.kill();
                }
            }
            mgr.slots.lock().unwrap().remove(&key);
        });
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

/// Windows：构造 CodeBuddy 启动命令（复用 build_acp_command 的 exe/cmd 解析逻辑）。
#[cfg(windows)]
fn codebuddy_command(program: &str, args: &[&str]) -> (String, tokio::process::Command) {
    let mut cmd = build_acp_command(program, &args.join(" "));
    // CodeBuddy 2.143 即使 MCP server 标记 defer_loading=false，仍可能把工具放进
    // DeferExecuteTool；进程级关闭后 nova-tools 的 polaris/browser 会直接进入工具列表。
    cmd.env("CODEBUDDY_DEFER_TOOL_LOADING", "0");
    (program.to_string(), cmd)
}

/// 非 Windows：直接以给定程序与参数启动。
#[cfg(not(windows))]
fn codebuddy_command(program: &str, args: &[&str]) -> (String, tokio::process::Command) {
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args).env("CODEBUDDY_DEFER_TOOL_LOADING", "0");
    (program.to_string(), cmd)
}

/// LRU 回收决策：keep_key 即将占用一个名额，返回最旧的若干候选键让总数压到 cap 以内。
/// 纯函数抽出，evict_lru_thread_conns 收集候选后据此挑选回收对象。
fn lru_evict_keys<'a>(
    candidates: impl Iterator<Item = (u64, &'a str)>,
    keep_key: &'a str,
    cap: usize,
) -> Vec<&'a str> {
    let mut pool: Vec<(u64, &'a str)> = candidates.collect();
    pool.push((u64::MAX, keep_key)); // keep 永远最新，不参与淘汰
    let overflow = pool.len().saturating_sub(cap);
    pool.sort_unstable_by_key(|(used, _)| *used);
    pool.into_iter()
        .take(overflow)
        .filter(|(_, key)| *key != keep_key)
        .map(|(_, key)| key)
        .collect()
}

/// `cbc-prewarm` 与配置的 CodeBuddy CLI 同目录安装；找不到同目录 helper 时退回 PATH。
fn resolve_sibling_program(configured_program: &str, sibling: &str) -> String {
    let Some(program) = resolve_program_on_path(configured_program) else {
        return sibling.to_string();
    };
    let Some(parent) = program.parent() else {
        return sibling.to_string();
    };
    #[cfg(windows)]
    for extension in ["exe", "cmd", "bat"] {
        let candidate = parent.join(format!("{sibling}.{extension}"));
        if candidate.is_file() {
            return candidate.to_string_lossy().to_string();
        }
    }
    #[cfg(not(windows))]
    {
        let candidate = parent.join(sibling);
        if candidate.is_file() {
            return candidate.to_string_lossy().to_string();
        }
    }
    sibling.to_string()
}

/// `cbc-prewarm activate` 通过 `--env` 显式构造最终服务环境；普通本机会话必须把
/// 当前进程继承的 CodeBuddy 配置一并传入，否则预热路径会表现为启动后环境变量消失。
/// 额度借用保持隔离，只使用租约提供的 launch_env，禁止继承本机 CodeBuddy 凭据。
fn merge_codebuddy_activation_env(
    launch_env: &HashMap<String, String>,
    inherited: impl IntoIterator<Item = (String, String)>,
) -> std::collections::BTreeMap<String, String> {
    let borrowed = launch_env.contains_key("NOVA_QUOTA_BORROWED");
    let mut env = std::collections::BTreeMap::new();
    if !borrowed {
        env.extend(
            inherited
                .into_iter()
                .filter(|(key, _)| key.to_ascii_uppercase().starts_with("CODEBUDDY_")),
        );
    }
    env.extend(launch_env.clone());
    env
}

fn codebuddy_activation_env(
    launch_env: &HashMap<String, String>,
) -> std::collections::BTreeMap<String, String> {
    merge_codebuddy_activation_env(launch_env, std::env::vars())
}

/// 从 CodeBuddy `--serve` 的 stdout 行提取服务入口（官方文档：打印 "Endpoint  http://127.0.0.1:<port>"）。
fn extract_codebuddy_endpoint(line: &str) -> Option<String> {
    let rest = line.strip_prefix("Endpoint")?.trim_start();
    let rest = rest.strip_prefix("http://127.0.0.1:")?;
    let port: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    (!port.is_empty()).then(|| format!("http://127.0.0.1:{port}"))
}

#[cfg(test)]
mod codebuddy_http_tests {
    use super::{
        codebuddy_command, codebuddy_nova_tools_mcp_server_value, codebuddy_runtime_guidance,
        extract_codebuddy_endpoint, json_rpc_request_id, lru_evict_keys,
        merge_codebuddy_activation_env, merge_current_model_option, merge_model_config_option,
        publishes_model_options, thread_connection_key, AcpManager, SseDecoder,
    };
    use serde_json::json;
    use std::collections::HashMap;

    #[test]
    fn codebuddy_process_disables_deferred_tool_loading() {
        let (_, command) = codebuddy_command("codebuddy", &["--serve"]);
        assert!(command.as_std().get_envs().any(|(key, value)| {
            key == "CODEBUDDY_DEFER_TOOL_LOADING" && value == Some(std::ffi::OsStr::new("0"))
        }));
    }

    #[test]
    fn lru_evict_keys_keeps_total_within_cap_and_never_evicts_keep() {
        // 未超上限：不回收
        assert!(lru_evict_keys([(1, "thread:a")].into_iter(), "thread:b", 2).is_empty());
        // cap=1（CodeBuddy）：新连接占掉名额，旧连接必须回收
        assert_eq!(
            lru_evict_keys([(1, "thread:a")].into_iter(), "thread:b", 1),
            ["thread:a"]
        );
        // 超上限：回收最旧的，keep 即使最旧也不回收
        assert_eq!(
            lru_evict_keys(
                [(3, "thread:c"), (1, "thread:a"), (2, "thread:b")].into_iter(),
                "thread:d",
                3
            ),
            ["thread:a"]
        );
        assert_eq!(
            lru_evict_keys([(1, "thread:a"), (2, "thread:b")].into_iter(), "thread:c", 1),
            ["thread:a", "thread:b"]
        );
        // cap 为 0 时全部回收（防御分支，实际 cap >= 1）
        assert_eq!(
            lru_evict_keys([(1, "thread:a")].into_iter(), "thread:b", 0),
            ["thread:a"]
        );
    }

    #[test]
    fn merge_current_model_option_pins_unlisted_current_model() {
        // 当前模型不在清单里：并入并置顶，且不影响其它项
        let merged = merge_current_model_option(
            vec![json!({ "value": "hy3", "name": "Hy3" })],
            "hy4-preview",
        );
        assert_eq!(merged[0]["value"], "hy4-preview");
        assert_eq!(merged[1]["value"], "hy3");
        // 已在清单里：不重复添加
        let existing = merge_current_model_option(
            vec![json!({ "value": "hy4-preview", "name": "Hy4" })],
            "hy4-preview",
        );
        assert_eq!(existing.len(), 1);
        // 空当前模型：原样返回
        let passthrough = merge_current_model_option(vec![json!({ "value": "hy3" })], "");
        assert_eq!(passthrough.len(), 1);
    }

    #[test]
    fn codebuddy_merges_standard_and_extension_models_and_borrowed_instances_do_not_publish_them() {
        let merged = merge_model_config_option(
            json!([
                { "id": "mode", "options": [{ "value": "plan", "name": "Plan" }] },
                { "id": "model", "currentValue": "glm-5.3-flash", "options": [
                    { "value": "glm-5.3-flash", "name": "GLM-5.3-Flash" }
                ] }
            ]),
            json!([{ "id": "model", "currentValue": "hy4-preview", "options": [
                { "value": "hy4-preview", "name": "HY4 Preview" }
            ] }]),
        );
        assert_eq!(merged[0]["id"], "mode");
        assert_eq!(merged[1]["currentValue"], "hy4-preview");
        assert_eq!(
            merged[1]["options"],
            json!([
                { "value": "hy4-preview", "name": "HY4 Preview" },
                { "value": "glm-5.3-flash", "name": "GLM-5.3-Flash" }
            ])
        );
        assert!(publishes_model_options(&HashMap::new()));
        assert!(!publishes_model_options(&HashMap::from([(
            "NOVA_QUOTA_BORROWED".into(),
            "1".into(),
        )])));
    }

    #[test]
    fn prewarm_activation_preserves_local_codebuddy_environment_but_isolates_borrowed_sessions() {
        let inherited = [
            ("CODEBUDDY_API_KEY".into(), "local-secret".into()),
            ("OTHER_KEY".into(), "ignored".into()),
        ];
        let local = merge_codebuddy_activation_env(&HashMap::new(), inherited.clone());
        assert_eq!(
            local.get("CODEBUDDY_API_KEY").map(String::as_str),
            Some("local-secret")
        );
        assert!(!local.contains_key("OTHER_KEY"));

        let borrowed = HashMap::from([
            ("NOVA_QUOTA_BORROWED".into(), "1".into()),
            ("CODEBUDDY_CONFIG_DIR".into(), "isolated".into()),
        ]);
        let isolated = merge_codebuddy_activation_env(&borrowed, inherited);
        assert!(!isolated.contains_key("CODEBUDDY_API_KEY"));
        assert_eq!(
            isolated.get("CODEBUDDY_CONFIG_DIR").map(String::as_str),
            Some("isolated")
        );
    }

    #[test]
    fn only_client_requests_expect_a_post_stream_response() {
        assert_eq!(
            json_rpc_request_id(&json!({"jsonrpc":"2.0","id":7,"method":"initialize"})),
            Some(7)
        );
        assert_eq!(
            json_rpc_request_id(&json!({"jsonrpc":"2.0","id":7,"result":{}})),
            None
        );
        assert_eq!(
            json_rpc_request_id(&json!({"jsonrpc":"2.0","method":"session/cancel"})),
            None
        );
    }

    #[test]
    fn decodes_fragmented_streamable_http_sse() {
        let mut decoder = SseDecoder::default();
        assert!(decoder.push(b":ok\r\n\r\nevent: message\r\nda").is_empty());
        assert_eq!(
            decoder.push(b"ta: {\"jsonrpc\":\"2.0\",\r\ndata: \"id\":1}\r\n\r\n"),
            vec!["{\"jsonrpc\":\"2.0\",\n\"id\":1}".to_string()]
        );
        assert!(decoder.push(b"data: final").is_empty());
        assert_eq!(decoder.finish(), vec!["final".to_string()]);
    }

    #[test]
    fn user_threads_use_distinct_codebuddy_connections() {
        assert_eq!(thread_connection_key("alpha"), "thread:alpha");
        assert_ne!(
            thread_connection_key("alpha"),
            thread_connection_key("beta")
        );
        assert_ne!(thread_connection_key("alpha"), super::SHARED_KEY);
    }

    #[test]
    fn nova_tools_server_carries_project_and_context_service_env() {
        let server = codebuddy_nova_tools_mcp_server_value(
            "C:/node.exe",
            "C:/nova-tools.mjs",
            "D:/repo",
            "fast",
            true,
            "http://127.0.0.1:1234",
            "secret",
            false,
            std::path::Path::new("C:/nova-config"),
        );
        assert_eq!(server["name"], "nova-tools");
        assert_eq!(server["command"], "C:/node.exe");
        assert_eq!(server["args"], json!(["C:/nova-tools.mjs"]));
        // CodeBuddy 从 _meta 读取 defer_loading / tools，顶层字段会被丢弃；
        // 必须在 _meta 显式关闭延迟加载，否则工具退回默认 defer、走 ToolSearch/Defer。
        assert_eq!(server["_meta"]["defer_loading"], json!(false));
        assert_eq!(
            server["_meta"]["tools"]["polaris"]["defer_loading"],
            json!(false)
        );
        let env = server["env"].as_array().unwrap();
        for (name, value) in [
            ("NOVA_TOOLS_CWD", "D:/repo"),
            ("NOVA_MCP_DIRECT", "1"),
            ("NOVA_CONTEXT_RETRIEVAL_MODE", "fast"),
            ("NOVA_CONTEXT_SERVICE_ENDPOINT", "http://127.0.0.1:1234"),
            ("NOVA_CONTEXT_SERVICE_TOKEN", "secret"),
            ("NOVA_TOOLS_READ_ONLY", "1"),
        ] {
            assert!(env.iter().any(|item| {
                item["name"].as_str() == Some(name) && item["value"].as_str() == Some(value)
            }));
        }
    }

    #[test]
    fn browser_only_server_does_not_reenable_disabled_context_tools() {
        let server = codebuddy_nova_tools_mcp_server_value(
            "C:/node.exe",
            "C:/nova-tools.mjs",
            "D:/repo",
            "none",
            false,
            "http://127.0.0.1:1234",
            "secret",
            true,
            std::path::Path::new("C:/nova-config"),
        );
        let env = server["env"].as_array().unwrap();
        assert!(env
            .iter()
            .any(|item| { item["name"] == "NOVA_FAST_CONTEXT" && item["value"] == "0" }));
        assert!(env.iter().any(|item| item["name"] == "NOVA_BROWSER_DEBUG"));
    }

    #[test]
    fn codebuddy_guidance_pins_native_tools_to_the_session_workspace() {
        let guidance = codebuddy_runtime_guidance("D:/code/intelligence-pc-v2").unwrap();
        assert!(guidance.contains("D:/code/intelligence-pc-v2"));
        assert!(guidance.contains("required working directory"));
        assert!(guidance.contains("not in Nova's own"));
    }

    #[test]
    fn codebuddy_guidance_keeps_the_latest_user_request_in_the_same_text_block() {
        let manager = AcpManager::new_with_env;
        let source = include_str!("acp.rs");
        let body = source
            .split("fn build_user_prompt_blocks(")
            .nth(1)
            .unwrap()
            .split("/// 运行中追加提示")
            .next()
            .unwrap();
        let _ = manager;
        assert!(body.contains("<system-reminder>"));
        assert!(body.contains("block[\"text\"]"));
        assert!(!body.contains("AgentKind::CodeBuddy => prompt.insert(0"));
    }

    #[test]
    fn extracts_endpoint_line() {
        assert_eq!(
            extract_codebuddy_endpoint("Endpoint  http://127.0.0.1:8321"),
            Some("http://127.0.0.1:8321".to_string())
        );
        assert_eq!(
            extract_codebuddy_endpoint("Web UI  http://127.0.0.1:8321/?password=x"),
            None
        );
        assert_eq!(
            extract_codebuddy_endpoint("Endpoint  http://0.0.0.0:8321"),
            None
        );
        assert_eq!(extract_codebuddy_endpoint("random log line"), None);
    }
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

const NOVA_TOOLS_MCP_JS: &[u8] = include_bytes!("../resources/nova-tools-mcp.mjs");

fn materialize_nova_tools_mcp(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = nova_data_dir(app).join("runtime");
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建 runtime 目录失败：{e}"))?;
    let path = dir.join("nova-tools-mcp.mjs");
    if std::fs::read(&path).ok().as_deref() != Some(NOVA_TOOLS_MCP_JS) {
        std::fs::write(&path, NOVA_TOOLS_MCP_JS)
            .map_err(|e| format!("写入 nova-tools-mcp.mjs 失败：{e}"))?;
    }
    Ok(path)
}

#[cfg(windows)]
fn codebuddy_runtime_guidance(cwd: &str) -> Option<String> {
    Some(format!(
        "Windows shell contract for this local CodeBuddy session (hard constraint): Nova runs on Windows and the Bash tool is configured to use PowerShell (`CODEBUDDY_CODE_SHELL=powershell`). The session workspace and required working directory is `{cwd}`; run every code/search/shell operation there, not in Nova's own installation or source directory. Write commands in PowerShell syntax; use `;` to chain commands and `$env:NAME` for environment variables. Do not use Bash syntax (`export`, `&&` chains, POSIX grep/sed/awk) in Bash tool commands."
    ))
}

#[cfg(not(windows))]
fn codebuddy_runtime_guidance(cwd: &str) -> Option<String> {
    Some(format!(
        "The session workspace and required working directory is `{cwd}`; run every code/search/shell operation there, not in Nova's own installation or source directory."
    ))
}

fn codebuddy_nova_tools_mcp_server(
    app: &AppHandle,
    cwd: &str,
    context_mode: &str,
    read_only: bool,
    context_endpoint: &str,
    context_token: &str,
    browser_debug: bool,
    config_dir: &std::path::Path,
) -> Result<Value, String> {
    let script = materialize_nova_tools_mcp(app)?;
    let node = resolve_program_on_path("node")
        .ok_or_else(|| "未在 PATH 中找到 node，无法注入 nova-tools MCP".to_string())?;
    Ok(codebuddy_nova_tools_mcp_server_value(
        &node.to_string_lossy(),
        &script.to_string_lossy(),
        cwd,
        context_mode,
        read_only,
        context_endpoint,
        context_token,
        browser_debug,
        config_dir,
    ))
}

fn codebuddy_nova_tools_mcp_server_value(
    node: &str,
    script: &str,
    cwd: &str,
    context_mode: &str,
    read_only: bool,
    context_endpoint: &str,
    context_token: &str,
    browser_debug: bool,
    config_dir: &std::path::Path,
) -> Value {
    let mut env = vec![
        json!({ "name": "NOVA_TOOLS_CWD", "value": cwd }),
        json!({ "name": "NOVA_MCP_DIRECT", "value": "1" }),
        json!({ "name": "NOVA_FAST_CONTEXT", "value": if context_mode == "none" { "0" } else { "1" } }),
        json!({ "name": "NOVA_CONTEXT_RETRIEVAL_MODE", "value": context_mode }),
        json!({ "name": "NOVA_CONTEXT_SERVICE_ENDPOINT", "value": context_endpoint }),
        json!({ "name": "NOVA_CONTEXT_SERVICE_TOKEN", "value": context_token }),
    ];
    if read_only {
        env.push(json!({ "name": "NOVA_TOOLS_READ_ONLY", "value": "1" }));
    }
    if browser_debug {
        env.push(json!({ "name": "NOVA_BROWSER_DEBUG", "value": "1" }));
        env.push(json!({
            "name": "NOVA_BROWSER_MCP_PORT_FILE",
            "value": crate::browser::mcp_port_file(config_dir).to_string_lossy(),
        }));
    }
    // CodeBuddy 从 `_meta` 读取 defer_loading / tools（见 AcpUtils.convertAcpMcpServersToDynamic），
    // 顶层同名字段会被丢弃，工具就退回内置默认（MCP 工具默认延迟加载，走 ToolSearch/Defer 检索）。
    // 服务器级 + 工具级都显式置 false，确保 polaris / browser 作为顶层工具直接进模型工具列表。
    let mut meta = json!({ "defer_loading": false });
    if browser_debug {
        meta["tools"] = json!({
            "polaris": { "defer_loading": false },
            "browser": { "defer_loading": false },
        });
    } else {
        meta["tools"] = json!({ "polaris": { "defer_loading": false } });
    }
    json!({
        "name": "nova-tools",
        "command": node,
        "args": [script],
        "env": env,
        "_meta": meta,
        "defer_loading": false
    })
}

fn devin_nova_tools_config(
    node: &std::path::Path,
    script: &std::path::Path,
    cwd: &str,
    context_mode: &str,
    read_only: bool,
    context_endpoint: &str,
    context_token: &str,
) -> Value {
    let context_mode = if context_mode == "none" {
        "none"
    } else {
        "fast"
    };
    let enabled = context_mode != "none";
    let mut env = serde_json::Map::new();
    env.insert(
        "NOVA_FAST_CONTEXT".into(),
        Value::String(if enabled { "1" } else { "0" }.into()),
    );
    env.insert(
        "NOVA_CONTEXT_RETRIEVAL_MODE".into(),
        Value::String(context_mode.into()),
    );
    env.insert("NOVA_TOOLS_CWD".into(), Value::String(cwd.into()));
    env.insert(
        "NOVA_CONTEXT_SERVICE_ENDPOINT".into(),
        Value::String(context_endpoint.into()),
    );
    env.insert(
        "NOVA_CONTEXT_SERVICE_TOKEN".into(),
        Value::String(context_token.into()),
    );
    if read_only {
        env.insert("NOVA_TOOLS_READ_ONLY".into(), Value::String("1".into()));
    }
    json!({
        "mcpServers": {
            "nova-tools": {
                "command": node.to_string_lossy(),
                "args": [script.to_string_lossy()],
                "env": env,
                "transport": "stdio"
            }
        }
    })
}

/// Devin 3000.2.17 会启动 `session/new.mcpServers` 中的 stdio 进程，却不会把它加入
/// mcp_list_tools / mcp_call_tool 的实际注册表。把同一配置写入隔离启动目录的项目级配置，
/// 可兼容当前缺陷。
/// Devin 3000.3.x 起 MCP 项目配置改名并改为按 session cwd 解析：
/// `.devin/mcp_config.local.json`（启动目录的旧文件只会被启动、不会注册，
/// mcp_call_tool 报 `Server 'nova-tools' not found. Available servers: []`）。
/// 因此同时写两处：启动目录的 config.local.json 兼容 3000.2.x，
/// 会话目录的 mcp_config.local.json 适配 3000.3.x（合并写入，保留用户已有服务器）。
fn prepare_devin_nova_tools_config(
    app: &AppHandle,
    conn_key: &str,
    cwd: &str,
    context_mode: &str,
    read_only: bool,
) -> Result<PathBuf, String> {
    let script = materialize_nova_tools_mcp(app)?;
    let node = resolve_program_on_path("node")
        .ok_or_else(|| "未找到 Node.js，无法挂载 nova-tools MCP".to_string())?;
    let safe_key: String = conn_key
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let launch_dir = nova_data_dir(app)
        .join("runtime")
        .join("devin-mcp")
        .join(safe_key);
    let config_dir = launch_dir.join(".devin");
    std::fs::create_dir_all(&config_dir)
        .map_err(|e| format!("创建 Devin MCP 配置目录失败：{e}"))?;
    let state = app.state::<AppState>();
    let config = devin_nova_tools_config(
        &node,
        &script,
        cwd,
        context_mode,
        read_only,
        state.context_service.endpoint(),
        state.context_service.token(),
    );
    let bytes = serde_json::to_vec_pretty(&config)
        .map_err(|e| format!("序列化 Devin MCP 配置失败：{e}"))?;
    let path = config_dir.join("config.local.json");
    if std::fs::read(&path).ok().as_deref() != Some(bytes.as_slice()) {
        std::fs::write(&path, bytes).map_err(|e| format!("写入 Devin MCP 配置失败：{e}"))?;
    }
    // 3000.3.x 只认 session cwd 下的 mcp_config.local.json；写失败不阻断会话，仅记日志。
    if let Err(e) = merge_devin_session_mcp_config(cwd, &config["mcpServers"]["nova-tools"]) {
        let _ = app.emit(EV_LOG, format!("[nova] 写入 Devin 会话 MCP 配置失败：{e}"));
    }
    Ok(launch_dir)
}

/// 把 nova-tools 服务器合并进 session cwd 的 `.devin/mcp_config.local.json`（Devin 3000.3.x）。
/// 已存在的其它服务器配置保留；内容未变时不重写。
fn merge_devin_session_mcp_config(cwd: &str, server: &Value) -> Result<PathBuf, String> {
    let config_dir = PathBuf::from(cwd).join(".devin");
    std::fs::create_dir_all(&config_dir)
        .map_err(|e| format!("创建 Devin 会话 MCP 配置目录失败：{e}"))?;
    let path = config_dir.join("mcp_config.local.json");
    let mut doc = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .filter(|value| value.is_object())
        .unwrap_or_else(|| json!({}));
    if doc["mcpServers"]["nova-tools"] == *server {
        return Ok(path);
    }
    doc["mcpServers"]["nova-tools"] = server.clone();
    let bytes = serde_json::to_vec_pretty(&doc)
        .map_err(|e| format!("序列化 Devin 会话 MCP 配置失败：{e}"))?;
    std::fs::write(&path, bytes).map_err(|e| format!("写入 Devin 会话 MCP 配置失败：{e}"))?;
    Ok(path)
}

#[cfg(test)]
mod nova_tools_config_tests {
    use super::{devin_nova_tools_config, merge_devin_session_mcp_config};
    use serde_json::{json, Value};
    use std::path::Path;

    fn merge_test_dir(tag: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("nova-devin-mcp-merge-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn session_mcp_config_merges_without_clobbering_existing_servers() {
        let dir = merge_test_dir("merge");
        let config_dir = dir.join(".devin");
        std::fs::create_dir_all(&config_dir).unwrap();
        let path = config_dir.join("mcp_config.local.json");
        std::fs::write(
            &path,
            r#"{"mcpServers":{"mine":{"command":"foo"}},"other":1}"#,
        )
        .unwrap();
        let server = json!({"command":"node","args":["nova-tools-mcp.mjs"],"transport":"stdio"});

        merge_devin_session_mcp_config(dir.to_str().unwrap(), &server).unwrap();
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(doc["mcpServers"]["mine"]["command"], "foo");
        assert_eq!(doc["other"], 1);
        assert_eq!(doc["mcpServers"]["nova-tools"], server);

        // 内容未变时不重写（mtime 不变）
        let before = std::fs::metadata(&path).unwrap().modified().unwrap();
        merge_devin_session_mcp_config(dir.to_str().unwrap(), &server).unwrap();
        let after = std::fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(before, after);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn session_mcp_config_replaces_unparseable_file() {
        let dir = merge_test_dir("broken");
        let config_dir = dir.join(".devin");
        std::fs::create_dir_all(&config_dir).unwrap();
        let path = config_dir.join("mcp_config.local.json");
        std::fs::write(&path, "not json").unwrap();
        let server = json!({"command":"node"});
        merge_devin_session_mcp_config(dir.to_str().unwrap(), &server).unwrap();
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(doc["mcpServers"]["nova-tools"], server);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn devin_local_config_registers_nova_tools_with_project_policy() {
        let config = devin_nova_tools_config(
            Path::new("C:/node.exe"),
            Path::new("C:/nova-tools.mjs"),
            "D:/repo",
            "super",
            true,
            "test-endpoint",
            "test-token",
        );
        let server = &config["mcpServers"]["nova-tools"];
        assert_eq!(server["transport"], "stdio");
        assert_eq!(server["env"]["NOVA_TOOLS_CWD"], "D:/repo");
        assert_eq!(server["env"]["NOVA_FAST_CONTEXT"], "1");
        assert_eq!(server["env"]["NOVA_CONTEXT_RETRIEVAL_MODE"], "fast");
        assert!(server["env"].get("NOVA_CONTEXT_NO_INDEX").is_none());
        assert_eq!(server["env"]["NOVA_TOOLS_READ_ONLY"], "1");
        assert_eq!(
            server["env"]["NOVA_CONTEXT_SERVICE_ENDPOINT"],
            "test-endpoint"
        );
        assert_eq!(server["env"]["NOVA_CONTEXT_SERVICE_TOKEN"], "test-token");
    }

    #[test]
    fn devin_nova_tools_guidance_requires_wrapper_server_name() {
        let guidance = super::nova_tools_prompt_guidance(true, false);
        assert!(guidance.contains("ROUTING RULE — before choosing any tool"));
        assert!(guidance.contains("must NEVER be selected as direct tool calls"));
        assert!(guidance.contains("polaris"));
        assert!(!guidance.contains("find_symbols"));
        assert!(!guidance.contains("you must use edit_files"));
        assert!(guidance.contains("do not expect a Nova edit_files tool"));
        assert!(!guidance.contains("read_files"));
        assert!(guidance.contains("only valid execution path"));
        assert!(guidance.contains("generic mcp_call_tool wrapper"));
        assert!(guidance.contains("Set server_name to the top-level string \"nova-tools\""));
        assert!(guidance.contains("\"server_name\":\"nova-tools\""));
        assert!(guidance.contains("wording such as `use/call polaris`"));
        assert!(guidance.contains("do not call mcp_list_tools merely to discover them"));
        assert!(guidance.contains("Never repeat a malformed call unchanged"));
        assert!(guidance.contains("Prefer minimal reads via Devin native read"));
        assert!(!guidance.contains("never invent arbitrary 100/200-line pages"));
        assert!(!guidance.contains("merge them into one read_files"));
    }

    #[test]
    fn disabled_polaris_is_absent_from_devin_guidance() {
        let guidance = super::nova_tools_prompt_guidance(false, false);
        assert!(!guidance.contains("polaris"));
        assert!(!guidance.contains("find_symbols"));
        assert!(!guidance.contains("read_files"));
        assert!(!guidance.contains("you must use edit_files"));
        assert!(guidance.contains("Devin built-in tools"));
    }

    #[cfg(windows)]
    #[test]
    fn windows_devin_guidance_requires_powershell_5_1_syntax() {
        let guidance = super::devin_runtime_guidance(true).unwrap();
        assert!(guidance.contains("Windows PowerShell 5.1"));
        assert!(guidance.contains("Use PowerShell syntax from the first command onward"));
        assert!(guidance.contains("`$env:NAME`"));
        assert!(guidance.contains("Do not emit Bash/POSIX syntax"));
        assert!(guidance.contains("do not use `&&` or `||`"));
        assert!(!super::devin_runtime_guidance(false)
            .unwrap()
            .contains("polaris"));
    }
}

/// First-prompt guidance for Nova MCP tools attached to Devin ACP sessions.
/// Aligned with scripts/nova-batch-tools.mjs `novaDevinBatchToolPolicy`:
/// read_files / edit_files are intentionally omitted (edit_files 全链路默认禁用，
/// NOVA_EDIT_FILES=1 才在 MCP 工具层恢复，此处提示词同步恢复前仍按原生 edit 引导)。
fn nova_tools_prompt_guidance(polaris: bool, read_only: bool) -> String {
    let mut tool_names: Vec<&str> = Vec::new();
    if polaris {
        tool_names.extend(["polaris"]);
    }
    if tool_names.is_empty() {
        let mut lines = vec![
            "Nova MCP server nova-tools exposes no tools in this mode; use Devin built-in tools."
                .to_string(),
        ];
        if read_only {
            lines.push("Current mode is plan/read-only: analyze only; do not modify files.".into());
        }
        return lines.join("\n");
    }
    let tools = tool_names.join(", ");
    let example =
        r#"{"server_name":"nova-tools","tool_name":"polaris","arguments":{"query":"cursor"}}"#;
    let call_example_name = "polaris";
    let nova_tools_phrase = format!(
        "You have Nova MCP endpoints from server nova-tools ({tools}) plus Devin built-in tools. In this Devin version, {tools} are remote MCP tool names, NOT top-level callable Devin tools."
    );
    let mut lines = vec![
        format!(
            "ROUTING RULE — before choosing any tool: Nova endpoints must NEVER be selected as direct tool calls. Select Devin's top-level mcp_call_tool first, then pass server_name=\"nova-tools\" and the endpoint name in tool_name. {nova_tools_phrase} Never select or invoke any of those names directly, even after mcp_list_tools lists them; a direct invocation produces `Unknown tool ... This tool is not available.` Your only valid execution path for a Nova tool is Devin's generic mcp_call_tool wrapper. Set server_name to the top-level string \"nova-tools\" (never omit it or put it inside arguments), and put only the selected Nova tool's inputs in arguments. Example: {example}. Follow the wrapper's declared tool-name field if its schema uses a different spelling. The available Nova tools are already stated above; do not call mcp_list_tools merely to discover them. In every rule below, wording such as `use/call {call_example_name}` means `call mcp_call_tool with server_name nova-tools and tool_name {call_example_name}`; it never authorizes a direct tool call. If a direct call reports `Unknown tool`, retry once through mcp_call_tool. If parsing reports missing field `server_name`, correct the wrapper call once. Never repeat a malformed call unchanged. The following tool-selection rules are hard constraints."
        ),
        format!(
            "Prefer minimal reads via Devin native read: when line ranges are known, read only those segments; expand nearby context only as needed. {}Do not dump large files blindly.{}",
            if polaris {
                "When location is unknown — or when you plan to modify two or more files not yet read in this session — you must call polaris first; one call typically replaces 5–10 grep+read round-trips. Then read only coverage gaps / next_reads with native read. "
            } else {
                "When location is unknown, search first (see below), then read near hits. "
            },
            if read_only {
                " For edits, use Devin native edit tools; do not expect a Nova edit_files tool in this mode."
            } else {
                " For edits, use Devin native edit tools; do not expect a Nova edit_files tool. Multiple edits for the same file must be merged into one native edit call."
            }
        ),
        if polaris {
            "Search and traversal must be cost-bounded. When symbol/keyword distribution or surrounding code is unknown, you MUST call only polaris (packs definition bodies + 1-hop neighbors + coverage; internal rg, honors `.gitignore`). Do not re-read FULL/BODY.covered ranges; fill gaps via next_reads with Devin native read. After polaris, do not re-discover the same keywords with shell `rg`/`git grep` or Devin grep—rg is already inside polaris. External rg/grep/git grep are allowed only when: (1) next_reads/gaps are still insufficient, or (2) the task explicitly needs a scoped literal search that polaris did not cover. Do not use `grep -r` or `grep -R` for unscoped recursive searches of a repo/source root. Fallback searches must honor `.gitignore` by default. Unless the task requires it, do not scan build artifacts, dependencies, caches, generated files, or large binary asset dirs. `| head` / `| tail` and output truncation only limit display, not work; recursive commands must narrow via path/glob/type/excludes and use a short timeout. After a recursive timeout, do not retry the same command unchanged—narrow scope or switch tools.".into()
        } else {
            "Search and traversal must be cost-bounded. Do not use `grep -r` or `grep -R` for unscoped recursive searches of a repo/source root. Prefer `rg` (honors `.gitignore`); use `git grep` only as a fallback for tracked-only searches. Unless the task requires it, do not scan build artifacts, dependencies, caches, generated files, or large binary asset dirs. `| head` / `| tail` and output truncation only limit display, not work; recursive commands must narrow via path/glob/type/excludes and use a short timeout. After a recursive timeout, do not retry the same command unchanged—narrow scope or switch tools.".into()
        },
    ];
    if read_only {
        lines.push("Current mode is plan/read-only: analyze only; do not modify files.".into());
    }
    lines.join("\n")
}

#[cfg(windows)]
fn devin_runtime_guidance(polaris: bool) -> Option<String> {
    let search_guidance = if polaris {
        " Use Nova polaris instead of shell recursion when it covers the search task."
    } else {
        ""
    };
    Some(format!(
        "Windows shell contract for this local Devin session (hard constraint): the native command tool runs Windows PowerShell 5.1. Use PowerShell syntax from the first command onward. Use cmdlets such as `Get-ChildItem`, `Select-String`, and `Get-Content`; use `$env:NAME` for environment variables and `;` to sequence commands. Do not emit Bash/POSIX syntax such as `export`, `VAR=value command`, `/dev/null`, `ls -la`, `rm -rf`, `grep`, `sed`, or `awk`; Windows PowerShell 5.1 does not support Bash-style chaining, so do not use `&&` or `||`.{search_guidance} Use Bash only when the user explicitly requests it or a required script has no PowerShell-compatible entry point."
    ))
}

#[cfg(not(windows))]
fn devin_runtime_guidance(_polaris: bool) -> Option<String> {
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

fn set_tool_duration(call: &mut ToolCall, duration_ms: u64) {
    let output = call.raw_output.get_or_insert_with(|| json!({}));
    if let Some(object) = output.as_object_mut() {
        object.insert("durationMs".into(), json!(duration_ms));
    }
}

fn complete_pending_tools(thread: &mut Thread, except_tool_call_id: Option<&str>) -> Vec<Item> {
    let mut changed = Vec::new();
    let finished_at = now_ms();
    for item in &mut thread.items {
        let Item::Tool { ts, call, .. } = item else {
            continue;
        };
        if except_tool_call_id == Some(call.tool_call_id.as_str()) {
            continue;
        }
        if call.status == "pending" || call.status == "in_progress" {
            call.status = "completed".to_string();
            set_tool_duration(call, finished_at.saturating_sub(*ts) as u64);
            changed.push(item.clone());
        }
    }
    changed
}
