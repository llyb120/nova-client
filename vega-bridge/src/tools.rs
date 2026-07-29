//! Vega 工具实现 —— read_files / edit_files / read / edit / write / bash / grep / ls / glob。
use crate::smart_edit::apply_smart_edits;
use crate::system_prompt;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::Command;

pub const DEFAULT_BATCH_READ_LINES: usize = 200;
pub const READ_FILES_MAX_BYTES: usize = 32 * 1024;
pub const TOOL_OUTPUT_CONTEXT_MAX_BYTES: usize = 32 * 1024;
pub const OPENAI_TOOL_OUTPUT_MAX_CHARS: usize = 10_485_760;
pub const OPENAI_TOOL_OUTPUT_SAFE_MAX_CHARS: usize = OPENAI_TOOL_OUTPUT_MAX_CHARS - 512;

pub struct ToolResult {
    pub content: Vec<Value>,
    pub details: Value,
}

impl ToolResult {
    pub fn text(text: impl Into<String>) -> Self {
        ToolResult { content: vec![json!({ "type": "text", "text": text.into() })], details: Value::Null }
    }
    pub fn text_with_details(text: impl Into<String>, details: Value) -> Self {
        ToolResult { content: vec![json!({ "type": "text", "text": text.into() })], details }
    }
    pub fn error(message: impl Into<String>) -> Self {
        ToolResult { content: vec![json!({ "type": "text", "text": message.into() })], details: Value::Null }
    }
}

pub fn clamp_tool_output_text(text: &str, max_chars: usize) -> String {
    if text.len() <= max_chars {
        return text.to_string();
    }
    let notice = format!("\n\n…[truncated: tool output exceeded {max_chars} chars; original length {}]", text.len());
    let keep = max_chars.saturating_sub(notice.len());
    let head: String = text.chars().take(keep).collect();
    format!("{head}{notice}")
}

/// Detect text encoding from a BOM or NUL-byte alternation heuristic (mirrors `detectTextEncoding`).
fn detect_encoding(buf: &[u8]) -> (&'static str, usize) {
    if buf.len() >= 2 && buf[0] == 0xff && buf[1] == 0xfe {
        return ("utf16le", 2);
    }
    if buf.len() >= 2 && buf[0] == 0xfe && buf[1] == 0xff {
        return ("utf16be", 2);
    }
    if buf.len() >= 3 && buf[0] == 0xef && buf[1] == 0xbb && buf[2] == 0xbf {
        return ("utf8", 3);
    }
    let sample_len = buf.len().min(512);
    let mut even_nuls = 0usize;
    let mut odd_nuls = 0usize;
    for i in 0..sample_len {
        if buf[i] != 0 { continue; }
        if i % 2 == 0 { even_nuls += 1; } else { odd_nuls += 1; }
    }
    let pairs = sample_len / 2;
    if pairs >= 4 && (odd_nuls as f64 / pairs as f64) > 0.6 && (even_nuls as f64 / pairs as f64) < 0.1 {
        return ("utf16le", 0);
    }
    if pairs >= 4 && (even_nuls as f64 / pairs as f64) > 0.6 && (odd_nuls as f64 / pairs as f64) < 0.1 {
        return ("utf16be", 0);
    }
    ("utf8", 0)
}

fn swap_utf16_bytes(buf: &[u8]) -> Vec<u8> {
    let mut out = buf.to_vec();
    let mut i = 0;
    while i + 1 < out.len() {
        out.swap(i, i + 1);
        i += 2;
    }
    out
}

