//! Native tool executor — the `ToolFn` implementation that makes the ported
//! agent run without node. Dispatches each tool name to a real implementation
//! built on `pi_core`'s ported logic plus `std` filesystem/process I/O.
//!
//! The deterministic tools (`read`, `read_files`, `edit`, `edit_files`,
//! `write`, `ls`) reproduce the node tool output and are parity-tested via
//! fixtures. `bash`, `grep`, and `find` shell out to the real programs; their
//! output is environment-dependent and therefore not byte-parity-checked.

use std::path::PathBuf;
use std::process::Command;

use serde_json::{json, Value};

use crate::edit_diff::{
    apply_edits_to_normalized_content, detect_line_ending, normalize_to_lf, restore_line_endings,
    strip_bom,
};
use crate::ls::ls_tool;
use crate::paths::resolve_to_cwd;
use crate::read::read_files_one;
use crate::smart_edit::apply_smart_edits;
use crate::truncate::{format_size, truncate_head, DEFAULT_MAX_BYTES};
use crate::write::write_tool;

fn text_result(text: &str) -> Value {
    json!({ "content": [{ "type": "text", "text": text }] })
}

fn error_result(message: &str) -> Value {
    json!({ "content": [{ "type": "text", "text": message }] })
}

/// A native tool executor rooted at a working directory.
pub struct NativeTools {
    pub cwd: PathBuf,
    /// Shell used by the `bash` tool (defaults to `/bin/bash`).
    pub shell: String,
}

