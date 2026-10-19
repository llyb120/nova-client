//! Main or same-app runtime entry point: execute the script, never an AI/session.
use crate::lyra::prompt::{detect_shell, ShellConfig, ShellKind};
use crate::miniapp_window::{authorize_app, parse_id, runtime_app_id};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tauri::{Manager, WebviewWindow};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};
use tokio::sync::Notify;
use uuid::Uuid;

const SCRIPT_LIMIT: usize = 32 * 1024;
const INPUT_LIMIT: usize = 160 * 1024;
const PIPE_LIMIT: usize = 64 * 1024;
const RUN_LIMIT: usize = 64;
const ACTIVE_LIMIT: usize = 32;
const SEEN_LIMIT: usize = 65_536;
const POWERSHELL_PREFIX: &str = "$ErrorActionPreference='Stop'; [Console]::InputEncoding = [System.Text.UTF8Encoding]::new($false); [Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false); $OutputEncoding = [Console]::OutputEncoding; $nova = [Console]::In.ReadToEnd() | ConvertFrom-Json;\n";

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandSnapshot {
    pub run_id: String,
    pub status: &'static str,
    pub stdout: String,
    pub output: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    pub truncated: bool,
}

#[derive(Default)]
struct Registry {
    runs: HashMap<Uuid, Arc<Run>>,
    seen: HashSet<Uuid>,
}

static RUNS: OnceLock<Mutex<Registry>> = OnceLock::new();

fn registry() -> &'static Mutex<Registry> {
    RUNS.get_or_init(|| Mutex::new(Registry::default()))
}

struct Run {
    id: Uuid,
    app_id: Uuid,
    created: Instant,
    state: Mutex<RunState>,
    cancel: Notify,
}

struct RunState {
    status: &'static str,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    exit_code: Option<i32>,
    truncated: bool,
    message: String,
}

impl Run {
    fn snapshot(&self) -> CommandSnapshot {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let mut stdout = String::new();
        let mut output = String::new();
        let mut truncated = state.truncated;
        for (bytes, is_stdout) in [(&state.stdout, true), (&state.stderr, false)] {
            if bytes.is_empty() {
                continue;
            }
            if !output.is_empty() && !output.ends_with('\n') {
                output.push('\n');
            }
            let text = String::from_utf8_lossy(bytes);
            let mut end = text.len().min(PIPE_LIMIT);
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            truncated |= end < text.len();
            if is_stdout {
                stdout.push_str(&text[..end]);
            }
            output.push_str(&text[..end]);
        }
        if !state.message.is_empty() {
            if !output.is_empty() && !output.ends_with('\n') {
                output.push('\n');
            }
            output.push_str(&state.message);
        }
        CommandSnapshot {
            run_id: self.id.to_string(),
            status: state.status,
            stdout,
            output,
            exit_code: state.exit_code,
            truncated,
        }
    }

    fn running(&self) -> bool {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).status == "running"
    }
}

struct Request {
    run_id: Uuid,
    app_id: Uuid,
    script: String,
    shell: ShellConfig,
    input: Vec<u8>,
    timeout: Duration,
}

