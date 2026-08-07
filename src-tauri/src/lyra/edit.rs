use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

const FUZZY_THRESHOLD: f64 = 0.90;
const AMBIGUITY_MARGIN: f64 = 0.08;
const MAX_FALLBACK_CANDIDATES: usize = 20_000;
/// E 成功回显的上下文行数（改动区上下各 N 行）；真实会话基准（DeepSeek V4 Flash，30 轮）
/// 显示 ±3 与 ±1 差异在噪声内，取 ±3 保留更多上下文。
/// 改动区本身的完整新内容始终在回显内。
const PREVIEW_CONTEXT_LINES: usize = 3;
/// E 回显的最大行数（超过截断中段）。
const PREVIEW_CAP_LINES: usize = 30;
/// A 失败回读中单个候选区域的最大行数（超过截断中段）。
const CANDIDATE_READBACK_CAP: usize = 20;

/// 基准对照/调优旋钮，仅增强路径（Lyra）生效，legacy 入口（Vega/napi）不读；
/// 生产不设置即默认值。LYRA_EDIT_PREVIEW_CTX / LYRA_EDIT_READBACK_CAP。
fn tune_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|v| *v <= 64)
        .unwrap_or(default)
}

fn preview_context_lines() -> usize {
    tune_usize("LYRA_EDIT_PREVIEW_CTX", PREVIEW_CONTEXT_LINES)
}

fn candidate_readback_cap() -> usize {
    tune_usize("LYRA_EDIT_READBACK_CAP", CANDIDATE_READBACK_CAP)
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EditInput {
    /// 文本定位模式：要替换的原文（与 startLine/endLine 二选一）。
    #[serde(default)]
    old_text: Option<String>,
    new_text: String,
    /// 行区间定位模式（H，仅 edit_files_enhanced / Lyra 生效；legacy 入口忽略这些字段）：
    /// 1 起始闭区间，直接替换这些行，无需复述原文。
    #[serde(default)]
    start_line: Option<usize>,
    #[serde(default)]
    end_line: Option<usize>,
    /// 防漂移护栏（F-lite，仅增强模式生效）：startLine/endLine 行应有的当前原文，不匹配则拒绝并回显实际内容（A）。
    #[serde(default)]
    first_line: Option<String>,
    #[serde(default)]
    last_line: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct MatchInfo {
    edit_index: usize,
    mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    line: Option<usize>,
}

/// E：改动后回显——最终内容中该 edit 的起始行号与带行号上下文，免验证性 read。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PreviewInfo {
    edit_index: usize,
    line: usize,
    text: String,
}

#[derive(Debug, Clone)]
struct Located {
    index: usize,
    length: usize,
    line: Option<usize>,
    mode: &'static str,
    new_text: String,
    old_text: String,
    matched_text: String,
    edit_index: usize,
}

fn normalize_unicode(value: &str) -> String {
    value
        .chars()
        .map(|c| match c {
            '‐' | '‑' | '‒' | '–' | '—' | '―' | '−' => '-',
            '‘' | '’' | '‚' | '‛' => '\'',
            '“' | '”' | '„' | '‟' => '"',
            '\u{00a0}' | ' ' | ' ' | ' ' | ' ' | ' ' | ' ' | ' ' | ' ' | ' ' | ' ' | ' ' | '　' => {
                ' '
            }
            other => other,
        })
        .collect()
}

fn line_indent(line: &str) -> &str {
    let end = line
        .char_indices()
        .find(|(_, c)| !matches!(c, ' ' | '\t'))
        .map(|(i, _)| i)
        .unwrap_or(line.len());
    &line[..end]
}

fn find_occurrences(content: &str, needle: &str) -> Vec<usize> {
    if needle.is_empty() {
        return Vec::new();
    }
    content.match_indices(needle).map(|(i, _)| i).collect()
}

fn line_starts(content: &str) -> (Vec<&str>, Vec<usize>) {
    let lines: Vec<_> = content.split('\n').collect();
    let mut starts = Vec::with_capacity(lines.len());
    let mut offset = 0;
    for (i, line) in lines.iter().enumerate() {
        starts.push(offset);
        offset += line.len() + usize::from(i + 1 < lines.len());
    }
    (lines, starts)
}

/// 带行号的区域快照（`  行号|内容`），用于错误回读（A）与成功回显（E），超过 cap 行时截断中段。
fn numbered_region(lines: &[&str], start: usize, len: usize, cap: usize) -> String {
    let end = (start + len).min(lines.len());
    if start >= end {
        return String::new();
    }
    let mut out = Vec::new();
    if end - start <= cap {
        for (i, line) in lines[start..end].iter().enumerate() {
            out.push(format!("  {}|{}", start + i + 1, line));
        }
    } else {
        let head = cap * 2 / 3;
        let tail = cap - head;
        for (i, line) in lines[start..start + head].iter().enumerate() {
            out.push(format!("  {}|{}", start + i + 1, line));
        }
        out.push(format!("  ... ({} more lines)", end - start - head - tail));
        for (i, line) in lines[end - tail..end].iter().enumerate() {
            out.push(format!("  {}|{}", end - tail + i + 1, line));
        }
    }
    out.join("\n")
}

fn token_set(value: &str) -> HashSet<String> {
    let normalized = normalize_unicode(value);
    let mut out = HashSet::new();
    let mut current = String::new();
    for c in normalized.chars() {
        if c.is_alphanumeric() || matches!(c, '_' | '$') {
            current.push(c);
        } else {
            if !current.is_empty() {
                out.insert(std::mem::take(&mut current));
            }
            if !c.is_whitespace() {
                out.insert(c.to_string());
            }
        }
    }
    if !current.is_empty() {
        out.insert(current);
    }
    out
}

fn jaccard(a: &HashSet<String>, b: &HashSet<String>) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let common = a.intersection(b).count() as f64;
    common / (a.len() + b.len() - common as usize) as f64
}

