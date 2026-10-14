#[cfg(any(windows, target_os = "macos", test))]
use std::collections::HashSet;
#[cfg(any(windows, target_os = "macos", test))]
use std::ffi::{OsStr, OsString};
#[cfg(any(windows, target_os = "macos", test))]
use std::path::PathBuf;

#[cfg(any(target_os = "macos", test))]
const PATH_MARKER_BEGIN: &[u8] = b"__NOVA_PATH_BEGIN__";
#[cfg(any(target_os = "macos", test))]
const PATH_MARKER_END: &[u8] = b"__NOVA_PATH_END__";

/// 桌面启动不会加载终端的 shell 配置，Windows 注册表也不包含 fnm 动态注入的 PATH。
/// 必须在任何后端检测、CLI 子命令或 Tauri 线程启动前恢复 Node 和全局 CLI 的搜索路径。
pub fn init_process_path() {
    #[cfg(windows)]
    {
        // Tauri GUI may inherit Explorer's stale environment block. Refresh synchronously before
        // any CodeBuddy prewarm/model probe can spawn and snapshot missing CODEBUDDY_* values.
        if refresh_windows_process_environment().is_err() {
            init_windows_process_path();
        }
    }
    #[cfg(target_os = "macos")]
    init_macos_process_path();
}

/// Refresh the current process environment from the Windows system and current-user registry.
/// Values from the user key override system values; `Path` keeps both scopes, matching Windows'
/// environment-block behavior. Existing process-only variables are left untouched.
pub fn refresh_process_environment() -> Result<usize, String> {
    #[cfg(windows)]
    {
        refresh_windows_process_environment()
    }
    #[cfg(not(windows))]
    {
        Err("刷新环境变量仅支持 Windows".into())
    }
}

#[cfg(any(windows, target_os = "macos", test))]
fn merge_paths<'a>(groups: impl IntoIterator<Item = &'a OsStr>) -> Option<OsString> {
    let mut seen = HashSet::<PathBuf>::new();
    let mut merged = Vec::<PathBuf>::new();

    for group in groups {
        for path in std::env::split_paths(group) {
            if !path.as_os_str().is_empty() && seen.insert(path.clone()) {
                merged.push(path);
            }
        }
    }

    (!merged.is_empty())
        .then(|| std::env::join_paths(merged).ok())
        .flatten()
}

#[cfg(windows)]
fn init_windows_process_path() {
    if let Some(path) = windows_process_path(
        std::env::var_os("PATH"),
        std::env::var_os("USERPROFILE").map(PathBuf::from),
        std::env::var_os("APPDATA").map(PathBuf::from),
        std::env::var_os("FNM_DIR").map(PathBuf::from),
        std::env::var_os("FNM_MULTISHELL_PATH").map(PathBuf::from),
    ) {
        std::env::set_var("PATH", path);
    }
}

#[cfg(any(windows, test))]
fn windows_process_path(
    current: Option<OsString>,
    home: Option<PathBuf>,
    appdata: Option<PathBuf>,
    fnm_dir: Option<PathBuf>,
    multishell: Option<PathBuf>,
) -> Option<OsString> {
    let home = home.filter(|path| path.is_absolute());
    let appdata = appdata.filter(|path| path.is_absolute());
    // Follow fnm's root selection: explicit FNM_DIR, existing modern root, then legacy ~/.fnm.
    let default = fnm_dir
        .filter(|path| path.is_absolute())
        .or_else(|| {
            appdata
                .as_ref()
                .map(|path| path.join("fnm"))
                .filter(|path| path.exists())
        })
        .or_else(|| home.as_ref().map(|path| path.join(".fnm")))
        .map(|path| path.join("aliases/default"));
    let active = multishell.filter(|path| {
        path.is_absolute() && (path.join("node.exe").is_file() || std::fs::read_link(path).is_ok())
    });
    // `fnm use system` deliberately leaves a dangling session link. Honor any existing link,
    // even without Node, instead of silently switching the user's selected version to default.
    // Windows aliases/sessions point directly to installation (no bin suffix). Use one version
    // only, so a missing CLI is not borrowed from a different version's global installation.
    let mut paths: Vec<PathBuf> = active
        .or(default)
        .filter(|path| path.join("node.exe").is_file())
        .into_iter()
        .collect();
    // 与 fnm env 一样让所选版本优先于系统 Node；子进程也会继承同一份 PATH。
    if let Some(current) = current {
        paths.extend(std::env::split_paths(&current));
    }
    if let Some(home) = home {
        paths.push(home.join(".local/bin"));
    }
    if let Some(appdata) = appdata {
        paths.push(appdata.join("npm"));
    }
    let path = std::env::join_paths(paths).ok()?;
    merge_paths([path.as_os_str()])
}