impl Request {
    fn validate(
        run_id: &str,
        app_id: &str,
        script: String,
        shell: &str,
        inputs: BTreeMap<String, String>,
        timeout_seconds: u64,
    ) -> Result<Self, String> {
        let run_id = parse_id(run_id)?;
        let app_id = parse_id(app_id)?;
        if script.trim().is_empty() || script.len() > SCRIPT_LIMIT || script.contains('\0') {
            return Err("脚本必须非空且不超过 32KB UTF-8，不能包含 NUL".into());
        }
        if script.contains("{{") || script.contains("}}") {
            return Err("脚本不能包含模板插值；请通过 stdin JSON / $nova 读取参数".into());
        }
        if !(1..=120).contains(&timeout_seconds) {
            return Err("timeoutSeconds 必须为 1..120".into());
        }
        let kind = match shell {
            "powershell" => ShellKind::PowerShell,
            "bash" => ShellKind::Bash,
            _ => return Err("shell 必须为 powershell 或 bash".into()),
        };
        let detected = detect_shell();
        if detected.kind != kind {
            return Err("shell 与当前系统不匹配".into());
        }
        if inputs.len() > 200 {
            return Err("参数最多 200 个字段".into());
        }
        let mut bytes = 0usize;
        let mut ps_keys = HashSet::new();
        for (key, value) in &inputs {
            if key.is_empty()
                || key.len() > 64
                || !key.as_bytes()[0].is_ascii_alphabetic()
                || !key
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
                || matches!(key.as_str(), "__proto__" | "constructor" | "prototype")
            {
                return Err("参数名无效或为保留属性".into());
            }
            if kind == ShellKind::PowerShell && !ps_keys.insert(key.to_ascii_lowercase()) {
                return Err("PowerShell 参数名不能仅大小写不同（如 foo / Foo）".into());
            }
            bytes = bytes.saturating_add(key.len()).saturating_add(value.len());
            if bytes > INPUT_LIMIT {
                return Err("参数 JSON 不得超过 160KB".into());
            }
        }
        let input = serde_json::to_vec(&inputs).map_err(|e| e.to_string())?;
        if input.len() > INPUT_LIMIT {
            return Err("参数 JSON 不得超过 160KB".into());
        }
        Ok(Self {
            run_id,
            app_id,
            script,
            shell: detected,
            input,
            timeout: Duration::from_secs(timeout_seconds),
        })
    }
}

impl Registry {
    fn get_authorized(&self, label: &str, run_id: &str) -> Result<Option<&Arc<Run>>, String> {
        // Validate the caller even for unknown/evicted runs, before any lookup.
        runtime_app_id(label)?;
        let run = self.runs.get(&parse_id(run_id)?);
        if let Some(run) = run {
            authorize_app(label, &run.app_id)?;
        }
        Ok(run)
    }

    fn reserve(&mut self, request: &Request) -> Result<Arc<Run>, String> {
        if self.seen.contains(&request.run_id) {
            return Err("runId 已使用，禁止重复执行".into());
        }
        // ponytail: bounded lifetime tombstones fail closed at 65,536 runs; use a
        // durable deduplication store if a single process must exceed this ceiling.
        if self.seen.len() >= SEEN_LIMIT {
            return Err("运行 ID 记录已满，请重启应用后使用新的 runId".into());
        }
        let active: Vec<_> = self.runs.values().filter(|r| r.running()).collect();
        if active.iter().any(|r| r.app_id == request.app_id) {
            return Err("该观星台应用已有运行中的脚本".into());
        }
        if active.len() >= ACTIVE_LIMIT {
            return Err("同时运行的脚本已达上限".into());
        }
        if self.runs.len() >= RUN_LIMIT {
            let oldest = self
                .runs
                .values()
                .filter(|r| !r.running())
                .min_by_key(|r| r.created)
                .map(|r| r.id)
                .ok_or("运行状态已满")?;
            self.runs.remove(&oldest);
        }
        let run = Arc::new(Run {
            id: request.run_id,
            app_id: request.app_id,
            created: Instant::now(),
            state: Mutex::new(RunState {
                status: "running",
                stdout: Vec::new(),
                stderr: Vec::new(),
                exit_code: None,
                truncated: false,
                message: String::new(),
            }),
            cancel: Notify::new(),
        });
        self.seen.insert(run.id);
        self.runs.insert(run.id, run.clone());
        Ok(run)
    }
}

