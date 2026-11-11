//! 按 git 仓库划分的项目记忆前缀。
//! 开启且模型上下文窗口 ≥ 1M 时，压缩只看见后半段工作区；前半段按项目快照加载，会话中不折叠。
//! 每个项目存一份结构化记忆：冻结区跨会话复用；会话结束后由后台整理把浅层变更追加在冻结区之后，
//! 用得多了再并入冻结区，以保持前缀稳定。

use crate::lyra::history::{now_ms, text_content};
use crate::lyra::tokens::TokenCounter;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
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
- 「近期」是尚未并入长期记忆的浅层补充，用得多了才会写进上面各节。和冻结区冲突时先看近期。
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
pub(crate) const ACK: &str = "已了解。";
pub(crate) const ACK_RECENT: &str = "记下了。";
const HEADING_RECENT: &str = "近期";
const STALE_MARK: &str = "已过时，需重新读取";
/// 浅层条目被强化到这个次数后，才并入冻结区。索引/代码按文件命中累计（一个会话里
/// 叨登几次就快）；笔记类每次会话结束最多 +1，所以至少跨三个会话才晋升。
pub const PROMOTION_USES: u64 = 3;
/// 笔记类候选连续这么多次会话结束都没被用到，才从浅层淘汰。
/// 宁慢不宁快：冻结区前缀要尽量稳定，而没晋升的条目本来就挂在冻结区后面给模型看，
/// 多留几个会话不影响使用。
const NOTE_PATIENCE: u32 = 8;

/// 经验/事实：次数来自整理模型报告的「本会话用到了哪一条」，而不是文件命中。
fn is_note_kind(kind: &str) -> bool {
    matches!(kind, "fact" | "positive" | "negative")
}

/// 条目编号：正文哈希前 6 位十六进制，让整理模型能指代「这一条」。
fn note_id(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.trim().as_bytes());
    format!("{:x}", hasher.finalize())[..6].to_string()
}
/// 淘汰慢了，浅层会多积一些待定条目；上限跟着放宽，别把索引/代码候选挤掉。
const SHALLOW_CAP: usize = 60;
const MAX_INGEST_CHARS: usize = 24_000;

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

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct MemoryInspectMessage {
    pub role: String,
    pub text: String,
}

/// 项目级记忆检查：读已落盘的冻结区和浅层，不是当前会话里已经固定的那份前缀。
#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct ProjectMemoryInspect {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub empty_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<u64>,
    pub frozen: Vec<MemoryInspectMessage>,
    pub shallow: Vec<MemoryInspectMessage>,
    pub shallow_entries: Vec<ShallowEntry>,
}

fn inspect_messages(messages: &[Value]) -> Vec<MemoryInspectMessage> {
    messages
        .iter()
        .map(|message| MemoryInspectMessage {
            role: message
                .get("role")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            text: text_content(&message["content"]),
        })
        .collect()
}

fn inspect_missing(reason: &str, key: Option<String>) -> ProjectMemoryInspect {
    ProjectMemoryInspect {
        empty_reason: Some(reason.to_string()),
        project_key: key,
        ..ProjectMemoryInspect::default()
    }
}

/// 按当前工作目录读取该项目已保存的冻结区和浅层记忆。
pub fn inspect_project(store_dir: &Path, cwd: &Path) -> ProjectMemoryInspect {
    let Some(key) = project_key(cwd) else {
        return inspect_missing("当前目录不是 git 仓库，没有项目记忆。", None);
    };
    inspect_project_key(store_dir, &key)
}

