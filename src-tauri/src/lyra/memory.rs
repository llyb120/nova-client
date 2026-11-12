//! 按 git 仓库划分的项目记忆前缀。
//! 开启且模型上下文窗口 ≥ 1M 时，压缩只看见后半段工作区；前半段按项目快照加载，会话中不折叠。
//! 每个项目存一份结构化记忆：冻结区跨会话复用；会话结束后由后台整理把浅层变更按添加顺序追加在冻结区之后，
//! 用得多了再并入冻结区（空间宽裕时也只追加、不改已发布正文），以保持前缀缓存。

use crate::lyra::history::{now_ms, text_content};
use crate::lyra::tokens::TokenCounter;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, OnceLock};

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
- 「近期」是尚未并入长期记忆的浅层补充，按添加顺序排在冻结区之后，代码含正文，不必按索引、经验、事实、代码分节。空间宽裕时只追加、不改已发布正文，用过三次才并入长期记忆（同样只追加在冻结区后面）；占用高或浅层条目满了才重新整理。和冻结区冲突时先看近期。
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
const STALE_MARK: &str = "已过时，需重新读取";
/// 空间宽裕时，浅层条目跨会话累计到这个次数后才并入冻结区。
/// 一次会话里无论读写多少次，同一条最多只记一次使用。
pub const PROMOTION_USES: u64 = 3;
/// 冻结区某段占用达到这个比例，视为空间不够：晋升和淘汰改走相对比例。
const TIGHT_FILL: f64 = 0.80;
/// 空间不够时，淘汰使用次数最低的这一档，晋升只收最高的这一档（约两成）。
const COLD_RATIO: f64 = 0.20;

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
/// 浅层空间满了才遗忘用得少的条目；上限略宽，避免刚记下的内容被挤掉。
const SHALLOW_CAP: usize = 60;
const MAX_INGEST_CHARS: usize = 24_000;
const SHALLOW_OPEN: &str = "<project-memory-recent>\n";
const SHALLOW_CLOSE: &str = "</project-memory-recent>";
const FROZEN_ADD_OPEN: &str = "<project-memory-add>\n";
const FROZEN_ADD_CLOSE: &str = "</project-memory-add>";

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

