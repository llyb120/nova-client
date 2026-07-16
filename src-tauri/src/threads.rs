use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::Notify;

use crate::clues::ClueContextSnapshot;

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum AgentKind {
    Devin,
    Codex,
    CodexPlus,
    CodeBuddy,
    CodeBuddyPlus,
    ClaudeCode,
    Cursor,
    OpenCode,
    OpenCodePlus,
}

impl Default for AgentKind {
    fn default() -> Self {
        AgentKind::Devin
    }
}

impl AgentKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            AgentKind::Devin => "devin",
            AgentKind::Codex => "codex",
            AgentKind::CodexPlus => "codexplus",
            AgentKind::CodeBuddy => "codebuddy",
            AgentKind::CodeBuddyPlus => "codebuddyplus",
            AgentKind::ClaudeCode => "claudecode",
            AgentKind::Cursor => "cursor",
            AgentKind::OpenCode => "opencode",
            AgentKind::OpenCodePlus => "opencodeplus",
        }
    }

    /// 从字符串解析后端标识（大小写不敏感）；无法识别返回 None。
    pub fn from_str(s: &str) -> Option<AgentKind> {
        match s.trim().to_ascii_lowercase().as_str() {
            "devin" => Some(AgentKind::Devin),
            "codex" => Some(AgentKind::Codex),
            "codexplus" => Some(AgentKind::CodexPlus),
            "codebuddy" => Some(AgentKind::CodeBuddy),
            "codebuddyplus" => Some(AgentKind::CodeBuddyPlus),
            "claudecode" => Some(AgentKind::ClaudeCode),
            "cursor" => Some(AgentKind::Cursor),
            "opencode" => Some(AgentKind::OpenCode),
            "opencodeplus" => Some(AgentKind::OpenCodePlus),
            _ => None,
        }
    }

    /// 展示用名称（注入接力上下文 / 系统提示用）
    pub fn label(&self) -> &'static str {
        match self {
            AgentKind::Devin => "Devin",
            AgentKind::Codex => "Codex",
            AgentKind::CodexPlus => "Codex",
            AgentKind::CodeBuddy => "CodeBuddy",
            AgentKind::CodeBuddyPlus => "CodeBuddy",
            AgentKind::ClaudeCode => "Claude Code",
            AgentKind::Cursor => "Cursor",
            AgentKind::OpenCode => "OpenCode",
            AgentKind::OpenCodePlus => "OpenCode",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ToolCall {
    pub tool_call_id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default = "default_kind")]
    pub kind: String,
    #[serde(default = "default_status")]
    pub status: String,
    #[serde(default)]
    pub content: Vec<Value>,
    #[serde(default)]
    pub locations: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_input: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_output: Option<Value>,
}

fn default_kind() -> String {
    "other".into()
}
fn default_status() -> String {
    "pending".into()
}

/// 用户随 prompt 带上的附件。图片可带 base64，普通文件走 file:// resource_link。
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PromptImage {
    #[serde(default)]
    pub name: String,
    pub mime_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

/// 单个附件最大内嵌字节数（base64 前）。超过则不跨机器传输，避免撑爆中转。
pub const MAX_EMBED_BYTES: u64 = 25 * 1024 * 1024;

/// 把仅有本地 uri、尚无 base64 内容的附件读成 data（用于漫游/分享跨机器传输）。
/// 读到内容后清掉 uri：对端的本地路径无意义，由对端按 data 重建临时文件。
/// 约定：有 uri = 本机直接可读；只有 data = 跨机器、需落临时文件。
pub fn embed_attachment_data(images: &mut [PromptImage]) {
    use base64::Engine;
    for img in images.iter_mut() {
        if img.data.is_some() {
            // 已内嵌：清掉本机 uri，避免对端误用
            img.uri = None;
            continue;
        }
        let Some(uri) = img.uri.clone() else {
            continue;
        };
        let Some(path) = file_uri_to_local_path(&uri) else {
            continue;
        };
        let Ok(meta) = fs::metadata(&path) else {
            continue;
        };
        if meta.len() > MAX_EMBED_BYTES {
            continue;
        }
        if let Ok(bytes) = fs::read(&path) {
            img.size = Some(meta.len());
            img.data = Some(base64::engine::general_purpose::STANDARD.encode(&bytes));
            img.uri = None;
        }
    }
}

/// 把带 data 的附件落到本机临时文件，保留原始文件名，返回本机绝对路径。
/// 跨机器（漫游/分享）收到的附件用它在本机重建文件，供 agent 读取。
pub fn save_attachment_to_temp(img: &PromptImage) -> Option<String> {
    use base64::Engine;
    let data = img.data.as_ref()?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data.as_bytes())
        .ok()?;
    let dir = std::env::temp_dir()
        .join("Nova-attachments")
        .join(uuid::Uuid::new_v4().to_string());
    fs::create_dir_all(&dir).ok()?;
    let name = sanitize_filename(&img.name);
    let path = dir.join(name);
    fs::write(&path, bytes).ok()?;
    Some(path.to_string_lossy().to_string())
}