#[cfg(windows)]
#[derive(Clone)]
struct RegistryEnvironmentValue {
    name: String,
    value: String,
    expandable: bool,
}

#[cfg(windows)]
fn refresh_windows_process_environment() -> Result<usize, String> {
    use std::collections::HashMap;
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
    use winreg::RegKey;

    const SYSTEM_ENVIRONMENT: &str =
        r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment";
    const USER_ENVIRONMENT: &str = r"Environment";

    let system = RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey(SYSTEM_ENVIRONMENT)
        .map_err(|error| format!("读取系统环境变量注册表失败：{error}"))?;
    let user = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(USER_ENVIRONMENT)
        .map_err(|error| format!("读取用户环境变量注册表失败：{error}"))?;

    let system_values = read_registry_environment(&system, "系统")?;
    let user_values = read_registry_environment(&user, "用户")?;
    let mut values = HashMap::<String, RegistryEnvironmentValue>::new();
    for value in system_values {
        values.insert(value.name.to_uppercase(), value);
    }
    for value in user_values {
        let key = value.name.to_uppercase();
        if key == "PATH" {
            if let Some(system_path) = values.get_mut(&key) {
                if !system_path.value.is_empty() && !value.value.is_empty() {
                    system_path.value.push(';');
                }
                system_path.value.push_str(&value.value);
                system_path.expandable |= value.expandable;
                continue;
            }
        }
        values.insert(key, value);
    }

    // Registry expandable strings commonly refer to process-only values such as SystemRoot and
    // USERPROFILE. Seed expansion with the current block, then overlay the fresh registry values.
    let mut expansion_environment: HashMap<String, String> = std::env::vars()
        .map(|(name, value)| (name.to_uppercase(), value))
        .collect();
    for (key, value) in &values {
        expansion_environment.insert(key.clone(), value.value.clone());
    }

    let mut expanded_values = Vec::with_capacity(values.len());
    for (key, value) in values {
        let expanded = if value.expandable {
            expand_windows_environment_value(&value.value, &expansion_environment)
        } else {
            value.value
        };
        expansion_environment.insert(key, expanded.clone());
        expanded_values.push((value.name, expanded));
    }

    for (name, value) in &expanded_values {
        std::env::set_var(name, value);
    }

    // Registry Path omits fnm's shell-only Node path. Restore it on refresh as well as startup.
    init_windows_process_path();

    Ok(expanded_values.len())
}

#[cfg(windows)]
fn read_registry_environment(
    key: &winreg::RegKey,
    scope: &str,
) -> Result<Vec<RegistryEnvironmentValue>, String> {
    use winreg::enums::{REG_EXPAND_SZ, REG_SZ};
    use winreg::types::FromRegValue;

    let mut values = Vec::new();
    for entry in key.enum_values() {
        let (name, raw) = entry.map_err(|error| format!("枚举{scope}环境变量失败：{error}"))?;
        if raw.vtype != REG_SZ && raw.vtype != REG_EXPAND_SZ {
            continue;
        }
        let value = String::from_reg_value(&raw)
            .map_err(|error| format!("读取{scope}环境变量 {name} 失败：{error}"))?;
        values.push(RegistryEnvironmentValue {
            name,
            value,
            expandable: raw.vtype == REG_EXPAND_SZ,
        });
    }
    Ok(values)
}