pub fn inspect_project_key(store_dir: &Path, key: &str) -> ProjectMemoryInspect {
    let Some(snap) = Snapshot::load(&snapshot_path(store_dir, key)) else {
        return inspect_missing("这个项目还没有记忆。", Some(key.to_string()));
    };
    ProjectMemoryInspect {
        empty_reason: None,
        project_key: Some(snap.project_key.clone()),
        updated_at: Some(snap.updated_at),
        frozen: inspect_messages(&snap.frozen_messages()),
        shallow: inspect_messages(&snap.shallow_messages()),
        shallow_entries: snap.shallow,
    }
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

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
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

/// 浅层记忆：会话结束后追加，用得多了再并入冻结区。
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct ShallowEntry {
    /// index / fact / positive / negative / code
    pub kind: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub path: String,
    pub uses: u64,
    /// 连续多少次会话结束没被用到；笔记类候选到期就淘汰。
    #[serde(default)]
    pub missed: u32,
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
    /// 已发布的冻结区正文（不含浅层）。新会话原样复用，不重新渲染。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rendered: Option<String>,
    /// 浅层记忆，追加在冻结区之后。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shallow: Vec<ShallowEntry>,
    /// 尚未进入索引的路径命中次数。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub pending_hits: BTreeMap<String, u64>,
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
            return;
        }
        *self.pending_hits.entry(path.to_string()).or_insert(0) += 1;
    }

    pub fn total_hits(&self, path: &str) -> u64 {
        self.index
            .iter()
            .find(|e| e.path == path)
            .map(|e| e.hits)
            .or_else(|| self.pending_hits.get(path).copied())
            .unwrap_or(0)
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

    /// 若还没有发布过冻结区，用当前结构渲染并保存正文。
    pub fn ensure_frozen(&mut self, prose_budget: u64, code_budget: u64, counter: &TokenCounter) {
        if self.rendered.is_none() {
            self.publish_frozen(prose_budget, code_budget, counter);
        }
    }

    pub fn publish_frozen(&mut self, prose_budget: u64, code_budget: u64, counter: &TokenCounter) {
        let text = self.render_text(prose_budget, code_budget, counter);
        self.rendered = if text.is_empty() { None } else { Some(text) };
    }

    /// 跨会话复用的冻结区消息（用户正文 + 短确认）。
    pub fn frozen_messages(&self) -> Vec<Value> {
        let Some(text) = self.rendered.as_deref().filter(|t| !t.is_empty()) else {
            return Vec::new();
        };
        vec![
            json!({ "role": "user", "content": [{ "type": "text", "text": text }] }),
            json!({ "role": "assistant", "content": [{ "type": "text", "text": ACK }] }),
        ]
    }

    /// 浅层记忆消息，跟在冻结区后面；没有浅层时为空。
    pub fn shallow_messages(&self) -> Vec<Value> {
        let text = render_shallow(&self.shallow);
        if text.is_empty() {
            return Vec::new();
        }
        vec![
            json!({ "role": "user", "content": [{ "type": "text", "text": text }] }),
            json!({ "role": "assistant", "content": [{ "type": "text", "text": ACK_RECENT }] }),
        ]
    }

    /// 新会话实际挂上的前缀：冻结区 + 浅层。
    pub fn prefix_messages_published(
        &mut self,
        prose_budget: u64,
        code_budget: u64,
        counter: &TokenCounter,
    ) -> Vec<Value> {
        self.ensure_frozen(prose_budget, code_budget, counter);
        let mut out = self.frozen_messages();
        out.extend(self.shallow_messages());
        out
    }

    /// 将会话结束时的整理结果与命中写入浅层；达到次数的条目并入冻结区。
    /// 有实际变化才返回 true。不会为了磁盘上未使用的新文件而重做索引。
    pub fn apply_session_end(
        &mut self,
        distill: &str,
        session_hits: &BTreeMap<String, u64>,
        cwd: &Path,
        prose_budget: u64,
        code_budget: u64,
        counter: &TokenCounter,
    ) -> bool {
        self.ensure_frozen(prose_budget, code_budget, counter);
        let before_frozen = self.rendered.clone();
        let before_shallow = self.shallow.clone();

        for (path, hits) in session_hits {
            if *hits == 0 {
                continue;
            }
            let mut touched = false;
            for entry in self
                .shallow
                .iter_mut()
                .filter(|e| e.path == *path && e.kind != "code")
            {
                entry.uses = entry.uses.saturating_add(1);
                touched = true;
            }
            if !touched {
                if !self.index.iter().any(|e| e.path == *path) {
                    self.shallow.push(ShallowEntry {
                        kind: "index".into(),
                        text: format!("索引 新增 {path}"),
                        path: path.clone(),
                        uses: 1,
                        missed: 0,
                    });
                } else if *hits >= 2 {
                    self.shallow.push(ShallowEntry {
                        kind: "index".into(),
                        text: format!("索引 {path} 多次使用"),
                        path: path.clone(),
                        uses: 1,
                        missed: 0,
                    });
                }
            }
            // 读过又改过的文件机械地进代码候选：整理模型看不到工具调用，不能指望它指路。
            if *hits >= 2 && !self.code.iter().any(|s| s.path == *path) {
                self.reinforce(ShallowEntry {
                    kind: "code".into(),
                    text: format!("代码 新增 {path}"),
                    path: path.clone(),
                    uses: 1,
                    missed: 0,
                });
            }
        }

        let distilled = parse_distill(distill);
        // 先结算引用：只有本会话真正用到的候选才算一次使用，没被用到的记一次 miss。
        self.age_notes(&distilled.hits);
        for entry in distilled.entries {
            self.reinforce(entry);
        }

        self.trim_shallow();
        let before_code = self.code.clone();
        // 磁盘优于缓存：文件改过就标过时并抽走旧正文，别让冻结区里的旧片段冒充现状。
        self.verify_code(cwd);
        if self.promote(cwd) || self.code != before_code {
            self.publish_frozen(prose_budget, code_budget, counter);
        }
        self.rendered != before_frozen || self.shallow != before_shallow
    }

    fn reinforce(&mut self, incoming: ShallowEntry) {
        if incoming.text.is_empty() && incoming.path.is_empty() {
            return;
        }
        if let Some(existing) = self.shallow.iter_mut().find(|e| shallow_same(e, &incoming)) {
            existing.uses = existing.uses.saturating_add(1);
            // 又被整理出来一次，说明这条还有用：mis 清零，不往淘汰方向走。
            existing.missed = 0;
            if !incoming.text.is_empty() {
                existing.text = incoming.text;
            }
            return;
        }
        let mut entry = incoming;
        entry.uses = entry.uses.max(1);
        self.shallow.push(entry);
    }

    /// 会话结束结算笔记候选：被引用的 uses+1、miss 清零；没被引用的记一次 miss，
    /// 连续 `NOTE_PATIENCE` 次都没用到就淘汰。索引/代码仍按文件命中累计，不在这里增删。
    fn age_notes(&mut self, hits: &[String]) {
        self.shallow.retain_mut(|entry| {
            if !is_note_kind(&entry.kind) {
                return true;
            }
            if hits.iter().any(|id| note_id(&entry.text) == *id) {
                entry.uses = entry.uses.saturating_add(1);
                entry.missed = 0;
                return true;
            }
            entry.missed += 1;
            entry.missed < NOTE_PATIENCE
        });
    }

    /// 待定笔记候选（带编号），交给整理模型报告本会话真正用到了哪几条。
    pub fn candidate_index(&self) -> String {
        let mut out = String::new();
        for entry in self.shallow.iter().filter(|e| is_note_kind(&e.kind)) {
            out.push_str("- [");
            out.push_str(&note_id(&entry.text));
            out.push_str("] ");
            out.push_str(&strip_note_prefix(&entry.text));
            out.push('\n');
        }
        if out.is_empty() {
            out.push_str("- （暂无）\n");
        }
        out
    }

    fn trim_shallow(&mut self) {
        if self.shallow.len() <= SHALLOW_CAP {
            return;
        }
        self.shallow
            .sort_by(|a, b| b.uses.cmp(&a.uses).then_with(|| a.text.cmp(&b.text)));
        self.shallow.truncate(SHALLOW_CAP);
        self.shallow
            .sort_by(|a, b| a.kind.cmp(&b.kind).then_with(|| a.text.cmp(&b.text)));
    }

    fn promote(&mut self, cwd: &Path) -> bool {
        let (keep, ready): (Vec<_>, Vec<_>) = self
            .shallow
            .drain(..)
            .partition(|e| e.uses < PROMOTION_USES);
        self.shallow = keep;
        if ready.is_empty() {
            return false;
        }
        let mut changed = false;
        for entry in ready {
            if self.promote_one(cwd, &entry) {
                changed = true;
            } else if entry.kind == "code" {
                // 只有代码片段会因文件暂时读不到而失败；留着等下次，别把累计的 uses 丢掉。
                self.shallow.push(entry);
            }
        }
        changed
    }

    fn promote_one(&mut self, cwd: &Path, entry: &ShallowEntry) -> bool {
        match entry.kind.as_str() {
            "index" => {
                if entry.path.is_empty() {
                    return false;
                }
                if let Some(item) = self.index.iter_mut().find(|e| e.path == entry.path) {
                    if item.hits < 2 {
                        item.hits = 2;
                    }
                    true
                } else {
                    let hits = self.pending_hits.remove(&entry.path).unwrap_or(1).max(1);
                    self.index.push(IndexEntry {
                        path: entry.path.clone(),
                        role: default_role(&entry.path),
                        hits,
                    });
                    self.index.sort_by(|a, b| a.path.cmp(&b.path));
                    true
                }
            }
            "fact" => push_note(&mut self.facts, &entry.text, &entry.path),
            "positive" => push_note(&mut self.positive, &entry.text, &entry.path),
            "negative" => push_note(&mut self.negative, &entry.text, &entry.path),
            "code" => self.promote_code(cwd, entry),
            _ => false,
        }
    }

    fn promote_code(&mut self, cwd: &Path, entry: &ShallowEntry) -> bool {
        if entry.path.is_empty() {
            return true;
        }
        if entry.text.contains("已过时") {
            for snippet in &mut self.code {
                if snippet.path == entry.path {
                    verify_one(snippet, cwd);
                }
            }
            return true;
        }
        if self.code.iter().any(|s| s.path == entry.path) {
            // 已经收过：条目完成使命，不能留在浅层反复重试。
            return true;
        }
        ingest_code(self, cwd, &entry.path)
    }
}