pub fn decode_text_buffer(buf: &[u8]) -> String {
    let (encoding, bom) = detect_encoding(buf);
    let content = &buf[bom..];
    match encoding {
        "utf16be" => {
            let swapped = swap_utf16_bytes(content);
            String::from_utf16_lossy(&swapped.iter().step_by(2).zip(swapped.iter().skip(1).step_by(2)).map(|(&a, &b)| a as u16 | ((b as u16) << 8)).collect::<Vec<u16>>())
        }
        "utf16le" => {
            let u16s: Vec<u16> = content.chunks_exact(2).map(|c| c[0] as u16 | ((c[1] as u16) << 8)).collect();
            String::from_utf16_lossy(&u16s)
        }
        _ => String::from_utf8_lossy(content).to_string(),
    }
}

fn truncate_utf8_to_bytes(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let mut end = text.len().min(max_bytes);
    while end > 0 && text[..end].is_char_boundary(end) == false {
        end -= 1;
    }
    while end > 0 && text[..end].len() > max_bytes {
        end -= 1;
        while end > 0 && !text[..end].is_char_boundary(end) { end -= 1; }
    }
    text[..end].to_string()
}

struct ReadLinesResult {
    content: String,
    truncated: bool,
    next_offset: Option<usize>,
}

fn read_text_lines(path: &Path, offset: usize, limit: usize, max_bytes: usize) -> std::io::Result<ReadLinesResult> {
    let buf = std::fs::read(path)?;
    let text = decode_text_buffer(&buf);
    let lines: Vec<&str> = text.split('\n').collect();
    let mut content: Vec<String> = Vec::new();
    let mut byte_count = 0usize;
    let mut truncated = false;
    let mut idx = 0usize;
    for (line_no, line) in lines.iter().enumerate() {
        idx = line_no + 1;
        if idx < offset { continue; }
        if content.len() == limit {
            truncated = true;
            break;
        }
        let separator = if content.is_empty() { 0 } else { 1 };
        let line_bytes = line.len();
        if byte_count + separator + line_bytes > max_bytes {
            truncated = true;
            let remaining = max_bytes.saturating_sub(byte_count + separator);
            if remaining > 0 {
                content.push(truncate_utf8_to_bytes(line, remaining));
            }
            break;
        }
        content.push(line.to_string());
        byte_count += separator + line_bytes;
    }
    let joined = content.join("\n");
    let next_offset = if truncated { Some(offset + content.len().max(1)) } else { None };
    Ok(ReadLinesResult { content: joined, truncated, next_offset })
}

pub async fn tool_read_files(args: &Value, root: &Path) -> ToolResult {
    let Some(paths) = args.get("paths").and_then(Value::as_array) else {
        return ToolResult::error("read_files 缺少 paths");
    };
    let mut results: Vec<Value> = Vec::with_capacity(paths.len());
    for input in paths {
        let (path_str, offset, limit) = if let Some(s) = input.as_str() {
            (s.to_string(), 1usize, DEFAULT_BATCH_READ_LINES)
        } else {
            let p = input.get("path").and_then(Value::as_str).unwrap_or("").to_string();
            let off = input.get("offset").and_then(Value::as_u64).unwrap_or(1) as usize;
            let lim = input.get("limit").and_then(Value::as_u64).unwrap_or(DEFAULT_BATCH_READ_LINES as u64).min(2000) as usize;
            (p, off, lim)
        };
        let abs = root.join(&path_str);
        match read_text_lines(&abs, offset, limit, READ_FILES_MAX_BYTES) {
            Ok(r) => {
                let mut entry = json!({ "path": path_str, "content": r.content });
                if r.truncated {
                    entry["truncated"] = Value::Bool(true);
                    entry["nextOffset"] = Value::from(r.next_offset.unwrap_or(0));
                }
                results.push(entry);
            }
            Err(e) => results.push(json!({ "path": path_str, "error": e.to_string() })),
        }
    }
    ToolResult::text_with_details(serde_json::to_string(&results).unwrap_or_default(), json!({ "count": results.len() }))
}

