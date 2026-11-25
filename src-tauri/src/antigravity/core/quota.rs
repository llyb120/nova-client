//! Antigravity 配额耗尽与 stderr 静默 429 重试风暴检测。
//!
//! 覆盖两种隐蔽配额耗尽形态：
//! 1. **stderr 静默无限 429 重试**：`agy` 命中 `RESOURCE_EXHAUSTED` / `Error 429` 后仅向 stderr 打印
//!    `Encountered retryable api error. retrying in <go-duration>` 而不发任何 ACP update，界面看起来像假死。
//!    `ResourceExhaustionTracker` 实时解析 stderr 行：重试 ≥ 2 次或累计退避 > 15s 立即触发配额错误中止本轮。
//! 2. **伪正常回复**：`session/prompt` 返回 `totalTokens == 0` 或正文命中 `"Usage Limit Reached"` / `"quota"`。

use chrono::DateTime;
use serde::{Deserialize, Serialize};
use serde_json::Value;

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

/// 单个 Antigravity 配额时间窗口桶（如 5h 限额、周限额）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AntigravityQuotaBucket {
    pub id: String,
    pub name: String,
    pub window: String,
    pub remaining_fraction: f64,
    pub remaining_percent: u8,
    pub reset_time: Option<i64>,
    pub description: Option<String>,
}

/// 单个模型家族配额分组（如 Gemini Models、Claude and GPT models）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AntigravityQuotaGroup {
    pub name: String,
    pub description: Option<String>,
    pub buckets: Vec<AntigravityQuotaBucket>,
}

/// 当前 Antigravity 账号的配额快照。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AntigravityQuotaSnapshot {
    pub account_id: Option<String>,
    pub fetched_at: i64,
    pub description: Option<String>,
    pub groups: Vec<AntigravityQuotaGroup>,
}

fn normalize_quota_window(window: &str, id: &str, name: &str) -> String {
    let w = window.trim().to_ascii_lowercase();
    let id_lower = id.trim().to_ascii_lowercase();
    let name_lower = name.trim().to_ascii_lowercase();
    if w == "5h"
        || id_lower.ends_with("-5h")
        || name_lower.contains("five hour")
        || name_lower.contains("5-hour")
        || name_lower.contains("5h")
    {
        return "5h".to_string();
    }
    if w == "weekly" || id_lower.ends_with("-weekly") || name_lower.contains("weekly") {
        return "weekly".to_string();
    }
    if !w.is_empty() {
        w
    } else {
        id.trim().to_string()
    }
}

fn parse_reset_time_ms(raw: Option<&str>) -> Option<i64> {
    let s = raw?.trim();
    if s.is_empty() {
        return None;
    }
    let dt = DateTime::parse_from_rfc3339(s).ok()?;
    let ms = dt.timestamp_millis();
    if ms <= 0 {
        None
    } else {
        Some(ms)
    }
}

fn non_empty_opt(raw: Option<&str>) -> Option<String> {
    raw.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn parse_usage_json_root(raw: &str) -> Option<Value> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
        if v.is_object() {
            return Some(v);
        }
    }
    for line in trimmed.lines().rev() {
        let candidate = line.trim();
        if candidate.starts_with('{') {
            if let Ok(v) = serde_json::from_str::<Value>(candidate) {
                if v.is_object() {
                    return Some(v);
                }
            }
        }
    }
    None
}

fn parse_buckets_from_json(arr: &[Value]) -> Vec<AntigravityQuotaBucket> {
    let mut out = Vec::new();
    for item in arr {
        let Some(obj) = item.as_object() else {
            continue;
        };
        let id = obj
            .get("id")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default()
            .to_string();
        let name = obj
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(&id)
            .to_string();
        let raw_window = obj
            .get("window")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let window = normalize_quota_window(raw_window, &id, &name);
        let Some(raw_fraction) = obj.get("remaining_fraction").and_then(Value::as_f64) else {
            continue;
        };
        if !raw_fraction.is_finite() {
            continue;
        }
        let remaining_fraction = raw_fraction.clamp(0.0, 1.0);
        let remaining_percent = (remaining_fraction * 100.0).round().clamp(0.0, 100.0) as u8;
        let reset_time = parse_reset_time_ms(obj.get("reset_time").and_then(Value::as_str));
        let description = non_empty_opt(obj.get("description").and_then(Value::as_str));
        out.push(AntigravityQuotaBucket {
            id,
            name,
            window,
            remaining_fraction,
            remaining_percent,
            reset_time,
            description,
        });
    }
    out
}

