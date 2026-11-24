//! `agy acp` 子进程启动、隔离环境准备、16 KiB stderr 环形缓冲与敏感信息脱敏、进程树清理。

use super::protocol::{AcpError, AcpErrorKind};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

/// 隔离 SQLite 会话库目录名，避免污染用户终端手跑的 `antigravity-cli`。
pub const DEFAULT_APP_DATA_DIR: &str = "antigravity-acp";
/// stderr 环形缓冲硬上限（16 KiB）。
pub const STDERR_RING_LIMIT: usize = 16 * 1024;
/// 报错附带的 stderr 尾部截断长度（4 KiB，保留最新崩溃栈）。
pub const STDERR_TAIL_LIMIT: usize = 4 * 1024;
/// 优雅终止等待宽限（ms），超时升级 SIGKILL。
pub const KILL_GRACE_MS: u64 = 1_500;

const LOOPBACK_NO_PROXY: &[&str] = &["localhost", "127.0.0.1", "::1"];

#[cfg(unix)]
mod posix {
    pub const SIGTERM: i32 = 15;
    pub const SIGKILL: i32 = 9;
    extern "C" {
        pub fn kill(pid: i32, sig: i32) -> i32;
    }
}

/// 构造指定数据目录的 `agy acp` 启动参数。
pub fn default_agy_args_with_dir(
    app_data_dir: &str,
    dangerously_skip_permissions: bool,
) -> Vec<String> {
    let dir = app_data_dir.trim();
    let dir = if dir.is_empty() {
        DEFAULT_APP_DATA_DIR
    } else {
        dir
    };
    let mut args = vec!["acp".to_string(), format!("--app_data_dir={dir}")];
    if dangerously_skip_permissions {
        args.push("--dangerously-skip-permissions".to_string());
    }
    args
}

