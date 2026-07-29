//! Shared tool-output truncation utilities, ported from
//! `pi-coding-agent/dist/core/tools/truncate.js`. Used by grep/find/ls/bash.
//!
//! Two independent limits — line count and byte count — whichever is hit first
//! wins. `formatSize` reproduces JS `Number.prototype.toFixed(1)`, which rounds
//! half *away from zero* (unlike Rust's banker's-rounding `{:.1}`), via exact
//! integer arithmetic so results match byte-for-byte.

use serde::Serialize;

pub const DEFAULT_MAX_LINES: usize = 2000;
pub const DEFAULT_MAX_BYTES: usize = 50 * 1024;
pub const GREP_MAX_LINE_LENGTH: usize = 500;

/// Port of `formatSize(bytes)`: `B` under 1KB, `KB` under 1MB, else `MB`, each
/// with one decimal rounded half away from zero (JS `toFixed(1)` semantics).
pub fn format_size(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes}B")
    } else if bytes < 1024 * 1024 {
        format!("{}KB", to_fixed_1_div(bytes, 1024))
    } else {
        format!("{}MB", to_fixed_1_div(bytes, 1024 * 1024))
    }
}

/// Compute `(bytes / divisor).toFixed(1)` with integer arithmetic and
/// round-half-up, matching JS for the non-negative byte counts used here.
fn to_fixed_1_div(bytes: usize, divisor: usize) -> String {
    let numerator = bytes.saturating_mul(10);
    let mut quotient = numerator / divisor;
    let remainder = numerator % divisor;
    if remainder * 2 >= divisor {
        quotient += 1;
    }
    format!("{}.{}", quotient / 10, quotient % 10)
}

