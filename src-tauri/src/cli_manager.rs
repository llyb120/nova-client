use crate::acp::{apply_proxy_env, resolve_program_on_path};
use crate::settings::Settings;
use crate::threads::AgentKind;
use crate::AppState;
use serde::Serialize;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};

pub const EV_CLI_OPERATION_PROGRESS: &str = "cli:operation-progress";
const CANCELLED_ERROR: &str = "CLI 操作已取消";
const MAX_COMMAND_OUTPUT_LEN: usize = 4000;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CliStatus {
    pub agent_kind: String,
    pub cli_name: String,
    pub installed: bool,
    pub version: String,
    pub upgrade_supported: bool,
    pub detail: String,
}

struct CliSpec {
    kind: AgentKind,
    cli_name: &'static str,
    program: String,
    version_args: Vec<String>,
    install_program: String,
    install_args: Vec<String>,
    upgrade_program: String,
    upgrade_args: Vec<String>,
    proxy: String,
}

#[cfg(windows)]
fn powershell_script_installer(url: &str, elevated: bool) -> (String, Vec<String>) {
    use base64::Engine;

    // Devin 脚本最后会运行交互式 `devin setup`，脚本安装必须使用可见的独立 PowerShell。
    let script = format!(
        "$ProgressPreference='SilentlyContinue'; Invoke-RestMethod '{}' | Invoke-Expression",
        url.replace('\'', "''")
    );
    let encoded_script = base64::engine::general_purpose::STANDARD.encode(
        script
            .encode_utf16()
            .flat_map(|unit| unit.to_le_bytes())
            .collect::<Vec<_>>(),
    );
    let run_as = if elevated { " -Verb RunAs" } else { "" };
    (
        "powershell.exe".into(),
        vec![
            "-NoProfile".into(),
            "-NonInteractive".into(),
            "-Command".into(),
            format!(
                "$process = Start-Process -FilePath 'powershell.exe'{run_as} -WindowStyle Normal -Wait -PassThru -ArgumentList @('-NoProfile','-ExecutionPolicy','Bypass','-EncodedCommand','{encoded_script}'); exit $process.ExitCode"
            ),
        ],
    )
}

#[cfg(windows)]
fn script_installer(url: &str) -> (String, Vec<String>) {
    powershell_script_installer(url, false)
}

#[cfg(windows)]
fn elevated_powershell_script_installer(url: &str) -> (String, Vec<String>) {
    powershell_script_installer(url, true)
}

#[cfg(not(windows))]
fn script_installer(url: &str) -> (String, Vec<String>) {
    (
        "sh".into(),
        vec!["-lc".into(), format!("curl -fsSL '{}' | bash", url)],
    )
}

fn npm_installer(package: &str) -> (String, Vec<String>) {
    (
        "npm".into(),
        vec!["install".into(), "-g".into(), package.into()],
    )
}

fn configured_cli_program(configured: &str, expected_names: &[&str], fallback: &str) -> String {
    let name = std::path::Path::new(configured)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(configured)
        .to_ascii_lowercase();
    if expected_names.iter().any(|expected| name == *expected) {
        configured.to_string()
    } else {
        fallback.to_string()
    }
}

