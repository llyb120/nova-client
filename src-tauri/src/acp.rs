//! Shared runtime events and subprocess utilities. No standalone ACP backend.

pub const EV_UPDATE: &str = "acp:update";
pub const EV_TURN: &str = "acp:turn";
pub const EV_PERMISSION: &str = "acp:permission";
pub const EV_PERMISSION_RESOLVED: &str = "acp:permission-resolved";
pub const EV_LOG: &str = "acp:log";
pub const EV_THREADS: &str = "threads:changed";
pub const EV_TITLE_GENERATED: &str = "threads:title-generated";
pub const EV_OPTIONS: &str = "acp:options";
pub const EV_NOTIFY_OPEN: &str = "acp:notify-open";

/// Windows：杀掉以 `pid` 为根的整棵进程树（含 agent 经 cmd 垫片拉起的 node、
/// 以及 agent 执行工具调用时 spawn 的 shell 等所有后代）。
///
/// 用 Win32 原生 API（Toolhelp 快照建父子表 + TerminateProcess，子进程先杀、根最后杀），
/// **不再外挂 taskkill.exe 子进程**。原因：GUI（windows 子系统）进程在「正在退出」时再去启动
/// 控制台子进程（taskkill 是 CUI 程序）会触发控制台初始化失败，弹出
/// 「taskkill.exe - 应用程序错误 0xc0000142」。原生调用无需拉起任何子进程，退出期也稳定，
/// 且同步完成，保证「退出/升级/取消」时清理干净、不残留孤儿。
#[cfg(windows)]
pub(crate) fn kill_process_tree(pid: u32) {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32First, Process32Next, PROCESSENTRY32, TH32CS_SNAPPROCESS,
    };

    unsafe {
        // 1) 快照全部进程，建 (pid, ppid) 表。失败则至少杀掉根进程。
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            terminate_pid(pid);
            return;
        }
        let mut pairs: Vec<(u32, u32)> = Vec::new();
        let mut entry: PROCESSENTRY32 = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32>() as u32;
        if Process32First(snapshot, &mut entry) != 0 {
            loop {
                pairs.push((entry.th32ProcessID, entry.th32ParentProcessID));
                if Process32Next(snapshot, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snapshot);

        // 2) 广度优先收集以 pid 为根的整棵树。
        let mut tree = vec![pid];
        let mut i = 0;
        while i < tree.len() {
            let cur = tree[i];
            for &(p, pp) in &pairs {
                if pp == cur && p != 0 && !tree.contains(&p) {
                    tree.push(p);
                }
            }
            i += 1;
        }

        // 3) 子进程先杀、根最后杀，尽量避免中间态再拉起新子进程。
        for &p in tree.iter().rev() {
            terminate_pid(p);
        }
        // TerminateProcess 只是发终止请求。退出/升级路径会紧接着拉起新进程，
        // 这里同步等一小段时间，避免旧 agent/shell 仍处于 terminating 状态时被误认为残留。
        for &p in &tree {
            wait_pid_exit(p, 800);
        }
    }
}

/// 强制结束单个进程（找不到/已退出则忽略）。
#[cfg(windows)]
unsafe fn terminate_pid(pid: u32) {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};
    let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
    if !handle.is_null() {
        TerminateProcess(handle, 1);
        CloseHandle(handle);
    }
}

#[cfg(windows)]
fn wait_pid_exit(pid: u32, timeout_ms: u32) {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, WaitForSingleObject};
    const SYNCHRONIZE: u32 = 0x0010_0000;
    let handle = unsafe { OpenProcess(SYNCHRONIZE, 0, pid) };
    if !handle.is_null() {
        unsafe {
            let _ = WaitForSingleObject(handle, timeout_ms);
            CloseHandle(handle);
        }
    }
}

#[cfg(not(windows))]
pub(crate) fn kill_process_tree(pid: u32) {
    // spawn 时 process_group(0)：整组 SIGKILL（根 pid == pgid）
    #[cfg(unix)]
    unsafe {
        let _ = libc::kill(-(pid as i32), libc::SIGKILL);
    }

    fn collect_children(parent: u32, out: &mut Vec<u32>) {
        let Ok(output) = std::process::Command::new("pgrep")
            .args(["-P", &parent.to_string()])
            .output()
        else {
            return;
        };
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            if let Ok(child) = line.trim().parse::<u32>() {
                if child != 0 && !out.contains(&child) {
                    collect_children(child, out);
                    out.push(child);
                }
            }
        }
    }

    let mut tree = Vec::new();
    collect_children(pid, &mut tree);
    tree.push(pid);
    for &p in tree.iter().rev() {
        #[cfg(unix)]
        unsafe {
            let _ = libc::kill(p as i32, libc::SIGKILL);
        }
        #[cfg(not(unix))]
        let _ = p;
    }
}

