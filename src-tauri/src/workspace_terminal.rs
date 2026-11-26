//! Main-webview-only interactive PTYs, retained while the workspace is hidden.
use crate::AppState;
use portable_pty::{native_pty_system, Child, ChildKiller, CommandBuilder, MasterPty, PtySize};
use regex::Regex;
use serde::Serialize;
use std::{
    collections::HashMap,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex, OnceLock},
};
use tauri::{ipc::Channel, AppHandle, Emitter, Manager, State, Webview};
const MAX_TERMINALS: usize = 32;
const OUTPUT_WINDOW: usize = 128 * 1024;
const INPUT_LIMIT: usize = 64 * 1024;
#[derive(Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum TerminalEvent {
    #[cfg(windows)]
    WindowsPty {
        build: Option<u32>,
    },
    Data {
        data: Vec<u8>,
    },
    Exit {
        code: Option<u32>,
    },
    Error {
        message: String,
    },
}
#[derive(Default)]
struct Flow {
    pending: usize,
    closed: bool,
    reaped: bool,
}
struct Session {
    thread_id: Option<String>,
    master: Mutex<Option<Box<dyn MasterPty + Send>>>,
    writer: Mutex<Box<dyn Write + Send>>,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    pid: Option<u32>,
    flow: Mutex<Flow>,
    ready: Condvar,
}
impl Session {
    fn stop(&self) {
        let mut flow = self.flow.lock().unwrap_or_else(|e| e.into_inner());
        if flow.closed {
            return;
        }
        flow.closed = true;
        let reaped = flow.reaped;
        self.ready.notify_all();
        // The output thread must drain while process termination/ConPTY close
        // runs. Never hold its flow-control mutex across blocking OS calls.
        drop(flow);
        if !reaped {
            if let Some(pid) = self.pid {
                crate::acp::kill_process_tree(pid);
            }
            let _ = self.killer.lock().unwrap_or_else(|e| e.into_inner()).kill();
        }
        // A startup query may already have reached a now-disposed renderer.
        // Complete that handshake even when no further output is delivered.
        #[cfg(windows)]
        self.finish_cursor_handshake();
    }
    #[cfg(windows)]
    fn finish_cursor_handshake(&self) {
        let mut writer = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        let _ = writer.write_all(b"\x1b[1;1R").and_then(|_| writer.flush());
    }
    fn finish(&self) {
        self.flow.lock().unwrap_or_else(|e| e.into_inner()).closed = true;
        self.ready.notify_all();
    }
    fn acknowledge(&self, bytes: usize) {
        let mut flow = self.flow.lock().unwrap_or_else(|e| e.into_inner());
        flow.pending = flow.pending.saturating_sub(bytes);
        self.ready.notify_all();
    }
    fn is_closed(&self) -> bool {
        self.flow.lock().unwrap_or_else(|e| e.into_inner()).closed
    }
}
/// Wait independently of the output reader. ConPTY does not send EOF until the
/// pseudoconsole is closed; waiting for EOF before dropping the master deadlocks.
/// Keep draining output concurrently with ClosePseudoConsole on Windows 10.
fn wait_for_exit(mut child: Box<dyn Child + Send + Sync>, session: &Session) -> Option<u32> {
    let code = child.wait().ok().map(|status| status.exit_code());
    session
        .flow
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .reaped = true;
    #[cfg(windows)]
    {
        let master = session
            .master
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        drop(master); // Never close the pseudoconsole under a mutex or on the reader.
    }
    code
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgyAgentPhase {
    Idle,
    Working,
    ToolUse,
    Reviewing,
    Error,
}

impl AgyAgentPhase {
    pub fn is_running(self) -> bool {
        matches!(self, Self::Working | Self::ToolUse | Self::Reviewing)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Working => "working",
            Self::ToolUse => "tool_use",
            Self::Reviewing => "reviewing",
            Self::Error => "error",
        }
    }

    fn priority(self) -> u8 {
        match self {
            Self::Reviewing => 4,
            Self::ToolUse => 3,
            Self::Working => 2,
            Self::Error => 1,
            Self::Idle => 0,
        }
    }

    fn max_phase(self, other: Self) -> Self {
        if other.priority() > self.priority() {
            other
        } else {
            self
        }
    }
}

#[derive(Clone, Debug)]
struct AgySessionState {
    thread_id: String,
    phase: AgyAgentPhase,
    started_at: Option<i64>,
}

