//! Bounded, owner-scoped command sessions. This module is mounted by tools.rs.
//! Owners come from the host, never tool arguments or an archive path. A turn is
//! not an owner lifetime: call cancel_owner on explicit cancel/interrupt/close,
//! NOT when dropping a per-turn Session. Runtime task drops also kill children.
//! Limits: 32 commands globally / 4 per owner (including unconsumed completion
//! records), 256 KiB output + at most 5 x 16 KiB queued/in-flight input each.
//! Output is lossy UTF-8 for invalid/binary bytes; overflow discards old bytes.
//! No unbounded output archive, reader thread, per-completion timer or owner map.

use crate::lyra::prompt::ShellConfig;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;

const DEFAULT_TIMEOUT_SECS: u64 = 3600;
const MAX_TIMEOUT_SECS: u64 = 86400;
const MAX_SESSIONS: usize = 32;
const MAX_PER_OWNER: usize = 4;
const CACHE_BYTES: usize = 256 * 1024;
// Even JSON-escaped binary output fits the existing 32 KiB tool-result budget.
const PAGE_BYTES: usize = 4096;
const INPUT_BYTES: usize = 16 * 1024;
const RETENTION: Duration = Duration::from_secs(60);
const TICK: Duration = Duration::from_millis(20);

type Registry = HashMap<String, Arc<Session>>;
fn sessions() -> &'static Mutex<Registry> {
    static SESSIONS: OnceLock<Mutex<Registry>> = OnceLock::new();
    SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn tool_definitions() -> Vec<super::Tool> {
    vec![
        super::Tool {
            name: "exec_command",
            description: "启动可交互命令（不替代一次性的 bash）。yield_time_ms 默认1000、最多10000，只限制本次等待；timeout 默认3600、最多86400秒，是命令总时限（独立于bash的120/600秒）。返回 session_id 后用 write_stdin 轮询/输入。tty 默认false：stdin保持打开，stdout/stderr分开；tty=true 使用终端行规程、可能回显且输出合流（当前仅Unix支持；Windows用pipe）。每owner最多4个，全局32个；未消费输出最多256KiB/会话，超出丢弃旧输出并报告 dropped_bytes；结束结果最多保留60秒。返回输出为分段消费，has_more时继续轮询。".into(),
            parameters: json!({"type":"object","additionalProperties":false,"properties":{
                "command":{"type":"string","minLength":1,"maxLength":65536},
                "tty":{"type":"boolean","default":false},
                "yield_time_ms":{"type":"integer","minimum":0,"maximum":10000},
                "timeout":{"type":"integer","minimum":1,"maximum":MAX_TIMEOUT_SECS,"default":DEFAULT_TIMEOUT_SECS}
            },"required":["command"]}),
        },
        super::Tool {
            name: "write_stdin",
            description: "当前agent命令会话的输入/轮询/取消。仅session_id即轮询；chars按UTF-8原样排队，不自动换行，每次最多16KiB，input_queued_bytes仅表示入队不是已被程序读取。pipe用eof=true关闭stdin；TTY不接受eof，换行/Ctrl-C等遵循终端行规程，Ctrl-D不是关闭管道。cancel=true强制终止命令树，不可同时输入。yield_time_ms默认1000、最多10000。stdout/stderr是本次消费的分段，has_more表示还有缓存；dropped_bytes是累计丢弃原始字节数。session_id为null表示结束且已无缓存，不要再输入。".into(),
            parameters: json!({"type":"object","additionalProperties":false,"properties":{
                "session_id":{"type":"string"},"chars":{"type":"string","maxLength":16384},
                "eof":{"type":"boolean"},"cancel":{"type":"boolean"},
                "yield_time_ms":{"type":"integer","minimum":0,"maximum":10000}
            },"required":["session_id"]}),
        },
    ]
}

struct Output {
    stdout: VecDeque<u8>,
    stderr: VecDeque<u8>,
    dropped: u64,
    status: &'static str,
    exit_code: Option<i64>,
    error: Option<String>,
    finished: Option<Instant>,
}

impl Output {
    fn new() -> Self {
        Self { stdout: VecDeque::new(), stderr: VecDeque::new(), dropped: 0,
            status: "running", exit_code: None, error: None, finished: None }
    }

    fn append(&mut self, stderr: bool, bytes: &[u8]) {
        // Each stream gets half the cache; neither a noisy stdout nor stderr can
        // starve the other. Allocations and total retained output stay bounded.
        let stream = if stderr { &mut self.stderr } else { &mut self.stdout };
        let overflow = (stream.len() + bytes.len()).saturating_sub(CACHE_BYTES / 2);
        let remove = overflow.min(stream.len());
        stream.drain(..remove);
        stream.extend(&bytes[overflow - remove..]);
        self.dropped = self.dropped.saturating_add(overflow as u64);
    }
}

