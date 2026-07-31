use async_trait::async_trait;
use pi::sdk::{Config, ContentBlock, TextContent, Tool, ToolFactory, ToolOutput, ToolRegistry};
use pi::tools::ToolEffects;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

const DEFAULT_LINES: usize = 200;
const MAX_BYTES: usize = 32 * 1024;

pub struct VegaToolFactory {
    pub mcp_tools: Vec<super::mcp::McpToolSpec>,
}

impl ToolFactory for VegaToolFactory {
    fn create_tool_registry(&self, enabled: &[&str], cwd: &Path, config: &Config) -> ToolRegistry {
        let mut registry = pi::sdk::default_tool_registry(enabled, cwd, config);
        registry.push(Box::new(ReadFilesTool { cwd: cwd.to_path_buf() }));
        if enabled.contains(&"edit") {
            registry.push(Box::new(EditFilesTool { cwd: cwd.to_path_buf() }));
        }
        for tool in super::mcp::boxed_tools(&self.mcp_tools) { registry.push(tool); }
        registry
    }
}

fn output(text: impl Into<String>, details: Value) -> ToolOutput {
    ToolOutput {
        content: vec![ContentBlock::Text(TextContent::new(text))],
        details: Some(details),
        is_error: false,
    }
}

fn resolve_path(cwd: &Path, path: &str) -> PathBuf {
    let path = PathBuf::from(path);
    if path.is_absolute() { path } else { cwd.join(path) }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ReadTarget {
    Path(String),
    Options { path: String, offset: Option<usize>, limit: Option<usize> },
}

impl ReadTarget {
    fn parts(self) -> (String, usize, usize) {
        match self {
            Self::Path(path) => (path, 1, DEFAULT_LINES),
            Self::Options { path, offset, limit } => (path, offset.unwrap_or(1).max(1), limit.unwrap_or(DEFAULT_LINES).max(1)),
        }
    }
}

#[derive(Deserialize)]
struct ReadFilesInput { paths: Vec<ReadTarget> }

struct ReadFilesTool { cwd: PathBuf }

#[async_trait]
impl Tool for ReadFilesTool {
    fn name(&self) -> &str { "read_files" }
    fn label(&self) -> &str { "Read Files" }
    fn description(&self) -> &str { "并行读取多个互不依赖的 UTF-8 文本文件；每个目标可指定 offset/limit。" }
    fn parameters(&self) -> Value {
        json!({
            "type":"object",
            "properties":{"paths":{"type":"array","minItems":1,"items":{"anyOf":[
                {"type":"string"},
                {"type":"object","properties":{"path":{"type":"string"},"offset":{"type":"integer","minimum":1},"limit":{"type":"integer","minimum":1}},"required":["path"]}
            ]}}},
            "required":["paths"]
        })
    }
    fn effects(&self) -> ToolEffects { ToolEffects::read() }

    async fn execute(&self, _id: &str, input: Value, _update: Option<Box<dyn Fn(pi::sdk::ToolUpdate) + Send + Sync>>) -> pi::sdk::Result<ToolOutput> {
        let input: ReadFilesInput = serde_json::from_value(input)
            .map_err(|error| pi::sdk::Error::validation(error.to_string()))?;
        if input.paths.is_empty() { return Err(pi::sdk::Error::validation("paths 不能为空")); }
        let cwd = self.cwd.clone();
        let handles = input.paths.into_iter().map(|target| {
            let cwd = cwd.clone();
            std::thread::spawn(move || -> Result<Value, String> {
                let (display, offset, limit) = target.parts();
                let path = resolve_path(&cwd, &display);
                let bytes = fs::read(&path).map_err(|e| format!("读取 {display} 失败：{e}"))?;
                let text = String::from_utf8(bytes).map_err(|_| format!("{display} 不是 UTF-8 文本"))?;
                let lines: Vec<&str> = text.lines().collect();
                let start = offset.saturating_sub(1).min(lines.len());
                let end = (start + limit).min(lines.len());
                let mut selected = lines[start..end].join("\n");
                let mut truncated = end < lines.len();
                if selected.len() > MAX_BYTES {
                    selected.truncate(selected.floor_char_boundary(MAX_BYTES));
                    truncated = true;
                }
                Ok(json!({
                    "path": display, "content": selected, "offset": offset,
                    "lineCount": end.saturating_sub(start), "totalLines": lines.len(),
                    "nextOffset": truncated.then_some(end + 1)
                }))
            })
        }).collect::<Vec<_>>();
        let mut results = Vec::with_capacity(handles.len());
        for handle in handles {
            results.push(handle.join().map_err(|_| pi::sdk::Error::tool("read_files", "读取线程异常退出"))?
                .map_err(|e| pi::sdk::Error::tool("read_files", e))?);
        }
        Ok(output(serde_json::to_string(&results).unwrap_or_default(), json!({"files":results})))
    }
}

#[derive(Clone, Deserialize)]
struct SmartEdit { #[serde(rename="oldText")] old_text: String, #[serde(rename="newText")] new_text: String }
#[derive(Clone, Deserialize)]
struct EditTarget { path: String, edits: Vec<SmartEdit> }
#[derive(Deserialize)]
struct EditFilesInput { files: Vec<EditTarget> }

fn normalize_line(line: &str) -> String {
    line.trim_end()
        .replace(['‐','‑','‒','–','—','―','−'], "-")
        .replace(['‘','’','‚','‛'], "'")
        .replace(['“','”','„','‟'], "\"")
}

fn locate(content: &str, needle: &str, path: &str) -> Result<(usize, usize, &'static str), String> {
    let exact = content.match_indices(needle).map(|(index, _)| index).collect::<Vec<_>>();
    if exact.len() == 1 { return Ok((exact[0], exact[0] + needle.len(), "exact")); }
    if exact.len() > 1 { return Err(format!("{path}: oldText 精确匹配不唯一（{} 处）", exact.len())); }

    let target_lines = content.split_inclusive('\n').collect::<Vec<_>>();
    let needle_lines = needle.lines().collect::<Vec<_>>();
    if needle_lines.is_empty() { return Err(format!("{path}: oldText 不能为空")); }
    let normalized_needle = needle_lines.iter().map(|line| normalize_line(line)).collect::<Vec<_>>();
    let mut offsets = Vec::with_capacity(target_lines.len() + 1);
    offsets.push(0);
    for line in &target_lines { offsets.push(offsets.last().copied().unwrap_or(0) + line.len()); }
    let mut candidates = Vec::new();
    for start in 0..target_lines.len() {
        if start + normalized_needle.len() > target_lines.len() { break; }
        let candidate = target_lines[start..start + normalized_needle.len()].iter()
            .map(|line| normalize_line(line.trim_end_matches(['\r','\n'])))
            .collect::<Vec<_>>();
        if candidate == normalized_needle {
            let last = target_lines[start + normalized_needle.len() - 1];
            let trailing = last.len() - last.trim_end_matches(['\r', '\n']).len();
            candidates.push((offsets[start], offsets[start + normalized_needle.len()] - trailing));
        }
    }
    match candidates.as_slice() {
        [(start, end)] => Ok((*start, *end, "rstrip-unicode")),
        [] => Err(format!("{path}: 无法定位 oldText")),
        _ => Err(format!("{path}: oldText 归一化匹配不唯一（{} 处）", candidates.len())),
    }
}

fn apply_edits(content: &str, edits: &[SmartEdit], path: &str) -> Result<(String, Vec<Value>), String> {
    let mut ranges = Vec::with_capacity(edits.len());
    for edit in edits {
        let (start, end, mode) = locate(content, &edit.old_text, path)?;
        ranges.push((start, end, edit.new_text.as_str(), mode));
    }
    ranges.sort_by_key(|range| range.0);
    for pair in ranges.windows(2) {
        if pair[0].1 > pair[1].0 { return Err(format!("{path}: edits 目标区域重叠")); }
    }
    let matches = ranges.iter().map(|(start,end,_,mode)| json!({"start":start,"end":end,"mode":mode})).collect();
    let mut result = content.to_string();
    for (start, end, replacement, _) in ranges.into_iter().rev() { result.replace_range(start..end, replacement); }
    Ok((result, matches))
}

struct EditFilesTool { cwd: PathBuf }

#[async_trait]
impl Tool for EditFilesTool {
    fn name(&self) -> &str { "edit_files" }
    fn label(&self) -> &str { "Edit Files" }
    fn description(&self) -> &str { "事务式并行智能编辑多个文件；全部编辑先定位验证，歧义、重叠或任一写入失败时拒绝并回滚。" }
    fn parameters(&self) -> Value {
        json!({"type":"object","properties":{"files":{"type":"array","minItems":1,"items":{"type":"object","properties":{"path":{"type":"string"},"edits":{"type":"array","minItems":1,"items":{"type":"object","properties":{"oldText":{"type":"string"},"newText":{"type":"string"}},"required":["oldText","newText"]}}},"required":["path","edits"]}}},"required":["files"]})
    }
    fn effects(&self) -> ToolEffects { ToolEffects::write() }

    async fn execute(&self, _id: &str, input: Value, _update: Option<Box<dyn Fn(pi::sdk::ToolUpdate) + Send + Sync>>) -> pi::sdk::Result<ToolOutput> {
        let input: EditFilesInput = serde_json::from_value(input).map_err(|e| pi::sdk::Error::validation(e.to_string()))?;
        if input.files.is_empty() { return Err(pi::sdk::Error::validation("files 不能为空")); }
        let mut grouped: HashMap<PathBuf, (String, Vec<SmartEdit>)> = HashMap::new();
        for file in input.files {
            let target = resolve_path(&self.cwd, &file.path);
            let entry = grouped.entry(target).or_insert_with(|| (file.path.clone(), Vec::new()));
            entry.1.extend(file.edits);
        }
        let mut prepared = Vec::with_capacity(grouped.len());
        for (target, (display, edits)) in grouped {
            let original = fs::read_to_string(&target).map_err(|e| pi::sdk::Error::tool("edit_files", format!("读取 {display} 失败：{e}")))?;
            let (updated, matches) = apply_edits(&original, &edits, &display).map_err(|e| pi::sdk::Error::tool("edit_files", e))?;
            prepared.push((target, display, original, updated, matches));
        }
        let mut written = Vec::new();
        for (target, display, original, updated, _) in &prepared {
            let temp = target.with_extension(format!("nova-{}.tmp", std::process::id()));
            let result = fs::write(&temp, updated).and_then(|_| fs::rename(&temp, target));
            if let Err(error) = result {
                let _ = fs::remove_file(temp);
                for (path, prior) in &written { let _ = fs::write(path, prior); }
                return Err(pi::sdk::Error::tool("edit_files", format!("写入 {display} 失败，已回滚：{error}")));
            }
            written.push((target.clone(), original.clone()));
        }
        let matches = prepared.iter().map(|(_,path,_,_,matches)| json!({"path":path,"edits":matches})).collect::<Vec<_>>();
        Ok(output(format!("已并行智能编辑 {} 个文件", prepared.len()), json!({"paths":prepared.iter().map(|x|&x.1).collect::<Vec<_>>(),"matches":matches})))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smart_edits_match_exact_and_normalized_unicode() {
        let edits = vec![SmartEdit { old_text: "let x = 1;  \nlet y = \"ok\";".into(), new_text: "done();".into() }];
        let (result, matches) = apply_edits("let x = 1;\nlet y = “ok”;\n", &edits, "sample.rs").unwrap();
        assert_eq!(result, "done();\n");
        assert_eq!(matches[0]["mode"], "rstrip-unicode");
    }

    #[test]
    fn smart_edits_reject_ambiguity_and_overlap() {
        let duplicate = vec![SmartEdit { old_text: "same".into(), new_text: "next".into() }];
        assert!(apply_edits("same\nsame\n", &duplicate, "sample.txt").unwrap_err().contains("不唯一"));
        let overlap = vec![
            SmartEdit { old_text: "alpha beta".into(), new_text: "a".into() },
            SmartEdit { old_text: "beta gamma".into(), new_text: "b".into() },
        ];
        assert!(apply_edits("alpha beta gamma", &overlap, "sample.txt").unwrap_err().contains("重叠"));
    }
}
