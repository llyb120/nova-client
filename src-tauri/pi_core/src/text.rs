//! UTF-16-faithful text helpers.
//!
//! JavaScript strings are UTF-16 code-unit sequences: `.length` and `.slice`
//! operate on code units, `Buffer.byteLength(s, "utf8")` measures UTF-8 bytes,
//! and a surrogate pair split by a unit-wise slice leaves a *lone surrogate*
//! that Node counts as 3 CESU-8 bytes. To reproduce the node pi-agent output
//! byte-for-byte, every length/slice decision here is made on UTF-16 code
//! units with Node-compatible UTF-8 byte accounting.

/// UTF-8 byte length of a UTF-16 code-unit slice, matching Node
/// `Buffer.byteLength(s, "utf8")`. A well-formed surrogate pair counts as 4
/// bytes; a lone high or low surrogate counts as 3 bytes (CESU-8), as V8 does.
pub fn utf8_byte_len(units: &[u16]) -> usize {
    let mut total = 0usize;
    let mut i = 0usize;
    while i < units.len() {
        let u = units[i];
        if (0xD800..=0xDBFF).contains(&u) {
            if i + 1 < units.len() {
                let n = units[i + 1];
                if (0xDC00..=0xDFFF).contains(&n) {
                    total += 4;
                    i += 2;
                    continue;
                }
            }
            total += 3;
            i += 1;
        } else if (0xDC00..=0xDFFF).contains(&u) {
            total += 3;
            i += 1;
        } else if u < 0x80 {
            total += 1;
            i += 1;
        } else if u < 0x800 {
            total += 2;
            i += 1;
        } else {
            total += 3;
            i += 1;
        }
    }
    total
}

/// Number of UTF-16 code units in a Rust string (JS `.length`).
pub fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