fn push_note(notes: &mut Vec<Note>, text: &str, path: &str) -> bool {
    let text = strip_note_prefix(text);
    if text.is_empty() {
        return false;
    }
    if notes.iter().any(|n| n.text == text) {
        return false;
    }
    notes.push(Note {
        text,
        evidence: path.to_string(),
    });
    true
}

fn strip_note_prefix(text: &str) -> String {
    let text = text.trim();
    for prefix in ["事实 增加 ", "有效经验 增加 ", "失败经验 增加 "] {
        if let Some(rest) = text.strip_prefix(prefix) {
            return rest.trim().to_string();
        }
    }
    text.to_string()
}

fn shallow_same(a: &ShallowEntry, b: &ShallowEntry) -> bool {
    a.kind == b.kind
        && if a.path.is_empty() && b.path.is_empty() {
            a.text == b.text
        } else {
            !a.path.is_empty() && a.path == b.path
        }
}

fn render_shallow(entries: &[ShallowEntry]) -> String {
    if entries.is_empty() {
        return String::new();
    }
    let mut body = String::from("<project-memory-recent>\n");
    body.push_str("# ");
    body.push_str(HEADING_RECENT);
    body.push('\n');
    for entry in entries {
        let text = entry.text.trim();
        if text.is_empty() {
            continue;
        }
        body.push_str("- ");
        body.push_str(text);
        body.push('\n');
    }
    body.push_str("</project-memory-recent>");
    body
}