#[tauri::command]
pub async fn miniapp_command_start(
    window: WebviewWindow,
    run_id: String,
    app_id: String,
    script: String,
    shell: String,
    inputs: BTreeMap<String, String>,
    timeout_seconds: u64,
) -> Result<CommandSnapshot, String> {
    authorize_app(window.label(), &parse_id(&app_id)?)?;
    let request = Request::validate(&run_id, &app_id, script, &shell, inputs, timeout_seconds)?;
    let cwd = crate::nova_data_dir(window.app_handle())
        .join("miniapps")
        .join(request.app_id.to_string());
    let run = registry()
        .lock()
        .map_err(|_| "运行状态锁不可用")?
        .reserve(&request)?;
    let snapshot = run.snapshot();
    // Own the guard before spawning: even an unpolled/dropped future cleans up.
    let task = Execution {
        run,
        child: None,
        pid: None,
    };
    tauri::async_runtime::spawn(task.execute(request, cwd));
    Ok(snapshot)
}

#[tauri::command]
pub fn miniapp_command_status(
    window: WebviewWindow,
    run_id: String,
) -> Result<Option<CommandSnapshot>, String> {
    Ok(registry()
        .lock()
        .map_err(|_| "运行状态锁不可用")?
        .get_authorized(window.label(), &run_id)?
        .map(|run| run.snapshot()))
}

#[tauri::command]
pub fn miniapp_command_cancel(window: WebviewWindow, run_id: String) -> Result<(), String> {
    if let Some(run) = registry()
        .lock()
        .map_err(|_| "运行状态锁不可用")?
        .get_authorized(window.label(), &run_id)?
    {
        // notify_one retains a permit if cancellation beats the worker's first poll.
        if run.running() {
            run.cancel.notify_one();
        }
    }
    Ok(())
}

async fn drain(mut pipe: impl AsyncRead + Unpin, run: &Run, stderr: bool) -> std::io::Result<()> {
    let mut chunk = [0u8; 8192];
    loop {
        let count = pipe.read(&mut chunk).await?;
        if count == 0 {
            return Ok(());
        }
        let mut state = run.state.lock().unwrap_or_else(|e| e.into_inner());
        let output = if stderr {
            &mut state.stderr
        } else {
            &mut state.stdout
        };
        let keep = count.min(PIPE_LIMIT - output.len());
        output.extend_from_slice(&chunk[..keep]);
        state.truncated |= keep < count;
        // Continue draining after the cap so a noisy process cannot block its exit.
    }
}

struct Execution {
    run: Arc<Run>,
    child: Option<Child>,
    pid: Option<u32>,
}

impl Execution {
    fn kill(&mut self) {
        if let Some(pid) = self.pid.take() {
            crate::acp::kill_process_tree(pid);
        }
        if let Some(child) = self.child.as_mut() {
            let _ = child.start_kill();
        }
    }