#[derive(Clone, Default)]
pub struct TerminalManager {
    sessions: Arc<Mutex<HashMap<String, Arc<Session>>>>,
    agy_states: Arc<Mutex<HashMap<String, AgySessionState>>>,
}
impl TerminalManager {
    pub fn is_thread_running(&self, thread_id: &str) -> bool {
        let states = self.agy_states.lock().unwrap_or_else(|e| e.into_inner());
        states
            .values()
            .any(|s| s.thread_id == thread_id && s.phase.is_running())
    }
    pub fn thread_phase(&self, thread_id: &str) -> Option<AgyAgentPhase> {
        let states = self.agy_states.lock().unwrap_or_else(|e| e.into_inner());
        states
            .values()
            .filter(|s| s.thread_id == thread_id)
            .map(|s| s.phase)
            .reduce(AgyAgentPhase::max_phase)
    }
    pub fn thread_started_at(&self, thread_id: &str) -> Option<i64> {
        let states = self.agy_states.lock().unwrap_or_else(|e| e.into_inner());
        states
            .values()
            .filter(|s| s.thread_id == thread_id && s.phase.is_running())
            .filter_map(|s| s.started_at)
            .min()
    }
    fn set_session_agy_phase(
        &self,
        session_id: &str,
        thread_id: &str,
        phase: AgyAgentPhase,
    ) -> (bool, bool, bool) {
        let mut states = self.agy_states.lock().unwrap_or_else(|e| e.into_inner());
        let was_running = states
            .values()
            .any(|s| s.thread_id == thread_id && s.phase.is_running());
        let prev = states.get(session_id).cloned();
        let phase_changed = prev.as_ref().map(|s| s.phase) != Some(phase);
        let started_at = if phase.is_running() {
            prev.and_then(|s| s.started_at)
                .or_else(|| Some(crate::threads::now_ms()))
        } else {
            None
        };
        states.insert(
            session_id.to_string(),
            AgySessionState {
                thread_id: thread_id.to_string(),
                phase,
                started_at,
            },
        );
        let now_running = states
            .values()
            .any(|s| s.thread_id == thread_id && s.phase.is_running());
        (was_running, now_running, phase_changed)
    }
    fn remove_session_agy_state(&self, session_id: &str) -> Option<(String, bool, bool)> {
        let mut states = self.agy_states.lock().unwrap_or_else(|e| e.into_inner());
        let prev = states.remove(session_id)?;
        let was_running = prev.phase.is_running()
            || states
                .values()
                .any(|s| s.thread_id == prev.thread_id && s.phase.is_running());
        let now_running = states
            .values()
            .any(|s| s.thread_id == prev.thread_id && s.phase.is_running());
        Some((prev.thread_id, was_running, now_running))
    }
    fn apply_agy_phase(&self, app: &AppHandle, session_id: &str, thread_id: &str, phase: AgyAgentPhase) {
        let (was_running, now_running, phase_changed) =
            self.set_session_agy_phase(session_id, thread_id, phase);
        if phase_changed {
            let _ = app.emit(
                EV_TERMINAL_AGY_STATE,
                serde_json::json!({
                    "threadId": thread_id,
                    "sessionId": session_id,
                    "state": phase.as_str(),
                }),
            );
        }
        if was_running != now_running {
            let stop_reason = if now_running {
                None
            } else if phase == AgyAgentPhase::Error {
                Some("error")
            } else {
                Some("end_turn")
            };
            emit_agy_turn_transition(app, thread_id, now_running, stop_reason);
        }
    }
    fn clear_agy_session(&self, app: &AppHandle, session_id: &str) {
        let Some((thread_id, was_running, now_running)) = self.remove_session_agy_state(session_id)
        else {
            return;
        };
        let fallback_state = self
            .thread_phase(&thread_id)
            .map(|p| p.as_str())
            .unwrap_or("idle");
        let _ = app.emit(
            EV_TERMINAL_AGY_STATE,
            serde_json::json!({
                "threadId": &thread_id,
                "sessionId": session_id,
                "state": fallback_state,
            }),
        );
        if was_running && !now_running {
            emit_agy_turn_transition(app, &thread_id, false, Some("cancelled"));
        }
    }
    fn get(&self, id: &str) -> Result<Arc<Session>, String> {
        self.sessions
            .lock()
            .map_err(|e| e.to_string())?
            .get(id)
            .cloned()
            .ok_or_else(|| "终端已关闭".into())
    }
    pub fn close_all(&self) {
        let sessions: Vec<_> = self
            .sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain()
            .map(|(_, s)| s)
            .collect();
        for session in sessions {
            session.stop();
        }
    }
    fn close(&self, id: &str) -> Result<(), String> {
        let session = self.sessions.lock().map_err(|e| e.to_string())?.remove(id);
        if let Some(session) = session {
            session.stop();
        }
        Ok(())
    }
    pub fn close_for_thread(&self, thread_id: &str) {
        let sessions: Vec<_> = {
            let mut map = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
            let ids: Vec<String> = map
                .iter()
                .filter(|(_, s)| s.thread_id.as_deref() == Some(thread_id))
                .map(|(k, _)| k.clone())
                .collect();
            ids.into_iter().filter_map(|id| map.remove(&id)).collect()
        };
        for session in sessions {
            session.stop();
        }
    }
}
pub(crate) fn require_main(webview: &Webview) -> Result<(), String> {
    let url = webview.url().map_err(|e| e.to_string())?;
    let local = url.scheme() == "tauri"
        || (matches!(url.scheme(), "http" | "https")
            && matches!(
                url.host_str(),
                Some("tauri.localhost" | "localhost" | "127.0.0.1" | "[::1]")
            ));
    if webview.label() != "main" || !local {
        return Err("此操作仅允许本地主界面访问".into());
    }
    Ok(())
}
fn size(cols: u16, rows: u16) -> Result<PtySize, String> {
    if !(1..=1000).contains(&cols) || !(1..=1000).contains(&rows) {
        return Err("终端尺寸必须为 1–1000 行/列".into());
    }
    Ok(PtySize {
        cols,
        rows,
        pixel_width: 0,
        pixel_height: 0,
    })
}
fn resolve_shell_and_args(
    default_shell: &str,
    default_args: &[String],
    config: Option<&crate::threads::TerminalSessionConfig>,
) -> (String, Vec<String>) {
    match config {
        Some(cfg) if !cfg.shell.trim().is_empty() => (cfg.shell.trim().to_owned(), cfg.args.clone()),
        Some(cfg) if !cfg.args.is_empty() => (default_shell.to_owned(), cfg.args.clone()),
        _ => (default_shell.to_owned(), default_args.to_vec()),
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalCreateResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub init_command: Option<String>,
}

const AGY_LOG_ENV_VAR: &str = "NOVA_AGY_LOG_FILE";
const AGY_SETTINGS_ENV_VAR: &str = "NOVA_AGY_SETTINGS_FILE";
const AGY_STATE_FILE_ENV_VAR: &str = "NOVA_AGY_STATE_FILE";
const AGY_TITLE_CMD_ENV_VAR: &str = "NOVA_AGY_TITLE_CMD";
const AGY_TITLE_EXE_ENV_VAR: &str = "NOVA_AGY_TITLE_EXE";
pub const EV_TERMINAL_AGY_STATE: &str = "terminal:agy-state";
pub const EV_TERMINAL_AGY_PROMPTS: &str = "terminal:agy-prompts";
pub const EV_TERMINAL_AGY_TURNS: &str = "terminal:agy-turns";

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgyUserPrompt {
    pub index: usize,
    pub text: String,
    pub timestamp: i64,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgyTurn {
    pub index: usize,
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<String>,
    pub prompt_timestamp: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_timestamp: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// Antigravity CLI `title` 状态回调子模式（`nova --agy-state-sink`）：
/// 从 stdin 读取 CLI 推送的 JSON 状态快照并原子写入 `NOVA_AGY_STATE_FILE`，不输出任何内容。
pub fn maybe_run_agy_state_sink() -> bool {
    if !std::env::args_os()
        .nth(1)
        .is_some_and(|arg| arg == "--agy-state-sink")
    {
        return false;
    }
    let Some(state_path) = std::env::var_os(AGY_STATE_FILE_ENV_VAR)
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
    else {
        return true;
    };
    let mut buf = Vec::new();
    if std::io::stdin()
        .take(256 * 1024)
        .read_to_end(&mut buf)
        .is_err()
        || buf.iter().all(|b| b.is_ascii_whitespace())
    {
        return true;
    }
    if let Some(parent) = state_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut tmp_os = state_path.as_os_str().to_os_string();
    tmp_os.push(format!(".tmp.{}", std::process::id()));
    let tmp_path = PathBuf::from(tmp_os);
    if std::fs::write(&tmp_path, &buf).is_ok() {
        if std::fs::rename(&tmp_path, &state_path).is_err() {
            let _ = std::fs::remove_file(&tmp_path);
            let _ = std::fs::write(&state_path, &buf);
        }
    } else {
        let _ = std::fs::write(&state_path, &buf);
    }
    true
}

#[derive(Debug, PartialEq, Eq)]
struct AgyStateSnapshot {
    phase: Option<AgyAgentPhase>,
    conversation_id: Option<String>,
    title: Option<String>,
}

fn parse_agy_state_snapshot(text: &str) -> Option<AgyStateSnapshot> {
    let val = serde_json::from_str::<serde_json::Value>(text).ok()?;
    if !val.is_object() {
        return None;
    }
    let phase = match val["agent_state"]
        .as_str()
        .map(|s| s.trim().to_ascii_lowercase())
        .as_deref()
    {
        Some("working") => Some(AgyAgentPhase::Working),
        Some("tool_use") => Some(AgyAgentPhase::ToolUse),
        Some("reviewing") => Some(AgyAgentPhase::Reviewing),
        Some("idle") => Some(AgyAgentPhase::Idle),
        Some("error") => Some(AgyAgentPhase::Error),
        _ => None,
    };
    let conversation_id = val["conversation_id"]
        .as_str()
        .or_else(|| val["session_id"].as_str())
        .map(str::trim)
        .filter(|id| uuid::Uuid::parse_str(id).is_ok())
        .map(|id| id.to_ascii_lowercase());
    let title = val["conversation_title"]
        .as_str()
        .map(|s| s.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|s| !s.is_empty());
    Some(AgyStateSnapshot {
        phase,
        conversation_id,
        title,
    })
}

fn agy_title_command_value() -> &'static str {
    if cfg!(windows) {
        // Windows: agy statusline_runner uses `cmd /c <command>` via Go exec.Command, which
        // escapes quotes inside `<command>`. Keep bare %VAR% here and put the quoted exe in env.
        "%NOVA_AGY_TITLE_CMD%"
    } else {
        "\"$NOVA_AGY_TITLE_EXE\" --agy-state-sink"
    }
}

fn write_agy_settings_file(path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let payload = serde_json::json!({
        "title": {
            "type": "command",
            "command": agy_title_command_value(),
        }
    });
    let bytes = serde_json::to_vec(&payload).map_err(|e| e.to_string())?;
    std::fs::write(path, bytes).map_err(|e| e.to_string())
}

fn apply_agy_state_env(command: &mut CommandBuilder, state_path: &Path) {
    command.env(AGY_STATE_FILE_ENV_VAR, state_path);
    if let Ok(exe) = std::env::current_exe() {
        if cfg!(windows) {
            command.env(
                AGY_TITLE_CMD_ENV_VAR,
                format!("\"{}\" --agy-state-sink", exe.display()),
            );
        } else {
            command.env(AGY_TITLE_EXE_ENV_VAR, exe);
        }
    }
}

fn default_shell_path() -> String {
    if cfg!(windows) {
        std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".into())
    } else {
        std::env::var("SHELL")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "/bin/sh".into())
    }
}

fn effective_shell_name(shell: &str) -> String {
    let resolved = if shell.trim().is_empty() {
        default_shell_path()
    } else {
        shell.trim().to_owned()
    };
    Path::new(&resolved)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(&resolved)
        .to_ascii_lowercase()
}

fn shell_env_ref(shell: &str, var: &str) -> String {
    match effective_shell_name(shell).as_str() {
        "cmd" | "cmd.exe" => format!("%{var}%"),
        "pwsh" | "pwsh.exe" | "powershell" | "powershell.exe" => format!("$env:{var}"),
        _ => format!("${var}"),
    }
}

fn is_agy_executable(token: &str, configured_path: &str) -> bool {
    let unquoted = token
        .trim()
        .trim_matches('"')
        .trim_matches('\'');
    if unquoted.is_empty() {
        return false;
    }
    let name = Path::new(unquoted)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(unquoted)
        .to_ascii_lowercase();
    if name == "agy" || name == "agy.exe" {
        return true;
    }
    let cfg = configured_path.trim().trim_matches('"').trim_matches('\'');
    if cfg.is_empty() {
        return false;
    }
    if cfg!(windows) {
        if unquoted.eq_ignore_ascii_case(cfg) {
            return true;
        }
        let cfg_name = Path::new(cfg)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(cfg)
            .to_ascii_lowercase();
        !cfg_name.is_empty() && name == cfg_name
    } else {
        unquoted == cfg
    }
}

fn split_shell_tokens(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for ch in input.chars() {
        if let Some(q) = quote {
            if ch == q {
                quote = None;
            } else {
                cur.push(ch);
            }
        } else if ch == '"' || ch == '\'' {
            quote = Some(ch);
        } else if ch.is_whitespace() {
            if !cur.is_empty() {
                tokens.push(std::mem::take(&mut cur));
            }
        } else {
            cur.push(ch);
        }
    }
    if !cur.is_empty() {
        tokens.push(cur);
    }
    tokens
}

fn split_first_token(s: &str) -> Option<(&str, &str)> {
    let s = s.trim_start();
    if s.is_empty() {
        return None;
    }
    let first = s.chars().next()?;
    if first == '"' || first == '\'' {
        let idx = first.len_utf8();
        for (offset, ch) in s[idx..].char_indices() {
            if ch == first {
                let end = idx + offset + ch.len_utf8();
                return Some((&s[..end], &s[end..]));
            }
        }
        Some((s, ""))
    } else {
        let end = s
            .char_indices()
            .find(|(_, ch)| ch.is_whitespace())
            .map(|(i, _)| i)
            .unwrap_or(s.len());
        Some((&s[..end], &s[end..]))
    }
}

fn is_agy_non_interactive_tail(tail: &str) -> bool {
    let tokens = split_shell_tokens(tail);
    let Some(first) = tokens.first() else {
        return false;
    };
    if matches!(
        first.as_str(),
        "acp"
            | "agent"
            | "agents"
            | "changelog"
            | "help"
            | "install"
            | "mcp"
            | "mic-serve"
            | "models"
            | "plugin"
            | "plugins"
            | "remote-control"
            | "update"
    ) {
        return true;
    }
    tokens.iter().any(|tok| {
        matches!(
            tok.as_str(),
            "-p" | "--print" | "--prompt" | "-h" | "--help" | "-v" | "--version"
        ) || tok.starts_with("--print=")
            || tok.starts_with("--prompt=")
    })
}

fn rewrite_agy_line(
    line: &str,
    antigravity_path: &str,
    log_env_ref: &str,
    settings_env_ref: Option<&str>,
    resume_id: Option<&str>,
) -> Option<String> {
    let trimmed_start = line.trim_start();
    let indent = &line[..line.len() - trimmed_start.len()];
    let (prefix, rest) = if let Some(after_amp) = trimmed_start.strip_prefix('&') {
        if after_amp.starts_with(char::is_whitespace) {
            let after_ws = after_amp.trim_start();
            let prefix_len = trimmed_start.len() - after_ws.len();
            (&trimmed_start[..prefix_len], after_ws)
        } else {
            ("", trimmed_start)
        }
    } else {
        ("", trimmed_start)
    };
    let (exe_tok, tail) = split_first_token(rest)?;
    if !is_agy_executable(exe_tok, antigravity_path) || is_agy_non_interactive_tail(tail) {
        return None;
    }
    let tokens = split_shell_tokens(tail);
    let has_resume = tokens.iter().any(|tok| {
        matches!(
            tok.as_str(),
            "-r" | "--resume" | "--conversation" | "-c" | "--continue"
        ) || tok.starts_with("--resume=")
            || tok.starts_with("--conversation=")
    });
    let has_log_file = tokens
        .iter()
        .any(|tok| tok == "--log-file" || tok.starts_with("--log-file="));
    let has_settings = tokens
        .iter()
        .any(|tok| tok == "--settings" || tok.starts_with("--settings="));
    let mut injected = String::new();
    if let Some(cid) = resume_id.filter(|id| !id.is_empty() && !has_resume) {
        injected.push_str(" --resume ");
        injected.push_str(cid);
    }
    if !has_log_file {
        injected.push_str(" --log-file \"");
        injected.push_str(log_env_ref);
        injected.push('"');
    }
    if let Some(settings_ref) = settings_env_ref.filter(|_| !has_settings) {
        injected.push_str(" --settings \"");
        injected.push_str(settings_ref);
        injected.push('"');
    }
    Some(format!("{indent}{prefix}{exe_tok}{injected}{tail}"))
}

fn rewrite_agy_init_command(
    raw: &str,
    antigravity_path: &str,
    log_env_ref: &str,
    settings_env_ref: Option<&str>,
    resume_id: Option<&str>,
) -> Option<String> {
    let mut matched = false;
    let mut lines = Vec::new();
    for line in raw.replace("\r\n", "\n").split('\n') {
        if let Some(updated) = rewrite_agy_line(
            line,
            antigravity_path,
            log_env_ref,
            settings_env_ref,
            resume_id,
        ) {
            matched = true;
            lines.push(updated);
        } else {
            lines.push(line.to_string());
        }
    }
    matched.then(|| lines.join("\n"))
}

fn user_home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

fn agy_cli_dir() -> Option<PathBuf> {
    user_home_dir().map(|home| home.join(".gemini").join("antigravity-cli"))
}

fn agy_conversation_exists(cli_dir: &Path, conv_id: &str) -> bool {
    if uuid::Uuid::parse_str(conv_id).is_err() {
        return false;
    }
    cli_dir.join("conversations").join(format!("{conv_id}.db")).is_file()
        || cli_dir.join("brain").join(conv_id).is_dir()
}

fn extract_agy_conversation_ids(text: &str) -> Vec<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(
            r"(?:Forwarding user message to conversation|Sending user message to conversation|Full redraw completed \(RerenderAll\) for conversation) ([0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12})",
        )
        .unwrap()
    });
    re.captures_iter(text)
        .filter_map(|cap| cap.get(1).map(|m| m.as_str().to_ascii_lowercase()))
        .collect()
}

