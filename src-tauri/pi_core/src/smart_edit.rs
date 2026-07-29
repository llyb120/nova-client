//! Smart multi-edit locator/applier, ported from `scripts/alkaid-smart-edit.mjs`
//! (`applySmartEdits`). Locates each `oldText` against one immutable snapshot
//! through a cascade — exact → rstrip → unicode → relative-indent → fuzzy —
//! rejects ambiguity and overlap, then applies replacements bottom-up.
//!
//! All content indexing (`index`, `length`, `line`, spans) is done in UTF-16
//! code units to match JS `String.length`/`indexOf`/`slice`; per-line string
//! transforms (normalize/trim/tokenize) operate on code points.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use crate::text::utf16_len;

const FUZZY_THRESHOLD: f64 = 0.9;
const AMBIGUITY_MARGIN: f64 = 0.08;
const MAX_FALLBACK_CANDIDATES: usize = 20_000;

/// Fold Unicode punctuation/space look-alikes to their ASCII equivalents. The
/// table is extracted verbatim from the node source (see `scripts/pi-golden.mjs`
/// history); iterating `char`s mirrors JS `Array.from` code-point iteration.
fn map_unicode(c: char) -> char {
    match c {
        '\u{2010}'..='\u{2015}' => '-',
        '\u{2212}' => '-',
        '\u{2018}'..='\u{201B}' => '\'',
        '\u{201C}'..='\u{201F}' => '"',
        '\u{00A0}' => ' ',
        '\u{2002}'..='\u{200A}' => ' ',
        '\u{202F}' => ' ',
        '\u{205F}' => ' ',
        '\u{3000}' => ' ',
        _ => c,
    }
}

fn normalize_unicode(value: &str) -> String {
    value.chars().map(map_unicode).collect()
}

fn line_indent(line: &str) -> &str {
    let end: usize = line
        .chars()
        .take_while(|c| *c == '\t' || *c == ' ')
        .map(char::len_utf8)
        .sum();
    &line[..end]
}

fn relative_indent_lines(lines: &[String]) -> Vec<String> {
    let mut previous = 0usize;
    lines
        .iter()
        .enumerate()
        .map(|(index, line)| {
            let indent = line_indent(line).chars().count();
            let delta = if index == 0 {
                0
            } else {
                indent as isize - previous as isize
            };
            previous = indent;
            format!("{}:{}", delta, normalize_unicode(line).trim())
        })
        .collect()
}

#[derive(Clone, Copy)]
enum Mode {
    Rstrip,
    Unicode,
    RelativeAnchor,
}

impl Mode {
    fn name(self) -> &'static str {
        match self {
            Mode::Rstrip => "rstrip",
            Mode::Unicode => "unicode",
            Mode::RelativeAnchor => "relative-anchor",
        }
    }

    fn map(self, line: &str) -> String {
        match self {
            Mode::Rstrip => line.trim_end().to_string(),
            Mode::Unicode => normalize_unicode(line).trim_end().to_string(),
            Mode::RelativeAnchor => normalize_unicode(line).trim().to_string(),
        }
    }
}

fn find_occurrences(content: &[u16], needle: &[u16]) -> Vec<usize> {
    let mut positions = Vec::new();
    if needle.is_empty() || needle.len() > content.len() {
        return positions;
    }
    let mut from = 0usize;
    while from + needle.len() <= content.len() {
        match content[from..].windows(needle.len()).position(|w| w == needle) {
            Some(relative) => {
                let index = from + relative;
                positions.push(index);
                from = index + needle.len().max(1);
            }
            None => break,
        }
    }
    positions
}

struct Table {
    lines: Vec<Vec<u16>>,
    starts: Vec<usize>,
}

fn build_line_table(units: &[u16]) -> Table {
    let mut lines: Vec<Vec<u16>> = Vec::new();
    let mut current: Vec<u16> = Vec::new();
    for &unit in units {
        if unit == 0x0A {
            lines.push(std::mem::take(&mut current));
        } else {
            current.push(unit);
        }
    }
    lines.push(current);
    let mut starts = vec![0usize; lines.len()];
    let mut offset = 0usize;
    for i in 0..lines.len() {
        starts[i] = offset;
        offset += lines[i].len() + if i < lines.len() - 1 { 1 } else { 0 };
    }
    Table { lines, starts }
}

fn build_index(lines: &[String]) -> HashMap<String, Vec<usize>> {
    let mut index: HashMap<String, Vec<usize>> = HashMap::new();
    for (position, line) in lines.iter().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        index.entry(line.clone()).or_default().push(position);
    }
    index
}

struct Candidates {
    mapped_target: Vec<String>,
    mapped_pattern: Vec<String>,
    starts: Vec<usize>,
}