    async fn execute(mut self, request: Request, cwd: PathBuf) {
        let run = self.run.clone();
        let work = async {
            tokio::fs::create_dir_all(&cwd).await?;
            let mut command = Command::new(&request.shell.program);
            match request.shell.kind {
                ShellKind::PowerShell => {
                    command
                        .args(["-NoProfile", "-NonInteractive", "-Command"])
                        .arg(format!("{POWERSHELL_PREFIX}{}", request.script));
                }
                ShellKind::Bash => {
                    command.arg("-c").arg(&request.script);
                }
            }
            command
                .current_dir(cwd)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);
            #[cfg(unix)]
            command.process_group(0);
            #[cfg(windows)]
            command.creation_flags(0x08000000); // CREATE_NO_WINDOW
            self.child = Some(command.spawn()?);
            let child = self.child.as_mut().unwrap();
            self.pid = child.id();
            crate::acp::assign_to_agent_job(child);
            let mut stdin = child.stdin.take().unwrap();
            let stdout = child.stdout.take().unwrap();
            let stderr = child.stderr.take().unwrap();
            let write = async {
                let result = stdin.write_all(&request.input).await;
                drop(stdin); // EOF is part of the protocol, including empty inputs.
                match result {
                    Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
                    other => other,
                }
            };
            let pid = &mut self.pid;
            let wait = async {
                let status = child.wait().await?;
                *pid = None; // Reaped PID may be reused: never kill it again.
                run.state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .exit_code = status.code();
                Ok::<_, std::io::Error>(status)
            };
            let io = async {
                tokio::try_join!(write, drain(stdout, &run, false), drain(stderr, &run, true))?;
                Ok::<_, std::io::Error>(())
            };
            tokio::pin!(io);
            let status = tokio::select! {
                status = wait => {
                    let status = status?;
                    // A detached descendant may retain a pipe. Do not wait forever
                    // or kill a reaped root PID to chase it; return partial output.
                    match tokio::time::timeout(Duration::from_millis(500), &mut io).await {
                        Ok(result) => result?,
                        Err(_) => run.state.lock().unwrap_or_else(|e| e.into_inner()).truncated = true,
                    }
                    status
                }
                result = &mut io => {
                    result?;
                    let status = child.wait().await?;
                    self.pid = None;
                    status
                }
            };
            Ok::<_, std::io::Error>(status)
        };
        let (status, message) = tokio::select! {
            biased;
            _ = run.cancel.notified() => ("cancelled", "执行已取消".to_string()),
            result = tokio::time::timeout(request.timeout, work) => match result {
                Err(_) => ("error", format!("执行超时（{} 秒）", request.timeout.as_secs())),
                Ok(Err(e)) => ("error", format!("执行失败：{e}")),
                Ok(Ok(exit)) if exit.success() => ("finished", String::new()),
                Ok(Ok(exit)) => ("error", format!("脚本非零退出：{exit}")),
            },
        };
        // Normal exit has disarmed the PID guard. On interruption, pipe futures
        // are already dropped; kill the live tree and bound root-process reaping.
        self.kill();
        if let Some(child) = self.child.as_mut() {
            if let Ok(Ok(exit)) = tokio::time::timeout(Duration::from_secs(2), child.wait()).await {
                run.state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .exit_code = exit.code();
            }
        }
        let mut state = run.state.lock().unwrap_or_else(|e| e.into_inner());
        state.status = status;
        state.message = message;
    }
}

impl Drop for Execution {
    fn drop(&mut self) {
        self.kill();
        let mut state = self.run.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.status == "running" {
            state.status = "cancelled";
            state.message = "执行任务已中断".into();
        }
        // Child::kill_on_drop additionally arranges root-process reaping on abort.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shell() -> &'static str {
        if detect_shell().kind == ShellKind::PowerShell {
            "powershell"
        } else {
            "bash"
        }
    }

    fn request(script: &str, inputs: BTreeMap<String, String>, seconds: u64) -> Request {
        Request::validate(
            &Uuid::new_v4().to_string(),
            &Uuid::new_v4().to_string(),
            script.into(),
            shell(),
            inputs,
            seconds,
        )
        .unwrap()
    }

    fn launch(request: Request, cwd: PathBuf) -> (Arc<Run>, tokio::task::JoinHandle<()>) {
        let run = Registry::default().reserve(&request).unwrap();
        let execution = Execution {
            run: run.clone(),
            child: None,
            pid: None,
        };
        (run, tokio::spawn(execution.execute(request, cwd)))
    }

    async fn finish(task: tokio::task::JoinHandle<()>) {
        tokio::time::timeout(Duration::from_secs(15), task)
            .await
            .unwrap()
            .unwrap();
    }

