//! 按 git 仓库划分的项目记忆前缀。
//! 开启且模型上下文窗口 ≥ 1M 时，压缩只看见后半段工作区；前半段按项目快照加载，会话中不折叠。

use crate::lyra::history::{now_ms, text_content};
use crate::lyra::tokens::TokenCounter;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

/// 启用该机制所需的最小声明上下文窗口。
pub const MIN_WINDOW: u64 = 1_000_000;
/// 代码片段区锁定额度。
pub const CODE_BUDGET: u64 = 200_000;
const PROTOCOL_RESERVE: u64 = 256;
const MAX_OUTPUT_RESERVE_RATIO: f64 = 0.25;
const SNAPSHOT_VERSION: u32 = 1;

/// 开启项目记忆时追加到系统提示，说明前缀怎么用；不写进记忆正文。
pub const GUIDE: &str = r#"

项目记忆：
- 开头的 <project-memory> 是本仓库的长期记忆，不是用户提问。各节：索引、有效经验、失败经验、事实、代码。标了「已过时」的代码，先重新读取再引用。
- 这段前缀和系统提示不能压缩。用量数字只计算工作区，也就是这段前缀之后的内容。记忆里已有的先用，不必重复通读。路径、签名、哈希、行号按原文使用。
"#;

/// ACP 开启时追加：45% 是建议，不是一到点就压缩。
pub const ACP_NOTE: &str = "
- 不要把项目记忆前缀交给 compress。acp_status 和 <context-budget> 的窗口与百分比只计工作区。45% 起是建议压缩已完成的工作，由你决定；不调用 compress 就不会压。大约 92% 时系统才会兜底截断。
";

const HEADING_INDEX: &str = "索引";
const HEADING_POSITIVE: &str = "有效经验";
const HEADING_NEGATIVE: &str = "失败经验";
const HEADING_FACTS: &str = "事实";
const HEADING_CODE: &str = "代码";
const ACK: &str = "已了解。";
const STALE_MARK: &str = "已过时，需重新读取";

pub fn eligible(enabled: bool, context_window: u64) -> bool {
    enabled && context_window >= MIN_WINDOW
}

/// 返回 (前半段, 工作区, 记述区, 代码区)。输出预留先从整窗扣除，再对半。
pub fn budgets(window: u64, reserved_output: u64) -> (u64, u64, u64, u64) {
    let output = reserved_output.min((window as f64 * MAX_OUTPUT_RESERVE_RATIO) as u64);
    let usable = window
        .saturating_sub(output)
        .saturating_sub(PROTOCOL_RESERVE);
    let workspace = usable / 2;
    let front = usable.saturating_sub(workspace);
    let code = CODE_BUDGET.min(front);
    let prose = front.saturating_sub(code);
    (front, workspace, prose, code)
}

