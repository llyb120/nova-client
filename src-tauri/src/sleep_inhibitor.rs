use std::collections::HashSet;
use std::sync::{mpsc, Mutex};

/// 在专用线程上创建和销毁系统唤醒锁。
///
/// Windows 的 SetThreadExecutionState 是线程级 API，因此不能让唤醒锁随着
/// Tauri 异步任务在线程池间移动。Linux 上创建 inhibitor 也可能包含阻塞的
/// D-Bus 调用；统一放到这里可避免阻塞会话的流式处理。
pub struct SleepInhibitor {
    inner: Mutex<Inner>,
}

struct Inner {
    running_threads: HashSet<String>,
    tx: mpsc::Sender<bool>,
}

impl SleepInhibitor {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("nova-sleep-inhibitor".into())
            .spawn(move || {
                let mut awake: Option<keepawake::KeepAwake> = None;
                while let Ok(should_inhibit) = rx.recv() {
                    if should_inhibit && awake.is_none() {
                        match keepawake::Builder::default()
                            .idle(true)
                            .reason("Nova has running sessions")
                            .app_name("Nova")
                            .app_reverse_domain("com.nova.desktop")
                            .create()
                        {
                            Ok(handle) => awake = Some(handle),
                            Err(error) => {
                                eprintln!("[sleep] failed to inhibit system sleep: {error}")
                            }
                        }
                    } else if !should_inhibit {
                        awake = None;
                    }
                }
                // `awake` is dropped on the same thread that created it. This is required on
                // Windows and also releases the platform inhibitor during application exit.
            })
            .expect("failed to start sleep inhibitor thread");

        Self {
            inner: Mutex::new(Inner {
                running_threads: HashSet::new(),
                tx,
            }),
        }
    }

    /// Tracks a logical session idempotently. Only the empty/non-empty transitions touch the OS.
    pub fn set_running(&self, thread_id: &str, running: bool) {
        let mut inner = self.inner.lock().unwrap();
        let was_active = !inner.running_threads.is_empty();
        if running {
            inner.running_threads.insert(thread_id.to_string());
        } else {
            inner.running_threads.remove(thread_id);
        }
        let is_active = !inner.running_threads.is_empty();
        if was_active != is_active {
            let _ = inner.tx.send(is_active);
        }
    }
}
