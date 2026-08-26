//! Provider 空闲看门狗与超时诊断落盘，对齐 Vega/PI 的 createAlkaidIdleTimeout
//! 与 provider-timeouts.jsonl：流式请求无增量事件超过阈值即视为挂死，
//! 由 agent 终止本次请求并走既有重试链；超时时把现场快照追加到
//! `<sessions_root>/logs/provider-timeouts.jsonl`。健康请求零落盘开销。

use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

/// 无增量事件判定挂死的阈值，与 Vega 的 ALKAID_PROVIDER_IDLE_TIMEOUT_MS 对齐。
pub const IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

pub enum DeltaKind {
    Text,
    Thinking,
    ToolArgs,
}

#[derive(Default)]
pub struct ProviderActivity {
    pub message_starts: AtomicU64,
    pub text_deltas: AtomicU64,
    pub thinking_deltas: AtomicU64,
    pub tool_args_deltas: AtomicU64,
}

impl ProviderActivity {
    pub fn bump(&self, kind: DeltaKind) {
        match kind {
            DeltaKind::Text => self.text_deltas.fetch_add(1, Ordering::Relaxed),
            DeltaKind::Thinking => self.thinking_deltas.fetch_add(1, Ordering::Relaxed),
            DeltaKind::ToolArgs => self.tool_args_deltas.fetch_add(1, Ordering::Relaxed),
        };
    }
    fn snapshot(&self) -> Value {
        json!({
            "messageStarts": self.message_starts.load(Ordering::Relaxed),
            "textDeltas": self.text_deltas.load(Ordering::Relaxed),
            "thinkingDeltas": self.thinking_deltas.load(Ordering::Relaxed),
            "toolArgsDeltas": self.tool_args_deltas.load(Ordering::Relaxed),
        })
    }
}

/// 看门狗状态：0=未武装（工具执行/回合间隙），>0=最近一次增量事件的时间戳（ms）。
/// 观察任务只在状态为武装且时间戳过期时触发，天然支持 touch/pause/resume。
pub struct IdleWatchdog {
    last_event_ms: AtomicU64,
    paused: AtomicU64,
    pub activity: ProviderActivity,
    timeout_count: AtomicU64,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl IdleWatchdog {
    pub fn new() -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            last_event_ms: AtomicU64::new(0),
            paused: AtomicU64::new(0),
            activity: ProviderActivity::default(),
            timeout_count: AtomicU64::new(0),
        })
    }

    /// 流式请求开始/每个增量事件到达时调用。
    pub fn touch(&self) {
        self.last_event_ms.store(now_ms(), Ordering::Relaxed);
    }

    /// 工具执行期间暂停（可嵌套计数），最后一个工具结束后自动恢复。
    pub fn pause(&self) {
        self.paused.fetch_add(1, Ordering::Relaxed);
    }

    pub fn resume(&self) {
        // 与 touch 同效：恢复时点重新起算，避免工具结束瞬间被判超时。
        self.paused
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                Some(v.saturating_sub(1))
            })
            .ok();
        self.touch();
    }

    /// 回合间隙解除武装。
    pub fn disarm(&self) {
        self.last_event_ms.store(0, Ordering::Relaxed);
    }

    fn expired(&self) -> bool {
        let last = self.last_event_ms.load(Ordering::Relaxed);
        last != 0
            && self.paused.load(Ordering::Relaxed) == 0
            && now_ms().saturating_sub(last) > IDLE_TIMEOUT.as_millis() as u64
    }
}

