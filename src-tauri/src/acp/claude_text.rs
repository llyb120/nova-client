//! Restricted Claude print launcher. The parent owns stdin/stream-json, timeout and process trees.
//! Keep the backend's launch_env/proxy and scrub NOVA_* in the parent, just as for CodeBuddy.
//! Do not clear HOME/CLAUDE_CONFIG_DIR or read credentials here.

use std::io::Read;
use std::path::{Path, PathBuf};
use tokio::process::Command;

pub(super) fn args(model: &str) -> Vec<String> {
    // safe-mode preserves auth/model settings, but does NOT itself disable built-in tools.
    // Do not add --bare (disables OAuth) or --setting-sources "" (drops settings.env auth).
    let mut args: Vec<String> = [
        "--print", "--verbose", "--safe-mode", "--tools", "", "--strict-mcp-config",
        "--permission-mode", "dontAsk", "--no-session-persistence", "--max-turns", "1",
        "--input-format", "stream-json", "--output-format", "stream-json",
    ].into_iter().map(str::to_owned).collect();
    let (model, effort) = super::split_model_effort(Some(model.trim()));
    if let Some(model) = model {
        args.extend(["--model".into(), model]);
    }
    if let Some(effort) = effort {
        args.extend(["--effort".into(), effort]);
    }
    args
}

/// Prefer the configured adapter's SDK over an unrelated/global CLI. No probing process or
/// credential/config reads: unsupported CLI flags fail closed when the parent runs the command.
/// The caller must apply the same backend launch_env and proxy before spawning this command.
pub(super) fn command(adapter: &str, model: &str, cwd: &Path) -> Result<Command, String> {
    let adapter_path = super::resolve_program_on_path(adapter)
        .and_then(|path| dunce::canonicalize(&path).ok())
        .ok_or("找不到配置的 Claude ACP adapter，未回退到全工具会话")?;
    let cli = sdk_cli(&adapter_path).or_else(|| {
        let sibling = super::resolve_sibling_program(adapter, "claude");
        super::resolve_program_on_path(&sibling).and_then(|path| dunce::canonicalize(&path).ok())
    }).ok_or("找不到 Claude CLI 或 adapter 的 SDK CLI；请修复安装（保留 optional dependencies）")?;
    // Node's main-script loader (and cmd shims) reject canonicalize's verbatim paths.
    #[cfg(windows)]
    let cli = PathBuf::from(crate::explorer_path(&cli.to_string_lossy()));

    let mut cmd = if cli.extension().is_some_and(|ext| ext == "js") {
        let node = super::resolve_sibling_program(adapter, "node");
        let node = super::resolve_program_on_path(&node)
            .and_then(|path| dunce::canonicalize(&path).ok())
            .ok_or("Claude SDK cli.js 需要 adapter 使用的 Node.js 运行时")?;
        let mut cmd = Command::new(node);
        cmd.arg(&cli);
        cmd
    } else {
        // std handles Windows batch-file escaping, including empty args and shell metacharacters.
        Command::new(cli)
    };
    cmd.args(args(model)).current_dir(cwd)
        .env("DISABLE_AUTOUPDATER", "1").env_remove("CLAUDECODE");
    Ok(cmd)
}

fn native_sdk_cli() -> Option<String> {
    let os = match std::env::consts::OS {
        "windows" => "win32", "macos" => "darwin", "linux" => "linux", _ => return None,
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x64", "aarch64" => "arm64", _ => return None,
    };
    let libc = if cfg!(all(target_os = "linux", target_env = "musl")) { "-musl" } else { "" };
    Some(format!("@anthropic-ai/claude-agent-sdk-{os}-{arch}{libc}/claude{}", std::env::consts::EXE_SUFFIX))
}

