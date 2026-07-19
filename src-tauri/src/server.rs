//! Headless server-mode configuration.
//!
//! The agent/runtime implementation is intentionally shared with the desktop app. On Linux the
//! executable creates a private Xvfb display before Tauri starts, while this module makes sure no
//! window is presented and supplies the remote-control settings from command-line arguments.

use crate::settings::Settings;
use std::path::PathBuf;

const ENV_HEADLESS: &str = "NOVA_HEADLESS";
const ENV_RELAY_SERVER: &str = "NOVA_SERVER_RELAY_URL";
const ENV_TOKEN: &str = "NOVA_SERVER_TOKEN";
const ENV_NAME: &str = "NOVA_SERVER_NAME";
const ENV_PROXY: &str = "NOVA_SERVER_PROXY";
const ENV_PROJECTS: &str = "NOVA_SERVER_PROJECTS";

pub fn is_headless() -> bool {
    std::env::var_os(ENV_HEADLESS).is_some()
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
            unknown => return Err(format!("未知的 server 参数：--{unknown}")),
        }
        i += 1;
    }
    if !projects.is_empty() {
        // Unit Separator is valid in paths on Unix only in theory and avoids colon/semicolon
        // ambiguity across platforms. Values are process-local and never written as this string.
        std::env::set_var(ENV_PROJECTS, projects.join("\u{1f}"));
    }
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
    let roots = configured_projects();
    if roots.is_empty() || path.contains(crate::SCRATCH_MARK) {
        return true;
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
}

pub fn configured_proxy() -> Option<String> {
    std::env::var(ENV_PROXY)
        .ok()
        .filter(|value| !value.trim().is_empty())
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
}
