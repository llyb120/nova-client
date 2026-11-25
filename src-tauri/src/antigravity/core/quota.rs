//! Antigravity 配额耗尽与 stderr 静默 429 重试风暴检测。
//!
//! 覆盖两种隐蔽配额耗尽形态：
//! 1. **stderr 静默无限 429 重试**：`agy` 命中 `RESOURCE_EXHAUSTED` / `Error 429` 后仅向 stderr 打印
//!    `Encountered retryable api error. retrying in <go-duration>` 而不发任何 ACP update，界面看起来像假死。
//!    `ResourceExhaustionTracker` 实时解析 stderr 行：重试 ≥ 2 次或累计退避 > 15s 立即触发配额错误中止本轮。
//! 2. **伪正常回复**：`session/prompt` 返回 `totalTokens == 0` 或正文命中 `"Usage Limit Reached"` / `"quota"`。

/// 触发配额中止的最小重试次数（重试 ≥ 2 次）。
pub const RESOURCE_EXHAUSTED_MAX_RETRIES: u32 = 2;
/// 触发配额中止的累计退避上限（> 15s）。
pub const RESOURCE_EXHAUSTED_GRACE_MS: u64 = 15_000;
/// 当 stderr 重试行未携带可解析时长时的保守估计（ms）。
pub const RESOURCE_EXHAUSTED_UNKNOWN_DELAY_MS: u64 = 10_000;

/// 解析 Go `time.Duration.String()` 风格时长为毫秒，支持复合段如 `"4s"`、`"7.262817364s"`、
/// `"500ms"`、`"1m2.5s"`、`"1h2m3s"`、`"250µs"`、`"250us"`、`"100ns"`。
pub fn parse_go_duration_ms(raw: &str) -> Option<u64> {
    let token = raw
        .trim()
        .split_whitespace()
        .next()?
        .trim_end_matches(|c: char| matches!(c, ',' | ';' | ')' | ']' | '.'));
    if token.is_empty() {
        return None;
    }

    let bytes = token.as_bytes();
    let len = bytes.len();
    let mut i = 0usize;
    let mut total_ms: f64 = 0.0;
    let mut parsed_any = false;

    while i < len {
        let num_start = i;
        let mut saw_digit = false;
        let mut saw_dot = false;
        while i < len {
            let b = bytes[i];
            if b.is_ascii_digit() {
                saw_digit = true;
                i += 1;
            } else if b == b'.' && !saw_dot {
                saw_dot = true;
                i += 1;
            } else {
                break;
            }
        }
        if !saw_digit {
            return None;
        }
        let value: f64 = token[num_start..i].parse().ok()?;
        let rest = &token[i..];
        let (unit_len, scale_ms) = if rest.starts_with("ms") {
            (2, 1.0)
        } else if rest.starts_with("us") {
            (2, 1e-3)
        } else if rest.starts_with("µs") {
            ("µs".len(), 1e-3)
        } else if rest.starts_with("μs") {
            ("μs".len(), 1e-3)
        } else if rest.starts_with("ns") {
            (2, 1e-6)
        } else if rest.starts_with('h') {
            (1, 3_600_000.0)
        } else if rest.starts_with('m') {
            (1, 60_000.0)
        } else if rest.starts_with('s') {
            (1, 1_000.0)
        } else {
            return None;
        };
        total_ms += value * scale_ms;
        i += unit_len;
        parsed_any = true;
    }

    if !parsed_any || !total_ms.is_finite() || total_ms < 0.0 {
        return None;
    }
    Some(total_ms.round() as u64)
}

/// 实时跟踪单轮 `agy` stderr 中的 `RESOURCE_EXHAUSTED` 429 重试风暴。
#[derive(Clone, Debug, Default)]
pub struct ResourceExhaustionTracker {
    saw_resource_exhausted: bool,
    retries: u32,
    total_delay_ms: u64,
    last_delay_ms: Option<u64>,
    triggered: bool,
}

