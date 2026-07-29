//! 智能编辑算法 —— 忠实移植自 `scripts/alkaid-smart-edit.mjs`。
//!
//! 精确匹配优先；多义拒绝；稀有行锚点倒排；rstrip / Unicode / 相对缩进 / 模糊评分逐级定位。
use serde_json::Value;
use std::collections::HashMap;

const FUZZY_THRESHOLD: f64 = 0.9;
const AMBIGUITY_MARGIN: f64 = 0.08;
const MAX_FALLBACK_CANDIDATES: usize = 20_000;

fn unicode_map() -> HashMap<char, char> {
    let mut m = HashMap::new();
    for c in ['‐', '‑', '‒', '–', '—', '―', '−'] {
        m.insert(c, '-');
    }
    for c in ['‘', '’', '‚', '‛'] {
        m.insert(c, '\'');
    }
    for c in ['“', '”', '„', '‟'] {
        m.insert(c, '"');
    }
    for c in ['\u{00a0}', ' ', ' ', ' ', ' ', ' ', ' ', ' ', ' ', ' ', ' ', ' ', '　'] {
        m.insert(c, ' ');
    }
    m
}

fn normalize_unicode(value: &str) -> String {
    let map = unicode_map();
    value.chars().map(|c| *map.get(&c).unwrap_or(&c)).collect()
}

fn line_indent(line: &str) -> String {
    line.chars().take_while(|c| *c == '\t' || *c == ' ').collect()
}

fn relative_indent_lines(lines: &[String]) -> Vec<String> {
    let mut previous = 0usize;
    let mut out = Vec::with_capacity(lines.len());
    for (i, line) in lines.iter().enumerate() {
        let indent = line_indent(line).chars().count();
        let delta = if i == 0 { 0 } else { indent as i64 - previous as i64 };
        previous = indent;
        out.push(format!("{delta}:{}", normalize_unicode(line).trim()));
    }
    out
}

#[derive(Clone, Copy)]
enum LineMode {
    Rstrip,
    Unicode,
}

impl LineMode {
    fn name(self) -> &'static str {
        match self {
            LineMode::Rstrip => "rstrip",
            LineMode::Unicode => "unicode",
        }
    }
    fn map(self, lines: &[String]) -> Vec<String> {
        match self {
            LineMode::Rstrip => lines.iter().map(|l| trim_end(l)).collect(),
            LineMode::Unicode => lines.iter().map(|l| trim_end(&normalize_unicode(l))).collect(),
        }
    }
}

fn trim_end(s: &str) -> String {
    s.trim_end().to_string()
}

fn find_occurrences(content: &str, needle: &str) -> Vec<usize> {
    let mut positions = Vec::new();
    if needle.is_empty() {
        return positions;
    }
    let mut from = 0;
    let (cb, nb) = (content.as_bytes(), needle.as_bytes());
    while from + nb.len() <= cb.len() {
        match content[from..].find(needle) {
            Some(idx) => {
                let abs = from + idx;
                positions.push(abs);
                from = abs + needle.len().max(1);
            }
            None => break,
        }
    }
    positions
}

struct LineTable {
    lines: Vec<String>,
    starts: Vec<usize>,
}

fn build_line_table(content: &str) -> LineTable {
    let lines: Vec<String> = content.split('\n').map(str::to_string).collect();
    let mut starts = Vec::with_capacity(lines.len());
    let mut offset = 0usize;
    for i in 0..lines.len() {
        starts.push(offset);
        offset += lines[i].len() + if i < lines.len() - 1 { 1 } else { 0 };
    }
    LineTable { lines, starts }
}

fn build_index(lines: &[String]) -> HashMap<String, Vec<usize>> {
    let mut index: HashMap<String, Vec<usize>> = HashMap::new();
    for (pos, line) in lines.iter().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        index.entry(line.clone()).or_default().push(pos);
    }
    index
}

struct CandidateResult {
    starts: Vec<usize>,
}