/// 把 agent 连接进程挂进全局 Job 对象（KILL_ON_JOB_CLOSE）作兜底：
/// Nova 无论正常退出、崩溃还是被任务管理器强杀，内核都会随句柄关闭自动终结整棵
/// agent 进程树——包括经 cmd 垫片间接拉起、垫片先退导致 taskkill/快照法漏杀的后代。
/// 主动 kill 路径仍走 kill_process_tree，
/// Job 只兜「应用整个生命周期结束」这一层，且各连接互不影响。
#[cfg(windows)]
pub(crate) fn assign_to_agent_job(child: &tokio::process::Child) {
    use std::sync::OnceLock;
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    // HANDLE 本质是指针，不跨进程传递，仅进程内单例持有，Send/Sync 安全
    struct JobHandle(isize);
    unsafe impl Send for JobHandle {}
    unsafe impl Sync for JobHandle {}

    static JOB: OnceLock<Option<JobHandle>> = OnceLock::new();
    let job = JOB.get_or_init(|| unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            return None;
        }
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let ok = SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const std::ffi::c_void,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        );
        if ok == 0 {
            // 配置失败就不挂 Job（句柄随进程退出回收），行为退回原状
            return None;
        }
        Some(JobHandle(job as isize))
    });
    if let (Some(job), Some(handle)) = (job.as_ref(), child.raw_handle()) {
        unsafe {
            AssignProcessToJobObject(job.0 as _, handle as _);
        }
    }
}

#[cfg(not(windows))]
pub(crate) fn assign_to_agent_job(_child: &tokio::process::Child) {}

/// Windows：在 PATH 中按 exe/cmd/bat 顺序解析裸命令名为具体文件路径。
/// 带路径分隔符的输入视为具体文件；仅带扩展名的裸文件名仍需搜索 PATH。
/// 也被「后端可用性检查」复用：零成本判断某个 CLI 是否安装（不拉起进程）。
#[cfg(windows)]
pub(crate) fn resolve_program_on_path(name: &str) -> Option<std::path::PathBuf> {
    use std::path::{Path, PathBuf};
    if name.contains('\\') || name.contains('/') {
        let p = PathBuf::from(name);
        return p.is_file().then_some(p);
    }
    if Path::new(name).extension().is_some() {
        let p = PathBuf::from(name);
        if p.is_file() {
            return Some(p);
        }
        return std::env::var_os("PATH").and_then(|paths| {
            std::env::split_paths(&paths)
                .map(|dir| dir.join(name))
                .find(|p| p.is_file())
        });
    }
    let exts = ["exe", "cmd", "bat"];
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths).find_map(|dir| {
            exts.iter()
                .map(|ext| dir.join(format!("{name}.{ext}")))
                .find(|p| p.is_file())
        })
    })
}

/// 非 Windows：在 PATH 中解析裸命令名（带路径分隔符的输入视为具体文件）。
#[cfg(not(windows))]
pub(crate) fn resolve_program_on_path(name: &str) -> Option<std::path::PathBuf> {
    use std::path::PathBuf;
    if name.contains('/') {
        let p = PathBuf::from(name);
        return p.is_file().then_some(p);
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths).find_map(|dir| {
            let p = dir.join(name);
            p.is_file().then_some(p)
        })
    })
}

#[cfg(test)]
mod shared_tests {
    use super::resolve_program_on_path;

    #[test]
    fn explicit_paths_preserve_shared_cli_resolution() {
        let executable = std::env::current_exe().unwrap();
        let configured = executable.to_str().unwrap();
        assert_eq!(resolve_program_on_path(configured), Some(executable.clone()));
        let directory = executable.parent().unwrap();
        assert!(resolve_program_on_path(directory.to_str().unwrap()).is_none());
        let missing = directory.join("nova-missing-cli-for-path-test");
        assert!(resolve_program_on_path(missing.to_str().unwrap()).is_none());
    }
}
