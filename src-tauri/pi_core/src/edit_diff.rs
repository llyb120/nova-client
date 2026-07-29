//! Single-file edit algorithm, ported from
//! `pi-coding-agent/dist/core/tools/edit-diff.js`
//! (`applyEditsToNormalizedContent` and helpers).
//!
//! Indices are byte offsets used consistently throughout; because every match
//! and splice operates within one string representation, the resulting content
//! is byte-for-byte identical to the JS implementation (whose UTF-16 indices
//! differ in magnitude but select the same substrings).

use unicode_normalization::UnicodeNormalization;

/// Port of `detectLineEnding`: CRLF if the first CRLF precedes the first lone LF.
pub fn detect_line_ending(content: &str) -> &'static str {
    let crlf_idx = content.find("\r\n");
    let lf_idx = content.find('\n');
    let Some(lf) = lf_idx else {
        return "\n";
    };
    let Some(crlf) = crlf_idx else {
        return "\n";
    };
    if crlf < lf {
        "\r\n"
    } else {
        "\n"
    }
}

/// Port of `normalizeToLF`.
pub fn normalize_to_lf(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Port of `restoreLineEndings`.
pub fn restore_line_endings(text: &str, ending: &str) -> String {
    if ending == "\r\n" {
        text.replace('\n', "\r\n")
    } else {
        text.to_string()
    }
}

/// JS `String.prototype.trimEnd`: Unicode White_Space plus U+FEFF.
fn js_trim_end(line: &str) -> &str {
    let end = line
        .char_indices()
        .rev()
        .find(|(_, c)| !(c.is_whitespace() || *c == '\u{FEFF}'))
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(0);
    &line[..end]
}

fn fold_fuzzy_char(c: char) -> char {
    match c {
        '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => '\'',
        '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' => '"',
        '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}'
        | '\u{2212}' => '-',
        '\u{00A0}' | '\u{2002}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}' => ' ',
        _ => c,
    }
}

/// Port of `normalizeForFuzzyMatch`: NFKC, per-line trailing-whitespace strip,
/// then smart quote / dash / space folding.
pub fn normalize_for_fuzzy_match(text: &str) -> String {
    let nfkc: String = text.nfkc().collect();
    let trimmed = nfkc
        .split('\n')
        .map(js_trim_end)
        .collect::<Vec<_>>()
        .join("\n");
    trimmed.chars().map(fold_fuzzy_char).collect()
}

#[derive(Debug, Clone)]
pub struct FuzzyMatch {
    pub found: bool,
    pub index: usize,
    pub match_length: usize,
    pub used_fuzzy_match: bool,
    pub content_for_replacement: String,
}

/// Port of `fuzzyFindText`: exact byte match first, then NFKC-normalized match.
pub fn fuzzy_find_text(content: &str, old_text: &str) -> FuzzyMatch {
    if let Some(index) = content.find(old_text) {
        return FuzzyMatch {
            found: true,
            index,
            match_length: old_text.len(),
            used_fuzzy_match: false,
            content_for_replacement: content.to_string(),
        };
    }
    let fuzzy_content = normalize_for_fuzzy_match(content);
    let fuzzy_old = normalize_for_fuzzy_match(old_text);
    match fuzzy_content.find(&fuzzy_old) {
        Some(index) => FuzzyMatch {
            found: true,
            index,
            match_length: fuzzy_old.len(),
            used_fuzzy_match: true,
            content_for_replacement: fuzzy_content,
        },
        None => FuzzyMatch {
            found: false,
            index: usize::MAX,
            match_length: 0,
            used_fuzzy_match: false,
            content_for_replacement: content.to_string(),
        },
    }
}

/// Port of `stripBom`.
pub fn strip_bom(content: &str) -> (&str, &str) {
    match content.strip_prefix('\u{FEFF}') {
        Some(rest) => ("\u{FEFF}", rest),
        None => ("", content),
    }
}

fn count_occurrences(content: &str, old_text: &str) -> usize {
    let fuzzy_content = normalize_for_fuzzy_match(content);
    let fuzzy_old = normalize_for_fuzzy_match(old_text);
    fuzzy_content.split(&fuzzy_old).count() - 1
}

#[derive(Debug, Clone)]
struct MatchedEdit {
    edit_index: usize,
    match_index: usize,
    match_length: usize,
    new_text: String,
}

/// Port of `applyReplacements`: splice replacements in reverse so offsets stay
/// stable. `offset` is subtracted from each match index (for group-local apply).
fn apply_replacements(content: &str, replacements: &[MatchedEdit], offset: usize) -> String {
    let mut result = content.to_string();
    for replacement in replacements.iter().rev() {
        let match_index = replacement.match_index - offset;
        let end = match_index + replacement.match_length;
        let mut next = String::with_capacity(result.len());
        next.push_str(&result[..match_index]);
        next.push_str(&replacement.new_text);
        next.push_str(&result[end..]);
        result = next;
    }
    result
}

/// Port of `splitLinesWithEndings`: lines keep their trailing `\n`.
fn split_lines_with_endings(content: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0usize;
    let bytes = content.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'\n' {
            lines.push(&content[start..=i]);
            start = i + 1;
        }
    }
    if start < content.len() {
        lines.push(&content[start..]);
    }
    lines
}

