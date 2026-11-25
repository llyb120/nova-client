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
#[derive(Clone, Default)]
pub struct TerminalManager {
    sessions: Arc<Mutex<HashMap<String, Arc<Session>>>>,
}
impl TerminalManager {
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
    Some(format!("{indent}{prefix}{exe_tok}{injected}{tail}"))
}

fn rewrite_agy_init_command(
    raw: &str,
    antigravity_path: &str,
    log_env_ref: &str,
    resume_id: Option<&str>,
) -> Option<String> {
    let mut matched = false;
    let mut lines = Vec::new();
    for line in raw.replace("\r\n", "\n").split('\n') {
        if let Some(updated) = rewrite_agy_line(line, antigravity_path, log_env_ref, resume_id) {
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

fn spawn_agy_session_watcher(
    app: AppHandle,
    session: Arc<Session>,
    thread_id: String,
    log_path: PathBuf,
    cli_dir: Option<PathBuf>,
    initial_conv_id: Option<String>,
) {
    tauri::async_runtime::spawn(async move {
        let mut offset = 0u64;
        let mut carry = Vec::<u8>::new();
        let mut active_conv_id = initial_conv_id;
        let mut prompt_title_applied = false;
        let mut summary_title_applied = false;

        loop {
            let closing = session.is_closed();
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
                                carry.iter().rposition(|&b| b == b'\n').map(|i| i + 1).unwrap_or(0)
                            };
                            if complete_end > 0 {
                                let text = String::from_utf8_lossy(&carry[..complete_end]).into_owned();
                                carry.drain(..complete_end);
                                for cid in extract_agy_conversation_ids(&text) {
                                    if active_conv_id.as_deref() != Some(cid.as_str()) {
                                        active_conv_id = Some(cid.clone());
                                        prompt_title_applied = false;
                                        summary_title_applied = false;
                                        apply_agy_thread_update(&app, &thread_id, Some(&cid), None, false);
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
                            apply_agy_thread_update(&app, &thread_id, Some(cid), Some(&title), true);
                            summary_title_applied = true;
                            prompt_title_applied = true;
                        }
                    }
                    if !summary_title_applied && !prompt_title_applied {
                        let history_path = dir.join("history.jsonl");
                        if let Some(title) = read_agy_first_prompt(&history_path, cid) {
                            apply_agy_thread_update(&app, &thread_id, Some(cid), Some(&title), true);
                            prompt_title_applied = true;
                        }
                    }
                }
            }

            if closing {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
        let _ = std::fs::remove_file(&log_path);
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
    resume_id: Option<&str>,
    log_path: &Path,
) -> Vec<std::ffi::OsString> {
    let mut args = Vec::new();
    if bypass_permissions {
        args.push("--dangerously-skip-permissions".into());
    }
    if let Some(cid) = resume_id.filter(|id| !id.is_empty()) {
        args.push("--resume".into());
        args.push(cid.into());
    }
    args.push("--log-file".into());
    args.push(log_path.as_os_str().to_os_string());
    args
}

fn agy_command(
    program: &Path,
    cwd: &Path,
    bypass_permissions: bool,
    resume_id: Option<&str>,
    log_path: &Path,
) -> Result<CommandBuilder, String> {
    if !cwd.is_dir() {
        return Err(format!("终端工作目录不存在：{}", cwd.display()));
    }
    let args = agy_command_args(bypass_permissions, resume_id, log_path);
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
        let log_for_spawn = log_path.clone();
        let resume_for_spawn = resume_id.clone();
        tauri::async_runtime::spawn_blocking(move || {
            let command = agy_command(
                &program,
                &cwd,
                bypass_permissions,
                resume_for_spawn.as_deref(),
                &log_for_spawn,
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
            if allow_resume.unwrap_or(false) || !has_saved_agy_id {
                if let Ok(session) = manager.get(&id) {
                    spawn_agy_session_watcher(app, session, tid, log_path, cli_dir, resume_id);
                }
            }
        }
        return Ok(TerminalCreateResult { init_command: None });
    }

    let agy_env_ref = shell_env_ref(&shell, AGY_LOG_ENV_VAR);
    let rewritten_init = thread_id
        .as_ref()
        .and(init_command.as_deref())
        .and_then(|raw| rewrite_agy_init_command(raw, &antigravity_path, &agy_env_ref, resume_id.as_deref()));
    let agy_log_path = if rewritten_init.is_some() {
        let log_dir = state.config_dir.join("terminal-agy-logs");
        let _ = std::fs::create_dir_all(&log_dir);
        Some(log_dir.join(format!("{id}.log")))
    } else {
        None
    };
    let log_for_spawn = agy_log_path.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let mut command = shell_command(&shell, &args, &cwd)?;
        if let Some(ref log_path) = log_for_spawn {
            command.env(AGY_LOG_ENV_VAR, log_path);
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
        if allow_resume.unwrap_or(false) || !has_saved_agy_id {
            if let Ok(session) = manager.get(&id) {
                spawn_agy_session_watcher(app, session, tid, log_path, cli_dir, resume_id);
            }
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
            rewrite_agy_init_command("agy", "agy", "%NOVA_AGY_LOG_FILE%", None).as_deref(),
            Some("agy --log-file \"%NOVA_AGY_LOG_FILE%\"")
        );
        assert_eq!(
            rewrite_agy_init_command(
                "  & \"C:\\Tools\\agy.exe\" --model pro\n",
                "agy",
                "$env:NOVA_AGY_LOG_FILE",
                Some("11111111-1111-4111-8111-111111111111")
            )
            .as_deref(),
            Some("  & \"C:\\Tools\\agy.exe\" --resume 11111111-1111-4111-8111-111111111111 --log-file \"$env:NOVA_AGY_LOG_FILE\" --model pro\n")
        );
        // Existing resume / continue / log-file flags are respected.
        assert_eq!(
            rewrite_agy_init_command(
                "agy -c --log-file custom.log",
                "agy",
                "$NOVA_AGY_LOG_FILE",
                Some("11111111-1111-4111-8111-111111111111")
            )
            .as_deref(),
            Some("agy -c --log-file custom.log")
        );
        // Non-interactive subcommands and print mode are skipped.
        assert_eq!(rewrite_agy_init_command("agy models", "agy", "%NOVA_AGY_LOG_FILE%", None), None);
        assert_eq!(rewrite_agy_init_command("agy -p hello", "agy", "%NOVA_AGY_LOG_FILE%", None), None);
        assert_eq!(rewrite_agy_init_command("claude", "agy", "%NOVA_AGY_LOG_FILE%", None), None);

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
        assert_eq!(
            agy_command_args(true, None, log),
            vec![
                std::ffi::OsString::from("--dangerously-skip-permissions"),
                std::ffi::OsString::from("--log-file"),
                log.as_os_str().to_os_string(),
            ]
        );
        assert_eq!(
            agy_command_args(false, Some("conv-123"), log),
            vec![
                std::ffi::OsString::from("--resume"),
                std::ffi::OsString::from("conv-123"),
                std::ffi::OsString::from("--log-file"),
                log.as_os_str().to_os_string(),
            ]
        );
        assert_eq!(
            agy_command_args(true, Some("conv-123"), log),
            vec![
                std::ffi::OsString::from("--dangerously-skip-permissions"),
                std::ffi::OsString::from("--resume"),
                std::ffi::OsString::from("conv-123"),
                std::ffi::OsString::from("--log-file"),
                log.as_os_str().to_os_string(),
            ]
        );
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
