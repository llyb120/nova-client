//! WorkBuddy is a separate backend identity using its bundled CodeBuddy ACP entry point.
use std::io::Read;
use std::path::{Path, PathBuf};

// ponytail: WorkBuddy 5.7.7 gates live ACP models on this client name; replace
// with a negotiated capability when its CLI supports one. Keep the UI title Nova.
pub(crate) const ACP_CLIENT_NAME: &str = "codebuddy-web-ui";

// WorkBuddy 5.7.7 ships the CLI outside app.asar; its adjacent product.json selects
// workbuddy-desktop authentication, so never substitute the global CodeBuddy CLI.
fn bundled_cli(root: &Path) -> PathBuf {
    root.join("resources/app.asar.unpacked/cli/bin/codebuddy")
}

pub(crate) fn program(configured: &str) -> String {
    let configured = configured.trim();
    if !configured.is_empty() {
        let path = Path::new(configured);
        if path.is_dir() {
            return bundled_cli(path).to_string_lossy().into_owned();
        }
        if path.file_name().and_then(|name| name.to_str()).is_some_and(|name| name.eq_ignore_ascii_case("WorkBuddy.exe")) {
            return bundled_cli(path.parent().unwrap_or(Path::new("."))).to_string_lossy().into_owned();
        }
        return configured.into();
    }
    #[cfg(windows)]
    for (env, relative) in [("LOCALAPPDATA", "Programs/WorkBuddy"), ("ProgramFiles", "WorkBuddy"), ("ProgramFiles(x86)", "WorkBuddy")] {
        if let Some(root) = std::env::var_os(env) {
            let cli = bundled_cli(&PathBuf::from(root).join(relative));
            if cli.is_file() {
                return cli.to_string_lossy().into_owned();
            }
        }
    }
    String::new()
}

pub(crate) fn config_dir() -> Option<PathBuf> {
    std::env::var_os("WORKBUDDY_CONFIG_DIR").filter(|value| !value.is_empty()).map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))
            .map(|home| PathBuf::from(home).join(".workbuddy")))
}

fn bundled_node(root: &Path) -> Option<PathBuf> {
    let versions = root.join("binaries/node/versions");
    let version = std::fs::read_to_string(versions.join("current")).ok()?;
    let version = version.trim();
    if version.is_empty() || version.len() > 128 || matches!(version, "." | "..")
        || !version.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"-._".contains(&byte))
    {
        return None;
    }
    let node = versions.join(version).join(if cfg!(windows) { "node.exe" } else { "bin/node" });
    node.is_file().then_some(node)
}

fn node_script(path: &Path) -> bool {
    if path.extension().and_then(|ext| ext.to_str()).is_some_and(|ext| {
        ["js", "mjs", "cjs"].iter().any(|candidate| ext.eq_ignore_ascii_case(candidate))
    }) {
        return true;
    }
    let mut header = [0; 256];
    let len = std::fs::File::open(path)
        .and_then(|mut file| file.read(&mut header))
        .unwrap_or(0);
    let line = String::from_utf8_lossy(&header[..len]);
    let line = line.lines().next().unwrap_or("");
    line.starts_with("#!")
        && line.split_whitespace().any(|word| {
            word.rsplit(['/', '\\']).next().is_some_and(|name| name == "node" || name == "node.exe")
        })
}

/// Version/availability checks do not need a logged-in desktop runtime.
pub(crate) fn command(configured: &str) -> Result<tokio::process::Command, String> {
    build_command(configured, false)
}

/// The bridge spawns and unlocks its own worker; the WorkBuddy desktop app need not run.
pub(crate) fn acp_command(configured: &str) -> Result<tokio::process::Command, String> {
    build_command(configured, true)
}

