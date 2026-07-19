//! Headless server-mode configuration.
//!
//! The agent/runtime implementation is intentionally shared with the desktop app. On Linux the
//! executable creates a private Xvfb display before Tauri starts, while this module makes sure no
//! window is presented and supplies the remote-control settings from command-line arguments.

use crate::settings::Settings;
use crate::threads::ProjectStore;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::path::PathBuf;

const ENV_HEADLESS: &str = "NOVA_HEADLESS";
const ENV_RELAY_SERVER: &str = "NOVA_SERVER_RELAY_URL";
const ENV_TOKEN: &str = "NOVA_SERVER_TOKEN";
const ENV_NAME: &str = "NOVA_SERVER_NAME";
const ENV_PROXY: &str = "NOVA_SERVER_PROXY";
const ENV_PROJECTS: &str = "NOVA_SERVER_PROJECTS";
const ENV_DATA_DIR: &str = "NOVA_DATA_DIR";

#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct ServerFile {
    name: String,
    proxy: String,
    /// Extra environment inherited by agent processes. This is primarily for custom model
    /// provider credentials whose variable name is selected by Codex `env_key`.
    environment: HashMap<String, String>,
}

const HELP: &str = r#"Nova Headless Server

用法：
  Nova server [启动参数]                  启动无界面服务
  Nova server help                        显示帮助
  Nova server project list                列出允许远控的项目
  Nova server project add <目录>          添加项目
  Nova server project remove <目录>       删除项目（不删除文件和会话）
  Nova server config show [--show-token]  查看持久配置
  Nova server config set <键> <值>        更新配置（环境变量使用 env.<NAME>）
  Nova server config unset <键>           清空配置
  Nova server update --check              仅检查 Nova 更新
  Nova server update                      下载并安装最新版本

启动参数：
  --relay-server <URL>   本次运行使用的 Relay/Web 地址
  --token <TOKEN>        本次运行使用的远控 Token
  --name <NAME>          本次运行显示的设备名
  --proxy <URL>          本次运行的远程同步代理
  --project <DIR>        本次运行的项目白名单，可重复
  --data-dir <DIR>       Nova 数据目录（默认 ~/.nova）

可配置键：
  relay-server, token, groups, name, proxy, default-mode
  devin-path, codex-path, codex-args, codebuddy-path, claude-path,
  cursor-path, opencode-path
  devin-proxy, codex-proxy, codebuddy-proxy, claude-proxy,
  cursor-proxy, opencode-proxy
  devin-enabled, codex-enabled, codebuddy-enabled, claude-enabled,
  cursor-enabled, opencode-enabled
  env.<NAME>（例如 config.toml 中自定义 provider 的 env_key）

环境变量：NOVA_SERVER_RELAY_URL、NOVA_SERVER_TOKEN、NOVA_SERVER_NAME、
NOVA_SERVER_PROXY、NOVA_DATA_DIR。命令行/环境变量只覆盖本次运行；config set 会持久化。
"#;

pub fn is_headless() -> bool {
    std::env::var_os(ENV_HEADLESS).is_some()
}