/// 整理提示：让后台 agent 只输出可解析的浅层条目，并报出这次真正用到的候选编号。
pub fn distill_prompt(memory_outline: &str, candidates: &str, transcript: &str) -> String {
    format!(
        "你在整理项目的长期记忆。这不是对用户的回复，用户看不见这次整理。\n\n\
下面是冻结区（长期记忆）和近期浅层记忆，以及本会话工作区摘录。\n\
只输出需要追加到浅层记忆的条目，一行一条，不要解释，不要编号。\n\
没有值得记下的就只输出：无\n\n\
格式：\n\
索引 新增 <路径> <简短职责>\n\
索引 <路径> 更新 <简短说明>\n\
事实 增加 <一句>\n\
有效经验 增加 <一句>\n\
失败经验 增加 <一句>\n\
代码 新增 <路径>\n\
代码 <路径> 已过时\n\
用到 <下面候选里的编号>\n\n\
只记有实际意义的内容：新的事实、可复用的经验、用过且值得补进索引或代码区的文件。不要重复冻结区里已有的内容。不要编造。\n\
「用到」只写给本会话真的依赖了的那几条候选，没用上就不写。\n\n\
当前记忆：\n{memory_outline}\n\n\
待定候选：\n{candidates}\n\
本会话摘录：\n{transcript}\n"
    )
}