fn parse_groups_from_tsv(text: &str) -> Vec<AntigravityQuotaGroup> {
    let mut groups: Vec<AntigravityQuotaGroup> = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let cols: Vec<&str> = trimmed.split('\t').map(str::trim).collect();
        if cols.len() < 3 {
            continue;
        }
        let group_name = cols[0];
        let bucket_name = cols[1];
        let pct_str = cols[2].trim_end_matches('%').trim();
        let Ok(pct_val) = pct_str.parse::<f64>() else {
            continue;
        };
        if !pct_val.is_finite() {
            continue;
        }
        let remaining_fraction = (pct_val / 100.0).clamp(0.0, 1.0);
        let remaining_percent = (remaining_fraction * 100.0).round().clamp(0.0, 100.0) as u8;
        let reset_time = cols.get(3).copied().and_then(|s| parse_reset_time_ms(Some(s)));
        let window = normalize_quota_window("", "", bucket_name);
        let id_prefix = if group_name.to_ascii_lowercase().contains("gemini") {
            "gemini"
        } else {
            "3p"
        };
        let bucket = AntigravityQuotaBucket {
            id: format!("{id_prefix}-{window}"),
            name: bucket_name.to_string(),
            window,
            remaining_fraction,
            remaining_percent,
            reset_time,
            description: None,
        };
        if let Some(existing) = groups.iter_mut().find(|g| g.name == group_name) {
            existing.buckets.push(bucket);
        } else {
            groups.push(AntigravityQuotaGroup {
                name: group_name.to_string(),
                description: None,
                buckets: vec![bucket],
            });
        }
    }
    groups
}