fn spec_for(kind: &AgentKind, settings: &Settings) -> CliSpec {
    match kind {
        AgentKind::Devin => {
            let program = configured_cli_program(&settings.devin_path, &["devin"], "devin");
            #[cfg(windows)]
            let (install_program, install_args) =
                script_installer("https://static.devin.ai/cli/setup.ps1");
            #[cfg(not(windows))]
            let (install_program, install_args) =
                script_installer("https://cli.devin.ai/install.sh");
            CliSpec {
                kind: kind.clone(),
                cli_name: "devin-cli",
                program: program.clone(),
                version_args: vec!["--version".into()],
                install_program,
                install_args,
                upgrade_program: program,
                upgrade_args: vec!["update".into()],
                proxy: settings.devin_proxy.clone(),
            }
        }
        AgentKind::Codex => {
            let program = configured_cli_program(&settings.codex_path, &["codex"], "codex");
            let (install_program, install_args) = npm_installer("@openai/codex@latest");
            CliSpec {
                kind: kind.clone(),
                cli_name: "codex-cli",
                program,
                version_args: vec!["--version".into()],
                install_program: install_program.clone(),
                install_args: install_args.clone(),
                // Codex CLI 当前没有自更新子命令，官方 npm 包是 @openai/codex。
                upgrade_program: install_program,
                upgrade_args: install_args,
                proxy: settings.codex_proxy.clone(),
            }
        }
        AgentKind::CodeBuddy => {
            let program = configured_cli_program(
                &settings.codebuddy_path,
                &["codebuddy", "cbc"],
                "codebuddy",
            );
            let (install_program, install_args) =
                npm_installer("@tencent-ai/codebuddy-code@latest");
            CliSpec {
                kind: kind.clone(),
                cli_name: "codebuddy-cli",
                program: program.clone(),
                version_args: vec!["--version".into()],
                install_program,
                install_args,
                upgrade_program: program,
                upgrade_args: vec!["update".into()],
                proxy: settings.codebuddy_proxy.clone(),
            }
        }
        AgentKind::ClaudeCode => {
            let (install_program, install_args) =
                npm_installer("@anthropic-ai/claude-code@latest");
            CliSpec {
                kind: kind.clone(),
                cli_name: "claude-code-cli",
                // 后端通过 ACP 包装器启动，真正需要管理的是独立的 Claude Code CLI。
                program: "claude".into(),
                version_args: vec!["--version".into()],
                install_program,
                install_args,
                upgrade_program: "claude".into(),
                upgrade_args: vec!["update".into()],
                proxy: settings.claudecode_proxy.clone(),
            }
        }
        AgentKind::Cursor => {
            let program = configured_cli_program(
                &settings.cursor_path,
                &["cursor-agent", "agent"],
                "cursor-agent",
            );
            #[cfg(windows)]
            let (install_program, install_args) =
                elevated_powershell_script_installer("https://cursor.com/install?win32=true");
            #[cfg(not(windows))]
            let (install_program, install_args) =
                script_installer("https://cursor.com/install");
            CliSpec {
                kind: kind.clone(),
                cli_name: "cursor-agent-cli",
                program: program.clone(),
                version_args: vec!["--version".into()],
                install_program,
                install_args,
                upgrade_program: program,
                upgrade_args: vec!["update".into()],
                proxy: settings.cursor_proxy.clone(),
            }
        }
        AgentKind::OpenCode => {
            let program =
                configured_cli_program(&settings.opencode_path, &["opencode"], "opencode");
            let (install_program, install_args) = npm_installer("opencode-ai@latest");
            CliSpec {
                kind: kind.clone(),
                cli_name: "opencode-cli",
                program: program.clone(),
                version_args: vec!["--version".into()],
                install_program,
                install_args,
                upgrade_program: program,
                upgrade_args: vec!["upgrade".into()],
                proxy: settings.opencode_proxy.clone(),
            }
        }
    }
}

fn all_specs(settings: &Settings) -> Vec<CliSpec> {
    [
        AgentKind::Devin,
        AgentKind::Codex,
        AgentKind::CodeBuddy,
        AgentKind::ClaudeCode,
        AgentKind::Cursor,
        AgentKind::OpenCode,
    ]
    .iter()
    .map(|kind| spec_for(kind, settings))
    .collect()
}

#[cfg(windows)]
fn build_command(program: &str, args: &[String]) -> tokio::process::Command {
    let resolved = resolve_program_on_path(program);
    let mut cmd = match resolved
        .as_ref()
        .and_then(|p| p.extension())
        .and_then(|e| e.to_str())
    {
        Some(ext) if ext.eq_ignore_ascii_case("exe") => {
            tokio::process::Command::new(resolved.unwrap())
        }
        Some(ext) if ext.eq_ignore_ascii_case("ps1") => {
            let mut cmd = tokio::process::Command::new("powershell.exe");
            cmd.arg("-NoProfile")
                .arg("-ExecutionPolicy")
                .arg("Bypass")
                .arg("-File")
                .arg(resolved.unwrap());
            cmd
        }
        _ => {
            let mut cmd = tokio::process::Command::new("cmd.exe");
            cmd.arg("/D").arg("/S").arg("/C").arg(program);
            cmd
        }
    };
    cmd.args(args);
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    cmd
}