struct Input {
    chars: Vec<u8>,
    eof: bool,
}
struct InputQueue {
    sender: mpsc::Sender<Input>,
    closed: bool,
}
struct Session {
    owner: String,
    tty: bool,
    cancel: AtomicBool,
    output: Mutex<Output>,
    input: Mutex<InputQueue>,
}

impl Drop for Session {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
}

/// Synchronous, nonblocking cancellation request for ALL commands of this owner,
/// including those started with an older turn's cancel flag. No registry lock is
/// held during process/PTY IO. The supervisor kills/reaps within its next tick.
/// Main-agent cancellation must also call this for its child-agent owner IDs.
pub fn cancel_owner(owner: &str) -> usize {
    let registry = sessions().lock().unwrap();
    let mut count = 0;
    for session in registry.values().filter(|session| session.owner == owner) {
        session.cancel.store(true, Ordering::SeqCst);
        count += 1;
    }
    count
}

// A dropped tool call must not orphan a just-started process. Normal yield
// disarms this guard; only the process supervisor owns the long-lived resource.
struct SupervisorGuard(Arc<Session>);
impl Drop for SupervisorGuard {
    fn drop(&mut self) {
        let mut output = self.0.output.lock().unwrap();
        if output.finished.is_none() {
            output.status = "cancelled";
            output.error = Some("命令监控任务已被释放".into());
            output.finished = Some(Instant::now());
        }
        drop(output);
        self.0.input.lock().unwrap().closed = true;
    }
}

struct CallGuard(Option<Arc<Session>>);
impl Drop for CallGuard {
    fn drop(&mut self) {
        if let Some(session) = &self.0 {
            session.cancel.store(true, Ordering::SeqCst);
        }
    }
}

fn flag(args: &Value, key: &str) -> Result<bool, String> {
    args.get(key).map_or(Ok(false), |value| value.as_bool().ok_or_else(|| format!("{key} 必须是 boolean")))
}
fn number(args: &Value, key: &str, default: u64, min: u64, max: u64) -> Result<u64, String> {
    let n = args.get(key).map_or(Ok(default), |value| value.as_u64().ok_or_else(|| format!("{key} 必须是非负整数")))?;
    if !(min..=max).contains(&n) { return Err(format!("{key} 必须在 {min}..={max} 之间")); }
    Ok(n)
}
fn text<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key).and_then(Value::as_str).ok_or_else(|| format!("缺少字符串 {key}"))
}