fn units(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

/// Decode a UTF-16 code-unit slice to a Rust string, replacing lone surrogates
/// with U+FFFD. This mirrors how both `String.fromCharCode`-style JS output and
/// `serde_json` normalize lone surrogates when crossing the FFI/test boundary.
fn from_units(units: &[u16]) -> String {
    String::from_utf16_lossy(units)
}

/// Port of `truncateUtf8ToBytes(text, maxBytes)`: keep the leading slice whose
/// UTF-8 byte length fits `max_bytes`, shrinking geometrically then growing by
/// single UTF-16 units exactly like the JS loop.
pub fn truncate_utf8_to_bytes(text: &str, max_bytes: usize) -> String {
    let units = units(text);
    if utf8_byte_len(&units) <= max_bytes {
        return text.to_string();
    }
    let mut end = units.len().min(max_bytes);
    while end > 0 && utf8_byte_len(&units[..end]) > max_bytes {
        end = (end as f64 * 0.9).floor() as usize;
    }
    while end < units.len() && utf8_byte_len(&units[..end + 1]) <= max_bytes {
        end += 1;
    }
    from_units(&units[..end])
}

/// Port of `truncateUtf8TailToBytes(text, maxBytes)`: keep the trailing slice
/// whose UTF-8 byte length fits `max_bytes`.
pub fn truncate_utf8_tail_to_bytes(text: &str, max_bytes: usize) -> String {
    let units = units(text);
    if utf8_byte_len(&units) <= max_bytes {
        return text.to_string();
    }
    let mut start = units.len().saturating_sub(max_bytes);
    while start < units.len() && utf8_byte_len(&units[start..]) > max_bytes {
        let slice_len = units.len() - start;
        let inc = 1usize.max(((slice_len as f64) * 0.1).ceil() as usize);
        start += inc;
    }
    while start > 0 && utf8_byte_len(&units[start - 1..]) <= max_bytes {
        start -= 1;
    }
    from_units(&units[start..])
}

/// Port of `headTailUtf8(text, maxBytes, notice)`: 60/40 head/tail byte budget
/// around a fixed notice, each side truncated on UTF-8 byte boundaries.
pub fn head_tail_utf8(text: &str, max_bytes: usize, notice: &str) -> String {
    let budget = max_bytes.saturating_sub(notice.len());
    let head_budget = ((budget as f64) * 0.6).ceil() as usize;
    let tail_budget = budget.saturating_sub(head_budget);
    format!(
        "{}{}{}",
        truncate_utf8_to_bytes(text, head_budget),
        notice,
        truncate_utf8_tail_to_bytes(text, tail_budget)
    )
}

/// Port of `safeArchiveSegment(value)`: replace each run of characters outside
/// `[A-Za-z0-9_.-]` with a single `-`, then keep the first 96 UTF-16 units.
/// The regex runs over UTF-16 code units, so a supplementary character (a
/// surrogate pair) becomes two dashes, matching JS.
pub fn safe_archive_segment(value: Option<&str>) -> String {
    let source = value.unwrap_or("tool");
    let mut out: Vec<u16> = Vec::new();
    let mut in_run = false;
    for u in source.encode_utf16() {
        let allowed = (b'A' as u16..=b'Z' as u16).contains(&u)
            || (b'a' as u16..=b'z' as u16).contains(&u)
            || (b'0' as u16..=b'9' as u16).contains(&u)
            || u == b'_' as u16
            || u == b'.' as u16
            || u == b'-' as u16;
        if allowed {
            out.push(u);
            in_run = false;
        } else if !in_run {
            out.push(b'-' as u16);
            in_run = true;
        }
    }
    let sliced: Vec<u16> = out.into_iter().take(96).collect();
    if sliced.is_empty() {
        "tool".to_string()
    } else {
        from_units(&sliced)
    }
}

/// OpenAI Responses/Completions hard limit for a tool output string.
pub const OPENAI_TOOL_OUTPUT_MAX_CHARS: usize = 10_485_760;
/// Leave room for a truncation notice before the API rejects the request.
pub const OPENAI_TOOL_OUTPUT_SAFE_MAX_CHARS: usize = OPENAI_TOOL_OUTPUT_MAX_CHARS - 512;
/// Reasonix-style per-tool context budget.
pub const TOOL_OUTPUT_CONTEXT_MAX_BYTES: usize = 32 * 1024;

/// Port of `clampToolOutputText(text, maxChars)`. `text` is `None` for JS
/// `null`/`undefined`; the comparison and slice use UTF-16 code-unit length.
pub fn clamp_tool_output_text(text: Option<&str>, max_chars: usize) -> String {
    let value = text.unwrap_or("");
    let units = units(value);
    if units.len() <= max_chars {
        return value.to_string();
    }
    let notice = format!(
        "\n\n\u{2026}[truncated: tool output exceeded {} chars; original length {}]",
        max_chars,
        units.len()
    );
    let notice_len = utf16_len(&notice);
    let keep = max_chars.saturating_sub(notice_len).min(units.len());
    format!("{}{}", from_units(&units[..keep]), notice)
}

/// Result of governing an oversized text tool output.
#[derive(Debug, Clone, PartialEq)]
pub struct Governed {
    pub text: String,
    pub original_bytes: usize,
    pub archived_path: Option<String>,
}

/// Pure core of `governToolResult`: when `text` exceeds `max_bytes`, produce
/// the head/tail-elided replacement plus the notice metadata. Returns `None`
/// when the text already fits (the JS returns the result unchanged).
pub fn govern_text(text: &str, max_bytes: usize, archive_path: Option<&str>) -> Option<Governed> {
    let original_bytes = text.len();
    if original_bytes <= max_bytes {
        return None;
    }
    let location = match archive_path {
        Some(path) => format!(
            " archived at {}; use read with offset/limit to inspect it",
            path
        ),
        None => String::new(),
    };
    let notice = format!(
        "\n\n\u{2026}[elided tool result \u{2014} {} bytes{}]\n\n",
        original_bytes, location
    );
    let governed = head_tail_utf8(text, max_bytes, &notice);
    Some(Governed {
        text: governed,
        original_bytes,
        archived_path: archive_path.map(str::to_string),
    })
}
