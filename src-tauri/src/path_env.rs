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

/// macOS 从 Finder / Dock 启动 .app 时不会加载用户的 shell 配置，进程 PATH 通常只有
/// /usr/bin:/bin:/usr/sbin:/sbin。后端 CLI 大多由 Homebrew 或 Node 版本管理器安装，
/// 因此必须在任何后端检测、CLI 子命令或 Tauri 线程启动前恢复终端使用的 PATH。
pub fn init_process_path() {
    #[cfg(windows)]
    init_windows_process_path();
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

/// 启动后在独立线程刷新一次 Windows 环境块。失败静默忽略，不阻塞应用初始化。
#[cfg(windows)]
pub(crate) fn refresh_process_environment_in_background() {
    let _ = std::thread::Builder::new()
        .name("nova-env-refresh".into())
        .spawn(|| {
            let _ = refresh_windows_process_environment();
        });
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
    let current = std::env::var_os("PATH");
    let fallback = fallback_windows_path(std::env::var_os("USERPROFILE").map(PathBuf::from));
    let mut groups = Vec::<&OsStr>::new();
    if let Some(path) = current.as_deref() {
        groups.push(path);
    }
    if let Some(path) = fallback.as_deref() {
        groups.push(path);
    }
    if let Some(path) = merge_paths(groups) {
        std::env::set_var("PATH", path);
    }
}

#[cfg(any(windows, test))]
fn fallback_windows_path(home: Option<PathBuf>) -> Option<OsString> {
    let home = home?;
    std::env::join_paths([home.join(".local/bin"), home.join(".opencode/bin")]).ok()
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

    // Preserve Nova's native CLI fallback directories when replacing the inherited Path.
    if let (Some(registry_path), Some(fallback)) = (
        std::env::var_os("PATH"),
        fallback_windows_path(std::env::var_os("USERPROFILE").map(PathBuf::from)),
    ) {
        if let Some(path) = merge_paths([registry_path.as_os_str(), fallback.as_os_str()]) {
            std::env::set_var("PATH", path);
        }
    }

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
    use super::{extract_marked_path, fallback_windows_path, merge_paths};
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
    fn windows_fallback_includes_claude_and_opencode_native_bins() {
        let home = PathBuf::from("C:/Users/professor");
        let fallback = fallback_windows_path(Some(home.clone())).unwrap();
        let paths: Vec<PathBuf> = std::env::split_paths(&fallback).collect();

        assert_eq!(
            paths,
            vec![home.join(".local/bin"), home.join(".opencode/bin")]
        );
    }
}