/// shell=None is the tools-side read-only guard, also enforced for polling,
/// stdin/EOF and cancellation. Host lifecycle cancel_owner is not a model tool.
pub async fn execute(
    root: &Path,
    name: &str,
    args: &Value,
    shell: Option<&ShellConfig>,
    owner: &str,
    cancelled: Option<Arc<AtomicBool>>,
) -> Result<Value, String> {
    let shell = shell.ok_or("当前为只读模式，持久命令工具不可用")?;
    if owner.trim().is_empty() || owner.len() > 256 { return Err("持久命令需要独立、非空且不超过256字节的agent owner".into()); }
    let object = args.as_object().ok_or("参数必须是对象")?;
    let allowed: &[&str] = match name {
        "exec_command" => &["command", "tty", "yield_time_ms", "timeout"],
        "write_stdin" => &["session_id", "chars", "eof", "cancel", "yield_time_ms"],
        _ => return Err(format!("未知持久命令工具：{name}")),
    };
    if let Some(key) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(format!("未知参数：{key}"));
    }
    let yield_ms = number(args, "yield_time_ms", 1000, 0, 10_000)?;
    // Lazy expiry retains at most MAX_SESSIONS caches, never one sleeping task
    // per completed command. Expired IDs cannot be accessed or consume quota.
    sessions().lock().unwrap().retain(|_, session| {
        !session.output.lock().unwrap().finished.is_some_and(|at| at.elapsed() >= RETENTION)
    });
    if cancelled.as_ref().is_some_and(|flag| flag.load(Ordering::SeqCst)) {
        cancel_owner(owner);
        return Err("命令已被取消".into());
    }
    let (id, session, queued) = match name {
        "exec_command" => {
            let command = text(args, "command")?;
            if command.trim().is_empty() || command.len() > 65536 || command.contains('\0') {
                return Err("command 必须为非空、无NUL且不超过64KiB的字符串".into());
            }
            let tty = flag(args, "tty")?;
            // ConPTY exposes blocking anonymous pipes through portable-pty.
            // Do not leak uninterruptible reader/writer threads on Windows.
            // ponytail: Windows currently uses cancellable Tokio pipes; enable
            // TTY there only with cancellable overlapped ConPTY IO.
            if tty && !cfg!(unix) { return Err("当前平台暂不支持可取消TTY，请使用 tty=false（pipe）".into()); }
            let timeout = number(args, "timeout", DEFAULT_TIMEOUT_SECS, 1, MAX_TIMEOUT_SECS)?;
            let (sender, receiver) = mpsc::channel(4);
            let session = Arc::new(Session {
                owner: owner.into(), tty, cancel: AtomicBool::new(false),
                output: Mutex::new(Output::new()), input: Mutex::new(InputQueue { sender, closed: false }),
            });
            let id = uuid::Uuid::new_v4().to_string();
            {
                let mut registry = sessions().lock().unwrap();
                if registry.len() >= MAX_SESSIONS || registry.values().filter(|s| s.owner == owner).count() >= MAX_PER_OWNER {
                    return Err("持久命令会话达到上限（每owner 4，全局32）；请取消或消费已结束会话".into());
                }
                registry.insert(id.clone(), session.clone());
            }
            let task_session = session.clone();
            let root = root.to_path_buf();
            let program = shell.program.clone();
            let command = command.to_owned();
            let turn_cancel = cancelled.clone();
            let guard = SupervisorGuard(session.clone());
            tokio::spawn(async move {
                let _guard = guard;
                supervise(&root, &program, &command, timeout, task_session, receiver, turn_cancel).await;
            });
            (id, session, 0)
        }
        "write_stdin" => {
            let id = text(args, "session_id")?;
            let session = sessions().lock().unwrap().get(id)
                .filter(|session| session.owner == owner).cloned()
                .ok_or("命令会话不存在、已结束/过期或不属于当前agent")?;
            let chars = args.get("chars").map_or(Ok(""), |v| v.as_str().ok_or("chars 必须是字符串"))?;
            if chars.len() > INPUT_BYTES { return Err("单次输入不能超过16KiB".into()); }
            let eof = flag(args, "eof")?;
            let cancel = flag(args, "cancel")?;
            if cancel && (!chars.is_empty() || eof) { return Err("cancel 不能与输入/eof同时使用".into()); }
            if eof && session.tty { return Err("TTY不支持eof关闭；请发送终端控制字符或cancel，Ctrl-D不等于关闭管道".into()); }
            if cancel { session.cancel.store(true, Ordering::SeqCst); }
            if !chars.is_empty() || eof {
                let mut input = session.input.lock().unwrap();
                if input.closed || session.cancel.load(Ordering::SeqCst) { return Err("stdin已关闭或命令已取消".into()); }
                input.sender.try_send(Input { chars: chars.as_bytes().to_vec(), eof })
                    .map_err(|e| format!("输入未入队（请勿认为已输入）：{e}"))?;
                if eof { input.closed = true; }
            }
            (id.to_owned(), session, chars.len())
        }
        _ => return Err(format!("未知持久命令工具：{name}")),
    };
    let mut guard = CallGuard(Some(session.clone()));
    let deadline = Instant::now() + Duration::from_millis(yield_ms);
    loop {
        if cancelled.as_ref().is_some_and(|flag| flag.load(Ordering::SeqCst)) { cancel_owner(owner); }
        let ready = {
            let output = session.output.lock().unwrap();
            output.finished.is_some() || !output.stdout.is_empty() || !output.stderr.is_empty()
        };
        if ready || Instant::now() >= deadline { break; }
        tokio::time::sleep(TICK.min(deadline.saturating_duration_since(Instant::now()))).await;
    }
    let value = snapshot(&id, &session, queued);
    guard.0 = None;
    Ok(value)
}

fn take_page(bytes: &mut VecDeque<u8>, limit: usize, finished: bool) -> (String, usize) {
    let mut n = bytes.len().min(limit);
    // Avoid splitting valid UTF-8 at a page edge; invalid/binary bytes are lossy.
    for _ in 0..3 {
        if n > 0 && bytes.get(n).is_some_and(|b| b & 0xc0 == 0x80) { n -= 1; } else { break; }
    }
    let mut page: Vec<u8> = bytes.iter().take(n).copied().collect();
    if !finished && n == bytes.len() {
        if let Err(error) = std::str::from_utf8(&page) {
            if error.error_len().is_none() { n = error.valid_up_to(); page.truncate(n); }
        }
    }
    bytes.drain(..n);
    (String::from_utf8_lossy(&page).into_owned(), n)
}

fn snapshot(id: &str, session: &Arc<Session>, queued: usize) -> Value {
    let mut out = session.output.lock().unwrap();
    let finished = out.finished.is_some();
    let (stdout, used) = take_page(&mut out.stdout, PAGE_BYTES / 2, finished);
    let (stderr, _) = take_page(&mut out.stderr, PAGE_BYTES - used, finished);
    let has_more = !out.stdout.is_empty() || !out.stderr.is_empty();
    let remove = finished && !has_more;
    let value = json!({"session_id": if remove { None } else { Some(id) }, "command_id": id,
        "status":out.status,"exit_code":out.exit_code,"tty":session.tty,
        "stdout":stdout,"stderr":stderr,"has_more":has_more,
        "dropped_bytes":out.dropped,"error":out.error,"input_queued_bytes":queued,
        "cancel_requested":session.cancel.load(Ordering::SeqCst)});
    drop(out);
    if remove {
        let mut registry = sessions().lock().unwrap();
        if registry.get(id).is_some_and(|s| Arc::ptr_eq(s, session)) { registry.remove(id); }
    }
    value
}