/// Windows 下不弹出控制台窗口：每跑一次 git 都会闪一个黑框。
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn git(cwd: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new("git");
    cmd.args(args).current_dir(cwd);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

fn git_stdout(cwd: &Path, args: &[&str]) -> Option<String> {
    let out = git(cwd, args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

/// 一个 cwd 的仓库身份。git 子进程很贵（记忆面板每刷新一次、读文件工具每命中一次都要问一遍），
/// 而一次运行里 cwd 不会换仓库，所以算一次就缓存；会话中途 git init / 改 remote 要重启才认。
#[derive(Clone, Default)]
struct RepoInfo {
    /// 项目键：远程地址归一化，没有远程时用 common dir。
    key: Option<String>,
    /// 仓库根，索引用的仓库相对路径从它算。
    root: Option<PathBuf>,
}

static REPO_CACHE: LazyLock<Mutex<HashMap<PathBuf, RepoInfo>>> = LazyLock::new(Default::default);

fn repo_cache() -> MutexGuard<'static, HashMap<PathBuf, RepoInfo>> {
    REPO_CACHE.lock().unwrap_or_else(|e| e.into_inner())
}

fn repo_info(cwd: &Path) -> RepoInfo {
    if let Some(cached) = repo_cache().get(cwd).cloned() {
        return cached;
    }
    let info = probe_repo(cwd);
    repo_cache().insert(cwd.to_path_buf(), info.clone());
    info
}

/// 一次 git 调用同时问「在不在仓库里」和「仓库根」，不分开起两次进程。
fn probe_repo(cwd: &Path) -> RepoInfo {
    let Some(out) = git_stdout(
        cwd,
        &["rev-parse", "--is-inside-work-tree", "--show-toplevel"],
    ) else {
        return RepoInfo::default();
    };
    let mut lines = out.lines();
    if lines.next().map(str::trim) != Some("true") {
        return RepoInfo::default();
    }
    RepoInfo {
        key: repo_key(cwd),
        root: lines.next().map(|line| PathBuf::from(line.trim())),
    }
}

/// 以 git 远程路径作为项目键；没有远程时退回到该仓库的 common dir（同一 worktree 家族共用）。
fn repo_key(cwd: &Path) -> Option<String> {
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

pub fn project_key(cwd: &Path) -> Option<String> {
    repo_info(cwd).key
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
    let Some(mut snap) = Snapshot::load(&snapshot_path(store_dir, &key)) else {
        return inspect_missing("这个项目还没有记忆。", Some(key.to_string()));
    };
    snap.hydrate_shallow_code(cwd);
    ProjectMemoryInspect {
        empty_reason: None,
        project_key: Some(snap.project_key.clone()),
        updated_at: Some(snap.updated_at),
        frozen: inspect_messages(&snap.frozen_messages()),
        shallow: inspect_messages(&snap.shallow_messages()),
        shallow_entries: snap.shallow,
    }
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
    /// 并入后继续累计的使用次数，空间不够时按它做比例淘汰。
    #[serde(default, skip_serializing_if = "is_zero_hits")]
    pub hits: u64,
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
    /// 并入后继续累计的使用次数，空间不够时按它做比例淘汰。
    #[serde(default, skip_serializing_if = "is_zero_hits")]
    pub hits: u64,
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
    /// 代码候选的正文，结构和冻结区片段相同；其它种类保持默认空值。
    #[serde(default, skip_serializing_if = "is_zero_line")]
    pub start: u32,
    #[serde(default, skip_serializing_if = "is_zero_line")]
    pub end: u32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub hash: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub body: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
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
    /// 已发布的冻结区正文（不含浅层）。新会话原样复用，不重新渲染。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rendered: Option<String>,
    /// 空间宽裕时晋升进来的内容，追加在冻结区正文之后，不改已发布前缀。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub rendered_appends: String,
    /// 已发布的浅层正文。空间宽裕时只在末尾追加，不按分节重排。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rendered_shallow: Option<String>,
    /// 浅层记忆，追加在冻结区之后。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shallow: Vec<ShallowEntry>,
    /// 尚未进入索引的路径命中次数。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub pending_hits: BTreeMap<String, u64>,
}

static SNAPSHOT_WRITE_LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();

/// 同一快照的写入排队执行：后来的会等前面的写完再写，不会被拒绝。
pub fn with_snapshot_write<T>(path: &Path, f: impl FnOnce() -> T) -> T {
    let lock = {
        let mut map = SNAPSHOT_WRITE_LOCKS
            .get_or_init(Default::default)
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        map.entry(path.to_path_buf())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    };
    let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
    f()
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

    /// 核对磁盘：对不上哈希的片段标过时并去掉正文，已删文件只留短标记。
    /// 只给检查和测试用；会话结束时不会用它直接改冻结区。
    pub fn verify_code(&mut self, cwd: &Path) {
        for snippet in &mut self.code {
            verify_one(snippet, cwd);
        }
        self.code
            .sort_by(|a, b| a.path.cmp(&b.path).then(a.start.cmp(&b.start)));
    }

    pub fn record_hit(&mut self, path: &str) {
        // 冻结区的 hits 只在浅层条目晋升时更新，会话内的读写只记在 pending 里。
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
        self.rendered_appends.clear();
    }

    /// 跨会话复用的冻结区消息（用户正文 + 短确认）。
    /// 空间宽裕时后续晋升另成一条追加消息，不改已经发布的第一条正文。
    pub fn frozen_messages(&self) -> Vec<Value> {
        let Some(text) = self.rendered.as_deref().filter(|t| !t.is_empty()) else {
            return Vec::new();
        };
        let mut out = vec![
            json!({ "role": "user", "content": [{ "type": "text", "text": text }] }),
            json!({ "role": "assistant", "content": [{ "type": "text", "text": ACK }] }),
        ];
        if !self.rendered_appends.is_empty() {
            out.push(json!({ "role": "user", "content": [{ "type": "text", "text": self.rendered_appends }] }));
            out.push(json!({ "role": "assistant", "content": [{ "type": "text", "text": ACK }] }));
        }
        out
    }

    /// 浅层记忆消息，跟在冻结区后面；没有浅层时为空。
    /// 检查面板用完整渲染；挂到前缀时走 published_shallow_messages，按冻结区剩余额度裁剪。
    pub fn shallow_messages(&self) -> Vec<Value> {
        self.shallow_messages_budgeted(u64::MAX, u64::MAX, &TokenCounter::default())
    }

    pub fn shallow_messages_budgeted(
        &self,
        prose_budget: u64,
        code_budget: u64,
        counter: &TokenCounter,
    ) -> Vec<Value> {
        let text = render_shallow(&self.shallow, prose_budget, code_budget, counter);
        shallow_user_messages(&text)
    }

    /// 前缀里的浅层：只用冻结区还没用完的记述/代码额度，避免和冻结区叠满两份。
    pub fn published_shallow_messages(
        &self,
        prose_budget: u64,
        code_budget: u64,
        counter: &TokenCounter,
    ) -> Vec<Value> {
        let used_code = counter.count_text(&render_code(&self.code, code_budget, counter));
        let used_published = self
            .rendered
            .as_deref()
            .map(|text| counter.count_text(text))
            .unwrap_or(0)
            .saturating_add(counter.count_text(&self.rendered_appends));
        let used_prose = used_published.saturating_sub(used_code);
        if let Some(text) = self.rendered_shallow.as_deref().filter(|t| !t.is_empty()) {
            return shallow_user_messages(text);
        }
        self.shallow_messages_budgeted(
            prose_budget.saturating_sub(used_prose),
            code_budget.saturating_sub(used_code),
            counter,
        )
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
        out.extend(self.published_shallow_messages(prose_budget, code_budget, counter));
        out
    }

    /// 将会话结束时的整理结果与命中写入浅层；达到次数的条目并入冻结区。
    /// 有实际变化才返回 true。不会为了磁盘上未使用的新文件而重做索引。
    /// 一次会话里同一条最多 +1 次使用；纠错和文件更新也先留在浅层，不立刻改冻结区。
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
        self.hydrate_shallow_code(cwd);
        let before_frozen = self.rendered.clone();
        let before_appends = self.rendered_appends.clone();
        let before_rendered_shallow = self.rendered_shallow.clone();
        let before_shallow = self.shallow.clone();
        let mut bumped = HashSet::new();

        let distilled = parse_distill(distill);
        for entry in distilled.corrections {
            self.apply_shallow_correction(entry, &mut bumped, cwd);
        }

        for (path, hits) in session_hits {
            if *hits == 0 {
                continue;
            }
            if !self.index.iter().any(|e| e.path == *path) {
                self.upsert_and_touch(
                    ShallowEntry {
                        kind: "index".into(),
                        text: format!("索引 新增 {path}"),
                        path: path.clone(),
                        uses: 1,
                        missed: 0,
                        ..Default::default()
                    },
                    &mut bumped,
                    cwd,
                );
            } else {
                self.upsert_and_touch(
                    ShallowEntry {
                        kind: "index".into(),
                        text: format!("索引 {path} 多次使用"),
                        path: path.clone(),
                        uses: 1,
                        missed: 0,
                        ..Default::default()
                    },
                    &mut bumped,
                    cwd,
                );
            }
            if self.code.iter().any(|s| s.path == *path) {
                if code_changed_on_disk(&self.code, cwd, path) {
                    self.upsert_and_touch(
                        ShallowEntry {
                            kind: "code".into(),
                            text: format!("代码 {path} 已过时，需重新读取"),
                            path: path.clone(),
                            uses: 1,
                            missed: 0,
                            ..Default::default()
                        },
                        &mut bumped,
                        cwd,
                    );
                }
            } else {
                self.upsert_and_touch(
                    ShallowEntry {
                        kind: "code".into(),
                        text: format!("代码 新增 {path}"),
                        path: path.clone(),
                        uses: 1,
                        missed: 0,
                        ..Default::default()
                    },
                    &mut bumped,
                    cwd,
                );
            }
        }

        for id in &distilled.hits {
            if let Some(entry) = self
                .shallow
                .iter_mut()
                .find(|e| is_note_kind(&e.kind) && note_id(&e.text) == *id)
            {
                let key = entry_key(entry);
                if bumped.insert(key) {
                    entry.uses = entry.uses.saturating_add(1);
                }
                entry.missed = 0;
            }
        }

        for entry in distilled.entries {
            self.upsert_and_touch(entry, &mut bumped, cwd);
        }

        for entry in &mut self.shallow {
            if !bumped.contains(&entry_key(entry)) {
                entry.missed = entry.missed.saturating_add(1);
            }
        }

        self.bump_frozen_usage(session_hits, &distilled.hits);
        let over_cap = self.shallow.len() > SHALLOW_CAP;
        self.trim_shallow();
        let (used_prose, used_code) = self.frozen_used(prose_budget, code_budget, counter);
        let tight = (
            tight_fill(used_prose, prose_budget),
            tight_fill(used_code, code_budget),
        );
        let evicted = self.evict_cold_when_tight(prose_budget, code_budget, counter);
        let promoted = self.promote(cwd, prose_budget, code_budget, counter, tight);
        let reorganize = evicted || tight.0 || tight.1 || over_cap;
        if reorganize {
            self.publish_frozen(prose_budget, code_budget, counter);
            self.rebuild_rendered_shallow();
        } else {
            self.append_frozen_published(&promoted);
            self.sync_rendered_shallow(&before_shallow);
        }
        self.rendered != before_frozen
            || self.rendered_appends != before_appends
            || self.rendered_shallow != before_rendered_shallow
            || self.shallow != before_shallow
    }

    fn upsert_and_touch(
        &mut self,
        mut incoming: ShallowEntry,
        bumped: &mut HashSet<String>,
        cwd: &Path,
    ) {
        if incoming.kind == "code" {
            attach_code_body(&mut incoming, cwd);
        }
        if incoming.text.is_empty() && incoming.path.is_empty() {
            return;
        }
        let key = entry_key(&incoming);
        if let Some(existing) = self.shallow.iter_mut().find(|e| shallow_same(e, &incoming)) {
            if bumped.insert(key) {
                existing.uses = existing.uses.saturating_add(1);
            }
            existing.missed = 0;
            // 已有条目只更新次数，不改已发布正文，免得打断前缀缓存。
            // 代码正文若还是空的（旧快照），允许补上一次；已有正文等整理时再换。
            if incoming.kind == "code" && existing.body.is_empty() && !incoming.body.is_empty() {
                existing.start = incoming.start;
                existing.end = incoming.end;
                existing.hash = incoming.hash.clone();
                existing.body = incoming.body.clone();
                existing.stale = incoming.stale;
            }
            return;
        }
        let mut entry = incoming;
        entry.uses = 1;
        entry.missed = 0;
        bumped.insert(key);
        self.shallow.push(entry);
    }

    /// 给已有代码候选补上正文，让旧快照和检查面板也能看到代码，而不只是路径。
    pub fn hydrate_shallow_code(&mut self, cwd: &Path) -> bool {
        let mut changed = false;
        for entry in &mut self.shallow {
            if entry.kind != "code" || entry.path.is_empty() || delete_target(entry).is_some() {
                continue;
            }
            if !entry.body.is_empty() && !entry.stale && !entry.text.contains("已过时") {
                continue;
            }
            let before = (
                entry.body.clone(),
                entry.hash.clone(),
                entry.start,
                entry.end,
                entry.stale,
            );
            attach_code_body(entry, cwd);
            if (
                entry.body.clone(),
                entry.hash.clone(),
                entry.start,
                entry.end,
                entry.stale,
            ) != before
            {
                changed = true;
            }
        }
        changed
    }

    /// 纠错先落在浅层：冻结区里的旧内容保持不动，以免打断前缀缓存。
    /// 只存在于浅层的记错内容可以当场改掉或去掉，不必等空间满。
    fn apply_shallow_correction(
        &mut self,
        incoming: ShallowEntry,
        bumped: &mut HashSet<String>,
        cwd: &Path,
    ) {
        if let Some(target) = delete_target(&incoming) {
            let in_frozen = self.frozen_has(&incoming.kind, &target);
            self.shallow.retain(|e| {
                e.kind != incoming.kind || is_correction_entry(e) || !entry_targets(e, &target)
            });
            if in_frozen {
                self.upsert_and_touch(incoming, bumped, cwd);
            }
            return;
        }
        if let Some((old, new)) = replace_parts(&incoming) {
            let in_frozen = self.frozen_has(&incoming.kind, &old);
            if let Some(existing) = self.shallow.iter_mut().find(|e| {
                e.kind == incoming.kind && !is_correction_entry(e) && entry_targets(e, &old)
            }) {
                existing.text = format_note_add(&incoming.kind, &new);
                existing.missed = 0;
                let key = entry_key(existing);
                if bumped.insert(key) {
                    existing.uses = existing.uses.saturating_add(1);
                }
            }
            if in_frozen {
                self.upsert_and_touch(incoming, bumped, cwd);
            } else if !self.shallow.iter().any(|e| {
                e.kind == incoming.kind && !is_correction_entry(e) && entry_targets(e, &new)
            }) {
                self.upsert_and_touch(
                    ShallowEntry {
                        text: format_note_add(&incoming.kind, &new),
                        kind: incoming.kind,
                        path: incoming.path,
                        uses: 1,
                        missed: 0,
                        ..Default::default()
                    },
                    bumped,
                    cwd,
                );
            }
            return;
        }
        self.upsert_and_touch(incoming, bumped, cwd);
    }

    fn frozen_has(&self, kind: &str, target: &str) -> bool {
        match kind {
            "fact" => self.facts.iter().any(|n| n.text == target),
            "positive" => self.positive.iter().any(|n| n.text == target),
            "negative" => self.negative.iter().any(|n| n.text == target),
            "index" => self.index.iter().any(|e| e.path == target),
            "code" => self.code.iter().any(|s| s.path == target),
            _ => false,
        }
    }

    pub fn candidate_index(&self) -> String {
        let mut out = String::new();
        for entry in self.shallow.iter().filter(|e| is_note_kind(&e.kind)) {
            out.push_str("- [");
            out.push_str(&note_id(&entry.text));
            out.push_str("] ");
            out.push_str(&strip_note_prefix(&entry.text));
            out.push('\n');
        }
        for notes in [&self.facts, &self.positive, &self.negative] {
            for note in notes {
                let text = note.text.trim();
                if text.is_empty() {
                    continue;
                }
                out.push_str("- [");
                out.push_str(&note_id(text));
                out.push_str("] ");
                out.push_str(text);
                out.push('\n');
            }
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
        let mut ranked: Vec<usize> = (0..self.shallow.len()).collect();
        ranked.sort_by(|&a, &b| {
            let (x, y) = (&self.shallow[a], &self.shallow[b]);
            y.missed
                .cmp(&x.missed)
                .then(x.uses.cmp(&y.uses))
                .then_with(|| x.text.cmp(&y.text))
        });
        let drop = self.shallow.len() - SHALLOW_CAP;
        let evict: HashSet<usize> = ranked.into_iter().take(drop).collect();
        let mut index = 0;
        self.shallow.retain(|_| {
            let keep = !evict.contains(&index);
            index += 1;
            keep
        });
    }

    fn rebuild_rendered_shallow(&mut self) {
        let text = render_shallow(&self.shallow, u64::MAX, u64::MAX, &TokenCounter::default());
        self.rendered_shallow = if text.is_empty() { None } else { Some(text) };
    }

    fn sync_rendered_shallow(&mut self, before: &[ShallowEntry]) {
        if self.rendered_shallow.is_none() {
            self.rebuild_rendered_shallow();
            return;
        }
        let before_keys: HashSet<String> = before.iter().map(entry_key).collect();
        let chunks: Vec<String> = self
            .shallow
            .iter()
            .filter(|entry| !before_keys.contains(&entry_key(entry)))
            .map(format_shallow_item)
            .collect();
        for chunk in chunks {
            self.append_rendered_shallow(&chunk);
        }
    }

    fn append_rendered_shallow(&mut self, chunk: &str) {
        if chunk.is_empty() {
            return;
        }
        let mut text = self.rendered_shallow.clone().unwrap_or_default();
        append_inside(&mut text, SHALLOW_OPEN, SHALLOW_CLOSE, chunk);
        self.rendered_shallow = Some(text);
    }

    fn append_frozen_published(&mut self, promoted: &[ShallowEntry]) {
        if promoted.is_empty() {
            return;
        }
        let mut chunk = String::new();
        for entry in promoted {
            chunk.push_str(&format_shallow_item(entry));
        }
        append_inside(
            &mut self.rendered_appends,
            FROZEN_ADD_OPEN,
            FROZEN_ADD_CLOSE,
            &chunk,
        );
    }

    fn frozen_used(
        &self,
        prose_budget: u64,
        code_budget: u64,
        counter: &TokenCounter,
    ) -> (u64, u64) {
        let used_code = counter.count_text(&render_code(&self.code, code_budget, counter));
        let used_prose = counter
            .count_text(&self.render_text(prose_budget, code_budget, counter))
            .saturating_sub(used_code);
        (used_prose, used_code)
    }

    fn bump_frozen_usage(
        &mut self,
        session_hits: &BTreeMap<String, u64>,
        distilled_hits: &[String],
    ) {
        for (path, hits) in session_hits {
            if *hits == 0 {
                continue;
            }
            if let Some(entry) = self.index.iter_mut().find(|e| e.path == *path) {
                entry.hits = entry.hits.saturating_add(1);
            }
            for snippet in self.code.iter_mut().filter(|s| s.path == *path) {
                snippet.hits = snippet.hits.saturating_add(1);
            }
        }
        for id in distilled_hits {
            for notes in [&mut self.facts, &mut self.positive, &mut self.negative] {
                if let Some(note) = notes.iter_mut().find(|n| note_id(&n.text) == *id) {
                    note.hits = note.hits.saturating_add(1);
                }
            }
        }
    }

    fn evict_cold_when_tight(
        &mut self,
        prose_budget: u64,
        code_budget: u64,
        counter: &TokenCounter,
    ) -> bool {
        let (used_prose, used_code) = self.frozen_used(prose_budget, code_budget, counter);
        let mut changed = false;
        if tight_fill(used_code, code_budget) {
            let drop = cold_count(self.code.len());
            if drop > 0 {
                let mut ranked = self.code.clone();
                ranked.sort_by(|a, b| {
                    a.hits
                        .cmp(&b.hits)
                        .then_with(|| a.path.cmp(&b.path))
                        .then(a.start.cmp(&b.start))
                });
                let evict: HashSet<(String, u32, u32)> = ranked
                    .iter()
                    .take(drop)
                    .map(|s| (s.path.clone(), s.start, s.end))
                    .collect();
                let before = self.code.len();
                self.code
                    .retain(|s| !evict.contains(&(s.path.clone(), s.start, s.end)));
                changed |= self.code.len() != before;
            }
        }
        if tight_fill(used_prose, prose_budget) {
            let total = self.facts.len() + self.positive.len() + self.negative.len();
            let drop = cold_count(total);
            if drop > 0 {
                let mut ranked: Vec<(u64, String, String)> = Vec::new();
                for (kind, notes) in [
                    ("fact", &self.facts),
                    ("positive", &self.positive),
                    ("negative", &self.negative),
                ] {
                    for note in notes {
                        ranked.push((note.hits, kind.to_string(), note.text.clone()));
                    }
                }
                ranked.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.2.cmp(&b.2)));
                for (_, kind, text) in ranked.into_iter().take(drop) {
                    changed |= self.promote_delete(&kind, &text);
                }
            }
        }
        changed
    }

    fn should_promote(&self, entry: &ShallowEntry, tight: bool, peers: &[u64]) -> bool {
        if entry.uses < PROMOTION_USES {
            return false;
        }
        if is_correction_entry(entry) || !tight {
            return true;
        }
        is_hot_enough(entry.uses, peers)
    }

    fn promote(
        &mut self,
        cwd: &Path,
        _prose_budget: u64,
        _code_budget: u64,
        _counter: &TokenCounter,
        tight: (bool, bool),
    ) -> Vec<ShallowEntry> {
        let mut keep = Vec::new();
        let mut promoted = Vec::new();
        let candidates = std::mem::take(&mut self.shallow);
        let mut peers: HashMap<String, Vec<u64>> = HashMap::new();
        for entry in &candidates {
            if !is_correction_entry(entry) && entry.uses >= PROMOTION_USES {
                peers
                    .entry(entry.kind.clone())
                    .or_default()
                    .push(entry.uses);
            }
        }
        for entry in candidates {
            let kind_tight = if entry.kind == "code" {
                tight.1
            } else {
                tight.0
            };
            let kind_peers = peers.get(&entry.kind).map(Vec::as_slice).unwrap_or(&[]);
            if !self.should_promote(&entry, kind_tight, kind_peers) {
                keep.push(entry);
                continue;
            }
            if self.promote_one(cwd, &entry) {
                promoted.push(entry);
            } else if entry.kind == "code" && !is_correction_entry(&entry) {
                // 只有代码片段会因文件暂时读不到而失败；留着等下次，别把累计的 uses 丢掉。
                keep.push(entry);
            }
        }
        self.shallow = keep;
        promoted
    }

    fn promote_one(&mut self, cwd: &Path, entry: &ShallowEntry) -> bool {
        if let Some(target) = delete_target(entry) {
            return self.promote_delete(&entry.kind, &target);
        }
        if let Some((old, new)) = replace_parts(entry) {
            return self.promote_replace(&entry.kind, &old, &new, &entry.path);
        }
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
            "fact" => push_note(&mut self.facts, &entry.text, &entry.path, entry.uses),
            "positive" => push_note(&mut self.positive, &entry.text, &entry.path, entry.uses),
            "negative" => push_note(&mut self.negative, &entry.text, &entry.path, entry.uses),
            "code" => self.promote_code(cwd, entry),
            _ => false,
        }
    }

    fn promote_delete(&mut self, kind: &str, target: &str) -> bool {
        match kind {
            "fact" => delete_note(&mut self.facts, target),
            "positive" => delete_note(&mut self.positive, target),
            "negative" => delete_note(&mut self.negative, target),
            "index" => {
                let before = self.index.len();
                self.index.retain(|e| e.path != target);
                self.index.len() != before
            }
            "code" => {
                let before = self.code.len();
                self.code.retain(|s| s.path != target);
                self.code.len() != before
            }
            _ => false,
        }
    }

    fn promote_replace(&mut self, kind: &str, old: &str, new: &str, path: &str) -> bool {
        let notes = match kind {
            "fact" => &mut self.facts,
            "positive" => &mut self.positive,
            "negative" => &mut self.negative,
            _ => return false,
        };
        if let Some(note) = notes.iter_mut().find(|n| n.text == old) {
            note.text = new.to_string();
            if !path.is_empty() {
                note.evidence = path.to_string();
            }
            return true;
        }
        push_note(notes, new, path, 1)
    }

    fn promote_code(&mut self, cwd: &Path, entry: &ShallowEntry) -> bool {
        if entry.path.is_empty() {
            return true;
        }
        if entry.text.contains("已过时") || self.code.iter().any(|s| s.path == entry.path) {
            return refresh_code(self, cwd, &entry.path);
        }
        if ingest_code(self, cwd, &entry.path, entry.uses) {
            return true;
        }
        let Some(mut snippet) = shallow_snippet(entry) else {
            return false;
        };
        if snippet.body.is_empty() {
            return false;
        }
        snippet.hits = entry.uses.max(1);
        self.code.push(snippet);
        self.code
            .sort_by(|a, b| a.path.cmp(&b.path).then(a.start.cmp(&b.start)));
        true
    }
}

