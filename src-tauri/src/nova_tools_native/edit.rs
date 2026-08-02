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

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EditInput {
    old_text: String,
    new_text: String,
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
    total_edits: usize,
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
        return Err(format!("Ambiguous exact match for edits[{edit_index}] in {path}: {} occurrences; add more context.", exact.len()));
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
            return Err(format!("Ambiguous {mode} match for edits[{edit_index}] in {path}: {} occurrences; add more context.", matches.len()));
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
        return Err(format!(
            "Ambiguous relative-indent match for edits[{edit_index}] in {path}; add more context."
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
    if let (Some((_, a)), Some((_, b))) = (best, second) {
        if a >= FUZZY_THRESHOLD - AMBIGUITY_MARGIN && a - b < AMBIGUITY_MARGIN {
            return Err(format!("Ambiguous fuzzy match for edits[{edit_index}] in {path} ({}% vs {}%); add more context.", (a * 100.0).round(), (b * 100.0).round()));
        }
    }
    let Some((start, score)) = best else {
        return Err(format!(
            "Could not find a sufficiently similar match for edits[{edit_index}] in {path}."
        ));
    };
    if score < FUZZY_THRESHOLD {
        return Err(format!("Could not find a sufficiently similar match for edits[{edit_index}] in {path} (best {}%).", (score * 100.0).round()));
    }
    let (index, length) = span_for_lines(
        &target_lines,
        &starts,
        start,
        pattern_lines.len(),
        pattern_lines.last() == Some(&""),
    );
    let matched = content[index..index + length].to_string();
    let _ = total_edits;
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

fn apply_smart_edits(
    content: &str,
    edits: &[EditInput],
    path: &str,
) -> Result<(String, Vec<MatchInfo>), String> {
    let normalized: Vec<_> = edits
        .iter()
        .map(|e| EditInput {
            old_text: e.old_text.replace("\r\n", "\n"),
            new_text: e.new_text.replace("\r\n", "\n"),
        })
        .collect();
    let mut located = Vec::with_capacity(normalized.len());
    for (i, edit) in normalized.iter().enumerate() {
        let mut found = locate_edit(content, &edit.old_text, path, i, normalized.len())?;
        found.new_text = edit.new_text.clone();
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
    for m in located.iter().rev() {
        let replacement = rebase_indent(&m.new_text, &m.old_text, &m.matched_text);
        output.replace_range(m.index..m.index + m.length, &replacement);
    }
    if output == content {
        return Err(format!("No changes made to {path}."));
    }
    Ok((output, matches))
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

fn prepare_file(target: PathBuf, file: FileInput) -> Result<Prepared, String> {
    let original = fs::read(&target).map_err(|e| format!("{}: {e}", file.path))?;
    let raw =
        std::str::from_utf8(&original).map_err(|e| format!("{} is not UTF-8: {e}", file.path))?;
    let (bom, body) = raw
        .strip_prefix('\u{feff}')
        .map(|s| ("\u{feff}", s))
        .unwrap_or(("", raw));
    let crlf = body.contains("\r\n");
    let normalized = body.replace("\r\n", "\n");
    let (edited, matches) = apply_smart_edits(&normalized, &file.edits, &file.path)?;
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
    })
}

pub fn edit_files(root: &Path, params: Value) -> Result<Value, String> {
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
                results.lock().unwrap()[index] = Some(prepare_file(target, file));
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
    Ok(json!({
        "message": format!("已并行智能编辑 {} 个文件", prepared.len()),
        "paths": prepared.iter().map(|f| &f.display_path).collect::<Vec<_>>(),
        "matches": prepared.iter().map(|f| json!({"path": f.display_path, "edits": f.matches})).collect::<Vec<_>>(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

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
    }

    #[test]
    fn rejects_ambiguous_and_does_not_write() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), "same\nsame\n").unwrap();
        assert!(edit_files(
            dir.path(),
            json!({"files":[{"path":"a.txt","edits":[{"oldText":"same","newText":"x"}]}]})
        )
        .is_err());
        assert_eq!(
            fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "same\nsame\n"
        );
    }

    #[test]
    fn matches_relative_indent_and_rebases_replacement() {
        let source = "function outer() {\n    if (ready) {   \n        log(“old”);\n    }\n}";
        let edits = vec![EditInput {
            old_text: "if (ready) {\n    log(\"old\");\n}".into(),
            new_text: "if (ready) {\n    log(\"new\");\n}".into(),
        }];
        let (output, matches) = apply_smart_edits(source, &edits, "sample.js").unwrap();
        assert_eq!(matches[0].mode, "relative-indent");
        assert!(output.contains("    if (ready) {\n        log(\"new\");\n    }"));
    }

    #[test]
    fn rejects_fuzzy_ambiguity() {
        let source = "function first() {\n  calculate(invoiceSubtotal, regionalTax, shippingFee, discountCode, currencyA);\n}\nfunction second() {\n  calculate(invoiceSubtotal, regionalTax, shippingFee, discountCode, currencyB);\n}";
        let edits = vec![EditInput {
            old_text:
                "calculate(invoiceSubtotal, regionalTax, shippingFee, discountCode, currencyC);"
                    .into(),
            new_text: "return total;".into(),
        }];
        assert!(apply_smart_edits(source, &edits, "ambiguous.js")
            .unwrap_err()
            .contains("Ambiguous fuzzy match"));
    }
}
