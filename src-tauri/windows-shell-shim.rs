#![cfg(windows)]
#![windows_subsystem = "windows"]

use std::ffi::{c_void, OsString};
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::process::exit;
use std::ptr::{null, null_mut};

const CMD_REAL: &str = "NOVA_SHELL_SHIM_CMD_REAL";
const POWERSHELL_REAL: &str = "NOVA_SHELL_SHIM_POWERSHELL_REAL";
const PWSH_REAL: &str = "NOVA_SHELL_SHIM_PWSH_REAL";
const BASH_REAL: &str = "NOVA_SHELL_SHIM_BASH_REAL";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const STARTF_USESHOWWINDOW: u32 = 0x0000_0001;
const STARTF_USESTDHANDLES: u32 = 0x0000_0100;
const STD_INPUT_HANDLE: u32 = -10i32 as u32;
const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
const STD_ERROR_HANDLE: u32 = -12i32 as u32;
const WAIT_FAILED: u32 = 0xffff_ffff;
const INFINITE: u32 = 0xffff_ffff;

type Handle = *mut c_void;

#[repr(C)]
struct StartupInfoW {
    cb: u32,
    reserved: *mut u16,
    desktop: *mut u16,
    title: *mut u16,
    x: u32,
    y: u32,
    x_size: u32,
    y_size: u32,
    x_count_chars: u32,
    y_count_chars: u32,
    fill_attribute: u32,
    flags: u32,
    show_window: u16,
    reserved2_size: u16,
    reserved2: *mut u8,
    stdin: Handle,
    stdout: Handle,
    stderr: Handle,
}

#[repr(C)]
struct ProcessInformation {
    process: Handle,
    thread: Handle,
    process_id: u32,
    thread_id: u32,
}

#[link(name = "kernel32")]
extern "system" {
    fn GetCommandLineW() -> *const u16;
    fn GetConsoleCP() -> u32;
    fn GetStdHandle(handle: u32) -> Handle;
    fn CreateProcessW(
        application_name: *const u16,
        command_line: *mut u16,
        process_attributes: *mut c_void,
        thread_attributes: *mut c_void,
        inherit_handles: i32,
        creation_flags: u32,
        environment: *mut c_void,
        current_directory: *const u16,
        startup_info: *mut StartupInfoW,
        process_information: *mut ProcessInformation,
    ) -> i32;
    fn WaitForSingleObject(handle: Handle, milliseconds: u32) -> u32;
    fn GetExitCodeProcess(process: Handle, exit_code: *mut u32) -> i32;
    fn CloseHandle(handle: Handle) -> i32;
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
    } else if stem.eq_ignore_ascii_case("bash") {
        Some(BASH_REAL)
    } else {
        None
    }
}

fn quote_program(program: &std::ffi::OsStr) -> Vec<u16> {
    let mut quoted = vec![b'"' as u16];
    quoted.extend(program.encode_wide());
    quoted.push(b'"' as u16);
    quoted
}

fn run_hidden(real_shell: &std::ffi::OsStr, tail: &std::ffi::OsStr) -> Option<u32> {
    let mut application: Vec<u16> = real_shell.encode_wide().chain(std::iter::once(0)).collect();
    let mut command_line = quote_program(real_shell);
    if !tail.is_empty() {
        command_line.push(b' ' as u16);
        command_line.extend(tail.encode_wide());
    }
    command_line.push(0);

    let mut startup: StartupInfoW = unsafe { zeroed() };
    startup.cb = size_of::<StartupInfoW>() as u32;
    // CREATE_NO_WINDOW alone stops console allocation, but does not populate
    // STARTUPINFO. Git Bash can consequently activate its initial hidden window
    // and take focus. Propagate the parent's windowsHide intent explicitly.
    startup.flags = STARTF_USESHOWWINDOW | STARTF_USESTDHANDLES;
    startup.show_window = 0; // SW_HIDE
    startup.stdin = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
    startup.stdout = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
    startup.stderr = unsafe { GetStdHandle(STD_ERROR_HANDLE) };
    let mut process: ProcessInformation = unsafe { zeroed() };
    let flags = if unsafe { GetConsoleCP() } == 0 {
        CREATE_NO_WINDOW
    } else {
        0
    };
    let created = unsafe {
        CreateProcessW(
            application.as_mut_ptr(),
            command_line.as_mut_ptr(),
            null_mut(),
            null_mut(),
            1,
            flags,
            null_mut(),
            null(),
            &mut startup,
            &mut process,
        )
    };
    if created == 0 {
        return None;
    }
    unsafe { CloseHandle(process.thread) };
    let waited = unsafe { WaitForSingleObject(process.process, INFINITE) };
    let mut code = 1;
    let got_code =
        waited != WAIT_FAILED && unsafe { GetExitCodeProcess(process.process, &mut code) } != 0;
    unsafe { CloseHandle(process.process) };
    got_code.then_some(code)
}

fn main() {
    let Some(env_name) = real_shell_env() else {
        exit(1);
    };
    let Some(real_shell) = std::env::var_os(env_name) else {
        exit(1);
    };

    exit(run_hidden(&real_shell, &command_line_tail()).unwrap_or(1) as i32);
}