fn sanitize_filename(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name).trim();
    let cleaned: String = base
        .chars()
        .map(|c| if "<>:\"/\\|?*".contains(c) { '_' } else { c })
        .collect();
    if cleaned.is_empty() {
        "attachment.bin".into()
    } else {
        cleaned
    }
}

/// 给一组会话条目里的用户附件内嵌内容（用于分享跨机器传输）。
pub fn embed_items_attachments(items: &mut [Item]) {
    for it in items.iter_mut() {
        if let Item::User { images, .. } = it {
            embed_attachment_data(images);
        }
    }
}

/// 解析 file:// uri 为本机绝对路径。
pub fn file_uri_to_local_path(uri: &str) -> Option<String> {
    let raw = uri.strip_prefix("file://")?;
    let decoded = percent_decode(raw);
    #[cfg(windows)]
    {
        Some(decoded.trim_start_matches('/').replace('/', "\\"))
    }
    #[cfg(not(windows))]
    {
        Some(decoded)
    }
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(hex) = std::str::from_utf8(&bytes[i + 1..i + 3]) {
                if let Ok(value) = u8::from_str_radix(hex, 16) {
                    out.push(value);
                    i += 3;
                    continue;
                }
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum Item {
    User {
        id: u64,
        text: String,
        ts: i64,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<PromptImage>,
    },
    Assistant {
        id: u64,
        text: String,
        ts: i64,
    },
    Thought {
        id: u64,
        text: String,
        ts: i64,
    },
    Tool {
        id: u64,
        ts: i64,
        #[serde(flatten)]
        call: ToolCall,
    },
    System {
        id: u64,
        text: String,
        level: String,
        ts: i64,
    },
    /// 轮次结束标记：耗时 + token 用量，前端据此折叠过程并展示用量
    Turn {
        id: u64,
        ts: i64,
        duration_ms: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        total_tokens: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        input_tokens: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        output_tokens: Option<u64>,
        stop_reason: String,
    },
}

impl Item {
    pub fn id(&self) -> u64 {
        match self {
            Item::User { id, .. }
            | Item::Assistant { id, .. }
            | Item::Thought { id, .. }
            | Item::Tool { id, .. }
            | Item::System { id, .. }
            | Item::Turn { id, .. } => *id,
        }
    }
}

/// worktree 执行信息：会话在为某 git 仓库创建的独立 worktree（独立分支 + 工作目录）中运行，
/// 不干扰主工作区正在进行的任务。本地会话的 path 即 thread.cwd；漫游 guest 侧 path 为空
/// （真实工作目录在 host），仅用于展示「这是 worktree 会话」。
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Worktree {
    /// 源 git 仓库根目录
    pub repo: String,
    /// worktree 工作目录（漫游 guest 侧为空）
    #[serde(default)]
    pub path: String,
    /// 为该 worktree 新建的分支名
    pub branch: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Thread {
    pub id: String,
    pub title: String,
    pub cwd: String,
    #[serde(default)]
    pub agent_kind: AgentKind,
    #[serde(default)]
    pub acp_session_id: Option<String>,
    /// 会话使用的模型（None = devin 默认）
    #[serde(default)]
    pub model: Option<String>,
    /// 会话模式：统一为 build / plan（历史数据可能还存着后端原生模式，如 bypass / accept-edits）
    #[serde(default)]
    pub mode: Option<String>,
    /// Codex 思考强度（None = Codex 模型默认）
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    /// 上下文接力：跨 agent 切换时记录切换前的 agent；下一条消息发送时据此把历史
    /// 注入新 agent 的输入，随后清除。None = 无待接力的上下文。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handoff_from: Option<AgentKind>,
    /// 临时会话：程序关闭时自动删除，不跨重启持久化
    #[serde(default)]
    pub ephemeral: bool,
    /// 用户星标：在所在项目内置顶，并豁免自动清理与项目一键删除。
    #[serde(default)]
    pub starred: bool,
    /// 漫游角色：None = 本地会话；Some("host") = 我替别人在本机执行；
    /// Some("guest") = 我在别人机器上执行、本机只接收展示
    #[serde(default)]
    pub roaming_role: Option<String>,
    /// 漫游对端的 token（host 记录 guest，guest 记录 host）
    #[serde(default)]
    pub roaming_peer: Option<String>,
    /// 漫游对端的展示名
    #[serde(default)]
    pub roaming_peer_name: Option<String>,
    /// 漫游对端的线程 id（guest 记录 host 侧线程 id，用于发送/取消路由）
    #[serde(default)]
    pub roaming_remote_id: Option<String>,
    /// 额度租借提供方 token。非空时本机执行，但必须走独立租借进程，禁止回退到本机凭证。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quota_peer: Option<String>,
    /// 额度租借提供方展示名。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quota_peer_name: Option<String>,
    /// 非 None：本会话在独立 git worktree 中执行（cwd 已指向该 worktree 工作目录）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<Worktree>,
    /// 非 None：本会话由数字员工后台产生（记录员工 id）。此类会话不常驻左侧历史，
    /// 仅在「数字员工 / 御书房」里点开时查看。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub employee_id: Option<String>,
    /// Mind 自整理会话：默认不进入数字员工左侧会话列表，仅从关联入口查看。
    #[serde(default)]
    pub mind_thread: bool,
    /// 会话树父节点：用于把数字员工“开工预检”与后续开发会话关联展示。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_thread_id: Option<String>,
    /// 本会话从哪张线索卡发起；后续生成线索时默认以它为当前位置。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_clue_card_id: Option<String>,
    /// 发起会话时固定的证据链上下文快照。节点组是内部结构，快照只包含用户可见卡片。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clue_context: Option<ClueContextSnapshot>,
    pub created_at: i64,
    pub updated_at: i64,
    #[serde(default)]
    pub items: Vec<Item>,
    #[serde(default)]
    pub plan: Option<Value>,
}

impl Thread {
    pub fn new(
        cwd: String,
        agent_kind: AgentKind,
        model: Option<String>,
        mode: Option<String>,
        reasoning_effort: Option<String>,
        ephemeral: bool,
    ) -> Self {
        let now = now_ms();
        Thread {
            id: uuid::Uuid::new_v4().to_string(),
            title: "新会话".into(),
            cwd,
            agent_kind,
            acp_session_id: None,
            model,
            mode,
            reasoning_effort,
            handoff_from: None,
            ephemeral,
            starred: false,
            roaming_role: None,
            roaming_peer: None,
            roaming_peer_name: None,
            roaming_remote_id: None,
            quota_peer: None,
            quota_peer_name: None,
            worktree: None,
            employee_id: None,
            mind_thread: false,
            parent_thread_id: None,
            active_clue_card_id: None,
            clue_context: None,
            created_at: now,
            updated_at: now,
            items: Vec::new(),
            plan: None,
        }
    }

    /// 是否为漫游 guest 会话（本机只接收展示，真正执行在对端）
    pub fn is_roaming_guest(&self) -> bool {
        self.roaming_role.as_deref() == Some("guest")
    }

    pub fn is_quota_borrowed(&self) -> bool {
        self.quota_peer.is_some()
    }

    pub fn next_item_id(&self) -> u64 {
        self.items.iter().map(|i| i.id()).max().unwrap_or(0) + 1
    }

    /// guest 漫游会话本地乐观插入的条目 id 专用高位区间。host 转发来的条目 id 是
    /// 从 1 递增的小数，二者分处不同区间，永不冲突——避免 host 的 upsert/delta 误改
    /// guest 本地的用户消息（表现为「提示词消失 / 块归属错乱 / thinking 串行」）。
    pub fn next_local_item_id(&self) -> u64 {
        const BASE: u64 = 1_000_000_000;
        self.items
            .iter()
            .map(|i| i.id())
            .filter(|id| *id >= BASE)
            .max()
            .unwrap_or(BASE - 1)
            + 1
    }

    pub fn push_user(&mut self, text: String, images: Vec<PromptImage>) -> Item {
        let item = Item::User {
            id: self.next_item_id(),
            text,
            ts: now_ms(),
            images,
        };
        self.items.push(item.clone());
        self.updated_at = now_ms();
        item
    }

    /// guest 漫游会话专用：本地乐观落用户消息，id 取高位区间避免与 host 条目冲突
    pub fn push_user_local(&mut self, text: String, images: Vec<PromptImage>) -> Item {
        let item = Item::User {
            id: self.next_local_item_id(),
            text,
            ts: now_ms(),
            images,
        };
        self.items.push(item.clone());
        self.updated_at = now_ms();
        item
    }

    pub fn push_system(&mut self, text: String, level: &str) -> Item {
        let item = Item::System {
            id: self.next_item_id(),
            text,
            level: level.into(),
            ts: now_ms(),
        };
        self.items.push(item.clone());
        self.updated_at = now_ms();
        item
    }

    /// guest 漫游会话专用：本地系统提示用高位 id，避免被 host 转发条目覆盖
    pub fn push_system_local(&mut self, text: String, level: &str) -> Item {
        let item = Item::System {
            id: self.next_local_item_id(),
            text,
            level: level.into(),
            ts: now_ms(),
        };
        self.items.push(item.clone());
        self.updated_at = now_ms();
        item
    }

    pub fn push_turn(
        &mut self,
        duration_ms: u64,
        usage: Option<&Value>,
        stop_reason: &str,
    ) -> Item {
        let g = |k: &str| usage.and_then(|u| u.get(k)).and_then(|v| v.as_u64());
        let item = Item::Turn {
            id: self.next_item_id(),
            ts: now_ms(),
            duration_ms,
            total_tokens: g("totalTokens"),
            input_tokens: g("inputTokens"),
            output_tokens: g("outputTokens"),
            stop_reason: stop_reason.into(),
        };
        self.items.push(item.clone());
        self.updated_at = now_ms();
        item
    }

    /// 取出并清除「上下文接力」标记，渲染出注入给新 agent 的历史上下文。
    /// 无标记或无可用历史时返回 None。
    pub fn take_handoff_context(&mut self, to_label: &str) -> Option<String> {
        let from = self.handoff_from.take()?;
        render_handoff_context(&self.items, self.plan.as_ref(), from.label(), to_label)
    }

    /// 当前 prompt 需要额外注入的上下文：
    /// - 从线索发起的新会话首条消息注入固定 Paper Trail；
    /// - 跨 Agent 接力时再次携带该 Trail，并拼接原有会话历史。
    pub fn take_prompt_context(&mut self, to_label: &str) -> Option<String> {
        let has_user_message = self
            .items
            .iter()
            .any(|item| matches!(item, Item::User { .. }));
        let has_handoff = self.handoff_from.is_some();
        let clue = if !has_user_message || has_handoff {
            self.clue_context
                .as_ref()
                .map(|context| context.rendered_context.trim().to_string())
                .filter(|context| !context.is_empty())
        } else {
            None
        };
        let handoff = self.take_handoff_context(to_label);
        match (clue, handoff) {
            (Some(clue), Some(handoff)) => Some(format!("{clue}\n\n{handoff}")),
            (Some(clue), None) => Some(clue),
            (None, Some(handoff)) => Some(handoff),
            (None, None) => None,
        }
    }
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ThreadMeta {
    pub id: String,
    pub title: String,
    pub cwd: String,
    pub agent_kind: AgentKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub running: bool,
    #[serde(default)]
    pub ephemeral: bool,
    pub starred: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roaming_role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roaming_peer_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quota_peer_name: Option<String>,
    /// 非 None：该会话在独立 git worktree 中执行（前端据此显示分支标记）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<Worktree>,
    /// 非 None：该会话由数字员工后台产生（前端据此不在左侧历史列表展示）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub employee_id: Option<String>,
    /// Mind 自整理会话：默认不进入数字员工左侧会话列表。
    #[serde(default)]
    pub mind_thread: bool,
    /// 会话树父节点：用于把预检会话与后续开发会话关联展示。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_clue_card_id: Option<String>,
}

/// 最近项目列表（独立持久化，删除会话后仍保留）
#[derive(Serialize, Deserialize, Default)]
struct ProjectsFile {
    projects: Vec<String>,
}

pub struct ProjectStore {
    path: PathBuf,
    pub projects: Vec<String>,
}

impl ProjectStore {
    pub fn load(dir: &PathBuf) -> Self {
        let path = dir.join("projects.json");
        let projects = fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<ProjectsFile>(&s).ok())
            .map(|f| f.projects)
            .unwrap_or_default();
        ProjectStore { path, projects }
    }

    pub fn save(&self) {
        if let Some(parent) = self.path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let file = ProjectsFile {
            projects: self.projects.clone(),
        };
        if let Ok(json) = serde_json::to_string_pretty(&file) {
            let _ = fs::write(&self.path, json);
        }
    }

    /// 把项目移到最前（不存在则插入），保留最多 20 条
    pub fn touch(&mut self, cwd: &str) {
        self.projects.retain(|p| p != cwd);
        self.projects.insert(0, cwd.to_string());
        self.projects.truncate(20);
        self.save();
    }

    pub fn remove(&mut self, cwd: &str) {
        self.projects.retain(|p| p != cwd);
        self.save();
    }
}

/// 「允许漫游」的本地目录列表（其他漫游用户可在这些目录上新建会话、在本机执行）
#[derive(Serialize, Deserialize, Default)]
struct RoamingFile {
    folders: Vec<String>,
}

pub struct RoamingStore {
    path: PathBuf,
    pub folders: Vec<String>,
}

impl RoamingStore {
    pub fn load(dir: &PathBuf) -> Self {
        let path = dir.join("roaming.json");
        let folders = fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<RoamingFile>(&s).ok())
            .map(|f| f.folders)
            .unwrap_or_default();
        RoamingStore { path, folders }
    }

    pub fn save(&self) {
        if let Some(parent) = self.path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let file = RoamingFile {
            folders: self.folders.clone(),
        };
        if let Ok(json) = serde_json::to_string_pretty(&file) {
            let _ = fs::write(&self.path, json);
        }
    }

    pub fn is_allowed(&self, cwd: &str) -> bool {
        self.folders.iter().any(|f| f == cwd)
    }

    /// 切换某目录的漫游开关，返回切换后的状态（true = 现在允许）
    pub fn toggle(&mut self, cwd: &str) -> bool {
        if self.is_allowed(cwd) {
            self.folders.retain(|f| f != cwd);
            self.save();
            false
        } else {
            self.folders.push(cwd.to_string());
            self.save();
            true
        }
    }

    pub fn set(&mut self, cwd: &str, allowed: bool) {
        let has = self.is_allowed(cwd);
        if allowed && !has {
            self.folders.push(cwd.to_string());
            self.save();
        } else if !allowed && has {
            self.folders.retain(|f| f != cwd);
            self.save();
        }
    }
}

/// 一条已创建的 worktree 记录。独立持久化（worktrees.json）：会话删除后记录仍保留，
/// 由设置里的 Worktree 面板手动清理，避免误删用户未合并的改动。
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeRecord {
    pub id: String,
    /// 源 git 仓库根目录
    pub repo: String,
    /// worktree 工作目录
    pub path: String,
    /// 分支名
    pub branch: String,
    /// 关联的会话 id（会话被删后置为悬空记录，仍可在面板里清理）
    #[serde(default)]
    pub thread_id: Option<String>,
    /// 是否为漫游 host 侧代建
    #[serde(default)]
    pub roaming: bool,
    /// 分支是否由 Nova 新建（-b）；false = 直接检出用户已有分支，移除 worktree 时不删分支。
    /// 旧记录缺省 true（历史上一律新建分支）。
    #[serde(default = "default_true")]
    pub owned_branch: bool,
    pub created_at: i64,
}

fn default_true() -> bool {
    true
}

#[derive(Serialize, Deserialize, Default)]
struct WorktreeFile {
    worktrees: Vec<WorktreeRecord>,
}

pub struct WorktreeStore {
    path: PathBuf,
    pub worktrees: Vec<WorktreeRecord>,
}

impl WorktreeStore {
    pub fn load(dir: &PathBuf) -> Self {
        let path = dir.join("worktrees.json");
        let worktrees = fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<WorktreeFile>(&s).ok())
            .map(|f| f.worktrees)
            .unwrap_or_default();
        WorktreeStore { path, worktrees }
    }

    pub fn save(&self) {
        if let Some(parent) = self.path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let file = WorktreeFile {
            worktrees: self.worktrees.clone(),
        };
        if let Ok(json) = serde_json::to_string_pretty(&file) {
            let _ = fs::write(&self.path, json);
        }
    }

    pub fn add(&mut self, rec: WorktreeRecord) {
        self.worktrees.push(rec);
        self.save();
    }

    pub fn get(&self, id: &str) -> Option<&WorktreeRecord> {
        self.worktrees.iter().find(|w| w.id == id)
    }

    pub fn remove(&mut self, id: &str) -> Option<WorktreeRecord> {
        let pos = self.worktrees.iter().position(|w| w.id == id)?;
        let rec = self.worktrees.remove(pos);
        self.save();
        Some(rec)
    }

    pub fn list(&self) -> Vec<WorktreeRecord> {
        self.worktrees.clone()
    }
}

#[derive(Serialize, Deserialize, Default)]
struct StoreFile {
    threads: Vec<Thread>,
}

/// 序列化用的借用视图：避免 save 时把全部会话（可达数十 MB）深拷贝一遍
#[derive(Serialize)]
struct StoreFileRef<'a> {
    threads: &'a [Thread],
}