fn tight_fill(used: u64, budget: u64) -> bool {
    budget == 0 || (used as f64 / budget as f64) >= TIGHT_FILL
}

fn cold_count(len: usize) -> usize {
    (len as f64 * COLD_RATIO).floor() as usize
}

fn is_hot_enough(uses: u64, peers: &[u64]) -> bool {
    if peers.len() < 5 {
        return true;
    }
    let mut scores = peers.to_vec();
    scores.sort_unstable();
    let idx = ((scores.len() as f64) * (1.0 - COLD_RATIO)).floor() as usize;
    let idx = idx.min(scores.len().saturating_sub(1));
    uses >= scores[idx]
}

fn is_zero_hits(value: &u64) -> bool {
    *value == 0
}

fn push_note(notes: &mut Vec<Note>, text: &str, path: &str, hits: u64) -> bool {
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
        hits,
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
        && correction_kind(a) == correction_kind(b)
        && if a.path.is_empty() && b.path.is_empty() {
            match (
                delete_target(a),
                delete_target(b),
                replace_parts(a),
                replace_parts(b),
            ) {
                (Some(ta), Some(tb), _, _) => ta == tb,
                (_, _, Some(ta), Some(tb)) => ta == tb,
                _ => a.text == b.text,
            }
        } else {
            !a.path.is_empty() && a.path == b.path
        }
}

