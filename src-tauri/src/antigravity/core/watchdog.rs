//! 三档 Prompt 进展看门狗：
//! - 首条执行进展前：3 分钟（会话元数据、空文本块不重置计时，避免静默卡死干等 10 分钟）；
//! - 已有进展且无活跃工具/权限：5 分钟（避开 `agy` 内部 10m0s `stream idle timeout` 撞车）；
//! - 有活跃工具或挂起权限：自动切换至 60 分钟，避免误杀编译、测试等合法长任务。

use serde_json::Value;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;
use tokio::time::Instant;

pub const FIRST_PROGRESS_TIMEOUT: Duration = Duration::from_secs(3 * 60);
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
pub const DEFAULT_WORK_IDLE_TIMEOUT: Duration = Duration::from_secs(60 * 60);
/// `agy` 内部单次网络退避重试最多可获得的额外宽限预算（防止无限重试循环永久续命）。
const MAX_RETRY_GRACE_BUDGET: Duration = Duration::from_secs(90);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WatchdogConfig {
    pub first_progress_timeout: Duration,
    pub idle_timeout: Duration,
    pub work_idle_timeout: Duration,
}

impl Default for WatchdogConfig {
    fn default() -> Self {
        Self {
            first_progress_timeout: FIRST_PROGRESS_TIMEOUT,
            idle_timeout: DEFAULT_IDLE_TIMEOUT,
            work_idle_timeout: DEFAULT_WORK_IDLE_TIMEOUT,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WatchdogTimeout {
    pub timeout: Duration,
    pub had_active_work: bool,
    pub awaiting_first_progress: bool,
}

struct WatchdogState {
    config: WatchdogConfig,
    active_tool_calls: HashSet<String>,
    pending_permissions: usize,
    started_at: Instant,
    last_activity: Option<Instant>,
    retry_grace_used: Duration,
}

impl WatchdogState {
    fn has_active_work(&self) -> bool {
        !self.active_tool_calls.is_empty() || self.pending_permissions > 0
    }

    fn current_timeout(&self) -> Duration {
        if self.has_active_work() {
            self.config.work_idle_timeout
        } else if self.last_activity.is_none() {
            self.config.first_progress_timeout.min(self.config.idle_timeout)
        } else {
            self.config.idle_timeout
        }
    }

    fn reference_time(&self) -> Instant {
        self.last_activity.unwrap_or(self.started_at)
    }

    fn deadline(&self) -> Instant {
        let base = self.reference_time() + self.current_timeout();
        if self.has_active_work() {
            base
        } else {
            base + self.retry_grace_used
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
                started_at: Instant::now(),
                last_activity: None,
                retry_grace_used: Duration::ZERO,
            })),
            notify: Arc::new(Notify::new()),
        }
    }

    pub fn default_timeouts() -> Self {
        Self::new(WatchdogConfig::default())
    }

    pub fn awaiting_first_progress(&self) -> bool {
        self.inner
            .lock()
            .map(|st| st.last_activity.is_none())
            .unwrap_or(false)
    }

    /// 仅在收到实质执行进展时重置不活跃计时；元数据和空白块不重置。
    pub fn observe_update(&self, update: &Value) -> bool {
        let progress = match update["sessionUpdate"].as_str() {
            Some("agent_message_chunk" | "agent_thought_chunk") => {
                let block = &update["content"];
                match block["type"].as_str() {
                    Some("text") => block["text"].as_str().is_some_and(|s| !s.trim().is_empty()),
                    Some(_) => true,
                    None => block.as_str().is_some_and(|s| !s.trim().is_empty()),
                }
            }
            Some("plan" | "plan_update") => update["entries"]
                .as_array()
                .is_some_and(|entries| !entries.is_empty()),
            Some("compaction_update" | "compaction_summary_chunk" | "subagent_update") => true,
            // tool_call / tool_call_update 由 tool_started / tool_finished 精确登记
            _ => false,
        };
        if progress {
            self.record_activity();
        }
        progress
    }

    pub fn record_activity(&self) {
        if let Ok(mut st) = self.inner.lock() {
            st.last_activity = Some(Instant::now());
            st.retry_grace_used = Duration::ZERO;
        }
        self.notify.notify_one();
    }

    /// `agy` 日志报出瞬态网络退避重试时，给予有界的退避宽限，避免在 4s/8s 重连窗口被误杀。
    pub fn note_retry_backoff(&self, backoff: Duration) {
        let grant = backoff
            .saturating_add(Duration::from_secs(10))
            .min(Duration::from_secs(30));
        if let Ok(mut st) = self.inner.lock() {
            st.retry_grace_used = st
                .retry_grace_used
                .saturating_add(grant)
                .min(MAX_RETRY_GRACE_BUDGET);
        }
        self.notify.notify_one();
    }