pub async fn tool_edit_files(args: &Value, root: &Path) -> Result<ToolResult, String> {
    let Some(files) = args.get("files").and_then(Value::as_array) else {
        return Err("edit_files 缺少 files".into());
    };
    use std::collections::HashMap;
    let mut grouped: HashMap<PathBuf, (String, Vec<(String, String)>)> = HashMap::new();
    let mut order: Vec<PathBuf> = Vec::new();
    for file in files {
        let path = file.get("path").and_then(Value::as_str).unwrap_or("").to_string();
        if path.is_empty() { return Err("edit_files 缺少 path".into()); }
        let target = root.join(&path);
        let edits_arr = file.get("edits").and_then(Value::as_array).ok_or_else(|| "edit_files 缺少 edits".to_string())?;
        let mut edits: Vec<(String, String)> = Vec::new();
        for e in edits_arr {
            let old = e.get("oldText").and_then(Value::as_str).unwrap_or("").to_string();
            let new = e.get("newText").and_then(Value::as_str).unwrap_or("").to_string();
            edits.push((old, new));
        }
        if edits.is_empty() { return Err("edit_files edits 为空".into()); }
        let entry = grouped.entry(target.clone()).or_insert_with(|| { order.push(target.clone()); (path.clone(), Vec::new()) });
        entry.1.extend(edits);
    }
    let mut prepared: Vec<(String, PathBuf, String, String, Vec<Value>)> = Vec::new();
    for target in &order {
        let (path, edits) = &grouped[target];
        let raw = std::fs::read_to_string(target).map_err(|e| e.to_string())?;
        let bom = if raw.starts_with('\u{FEFF}') { "\u{FEFF}".to_string() } else { String::new() };
        let without_bom = if bom.is_empty() { raw.clone() } else { raw[bom.len()..].to_string() };
        let line_ending = if without_bom.contains("\r\n") { "\r\n" } else { "\n" };
        let normalized = without_bom.replace("\r\n", "\n");
        let result = apply_smart_edits(&normalized, edits, path)?;
        let output = if line_ending == "\r\n" {
            format!("{bom}{}", result.content.replace('\n', "\r\n"))
        } else {
            format!("{bom}{}", result.content)
        };
        prepared.push((path.clone(), target.clone(), raw.clone(), output, result.matches));
    }
    // Write all; rollback on failure.
    let mut written: Vec<(PathBuf, String)> = Vec::new();
    for (path, target, original, output, _matches) in &prepared {
        if let Err(e) = std::fs::write(target, output) {
            // rollback
            for (t, orig) in &written {
                let _ = std::fs::write(t, orig);
            }
            return Err(format!("写入 {path} 失败：{e}"));
        }
        written.push((target.clone(), original.clone()));
    }
    let paths: Vec<Value> = prepared.iter().map(|(p, _, _, _, _)| Value::String(p.clone())).collect();
    let matches: Vec<Value> = prepared.iter().map(|(p, _, _, _, m)| json!({ "path": p, "edits": m })).collect();
    Ok(ToolResult::text_with_details(format!("已并行智能编辑 {} 个文件", prepared.len()), json!({ "paths": paths, "matches": matches })))
}

pub async fn tool_read(args: &Value, root: &Path) -> ToolResult {
    let path = args.get("path").and_then(Value::as_str).unwrap_or("");
    let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(1) as usize;
    let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(DEFAULT_BATCH_READ_LINES as u64) as usize;
    let abs = root.join(path);
    match read_text_lines(&abs, offset, limit, READ_FILES_MAX_BYTES) {
        Ok(r) => {
            let mut text = r.content;
            if r.truncated {
                text.push_str(&format!("\n\n…[truncated; nextOffset {}]", r.next_offset.unwrap_or(0)));
            }
            ToolResult::text(text)
        }
        Err(e) => ToolResult::error(e.to_string()),
    }
}

