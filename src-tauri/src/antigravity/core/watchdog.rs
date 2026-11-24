//! 双档 Prompt 不活跃看门狗：
//! - 无活跃工具且无挂起权限：10 分钟无任何 `session/update` 判定超时；
//! - 有活跃工具或挂起权限：自动切换至 60 分钟不活跃阈值；
//! - 每次收到更新或状态变化均重置计时，避免误杀合法长任务。

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;
use tokio::time::Instant;

pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
pub const DEFAULT_WORK_IDLE_TIMEOUT: Duration = Duration::from_secs(60 * 60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WatchdogConfig {
    pub idle_timeout: Duration,
    pub work_idle_timeout: Duration,
}

impl Default for WatchdogConfig {
    fn default() -> Self {
        Self {
            idle_timeout: DEFAULT_IDLE_TIMEOUT,
            work_idle_timeout: DEFAULT_WORK_IDLE_TIMEOUT,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WatchdogTimeout {
    pub timeout: Duration,
    pub had_active_work: bool,
}

struct WatchdogState {
    config: WatchdogConfig,
    active_tool_calls: HashSet<String>,
    pending_permissions: usize,
    last_activity: Instant,
}

impl WatchdogState {
    fn has_active_work(&self) -> bool {
        !self.active_tool_calls.is_empty() || self.pending_permissions > 0
    }

    fn current_timeout(&self) -> Duration {
        if self.has_active_work() {
            self.config.work_idle_timeout
        } else {
            self.config.idle_timeout
        }
    }
}

#[derive(Clone)]
pub struct PromptWatchdog {
    inner: Arc<Mutex<WatchdogState>>,
    notify: Arc<Notify>,
}

impl PromptWatchdog {
    pub fn new(config: WatchdogConfig) -> Self {
        Self {
            inner: Arc::new(Mutex::new(WatchdogState {
                config,
                active_tool_calls: HashSet::new(),
                pending_permissions: 0,
                last_activity: Instant::now(),
            })),
            notify: Arc::new(Notify::new()),
        }
    }

    pub fn default_timeouts() -> Self {
        Self::new(WatchdogConfig::default())
    }

    pub fn record_activity(&self) {
        if let Ok(mut st) = self.inner.lock() {
            st.last_activity = Instant::now();
        }
        self.notify.notify_one();
    }

    pub fn tool_started(&self, tool_call_id: impl Into<String>) {
        let id = tool_call_id.into();
        if let Ok(mut st) = self.inner.lock() {
            if !id.is_empty() {
                st.active_tool_calls.insert(id);
            }
            st.last_activity = Instant::now();
        }
        self.notify.notify_one();
    }

    pub fn tool_finished(&self, tool_call_id: &str) {
        if let Ok(mut st) = self.inner.lock() {
            st.active_tool_calls.remove(tool_call_id);
            st.last_activity = Instant::now();
        }
        self.notify.notify_one();
    }

    pub fn permission_started(&self) {
        if let Ok(mut st) = self.inner.lock() {
            st.pending_permissions = st.pending_permissions.saturating_add(1);
            st.last_activity = Instant::now();
        }
        self.notify.notify_one();
    }

    pub fn permission_finished(&self) {
        if let Ok(mut st) = self.inner.lock() {
            st.pending_permissions = st.pending_permissions.saturating_sub(1);
            st.last_activity = Instant::now();
        }
        self.notify.notify_one();
    }

    pub fn has_active_work(&self) -> bool {
        self.inner
            .lock()
            .map(|st| st.has_active_work())
            .unwrap_or(false)
    }

    pub fn active_tool_count(&self) -> usize {
        self.inner
            .lock()
            .map(|st| st.active_tool_calls.len())
            .unwrap_or(0)
    }

    pub fn current_timeout(&self) -> Duration {
        self.inner
            .lock()
            .map(|st| st.current_timeout())
            .unwrap_or(DEFAULT_IDLE_TIMEOUT)
    }

    /// 挂起直至连续不活跃时长超过当前档位阈值（10min 或 60min）。
    pub async fn wait_for_timeout(&self) -> WatchdogTimeout {
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            // 先注册通知再读状态，消除读锁释放到 select 之间的竞态窗口。
            notified.as_mut().enable();

            let (deadline, timeout, has_work) = {
                let st = self.inner.lock().unwrap();
                let timeout = st.current_timeout();
                (st.last_activity + timeout, timeout, st.has_active_work())
            };
            if Instant::now() >= deadline {
                return WatchdogTimeout {
                    timeout,
                    had_active_work: has_work,
                };
            }

            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => {
                    let st = self.inner.lock().unwrap();
                    let cur_timeout = st.current_timeout();
                    if Instant::now() >= st.last_activity + cur_timeout {
                        return WatchdogTimeout {
                            timeout: cur_timeout,
                            had_active_work: st.has_active_work(),
                        };
                    }
                }
                _ = notified => {
                    // 活跃时间或工具状态发生变化，重新计算 deadline
                }
            }
        }
    }
}