#[derive(Serialize, Deserialize, Default)]
struct ThreadTrashFile {
    entries: Vec<TrashedThread>,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct TrashedThread {
    pub thread: Thread,
    pub trashed_at: i64,
}

/// 自动清理会话的延迟删除回收站。回收站独立落在 `thread-trash.json`，不参与正常会话列表。
pub struct ThreadTrashStore {
    path: PathBuf,
    entries: Vec<TrashedThread>,
}

impl ThreadTrashStore {
    pub fn load(dir: &PathBuf) -> Self {
        let path = dir.join("thread-trash.json");
        let entries = fs::read_to_string(&path)
            .ok()
            .and_then(|json| serde_json::from_str::<ThreadTrashFile>(&json).ok())
            .map(|file| file.entries)
            .unwrap_or_default();
        Self { path, entries }
    }

    fn save(&self) -> Result<(), String> {
        let json = serde_json::to_string(&ThreadTrashFile {
            entries: self.entries.clone(),
        })
        .map_err(|error| error.to_string())?;
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        let tmp = self.path.with_extension("json.tmp");
        fs::write(&tmp, json).map_err(|error| error.to_string())?;
        fs::rename(tmp, &self.path).map_err(|error| error.to_string())
    }

    /// 先持久化到回收站，再允许调用方从正常会话存储移除，优先保证不会直接丢失会话。
    pub fn move_to_trash(&mut self, threads: Vec<Thread>, trashed_at: i64) -> Result<(), String> {
        let len = self.entries.len();
        self.entries.extend(
            threads
                .into_iter()
                .map(|thread| TrashedThread { thread, trashed_at }),
        );
        if let Err(error) = self.save() {
            self.entries.truncate(len);
            return Err(error);
        }
        Ok(())
    }

