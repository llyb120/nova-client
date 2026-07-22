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
    let fallback = fallback_windows_path(
        std::env::var_os("USERPROFILE").map(PathBuf::from),
        std::env::var_os("APPDATA").map(PathBuf::from),
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from),
        std::env::var_os("ProgramFiles").map(PathBuf::from),
        std::env::var_os("ProgramFiles(x86)").map(PathBuf::from),
    );
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

/// 读取注册表中的最新系统/用户 Path，仅供可执行文件解析兜底。
/// 不写回 Nova 的全局 PATH，避免异常或陈旧注册表项影响 WebView2、DLL 和 shell shim。
#[cfg(windows)]
pub(crate) fn windows_registry_paths() -> Vec<PathBuf> {
    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};

    let system = registry_path(
        HKEY_LOCAL_MACHINE,
        "SYSTEM\\CurrentControlSet\\Control\\Session Manager\\Environment",
    );
    let user = registry_path(HKEY_CURRENT_USER, "Environment");
    [system, user]
        .into_iter()
        .flatten()
        .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .collect()
}

#[cfg(windows)]
fn registry_path(
    root: windows_sys::Win32::System::Registry::HKEY,
    subkey: &str,
) -> Option<OsString> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::System::Registry::{RegGetValueW, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ};

    let subkey: Vec<u16> = subkey.encode_utf16().chain(std::iter::once(0)).collect();
    let value: Vec<u16> = "Path".encode_utf16().chain(std::iter::once(0)).collect();
    let flags = RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ;
    let mut bytes = 0u32;
    let status = unsafe {
        RegGetValueW(
            root,
            subkey.as_ptr(),
            value.as_ptr(),
            flags,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut bytes,
        )
    };
    if status != 0 || bytes < 2 {
        return None;
    }
    let mut buffer = vec![0u16; (bytes as usize + 1) / 2];
    let status = unsafe {
        RegGetValueW(
            root,
            subkey.as_ptr(),
            value.as_ptr(),
            flags,
            std::ptr::null_mut(),
            buffer.as_mut_ptr().cast(),
            &mut bytes,
        )
    };
    if status != 0 {
        return None;
    }
    let len = (bytes as usize / 2).min(buffer.len());
    buffer.truncate(len);
    while buffer.last() == Some(&0) {
        buffer.pop();
    }
    (!buffer.is_empty()).then(|| OsString::from_wide(&buffer))
}

#[cfg(any(windows, test))]
fn fallback_windows_path(
    home: Option<PathBuf>,
    app_data: Option<PathBuf>,
    local_app_data: Option<PathBuf>,
    program_files: Option<PathBuf>,
    program_files_x86: Option<PathBuf>,
) -> Option<OsString> {
    let mut paths = Vec::new();
    if let Some(home) = home {
        paths.push(home.join(".local/bin"));
        paths.push(home.join(".opencode/bin"));
    }
    // npm 的 Windows 全局 shim 默认在 %APPDATA%\npm。终端可能通过 profile
    // 临时补上它，但从开始菜单启动的 GUI 不会加载该 profile。
    if let Some(app_data) = app_data {
        paths.push(app_data.join("npm"));
    }
    if let Some(local_app_data) = local_app_data {
        paths.push(local_app_data.join("Programs/nodejs"));
    }
    for root in [program_files, program_files_x86].into_iter().flatten() {
        paths.push(root.join("nodejs"));
    }
    (!paths.is_empty())
        .then(|| std::env::join_paths(paths).ok())
        .flatten()
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
    fn windows_fallback_includes_common_cli_install_locations() {
        let home = PathBuf::from("C:/Users/professor");
        let app_data = home.join("AppData/Roaming");
        let local_app_data = home.join("AppData/Local");
        let program_files = PathBuf::from("C:/Program Files");
        let program_files_x86 = PathBuf::from("C:/Program Files (x86)");
        let fallback = fallback_windows_path(
            Some(home.clone()),
            Some(app_data.clone()),
            Some(local_app_data.clone()),
            Some(program_files.clone()),
            Some(program_files_x86.clone()),
        )
        .unwrap();
        let paths: Vec<PathBuf> = std::env::split_paths(&fallback).collect();

        assert_eq!(
            paths,
            vec![
                home.join(".local/bin"),
                home.join(".opencode/bin"),
                app_data.join("npm"),
                local_app_data.join("Programs/nodejs"),
                program_files.join("nodejs"),
                program_files_x86.join("nodejs"),
            ]
        );
    }
}