    pub fn tool_started(&self, tool_call_id: impl Into<String>) {
        let id = tool_call_id.into();
        if let Ok(mut st) = self.inner.lock() {
            if !id.is_empty() {
                st.active_tool_calls.insert(id);
            }
            st.last_activity = Some(Instant::now());
            st.retry_grace_used = Duration::ZERO;
        }
        self.notify.notify_one();
    }

    pub fn tool_finished(&self, tool_call_id: &str) {
        if let Ok(mut st) = self.inner.lock() {
            st.active_tool_calls.remove(tool_call_id);
            st.last_activity = Some(Instant::now());
            st.retry_grace_used = Duration::ZERO;
        }
        self.notify.notify_one();
    }

    pub fn permission_started(&self) {
        if let Ok(mut st) = self.inner.lock() {
            st.pending_permissions = st.pending_permissions.saturating_add(1);
            st.last_activity = Some(Instant::now());
            st.retry_grace_used = Duration::ZERO;
        }
        self.notify.notify_one();
    }

    pub fn permission_finished(&self) {
        if let Ok(mut st) = self.inner.lock() {
            st.pending_permissions = st.pending_permissions.saturating_sub(1);
            st.last_activity = Some(Instant::now());
            st.retry_grace_used = Duration::ZERO;
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

    /// 挂起直至连续不活跃时长超过当前档位阈值。
    pub async fn wait_for_timeout(&self) -> WatchdogTimeout {
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            // 先注册通知再读状态，消除读锁释放到 select 之间的竞态窗口。
            notified.as_mut().enable();

            let (deadline, timeout, has_work, awaiting_first) = {
                let st = self.inner.lock().unwrap();
                (
                    st.deadline(),
                    st.current_timeout(),
                    st.has_active_work(),
                    st.last_activity.is_none(),
                )
            };
            if Instant::now() >= deadline {
                return WatchdogTimeout {
                    timeout,
                    had_active_work: has_work,
                    awaiting_first_progress: awaiting_first,
                };
            }

            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => {
                    let st = self.inner.lock().unwrap();
                    if Instant::now() >= st.deadline() {
                        return WatchdogTimeout {
                            timeout: st.current_timeout(),
                            had_active_work: st.has_active_work(),
                            awaiting_first_progress: st.last_activity.is_none(),
                        };
                    }
                }
                _ = notified => {
                    // 活跃时间、工具状态或重试宽限发生变化，重新计算 deadline
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test(start_paused = true)]
    async fn metadata_cannot_hide_silent_prompt_and_tiers_transition_cleanly() {
        let dog = PromptWatchdog::default_timeouts();
        let start = Instant::now();
        tokio::time::advance(Duration::from_secs(90)).await;
        for kind in [
            "usage_update",
            "session_info_update",
            "available_commands_update",
            "user_message_chunk",
            "current_mode_update",
            "config_option_update",
        ] {
            assert!(!dog.observe_update(&json!({"sessionUpdate": kind, "used": 1200})));
        }
        assert!(!dog.observe_update(&json!({
            "sessionUpdate": "agent_thought_chunk",
            "content": {"type": "text", "text": "   "}
        })));
        assert!(dog.awaiting_first_progress());
        let timed_out = dog.wait_for_timeout().await;
        assert!(timed_out.awaiting_first_progress);
        assert_eq!(timed_out.timeout, FIRST_PROGRESS_TIMEOUT);
        assert_eq!(start.elapsed(), FIRST_PROGRESS_TIMEOUT);

        let dog = PromptWatchdog::default_timeouts();
        assert!(dog.observe_update(&json!({
            "sessionUpdate": "agent_thought_chunk",
            "content": {"type": "text", "text": "checking"}
        })));
        assert!(!dog.awaiting_first_progress());
        assert_eq!(dog.current_timeout(), DEFAULT_IDLE_TIMEOUT);
        dog.tool_started("slow");
        assert_eq!(dog.current_timeout(), DEFAULT_WORK_IDLE_TIMEOUT);
        dog.tool_finished("slow");
        assert_eq!(dog.current_timeout(), DEFAULT_IDLE_TIMEOUT);
    }

    #[tokio::test(start_paused = true)]
    async fn retry_backoff_extends_deadline_with_bounded_budget() {
        let dog = PromptWatchdog::new(WatchdogConfig {
            first_progress_timeout: Duration::from_secs(60),
            idle_timeout: Duration::from_secs(120),
            work_idle_timeout: Duration::from_secs(600),
        });
        let start = Instant::now();
        for _ in 0..10 {
            dog.note_retry_backoff(Duration::from_secs(15));
        }
        let info = dog.wait_for_timeout().await;
        assert_eq!(info.timeout, Duration::from_secs(60));
        assert_eq!(start.elapsed(), Duration::from_secs(60) + MAX_RETRY_GRACE_BUDGET);
    }
}
