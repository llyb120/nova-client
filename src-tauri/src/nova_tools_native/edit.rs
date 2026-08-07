use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

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

#[derive(Debug, Deserialize)]
struct FileInput {
    path: String,
    edits: Vec<EditInput>,
}

#[derive(Debug, Deserialize)]
struct EditParams {
    files: Vec<FileInput>,
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

struct Prepared {
    display_path: String,
    target: PathBuf,
    original: Vec<u8>,
    output: Vec<u8>,
    matches: Vec<MatchInfo>,
    previews: Vec<PreviewInfo>,
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
                target_lines.get(line_no.saturating_sub(1)).copied().unwrap_or("")
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

fn prepare_file(target: PathBuf, file: FileInput, enhanced: bool) -> Result<Prepared, String> {
    let original = fs::read(&target).map_err(|e| format!("{}: {e}", file.path))?;
    let raw =
        std::str::from_utf8(&original).map_err(|e| format!("{} is not UTF-8: {e}", file.path))?;
    let (bom, body) = raw
        .strip_prefix('\u{feff}')
        .map(|s| ("\u{feff}", s))
        .unwrap_or(("", raw));
    let crlf = body.contains("\r\n");
    let normalized = body.replace("\r\n", "\n");
    let (edited, matches, previews) = apply_smart_edits(&normalized, &file.edits, &file.path, enhanced)?;
    let restored = if crlf {
        edited.replace('\n', "\r\n")
    } else {
        edited
    };
    Ok(Prepared {
        display_path: file.path,
        target,
        original,
        output: format!("{bom}{restored}").into_bytes(),
        matches,
        previews,
    })
}

/// legacy 入口（napi：Vega 及各 bridge 的 edit_files）。行为与增强前完全一致：
/// 仅 oldText 定位、紧凑错误文案、结果不含 previews。
#[allow(dead_code)] // 本 crate 内无调用方（Lyra 用增强入口）；napi crate 以 #[path] 共享本文件并调用此入口
pub fn edit_files(root: &Path, params: Value) -> Result<Value, String> {
    edit_files_with_mode(root, params, false)
}

/// Lyra 增强入口：H 行区间定位、A 失败回读（错误自带候选正文）、E 成功回显（previews）。
#[allow(dead_code)] // napi crate 以 #[path] 共享本文件但不调用此入口
pub fn edit_files_enhanced(root: &Path, params: Value) -> Result<Value, String> {
    edit_files_with_mode(root, params, true)
}

fn edit_files_with_mode(root: &Path, params: Value, enhanced: bool) -> Result<Value, String> {
    let params: EditParams =
        serde_json::from_value(params).map_err(|e| format!("invalid edit_files arguments: {e}"))?;
    if params.files.is_empty() {
        return Err("files must not be empty".into());
    }
    let mut grouped: Vec<(PathBuf, FileInput)> = Vec::new();
    let mut positions: HashMap<PathBuf, usize> = HashMap::new();
    for file in params.files {
        let target = resolve_target(root, &file.path)?;
        if let Some(index) = positions.get(&target).copied() {
            grouped[index].1.edits.extend(file.edits);
        } else {
            positions.insert(target.clone(), grouped.len());
            grouped.push((target, file));
        }
    }
    let count = grouped.len();
    let workers = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(2)
        .clamp(1, 8)
        .min(count);
    let requests = Mutex::new(grouped.into_iter().map(Some).collect::<Vec<_>>());
    let results = Mutex::new(
        (0..count)
            .map(|_| None)
            .collect::<Vec<Option<Result<Prepared, String>>>>(),
    );
    let next = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| loop {
                let index = next.fetch_add(1, Ordering::Relaxed);
                if index >= count {
                    break;
                }
                let (target, file) = requests.lock().unwrap()[index].take().unwrap();
                results.lock().unwrap()[index] = Some(prepare_file(target, file, enhanced));
            });
        }
    });
    let prepared = results
        .into_inner()
        .unwrap()
        .into_iter()
        .map(Option::unwrap)
        .collect::<Result<Vec<_>, String>>()?;

    // 全部验证完成后才写临时文件；rename 失败时按备份回滚。
    let transaction = format!(
        "{}.{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    );
    let mut staged = Vec::new();
    for (i, file) in prepared.iter().enumerate() {
        let temp = file
            .target
            .with_extension(format!("nova-tmp-{transaction}-{i}"));
        fs::write(&temp, &file.output).map_err(|e| {
            for path in &staged {
                let _ = fs::remove_file(path);
            }
            format!("failed to stage {}: {e}", file.display_path)
        })?;
        staged.push(temp);
    }
    let mut committed: Vec<usize> = Vec::new();
    for (i, file) in prepared.iter().enumerate() {
        if let Err(final_error) = replace_staged(&file.target, &staged[i]) {
            for &done in &committed {
                let _ = fs::write(&prepared[done].target, &prepared[done].original);
            }
            for path in staged.iter().skip(i) {
                let _ = fs::remove_file(path);
            }
            return Err(format!(
                "failed to replace {}: {final_error}",
                file.display_path
            ));
        }
        committed.push(i);
    }
    let mut result = json!({
        "message": format!("已并行智能编辑 {} 个文件", prepared.len()),
        "paths": prepared.iter().map(|f| &f.display_path).collect::<Vec<_>>(),
        "matches": prepared.iter().map(|f| json!({"path": f.display_path, "edits": f.matches})).collect::<Vec<_>>(),
    });
    if enhanced {
        result["previews"] = json!(prepared.iter().map(|f| json!({"path": f.display_path, "edits": f.previews})).collect::<Vec<_>>());
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn old_new(old: &str, new: &str) -> EditInput {
        EditInput {
            old_text: Some(old.into()),
            new_text: new.into(),
            start_line: None,
            end_line: None,
            first_line: None,
            last_line: None,
        }
    }

    fn lines_edit(
        start: usize,
        end: usize,
        new: &str,
        first: Option<&str>,
        last: Option<&str>,
    ) -> EditInput {
        EditInput {
            old_text: None,
            new_text: new.into(),
            start_line: Some(start),
            end_line: Some(end),
            first_line: first.map(Into::into),
            last_line: last.map(Into::into),
        }
    }

    /// 真实用例灯具：一个 Rust 模块。关键行号：
    /// 16 get / 17 self.entries.get(key) / 20 insert / 21 if capacity / 22 clear / 23 }
    /// 24 insert(key, value) / 28 total_price / 29 subtotal / 30 tax / 31 subtotal + tax / 32 }
    const CACHE_RS: &str = "use std::collections::HashMap;\n\npub struct Cache {\n    entries: HashMap<String, String>,\n    capacity: usize,\n}\n\nimpl Cache {\n    pub fn new(capacity: usize) -> Self {\n        Self {\n            entries: HashMap::new(),\n            capacity,\n        }\n    }\n\n    pub fn get(&self, key: &str) -> Option<&String> {\n        self.entries.get(key)\n    }\n\n    pub fn insert(&mut self, key: String, value: String) {\n        if self.entries.len() >= self.capacity {\n            self.entries.clear();\n        }\n        self.entries.insert(key, value);\n    }\n}\n\npub fn total_price(items: &[u32], tax_percent: u32) -> u32 {\n    let subtotal: u32 = items.iter().sum();\n    let tax = subtotal * tax_percent / 100;\n    subtotal + tax\n}\n";

    #[test]
    fn exact_and_crlf_edit_round_trip() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"hello\r\nworld\r\n").unwrap();
        let result = edit_files(
            dir.path(),
            json!({"files":[{"path":"a.txt","edits":[{"oldText":"hello","newText":"hola"}]}]}),
        )
        .unwrap();
        assert_eq!(
            fs::read(dir.path().join("a.txt")).unwrap(),
            b"hola\r\nworld\r\n"
        );
        assert_eq!(result["matches"][0]["edits"][0]["mode"], "exact");
        // legacy（Vega/napi）：结果不携带 previews
        assert!(result.get("previews").is_none(), "{result}");
    }

    #[test]
    fn rejects_ambiguous_and_does_not_write() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), "same\nsame\n").unwrap();
        let err = edit_files(
            dir.path(),
            json!({"files":[{"path":"a.txt","edits":[{"oldText":"same","newText":"x"}]}]}),
        )
        .unwrap_err();
        // legacy：紧凑歧义错误，不携带发生位置明细
        assert_eq!(
            err,
            "Ambiguous exact match for edits[0] in a.txt: 2 occurrences; add more context."
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "same\nsame\n"
        );
    }

    #[test]
    fn legacy_fuzzy_failure_message_is_compact() {
        // legacy：模糊失败只报分数，不回读候选正文
        let edits = vec![old_new(
            "pub fn compute_total(items: &[u32], tax_percent: u32) -> u32 {\n    let mut acc = 0u32;\n    for x in items { acc += x; }\n    let tax = acc * tax_percent / 100;\n    acc + tax\n}",
            "pub fn compute_total(items: &[u32], tax_percent: u32) -> u64 {\n    0\n}",
        )];
        let err = apply_smart_edits(CACHE_RS, &edits, "cache.rs", false).unwrap_err();
        assert!(err.starts_with("Could not find a sufficiently similar match for edits[0] in cache.rs (best "), "{err}");
        assert!(!err.contains("Best candidate"), "{err}");
    }

    #[test]
    fn legacy_rejects_line_range_without_old_text() {
        // legacy：行区间字段不生效，缺 oldText 直接报错（Vega 不会发送此类请求）
        let edits = vec![lines_edit(21, 23, "x", None, None)];
        let err = apply_smart_edits(CACHE_RS, &edits, "cache.rs", false).unwrap_err();
        assert_eq!(err, "edits[0].oldText is empty in cache.rs.");
    }

    #[test]
    fn legacy_ignores_line_range_fields_when_old_text_present() {
        // legacy：oldText 与行区间同时给出时忽略后者（serde 未知字段语义），不报错
        let mut edit = old_new("    subtotal + tax", "    subtotal.saturating_add(tax)");
        edit.start_line = Some(1);
        edit.end_line = Some(1);
        let (output, matches, previews) =
            apply_smart_edits(CACHE_RS, &[edit], "cache.rs", false).unwrap();
        assert_eq!(matches[0].mode, "exact");
        assert!(output.contains("subtotal.saturating_add(tax)"));
        assert!(previews.is_empty());
    }

    #[test]
    fn matches_relative_indent_and_rebases_replacement() {
        let source = "function outer() {\n    if (ready) {   \n        log(\u{201c}old\u{201d});\n    }\n}";
        let edits = vec![old_new(
            "if (ready) {\n    log(\"old\");\n}",
            "if (ready) {\n    log(\"new\");\n}",
        )];
        let (output, matches, _) = apply_smart_edits(source, &edits, "sample.js", false).unwrap();
        assert_eq!(matches[0].mode, "relative-indent");
        assert!(output.contains("    if (ready) {\n        log(\"new\");\n    }"));
    }

    #[test]
    fn rejects_fuzzy_ambiguity_and_lists_both_candidates() {
        let source = "function first() {\n  calculate(invoiceSubtotal, regionalTax, shippingFee, discountCode, currencyA);\n}\nfunction second() {\n  calculate(invoiceSubtotal, regionalTax, shippingFee, discountCode, currencyB);\n}";
        let edits = vec![old_new(
            "calculate(invoiceSubtotal, regionalTax, shippingFee, discountCode, currencyC);",
            "return total;",
        )];
        let err = apply_smart_edits(source, &edits, "ambiguous.js", true).unwrap_err();
        // A：歧义错误直接列出两个候选的实际内容，模型无需再 read 即可纠错
        assert!(err.contains("Ambiguous fuzzy match"), "{err}");
        assert!(err.contains("currencyA"), "{err}");
        assert!(err.contains("currencyB"), "{err}");
        assert!(err.contains("@@"), "{err}");
    }

    // ---- H：行区间编辑（免 oldText 复述）----

    #[test]
    fn h_line_range_replaces_without_old_text() {
        // 模型刚 read 过 20-25 行，直接把 21-23 换成淘汰逻辑，全程不复述原文
        let edits = vec![lines_edit(
            21,
            23,
            "        if self.entries.len() >= self.capacity {\n            if let Some((oldest, _)) = self.entries.iter().next() {\n                let oldest = oldest.clone();\n                self.entries.remove(&oldest);\n            }\n        }",
            Some("        if self.entries.len() >= self.capacity {"),
            Some("        }"),
        )];
        let (output, matches, previews) = apply_smart_edits(CACHE_RS, &edits, "cache.rs", true).unwrap();
        assert_eq!(matches[0].mode, "lines");
        assert_eq!(matches[0].line, Some(21));
        assert!(output.contains("self.entries.remove(&oldest);"));
        assert!(output.contains("        self.entries.insert(key, value);"));
        // E：回显含新代码与正确行号，免验证性 read
        assert_eq!(previews[0].line, 21);
        assert!(
            previews[0]
                .text
                .contains("21|        if self.entries.len() >= self.capacity {"),
            "{}",
            previews[0].text
        );
        assert!(previews[0].text.contains("self.entries.remove(&oldest);"));
    }

    #[test]
    fn h_line_range_guard_reports_actual_content_on_drift() {
        // read 之后文件被外部改动（顶部插入一行），护栏应拒绝并回显实际行内容
        let dir = tempdir().unwrap();
        let drifted = format!("// externally inserted line\n{CACHE_RS}");
        fs::write(dir.path().join("cache.rs"), &drifted).unwrap();
        let err = edit_files_enhanced(
            dir.path(),
            json!({"files":[{"path":"cache.rs","edits":[{
                "startLine": 21, "endLine": 23,
                "firstLine": "        if self.entries.len() >= self.capacity {",
                "lastLine": "        }",
                "newText": "        todo!()"
            }]}]}),
        )
        .unwrap_err();
        assert!(err.contains("line 21"), "{err}");
        // 实际第 21 行现在是原第 20 行（insert 函数头）
        assert!(err.contains("pub fn insert"), "{err}");
        // 整体拒绝，文件未被改动
        assert_eq!(
            fs::read_to_string(dir.path().join("cache.rs")).unwrap(),
            drifted
        );
    }

    #[test]
    fn h_line_range_out_of_bounds_is_rejected() {
        let edits = vec![lines_edit(30, 99, "x", None, None)];
        let err = apply_smart_edits(CACHE_RS, &edits, "cache.rs", true).unwrap_err();
        assert!(err.contains("out of bounds"), "{err}");
        assert!(err.contains("33 lines"), "{err}");
    }

    #[test]
    fn h_mixed_oldtext_and_line_range_in_one_call() {
        let edits = vec![
            old_new(
                "        self.entries.get(key)",
                "        self.entries.get(key)\n            .filter(|v| !v.is_empty())",
            ),
            lines_edit(
                29,
                29,
                "    let subtotal: u32 = items.iter().sum();\n    debug_assert!(subtotal < u32::MAX / 2);",
                Some("    let subtotal: u32 = items.iter().sum();"),
                None,
            ),
        ];
        let (output, matches, previews) = apply_smart_edits(CACHE_RS, &edits, "cache.rs", true).unwrap();
        assert_eq!(matches.len(), 2);
        assert!(output.contains(".filter(|v| !v.is_empty())"));
        assert!(output.contains("debug_assert!(subtotal < u32::MAX / 2);"));
        // E：第二个 edit 的行号已反映第一个 edit 在第 17 行多插入 1 行的位移
        assert_eq!(previews[0].edit_index, 0);
        assert_eq!(previews[0].line, 17);
        assert_eq!(previews[1].edit_index, 1);
        assert_eq!(previews[1].line, 30, "{}", previews[1].text);
        assert!(
            previews[1]
                .text
                .contains("30|    let subtotal: u32 = items.iter().sum();"),
            "{}",
            previews[1].text
        );
    }

    #[test]
    fn h_line_range_overlapping_oldtext_is_rejected() {
        let edits = vec![
            old_new(
                "        if self.entries.len() >= self.capacity {\n            self.entries.clear();\n        }",
                "        todo!()",
            ),
            lines_edit(20, 24, "    // replaced", None, None),
        ];
        let err = apply_smart_edits(CACHE_RS, &edits, "cache.rs", true).unwrap_err();
        assert!(err.contains("overlap"), "{err}");
    }

    #[test]
    fn h_line_range_preserves_crlf() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("crlf.txt"), b"a\r\nb\r\nc\r\n").unwrap();
        edit_files_enhanced(
            dir.path(),
            json!({"files":[{"path":"crlf.txt","edits":[{"startLine":2,"endLine":2,"firstLine":"b","newText":"B"}]}]}),
        )
        .unwrap();
        assert_eq!(
            fs::read(dir.path().join("crlf.txt")).unwrap(),
            b"a\r\nB\r\nc\r\n"
        );
    }

    // ---- A：失败即回读（错误自带实际内容，可直接驱动纠错重试）----

    #[test]
    fn a_failed_match_returns_actual_candidate_for_self_correction() {
        // 模型记错了函数名与实现细节，oldText 与真实代码差距超过模糊阈值
        let edits = vec![old_new(
            "pub fn compute_total(items: &[u32], tax_percent: u32) -> u32 {\n    let mut acc = 0u32;\n    for x in items { acc += x; }\n    let tax = acc * tax_percent / 100;\n    acc + tax\n}",
            "pub fn compute_total(items: &[u32], tax_percent: u32) -> u64 {\n    0\n}",
        )];
        let err = apply_smart_edits(CACHE_RS, &edits, "cache.rs", true).unwrap_err();
        // 错误信息自带最佳候选的真实正文，下一轮可直接修正 oldText 或改用行区间
        assert!(err.contains("Best candidate @@"), "{err}");
        assert!(err.contains("let subtotal: u32 = items.iter().sum();"), "{err}");
        assert!(err.contains("startLine/endLine"), "{err}");
    }

    #[test]
    fn a_ambiguous_exact_lists_occurrence_lines() {
        let source = "#[test]\nfn a() {}\n#[test]\nfn b() {}\n";
        let edits = vec![old_new("#[test]", "#[ignore]")];
        let err = apply_smart_edits(source, &edits, "tests.rs", true).unwrap_err();
        assert!(err.contains("1|#[test]"), "{err}");
        assert!(err.contains("3|#[test]"), "{err}");
    }

    // ---- E：成功回显 ----

    #[test]
    fn e_edit_result_carries_preview_in_json() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("cache.rs"), CACHE_RS).unwrap();
        let result = edit_files_enhanced(
            dir.path(),
            json!({"files":[{"path":"cache.rs","edits":[{"oldText":"    subtotal + tax","newText":"    subtotal.saturating_add(tax)"}]}]}),
        )
        .unwrap();
        let preview = &result["previews"][0]["edits"][0];
        assert_eq!(preview["line"], 31);
        let text = preview["text"].as_str().unwrap();
        assert!(text.contains("31|    subtotal.saturating_add(tax)"), "{text}");
        // 上下文行也在回显中
        assert!(
            text.contains("30|    let tax = subtotal * tax_percent / 100;"),
            "{text}"
        );
    }

    // ---- H/E/A 效率实测报告 ----

    /// 一次工具往返的计量：model_out = 模型需生成的字符数，tool_in = 工具返回给模型的字符数。
    #[derive(Default)]
    struct Meter {
        model_out: usize,
        tool_in: usize,
        calls: usize,
    }

    impl Meter {
        fn tokens(&self) -> usize {
            (self.model_out + self.tool_in) / 4
        }
    }

    /// 模拟 Lyra read 工具输出（带行号前缀）。
    fn read_chunk(content: &str, offset: usize, limit: usize) -> String {
        content
            .lines()
            .enumerate()
            .skip(offset - 1)
            .take(limit)
            .map(|(i, l)| format!("{}|{}", i + 1, l))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// 真实任务：改写位于中大型文件中部的 13 行函数 total_price（支持 discount 参数）。
    /// 文件够大、目标位于中部，E 的上下文行数才会真实生效——小文件/文件末尾会被边界钳制，测不出差异。
    /// （13 行候选低于默认 A 回读 cap 20，不触发截断；截断路径由专门测试覆盖。）
    /// 对比两条工作流的成本（token 估算 = 字符数 / 4）：
    ///   legacy   = PI/Vega 语义：复述 oldText + 验证性 read；记错时 报错→read→重试
    ///   enhanced = Lyra：H 行区间定位 + E 成功回显（±3 行）；记错时 A 错误自带候选正文（截断中段）直接重试
    /// 查看报告：cargo test --lib hea_efficiency -- --nocapture
    #[test]
    fn hea_efficiency_report() {
        // 13 行目标函数
        const OLD: &str = "pub fn total_price(items: &[u32], tax_percent: u32) -> u32 {\n    let subtotal: u32 = items.iter().sum();\n    let shipping = if subtotal > 10_000 { 0 } else { 599 };\n    let taxed = subtotal * tax_percent / 100;\n    let mut total = subtotal + taxed + shipping;\n    if items.len() > 3 {\n        total = total * 95 / 100;\n    }\n    if total > 50_000 {\n        total -= 1_000;\n    }\n    total\n}";
        const NEW: &str = "pub fn total_price(items: &[u32], tax_percent: u32, discount: u32) -> u32 {\n    let subtotal: u32 = items.iter().sum();\n    let shipping = if subtotal > 10_000 { 0 } else { 599 };\n    let taxed = subtotal * tax_percent / 100;\n    let mut total = subtotal + taxed + shipping;\n    if items.len() > 3 {\n        total = total * 95 / 100;\n    }\n    if total > 50_000 {\n        total -= 1_000;\n    }\n    total = total.saturating_sub(discount);\n    total\n}";
        const FIRST: &str = "pub fn total_price(items: &[u32], tax_percent: u32) -> u32 {";
        const LAST: &str = "}";
        // 模型记错版本（13 行）：行数一致、逐行对应（不增删行，避免候选窗口滑动），
        // 但 shipping 常量/变量名与收尾两行的实现记错（fuzzy 低于阈值，必然失败）
        const WRONG: &str = "pub fn total_price(items: &[u32], tax_percent: u32) -> u32 {\n    let subtotal: u32 = items.iter().sum();\n    let delivery = if subtotal > 20_000 { 0 } else { 399 };\n    let taxed = subtotal * tax_percent / 100;\n    let mut total = subtotal + taxed + delivery;\n    if items.len() > 3 {\n        total = total * 95 / 100;\n    }\n    if total > 50_000 {\n        total -= 1_000;\n    }\n    let fee = handling_fee(total);\n    total + fee\n}";

        // 160 个填充函数把 total_price 顶到文件中部（约第 322 行），远离文件边界钳制
        let mut src = String::from("// pricing module\n");
        for i in 0..80 {
            src.push_str(&format!("pub fn helper_{i}(x: u32) -> u32 {{\n    x.wrapping_add({i})\n}}\n\n"));
        }
        src.push_str(OLD);
        src.push('\n');
        for i in 80..160 {
            src.push_str(&format!("pub fn helper_{i}(x: u32) -> u32 {{\n    x.wrapping_add({i})\n}}\n\n"));
        }
        let first = src.lines().position(|l| l.starts_with("pub fn total_price")).unwrap() + 1; // 1 起始
        let last = first + OLD.lines().count() - 1;
        let coord_digits = first.to_string().len() + last.to_string().len();
        assert!(first > 100 && last < src.lines().count() - 100, "目标必须位于文件中部");

        // ======== 场景 1：模型知道位置，一次改对 ========
        let mut leg1 = Meter::default();
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("pricing.rs"), &src).unwrap();
        leg1.model_out += OLD.len() + NEW.len();
        let r = edit_files(
            dir.path(),
            json!({"files":[{"path":"pricing.rs","edits":[{"oldText":OLD,"newText":NEW}]}]}),
        )
        .unwrap();
        leg1.tool_in += r.to_string().len();
        leg1.calls += 1;
        // 验证性 read：改动处 ±3 行
        let final_legacy = fs::read_to_string(dir.path().join("pricing.rs")).unwrap();
        leg1.tool_in += read_chunk(&final_legacy, first - 3, NEW.lines().count() + 6).len();
        leg1.calls += 1;

        let mut enh1 = Meter::default();
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("pricing.rs"), &src).unwrap();
        enh1.model_out += FIRST.len() + LAST.len() + NEW.len() + coord_digits;
        let r = edit_files_enhanced(
            dir.path(),
            json!({"files":[{"path":"pricing.rs","edits":[{"startLine":first,"endLine":last,"firstLine":FIRST,"lastLine":LAST,"newText":NEW}]}]}),
        )
        .unwrap();
        enh1.tool_in += r.to_string().len(); // 含 E 回显（±1 行），验证性 read 被消掉
        enh1.calls += 1;
        let final_enhanced = fs::read_to_string(dir.path().join("pricing.rs")).unwrap();
        assert_eq!(final_legacy, final_enhanced);

        // ======== 场景 2：模型记错内容，首次 edit 失败 ========
        let mut leg2 = Meter::default();
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("pricing.rs"), &src).unwrap();
        leg2.model_out += WRONG.len() + NEW.len();
        let err = edit_files(
            dir.path(),
            json!({"files":[{"path":"pricing.rs","edits":[{"oldText":WRONG,"newText":NEW}]}]}),
        )
        .unwrap_err();
        leg2.tool_in += err.len();
        leg2.calls += 1;
        // 错误没有正文，模型只能 read 拿真实内容再重试
        leg2.tool_in += read_chunk(&src, first, OLD.lines().count()).len();
        leg2.calls += 1;
        leg2.model_out += OLD.len() + NEW.len();
        let r = edit_files(
            dir.path(),
            json!({"files":[{"path":"pricing.rs","edits":[{"oldText":OLD,"newText":NEW}]}]}),
        )
        .unwrap();
        leg2.tool_in += r.to_string().len();
        leg2.calls += 1;

        let mut enh2 = Meter::default();
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("pricing.rs"), &src).unwrap();
        enh2.model_out += WRONG.len() + NEW.len();
        let err = edit_files_enhanced(
            dir.path(),
            json!({"files":[{"path":"pricing.rs","edits":[{"oldText":WRONG,"newText":NEW}]}]}),
        )
        .unwrap_err();
        // A：候选 @@ 坐标可见，firstLine/lastLine 内容在回读中均可见；
        // 候选 14 行 < 默认 cap 20 时不截断，超过 cap 才出现 "... (" 截断标记
        assert!(err.contains(&format!("Best candidate @@ {first}-")), "{err}");
        if last - first + 1 > candidate_readback_cap() {
            assert!(err.contains("... ("), "{err}");
        }
        assert!(err.contains(FIRST), "{err}");
        enh2.tool_in += err.len();
        enh2.calls += 1;
        // 模型直接用 H 重试（坐标与护栏均来自错误回读），无需 read
        enh2.model_out += FIRST.len() + LAST.len() + NEW.len() + coord_digits;
        let r = edit_files_enhanced(
            dir.path(),
            json!({"files":[{"path":"pricing.rs","edits":[{"startLine":first,"endLine":last,"firstLine":FIRST,"lastLine":LAST,"newText":NEW}]}]}),
        )
        .unwrap();
        enh2.tool_in += r.to_string().len();
        enh2.calls += 1;

        let pct = |a: usize, b: usize| format!("{:.0}%", (1.0 - a as f64 / b as f64) * 100.0);
        println!("\n================ H/E/A 效率实测（真实任务：改写大文件中部的 13 行函数） ================");
        println!("token 估算 = 字符数 / 4；model_out = 模型生成量，tool_in = 工具返回量");
        println!("参数：E 回显 ±{} 行（cap {}），A 候选回读 cap {} 行", PREVIEW_CONTEXT_LINES, PREVIEW_CAP_LINES, CANDIDATE_READBACK_CAP);
        println!("\n--- 场景 1：知道位置一次改对 ---");
        println!("            legacy(PI语义)   enhanced(Lyra)   节省");
        println!("定位负载    {:>4} 字符         {:>4} 字符        {} (oldText 复述 vs firstLine/lastLine 护栏)", leg1.model_out - NEW.len(), enh1.model_out - NEW.len() - coord_digits, pct(enh1.model_out - NEW.len() - coord_digits, leg1.model_out - NEW.len()));
        println!("工具往返    {:>4} 次           {:>4} 次          {} (E 回显消掉验证性 read)", leg1.calls, enh1.calls, pct(enh1.calls, leg1.calls));
        println!("总 token    {:>4}             {:>4}              {}", leg1.tokens(), enh1.tokens(), pct(enh1.tokens(), leg1.tokens()));
        println!("\n--- 场景 2：记错内容首次失败 ---");
        println!("            legacy(PI语义)   enhanced(Lyra)   节省");
        println!("工具往返    {:>4} 次           {:>4} 次          {} (A 错误自带候选正文，消掉补救 read)", leg2.calls, enh2.calls, pct(enh2.calls, leg2.calls));
        println!("总 token    {:>4}             {:>4}              {}", leg2.tokens(), enh2.tokens(), pct(enh2.tokens(), leg2.tokens()));
        println!("\n注：每次工具往返还隐含一次完整 agent 循环延迟（模型重新生成），往返次数是主要延迟来源。");
        println!("==========================================================================\n");

        // ======== 硬性断言：保证成立的收益——往返更少、定位负载更小 ========
        assert!(enh1.calls < leg1.calls);
        assert!(enh1.model_out < leg1.model_out);
        assert!(enh2.calls < leg2.calls);
        assert!(enh2.model_out < leg2.model_out);
        // 总 token 仅打印不断言：A 把 read 的正文折叠进错误、E 附带回显，
        // 失败场景的 token 取决于目标函数大小与截断参数，收益主轴是少一次完整 agent 往返。
    }
}