pub async fn tool_edit(args: &Value, root: &Path) -> Result<ToolResult, String> {
    let path = args.get("path").and_then(Value::as_str).ok_or("edit 缺少 path")?;
    let old = args.get("oldText").and_then(Value::as_str).unwrap_or("");
    let new = args.get("newText").and_then(Value::as_str).unwrap_or("");
    let target = root.join(path);
    let raw = std::fs::read_to_string(&target).map_err(|e| e.to_string())?;
    let bom = if raw.starts_with('\u{FEFF}') { "\u{FEFF}".to_string() } else { String::new() };
    let without_bom = if bom.is_empty() { raw.clone() } else { raw[bom.len()..].to_string() };
    let line_ending = if without_bom.contains("\r\n") { "\r\n" } else { "\n" };
    let normalized = without_bom.replace("\r\n", "\n");
    let result = apply_smart_edits(&normalized, &[(old.to_string(), new.to_string())], path)?;
    let output = if line_ending == "\r\n" { format!("{bom}{}", result.content.replace('\n', "\r\n")) } else { format!("{bom}{}", result.content) };
    std::fs::write(&target, output).map_err(|e| e.to_string())?;
    Ok(ToolResult::text(format!("已编辑 {path}")))
}

pub async fn tool_write(args: &Value, root: &Path) -> Result<ToolResult, String> {
    let path = args.get("path").and_then(Value::as_str).ok_or("write 缺少 path")?;
    let content = args.get("content").and_then(Value::as_str).unwrap_or("");
    let target = root.join(path);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(&target, content).map_err(|e| e.to_string())?;
    Ok(ToolResult::text(format!("已写入 {path}")))
}

pub async fn tool_bash(args: &Value, shell: &system_prompt::ShellConfig) -> ToolResult {
    let command = args.get("command").and_then(Value::as_str).unwrap_or("");
    if command.is_empty() {
        return ToolResult::error("bash 缺少 command");
    }
    let timeout_secs = args.get("timeout").and_then(Value::as_u64).unwrap_or(120);
    let mut cmd = if shell.kind == "powershell" {
        let mut c = Command::new(&shell.shell);
        c.arg("-NoProfile").arg("-NoLogo").arg("-Command").arg(command);
        c
    } else {
        let mut c = Command::new(&shell.shell);
        c.arg("-c").arg(command);
        c
    };
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return ToolResult::error(format!("启动 shell 失败：{e}")),
    };
    let output = match tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), child.wait_with_output()).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => return ToolResult::error(e.to_string()),
        Err(_) => return ToolResult::error(format!("命令超时（{timeout_secs}s）")),
    };
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let combined = if stderr.trim().is_empty() { stdout } else { format!("{stdout}\n[stderr]\n{stderr}") };
    let code = output.status.code().unwrap_or(-1);
    let text = format!("{combined}\n[exit {code}]");
    ToolResult::text(clamp_tool_output_text(&text, 64 * 1024))
}

pub async fn tool_grep(args: &Value, root: &Path) -> ToolResult {
    let pattern = args.get("pattern").and_then(Value::as_str).unwrap_or("");
    let path = args.get("path").and_then(Value::as_str).unwrap_or(".");
    let max = args.get("maxResults").and_then(Value::as_u64).unwrap_or(100) as usize;
    let base = root.join(path);
    let re = match regex::Regex::new(pattern) {
        Ok(r) => r,
        Err(e) => return ToolResult::error(format!("非法正则：{e}")),
    };
    let mut hits: Vec<String> = Vec::new();
    grep_walk(&base, root, &re, max, &mut hits);
    ToolResult::text(hits.join("\n"))
}

fn grep_walk(dir: &Path, root: &Path, re: &regex::Regex, max: usize, hits: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        if hits.len() >= max { return; }
        let p = entry.path();
        if p.is_dir() {
            let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            if matches!(name.as_str(), ".git" | "node_modules" | "target" | "dist" | ".next" | "build") { continue; }
            grep_walk(&p, root, re, max, hits);
        } else if p.is_file() {
            if let Ok(text) = std::fs::read_to_string(&p) {
                for (i, line) in text.lines().enumerate() {
                    if re.is_match(line) {
                        let rel = p.strip_prefix(root).unwrap_or(&p).to_string_lossy().replace('\\', "/");
                        hits.push(format!("{rel}:{}:{}", i + 1, line.trim_end()));
                        if hits.len() >= max { return; }
                    }
                }
            }
        }
    }
}

