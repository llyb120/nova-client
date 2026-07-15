#![cfg(windows)]
#![windows_subsystem = "windows"]

use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::os::windows::process::CommandExt;
use std::process::{exit, Command, Stdio};

const CMD_REAL: &str = "NOVA_SHELL_SHIM_CMD_REAL";
const POWERSHELL_REAL: &str = "NOVA_SHELL_SHIM_POWERSHELL_REAL";
const PWSH_REAL: &str = "NOVA_SHELL_SHIM_PWSH_REAL";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[link(name = "kernel32")]
extern "system" {
    fn GetCommandLineW() -> *const u16;
    fn GetConsoleCP() -> u32;
}

fn is_command_line_space(ch: u16) -> bool {
    ch == b' ' as u16 || ch == b'\t' as u16
}

fn command_line_tail() -> OsString {
    let ptr = unsafe { GetCommandLineW() };
    if ptr.is_null() {
        return OsString::new();
    }
    let mut len = 0usize;
    while unsafe { *ptr.add(len) } != 0 {
        len += 1;
    }
    let line = unsafe { std::slice::from_raw_parts(ptr, len) };
    let mut i = 0usize;
    while i < line.len() && is_command_line_space(line[i]) {
        i += 1;
    }
    if i < line.len() && line[i] == b'"' as u16 {
        i += 1;
        while i < line.len() && line[i] != b'"' as u16 {
            i += 1;
        }
        if i < line.len() {
            i += 1;
        }
    } else {
        while i < line.len() && !is_command_line_space(line[i]) {
            i += 1;
        }
    }
    while i < line.len() && is_command_line_space(line[i]) {
        i += 1;
    }
    OsString::from_wide(&line[i..])
}

fn real_shell_env() -> Option<&'static str> {
    let exe = std::env::current_exe().ok()?;
    let stem = exe.file_stem()?.to_string_lossy();
    if stem.eq_ignore_ascii_case("cmd") {
        Some(CMD_REAL)
    } else if stem.eq_ignore_ascii_case("powershell") {
        Some(POWERSHELL_REAL)
    } else if stem.eq_ignore_ascii_case("pwsh") {
        Some(PWSH_REAL)
    } else {
        None
    }
}

fn main() {
    let Some(env_name) = real_shell_env() else {
        exit(1);
    };
    let Some(real_shell) = std::env::var_os(env_name) else {
        exit(1);
    };

    let mut command = Command::new(real_shell);
    let tail = command_line_tail();
    if !tail.is_empty() {
        command.raw_arg(tail);
    }
    command
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    // ConPTY 内保留真实 shell 的终端语义；普通 GUI/pipe 路径才禁止创建控制台窗口。
    if unsafe { GetConsoleCP() } == 0 {
        command.creation_flags(CREATE_NO_WINDOW);
    }

    let code = command
        .status()
        .ok()
        .and_then(|status| status.code())
        .unwrap_or(1);
    exit(code);
}