pub fn data_dir_override() -> Option<PathBuf> {
    std::env::var_os(ENV_DATA_DIR)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn data_dir() -> PathBuf {
    if let Some(path) = data_dir_override() {
        return path;
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(crate::nova_data_dir_name())
}

fn server_file_path() -> PathBuf {
    data_dir().join("server.json")
}

fn load_server_file() -> ServerFile {
    std::fs::read_to_string(server_file_path())
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn save_server_file(value: &ServerFile) -> Result<(), String> {
    let path = server_file_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let text = serde_json::to_string_pretty(value).map_err(|error| error.to_string())?;
    std::fs::write(&path, text).map_err(|error| error.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// Execute administrative `Nova server ...` commands without starting Tauri or Xvfb.
pub fn maybe_run_management_command() -> Option<Result<(), String>> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) != Some("server") {
        return None;
    }
    args.remove(0);
    extract_data_dir_arg(&mut args);
    let command = args.first().map(String::as_str)?;
    let result = match command {
        "help" | "--help" | "-h" => {
            println!("{HELP}");
            Ok(())
        }
        "project" | "projects" => manage_projects(&args[1..]),
        "config" => manage_config(&args[1..]),
        "update" => {
            let check_only = args[1..].iter().any(|arg| arg == "--check");
            crate::updater::run_server_update(check_only)
        }
        _ => return None,
    };
    Some(result)
}

fn extract_data_dir_arg(args: &mut Vec<String>) {
    let mut i = 0;
    while i < args.len() {
        if let Some(value) = args[i].strip_prefix("--data-dir=") {
            std::env::set_var(ENV_DATA_DIR, value);
            args.remove(i);
            continue;
        }
        if args[i] == "--data-dir" && i + 1 < args.len() {
            let value = args.remove(i + 1);
            args.remove(i);
            std::env::set_var(ENV_DATA_DIR, value);
            continue;
        }
        i += 1;
    }
}

fn manage_projects(args: &[String]) -> Result<(), String> {
    let dir = data_dir();
    let mut store = ProjectStore::load(&dir);
    match args.first().map(String::as_str) {
        Some("list") => {
            if store.projects.is_empty() {
                println!("（尚未添加项目）");
            } else {
                for project in &store.projects {
                    println!("{project}");
                }
            }
            Ok(())
        }
        Some("add") => {
            let raw = args.get(1).ok_or("用法：Nova server project add <目录>")?;
            let path = std::fs::canonicalize(raw)
                .map_err(|error| format!("项目目录不存在：{raw}（{error}）"))?;
            if !path.is_dir() {
                return Err(format!("不是目录：{}", path.display()));
            }
            let path = path.to_string_lossy().to_string();
            store.touch(&path);
            println!("已添加项目：{path}");
            Ok(())
        }
        Some("remove") | Some("delete") | Some("rm") => {
            let raw = args
                .get(1)
                .ok_or("用法：Nova server project remove <目录>")?;
            let requested = std::fs::canonicalize(raw)
                .unwrap_or_else(|_| PathBuf::from(raw))
                .to_string_lossy()
                .to_string();
            let before = store.projects.len();
            store.projects.retain(|stored| {
                let normalized = std::fs::canonicalize(stored)
                    .unwrap_or_else(|_| PathBuf::from(stored))
                    .to_string_lossy()
                    .to_string();
                stored != raw && normalized != requested
            });
            store.save();
            if store.projects.len() == before {
                return Err(format!("项目不在列表中：{raw}"));
            }
            println!("已移除项目：{raw}");
            Ok(())
        }
        _ => Err("用法：Nova server project <list|add|remove> [目录]".into()),
    }
}

fn manage_config(args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("show") | Some("list") => show_config(args.iter().any(|arg| arg == "--show-token")),
        Some("set") => {
            let key = args
                .get(1)
                .ok_or("用法：Nova server config set <键> <值>")?;
            let value = args
                .get(2)
                .ok_or("用法：Nova server config set <键> <值>")?;
            set_config(key, value)
        }
        Some("unset") => {
            let key = args.get(1).ok_or("用法：Nova server config unset <键>")?;
            set_config(key, "")
        }
        _ => Err("用法：Nova server config <show|set|unset> ...".into()),
    }
}

fn show_config(show_token: bool) -> Result<(), String> {
    let dir = data_dir();
    let settings = Settings::load(&dir);
    let server = load_server_file();
    let token = if show_token {
        settings.relay_token.clone()
    } else {
        mask_secret(&settings.relay_token)
    };
    let environment: HashMap<_, _> = server
        .environment
        .iter()
        .map(|(key, value)| {
            (
                key.clone(),
                if show_token {
                    value.clone()
                } else {
                    mask_secret(value)
                },
            )
        })
        .collect();
    let value = json!({
        "dataDir": dir,
        "relayServer": settings.relay_server,
        "token": token,
        "groups": settings.relay_groups,
        "name": server.name,
        "proxy": server.proxy,
        "environment": environment,
        "defaultMode": settings.default_mode,
        "agents": {
            "devin": { "enabled": settings.devin_enabled, "path": settings.devin_path, "proxy": settings.devin_proxy },
            "codex": { "enabled": settings.codex_enabled, "path": settings.codex_path, "args": settings.codex_args, "proxy": settings.codex_proxy },
            "codebuddy": { "enabled": settings.codebuddy_enabled, "path": settings.codebuddy_path, "proxy": settings.codebuddy_proxy },
            "claude": { "enabled": settings.claudecode_enabled, "path": settings.claudecode_path, "proxy": settings.claudecode_proxy },
            "cursor": { "enabled": settings.cursor_enabled, "path": settings.cursor_path, "proxy": settings.cursor_proxy },
            "opencode": { "enabled": settings.opencode_enabled, "path": settings.opencode_path, "proxy": settings.opencode_proxy }
        }
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&value).map_err(|error| error.to_string())?
    );
    Ok(())
}

fn parse_bool(value: &str) -> Result<bool, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" | "enable" | "enabled" => Ok(true),
        "false" | "0" | "no" | "off" | "disable" | "disabled" => Ok(false),
        _ => Err(format!("无效布尔值：{value}（应为 true/false）")),
    }
}