#[cfg(not(windows))]
fn build_command(program: &str, args: &[String]) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args);
    cmd
}

async fn run_command(
    program: &str,
    args: &[String],
    proxy: &str,
    timeout_duration: Duration,
) -> Result<String, String> {
    let mut cmd = build_command(program, args);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    apply_proxy_env(&mut cmd, proxy);
    let output = tokio::time::timeout(timeout_duration, cmd.output())
        .await
        .map_err(|_| format!("{program} 执行超时"))?
        .map_err(|e| format!("无法启动 {program}：{e}"))?;
    let text = command_output(&output.stdout, &output.stderr);
    if output.status.success() {
        Ok(text)
    } else {
        Err(if text.is_empty() {
            format!("{program} 退出码 {:?}", output.status.code())
        } else {
            text
        })
    }
}

fn emit_operation_progress(
    app: &AppHandle,
    operation_id: &str,
    kind: &AgentKind,
    action: &str,
    stage: &str,
    percent: u8,
    message: impl Into<String>,
) {
    let _ = app.emit(
        EV_CLI_OPERATION_PROGRESS,
        serde_json::json!({
            "operationId": operation_id,
            "agentKind": kind,
            "action": action,
            "stage": stage,
            "percent": percent,
            "message": message.into(),
        }),
    );
}

async fn read_progress_stream<R>(
    reader: R,
    app: AppHandle,
    operation_id: String,
    kind: AgentKind,
    action: String,
    percent: Arc<AtomicU8>,
) -> String
where
    R: AsyncRead + Unpin,
{
    let mut lines = BufReader::new(reader).lines();
    let mut output = String::new();
    while let Ok(Some(line)) = lines.next_line().await {
        let line = strip_ansi(line.trim());
        if line.is_empty() {
            continue;
        }
        if output.len() < MAX_COMMAND_OUTPUT_LEN {
            if !output.is_empty() {
                output.push('\n');
            }
            output.extend(
                line.chars()
                    .take(MAX_COMMAND_OUTPUT_LEN.saturating_sub(output.len())),
            );
        }
        emit_operation_progress(
            &app,
            &operation_id,
            &kind,
            &action,
            "running",
            percent.load(Ordering::SeqCst),
            line,
        );
    }
    output
}

async fn terminate_child(child: &mut tokio::process::Child, pid: Option<u32>) {
    if let Some(pid) = pid {
        crate::acp::kill_process_tree(pid);
    }
    let _ = child.start_kill();
    let _ = child.wait().await;
}