    /// 回收站内停留满同一个保留周期后才彻底清除，返回需要一并删除工作目录的会话。
    pub fn purge_expired(&mut self, now: i64, hours: u32) -> Vec<Thread> {
        let timeout_ms = i64::from(hours.max(1)).saturating_mul(60 * 60 * 1000);
        let cutoff = now.saturating_sub(timeout_ms);
        let (expired, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut self.entries)
            .into_iter()
            .partition(|entry| entry.trashed_at < cutoff);
        self.entries = kept;
        if self.save().is_err() {
            self.entries.extend(expired);
            return Vec::new();
        }
        expired.into_iter().map(|entry| entry.thread).collect()
    }
}

pub struct ThreadStore {
    path: PathBuf,
    pub threads: Vec<Thread>,
    /// 有未落盘的修改。save() 只置位，由后台 flusher（lib.rs）节流合并写盘。
    dirty: Arc<AtomicBool>,
    /// 唤醒后台 flusher（notify_one 在无等待者时会存一个 permit，不丢通知）
    save_notify: Arc<Notify>,
}

impl ThreadStore {
    pub fn load(dir: PathBuf) -> Self {
        let path = dir.join("threads.json");
        let threads = fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<StoreFile>(&s).ok())
            .map(|f| f.threads)
            .unwrap_or_default();
        let mut store = ThreadStore {
            path,
            threads,
            dirty: Arc::new(AtomicBool::new(false)),
            save_notify: Arc::new(Notify::new()),
        };
        // 上次进程未正常退出时残留的临时会话，启动时一并清掉
        if !store.purge_ephemeral().is_empty() {
            store.save();
        }
        store
    }