fn line_similarity(left: &str, right: &str) -> f64 {
    let a = normalize_unicode(left).trim().to_string();
    let b = normalize_unicode(right).trim().to_string();
    if a == b {
        return 1.0;
    }
    let aa = a.as_bytes();
    let bb = b.as_bytes();
    let length = aa.len().max(bb.len());
    if length == 0 {
        return 1.0;
    }
    let mut prefix = 0;
    while prefix < aa.len().min(bb.len()) && aa[prefix] == bb[prefix] {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < aa.len().min(bb.len()).saturating_sub(prefix)
        && aa[aa.len() - 1 - suffix] == bb[bb.len() - 1 - suffix]
    {
        suffix += 1;
    }
    0.55 * ((prefix + suffix) as f64 / length as f64)
        + 0.45 * jaccard(&token_set(&a), &token_set(&b))
}

fn span_for_lines(
    lines: &[&str],
    starts: &[usize],
    start: usize,
    pattern_len: usize,
    pattern_ends_empty: bool,
) -> (usize, usize) {
    let last = start + pattern_len - 1;
    let end = if pattern_ends_empty {
        starts[last]
    } else {
        starts[last] + lines[last].len()
    };
    (starts[start], end - starts[start])
}

fn mapped_line(line: &str, mode: &str) -> String {
    match mode {
        "rstrip" => line.trim_end().to_string(),
        "unicode" => normalize_unicode(line).trim_end().to_string(),
        _ => normalize_unicode(line).trim().to_string(),
    }
}

fn candidate_starts(target: &[&str], pattern: &[&str], mode: &str) -> Vec<usize> {
    let mapped_target: Vec<_> = target.iter().map(|l| mapped_line(l, mode)).collect();
    let mapped_pattern: Vec<_> = pattern.iter().map(|l| mapped_line(l, mode)).collect();
    let mut index: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, line) in mapped_target.iter().enumerate() {
        if !line.trim().is_empty() {
            index.entry(line).or_default().push(i);
        }
    }
    let mut anchors: Vec<_> = mapped_pattern
        .iter()
        .enumerate()
        .filter_map(|(offset, line)| {
            let positions = index.get(line.as_str())?;
            (!line.trim().is_empty() && positions.len() <= MAX_FALLBACK_CANDIDATES).then_some((
                offset,
                line.len(),
                positions,
            ))
        })
        .collect();
    anchors.sort_by_key(|(_, len, positions)| (positions.len(), usize::MAX - *len));
    anchors.truncate(4);
    let mut votes = HashMap::<usize, usize>::new();
    for (offset, _, positions) in anchors {
        for &position in positions {
            if position >= offset {
                let start = position - offset;
                if start + pattern.len() <= target.len() {
                    *votes.entry(start).or_default() += 1;
                }
            }
        }
    }
    let mut rows: Vec<_> = votes.into_iter().collect();
    rows.sort_by_key(|(start, votes)| (usize::MAX - *votes, *start));
    rows.truncate(MAX_FALLBACK_CANDIDATES);
    rows.into_iter().map(|(start, _)| start).collect()
}