fn read_agy_annotation_title(pbtxt: &str) -> Option<String> {
    let marker = "title:\"";
    let start = pbtxt.find(marker)? + marker.len();
    let bytes = pbtxt.as_bytes();
    let mut out = Vec::new();
    let mut i = start;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'"' {
            break;
        }
        if b == b'\\' && i + 1 < bytes.len() {
            let next = bytes[i + 1];
            match next {
                b'n' => {
                    out.push(b' ');
                    i += 2;
                    continue;
                }
                b'r' | b't' => {
                    out.push(b' ');
                    i += 2;
                    continue;
                }
                b'\\' | b'"' | b'\'' => {
                    out.push(next);
                    i += 2;
                    continue;
                }
                b'0'..=b'7' if i + 3 < bytes.len()
                    && (b'0'..=b'7').contains(&bytes[i + 2])
                    && (b'0'..=b'7').contains(&bytes[i + 3]) =>
                {
                    let val = ((next - b'0') << 6)
                        | ((bytes[i + 2] - b'0') << 3)
                        | (bytes[i + 3] - b'0');
                    out.push(val);
                    i += 4;
                    continue;
                }
                _ => {
                    out.push(next);
                    i += 2;
                    continue;
                }
            }
        }
        out.push(b);
        i += 1;
    }
    let title = String::from_utf8(out).ok()?;
    let trimmed = title.split_whitespace().collect::<Vec<_>>().join(" ");
    (!trimmed.is_empty()).then_some(trimmed)
}

fn format_prompt_title(prompt: &str) -> Option<String> {
    let collapsed = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return None;
    }
    let mut out: String = collapsed.chars().take(28).collect();
    if collapsed.chars().count() > 28 {
        out.push('…');
    }
    Some(out)
}

fn read_agy_first_prompt(history_path: &Path, conv_id: &str) -> Option<String> {
    let text = std::fs::read_to_string(history_path).ok()?;
    let mut first_any: Option<String> = None;
    for line in text.lines() {
        let Ok(val) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if !val["conversationId"]
            .as_str()
            .is_some_and(|id| id.eq_ignore_ascii_case(conv_id))
        {
            continue;
        }
        let Some(display) = val["display"].as_str().map(str::trim).filter(|s| !s.is_empty()) else {
            continue;
        };
        if first_any.is_none() {
            first_any = format_prompt_title(display);
        }
        if !display.starts_with('/') {
            return format_prompt_title(display);
        }
    }
    first_any
}

fn read_agy_user_prompts(history_path: &Path, conv_id: &str) -> Vec<AgyUserPrompt> {
    let Ok(text) = std::fs::read_to_string(history_path) else {
        return Vec::new();
    };
    let mut prompts = Vec::new();
    for line in text.lines() {
        let Ok(val) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if !val["conversationId"]
            .as_str()
            .is_some_and(|id| id.eq_ignore_ascii_case(conv_id))
        {
            continue;
        }
        if val["type"].as_str() == Some("slash_command") {
            continue;
        }
        let Some(display) = val["display"].as_str().map(str::trim).filter(|s| !s.is_empty()) else {
            continue;
        };
        if display.starts_with('/') {
            continue;
        }
        let timestamp = val["timestamp"].as_i64().unwrap_or(0);
        prompts.push(AgyUserPrompt {
            index: prompts.len() + 1,
            text: display.to_string(),
            timestamp,
        });
    }
    prompts
}

fn parse_rfc3339_ms(raw: Option<&str>) -> i64 {
    raw.and_then(|s| chrono::DateTime::parse_from_rfc3339(s.trim()).ok())
        .map(|dt| dt.timestamp_millis())
        .unwrap_or(0)
}

fn parse_agy_user_input_content(raw: &str) -> (String, Vec<String>) {
    let prompt = if let Some(start) = raw.find("<USER_REQUEST>") {
        let after = &raw[start + "<USER_REQUEST>".len()..];
        let body = match after.find("</USER_REQUEST>") {
            Some(end) => &after[..end],
            None => after,
        };
        body.trim_matches(|c| c == '\r' || c == '\n').to_string()
    } else {
        raw.trim().to_string()
    };

    let mut images = Vec::new();
    if let Some(meta_start) = raw.find("<ADDITIONAL_METADATA>") {
        let meta_after = &raw[meta_start + "<ADDITIONAL_METADATA>".len()..];
        let meta = match meta_after.find("</ADDITIONAL_METADATA>") {
            Some(end) => &meta_after[..end],
            None => meta_after,
        };
        let mut collecting_uploads = false;
        for line in meta.lines() {
            let trimmed = line.trim();
            if !collecting_uploads {
                if trimmed.starts_with("The user has uploaded ") {
                    collecting_uploads = true;
                }
                continue;
            }
            if trimmed.is_empty() {
                break;
            }
            if let Some(path) = trimmed.strip_prefix("- ") {
                let cleaned = path.trim();
                if !cleaned.is_empty() && !images.iter().any(|existing| existing == cleaned) {
                    images.push(cleaned.to_string());
                }
            } else {
                break;
            }
        }
    }

    (prompt, images)
}

fn read_agy_transcript_turns(transcript_path: &Path) -> Vec<AgyTurn> {
    let Ok(text) = std::fs::read_to_string(transcript_path) else {
        return Vec::new();
    };
    let mut turns: Vec<AgyTurn> = Vec::new();
    for line in text.lines() {
        let Ok(val) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let step_type = val["type"].as_str().unwrap_or("");
        let source = val["source"].as_str().unwrap_or("");
        if step_type == "USER_INPUT" && source == "USER_EXPLICIT" {
            let raw = val["content"].as_str().unwrap_or("");
            let (prompt, images) = parse_agy_user_input_content(raw);
            if prompt.trim().is_empty() && images.is_empty() {
                continue;
            }
            turns.push(AgyTurn {
                index: turns.len() + 1,
                prompt,
                images,
                prompt_timestamp: parse_rfc3339_ms(val["created_at"].as_str()),
                response: None,
                response_timestamp: None,
                model: None,
            });
            continue;
        }
        if step_type == "PLANNER_RESPONSE" && source == "MODEL" {
            let has_tool_calls = val["tool_calls"]
                .as_array()
                .is_some_and(|arr| !arr.is_empty());
            if has_tool_calls {
                continue;
            }
            let Some(content) = val["content"]
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            else {
                continue;
            };
            if let Some(last) = turns.last_mut() {
                last.response = Some(content.to_string());
                let ts = parse_rfc3339_ms(val["created_at"].as_str());
                last.response_timestamp = (ts > 0).then_some(ts);
                last.model = val["model"]
                    .as_str()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string);
            }
        }
    }
    turns
}

fn agy_transcript_path(cli_dir: &Path, conv_id: &str) -> PathBuf {
    let logs_dir = cli_dir
        .join("brain")
        .join(conv_id)
        .join(".system_generated")
        .join("logs");
    let full = logs_dir.join("transcript_full.jsonl");
    if full.is_file() {
        full
    } else {
        logs_dir.join("transcript.jsonl")
    }
}

fn read_agy_turns(cli_dir: &Path, conv_id: &str) -> Vec<AgyTurn> {
    let mut turns = read_agy_transcript_turns(&agy_transcript_path(cli_dir, conv_id));
    let history = read_agy_user_prompts(&cli_dir.join("history.jsonl"), conv_id);
    if turns.is_empty() {
        return history
            .into_iter()
            .map(|p| AgyTurn {
                index: p.index,
                prompt: p.text,
                images: Vec::new(),
                prompt_timestamp: p.timestamp,
                response: None,
                response_timestamp: None,
                model: None,
            })
            .collect();
    }
    if history.len() > turns.len() {
        for p in history.into_iter().skip(turns.len()) {
            turns.push(AgyTurn {
                index: turns.len() + 1,
                prompt: p.text,
                images: Vec::new(),
                prompt_timestamp: p.timestamp,
                response: None,
                response_timestamp: None,
                model: None,
            });
        }
    }
    turns
}

fn apply_agy_thread_update(
    app: &AppHandle,
    thread_id: &str,
    new_conv_id: Option<&str>,
    new_title: Option<&str>,
    emit_title_generated: bool,
) {
    let state = app.state::<AppState>();
    let mut changed = false;
    let mut title_updated = false;
    {
        let mut store = state.store.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(thread) = store.get_mut(thread_id) {
            if let Some(cid) = new_conv_id {
                if thread.agy_conversation_id.as_deref() != Some(cid) {
                    thread.agy_conversation_id = Some(cid.to_string());
                    changed = true;
                }
            }
            if let Some(title) = new_title {
                if !thread.title_locked && thread.title != title {
                    thread.title = title.to_string();
                    changed = true;
                    title_updated = true;
                }
            }
        }
        if changed {
            store.save();
        }
    }
    if changed {
        if title_updated && emit_title_generated {
            let _ = app.emit(
                crate::acp::EV_TITLE_GENERATED,
                serde_json::json!({ "threadId": thread_id }),
            );
        }
        let _ = app.emit(crate::acp::EV_THREADS, serde_json::json!({}));
    }
}

fn emit_agy_turn_transition(
    app: &AppHandle,
    thread_id: &str,
    running: bool,
    stop_reason: Option<&str>,
) {
    let state = app.state::<AppState>();
    state.sleep_inhibitor.set_running(thread_id, running);
    let title = {
        let mut store = state.store.lock().unwrap_or_else(|e| e.into_inner());
        let title = if let Some(thread) = store.get_mut(thread_id) {
            thread.updated_at = crate::threads::now_ms();
            thread.title.clone()
        } else {
            String::new()
        };
        if !title.is_empty() {
            store.save_thread(thread_id);
        }
        title
    };
    let _ = app.emit(
        crate::acp::EV_TURN,
        serde_json::json!({
            "threadId": thread_id,
            "running": running,
            "stopReason": stop_reason,
        }),
    );
    let _ = app.emit(crate::acp::EV_THREADS, serde_json::json!({}));
    if !running && matches!(stop_reason, Some("end_turn" | "error")) && !title.is_empty() {
        let body = if stop_reason == Some("error") {
            "反重力 CLI 执行出错"
        } else {
            "反重力 CLI 已完成"
        };
        crate::sys_notify::notify_thread_done(
            app,
            thread_id,
            &title,
            body,
            crate::acp::EV_NOTIFY_OPEN,
        );
    }
}