#[cfg(windows)]
fn expand_windows_environment_value(
    value: &str,
    environment: &std::collections::HashMap<String, String>,
) -> String {
    let mut result = value.to_string();
    for _ in 0..8 {
        let mut output = String::with_capacity(result.len());
        let mut rest = result.as_str();
        let mut changed = false;
        while let Some(begin) = rest.find('%') {
            output.push_str(&rest[..begin]);
            let after_begin = &rest[begin + 1..];
            let Some(end) = after_begin.find('%') else {
                output.push_str(&rest[begin..]);
                rest = "";
                break;
            };
            let name = &after_begin[..end];
            if let Some(replacement) = environment.get(&name.to_uppercase()) {
                output.push_str(replacement);
                changed = true;
            } else {
                output.push('%');
                output.push_str(name);
                output.push('%');
            }
            rest = &after_begin[end + 1..];
        }
        output.push_str(rest);
        if !changed || output == result {
            return output;
        }
        result = output;
    }
    result
}

#[cfg(any(target_os = "macos", test))]
fn extract_marked_path(output: &[u8]) -> Option<OsString> {
    let begin = output
        .windows(PATH_MARKER_BEGIN.len())
        .rposition(|window| window == PATH_MARKER_BEGIN)?
        + PATH_MARKER_BEGIN.len();
    let rest = &output[begin..];
    let end = rest
        .windows(PATH_MARKER_END.len())
        .position(|window| window == PATH_MARKER_END)?;
    let mut value = &rest[..end];
    while value
        .last()
        .is_some_and(|byte| matches!(byte, b'\r' | b'\n'))
    {
        value = &value[..value.len() - 1];
    }
    (!value.is_empty()).then(|| bytes_to_os_string(value))
}

#[cfg(target_os = "macos")]
fn bytes_to_os_string(value: &[u8]) -> OsString {
    use std::os::unix::ffi::OsStringExt;
    OsString::from_vec(value.to_vec())
}

#[cfg(all(test, not(target_os = "macos")))]
fn bytes_to_os_string(value: &[u8]) -> OsString {
    OsString::from(String::from_utf8_lossy(value).into_owned())
}

#[cfg(target_os = "macos")]
fn init_macos_process_path() {
    let current = std::env::var_os("PATH");
    let shell = login_shell_path();
    let shell_path = read_shell_path(&shell);
    let fallback = fallback_macos_path();

    let mut groups = Vec::<&OsStr>::new();
    if let Some(path) = shell_path.as_deref() {
        groups.push(path);
    }
    if let Some(path) = fallback.as_deref() {
        groups.push(path);
    }
    if let Some(path) = current.as_deref() {
        groups.push(path);
    }

    if let Some(path) = merge_paths(groups) {
        std::env::set_var("PATH", path);
    }
}

#[cfg(target_os = "macos")]
fn login_shell_path() -> PathBuf {
    std::env::var_os("SHELL")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute() && path.is_file())
        .unwrap_or_else(|| PathBuf::from("/bin/zsh"))
}

#[cfg(target_os = "macos")]
fn read_shell_path(shell: &std::path::Path) -> Option<OsString> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    // printenv 读取的是 shell 导出给子进程的 PATH；即使 fish 把 PATH 表示成列表，
    // 这里得到的仍是标准冒号分隔形式。标记符可避开 .zshrc 等文件输出的提示文本。
    let command = concat!(
        "/usr/bin/printf '__NOVA_PATH_BEGIN__'; ",
        "/usr/bin/printenv PATH; ",
        "/usr/bin/printf '__NOVA_PATH_END__'"
    );
    let mut child = Command::new(shell)
        .args(["-ilc", command])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut output = Vec::new();
        let _ = stdout.read_to_end(&mut output);
        output
    });

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let output = reader.join().ok()?;
                return if status.success() {
                    extract_marked_path(&output)
                } else {
                    None
                };
            }
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return None;
            }
        }
    }
}