/// 整理输出：新增/更新的浅层条目，以及本会话真正用到的候选编号。
#[derive(Debug, Default)]
pub struct Distilled {
    pub entries: Vec<ShallowEntry>,
    pub hits: Vec<String>,
}

/// 解析整理输出。无法识别或只有「无」时返回空。
pub fn parse_distill(text: &str) -> Distilled {
    let text = text.trim();
    if text.is_empty() || matches!(text, "无" | "没有" | "无。" | "无更新") {
        return Distilled::default();
    }
    let mut distilled = Distilled::default();
    for raw in text.lines().flat_map(|line| line.split(['，', ';', '；'])) {
        let line = raw.trim().trim_start_matches(['-', '*', '•', '、']).trim();
        // 「用到 <编号>」：本会话真的依赖了这条已有的候选。
        if let Some(rest) = line.strip_prefix("用到") {
            let id = rest.trim().trim_start_matches(['：', ':', '[']).trim_end_matches(']');
            if let Some(id) = id.split_whitespace().next() {
                let id = id.trim_end_matches('。').to_ascii_lowercase();
                if !id.is_empty() {
                    distilled.hits.push(id);
                }
            }
            continue;
        }
        if let Some(entry) = parse_distill_line(raw) {
            distilled.entries.push(entry);
        }
    }
    distilled
}

fn parse_distill_line(line: &str) -> Option<ShallowEntry> {
    let line = line.trim().trim_start_matches(['-', '*', '•', '、']).trim();
    let mut line = line.trim();
    while let Some(rest) = line.strip_prefix(|c: char| c.is_ascii_digit()) {
        line = rest.trim_start_matches(['.', '、', ')', '）', ' ']).trim();
    }
    if line.is_empty() || line == "无" {
        return None;
    }
    let (kind, rest) = if let Some(r) = line.strip_prefix("有效经验") {
        ("positive", r)
    } else if let Some(r) = line.strip_prefix("失败经验") {
        ("negative", r)
    } else if let Some(r) = line.strip_prefix("事实") {
        ("fact", r)
    } else if let Some(r) = line.strip_prefix("代码") {
        ("code", r)
    } else if let Some(r) = line.strip_prefix("索引") {
        ("index", r)
    } else {
        return None;
    };
    let rest = rest.trim().trim_start_matches(['：', ':', ' ']).trim();
    let rest = rest
        .trim_start_matches("增加")
        .trim_start_matches("新增")
        .trim_start_matches("更新")
        .trim();
    if rest.is_empty() {
        return None;
    }
    let path = distill_path(kind, rest);
    let text = match kind {
        "index" if rest.contains("多次使用") => format!("索引 {path} 多次使用"),
        "index" if line.contains("新增") => {
            if path.is_empty() {
                format!("索引 新增 {rest}")
            } else {
                format!("索引 新增 {path}")
            }
        }
        "index" => {
            if path.is_empty() {
                format!("索引 {rest}")
            } else {
                format!("索引 {path} 更新")
            }
        }
        "fact" => format!("事实 增加 {rest}"),
        "positive" => format!("有效经验 增加 {rest}"),
        "negative" => format!("失败经验 增加 {rest}"),
        "code" if rest.contains("已过时") || line.contains("已过时") => {
            format!("代码 {path} 已过时，需重新读取")
        }
        "code" => format!("代码 新增 {path}"),
        _ => rest.to_string(),
    };
    Some(ShallowEntry {
        kind: kind.to_string(),
        text,
        path,
        uses: 1,
        missed: 0,
    })
}

fn distill_path(kind: &str, rest: &str) -> String {
    if kind != "index" && kind != "code" {
        return String::new();
    }
    rest.split_whitespace()
        .find(|tok| tok.contains('/') || tok.contains('.'))
        .or_else(|| rest.split_whitespace().next())
        .unwrap_or("")
        .trim_matches(['《', '》', '`', '"', '“', '”', '。', ',', '，'])
        .to_string()
}