fn locate_edit(
    content: &str,
    old_text: &str,
    path: &str,
    edit_index: usize,
    enhanced: bool,
) -> Result<Located, String> {
    if old_text.is_empty() {
        return Err(format!("edits[{edit_index}].oldText is empty in {path}."));
    }
    let exact = find_occurrences(content, old_text);
    if exact.len() == 1 {
        return Ok(Located {
            index: exact[0],
            length: old_text.len(),
            line: Some(content[..exact[0]].bytes().filter(|b| *b == b'\n').count() + 1),
            mode: "exact",
            new_text: String::new(),
            old_text: old_text.into(),
            matched_text: old_text.into(),
            edit_index,
        });
    }
    if exact.len() > 1 {
        if !enhanced {
            return Err(format!("Ambiguous exact match for edits[{edit_index}] in {path}: {} occurrences; add more context.", exact.len()));
        }
        // A（增强模式）：歧义错误自带各发生的行号与内容，模型无需再 read 即可纠错。
        let (target_lines, _) = line_starts(content);
        let mut detail = String::new();
        for &pos in exact.iter().take(6) {
            let line_no = content[..pos].bytes().filter(|b| *b == b'\n').count() + 1;
            detail.push_str(&format!(
                "\n  {}|{}",
                line_no,
                target_lines
                    .get(line_no.saturating_sub(1))
                    .copied()
                    .unwrap_or("")
            ));
        }
        return Err(format!("Ambiguous exact match for edits[{edit_index}] in {path}: {} occurrences; add more context. Matching lines:{detail}", exact.len()));
    }

    let (target_lines, starts) = line_starts(content);
    let pattern_lines: Vec<_> = old_text.split('\n').collect();
    for mode in ["rstrip", "unicode"] {
        let candidates = candidate_starts(&target_lines, &pattern_lines, mode);
        let matches: Vec<_> = candidates
            .into_iter()
            .filter(|&start| {
                (0..pattern_lines.len()).all(|off| {
                    mapped_line(target_lines[start + off], mode)
                        == mapped_line(pattern_lines[off], mode)
                })
            })
            .collect();
        if matches.len() == 1 {
            let start = matches[0];
            let (index, length) = span_for_lines(
                &target_lines,
                &starts,
                start,
                pattern_lines.len(),
                pattern_lines.last() == Some(&""),
            );
            let matched = content[index..index + length].to_string();
            return Ok(Located {
                index,
                length,
                line: Some(start + 1),
                mode,
                new_text: String::new(),
                old_text: old_text.into(),
                matched_text: matched,
                edit_index,
            });
        }
        if matches.len() > 1 {
            if !enhanced {
                return Err(format!("Ambiguous {mode} match for edits[{edit_index}] in {path}: {} occurrences; add more context.", matches.len()));
            }
            let lines_list = matches
                .iter()
                .take(6)
                .map(|s| (s + 1).to_string())
                .collect::<Vec<_>>()
                .join(", ");
            let first_lines: String = matches
                .iter()
                .take(6)
                .map(|&s| format!("\n  {}|{}", s + 1, target_lines[s]))
                .collect();
            return Err(format!("Ambiguous {mode} match for edits[{edit_index}] in {path}: {} occurrences at lines {lines_list}; add more context. Matching lines:{first_lines}", matches.len()));
        }
    }

    let relative_candidates = candidate_starts(&target_lines, &pattern_lines, "relative-anchor");
    let relative_pattern = relative_indent_lines(&pattern_lines);
    let relative_matches: Vec<_> = relative_candidates
        .iter()
        .copied()
        .filter(|start| {
            relative_indent_lines(&target_lines[*start..*start + pattern_lines.len()])
                == relative_pattern
        })
        .collect();
    if relative_matches.len() > 1 {
        if !enhanced {
            return Err(format!(
                "Ambiguous relative-indent match for edits[{edit_index}] in {path}; add more context."
            ));
        }
        let lines_list = relative_matches
            .iter()
            .take(6)
            .map(|s| (s + 1).to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let first_lines: String = relative_matches
            .iter()
            .take(6)
            .map(|&s| format!("\n  {}|{}", s + 1, target_lines[s]))
            .collect();
        return Err(format!(
            "Ambiguous relative-indent match for edits[{edit_index}] in {path} at lines {lines_list}; add more context. Matching lines:{first_lines}"
        ));
    }
    if let Some(start) = relative_matches.first().copied() {
        let (index, length) = span_for_lines(
            &target_lines,
            &starts,
            start,
            pattern_lines.len(),
            pattern_lines.last() == Some(&""),
        );
        let matched = content[index..index + length].to_string();
        return Ok(Located {
            index,
            length,
            line: Some(start + 1),
            mode: "relative-indent",
            new_text: String::new(),
            old_text: old_text.into(),
            matched_text: matched,
            edit_index,
        });
    }

    let mut starts_candidates = candidate_starts(&target_lines, &pattern_lines, "rstrip");
    for start in candidate_starts(&target_lines, &pattern_lines, "unicode") {
        if !starts_candidates.contains(&start) {
            starts_candidates.push(start);
        }
    }
    if starts_candidates.is_empty()
        && target_lines.len() >= pattern_lines.len()
        && target_lines.len() <= MAX_FALLBACK_CANDIDATES
    {
        starts_candidates = (0..=target_lines.len() - pattern_lines.len()).collect();
    }
    let mut ranked: Vec<_> = starts_candidates
        .into_iter()
        .map(|start| {
            let mut line_score = 0.0;
            for off in 0..pattern_lines.len() {
                line_score += line_similarity(target_lines[start + off], pattern_lines[off]);
            }
            let boundary = (line_similarity(target_lines[start], pattern_lines[0])
                + line_similarity(
                    target_lines[start + pattern_lines.len() - 1],
                    pattern_lines[pattern_lines.len() - 1],
                ))
                / 2.0;
            (
                start,
                0.9 * line_score / pattern_lines.len() as f64 + 0.1 * boundary,
            )
        })
        .collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    let best = ranked.first().copied();
    let second = ranked.get(1).copied();
    if let (Some((sa, a)), Some((sb, b))) = (best, second) {
        if a >= FUZZY_THRESHOLD - AMBIGUITY_MARGIN && a - b < AMBIGUITY_MARGIN {
            if !enhanced {
                return Err(format!("Ambiguous fuzzy match for edits[{edit_index}] in {path} ({}% vs {}%); add more context.", (a * 100.0).round(), (b * 100.0).round()));
            }
            return Err(format!(
                "Ambiguous fuzzy match for edits[{edit_index}] in {path} ({}% vs {}%); add more context. Candidates:\n  @@ {}-{}\n{}\n  @@ {}-{}\n{}",
                (a * 100.0).round(),
                (b * 100.0).round(),
                sa + 1,
                sa + pattern_lines.len(),
                numbered_region(&target_lines, sa, pattern_lines.len(), 8),
                sb + 1,
                sb + pattern_lines.len(),
                numbered_region(&target_lines, sb, pattern_lines.len(), 8),
            ));
        }
    }
    let Some((start, score)) = best else {
        if !enhanced {
            return Err(format!(
                "Could not find a sufficiently similar match for edits[{edit_index}] in {path}."
            ));
        }
        return Err(format!(
            "Could not find a sufficiently similar match for edits[{edit_index}] in {path} (oldText spans {} lines, file has {} lines). Make sure you are editing the right file; read it or use fast_context to get the actual content.",
            pattern_lines.len(),
            target_lines.len()
        ));
    };
    if score < FUZZY_THRESHOLD {
        if !enhanced {
            return Err(format!("Could not find a sufficiently similar match for edits[{edit_index}] in {path} (best {}%).", (score * 100.0).round()));
        }
        return Err(format!(
            "Could not find a sufficiently similar match for edits[{edit_index}] in {path} (best {}%). Best candidate @@ {}-{}:\n{}\nCopy oldText from the candidate above and retry, or replace the lines directly with startLine/endLine (add firstLine/lastLine as a drift guard).",
            (score * 100.0).round(),
            start + 1,
            start + pattern_lines.len(),
            numbered_region(&target_lines, start, pattern_lines.len(), candidate_readback_cap()),
        ));
    }
    let (index, length) = span_for_lines(
        &target_lines,
        &starts,
        start,
        pattern_lines.len(),
        pattern_lines.last() == Some(&""),
    );
    let matched = content[index..index + length].to_string();
    Ok(Located {
        index,
        length,
        line: Some(start + 1),
        mode: "fuzzy",
        new_text: String::new(),
        old_text: old_text.into(),
        matched_text: matched,
        edit_index,
    })
}

fn relative_indent_lines(lines: &[&str]) -> Vec<String> {
    let mut previous = 0isize;
    lines
        .iter()
        .enumerate()
        .map(|(index, line)| {
            let indent = line_indent(line).len() as isize;
            let delta = if index == 0 { 0 } else { indent - previous };
            previous = indent;
            format!("{delta}:{}", normalize_unicode(line).trim())
        })
        .collect()
}

fn rebase_indent(new_text: &str, old_text: &str, matched_text: &str) -> String {
    let old_first = old_text.lines().find(|l| !l.trim().is_empty());
    let matched_first = matched_text.lines().find(|l| !l.trim().is_empty());
    let (Some(old_first), Some(matched_first)) = (old_first, matched_first) else {
        return new_text.into();
    };
    let old_indent = line_indent(old_first);
    let matched_indent = line_indent(matched_first);
    if old_indent == matched_indent {
        return new_text.into();
    }
    new_text
        .split('\n')
        .map(|line| {
            if line.trim().is_empty() {
                line.to_string()
            } else if let Some(rest) = line.strip_prefix(old_indent) {
                format!("{matched_indent}{rest}")
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// H：行区间定位。firstLine/lastLine 作为防漂移护栏，不匹配时回显实际行内容（A）供模型自纠正。
fn locate_line_range(
    content: &str,
    edit: &EditInput,
    path: &str,
    edit_index: usize,
) -> Result<Located, String> {
    let (Some(start_line), Some(end_line)) = (edit.start_line, edit.end_line) else {
        return Err(format!(
            "edits[{edit_index}] in {path}: provide either oldText or startLine+endLine."
        ));
    };
    let (target_lines, starts) = line_starts(content);
    if start_line == 0 || end_line < start_line || end_line > target_lines.len() {
        return Err(format!(
            "edits[{edit_index}] in {path}: line range {start_line}-{end_line} is out of bounds (file has {} lines).",
            target_lines.len()
        ));
    }
    let guard = |line_no: usize, expected: &Option<String>, tag: &str| -> Result<(), String> {
        let Some(expected) = expected else {
            return Ok(());
        };
        let actual = target_lines[line_no - 1];
        if normalize_unicode(actual).trim_end() == normalize_unicode(expected).trim_end() {
            return Ok(());
        }
        let lo = line_no.saturating_sub(2).max(1);
        let hi = (line_no + 2).min(target_lines.len());
        let context = numbered_region(&target_lines, lo - 1, hi - lo + 1, 8);
        Err(format!(
            "Line-range guard failed for edits[{edit_index}] in {path}: line {line_no} is '{actual}' but {tag} claims '{expected}'. The file likely changed since it was read. Actual context:\n{context}\nFix the coordinates from the actual lines above, or switch to oldText."
        ))
    };
    guard(start_line, &edit.first_line, "firstLine")?;
    guard(end_line, &edit.last_line, "lastLine")?;
    let first = start_line - 1;
    let last = end_line - 1;
    let index = starts[first];
    let length = starts[last] + target_lines[last].len() - index;
    let matched = content[index..index + length].to_string();
    Ok(Located {
        index,
        length,
        line: Some(start_line),
        mode: "lines",
        new_text: String::new(),
        // old_text 置为实际命中内容，rebase_indent 因此成为恒等变换。
        old_text: matched.clone(),
        matched_text: matched,
        edit_index,
    })
}

/// E：按最终内容计算每个 edit 的回显（行号 + 带行号上下文），多 edit 时行号已反映前序位移。
fn build_previews(output: &str, applied: &[(usize, usize, usize)]) -> Vec<PreviewInfo> {
    let (lines, starts) = line_starts(output);
    let mut items: Vec<_> = applied
        .iter()
        .map(|&(edit_index, index, len)| {
            let start_idx = starts.partition_point(|s| *s <= index).saturating_sub(1);
            let end_char = index + len;
            let mut end_idx = starts.partition_point(|s| *s < end_char).saturating_sub(1);
            if end_idx < start_idx {
                end_idx = start_idx;
            }
            let preview_ctx = preview_context_lines();
            let lo = start_idx.saturating_sub(preview_ctx);
            let hi = (end_idx + preview_ctx).min(lines.len().saturating_sub(1));
            PreviewInfo {
                edit_index,
                line: start_idx + 1,
                text: numbered_region(&lines, lo, hi - lo + 1, PREVIEW_CAP_LINES),
            }
        })
        .collect();
    items.sort_by_key(|p| p.edit_index);
    items
}

fn apply_smart_edits(
    content: &str,
    edits: &[EditInput],
    path: &str,
    enhanced: bool,
) -> Result<(String, Vec<MatchInfo>, Vec<PreviewInfo>), String> {
    let mut located = Vec::with_capacity(edits.len());
    for (i, edit) in edits.iter().enumerate() {
        let new_text = edit.new_text.replace("\r\n", "\n");
        let old_text = edit
            .old_text
            .as_deref()
            .map(|s| s.replace("\r\n", "\n"))
            .filter(|s| !s.is_empty());
        let mut found = match old_text {
            Some(old) => {
                // legacy（Vega/napi）：忽略行区间字段，行为与增强前完全一致。
                if enhanced && (edit.start_line.is_some() || edit.end_line.is_some()) {
                    return Err(format!(
                        "edits[{i}] in {path}: pass either oldText or startLine/endLine, not both."
                    ));
                }
                locate_edit(content, &old, path, i, enhanced)?
            }
            // H：行区间定位仅增强模式可用；legacy 下缺 oldText 直接报错。
            None if enhanced => locate_line_range(content, edit, path, i)?,
            None => {
                return Err(format!("edits[{i}].oldText is empty in {path}."));
            }
        };
        found.new_text = new_text;
        located.push(found);
    }
    located.sort_by_key(|m| m.index);
    for pair in located.windows(2) {
        if pair[0].index + pair[0].length > pair[1].index {
            return Err(format!(
                "edits[{}] and edits[{}] overlap in {path}.",
                pair[0].edit_index, pair[1].edit_index
            ));
        }
    }
    let matches = located
        .iter()
        .map(|m| MatchInfo {
            edit_index: m.edit_index,
            mode: m.mode.into(),
            line: m.line,
        })
        .collect();
    let mut output = content.to_string();
    // 正序推算每个替换在最终内容中的位置（非重叠、已按 index 排序），倒序执行替换。
    let replacements: Vec<String> = located
        .iter()
        .map(|m| rebase_indent(&m.new_text, &m.old_text, &m.matched_text))
        .collect();
    let mut applied: Vec<(usize, usize, usize)> = Vec::with_capacity(located.len());
    let mut delta: isize = 0;
    for (k, m) in located.iter().enumerate() {
        applied.push((
            m.edit_index,
            (m.index as isize + delta) as usize,
            replacements[k].len(),
        ));
        delta += replacements[k].len() as isize - m.length as isize;
    }
    for (k, m) in located.iter().enumerate().rev() {
        output.replace_range(m.index..m.index + m.length, &replacements[k]);
    }
    if output == content {
        return Err(format!("No changes made to {path}."));
    }
    // E：成功回显仅增强模式产出，legacy 结果 JSON 不携带 previews。
    let previews = if enhanced {
        build_previews(&output, &applied)
    } else {
        Vec::new()
    };
    Ok((output, matches, previews))
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

#[cfg(windows)]
fn replace_staged(target: &Path, staged: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::ReplaceFileW;
    let target: Vec<u16> = target.as_os_str().encode_wide().chain(Some(0)).collect();
    let staged: Vec<u16> = staged.as_os_str().encode_wide().chain(Some(0)).collect();
    let ok = unsafe {
        ReplaceFileW(
            target.as_ptr(),
            staged.as_ptr(),
            std::ptr::null(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn replace_staged(target: &Path, staged: &Path) -> std::io::Result<()> {
    fs::rename(staged, target)
}

/// 单文件 edit。保留智能 oldText 定位，并在增强模式下提供 H/A/E：
/// 行区间替换、失败候选回读、成功带行号回显。
pub fn edit(root: &Path, path: &str, edits: Value, enhanced: bool) -> Result<Value, String> {
    let edits: Vec<EditInput> =
        serde_json::from_value(edits).map_err(|e| format!("invalid edit arguments: {e}"))?;
    if edits.is_empty() {
        return Err("edits must not be empty".into());
    }
    let target = resolve_target(root, path)?;
    let original = fs::read(&target).map_err(|e| format!("{path}: {e}"))?;
    let raw = std::str::from_utf8(&original).map_err(|e| format!("{path} is not UTF-8: {e}"))?;
    let (bom, body) = raw
        .strip_prefix('\u{feff}')
        .map(|s| ("\u{feff}", s))
        .unwrap_or(("", raw));
    let crlf = body.contains("\r\n");
    let normalized = body.replace("\r\n", "\n");
    let (edited, matches, previews) = apply_smart_edits(&normalized, &edits, path, enhanced)?;
    let restored = if crlf {
        edited.replace('\n', "\r\n")
    } else {
        edited
    };
    let output = format!("{bom}{restored}").into_bytes();

    let transaction = format!(
        "{}.{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    );
    let staged = target.with_extension(format!("nova-tmp-{transaction}"));
    fs::write(&staged, output).map_err(|e| format!("failed to stage {path}: {e}"))?;
    if let Err(error) = replace_staged(&target, &staged) {
        let _ = fs::remove_file(&staged);
        return Err(format!("failed to replace {path}: {error}"));
    }

    let mut result =
        json!({ "message": format!("已编辑 {path}"), "path": path, "matches": matches });
    if enhanced {
        result["previews"] = json!(previews);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn line_range_guard_rejects_drift() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), "inserted\na\nb\nc\n").unwrap();
        let error = edit(dir.path(), "a.txt", json!([{
            "startLine": 2, "endLine": 3, "firstLine": "b", "lastLine": "c", "newText": "x"
        }]), true).unwrap_err();
        assert!(error.contains("likely changed"), "{error}");
        assert_eq!(fs::read_to_string(dir.path().join("a.txt")).unwrap(), "inserted\na\nb\nc\n");
    }

    #[test]
    fn enhanced_edit_preserves_crlf_and_returns_numbered_preview() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"a\r\nb\r\nc\r\n").unwrap();
        let result = edit(dir.path(), "a.txt", json!([{
            "startLine": 2, "endLine": 2, "firstLine": "b", "newText": "B"
        }]), true).unwrap();
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"a\r\nB\r\nc\r\n");
        assert!(result["previews"][0]["text"].as_str().unwrap().contains("2|B"));
    }
}