fn entry_key(entry: &ShallowEntry) -> String {
    let id = if entry.path.is_empty() {
        entry.text.as_str()
    } else {
        entry.path.as_str()
    };
    format!("{}:{}:{id}", entry.kind, correction_kind(entry))
}

fn correction_kind(entry: &ShallowEntry) -> &'static str {
    if delete_target(entry).is_some() {
        "delete"
    } else if replace_parts(entry).is_some() {
        "replace"
    } else if entry.text.contains("已过时") {
        "stale"
    } else {
        "add"
    }
}

fn is_correction_entry(entry: &ShallowEntry) -> bool {
    correction_kind(entry) != "add"
}

fn entry_targets(entry: &ShallowEntry, target: &str) -> bool {
    if !entry.path.is_empty() {
        entry.path == target
    } else {
        strip_note_prefix(&entry.text) == target
    }
}

fn format_note_add(kind: &str, text: &str) -> String {
    match kind {
        "fact" => format!("事实 增加 {text}"),
        "positive" => format!("有效经验 增加 {text}"),
        "negative" => format!("失败经验 增加 {text}"),
        _ => text.to_string(),
    }
}

fn delete_target(entry: &ShallowEntry) -> Option<String> {
    let text = entry.text.trim();
    for prefix in [
        "事实 删除 ",
        "有效经验 删除 ",
        "失败经验 删除 ",
        "索引 删除 ",
        "代码 删除 ",
    ] {
        if let Some(rest) = text.strip_prefix(prefix) {
            let rest = rest.trim();
            if rest.is_empty() {
                return None;
            }
            return Some(rest.to_string());
        }
    }
    if entry.kind == "code" && text.contains("删除") && !entry.path.is_empty() {
        return Some(entry.path.clone());
    }
    None
}

fn replace_parts(entry: &ShallowEntry) -> Option<(String, String)> {
    let text = entry.text.trim();
    for prefix in ["事实 更正 ", "有效经验 更正 ", "失败经验 更正 "] {
        if let Some(rest) = text.strip_prefix(prefix) {
            for sep in [" -> ", " → ", " 为 "] {
                if let Some((old, new)) = rest.split_once(sep) {
                    let old = old.trim();
                    let new = new.trim();
                    if !old.is_empty() && !new.is_empty() {
                        return Some((old.to_string(), new.to_string()));
                    }
                }
            }
        }
    }
    None
}

