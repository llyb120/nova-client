//! Windows「无窗口 shim」启用端。
//!
//! 后端 CLI（Cursor / CodeBuddy 等，多为 Node 程序）在跑 shell 工具命令、或拉起
//! `*-svc.cmd` 之类脚本时，可能绕过 `child_process`（走 node-pty / ConPTY / 直接
//! CreateProcess），此时 `cursor-windows-hide.cjs` 的 `windowsHide:true` 补丁覆盖不到，
//! 于是控制台一闪。
//!
//! 解决办法：把 nova.exe 复制成 `cmd.exe` / `powershell.exe` / `pwsh.exe` 放进一个私有
//! 目录，并把该目录塞到子进程 PATH 最前 + 覆盖 `ComSpec`。子进程按裸名拉起 shell 时命中
//! 这些 shim；nova.exe 是 GUI 子系统程序（release 下 `windows_subsystem="windows"`），
//! 被拉起时**不分配控制台**，随后它读取 `NOVA_NOWINDOW_*_REAL` 用 `CREATE_NO_WINDOW`
//! 重新拉起真实 shell（见 main.rs::run_nowindow_shell_wrapper），全程无窗口。
#![cfg(windows)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use tauri::AppHandle;

// 下列环境变量名必须与 main.rs 中 NOWINDOW_* 常量一致（main.rs 是 shim 的消费端）。
const WRAPPER_MARKER: &str = "NOVA_NOWINDOW_WRAPPER";
const PWSH_REAL: &str = "NOVA_NOWINDOW_PWSH_REAL";
const POWERSHELL_REAL: &str = "NOVA_NOWINDOW_POWERSHELL_REAL";
const CMD_REAL: &str = "NOVA_NOWINDOW_CMD_REAL";

/// shim 目录只在首次使用时建好，之后复用（复制 nova.exe 有一定开销）。
static SHIM_DIR: OnceLock<Result<PathBuf, String>> = OnceLock::new();

fn system32() -> PathBuf {
    std::env::var_os("SystemRoot")
        .or_else(|| std::env::var_os("windir"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
        .join("System32")
}

fn real_cmd() -> PathBuf {
    let p = system32().join("cmd.exe");
    if p.is_file() {
        p
    } else {
        crate::acp::resolve_program_on_path("cmd").unwrap_or(p)
    }
}

fn real_powershell() -> PathBuf {
    let p = system32()
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe");
    if p.is_file() {
        p
    } else {
        crate::acp::resolve_program_on_path("powershell").unwrap_or(p)
    }
}

/// PowerShell 7（可选，未安装则不注入对应 *_REAL）。
fn real_pwsh() -> Option<PathBuf> {
    crate::acp::resolve_program_on_path("pwsh")
}

/// 确保 `dir/name` 指向当前 nova.exe（内容一致即可）。用文件大小做「是否需刷新」的廉价判断
///（跨版本 exe 大小几乎必变）。优先硬链接（同卷即时、零额外空间），失败回退整份复制。
fn ensure_shim(dir: &Path, name: &str, exe: &Path, exe_len: u64) -> Result<(), String> {
    let dest = dir.join(name);
    if std::fs::metadata(&dest).ok().map(|m| m.len()) == Some(exe_len) {
        return Ok(());
    }
    // 需刷新（首建或版本变化）。旧 shim 可能正被存活子进程占用，删除失败就忽略。
    let _ = std::fs::remove_file(&dest);
    if std::fs::hard_link(exe, &dest).is_err() {
        let _ = std::fs::copy(exe, &dest);
    }
    // 刷新失败但仍有可用的旧 shim（被占用等）时退用旧的，不阻塞后端启动。
    if std::fs::metadata(&dest).is_ok() {
        Ok(())
    } else {
        Err(format!("写入无窗口 shim {name} 失败"))
    }
}

fn init_shim_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("获取 nova.exe 路径失败：{e}"))?;
    let exe_len = std::fs::metadata(&exe).map(|m| m.len()).unwrap_or(0);
    let dir = crate::nova_data_dir(app)
        .join("runtime")
        .join("nowindow-shims");
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建无窗口 shim 目录失败：{e}"))?;
    for name in ["cmd.exe", "powershell.exe", "pwsh.exe"] {
        ensure_shim(&dir, name, &exe, exe_len)?;
    }
    Ok(dir)
}

fn shim_dir(app: &AppHandle) -> Result<PathBuf, String> {
    SHIM_DIR.get_or_init(|| init_shim_dir(app)).clone()
}

/// 给即将 spawn 的子进程注入无窗口 shim 环境：shim 目录置于 PATH 最前、覆盖 `ComSpec`、
/// 并用 `NOVA_NOWINDOW_*_REAL` 指向真实 shell 的绝对路径（供 shim 重新拉起，不再经 PATH，
/// 避免自我递归）。`launch_env` 为该实例已有的环境（可能含独立 PATH），在其基础上前插。
///
/// 必须在 `cmd.envs(launch_env)` 之后调用，确保这里对 PATH / ComSpec 的覆盖生效。
pub fn apply(
    app: &AppHandle,
    cmd: &mut tokio::process::Command,
    launch_env: &HashMap<String, String>,
) -> Result<(), String> {
    let dir = shim_dir(app)?;

    cmd.env(WRAPPER_MARKER, "1");
    cmd.env(CMD_REAL, real_cmd());
    cmd.env(POWERSHELL_REAL, real_powershell());
    if let Some(pwsh) = real_pwsh() {
        cmd.env(PWSH_REAL, pwsh);
    }

    let base_path = launch_env
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("PATH"))
        .map(|(_, v)| v.clone())
        .or_else(|| std::env::var("PATH").ok())
        .unwrap_or_default();
    let mut paths = vec![dir.clone()];
    paths.extend(std::env::split_paths(&base_path));
    let joined = std::env::join_paths(paths).map_err(|e| format!("拼接无窗口 shim PATH 失败：{e}"))?;
    cmd.env("PATH", joined);

    // node 的 `shell:true` 用 ComSpec 起 cmd；指向 shim 让这条路径也无窗口。
    cmd.env("ComSpec", dir.join("cmd.exe"));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_var_names_match_wrapper_contract() {
        // 必须与 main.rs 的 NOWINDOW_* 常量字面量一致，否则 shim 读不到真实 shell 路径而退出。
        assert_eq!(WRAPPER_MARKER, "NOVA_NOWINDOW_WRAPPER");
        assert_eq!(CMD_REAL, "NOVA_NOWINDOW_CMD_REAL");
        assert_eq!(POWERSHELL_REAL, "NOVA_NOWINDOW_POWERSHELL_REAL");
        assert_eq!(PWSH_REAL, "NOVA_NOWINDOW_PWSH_REAL");
    }

    #[test]
    fn ensure_shim_creates_then_is_idempotent() {
        let root = std::env::temp_dir().join(format!("nova-nowindow-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let exe = root.join("nova.exe");
        std::fs::write(&exe, b"fake-nova-exe-bytes").unwrap();
        let exe_len = std::fs::metadata(&exe).unwrap().len();

        ensure_shim(&root, "cmd.exe", &exe, exe_len).unwrap();
        let dest = root.join("cmd.exe");
        assert_eq!(std::fs::metadata(&dest).unwrap().len(), exe_len);

        // 大小一致 → 第二次直接跳过刷新，文件仍在。
        ensure_shim(&root, "cmd.exe", &exe, exe_len).unwrap();
        assert!(dest.is_file());

        std::fs::remove_dir_all(&root).ok();
    }
}