async fn run_command_with_progress(
    app: &AppHandle,
    operation_id: &str,
    kind: &AgentKind,
    action: &str,
    program: &str,
    args: &[String],
    proxy: &str,
    timeout_duration: Duration,
    cancelled: Arc<AtomicBool>,
) -> Result<String, String> {
    let mut cmd = build_command(program, args);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);
    apply_proxy_env(&mut cmd, proxy);
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("无法启动 {program}：{e}"))?;
    let pid = child.id();
    let percent = Arc::new(AtomicU8::new(10));
    let stdout_task = child.stdout.take().map(|stdout| {
        tauri::async_runtime::spawn(read_progress_stream(
            stdout,
            app.clone(),
            operation_id.to_string(),
            kind.clone(),
            action.to_string(),
            percent.clone(),
        ))
    });
    let stderr_task = child.stderr.take().map(|stderr| {
        tauri::async_runtime::spawn(read_progress_stream(
            stderr,
            app.clone(),
            operation_id.to_string(),
            kind.clone(),
            action.to_string(),
            percent.clone(),
        ))
    });
    let mut ticker = tokio::time::interval(Duration::from_secs(1));
    let started = tokio::time::Instant::now();

    let status = loop {
        tokio::select! {
            result = child.wait() => break result.map_err(|e| format!("等待 {program} 失败：{e}"))?,
            _ = ticker.tick() => {
                if cancelled.load(Ordering::SeqCst) {
                    terminate_child(&mut child, pid).await;
                    return Err(CANCELLED_ERROR.into());
                }
                if started.elapsed() >= timeout_duration {
                    terminate_child(&mut child, pid).await;
                    return Err(format!("{program} 执行超时"));
                }
                let next = percent.load(Ordering::SeqCst).saturating_add(1).min(85);
                percent.store(next, Ordering::SeqCst);
                emit_operation_progress(
                    app,
                    operation_id,
                    kind,
                    action,
                    "running",
                    next,
                    format!("正在{action} {}…", kind.label()),
                );
            }
        }
    };

    let stdout = match stdout_task {
        Some(task) => task.await.unwrap_or_default(),
        None => String::new(),
    };
    let stderr = match stderr_task {
        Some(task) => task.await.unwrap_or_default(),
        None => String::new(),
    };
    let text = [stdout.trim(), stderr.trim()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if status.success() {
        Ok(text)
    } else if text.is_empty() {
        Err(format!("{program} 退出码 {:?}", status.code()))
    } else {
        Err(text)
    }
}

fn command_output(stdout: &[u8], stderr: &[u8]) -> String {
    let stdout = strip_ansi(&String::from_utf8_lossy(stdout));
    let stderr = strip_ansi(&String::from_utf8_lossy(stderr));
    let joined = [stdout.trim(), stderr.trim()]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    joined.chars().take(MAX_COMMAND_OUTPUT_LEN).collect()
}

fn strip_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for c in chars.by_ref() {
                if ('@'..='~').contains(&c) {
                    break;
                }
            }
        } else {
            out.push(ch);
        }
    }
    out
}

async fn status_for(spec: CliSpec) -> CliStatus {
    let available = resolve_program_on_path(&spec.program).is_some();
    if !available {
        let install_available = resolve_program_on_path(&spec.install_program).is_some();
        return CliStatus {
            agent_kind: spec.kind.as_str().into(),
            cli_name: spec.cli_name.into(),
            installed: false,
            version: "未安装".into(),
            upgrade_supported: install_available,
            detail: if install_available {
                format!("未找到 {}，可一键安装", spec.program)
            } else {
                format!(
                    "未找到 {}，且安装程序 {} 不可用",
                    spec.program, spec.install_program
                )
            },
        };
    }
    let upgrade_available = resolve_program_on_path(&spec.upgrade_program).is_some();
    match run_command(
        &spec.program,
        &spec.version_args,
        &spec.proxy,
        Duration::from_secs(30),
    )
    .await
    {
        Ok(version) => CliStatus {
            agent_kind: spec.kind.as_str().into(),
            cli_name: spec.cli_name.into(),
            installed: true,
            version: version.lines().next().unwrap_or("未知版本").trim().into(),
            upgrade_supported: upgrade_available,
            detail: if upgrade_available {
                String::new()
            } else {
                format!("未找到升级程序 {}", spec.upgrade_program)
            },
        },
        Err(error) => CliStatus {
            agent_kind: spec.kind.as_str().into(),
            cli_name: spec.cli_name.into(),
            installed: true,
            version: "版本读取失败".into(),
            upgrade_supported: upgrade_available,
            detail: if upgrade_available {
                error
            } else {
                format!("{error}\n未找到升级程序 {}", spec.upgrade_program)
            },
        },
    }
}

pub async fn statuses(settings: &Settings) -> Vec<CliStatus> {
    let tasks = all_specs(settings)
        .into_iter()
        .map(|spec| tauri::async_runtime::spawn(status_for(spec)))
        .collect::<Vec<_>>();
    let mut result = Vec::with_capacity(tasks.len());
    for task in tasks {
        if let Ok(status) = task.await {
            result.push(status);
        }
    }
    result
}

