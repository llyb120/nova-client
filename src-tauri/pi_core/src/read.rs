//! `read` / `read_files` line reading, ported from `readTextLines` and the
//! `read_files` tool in `alkaid-core.mjs`.
//!
//! Error *messages* are intentionally not parity-checked: node surfaces libuv
//! strings like `ENOENT: no such file or directory, open '<abs>'` while Rust
//! yields the platform `io::Error` text. The observable contract that is
//! portable — content, truncation, `nextOffset`, and error *presence* — is.

use std::path::Path;

use serde_json::{Map, Value};

use crate::encoding::{decode_text_buffer, detect_text_encoding, Encoding};
use crate::text::truncate_utf8_to_bytes;

/// Default lines read per file when no `limit` is given.
pub const DEFAULT_BATCH_READ_LINES: usize = 200;
/// Per-file byte budget for `read_files` (matches the pi coding tools).
pub const READ_FILES_MAX_BYTES: usize = 32 * 1024;

/// Split decoded text into lines using node `readline` semantics: `\r\n`, `\n`
/// and `\r` are line terminators (`\r\n` counts once), and a trailing
/// terminator does not emit an extra empty line. Empty text yields no lines.
fn split_lines_readline(text: &str) -> Vec<String> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                lines.push(std::mem::take(&mut current));
            }
            '\n' => lines.push(std::mem::take(&mut current)),
            _ => current.push(c),
        }
    }
    let ended_with_terminator = text.ends_with('\n') || text.ends_with('\r');
    if !ended_with_terminator {
        lines.push(current);
    }
    lines
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReadLines {
    pub content: String,
    pub truncated: bool,
    pub next_offset: Option<usize>,
}

/// Port of `readTextLines` core: select lines from `text` starting at 1-based
/// `offset`, collect at most `limit` lines, and stop once the joined UTF-8 byte
/// budget `max_bytes` would be exceeded (partially truncating the last line).
pub fn read_text_lines(text: &str, offset: usize, limit: usize, max_bytes: usize) -> ReadLines {
    let lines = split_lines_readline(text);
    let mut content: Vec<String> = Vec::new();
    let mut line_number = 0usize;
    let mut truncated = false;
    let mut byte_count = 0usize;

    for line in &lines {
        line_number += 1;
        if line_number < offset {
            continue;
        }
        if content.len() == limit {
            truncated = true;
            break;
        }
        let separator_bytes = if content.is_empty() { 0 } else { 1 };
        let line_bytes = line.len();
        if byte_count + separator_bytes + line_bytes > max_bytes {
            truncated = true;
            let remaining = max_bytes as isize - byte_count as isize - separator_bytes as isize;
            if remaining > 0 {
                content.push(truncate_utf8_to_bytes(line, remaining as usize));
            }
            break;
        }
        content.push(line.clone());
        byte_count += separator_bytes + line_bytes;
    }

    let joined = content.join("\n");
    let next_offset = if truncated {
        Some(offset + content.len().max(1))
    } else {
        None
    };
    ReadLines {
        content: joined,
        truncated,
        next_offset,
    }
}

/// Decode a whole file's bytes to text using the same sample-based encoding
/// detection as `readTextLines` (first ≤512 bytes), then full decode. This is
/// observationally identical to node's stream decoding for UTF-8/UTF-16LE/BE.
pub fn read_file_text(bytes: &[u8]) -> String {
    let sample_len = bytes.len().min(512);
    let (encoding, bom_bytes) = detect_text_encoding(&bytes[..sample_len]);
    match encoding {
        // node reads the whole file and byte-swaps for UTF-16BE.
        Encoding::Utf16Be => decode_text_buffer(bytes),
        Encoding::Utf16Le => decode_utf16le(&bytes[bom_bytes..]),
        Encoding::Utf8 => String::from_utf8_lossy(&bytes[bom_bytes..]).into_owned(),
    }
}

fn decode_utf16le(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    String::from_utf16_lossy(&units)
}

/// A `read_files` path request.
#[derive(Debug, Clone)]
pub struct ReadRequest {
    pub path: String,
    pub offset: Option<usize>,
    pub limit: Option<usize>,
}

/// Port of the `read_files` per-path result: resolve against `root`, read and
/// decode the file, apply offset/limit/byte-budget, and build the JSON result
/// object in node's key order (`path`, `content`, then `truncated`/`nextOffset`
/// when truncated; or `path`, `error` on failure).
pub fn read_files_one(root: &Path, request: &ReadRequest) -> Value {
    let resolved = root.join(&request.path);
    let mut map = Map::new();
    map.insert("path".to_string(), Value::String(request.path.clone()));
    match std::fs::read(&resolved) {
        Ok(bytes) => {
            let text = read_file_text(&bytes);
            let offset = request.offset.unwrap_or(1);
            let limit = request.limit.unwrap_or(DEFAULT_BATCH_READ_LINES);
            let result = read_text_lines(&text, offset, limit, READ_FILES_MAX_BYTES);
            map.insert("content".to_string(), Value::String(result.content));
            if result.truncated {
                map.insert("truncated".to_string(), Value::Bool(true));
                if let Some(next_offset) = result.next_offset {
                    map.insert("nextOffset".to_string(), Value::from(next_offset));
                }
            }
            Value::Object(map)
        }
        Err(error) => {
            map.insert("error".to_string(), Value::String(error.to_string()));
            Value::Object(map)
        }
    }
}