struct LineSpan {
    start: usize,
    end: usize,
}

fn get_line_spans(content: &str) -> Vec<LineSpan> {
    let mut offset = 0usize;
    split_lines_with_endings(content)
        .iter()
        .map(|line| {
            let span = LineSpan {
                start: offset,
                end: offset + line.len(),
            };
            offset = span.end;
            span
        })
        .collect()
}

fn get_replacement_line_range(
    lines: &[LineSpan],
    replacement: &MatchedEdit,
) -> Result<(usize, usize), String> {
    let replacement_start = replacement.match_index;
    let replacement_end = replacement.match_index + replacement.match_length;
    let mut start_line = None;
    for (i, line) in lines.iter().enumerate() {
        if replacement_start >= line.start && replacement_start < line.end {
            start_line = Some(i);
            break;
        }
    }
    let Some(mut start_line) = start_line else {
        return Err("Replacement range is outside the base content.".to_string());
    };
    let _ = &mut start_line;
    let mut end_line = start_line;
    while end_line < lines.len() && lines[end_line].end < replacement_end {
        end_line += 1;
    }
    if end_line >= lines.len() {
        return Err("Replacement range is outside the base content.".to_string());
    }
    Ok((start_line, end_line + 1))
}

/// Port of `applyReplacementsPreservingUnchangedLines`.
fn apply_replacements_preserving_unchanged_lines(
    original_content: &str,
    base_content: &str,
    replacements: &[MatchedEdit],
) -> Result<String, String> {
    let original_lines = split_lines_with_endings(original_content);
    let base_lines = get_line_spans(base_content);
    if original_lines.len() != base_lines.len() {
        return Err(
            "Cannot preserve unchanged lines because the base content has a different line count."
                .to_string(),
        );
    }

    struct Group {
        start_line: usize,
        end_line: usize,
        replacements: Vec<MatchedEdit>,
    }
    let mut groups: Vec<Group> = Vec::new();
    let mut sorted: Vec<MatchedEdit> = replacements.to_vec();
    sorted.sort_by_key(|r| r.match_index);
    for replacement in sorted {
        let (start_line, end_line) = get_replacement_line_range(&base_lines, &replacement)?;
        if let Some(current) = groups.last_mut() {
            if start_line < current.end_line {
                current.end_line = current.end_line.max(end_line);
                current.replacements.push(replacement);
                continue;
            }
        }
        groups.push(Group {
            start_line,
            end_line,
            replacements: vec![replacement],
        });
    }

    let mut original_line_index = 0usize;
    let mut result = String::new();
    for group in &groups {
        result.push_str(&original_lines[original_line_index..group.start_line].join(""));
        let group_start_offset = base_lines[group.start_line].start;
        let group_end_offset = base_lines[group.end_line - 1].end;
        result.push_str(&apply_replacements(
            &base_content[group_start_offset..group_end_offset],
            &group.replacements,
            group_start_offset,
        ));
        original_line_index = group.end_line;
    }
    result.push_str(&original_lines[original_line_index..].join(""));
    Ok(result)
}