fn candidate_starts(target_lines: &[String], pattern_lines: &[String], rstrip: bool, unicode: bool) -> CandidateResult {
    let mode = if unicode { LineMode::Unicode } else { LineMode::Rstrip };
    let _ = rstrip;
    let mapped_target = mode.map(target_lines);
    let mapped_pattern = mode.map(pattern_lines);
    let index = build_index(&mapped_target);
    let mut anchors: Vec<(usize, usize, Vec<usize>)> = Vec::new();
    for (offset, line) in mapped_pattern.iter().enumerate() {
        if let Some(positions) = index.get(line) {
            if !line.trim().is_empty() && !positions.is_empty() && positions.len() <= MAX_FALLBACK_CANDIDATES {
                anchors.push((offset, line.chars().count(), positions.clone()));
            }
        }
    }
    anchors.sort_by(|a, b| a.2.len().cmp(&b.2.len()).then_with(|| b.1.cmp(&a.1)));
    let anchors: Vec<(usize, usize, Vec<usize>)> = anchors.into_iter().take(4).collect();
    let mut votes: HashMap<usize, usize> = HashMap::new();
    for (offset, _len, positions) in &anchors {
        for position in positions {
            let start = (*position as i64) - (*offset as i64);
            if start < 0 {
                continue;
            }
            let start = start as usize;
            if start + pattern_lines.len() > target_lines.len() {
                continue;
            }
            *votes.entry(start).or_insert(0) += 1;
        }
    }
    let mut sorted: Vec<(usize, usize)> = votes.into_iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let starts = sorted.into_iter().take(MAX_FALLBACK_CANDIDATES).map(|(s, _)| s).collect();
    CandidateResult { starts }
}

fn same_sequence(lines: &[String], pattern: &[String], start: usize) -> bool {
    for (offset, p) in pattern.iter().enumerate() {
        match lines.get(start + offset) {
            Some(l) if l == p => {}
            _ => return false,
        }
    }
    true
}

struct Span {
    index: usize,
    length: usize,
    line: usize,
}

fn span_for_lines(table: &LineTable, start: usize, pattern: &[String]) -> Span {
    let last = start + pattern.len() - 1;
    let end = if pattern.last().map(|s| s.is_empty()).unwrap_or(false) && last < table.starts.len() {
        table.starts[last]
    } else {
        table.starts[last] + table.lines[last].len()
    };
    Span { index: table.starts[start], length: end - table.starts[start], line: start }
}

fn token_set(value: &str) -> std::collections::HashSet<String> {
    let normalized = normalize_unicode(value);
    let re = regex::Regex::new(r"[\p{L}\p{N}_$]+|[^\s\p{L}\p{N}_$]").unwrap();
    re.find_iter(&normalized).map(|m| m.as_str().to_string()).collect()
}

fn jaccard(left: &std::collections::HashSet<String>, right: &std::collections::HashSet<String>) -> f64 {
    if left.is_empty() && right.is_empty() {
        return 1.0;
    }
    let mut common = 0usize;
    for v in left {
        if right.contains(v) {
            common += 1;
        }
    }
    let denom = left.len() + right.len() - common;
    if denom == 0 {
        1.0
    } else {
        common as f64 / denom as f64
    }
}

fn line_similarity(left: &str, right: &str) -> f64 {
    let a = normalize_unicode(left).trim().to_string();
    let b = normalize_unicode(right).trim().to_string();
    if a == b {
        return 1.0;
    }
    let length = a.chars().count().max(b.chars().count());
    if length == 0 {
        return 1.0;
    }
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let mut prefix = 0usize;
    while prefix < a_chars.len().min(b_chars.len()) && a_chars[prefix] == b_chars[prefix] {
        prefix += 1;
    }
    let mut suffix = 0usize;
    while suffix < a_chars.len().min(b_chars.len()) - prefix
        && a_chars[a_chars.len() - 1 - suffix] == b_chars[b_chars.len() - 1 - suffix]
    {
        suffix += 1;
    }
    let positional = (prefix + suffix) as f64 / length as f64;
    0.55 * positional + 0.45 * jaccard(&token_set(&a), &token_set(&b))
}

fn score_candidate(target_lines: &[String], pattern_lines: &[String], start: usize) -> f64 {
    let mut line_score = 0.0;
    for (offset, p) in pattern_lines.iter().enumerate() {
        line_score += line_similarity(&target_lines[start + offset], p);
    }
    let count = pattern_lines.len();
    let boundary = (line_similarity(&target_lines[start], &pattern_lines[0])
        + line_similarity(&target_lines[start + count - 1], &pattern_lines[count - 1]))
        / 2.0;
    0.9 * (line_score / count as f64) + 0.1 * boundary
}

