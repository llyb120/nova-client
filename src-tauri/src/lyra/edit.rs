use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use unicode_normalization::UnicodeNormalization;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EditInput {
    old_text: String,
    new_text: String,
}

#[derive(Debug, Clone)]
struct Replacement {
    edit_index: usize,
    match_index: usize,
    match_length: usize,
    new_text: String,
}

static FILE_MUTATION_QUEUES: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();

fn normalize_to_lf(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn detect_line_ending(text: &str) -> &'static str {
    match (text.find("\r\n"), text.find('\n')) {
        (Some(crlf), Some(lf)) if crlf <= lf => "\r\n",
        _ => "\n",
    }
}

fn restore_line_endings(text: &str, ending: &str) -> String {
    if ending == "\r\n" {
        text.replace('\n', "\r\n")
    } else {
        text.to_string()
    }
}

/// Match PI's fuzzy normalization: NFKC, trim line endings, then normalize
/// typographic quotes/dashes and Unicode spaces.
fn normalize_for_fuzzy_match(text: &str) -> String {
    let nfkc: String = text.nfkc().collect();
    nfkc.split('\n')
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        .chars()
        .map(|c| match c {
            '‘' | '’' | '‚' | '‛' => '\'',
            '“' | '”' | '„' | '‟' => '"',
            '‐' | '‑' | '‒' | '–' | '—' | '―' | '−' => '-',
            '\u{00a0}' | '\u{2002}'..='\u{200a}' | '\u{202f}' | '\u{205f}' | '\u{3000}' => ' ',
            other => other,
        })
        .collect()
}

fn count_occurrences(content: &str, needle: &str) -> usize {
    let content = normalize_for_fuzzy_match(content);
    let needle = normalize_for_fuzzy_match(needle);
    if needle.is_empty() {
        return 0;
    }
    content.match_indices(&needle).count()
}

fn not_found_error(path: &str, edit_index: usize, total: usize) -> String {
    if total == 1 {
        format!("Could not find the exact text in {path}. The old text must match exactly including all whitespace and newlines.")
    } else {
        format!("Could not find edits[{edit_index}] in {path}. The oldText must match exactly including all whitespace and newlines.")
    }
}

fn duplicate_error(path: &str, edit_index: usize, total: usize, occurrences: usize) -> String {
    if total == 1 {
        format!("Found {occurrences} occurrences of the text in {path}. The text must be unique. Please provide more context to make it unique.")
    } else {
        format!("Found {occurrences} occurrences of edits[{edit_index}] in {path}. Each oldText must be unique. Please provide more context to make it unique.")
    }
}

fn empty_old_text_error(path: &str, edit_index: usize, total: usize) -> String {
    if total == 1 {
        format!("oldText must not be empty in {path}.")
    } else {
        format!("edits[{edit_index}].oldText must not be empty in {path}.")
    }
}

fn no_change_error(path: &str, total: usize) -> String {
    if total == 1 {
        format!("No changes made to {path}. The replacement produced identical content. This might indicate an issue with special characters or the text not existing as expected.")
    } else {
        format!("No changes made to {path}. The replacements produced identical content.")
    }
}

fn apply_replacements(content: &str, replacements: &[Replacement], offset: usize) -> String {
    let mut result = content.to_string();
    for replacement in replacements.iter().rev() {
        let start = replacement.match_index - offset;
        result.replace_range(
            start..start + replacement.match_length,
            &replacement.new_text,
        );
    }
    result
}

fn split_lines_with_endings(content: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    for (index, byte) in content.bytes().enumerate() {
        if byte == b'\n' {
            lines.push(&content[start..=index]);
            start = index + 1;
        }
    }
    if start < content.len() {
        lines.push(&content[start..]);
    }
    lines
}

fn line_spans(content: &str) -> Vec<(usize, usize)> {
    let mut offset = 0;
    split_lines_with_endings(content)
        .into_iter()
        .map(|line| {
            let span = (offset, offset + line.len());
            offset = span.1;
            span
        })
        .collect()
}

fn replacement_line_range(
    spans: &[(usize, usize)],
    replacement: &Replacement,
) -> Result<(usize, usize), String> {
    let start = replacement.match_index;
    let end = start + replacement.match_length;
    let start_line = spans
        .iter()
        .position(|(lo, hi)| start >= *lo && start < *hi)
        .ok_or_else(|| "Replacement range is outside the base content.".to_string())?;
    let mut end_line = start_line;
    while end_line < spans.len() && spans[end_line].1 < end {
        end_line += 1;
    }
    if end_line >= spans.len() {
        return Err("Replacement range is outside the base content.".into());
    }
    Ok((start_line, end_line + 1))
}