    /// 删除所有临时会话，返回被删掉的会话（调用方负责清理其工作目录等）
    pub fn purge_ephemeral(&mut self) -> Vec<Thread> {
        let (ephemeral, kept): (Vec<Thread>, Vec<Thread>) = std::mem::take(&mut self.threads)
            .into_iter()
            .partition(|t| t.ephemeral);
        self.threads = kept;
        ephemeral
    }

    /// 请求持久化（异步节流）。
    ///
    /// 这里只置脏标记并唤醒后台 flusher，不做任何序列化/IO：save 的调用点遍布流式
    /// 热路径（每个工具完成、每条消息、每次 plan 更新……），而 threads.json 会随
    /// 历史增长到数十 MB——旧实现每次全量 clone + pretty 序列化 + 同步写盘，一轮任务
    /// 触发上百次，既把流式输出卡出明显顿挫，又因反复大块分配造成堆碎片、内存阶梯上涨。
    pub fn save(&self) {
        self.dirty.store(true, Ordering::Release);
        self.save_notify.notify_one();
    }

    /// 后台 flusher 用：取走脏标记（返回 true 表示需要写盘）
    pub fn take_dirty(&self) -> bool {
        self.dirty.swap(false, Ordering::AcqRel)
    }

    /// 后台 flusher 用：save() 的唤醒器句柄
    pub fn save_notify_handle(&self) -> Arc<Notify> {
        self.save_notify.clone()
    }