fn spawn_agy_session_watcher(
    app: AppHandle,
    manager: TerminalManager,
    session_id: String,
    session: Arc<Session>,
    thread_id: String,
    log_path: PathBuf,
    state_path: Option<PathBuf>,
    settings_path: Option<PathBuf>,
    cli_dir: Option<PathBuf>,
    initial_conv_id: Option<String>,
    bind_conversation: bool,
) {
    tauri::async_runtime::spawn(async move {
        let mut offset = 0u64;
        let mut carry = Vec::<u8>::new();
        let mut active_conv_id = initial_conv_id;
        let mut prompt_title_applied = false;
        let mut summary_title_applied = false;
        let mut last_state_stamp: Option<(Option<std::time::SystemTime>, u64)> = None;
        let mut last_history_stamp: Option<(Option<std::time::SystemTime>, u64)> = None;
        let mut last_transcript_stamp: Option<(PathBuf, Option<std::time::SystemTime>, u64)> = None;
        let mut last_prompts: Option<Vec<AgyUserPrompt>> = None;
        let mut last_turns: Option<Vec<AgyTurn>> = None;

        loop {
            let closing = session.is_closed();
            if let Some(ref sp) = state_path {
                if let Ok(meta) = std::fs::metadata(sp) {
                    let stamp = (meta.modified().ok(), meta.len());
                    if closing || last_state_stamp.as_ref() != Some(&stamp) {
                        if let Ok(text) = std::fs::read_to_string(sp) {
                            last_state_stamp = Some(stamp);
                            if let Some(snap) = parse_agy_state_snapshot(&text) {
                                if let Some(phase) = snap.phase {
                                    manager.apply_agy_phase(&app, &session_id, &thread_id, phase);
                                }
                                if bind_conversation {
                                    if let Some(cid) = snap.conversation_id.as_deref() {
                                        if active_conv_id.as_deref() != Some(cid) {
                                            active_conv_id = Some(cid.to_string());
                                            prompt_title_applied = false;
                                            summary_title_applied = false;
                                            last_history_stamp = None;
                                            last_transcript_stamp = None;
                                            last_prompts = None;
                                            last_turns = None;
                                            apply_agy_thread_update(
                                                &app,
                                                &thread_id,
                                                Some(cid),
                                                None,
                                                false,
                                            );
                                        }
                                    }
                                    if let Some(title) = snap.title.as_deref() {
                                        if !summary_title_applied {
                                            apply_agy_thread_update(
                                                &app,
                                                &thread_id,
                                                active_conv_id.as_deref(),
                                                Some(title),
                                                true,
                                            );
                                            summary_title_applied = true;
                                            prompt_title_applied = true;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            if bind_conversation {
                if let Ok(mut file) = std::fs::File::open(&log_path) {
                    if let Ok(meta) = file.metadata() {
                        if meta.len() < offset {
                            offset = 0;
                            carry.clear();
                        }
                        if meta.len() > offset && file.seek(SeekFrom::Start(offset)).is_ok() {
                            let mut chunk = Vec::new();
                            if file.read_to_end(&mut chunk).is_ok() && !chunk.is_empty() {
                                offset += chunk.len() as u64;
                                carry.extend_from_slice(&chunk);
                                let complete_end = if closing {
                                    carry.len()
                                } else {
                                    carry
                                        .iter()
                                        .rposition(|&b| b == b'\n')
                                        .map(|i| i + 1)
                                        .unwrap_or(0)
                                };
                                if complete_end > 0 {
                                    let text =
                                        String::from_utf8_lossy(&carry[..complete_end]).into_owned();
                                    carry.drain(..complete_end);
                                    for cid in extract_agy_conversation_ids(&text) {
                                        if active_conv_id.as_deref() != Some(cid.as_str()) {
                                            active_conv_id = Some(cid.clone());
                                            prompt_title_applied = false;
                                            summary_title_applied = false;
                                            last_history_stamp = None;
                                            last_transcript_stamp = None;
                                            last_prompts = None;
                                            last_turns = None;
                                            apply_agy_thread_update(
                                                &app,
                                                &thread_id,
                                                Some(&cid),
                                                None,
                                                false,
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                if let (Some(cid), Some(dir)) = (active_conv_id.as_deref(), cli_dir.as_deref()) {
                    if !summary_title_applied {
                        let annotation_path = dir.join("annotations").join(format!("{cid}.pbtxt"));
                        if let Ok(pbtxt) = std::fs::read_to_string(&annotation_path) {
                            if let Some(title) = read_agy_annotation_title(&pbtxt) {
                                apply_agy_thread_update(
                                    &app,
                                    &thread_id,
                                    Some(cid),
                                    Some(&title),
                                    true,
                                );
                                summary_title_applied = true;
                                prompt_title_applied = true;
                            }
                        }
                        if !summary_title_applied && !prompt_title_applied {
                            let history_path = dir.join("history.jsonl");
                            if let Some(title) = read_agy_first_prompt(&history_path, cid) {
                                apply_agy_thread_update(
                                    &app,
                                    &thread_id,
                                    Some(cid),
                                    Some(&title),
                                    true,
                                );
                                prompt_title_applied = true;
                            }
                        }
                    }
                }
            }

            if let (Some(cid), Some(dir)) = (active_conv_id.as_deref(), cli_dir.as_deref()) {
                let history_path = dir.join("history.jsonl");
                let mut history_changed = false;
                if let Ok(meta) = std::fs::metadata(&history_path) {
                    let stamp = (meta.modified().ok(), meta.len());
                    if closing || last_prompts.is_none() || last_history_stamp.as_ref() != Some(&stamp) {
                        last_history_stamp = Some(stamp);
                        history_changed = true;
                        let prompts = read_agy_user_prompts(&history_path, cid);
                        if last_prompts.as_ref() != Some(&prompts) {
                            let _ = app.emit(
                                EV_TERMINAL_AGY_PROMPTS,
                                serde_json::json!({
                                    "threadId": &thread_id,
                                    "conversationId": cid,
                                    "prompts": &prompts,
                                }),
                            );
                            last_prompts = Some(prompts);
                        }
                    }
                }
                let transcript_path = agy_transcript_path(dir, cid);
                let transcript_stamp = std::fs::metadata(&transcript_path)
                    .ok()
                    .map(|meta| (transcript_path.clone(), meta.modified().ok(), meta.len()));
                if closing
                    || history_changed
                    || last_turns.is_none()
                    || last_transcript_stamp != transcript_stamp
                {
                    last_transcript_stamp = transcript_stamp;
                    let turns = read_agy_turns(dir, cid);
                    if last_turns.as_ref() != Some(&turns) {
                        let _ = app.emit(
                            EV_TERMINAL_AGY_TURNS,
                            serde_json::json!({
                                "threadId": &thread_id,
                                "conversationId": cid,
                                "turns": &turns,
                            }),
                        );
                        last_turns = Some(turns);
                    }
                }
            }

            if closing {
                manager.clear_agy_session(&app, &session_id);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
        let _ = std::fs::remove_file(&log_path);
        if let Some(sp) = state_path {
            let _ = std::fs::remove_file(sp);
        }
        if let Some(sp) = settings_path {
            let _ = std::fs::remove_file(sp);
        }
    });
}

#[cfg(any(windows, test))]
fn terminal_proxy_env(
    server: &str,
    bypass: &str,
    existing_http: Option<&str>,
    existing_https: Option<&str>,
    existing_all: Option<&str>,
    existing_no_proxy: Option<&str>,
) -> HashMap<String, String> {
    if existing_http.is_some_and(|v| !v.trim().is_empty())
        || existing_https.is_some_and(|v| !v.trim().is_empty())
    {
        return HashMap::new();
    }
    let entries: Vec<_> = server
        .split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if entries.is_empty() {
        return HashMap::new();
    }

    let lookup_proxy = |scheme: &str, default_prefix: &str, allow_fallback: bool| {
        let raw = entries
            .iter()
            .find_map(|s| {
                let (protocol, value) = s.split_once('=')?;
                protocol
                    .trim()
                    .eq_ignore_ascii_case(scheme)
                    .then_some(value.trim())
            })
            .or_else(|| {
                allow_fallback
                    .then(|| entries.iter().copied().find(|s| !s.contains('=')))
                    .flatten()
            })
            .filter(|v| !v.is_empty())?;
        Some(if raw.contains("://") {
            raw.to_owned()
        } else {
            format!("{default_prefix}://{raw}")
        })
    };

    let http_url = lookup_proxy("http", "http", true);
    let https_url = lookup_proxy("https", "http", true);
    let socks_url = lookup_proxy("socks", "socks5", false);

    let mut env = HashMap::new();
    if let Some(ref url) = http_url {
        env.insert("HTTP_PROXY".into(), url.clone());
    }
    if let Some(ref url) = https_url {
        env.insert("HTTPS_PROXY".into(), url.clone());
    }
    if existing_all.is_none_or(|v| v.trim().is_empty()) {
        if let Some(all_url) = https_url.or(http_url).or(socks_url) {
            env.insert("ALL_PROXY".into(), all_url);
        }
    }

    if !env.is_empty() {
        let mut rules = Vec::<String>::new();
        let mut push_unique = |item: String| {
            if !item.is_empty() && !rules.contains(&item) {
                rules.push(item);
            }
        };
        if let Some(existing) = existing_no_proxy {
            for host in existing.split(',').map(str::trim) {
                push_unique(host.to_owned());
            }
        }
        for host in bypass.split(';').map(str::trim).filter(|s| !s.is_empty()) {
            if host.eq_ignore_ascii_case("<local>") {
                push_unique("localhost".into());
                push_unique("127.0.0.1".into());
                continue;
            }
            if host.starts_with('<') {
                continue;
            }
            if let Some(prefix) = host.strip_suffix(".*") {
                let mut octets: Vec<_> = prefix.split('.').collect();
                if octets.len() <= 3 && octets.iter().all(|n| n.parse::<u8>().is_ok()) {
                    let bits = octets.len() * 8;
                    octets.resize(4, "0");
                    push_unique(format!("{}/{bits}", octets.join(".")));
                    continue;
                }
            }
            push_unique(host.to_owned());
        }
        if !rules.is_empty() {
            env.insert("NO_PROXY".into(), rules.join(","));
        }
    }
    env
}

fn apply_terminal_env(command: &mut CommandBuilder) {
    command.env("TERM", "xterm-256color");
    command.env("COLORTERM", "truecolor");
    #[cfg(windows)]
    {
        let (enabled, server, bypass, _) = crate::path_env::windows_system_proxy();
        if enabled != 0 {
            let env_var = |upper: &str, lower: &str| {
                std::env::var(upper)
                    .ok()
                    .filter(|v| !v.trim().is_empty())
                    .or_else(|| std::env::var(lower).ok().filter(|v| !v.trim().is_empty()))
            };
            let existing_http = env_var("HTTP_PROXY", "http_proxy");
            let existing_https = env_var("HTTPS_PROXY", "https_proxy");
            let existing_all = env_var("ALL_PROXY", "all_proxy");
            let existing_no_proxy = env_var("NO_PROXY", "no_proxy");
            for (key, value) in terminal_proxy_env(
                &server,
                &bypass,
                existing_http.as_deref(),
                existing_https.as_deref(),
                existing_all.as_deref(),
                existing_no_proxy.as_deref(),
            ) {
                command.env(key, value);
            }
        }
    }
}

fn agy_command_args(
    bypass_permissions: bool,
    model: Option<&str>,
    resume_id: Option<&str>,
    log_path: &Path,
    settings_path: Option<&Path>,
) -> Vec<std::ffi::OsString> {
    let mut args = Vec::new();
    if bypass_permissions {
        args.push("--dangerously-skip-permissions".into());
    }
    if let Some(m) = model.map(str::trim).filter(|m| !m.is_empty()) {
        args.push("--model".into());
        args.push(m.into());
    }
    if let Some(cid) = resume_id.filter(|id| !id.is_empty()) {
        args.push("--resume".into());
        args.push(cid.into());
    }
    args.push("--log-file".into());
    args.push(log_path.as_os_str().to_os_string());
    if let Some(settings) = settings_path {
        args.push("--settings".into());
        args.push(settings.as_os_str().to_os_string());
    }
    args
}

fn agy_command(
    program: &Path,
    cwd: &Path,
    bypass_permissions: bool,
    model: Option<&str>,
    resume_id: Option<&str>,
    log_path: &Path,
    settings_path: Option<&Path>,
    state_path: Option<&Path>,
) -> Result<CommandBuilder, String> {
    if model.is_some_and(|m| m.contains('\0')) {
        return Err("反重力 CLI 模型参数含有无效字符".into());
    }
    if !cwd.is_dir() {
        return Err(format!("终端工作目录不存在：{}", cwd.display()));
    }
    let args = agy_command_args(bypass_permissions, model, resume_id, log_path, settings_path);
    let is_batch = cfg!(windows)
        && program
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"));
    let mut command = if is_batch {
        let comspec = std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".into());
        let mut cmd = CommandBuilder::new(comspec);
        cmd.arg("/C");
        cmd.arg(program.as_os_str());
        cmd.args(args);
        cmd
    } else {
        let mut cmd = CommandBuilder::new(program.as_os_str());
        cmd.args(args);
        cmd
    };
    command.cwd(dunce::simplified(cwd));
    apply_terminal_env(&mut command);
    if let Some(sp) = state_path {
        apply_agy_state_env(&mut command, sp);
    }
    Ok(command)
}

fn shell_command(
    shell: &str,
    args: &[String],
    cwd: &std::path::Path,
) -> Result<CommandBuilder, String> {
    let shell = if shell.trim().is_empty() {
        default_shell_path()
    } else {
        shell.trim().to_owned()
    };
    if shell.contains('\0') || args.iter().any(|arg| arg.contains('\0')) {
        return Err("终端程序或参数含有无效字符".into());
    }
    if !cwd.is_dir() {
        return Err(format!("终端工作目录不存在：{}", cwd.display()));
    }
    let mut command = CommandBuilder::new(shell);
    command.args(args);
    command.cwd(dunce::simplified(cwd));
    apply_terminal_env(&mut command);
    Ok(command)
}
#[cfg(windows)]
fn conpty_build_number() -> Option<u32> {
    use windows_sys::Win32::System::SystemInformation::{GetVersionExW, OSVERSIONINFOW};
    let mut version = OSVERSIONINFOW {
        dwOSVersionInfoSize: std::mem::size_of::<OSVERSIONINFOW>() as u32,
        ..Default::default()
    };
    // The app manifest declares Windows 10+ support so GetVersionExW reports the actual build.
    (unsafe { GetVersionExW(&mut version) } != 0 && version.dwMajorVersion >= 10)
        .then_some(version.dwBuildNumber)
}

fn start<F>(
    manager: TerminalManager,
    id: String,
    thread_id: Option<String>,
    command: CommandBuilder,
    size: PtySize,
    send: F,
) -> Result<(), String>
where
    F: Fn(TerminalEvent) -> Result<(), String> + Send + 'static,
{
    // The channel must configure xterm before any ConPTY output can arrive.
    #[cfg(windows)]
    send(TerminalEvent::WindowsPty {
        build: conpty_build_number(),
    })?;
    let mut sessions = manager.sessions.lock().map_err(|e| e.to_string())?;
    if sessions.contains_key(&id) {
        return Err("终端标识已存在".into());
    }
    if sessions.len() >= MAX_TERMINALS {
        return Err("最多同时运行 32 个终端，请先关闭不用的标签".into());
    }
    let pair = native_pty_system()
        .openpty(size)
        .map_err(|e| format!("创建终端失败：{e}"))?;
    let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
    let writer = pair.master.take_writer().map_err(|e| e.to_string())?;
    let mut child = pair
        .slave
        .spawn_command(command)
        .map_err(|e| format!("启动终端失败，请检查程序和参数：{e}"))?;
    drop(pair.slave);
    let session = Arc::new(Session {
        thread_id,
        master: Mutex::new(Some(pair.master)),
        writer: Mutex::new(writer),
        killer: Mutex::new(child.clone_killer()),
        pid: child.process_id(),
        flow: Mutex::new(Flow::default()),
        ready: Condvar::new(),
    });
    sessions.insert(id.clone(), session.clone());
    drop(sessions);
    let (exit_tx, exit_rx) = std::sync::mpsc::channel();
    let live = session.clone();
    let cleanup = manager.clone();
    let thread_id = id.clone();
    let spawned = std::thread::Builder::new()
        .name("nova-terminal-reader".into())
        .spawn(move || {
            let mut buffer = [0u8; 8192];
            #[cfg(windows)]
            let mut cursor_query = 0usize;
            loop {
                let mut flow = live.flow.lock().unwrap_or_else(|e| e.into_inner());
                while !flow.closed && flow.pending >= OUTPUT_WINDOW {
                    flow = live.ready.wait(flow).unwrap_or_else(|e| e.into_inner());
                }
                drop(flow); // Closed sessions still drain output so ConPTY can finish.
                match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(count) => {
                        // Preserve a partial DSR across reads. During shutdown
                        // the renderer no longer answers, but ConPTY still needs
                        // its inherited cursor response before it can close.
                        #[cfg(windows)]
                        let mut queried_cursor = false;
                        #[cfg(windows)]
                        for &byte in &buffer[..count] {
                            if byte == b"\x1b[6n"[cursor_query] {
                                cursor_query += 1;
                                if cursor_query == 4 {
                                    queried_cursor = true;
                                    cursor_query = 0;
                                }
                            } else {
                                cursor_query = usize::from(byte == 0x1b);
                            }
                        }
                        let mut flow = live.flow.lock().unwrap_or_else(|e| e.into_inner());
                        if flow.closed {
                            drop(flow);
                            #[cfg(windows)]
                            if queried_cursor {
                                live.finish_cursor_handshake();
                            }
                            continue;
                        }
                        flow.pending += count;
                        drop(flow);
                        if send(TerminalEvent::Data {
                            data: buffer[..count].to_vec(),
                        })
                        .is_err()
                        {
                            live.stop();
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) if cfg!(unix) && error.raw_os_error() == Some(5) => break,
                    Err(error)
                        if cfg!(windows) && error.kind() == std::io::ErrorKind::BrokenPipe =>
                    {
                        break
                    }
                    Err(error) => {
                        let _ = send(TerminalEvent::Error {
                            message: error.to_string(),
                        });
                        live.stop();
                        break;
                    }
                }
            }
            drop(reader);
            let code = exit_rx.recv().ok().flatten();
            live.finish();
            if let Ok(mut all) = cleanup.sessions.lock() {
                all.remove(&thread_id);
            }
            let _ = send(TerminalEvent::Exit { code });
        });
    if let Err(error) = spawned {
        session.stop();
        let _ = child.wait();
        manager.close(&id)?;
        return Err(format!("启动终端读取线程失败：{error}"));
    }
    let waiting = session.clone();
    let waiter = std::thread::Builder::new()
        .name("nova-terminal-exit".into())
        .spawn(move || {
            let code = wait_for_exit(child, &waiting);
            let _ = exit_tx.send(code);
        });
    if let Err(error) = waiter {
        session.stop();
        let master = session
            .master
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        drop(master);
        manager.close(&id)?;
        return Err(format!("启动终端退出线程失败：{error}"));
    }
    Ok(())
}
#[tauri::command]
pub async fn terminal_create(
    app: AppHandle,
    webview: Webview,
    state: State<'_, AppState>,
    manager: State<'_, TerminalManager>,
    id: String,
    thread_id: Option<String>,
    cwd: Option<String>,
    cols: u16,
    rows: u16,
    on_event: Channel<TerminalEvent>,
    init_command: Option<String>,
    allow_resume: Option<bool>,
) -> Result<TerminalCreateResult, String> {
    require_main(&webview)?;
    uuid::Uuid::parse_str(&id).map_err(|_| "无效的终端标识")?;
    let size = size(cols, rows)?;
    let cwd = match thread_id.as_deref() {
        Some(id) => crate::workspace_files::root(&state, id)?,
        None => match cwd.filter(|p| !p.trim().is_empty()) {
            Some(path) => PathBuf::from(path),
            None => std::env::current_dir().map_err(|e| e.to_string())?,
        },
    };
    let (thread_config, saved_agy_id) = match thread_id.as_deref() {
        Some(id) => {
            let store = state.store.lock().map_err(|e| e.to_string())?;
            store
                .get(id)
                .map(|thread| (thread.terminal_config.clone(), thread.agy_conversation_id.clone()))
                .unwrap_or((None, None))
        }
        None => (None, None),
    };
    let (shell, args, antigravity_path) = {
        let settings = state.settings.lock().map_err(|e| e.to_string())?;
        let (shell, args) = resolve_shell_and_args(
            &settings.terminal_shell,
            &settings.terminal_args,
            thread_config.as_ref(),
        );
        (shell, args, settings.antigravity_path.clone())
    };
    let is_direct_agy = thread_config
        .as_ref()
        .is_some_and(|cfg| cfg.mode == crate::settings::TerminalMenuMode::Antigravity);
    let bypass_permissions = thread_config
        .as_ref()
        .map(|cfg| cfg.bypass_permissions)
        .unwrap_or(true);
    let agy_model = thread_config
        .as_ref()
        .map(|cfg| cfg.model.trim().to_string())
        .filter(|m| !m.is_empty());
    let cli_dir = agy_cli_dir();
    let has_saved_agy_id = saved_agy_id.is_some();
    let resume_id = if allow_resume.unwrap_or(false) {
        saved_agy_id.filter(|cid| cli_dir.as_deref().is_some_and(|dir| agy_conversation_exists(dir, cid)))
    } else {
        None
    };

    let manager_ref = manager.inner().clone();
    let id_for_spawn = id.clone();
    let thread_id_for_spawn = thread_id.clone();

    if is_direct_agy {
        let configured = antigravity_path.trim();
        let configured = if configured.is_empty() { "agy" } else { configured };
        let program = crate::acp::resolve_program_on_path(configured).ok_or_else(|| {
            format!(
                "未找到 Antigravity CLI（{configured}），请在设置中检查路径或确认 agy 已在 PATH 中"
            )
        })?;
        let log_dir = state.config_dir.join("terminal-agy-logs");
        let _ = std::fs::create_dir_all(&log_dir);
        let log_path = log_dir.join(format!("{id}.log"));
        let settings_path = log_dir.join(format!("{id}.settings.json"));
        let state_path = log_dir.join(format!("{id}.state.json"));
        let (effective_settings, effective_state) = match write_agy_settings_file(&settings_path) {
            Ok(()) => (Some(settings_path), Some(state_path)),
            Err(error) => {
                eprintln!("[workspace-terminal] write agy settings failed: {error}");
                (None, None)
            }
        };
        let log_for_spawn = log_path.clone();
        let settings_for_spawn = effective_settings.clone();
        let state_for_spawn = effective_state.clone();
        let resume_for_spawn = resume_id.clone();
        let model_for_spawn = agy_model.clone();
        tauri::async_runtime::spawn_blocking(move || {
            let command = agy_command(
                &program,
                &cwd,
                bypass_permissions,
                model_for_spawn.as_deref(),
                resume_for_spawn.as_deref(),
                &log_for_spawn,
                settings_for_spawn.as_deref(),
                state_for_spawn.as_deref(),
            )?;
            start(
                manager_ref,
                id_for_spawn,
                thread_id_for_spawn,
                command,
                size,
                move |event| on_event.send(event).map_err(|e| e.to_string()),
            )
        })
        .await
        .map_err(|e| e.to_string())??;

        if let Some(tid) = thread_id {
            let bind_conversation = allow_resume.unwrap_or(false) || !has_saved_agy_id;
            if let Ok(session) = manager.get(&id) {
                spawn_agy_session_watcher(
                    app,
                    manager.inner().clone(),
                    id,
                    session,
                    tid,
                    log_path,
                    effective_state,
                    effective_settings,
                    cli_dir,
                    resume_id,
                    bind_conversation,
                );
            }
        } else {
            if let Some(sp) = effective_settings {
                let _ = std::fs::remove_file(sp);
            }
            if let Some(sp) = effective_state {
                let _ = std::fs::remove_file(sp);
            }
        }
        return Ok(TerminalCreateResult { init_command: None });
    }

    let agy_env_ref = shell_env_ref(&shell, AGY_LOG_ENV_VAR);
    let agy_settings_env_ref = shell_env_ref(&shell, AGY_SETTINGS_ENV_VAR);
    let raw_init = thread_id.as_ref().and(init_command.as_deref());
    let candidate_rewritten = raw_init.and_then(|raw| {
        rewrite_agy_init_command(
            raw,
            &antigravity_path,
            &agy_env_ref,
            Some(&agy_settings_env_ref),
            resume_id.as_deref(),
        )
    });
    let (rewritten_init, agy_log_path, agy_settings_path, agy_state_path) =
        if let Some(rewritten) = candidate_rewritten {
            let log_dir = state.config_dir.join("terminal-agy-logs");
            let _ = std::fs::create_dir_all(&log_dir);
            let log_path = log_dir.join(format!("{id}.log"));
            let settings_path = log_dir.join(format!("{id}.settings.json"));
            let state_path = log_dir.join(format!("{id}.state.json"));
            match write_agy_settings_file(&settings_path) {
                Ok(()) => (
                    Some(rewritten),
                    Some(log_path),
                    Some(settings_path),
                    Some(state_path),
                ),
                Err(error) => {
                    eprintln!("[workspace-terminal] write agy settings failed: {error}");
                    let fallback = raw_init.and_then(|raw| {
                        rewrite_agy_init_command(
                            raw,
                            &antigravity_path,
                            &agy_env_ref,
                            None,
                            resume_id.as_deref(),
                        )
                    });
                    (fallback, Some(log_path), None, None)
                }
            }
        } else {
            (None, None, None, None)
        };
    let log_for_spawn = agy_log_path.clone();
    let settings_for_spawn = agy_settings_path.clone();
    let state_for_spawn = agy_state_path.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let mut command = shell_command(&shell, &args, &cwd)?;
        if let Some(ref log_path) = log_for_spawn {
            command.env(AGY_LOG_ENV_VAR, log_path);
        }
        if let Some(ref settings_path) = settings_for_spawn {
            command.env(AGY_SETTINGS_ENV_VAR, settings_path);
        }
        if let Some(ref state_path) = state_for_spawn {
            apply_agy_state_env(&mut command, state_path);
        }
        start(
            manager_ref,
            id_for_spawn,
            thread_id_for_spawn,
            command,
            size,
            move |event| on_event.send(event).map_err(|e| e.to_string()),
        )
    })
    .await
    .map_err(|e| e.to_string())??;

    if let (Some(tid), Some(log_path)) = (thread_id, agy_log_path) {
        let bind_conversation = allow_resume.unwrap_or(false) || !has_saved_agy_id;
        if let Ok(session) = manager.get(&id) {
            spawn_agy_session_watcher(
                app,
                manager.inner().clone(),
                id,
                session,
                tid,
                log_path,
                agy_state_path,
                agy_settings_path,
                cli_dir,
                resume_id,
                bind_conversation,
            );
        }
    }
    Ok(TerminalCreateResult {
        init_command: rewritten_init,
    })
}
#[tauri::command]
pub async fn terminal_write(
    webview: Webview,
    manager: State<'_, TerminalManager>,
    id: String,
    data: Vec<u8>,
) -> Result<(), String> {
    require_main(&webview)?;
    if data.len() > INPUT_LIMIT {
        return Err("单次终端输入不能超过 64 KiB".into());
    }
    let session = manager.get(&id)?;
    tauri::async_runtime::spawn_blocking(move || {
        let mut writer = session.writer.lock().map_err(|e| e.to_string())?;
        writer
            .write_all(&data)
            .and_then(|_| writer.flush())
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
pub async fn terminal_resize(
    webview: Webview,
    manager: State<'_, TerminalManager>,
    id: String,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    require_main(&webview)?;
    let size = size(cols, rows)?;
    let session = manager.get(&id)?;
    tauri::async_runtime::spawn_blocking(move || {
        session
            .master
            .lock()
            .map_err(|e| e.to_string())?
            .as_ref()
            .ok_or("终端已退出")?
            .resize(size)
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
pub fn terminal_ack(
    webview: Webview,
    manager: State<'_, TerminalManager>,
    id: String,
    bytes: usize,
) -> Result<(), String> {
    require_main(&webview)?;
    if let Ok(session) = manager.get(&id) {
        session.acknowledge(bytes.min(OUTPUT_WINDOW));
    }
    Ok(())
}
#[tauri::command]
pub async fn terminal_close(
    webview: Webview,
    manager: State<'_, TerminalManager>,
    id: String,
) -> Result<(), String> {
    require_main(&webview)?;
    let manager = manager.inner().clone();
    tauri::async_runtime::spawn_blocking(move || manager.close(&id))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn get_agy_terminal_prompts(
    state: State<'_, AppState>,
    thread_id: String,
) -> Result<Vec<AgyUserPrompt>, String> {
    let conv_id = {
        let store = state.store.lock().map_err(|e| e.to_string())?;
        store
            .get(&thread_id)
            .and_then(|thread| thread.agy_conversation_id.clone())
    };
    let Some(cid) = conv_id.filter(|id| !id.trim().is_empty()) else {
        return Ok(Vec::new());
    };
    let Some(dir) = agy_cli_dir() else {
        return Ok(Vec::new());
    };
    Ok(read_agy_user_prompts(&dir.join("history.jsonl"), &cid))
}

#[tauri::command]
pub fn get_agy_terminal_turns(
    state: State<'_, AppState>,
    thread_id: String,
) -> Result<Vec<AgyTurn>, String> {
    let conv_id = {
        let store = state.store.lock().map_err(|e| e.to_string())?;
        store
            .get(&thread_id)
            .and_then(|thread| thread.agy_conversation_id.clone())
    };
    let Some(cid) = conv_id.filter(|id| !id.trim().is_empty()) else {
        return Ok(Vec::new());
    };
    let Some(dir) = agy_cli_dir() else {
        return Ok(Vec::new());
    };
    Ok(read_agy_turns(&dir, &cid))
}

fn ensure_attachment_extension(name: &str, mime_type: &str) -> String {
    let trimmed = name.trim();
    let base = if trimmed.is_empty() {
        "attachment"
    } else {
        trimmed
    };
    if Path::new(base).extension().is_some() {
        return base.to_string();
    }
    let ext = match mime_type.split(';').next().unwrap_or("").trim().to_ascii_lowercase().as_str() {
        "image/png" => Some("png"),
        "image/jpeg" | "image/jpg" => Some("jpg"),
        "image/webp" => Some("webp"),
        "image/gif" => Some("gif"),
        "image/bmp" => Some("bmp"),
        "image/svg+xml" => Some("svg"),
        "application/pdf" => Some("pdf"),
        "text/plain" => Some("txt"),
        "text/markdown" => Some("md"),
        "application/json" => Some("json"),
        _ => None,
    };
    match ext {
        Some(e) => format!("{base}.{e}"),
        None => base.to_string(),
    }
}

#[tauri::command]
pub fn save_terminal_attachments(
    webview: Webview,
    attachments: Vec<crate::threads::PromptImage>,
) -> Result<Vec<String>, String> {
    require_main(&webview)?;
    let mut paths = Vec::with_capacity(attachments.len());
    for mut img in attachments {
        if let Some(uri) = img.uri.as_deref() {
            if let Some(local) = crate::threads::file_uri_to_local_path(uri) {
                if Path::new(&local).exists() {
                    paths.push(local);
                    continue;
                }
            }
        }
        if img.data.is_some() {
            img.name = ensure_attachment_extension(&img.name, &img.mime_type);
            let saved = crate::threads::save_attachment_to_temp(&img)
                .ok_or_else(|| format!("保存附件「{}」到临时文件失败", img.name))?;
            paths.push(saved);
        }
    }
    Ok(paths)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dimensions_and_arguments() {
        assert!(size(80, 24).is_ok());
        assert!(size(0, 24).is_err());
        assert!(size(80, 0).is_err());
        assert!(size(1001, 24).is_err());
        let dir = tempfile::tempdir().unwrap();
        let command = shell_command(
            "custom shell",
            &["a b".into(), "x;echo unsafe".into()],
            dir.path(),
        )
        .unwrap();
        let args: Vec<_> = command
            .get_argv()
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args, ["custom shell", "a b", "x;echo unsafe"]);
        assert!(shell_command("bad\0shell", &[], dir.path()).is_err());
        assert!(shell_command("sh", &[], &dir.path().join("missing")).is_err());

        let default_args = vec!["-NoLogo".into()];
        assert_eq!(
            resolve_shell_and_args("pwsh.exe", &default_args, None),
            ("pwsh.exe".into(), default_args.clone())
        );
        let custom_cmd_only = crate::threads::TerminalSessionConfig {
            init_command: "claude".into(),
            shell: "".into(),
            args: vec![],
            ..Default::default()
        };
        assert_eq!(
            resolve_shell_and_args("pwsh.exe", &default_args, Some(&custom_cmd_only)),
            ("pwsh.exe".into(), default_args.clone())
        );
        let custom_args_only = crate::threads::TerminalSessionConfig {
            init_command: "".into(),
            shell: "".into(),
            args: vec!["-l".into()],
            ..Default::default()
        };
        assert_eq!(
            resolve_shell_and_args("pwsh.exe", &default_args, Some(&custom_args_only)),
            ("pwsh.exe".into(), vec!["-l".into()])
        );
        let custom_shell = crate::threads::TerminalSessionConfig {
            init_command: "dir".into(),
            shell: " cmd.exe ".into(),
            args: vec![],
            ..Default::default()
        };
        assert_eq!(
            resolve_shell_and_args("pwsh.exe", &default_args, Some(&custom_shell)),
            ("cmd.exe".into(), vec![])
        );
    }

    #[test]
    fn agy_command_rewriting_and_metadata_parsing() {
        assert_eq!(shell_env_ref("cmd.exe", AGY_LOG_ENV_VAR), "%NOVA_AGY_LOG_FILE%");
        assert_eq!(shell_env_ref("pwsh.exe", AGY_LOG_ENV_VAR), "$env:NOVA_AGY_LOG_FILE");
        assert_eq!(shell_env_ref("/bin/zsh", AGY_LOG_ENV_VAR), "$NOVA_AGY_LOG_FILE");

        assert_eq!(
            rewrite_agy_init_command(
                "agy",
                "agy",
                "%NOVA_AGY_LOG_FILE%",
                Some("%NOVA_AGY_SETTINGS_FILE%"),
                None
            )
            .as_deref(),
            Some("agy --log-file \"%NOVA_AGY_LOG_FILE%\" --settings \"%NOVA_AGY_SETTINGS_FILE%\"")
        );
        assert_eq!(
            rewrite_agy_init_command(
                "  & \"C:\\Tools\\agy.exe\" --model pro\n",
                "agy",
                "$env:NOVA_AGY_LOG_FILE",
                Some("$env:NOVA_AGY_SETTINGS_FILE"),
                Some("11111111-1111-4111-8111-111111111111")
            )
            .as_deref(),
            Some("  & \"C:\\Tools\\agy.exe\" --resume 11111111-1111-4111-8111-111111111111 --log-file \"$env:NOVA_AGY_LOG_FILE\" --settings \"$env:NOVA_AGY_SETTINGS_FILE\" --model pro\n")
        );
        // Existing resume / continue / log-file / settings flags are respected.
        assert_eq!(
            rewrite_agy_init_command(
                "agy -c --log-file custom.log --settings custom.json",
                "agy",
                "$NOVA_AGY_LOG_FILE",
                Some("$NOVA_AGY_SETTINGS_FILE"),
                Some("11111111-1111-4111-8111-111111111111")
            )
            .as_deref(),
            Some("agy -c --log-file custom.log --settings custom.json")
        );
        // Non-interactive subcommands and print mode are skipped.
        assert_eq!(rewrite_agy_init_command("agy models", "agy", "%NOVA_AGY_LOG_FILE%", Some("%NOVA_AGY_SETTINGS_FILE%"), None), None);
        assert_eq!(rewrite_agy_init_command("agy -p hello", "agy", "%NOVA_AGY_LOG_FILE%", Some("%NOVA_AGY_SETTINGS_FILE%"), None), None);
        assert_eq!(rewrite_agy_init_command("claude", "agy", "%NOVA_AGY_LOG_FILE%", Some("%NOVA_AGY_SETTINGS_FILE%"), None), None);

        let log = "\
I1125 09:55:35.508 server.go:1269] Created conversation 00000000-0000-4000-8000-000000000001\n\
I1125 09:55:54.417 server.go:1883] Sending user message to conversation ad422cca-2fa4-445d-ad92-86f3c81768e1 (items=1)\n\
I1125 09:59:00.196 manager.go:1190] Full redraw completed (RerenderAll) for conversation f26dd82c-b1a5-4bb0-9d1f-5e655ebda1fa (epoch 1)\n";
        assert_eq!(
            extract_agy_conversation_ids(log),
            vec![
                "ad422cca-2fa4-445d-ad92-86f3c81768e1".to_string(),
                "f26dd82c-b1a5-4bb0-9d1f-5e655ebda1fa".to_string(),
            ]
        );

        assert_eq!(
            read_agy_annotation_title("title:\"自定义终端会话菜单调整\"  plan_execute:true").as_deref(),
            Some("自定义终端会话菜单调整")
        );
        assert_eq!(
            read_agy_annotation_title("title:\"Fix \\\"bug\\\" \\344\\270\\255\\346\\226\\207\"").as_deref(),
            Some("Fix \"bug\" 中文")
        );

        let dir = tempfile::tempdir().unwrap();
        let history = dir.path().join("history.jsonl");
        std::fs::write(
            &history,
            concat!(
                "{\"display\":\"/model\",\"conversationId\":\"c1\"}\n",
                "{\"display\":\"  帮我排查一下   这个很长很长很长很长很长很长很长的问题  \",\"conversationId\":\"c1\"}\n",
                "{\"display\":\"第二条消息\",\"conversationId\":\"c1\"}\n"
            ),
        )
        .unwrap();
        assert_eq!(
            read_agy_first_prompt(&history, "c1").as_deref(),
            Some("帮我排查一下 这个很长很长很长很长很长很长很长的问题")
        );
        assert_eq!(read_agy_first_prompt(&history, "missing"), None);
    }

    #[test]
    fn read_agy_user_prompts_filters_conversation_and_slash_commands() {
        let dir = tempfile::tempdir().unwrap();
        let history = dir.path().join("history.jsonl");
        std::fs::write(
            &history,
            concat!(
                "not-json\n",
                "{\"display\":\"/usage\",\"timestamp\":100,\"conversationId\":\"C1\",\"type\":\"slash_command\"}\n",
                "{\"display\":\"/thinking\",\"timestamp\":101,\"conversationId\":\"c1\"}\n",
                "{\"display\":\"   \",\"timestamp\":102,\"conversationId\":\"c1\"}\n",
                "{\"display\":\"第一条消息\\n带换行\",\"timestamp\":103,\"conversationId\":\"C1\"}\n",
                "{\"display\":\"其他会话消息\",\"timestamp\":104,\"conversationId\":\"c2\"}\n",
                "{\"display\":\"  第二条消息  \",\"timestamp\":105,\"conversationId\":\"c1\"}\n"
            ),
        )
        .unwrap();
        assert_eq!(
            read_agy_user_prompts(&history, "c1"),
            vec![
                AgyUserPrompt {
                    index: 1,
                    text: "第一条消息\n带换行".to_string(),
                    timestamp: 103,
                },
                AgyUserPrompt {
                    index: 2,
                    text: "第二条消息".to_string(),
                    timestamp: 105,
                },
            ]
        );
        assert!(read_agy_user_prompts(&history, "missing").is_empty());
        assert!(read_agy_user_prompts(&dir.path().join("missing.jsonl"), "c1").is_empty());
    }

    #[test]
    fn read_agy_transcript_turns_extracts_user_inputs_and_final_conclusions() {
        let (plain_prompt, plain_imgs) = parse_agy_user_input_content("  直接文本  ");
        assert_eq!(plain_prompt, "直接文本");
        assert!(plain_imgs.is_empty());

        let raw_with_uploads = concat!(
            "<USER_REQUEST>\n",
            "第一行问题\n",
            "  第二行缩进\n",
            "</USER_REQUEST>\n",
            "<ADDITIONAL_METADATA>\n",
            "The current local time is: 2026-11-26T14:00:00+08:00.\n\n",
            "The user has uploaded 2 image(s):\n",
            "- C:/tmp/a.png\n",
            "- C:/tmp/b.jpg\n",
            "You can embed this image.\n",
            "</ADDITIONAL_METADATA>"
        );
        let (parsed_prompt, parsed_imgs) = parse_agy_user_input_content(raw_with_uploads);
        assert_eq!(parsed_prompt, "第一行问题\n  第二行缩进");
        assert_eq!(parsed_imgs, vec!["C:/tmp/a.png", "C:/tmp/b.jpg"]);

        let dir = tempfile::tempdir().unwrap();
        let conv_id = "11111111-1111-4111-8111-111111111111";
        let logs_dir = dir
            .path()
            .join("brain")
            .join(conv_id)
            .join(".system_generated")
            .join("logs");
        std::fs::create_dir_all(&logs_dir).unwrap();
        let transcript = logs_dir.join("transcript_full.jsonl");
        std::fs::write(
            &transcript,
            concat!(
                "corrupted-line\n",
                "{\"step_index\":0,\"source\":\"USER_EXPLICIT\",\"type\":\"USER_INPUT\",\"created_at\":\"2026-11-26T06:00:00Z\",\"content\":\"<USER_REQUEST>\\n帮我看下这个问题\\n</USER_REQUEST>\\n<ADDITIONAL_METADATA>\\nThe user has uploaded 1 image(s):\\n- C:/img/1.png\\n</ADDITIONAL_METADATA>\"}\n",
                "{\"step_index\":1,\"source\":\"MODEL\",\"type\":\"PLANNER_RESPONSE\",\"created_at\":\"2026-11-26T06:00:01Z\",\"model\":\"gemini-4-neon-high\",\"thinking\":\"secret\",\"tool_calls\":[{\"name\":\"read_file\"}]}\n",
                "{\"step_index\":2,\"source\":\"MODEL\",\"type\":\"GENERIC\",\"created_at\":\"2026-11-26T06:00:02Z\",\"content\":\"tool output\"}\n",
                "{\"step_index\":3,\"source\":\"MODEL\",\"type\":\"PLANNER_RESPONSE\",\"created_at\":\"2026-11-26T06:00:03Z\",\"model\":\"gemini-4-neon-high\",\"content\":\"阶段播报\"}\n",
                "{\"step_index\":4,\"source\":\"SYSTEM\",\"type\":\"SYSTEM_MESSAGE\",\"created_at\":\"2026-11-26T06:00:04Z\",\"content\":\"sys\"}\n",
                "{\"step_index\":5,\"source\":\"SYSTEM\",\"type\":\"ERROR_MESSAGE\",\"created_at\":\"2026-11-26T06:00:05Z\",\"error\":\"transient\"}\n",
                "{\"step_index\":6,\"source\":\"MODEL\",\"type\":\"PLANNER_RESPONSE\",\"created_at\":\"2026-11-26T06:00:06Z\",\"model\":\"gemini-4-neon-high\",\"content\":\"最终结论：已定位原因。\"}\n",
                "{\"step_index\":7,\"source\":\"USER_EXPLICIT\",\"type\":\"USER_INPUT\",\"created_at\":\"2026-11-26T06:01:00Z\",\"content\":\"<USER_REQUEST>\\n继续修复\\n</USER_REQUEST>\"}\n",
                "{\"step_index\":8,\"source\":\"MODEL\",\"type\":\"PLANNER_RESPONSE\",\"created_at\":\"2026-11-26T06:01:02Z\",\"model\":\"gemini-4-neon-high\",\"content\":\"中途文本\",\"tool_calls\":[{\"name\":\"edit_file\"}]}\n"
            ),
        )
        .unwrap();

        let turns = read_agy_turns(dir.path(), conv_id);
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].index, 1);
        assert_eq!(turns[0].prompt, "帮我看下这个问题");
        assert_eq!(turns[0].images, vec!["C:/img/1.png"]);
        assert_eq!(turns[0].response.as_deref(), Some("最终结论：已定位原因。"));
        assert_eq!(turns[0].model.as_deref(), Some("gemini-4-neon-high"));
        assert!(turns[0].prompt_timestamp > 0);
        assert!(turns[0].response_timestamp.unwrap_or(0) >= turns[0].prompt_timestamp);

        assert_eq!(turns[1].index, 2);
        assert_eq!(turns[1].prompt, "继续修复");
        assert!(turns[1].images.is_empty());
        assert_eq!(turns[1].response, None);

        // Falls back to history.jsonl when transcript does not exist yet.
        let conv_hist = "22222222-2222-4222-8222-222222222222";
        std::fs::write(
            dir.path().join("history.jsonl"),
            format!("{{\"display\":\"首条输入\",\"timestamp\":12345,\"conversationId\":\"{conv_hist}\"}}\n"),
        )
        .unwrap();
        let fallback = read_agy_turns(dir.path(), conv_hist);
        assert_eq!(fallback.len(), 1);
        assert_eq!(fallback[0].prompt, "首条输入");
        assert_eq!(fallback[0].prompt_timestamp, 12345);
        assert_eq!(fallback[0].response, None);

        // Trailing prompt in history.jsonl ahead of transcript_full.jsonl is merged immediately.
        std::fs::write(
            dir.path().join("history.jsonl"),
            format!(
                concat!(
                    "{{\"display\":\"帮我看下这个问题\",\"timestamp\":1000,\"conversationId\":\"{cid}\"}}\n",
                    "{{\"display\":\"继续修复\",\"timestamp\":2000,\"conversationId\":\"{cid}\"}}\n",
                    "{{\"display\":\"第三条刚发出的消息\",\"timestamp\":3000,\"conversationId\":\"{cid}\"}}\n"
                ),
                cid = conv_id
            ),
        )
        .unwrap();
        let merged = read_agy_turns(dir.path(), conv_id);
        assert_eq!(merged.len(), 3);
        assert_eq!(merged[2].index, 3);
        assert_eq!(merged[2].prompt, "第三条刚发出的消息");
        assert_eq!(merged[2].prompt_timestamp, 3000);
        assert_eq!(merged[2].response, None);

        assert_eq!(
            ensure_attachment_extension("粘贴的图片", "image/png"),
            "粘贴的图片.png"
        );
        assert_eq!(
            ensure_attachment_extension("shot.jpg", "image/png"),
            "shot.jpg"
        );
    }

    #[test]
    fn terminal_proxy_env_inherits_windows_proxy_and_respects_overrides() {
        let p = terminal_proxy_env("127.0.0.1:10808", "<local>;10.*;*.internal", None, None, None, None);
        assert_eq!(p["HTTP_PROXY"], "http://127.0.0.1:10808");
        assert_eq!(p["HTTPS_PROXY"], "http://127.0.0.1:10808");
        assert_eq!(p["ALL_PROXY"], "http://127.0.0.1:10808");
        assert_eq!(p["NO_PROXY"], "localhost,127.0.0.1,10.0.0.0/8,*.internal");

        // Respect explicit HTTP_PROXY / HTTPS_PROXY overrides.
        assert!(terminal_proxy_env(
            "127.0.0.1:10808",
            "<local>",
            Some("http://127.0.0.1:7890"),
            None,
            None,
            None,
        )
        .is_empty());

        // Keep existing ALL_PROXY and merge existing NO_PROXY without duplicates or leading commas.
        let merged = terminal_proxy_env(
            "127.0.0.1:10808",
            "<local>",
            None,
            None,
            Some("socks5://127.0.0.1:10809"),
            Some("localhost,example.com"),
        );
        assert!(!merged.contains_key("ALL_PROXY"));
        assert_eq!(merged["NO_PROXY"], "localhost,example.com,127.0.0.1");

        // Dedicated socks= entry maps to ALL_PROXY with socks5:// prefix.
        let socks = terminal_proxy_env("socks=127.0.0.1:1080", "", None, None, None, None);
        assert!(!socks.contains_key("HTTP_PROXY"));
        assert!(!socks.contains_key("HTTPS_PROXY"));
        assert_eq!(socks["ALL_PROXY"], "socks5://127.0.0.1:1080");
        assert!(!socks.contains_key("NO_PROXY"));

        assert!(terminal_proxy_env("", "<local>", None, None, None, None).is_empty());
    }

    #[test]
    fn agy_command_args_builds_bypass_resume_and_log_flags() {
        let log = Path::new("C:\\logs\\session.log");
        let settings = Path::new("C:\\logs\\session.settings.json");
        assert_eq!(
            agy_command_args(true, None, None, log, None),
            vec![
                std::ffi::OsString::from("--dangerously-skip-permissions"),
                std::ffi::OsString::from("--log-file"),
                log.as_os_str().to_os_string(),
            ]
        );
        assert_eq!(
            agy_command_args(false, None, Some("conv-123"), log, Some(settings)),
            vec![
                std::ffi::OsString::from("--resume"),
                std::ffi::OsString::from("conv-123"),
                std::ffi::OsString::from("--log-file"),
                log.as_os_str().to_os_string(),
                std::ffi::OsString::from("--settings"),
                settings.as_os_str().to_os_string(),
            ]
        );
        assert_eq!(
            agy_command_args(true, Some(" gemini-4-neon-high "), Some("conv-123"), log, None),
            vec![
                std::ffi::OsString::from("--dangerously-skip-permissions"),
                std::ffi::OsString::from("--model"),
                std::ffi::OsString::from("gemini-4-neon-high"),
                std::ffi::OsString::from("--resume"),
                std::ffi::OsString::from("conv-123"),
                std::ffi::OsString::from("--log-file"),
                log.as_os_str().to_os_string(),
            ]
        );
        assert_eq!(
            agy_command_args(false, Some("   "), None, log, None),
            vec![
                std::ffi::OsString::from("--log-file"),
                log.as_os_str().to_os_string(),
            ]
        );
    }

    #[test]
    fn parse_agy_state_snapshot_handles_all_phases_and_ignores_startup() {
        assert_eq!(parse_agy_state_snapshot("not json"), None);
        assert_eq!(parse_agy_state_snapshot("[]"), None);

        let startup = parse_agy_state_snapshot(
            r#"{"agent_state":"authenticating","session_id":"","conversation_id":""}"#,
        )
        .unwrap();
        assert_eq!(startup.phase, None);
        assert_eq!(startup.conversation_id, None);
        assert_eq!(startup.title, None);

        let initializing =
            parse_agy_state_snapshot(r#"{"agent_state":"INITIALIZING"}"#).unwrap();
        assert_eq!(initializing.phase, None);

        for (raw, expected) in [
            ("working", AgyAgentPhase::Working),
            ("TOOL_USE", AgyAgentPhase::ToolUse),
            ("reviewing", AgyAgentPhase::Reviewing),
            ("idle", AgyAgentPhase::Idle),
            ("error", AgyAgentPhase::Error),
        ] {
            let parsed =
                parse_agy_state_snapshot(&format!(r#"{{"agent_state":"{raw}"}}"#)).unwrap();
            assert_eq!(parsed.phase, Some(expected));
        }

        let fallback_cid = parse_agy_state_snapshot(
            r#"{"agent_state":"working","session_id":"AD422CCA-2FA4-445D-AD92-86F3C81768E1","conversation_title":"  修复   标题  "}"#,
        )
        .unwrap();
        assert_eq!(fallback_cid.phase, Some(AgyAgentPhase::Working));
        assert_eq!(
            fallback_cid.conversation_id.as_deref(),
            Some("ad422cca-2fa4-445d-ad92-86f3c81768e1")
        );
        assert_eq!(fallback_cid.title.as_deref(), Some("修复 标题"));

        let invalid_cid = parse_agy_state_snapshot(
            r#"{"agent_state":"idle","conversation_id":"not-a-uuid","session_id":"still-invalid"}"#,
        )
        .unwrap();
        assert_eq!(invalid_cid.phase, Some(AgyAgentPhase::Idle));
        assert_eq!(invalid_cid.conversation_id, None);
    }

    #[test]
    fn terminal_manager_tracks_multi_session_agy_running_state() {
        let manager = TerminalManager::default();
        assert!(!manager.is_thread_running("t1"));
        assert_eq!(manager.thread_phase("t1"), None);
        assert_eq!(manager.thread_started_at("t1"), None);

        // Initial idle never reports a running transition.
        assert_eq!(
            manager.set_session_agy_phase("s1", "t1", AgyAgentPhase::Idle),
            (false, false, true)
        );
        assert!(!manager.is_thread_running("t1"));

        // s1 starts working.
        assert_eq!(
            manager.set_session_agy_phase("s1", "t1", AgyAgentPhase::Working),
            (false, true, true)
        );
        assert!(manager.is_thread_running("t1"));
        assert_eq!(manager.thread_phase("t1"), Some(AgyAgentPhase::Working));
        let started_s1 = manager.thread_started_at("t1").expect("started_at set");

        // s2 enters reviewing -> higher priority phase wins, running stays true.
        assert_eq!(
            manager.set_session_agy_phase("s2", "t1", AgyAgentPhase::Reviewing),
            (true, true, true)
        );
        assert_eq!(manager.thread_phase("t1"), Some(AgyAgentPhase::Reviewing));

        // s1 transitions to ToolUse -> preserves its started_at.
        assert_eq!(
            manager.set_session_agy_phase("s1", "t1", AgyAgentPhase::ToolUse),
            (true, true, true)
        );
        assert_eq!(manager.thread_started_at("t1"), Some(started_s1));

        // Removing s2 while s1 is still ToolUse keeps thread running.
        assert_eq!(
            manager.remove_session_agy_state("s2"),
            Some(("t1".to_string(), true, true))
        );
        assert!(manager.is_thread_running("t1"));
        assert_eq!(manager.thread_phase("t1"), Some(AgyAgentPhase::ToolUse));

        // Removing s1 transitions thread to not running.
        assert_eq!(
            manager.remove_session_agy_state("s1"),
            Some(("t1".to_string(), true, false))
        );
        assert!(!manager.is_thread_running("t1"));
        assert_eq!(manager.thread_phase("t1"), None);
        assert_eq!(manager.thread_started_at("t1"), None);
    }
    fn collect_events(
        manager: &TerminalManager,
        id: &str,
        rx: &std::sync::mpsc::Receiver<TerminalEvent>,
    ) -> (String, Option<u32>) {
        let mut output = Vec::new();
        loop {
            let event = rx.recv_timeout(std::time::Duration::from_secs(20));
            if event.is_err() {
                if let Ok(session) = manager.get(id) {
                    let flow = session.flow.lock().unwrap();
                    eprintln!(
                        "PTY timeout: closed={}, reaped={}, output={:?}",
                        flow.closed,
                        flow.reaped,
                        String::from_utf8_lossy(&output)
                    );
                }
                manager.close(id).unwrap();
            }
            match event.expect("terminal did not exit and release its PTY") {
                #[cfg(windows)]
                TerminalEvent::WindowsPty { .. } => assert!(output.is_empty()),
                TerminalEvent::Data { data } => {
                    if let Ok(session) = manager.get(id) {
                        session.acknowledge(data.len());
                    }
                    let previous_len = output.len();
                    output.extend(data);
                    // ConPTY requests the inherited cursor position. xterm answers
                    // this in the app; a read-only collector would stall cmd.exe.
                    // Retain the tail to handle queries split across PTY reads.
                    let queries = output[previous_len.saturating_sub(3)..]
                        .windows(4)
                        .filter(|bytes| *bytes == b"\x1b[6n")
                        .count();
                    if queries > 0 {
                        if let Ok(session) = manager.get(id) {
                            let mut writer = session.writer.lock().unwrap();
                            for _ in 0..queries {
                                writer.write_all(b"\x1b[1;1R").unwrap();
                            }
                            writer.flush().unwrap();
                        }
                    }
                }
                TerminalEvent::Error { message } => panic!("{message}"),
                TerminalEvent::Exit { code } => {
                    return (String::from_utf8_lossy(&output).into_owned(), code)
                }
            }
        }
    }
    #[test]
    fn real_pty_output_resize_and_exit() {
        let dir = tempfile::tempdir().unwrap();
        let (shell, args) = if cfg!(windows) {
            (
                "cmd.exe",
                vec!["/D".into(), "/C".into(), "echo nova-pty-probe".into()],
            )
        } else {
            (
                "/bin/sh",
                vec!["-c".into(), "printf 'nova-pty-probe\\n'; pwd".into()],
            )
        };
        let pair = native_pty_system().openpty(size(80, 24).unwrap()).unwrap();
        pair.master.resize(size(100, 30).unwrap()).unwrap();
        assert_eq!(pair.master.get_size().unwrap().cols, 100);
        drop(pair);
        let manager = TerminalManager::default();
        let id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = std::sync::mpsc::channel();
        start(
            manager.clone(),
            id.clone(),
            None,
            shell_command(shell, &args, dir.path()).unwrap(),
            size(80, 24).unwrap(),
            move |event| tx.send(event).map_err(|e| e.to_string()),
        )
        .unwrap();
        #[cfg(windows)]
        assert!(matches!(
            rx.try_recv(),
            Ok(TerminalEvent::WindowsPty { .. })
        ));
        let (output, code) = collect_events(&manager, &id, &rx);
        assert!(output.contains("nova-pty-probe"), "{output:?}");
        assert_eq!(code, Some(0));
        assert!(manager.get(&id).is_err());
        if cfg!(unix) {
            assert!(output.contains(dir.path().to_str().unwrap()), "{output:?}");
        }
    }
    #[test]
    fn closing_a_running_terminal_releases_the_process_and_reader() {
        let dir = tempfile::tempdir().unwrap();
        let shell = if cfg!(windows) { "cmd.exe" } else { "/bin/sh" };
        let manager = TerminalManager::default();
        let id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = std::sync::mpsc::channel();
        start(
            manager.clone(),
            id.clone(),
            None,
            shell_command(shell, &[], dir.path()).unwrap(),
            size(80, 24).unwrap(),
            move |event| tx.send(event).map_err(|e| e.to_string()),
        )
        .unwrap();
        let session = manager.get(&id).unwrap();
        session
            .master
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .resize(size(100, 30).unwrap())
            .unwrap();
        manager.close(&id).unwrap();
        let _ = collect_events(&manager, &id, &rx);
        assert!(session.flow.lock().unwrap().reaped);
        assert!(manager.get(&id).is_err());
    }
    #[test]
    fn closing_all_during_startup_releases_every_reader() {
        let dir = tempfile::tempdir().unwrap();
        let shell = if cfg!(windows) { "cmd.exe" } else { "/bin/sh" };
        for _ in 0..8 {
            let manager = TerminalManager::default();
            let id = uuid::Uuid::new_v4().to_string();
            let (tx, rx) = std::sync::mpsc::channel();
            start(
                manager.clone(),
                id.clone(),
                None,
                shell_command(shell, &[], dir.path()).unwrap(),
                size(80, 24).unwrap(),
                move |event| tx.send(event).map_err(|e| e.to_string()),
            )
            .unwrap();
            let session = manager.get(&id).unwrap();
            manager.close_all();
            let _ = collect_events(&manager, &id, &rx);
            assert!(session.flow.lock().unwrap().reaped);
            assert!(manager.get(&id).is_err());
        }
    }
}