/// PI performs fuzzy replacements in normalized space, but copies every
/// untouched line back from the original so fuzzy matching cannot rewrite
/// unrelated trailing whitespace or Unicode characters.
fn apply_replacements_preserving_unchanged_lines(
    original: &str,
    base: &str,
    replacements: &[Replacement],
) -> Result<String, String> {
    let original_lines = split_lines_with_endings(original);
    let spans = line_spans(base);
    if original_lines.len() != spans.len() {
        return Err(
            "Cannot preserve unchanged lines because the base content has a different line count."
                .into(),
        );
    }

    let mut groups: Vec<(usize, usize, Vec<Replacement>)> = Vec::new();
    for replacement in replacements {
        let (start_line, end_line) = replacement_line_range(&spans, replacement)?;
        if let Some(last) = groups.last_mut() {
            if start_line < last.1 {
                last.1 = last.1.max(end_line);
                last.2.push(replacement.clone());
                continue;
            }
        }
        groups.push((start_line, end_line, vec![replacement.clone()]));
    }

    let mut original_line_index = 0;
    let mut result = String::new();
    for (start_line, end_line, group_replacements) in groups {
        result.push_str(&original_lines[original_line_index..start_line].concat());
        let start_offset = spans[start_line].0;
        let end_offset = spans[end_line - 1].1;
        result.push_str(&apply_replacements(
            &base[start_offset..end_offset],
            &group_replacements,
            start_offset,
        ));
        original_line_index = end_line;
    }
    result.push_str(&original_lines[original_line_index..].concat());
    Ok(result)
}

fn apply_edits(content: &str, edits: &[EditInput], path: &str) -> Result<String, String> {
    let edits: Vec<EditInput> = edits
        .iter()
        .map(|edit| EditInput {
            old_text: normalize_to_lf(&edit.old_text),
            new_text: normalize_to_lf(&edit.new_text),
        })
        .collect();
    for (index, edit) in edits.iter().enumerate() {
        if edit.old_text.is_empty() {
            return Err(empty_old_text_error(path, index, edits.len()));
        }
    }

    let used_fuzzy_match = edits.iter().any(|edit| {
        !content.contains(&edit.old_text)
            && normalize_for_fuzzy_match(content)
                .contains(&normalize_for_fuzzy_match(&edit.old_text))
    });
    let replacement_base = if used_fuzzy_match {
        normalize_for_fuzzy_match(content)
    } else {
        content.to_string()
    };

    let mut replacements = Vec::with_capacity(edits.len());
    for (index, edit) in edits.iter().enumerate() {
        let needle = if used_fuzzy_match {
            normalize_for_fuzzy_match(&edit.old_text)
        } else {
            edit.old_text.clone()
        };
        let Some(match_index) = replacement_base.find(&needle) else {
            return Err(not_found_error(path, index, edits.len()));
        };
        let occurrences = count_occurrences(&replacement_base, &needle);
        if occurrences > 1 {
            return Err(duplicate_error(path, index, edits.len(), occurrences));
        }
        replacements.push(Replacement {
            edit_index: index,
            match_index,
            match_length: needle.len(),
            new_text: edit.new_text.clone(),
        });
    }

    replacements.sort_by_key(|replacement| replacement.match_index);
    for pair in replacements.windows(2) {
        if pair[0].match_index + pair[0].match_length > pair[1].match_index {
            return Err(format!(
                "edits[{}] and edits[{}] overlap in {path}. Merge them into one edit or target disjoint regions.",
                pair[0].edit_index, pair[1].edit_index
            ));
        }
    }

    let output = if used_fuzzy_match {
        apply_replacements_preserving_unchanged_lines(content, &replacement_base, &replacements)?
    } else {
        apply_replacements(&replacement_base, &replacements, 0)
    };
    if output == content {
        return Err(no_change_error(path, edits.len()));
    }
    Ok(output)
}

fn resolve_target(root: &Path, input: &str) -> Result<PathBuf, String> {
    if input.trim().is_empty() {
        return Err("file path is empty".into());
    }
    let target = if Path::new(input).is_absolute() {
        PathBuf::from(input)
    } else {
        root.join(input)
    };
    Ok(target.canonicalize().unwrap_or(target))
}