impl ResourceExhaustionTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn retries(&self) -> u32 {
        self.retries
    }

    pub fn total_delay_ms(&self) -> u64 {
        self.total_delay_ms
    }

    pub fn is_triggered(&self) -> bool {
        self.triggered
    }

    /// 当模型产生真实流式输出时调用，表明此前重试已恢复，重置计数。
    pub fn note_progress(&mut self) {
        if self.triggered {
            return;
        }
        self.saw_resource_exhausted = false;
        self.retries = 0;
        self.total_delay_ms = 0;
        self.last_delay_ms = None;
    }

    /// 消费一行 stderr；达到配额中止阈值（重试 ≥ 2 次或累计退避 > 15s）时返回面向用户的配额错误说明。
    pub fn observe_stderr_line(&mut self, line: &str) -> Option<String> {
        if self.triggered {
            return None;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return None;
        }
        let lower = trimmed.to_ascii_lowercase();

        // 1. 识别 API 错误行：若为 429 / RESOURCE_EXHAUSTED 置位，否则其他 HTTP 错误清零该标志。
        //    排除非生成接口（如 writeTrajectoryAcls）的偶发 429，避免误把普通网络 EOF 重试判为配额耗尽。
        let auxiliary_endpoint = lower.contains("writetrajectoryacls")
            || lower.contains("recordtrajectoryanalytics")
            || lower.contains("listexperiments");
        let line_is_exhausted = !auxiliary_endpoint
            && (lower.contains("resource_exhausted")
                || lower.contains("resource has been exhausted")
                || lower.contains("ratelimitexceeded")
                || lower.contains("quota_exceeded")
                || (lower.contains("error 429") && lower.contains("message:")));
        if line_is_exhausted {
            self.saw_resource_exhausted = true;
        } else if auxiliary_endpoint || is_other_api_error_line(&lower) {
            self.saw_resource_exhausted = false;
        }

        // 2. 识别重试退避行：`Encountered retryable api error. retrying in <go-duration>`
        //    以及 `agy` 日志中的 `Run: attempt N failed (...), retrying in <go-duration>`。
        let retry_pos = lower
            .find("encountered retryable api error")
            .or_else(|| lower.find("run: attempt "));
        let retrying_in_pos = lower.find("retrying in ");
        if retry_pos.is_none() && retrying_in_pos.is_none() {
            return None;
        }
        // 只对 429 / RESOURCE_EXHAUSTED 后的重试计数（非 429 网络 EOF 重试不误报为配额耗尽）。
        if !self.saw_resource_exhausted && !line_is_exhausted {
            return None;
        }

        let parsed_delay = retrying_in_pos
            .map(|pos| &trimmed[pos + "retrying in ".len()..])
            .and_then(parse_go_duration_ms);
        let delay_ms = parsed_delay.unwrap_or(RESOURCE_EXHAUSTED_UNKNOWN_DELAY_MS);
        self.retries = self.retries.saturating_add(1);
        self.last_delay_ms = parsed_delay;
        self.total_delay_ms = self.total_delay_ms.saturating_add(delay_ms);

        if self.retries >= RESOURCE_EXHAUSTED_MAX_RETRIES
            || self.total_delay_ms > RESOURCE_EXHAUSTED_GRACE_MS
        {
            self.triggered = true;
            let delay_desc = match self.last_delay_ms {
                Some(ms) => format!("最近退避 {:.2}s", ms as f64 / 1000.0),
                None => "退避时长未知".into(),
            };
            return Some(format!(
                "Antigravity 配额已耗尽或触发速率限制（HTTP 429 RESOURCE_EXHAUSTED）：已重试 {} 次、累计退避 {:.1}s（{}），已中止本轮。请稍后重试或切换模型。",
                self.retries,
                self.total_delay_ms as f64 / 1000.0,
                delay_desc
            ));
        }
        None
    }
}

fn is_other_api_error_line(lower: &str) -> bool {
    if let Some(pos) = lower.find("error ") {
        let rest = &lower[pos + 6..];
        let digits: String = rest.chars().take(3).collect();
        if digits.len() == 3
            && digits.chars().all(|c| c.is_ascii_digit())
            && digits != "429"
            && rest[3..].contains("message:")
        {
            return true;
        }
    }
    false
}