fn candidate_starts(target: &[String], pattern: &[String], mode: Mode) -> Candidates {
    let mapped_target: Vec<String> = target.iter().map(|l| mode.map(l)).collect();
    let mapped_pattern: Vec<String> = pattern.iter().map(|l| mode.map(l)).collect();
    let index = build_index(&mapped_target);
    let mut anchors: Vec<(String, usize, Vec<usize>)> = mapped_pattern
        .iter()
        .enumerate()
        .map(|(offset, line)| (line.clone(), offset, index.get(line).cloned().unwrap_or_default()))
        .filter(|(line, _, positions)| {
            !line.trim().is_empty() && !positions.is_empty() && positions.len() <= MAX_FALLBACK_CANDIDATES
        })
        .collect();
    anchors.sort_by(|a, b| {
        a.2.len()
            .cmp(&b.2.len())
            .then_with(|| utf16_len(&b.0).cmp(&utf16_len(&a.0)))
    });
    anchors.truncate(4);

    let mut votes: HashMap<usize, usize> = HashMap::new();
    for (_, offset, positions) in &anchors {
        for &position in positions {
            let start = position as isize - *offset as isize;
            if start < 0 || start + pattern.len() as isize > target.len() as isize {
                continue;
            }
            *votes.entry(start as usize).or_default() += 1;
        }
    }
    let mut starts: Vec<(usize, usize)> = votes.into_iter().collect();
    starts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    starts.truncate(MAX_FALLBACK_CANDIDATES);
    Candidates {
        mapped_target,
        mapped_pattern,
        starts: starts.into_iter().map(|(start, _)| start).collect(),
    }
}

fn same_sequence(lines: &[String], pattern: &[String], start: usize) -> bool {
    for offset in 0..pattern.len() {
        if lines[start + offset] != pattern[offset] {
            return false;
        }
    }
    true
}

struct Span {
    index: usize,
    length: usize,
    line: usize,
}

fn span_for_lines(table: &Table, start: usize, pattern: &[String]) -> Span {
    let last = start + pattern.len() - 1;
    let end = if pattern.last().map(String::as_str) == Some("") && last < table.starts.len() {
        table.starts[last]
    } else {
        table.starts[last] + table.lines[last].len()
    };
    Span {
        index: table.starts[start],
        length: end - table.starts[start],
        line: start,
    }
}

fn token_set(value: &str) -> HashSet<String> {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"[\p{L}\p{N}_$]+|[^\s\p{L}\p{N}_$]").unwrap());
    let normalized = normalize_unicode(value);
    re.find_iter(&normalized).map(|m| m.as_str().to_string()).collect()
}

fn jaccard(left: &HashSet<String>, right: &HashSet<String>) -> f64 {
    if left.is_empty() && right.is_empty() {
        return 1.0;
    }
    let common = left.iter().filter(|value| right.contains(*value)).count();
    common as f64 / (left.len() + right.len() - common) as f64
}

fn line_similarity(left: &str, right: &str) -> f64 {
    let a = normalize_unicode(left);
    let a = a.trim();
    let b = normalize_unicode(right);
    let b = b.trim();
    if a == b {
        return 1.0;
    }
    let au: Vec<u16> = a.encode_utf16().collect();
    let bu: Vec<u16> = b.encode_utf16().collect();
    let length = au.len().max(bu.len());
    if length == 0 {
        return 1.0;
    }
    let min_len = au.len().min(bu.len());
    let mut prefix = 0usize;
    while prefix < min_len && au[prefix] == bu[prefix] {
        prefix += 1;
    }
    let mut suffix = 0usize;
    while suffix < min_len - prefix && au[au.len() - 1 - suffix] == bu[bu.len() - 1 - suffix] {
        suffix += 1;
    }
    0.55 * ((prefix + suffix) as f64 / length as f64) + 0.45 * jaccard(&token_set(a), &token_set(b))
}

fn score_candidate(target: &[String], pattern: &[String], start: usize) -> f64 {
    let mut line_score = 0.0;
    for offset in 0..pattern.len() {
        line_score += line_similarity(&target[start + offset], &pattern[offset]);
    }
    let count = pattern.len();
    let boundary = (line_similarity(&target[start], &pattern[0])
        + line_similarity(&target[start + count - 1], &pattern[count - 1]))
        / 2.0;
    0.9 * (line_score / count as f64) + 0.1 * boundary
}