fn set_config(key: &str, value: &str) -> Result<(), String> {
    let dir = data_dir();
    let mut settings = Settings::load(&dir);
    let mut server = load_server_file();
    let key = key.trim_start_matches("--");
    if let Some(name) = key.strip_prefix("env.") {
        validate_environment_name(name)?;
        if value.is_empty() {
            server.environment.remove(name);
        } else {
            server
                .environment
                .insert(name.to_string(), value.to_string());
        }
    } else {
        match key {
            "relay-server" => settings.relay_server = value.into(),
            "token" | "relay-token" => settings.relay_token = value.into(),
            "groups" | "relay-groups" => settings.relay_groups = value.into(),
            "name" => server.name = value.into(),
            "proxy" => server.proxy = value.into(),
            "default-mode" => settings.default_mode = value.into(),
            "devin-path" => settings.devin_path = value.into(),
            "codex-path" => settings.codex_path = value.into(),
            "codex-args" => settings.codex_args = value.into(),
            "codebuddy-path" => settings.codebuddy_path = value.into(),
            "claude-path" => settings.claudecode_path = value.into(),
            "cursor-path" => settings.cursor_path = value.into(),
            "opencode-path" => settings.opencode_path = value.into(),
            "devin-proxy" => settings.devin_proxy = value.into(),
            "codex-proxy" => settings.codex_proxy = value.into(),
            "codebuddy-proxy" => settings.codebuddy_proxy = value.into(),
            "claude-proxy" => settings.claudecode_proxy = value.into(),
            "cursor-proxy" => settings.cursor_proxy = value.into(),
            "opencode-proxy" => settings.opencode_proxy = value.into(),
            "devin-enabled" => settings.devin_enabled = parse_bool(value)?,
            "codex-enabled" => settings.codex_enabled = parse_bool(value)?,
            "codebuddy-enabled" => settings.codebuddy_enabled = parse_bool(value)?,
            "claude-enabled" => settings.claudecode_enabled = parse_bool(value)?,
            "cursor-enabled" => settings.cursor_enabled = parse_bool(value)?,
            "opencode-enabled" => settings.opencode_enabled = parse_bool(value)?,
            unknown => return Err(format!("未知配置键：{unknown}\n\n{HELP}")),
        }
    }
    settings.remote_control_enabled = true;
    settings.save(&dir);
    save_server_file(&server)?;
    println!("已更新配置：{key}");
    Ok(())
}

fn validate_environment_name(name: &str) -> Result<(), String> {
    let mut chars = name.chars();
    if !chars
        .next()
        .is_some_and(|ch| ch == '_' || ch.is_ascii_alphabetic())
        || !chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
    {
        return Err(format!("无效环境变量名：{name}"));
    }
    Ok(())
}

/// Persisted agent credentials are a fallback. Values explicitly supplied by systemd, a shell,
/// or an EnvironmentFile win so operators can rotate secrets without rewriting Nova data.
fn apply_server_environment() {
    for (name, value) in load_server_file().environment {
        if validate_environment_name(&name).is_ok() && std::env::var_os(&name).is_none() {
            std::env::set_var(name, value);
        }
    }
}

fn mask_secret(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.is_empty() {
        return String::new();
    }
    if chars.len() <= 8 {
        return "****".into();
    }
    format!(
        "{}****{}",
        chars[..4].iter().collect::<String>(),
        chars[chars.len() - 4..].iter().collect::<String>()
    )
}