async fn capture<R: AsyncRead + Unpin>(mut reader: R, session: Arc<Session>, stderr: bool) {
    let mut buffer = [0; 8192];
    loop {
        match reader.read(&mut buffer).await {
            Ok(0) => break,
            Ok(n) => session.output.lock().unwrap().append(stderr, &buffer[..n]),
            Err(error) => {
                // Linux PTYs report EIO when the slave side closes.
                #[cfg(unix)]
                if session.tty && error.raw_os_error() == Some(libc::EIO) { break; }
                session.output.lock().unwrap().error = Some(format!("读取输出失败：{error}"));
                break;
            }
        }
    }
}

async fn feed<W: AsyncWrite + Unpin>(mut writer: W, mut input: mpsc::Receiver<Input>, session: Arc<Session>) {
    while let Some(chunk) = input.recv().await {
        if let Err(error) = writer.write_all(&chunk.chars).await {
            session.output.lock().unwrap().error = Some(format!("stdin可能部分写入，请勿盲目重发：{error}"));
            break;
        }
        if chunk.eof { let _ = writer.shutdown().await; break; }
    }
    session.input.lock().unwrap().closed = true;
}

// The supervisor, not the registry or per-turn Session, owns OS resources.
// All reads/writes are async/cancellable; no PTY read ever holds a global lock.
enum Process {
    Pipe(tokio::process::Child),
    #[cfg(unix)]
    Pty(Box<dyn portable_pty::Child + Send + Sync>),
}
impl Process {
    fn id(&self) -> Option<u32> {
        match self { Self::Pipe(child) => child.id(), #[cfg(unix)] Self::Pty(child) => child.process_id() }
    }
    fn try_wait(&mut self) -> std::io::Result<Option<i64>> {
        match self {
            Self::Pipe(child) => child.try_wait().map(|s| s.map(|s| s.code().map(i64::from).unwrap_or(-1))),
            #[cfg(unix)]
            Self::Pty(child) => child.try_wait().map(|s| s.map(|s| if s.signal().is_some() { -1 } else { i64::from(s.exit_code()) })),
        }
    }
    fn kill(&mut self) {
        match self {
            Self::Pipe(child) => { let _ = child.start_kill(); }
            #[cfg(unix)]
            Self::Pty(child) => { let _ = child.kill(); }
        }
    }
}
struct ProcessGuard {
    process: Option<Process>, pid: Option<u32>, reaped: bool,
    #[cfg(windows)]
    job: Option<CommandJob>,
}
impl ProcessGuard {
    fn kill(&mut self) {
        #[cfg(unix)]
        if let Some(pid) = self.pid.take() {
            // portable-pty starts a new session; pipes use process_group(0).
            // ponytail: process-group containment is not a sandbox; deliberately
            // setsid-detached processes require a cgroup/job-based executor.
            unsafe { libc::kill(-(pid as i32), libc::SIGKILL); }
        }
        #[cfg(windows)]
        {
            if self.job.is_none() && !self.reaped {
                if let Some(pid) = self.pid.take() { crate::acp::kill_process_tree(pid); }
            }
            self.pid = None;
            self.job.take(); // KILL_ON_JOB_CLOSE also terminates descendants after shell exit.
        }
        if !self.reaped { self.process.as_mut().unwrap().kill(); }
    }
    fn poll(&mut self) -> std::io::Result<Option<i64>> {
        let status = self.process.as_mut().unwrap().try_wait()?;
        if status.is_some() { self.reaped = true; }
        Ok(status)
    }
}
impl Drop for ProcessGuard {
    fn drop(&mut self) {
        self.kill();
        #[cfg(unix)]
        if !self.reaped {
            // try_wait after SIGKILL normally succeeds on the next tick. For
            // an aborted supervisor, retain ownership in a reaper thread.
            if let Some(Process::Pty(mut child)) = self.process.take() {
                std::thread::spawn(move || { let _ = child.wait(); });
            }
        }
    }
}

type Reader = Box<dyn AsyncRead + Unpin + Send>;
type Writer = Box<dyn AsyncWrite + Unpin + Send>;
fn spawn(root: &Path, program: &str, command: &str, tty: bool) -> Result<(ProcessGuard, Reader, Option<Reader>, Writer), String> {
    if tty {
        #[cfg(unix)]
        {
            use portable_pty::{native_pty_system, CommandBuilder, PtySize};
            let pair = native_pty_system().openpty(PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 }).map_err(|e| e.to_string())?;
            // Duplicate the master descriptor into cancellable Tokio AsyncFd IO;
            // portable-pty's default Read/Write wrappers are blocking.
            let fd = pair.master.as_raw_fd().ok_or("PTY缺少原生fd")?;
            let reader = PtyIo::new(fd).map_err(|e| e.to_string())?;
            let writer = PtyIo::new(fd).map_err(|e| e.to_string())?;
            let mut builder = CommandBuilder::new(program);
            builder.args(["-c", command]);
            builder.cwd(root);
            let child = pair.slave.spawn_command(builder).map_err(|e| format!("启动PTY失败：{e}"))?;
            let process = Process::Pty(child);
            let pid = process.id();
            return Ok((ProcessGuard { process: Some(process), pid, reaped: false }, Box::new(reader), None, Box::new(writer)));
        }
        #[cfg(not(unix))]
        return Err("当前平台不支持TTY".into());
    }
    let mut command_builder = tokio::process::Command::new(program);
    command_builder.args(["-c", command]).current_dir(root)
        .stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped()).kill_on_drop(true);
    #[cfg(unix)]
    command_builder.process_group(0);
    #[cfg(windows)]
    command_builder.creation_flags(0x08000000);
    let child = command_builder.spawn().map_err(|e| format!("启动命令失败：{e}"))?;
    let pid = child.id();
    let mut guard = ProcessGuard { process: Some(Process::Pipe(child)), pid, reaped: false,
        #[cfg(windows)]
        job: None,
    };
    let Some(Process::Pipe(child)) = &mut guard.process else { unreachable!() };
    #[cfg(windows)]
    { guard.job = Some(CommandJob::new(child).map_err(|e| format!("无法建立命令进程树Job：{e}"))?); }
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let stdin = child.stdin.take().unwrap();
    Ok((guard, Box::new(stdout), Some(Box::new(stderr)), Box::new(stdin)))
}