impl NativeTools {
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        NativeTools {
            cwd: cwd.into(),
            shell: "/bin/bash".to_string(),
        }
    }

    fn cwd_str(&self) -> String {
        self.cwd.to_string_lossy().into_owned()
    }

    /// Dispatch a tool call. Returns `(result, isError)` matching the `ToolFn`
    /// contract: `result` is `{ content: [...], details? }`.
    pub fn execute(&self, name: &str, args: &Value) -> (Value, bool) {
        match name {
            "read" => self.tool_read(args),
            "read_files" => self.tool_read_files(args),
            "edit" => self.tool_edit(args),
            "edit_files" => self.tool_edit_files(args),
            "write" => self.tool_write(args),
            "ls" => self.tool_ls(args),
            "bash" => self.tool_bash(args),
            "grep" => self.tool_grep(args),
            "find" => self.tool_find(args),
            _ => (error_result(&format!("Tool {name} not found")), true),
        }
    }

    /// Port of the pi-coding-agent single-file `read` tool.
    fn tool_read(&self, args: &Value) -> (Value, bool) {
        let path = args.get("path").and_then(Value::as_str).unwrap_or("");
        let offset = args.get("offset").and_then(Value::as_u64).map(|v| v as usize);
        let limit = args.get("limit").and_then(Value::as_u64).map(|v| v as usize);

        let absolute = resolve_to_cwd(path, &self.cwd_str());
        let bytes = match std::fs::read(&absolute) {
            Ok(bytes) => bytes,
            Err(error) => return (error_result(&error.to_string()), true),
        };
        let text_content = String::from_utf8_lossy(&bytes).into_owned();
        let all_lines: Vec<&str> = text_content.split('\n').collect();
        let total_file_lines = all_lines.len();

        let start_line = offset.map(|o| o.saturating_sub(1)).unwrap_or(0);
        if start_line >= all_lines.len() {
            return (
                error_result(&format!(
                    "Offset {} is beyond end of file ({} lines total)",
                    offset.unwrap_or(0),
                    all_lines.len()
                )),
                true,
            );
        }
        let start_line_display = start_line + 1;

        let (selected_content, user_limited_lines) = if let Some(limit) = limit {
            let end_line = (start_line + limit).min(all_lines.len());
            (
                all_lines[start_line..end_line].join("\n"),
                Some(end_line - start_line),
            )
        } else {
            (all_lines[start_line..].join("\n"), None)
        };

        let truncation = truncate_head(&selected_content, None, None);
        let mut details: Option<Value> = None;
        let output_text = if truncation.first_line_exceeds_limit {
            let first_line_size = format_size(all_lines[start_line].len());
            details = Some(json!({ "truncation": truncation }));
            format!(
                "[Line {} is {}, exceeds {} limit. Use bash: sed -n '{}p' {} | head -c {}]",
                start_line_display,
                first_line_size,
                format_size(DEFAULT_MAX_BYTES),
                start_line_display,
                path,
                DEFAULT_MAX_BYTES
            )
        } else if truncation.truncated {
            let end_line_display = start_line_display + truncation.output_lines - 1;
            let next_offset = end_line_display + 1;
            let notice = if truncation.truncated_by.as_deref() == Some("lines") {
                format!(
                    "\n\n[Showing lines {}-{} of {}. Use offset={} to continue.]",
                    start_line_display, end_line_display, total_file_lines, next_offset
                )
            } else {
                format!(
                    "\n\n[Showing lines {}-{} of {} ({} limit). Use offset={} to continue.]",
                    start_line_display,
                    end_line_display,
                    total_file_lines,
                    format_size(DEFAULT_MAX_BYTES),
                    next_offset
                )
            };
            details = Some(json!({ "truncation": truncation }));
            format!("{}{}", truncation.content, notice)
        } else if let Some(user_limited) = user_limited_lines {
            if start_line + user_limited < all_lines.len() {
                let remaining = all_lines.len() - (start_line + user_limited);
                let next_offset = start_line + user_limited + 1;
                format!(
                    "{}\n\n[{} more lines in file. Use offset={} to continue.]",
                    truncation.content, remaining, next_offset
                )
            } else {
                truncation.content.clone()
            }
        } else {
            truncation.content.clone()
        };

        let mut result = json!({ "content": [{ "type": "text", "text": output_text }] });
        if let Some(details) = details {
            result["details"] = details;
        }
        (result, false)
    }

    /// Port of the alkaid batch `read_files` tool.
    fn tool_read_files(&self, args: &Value) -> (Value, bool) {
        let paths = args.get("paths").and_then(Value::as_array);
        let Some(paths) = paths else {
            return (error_result("read_files: paths must be an array"), true);
        };
        let root = self.cwd.clone();
        let results: Vec<Value> = paths
            .iter()
            .map(|input| {
                let request = crate::read::ReadRequest {
                    path: match input {
                        Value::String(path) => path.clone(),
                        Value::Object(_) => input
                            .get("path")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        _ => String::new(),
                    },
                    offset: input.get("offset").and_then(Value::as_u64).map(|v| v as usize),
                    limit: input.get("limit").and_then(Value::as_u64).map(|v| v as usize),
                };
                read_files_one(&root, &request)
            })
            .collect();
        let text = serde_json::to_string(&results).unwrap_or_else(|_| "[]".to_string());
        (
            json!({ "content": [{ "type": "text", "text": text }], "details": { "count": results.len() } }),
            false,
        )
    }

    /// Port of the pi-coding-agent single-file `edit` tool.
    fn tool_edit(&self, args: &Value) -> (Value, bool) {
        let path = args.get("path").and_then(Value::as_str).unwrap_or("");
        let edits = match parse_edits(args) {
            Some(edits) => edits,
            None => {
                return (
                    error_result(
                        "Edit tool input is invalid. edits must contain at least one replacement.",
                    ),
                    true,
                )
            }
        };
        let absolute = resolve_to_cwd(path, &self.cwd_str());
        let raw = match std::fs::read_to_string(&absolute) {
            Ok(raw) => raw,
            Err(error) => return (error_result(&error.to_string()), true),
        };
        let (bom, content) = strip_bom(&raw);
        let line_ending = detect_line_ending(content);
        let normalized = normalize_to_lf(content);
        match apply_edits_to_normalized_content(&normalized, &edits, path) {
            Ok(result) => {
                let restored = restore_line_endings(&result.new_content, line_ending);
                let output = format!("{bom}{restored}");
                if let Err(error) = std::fs::write(&absolute, output) {
                    return (error_result(&error.to_string()), true);
                }
                (
                    json!({
                        "content": [{ "type": "text", "text": format!("Successfully replaced {} block(s) in {}.", edits.len(), path) }],
                        "details": { "firstChangedLine": null },
                    }),
                    false,
                )
            }
            Err(error) => (error_result(&error), true),
        }
    }

    /// Port of the alkaid batch `edit_files` tool.
    fn tool_edit_files(&self, args: &Value) -> (Value, bool) {
        let files = args.get("files").and_then(Value::as_array);
        let Some(files) = files else {
            return (error_result("edit_files: files must be an array"), true);
        };
        // Group edits by resolved target path (later entries append).
        let mut grouped: Vec<(String, PathBuf, Vec<(String, String)>)> = Vec::new();
        for file in files {
            let path = file.get("path").and_then(Value::as_str).unwrap_or("").to_string();
            let target = PathBuf::from(resolve_to_cwd(&path, &self.cwd_str()));
            let edits = file
                .get("edits")
                .and_then(Value::as_array)
                .map(|edits| {
                    edits
                        .iter()
                        .map(|edit| {
                            (
                                edit.get("oldText").and_then(Value::as_str).unwrap_or("").to_string(),
                                edit.get("newText").and_then(Value::as_str).unwrap_or("").to_string(),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            if let Some(existing) = grouped.iter_mut().find(|(_, t, _)| *t == target) {
                existing.2.extend(edits);
            } else {
                grouped.push((path, target, edits));
            }
        }

        // Prepare every edit against immutable snapshots before writing anything.
        let mut prepared: Vec<(String, PathBuf, String, String)> = Vec::new();
        for (path, target, edits) in &grouped {
            let raw = match std::fs::read_to_string(target) {
                Ok(raw) => raw,
                Err(error) => return (error_result(&error.to_string()), true),
            };
            let bom = if raw.starts_with('\u{FEFF}') { "\u{FEFF}" } else { "" };
            let without_bom = raw.strip_prefix('\u{FEFF}').unwrap_or(&raw);
            let line_ending = if without_bom.contains("\r\n") { "\r\n" } else { "\n" };
            let normalized = without_bom.replace("\r\n", "\n");
            match apply_smart_edits(&normalized, edits, path) {
                Ok(result) => {
                    let body = if line_ending == "\r\n" {
                        result.content.replace('\n', "\r\n")
                    } else {
                        result.content
                    };
                    prepared.push((path.clone(), target.clone(), raw, format!("{bom}{body}")));
                }
                Err(error) => return (error_result(&error), true),
            }
        }

        // Write all, rolling back on first failure.
        let mut written: Vec<usize> = Vec::new();
        for (index, (_, target, _, output)) in prepared.iter().enumerate() {
            if let Err(error) = std::fs::write(target, output) {
                for &done in &written {
                    let _ = std::fs::write(&prepared[done].1, &prepared[done].2);
                }
                return (error_result(&error.to_string()), true);
            }
            written.push(index);
        }

        let paths: Vec<Value> = prepared.iter().map(|(p, _, _, _)| json!(p)).collect();
        (
            json!({
                "content": [{ "type": "text", "text": format!("已并行智能编辑 {} 个文件", prepared.len()) }],
                "details": { "paths": paths },
            }),
            false,
        )
    }

    /// Port of the pi-coding-agent `write` tool.
    fn tool_write(&self, args: &Value) -> (Value, bool) {
        let path = args.get("path").and_then(Value::as_str).unwrap_or("");
        let content = args.get("content").and_then(Value::as_str).unwrap_or("");
        match write_tool(&self.cwd_str(), path, content) {
            Ok(message) => (text_result(&message), false),
            Err(error) => (error_result(&error), true),
        }
    }

    /// Port of the pi-coding-agent `ls` tool.
    fn tool_ls(&self, args: &Value) -> (Value, bool) {
        let path = args.get("path").and_then(Value::as_str);
        let limit = args.get("limit").and_then(Value::as_u64).map(|v| v as usize);
        match ls_tool(&self.cwd_str(), path, limit) {
            Ok(output) => {
                let mut result = json!({ "content": [{ "type": "text", "text": output.text }] });
                if let Some(details) = output.details {
                    result["details"] = details;
                }
                (result, false)
            }
            Err(error) => (error_result(&error), true),
        }
    }

    /// Functional `bash` tool: run the command in the configured shell, capture
    /// combined output and exit status. Output is environment-dependent.
    fn tool_bash(&self, args: &Value) -> (Value, bool) {
        let command = args.get("command").and_then(Value::as_str).unwrap_or("");
        let output = Command::new(&self.shell)
            .arg("-c")
            .arg(command)
            .current_dir(&self.cwd)
            .output();
        match output {
            Ok(output) => {
                let mut text = String::new();
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                if !stdout.is_empty() {
                    text.push_str(&stdout);
                }
                if !stderr.is_empty() {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(&stderr);
                }
                let code = output.status.code().unwrap_or(-1);
                if code != 0 {
                    text.push_str(&format!("\n[exit code: {code}]"));
                }
                (text_result(&text), code != 0)
            }
            Err(error) => (error_result(&error.to_string()), true),
        }
    }

    /// Functional `grep` tool backed by `rg` (respects .gitignore like node).
    fn tool_grep(&self, args: &Value) -> (Value, bool) {
        let pattern = args.get("pattern").and_then(Value::as_str).unwrap_or("");
        let path = args.get("path").and_then(Value::as_str).unwrap_or(".");
        let search_path = resolve_to_cwd(path, &self.cwd_str());
        let mut cmd = Command::new("rg");
        cmd.arg("--line-number").arg("--color=never").arg("--hidden");
        if args.get("ignoreCase").and_then(Value::as_bool).unwrap_or(false) {
            cmd.arg("--ignore-case");
        }
        if args.get("literal").and_then(Value::as_bool).unwrap_or(false) {
            cmd.arg("--fixed-strings");
        }
        if let Some(glob) = args.get("glob").and_then(Value::as_str) {
            cmd.arg("--glob").arg(glob);
        }
        cmd.arg("--").arg(pattern).arg(&search_path);
        run_capture(cmd)
    }

    /// Functional `find` tool backed by `fd`.
    fn tool_find(&self, args: &Value) -> (Value, bool) {
        let pattern = args.get("pattern").and_then(Value::as_str).unwrap_or("");
        let path = args.get("path").and_then(Value::as_str).unwrap_or(".");
        let search_path = resolve_to_cwd(path, &self.cwd_str());
        let mut cmd = Command::new("fd");
        cmd.arg("--glob").arg("--color=never").arg("--hidden");
        cmd.arg("--").arg(pattern).arg(&search_path);
        run_capture(cmd)
    }
}

/// Parse the `edits` array, tolerating the legacy flat `{oldText,newText}` form
/// and a JSON-string `edits` (pi-coding-agent `prepareEditArguments`).
fn parse_edits(args: &Value) -> Option<Vec<(String, String)>> {
    let mut edits: Vec<(String, String)> = Vec::new();
    if let Some(Value::String(encoded)) = args.get("edits") {
        if let Ok(Value::Array(parsed)) = serde_json::from_str::<Value>(encoded) {
            for edit in parsed {
                edits.push((
                    edit.get("oldText").and_then(Value::as_str).unwrap_or("").to_string(),
                    edit.get("newText").and_then(Value::as_str).unwrap_or("").to_string(),
                ));
            }
        }
    } else if let Some(arr) = args.get("edits").and_then(Value::as_array) {
        for edit in arr {
            edits.push((
                edit.get("oldText").and_then(Value::as_str).unwrap_or("").to_string(),
                edit.get("newText").and_then(Value::as_str).unwrap_or("").to_string(),
            ));
        }
    }
    if let (Some(old), Some(new)) = (
        args.get("oldText").and_then(Value::as_str),
        args.get("newText").and_then(Value::as_str),
    ) {
        edits.push((old.to_string(), new.to_string()));
    }
    if edits.is_empty() {
        None
    } else {
        Some(edits)
    }
}

fn run_capture(mut cmd: Command) -> (Value, bool) {
    match cmd.output() {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            let code = output.status.code().unwrap_or(-1);
            // rg/fd exit 1 means "no matches" (not an error).
            let is_error = code != 0 && code != 1;
            let mut text = stdout;
            if is_error && !stderr.is_empty() {
                text.push_str(&stderr);
            }
            (text_result(text.trim_end()), is_error)
        }
        Err(error) => (error_result(&error.to_string()), true),
    }
}

/// Convenience adapter matching the agent `ToolFn` signature.
pub fn tool_fn_for(tools: &NativeTools) -> impl FnMut(&str, &str, &Value) -> (Value, bool) + '_ {
    move |_id: &str, name: &str, args: &Value| tools.execute(name, args)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, NativeTools) {
        let dir = tempfile::tempdir().unwrap();
        let tools = NativeTools::new(dir.path().to_path_buf());
        (dir, tools)
    }

    #[test]
    fn read_basic_and_offset() {
        let (dir, tools) = setup();
        std::fs::write(dir.path().join("f.txt"), "l1\nl2\nl3\nl4\n").unwrap();
        let (result, is_error) = tools.execute("read", &json!({ "path": "f.txt" }));
        assert!(!is_error);
        // JS split("\n") keeps the trailing empty element, so the trailing
        // newline is preserved in the output.
        assert_eq!(result["content"][0]["text"], json!("l1\nl2\nl3\nl4\n"));

        let (result, _) = tools.execute("read", &json!({ "path": "f.txt", "offset": 2, "limit": 2 }));
        assert_eq!(result["content"][0]["text"], json!("l2\nl3\n\n[2 more lines in file. Use offset=4 to continue.]"));
    }

    #[test]
    fn read_offset_beyond_end_errors() {
        let (dir, tools) = setup();
        std::fs::write(dir.path().join("f.txt"), "a\nb\n").unwrap();
        let (_, is_error) = tools.execute("read", &json!({ "path": "f.txt", "offset": 99 }));
        assert!(is_error);
    }

    #[test]
    fn read_files_batch() {
        let (dir, tools) = setup();
        std::fs::write(dir.path().join("a.txt"), "x\ny\n").unwrap();
        let (result, is_error) = tools.execute("read_files", &json!({ "paths": ["a.txt", "missing.txt"] }));
        assert!(!is_error);
        let parsed: Value = serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(parsed[0]["content"], json!("x\ny"));
        assert!(parsed[1].get("error").is_some());
        assert_eq!(result["details"]["count"], json!(2));
    }

    #[test]
    fn edit_single_exact_and_fuzzy() {
        let (dir, tools) = setup();
        std::fs::write(dir.path().join("e.txt"), "alpha\nbeta\ngamma\n").unwrap();
        let (result, is_error) = tools.execute(
            "edit",
            &json!({ "path": "e.txt", "edits": [{ "oldText": "beta", "newText": "BETA" }] }),
        );
        assert!(!is_error);
        assert_eq!(result["content"][0]["text"], json!("Successfully replaced 1 block(s) in e.txt."));
        assert_eq!(std::fs::read_to_string(dir.path().join("e.txt")).unwrap(), "alpha\nBETA\ngamma\n");

        // fuzzy: trailing whitespace in file
        std::fs::write(dir.path().join("e2.txt"), "keep me   \nother\n").unwrap();
        let (_, is_error) = tools.execute(
            "edit",
            &json!({ "path": "e2.txt", "edits": [{ "oldText": "keep me", "newText": "changed" }] }),
        );
        assert!(!is_error);
        assert_eq!(std::fs::read_to_string(dir.path().join("e2.txt")).unwrap(), "changed   \nother\n");
    }

    #[test]
    fn edit_files_batch() {
        let (dir, tools) = setup();
        std::fs::write(dir.path().join("m1.txt"), "one\n").unwrap();
        std::fs::write(dir.path().join("m2.txt"), "two\n").unwrap();
        let (result, is_error) = tools.execute(
            "edit_files",
            &json!({ "files": [
                { "path": "m1.txt", "edits": [{ "oldText": "one", "newText": "ONE" }] },
                { "path": "m2.txt", "edits": [{ "oldText": "two", "newText": "TWO" }] },
            ] }),
        );
        assert!(!is_error);
        assert_eq!(result["content"][0]["text"], json!("已并行智能编辑 2 个文件"));
        assert_eq!(std::fs::read_to_string(dir.path().join("m1.txt")).unwrap(), "ONE\n");
        assert_eq!(std::fs::read_to_string(dir.path().join("m2.txt")).unwrap(), "TWO\n");
    }

    #[test]
    fn write_creates_and_reports_utf16_length() {
        let (dir, tools) = setup();
        let (result, is_error) = tools.execute(
            "write",
            &json!({ "path": "nested/new.txt", "content": "中文😀" }),
        );
        assert!(!is_error);
        assert_eq!(result["content"][0]["text"], json!("Successfully wrote 4 bytes to nested/new.txt"));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("nested/new.txt")).unwrap(),
            "中文😀"
        );
    }

    #[test]
    fn ls_sorted_with_dir_suffix() {
        let (dir, tools) = setup();
        std::fs::write(dir.path().join("banana"), "x").unwrap();
        std::fs::write(dir.path().join("Apple"), "x").unwrap();
        std::fs::create_dir(dir.path().join("zulu")).unwrap();
        let (result, is_error) = tools.execute("ls", &json!({ "path": "." }));
        assert!(!is_error);
        assert_eq!(result["content"][0]["text"], json!("Apple\nbanana\nzulu/"));
    }

    #[test]
    fn unknown_tool_errors() {
        let (_dir, tools) = setup();
        let (result, is_error) = tools.execute("ghost", &json!({}));
        assert!(is_error);
        assert_eq!(result["content"][0]["text"], json!("Tool ghost not found"));
    }
}