/// 把各种 git remote URL 收成 `host/owner/repo`，使同一仓库的不同本地目录共用一把钥匙。
pub fn normalize_remote(url: &str) -> String {
    let mut s = url.trim().to_string();
    if s.len() >= 2 {
        let bytes = s.as_bytes();
        if (bytes[0] == b'"' && bytes[s.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[s.len() - 1] == b'\'')
        {
            s = s[1..s.len() - 1].trim().to_string();
        }
    }
    if !s.contains("://") {
        if let Some((left, right)) = s.split_once(':') {
            let drive =
                left.len() == 1 && left.chars().next().is_some_and(|c| c.is_ascii_alphabetic());
            if !drive && !left.contains('\\') && !Path::new(left).is_absolute() {
                let host = left.rsplit('@').next().unwrap_or(left);
                return strip_git(&format!("{host}/{right}"));
            }
        }
        return strip_git(&s);
    }
    let rest = s.splitn(2, "://").nth(1).unwrap_or(&s);
    let rest = rest.split_once('@').map(|(_, host)| host).unwrap_or(rest);
    let rest = rest.trim_start_matches('/');
    let normalized = if let Some(slash) = rest.find('/') {
        let hostport = &rest[..slash];
        let path = &rest[slash + 1..];
        let host = hostport.split(':').next().unwrap_or(hostport);
        format!("{host}/{path}")
    } else {
        rest.to_string()
    };
    strip_git(&normalized)
}

fn strip_git(s: &str) -> String {
    s.trim()
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .trim_end_matches('/')
        .to_lowercase()
}

fn git_stdout(cwd: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// 以 git 远程路径作为项目键；没有远程时退回到该仓库的 common dir（同一 worktree 家族共用）。
pub fn project_key(cwd: &Path) -> Option<String> {
    if git_stdout(cwd, &["rev-parse", "--is-inside-work-tree"]).as_deref() != Some("true") {
        return None;
    }
    if let Some(url) = git_stdout(cwd, &["remote", "get-url", "origin"]).or_else(|| {
        let remotes = git_stdout(cwd, &["remote"])?;
        let first = remotes.lines().map(str::trim).find(|n| !n.is_empty())?;
        git_stdout(cwd, &["remote", "get-url", first])
    }) {
        return Some(normalize_remote(&url));
    }
    let dir = git_stdout(
        cwd,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .or_else(|| git_stdout(cwd, &["rev-parse", "--git-common-dir"]))?;
    let path = PathBuf::from(&dir);
    let abs = dunce::canonicalize(&path).unwrap_or(path);
    Some(format!(
        "local:{}",
        abs.to_string_lossy().replace('\\', "/").to_lowercase()
    ))
}


/// 前缀（系统提示、工具定义、项目记忆正文）不变时得到同一个 `prompt_cache_key`；
/// 没有项目记忆前缀时返回 None，由调用方回退到 session_id。
pub fn prompt_cache_key(system: &str, tools: &Value, prefix_messages: &[Value]) -> Option<String> {
    if prefix_messages.is_empty() {
        return None;
    }
    let mut hasher = Sha256::new();
    hasher.update(system.as_bytes());
    hasher.update([0]);
    hasher.update(tools.to_string().as_bytes());
    hasher.update([0]);
    for message in prefix_messages {
        let role = message.get("role").and_then(Value::as_str).unwrap_or("");
        hasher.update(role.as_bytes());
        hasher.update([0]);
        hasher.update(text_content(&message["content"]).as_bytes());
        hasher.update([0]);
    }
    Some(format!("{:x}", hasher.finalize()))
}

pub fn snapshot_path(store_dir: &Path, key: &str) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(key.as_bytes());
    let hash = format!("{:x}", hasher.finalize());
    let mut slug: String = key
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    slug.truncate(80);
    store_dir.join(format!("{slug}-{}.json", &hash[..12]))
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct IndexEntry {
    pub path: String,
    pub role: String,
    pub hits: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Note {
    /// 简练白话，把意思写清楚即可。
    pub text: String,
    pub evidence: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct CodeSnippet {
    pub path: String,
    pub start: u32,
    pub end: u32,
    pub hash: String,
    pub body: String,
    #[serde(default)]
    pub stale: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Snapshot {
    pub version: u32,
    pub project_key: String,
    pub updated_at: u64,
    pub index: Vec<IndexEntry>,
    pub positive: Vec<Note>,
    pub negative: Vec<Note>,
    pub facts: Vec<Note>,
    pub code: Vec<CodeSnippet>,
}

impl Snapshot {
    pub fn new(key: &str) -> Self {
        Self {
            version: SNAPSHOT_VERSION,
            project_key: key.to_string(),
            updated_at: now_ms(),
            ..Self::default()
        }
    }

    pub fn load(path: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(path).ok()?;
        let snap: Self = serde_json::from_str(&text).ok()?;
        (snap.version == SNAPSHOT_VERSION).then_some(snap)
    }

    pub fn save(&mut self, path: &Path) {
        self.version = SNAPSHOT_VERSION;
        self.updated_at = now_ms();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(text) = serde_json::to_string_pretty(self) {
            let tmp = path.with_extension("json.tmp");
            if std::fs::write(&tmp, text)
                .and_then(|_| std::fs::rename(&tmp, path))
                .is_err()
            {
                eprintln!("lyra: 写项目记忆 {} 失败", path.display());
            }
        }
    }

    /// 用当前仓库文件列表刷新索引：已有职责和点击次数保留；额度裁剪在渲染时做。
    pub fn refresh_index(&mut self, cwd: &Path) {
        let Some(files) = list_repo_files(cwd) else {
            return;
        };
        let mut known: HashMap<String, IndexEntry> =
            self.index.drain(..).map(|e| (e.path.clone(), e)).collect();
        let mut next = Vec::with_capacity(files.len());
        for path in files {
            if let Some(old) = known.remove(&path) {
                next.push(old);
            } else {
                next.push(IndexEntry {
                    role: default_role(&path),
                    hits: 0,
                    path,
                });
            }
        }
        next.sort_by(|a, b| a.path.cmp(&b.path));
        self.index = next;
    }

    /// 磁盘优于缓存：对不上哈希的片段标过时并去掉正文，已删文件只留短标记。
    pub fn verify_code(&mut self, cwd: &Path) {
        for snippet in &mut self.code {
            let file = cwd.join(&snippet.path);
            match std::fs::read_to_string(&file) {
                Err(_) => {
                    snippet.stale = true;
                    snippet.body.clear();
                    snippet.hash.clear();
                }
                Ok(text) => {
                    let current = hash_range(&text, snippet.start, snippet.end);
                    if current != snippet.hash {
                        snippet.stale = true;
                        snippet.body.clear();
                    }
                }
            }
        }
        self.code
            .sort_by(|a, b| a.path.cmp(&b.path).then(a.start.cmp(&b.start)));
    }

    pub fn record_hit(&mut self, path: &str) {
        if let Some(entry) = self.index.iter_mut().find(|e| e.path == path) {
            entry.hits = entry.hits.saturating_add(1);
        }
    }

    /// 渲染成稳定的前缀消息（用户条目 + 短确认，避免与下一轮用户消息合并）。标题和正文都用简练白话。
    pub fn render_messages(
        &self,
        prose_budget: u64,
        code_budget: u64,
        counter: &TokenCounter,
    ) -> Vec<Value> {
        let text = self.render_text(prose_budget, code_budget, counter);
        if text.is_empty() {
            return Vec::new();
        }
        vec![
            json!({ "role": "user", "content": [{ "type": "text", "text": text }] }),
            json!({ "role": "assistant", "content": [{ "type": "text", "text": ACK }] }),
        ]
    }

    fn render_text(&self, prose_budget: u64, code_budget: u64, counter: &TokenCounter) -> String {
        let mut body = String::from("<project-memory>\n");
        body.push_str("# ");
        body.push_str(HEADING_INDEX);
        body.push('\n');
        body.push_str(&render_index(
            &self.index,
            prose_budget.saturating_mul(18) / 100,
            counter,
        ));

        let mut notes = String::new();
        append_notes(&mut notes, HEADING_POSITIVE, &self.positive);
        append_notes(&mut notes, HEADING_NEGATIVE, &self.negative);
        append_notes(&mut notes, HEADING_FACTS, &self.facts);
        let notes_budget = prose_budget.saturating_sub(counter.count_text(&body));
        body.push_str(&trim_to_budget(&notes, notes_budget, counter));

        body.push_str("# ");
        body.push_str(HEADING_CODE);
        body.push('\n');
        body.push_str(&render_code(&self.code, code_budget, counter));
        body.push_str("</project-memory>");
        body
    }
}

fn append_notes(out: &mut String, heading: &str, notes: &[Note]) {
    if notes.is_empty() {
        return;
    }
    out.push('#');
    out.push(' ');
    out.push_str(heading);
    out.push('\n');
    for note in notes {
        let text = note.text.trim();
        if text.is_empty() {
            continue;
        }
        out.push_str("- ");
        out.push_str(text);
        if !note.evidence.trim().is_empty() {
            out.push(' ');
            out.push('[');
            out.push_str(note.evidence.trim());
            out.push(']');
        }
        out.push('\n');
    }
}

fn render_index(entries: &[IndexEntry], budget: u64, counter: &TokenCounter) -> String {
    if entries.is_empty() || budget == 0 {
        return String::new();
    }
    let mut ranked: Vec<&IndexEntry> = entries.iter().collect();
    // 先按路径排好（稳定前缀）；额度不够时再按高频取，但写出时仍按路径排序。
    ranked.sort_by(|a, b| a.path.cmp(&b.path));
    let marks = index_marks(entries);
    let full = format_index(&ranked, &marks);
    if counter.count_text(&full) <= budget {
        return full;
    }
    ranked.sort_by(|a, b| b.hits.cmp(&a.hits).then_with(|| a.path.cmp(&b.path)));
    let mut kept: Vec<&IndexEntry> = Vec::new();
    for entry in ranked {
        let mut trial = kept.clone();
        trial.push(entry);
        trial.sort_by(|a, b| a.path.cmp(&b.path));
        if counter.count_text(&format_index(&trial, &marks)) > budget {
            continue;
        }
        kept = trial;
    }
    format_index(&kept, &marks)
}

fn format_index(entries: &[&IndexEntry], marks: &IndexMarks) -> String {
    let mut out = String::new();
    let mut last_module = "";
    for entry in entries {
        let module = entry.path.split('/').next().unwrap_or("");
        if module != last_module {
            out.push_str("## ");
            out.push_str(module);
            out.push('\n');
            last_module = module;
        }
        out.push_str(&entry.path);
        if !entry.role.is_empty() {
            out.push(' ');
            out.push_str(&entry.role);
        }
        if marks.key.contains(&entry.path) {
            out.push(' ');
            out.push_str(MARK_KEY);
        }
        if marks.hot.contains(&entry.path) {
            out.push(' ');
            out.push_str(MARK_HOT);
        }
        out.push('\n');
    }
    out
}

const MARK_HOT: &str = "高频";
const MARK_KEY: &str = "重点";

struct IndexMarks {
    key: HashSet<String>,
    hot: HashSet<String>,
}

fn index_marks(entries: &[IndexEntry]) -> IndexMarks {
    let mut key = HashSet::new();
    let mut ranked: Vec<&IndexEntry> = entries.iter().collect();
    ranked.sort_by(|a, b| b.hits.cmp(&a.hits).then_with(|| a.path.cmp(&b.path)));
    let mut picked = 0;
    for entry in ranked {
        if entry.hits < 2 || picked >= 3 {
            break;
        }
        key.insert(entry.path.clone());
        picked += 1;
    }
    for entry in entries {
        if is_key_path(&entry.path) {
            key.insert(entry.path.clone());
        }
    }
    IndexMarks {
        key,
        hot: hot_paths(entries),
    }
}

fn hot_paths(entries: &[IndexEntry]) -> HashSet<String> {
    let mut hits: Vec<u64> = entries.iter().map(|e| e.hits).filter(|&h| h > 0).collect();
    if hits.is_empty() {
        return HashSet::new();
    }
    hits.sort_unstable();
    let max = *hits.last().unwrap();
    if max < 3 {
        return HashSet::new();
    }
    let median = hits[hits.len() / 2];
    let thresh = if median.saturating_mul(2) <= max {
        3.max(median.saturating_mul(2))
    } else {
        let second = hits.iter().rev().find(|&&h| h < max).copied().unwrap_or(0);
        let unique_max = hits.iter().filter(|&&h| h == max).count() == 1;
        if unique_max && max >= 3 && max >= second.saturating_mul(2) {
            max
        } else {
            return HashSet::new();
        }
    };
    let qualify: Vec<&IndexEntry> = entries.iter().filter(|e| e.hits >= thresh).collect();
    if qualify.len() > 8 {
        return HashSet::new();
    }
    qualify.into_iter().map(|e| e.path.clone()).collect()
}

fn is_key_path(path: &str) -> bool {
    let path = path.replace('\\', "/");
    let (dir, name) = match path.rfind('/') {
        Some(i) => (&path[..i], &path[i + 1..]),
        None => ("", path.as_str()),
    };
    let name = name.to_ascii_lowercase();
    if is_readme(&name) {
        return dir.is_empty();
    }
    is_entry_file(&name) && matches!(dir, "" | "src" | "src-tauri/src" | "lib" | "app" | "cmd")
}

fn is_readme(name: &str) -> bool {
    name == "readme" || name.starts_with("readme.")
}

fn is_entry_file(name: &str) -> bool {
    matches!(
        name,
        "main.rs"
            | "lib.rs"
            | "main.ts"
            | "main.tsx"
            | "main.js"
            | "main.jsx"
            | "main.py"
            | "index.ts"
            | "index.tsx"
            | "index.js"
            | "index.jsx"
            | "app.ts"
            | "app.tsx"
            | "app.js"
            | "app.jsx"
            | "app.py"
    )
}

/// 把工具参数里的路径收成索引用的仓库相对路径；对不上就忽略。
pub fn index_path(cwd: &Path, input: &str) -> Option<String> {
    let resolved = crate::lyra::read::resolve_read_path(cwd, input).ok()?;
    let root = git_stdout(cwd, &["rev-parse", "--show-toplevel"])?;
    let root = PathBuf::from(root);
    let resolved = dunce::canonicalize(&resolved).ok()?;
    let root = dunce::canonicalize(&root).ok()?;
    let rel = resolved.strip_prefix(&root).ok()?;
    Some(rel.to_string_lossy().replace('\\', "/"))
}

fn render_code(snippets: &[CodeSnippet], budget: u64, counter: &TokenCounter) -> String {
    let mut snippets: Vec<&CodeSnippet> = snippets.iter().collect();
    snippets.sort_by(|a, b| a.path.cmp(&b.path).then(a.start.cmp(&b.start)));
    let mut out = String::new();
    for snippet in snippets {
        let block = format_snippet(snippet);
        let mut trial = out.clone();
        trial.push_str(&block);
        if counter.count_text(&trial) > budget {
            continue;
        }
        out = trial;
    }
    out
}

fn format_snippet(snippet: &CodeSnippet) -> String {
    if snippet.stale || snippet.body.is_empty() {
        return format!(
            "## {} {}-{}\n{STALE_MARK}\n",
            snippet.path, snippet.start, snippet.end
        );
    }
    format!(
        "## {} {}-{} {}\n{}\n",
        snippet.path,
        snippet.start,
        snippet.end,
        snippet.hash,
        snippet.body.trim_end()
    )
}

fn trim_to_budget(text: &str, budget: u64, counter: &TokenCounter) -> String {
    if budget == 0 || text.is_empty() {
        return String::new();
    }
    if counter.count_text(text) <= budget {
        return text.to_string();
    }
    let mut kept = String::new();
    for line in text.lines() {
        let mut trial = kept.clone();
        trial.push_str(line);
        trial.push('\n');
        if counter.count_text(&trial) > budget {
            break;
        }
        kept = trial;
    }
    kept
}

fn default_role(path: &str) -> String {
    Path::new(path)
        .parent()
        .and_then(|p| p.file_name())
        .or_else(|| Path::new(path).file_stem())
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn hash_range(text: &str, start: u32, end: u32) -> String {
    let start = start.max(1) as usize;
    let end = end.max(start as u32) as usize;
    let slice: String = text
        .lines()
        .skip(start.saturating_sub(1))
        .take(end.saturating_sub(start - 1))
        .collect::<Vec<_>>()
        .join("\n");
    let mut hasher = Sha256::new();
    hasher.update(slice.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn list_repo_files(cwd: &Path) -> Option<Vec<String>> {
    let out = Command::new("git")
        .args(["ls-files", "-z"])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let mut files: Vec<String> = out
        .stdout
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .filter_map(|s| String::from_utf8(s.to_vec()).ok())
        .filter(|p| !ignore_index_path(p))
        .collect();
    files.sort();
    files.dedup();
    Some(files)
}

fn ignore_index_path(path: &str) -> bool {
    let lower = path.to_lowercase();
    const SKIP: &[&str] = &[
        "/node_modules/",
        "/target/",
        "/dist/",
        "/.git/",
        "/vendor/",
        "package-lock.json",
        "cargo.lock",
        "yarn.lock",
        "pnpm-lock.yaml",
    ];
    if SKIP.iter().any(|s| lower.contains(s)) {
        return true;
    }
    const BIN: &[&str] = &[
        ".png", ".jpg", ".jpeg", ".gif", ".webp", ".ico", ".pdf", ".zip", ".gz", ".tar", ".woff",
        ".woff2", ".ttf", ".otf", ".exe", ".dll", ".so", ".dylib", ".bin", ".wasm", ".mp4", ".mov",
        ".webm", ".mp3", ".wav",
    ];
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| {
            BIN.iter()
                .any(|b| ext.eq_ignore_ascii_case(b.trim_start_matches('.')))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn normalize_remote_unifies_clone_urls() {
        let expected = "github.com/llyb120/nova-client";
        for url in [
            "https://github.com/llyb120/nova-client.git",
            "https://github.com/Llyb120/Nova-Client",
            "git@github.com:llyb120/nova-client.git",
            "ssh://git@github.com/llyb120/nova-client.git",
            "https://user:token@github.com/llyb120/nova-client.git/",
            "ssh://git@github.com:22/llyb120/nova-client.git",
        ] {
            assert_eq!(normalize_remote(url), expected, "{url}");
        }
    }

    #[test]
    fn budgets_split_usable_window_and_lock_code() {
        let (front, workspace, prose, code) = budgets(1_000_000, 0);
        assert_eq!(front + workspace, 1_000_000 - PROTOCOL_RESERVE);
        assert_eq!(front, workspace);
        assert_eq!(code, CODE_BUDGET);
        assert_eq!(prose + code, front);
        let (front_r, work_r, _, _) = budgets(1_000_000, 200_000);
        assert!(front_r + work_r < front + workspace);
        assert_eq!(front_r, work_r);
    }

    #[test]
    fn eligible_requires_switch_and_million_window() {
        assert!(!eligible(false, 1_000_000));
        assert!(!eligible(true, 999_999));
        assert!(eligible(true, 1_000_000));
        assert!(eligible(true, 2_000_000));
    }

    #[test]
    fn project_key_follows_remote_not_folder() {
        let clone_a = repo_with_remote("https://github.com/llyb120/nova-client.git");
        let clone_b = repo_with_remote("git@github.com:llyb120/nova-client.git");
        assert_eq!(
            project_key(&clone_a).as_deref(),
            Some("github.com/llyb120/nova-client")
        );
        assert_eq!(project_key(&clone_a), project_key(&clone_b));
        assert_ne!(clone_a, clone_b);
    }

    #[test]
    fn render_uses_plain_section_headings() {
        let mut snap = Snapshot::new("k");
        snap.positive.push(Note {
            text: "先跑测试再改接口".into(),
            evidence: "src/a.rs".into(),
        });
        snap.negative.push(Note {
            text: "不要直接改生成文件".into(),
            evidence: String::new(),
        });
        snap.facts.push(Note {
            text: "默认压缩是 ACP".into(),
            evidence: String::new(),
        });
        let text = snap.render_text(10_000, 200, &TokenCounter::default());
        for heading in ["# 索引", "# 有效经验", "# 失败经验", "# 事实", "# 代码"] {
            assert!(text.contains(heading), "{text}");
        }
        assert!(text.contains("先跑测试再改接口") && text.contains("[src/a.rs]"));
        for heading in ["# 索\n", "# 正\n", "# 反\n", "# 事\n", "# 码\n"] {
            assert!(!text.contains(heading), "{text}");
        }
        let messages = snap.render_messages(10_000, 200, &TokenCounter::default());
        assert_eq!(messages[1]["content"][0]["text"], ACK);
        assert!(
            GUIDE.contains("长期记忆") && GUIDE.contains("工作区") && !GUIDE.contains("半文言")
        );
        assert!(
            ACP_NOTE.contains("45%") && ACP_NOTE.contains("建议") && ACP_NOTE.contains("compress")
        );
    }

    #[test]
    fn index_keeps_path_order_and_drops_cold_files_when_tight() {
        let snap = Snapshot {
            index: vec![
                IndexEntry {
                    path: "b.rs".into(),
                    role: "冷".into(),
                    hits: 0,
                },
                IndexEntry {
                    path: "a.rs".into(),
                    role: "热".into(),
                    hits: 9,
                },
            ],
            ..Snapshot::new("k")
        };
        let counter = TokenCounter::default();
        let full = snap.render_text(10_000, 0, &counter);
        assert!(full.find("##").is_some());
        assert!(full.contains("a.rs"));
        assert!(full.contains("b.rs"));
        assert!(full.find("a.rs") < full.find("b.rs"));

        let hot = snap.index.iter().find(|e| e.path == "a.rs").unwrap();
        let hot_budget = counter.count_text(&format_index(&[hot], &index_marks(&snap.index)));
        let tight = render_index(&snap.index, hot_budget, &counter);
        assert!(tight.contains("a.rs"), "{tight}");
        assert!(!tight.contains("b.rs"), "{tight}");
        assert!(hot_budget < counter.count_text(&render_index(&snap.index, 10_000, &counter)));
    }

    #[test]
    fn index_marks_hot_and_key_files_without_reordering() {
        let snap = Snapshot {
            index: vec![
                IndexEntry {
                    path: "README.md".into(),
                    role: String::new(),
                    hits: 0,
                },
                IndexEntry {
                    path: "notes.txt".into(),
                    role: "备忘".into(),
                    hits: 1,
                },
                IndexEntry {
                    path: "src/a.rs".into(),
                    role: "解析".into(),
                    hits: 8,
                },
                IndexEntry {
                    path: "src/b.rs".into(),
                    role: "冷".into(),
                    hits: 1,
                },
                IndexEntry {
                    path: "src/main.rs".into(),
                    role: "入口".into(),
                    hits: 3,
                },
                IndexEntry {
                    path: "src/util.rs".into(),
                    role: String::new(),
                    hits: 1,
                },
            ],
            ..Snapshot::new("k")
        };
        let text = render_index(&snap.index, 10_000, &TokenCounter::default());
        assert!(text.find("README.md").unwrap() < text.find("notes.txt").unwrap());
        assert!(text.find("notes.txt").unwrap() < text.find("src/a.rs").unwrap());
        assert!(text.find("src/a.rs").unwrap() < text.find("src/b.rs").unwrap());
        assert!(text.contains("README.md 重点"), "{text}");
        assert!(text.contains("src/main.rs 入口 重点 高频"), "{text}");
        assert!(text.contains("src/a.rs 解析 重点 高频"), "{text}");
        assert!(text.contains("notes.txt 备忘\n"), "{text}");
        assert!(!text.contains("notes.txt 备忘 重点"), "{text}");
        assert!(!text.contains("notes.txt 备忘 高频"), "{text}");
        assert!(!text.contains("src/b.rs 冷 重点"), "{text}");
        assert!(!text.contains("src/b.rs 冷 高频"), "{text}");
        assert!(!text.contains("src/util.rs 重点"), "{text}");
        assert!(!text.contains("src/util.rs 高频"), "{text}");
        let marked = text.matches("重点").count() + text.matches("高频").count();
        assert!(marked <= 5, "too many marks: {text}");
    }

    #[test]
    fn index_does_not_mark_a_flat_pack_as_hot() {
        let index: Vec<IndexEntry> = (0..20)
            .map(|i| IndexEntry {
                path: format!("src/f{i:02}.rs"),
                role: String::new(),
                hits: 5,
            })
            .collect();
        let text = render_index(&index, 10_000, &TokenCounter::default());
        assert!(!text.contains("高频"), "{text}");
        assert_eq!(text.matches("重点").count(), 3, "{text}");
    }

    #[test]
    fn prompt_cache_key_stays_the_same_only_when_the_prefix_matches() {
        let prefix = vec![
            json!({ "role": "user", "content": [{ "type": "text", "text": "# 索引\nREADME.md 重点\n" }] }),
            json!({ "role": "assistant", "content": [{ "type": "text", "text": ACK }] }),
        ];
        let tools = json!([{ "name": "read" }]);
        let key = prompt_cache_key("sys", &tools, &prefix).unwrap();
        assert_eq!(key, prompt_cache_key("sys", &tools, &prefix).unwrap());
        assert_eq!(key.chars().count(), 64);
        let changed = vec![
            json!({ "role": "user", "content": [{ "type": "text", "text": "# 索引\nsrc/new.rs\n" }] }),
            json!({ "role": "assistant", "content": [{ "type": "text", "text": ACK }] }),
        ];
        assert_ne!(prompt_cache_key("sys", &tools, &changed).unwrap(), key);
        assert_ne!(prompt_cache_key("other-sys", &tools, &prefix).unwrap(), key);
        assert_ne!(prompt_cache_key("sys", &json!([{ "name": "edit" }]), &prefix).unwrap(), key);
        assert!(prompt_cache_key("sys", &tools, &[]).is_none());
    }


    #[test]
    fn stale_code_is_marked_and_body_dropped() {
        let dir = tempfile();
        fs::write(dir.join("a.rs"), "fn a() {}\n").unwrap();
        let hash = hash_range("fn a() {}\n", 1, 1);
        let mut snap = Snapshot::new("k");
        snap.code.push(CodeSnippet {
            path: "a.rs".into(),
            start: 1,
            end: 1,
            hash: hash.clone(),
            body: "fn a() {}".into(),
            stale: false,
        });
        snap.verify_code(&dir);
        assert!(!snap.code[0].stale);
        fs::write(dir.join("a.rs"), "fn a() { 1 }\n").unwrap();
        snap.verify_code(&dir);
        assert!(snap.code[0].stale);
        assert!(snap.code[0].body.is_empty());
        assert!(format_snippet(&snap.code[0]).contains(STALE_MARK));
    }

    fn repo_with_remote(url: &str) -> PathBuf {
        let dir = tempfile();
        git(&dir, &["init"]);
        git(&dir, &["remote", "add", "origin", url]);
        dir
    }

    fn tempfile() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nova-mem-{}", uuid()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn uuid() -> String {
        format!("{}-{}", std::process::id(), now_ms())
    }

    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