#[cfg(windows)]
struct CommandJob(isize);
#[cfg(windows)]
impl CommandJob {
    fn new(child: &tokio::process::Child) -> std::io::Result<Self> {
        use windows_sys::Win32::System::JobObjects::*;
        unsafe {
            let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if handle.is_null() { return Err(std::io::Error::last_os_error()); }
            let job = Self(handle as isize);
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if SetInformationJobObject(handle, JobObjectExtendedLimitInformation,
                &info as *const _ as _, std::mem::size_of_val(&info) as u32) == 0 {
                return Err(std::io::Error::last_os_error());
            }
            // ponytail: Tokio exposes only the process handle, so assignment is
            // just after spawn. Atomic containment needs STARTUPINFOEX job-list
            // support in the launcher; the fallback tree-kill covers live roots.
            let process = child.raw_handle().ok_or_else(|| std::io::Error::other("child handle missing"))?;
            if AssignProcessToJobObject(handle, process as _) == 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(job)
        }
    }
}
#[cfg(windows)]
impl Drop for CommandJob {
    fn drop(&mut self) { unsafe { windows_sys::Win32::Foundation::CloseHandle(self.0 as _); } }
}

async fn supervise(root: &Path, program: &str, command: &str, timeout: u64, session: Arc<Session>, input: mpsc::Receiver<Input>, turn_cancel: Option<Arc<AtomicBool>>) {
    let mut readers = tokio::task::JoinSet::new();
    let mut writers = tokio::task::JoinSet::new();
    let result = async {
        if session.cancel.load(Ordering::SeqCst) { return Ok(("cancelled", None)); }
        let (mut process, stdout, stderr, stdin) = spawn(root, program, command, session.tty)?;
        readers.spawn(capture(stdout, session.clone(), false));
        if let Some(stderr) = stderr { readers.spawn(capture(stderr, session.clone(), true)); }
        writers.spawn(feed(stdin, input, session.clone()));
        let deadline = Instant::now() + Duration::from_secs(timeout);
        let completion = loop {
            if session.cancel.load(Ordering::SeqCst) || turn_cancel.as_ref().is_some_and(|f| f.load(Ordering::SeqCst)) {
                break ("cancelled", None);
            }
            if Instant::now() >= deadline { break ("timed_out", None); }
            match process.poll().map_err(|e| e.to_string())? {
                Some(code) => break ("exited", Some(code)),
                None => tokio::time::sleep(TICK).await,
            }
        };
        writers.abort_all();
        while writers.join_next().await.is_some() {}
        // Kill descendants also on natural shell exit; do not leave background
        // jobs holding pipes/PTYs. Keep the slot occupied until cleanup completes.
        process.kill();
        while !process.reaped {
            process.poll().map_err(|e| e.to_string())?;
            if !process.reaped { tokio::time::sleep(TICK).await; }
        }
        if tokio::time::timeout(Duration::from_millis(250), async {
            while readers.join_next().await.is_some() {}
        }).await.is_err() {
            session.output.lock().unwrap().error = Some("输出排空超过250ms，已关闭读取端；末尾输出可能不完整".into());
        }
        Ok::<_, String>(completion)
    }.await;
    readers.abort_all();
    writers.abort_all();
    while readers.join_next().await.is_some() {}
    while writers.join_next().await.is_some() {}
    session.input.lock().unwrap().closed = true;
    let mut output = session.output.lock().unwrap();
    match result {
        Ok((status, code)) => { output.status = status; output.exit_code = code; }
        Err(error) => { output.status = "failed"; output.error = Some(error); }
    }
    output.finished = Some(Instant::now());
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::lyra::prompt::ShellKind;

    async fn call(owner: &str, name: &str, args: Value) -> Result<Value, String> {
        execute(&std::env::temp_dir(), name, &args,
            Some(&ShellConfig { program: "/bin/bash".into(), kind: ShellKind::Bash }), owner, None).await
    }

    async fn finish(owner: &str, mut value: Value) -> Value {
        let mut stdout = value["stdout"].as_str().unwrap().to_owned();
        let mut stderr = value["stderr"].as_str().unwrap().to_owned();
        let deadline = Instant::now() + Duration::from_secs(4);
        while let Some(id) = value["session_id"].as_str() {
            assert!(Instant::now() < deadline, "command did not finish: {value}");
            tokio::time::sleep(TICK).await;
            value = call(owner, "write_stdin", json!({"session_id":id,"yield_time_ms":50})).await.unwrap();
            stdout.push_str(value["stdout"].as_str().unwrap());
            stderr.push_str(value["stderr"].as_str().unwrap());
        }
        value["stdout"] = json!(stdout);
        value["stderr"] = json!(stderr);
        value
    }

    #[tokio::test]
    async fn pipe_and_pty_lifecycle() {
        let owner = uuid::Uuid::new_v4().to_string();
        for tty in [false, true] {
            let start = call(&owner, "exec_command", json!({"command":"echo ready; read -r value; printf 'got:%s\\n' \"$value\"", "tty":tty})).await.unwrap();
            assert!(start["stdout"].as_str().unwrap().contains("ready"));
            let id = start["session_id"].as_str().unwrap();
            for args in [json!({"session_id":id}),json!({"session_id":id,"chars":"bad\n"}),json!({"session_id":id,"cancel":true})] {
                assert!(call("another-owner", "write_stdin", args).await.is_err());
            }
            let poll = call(&owner, "write_stdin", json!({"session_id":id,"yield_time_ms":0})).await.unwrap();
            assert_eq!(poll["stdout"], ""); // earlier output is not replayed
            assert_eq!(poll["status"], "running");
            for args in [json!({"session_id":id}), json!({"session_id":id,"chars":"bad\n"}), json!({"session_id":id,"cancel":true}), json!({"session_id":id,"eof":true})] {
                assert!(execute(Path::new("/tmp"), "write_stdin", &args, None, &owner, None).await.unwrap_err().contains("只读"));
            }
            if tty {
                assert!(call(&owner, "write_stdin", json!({"session_id":id,"eof":true})).await.is_err());
            }
            let sent = call(&owner, "write_stdin", json!({"session_id":id,"chars":"hello世界\n"})).await.unwrap();
            assert_eq!(sent["input_queued_bytes"], "hello世界\n".len());
            let done = finish(&owner, sent).await;
            assert_eq!(done["status"], "exited");
            assert_eq!(done["exit_code"], 0);
            assert!(done["stdout"].as_str().unwrap().contains("got:hello世界"));
        }
        let start = call(&owner, "exec_command", json!({"command":"cat; echo eof", "yield_time_ms":0})).await.unwrap();
        let sent = call(&owner, "write_stdin", json!({"session_id":start["session_id"],"chars":"tail","eof":true})).await.unwrap();
        let done = finish(&owner, sent).await;
        assert_eq!(done["stdout"], "taileof\n");

        // Different turns have distinct cancel flags. Neither flag is set:
        // lifecycle cancellation must find all old sessions by owner alone.
        let shell = ShellConfig { program: "/bin/bash".into(), kind: ShellKind::Bash };
        let old_flag = Arc::new(AtomicBool::new(false));
        let new_flag = Arc::new(AtomicBool::new(false));
        let a = execute(Path::new("/tmp"), "exec_command", &json!({"command":"echo a; sleep 30", "tty":true}), Some(&shell), &owner, Some(old_flag.clone())).await.unwrap();
        let b = execute(Path::new("/tmp"), "exec_command", &json!({"command":"echo b; sleep 30"}), Some(&shell), &owner, Some(new_flag.clone())).await.unwrap();
        assert_eq!(cancel_owner("unrelated-owner"), 0);
        assert_eq!(cancel_owner(&owner), 2);
        assert_eq!(finish(&owner, a).await["status"], "cancelled");
        assert_eq!(finish(&owner, b).await["status"], "cancelled");
        assert!(!old_flag.load(Ordering::SeqCst) && !new_flag.load(Ordering::SeqCst));
        let start = call(&owner, "exec_command", json!({"command":"sleep 30", "yield_time_ms":0})).await.unwrap();
        let stop = call(&owner, "write_stdin", json!({"session_id":start["session_id"], "cancel":true})).await.unwrap();
        assert_eq!(finish(&owner, stop).await["status"], "cancelled");
        assert_eq!(sessions().lock().unwrap().values().filter(|s| s.owner == owner).count(), 0);
    }

    #[tokio::test]
    async fn limits_read_only_and_timeout() {
        let owner = uuid::Uuid::new_v4().to_string();
        for name in ["exec_command", "write_stdin"] {
            assert!(execute(Path::new("/tmp"), name, &json!({}), None, &owner, None).await.unwrap_err().contains("只读"));
        }
        assert!(call("", "exec_command", json!({"command":"true"})).await.is_err());
        assert!(call(&owner, "exec_command", json!({"command":"true","timeout":0})).await.is_err());
        assert!(call(&owner, "exec_command", json!({"command":"true","timeout":86401})).await.is_err());
        let definition = tool_definitions().into_iter().find(|tool| tool.name == "exec_command").unwrap();
        assert_eq!(definition.parameters["properties"]["timeout"]["default"], 3600);
        assert_eq!(definition.parameters["properties"]["timeout"]["maximum"], 86400);
        assert!(definition.description.contains("默认3600、最多86400秒"));
        assert_eq!(number(&json!({}), "timeout", DEFAULT_TIMEOUT_SECS, 1, MAX_TIMEOUT_SECS).unwrap(), 3600);
        let long = call(&owner, "exec_command", json!({"command":"true","timeout":86400})).await.unwrap();
        assert_eq!(finish(&owner, long).await["exit_code"], 0);
        assert!(call(&owner, "exec_command", json!({"command":"true","owner":"spoof"})).await.is_err());
        let mut commands = Vec::new();
        for _ in 0..MAX_PER_OWNER {
            commands.push(call(&owner, "exec_command", json!({"command":"sleep 30","yield_time_ms":0})).await.unwrap());
        }
        assert!(call(&owner, "exec_command", json!({"command":"true"})).await.is_err());
        cancel_owner(&owner);
        for command in commands { finish(&owner, command).await; }
        let fast = call(&owner, "exec_command", json!({"command":"printf ok; printf err >&2; exit 7"})).await.unwrap();
        let fast = finish(&owner, fast).await;
        assert_eq!(fast["exit_code"], 7);
        assert_eq!(fast["stdout"], "ok");
        assert_eq!(fast["stderr"], "err");
        for tty in [false, true] {
            let value = call(&owner, "exec_command", json!({"command":"sleep 30 & echo $!; wait","timeout":1,"tty":tty})).await.unwrap();
            let done = finish(&owner, value).await;
            assert_eq!(done["status"], "timed_out");
            #[cfg(target_os = "linux")]
            {
                let pid: u32 = done["stdout"].as_str().unwrap().trim().parse().unwrap();
                let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
                assert!(stat.is_empty() || stat.split(") ").nth(1).unwrap().starts_with('Z'), "descendant survived timeout: {stat}");
            }
        }
        // A child that never reads stdin cannot pin the writer or cancel path.
        let blocked = call(&owner, "exec_command", json!({"command":"sleep 30", "yield_time_ms":0})).await.unwrap();
        let mut rejected = false;
        for _ in 0..12 {
            if call(&owner, "write_stdin", json!({"session_id":blocked["session_id"],"chars":"x".repeat(INPUT_BYTES),"yield_time_ms":0})).await.is_err() {
                rejected = true;
                break;
            }
        }
        assert!(rejected, "input queue must have a hard bound");
        cancel_owner(&owner);
        assert_eq!(finish(&owner, blocked).await["status"], "cancelled");
        let noisy = call(&owner, "exec_command", json!({"command":"head -c 800000 /dev/zero", "yield_time_ms":0})).await.unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        let id = noisy["session_id"].as_str().unwrap();
        let session = sessions().lock().unwrap().get(id).unwrap().clone();
        {
            let output = session.output.lock().unwrap();
            assert!(output.stdout.len() + output.stderr.len() <= CACHE_BYTES);
            assert!(output.dropped > 0);
        }
        let page = call(&owner, "write_stdin", json!({"session_id":id})).await.unwrap();
        assert!(page.to_string().len() < 32 * 1024);
        assert_eq!(page["has_more"], true);
        finish(&owner, page).await;
        let expired = call(&owner, "exec_command", json!({"command":"true", "yield_time_ms":0})).await.unwrap();
        let id = expired["session_id"].as_str().unwrap();
        let session = sessions().lock().unwrap().get(id).unwrap().clone();
        let deadline = Instant::now() + Duration::from_secs(1);
        while session.output.lock().unwrap().finished.is_none() {
            assert!(Instant::now() < deadline);
            tokio::time::sleep(TICK).await;
        }
        session.output.lock().unwrap().finished = Some(Instant::now() - RETENTION);
        assert!(call(&owner, "write_stdin", json!({"session_id":id})).await.is_err());
        let mut utf8 = VecDeque::from("世界".as_bytes().to_vec());
        assert_eq!(take_page(&mut utf8, 4, true).0, "世");
        assert_eq!(take_page(&mut utf8, 4, true).0, "界");
        let mut pending = VecDeque::from(vec![0xe4, 0xb8]);
        assert_eq!(take_page(&mut pending, 4, false).0, "");
        pending.push_back(0x96);
        assert_eq!(take_page(&mut pending, 4, false).0, "世");
    }

    #[test]
    fn runtime_drop_reaps_pipe_and_pty_resources() {
        let owner = uuid::Uuid::new_v4().to_string();
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let mut pids = Vec::new();
        runtime.block_on(async {
            for tty in [false, true] {
                let value = call(&owner, "exec_command", json!({"command":"echo $$; read -r line", "tty":tty})).await.unwrap();
                pids.push(value["stdout"].as_str().unwrap().trim().parse::<u32>().unwrap());
            }
        });
        drop(runtime);
        let deadline = Instant::now() + Duration::from_secs(2);
        while pids.iter().any(|pid| unsafe { libc::kill(*pid as i32, 0) } == 0) && Instant::now() < deadline {
            std::thread::sleep(TICK);
        }
        for pid in pids {
            assert_eq!(unsafe { libc::kill(pid as i32, 0) }, -1, "runtime drop leaked process {pid}");
        }
        let mut registry = sessions().lock().unwrap();
        for session in registry.values().filter(|s| s.owner == owner) {
            assert!(session.output.lock().unwrap().finished.is_some());
        }
        registry.retain(|_, session| session.owner != owner);
    }

    #[tokio::test]
    async fn dropping_inflight_call_cancels_without_blocking_other_sessions() {
        let owner = uuid::Uuid::new_v4().to_string();
        let task_owner = owner.clone();
        let task = tokio::spawn(async move {
            call(&task_owner, "exec_command", json!({"command":"read -r value", "tty":true,"yield_time_ms":10000})).await
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        // A PTY waiting for input cannot hold up another session/registry access.
        let other = uuid::Uuid::new_v4().to_string();
        let quick = tokio::time::timeout(Duration::from_secs(1), call(&other, "exec_command", json!({"command":"echo ok"}))).await.unwrap().unwrap();
        finish(&other, quick).await;
        task.abort();
        let _ = task.await;
        let id = sessions().lock().unwrap().iter().find(|(_,s)| s.owner == owner).unwrap().0.clone();
        let stopped = call(&owner, "write_stdin", json!({"session_id":id})).await.unwrap();
        assert_eq!(finish(&owner, stopped).await["status"], "cancelled");
    }
}

#[cfg(unix)]
struct PtyIo(tokio::io::unix::AsyncFd<std::fs::File>);
#[cfg(unix)]
impl PtyIo {
    fn new(fd: std::os::fd::RawFd) -> std::io::Result<Self> {
        use std::os::fd::FromRawFd;
        let fd = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
        if fd < 0 { return Err(std::io::Error::last_os_error()); }
        let file = unsafe { std::fs::File::from_raw_fd(fd) };
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self(tokio::io::unix::AsyncFd::new(file)?))
    }
}
#[cfg(unix)]
impl AsyncRead for PtyIo {
    fn poll_read(self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>, buf: &mut tokio::io::ReadBuf<'_>) -> std::task::Poll<std::io::Result<()>> {
        loop {
            let mut ready = std::task::ready!(self.0.poll_read_ready(cx))?;
            match ready.try_io(|fd| { use std::io::Read; fd.get_ref().read(buf.initialize_unfilled()) }) {
                Ok(Ok(n)) => { buf.advance(n); return std::task::Poll::Ready(Ok(())); }
                Ok(Err(e)) => return std::task::Poll::Ready(Err(e)),
                Err(_) => continue,
            }
        }
    }
}
#[cfg(unix)]
impl AsyncWrite for PtyIo {
    fn poll_write(self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>, buf: &[u8]) -> std::task::Poll<std::io::Result<usize>> {
        loop {
            let mut ready = std::task::ready!(self.0.poll_write_ready(cx))?;
            match ready.try_io(|fd| { use std::io::Write; fd.get_ref().write(buf) }) {
                Ok(result) => return std::task::Poll::Ready(result),
                Err(_) => continue,
            }
        }
    }
    fn poll_flush(self: std::pin::Pin<&mut Self>, _: &mut std::task::Context<'_>) -> std::task::Poll<std::io::Result<()>> { std::task::Poll::Ready(Ok(())) }
    fn poll_shutdown(self: std::pin::Pin<&mut Self>, _: &mut std::task::Context<'_>) -> std::task::Poll<std::io::Result<()>> { std::task::Poll::Ready(Ok(())) }
}