fn verify_one(snippet: &mut CodeSnippet, cwd: &Path) {
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

fn ingest_code(snap: &mut Snapshot, cwd: &Path, path: &str) -> bool {
    let file = cwd.join(path);
    let Ok(text) = std::fs::read_to_string(&file) else {
        return false;
    };
    if text.contains('\u{0}') {
        return false;
    }
    // shortcut: 大文件只收开头 MAX_INGEST_CHARS 个字符以内的整行，够看清接口；要整份时再按窗口续收。
    let mut body = String::new();
    let mut end = 0u32;
    for line in text.lines() {
        if end > 0 && body.chars().count() + line.chars().count() + 1 > MAX_INGEST_CHARS {
            break;
        }
        if end > 0 {
            body.push('\n');
        }
        body.push_str(line);
        end += 1;
    }
    if end == 0 {
        return false;
    }
    snap.code.push(CodeSnippet {
        path: path.to_string(),
        start: 1,
        end,
        hash: hash_range(&text, 1, end),
        body,
        stale: false,
    });
    snap.code
        .sort_by(|a, b| a.path.cmp(&b.path).then(a.start.cmp(&b.start)));
    true
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
    use std::collections::BTreeMap;
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
        assert_ne!(
            prompt_cache_key("sys", &json!([{ "name": "edit" }]), &prefix).unwrap(),
            key
        );
        assert!(prompt_cache_key("sys", &tools, &[]).is_none());
    }

    #[test]
    fn file_touched_twice_in_a_session_reaches_the_code_section() {
        let dir = tempfile();
        fs::write(dir.join("a.rs"), "fn a() {}\n").unwrap();
        let counter = TokenCounter::default();
        let mut snap = Snapshot::new("k");
        snap.index.push(IndexEntry {
            path: "a.rs".into(),
            role: String::new(),
            hits: 0,
        });
        snap.ensure_frozen(10_000, 200, &counter);
        let mut hits = BTreeMap::new();
        hits.insert("a.rs".to_string(), 2);
        for _ in 0..PROMOTION_USES {
            snap.apply_session_end("", &hits, &dir, 10_000, 200, &counter);
        }
        assert!(
            snap.code
                .iter()
                .any(|s| s.path == "a.rs" && !s.stale && s.body.contains("fn a")),
            "{:?}",
            snap.code
        );
        let frozen = snap.frozen_messages();
        assert!(frozen[0]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("fn a() {}"));
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

    #[test]
    fn parse_distill_reads_plain_lines_and_ignores_none() {
        assert!(parse_distill("无").entries.is_empty());
        assert!(parse_distill("").entries.is_empty());
        let entries = parse_distill(
            "索引 新增 src/util.rs 工具函数\n事实 增加 默认压缩是 ACP\n有效经验 增加 先跑测试再改接口\n代码 src/a.rs 已过时",
        )
        .entries;
        assert_eq!(entries.len(), 4);
        assert_eq!(entries[0].kind, "index");
        assert_eq!(entries[0].path, "src/util.rs");
        assert!(entries[0].text.contains("索引 新增 src/util.rs"));
        assert_eq!(entries[1].kind, "fact");
        assert_eq!(entries[2].kind, "positive");
        assert_eq!(entries[3].kind, "code");
        assert!(entries[3].text.contains("已过时"));
    }

    #[test]
    fn parse_distill_reads_used_candidate_ids() {
        let distilled = parse_distill("- 用到 [a1b2c3]\n用到：D4E5F6 关键\n事实 增加 新的一句");
        assert_eq!(distilled.hits, vec!["a1b2c3".to_string(), "d4e5f6".to_string()]);
        assert_eq!(distilled.entries.len(), 1);
        assert_eq!(distilled.entries[0].kind, "fact");
    }

    #[test]
    fn new_session_reuses_frozen_prefix_until_shallow_promotes() {
        let dir = tempfile();
        fs::write(dir.join("a.rs"), "fn a() {}\n").unwrap();
        let mut snap = Snapshot::new("k");
        snap.index.push(IndexEntry {
            path: "a.rs".into(),
            role: "入口".into(),
            hits: 0,
        });
        let counter = TokenCounter::default();
        snap.ensure_frozen(10_000, 200, &counter);
        let frozen = snap.frozen_messages();
        assert_eq!(frozen[1]["content"][0]["text"], ACK);
        assert!(snap.shallow.is_empty());

        let mut hits = BTreeMap::new();
        hits.insert("b.rs".into(), 1);
        assert!(snap.apply_session_end(
            "事实 增加 只在有意义时更新",
            &hits,
            &dir,
            10_000,
            200,
            &counter
        ));
        // 整理出来还不够：先挂在浅层，冻结区不动。
        assert!(snap.facts.is_empty(), "{:?}", snap.facts);
        assert_eq!(snap.shallow.len(), 2);
        assert_eq!(
            snap.frozen_messages(),
            frozen,
            "shallow append must not rewrite the frozen zone"
        );

        // 整理模型再连续两次报「用到这一条」，累计三次才并进冻结区。
        let line = snap
            .candidate_index()
            .lines()
            .find(|line| line.contains("只在有意义时更新"))
            .expect("candidate listed")
            .to_string();
        let id = line[3..line.find(']').unwrap()].to_string();
        for _ in 0..2 {
            snap.apply_session_end(
                &format!("用到 {id}"),
                &BTreeMap::new(),
                &dir,
                10_000,
                200,
                &counter,
            );
        }
        assert!(
            snap.facts.iter().any(|n| n.text == "只在有意义时更新"),
            "{:?}",
            snap.facts
        );
        assert_ne!(snap.frozen_messages(), frozen);
        assert!(snap.shallow.iter().all(|e| e.kind != "fact"));

        // 索引仍按文件命中累计：再命中两次才晋升。
        assert!(snap.shallow.iter().any(|e| e.path == "b.rs"));
        assert!(!snap.index.iter().any(|e| e.path == "b.rs"));
        for _ in 0..2 {
            snap.apply_session_end(
                "索引 新增 b.rs",
                &BTreeMap::new(),
                &dir,
                10_000,
                200,
                &counter,
            );
        }
        assert!(
            snap.index.iter().any(|e| e.path == "b.rs"),
            "promoted index entry"
        );
        assert!(snap.shallow.iter().all(|e| e.path != "b.rs" && e.kind != "fact"));
    }

    #[test]
    fn uncited_note_candidates_are_dropped_after_patience() {
        let dir = tempfile();
        let mut snap = Snapshot::new("k");
        let counter = TokenCounter::default();
        for index in 0..=NOTE_PATIENCE {
            snap.apply_session_end(
                &format!("事实 增加 结论 {index}"),
                &BTreeMap::new(),
                &dir,
                10_000,
                200,
                &counter,
            );
        }
        // 最早那条连续 NOTE_PATIENCE 次没被用到，已淘汰；刚整理出来的还在。
        assert!(
            snap.shallow.iter().all(|e| !e.text.contains("结论 0")),
            "{:?}",
            snap.shallow
        );
        assert!(snap
            .shallow
            .iter()
            .any(|e| e.text.contains(&format!("结论 {NOTE_PATIENCE}"))));
        assert!(snap.facts.is_empty());
    }

    #[test]
    fn inspect_project_shows_frozen_and_shallow() {
        let store = tempfile();
        let repo = repo_with_remote("https://github.com/llyb120/nova-client.git");
        let key = project_key(&repo).unwrap();
        let mut snap = Snapshot::new(&key);
        snap.rendered = Some("<project-memory>\n# 索引\n</project-memory>".into());
        snap.shallow.push(ShallowEntry {
            kind: "fact".into(),
            text: "只在有意义时更新".into(),
            path: String::new(),
            uses: 1,
            missed: 0,
        });
        snap.save(&snapshot_path(&store, &key));

        let inspect = inspect_project(&store, &repo);
        assert_eq!(inspect.empty_reason, None);
        assert_eq!(
            inspect.project_key.as_deref(),
            Some("github.com/llyb120/nova-client")
        );
        assert!(inspect.frozen[0].text.contains("<project-memory>"));
        assert_eq!(inspect.frozen[1].text, ACK);
        assert!(inspect.shallow[0].text.contains("只在有意义时更新"));
        assert_eq!(inspect.shallow[1].text, ACK_RECENT);
        assert_eq!(inspect.shallow_entries[0].uses, 1);

        let missing = inspect_project(&tempfile(), &repo);
        assert_eq!(
            missing.empty_reason.as_deref(),
            Some("这个项目还没有记忆。")
        );
        assert_eq!(
            missing.project_key.as_deref(),
            Some("github.com/llyb120/nova-client")
        );
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
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        format!(
            "{}-{}-{}",
            std::process::id(),
            now_ms(),
            N.fetch_add(1, Ordering::Relaxed)
        )
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
