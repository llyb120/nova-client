use crate::acp::{apply_proxy_env, resolve_program_on_path};
use crate::settings::Settings;
use crate::threads::AgentKind;
use serde::Serialize;
use std::process::Stdio;
use std::time::Duration;

const MAX_COMMAND_OUTPUT_LEN: usize = 4000;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CliStatus {
    pub agent_kind: String,
    pub cli_name: String,
    pub installed: bool,
    pub version: String,
    pub install_command: String,
    pub detail: String,
}

struct CliSpec {
    kind: AgentKind,
    cli_name: &'static str,
    program: String,
    version_args: Vec<String>,
    install_command: String,
    proxy: String,
}

#[cfg(windows)]
fn devin_install_command() -> String {
    "powershell -NoProfile -ExecutionPolicy Bypass -Command \"Invoke-RestMethod https://static.devin.ai/cli/setup.ps1 | Invoke-Expression\"".into()
}

#[cfg(not(windows))]
fn devin_install_command() -> String {
    "curl -fsSL https://cli.devin.ai/install.sh | bash".into()
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
        AgentKind::Alkaid => CliSpec {
            kind: kind.clone(),
            cli_name: "alkaid",
            program: "node".into(),
            version_args: vec!["--version".into()],
            install_command: String::new(),
            proxy: String::new(),
        },
        AgentKind::Lyra => CliSpec {
            kind: kind.clone(),
            cli_name: "lyra",
            program: "lyra".into(),
            version_args: vec!["--version".into()],
            install_command: String::new(),
            proxy: String::new(),
        },
        AgentKind::Devin => CliSpec {
            kind: kind.clone(),
            cli_name: "devin-cli",
            program: configured_cli_program(&settings.devin_path, &["devin"], "devin"),
            version_args: vec!["--version".into()],
            install_command: devin_install_command(),
            proxy: settings.devin_proxy.clone(),
        },
        AgentKind::Codex | AgentKind::CodexPlus => CliSpec {
            kind: kind.clone(),
            cli_name: "codex-cli",
            program: configured_cli_program(&settings.codex_path, &["codex"], "codex"),
            version_args: vec!["--version".into()],
            install_command: "npm install -g @openai/codex@latest".into(),
            proxy: settings.codex_proxy.clone(),
        },
        AgentKind::CodeBuddy | AgentKind::CodeBuddyPlus => CliSpec {
            kind: kind.clone(),
            cli_name: "codebuddy-cli",
            program: configured_cli_program(
                &settings.codebuddy_path,
                &["codebuddy", "cbc"],
                "codebuddy",
            ),
            version_args: vec!["--version".into()],
            install_command: "npm install -g @tencent-ai/codebuddy-code@latest".into(),
            proxy: settings.codebuddy_proxy.clone(),
        },
        AgentKind::ClaudeCode => CliSpec {
            kind: kind.clone(),
            cli_name: "claude-code-cli",
            program: "claude".into(),
            version_args: vec!["--version".into()],
            install_command: "npm install -g @anthropic-ai/claude-code@latest".into(),
            proxy: settings.claudecode_proxy.clone(),
        },
        AgentKind::Cursor => CliSpec {
            kind: kind.clone(),
            cli_name: "cursor-sdk",
            program: "node".into(),
            version_args: vec!["--version".into()],
            install_command: String::new(),
            proxy: settings.cursor_proxy.clone(),
        },
        AgentKind::OpenCode | AgentKind::OpenCodePlus => CliSpec {
            kind: kind.clone(),
            cli_name: "opencode-cli",
            program: configured_cli_program(&settings.opencode_path, &["opencode"], "opencode"),
            version_args: vec!["--version".into()],
            install_command: "npm install -g opencode-ai@latest".into(),
            proxy: settings.opencode_proxy.clone(),
        },
    }
}

fn all_specs(settings: &Settings) -> Vec<CliSpec> {
    [
        AgentKind::Devin,
        AgentKind::Codex,
        AgentKind::CodeBuddy,
        AgentKind::ClaudeCode,
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
        _ => {
            let mut cmd = tokio::process::Command::new("cmd.exe");
            cmd.arg("/D").arg("/S").arg("/C").arg(program);
            cmd
        }
    };
    cmd.args(args);
    cmd.creation_flags(0x0800_0000);
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
    } else if text.is_empty() {
        Err(format!("{program} 退出码 {:?}", output.status.code()))
    } else {
        Err(text)
    }
}

fn command_output(stdout: &[u8], stderr: &[u8]) -> String {
    let stdout = strip_ansi(&String::from_utf8_lossy(stdout));
    let stderr = strip_ansi(&String::from_utf8_lossy(stderr));
    [stdout.trim(), stderr.trim()]
        .into_iter()
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
        .chars()
        .take(MAX_COMMAND_OUTPUT_LEN)
        .collect()
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
    if resolve_program_on_path(&spec.program).is_none() {
        return CliStatus {
            agent_kind: spec.kind.as_str().into(),
            cli_name: spec.cli_name.into(),
            installed: false,
            version: "未安装".into(),
            install_command: spec.install_command,
            detail: format!("未找到 {}", spec.program),
        };
    }
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
            install_command: spec.install_command,
            detail: String::new(),
        },
        Err(error) => CliStatus {
            agent_kind: spec.kind.as_str().into(),
            cli_name: spec.cli_name.into(),
            installed: true,
            version: "版本读取失败".into(),
            install_command: spec.install_command,
            detail: error,
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

pub fn install_command(kind: &AgentKind, settings: &Settings) -> String {
    spec_for(kind, settings).install_command
}

pub fn is_installed(kind: &AgentKind, settings: &Settings) -> bool {
    if matches!(
        kind,
        AgentKind::Cursor | AgentKind::Alkaid | AgentKind::Lyra
    ) {
        return true;
    }
    resolve_program_on_path(&spec_for(kind, settings).program).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_backends_expose_manual_install_commands() {
        let settings = Settings::default();
        for kind in [
            AgentKind::Devin,
            AgentKind::Codex,
            AgentKind::CodeBuddy,
            AgentKind::ClaudeCode,
            AgentKind::OpenCode,
        ] {
            assert!(!install_command(&kind, &settings).is_empty());
        }
        assert!(install_command(&AgentKind::CodeBuddy, &settings)
            .contains("@tencent-ai/codebuddy-code@latest"));
    }

    #[test]
    fn cursor_is_sdk_only_and_skips_cli_status() {
        assert!(is_installed(&AgentKind::Cursor, &Settings::default()));
        assert!(!all_specs(&Settings::default())
            .iter()
            .any(|spec| matches!(spec.kind, AgentKind::Cursor)));
    }
}
