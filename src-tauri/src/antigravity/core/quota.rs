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
        let line_is_exhausted = lower.contains("resource_exhausted")
            || lower.contains("resource has been exhausted")
            || lower.contains("ratelimitexceeded")
            || lower.contains("quota_exceeded")
            || (lower.contains("error 429") && lower.contains("message:"));
        if line_is_exhausted {
            self.saw_resource_exhausted = true;
        } else if is_other_api_error_line(&lower) {
            self.saw_resource_exhausted = false;
        }

        // 2. 识别重试退避行：`Encountered retryable api error. retrying in <go-duration>`
        let retry_pos = lower.find("encountered retryable api error");
        let retrying_in_pos = lower.find("retrying in ");
        if retry_pos.is_none() && retrying_in_pos.is_none() {
            return None;
        }
        // 若既未见过 RESOURCE_EXHAUSTED，本行也未出现 retryable api error，则忽略普通日志。
        if !self.saw_resource_exhausted && !line_is_exhausted && retry_pos.is_none() {
            return None;
        }
        // 只对 429 / RESOURCE_EXHAUSTED 后的重试，或连续重试风暴计数（防止其他非 429 瞬态错误误报单次重试）。
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