fn build_command(configured: &str, managed: bool) -> Result<tokio::process::Command, String> {
    let configured = program(configured);
    if configured.is_empty() {
        return Err("未找到 WorkBuddy，请先安装并登录；自定义安装位置可在设置中填写安装目录或内置 CodeBuddy CLI 路径".into());
    }
    let path = crate::acp::resolve_program_on_path(&configured)
        .ok_or_else(|| format!("未找到 WorkBuddy 内置 CodeBuddy CLI：{configured}"))?;
    if path.file_stem().and_then(|name| name.to_str()).is_some_and(|name| name.eq_ignore_ascii_case("workbuddy")) {
        return Err("请选择 WorkBuddy 内置的 CodeBuddy CLI，而不是 WorkBuddy 桌面程序".into());
    }
    let root = config_dir().ok_or("无法确定 WorkBuddy 用户配置目录")?;
    let script = node_script(&path);
    let mut command = if script || managed {
        let node = path.parent().map(|dir| dir.join(if cfg!(windows) { "node.exe" } else { "node" }))
            .filter(|node| node.is_file())
            .or_else(|| bundled_node(&root))
            .or_else(|| crate::acp::resolve_program_on_path("node"))
            .ok_or("运行 WorkBuddy 内置 CodeBuddy 脚本需要 Node.js，请先启动一次 WorkBuddy 或安装 Node.js")?;
        let mut command = tokio::process::Command::new(node);
        if managed {
            // ponytail: embedded bridge stays below Windows' 32K command-line ceiling;
            // materialize it as a resource if it outgrows the bound checked below.
            command.args(["--input-type=module", "-e", include_str!("../../scripts/workbuddy-acp.mjs"), "--"]);
        }
        command.arg(&path);
        if managed && !script { command.arg("--native"); }
        command
    } else {
        #[cfg(windows)]
        let script = path.extension().and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("cmd") || ext.eq_ignore_ascii_case("bat"));
        #[cfg(not(windows))]
        let script = false;
        if script {
            let mut command = tokio::process::Command::new("cmd.exe");
            command.args(["/D", "/S", "/C"]).arg(&path);
            command
        } else {
            tokio::process::Command::new(&path)
        }
    };
    apply_runtime_env(&mut command, &root);
    Ok(command)
}