/// 从 `agy acp --log-file` 单行 glog 日志中解析出的关键运行态信号。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgyLogSignal {
    /// `Run: attempt N failed (...), retrying in <dur>`
    RetryAttempt {
        attempt: u32,
        reason: String,
        retry_in_ms: Option<u64>,
        is_quota: bool,
    },
    /// 已拿到 `streamGenerateContent` 响应头与 `ResponseID`（说明未卡在建连/响应头前）。
    StreamHeadersReceived,
    /// 非重试行的网络/认证底层错误（如 token 刷新超时、连接重置）。
    NetworkFailure(String),
}

/// 解析单行 `agy acp` 日志；普通信息行返回 `None`。
pub fn parse_agy_log_line(line: &str) -> Option<AgyLogSignal> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    let lower = trimmed.to_ascii_lowercase();

    // 1. `Run: attempt <N> failed (<reason>), retrying in <go-dur>`
    if let Some(pos) = lower.find("run: attempt ") {
        let rest = &trimmed[pos + "run: attempt ".len()..];
        let num_end = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        let attempt = rest[..num_end].parse::<u32>().unwrap_or(1);
        let retry_in_ms = lower
            .find("retrying in ")
            .map(|p| &trimmed[p + "retrying in ".len()..])
            .and_then(parse_go_duration_ms);
        let reason_raw = rest
            .find("failed (")
            .map(|start| {
                let after = &rest[start + "failed (".len()..];
                after
                    .rfind("), retrying")
                    .or_else(|| after.rfind(')'))
                    .map(|end| &after[..end])
                    .unwrap_or(after)
            })
            .unwrap_or("上游请求异常");
        let is_quota = lower.contains("resource_exhausted")
            || lower.contains("quota")
            || lower.contains("error 429");
        return Some(AgyLogSignal::RetryAttempt {
            attempt,
            reason: summarize_agy_error_reason(reason_raw),
            retry_in_ms,
            is_quota,
        });
    }

    // 2. 拿到主生成流响应头
    if lower.contains("v1internal:streamgeneratecontent") && trimmed.contains("ResponseID:") {
        return Some(AgyLogSignal::StreamHeadersReceived);
    }

    // 3. 明确的网络/连接错误（排除遥测 `play.googleapis.com` 与非关键 `aicode_fallback.go`）
    if lower.contains("play.googleapis.com") || lower.contains("aicode_fallback.go") {
        return None;
    }
    if lower.contains("token refresh failed due to network error")
        || lower.contains("stream idle timeout")
        || lower.contains("wsarecv:")
        || lower.contains("wsasend:")
        || lower.contains("tls handshake timeout")
        || lower.contains("connectex:")
    {
        return Some(AgyLogSignal::NetworkFailure(summarize_agy_error_reason(
            trimmed,
        )));
    }

    None
}

fn summarize_agy_error_reason(raw: &str) -> String {
    let lower = raw.to_ascii_lowercase();
    if lower.contains("stream idle timeout") {
        "上游流空闲超时（10 分钟未收到数据）".into()
    } else if lower.contains("unexpected eof") {
        "上游连接意外中断（unexpected EOF）".into()
    } else if lower.contains("wsarecv")
        || lower.contains("wsasend")
        || lower.contains("forcibly closed")
    {
        "远端或代理强制关闭了连接".into()
    } else if lower.contains("tls handshake timeout") {
        "TLS 握手超时（请检查代理或网络）".into()
    } else if lower.contains("connectex") || lower.contains("connection refused") {
        "无法建立到上游的 TCP 连接（请检查代理）".into()
    } else if lower.contains("token refresh failed") {
        "刷新 OAuth 凭据时网络超时".into()
    } else if lower.contains("resource_exhausted") || lower.contains("429") {
        "触发 429 速率/配额限制".into()
    } else if lower.ends_with(": eof") || lower.contains(": eof)") || lower == "eof" {
        "连接被提前关闭（EOF）".into()
    } else {
        let compact = raw.trim();
        let mut end = compact.len().min(140);
        while end > 0 && !compact.is_char_boundary(end) {
            end -= 1;
        }
        compact[..end].to_string()
    }
}