fn sdk_cli(adapter: &Path) -> Option<PathBuf> {
    let adapter = dunce::canonicalize(&adapter).ok()?;
    // Only read the installed entry/shim, never user settings. This disambiguates co-installed
    // old/new npm scopes without evaluating shell code. Unix symlinks resolve to the entry itself.
    let mut shim = String::new();
    if let Ok(file) = std::fs::File::open(&adapter) {
        let _ = file.take(16 * 1024).read_to_string(&mut shim);
    }
    let scope = ["@agentclientprotocol", "@zed-industries"]
        .into_iter().find(|scope| shim.contains(&format!("{scope}/claude-agent-acp"))
            || shim.contains(&format!("{scope}\\claude-agent-acp")));
    // ponytail: support npm shims, direct entries and pnpm/symlinked packages within 8 ancestors;
    // add a concrete layout if another installer needs it, never recursively scan disk/PATH.
    for dir in adapter.parent()?.ancestors().take(8) {
        let package = scope.map(|scope| dir.join("node_modules").join(scope).join("claude-agent-acp"));
        for root in package.as_deref().into_iter().chain(std::iter::once(dir)) {
            let sdk = root.join("node_modules/@anthropic-ai/claude-agent-sdk");
            if !sdk.join("package.json").is_file() { continue; }
            let sdk = dunce::canonicalize(&sdk).ok()?;
            if let Some(relative) = native_sdk_cli() {
                // Node's dependency lookup: nested optional dependency, then hoisted ancestors.
                for parent in sdk.ancestors().take(8) {
                    let native = parent.join("node_modules").join(&relative);
                    if native.is_file() { return Some(native); }
                }
            }
            let legacy = sdk.join("cli.js");
            return legacy.is_file().then_some(legacy);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn restrictions_preserve_auth_and_model_effort() {
        let args = args(" claude-sonnet-4-6:high ");
        for pair in [
            ["--tools", ""], ["--max-turns", "1"], ["--permission-mode", "dontAsk"],
            ["--input-format", "stream-json"], ["--output-format", "stream-json"],
            ["--model", "claude-sonnet-4-6"], ["--effort", "high"],
        ] {
            assert!(args.windows(2).any(|window| window == pair));
        }
        for flag in ["--print", "--verbose", "--safe-mode", "--strict-mcp-config", "--no-session-persistence"] {
            assert!(args.iter().any(|arg| arg == flag));
        }
        for flag in ["--bare", "--setting-sources", "--settings", "--mcp-config"] {
            assert!(!args.iter().any(|arg| arg == flag));
        }
        assert!(!super::args("  ").iter().any(|arg| arg == "--model" || arg == "--effort"));
        assert!(super::args("custom:variant").ends_with(&["--model".into(), "custom:variant".into()]));
    }

    #[test]
    fn sdk_resolution_covers_nested_hoisted_direct_and_both_npm_scopes() {
        for scope in ["@agentclientprotocol", "@zed-industries"] {
            for hoisted in [false, true] {
                let temp = tempfile::tempdir().unwrap();
                let prefix = temp.path().join("install with spaces 中文");
                let package = prefix.join("node_modules").join(scope).join("claude-agent-acp");
                let shim = prefix.join("claude-agent-acp.cmd");
                file(&shim, &format!("%dp0%\\node_modules\\{scope}\\claude-agent-acp\\dist\\index.js"));
                let entry = package.join("dist/index.js");
                file(&entry, "#!/usr/bin/env node\n");
                let root = if hoisted { &prefix } else { &package };
                let sdk = root.join("node_modules/@anthropic-ai/claude-agent-sdk");
                file(&sdk.join("package.json"), "{}");
                let legacy = sdk.join("cli.js");
                file(&legacy, "// legacy SDK CLI");
                for adapter in [&shim, &entry] {
                    assert_eq!(sdk_cli(adapter), Some(dunce::canonicalize(&legacy).unwrap()));
                }
                if let Some(relative) = native_sdk_cli() {
                    // Both SDK-local and hoisted native optional dependencies.
                    for native_root in [&sdk, root] {
                        let native = native_root.join("node_modules").join(&relative);
                        file(&native, "native fixture");
                        assert_eq!(sdk_cli(&shim), Some(dunce::canonicalize(&native).unwrap()));
                        std::fs::remove_file(native).unwrap();
                    }
                }
                std::fs::remove_file(legacy).unwrap();
                assert_eq!(sdk_cli(&shim), None);
            }
        }
    }

    #[test]
    fn missing_adapter_fails_even_if_global_claude_is_installed() {
        let temp = tempfile::tempdir().unwrap();
        let absent = temp.path().join("absent-adapter");
        assert!(command(&absent.to_string_lossy(), "", temp.path()).is_err());
    }

    #[cfg(windows)]
    #[tokio::test]
    #[ignore = "requires Node.js, the Claude adapter runtime"]
    async fn sibling_cmd_mock_preserves_empty_args_and_shell_metacharacters() {
        let temp = tempfile::tempdir().unwrap();
        let prefix = temp.path().join("shim with spaces 中文");
        let adapter = prefix.join("claude-agent-acp.cmd");
        file(&adapter, "@echo off\r\n");
        file(&prefix.join("claude.cmd"), "@echo off\r\nnode \"%~dp0mock.js\" %*\r\n");
        file(&prefix.join("mock.js"), "console.log(JSON.stringify(process.argv.slice(2)));\n");
        let model = "custom model & echo should-not-run:high";
        let mut cmd = command(&adapter.to_string_lossy(), model, temp.path()).unwrap();
        let output = tokio::time::timeout(std::time::Duration::from_secs(10), cmd.output())
            .await.unwrap().unwrap();
        assert!(output.status.success(), "mock exited {}: {}", output.status, String::from_utf8_lossy(&output.stderr));
        let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result, serde_json::json!(args(model)));
    }

    #[tokio::test]
    #[ignore = "requires Node.js, the Claude adapter runtime"]
    async fn legacy_cli_mock_preserves_empty_arg_cwd_and_inherited_environment() {
        let temp = tempfile::tempdir().unwrap();
        let package = temp.path().join("adapter with spaces 中文");
        let adapter = package.join("dist/index.js");
        file(&adapter, "#!/usr/bin/env node\n");
        let sdk = package.join("node_modules/@anthropic-ai/claude-agent-sdk");
        file(&sdk.join("package.json"), "{}");
        file(&sdk.join("cli.js"), r#"
console.log(JSON.stringify({args:process.argv.slice(2),cwd:process.cwd(),
  inherited:process.env.CLAUDE_TEXT_MOCK === 'fake-local-only',nested:'CLAUDECODE' in process.env}));
"#);
        let mut cmd = command(&adapter.to_string_lossy(), "custom model:high", temp.path()).unwrap();
        cmd.env("CLAUDE_TEXT_MOCK", "fake-local-only");
        let output = tokio::time::timeout(std::time::Duration::from_secs(10), cmd.output())
            .await.unwrap().unwrap();
        // Only this synthetic script's diagnostics; never surface a real CLI's stderr/auth data.
        assert!(output.status.success(), "mock exited {}: {}", output.status, String::from_utf8_lossy(&output.stderr));
        let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["args"], serde_json::json!(args("custom model:high")));
        assert_eq!(dunce::canonicalize(result["cwd"].as_str().unwrap()).unwrap(), dunce::canonicalize(temp.path()).unwrap());
        assert_eq!(result["inherited"], true);
        assert_eq!(result["nested"], false);
    }
}