    #[test]
    fn validation_and_wire_contract() {
        let id = Uuid::new_v4().to_string();
        let check = |script: &str, inputs: BTreeMap<String, String>, seconds| {
            Request::validate(&id, &id, script.into(), shell(), inputs, seconds)
        };
        assert!(parse_id("../escape").is_err());
        for script in ["", " \n", "{{field", "field}}", "a\0b"] {
            assert!(check(script, BTreeMap::new(), 1).is_err());
        }
        assert!(check(&"#".repeat(SCRIPT_LIMIT), BTreeMap::new(), 120).is_ok());
        assert!(check(&"é".repeat(SCRIPT_LIMIT / 2 + 1), BTreeMap::new(), 1).is_err());
        for seconds in [0, 121, u64::MAX] {
            assert!(check("exit 0", BTreeMap::new(), seconds).is_err());
        }
        for key in [
            "",
            "1abc",
            "a.b",
            "__proto__",
            "prototype",
            "constructor",
            "字段",
        ] {
            assert!(check("exit 0", BTreeMap::from([(key.into(), "x".into())]), 1).is_err());
        }
        for key in ["toString", "hasOwnProperty", "Constructor", "Prototype"] {
            assert!(check("exit 0", BTreeMap::from([(key.into(), "x".into())]), 1).is_ok());
        }
        let colliding = BTreeMap::from([("foo".into(), "1".into()), ("Foo".into(), "2".into())]);
        assert_eq!(
            check("exit 0", colliding, 1).is_err(),
            shell() == "powershell"
        );
        assert!(check("exit 0", BTreeMap::from([("a".repeat(64), "ok".into())]), 1).is_ok());
        assert!(check("exit 0", BTreeMap::from([("a".repeat(65), "ok".into())]), 1).is_err());
        let fields = (0..200)
            .map(|i| (format!("field_{i}"), String::new()))
            .collect::<BTreeMap<_, _>>();
        assert!(check("exit 0", fields.clone(), 1).is_ok());
        let mut too_many = fields;
        too_many.insert("extra".into(), String::new());
        assert!(check("exit 0", too_many, 1).is_err());
        assert!(check(
            "exit 0",
            BTreeMap::from([("a".into(), "x".repeat(INPUT_LIMIT - 8))]),
            1
        )
        .is_ok());
        assert!(check(
            "exit 0",
            BTreeMap::from([("a".into(), "x".repeat(INPUT_LIMIT - 7))]),
            1
        )
        .is_err());
        assert!(check(
            "exit 0",
            BTreeMap::from([("a".into(), "\0".repeat(INPUT_LIMIT / 2))]),
            1
        )
        .is_err());
        for invalid_shell in [
            "cmd",
            "PowerShell",
            "sh",
            if shell() == "bash" {
                "powershell"
            } else {
                "bash"
            },
        ] {
            assert!(Request::validate(
                &id,
                &id,
                "exit 0".into(),
                invalid_shell,
                BTreeMap::new(),
                1
            )
            .is_err());
        }
        let mut registry = Registry::default();
        let run = registry
            .reserve(&request("exit 0", BTreeMap::new(), 1))
            .unwrap();
        let wire = serde_json::to_value(run.snapshot()).unwrap();
        assert_eq!(wire["runId"], run.id.to_string());
        assert_eq!(wire["status"], "running");
        assert_eq!(wire["stdout"], "");
        assert!(wire.get("exitCode").is_none());
        assert_eq!(wire["truncated"], false);
    }

    #[test]
    fn run_lookup_rejects_other_apps_and_invalid_callers_even_for_unknown_ids() {
        let app = Uuid::new_v4();
        let other = Uuid::new_v4();
        let run = Arc::new(Run {
            id: Uuid::new_v4(),
            app_id: app,
            created: Instant::now(),
            state: Mutex::new(RunState {
                status: "running", stdout: Vec::new(), stderr: Vec::new(),
                exit_code: None, truncated: false, message: String::new(),
            }),
            cancel: Notify::new(),
        });
        let mut registry = Registry::default();
        registry.runs.insert(run.id, run.clone());
        let id = run.id.to_string();
        let unknown = Uuid::new_v4().to_string();
        for label in ["main".to_string(), format!("miniapp-{app}")] {
            assert!(Arc::ptr_eq(registry.get_authorized(&label, &id).unwrap().unwrap(), &run));
            assert!(registry.get_authorized(&label, &unknown).unwrap().is_none());
            assert!(registry.get_authorized(&label, "invalid").is_err());
        }
        let other_label = format!("miniapp-{other}");
        assert!(registry.get_authorized(&other_label, &id).is_err());
        assert!(registry.get_authorized(&other_label, &unknown).unwrap().is_none());
        for label in ["meteor", "taskbar-bubble", "miniapp-", "miniapp-not-a-uuid", "main-other"] {
            assert!(registry.get_authorized(label, &id).is_err());
            assert!(registry.get_authorized(label, &unknown).is_err());
        }
        assert!(run.running());
        assert!(!Box::pin(run.cancel.notified()).as_mut().enable());
    }