async fn stop_backend(state: &AppState, kind: &AgentKind) {
    match kind {
        AgentKind::Codex => state.codex.restart().await,
        _ => {
            if let Some(manager) = state.acp_for(kind) {
                manager.restart().await;
            }
        }
    }
}

#[cfg(windows)]
fn devin_process_running() -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };

    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return true;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut found = false;
        if Process32FirstW(snapshot, &mut entry) != 0 {
            loop {
                let len = entry
                    .szExeFile
                    .iter()
                    .position(|c| *c == 0)
                    .unwrap_or(entry.szExeFile.len());
                let name = String::from_utf16_lossy(&entry.szExeFile[..len]);
                if name.eq_ignore_ascii_case("devin.exe") {
                    found = true;
                    break;
                }
                if Process32NextW(snapshot, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snapshot);
        found
    }
}

#[cfg(not(windows))]
fn devin_process_running() -> bool {
    std::process::Command::new("pgrep")
        .arg("-x")
        .arg("devin")
        .status()
        .map(|status| status.success())
        .unwrap_or(true)
}

pub async fn upgrade(
    app: &AppHandle,
    state: &AppState,
    kind: AgentKind,
    settings: &Settings,
    operation_id: &str,
) -> Result<CliStatus, String> {
    let spec = spec_for(&kind, settings);
    let installed = resolve_program_on_path(&spec.program).is_some();
    let action = if installed { "升级" } else { "安装" };
    let cancelled = Arc::new(AtomicBool::new(false));
    {
        let mut operations = state.cli_operations.lock().unwrap();
        if operations.contains_key(operation_id) {
            return Err("CLI 操作编号重复".into());
        }
        operations.insert(operation_id.to_string(), cancelled.clone());
    }
    emit_operation_progress(
        app,
        operation_id,
        &kind,
        action,
        "waiting",
        0,
        format!("正在准备{action} {}…", kind.label()),
    );

    let result = async {
        let _guard = state.cli_upgrade_lock.lock().await;
        if cancelled.load(Ordering::SeqCst) {
            return Err(CANCELLED_ERROR.into());
        }

        if installed {
            stop_backend(state, &kind).await;
            if kind == AgentKind::Devin && devin_process_running() {
                return Err(
                    "检测到仍在运行的 devin 进程。请先结束 Nova 之外的 Devin 进程再升级。"
                        .into(),
                );
            }
        }

        let (program, args) = if installed {
            (&spec.upgrade_program, &spec.upgrade_args)
        } else {
            (&spec.install_program, &spec.install_args)
        };
        run_command_with_progress(
            app,
            operation_id,
            &kind,
            action,
            program,
            args,
            &spec.proxy,
            Duration::from_secs(15 * 60),
            cancelled.clone(),
        )
        .await
        .map_err(|e| {
            if e == CANCELLED_ERROR {
                e
            } else {
                format!("{} {action}失败：{e}", spec.cli_name)
            }
        })?;

        emit_operation_progress(
            app,
            operation_id,
            &kind,
            action,
            "verifying",
            92,
            format!("{action}命令已完成，正在校验 CLI…"),
        );
        refresh_cli_search_path();
        let status = status_for(spec).await;
        if !status.installed {
            return Err(format!(
                "{} {action}完成，但 Nova 仍未找到可执行文件",
                status.cli_name
            ));
        }
        Ok(status)
    }
    .await;

    state.cli_operations.lock().unwrap().remove(operation_id);
    match &result {
        Ok(status) => emit_operation_progress(
            app,
            operation_id,
            &kind,
            action,
            "completed",
            100,
            format!("{} 已就绪：{}", status.cli_name, status.version),
        ),
        Err(error) if error == CANCELLED_ERROR => emit_operation_progress(
            app,
            operation_id,
            &kind,
            action,
            "cancelled",
            0,
            format!("已取消{action} {}", kind.label()),
        ),
        Err(error) => emit_operation_progress(
            app,
            operation_id,
            &kind,
            action,
            "failed",
            0,
            error,
        ),
    }
    result
}