fn rebase_indent(new_text: &str, old_text: &str, matched_text: &str) -> String {
    let old_first = old_text.split('\n').find(|l| !l.trim().is_empty());
    let matched_first = matched_text.split('\n').find(|l| !l.trim().is_empty());
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
                line.to_string()
            } else if line.starts_with(&old_indent) {
                format!("{}{}", matched_indent, &line[old_indent.len()..])
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn fuzzy_candidates(target_lines: &[String], pattern_lines: &[String]) -> Vec<usize> {
    let mut starts: std::collections::BTreeSet<usize> = std::collections::BTreeSet::new();
    for mode in [LineMode::Rstrip, LineMode::Unicode] {
        let unicode = matches!(mode, LineMode::Unicode);
        for start in candidate_starts(target_lines, pattern_lines, !unicode, unicode).starts {
            starts.insert(start);
        }
    }
    let max_start = target_lines.len().saturating_sub(pattern_lines.len());
    if starts.is_empty() && max_start + 1 <= MAX_FALLBACK_CANDIDATES {
        for start in 0..=max_start {
            starts.insert(start);
        }
    }
    starts.into_iter().take(MAX_FALLBACK_CANDIDATES).collect()
}

struct LocatedEdit {
    index: usize,
    length: usize,
    line: usize,
    mode: String,
    rebaser_old: String,
    rebaser_matched: String,
}

struct PreparedEdit {
    index: usize,
    length: usize,
    line: usize,
    mode: String,
    edit_index: usize,
    old: String,
    matched: String,
}

fn locate_edit(content: &str, old_text: &str, path: &str, edit_index: usize, total: usize) -> Result<LocatedEdit, String> {
    if old_text.is_empty() {
        let label = if total == 1 { "oldText".to_string() } else { format!("edits[{edit_index}].oldText") };
        return Err(format!("{label} must not be empty in {path}."));
    }
    let exact = find_occurrences(content, old_text);
    if exact.len() == 1 {
        let index = exact[0];
        let line = content[..index].split('\n').count() - 1;
        return Ok(LocatedEdit { index, length: old_text.len(), line, mode: "exact".into(), rebaser_old: String::new(), rebaser_matched: String::new() });
    }
    if exact.len() > 1 {
        let label = if total == 1 { "the text".to_string() } else { format!("edits[{edit_index}]") };
        return Err(format!("Found {} occurrences of {label} in {path}. Add context to make oldText unique.", exact.len()));
    }

    let table = build_line_table(content);
    let pattern_lines: Vec<String> = old_text.split('\n').map(str::to_string).collect();
    if pattern_lines.len() > table.lines.len() {
        return Err(format!("Could not find edits[{edit_index}] in {path}."));
    }

    for mode in [LineMode::Rstrip, LineMode::Unicode] {
        let unicode = matches!(mode, LineMode::Unicode);
        let candidates = candidate_starts(&table.lines, &pattern_lines, !unicode, unicode);
        let mapped_target = mode.map(&table.lines);
        let mapped_pattern = mode.map(&pattern_lines);
        let matches: Vec<usize> = candidates.starts.into_iter().filter(|&s| same_sequence(&mapped_target, &mapped_pattern, s)).collect();
        if matches.len() > 1 {
            return Err(format!("Ambiguous {} match for edits[{edit_index}] in {path}; add more context.", mode.name()));
        }
        if matches.len() == 1 {
            let span = span_for_lines(&table, matches[0], &pattern_lines);
            let matched = &content[span.index..span.index + span.length];
            return Ok(LocatedEdit { index: span.index, length: span.length, line: span.line, mode: mode.name().into(), rebaser_old: old_text.to_string(), rebaser_matched: matched.to_string() });
        }
    }

    let relative_candidates = candidate_starts(&table.lines, &pattern_lines, false, true);
    let relative_pattern = relative_indent_lines(&pattern_lines);
    let mut relative_matches = Vec::new();
    for &start in &relative_candidates.starts {
        let slice: Vec<String> = table.lines[start..(start + pattern_lines.len())].to_vec();
        if same_sequence(&relative_indent_lines(&slice), &relative_pattern, 0) {
            relative_matches.push(start);
        }
    }
    if relative_matches.len() > 1 {
        return Err(format!("Ambiguous relative-indent match for edits[{edit_index}] in {path}; add more context."));
    }
    if relative_matches.len() == 1 {
        let span = span_for_lines(&table, relative_matches[0], &pattern_lines);
        let matched = &content[span.index..span.index + span.length];
        return Ok(LocatedEdit { index: span.index, length: span.length, line: span.line, mode: "relative-indent".into(), rebaser_old: old_text.to_string(), rebaser_matched: matched.to_string() });
    }

    let mut ranked: Vec<(usize, f64)> = fuzzy_candidates(&table.lines, &pattern_lines)
        .into_iter()
        .map(|start| (start, score_candidate(&table.lines, &pattern_lines, start)))
        .collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal).then_with(|| a.0.cmp(&b.0)));
    let best = ranked.first().map(|(s, sc)| (*s, *sc));
    let second = ranked.get(1).map(|(s, sc)| (*s, *sc));
    if let (Some((_, best_score)), Some((_, second_score))) = (best, second) {
        if best_score >= FUZZY_THRESHOLD - AMBIGUITY_MARGIN && (best_score - second_score).abs() < AMBIGUITY_MARGIN {
            return Err(format!("Ambiguous fuzzy match for edits[{edit_index}] in {path} ({}% vs {}%); add more context.", (best_score * 100.0).round() as u64, (second_score * 100.0).round() as u64));
        }
    }
    let Some((best_start, best_score)) = best else {
        return Err(format!("Could not find a sufficiently similar match for edits[{edit_index}] in {path}."));
    };
    if best_score < FUZZY_THRESHOLD {
        return Err(format!("Could not find a sufficiently similar match for edits[{edit_index}] in {path} (best {}%).", (best_score * 100.0).round() as u64));
    }
    let span = span_for_lines(&table, best_start, &pattern_lines);
    let matched = &content[span.index..span.index + span.length];
    Ok(LocatedEdit { index: span.index, length: span.length, line: span.line, mode: "fuzzy".into(), rebaser_old: old_text.to_string(), rebaser_matched: matched.to_string() })
}