    #[test]
    fn snapshot_stdout_is_separate_and_bounded_utf8() {
        let run = Registry::default()
            .reserve(&request("exit 0", BTreeMap::new(), 1))
            .unwrap();
        for (bytes, expected, truncated) in [
            (b"[\"main\"]".to_vec(), "[\"main\"]".to_string(), false),
            (Vec::new(), String::new(), false),
            // Lossy decoding expands invalid bytes; cap the UTF-8 string too.
            (
                vec![0xff; PIPE_LIMIT],
                "\u{fffd}".repeat(PIPE_LIMIT / 3),
                true,
            ),
        ] {
            {
                let mut state = run.state.lock().unwrap();
                state.stdout = bytes;
                state.stderr = b"warning".to_vec();
                state.message = "runner message".into();
                state.truncated = false;
            }
            let snapshot = run.snapshot();
            assert_eq!(snapshot.stdout, expected);
            assert!(snapshot.stdout.len() <= PIPE_LIMIT);
            assert_eq!(snapshot.truncated, truncated);
            assert_eq!(
                snapshot.output,
                if expected.is_empty() {
                    "warning\nrunner message".into()
                } else {
                    format!("{expected}\nwarning\nrunner message")
                }
            );
            run.state.lock().unwrap().truncated = true;
            assert!(run.snapshot().truncated);
        }
    }

    #[test]
    fn duplicate_and_same_app_are_atomic_and_survive_eviction() {
        let locked = Mutex::new(Registry::default());
        let mut first = request("exit 0", BTreeMap::new(), 1);
        let other = Request {
            app_id: first.app_id,
            ..request("exit 0", BTreeMap::new(), 1)
        };
        let barrier = std::sync::Barrier::new(3);
        let run = std::thread::scope(|scope| {
            let locked = &locked;
            let barrier = &barrier;
            let workers = [&first, &first, &other].map(|request| {
                scope.spawn(move || {
                    barrier.wait();
                    locked.lock().unwrap().reserve(request)
                })
            });
            let mut accepted: Vec<_> = workers
                .into_iter()
                .filter_map(|w| w.join().unwrap().ok())
                .collect();
            assert_eq!(accepted.len(), 1); // duplicate run and same-app contenders
            accepted.pop().unwrap()
        });
        let mut registry = locked.into_inner().unwrap();
        first.run_id = run.id;
        assert!(registry.reserve(&first).is_err());
        let first_id = first.run_id;
        first.run_id = Uuid::new_v4();
        assert!(registry.reserve(&first).is_err());
        run.state.lock().unwrap().status = "finished";
        for _ in 0..RUN_LIMIT {
            let run = registry
                .reserve(&request("exit 0", BTreeMap::new(), 1))
                .unwrap();
            run.state.lock().unwrap().status = "finished";
        }
        assert_eq!(registry.runs.len(), RUN_LIMIT);
        assert!(!registry.runs.contains_key(&first_id));
        first.run_id = first_id;
        assert!(registry.reserve(&first).is_err());
        assert!(registry
            .reserve(&Request {
                run_id: Uuid::new_v4(),
                ..first
            })
            .is_ok());
    }

