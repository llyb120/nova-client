// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(windows)]
const NOWINDOW_WRAPPER_MARKER: &str = "NOVA_NOWINDOW_WRAPPER";
#[cfg(windows)]
const NOWINDOW_PWSH_REAL: &str = "NOVA_NOWINDOW_PWSH_REAL";
#[cfg(windows)]
const NOWINDOW_POWERSHELL_REAL: &str = "NOVA_NOWINDOW_POWERSHELL_REAL";
#[cfg(windows)]
const NOWINDOW_CMD_REAL: &str = "NOVA_NOWINDOW_CMD_REAL";
#[cfg(windows)]
const NOWINDOW_REAL_PREFIX: &str = "NOVA_NOWINDOW_REAL_";
#[cfg(windows)]
const RESTART_PARENT_PID: &str = "NOVA_RESTART_PARENT_PID";
#[cfg(windows)]
const SINGLE_INSTANCE_MUTEX: &str = "Local\\NovaDesktopSingleInstanceMutex";
#[cfg(windows)]
const FOCUS_EVENT: &str = "Local\\NovaDesktopFocusEvent";

#[cfg(windows)]
fn nowindow_real_env(stem: &str) -> String {
    let key: String = stem
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("{NOWINDOW_REAL_PREFIX}{key}")
}

#[cfg(windows)]
fn run_nowindow_shell_wrapper() -> bool {
    use std::ffi::OsString;
    use std::io::{self, Write};
    use std::os::windows::process::CommandExt;
    use std::path::PathBuf;
    use std::process::{exit, Command, Stdio};
    use std::thread;

    if std::env::var(NOWINDOW_WRAPPER_MARKER).ok().as_deref() != Some("1") {
        return false;
    }

    let Ok(current_exe) = std::env::current_exe() else {
        return false;
    };
    let Some(stem) = current_exe.file_stem().and_then(|s| s.to_str()) else {
        return false;
    };
    let lower_stem = stem.to_ascii_lowercase();
    let real = match lower_stem.as_str() {
        "pwsh" => std::env::var_os(NOWINDOW_PWSH_REAL),
        "powershell" => std::env::var_os(NOWINDOW_POWERSHELL_REAL),
        "cmd" => std::env::var_os(NOWINDOW_CMD_REAL),
        _ => std::env::var_os(nowindow_real_env(&lower_stem)),
    };
    let Some(real_shell) = real.map(PathBuf::from) else {
        exit(1);
    };

    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let mut child = Command::new(real_shell);
    child
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(0x08000000);

    let Ok(mut child) = child.spawn() else {
        exit(1);
    };

    if let Some(mut child_stdin) = child.stdin.take() {
        thread::spawn(move || {
            let mut stdin = io::stdin();
            let _ = io::copy(&mut stdin, &mut child_stdin);
        });
    }

    let stdout_thread = child.stdout.take().map(|mut child_stdout| {
        thread::spawn(move || {
            let mut stdout = io::stdout();
            let _ = io::copy(&mut child_stdout, &mut stdout);
            let _ = stdout.flush();
        })
    });

    let stderr_thread = child.stderr.take().map(|mut child_stderr| {
        thread::spawn(move || {
            let mut stderr = io::stderr();
            let _ = io::copy(&mut child_stderr, &mut stderr);
            let _ = stderr.flush();
        })
    });

    let code = child
        .wait()
        .ok()
        .and_then(|status| status.code())
        .unwrap_or(1);
    if let Some(handle) = stdout_thread {
        let _ = handle.join();
    }
    if let Some(handle) = stderr_thread {
        let _ = handle.join();
    }
    exit(code);
}

#[cfg(windows)]
fn wait_for_restart_parent() {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, WaitForSingleObject};
    const SYNCHRONIZE: u32 = 0x0010_0000;

    let pid = std::env::var(RESTART_PARENT_PID)
        .ok()
        .and_then(|s| s.parse::<u32>().ok());
    std::env::remove_var(RESTART_PARENT_PID);
    let Some(pid) = pid else {
        return;
    };
    let handle = unsafe { OpenProcess(SYNCHRONIZE, 0, pid) };
    if !handle.is_null() {
        unsafe {
            let _ = WaitForSingleObject(handle, 10_000);
            CloseHandle(handle);
        }
    }
}

#[cfg(windows)]
struct SingleInstanceGuard {
    mutex: isize,
    focus_event: isize,
}

#[cfg(windows)]
impl Drop for SingleInstanceGuard {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::CloseHandle;
        unsafe {
            if self.focus_event != 0 {
                CloseHandle(self.focus_event as _);
            }
            if self.mutex != 0 {
                CloseHandle(self.mutex as _);
            }
        }
    }
}

