//! Windows 后端通用 shell 微型 shim。
//!
//! 父进程的 `CREATE_NO_WINDOW` 不会强制继承给孙进程；Node `child_process` 补丁也覆盖不到
//! node-pty / ConPTY 等原生启动路径。构建时生成一个纯 std 的 GUI-subsystem helper，嵌入
//! Nova.exe，运行时按内容哈希释放，并以硬链接映射为 cmd/powershell/pwsh。主发布包仍是单文件。
#![cfg(windows)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use sha2::{Digest, Sha256};
use tauri::AppHandle;

const SHELL_SHIM_EXE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/nova-shell-shim.exe"));
const CMD_REAL: &str = "NOVA_SHELL_SHIM_CMD_REAL";
const POWERSHELL_REAL: &str = "NOVA_SHELL_SHIM_POWERSHELL_REAL";
const PWSH_REAL: &str = "NOVA_SHELL_SHIM_PWSH_REAL";
const BASH_REAL: &str = "NOVA_SHELL_SHIM_BASH_REAL";
const BASH_SHIM: &str = "NOVA_SHELL_SHIM_BASH";

#[derive(Clone)]
struct ShellShim {
    dir: PathBuf,
    cmd: PathBuf,
    powershell: PathBuf,
    pwsh: Option<PathBuf>,
    bash: Option<PathBuf>,
}

static SHELL_SHIM: OnceLock<Result<ShellShim, String>> = OnceLock::new();