fn delete_note(notes: &mut Vec<Note>, target: &str) -> bool {
    let before = notes.len();
    notes.retain(|n| n.text != target);
    notes.len() != before
}

fn code_changed_on_disk(code: &[CodeSnippet], cwd: &Path, path: &str) -> bool {
    code.iter().any(|snippet| {
        if snippet.path != path {
            return false;
        }
        if snippet.stale {
            return true;
        }
        match std::fs::read_to_string(cwd.join(&snippet.path)) {
            Err(_) => true,
            Ok(text) => hash_range(&text, snippet.start, snippet.end) != snippet.hash,
        }
    })
}

fn load_snippet(cwd: &Path, path: &str) -> Option<CodeSnippet> {
    let text = std::fs::read_to_string(cwd.join(path)).ok()?;
    if text.contains('\u{0}') {
        return None;
    }
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
        return None;
    }
    Some(CodeSnippet {
        path: path.to_string(),
        start: 1,
        end,
        hash: hash_range(&text, 1, end),
        body,
        stale: false,
        hits: 0,
    })
}

fn ingest_code(snap: &mut Snapshot, cwd: &Path, path: &str, hits: u64) -> bool {
    let Some(mut snippet) = load_snippet(cwd, path) else {
        return false;
    };
    snippet.hits = hits.max(1);
    snap.code.push(snippet);
    snap.code
        .sort_by(|a, b| a.path.cmp(&b.path).then(a.start.cmp(&b.start)));
    true
}

fn refresh_code(snap: &mut Snapshot, cwd: &Path, path: &str) -> bool {
    let hits = snap
        .code
        .iter()
        .filter(|s| s.path == path)
        .map(|s| s.hits)
        .max()
        .unwrap_or(0);
    if let Some(mut snippet) = load_snippet(cwd, path) {
        snippet.hits = hits;
        snap.code.retain(|s| s.path != path);
        snap.code.push(snippet);
        snap.code
            .sort_by(|a, b| a.path.cmp(&b.path).then(a.start.cmp(&b.start)));
        return true;
    }
    let mut found = false;
    for snippet in snap.code.iter_mut().filter(|s| s.path == path) {
        snippet.stale = true;
        snippet.body.clear();
        snippet.hash.clear();
        found = true;
    }
    found
}

fn is_zero_line(value: &u32) -> bool {
    *value == 0
}

fn attach_code_body(entry: &mut ShallowEntry, cwd: &Path) {
    if entry.kind != "code" || entry.path.is_empty() || delete_target(entry).is_some() {
        return;
    }
    if let Some(snippet) = load_snippet(cwd, &entry.path) {
        entry.start = snippet.start;
        entry.end = snippet.end;
        entry.hash = snippet.hash;
        entry.body = snippet.body;
        entry.stale = false;
    } else {
        entry.stale = true;
        entry.body.clear();
    }
}

fn shallow_snippet(entry: &ShallowEntry) -> Option<CodeSnippet> {
    if entry.kind != "code" || entry.path.is_empty() {
        return None;
    }
    Some(CodeSnippet {
        path: entry.path.clone(),
        start: if entry.start == 0 { 1 } else { entry.start },
        end: entry.end,
        hash: entry.hash.clone(),
        body: entry.body.clone(),
        stale: entry.stale || entry.body.is_empty(),
        hits: entry.uses,
    })
}

fn shallow_index_entry(entry: &ShallowEntry) -> Option<IndexEntry> {
    if entry.kind != "index" || is_correction_entry(entry) || entry.path.is_empty() {
        return None;
    }
    let path = entry.path.clone();
    let rest = entry
        .text
        .trim()
        .strip_prefix("索引 新增 ")
        .or_else(|| entry.text.trim().strip_prefix("索引 "))
        .unwrap_or(entry.text.trim());
    let after = rest.strip_prefix(&path).unwrap_or("").trim();
    let after = after
        .strip_prefix("多次使用")
        .or_else(|| after.strip_prefix("更新"))
        .unwrap_or(after)
        .trim();
    let role = if after.is_empty() {
        default_role(&path)
    } else {
        after.to_string()
    };
    Some(IndexEntry {
        path,
        role,
        hits: entry.uses,
    })
}

fn shallow_notes(entries: &[ShallowEntry], kind: &str) -> Vec<Note> {
    entries
        .iter()
        .filter(|entry| entry.kind == kind && !is_correction_entry(entry))
        .map(|entry| Note {
            text: strip_note_prefix(&entry.text),
            evidence: entry.path.clone(),
            hits: entry.uses,
        })
        .filter(|note| !note.text.is_empty())
        .collect()
}

fn append_corrections(out: &mut String, entries: &[ShallowEntry], kind: &str) {
    for entry in entries {
        if entry.kind != kind {
            continue;
        }
        if let Some(target) = delete_target(entry) {
            out.push_str("- 删除 ");
            out.push_str(&target);
            out.push('\n');
        } else if let Some((old, new)) = replace_parts(entry) {
            out.push_str("- 更正 ");
            out.push_str(&old);
            out.push_str(" -> ");
            out.push_str(&new);
            out.push('\n');
        }
    }
}

/// 浅层按添加顺序写出，不按索引 / 经验 / 事实 / 代码分节重排，以便只在末尾追加时复用前缀缓存。
/// 代码仍然写出正文，而不是只留路径。额度紧张时从末尾裁掉较新的条目，保住已经发布的前缀。
fn render_shallow(
    entries: &[ShallowEntry],
    prose_budget: u64,
    code_budget: u64,
    counter: &TokenCounter,
) -> String {
    if entries.is_empty() {
        return String::new();
    }
    let mut items = String::new();
    let mut used_code = 0u64;
    for entry in entries {
        let item = format_shallow_item(entry);
        if item.is_empty() {
            continue;
        }
        if entry.kind == "code" && delete_target(entry).is_none() {
            let next_code = used_code.saturating_add(counter.count_text(&item));
            if code_budget != u64::MAX && next_code > code_budget {
                continue;
            }
            used_code = next_code;
        }
        items.push_str(&item);
    }
    let budget = if prose_budget == u64::MAX && code_budget == u64::MAX {
        u64::MAX
    } else {
        prose_budget.saturating_add(code_budget)
    };
    let items = if budget == u64::MAX {
        items
    } else {
        trim_to_budget(&items, budget, counter)
    };
    if items.is_empty() {
        return String::new();
    }
    let mut body = String::from(SHALLOW_OPEN);
    body.push_str(&items);
    body.push_str(SHALLOW_CLOSE);
    body
}

fn format_shallow_item(entry: &ShallowEntry) -> String {
    if let Some(target) = delete_target(entry) {
        return format!("- 删除 {target}\n");
    }
    if let Some((old, new)) = replace_parts(entry) {
        return format!("- 更正 {old} -> {new}\n");
    }
    match entry.kind.as_str() {
        "index" => {
            if let Some(item) = shallow_index_entry(entry) {
                let role = item.role.trim();
                if role.is_empty() {
                    format!("- 索引 {}\n", item.path)
                } else {
                    format!("- 索引 {} {}\n", item.path, role)
                }
            } else if entry.text.is_empty() {
                String::new()
            } else {
                format!("- {}\n", entry.text.trim())
            }
        }
        "fact" | "positive" | "negative" => {
            let heading = match entry.kind.as_str() {
                "positive" => HEADING_POSITIVE,
                "negative" => HEADING_NEGATIVE,
                _ => HEADING_FACTS,
            };
            let text = strip_note_prefix(&entry.text);
            if text.is_empty() {
                return String::new();
            }
            if entry.path.trim().is_empty() {
                format!("- {heading} {text}\n")
            } else {
                format!("- {heading} {text} [{}]\n", entry.path.trim())
            }
        }
        "code" => shallow_snippet(entry)
            .map(|snippet| format_snippet(&snippet))
            .filter(|text| !text.is_empty())
            .unwrap_or_else(|| {
                if entry.text.is_empty() {
                    String::new()
                } else {
                    format!("- {}\n", entry.text.trim())
                }
            }),
        _ => {
            if entry.text.is_empty() {
                String::new()
            } else {
                format!("- {}\n", entry.text.trim())
            }
        }
    }
}