pub async fn tool_ls(args: &Value, root: &Path) -> ToolResult {
    let path = args.get("path").and_then(Value::as_str).unwrap_or(".");
    let base = root.join(path);
    let Ok(entries) = std::fs::read_dir(&base) else {
        return ToolResult::error(format!("无法读取目录：{}", base.display()));
    };
    let mut lines: Vec<String> = Vec::new();
    let mut items: Vec<_> = entries.flatten().collect();
    items.sort_by_key(|e| e.file_name());
    for entry in items {
        let name = entry.file_name().to_string_lossy().to_string();
        let kind = if entry.path().is_dir() { "dir" } else { "file" };
        lines.push(format!("{kind}\t{name}"));
    }
    ToolResult::text(lines.join("\n"))
}

pub async fn tool_glob(args: &Value, root: &Path) -> ToolResult {
    let pattern = args.get("pattern").and_then(Value::as_str).unwrap_or("");
    let path = args.get("path").and_then(Value::as_str).unwrap_or(".");
    let base = root.join(path);
    let mut hits: Vec<String> = Vec::new();
    let glob = glob_pattern::compile(pattern);
    glob_walk(&base, root, &glob, &mut hits);
    hits.sort();
    ToolResult::text(hits.join("\n"))
}

fn glob_walk(dir: &Path, root: &Path, glob: &glob_pattern::Glob, hits: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let p = entry.path();
        let rel = p.strip_prefix(root).unwrap_or(&p).to_string_lossy().replace('\\', "/");
        if p.is_dir() {
            let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            if matches!(name.as_str(), ".git" | "node_modules" | "target" | "dist") { continue; }
            if glob.matches(&rel) { hits.push(rel.clone()); }
            glob_walk(&p, root, glob, hits);
        } else if glob.matches(&rel) {
            hits.push(rel);
        }
    }
}