fn system32() -> PathBuf {
    std::env::var_os("SystemRoot")
        .or_else(|| std::env::var_os("windir"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
        .join("System32")
}

fn real_cmd() -> PathBuf {
    let path = system32().join("cmd.exe");
    if path.is_file() {
        path
    } else {
        crate::acp::resolve_program_on_path("cmd").unwrap_or(path)
    }
}

pub(crate) fn real_powershell() -> PathBuf {
    let path = system32()
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe");
    if path.is_file() {
        path
    } else {
        crate::acp::resolve_program_on_path("powershell").unwrap_or(path)
    }
}

fn find_executable_on_path(name: &str, path: &std::ffi::OsStr) -> Option<PathBuf> {
    std::env::split_paths(path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

fn find_bash_on_path(path: &std::ffi::OsStr) -> Option<PathBuf> {
    // 先尊重应用启动环境中的 PATH。Git for Windows 通常只把 Git\cmd（git.exe）
    // 加入 PATH，因此 bash.exe 不一定能以裸命令找到；这种情况再从 git.exe 反推 Git 根目录。
    find_executable_on_path("bash.exe", path).or_else(|| {
        find_executable_on_path("git.exe", path).and_then(|git| {
            let bash = git.parent()?.parent()?.join("bin").join("bash.exe");
            bash.is_file().then_some(bash)
        })
    })
}

fn real_bash(launch_env: &HashMap<String, String>) -> Option<PathBuf> {
    let launch_path = launch_env
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("PATH"))
        .map(|(_, value)| std::ffi::OsStr::new(value));

    launch_path
        .and_then(find_bash_on_path)
        .or_else(|| {
            std::env::var_os("PATH")
                .as_deref()
                .and_then(find_bash_on_path)
        })
        .or_else(|| {
            ["ProgramFiles", "ProgramFiles(x86)"]
                .into_iter()
                .filter_map(std::env::var_os)
                .map(PathBuf::from)
                .map(|root| root.join("Git").join("bin").join("bash.exe"))
                .find(|path| path.is_file())
        })
}

fn content_key() -> String {
    Sha256::digest(SHELL_SHIM_EXE)[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn write_helper(dir: &Path) -> Result<PathBuf, String> {
    let helper = dir.join("nova-shell-shim.exe");
    if std::fs::read(&helper).ok().as_deref() != Some(SHELL_SHIM_EXE) {
        let temp = dir.join(format!("nova-shell-shim-{}.tmp", std::process::id()));
        std::fs::write(&temp, SHELL_SHIM_EXE)
            .map_err(|e| format!("写入 Windows shell shim 临时文件失败：{e}"))?;
        let _ = std::fs::remove_file(&helper);
        std::fs::rename(&temp, &helper).map_err(|e| {
            let _ = std::fs::remove_file(&temp);
            format!("安装 Windows shell shim 失败：{e}")
        })?;
    }
    Ok(helper)
}

fn ensure_alias(helper: &Path, dir: &Path, name: &str) -> Result<(), String> {
    let dest = dir.join(name);
    if std::fs::read(&dest).ok().as_deref() == Some(SHELL_SHIM_EXE) {
        return Ok(());
    }
    let _ = std::fs::remove_file(&dest);
    if std::fs::hard_link(helper, &dest).is_err() {
        std::fs::copy(helper, &dest)
            .map_err(|e| format!("创建 Windows shell shim {name} 失败：{e}"))?;
    }
    Ok(())
}

fn init(app: &AppHandle, launch_env: &HashMap<String, String>) -> Result<ShellShim, String> {
    let dir = crate::nova_data_dir(app)
        .join("runtime")
        .join("windows-shell-shim")
        .join(content_key());
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建 Windows shell shim 目录失败：{e}"))?;
    let helper = write_helper(&dir)?;
    ensure_alias(&helper, &dir, "cmd.exe")?;
    ensure_alias(&helper, &dir, "powershell.exe")?;

    let pwsh = crate::acp::resolve_program_on_path("pwsh");
    if pwsh.is_some() {
        ensure_alias(&helper, &dir, "pwsh.exe")?;
    }
    // Alkaid 的命令工具直接用绝对路径启动 Git Bash，单纯覆盖 PATH 无法拦截。
    // 为它额外提供 bash helper 路径，并保留探测到的真实 bash 以避免递归。
    let bash = real_bash(launch_env);
    if bash.is_some() {
        ensure_alias(&helper, &dir, "bash.exe")?;
    }
    Ok(ShellShim {
        dir,
        cmd: real_cmd(),
        powershell: real_powershell(),
        pwsh,
        bash,
    })
}

/// 给所有 Windows 后端前置微型 shell helper。PATH 覆盖原生裸名启动，ComSpec 覆盖
/// `shell:true` / cross-spawn；真实 shell 使用绝对路径，避免递归。
pub(crate) fn apply(
    app: &AppHandle,
    command: &mut tokio::process::Command,
    launch_env: &HashMap<String, String>,
) -> Result<(), String> {
    let shim = SHELL_SHIM.get_or_init(|| init(app, launch_env)).clone()?;
    command.env(CMD_REAL, &shim.cmd);
    command.env(POWERSHELL_REAL, &shim.powershell);
    if let Some(pwsh) = shim.pwsh.as_ref() {
        command.env(PWSH_REAL, pwsh);
    }
    if let Some(bash) = shim.bash.as_ref() {
        command.env(BASH_REAL, bash);
        command.env(BASH_SHIM, shim.dir.join("bash.exe"));
    }

    let base_path = launch_env
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("PATH"))
        .map(|(_, value)| value.clone())
        .or_else(|| std::env::var("PATH").ok())
        .unwrap_or_default();
    let mut paths = vec![shim.dir.clone()];
    paths.extend(std::env::split_paths(&base_path));
    let joined = std::env::join_paths(paths)
        .map_err(|e| format!("拼接 Windows shell shim PATH 失败：{e}"))?;
    command.env("PATH", joined);
    command.env("ComSpec", shim.dir.join("cmd.exe"));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_git_bash_from_path_before_fixed_install_locations() {
        let root = std::env::temp_dir().join(format!("nova-git-path-{}", uuid::Uuid::new_v4()));
        let cmd = root.join("cmd");
        let bin = root.join("bin");
        std::fs::create_dir_all(&cmd).unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(cmd.join("git.exe"), b"git").unwrap();
        std::fs::write(bin.join("bash.exe"), b"bash").unwrap();

        assert_eq!(
            find_bash_on_path(cmd.as_os_str()),
            Some(bin.join("bash.exe"))
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn embedded_helper_is_windows_executable() {
        assert!(SHELL_SHIM_EXE.starts_with(b"MZ"));
        assert!(SHELL_SHIM_EXE.len() < 2 * 1024 * 1024);
    }

    #[test]
    fn helper_aliases_share_one_payload() {
        let root = std::env::temp_dir().join(format!("nova-shell-shim-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let helper = write_helper(&root).unwrap();
        ensure_alias(&helper, &root, "cmd.exe").unwrap();
        ensure_alias(&helper, &root, "powershell.exe").unwrap();
        ensure_alias(&helper, &root, "bash.exe").unwrap();
        assert_eq!(std::fs::read(root.join("cmd.exe")).unwrap(), SHELL_SHIM_EXE);
        assert_eq!(
            std::fs::read(root.join("powershell.exe")).unwrap(),
            SHELL_SHIM_EXE
        );
        assert_eq!(
            std::fs::read(root.join("bash.exe")).unwrap(),
            SHELL_SHIM_EXE
        );
        std::fs::remove_dir_all(root).ok();
    }
}
