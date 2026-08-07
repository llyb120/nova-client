//! Lyra 单文件 read：语义对齐 PI coding-agent，并为行区间 edit 增加稳定行号坐标。

use base64::Engine as _;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

const DEFAULT_MAX_LINES: usize = 2000;
const DEFAULT_MAX_BYTES: usize = 50 * 1024;

fn expand_home(input: &str) -> PathBuf {
    let trimmed = input.trim_matches(|c: char| c.is_whitespace() || c == '\u{00a0}');
    let trimmed = trimmed.strip_prefix('@').unwrap_or(trimmed);
    if trimmed == "~" || trimmed.starts_with("~/") || trimmed.starts_with("~\\") {
        if let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) {
            let suffix = trimmed.get(2..).unwrap_or_default();
            return PathBuf::from(home).join(suffix);
        }
    }
    PathBuf::from(trimmed)
}

fn path_candidates(root: &Path, input: &str) -> Vec<PathBuf> {
    let path = expand_home(input);
    let primary = if path.is_absolute() {
        path
    } else {
        root.join(path)
    };
    let mut candidates = vec![primary.clone()];
    let Some(name) = primary.file_name().and_then(|s| s.to_str()) else {
        return candidates;
    };
    let variants = [
        name.replace('’', "'").replace('‘', "'"),
        name.replace(" AM", " AM").replace(" PM", " PM"),
        name.replace(" AM", "\u{202f}AM")
            .replace(" PM", "\u{202f}PM"),
    ];
    for variant in variants {
        let candidate = primary.with_file_name(variant);
        if !candidates.contains(&candidate) {
            candidates.push(candidate);
        }
    }
    candidates
}

fn resolve_read_path(root: &Path, input: &str) -> Result<PathBuf, String> {
    if input.trim().is_empty() {
        return Err("file path is empty".into());
    }
    let candidates = path_candidates(root, input);
    candidates
        .into_iter()
        .find(|path| path.is_file())
        .ok_or_else(|| format!("File not found: {input}"))
}

fn image_mime(path: &Path) -> Option<&'static str> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "jpg" | "jpeg" => Some("image/jpeg"),
        "png" => Some("image/png"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "bmp" => Some("image/bmp"),
        _ => None,
    }
}

fn format_size(bytes: usize) -> String {
    if bytes >= 1024 {
        format!("{:.1}KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes}B")
    }
}

fn numbered(lines: &[&str], start_line: usize) -> String {
    lines
        .iter()
        .enumerate()
        .map(|(i, line)| format!("{}|{}", start_line + i, line))
        .collect::<Vec<_>>()
        .join("\n")
}

fn text_read(
    bytes: &[u8],
    offset: Option<usize>,
    limit: Option<usize>,
    line_numbers: bool,
) -> Result<Value, String> {
    let text = String::from_utf8_lossy(bytes);
    let all_lines: Vec<&str> = text.split('\n').collect();
    let total = all_lines.len();
    let start = offset.unwrap_or(1).max(1) - 1;
    if start >= total {
        return Err(format!(
            "Offset {} is beyond end of file ({total} lines total)",
            offset.unwrap_or(1)
        ));
    }

    let requested_end = limit
        .map(|n| start.saturating_add(n))
        .unwrap_or(total)
        .min(total);
    let max_end = requested_end.min(start + DEFAULT_MAX_LINES);
    let mut end = start;
    let mut content_bytes = 0usize;
    while end < max_end {
        let next = all_lines[end].as_bytes().len() + usize::from(end > start);
        if content_bytes + next > DEFAULT_MAX_BYTES {
            break;
        }
        content_bytes += next;
        end += 1;
    }

    if end == start {
        let size = all_lines[start].len();
        return Ok(json!({
            "content": format!("[Line {} is {}, exceeds {} limit. Use bash to inspect it.]", start + 1, format_size(size), format_size(DEFAULT_MAX_BYTES)),
            "truncated": true,
            "firstLineExceedsLimit": true
        }));
    }

    let selected = &all_lines[start..end];
    let mut output = if line_numbers {
        numbered(selected, start + 1)
    } else {
        selected.join("\n")
    };
    if end < total {
        let byte_limited = end < max_end;
        if limit.is_some()
            && end == requested_end
            && !byte_limited
            && requested_end <= start + DEFAULT_MAX_LINES
        {
            output.push_str(&format!(
                "\n\n[{} more lines in file. Use offset={} to continue.]",
                total - end,
                end + 1
            ));
        } else {
            let byte_note = if byte_limited {
                format!(" ({} limit)", format_size(DEFAULT_MAX_BYTES))
            } else {
                String::new()
            };
            output.push_str(&format!(
                "\n\n[Showing lines {}-{} of {}{}. Use offset={} to continue.]",
                start + 1,
                end,
                total,
                byte_note,
                end + 1
            ));
        }
    }
    Ok(json!({ "content": output }))
}

pub fn read(
    root: &Path,
    path_arg: &str,
    offset: Option<usize>,
    limit: Option<usize>,
    line_numbers: bool,
) -> Result<Vec<Value>, String> {
    let path = resolve_read_path(root, path_arg)?;
    let bytes = fs::read(&path).map_err(|e| format!("Failed to read {path_arg}: {e}"))?;
    if let Some(mime_type) = image_mime(&path) {
        return Ok(vec![
            json!({ "type": "text", "text": format!("Read image file [{mime_type}]") }),
            json!({ "type": "image", "data": base64::engine::general_purpose::STANDARD.encode(bytes), "mimeType": mime_type }),
        ]);
    }
    let result = text_read(&bytes, offset, limit, line_numbers)?;
    Ok(vec![json!({ "type": "text", "text": result["content"] })])
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn numbers_lines_preserves_cr_and_reports_continuation() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"a\r\nb\r\nc\r\n").unwrap();
        let out = read(dir.path(), "a.txt", Some(2), Some(1), true).unwrap();
        assert_eq!(
            out[0]["text"],
            "2|b\r\n\n[2 more lines in file. Use offset=3 to continue.]"
        );
    }

    #[test]
    fn rejects_offset_beyond_eof() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), "a\n").unwrap();
        assert!(read(dir.path(), "a.txt", Some(3), None, true)
            .unwrap_err()
            .contains("beyond end"));
    }
}