#[cfg(target_os = "macos")]
fn fallback_macos_path() -> Option<OsString> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut paths = vec![
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/opt/homebrew/sbin"),
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/usr/local/sbin"),
    ];

    if let Some(home) = home {
        for relative in [
            ".local/bin",
            ".cargo/bin",
            ".volta/bin",
            ".bun/bin",
            ".asdf/shims",
            ".local/share/mise/shims",
        ] {
            paths.push(home.join(relative));
        }
        append_matching_dirs(&mut paths, home.join(".nvm/versions/node"), "bin");
        append_matching_dirs(
            &mut paths,
            home.join(".fnm/node-versions"),
            "installation/bin",
        );
        append_matching_dirs(
            &mut paths,
            home.join("Library/Application Support/fnm/node-versions"),
            "installation/bin",
        );
    }

    std::env::join_paths(paths).ok()
}

#[cfg(target_os = "macos")]
fn append_matching_dirs(paths: &mut Vec<PathBuf>, root: PathBuf, suffix: &str) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    let mut matches: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path().join(suffix))
        .filter(|path| path.is_dir())
        .collect();
    matches.sort_by(|a, b| b.cmp(a));
    paths.extend(matches);
}

#[cfg(test)]
mod tests {
    use super::{extract_marked_path, merge_paths, windows_process_path};
    use std::ffi::{OsStr, OsString};
    use std::path::PathBuf;

    #[test]
    fn extracts_path_while_ignoring_shell_startup_output() {
        let output =
            b"shell greeting\n__NOVA_PATH_BEGIN__/opt/homebrew/bin:/usr/bin\n__NOVA_PATH_END__";
        assert_eq!(
            extract_marked_path(output),
            Some(OsString::from("/opt/homebrew/bin:/usr/bin"))
        );
    }

    #[test]
    fn merge_keeps_priority_and_removes_duplicates() {
        let first =
            std::env::join_paths([PathBuf::from("shell"), PathBuf::from("shared")]).unwrap();
        let second =
            std::env::join_paths([PathBuf::from("fallback"), PathBuf::from("shared")]).unwrap();
        let merged = merge_paths([OsStr::new(&first), OsStr::new(&second)]).unwrap();
        let paths: Vec<PathBuf> = std::env::split_paths(&merged).collect();
        assert_eq!(
            paths,
            vec![
                PathBuf::from("shell"),
                PathBuf::from("shared"),
                PathBuf::from("fallback")
            ]
        );
    }