#[derive(Debug, Clone, PartialEq)]
pub struct EditResult {
    pub base_content: String,
    pub new_content: String,
}

fn not_found_error(path: &str, edit_index: usize, total_edits: usize) -> String {
    if total_edits == 1 {
        format!("Could not find the exact text in {path}. The old text must match exactly including all whitespace and newlines.")
    } else {
        format!("Could not find edits[{edit_index}] in {path}. The oldText must match exactly including all whitespace and newlines.")
    }
}

fn duplicate_error(path: &str, edit_index: usize, total_edits: usize, occurrences: usize) -> String {
    if total_edits == 1 {
        format!("Found {occurrences} occurrences of the text in {path}. The text must be unique. Please provide more context to make it unique.")
    } else {
        format!("Found {occurrences} occurrences of edits[{edit_index}] in {path}. Each oldText must be unique. Please provide more context to make it unique.")
    }
}

fn empty_old_text_error(path: &str, edit_index: usize, total_edits: usize) -> String {
    if total_edits == 1 {
        format!("oldText must not be empty in {path}.")
    } else {
        format!("edits[{edit_index}].oldText must not be empty in {path}.")
    }
}

fn no_change_error(path: &str, total_edits: usize) -> String {
    if total_edits == 1 {
        format!("No changes made to {path}. The replacement produced identical content. This might indicate an issue with special characters or the text not existing as expected.")
    } else {
        format!("No changes made to {path}. The replacements produced identical content.")
    }
}

/// Port of `applyEditsToNormalizedContent`: apply exact/fuzzy replacements to
/// LF-normalized content, preserving unchanged line bytes when fuzzy matching.
pub fn apply_edits_to_normalized_content(
    normalized_content: &str,
    edits: &[(String, String)],
    path: &str,
) -> Result<EditResult, String> {
    let normalized_edits: Vec<(String, String)> = edits
        .iter()
        .map(|(old, new)| (normalize_to_lf(old), normalize_to_lf(new)))
        .collect();
    for (i, (old, _)) in normalized_edits.iter().enumerate() {
        if old.is_empty() {
            return Err(empty_old_text_error(path, i, normalized_edits.len()));
        }
    }

    let initial_matches: Vec<FuzzyMatch> = normalized_edits
        .iter()
        .map(|(old, _)| fuzzy_find_text(normalized_content, old))
        .collect();
    let used_fuzzy_match = initial_matches.iter().any(|m| m.used_fuzzy_match);
    let replacement_base_content = if used_fuzzy_match {
        normalize_for_fuzzy_match(normalized_content)
    } else {
        normalized_content.to_string()
    };

    let mut matched_edits: Vec<MatchedEdit> = Vec::new();
    for (i, (old, new)) in normalized_edits.iter().enumerate() {
        let match_result = fuzzy_find_text(&replacement_base_content, old);
        if !match_result.found {
            return Err(not_found_error(path, i, normalized_edits.len()));
        }
        let occurrences = count_occurrences(&replacement_base_content, old);
        if occurrences > 1 {
            return Err(duplicate_error(path, i, normalized_edits.len(), occurrences));
        }
        matched_edits.push(MatchedEdit {
            edit_index: i,
            match_index: match_result.index,
            match_length: match_result.match_length,
            new_text: new.clone(),
        });
    }

    matched_edits.sort_by_key(|m| m.match_index);
    for i in 1..matched_edits.len() {
        let previous = &matched_edits[i - 1];
        let current = &matched_edits[i];
        if previous.match_index + previous.match_length > current.match_index {
            return Err(format!(
                "edits[{}] and edits[{}] overlap in {path}. Merge them into one edit or target disjoint regions.",
                previous.edit_index, current.edit_index
            ));
        }
    }

    let base_content = normalized_content.to_string();
    let new_content = if used_fuzzy_match {
        apply_replacements_preserving_unchanged_lines(
            normalized_content,
            &replacement_base_content,
            &matched_edits,
        )?
    } else {
        apply_replacements(&replacement_base_content, &matched_edits, 0)
    };

    if base_content == new_content {
        return Err(no_change_error(path, normalized_edits.len()));
    }
    Ok(EditResult {
        base_content,
        new_content,
    })
}