/// 构造默认 `agy acp` 启动参数。
pub fn default_agy_args(dangerously_skip_permissions: bool) -> Vec<String> {
    default_agy_args_with_dir(DEFAULT_APP_DATA_DIR, dangerously_skip_permissions)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchConfig {
    pub program: String,
    /// 为空时自动使用 `default_agy_args_with_dir(&self.app_data_dir, self.dangerously_skip_permissions)`。
    pub args: Vec<String>,
    pub cwd: PathBuf,
    /// 隔离的 `--app_data_dir` 目录名（按账号区分，默认 `antigravity-acp`）。
    pub app_data_dir: String,
    pub env_overrides: HashMap<String, String>,
    pub dangerously_skip_permissions: bool,
}

impl LaunchConfig {
    pub fn for_agy(program: impl Into<String>, cwd: impl Into<PathBuf>) -> Self {
        Self::for_agy_with_dir(program, cwd, DEFAULT_APP_DATA_DIR)
    }

    pub fn for_agy_with_dir(
        program: impl Into<String>,
        cwd: impl Into<PathBuf>,
        app_data_dir: impl Into<String>,
    ) -> Self {
        let dir = app_data_dir.into();
        let app_data_dir = if dir.trim().is_empty() {
            DEFAULT_APP_DATA_DIR.to_string()
        } else {
            dir.trim().to_string()
        };
        Self {
            program: program.into(),
            args: Vec::new(),
            cwd: cwd.into(),
            app_data_dir,
            env_overrides: HashMap::new(),
            dangerously_skip_permissions: true,
        }
    }

    pub fn effective_args(&self) -> Vec<String> {
        if self.args.is_empty() {
            default_agy_args_with_dir(&self.app_data_dir, self.dangerously_skip_permissions)
        } else {
            self.args.clone()
        }
    }
}

/// 向 `NO_PROXY` / `no_proxy` 值追加本地回环地址（保留已有项并去重）。
pub fn append_loopback_no_proxy(existing: Option<&str>) -> String {
    let mut entries: Vec<String> = existing
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    for host in LOOPBACK_NO_PROXY {
        if !entries.iter().any(|e| e.eq_ignore_ascii_case(host)) {
            entries.push((*host).to_string());
        }
    }
    entries.join(",")
}

/// 探测常见系统 CA 证书包路径（用于容器/Nix 环境补齐 `SSL_CERT_FILE`）。
pub fn detect_ssl_cert_file(env: &HashMap<String, String>) -> Option<String> {
    if let Some(existing) = env.get("SSL_CERT_FILE").filter(|v| !v.trim().is_empty()) {
        return Some(existing.clone());
    }
    if let Some(nix) = env.get("NIX_SSL_CERT_FILE").filter(|v| !v.trim().is_empty()) {
        if Path::new(nix).is_file() {
            return Some(nix.clone());
        }
    }
    const CANDIDATES: &[&str] = &[
        "/etc/ssl/certs/ca-certificates.crt",
        "/etc/pki/tls/certs/ca-bundle.crt",
        "/etc/ssl/ca-bundle.pem",
        "/etc/ssl/cert.pem",
    ];
    CANDIDATES
        .iter()
        .find(|p| Path::new(p).is_file())
        .map(|p| (*p).to_string())
}

/// 准备子进程环境变量增量：
/// 1. 向 `NO_PROXY` / `no_proxy` 追加 `localhost,127.0.0.1,::1`；
/// 2. Linux/Nix 自动补齐 `SSL_CERT_FILE`；
/// 3. macOS 若文件存储存在 `refresh_token` 且未显式设置则视情况补齐 `AGY_ACP_FORCE_FILE_STORAGE=1`；
/// 4. 叠加调用方 `env_overrides`。
pub fn prepare_env_patch(
    base_env: &HashMap<String, String>,
    overrides: &HashMap<String, String>,
) -> HashMap<String, String> {
    let mut merged = base_env.clone();
    for (k, v) in overrides {
        merged.insert(k.clone(), v.clone());
    }

    let mut patch = overrides.clone();
    let existing_no_proxy = merged
        .get("NO_PROXY")
        .or_else(|| merged.get("no_proxy"))
        .map(String::as_str);
    let no_proxy_val = append_loopback_no_proxy(existing_no_proxy);
    patch.insert("NO_PROXY".into(), no_proxy_val.clone());
    patch.insert("no_proxy".into(), no_proxy_val);

    #[cfg(unix)]
    if !merged
        .get("SSL_CERT_FILE")
        .is_some_and(|v| !v.trim().is_empty())
    {
        if let Some(cert) = detect_ssl_cert_file(&merged) {
            patch.insert("SSL_CERT_FILE".into(), cert);
        }
    }

    #[cfg(target_os = "macos")]
    if !merged.contains_key("AGY_ACP_FORCE_FILE_STORAGE") {
        if should_force_macos_file_storage(&merged) {
            patch.insert("AGY_ACP_FORCE_FILE_STORAGE".into(), "1".into());
        }
    }

    patch
}

#[cfg(target_os = "macos")]
fn should_force_macos_file_storage(env: &HashMap<String, String>) -> bool {
    let home = env
        .get("GEMINI_HOME")
        .map(PathBuf::from)
        .or_else(|| env.get("HOME").map(|h| Path::new(h).join(".gemini")));
    let Some(token_path) = home.map(|g| g.join(DEFAULT_APP_DATA_DIR).join("acp_token.json")) else {
        return false;
    };
    let Ok(raw) = std::fs::read_to_string(&token_path) else {
        return false;
    };
    if !raw.contains("refresh_token") {
        return false;
    }
    // 若 Keychain 中不存在对应条目，则强制回退文件凭据存储
    let status = std::process::Command::new("security")
        .args(["find-generic-password", "-s", "gemini/antigravity-acp"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    matches!(status, Ok(s) if !s.success())
}

/// 有界 stderr 环形缓冲：超限时仅保留最新尾部字节（默认 16 KiB）。
#[derive(Debug)]
pub struct StderrRing {
    buf: Vec<u8>,
    limit: usize,
}

impl Default for StderrRing {
    fn default() -> Self {
        Self::new(STDERR_RING_LIMIT)
    }
}

impl StderrRing {
    pub fn new(limit: usize) -> Self {
        Self {
            buf: Vec::new(),
            limit: limit.max(1),
        }
    }

    pub fn push_bytes(&mut self, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        if data.len() >= self.limit {
            self.buf.clear();
            self.buf
                .extend_from_slice(&data[data.len() - self.limit..]);
            return;
        }
        let total = self.buf.len() + data.len();
        if total > self.limit {
            self.buf.drain(..total - self.limit);
        }
        self.buf.extend_from_slice(data);
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// 取最新 `STDERR_TAIL_LIMIT`（4 KiB）字节并完成凭据脱敏。
    pub fn tail_redacted(&self) -> String {
        self.tail_redacted_with_limit(STDERR_TAIL_LIMIT)
    }

    pub fn tail_redacted_with_limit(&self, max_bytes: usize) -> String {
        if self.buf.is_empty() {
            return String::new();
        }
        let slice = if self.buf.len() > max_bytes {
            &self.buf[self.buf.len() - max_bytes..]
        } else {
            &self.buf
        };
        let text = String::from_utf8_lossy(slice);
        redact_stderr(&text)
    }
}

/// 脱敏 stderr 文本中的 URL 凭据、API Key / OAuth token 与认证头。
pub fn redact_stderr(input: &str) -> String {
    let mut out = redact_url_credentials(input);
    out = redact_token_prefixes(&out);
    out = redact_header_lines(&out);
    out = redact_kv_secrets(&out);
    out
}

fn is_token_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'/' | b'+' | b'=')
}

fn redact_url_credentials(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(pos) = rest.find("://") {
        let after_scheme = &rest[pos + 3..];
        let auth_end = after_scheme
            .find(|c: char| matches!(c, '/' | '?' | '#' | ' ' | '\t' | '\r' | '\n' | '"' | '\''))
            .unwrap_or(after_scheme.len());
        let authority = &after_scheme[..auth_end];
        if let Some(at) = authority.rfind('@') {
            out.push_str(&rest[..pos + 3]);
            out.push_str("[REDACTED]@");
            out.push_str(&authority[at + 1..]);
            rest = &after_scheme[auth_end..];
        } else {
            out.push_str(&rest[..pos + 3]);
            rest = after_scheme;
        }
    }
    out.push_str(rest);
    out
}

fn redact_token_prefixes(input: &str) -> String {
    const PREFIXES: &[(&str, usize)] = &[
        ("AIza", 12),
        ("ya29.", 12),
        ("1//", 12),
        ("sk-", 12),
    ];
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while i < bytes.len() {
        let mut matched = false;
        for &(prefix, min_len) in PREFIXES {
            if bytes[i..].starts_with(prefix.as_bytes()) {
                let mut j = i + prefix.len();
                while j < bytes.len() && is_token_char(bytes[j]) {
                    j += 1;
                }
                if j - i >= min_len {
                    out.push_str("[REDACTED]");
                    i = j;
                    matched = true;
                    break;
                }
            }
        }
        if !matched {
            let ch = input[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

fn redact_header_lines(input: &str) -> String {
    let mut lines = Vec::new();
    for line in input.split_inclusive('\n') {
        let (body, nl) = match line.strip_suffix('\n') {
            Some(b) => (b, "\n"),
            None => (line, ""),
        };
        let lower = body.to_ascii_lowercase();
        if let Some(idx) = lower.find("authorization:").or_else(|| lower.find("cookie:")) {
            let colon = idx + body[idx..].find(':').unwrap();
            lines.push(format!("{}: [REDACTED]{nl}", &body[..colon]));
        } else if let Some(idx) = lower.find("bearer ") {
            let mut end = idx + "bearer ".len();
            let b = body.as_bytes();
            while end < b.len() && is_token_char(b[end]) {
                end += 1;
            }
            lines.push(format!("{}Bearer [REDACTED]{}{nl}", &body[..idx], &body[end..]));
        } else {
            lines.push(line.to_string());
        }
    }
    lines.concat()
}

fn redact_kv_secrets(input: &str) -> String {
    const KEYS: &[&str] = &[
        "access_token",
        "refresh_token",
        "id_token",
        "api_key",
        "apikey",
        "token",
    ];
    let mut out = String::with_capacity(input.len());
    let lower = input.to_ascii_lowercase();
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let mut hit = None;
        for &key in KEYS {
            if lower[i..].starts_with(key) {
                let prev_ok = i == 0 || !bytes[i - 1].is_ascii_alphanumeric();
                let after = i + key.len();
                let next_ok = after < bytes.len()
                    && matches!(bytes[after], b'"' | b'\'' | b':' | b'=' | b' ');
                if prev_ok && next_ok {
                    hit = Some(after);
                    break;
                }
            }
        }
        if let Some(mut j) = hit {
            let start = i;
            if j < bytes.len() && matches!(bytes[j], b'"' | b'\'') {
                j += 1;
            }
            while j < bytes.len() && bytes[j] == b' ' {
                j += 1;
            }
            if j < bytes.len() && matches!(bytes[j], b':' | b'=') {
                j += 1;
                while j < bytes.len() && bytes[j] == b' ' {
                    j += 1;
                }
                if j < bytes.len() && matches!(bytes[j], b'"' | b'\'') {
                    let quote = bytes[j];
                    let val_start = j + 1;
                    let mut k = val_start;
                    while k < bytes.len() && bytes[k] != quote {
                        k += 1;
                    }
                    if k < bytes.len() && k > val_start {
                        out.push_str(&input[start..val_start]);
                        out.push_str("[REDACTED]");
                        out.push(quote as char);
                        i = k + 1;
                        continue;
                    }
                } else {
                    let val_start = j;
                    let mut k = val_start;
                    while k < bytes.len() && is_token_char(bytes[k]) {
                        k += 1;
                    }
                    if k > val_start {
                        out.push_str(&input[start..val_start]);
                        out.push_str("[REDACTED]");
                        i = k;
                        continue;
                    }
                }
            }
        }
        let ch = input[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// 运行中的 ACP 子进程句柄（持有 Child、PID 与 stderr 环形缓冲）。
pub struct AcpProcess {
    child: Child,
    pid: Option<u32>,
    stderr_ring: Arc<Mutex<StderrRing>>,
    stderr_task: Option<tokio::task::JoinHandle<()>>,
}

impl AcpProcess {
    pub fn spawn_with_hook<F>(
        cfg: &LaunchConfig,
        on_child: F,
    ) -> Result<(Self, ChildStdin, ChildStdout), AcpError>
    where
        F: FnOnce(&Child),
    {
        Self::spawn_with_hook_and_stderr(cfg, on_child, None)
    }

    pub fn spawn_with_hook_and_stderr<F>(
        cfg: &LaunchConfig,
        on_child: F,
        stderr_line_tx: Option<tokio::sync::mpsc::UnboundedSender<String>>,
    ) -> Result<(Self, ChildStdin, ChildStdout), AcpError>
    where
        F: FnOnce(&Child),
    {
        let mut cmd = Command::new(&cfg.program);
        cmd.args(cfg.effective_args())
            .current_dir(&cfg.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let base_env: HashMap<String, String> = std::env::vars().collect();
        let env_patch = prepare_env_patch(&base_env, &cfg.env_overrides);
        cmd.envs(env_patch);

        #[cfg(unix)]
        cmd.process_group(0);
        #[cfg(windows)]
        cmd.creation_flags(0x0800_0000);

        let mut child = cmd.spawn().map_err(|e| {
            AcpError::new(
                AcpErrorKind::Spawn,
                format!("无法启动 Antigravity CLI（{}）：{e}", cfg.program),
            )
        })?;
        on_child(&child);

        let pid = child.id();
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| AcpError::spawn("无法接管子进程 stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| AcpError::spawn("无法接管子进程 stdout"))?;
        let stderr_ring = Arc::new(Mutex::new(StderrRing::default()));
        let stderr_task = child.stderr.take().map(|mut stderr| {
            let ring = stderr_ring.clone();
            tokio::spawn(async move {
                let mut chunk = [0u8; 4096];
                let mut line_buf: Vec<u8> = Vec::new();
                loop {
                    match stderr.read(&mut chunk).await {
                        Ok(0) | Err(_) => {
                            if let Some(ref tx) = stderr_line_tx {
                                if !line_buf.is_empty() {
                                    let tail = String::from_utf8_lossy(&line_buf)
                                        .trim_end_matches('\r')
                                        .to_string();
                                    if !tail.trim().is_empty() {
                                        let _ = tx.send(tail);
                                    }
                                    line_buf.clear();
                                }
                            }
                            break;
                        }
                        Ok(n) => {
                            let bytes = &chunk[..n];
                            if let Ok(mut guard) = ring.lock() {
                                guard.push_bytes(bytes);
                            }
                            if let Some(ref tx) = stderr_line_tx {
                                for &b in bytes {
                                    if b == b'\n' || b == b'\r' {
                                        if !line_buf.is_empty() {
                                            let text =
                                                String::from_utf8_lossy(&line_buf).into_owned();
                                            if !text.trim().is_empty() {
                                                let _ = tx.send(text);
                                            }
                                            line_buf.clear();
                                        }
                                    } else if line_buf.len() < 16 * 1024 {
                                        line_buf.push(b);
                                    }
                                }
                            }
                        }
                    }
                }
            })
        });

        Ok((
            Self {
                child,
                pid,
                stderr_ring,
                stderr_task,
            },
            stdin,
            stdout,
        ))
    }

    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    pub fn stderr_ring(&self) -> Arc<Mutex<StderrRing>> {
        self.stderr_ring.clone()
    }

    pub fn stderr_tail(&self) -> Option<String> {
        let tail = self.stderr_ring.lock().ok()?.tail_redacted();
        (!tail.trim().is_empty()).then_some(tail)
    }

    pub fn try_wait_code(&mut self) -> Option<Option<i32>> {
        self.child.try_wait().ok().flatten().map(|s| s.code())
    }

    pub async fn wait_exit_code(&mut self, timeout: Duration) -> Option<i32> {
        match tokio::time::timeout(timeout, self.child.wait()).await {
            Ok(Ok(status)) => status.code(),
            _ => None,
        }
    }

    /// 优雅终止整棵进程树：POSIX 先向 `-pgid` 发 `SIGTERM`，等待 `grace_ms` 后升级 `SIGKILL`；
    /// Windows 直接终止进程并等待回收。
    pub async fn terminate(&mut self, grace_ms: u64) {
        if self.child.try_wait().ok().flatten().is_some() {
            self.abort_stderr_task();
            return;
        }
        #[cfg(unix)]
        if let Some(pid) = self.pid {
            unsafe {
                let _ = posix::kill(-(pid as i32), posix::SIGTERM);
            }
            if grace_ms > 0 {
                if tokio::time::timeout(Duration::from_millis(grace_ms), self.child.wait())
                    .await
                    .is_ok()
                {
                    self.abort_stderr_task();
                    return;
                }
            }
            unsafe {
                let _ = posix::kill(-(pid as i32), posix::SIGKILL);
            }
        }
        let _ = self.child.start_kill();
        let _ = tokio::time::timeout(Duration::from_millis(500), self.child.wait()).await;
        self.abort_stderr_task();
    }

    fn abort_stderr_task(&mut self) {
        if let Some(handle) = self.stderr_task.take() {
            handle.abort();
        }
    }
}

/// 同步强制清理整个 POSIX 进程组（供 Drop / 紧急取消使用）。
pub fn kill_process_group_force(pid: u32) {
    #[cfg(unix)]
    unsafe {
        let _ = posix::kill(-(pid as i32), posix::SIGKILL);
        let _ = posix::kill(pid as i32, posix::SIGKILL);
    }
    #[cfg(not(unix))]
    let _ = pid;
}

impl Drop for AcpProcess {
    fn drop(&mut self) {
        self.abort_stderr_task();
        if self.child.try_wait().ok().flatten().is_none() {
            if let Some(pid) = self.pid {
                kill_process_group_force(pid);
            }
            let _ = self.child.start_kill();
        }
    }
}