/// Recognize `Nova server` / `Nova --headless`, persist the options in process-local environment
/// variables, and return whether server mode was selected.
pub fn configure_from_args() -> Result<bool, String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let selected = args.first().map(|v| v == "server").unwrap_or(false)
        || args.iter().any(|v| v == "--headless");
    if !selected {
        return Ok(false);
    }

    std::env::set_var(ENV_HEADLESS, "1");
    let mut projects = Vec::new();
    let mut i = if args.first().map(|v| v == "server").unwrap_or(false) {
        1
    } else {
        0
    };
    while i < args.len() {
        let arg = &args[i];
        let (key, inline) = arg
            .strip_prefix("--")
            .and_then(|v| v.split_once('='))
            .map(|(k, v)| (k, Some(v.to_string())))
            .unwrap_or_else(|| (arg.trim_start_matches("--"), None));
        if key == "headless" {
            i += 1;
            continue;
        }
        let value = if let Some(value) = inline {
            value
        } else {
            i += 1;
            args.get(i)
                .cloned()
                .ok_or_else(|| format!("--{key} 缺少参数"))?
        };
        match key {
            "relay-server" => std::env::set_var(ENV_RELAY_SERVER, value),
            "token" => std::env::set_var(ENV_TOKEN, value),
            "name" => std::env::set_var(ENV_NAME, value),
            "proxy" => std::env::set_var(ENV_PROXY, value),
            "project" => projects.push(value),
            "data-dir" => std::env::set_var(ENV_DATA_DIR, value),
            unknown => return Err(format!("未知的 server 参数：--{unknown}")),
        }
        i += 1;
    }
    if !projects.is_empty() {
        // Unit Separator is valid in paths on Unix only in theory and avoids colon/semicolon
        // ambiguity across platforms. Values are process-local and never written as this string.
        std::env::set_var(ENV_PROJECTS, projects.join("\u{1f}"));
    }
    apply_server_environment();
    Ok(true)
}

pub fn apply_settings(settings: &mut Settings) {
    if !is_headless() {
        return;
    }
    settings.remote_control_enabled = true;
    if let Ok(value) = std::env::var(ENV_RELAY_SERVER) {
        settings.relay_server = value;
    }
    if let Ok(value) = std::env::var(ENV_TOKEN) {
        settings.relay_token = value;
    }
}

pub fn configured_projects() -> Vec<PathBuf> {
    std::env::var(ENV_PROJECTS)
        .unwrap_or_default()
        .split('\u{1f}')
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .collect()
}

/// `--project` 同时充当网页远控的目录白名单。未显式配置时保留桌面 settings/projects
/// 的兼容行为；临时会话由 Nova 自己创建，可以安全展示。
pub fn path_allowed(path: &str) -> bool {
    if !is_headless() || path.contains(crate::SCRATCH_MARK) {
        return true;
    }
    let mut roots = configured_projects();
    if roots.is_empty() {
        roots = ProjectStore::load(&data_dir())
            .projects
            .into_iter()
            .map(PathBuf::from)
            .collect();
    }
    if roots.is_empty() {
        return false;
    }
    let candidate = std::fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path));
    roots.into_iter().any(|root| {
        let root = std::fs::canonicalize(&root).unwrap_or(root);
        candidate.starts_with(root)
    })
}

pub fn configured_name() -> Option<String> {
    std::env::var(ENV_NAME)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            let value = load_server_file().name;
            (!value.trim().is_empty()).then_some(value)
        })
}

pub fn configured_proxy() -> Option<String> {
    std::env::var(ENV_PROXY)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            let value = load_server_file().proxy;
            (!value.trim().is_empty()).then_some(value)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_projects_uses_unit_separator() {
        std::env::set_var(ENV_PROJECTS, "/srv/a\u{1f}/srv/b");
        assert_eq!(
            configured_projects(),
            vec![PathBuf::from("/srv/a"), PathBuf::from("/srv/b")]
        );
        std::env::remove_var(ENV_PROJECTS);
    }

    #[test]
    fn token_masking_does_not_expose_full_secret() {
        assert_eq!(mask_secret(""), "");
        assert_eq!(mask_secret("short"), "****");
        assert_eq!(mask_secret("abcd-very-secret-wxyz"), "abcd****wxyz");
    }

    #[test]
    fn boolean_config_values_are_human_friendly() {
        assert_eq!(parse_bool("yes"), Ok(true));
        assert_eq!(parse_bool("disabled"), Ok(false));
        assert!(parse_bool("maybe").is_err());
    }

    #[test]
    fn validates_environment_variable_names() {
        assert!(validate_environment_name("OPENAI_API_KEY").is_ok());
        assert!(validate_environment_name("_PROVIDER_TOKEN_2").is_ok());
        assert!(validate_environment_name("").is_err());
        assert!(validate_environment_name("2TOKEN").is_err());
        assert!(validate_environment_name("BAD-NAME").is_err());
    }
}