#[derive(Debug)]
pub struct SmartEditResult {
    pub content: String,
    pub matches: Vec<Value>,
}

pub fn apply_smart_edits(content: &str, edits: &[(String, String)], path: &str) -> Result<SmartEditResult, String> {
    let normalized: Vec<(String, String)> = edits
        .iter()
        .map(|(old, new)| (old.replace("\r\n", "\n"), new.replace("\r\n", "\n")))
        .collect();
    let mut prepared: Vec<PreparedEdit> = Vec::with_capacity(normalized.len());
    for (i, (old, _new)) in normalized.iter().enumerate() {
        let l = locate_edit(content, old, path, i, normalized.len())?;
        prepared.push(PreparedEdit {
            index: l.index,
            length: l.length,
            line: l.line,
            mode: l.mode,
            edit_index: i,
            old: l.rebaser_old,
            matched: l.rebaser_matched,
        });
    }
    prepared.sort_by_key(|e| e.index);
    for w in 1..prepared.len() {
        if prepared[w - 1].index + prepared[w - 1].length > prepared[w].index {
            let prev_edit = prepared[w - 1].edit_index;
            let cur_edit = prepared[w].edit_index;
            return Err(format!("edits[{prev_edit}] and edits[{cur_edit}] overlap in {path}."));
        }
    }
    let mut output = content.to_string();
    for entry in prepared.iter().rev() {
        let new_text = &normalized[entry.edit_index].1;
        let replacement = if entry.matched.is_empty() {
            new_text.clone()
        } else {
            rebase_indent(new_text, &entry.old, &entry.matched)
        };
        let mut out = String::with_capacity(output.len() + replacement.len());
        out.push_str(&output[..entry.index]);
        out.push_str(&replacement);
        out.push_str(&output[entry.index + entry.length..]);
        output = out;
    }
    if output == content {
        return Err(format!("No changes made to {path}."));
    }
    let matches = prepared
        .iter()
        .map(|e| serde_json::json!({ "editIndex": e.edit_index, "mode": e.mode, "line": e.line + 1 }))
        .collect();
    Ok(SmartEditResult { content: output, matches })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_single_match() {
        let content = "fn main() {\n    println!(\"hi\");\n}\n";
        let r = apply_smart_edits(content, &[("println!(\"hi\")".into(), "println!(\"bye\")".into())], "a.rs").unwrap();
        assert_eq!(r.content, "fn main() {\n    println!(\"bye\");\n}\n");
        assert_eq!(r.matches[0]["mode"], "exact");
        assert_eq!(r.matches[0]["line"], 2);
    }

    #[test]
    fn multiple_unique_occurrences_rejected() {
        let content = "a\nb\na\n";
        let err = apply_smart_edits(content, &[("a".into(), "c".into())], "a.rs").unwrap_err();
        assert!(err.contains("2 occurrences"));
    }

    #[test]
    fn empty_oldtext_rejected() {
        let err = apply_smart_edits("x", &[("".into(), "y".into())], "a.rs").unwrap_err();
        assert!(err.contains("must not be empty"));
    }

    #[test]
    fn no_changes_rejected() {
        let content = "hello\n";
        let err = apply_smart_edits(content, &[("hello".into(), "hello".into())], "a.rs").unwrap_err();
        assert!(err.contains("No changes"));
    }

    #[test]
    fn rstrip_match_locates_despite_trailing_ws() {
        // oldText has trailing whitespace the file lacks; exact match fails, rstrip matches.
        let content = "def foo():\n    return 1\n";
        let r = apply_smart_edits(content, &[("    return 1   ".into(), "    return 2".into())], "a.py").unwrap();
        assert_eq!(r.content, "def foo():\n    return 2\n");
        assert_eq!(r.matches[0]["mode"], "rstrip");
    }

    #[test]
    fn two_edits_applied_non_overlapping() {
        let content = "a=1\nb=2\nc=3\n";
        let r = apply_smart_edits(content, &[("a=1".into(), "a=10".into()), ("c=3".into(), "c=30".into())], "a.rs").unwrap();
        assert_eq!(r.content, "a=10\nb=2\nc=30\n");
    }
}