fn rebase_indent(new_text: &str, old_text: &str, matched_text: &str) -> String {
    let old_first = old_text.split('\n').find(|line| !line.trim().is_empty());
    let matched_first = matched_text.split('\n').find(|line| !line.trim().is_empty());
    let (Some(old_first), Some(matched_first)) = (old_first, matched_first) else {
        return new_text.to_string();
    };
    let old_indent = line_indent(old_first);
    let matched_indent = line_indent(matched_first);
    if old_indent == matched_indent {
        return new_text.to_string();
    }
    new_text
        .split('\n')
        .map(|line| {
            if line.trim().is_empty() {
                return line.to_string();
            }
            match line.strip_prefix(old_indent) {
                Some(rest) => format!("{}{}", matched_indent, rest),
                None => line.to_string(),
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn fuzzy_candidates(target: &[String], pattern: &[String]) -> Vec<usize> {
    let mut order: Vec<usize> = Vec::new();
    let mut seen: HashSet<usize> = HashSet::new();
    for mode in [Mode::Rstrip, Mode::Unicode] {
        for start in candidate_starts(target, pattern, mode).starts {
            if seen.insert(start) {
                order.push(start);
            }
        }
    }
    let max_start = target.len() as isize - pattern.len() as isize;
    if order.is_empty() && max_start >= 0 && (max_start as usize + 1) <= MAX_FALLBACK_CANDIDATES {
        for start in 0..=max_start as usize {
            if seen.insert(start) {
                order.push(start);
            }
        }
    }
    order.truncate(MAX_FALLBACK_CANDIDATES);
    order
}

struct Located {
    index: usize,
    length: usize,
    mode: &'static str,
    line: Option<usize>,
    /// Matched original text for indentation rebasing; `None` for exact matches
    /// (identity rebaser).
    matched: Option<String>,
    edit_index: usize,
    new_text: String,
}

fn locate_edit(
    units: &[u16],
    old_text: &str,
    path: &str,
    edit_index: usize,
    total_edits: usize,
) -> Result<Located, String> {
    let label = if total_edits == 1 {
        "oldText".to_string()
    } else {
        format!("edits[{edit_index}].oldText")
    };
    if old_text.is_empty() {
        return Err(format!("{label} must not be empty in {path}."));
    }
    let old_units: Vec<u16> = old_text.encode_utf16().collect();
    let exact = find_occurrences(units, &old_units);
    if exact.len() == 1 {
        let index = exact[0];
        let line = units[..index].iter().filter(|&&u| u == 0x0A).count();
        return Ok(Located {
            index,
            length: old_units.len(),
            mode: "exact",
            line: Some(line),
            matched: None,
            edit_index,
            new_text: String::new(),
        });
    }
    if exact.len() > 1 {
        let what = if total_edits == 1 {
            "the text".to_string()
        } else {
            format!("edits[{edit_index}]")
        };
        return Err(format!(
            "Found {} occurrences of {what} in {path}. Add context to make oldText unique.",
            exact.len()
        ));
    }

    let table = build_line_table(units);
    let line_strings: Vec<String> = table
        .lines
        .iter()
        .map(|line| String::from_utf16_lossy(line))
        .collect();
    let pattern_lines: Vec<String> = old_text.split('\n').map(str::to_string).collect();
    if pattern_lines.len() > table.lines.len() {
        return Err(format!("Could not find edits[{edit_index}] in {path}."));
    }

    for mode in [Mode::Rstrip, Mode::Unicode] {
        let candidates = candidate_starts(&line_strings, &pattern_lines, mode);
        let matches: Vec<usize> = candidates
            .starts
            .iter()
            .filter(|&&start| same_sequence(&candidates.mapped_target, &candidates.mapped_pattern, start))
            .cloned()
            .collect();
        if matches.len() > 1 {
            return Err(format!(
                "Ambiguous {} match for edits[{edit_index}] in {path}; add more context.",
                mode.name()
            ));
        }
        if matches.len() == 1 {
            let span = span_for_lines(&table, matches[0], &pattern_lines);
            let matched = String::from_utf16_lossy(&units[span.index..span.index + span.length]);
            return Ok(Located {
                index: span.index,
                length: span.length,
                mode: mode.name(),
                line: Some(span.line),
                matched: Some(matched),
                edit_index,
                new_text: String::new(),
            });
        }
    }

    let relative_candidates = candidate_starts(&line_strings, &pattern_lines, Mode::RelativeAnchor);
    let relative_pattern = relative_indent_lines(&pattern_lines);
    let relative_matches: Vec<usize> = relative_candidates
        .starts
        .iter()
        .filter(|&&start| {
            let slice: Vec<String> = table.lines[start..start + pattern_lines.len()]
                .iter()
                .map(|line| String::from_utf16_lossy(line))
                .collect();
            let mapped = relative_indent_lines(&slice);
            same_sequence(&mapped, &relative_pattern, 0)
        })
        .cloned()
        .collect();
    if relative_matches.len() > 1 {
        return Err(format!(
            "Ambiguous relative-indent match for edits[{edit_index}] in {path}; add more context."
        ));
    }
    if relative_matches.len() == 1 {
        let span = span_for_lines(&table, relative_matches[0], &pattern_lines);
        let matched = String::from_utf16_lossy(&units[span.index..span.index + span.length]);
        return Ok(Located {
            index: span.index,
            length: span.length,
            mode: "relative-indent",
            line: Some(span.line),
            matched: Some(matched),
            edit_index,
            new_text: String::new(),
        });
    }

    let mut ranked: Vec<(usize, f64)> = fuzzy_candidates(&line_strings, &pattern_lines)
        .into_iter()
        .map(|start| (start, score_candidate(&line_strings, &pattern_lines, start)))
        .collect();
    ranked.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    let best = ranked.first().copied();
    let second = ranked.get(1).copied();
    if let (Some((_, best_score)), Some((_, second_score))) = (best, second) {
        if best_score >= FUZZY_THRESHOLD - AMBIGUITY_MARGIN
            && best_score - second_score < AMBIGUITY_MARGIN
        {
            return Err(format!(
                "Ambiguous fuzzy match for edits[{edit_index}] in {path} ({}% vs {}%); add more context.",
                (best_score * 100.0).round() as i64,
                (second_score * 100.0).round() as i64
            ));
        }
    }
    match best {
        Some((start, score)) if score >= FUZZY_THRESHOLD => {
            let span = span_for_lines(&table, start, &pattern_lines);
            let matched = String::from_utf16_lossy(&units[span.index..span.index + span.length]);
            Ok(Located {
                index: span.index,
                length: span.length,
                mode: "fuzzy",
                line: Some(span.line),
                matched: Some(matched),
                edit_index,
                new_text: String::new(),
            })
        }
        best => {
            let detail = match best {
                Some((_, score)) => format!(" (best {}%)", (score * 100.0).round() as i64),
                None => String::new(),
            };
            Err(format!(
                "Could not find a sufficiently similar match for edits[{edit_index}] in {path}{detail}."
            ))
        }
    }
}

/// One applied edit's location metadata, matching the node `matches[]` shape.
#[derive(Debug, Clone, PartialEq)]
pub struct SmartMatch {
    pub edit_index: usize,
    pub mode: String,
    /// 1-based line number, if known.
    pub line: Option<usize>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SmartResult {
    pub content: String,
    pub matches: Vec<SmartMatch>,
}

/// Port of `applySmartEdits(content, edits, path)`. `edits` is a list of
/// `(oldText, newText)` pairs. Returns the rewritten content and per-edit match
/// metadata, or the first localization/overlap/no-change error message.
pub fn apply_smart_edits(
    content: &str,
    edits: &[(String, String)],
    path: &str,
) -> Result<SmartResult, String> {
    let units: Vec<u16> = content.encode_utf16().collect();
    let normalized: Vec<(String, String)> = edits
        .iter()
        .map(|(old, new)| (old.replace("\r\n", "\n"), new.replace("\r\n", "\n")))
        .collect();
    let total = normalized.len();

    let mut located: Vec<Located> = Vec::new();
    for (index, (old, new)) in normalized.iter().enumerate() {
        let mut match_ = locate_edit(&units, old, path, index, total)?;
        match_.new_text = new.clone();
        located.push(match_);
    }
    located.sort_by(|a, b| a.index.cmp(&b.index));
    for i in 1..located.len() {
        let previous = &located[i - 1];
        let current = &located[i];
        if previous.index + previous.length > current.index {
            return Err(format!(
                "edits[{}] and edits[{}] overlap in {path}.",
                previous.edit_index, current.edit_index
            ));
        }
    }

    let mut output: Vec<u16> = units.clone();
    for match_ in located.iter().rev() {
        let replacement_text = match &match_.matched {
            None => match_.new_text.clone(),
            Some(matched) => rebase_indent(&match_.new_text, &normalized[match_.edit_index].0, matched),
        };
        let replacement: Vec<u16> = replacement_text.encode_utf16().collect();
        let mut next: Vec<u16> = Vec::with_capacity(output.len());
        next.extend_from_slice(&output[..match_.index]);
        next.extend_from_slice(&replacement);
        next.extend_from_slice(&output[match_.index + match_.length..]);
        output = next;
    }

    if output == units {
        return Err(format!("No changes made to {path}."));
    }

    Ok(SmartResult {
        content: String::from_utf16_lossy(&output),
        matches: located
            .iter()
            .map(|match_| SmartMatch {
                edit_index: match_.edit_index,
                mode: match_.mode.to_string(),
                line: match_.line.map(|line| line + 1),
            })
            .collect(),
    })
}