fn shallow_user_messages(text: &str) -> Vec<Value> {
    if text.is_empty() {
        return Vec::new();
    }
    vec![
        json!({ "role": "user", "content": [{ "type": "text", "text": text }] }),
        json!({ "role": "assistant", "content": [{ "type": "text", "text": ACK_RECENT }] }),
    ]
}

fn append_inside(buf: &mut String, open: &str, close: &str, chunk: &str) {
    if chunk.is_empty() {
        return;
    }
    if buf.is_empty() {
        buf.push_str(open);
        buf.push_str(chunk);
        buf.push_str(close);
        return;
    }
    if let Some(at) = buf.rfind(close) {
        buf.insert_str(at, chunk);
    } else {
        buf.push_str(chunk);
    }
}

/// 整理指令：并进会话末尾那条可变段，不另起一条 user。记忆与对话都已经在上文前缀里，
/// 这里只给格式和候选编号——把记忆正文再贴一遍会按全价重买一次。
pub fn distill_instruction(candidates: &str) -> String {
    format!(
        "这次请求只用来整理项目记忆：不是对用户的回复，用户看不见。不要调用任何工具，直接输出条目。\n\n\
上文开头的项目记忆是当前冻结区与近期浅层（「近期」那一段），之后是本会话的完整过程；两者都已经在上下文里。\n\
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
用到 <下面候选里的编号>\n\
事实 删除 <一句>\n\
事实 更正 <原句> -> <新句>\n\
有效经验 删除 <一句>\n\
有效经验 更正 <原句> -> <新句>\n\
失败经验 删除 <一句>\n\
失败经验 更正 <原句> -> <新句>\n\
索引 删除 <路径>\n\n\
只记有实际意义的内容：新的事实、可复用的经验、用过且值得补进索引或代码区的文件。不要重复冻结区里已有的内容。不要编造。\n\
「用到」只写给本会话真的依赖了的那几条候选，没用上就不写。同一条在一次整理里不要重复写来凑次数。\n\
发现记错了才删除或更正；文件更新了也用「已过时」记在浅层。这些都先留在浅层，不要指望立刻改冻结区。不要因为暂时没用到就删。\n\n\
待定候选（「用到」只填这里的编号，没有候选就不写）：\n{}\n",
        if candidates.trim().is_empty() { "（暂无候选）" } else { candidates.trim() }
    )
}

/// 整理输出：新增的浅层条目、纠错，以及本会话真正用到的候选编号。
#[derive(Debug, Default)]
pub struct Distilled {
    pub entries: Vec<ShallowEntry>,
    pub hits: Vec<String>,
    pub corrections: Vec<ShallowEntry>,
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
            let id = rest
                .trim()
                .trim_start_matches(['：', ':', '['])
                .trim_end_matches(']');
            if let Some(id) = id.split_whitespace().next() {
                let id = id.trim_end_matches('。').to_ascii_lowercase();
                if !id.is_empty() {
                    distilled.hits.push(id);
                }
            }
            continue;
        }
        if let Some(entry) = parse_correction_line(line) {
            distilled.corrections.push(entry);
            continue;
        }
        if let Some(entry) = parse_distill_line(line) {
            distilled.entries.push(entry);
        }
    }
    distilled
}

fn parse_kind_prefix(line: &str) -> Option<(&str, &str, &str)> {
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
    Some((kind, rest, line))
}