/// 单次 provider 流式调用挂上看门狗：返回独立的取消标志与观察任务句柄。
/// 观察任务 1s 轮询；超时即置位取消标志（provider 内 wait_cancelled 50ms 轮询，
/// 会将其转成流错误，错误文案带 "idle timeout"，命中既有可重试清单）。
pub fn arm_idle_watchdog(
    watchdog: &std::sync::Arc<IdleWatchdog>,
) -> (
    std::sync::Arc<std::sync::atomic::AtomicBool>,
    tokio::task::JoinHandle<()>,
) {
    use std::sync::atomic::AtomicBool;
    let trip = std::sync::Arc::new(AtomicBool::new(false));
    let weak = std::sync::Arc::downgrade(watchdog);
    let trip_task = trip.clone();
    watchdog.touch();
    let handle = tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            let Some(watchdog) = weak.upgrade() else {
                return;
            };
            if watchdog.last_event_ms.load(Ordering::Relaxed) == 0 {
                return; // 已解除武装：本次请求结束。
            }
            if watchdog.expired() {
                watchdog.timeout_count.fetch_add(1, Ordering::Relaxed);
                trip_task.store(true, Ordering::SeqCst);
                return;
            }
        }
    });
    (trip, handle)
}

/// 诊断记录器：仅在超时发生时才建目录/追加一行 JSONL；写失败静默，绝不影响请求。
pub struct DiagnosticLog {
    path: Mutex<Option<PathBuf>>,
}

impl DiagnosticLog {
    pub fn new(root: Option<PathBuf>) -> Self {
        Self {
            path: Mutex::new(root.map(|r| r.join("logs").join("provider-timeouts.jsonl"))),
        }
    }

    pub fn record(&self, event: Value) {
        let Ok(guard) = self.path.lock() else { return };
        let Some(path) = guard.as_ref() else { return };
        let line = format!("{}\n", serde_json::to_string(&event).unwrap_or_default());
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        use std::io::Write;
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = file.write_all(line.as_bytes());
        }
    }
}

/// 对齐 Vega provider_stream_idle_timeout 的现场快照（去掉凭证与完整内容，只留形状）。
pub fn timeout_event(
    session_id: &str,
    provider: &str,
    model: &str,
    api: &str,
    activity: &ProviderActivity,
    message_tail: &[Value],
) -> Value {
    let tail: Vec<Value> = message_tail
        .iter()
        .map(|m| {
            json!({
                "role": m.get("role").and_then(Value::as_str).unwrap_or("?"),
                "stopReason": m.get("stopReason").and_then(Value::as_str),
                "contentParts": m.get("content").and_then(Value::as_array).map(|c| c.len()).unwrap_or(0),
                "hasError": m.get("errorMessage").and_then(Value::as_str).is_some_and(|e| !e.is_empty()),
            })
        })
        .collect();
    json!({
        "timestamp": now_ms(),
        "event": "provider_stream_idle_timeout",
        "timeoutMs": IDLE_TIMEOUT.as_millis() as u64,
        "sessionId": session_id,
        "provider": provider,
        "model": model,
        "api": api,
        "activity": activity.snapshot(),
        "messageCount": message_tail.len(),
        "messageTail": tail,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watchdog_expires_only_when_armed_and_unpaused() {
        let w = IdleWatchdog::new();
        assert!(!w.expired(), "未武装不应过期");
        w.touch();
        assert!(!w.expired(), "刚 touch 不应过期");
        w.last_event_ms.store(
            now_ms() - IDLE_TIMEOUT.as_millis() as u64 - 1,
            Ordering::Relaxed,
        );
        assert!(w.expired(), "超过阈值应过期");
        w.pause();
        assert!(!w.expired(), "工具执行暂停期间不应过期");
        w.resume();
        assert!(!w.expired(), "resume 重新起算后不应过期");
        w.disarm();
        assert!(!w.expired(), "解除武装后不应过期");
    }

    #[test]
    fn timeout_event_drops_content_and_keeps_shape() {
        let activity = ProviderActivity::default();
        activity.bump(DeltaKind::Text);
        let tail = vec![json!({
            "role": "assistant",
            "content": [{"type": "text", "text": "敏感正文"}],
            "stopReason": "toolUse",
        })];
        let event = timeout_event("s1", "kimi", "k3", "openai-completions", &activity, &tail);
        assert_eq!(event["event"], "provider_stream_idle_timeout");
        assert_eq!(event["messageTail"][0]["contentParts"], 1);
        assert_eq!(event["activity"]["textDeltas"], 1);
        assert!(!event.to_string().contains("敏感正文"), "诊断不得携带正文");
    }
}