fn mutation_lock(target: &Path) -> Arc<Mutex<()>> {
    let queues = FILE_MUTATION_QUEUES.get_or_init(|| Mutex::new(HashMap::new()));
    let mut queues = queues
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    queues
        .entry(target.to_path_buf())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

fn generate_display_diff(old: &str, new: &str, context_lines: usize) -> (String, usize) {
    use similar::ChangeTag;

    let diff = similar::TextDiff::from_lines(old, new);
    let changes: Vec<_> = diff.iter_all_changes().collect();
    let changed: Vec<usize> = changes
        .iter()
        .enumerate()
        .filter_map(|(index, change)| (change.tag() != ChangeTag::Equal).then_some(index))
        .collect();
    let first_changed_line = changes
        .iter()
        .find(|change| change.tag() != ChangeTag::Equal)
        .and_then(|change| change.new_index())
        .map(|index| index + 1)
        .unwrap_or(1);
    let max_line = old.split('\n').count().max(new.split('\n').count());
    let width = max_line.to_string().len();
    let mut output = Vec::new();
    let mut skipped = false;

    for (index, change) in changes.iter().enumerate() {
        let visible = change.tag() != ChangeTag::Equal
            || changed
                .iter()
                .any(|changed_index| index.abs_diff(*changed_index) <= context_lines);
        if !visible {
            skipped = true;
            continue;
        }
        if skipped {
            output.push(format!(" {} ...", " ".repeat(width)));
            skipped = false;
        }
        let text = change.value().strip_suffix('\n').unwrap_or(change.value());
        match change.tag() {
            ChangeTag::Delete => output.push(format!(
                "-{:>width$} {text}",
                change.old_index().map(|line| line + 1).unwrap_or(0)
            )),
            ChangeTag::Insert => output.push(format!(
                "+{:>width$} {text}",
                change.new_index().map(|line| line + 1).unwrap_or(0)
            )),
            ChangeTag::Equal => output.push(format!(
                " {:>width$} {text}",
                change.old_index().map(|line| line + 1).unwrap_or(0)
            )),
        }
    }
    if skipped {
        output.push(format!(" {} ...", " ".repeat(width)));
    }
    (output.join("\n"), first_changed_line)
}

fn generate_diff(path: &str, old: &str, new: &str) -> (String, String, usize) {
    let diff = similar::TextDiff::from_lines(old, new);
    let patch = diff
        .unified_diff()
        .context_radius(4)
        .header(path, path)
        .to_string();
    let (display, first_changed_line) = generate_display_diff(old, new, 4);
    (display, patch, first_changed_line)
}

/// Lyra edit intentionally mirrors Vega's PI edit contract and behavior:
/// path + edits[{oldText,newText}], all matches against one original snapshot.
pub fn edit(root: &Path, path: &str, edits: Value) -> Result<Value, String> {
    let edits: Vec<EditInput> =
        serde_json::from_value(edits).map_err(|e| format!("invalid edit arguments: {e}"))?;
    if edits.is_empty() {
        return Err(
            "Edit tool input is invalid. edits must contain at least one replacement.".into(),
        );
    }

    let target = resolve_target(root, path)?;
    let lock = mutation_lock(&target);
    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

    let original = fs::read(&target).map_err(|e| format!("Could not edit file: {path}. {e}."))?;
    let raw = std::str::from_utf8(&original).map_err(|e| format!("{path} is not UTF-8: {e}"))?;
    let (bom, body) = raw
        .strip_prefix('\u{feff}')
        .map(|text| ("\u{feff}", text))
        .unwrap_or(("", raw));
    let line_ending = detect_line_ending(body);
    let normalized = normalize_to_lf(body);
    let edited = apply_edits(&normalized, &edits, path)?;
    let final_content = format!("{bom}{}", restore_line_endings(&edited, line_ending));
    fs::write(&target, final_content.as_bytes())
        .map_err(|e| format!("Could not edit file: {path}. {e}."))?;

    let (diff, patch, first_changed_line) = generate_diff(path, &normalized, &edited);
    Ok(json!({
        "message": format!("Successfully replaced {} block(s) in {path}.", edits.len()),
        "details": {
            "diff": diff,
            "patch": patch,
            "firstChangedLine": first_changed_line
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn exact_edit_preserves_bom_and_crlf() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"\xef\xbb\xbfhello\r\nworld\r\n").unwrap();
        let result = edit(
            dir.path(),
            "a.txt",
            json!([{"oldText":"hello","newText":"hola"}]),
        )
        .unwrap();
        assert_eq!(
            fs::read(dir.path().join("a.txt")).unwrap(),
            b"\xef\xbb\xbfhola\r\nworld\r\n"
        );
        assert_eq!(
            result["message"],
            "Successfully replaced 1 block(s) in a.txt."
        );
        assert!(result["details"]["patch"]
            .as_str()
            .unwrap()
            .contains("-hello"));
    }

    #[test]
    fn rejects_ambiguous_text_without_writing() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), "same\nsame\n").unwrap();
        let error = edit(
            dir.path(),
            "a.txt",
            json!([{"oldText":"same","newText":"x"}]),
        )
        .unwrap_err();
        assert!(error.contains("Found 2 occurrences"));
        assert_eq!(
            fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "same\nsame\n"
        );
    }

    #[test]
    fn fuzzy_match_preserves_untouched_lines() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("a.txt"),
            "keep   \nquote(“old”);   \ntail\n",
        )
        .unwrap();
        edit(
            dir.path(),
            "a.txt",
            json!([{"oldText":"quote(\"old\");","newText":"quote(\"new\");"}]),
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "keep   \nquote(\"new\");\ntail\n"
        );
    }

    #[test]
    fn all_edits_match_the_original_and_must_not_overlap() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), "alpha\nbeta\ngamma").unwrap();
        let error = edit(
            dir.path(),
            "a.txt",
            json!([
                {"oldText":"alpha\nbeta","newText":"first"},
                {"oldText":"beta\ngamma","newText":"second"}
            ]),
        )
        .unwrap_err();
        assert!(error.contains("overlap"));
    }
}