/// 识别伪正常配额耗尽回复：
/// - 正文命中 `"Usage Limit Reached"` / `"you have reached your current quota"` / `"quota exceeded"` /
///   `"insufficient quota"` / `"credits balance is too low"`；
/// - 或 `totalTokens == 0` 且（正文命中 `"quota"` / `"usage limit"` / `"rate limit"` / `"resource_exhausted"`，
///   或 `stop_reason == "refusal"`，或仅返回了纯文本且零 token 计费）。
pub fn is_usage_limit_reply(text: &str, total_tokens: u64, stop_reason: &str) -> bool {
    let trimmed = text.trim();
    let lower = trimmed.to_ascii_lowercase();

    let explicit_quota_banner = lower.contains("usage limit reached")
        || lower.contains("you have reached your current quota")
        || lower.contains("quota exceeded")
        || lower.contains("insufficient quota")
        || lower.contains("credits balance is too low")
        || lower.contains("resource_exhausted");
    if explicit_quota_banner {
        return true;
    }

    if total_tokens == 0 {
        if lower.contains("quota")
            || lower.contains("usage limit")
            || lower.contains("rate limit")
            || stop_reason == "refusal"
        {
            return true;
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_agy_retry_stream_headers_and_network_log_signals() {
        assert_eq!(
            parse_agy_log_line(
                "I1125 07:51:55.722558 1136 run.go:308] Run: attempt 1 failed (stream reading error: stream idle timeout (10m0s): context deadline exceeded), retrying in 4s"
            ),
            Some(AgyLogSignal::RetryAttempt {
                attempt: 1,
                reason: "上游流空闲超时（10 分钟未收到数据）".into(),
                retry_in_ms: Some(4000),
                is_quota: false,
            })
        );
        assert_eq!(
            parse_agy_log_line(
                "I1124 23:31:06.850055 7551 run.go:308] Run: attempt 2 failed (request failed: Post \"https://daily-cloudcode-pa.googleapis.com/v1internal:streamGenerateContent?alt=sse\": read tcp 198.18.0.0:59773->198.18.0.120:443: wsarecv: An existing connection was forcibly closed by the remote host.), retrying in 7.739555581s"
            ),
            Some(AgyLogSignal::RetryAttempt {
                attempt: 2,
                reason: "远端或代理强制关闭了连接".into(),
                retry_in_ms: Some(7740),
                is_quota: false,
            })
        );
        assert_eq!(
            parse_agy_log_line(
                "I1125 07:43:02.292190 271 http_helpers.go:748] URL: https://daily-cloudcode-pa.googleapis.com/v1internal:streamGenerateContent?alt=sse Trace: 0x46d09074c9b06dec ResponseID: AiEGa_6KDOqk0-kPn8LMCQ"
            ),
            Some(AgyLogSignal::StreamHeadersReceived)
        );
        // 遥测与 aicode 降级噪声不触发故障告警
        assert_eq!(
            parse_agy_log_line(
                "E1124 23:27:36.558329 84 client.go:97] Post \"https://play.googleapis.com/log\": read tcp 198.18.0.0:64809->198.18.0.123:443: wsarecv: An existing connection was forcibly closed by the remote host."
            ),
            None
        );
        // 辅助接口 writeTrajectoryAcls 的 429 不会误污染后续普通网络重试
        let mut tracker = ResourceExhaustionTracker::new();
        assert!(tracker
            .observe_stderr_line(
                "Failed to make code assist backend request (https://daily-cloudcode-pa.googleapis.com/v1internal:writeTrajectoryAcls): RESOURCE_EXHAUSTED"
            )
            .is_none());
        assert!(tracker
            .observe_stderr_line("Run: attempt 1 failed (request failed: EOF), retrying in 4s")
            .is_none());
        assert!(tracker
            .observe_stderr_line("Run: attempt 2 failed (request failed: EOF), retrying in 8s")
            .is_none());
        assert!(!tracker.is_triggered());
    }
}