#[cfg(windows)]
fn wide_null(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
fn signal_existing_gui_instance() {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{CreateEventW, SetEvent};
    use windows_sys::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow;
    const ASFW_ANY: u32 = 0xFFFF_FFFF;

    let name = wide_null(FOCUS_EVENT);
    let event = unsafe { CreateEventW(std::ptr::null(), 0, 0, name.as_ptr()) };
    if !event.is_null() {
        unsafe {
            let _ = AllowSetForegroundWindow(ASFW_ANY);
            let _ = SetEvent(event);
            CloseHandle(event);
        }
    }
}

#[cfg(windows)]
fn acquire_single_instance() -> Option<SingleInstanceGuard> {
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS,
    };
    use windows_sys::Win32::System::Threading::{CreateEventW, CreateMutexW};

    let mutex_name = wide_null(SINGLE_INSTANCE_MUTEX);
    let mutex = unsafe { CreateMutexW(std::ptr::null(), 1, mutex_name.as_ptr()) };
    if mutex.is_null() {
        let err = unsafe { GetLastError() };
        if err == ERROR_ACCESS_DENIED {
            signal_existing_gui_instance();
            return None;
        }
        return Some(SingleInstanceGuard {
            mutex: 0,
            focus_event: 0,
        });
    }
    let err = unsafe { GetLastError() };
    if err == ERROR_ALREADY_EXISTS || err == ERROR_ACCESS_DENIED {
        signal_existing_gui_instance();
        unsafe {
            CloseHandle(mutex);
        }
        return None;
    }

    let event_name = wide_null(FOCUS_EVENT);
    let focus_event = unsafe { CreateEventW(std::ptr::null(), 0, 0, event_name.as_ptr()) };
    if focus_event.is_null() {
        return Some(SingleInstanceGuard {
            mutex: mutex as isize,
            focus_event: 0,
        });
    }

    Some(SingleInstanceGuard {
        mutex: mutex as isize,
        focus_event: focus_event as isize,
    })
}

#[cfg(windows)]
fn should_enforce_single_instance() -> bool {
    !cfg!(debug_assertions)
}

#[cfg(target_os = "macos")]
struct SingleInstanceGuard {
    /// 保持打开以维持 flock；关闭后锁自动释放
    #[allow(dead_code)]
    file: std::fs::File,
}

#[cfg(target_os = "macos")]
fn signal_existing_gui_instance() {
    // 尝试通过 bundle id / 应用名激活已有实例
    let _ = std::process::Command::new("osascript")
        .args([
            "-e",
            "tell application \"System Events\" to set frontmost of first process whose name is \"Nova\" to true",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

#[cfg(target_os = "macos")]
fn acquire_single_instance() -> Option<SingleInstanceGuard> {
    use std::os::unix::io::AsRawFd;

    let lock_path = dirs_nova_lock_path();
    if let Some(parent) = lock_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let file = match std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .open(&lock_path)
    {
        Ok(f) => f,
        Err(_) => {
            // 锁文件不可用：不强制单实例，仍允许启动
            let fallback = std::env::temp_dir().join("nova-instance-nolock");
            match std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .open(fallback)
            {
                Ok(f) => return Some(SingleInstanceGuard { file: f }),
                Err(_) => {
                    return Some(SingleInstanceGuard {
                        // /dev/null 几乎总是可读
                        file: std::fs::File::open("/dev/null").ok()?,
                    });
                }
            }
        }
    };
    let fd = file.as_raw_fd();
    // LOCK_EX | LOCK_NB
    let rc = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
    if rc != 0 {
        signal_existing_gui_instance();
        return None;
    }
    Some(SingleInstanceGuard { file })
}

#[cfg(target_os = "macos")]
fn dirs_nova_lock_path() -> std::path::PathBuf {
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    home.join(".nova").join("nova.lock")
}

#[cfg(target_os = "macos")]
fn should_enforce_single_instance() -> bool {
    !cfg!(debug_assertions)
}

fn main() {
    #[cfg(windows)]
    if run_nowindow_shell_wrapper() {
        return;
    }

    // Finder / Dock 启动的 macOS .app 不继承终端 PATH。必须在任何 CLI 探测或
    // 后端线程启动前恢复，否则已安装的 codex、npx 等都会被误判为不可用。
    nova_lib::init_process_path();

    // 自更新 helper：新版 exe 被旧版拉起后，先替换旧 exe，再启动正式实例。
    if nova_lib::maybe_run_update_helper() {
        return;
    }

    #[cfg(windows)]
    wait_for_restart_parent();

    // 命令行子工具（如 `nova mem-search ...`，供数字员工 agent 调用）：命中即执行并退出，不启动 GUI。
    if nova_lib::maybe_run_cli() {
        return;
    }

    #[cfg(any(windows, target_os = "macos"))]
    let _single_instance = if should_enforce_single_instance() {
        match acquire_single_instance() {
            Some(guard) => Some(guard),
            None => return,
        }
    } else {
        None
    };

    nova_lib::run()
}