pub fn cancel(state: &AppState, operation_id: &str) -> bool {
    let Some(cancelled) = state
        .cli_operations
        .lock()
        .unwrap()
        .get(operation_id)
        .cloned()
    else {
        return false;
    };
    cancelled.store(true, Ordering::SeqCst);
    true
}

pub fn is_installed(kind: &AgentKind, settings: &Settings) -> bool {
    let spec = spec_for(kind, settings);
    resolve_program_on_path(&spec.program).is_some()
}

pub async fn ensure_installed(
    app: &AppHandle,
    state: &AppState,
    kind: AgentKind,
    settings: &Settings,
    operation_id: &str,
) -> Result<CliStatus, String> {
    if is_installed(&kind, settings) {
        return Ok(status_for(spec_for(&kind, settings)).await);
    }
    let status = upgrade(app, state, kind, settings, operation_id).await?;
    if !status.installed {
        return Err(format!("{} 安装完成，但 Nova 仍未找到可执行文件", status.cli_name));
    }
    Ok(status)
}

fn refresh_cli_search_path() {
    let Some(current) = std::env::var_os("PATH") else {
        return;
    };
    let mut paths: Vec<std::path::PathBuf> = std::env::split_paths(&current).collect();

    #[cfg(windows)]
    {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            paths.push(std::path::PathBuf::from(appdata).join("npm"));
        }
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            let local = std::path::PathBuf::from(local);
            paths.push(local.join("devin").join("cli").join("bin"));
            paths.push(local.join("codebuddy").join("bin"));
            paths.push(local.join("cursor-agent"));
        }
    }

    #[cfg(not(windows))]
    if let Some(home) = std::env::var_os("HOME") {
        let home = std::path::PathBuf::from(home);
        paths.push(home.join(".local").join("bin"));
        paths.push(home.join(".npm-global").join("bin"));
    }

    let mut seen = std::collections::HashSet::new();
    paths.retain(|path| {
        if !path.is_dir() {
            return false;
        }
        #[cfg(windows)]
        let key = path.to_string_lossy().to_ascii_lowercase();
        #[cfg(not(windows))]
        let key = path.to_string_lossy().to_string();
        seen.insert(key)
    });
    if let Ok(path) = std::env::join_paths(paths) {
        std::env::set_var("PATH", path);
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn windows_script_installers_open_visible_powershell_and_cursor_elevates() {
        let settings = Settings::default();
        let cursor = spec_for(&AgentKind::Cursor, &settings);
        let devin = spec_for(&AgentKind::Devin, &settings);
        let cursor_args = cursor.install_args.join(" ");
        let devin_args = devin.install_args.join(" ");

        assert_eq!(cursor.install_program, "powershell.exe");
        assert_eq!(devin.install_program, "powershell.exe");
        assert!(cursor_args.contains("Start-Process"));
        assert!(devin_args.contains("Start-Process"));
        assert!(cursor_args.contains("-WindowStyle Normal"));
        assert!(devin_args.contains("-WindowStyle Normal"));
        assert!(cursor_args.contains("-Verb RunAs"));
        assert!(cursor_args.contains("-EncodedCommand"));
        assert!(!devin_args.contains("-Verb RunAs"));
        assert!(devin_args.contains("-EncodedCommand"));
        assert!(!cursor_args.contains("-WindowStyle Hidden"));
        assert!(!devin_args.contains("-WindowStyle Hidden"));
    }

    #[test]
    fn cursor_installer_program_with_extension_resolves_from_path() {
        let cursor = spec_for(&AgentKind::Cursor, &Settings::default());

        assert_eq!(cursor.install_program, "powershell.exe");
        assert!(resolve_program_on_path(&cursor.install_program).is_some());
    }
}