    /// 序列化当前全部会话为紧凑 JSON（借用序列化，不 clone；写盘由调用方在锁外执行）
    pub fn serialize_json(&self) -> Option<String> {
        serde_json::to_string(&StoreFileRef {
            threads: &self.threads,
        })
        .ok()
    }

    pub fn file_path(&self) -> PathBuf {
        self.path.clone()
    }

    /// 原子写盘：先写临时文件再改名，避免中途崩溃留下半个文件
    pub fn write_json(path: &std::path::Path, json: &str) {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let tmp = path.with_extension("json.tmp");
        if fs::write(&tmp, json).is_ok() {
            let _ = fs::rename(&tmp, path);
        }
    }

    /// 立即同步落盘（进程退出/升级重启前的最终保存），并清除脏标记
    pub fn save_now(&self) {
        self.dirty.store(false, Ordering::Release);
        if let Some(json) = self.serialize_json() {
            Self::write_json(&self.path, &json);
        }
    }

    pub fn get(&self, id: &str) -> Option<&Thread> {
        self.threads.iter().find(|t| t.id == id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut Thread> {
        self.threads.iter_mut().find(|t| t.id == id)
    }

    pub fn clear_active_clue_card(&mut self, card_id: &str) -> bool {
        let mut changed = false;
        for thread in &mut self.threads {
            if thread.active_clue_card_id.as_deref() == Some(card_id) {
                thread.active_clue_card_id = None;
                changed = true;
            }
        }
        changed
    }

    pub fn thread_by_session_mut(&mut self, session_id: &str) -> Option<&mut Thread> {
        self.threads
            .iter_mut()
            .find(|t| t.acp_session_id.as_deref() == Some(session_id))
    }
}

// ===== 上下文接力：把已有会话历史渲染成注入给新 agent 的上下文 =====

const HANDOFF_USER_MAX: usize = 4000;
const HANDOFF_ASSISTANT_MAX: usize = 6000;
const HANDOFF_TOOL_OUTPUT_MAX: usize = 800;
const HANDOFF_TOTAL_BUDGET: usize = 48000;

/// 跨 agent 切换时，把按时间排列的会话条目 + 计划进度渲染成一段上下文文本，
/// 供新 agent 接续。无可用内容时返回 None。
pub fn render_handoff_context(
    items: &[Item],
    plan: Option<&Value>,
    from_label: &str,
    to_label: &str,
) -> Option<String> {
    let mut blocks: Vec<String> = Vec::new();
    for it in items {
        match it {
            Item::User { text, images, .. } => {
                let mut t = truncate_middle(text.trim(), HANDOFF_USER_MAX);
                if !images.is_empty() {
                    t.push_str(&format!("（另附 {} 个文件/图片）", images.len()));
                }
                if !t.trim().is_empty() {
                    blocks.push(format!("用户：\n{t}"));
                }
            }
            Item::Assistant { text, .. } => {
                let t = truncate_middle(text.trim(), HANDOFF_ASSISTANT_MAX);
                if !t.trim().is_empty() {
                    blocks.push(format!("{from_label}：\n{t}"));
                }
            }
            Item::Tool { call, .. } => {
                blocks.push(render_handoff_tool(call));
            }
            Item::System { text, level, .. } if level == "error" => {
                let t = truncate_middle(text.trim(), 600);
                if !t.trim().is_empty() {
                    blocks.push(format!("系统错误：{t}"));
                }
            }
            _ => {}
        }
    }
    let plan_text = render_handoff_plan(plan);
    if blocks.is_empty() && plan_text.is_empty() {
        return None;
    }
    let body = clip_blocks_to_budget(&blocks, HANDOFF_TOTAL_BUDGET);
    let mut out = String::new();
    out.push_str(&format!(
        "［上下文接力］本会话此前由 {from_label} 处理，现在改由你（{to_label}）接手。\
下面是按时间顺序的对话与操作记录，请通读后在此基础上继续完成用户的任务：不要重复已经做过的工作；\
涉及的文件请以工作目录中的当前实际内容为准，必要时用读取工具确认。\n\n"
    ));
    out.push_str("========== 历史记录开始 ==========\n");
    out.push_str(&body);
    out.push_str("\n========== 历史记录结束 ==========");
    out.push_str(&plan_text);
    out.push_str("\n\n以上为历史背景，请据此理解上下文，并回应用户接下来的消息。");
    Some(out)
}

fn render_handoff_plan(plan: Option<&Value>) -> String {
    let Some(arr) = plan.and_then(|p| p.as_array()) else {
        return String::new();
    };
    let mut lines: Vec<String> = Vec::new();
    for e in arr {
        let content = e["content"].as_str().unwrap_or("").trim();
        if content.is_empty() {
            continue;
        }
        let mark = match e["status"].as_str().unwrap_or("pending") {
            "completed" => "[完成]",
            "in_progress" => "[进行中]",
            "interrupted" | "cancelled" => "[已中断]",
            _ => "[待办]",
        };
        lines.push(format!("{mark} {content}"));
    }
    if lines.is_empty() {
        String::new()
    } else {
        format!("\n\n【此前的计划进度】\n{}", lines.join("\n"))
    }
}

fn render_handoff_tool(call: &ToolCall) -> String {
    let title = if call.title.trim().is_empty() {
        call.kind.as_str()
    } else {
        call.title.trim()
    };
    let mut s = format!("[工具] {title}");
    let status = match call.status.as_str() {
        "completed" => "完成",
        "failed" => "失败",
        "in_progress" => "进行中",
        "pending" => "待执行",
        _ => "",
    };
    if !status.is_empty() {
        s.push_str(&format!("（{status}）"));
    }
    let paths: Vec<String> = call
        .locations
        .iter()
        .filter_map(|l| l["path"].as_str().map(|p| p.to_string()))
        .collect();
    if !paths.is_empty() {
        s.push_str(&format!("\n  涉及文件：{}", paths.join("、")));
    }
    let out = tool_output_text(&call.content);
    let out = out.trim();
    if !out.is_empty() {
        s.push_str(&format!(
            "\n  输出：{}",
            truncate_middle(out, HANDOFF_TOOL_OUTPUT_MAX)
        ));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handoff_to_opencode_preserves_history_once() {
        let mut thread = Thread::new(
            "D:/project".into(),
            AgentKind::Devin,
            None,
            None,
            None,
            false,
        );
        thread.push_user("请修复这个问题".into(), Vec::new());
        thread.items.push(Item::Assistant {
            id: thread.next_item_id(),
            text: "已经定位到旧会话状态。".into(),
            ts: now_ms(),
        });
        thread.agent_kind = AgentKind::OpenCode;
        thread.acp_session_id = None;
        thread.handoff_from = Some(AgentKind::Devin);

        let context = thread
            .take_prompt_context("OpenCode")
            .expect("跨后端切换应把已有历史交给 OpenCode");

        assert!(context.contains("此前由 Devin 处理，现在改由你（OpenCode）接手"));
        assert!(context.contains("用户：\n请修复这个问题"));
        assert!(context.contains("Devin：\n已经定位到旧会话状态。"));
        assert_eq!(thread.handoff_from, None);
        assert!(thread.take_prompt_context("OpenCode").is_none());
    }

    #[test]
    fn edited_opencode_prompt_replays_retained_history_once() {
        let mut thread = Thread::new(
            "D:/project".into(),
            AgentKind::OpenCode,
            None,
            None,
            None,
            false,
        );
        thread.push_user("先定位问题".into(), Vec::new());
        thread.items.push(Item::Assistant {
            id: thread.next_item_id(),
            text: "问题位于会话恢复逻辑。".into(),
            ts: now_ms(),
        });
        thread.handoff_from = Some(AgentKind::OpenCode);

        let context = thread
            .take_prompt_context("OpenCode")
            .expect("重编辑后应把截断点前的历史交给新的 OpenCode 会话");

        assert!(context.contains("用户：\n先定位问题"));
        assert!(context.contains("OpenCode：\n问题位于会话恢复逻辑。"));
        assert!(thread.take_prompt_context("OpenCode").is_none());

        thread.items.clear();
        thread.handoff_from = None;
        assert!(thread.take_prompt_context("OpenCode").is_none());
    }

    #[test]
    fn legacy_thread_without_starred_defaults_to_false() {
        let thread = Thread::new(String::new(), AgentKind::Devin, None, None, None, false);
        let mut value = serde_json::to_value(thread).expect("线程应可序列化");
        value
            .as_object_mut()
            .expect("线程应序列化为对象")
            .remove("starred");

        let restored: Thread = serde_json::from_value(value).expect("旧线程应可反序列化");
        assert!(!restored.starred);
    }
}

fn tool_output_text(content: &[Value]) -> String {
    let mut parts: Vec<String> = Vec::new();
    for c in content {
        match c["type"].as_str() {
            Some("content") => {
                if let Some(t) = c["content"]["text"].as_str() {
                    if !t.trim().is_empty() {
                        parts.push(t.to_string());
                    }
                }
            }
            Some("diff") => {
                if let Some(p) = c["path"].as_str() {
                    parts.push(format!("（修改文件 {p}）"));
                }
            }
            _ => {}
        }
    }
    parts.join("\n")
}

/// 按字符数从中间截断，保留首尾，避免单条过长撑爆预算
fn truncate_middle(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        return s.to_string();
    }
    let head = max * 2 / 3;
    let tail = max.saturating_sub(head);
    let omitted = chars.len() - head - tail;
    let mut out: String = chars[..head].iter().collect();
    out.push_str(&format!("\n…〔省略 {omitted} 字〕…\n"));
    out.extend(chars[chars.len() - tail..].iter());
    out
}

/// 控制总长度：超预算时保留首条（最初的用户需求）+ 从尾部尽量多保留最近记录
fn clip_blocks_to_budget(blocks: &[String], budget: usize) -> String {
    let sep = 2usize; // "\n\n"
    let total: usize = blocks.iter().map(|b| b.chars().count() + sep).sum();
    if total <= budget {
        return blocks.join("\n\n");
    }
    let first = blocks.first().cloned().unwrap_or_default();
    let mut used = first.chars().count() + sep;
    let mut tail: Vec<String> = Vec::new();
    for b in blocks.iter().skip(1).rev() {
        let cost = b.chars().count() + sep;
        if used + cost > budget {
            break;
        }
        used += cost;
        tail.push(b.clone());
    }
    tail.reverse();
    let omitted = blocks.len().saturating_sub(1).saturating_sub(tail.len());
    let mut parts: Vec<String> = vec![first];
    if omitted > 0 {
        parts.push(format!("…〔为控制长度，省略中间 {omitted} 条记录〕…"));
    }
    parts.extend(tail);
    parts.join("\n\n")
}
