//! The `ls` tool, ported from `pi-coding-agent/dist/core/tools/ls.js`.
//!
//! Sorting approximates node's `toLowerCase().localeCompare` (ICU collation)
//! with a lowercase-first byte-order comparison. This matches exactly for ASCII
//! letters and digits — the common case — but punctuation and non-ASCII
//! filenames may order differently, since node delegates to the runtime ICU
//! collator whose rules are not reproduced here.

use serde_json::{Map, Value};

use crate::paths::resolve_to_cwd;
use crate::truncate::{format_size, truncate_head, DEFAULT_MAX_BYTES};

const DEFAULT_LIMIT: usize = 500;

#[derive(Debug, Clone, PartialEq)]
pub struct LsOutput {
    pub text: String,
    pub details: Option<Value>,
}

/// Port of the `ls` tool execute: resolve the directory, list entries sorted
/// case-insensitively, suffix directories with `/`, apply the entry limit and
/// byte-budget truncation, and append actionable notices.
pub fn ls_tool(cwd: &str, path: Option<&str>, limit: Option<usize>) -> Result<LsOutput, String> {
    let dir_path = resolve_to_cwd(path.unwrap_or("."), cwd);
    let effective_limit = limit.unwrap_or(DEFAULT_LIMIT);

    let metadata = std::fs::metadata(&dir_path).map_err(|_| format!("Path not found: {dir_path}"))?;
    if !metadata.is_dir() {
        return Err(format!("Not a directory: {dir_path}"));
    }

    let mut entries: Vec<String> = std::fs::read_dir(&dir_path)
        .map_err(|e| format!("Cannot read directory: {e}"))?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();

    entries.sort_by(|a, b| {
        let al = a.to_lowercase();
        let bl = b.to_lowercase();
        al.cmp(&bl).then_with(|| a.cmp(b))
    });

    let mut results: Vec<String> = Vec::new();
    let mut entry_limit_reached = false;
    for entry in &entries {
        if results.len() >= effective_limit {
            entry_limit_reached = true;
            break;
        }
        let full_path = format!("{}/{}", dir_path.trim_end_matches('/'), entry);
        match std::fs::metadata(&full_path) {
            Ok(stat) => {
                let suffix = if stat.is_dir() { "/" } else { "" };
                results.push(format!("{entry}{suffix}"));
            }
            Err(_) => continue,
        }
    }

    if results.is_empty() {
        return Ok(LsOutput {
            text: "(empty directory)".to_string(),
            details: None,
        });
    }

    let raw_output = results.join("\n");
    let truncation = truncate_head(&raw_output, Some(usize::MAX), None);
    let mut output = truncation.content.clone();
    let mut notices: Vec<String> = Vec::new();
    let mut details = Map::new();

    if entry_limit_reached {
        notices.push(format!(
            "{effective_limit} entries limit reached. Use limit={} for more",
            effective_limit * 2
        ));
        details.insert("entryLimitReached".to_string(), Value::from(effective_limit));
    }
    if truncation.truncated {
        notices.push(format!("{} limit reached", format_size(DEFAULT_MAX_BYTES)));
        details.insert(
            "truncation".to_string(),
            serde_json::to_value(&truncation).unwrap(),
        );
    }
    if !notices.is_empty() {
        output += &format!("\n\n[{}]", notices.join(". "));
    }

    let details_value = if details.is_empty() {
        None
    } else {
        Some(Value::Object(details))
    };
    Ok(LsOutput {
        text: output,
        details: details_value,
    })
}