    #[test]
    fn windows_path_preserves_existing_entries_and_adds_cli_directories() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("用户 home");
        let appdata = home.join("AppData/Roaming");
        let system = temp.path().join("system");
        let current = std::env::join_paths([&system, &home.join(".local/bin")]).unwrap();
        let path = windows_process_path(
            Some(current),
            Some(home.clone()),
            Some(appdata.clone()),
            None,
            None,
        )
        .unwrap();
        assert_eq!(
            std::env::split_paths(&path).collect::<Vec<_>>(),
            vec![system, home.join(".local/bin"), appdata.join("npm")],
        );
    }

    #[test]
    fn windows_path_recovers_fnm_default_node_and_global_clis() {
        let temp = tempfile::tempdir().unwrap();
        let appdata = temp.path().join("用户 data; with spaces");
        let default = appdata.join("fnm/aliases/default");
        let npm = appdata.join("npm");
        std::fs::create_dir_all(&default).unwrap();
        std::fs::create_dir_all(&npm).unwrap();
        for name in ["node.exe", "npm.cmd", "npx.cmd", "codebuddy.cmd"] {
            std::fs::write(default.join(name), "").unwrap();
        }
        std::fs::write(npm.join("claude.cmd"), "").unwrap();
        let system = temp.path().join("system-node");
        let path = windows_process_path(
            Some(std::env::join_paths([&system]).unwrap()),
            None,
            Some(appdata),
            None,
            None,
        )
        .unwrap();
        let paths = std::env::split_paths(&path).collect::<Vec<_>>();
        assert_eq!(paths, vec![default.clone(), system, npm.clone()]);
        for name in [
            "node.exe",
            "npm.cmd",
            "npx.cmd",
            "codebuddy.cmd",
            "claude.cmd",
        ] {
            assert_eq!(
                paths
                    .iter()
                    .map(|dir| dir.join(name))
                    .find(|file| file.is_file()),
                Some(if name == "claude.cmd" {
                    npm.join(name)
                } else {
                    default.join(name)
                }),
            );
        }
    }

    #[test]
    fn windows_path_uses_active_fnm_session_then_custom_default_not_other_versions() {
        let temp = tempfile::tempdir().unwrap();
        let appdata = temp.path().join("roaming");
        let custom = temp.path().join("custom fnm");
        let default = custom.join("aliases/default");
        let active = temp.path().join("fnm_multishells/session");
        let other = custom.join("node-versions/v99.0.0/installation");
        for dir in [
            &active,
            &default,
            &other,
            &appdata.join("fnm/aliases/default"),
        ] {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(dir.join("node.exe"), "").unwrap();
        }
        let missing = temp.path().join("stale-session");
        for (session, expected) in [
            (Some(active.clone()), active),
            (Some(missing), default.clone()),
            (None, default.clone()),
        ] {
            let path = windows_process_path(
                None,
                None,
                Some(appdata.clone()),
                Some(custom.clone()),
                session,
            )
            .unwrap();
            assert_eq!(
                std::env::split_paths(&path).collect::<Vec<_>>(),
                vec![expected, appdata.join("npm")],
            );
        }
        // A broken custom default must not silently switch to a different fnm installation/version.
        std::fs::remove_file(default.join("node.exe")).unwrap();
        let path =
            windows_process_path(None, None, Some(appdata.clone()), Some(custom), None).unwrap();
        assert_eq!(
            std::env::split_paths(&path).collect::<Vec<_>>(),
            vec![appdata.join("npm")]
        );
    }

    #[test]
    fn windows_path_uses_legacy_fnm_root_only_without_modern_root() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().to_path_buf();
        let appdata = home.join("roaming");
        let legacy = home.join(".fnm/aliases/default");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("node.exe"), "").unwrap();
        let path =
            windows_process_path(None, Some(home.clone()), Some(appdata.clone()), None, None)
                .unwrap();
        assert_eq!(std::env::split_paths(&path).next(), Some(legacy));
        std::fs::create_dir_all(appdata.join("fnm")).unwrap();
        let path =
            windows_process_path(None, Some(home.clone()), Some(appdata.clone()), None, None)
                .unwrap();
        assert_eq!(
            std::env::split_paths(&path).collect::<Vec<_>>(),
            vec![home.join(".local/bin"), appdata.join("npm")],
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_path_honors_dangling_fnm_session_link_for_system_node() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let default = root.join("aliases/default");
        let target = root.join("system-placeholder");
        let session = root.join("session");
        std::fs::create_dir_all(&default).unwrap();
        std::fs::write(default.join("node.exe"), "").unwrap();
        std::fs::create_dir(&target).unwrap();
        let output = std::process::Command::new("cmd")
            .args(["/D", "/C", "mklink", "/J"])
            .arg(&session)
            .arg(&target)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        std::fs::remove_dir(&target).unwrap();
        assert!(std::fs::read_link(&session).is_ok());
        let current = std::env::join_paths([root.join("system-node")]).unwrap();
        assert_eq!(
            windows_process_path(
                Some(current.clone()),
                None,
                None,
                Some(root.to_path_buf()),
                Some(session),
            ),
            Some(current),
        );
    }
}