fn parse_correction_line(line: &str) -> Option<ShallowEntry> {
    let (kind, rest, raw) = parse_kind_prefix(line)?;
    let rest = rest.trim().trim_start_matches(['：', ':', ' ']).trim();
    let path = distill_path(kind, rest);
    if rest.starts_with("删除") || rest.starts_with("去掉") {
        let target = rest
            .trim_start_matches("删除")
            .trim_start_matches("去掉")
            .trim();
        let target = if kind == "index" || kind == "code" {
            if path.is_empty() {
                target.to_string()
            } else {
                path.clone()
            }
        } else {
            target.to_string()
        };
        if target.is_empty() {
            return None;
        }
        let text = match kind {
            "fact" => format!("事实 删除 {target}"),
            "positive" => format!("有效经验 删除 {target}"),
            "negative" => format!("失败经验 删除 {target}"),
            "index" => format!("索引 删除 {target}"),
            "code" => format!("代码 删除 {target}"),
            _ => return None,
        };
        return Some(ShallowEntry {
            kind: kind.to_string(),
            text,
            path: if kind == "index" || kind == "code" {
                target
            } else {
                String::new()
            },
            uses: 1,
            missed: 0,
            ..Default::default()
        });
    }
    if rest.starts_with("更正") {
        let body = rest.trim_start_matches("更正").trim();
        let (old, new) = body
            .split_once(" -> ")
            .or_else(|| body.split_once(" → "))
            .or_else(|| body.split_once(" 为 "))?;
        let old = old.trim();
        let new = new.trim();
        if old.is_empty() || new.is_empty() {
            return None;
        }
        let text = match kind {
            "fact" => format!("事实 更正 {old} -> {new}"),
            "positive" => format!("有效经验 更正 {old} -> {new}"),
            "negative" => format!("失败经验 更正 {old} -> {new}"),
            _ => return None,
        };
        return Some(ShallowEntry {
            kind: kind.to_string(),
            text,
            path: String::new(),
            uses: 1,
            missed: 0,
            ..Default::default()
        });
    }
    if kind == "code" && (rest.contains("已过时") || raw.contains("已过时")) {
        if path.is_empty() {
            return None;
        }
        return Some(ShallowEntry {
            kind: "code".into(),
            text: format!("代码 {path} 已过时，需重新读取"),
            path,
            uses: 1,
            missed: 0,
            ..Default::default()
        });
    }
    None
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
        ..Default::default()
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
    let root = repo_info(cwd).root?;
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
    let out = git(cwd, &["ls-files", "-z"]).output().ok()?;
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
            hits: 0,
        });
        snap.negative.push(Note {
            text: "不要直接改生成文件".into(),
            evidence: String::new(),
            hits: 0,
        });
        snap.facts.push(Note {
            text: "默认压缩是 ACP".into(),
            evidence: String::new(),
            hits: 0,
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
    fn one_session_counts_file_uses_once_even_if_touched_many_times() {
        let dir = tempfile();
        fs::write(dir.join("a.rs"), "fn a() {}\n").unwrap();
        let counter = TokenCounter::default();
        let mut snap = Snapshot::new("k");
        snap.ensure_frozen(10_000, 200, &counter);
        let mut hits = BTreeMap::new();
        hits.insert("a.rs".to_string(), 5);
        snap.apply_session_end("", &hits, &dir, 10_000, 200, &counter);
        let code = snap
            .shallow
            .iter()
            .find(|e| e.kind == "code" && e.path == "a.rs")
            .expect("code candidate");
        assert_eq!(code.uses, 1);
        assert!(code.body.contains("fn a() {}"), "{:?}", code);
        assert!(snap.code.is_empty(), "{:?}", snap.code);
        let messages = snap.shallow_messages();
        let recent = messages[0]["content"][0]["text"].as_str().unwrap();
        assert!(recent.contains("- 索引 a.rs"), "{recent}");
        assert!(recent.contains("fn a() {}"), "{recent}");
        assert!(
            recent.find("- 索引 a.rs").unwrap() < recent.find("fn a() {}").unwrap(),
            "shallow keeps add order, not section order: {recent}"
        );
        assert!(!recent.contains("# 代码"), "{recent}");
        assert!(!recent.contains("# 近期"), "{recent}");
        let index = snap
            .shallow
            .iter()
            .find(|e| e.kind == "index" && e.path == "a.rs")
            .expect("index candidate");
        assert_eq!(index.uses, 1);
        assert!(!snap.index.iter().any(|e| e.path == "a.rs"));
    }

    #[test]
    fn hydrate_fills_code_body_for_path_only_shallow_entries() {
        let dir = tempfile();
        fs::write(dir.join("a.rs"), "fn a() { 2 }\n").unwrap();
        let mut snap = Snapshot::new("k");
        snap.shallow.push(ShallowEntry {
            kind: "code".into(),
            text: "代码 新增 a.rs".into(),
            path: "a.rs".into(),
            uses: 2,
            missed: 0,
            ..Default::default()
        });
        assert!(snap.hydrate_shallow_code(&dir));
        assert!(
            snap.shallow[0].body.contains("fn a() { 2 }"),
            "{:?}",
            snap.shallow[0]
        );
        let messages = snap.shallow_messages();
        let recent = messages[0]["content"][0]["text"].as_str().unwrap();
        assert!(recent.contains("fn a() { 2 }"), "{recent}");
    }

    #[test]
    fn file_uses_across_sessions_reach_the_code_section() {
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
        snap.apply_session_end("", &hits, &dir, 10_000, 200, &counter);
        assert!(
            snap.code.is_empty(),
            "must stay in shallow after one session"
        );
        for _ in 0..(PROMOTION_USES - 1) {
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
        let frozen_text: String = frozen
            .iter()
            .filter(|m| m["role"] == "user")
            .map(|m| m["content"][0]["text"].as_str().unwrap_or("").to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(frozen_text.contains("fn a() {}"), "{frozen_text}");
        assert_eq!(
            frozen[0]["content"][0]["text"],
            snap.rendered.clone().unwrap()
        );
    }

    #[test]
    fn stale_file_waits_in_shallow_before_rewriting_frozen_code() {
        let dir = tempfile();
        fs::write(dir.join("a.rs"), "fn a() {}\n").unwrap();
        let counter = TokenCounter::default();
        let mut snap = Snapshot::new("k");
        snap.code.push(CodeSnippet {
            path: "a.rs".into(),
            start: 1,
            end: 1,
            hash: hash_range("fn a() {}\n", 1, 1),
            body: "fn a() {}".into(),
            stale: false,
            hits: 0,
        });
        snap.ensure_frozen(10_000, 200, &counter);
        let frozen = snap.frozen_messages();
        fs::write(dir.join("a.rs"), "fn a() { 1 }\n").unwrap();
        let mut hits = BTreeMap::new();
        hits.insert("a.rs".to_string(), 1);
        snap.apply_session_end("", &hits, &dir, 10_000, 200, &counter);
        assert_eq!(snap.frozen_messages(), frozen, "correction stays shallow");
        assert!(!snap.code[0].stale);
        assert!(snap.code[0].body.contains("fn a() {}"));
        assert!(
            snap.shallow
                .iter()
                .any(|e| e.kind == "code" && e.text.contains("已过时") && e.uses == 1)
        );
        let first = frozen[0].clone();
        for _ in 0..(PROMOTION_USES - 1) {
            snap.apply_session_end("", &hits, &dir, 10_000, 200, &counter);
        }
        assert_ne!(snap.frozen_messages(), frozen);
        assert_eq!(
            snap.frozen_messages()[0],
            first,
            "ample promotion must append, not rewrite the published frozen prefix"
        );
        assert!(
            snap.code
                .iter()
                .any(|s| s.path == "a.rs" && !s.stale && s.body.contains("fn a() { 1 }")),
            "{:?}",
            snap.code
        );
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
            hits: 0,
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
        let distilled = parse_distill(
            "索引 新增 src/util.rs 工具函数\n事实 增加 默认压缩是 ACP\n有效经验 增加 先跑测试再改接口\n代码 src/a.rs 已过时",
        );
        let entries = distilled.entries;
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].kind, "index");
        assert_eq!(entries[0].path, "src/util.rs");
        assert!(entries[0].text.contains("索引 新增 src/util.rs"));
        assert_eq!(entries[1].kind, "fact");
        assert_eq!(entries[2].kind, "positive");
        assert_eq!(distilled.corrections.len(), 1);
        assert_eq!(distilled.corrections[0].kind, "code");
        assert!(distilled.corrections[0].text.contains("已过时"));
    }

    #[test]
    fn parse_distill_reads_used_candidate_ids() {
        let distilled = parse_distill("- 用到 [a1b2c3]\n用到：D4E5F6 关键\n事实 增加 新的一句");
        assert_eq!(
            distilled.hits,
            vec!["a1b2c3".to_string(), "d4e5f6".to_string()]
        );
        assert_eq!(distilled.entries.len(), 1);
        assert_eq!(distilled.entries[0].kind, "fact");
    }

    #[test]
    fn parse_distill_reads_corrections() {
        let distilled =
            parse_distill("事实 删除 旧结论\n事实 更正 旧句 -> 新句\n索引 删除 src/old.rs");
        assert!(distilled.entries.is_empty());
        assert_eq!(distilled.corrections.len(), 3);
        assert_eq!(
            delete_target(&distilled.corrections[0]).as_deref(),
            Some("旧结论")
        );
        assert_eq!(
            replace_parts(&distilled.corrections[1]),
            Some(("旧句".into(), "新句".into()))
        );
        assert_eq!(distilled.corrections[2].path, "src/old.rs");
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
        assert!(snap.shallow.iter().any(|e| e.kind == "fact"));
        assert!(
            snap.shallow
                .iter()
                .any(|e| e.kind == "index" && e.path == "b.rs")
        );
        assert!(
            snap.shallow
                .iter()
                .any(|e| e.kind == "code" && e.path == "b.rs")
        );
        assert_eq!(snap.shallow.len(), 3);
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
        assert_eq!(
            snap.frozen_messages()[0],
            frozen[0],
            "ample promotion appends after the published frozen prefix"
        );
        assert!(snap.shallow.iter().all(|e| e.kind != "fact"));

        // 索引也按跨会话次数累计：再经过两个会话才晋升。
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
        assert!(
            snap.shallow
                .iter()
                .all(|e| e.kind != "index" && e.kind != "fact")
        );
        // 代码候选还要等自己的跨会话次数，不会跟着索引一起进冻结区。
        assert!(
            snap.shallow
                .iter()
                .any(|e| e.kind == "code" && e.path == "b.rs" && e.uses == 1)
        );
    }

    #[test]
    fn unused_notes_stay_until_shallow_is_full() {
        let dir = tempfile();
        let mut snap = Snapshot::new("k");
        let counter = TokenCounter::default();
        snap.apply_session_end(
            "事实 增加 结论 0",
            &BTreeMap::new(),
            &dir,
            10_000,
            200,
            &counter,
        );
        for index in 1..=8 {
            snap.apply_session_end(
                &format!("事实 增加 结论 {index}"),
                &BTreeMap::new(),
                &dir,
                10_000,
                200,
                &counter,
            );
        }
        assert!(
            snap.shallow.iter().any(|e| e.text.contains("结论 0")),
            "unused notes are not dropped just because time passed: {:?}",
            snap.shallow
        );
        assert!(snap.facts.is_empty());
    }

    #[test]
    fn cold_notes_are_forgotten_when_shallow_is_full() {
        let dir = tempfile();
        let mut snap = Snapshot::new("k");
        let counter = TokenCounter::default();
        for index in 0..=SHALLOW_CAP {
            snap.apply_session_end(
                &format!("事实 增加 结论 {index}"),
                &BTreeMap::new(),
                &dir,
                10_000,
                200,
                &counter,
            );
        }
        assert_eq!(snap.shallow.len(), SHALLOW_CAP);
        assert!(
            snap.shallow.iter().all(|e| !e.text.contains("结论 0")),
            "the coldest entry should be forgotten when full: {:?}",
            snap.shallow
        );
        assert!(
            snap.shallow
                .iter()
                .any(|e| e.text.contains(&format!("结论 {SHALLOW_CAP}")))
        );
        assert!(snap.facts.is_empty());
    }

    #[test]
    fn correction_stays_in_shallow_until_promoted() {
        let dir = tempfile();
        let mut snap = Snapshot::new("k");
        let counter = TokenCounter::default();
        snap.apply_session_end(
            "事实 增加 旧结论",
            &BTreeMap::new(),
            &dir,
            10_000,
            200,
            &counter,
        );
        let line = snap
            .candidate_index()
            .lines()
            .find(|line| line.contains("旧结论"))
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
        assert!(snap.facts.iter().any(|n| n.text == "旧结论"));
        let frozen = snap.frozen_messages();

        snap.apply_session_end(
            "事实 更正 旧结论 -> 新结论",
            &BTreeMap::new(),
            &dir,
            10_000,
            200,
            &counter,
        );
        assert_eq!(
            snap.frozen_messages(),
            frozen,
            "correction must not break the prefix cache"
        );
        assert!(snap.facts.iter().any(|n| n.text == "旧结论"));
        assert!(
            snap.shallow
                .iter()
                .any(|e| e.text.contains("更正") && e.uses == 1)
        );

        for _ in 0..2 {
            snap.apply_session_end(
                "事实 更正 旧结论 -> 新结论",
                &BTreeMap::new(),
                &dir,
                10_000,
                200,
                &counter,
            );
        }
        assert!(
            snap.facts.iter().any(|n| n.text == "新结论"),
            "{:?}",
            snap.facts
        );
        assert!(snap.facts.iter().all(|n| n.text != "旧结论"));
        assert_ne!(snap.frozen_messages(), frozen);
        assert_eq!(
            snap.frozen_messages()[0],
            frozen[0],
            "ample correction promotion must not rewrite the published frozen prefix"
        );
    }

    #[test]
    fn correction_can_fix_shallow_notes_immediately() {
        let dir = tempfile();
        let mut snap = Snapshot::new("k");
        let counter = TokenCounter::default();
        snap.apply_session_end(
            "事实 增加 记错了",
            &BTreeMap::new(),
            &dir,
            10_000,
            200,
            &counter,
        );
        snap.apply_session_end(
            "事实 删除 记错了",
            &BTreeMap::new(),
            &dir,
            10_000,
            200,
            &counter,
        );
        assert!(
            snap.shallow.iter().all(|e| !e.text.contains("记错了")),
            "{:?}",
            snap.shallow
        );
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
            ..Default::default()
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

    #[test]
    fn tight_fill_uses_occupancy_ratio() {
        assert!(!tight_fill(79, 100));
        assert!(tight_fill(80, 100));
        assert!(tight_fill(1, 1));
        assert!(tight_fill(0, 0));
        assert_eq!(cold_count(5), 1);
        assert_eq!(cold_count(4), 0);
        assert!(is_hot_enough(3, &[3, 3]));
        assert!(is_hot_enough(5, &[3, 3, 3, 3, 5]));
        assert!(!is_hot_enough(3, &[3, 3, 3, 3, 5]));
    }

    #[test]
    fn ample_budget_still_needs_three_uses_to_promote() {
        let dir = tempfile();
        let mut snap = Snapshot::new("k");
        let counter = TokenCounter::default();
        snap.apply_session_end(
            "事实 增加 只在有意义时更新",
            &BTreeMap::new(),
            &dir,
            10_000,
            200,
            &counter,
        );
        let line = snap
            .candidate_index()
            .lines()
            .find(|line| line.contains("只在有意义时更新"))
            .expect("candidate listed")
            .to_string();
        let id = line[3..line.find(']').unwrap()].to_string();
        snap.apply_session_end(
            &format!("用到 {id}"),
            &BTreeMap::new(),
            &dir,
            10_000,
            200,
            &counter,
        );
        assert!(
            snap.facts.is_empty(),
            "two sessions must stay shallow: {:?}",
            snap.facts
        );
        snap.apply_session_end(
            &format!("用到 {id}"),
            &BTreeMap::new(),
            &dir,
            10_000,
            200,
            &counter,
        );
        assert!(
            snap.facts.iter().any(|n| n.text == "只在有意义时更新"),
            "{:?}",
            snap.facts
        );
    }

    #[test]
    fn tight_budget_evicts_coldest_fifth_and_promotes_only_hottest() {
        let dir = tempfile();
        let counter = TokenCounter::default();
        let body = "fn x() { 1 }\n".repeat(30);
        let mut snap = Snapshot::new("k");
        for index in 0..5 {
            snap.code.push(CodeSnippet {
                path: format!("c{index}.rs"),
                start: 1,
                end: 30,
                hash: format!("h{index}"),
                body: body.clone(),
                stale: false,
                hits: (index + 1) as u64,
            });
        }
        let used = counter.count_text(&render_code(&snap.code, u64::MAX, &counter));
        let budget = used * 4 / 5;
        assert!(tight_fill(used, budget));
        snap.ensure_frozen(10_000, budget, &counter);
        assert!(snap.evict_cold_when_tight(10_000, budget, &counter));
        assert!(
            snap.code.iter().all(|s| s.path != "c0.rs"),
            "lowest-hit snippet should be evicted: {:?}",
            snap.code
        );
        assert_eq!(snap.code.len(), 4);

        for index in 0..5 {
            let path = format!("n{index}.rs");
            fs::write(dir.join(&path), "fn n() {}\n").unwrap();
            snap.shallow.push(ShallowEntry {
                kind: "code".into(),
                text: format!("代码 新增 {path}"),
                path: path.clone(),
                uses: if index == 4 { 8 } else { 3 },
                missed: 0,
                ..Default::default()
            });
        }
        snap.hydrate_shallow_code(&dir);
        let (used_prose, used_code) = snap.frozen_used(10_000, budget, &counter);
        assert!(
            tight_fill(used_code, budget),
            "code occupancy should stay tight after evict: {used_code}/{budget}, prose={used_prose}"
        );
        assert!(!snap
            .promote(&dir, 10_000, budget, &counter, (false, true))
            .is_empty());
        assert!(
            snap.code.iter().any(|s| s.path == "n4.rs"),
            "hottest candidate should promote: {:?}",
            snap.code
        );
        assert_eq!(
            snap.shallow
                .iter()
                .filter(|e| e.kind == "code" && e.path.starts_with('n') && e.path != "n4.rs")
                .count(),
            4,
            "colder ready candidates stay shallow when tight: {:?}",
            snap.shallow
        );
    }

    #[test]
    fn shallow_keeps_add_order_and_appends_without_rewriting_prefix() {
        let dir = tempfile();
        let counter = TokenCounter::default();
        let mut snap = Snapshot::new("k");
        fs::write(dir.join("later.rs"), "fn later() {}\n").unwrap();
        snap.apply_session_end(
            "代码 新增 later.rs",
            &BTreeMap::new(),
            &dir,
            10_000,
            200,
            &counter,
        );
        snap.apply_session_end(
            "事实 增加 后追加的事实",
            &BTreeMap::new(),
            &dir,
            10_000,
            200,
            &counter,
        );
        let recent = snap.published_shallow_messages(10_000, 200, &counter);
        let text = recent[0]["content"][0]["text"].as_str().unwrap();
        let code_at = text.find("fn later() {}").expect(text);
        let fact_at = text.find("后追加的事实").expect(text);
        assert!(
            code_at < fact_at,
            "must follow add order, not section order: {text}"
        );
        assert!(!text.contains("# 事实"), "{text}");
        let before = text[..fact_at].to_string();
        snap.apply_session_end(
            "有效经验 增加 再追加一条经验",
            &BTreeMap::new(),
            &dir,
            10_000,
            200,
            &counter,
        );
        let later = snap.published_shallow_messages(10_000, 200, &counter);
        let later_text = later[0]["content"][0]["text"].as_str().unwrap();
        assert!(
            later_text.starts_with(&before),
            "append must not rewrite earlier shallow text:\n{before}\n---\n{later_text}"
        );
        assert!(later_text.contains("再追加一条经验"), "{later_text}");
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