mod glob_pattern {
    pub struct Glob { segments: Vec<String> }
    pub fn compile(pattern: &str) -> Glob {
        let segments = pattern.split('/').map(str::to_string).collect();
        Glob { segments }
    }
    impl Glob {
        pub fn matches(&self, path: &str) -> bool {
            let parts: Vec<&str> = path.split('/').collect();
            self.match_segments(&self.segments, &parts)
        }
        fn match_segments(&self, segs: &[String], parts: &[&str]) -> bool {
            if segs.is_empty() { return parts.is_empty(); }
            let seg = &segs[0];
            if seg == "**" {
                for i in 0..=parts.len() {
                    if self.match_segments(&segs[1..], &parts[i..]) { return true; }
                }
                return false;
            }
            if parts.is_empty() { return false; }
            if match_one(seg, parts[0]) {
                return self.match_segments(&segs[1..], &parts[1..]);
            }
            false
        }
    }
    fn match_one(pat: &str, text: &str) -> bool {
        // Convert glob to regex: * -> [^/]*, ? -> [^/], escape others.
        let mut regex = String::from("^");
        let mut chars = pat.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '*' => regex.push_str("[^/]*"),
                '?' => regex.push_str("[^/]"),
                '.' | '+' | '(' | ')' | '|' | '^' | '$' | '\\' | '{' | '}' => { regex.push('\\'); regex.push(c); }
                '[' => {
                    // pass through character class roughly
                    regex.push('[');
                    while let Some(&n) = chars.peek() {
                        regex.push(n);
                        chars.next();
                        if n == ']' { break; }
                    }
                }
                _ => regex.push(c),
            }
        }
        regex.push('$');
        regex::Regex::new(&regex).map(|r| r.is_match(text)).unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write_file(root: &Path, rel: &str, content: &str) {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, content).unwrap();
    }

    fn write_bytes(root: &Path, rel: &str, bytes: &[u8]) {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, bytes).unwrap();
    }

    #[test]
    fn detect_encoding_utf8_bom_stripped() {
        let buf = [0xEF, 0xBB, 0xBF, b'h', b'i'];
        let (enc, bom) = detect_encoding(&buf);
        assert_eq!(enc, "utf8");
        assert_eq!(bom, 3);
    }

    #[test]
    fn detect_encoding_utf16le_bom() {
        let buf = [0xFF, 0xFE, b'h', 0x00];
        let (enc, bom) = detect_encoding(&buf);
        assert_eq!(enc, "utf16le");
        assert_eq!(bom, 2);
    }

    #[test]
    fn detect_encoding_utf16be_bom() {
        let buf = [0xFE, 0xFF, 0x00, b'h'];
        let (enc, bom) = detect_encoding(&buf);
        assert_eq!(enc, "utf16be");
        assert_eq!(bom, 2);
    }

    #[test]
    fn decode_utf16le_bom_text() {
        // "hi" in UTF-16LE with BOM
        let buf = [0xFF, 0xFE, b'h', 0x00, b'i', 0x00];
        assert_eq!(decode_text_buffer(&buf), "hi");
    }

    #[test]
    fn decode_utf16be_bom_text() {
        // "hi" in UTF-16BE with BOM
        let buf = [0xFE, 0xFF, 0x00, b'h', 0x00, b'i'];
        assert_eq!(decode_text_buffer(&buf), "hi");
    }

    #[test]
    fn decode_utf8_bom_text() {
        let buf = [0xEF, 0xBB, 0xBF, b'h', b'i'];
        assert_eq!(decode_text_buffer(&buf), "hi");
    }

    #[test]
    fn clamp_tool_output_keeps_short_text() {
        assert_eq!(clamp_tool_output_text("short", 100), "short");
    }

    #[test]
    fn clamp_tool_output_truncates_long_text() {
        let big = "x".repeat(200);
        let out = clamp_tool_output_text(&big, 100);
        assert!(out.contains("truncated"));
        assert!(out.len() <= 100);
    }

    #[tokio::test]
    async fn tool_read_basic() {
        let dir = tempdir().unwrap();
        write_file(dir.path(), "a.txt", "line1\nline2\nline3\n");
        let args = json!({ "path": "a.txt" });
        let result = tool_read(&args, dir.path()).await;
        let text = result.content[0]["text"].as_str().unwrap();
        assert!(text.contains("line1"));
        assert!(text.contains("line3"));
    }

    #[tokio::test]
    async fn tool_read_with_offset_and_limit() {
        let dir = tempdir().unwrap();
        let content: Vec<String> = (1..=10).map(|i| format!("line{i}")).collect();
        write_file(dir.path(), "b.txt", &content.join("\n"));
        let args = json!({ "path": "b.txt", "offset": 3, "limit": 2 });
        let result = tool_read(&args, dir.path()).await;
        let text = result.content[0]["text"].as_str().unwrap();
        // offset=3 skips lines 1-2; limit=2 returns lines 3-4 then marks truncated
        // because the limit was reached (more lines remain).
        assert!(text.contains("line3"));
        assert!(text.contains("line4"));
        assert!(text.contains("truncated"));
        assert!(text.contains("nextOffset 5"));
        // Lines after the limit must not appear.
        assert!(!text.contains("line5"));
    }

    #[tokio::test]
    async fn tool_read_utf16le_file() {
        let dir = tempdir().unwrap();
        // "hello\nworld\n" in UTF-16LE with BOM
        let mut buf = vec![0xFF, 0xFE];
        for &b in "hello\nworld\n".as_bytes() {
            buf.push(b);
            buf.push(0x00);
        }
        write_bytes(dir.path(), "u16.txt", &buf);
        let args = json!({ "path": "u16.txt" });
        let result = tool_read(&args, dir.path()).await;
        let text = result.content[0]["text"].as_str().unwrap();
        assert!(text.contains("hello"));
        assert!(text.contains("world"));
    }

    #[tokio::test]
    async fn tool_read_truncates_at_max_bytes() {
        let dir = tempdir().unwrap();
        let big = "x".repeat(READ_FILES_MAX_BYTES + 1000);
        write_file(dir.path(), "big.txt", &big);
        let args = json!({ "path": "big.txt" });
        let result = tool_read(&args, dir.path()).await;
        let text = result.content[0]["text"].as_str().unwrap();
        assert!(text.contains("truncated"));
    }

    #[tokio::test]
    async fn tool_read_files_batch() {
        let dir = tempdir().unwrap();
        write_file(dir.path(), "a.txt", "aaa");
        write_file(dir.path(), "b.txt", "bbb");
        let args = json!({ "paths": ["a.txt", "b.txt"] });
        let result = tool_read_files(&args, dir.path()).await;
        let text = result.content[0]["text"].as_str().unwrap();
        assert!(text.contains("aaa"));
        assert!(text.contains("bbb"));
    }

    #[tokio::test]
    async fn tool_edit_exact_match() {
        let dir = tempdir().unwrap();
        write_file(dir.path(), "e.txt", "foo\nbar\nbaz\n");
        let args = json!({ "path": "e.txt", "oldText": "bar", "newText": "BAR" });
        let result = tool_edit(&args, dir.path()).await.unwrap();
        assert!(result.content[0]["text"].as_str().unwrap().contains("已编辑"));
        let new = fs::read_to_string(dir.path().join("e.txt")).unwrap();
        assert!(new.contains("BAR"));
        assert!(!new.contains("bar"));
    }

    #[tokio::test]
    async fn tool_edit_preserves_crlf() {
        let dir = tempdir().unwrap();
        write_file(dir.path(), "crlf.txt", "foo\r\nbar\r\n");
        let args = json!({ "path": "crlf.txt", "oldText": "bar", "newText": "BAR" });
        let _ = tool_edit(&args, dir.path()).await;
        let new = fs::read_to_string(dir.path().join("crlf.txt")).unwrap();
        assert!(new.contains("BAR\r\n"));
    }

    #[tokio::test]
    async fn tool_edit_files_batch() {
        let dir = tempdir().unwrap();
        write_file(dir.path(), "a.txt", "one");
        write_file(dir.path(), "b.txt", "two");
        let args = json!({
            "files": [
                { "path": "a.txt", "edits": [{ "oldText": "one", "newText": "ONE" }] },
                { "path": "b.txt", "edits": [{ "oldText": "two", "newText": "TWO" }] }
            ]
        });
        let result = tool_edit_files(&args, dir.path()).await;
        assert!(result.is_ok());
        assert_eq!(fs::read_to_string(dir.path().join("a.txt")).unwrap(), "ONE");
        assert_eq!(fs::read_to_string(dir.path().join("b.txt")).unwrap(), "TWO");
    }

    #[tokio::test]
    async fn tool_write_creates_nested_file() {
        let dir = tempdir().unwrap();
        let args = json!({ "path": "sub/dir/new.txt", "content": "hello" });
        let result = tool_write(&args, dir.path()).await;
        assert!(result.is_ok());
        assert_eq!(fs::read_to_string(dir.path().join("sub/dir/new.txt")).unwrap(), "hello");
    }

    #[tokio::test]
    async fn tool_grep_finds_matches() {
        let dir = tempdir().unwrap();
        write_file(dir.path(), "a.rs", "fn foo() {}\nfn bar() {}\n");
        write_file(dir.path(), "b.rs", "fn foo() {}\n");
        let args = json!({ "pattern": "fn foo", "path": "." });
        let result = tool_grep(&args, dir.path()).await;
        let text = result.content[0]["text"].as_str().unwrap();
        // Both files contain "fn foo" — at least two hits.
        assert_eq!(text.lines().filter(|l| l.contains("fn foo")).count(), 2);
    }

    #[tokio::test]
    async fn tool_grep_skips_ignored_dirs() {
        let dir = tempdir().unwrap();
        write_file(dir.path(), "src/a.rs", "target_marker\n");
        write_file(dir.path(), "target/a.rs", "target_marker\n");
        let args = json!({ "pattern": "target_marker", "path": "." });
        let result = tool_grep(&args, dir.path()).await;
        let text = result.content[0]["text"].as_str().unwrap();
        // target/ is skipped; only src/a.rs should match.
        assert_eq!(text.lines().filter(|l| l.contains("target_marker")).count(), 1);
        assert!(text.contains("src/a.rs"));
    }

    #[tokio::test]
    async fn tool_ls_lists_directory() {
        let dir = tempdir().unwrap();
        write_file(dir.path(), "a.txt", "x");
        fs::create_dir_all(dir.path().join("sub")).unwrap();
        let args = json!({ "path": "." });
        let result = tool_ls(&args, dir.path()).await;
        let text = result.content[0]["text"].as_str().unwrap();
        assert!(text.contains("file\ta.txt"));
        assert!(text.contains("dir\tsub"));
    }

    #[tokio::test]
    async fn tool_glob_matches_pattern() {
        let dir = tempdir().unwrap();
        write_file(dir.path(), "a.rs", "x");
        write_file(dir.path(), "b.ts", "x");
        write_file(dir.path(), "sub/c.rs", "x");
        let args = json!({ "pattern": "**/*.rs", "path": "." });
        let result = tool_glob(&args, dir.path()).await;
        let text = result.content[0]["text"].as_str().unwrap();
        let hits: Vec<&str> = text.lines().filter(|l| !l.is_empty()).collect();
        assert!(hits.iter().any(|h| h.ends_with("a.rs")));
        assert!(hits.iter().any(|h| h.ends_with("c.rs")));
        assert!(!hits.iter().any(|h| h.ends_with("b.ts")));
    }

    #[tokio::test]
    async fn tool_bash_powershell_branch() {
        // Use the actual shell available on the test host. On Windows that's typically
        // powershell.exe; on Unix it's bash/sh. We just verify the command runs and
        // produces an exit code line.
        let shell = system_prompt::detect_shell_config();
        let args = json!({ "command": "echo vega_test_marker", "timeout": 10 });
        let result = tool_bash(&args, &shell).await;
        let text = result.content[0]["text"].as_str().unwrap();
        assert!(text.contains("vega_test_marker"));
        assert!(text.contains("[exit"));
    }

    #[tokio::test]
    async fn tool_bash_empty_command_rejected() {
        let shell = system_prompt::ShellConfig { shell: "bash".into(), kind: "bash".into() };
        let args = json!({ "command": "" });
        let result = tool_bash(&args, &shell).await;
        let text = result.content[0]["text"].as_str().unwrap();
        assert!(text.contains("bash 缺少 command"));
    }

    #[test]
    fn glob_match_one_star() {
        let g = glob_pattern::compile("*.rs");
        assert!(g.matches("a.rs"));
        assert!(!g.matches("a.ts"));
        assert!(!g.matches("sub/a.rs")); // * does not cross /
    }

    #[test]
    fn glob_double_star_crosses_dirs() {
        let g = glob_pattern::compile("**/*.rs");
        assert!(g.matches("a.rs"));
        assert!(g.matches("sub/a.rs"));
        assert!(g.matches("sub/deep/a.rs"));
        assert!(!g.matches("a.ts"));
    }
}