fn apply_runtime_env(command: &mut tokio::process::Command, root: &Path) {
    // Match WorkBuddy's buildAgentCliRuntimeEnv rather than inheriting ~/.codebuddy.
    command.env("CODEBUDDY_CONFIG_DIR", root).env("WORKBUDDY_CONFIG_DIR", root);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::threads::AgentKind;

    #[test]
    fn workbuddy_keeps_its_identity_and_never_falls_back_to_global_codebuddy() {
        let kind = AgentKind::from_str(" WorkBuddy ").unwrap();
        assert_eq!(kind, AgentKind::WorkBuddy);
        assert!(kind.is_codebuddy());
        assert_eq!(kind.as_str(), "workbuddy");
        assert_eq!(serde_json::to_string(&kind).unwrap(), "\"workbuddy\"");
        let missing = tempfile::tempdir().unwrap().path().join("missing-codebuddy");
        assert_eq!(program(missing.to_str().unwrap()), missing.to_str().unwrap());
        assert!(command(missing.to_str().unwrap()).is_err());
        assert!(!crate::settings::Settings::default().workbuddy_enabled);
    }

    #[test]
    fn workbuddy_maps_install_directory_and_desktop_exe_to_bundled_cli() {
        let dir = tempfile::tempdir().unwrap();
        let cli = bundled_cli(dir.path());
        std::fs::create_dir_all(cli.parent().unwrap()).unwrap();
        std::fs::write(&cli, "#!/usr/bin/env node\n").unwrap();
        assert_eq!(program(dir.path().to_str().unwrap()), cli.to_str().unwrap());
        assert_eq!(program(dir.path().join("WorkBuddy.exe").to_str().unwrap()), cli.to_str().unwrap());
        assert_eq!(program(cli.to_str().unwrap()), cli.to_str().unwrap());
        let node = cli.parent().unwrap().join(if cfg!(windows) { "node.exe" } else { "node" });
        std::fs::write(&node, []).unwrap();
        let command = command(dir.path().to_str().unwrap()).unwrap();
        assert_eq!(command.as_std().get_program(), node.as_os_str());
        assert_eq!(command.as_std().get_args().next(), Some(cli.as_os_str()));
        let env: std::collections::HashMap<_, _> = command.as_std().get_envs().collect();
        let root = config_dir().unwrap();
        assert_eq!(env.get(std::ffi::OsStr::new("CODEBUDDY_CONFIG_DIR")), Some(&Some(root.as_os_str())));
        assert_eq!(env.get(std::ffi::OsStr::new("WORKBUDDY_CONFIG_DIR")), Some(&Some(root.as_os_str())));
    }

    #[test]
    fn workbuddy_acp_uses_managed_bridge_without_exporting_desktop_credentials() {
        let bridge = include_str!("../../scripts/workbuddy-acp.mjs");
        assert!(bridge.encode_utf16().count() < 16000, "materialize an oversized bridge instead of passing it on the Windows command line");
        let dir = tempfile::tempdir().unwrap();
        let cli = bundled_cli(dir.path());
        std::fs::create_dir_all(cli.parent().unwrap()).unwrap();
        std::fs::write(&cli, "#!/usr/bin/env node\n").unwrap();
        std::fs::write(cli.parent().unwrap().join(if cfg!(windows) { "node.exe" } else { "node" }), []).unwrap();
        let command = acp_command(dir.path().to_str().unwrap()).unwrap();
        let args: Vec<_> = command.as_std().get_args().collect();
        assert_eq!(args, vec![std::ffi::OsStr::new("--input-type=module"), std::ffi::OsStr::new("-e"),
            std::ffi::OsStr::new(bridge), std::ffi::OsStr::new("--"), cli.as_os_str()]);
    }

    #[test]
    fn workbuddy_does_not_pin_models_to_the_desktop_bootstrap_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let product = dir.path().join("cache/acc-product-config-v3.json");
        let mut command = tokio::process::Command::new("node");
        apply_runtime_env(&mut command, dir.path());
        assert!(!command.as_std().get_envs().any(|(key, _)| key == "ACC_PRODUCT_CONFIG_PATH"));
        std::fs::create_dir_all(product.parent().unwrap()).unwrap();
        std::fs::write(&product, r#"{"agents":[{"name":"cli","models":["live-model"]}]}"#).unwrap();
        apply_runtime_env(&mut command, dir.path());
        assert!(!command.as_std().get_envs().any(|(key, _)| key == "ACC_PRODUCT_CONFIG_PATH"));
        assert_eq!(ACP_CLIENT_NAME, "codebuddy-web-ui");
        assert!(include_str!("acp.rs").contains("crate::workbuddy::ACP_CLIENT_NAME"));
    }

    #[test]
    fn workbuddy_uses_the_node_version_pointer_without_allowing_path_escape() {
        let dir = tempfile::tempdir().unwrap();
        let versions = dir.path().join("binaries/node/versions");
        let node = versions.join("22.22.2-6").join(if cfg!(windows) { "node.exe" } else { "bin/node" });
        std::fs::create_dir_all(node.parent().unwrap()).unwrap();
        std::fs::write(&node, []).unwrap();
        let current = versions.join("current");
        std::fs::write(&current, "22.22.2-6\r\n").unwrap();
        assert_eq!(bundled_node(dir.path()), Some(node));
        for invalid in ["", "..", "../22.22.2-6", "C:\\node", "missing"] {
            std::fs::write(&current, invalid).unwrap();
            assert!(bundled_node(dir.path()).is_none());
        }
    }

    #[test]
    fn workbuddy_accepts_bundled_node_scripts_without_an_extension() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("codebuddy");
        std::fs::write(&script, "#!/usr/bin/env node\n// bundled entry point").unwrap();
        assert!(node_script(&script));
        std::fs::write(&script, "#!/bin/sh\n").unwrap();
        assert!(!node_script(&script));
        assert!(node_script(&dir.path().join("cli.JS")));
        let desktop = dir.path().join("WorkBuddy.exe");
        std::fs::write(&desktop, []).unwrap();
        assert!(command(desktop.to_str().unwrap()).is_err());
    }
}