    #[tokio::test]
    async fn local_shell_inputs_are_literal_and_nonzero_is_error() {
        let dir = tempfile::tempdir().unwrap();
        let malicious =
            "中文 \"'; $(New-Item -ItemType File pwned); $(touch pwned); `touch pwned`\nexit 77";
        let inputs = BTreeMap::from([("field".into(), malicious.into())]);
        let expected = if shell() == "powershell" {
            malicious.to_string()
        } else {
            serde_json::to_string(&inputs).unwrap()
        };
        let script = if shell() == "powershell" {
            "[Console]::Out.Write($nova.field)"
        } else {
            "cat"
        };
        let (run, task) = launch(request(script, inputs, 10), dir.path().to_path_buf());
        finish(task).await;
        let snapshot = run.snapshot();
        assert_eq!(snapshot.status, "finished", "{snapshot:?}");
        assert_eq!(snapshot.output, expected);
        assert_eq!(snapshot.stdout, expected);
        assert_eq!(snapshot.exit_code, Some(0));
        assert!(!dir.path().join("pwned").exists());
        let (run, task) = launch(
            request("exit 7", BTreeMap::new(), 10),
            dir.path().to_path_buf(),
        );
        finish(task).await;
        assert_eq!(run.snapshot().status, "error");
        assert_eq!(run.snapshot().exit_code, Some(7));
    }

    #[tokio::test]
    async fn local_shell_output_caps_continue_draining() {
        let dir = tempfile::tempdir().unwrap();
        let script = if shell() == "powershell" {
            "[Console]::Out.Write(('o' * 100000)); [Console]::Error.Write(('e' * 100000))"
        } else {
            "head -c 100000 /dev/zero | tr '\\0' o; head -c 100000 /dev/zero | tr '\\0' e >&2"
        };
        let (run, task) = launch(
            request(script, BTreeMap::new(), 10),
            dir.path().to_path_buf(),
        );
        finish(task).await;
        let snapshot = run.snapshot();
        assert_eq!(snapshot.status, "finished", "{snapshot:?}");
        assert!(snapshot.truncated);
        assert_eq!(snapshot.stdout, "o".repeat(PIPE_LIMIT));
        assert_eq!(
            snapshot.output,
            format!("{}\n{}", "o".repeat(PIPE_LIMIT), "e".repeat(PIPE_LIMIT))
        );
    }

    #[tokio::test]
    async fn local_shell_timeout_cancel_and_drop() {
        let dir = tempfile::tempdir().unwrap();
        let script = if shell() == "powershell" {
            "[Console]::Out.Write('ready'); Start-Sleep -Seconds 30; Set-Content leaked bad"
        } else {
            "printf ready; sleep 30; touch leaked"
        };
        for mode in ["timeout", "cancel", "drop"] {
            // Bash does not read this larger-than-pipe input; cancellation/timeout
            // must cover a blocked stdin writer as well as child.wait/readers.
            let inputs = BTreeMap::from([("data".into(), "x".repeat(150 * 1024))]);
            let (run, task) = launch(
                request(script, inputs, if mode == "timeout" { 1 } else { 10 }),
                dir.path().to_path_buf(),
            );
            if mode != "timeout" {
                tokio::time::timeout(Duration::from_secs(8), async {
                    while !run.snapshot().output.contains("ready") {
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                })
                .await
                .unwrap();
                if mode == "cancel" {
                    run.cancel.notify_one();
                } else {
                    task.abort();
                }
            }
            if mode == "drop" {
                assert!(task.await.unwrap_err().is_cancelled());
            } else {
                finish(task).await;
            }
            assert_eq!(
                run.snapshot().status,
                if mode == "timeout" {
                    "error"
                } else {
                    "cancelled"
                }
            );
            assert!(!dir.path().join("leaked").exists());
        }
        let (run, task) = launch(
            request(script, BTreeMap::new(), 10),
            dir.path().to_path_buf(),
        );
        run.cancel.notify_one();
        finish(task).await;
        assert_eq!(run.snapshot().status, "cancelled");
        assert!(run.snapshot().output.is_empty() || run.snapshot().output.contains("执行已取消"));
    }
}