/// Port of `splitLinesForCounting`: split on `\n`, dropping the trailing empty
/// element produced by a final newline.
fn split_lines_for_counting(content: &str) -> Vec<&str> {
    if content.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<&str> = content.split('\n').collect();
    if content.ends_with('\n') {
        lines.pop();
    }
    lines
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Truncation {
    pub content: String,
    pub truncated: bool,
    #[serde(rename = "truncatedBy")]
    pub truncated_by: Option<String>,
    #[serde(rename = "totalLines")]
    pub total_lines: usize,
    #[serde(rename = "totalBytes")]
    pub total_bytes: usize,
    #[serde(rename = "outputLines")]
    pub output_lines: usize,
    #[serde(rename = "outputBytes")]
    pub output_bytes: usize,
    #[serde(rename = "lastLinePartial")]
    pub last_line_partial: bool,
    #[serde(rename = "firstLineExceedsLimit")]
    pub first_line_exceeds_limit: bool,
    #[serde(rename = "maxLines")]
    pub max_lines: usize,
    #[serde(rename = "maxBytes")]
    pub max_bytes: usize,
}

fn no_truncation(content: &str, total_lines: usize, total_bytes: usize, max_lines: usize, max_bytes: usize) -> Truncation {
    Truncation {
        content: content.to_string(),
        truncated: false,
        truncated_by: None,
        total_lines,
        total_bytes,
        output_lines: total_lines,
        output_bytes: total_bytes,
        last_line_partial: false,
        first_line_exceeds_limit: false,
        max_lines,
        max_bytes,
    }
}

/// Port of `truncateHead`: keep the first lines that fit both limits. Never
/// returns partial lines; if the first line alone exceeds the byte limit,
/// returns empty content with `firstLineExceedsLimit`.
pub fn truncate_head(content: &str, max_lines: Option<usize>, max_bytes: Option<usize>) -> Truncation {
    let max_lines = max_lines.unwrap_or(DEFAULT_MAX_LINES);
    let max_bytes = max_bytes.unwrap_or(DEFAULT_MAX_BYTES);
    let total_bytes = content.len();
    let lines = split_lines_for_counting(content);
    let total_lines = lines.len();

    if total_lines <= max_lines && total_bytes <= max_bytes {
        return no_truncation(content, total_lines, total_bytes, max_lines, max_bytes);
    }

    let first_line_bytes = lines.first().map_or(0, |line| line.len());
    if first_line_bytes > max_bytes {
        return Truncation {
            content: String::new(),
            truncated: true,
            truncated_by: Some("bytes".to_string()),
            total_lines,
            total_bytes,
            output_lines: 0,
            output_bytes: 0,
            last_line_partial: false,
            first_line_exceeds_limit: true,
            max_lines,
            max_bytes,
        };
    }

    let mut output: Vec<&str> = Vec::new();
    let mut output_bytes = 0usize;
    let mut truncated_by = "lines";
    for (i, line) in lines.iter().enumerate() {
        if i >= max_lines {
            break;
        }
        let line_bytes = line.len() + if i > 0 { 1 } else { 0 };
        if output_bytes + line_bytes > max_bytes {
            truncated_by = "bytes";
            break;
        }
        output.push(line);
        output_bytes += line_bytes;
    }
    if output.len() >= max_lines && output_bytes <= max_bytes {
        truncated_by = "lines";
    }
    let output_content = output.join("\n");
    let final_bytes = output_content.len();
    Truncation {
        content: output_content,
        truncated: true,
        truncated_by: Some(truncated_by.to_string()),
        total_lines,
        total_bytes,
        output_lines: output.len(),
        output_bytes: final_bytes,
        last_line_partial: false,
        first_line_exceeds_limit: false,
        max_lines,
        max_bytes,
    }
}

/// Port of `truncateTail`: keep the last lines that fit both limits. May return
/// a partial first line (tail slice on a UTF-8 boundary) when the final line
/// alone exceeds the byte limit.
pub fn truncate_tail(content: &str, max_lines: Option<usize>, max_bytes: Option<usize>) -> Truncation {
    let max_lines = max_lines.unwrap_or(DEFAULT_MAX_LINES);
    let max_bytes = max_bytes.unwrap_or(DEFAULT_MAX_BYTES);
    let total_bytes = content.len();
    let lines = split_lines_for_counting(content);
    let total_lines = lines.len();

    if total_lines <= max_lines && total_bytes <= max_bytes {
        return no_truncation(content, total_lines, total_bytes, max_lines, max_bytes);
    }

    let mut output: Vec<String> = Vec::new();
    let mut output_bytes = 0usize;
    let mut truncated_by = "lines";
    let mut last_line_partial = false;

    let mut i = lines.len();
    while i > 0 && output.len() < max_lines {
        i -= 1;
        let line = lines[i];
        let line_bytes = line.len() + if !output.is_empty() { 1 } else { 0 };
        if output_bytes + line_bytes > max_bytes {
            truncated_by = "bytes";
            if output.is_empty() {
                let truncated_line = truncate_string_to_bytes_from_end(line, max_bytes);
                output_bytes = truncated_line.len();
                output.push(truncated_line);
                last_line_partial = true;
            }
            break;
        }
        output.push(line.to_string());
        output_bytes += line_bytes;
    }
    if output.len() >= max_lines && output_bytes <= max_bytes {
        truncated_by = "lines";
    }
    output.reverse();
    let output_content = output.join("\n");
    let final_bytes = output_content.len();
    Truncation {
        content: output_content,
        truncated: true,
        truncated_by: Some(truncated_by.to_string()),
        total_lines,
        total_bytes,
        output_lines: output.len(),
        output_bytes: final_bytes,
        last_line_partial,
        first_line_exceeds_limit: false,
        max_lines,
        max_bytes,
    }
}

/// Port of `truncateStringToBytesFromEnd`: keep the trailing slice that fits
/// `max_bytes`, advancing to a UTF-8 character boundary.
fn truncate_string_to_bytes_from_end(value: &str, max_bytes: usize) -> String {
    let bytes = value.as_bytes();
    if bytes.len() <= max_bytes {
        return value.to_string();
    }
    let mut start = bytes.len() - max_bytes;
    while start < bytes.len() && (bytes[start] & 0xc0) == 0x80 {
        start += 1;
    }
    String::from_utf8_lossy(&bytes[start..]).into_owned()
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TruncateLineResult {
    pub text: String,
    #[serde(rename = "wasTruncated")]
    pub was_truncated: bool,
}

/// Port of `truncateLine`: cap a single line at `max_chars` UTF-16 code units,
/// appending `... [truncated]` when cut.
pub fn truncate_line(line: &str, max_chars: Option<usize>) -> TruncateLineResult {
    let max_chars = max_chars.unwrap_or(GREP_MAX_LINE_LENGTH);
    let units: Vec<u16> = line.encode_utf16().collect();
    if units.len() <= max_chars {
        return TruncateLineResult {
            text: line.to_string(),
            was_truncated: false,
        };
    }
    let sliced = String::from_utf16_lossy(&units[..max_chars]);
    TruncateLineResult {
        text: format!("{sliced}... [truncated]"),
        was_truncated: true,
    }
}
