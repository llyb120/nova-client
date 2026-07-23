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

/// 自动清理只累计本地时间周一至周五的时长，周六、周日暂停计时。
pub fn session_cleanup_is_expired(timestamp: i64, now: i64, hours: u32) -> bool {
    use chrono::{Datelike, TimeZone};

    let timeout_ms = i64::from(hours.max(1)).saturating_mul(60 * 60 * 1000);
    let Some(start) = chrono::Local.timestamp_millis_opt(timestamp).single() else {
        return timestamp < now.saturating_sub(timeout_ms);
    };
    let Some(end) = chrono::Local.timestamp_millis_opt(now).single() else {
        return timestamp < now.saturating_sub(timeout_ms);
    };
    let mut cursor = start.naive_local();
    let end = end.naive_local();
    let mut elapsed_ms = 0i64;

    while cursor < end {
        let next_midnight = cursor
            .date()
            .succ_opt()
            .and_then(|date| date.and_hms_opt(0, 0, 0))
            .unwrap_or(end);
        let segment_end = end.min(next_midnight);
        if !matches!(
            cursor.weekday(),
            chrono::Weekday::Sat | chrono::Weekday::Sun
        ) {
            elapsed_ms = elapsed_ms.saturating_add((segment_end - cursor).num_milliseconds());
        }
        cursor = segment_end;
    }

    elapsed_ms > timeout_ms
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum AgentKind {
    Alkaid,
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
            AgentKind::Alkaid => "alkaid",
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
            "alkaid" => Some(AgentKind::Alkaid),
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
            AgentKind::Alkaid => "Vega",
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

pub(crate) fn percent_decode(s: &str) -> String {
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
        /// Auto 模式在本轮实际路由到的模型及推理档位。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        actual_model: Option<String>,
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

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProviderCheckpoint {
    pub user_item_id: u64,
    pub agent_kind: AgentKind,
    pub session_id: String,
    pub position: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PendingNativeRestore {
    pub session_id: String,
    pub position: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CodexUsageSnapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub input_tokens: u64,
    pub output_tokens: u64,
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
    /// Auto 模式首次查询后缓存的实际模型 value；后续轮次不再抓取雷达。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_routed_model: Option<String>,
    /// 与缓存对应的 Auto 选项（按性价比 / 按智商）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_route_selection: Option<String>,
    /// 会话和轮次中展示的实际模型名称。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_routed_label: Option<String>,
    /// 上下文接力：跨 agent 切换时记录切换前的 agent；下一条消息发送时据此把历史
    /// 注入新 agent 的输入，随后清除。None = 无待接力的上下文。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handoff_from: Option<AgentKind>,
    /// 支持历史节点分叉的 SDK 后端在每轮完成后记录的远端位置。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provider_checkpoints: Vec<ProviderCheckpoint>,
    /// Claude / CodeBuddy 在下一条 prompt 启动时执行的原生分叉位置。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_native_restore: Option<PendingNativeRestore>,
    /// Codex SDK 返回会话累计量；保留上次快照以换算本轮增量。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex_usage_snapshot: Option<CodexUsageSnapshot>,
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
    pub fn cached_auto_model(&self, selection: &str) -> Option<String> {
        (self.auto_route_selection.as_deref() == Some(selection))
            .then(|| self.auto_routed_model.clone())
            .flatten()
    }

    pub fn clear_auto_route(&mut self) {
        self.auto_routed_model = None;
        self.auto_route_selection = None;
        self.auto_routed_label = None;
    }

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
            auto_routed_model: None,
            auto_route_selection: None,
            auto_routed_label: None,
            handoff_from: None,
            provider_checkpoints: Vec::new(),
            pending_native_restore: None,
            codex_usage_snapshot: None,
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

    pub fn record_provider_checkpoint(
        &mut self,
        user_item_id: u64,
        session_id: String,
        position: String,
    ) {
        self.provider_checkpoints.retain(|checkpoint| {
            checkpoint.user_item_id != user_item_id || checkpoint.agent_kind != self.agent_kind
        });
        self.provider_checkpoints.push(ProviderCheckpoint {
            user_item_id,
            agent_kind: self.agent_kind.clone(),
            session_id,
            position,
        });
    }

    pub fn checkpoint_before(&self, item_id: u64) -> Option<ProviderCheckpoint> {
        self.provider_checkpoints
            .iter()
            .filter(|checkpoint| {
                checkpoint.user_item_id < item_id && checkpoint.agent_kind == self.agent_kind
            })
            .max_by_key(|checkpoint| checkpoint.user_item_id)
            .cloned()
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
            actual_model: self
                .model
                .as_deref()
                .filter(|model| model.starts_with("__nova_auto_"))
                .and_then(|_| self.auto_routed_label.clone()),
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
        let (expired, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut self.entries)
            .into_iter()
            .partition(|entry| session_cleanup_is_expired(entry.trashed_at, now, hours));
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
const HANDOFF_FULL_TURNS: usize = 10;
const HANDOFF_CONTEXT_THRESHOLD: usize = HANDOFF_TOTAL_BUDGET * 4 / 5;

/// 跨 agent 切换时，把按时间排列的会话条目 + 计划进度渲染成一段上下文文本，
/// 供新 agent 接续。无可用内容时返回 None。
pub fn render_handoff_context(
    items: &[Item],
    plan: Option<&Value>,
    from_label: &str,
    to_label: &str,
) -> Option<String> {
    let mut blocks: Vec<String> = Vec::new();
    let mut slim_blocks: Vec<String> = Vec::new();
    let mut turn_users: Vec<String> = Vec::new();
    let mut turn_conclusion: Option<String> = None;
    let mut completed_turns = 0;
    // Everything after the last normally completed turn is an unfinished native trajectory.
    // Under the handoff budget it takes priority over old completed history, so a cancelled Vega
    // or Cursor turn keeps its assistant/tool state instead of looking like a new conversation.
    let unfinished_item_start = items
        .iter()
        .rposition(|item| {
            matches!(
                item,
                Item::Turn { stop_reason, .. }
                    if matches!(stop_reason.as_str(), "end_turn" | "max_turn_requests")
            )
        })
        .map_or(0, |index| index + 1);
    let mut unfinished_block_start = None;
    for (index, it) in items.iter().enumerate() {
        if index == unfinished_item_start {
            unfinished_block_start = Some(blocks.len());
            // Any prompts not closed by a successful Turn marker belong to the native unfinished
            // trajectory and must not also appear in completed slim memory.
            turn_users.clear();
            turn_conclusion = None;
        }
        match it {
            Item::User { text, images, .. } => {
                let mut t = truncate_middle(text.trim(), HANDOFF_USER_MAX);
                if !images.is_empty() {
                    t.push_str(&format!("（另附 {} 个文件/图片）", images.len()));
                }
                if !t.trim().is_empty() {
                    let block = format!("用户：\n{t}");
                    blocks.push(block.clone());
                    turn_users.push(block);
                }
            }
            Item::Assistant { text, .. } => {
                let t = truncate_middle(text.trim(), HANDOFF_ASSISTANT_MAX);
                if !t.trim().is_empty() {
                    let block = format!("{from_label}：\n{t}");
                    blocks.push(block.clone());
                    turn_conclusion = Some(block);
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
            Item::Turn { stop_reason, .. }
                if matches!(stop_reason.as_str(), "end_turn" | "max_turn_requests") =>
            {
                slim_blocks.append(&mut turn_users);
                if let Some(conclusion) = turn_conclusion.take() {
                    slim_blocks.push(conclusion);
                }
                completed_turns += 1;
            }
            _ => {}
        }
    }
    let plan_text = render_handoff_plan(plan);
    if blocks.is_empty() && plan_text.is_empty() {
        return None;
    }
    let native_chars = blocks.iter().map(String::len).sum::<usize>();
    let use_full_context = completed_turns < HANDOFF_FULL_TURNS
        && native_chars < HANDOFF_CONTEXT_THRESHOLD;
    let body = if use_full_context {
        clip_blocks_to_budget(
            &blocks,
            HANDOFF_TOTAL_BUDGET,
            unfinished_block_start.filter(|start| *start < blocks.len()),
        )
    } else {
        // Stage one keeps completed prompts and conclusions but drops their tool trajectory.
        // Unfinished work remains native. Stage two only clips old compact turns after this
        // prompt/conclusion representation reaches the same 80% capacity threshold.
        let slim_completed_count = slim_blocks.len();
        slim_blocks.extend(blocks.iter().skip(unfinished_block_start.unwrap_or(blocks.len())).cloned());
        let slim_chars = slim_blocks.iter().map(String::len).sum::<usize>();
        let budget = if slim_chars >= HANDOFF_CONTEXT_THRESHOLD {
            HANDOFF_CONTEXT_THRESHOLD
        } else {
            HANDOFF_TOTAL_BUDGET
        };
        clip_blocks_to_budget(
            &slim_blocks,
            budget,
            (slim_completed_count < slim_blocks.len()).then_some(slim_completed_count),
        )
    };
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
    fn auto_route_cache_is_reused_and_written_to_turns() {
        let mut thread = Thread::new(
            "D:/project".into(),
            AgentKind::Codex,
            Some("__nova_auto_iq__".into()),
            None,
            None,
            false,
        );
        thread.auto_route_selection = Some("__nova_auto_iq__".into());
        thread.auto_routed_model = Some("gpt-5.6-sol:max".into());
        thread.auto_routed_label = Some("gpt-5.6-sol · max".into());

        assert_eq!(
            thread.cached_auto_model("__nova_auto_iq__").as_deref(),
            Some("gpt-5.6-sol:max")
        );
        assert!(thread.cached_auto_model("__nova_auto_value__").is_none());
        let turn = thread.push_turn(1, None, "end_turn");
        assert!(matches!(
            turn,
            Item::Turn {
                actual_model: Some(model),
                ..
            } if model == "gpt-5.6-sol · max"
        ));
    }

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
    fn handoff_uses_two_stage_context_compaction() {
        let mut items = Vec::new();
        for index in 1..=10 {
            items.push(Item::User {
                id: index * 4,
                text: format!("提示 {index}"),
                ts: now_ms(),
                images: Vec::new(),
            });
            items.push(Item::Tool {
                id: index * 4 + 1,
                ts: now_ms(),
                call: ToolCall {
                    tool_call_id: format!("tool-{index}"),
                    title: "read".into(),
                    kind: "read".into(),
                    status: "completed".into(),
                    content: vec![serde_json::json!({ "type": "content", "text": "工具轨迹" })],
                    locations: Vec::new(),
                    raw_input: None,
                    raw_output: None,
                },
            });
            items.push(Item::Assistant {
                id: index * 4 + 2,
                text: format!("结论 {index}"),
                ts: now_ms(),
            });
            items.push(Item::Turn {
                id: index * 4 + 3,
                ts: now_ms(),
                duration_ms: 1,
                total_tokens: None,
                input_tokens: None,
                output_tokens: None,
                actual_model: None,
                stop_reason: "end_turn".into(),
            });
        }

        let context = render_handoff_context(&items, None, "Vega", "Cursor").unwrap();

        assert!(context.contains("提示 1"));
        assert!(context.contains("结论 10"));
        assert!(!context.contains("工具轨迹"));
    }

    #[test]
    fn handoff_budget_prioritizes_an_interrupted_tool_trajectory() {
        let old = "旧轮已完成".repeat(HANDOFF_TOTAL_BUDGET);
        let blocks = vec![
            format!("用户：\n{old}"),
            "旧轮结论".into(),
            "用户：\n请继续未完成任务".into(),
            "[工具] read（完成）\n  输出：关键文件内容".into(),
        ];

        let context = clip_blocks_to_budget(&blocks, 120, Some(2));

        assert!(!context.contains("旧轮已完成"));
        assert!(context.contains("请继续未完成任务"));
        assert!(context.contains("[工具] read"));
        assert!(context.contains("关键文件内容"));
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
    fn checkpoint_before_uses_latest_position_from_current_backend() {
        let mut thread = Thread::new(
            "D:/project".into(),
            AgentKind::OpenCode,
            None,
            None,
            None,
            false,
        );
        thread.record_provider_checkpoint(1, "open-session".into(), "message-1".into());
        thread.record_provider_checkpoint(5, "open-session".into(), "message-5".into());
        thread.agent_kind = AgentKind::ClaudeCode;
        thread.record_provider_checkpoint(3, "claude-session".into(), "message-3".into());

        assert_eq!(
            thread.checkpoint_before(5),
            Some(ProviderCheckpoint {
                user_item_id: 3,
                agent_kind: AgentKind::ClaudeCode,
                session_id: "claude-session".into(),
                position: "message-3".into(),
            })
        );
        assert!(thread.checkpoint_before(1).is_none());
    }

    #[test]
    fn recording_checkpoint_replaces_same_turn_position() {
        let mut thread = Thread::new(
            "D:/project".into(),
            AgentKind::CodeBuddy,
            None,
            None,
            None,
            false,
        );
        thread.record_provider_checkpoint(2, "old-session".into(), "old-message".into());
        thread.record_provider_checkpoint(2, "new-session".into(), "new-message".into());

        assert_eq!(thread.provider_checkpoints.len(), 1);
        assert_eq!(thread.provider_checkpoints[0].session_id, "new-session");
        assert_eq!(thread.provider_checkpoints[0].position, "new-message");
    }

    #[test]
    fn legacy_thread_defaults_new_persistence_fields() {
        let thread = Thread::new(String::new(), AgentKind::Devin, None, None, None, false);
        let mut value = serde_json::to_value(thread).expect("线程应可序列化");
        let object = value.as_object_mut().expect("线程应序列化为对象");
        object.remove("starred");
        object.remove("providerCheckpoints");
        object.remove("pendingNativeRestore");
        object.remove("codexUsageSnapshot");

        let restored: Thread = serde_json::from_value(value).expect("旧线程应可反序列化");
        assert!(!restored.starred);
        assert!(restored.provider_checkpoints.is_empty());
        assert!(restored.pending_native_restore.is_none());
        assert!(restored.codex_usage_snapshot.is_none());
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

/// 控制总长度：通常保留首条需求和最近记录；若尾部是未完成轮次，则优先完整保留该轮的
/// 用户、assistant 与工具块，剩余预算才用于更早历史。
fn clip_blocks_to_budget(
    blocks: &[String],
    budget: usize,
    protected_tail_start: Option<usize>,
) -> String {
    let sep = 2usize; // "\n\n"
    let total: usize = blocks.iter().map(|block| block.chars().count() + sep).sum();
    if total <= budget {
        return blocks.join("\n\n");
    }

    let protected_start = protected_tail_start.unwrap_or(blocks.len());
    let protected_cost: usize = blocks[protected_start..]
        .iter()
        .map(|block| block.chars().count() + sep)
        .sum();
    if protected_start < blocks.len() && protected_cost <= budget {
        let mut used = protected_cost;
        let mut prefix = Vec::new();
        for block in blocks[..protected_start].iter().rev() {
            let cost = block.chars().count() + sep;
            if used + cost > budget {
                continue;
            }
            used += cost;
            prefix.push(block.clone());
        }
        prefix.reverse();
        let omitted = protected_start.saturating_sub(prefix.len());
        if omitted > 0 {
            prefix.insert(
                0,
                format!("…〔为保留未完成轮次，省略更早 {omitted} 条记录〕…"),
            );
        }
        prefix.extend_from_slice(&blocks[protected_start..]);
        return prefix.join("\n\n");
    }

    let first = blocks.first().cloned().unwrap_or_default();
    let mut used = first.chars().count() + sep;
    let mut tail = Vec::new();
    for block in blocks.iter().skip(1).rev() {
        let cost = block.chars().count() + sep;
        if used + cost > budget {
            continue;
        }
        used += cost;
        tail.push(block.clone());
    }
    tail.reverse();
    let omitted = blocks.len().saturating_sub(1).saturating_sub(tail.len());
    let mut parts = vec![first];
    if omitted > 0 {
        parts.push(format!("…〔为控制长度，省略中间 {omitted} 条记录〕…"));
    }
    parts.extend(tail);
    parts.join("\n\n")
}