/// 解析 `agy --print "/usage" --output-format json`（及 TSV 文本回退）输出为结构化配额快照。
pub fn parse_antigravity_usage_output(
    raw: &str,
    account_id: Option<String>,
    fetched_at: i64,
) -> Result<AntigravityQuotaSnapshot, String> {
    if let Some(root) = parse_usage_json_root(raw) {
        if let Some(status) = root.get("status").and_then(Value::as_str) {
            if !status.eq_ignore_ascii_case("SUCCESS") {
                let detail = root
                    .get("error")
                    .or_else(|| root.get("message"))
                    .or_else(|| root.get("response"))
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .unwrap_or(status);
                return Err(format!("获取 Antigravity 用量失败：{detail}"));
            }
        }

        let data_node = root
            .pointer("/command/data")
            .or_else(|| root.get("data"))
            .unwrap_or(&root);
        let description = non_empty_opt(data_node.get("description").and_then(Value::as_str));

        let mut groups = Vec::new();
        if let Some(raw_groups) = data_node.get("groups").and_then(Value::as_array) {
            for g in raw_groups {
                let Some(name) = non_empty_opt(g.get("name").and_then(Value::as_str)) else {
                    continue;
                };
                let g_desc = non_empty_opt(g.get("description").and_then(Value::as_str));
                let buckets = g
                    .get("buckets")
                    .and_then(Value::as_array)
                    .map(|arr| parse_buckets_from_json(arr))
                    .unwrap_or_default();
                if !buckets.is_empty() {
                    groups.push(AntigravityQuotaGroup {
                        name,
                        description: g_desc,
                        buckets,
                    });
                }
            }
        }

        if groups.is_empty() {
            if let Some(resp) = root.get("response").and_then(Value::as_str) {
                groups = parse_groups_from_tsv(resp);
            }
        }

        if !groups.is_empty() {
            return Ok(AntigravityQuotaSnapshot {
                account_id,
                fetched_at,
                description,
                groups,
            });
        }
        return Err("Antigravity 未返回有效配额数据".into());
    }

    let groups = parse_groups_from_tsv(raw);
    if !groups.is_empty() {
        return Ok(AntigravityQuotaSnapshot {
            account_id,
            fetched_at,
            description: None,
            groups,
        });
    }

    Err("无法解析 Antigravity 用量输出".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_real_agy_1516_json_payload() {
        let raw = r#"{"conversation_id":"","status":"SUCCESS","response":"Gemini Models\tWeekly Limit Remaining\t48%\t2026-12-01T02:13:33Z\nGemini Models\tFive Hour Limit Remaining\t97%\t2026-11-25T06:55:55Z\nClaude and GPT models\tWeekly Limit Remaining\t100%\t1970-01-01T00:00:00Z\nClaude and GPT models\tFive Hour Limit Remaining\t100%\t1970-01-01T00:00:00Z\n","duration_seconds":0,"num_turns":0,"usage":{"input_tokens":0,"output_tokens":0,"thinking_tokens":0,"cache_read_tokens":0,"total_tokens":0},"command":{"name":"usage","data":{"description":"Within each group, models share a weekly limit and a 5-hour limit.","groups":[{"name":"Gemini Models","description":"Models within this group: Gemini 4 Neon, Gemini 4 Carbon","buckets":[{"id":"gemini-weekly","name":"Weekly Limit Remaining","description":"You have used some of your weekly limit, it will fully refresh in 6 days.","window":"weekly","remaining_fraction":0.4772486388683319,"reset_time":"2026-12-01T02:13:33Z"},{"id":"gemini-5h","name":"Five Hour Limit Remaining","description":"You have used some of your 5-hour limit, it will fully refresh in 4 hours, 55 minutes.","window":"5h","remaining_fraction":0.9669256806373596,"reset_time":"2026-11-25T06:55:55Z"}]},{"name":"Claude and GPT models","description":"Models within this group: Claude Opus 5.5, Claude Sonnet 5.5, GPT-OSS 120B","buckets":[{"id":"3p-weekly","name":"Weekly Limit Remaining","window":"weekly","remaining_fraction":1,"reset_time":"1970-01-01T00:00:00Z"},{"id":"3p-5h","name":"Five Hour Limit Remaining","window":"5h","remaining_fraction":1,"reset_time":"1970-01-01T00:00:00Z"}]}]}}}"#;
        let snap = parse_antigravity_usage_output(raw, Some("acc-1".into()), 123456).unwrap();
        assert_eq!(snap.account_id.as_deref(), Some("acc-1"));
        assert_eq!(snap.fetched_at, 123456);
        assert_eq!(snap.groups.len(), 2);

        let gemini = &snap.groups[0];
        assert_eq!(gemini.name, "Gemini Models");
        assert_eq!(gemini.buckets.len(), 2);
        assert_eq!(gemini.buckets[0].window, "weekly");
        assert_eq!(gemini.buckets[0].remaining_percent, 48);
        assert!(gemini.buckets[0].reset_time.is_some());
        assert_eq!(gemini.buckets[1].window, "5h");
        assert_eq!(gemini.buckets[1].remaining_percent, 97);
        assert!(gemini.buckets[1].reset_time.is_some());

        let third_party = &snap.groups[1];
        assert_eq!(third_party.name, "Claude and GPT models");
        assert_eq!(third_party.buckets[0].remaining_percent, 100);
        assert_eq!(third_party.buckets[0].reset_time, None);
        assert_eq!(third_party.buckets[1].remaining_percent, 100);
        assert_eq!(third_party.buckets[1].reset_time, None);
    }

    #[test]
    fn parses_noise_prefixed_json_stdout() {
        let raw = "Opening in existing browser session.\n{\"status\":\"SUCCESS\",\"command\":{\"name\":\"usage\",\"data\":{\"groups\":[{\"name\":\"Gemini Models\",\"buckets\":[{\"id\":\"gemini-5h\",\"name\":\"Five Hour Limit Remaining\",\"window\":\"5h\",\"remaining_fraction\":0.5,\"reset_time\":\"2026-11-25T06:55:55Z\"}]}]}}}";
        let snap = parse_antigravity_usage_output(raw, None, 999).unwrap();
        assert_eq!(snap.groups.len(), 1);
        assert_eq!(snap.groups[0].buckets[0].remaining_percent, 50);
    }

    #[test]
    fn parses_tsv_fallback() {
        let raw = "Gemini Models\tWeekly Limit Remaining\t48%\t2026-12-01T02:13:33Z\nGemini Models\tFive Hour Limit Remaining\t97%\t2026-11-25T06:55:55Z\nClaude and GPT models\tFive Hour Limit Remaining\t100%\t1970-01-01T00:00:00Z\n";
        let snap = parse_antigravity_usage_output(raw, None, 100).unwrap();
        assert_eq!(snap.groups.len(), 2);
        assert_eq!(snap.groups[0].buckets[0].window, "weekly");
        assert_eq!(snap.groups[0].buckets[0].remaining_percent, 48);
        assert_eq!(snap.groups[0].buckets[1].window, "5h");
        assert_eq!(snap.groups[0].buckets[1].remaining_percent, 97);
        assert_eq!(snap.groups[1].buckets[0].reset_time, None);
    }

    #[test]
    fn rejects_invalid_or_empty_output() {
        assert!(parse_antigravity_usage_output("", None, 0).is_err());
        assert!(parse_antigravity_usage_output("not valid output", None, 0).is_err());
        assert!(
            parse_antigravity_usage_output(r#"{"status":"FAILED","error":"auth expired"}"#, None, 0)
                .is_err()
        );
    }
}
