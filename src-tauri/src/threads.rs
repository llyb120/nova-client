use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
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

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum AgentKind {
    Lyra,
    /// 一次性迁移的来源标识，不可用于执行。
    Legacy(String),
    /// 未知或缺失的后端标识，不可用于执行。
    Unsupported,
}

impl Serialize for AgentKind {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

fn legacy_devin() -> AgentKind {
    AgentKind::Legacy("devin".into())
}

impl Default for AgentKind {
    fn default() -> Self {
        // 缺失标识不能静默改由 Lyra 执行。
        AgentKind::Unsupported
    }
}

/// 已知退休来源保留到迁移完成；执行入口仍须使用 from_str 或显式检查 Lyra。
impl<'de> Deserialize<'de> for AgentKind {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?.trim().to_ascii_lowercase();
        Ok(match raw.as_str() {
            "lyra" | "alkaid" => AgentKind::Lyra,
            "claude" | "claudecode" | "codex" | "codexplus" | "devin" | "codebuddy" | "codebuddyplus"
            | "workbuddy" | "opencode" | "opencodeplus" => AgentKind::Legacy(raw),
            _ => AgentKind::Unsupported,
        })
    }
}

impl AgentKind {
    pub fn as_str(&self) -> &str {
        match self {
            AgentKind::Lyra => "lyra",
            AgentKind::Legacy(source) => source,
            AgentKind::Unsupported => "unsupported",
        }
    }

    /// 从字符串解析后端标识（大小写不敏感）；未知后端返回 None。
    pub fn from_str(s: &str) -> Option<AgentKind> {
        match s.trim().to_ascii_lowercase().as_str() {
            "lyra" | "alkaid" => Some(AgentKind::Lyra),
            _ => None,
        }
    }

    /// 展示用名称（注入接力上下文 / 系统提示用）
    pub fn label(&self) -> &'static str {
        match self {
            AgentKind::Lyra => "Lyra",
            AgentKind::Legacy(_) => "待迁移后端",
            AgentKind::Unsupported => "不支持的后端",
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

fn raw_value_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Array(values) => {
            let parts: Vec<String> = values.iter().filter_map(raw_value_text).collect();
            (!parts.is_empty()).then(|| parts.join("\n"))
        }
        Value::Object(object) => object
            .get("text")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| object.get("content").and_then(raw_value_text)),
        _ => None,
    }
}

fn same_output_text(value: &Value, display: &str) -> bool {
    raw_value_text(value).is_some_and(|text| text.trim() == display.trim())
}

/// 去掉 rawOutput 中已经完整存在于展示 content 的文本，同时保留 exitCode、details 等
/// 结构化元数据。这样会话文件不会为同一份命令/MCP 输出保存两遍。
fn strip_duplicate_raw_output(value: Value, display: &str) -> Option<Value> {
    match value {
        Value::String(text) => (text.trim() != display.trim()).then_some(Value::String(text)),
        Value::Array(values) => {
            if same_output_text(&Value::Array(values.clone()), display) {
                return None;
            }
            let kept: Vec<Value> = values
                .into_iter()
                .filter_map(|value| strip_duplicate_raw_output(value, display))
                .collect();
            (!kept.is_empty()).then_some(Value::Array(kept))
        }
        Value::Object(mut object) => {
            let text_block = object.get("text").and_then(Value::as_str).is_some()
                && object
                    .keys()
                    .all(|key| matches!(key.as_str(), "type" | "text" | "mimeType" | "mime_type"));
            if text_block && same_output_text(&Value::Object(object.clone()), display) {
                return None;
            }
            let keys: Vec<String> = object.keys().cloned().collect();
            for key in keys {
                let Some(child) = object.remove(&key) else {
                    continue;
                };
                let output_field = matches!(
                    key.as_str(),
                    "content" | "text" | "output" | "aggregatedOutput" | "aggregated_output"
                );
                if output_field && same_output_text(&child, display) {
                    continue;
                }
                if let Some(child) = strip_duplicate_raw_output(child, display) {
                    object.insert(key, child);
                }
            }
            (!object.is_empty()).then_some(Value::Object(object))
        }
        other => Some(other),
    }
}

fn deduplicate_tool_output(call: &mut ToolCall) {
    let display = tool_output_text(&call.content);
    if display.trim().is_empty() {
        return;
    }
    if let Some(raw_output) = call.raw_output.take() {
        call.raw_output = strip_duplicate_raw_output(raw_output, &display);
    }
}

/// 工具详情展示上限。后端的 read 结果可能携带整块大文件文本甚至
/// 图片 base64，不经截断就进 ToolCall 会让前端详情渲染、画布布局签名和会话落盘全部卡死。
pub(crate) const TOOL_OUTPUT_LIMIT: usize = 64 * 1024;

/// 超长文本保留尾部（与 acp.rs 的截断策略一致）。
fn limit_display_text(text: &str) -> String {
    // 先清除显示副本中的图片编码，再截尾；否则 JSON 的说明和路径全被挤掉。
    static IMAGE_DATA: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let text = IMAGE_DATA
        .get_or_init(|| {
            regex::Regex::new(r"data:image/[A-Za-z0-9.+-]+;base64,[A-Za-z0-9+/=_-]+").unwrap()
        })
        .replace_all(text, "[base64 图片数据已省略]");
    if text.len() <= TOOL_OUTPUT_LIMIT {
        return text.to_string();
    }
    let mut start = text.len().saturating_sub(TOOL_OUTPUT_LIMIT);
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    format!(
        "[输出过长，已省略前面内容，仅保留最后 {}KB]\n{}",
        TOOL_OUTPUT_LIMIT / 1024,
        &text[start..]
    )
}

/// 递归压缩工具输入/输出：字符串限长；图片 base64 对详情展示无意义，整体换成占位说明。
pub(crate) fn compact_tool_value(value: &Value) -> Value {
    match value {
        Value::String(s) => {
            // ACP 还会把 MCP 图片块序列化进 text，先保留 JSON 结构中的有效元数据。
            if s.trim_start().starts_with('{') || s.trim_start().starts_with('[') {
                if let Ok(parsed) = serde_json::from_str::<Value>(s) {
                    // Unwrap nested ACP/MCP JSON before limiting any strings. Limiting the
                    // serialized envelope loses status/snapshot/path fields and can leave only base64.
                    return Value::String(compact_tool_value(&parsed).to_string());
                }
            }
            Value::String(limit_display_text(s))
        }
        Value::Array(items) => Value::Array(items.iter().map(compact_tool_value).collect()),
        Value::Object(map) => {
            if map.get("type").and_then(Value::as_str) == Some("image") {
                if let Some(data) = map.get("data").and_then(Value::as_str) {
                    let mut out = map.clone();
                    out.insert(
                        "data".into(),
                        serde_json::json!(format!(
                            "[base64 图片数据已省略，共 {} 字符]",
                            data.len()
                        )),
                    );
                    return Value::Object(out);
                }
            }
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                out.insert(k.clone(), compact_tool_value(v));
            }
            Value::Object(out)
        }
        _ => value.clone(),
    }
}

pub(crate) fn compact_tool_values(values: &[Value]) -> Vec<Value> {
    values.iter().map(compact_tool_value).collect()
}

#[test]
fn tool_images_are_compacted_before_text_is_truncated() {
    let encoded = "A".repeat(TOOL_OUTPUT_LIMIT * 2);
    let result = serde_json::json!([
        {"type":"text", "text":"动作未执行，请重新截图", "path":"C:/shots/a.png"},
        {"type":"image_url", "imageUrl":{"url":format!("data:image/png;base64,{encoded}")}},
        {"type":"image", "data":encoded, "mimeType":"image/png"}
    ]);
    for input in [result.clone(), Value::String(result.to_string())] {
        let output = compact_tool_value(&input).to_string();
        assert!(output.len() < 1024);
        assert!(output.contains("动作未执行，请重新截图"));
        assert!(output.contains("C:/shots/a.png"));
        assert!(output.contains("图片数据已省略"));
        assert!(!output.contains("AAAA"));
    }
    let nested = serde_json::json!({"content":[{"type":"text","text":result.to_string()}],
        "status":"needs_review","snapshotId":"new","documentPath":"C:/shots/new.json"});
    let output = compact_tool_value(&Value::String(nested.to_string())).to_string();
    assert!(output.contains("needs_review") && output.contains("new.json"));
    assert!(output.contains("动作未执行") && !output.contains("AAAA"));
    assert!(output.len() < 2048);
    let unicode = compact_tool_value(&Value::String("界".repeat(TOOL_OUTPUT_LIMIT)));
    assert!(unicode.as_str().unwrap().ends_with('界'));
    assert!(unicode.as_str().unwrap().len() < TOOL_OUTPUT_LIMIT + 128);
}

fn deduplicate_thread_outputs(thread: &mut Thread) {
    for item in &mut thread.items {
        if let Item::Tool { call, .. } = item {
            deduplicate_tool_output(call);
        }
    }
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
        /// 最后一次模型请求实际携带的上下文；input_tokens 是整轮多次请求的累计输入。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        context_tokens: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        output_tokens: Option<u64>,
        /// 缓存 token 仅在后端提供明细时出现；不支持的后端保持 None。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache_read_tokens: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache_write_tokens: Option<u64>,
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
    /// 早期会话文件缺省 agentKind 时实际是 Devin，需进入迁移而不是被跳过。
    #[serde(default = "legacy_devin")]
    pub agent_kind: AgentKind,
    #[serde(default)]
    pub acp_session_id: Option<String>,
    /// 会话使用的模型（None = 后端默认）
    #[serde(default)]
    pub model: Option<String>,
    /// 会话模式：build / plan。
    #[serde(default)]
    pub mode: Option<String>,
    /// 思考强度（None = 模型配置默认）。
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    /// 一次性迁移输入：旧 Auto 路由实际选中的模型，成功迁移后清除。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_routed_model: Option<String>,
    /// 一次性迁移输入：上述模型对应的旧选项，不再参与运行时路由。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_route_selection: Option<String>,
    /// 上下文接力：fork、编辑或恢复后，下一条消息发送时把历史
    /// 注入新会话的输入，随后清除。None = 无待接力的上下文。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handoff_from: Option<AgentKind>,
    /// 临时会话：程序关闭时自动删除，不跨重启持久化
    #[serde(default)]
    pub ephemeral: bool,
    /// 用户星标：在所在项目内置顶，并豁免自动清理与项目一键删除。
    #[serde(default)]
    pub starred: bool,
    /// 尚未回看的收尾轮次数。只在内存里计数的话，重启会把后台完成的未读点抹掉，
    /// 因此随会话文件落盘，由前端在计数变化时写回。
    #[serde(default)]
    pub unread_turns: u32,
    /// Butterfly 已查看的结果版本；随会话保存，重启后不重复提醒。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub taskbar_seen_at: Option<i64>,
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
    /// 非 None：本会话在独立 git worktree 中执行（cwd 已指向该 worktree 工作目录）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<Worktree>,
    /// 旧版本训练记录标记：仅为保持本地隔离而兼容，不再创建此类会话。
    #[serde(default)]
    pub experience_thread: bool,
    /// 数字员工会话：不进普通会话列表，只在员工页「最近运行」里出现。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub employee_thread: bool,
    /// 由主会话调度的子 Agent 执行记录；前端仅供查看。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub subagent: bool,
    /// 会话树父节点：用于关联 Fire 和 Stage 会话。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_thread_id: Option<String>,
    /// Stage 会话引用的源会话。每次发送前都会从源会话最新时间线重建上下文。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage_source_thread_id: Option<String>,
    /// 自动模式会话的场景 key；None = 普通会话，场景切换与失效切换不生效。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_scene: Option<String>,
    /// 仅供下一次 prompt 隐式注入，不持久化到会话文件或前端快照。
    #[serde(skip)]
    pub pending_stage_context: Option<String>,
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

#[test]
fn subagent_metadata_defaults_and_roundtrips() {
    let thread = Thread::new(".".into(), AgentKind::Lyra, None, None, None, false);
    assert!(!thread.subagent);
    let mut value = serde_json::to_value(&thread).unwrap();
    assert!(value.get("subagent").is_none());
    assert!(!serde_json::from_value::<Thread>(value.clone()).unwrap().subagent);
    value["subagent"] = true.into();
    value["parentThreadId"] = "main".into();
    let child: Thread = serde_json::from_value(value).unwrap();
    assert!(child.subagent);
    assert_eq!(child.parent_thread_id.as_deref(), Some("main"));
    assert!(child.stage_source_thread_id.is_none());
    assert_eq!(serde_json::to_value(child).unwrap()["subagent"], true);
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
        let cwd = plain_path(&cwd);
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
            handoff_from: None,
            ephemeral,
            starred: false,
            unread_turns: 0,
            taskbar_seen_at: None,
            roaming_role: None,
            roaming_peer: None,
            roaming_peer_name: None,
            roaming_remote_id: None,
            worktree: None,
            experience_thread: false,
            employee_thread: false,
            subagent: false,
            parent_thread_id: None,
            stage_source_thread_id: None,
            auto_scene: None,
            pending_stage_context: None,
            active_clue_card_id: None,
            clue_context: None,
            created_at: now,
            updated_at: now,
            items: Vec::new(),
            plan: None,
        }
    }

    /// 调用方须先成功持久化 provider 配置，将思考档位编码进模型 selector，再迁移并保存线程。
    /// 只处理 Legacy，重复调用不会清除已迁移 Lyra 会话的新 session。
    pub fn migrate_to_lyra(&mut self, model: Option<String>) -> bool {
        if !matches!(&self.agent_kind, AgentKind::Legacy(_)) {
            return false;
        }
        self.agent_kind = AgentKind::Lyra;
        self.model = model;
        self.reasoning_effort = None;
        self.acp_session_id = None;
        self.auto_routed_model = None;
        self.auto_route_selection = None;
        self.handoff_from = (!self.items.is_empty()).then_some(AgentKind::Lyra);
        true
    }

    /// 是否为漫游 guest 会话（本机只接收展示，真正执行在对端）
    pub fn is_roaming_guest(&self) -> bool {
        self.roaming_role.as_deref() == Some("guest")
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
        let g = |keys: &[&str]| {
            usage.and_then(|u| {
                keys.iter()
                    .find_map(|key| u.get(*key).and_then(Value::as_u64))
            })
        };
        let item = Item::Turn {
            id: self.next_item_id(),
            ts: now_ms(),
            duration_ms,
            total_tokens: g(&["totalTokens", "total_tokens"]),
            input_tokens: g(&["inputTokens", "input_tokens"]),
            context_tokens: g(&["contextTokens", "context_tokens"]),
            output_tokens: g(&["outputTokens", "output_tokens"]),
            cache_read_tokens: g(&[
                "cacheReadTokens",
                "cachedInputTokens",
                "cached_input_tokens",
                "cache_read_input_tokens",
            ]),
            cache_write_tokens: g(&["cacheWriteTokens", "cache_creation_input_tokens"]),
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
                .map(|context| truncate_middle(context.rendered_context.trim(), CLUE_CONTEXT_MAX))
                .filter(|context| !context.is_empty())
        } else {
            None
        };
        let handoff = self.take_handoff_context(to_label);
        let stage = self.pending_stage_context.take();
        [clue, handoff, stage]
            .into_iter()
            .flatten()
            .reduce(|left, right| format!("{left}\n\n{right}"))
    }
}

/// 给 Stage 会话提供源会话的轻量引用：只保留每轮用户提示词和该轮最后一段助手结论。
pub fn render_stage_context(source: &Thread) -> String {
    let mut turns: Vec<(String, String)> = Vec::new();
    let mut prompt: Option<String> = None;
    let mut conclusion = String::new();
    for item in &source.items {
        match item {
            Item::User { text, .. } => {
                if let Some(previous) = prompt.take() {
                    turns.push((previous, conclusion.trim().to_string()));
                }
                prompt = Some(text.trim().to_string());
                conclusion.clear();
            }
            Item::Assistant { text, .. } if prompt.is_some() && !text.trim().is_empty() => {
                conclusion = text.trim().to_string();
            }
            _ => {}
        }
    }
    if let Some(previous) = prompt {
        turns.push((previous, conclusion.trim().to_string()));
    }
    let body = turns
        .into_iter()
        .enumerate()
        .map(|(index, (prompt, conclusion))| {
            let conclusion = if conclusion.is_empty() {
                "（本轮尚无结论）"
            } else {
                &conclusion
            };
            format!(
                "第 {} 轮\n提示词：{}\n结论：{}",
                index + 1,
                prompt,
                conclusion
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    format!(
        "[Stage 引用上下文]\n你正在一个引用会话中工作。以下内容来自源会话的最新时间线；每次发送消息时都会重新注入，请以最新版本为准。\n\n{}",
        if body.is_empty() { "（源会话暂无对话）" } else { &body }
    )
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    pub item_id: u64,
    pub snippet: String,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ThreadSearch {
    pub thread_id: String,
    pub title_hit: bool,
    /// 前 `max_hits` 条命中消息的摘要（每条消息取首个命中位置）
    pub hits: Vec<SearchHit>,
    /// 命中的消息总数
    pub total: usize,
}

/// 会话搜索：只匹配标题、用户输入和助手正文（含中间分段结论），不含思考与工具。
pub fn search_thread(thread: &Thread, query: &str, max_hits: usize) -> Option<ThreadSearch> {
    // 逐字符折叠大小写，保证与原文字符一一对应，便于截取摘要。
    let fold = |s: &str| -> Vec<char> {
        s.chars().map(|c| c.to_lowercase().next().unwrap_or(c)).collect()
    };
    let needle = fold(query.trim());
    if needle.is_empty() {
        return None;
    }
    let find = |text: &str| -> Option<String> {
        let hay = fold(text);
        let at = hay.windows(needle.len()).position(|w| w == needle.as_slice())?;
        let chars: Vec<char> = text.chars().collect();
        let start = at.saturating_sub(30);
        let end = (at + needle.len() + 60).min(chars.len());
        let body = chars[start..end].iter().collect::<String>();
        let body = body.split_whitespace().collect::<Vec<_>>().join(" ");
        Some(format!(
            "{}{body}{}",
            if start > 0 { "…" } else { "" },
            if end < chars.len() { "…" } else { "" }
        ))
    };
    let title_hit = find(&thread.title).is_some();
    let mut hits = Vec::new();
    let mut total = 0;
    for item in &thread.items {
        let (Item::User { id, text, .. } | Item::Assistant { id, text, .. }) = item else { continue };
        let Some(snippet) = find(text) else { continue };
        total += 1;
        if hits.len() < max_hits {
            hits.push(SearchHit { item_id: *id, snippet });
        }
    }
    (title_hit || total > 0).then(|| ThreadSearch { thread_id: thread.id.clone(), title_hit, hits, total })
}

#[test]
fn search_snippet_skips_thoughts_and_tools() {
    let mut thread = Thread::new(".".into(), AgentKind::Lyra, None, None, None, false);
    thread.title = "Deploy Plan".into();
    thread.push_user("帮我看看 nginx 配置".into(), Vec::new());
    thread.items.push(Item::Thought { id: 90, text: "secret-thought".into(), ts: 0 });
    thread.items.push(Item::Assistant { id: 91, text: "中间结论：端口是 8080".into(), ts: 0 });
    thread.items.push(Item::Assistant { id: 92, text: "最终结论：nginx 已重启".into(), ts: 0 });
    let snippets = |q: &str| search_thread(&thread, q, 5).map(|r| (r.title_hit, r.total, r.hits.into_iter().map(|h| (h.item_id, h.snippet)).collect::<Vec<_>>()));
    assert_eq!(snippets("deploy"), Some((true, 0, vec![])));
    let user_id = thread.items[0].id();
    assert_eq!(snippets("NGINX"), Some((false, 2, vec![(user_id, "帮我看看 nginx 配置".into()), (92, "最终结论：nginx 已重启".into())])));
    assert_eq!(snippets("8080"), Some((false, 1, vec![(91, "中间结论：端口是 8080".into())])));
    assert_eq!(search_thread(&thread, "nginx", 1).map(|r| (r.total, r.hits.len())), Some((2, 1)));
    assert!(snippets("secret").is_none());
    assert!(snippets("  ").is_none());
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
    /// 未读轮次数：前端据此在重启后恢复未读点。
    #[serde(default)]
    pub unread_turns: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roaming_role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roaming_peer_name: Option<String>,
    /// 非 None：该会话在独立 git worktree 中执行（前端据此显示分支标记）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<Worktree>,
    /// 兼容旧版本训练记录，避免混入普通历史。
    #[serde(default)]
    pub experience_thread: bool,
    /// 数字员工会话：不进普通会话列表，只在员工页「最近运行」里出现。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub employee_thread: bool,
    /// 由主会话调度的子 Agent 执行记录；前端仅供查看。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub subagent: bool,
    /// 会话树父节点：用于关联 Fire 和 Stage 会话。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_thread_id: Option<String>,
    /// /stage 引用的源会话。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage_source_thread_id: Option<String>,
    /// 自动模式标记随会话列表返回，供界面区分普通会话。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_scene: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_clue_card_id: Option<String>,
}

fn plain_path(path: &str) -> String {
    crate::lyra::runtime::display_working_directory(std::path::Path::new(path))
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
            .unwrap_or_default()
            .iter()
            .map(|p| plain_path(p))
            .collect();
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
        let key = crate::project_path_key(cwd);
        self.projects.retain(|p| crate::project_path_key(p) != key);
        self.projects.insert(0, plain_path(cwd));
        self.projects.truncate(20);
        self.save();
    }

    pub fn remove(&mut self, cwd: &str) {
        let key = crate::project_path_key(cwd);
        self.projects.retain(|p| crate::project_path_key(p) != key);
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
            .unwrap_or_default()
            .iter()
            .map(|p| plain_path(p))
            .collect();
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

    pub fn session_ids(&self) -> Vec<String> {
        self.entries
            .iter()
            .filter_map(|entry| entry.thread.acp_session_id.clone())
            .collect()
    }

    /// 回收站中的会话 id：延迟删除期间这些会话的关联数据（世界线、scratch 目录）要保留。
    pub fn thread_ids(&self) -> Vec<String> {
        self.entries
            .iter()
            .map(|entry| entry.thread.id.clone())
            .collect()
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

#[derive(Default)]
struct DirtyThreads {
    all: bool,
    ids: HashSet<String>,
}

/// 一次后台持久化的不可变会话快照。快照在 ThreadStore 锁内只做必要 clone，
/// JSON 序列化与文件 IO 均在锁外的 blocking worker 完成。
pub struct ThreadPersistSnapshot {
    writer: Arc<Mutex<u64>>,
    revision: u64,
    dir: PathBuf,
    threads: Vec<Thread>,
    requested_ids: HashSet<String>,
    full: bool,
}

pub struct ThreadStore {
    writer: Arc<Mutex<u64>>,
    revision: std::sync::atomic::AtomicU64,
    dir: PathBuf,
    pub threads: Vec<Thread>,
    /// 待落盘会话。普通会话更新只登记对应 id；结构变化（迁移/删除）登记 full。
    dirty: Arc<Mutex<DirtyThreads>>,
    /// 唤醒后台 flusher（notify_one 在无等待者时会存一个 permit，不丢通知）
    save_notify: Arc<Notify>,
}

const THREAD_ARTIFACT_SUFFIXES: [&str; 3] = [".parts", ".json.previous", ".json.backup"];

/// 删除会话索引 `<id>.json` 同名的分块目录与备份。
fn remove_thread_artifacts(index: &Path) {
    let base = index.with_extension("");
    for suffix in THREAD_ARTIFACT_SUFFIXES {
        let path = PathBuf::from(format!("{}{suffix}", base.display()));
        let result = if path.is_dir() { fs::remove_dir_all(&path) } else { fs::remove_file(&path) };
        match result {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                eprintln!("[threads] 清理会话残留 {} 失败：{error}", path.display())
            }
            _ => {}
        }
    }
}

/// 没有存活索引的分块/备份永远不会被读取（已删除会话不得复活），只在启动时无并发写入时回收。
fn sweep_orphan_thread_artifacts(dir: &Path) {
    for path in fs::read_dir(dir).into_iter().flatten().flatten().map(|entry| entry.path()) {
        let Some(name) = path.file_name().and_then(OsStr::to_str) else { continue };
        let Some(base) = THREAD_ARTIFACT_SUFFIXES.iter().find_map(|suffix| name.strip_suffix(suffix)) else { continue };
        let index = dir.join(format!("{base}.json"));
        if !base.is_empty() && !index.exists() {
            remove_thread_artifacts(&index);
        }
    }
}

impl ThreadStore {
    pub fn load(data_dir: PathBuf) -> Self {
        let dir = data_dir.join("threads");
        let legacy_path = data_dir.join("threads.json");
        let mut legacy = false;
        let mut legacy_blocked = false;
        let mut threads = Self::load_split_threads(&dir);
        if legacy_path.exists() {
            match fs::read_to_string(&legacy_path)
                .map_err(|e| e.to_string())
                .and_then(|text| {
                    serde_json::from_str::<StoreFile>(&text).map_err(|e| e.to_string())
                }) {
                Ok(file) => {
                    legacy = true;
                    // A partially completed migration may already have newer per-thread commits.
                    for thread in file.threads {
                        if !threads.iter().any(|existing| existing.id == thread.id) {
                            if dir.join(Self::thread_file_name(&thread.id)).exists() {
                                // An unreadable/newer-format live file is not permission to overwrite it.
                                legacy_blocked = true;
                                eprintln!(
                                    "[threads] 保留旧集合及无法读取的同名会话：{}",
                                    thread.id
                                );
                                continue;
                            }
                            threads.push(thread);
                        }
                    }
                }
                Err(error) => eprintln!("[threads] 无法读取旧会话，保留原文件：{error}"),
            }
        }
        // 旧版本用 std canonicalize 存下了 \\?\ 前缀路径，cmd/资源管理器等不接受，加载时一次性修正。
        let mut fixed_ids = Vec::new();
        for thread in &mut threads {
            let mut paths = vec![&mut thread.cwd];
            if let Some(worktree) = thread.worktree.as_mut() {
                paths.extend([&mut worktree.repo, &mut worktree.path]);
            }
            let mut fixed = false;
            for path in paths {
                let plain = plain_path(path);
                if plain != *path {
                    *path = plain;
                    fixed = true;
                }
            }
            if fixed {
                fixed_ids.push(thread.id.clone());
            }
        }
        let mut store = ThreadStore {
            dir,
            threads,
            writer: Arc::new(Mutex::new(0)),
            revision: std::sync::atomic::AtomicU64::new(0),
            dirty: Arc::new(Mutex::new(DirtyThreads::default())),
            save_notify: Arc::new(Notify::new()),
        };
        // Migrate one conversation at a time, without cloning the entire historical store.
        let mut migrated = !legacy_blocked;
        let mut retry_ids = Vec::new();
        for thread in &mut store.threads {
            let path = store.dir.join(Self::thread_file_name(&thread.id));
            if legacy || !crate::thread_storage::is_chunked(&path) {
                if let Err(error) = crate::thread_storage::write(&path, thread) {
                    eprintln!("[threads] 会话迁移失败，保留原数据：{error}");
                    migrated = false;
                    retry_ids.push(thread.id.clone());
                }
            }
        }
        for id in retry_ids.into_iter().chain(fixed_ids) {
            store.save_thread(&id);
        }
        if legacy && migrated {
            let backup = if legacy_path.with_extension("json.backup").exists() {
                legacy_path.with_extension(format!("json.backup-{}", uuid::Uuid::new_v4()))
            } else {
                legacy_path.with_extension("json.backup")
            };
            if let Err(error) = fs::rename(&legacy_path, backup) {
                eprintln!("[threads] 旧会话备份失败：{error}");
            }
        }
        // 启动时没有并发写入：回收旧版本删除会话后遗留的分块/备份。刚加载的会话条目都是内嵌数据，
        // 还没有引用 thread-assets，此时回收长期未更新的展示缓存是安全的（查看时会按需重建）。
        sweep_orphan_thread_artifacts(&store.dir);
        if let Some(root) = store.dir.parent() {
            crate::sweep_expired_entries(&root.join("thread-assets"), now_ms(), 24 * 7);
        }
        // 上次进程未正常退出时残留的临时会话，启动时一并清掉
        if !store.purge_ephemeral().is_empty() {
            store.save();
        }
        store
    }

    fn load_split_threads(dir: &Path) -> Vec<Thread> {
        let Ok(entries) = fs::read_dir(dir) else {
            return Vec::new();
        };
        let mut threads: Vec<Thread> = entries
            .flatten()
            .filter_map(|entry| {
                let path = entry.path();
                if path.extension() != Some(OsStr::new("json")) {
                    return None;
                }
                match crate::thread_storage::read(&path) {
                    Ok(mut thread) => {
                        deduplicate_thread_outputs(&mut thread);
                        Some(thread)
                    }
                    Err(error) => {
                        eprintln!("[threads] 保留无法读取的会话 {}: {error}", path.display());
                        None
                    }
                }
            })
            .collect();
        threads.sort_by_key(|thread| thread.created_at);
        threads
    }

    /// 磁盘上存在的会话索引 id（含无法读入内存的），供衍生数据对账时保守保留。
    pub fn indexed_ids(&self) -> HashSet<String> {
        fs::read_dir(&self.dir).into_iter().flatten().flatten()
            .filter_map(|entry| entry.file_name().to_str()?.strip_suffix(".json").map(str::to_owned))
            .collect()
    }

    fn thread_file_name(id: &str) -> String {
        if !id.is_empty()
            && id
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
        {
            return format!("{id}.json");
        }
        let encoded = id
            .as_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        format!("encoded-{encoded}.json")
    }

    /// 删除所有临时会话，返回被删掉的会话（调用方负责清理其工作目录等）
    pub fn purge_ephemeral(&mut self) -> Vec<Thread> {
        let (ephemeral, kept): (Vec<Thread>, Vec<Thread>) = std::mem::take(&mut self.threads)
            .into_iter()
            .partition(|t| t.ephemeral);
        self.threads = kept;
        ephemeral
    }

    /// 请求全量持久化（异步节流）。用于会话集合发生结构变化的路径，例如删除。
    /// 普通单会话更新应调用 save_thread，避免一个工具事件重写全部历史。
    pub fn save(&self) {
        let mut dirty = self.dirty.lock().unwrap();
        dirty.all = true;
        dirty.ids.clear();
        drop(dirty);
        self.save_notify.notify_one();
    }

    /// 只标记一个会话待持久化。登记和唤醒均为常量开销，可安全用于流式/工具热路径。
    pub fn save_thread(&self, id: &str) {
        if id.is_empty() {
            self.save();
            return;
        }
        let mut dirty = self.dirty.lock().unwrap();
        if !dirty.all {
            dirty.ids.insert(id.to_string());
        }
        drop(dirty);
        self.save_notify.notify_one();
    }

    /// 取走当前脏集合并在 ThreadStore 锁内创建最小快照。取走后发生的新更新会重新
    /// 登记并留下 notify permit，因此当前快照写盘期间的更新不会丢失。
    pub fn take_persist_snapshot(&self) -> Option<ThreadPersistSnapshot> {
        let (full, requested_ids) = {
            let mut dirty = self.dirty.lock().unwrap();
            if !dirty.all && dirty.ids.is_empty() {
                return None;
            }
            let full = dirty.all;
            dirty.all = false;
            let ids = std::mem::take(&mut dirty.ids);
            (full, ids)
        };
        let threads = if full {
            self.threads.clone()
        } else {
            self.threads
                .iter()
                .filter(|thread| requested_ids.contains(&thread.id))
                .cloned()
                .collect()
        };
        Some(ThreadPersistSnapshot {
            writer: self.writer.clone(),
            revision: self
                .revision
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                + 1,
            dir: self.dir.clone(),
            threads,
            requested_ids,
            full,
        })
    }

    /// 写盘失败时把快照对应的脏标记放回队列；期间新增的脏标记会一并保留。
    pub fn retry_persist_snapshot(&self, snapshot: &ThreadPersistSnapshot) {
        let mut dirty = self.dirty.lock().unwrap();
        if snapshot.full {
            dirty.all = true;
            dirty.ids.clear();
        } else if !dirty.all {
            dirty.ids.extend(snapshot.requested_ids.iter().cloned());
        }
        drop(dirty);
        self.save_notify.notify_one();
    }

    /// 后台 flusher 用：save() 的唤醒器句柄
    pub fn save_notify_handle(&self) -> Arc<Notify> {
        self.save_notify.clone()
    }

    /// Single writer plus monotonically increasing snapshots prevents a background save
    /// from overwriting the newer synchronous exit/update snapshot.
    pub fn write_persist_snapshot(snapshot: &mut ThreadPersistSnapshot) -> Result<(), String> {
        let mut committed = snapshot.writer.lock().map_err(|e| e.to_string())?;
        if snapshot.revision <= *committed {
            return Ok(());
        }
        fs::create_dir_all(&snapshot.dir).map_err(|e| e.to_string())?;
        let mut expected = HashSet::new();
        for thread in &mut snapshot.threads {
            deduplicate_thread_outputs(thread);
            let path = snapshot.dir.join(Self::thread_file_name(&thread.id));
            crate::thread_storage::write(&path, thread)?;
            expected.insert(path);
        }
        let candidates: Vec<PathBuf> = if snapshot.full {
            fs::read_dir(&snapshot.dir)
                .map_err(|e| e.to_string())?
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|path| path.extension() == Some(OsStr::new("json")))
                .collect()
        } else {
            snapshot
                .requested_ids
                .iter()
                .map(|id| snapshot.dir.join(Self::thread_file_name(id)))
                .collect()
        };
        for path in candidates {
            // Never delete an unreadable historical file that failed to enter the in-memory store.
            if !expected.contains(&path)
                && path.exists()
                && crate::thread_storage::read(&path).is_ok()
            {
                fs::remove_file(&path).map_err(|e| e.to_string())?;
                // The absent live index is the tombstone: read() never recovers without it and the
                // trash keeps a self-contained snapshot, so the chunks and backups are garbage now.
                remove_thread_artifacts(&path);
            }
        }
        *committed = snapshot.revision;
        Ok(())
    }

    pub fn save_now(&mut self) {
        self.save();
        if let Some(mut snapshot) = self.take_persist_snapshot() {
            if let Err(error) = Self::write_persist_snapshot(&mut snapshot) {
                eprintln!("[threads] 保存会话失败：{error}");
                self.retry_persist_snapshot(&snapshot);
            }
        }
    }

    pub fn get(&self, id: &str) -> Option<&Thread> {
        self.threads.iter().find(|t| t.id == id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut Thread> {
        self.threads.iter_mut().find(|t| t.id == id)
    }

    /// 复制当前节点所在的整条 Stage 树；只登记副本，不触碰源会话及其运行时。
    pub fn fork_thread(&mut self, thread_id: &str, title: &str) -> Result<Thread, String> {
        let title = title.trim();
        if title.is_empty() {
            return Err("标题不能为空".into());
        }
        let by_id: HashMap<&str, &Thread> = self.threads.iter().map(|t| (t.id.as_str(), t)).collect();
        let mut root = *by_id.get(thread_id).ok_or("会话不存在")?;
        let mut ancestors = HashSet::from([root.id.as_str()]);
        while let Some(parent) = root.parent_thread_id.as_deref().and_then(|id| by_id.get(id)) {
            if !ancestors.insert(parent.id.as_str()) {
                return Err("会话 Stage 关系存在循环，无法 fork".into());
            }
            root = parent;
        }
        let mut children: HashMap<&str, Vec<&Thread>> = HashMap::new();
        for thread in &self.threads {
            if let Some(parent) = thread.parent_thread_id.as_deref() {
                children.entry(parent).or_default().push(thread);
            }
        }
        let mut sources = vec![root];
        let mut seen = HashSet::from([root.id.as_str()]);
        let mut index = 0;
        while index < sources.len() {
            for child in children.get(sources[index].id.as_str()).into_iter().flatten() {
                if seen.insert(child.id.as_str()) {
                    sources.push(child);
                }
            }
            index += 1;
        }
        // 远端路由绑定旧 thread id，不能让副本借用原会话的执行通道。
        if sources.iter().any(|t| t.roaming_role.is_some()) {
            return Err("包含漫游的会话暂不支持 fork，请先召回为本地会话".into());
        }
        let ids: HashMap<&str, String> = sources.iter()
            .map(|t| (t.id.as_str(), uuid::Uuid::new_v4().to_string())).collect();
        if sources.iter().any(|t| t.stage_source_thread_id.as_deref().is_some_and(|id| !ids.contains_key(id))) {
            return Err("Stage 引用了会话树外的源会话，无法安全 fork".into());
        }
        sources.sort_by_key(|t| t.created_at);
        let now = now_ms();
        let copies: Vec<Thread> = sources.iter().enumerate().map(|(index, source)| {
            let mut copy = (*source).clone();
            copy.id = ids[source.id.as_str()].clone();
            copy.parent_thread_id = source.parent_thread_id.as_deref().and_then(|id| ids.get(id)).cloned();
            copy.stage_source_thread_id = source.stage_source_thread_id.as_deref().and_then(|id| ids.get(id)).cloned();
            if source.id == root.id {
                copy.title = title.to_string();
            }
            // 不复用 session：下一次发送走现有历史接力，不能写入源 session。
            copy.acp_session_id = None;
            copy.pending_stage_context = None;
            copy.handoff_from = (!copy.items.is_empty()).then(|| copy.agent_kind.clone());
            copy.unread_turns = 0;
            copy.taskbar_seen_at = None;
            copy.ephemeral = false;
            copy.employee_thread = false;
            copy.experience_thread = false;
            copy.created_at = now + index as i64;
            copy.updated_at = copy.created_at;
            copy
        }).collect();
        let selected = copies.iter().find(|t| t.id == ids[thread_id]).unwrap().clone();
        for copy in copies {
            self.save_thread(&copy.id);
            self.threads.push(copy);
        }
        Ok(selected)
    }

    /// 持有 store/trash 锁时清理，避免 fork 或 Stage 仍引用的共享 scratch 目录被删除。
    pub fn cleanup_unused_scratch_dirs(&self, removed: &[Thread], trash: &ThreadTrashStore) {
        for thread in removed {
            let path = PathBuf::from(crate::project_path_key(&thread.cwd));
            if thread.cwd.contains(crate::SCRATCH_MARK)
                && !self.threads.iter().chain(trash.entries.iter().map(|entry| &entry.thread))
                    .any(|retained| {
                        let retained = PathBuf::from(crate::project_path_key(&retained.cwd));
                        retained.starts_with(&path) || path.starts_with(retained)
                    })
            {
                let _ = fs::remove_dir_all(&thread.cwd);
            }
        }
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
const CLUE_CONTEXT_MAX: usize = 4000;
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
    // Under the handoff budget it takes priority over old completed history, so a cancelled
    // turn keeps its assistant/tool state instead of looking like a new conversation.
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
            Item::Thought { text, .. } if index >= unfinished_item_start => {
                let t = truncate_middle(text.trim(), HANDOFF_ASSISTANT_MAX);
                if !t.trim().is_empty() {
                    blocks.push(format!("{from_label} 推理：\n{t}"));
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
    let use_full_context =
        completed_turns < HANDOFF_FULL_TURNS && native_chars < HANDOFF_CONTEXT_THRESHOLD;
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
        slim_blocks.extend(
            blocks
                .iter()
                .skip(unfinished_block_start.unwrap_or(blocks.len()))
                .cloned(),
        );
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
    fn only_artifacts_without_a_live_index_are_swept() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("live.json"), "{}").unwrap();
        for name in ["live.parts", "gone.parts"] {
            fs::create_dir_all(root.join(name)).unwrap();
            fs::write(root.join(name).join("object"), "x").unwrap();
        }
        for name in ["live.json.previous", "gone.json.previous", "gone.json.backup", ".parts"] {
            fs::write(root.join(name), "x").unwrap();
        }
        sweep_orphan_thread_artifacts(root);
        let mut left: Vec<_> = fs::read_dir(root).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        left.sort();
        assert_eq!(left, [".parts", "live.json", "live.json.previous", "live.parts"]);
    }

    fn temp_thread_store_dir() -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("nova-thread-store-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn fork_thread_copies_entire_stage_tree_without_changing_source() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = ThreadStore::load(dir.path().to_path_buf());
        let mut root = Thread::new("project".into(), AgentKind::Lyra, Some("model".into()), Some("plan".into()), None, true);
        root.title = "original".into();
        root.acp_session_id = Some("original-provider".into());
        root.pending_stage_context = Some("stale context".into());
        root.unread_turns = 3;
        root.plan = Some(serde_json::json!([{ "step": "test", "status": "pending" }]));
        for i in 0..140 {
            root.push_user(format!("history {i}"), vec![PromptImage {
                name: "attachment".into(), mime_type: "image/png".into(), data: Some("YWJj".into()), uri: None, size: Some(3),
            }]);
        }
        let mut stage = root.clone();
        stage.id = "stage".into();
        stage.title = "stage title".into();
        stage.parent_thread_id = Some(root.id.clone());
        stage.stage_source_thread_id = Some(root.id.clone());
        let mut sibling = stage.clone();
        sibling.id = "sibling".into();
        let mut child = stage.clone();
        child.id = "child".into();
        child.parent_thread_id = Some(stage.id.clone());
        child.stage_source_thread_id = Some(stage.id.clone());
        child.subagent = true;
        let unrelated = Thread::new("other".into(), AgentKind::Lyra, None, None, None, false);
        store.threads = vec![root, stage, sibling, child, unrelated];
        store.save_now();
        let originals = serde_json::to_value(&store.threads).unwrap();
        let original_paths: Vec<_> = store.threads.iter().map(|t| dir.path().join("threads").join(ThreadStore::thread_file_name(&t.id))).collect();
        let original_bytes: Vec<_> = original_paths.iter().map(|path| fs::read(path).unwrap()).collect();

        let selected = store.fork_thread("stage", "  independent fork  ").unwrap();
        assert_eq!(store.threads.len(), 9);
        assert_eq!(serde_json::to_value(&store.threads[..5]).unwrap(), originals);
        let copies = &store.threads[5..];
        let root_copy = copies.iter().find(|t| t.parent_thread_id.is_none()).unwrap();
        assert_eq!(root_copy.title, "independent fork");
        assert_eq!(selected.title, "stage title");
        assert_eq!(selected.parent_thread_id.as_deref(), Some(root_copy.id.as_str()));
        assert_eq!(selected.stage_source_thread_id, selected.parent_thread_id);
        let child_copy = copies.iter().find(|t| t.subagent).unwrap();
        assert_eq!(child_copy.parent_thread_id.as_deref(), Some(selected.id.as_str()));
        assert_eq!(child_copy.stage_source_thread_id, child_copy.parent_thread_id);
        for copy in copies {
            assert!(!store.threads[..5].iter().any(|t| t.id == copy.id));
            assert!(copy.acp_session_id.is_none());
            assert!(copy.pending_stage_context.is_none() && copy.handoff_from == Some(AgentKind::Lyra));
            assert!(!copy.ephemeral && copy.unread_turns == 0);
            assert_eq!(copy.cwd, "project");
            assert_eq!(copy.model.as_deref(), Some("model"));
            assert_eq!(copy.mode.as_deref(), Some("plan"));
            assert_eq!(copy.plan, store.threads[0].plan);
            assert_eq!(serde_json::to_value(&copy.items).unwrap(), originals[0]["items"]);
        }
        let mut snapshot = store.take_persist_snapshot().unwrap();
        assert!(!snapshot.full);
        assert_eq!(snapshot.threads.len(), 4);
        ThreadStore::write_persist_snapshot(&mut snapshot).unwrap();
        for (path, bytes) in original_paths.iter().zip(original_bytes) {
            assert_eq!(fs::read(path).unwrap(), bytes);
        }
        let reloaded = ThreadStore::load(dir.path().to_path_buf());
        assert_eq!(reloaded.get(&selected.id).unwrap().items.len(), 140);
        let fork = store.get_mut(&selected.id).unwrap();
        assert!(fork.take_prompt_context("Lyra").unwrap().contains("history 139"));
        fork.title = "renamed fork".into();
        fork.items.clear();
        store.threads.truncate(5);
        assert_eq!(serde_json::to_value(&store.threads).unwrap(), originals);
        assert_eq!(store.get("stage").unwrap().pending_stage_context.as_deref(), Some("stale context"));
    }

    #[test]
    fn fork_thread_rejects_invalid_or_shared_remote_routes_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = ThreadStore::load(dir.path().to_path_buf());
        let source = Thread::new("project".into(), AgentKind::Lyra, None, None, None, false);
        let id = source.id.clone();
        store.threads.push(source.clone());
        assert!(store.fork_thread(&id, "  ").is_err());
        assert!(store.fork_thread("missing", "fork").is_err());
        for case in 0..3 {
            let mut invalid = source.clone();
            match case {
                0 => invalid.roaming_role = Some("guest".into()),
                1 => invalid.parent_thread_id = Some(id.clone()),
                _ => invalid.stage_source_thread_id = Some("outside-tree".into()),
            }
            store.threads = vec![invalid];
            let before = serde_json::to_value(&store.threads).unwrap();
            assert!(store.fork_thread(&id, "fork").is_err());
            assert_eq!(serde_json::to_value(&store.threads).unwrap(), before);
            assert!(store.take_persist_snapshot().is_none());
        }
    }

    #[test]
    fn fork_thread_scratch_cleanup_keeps_live_and_trashed_copies() {
        let dir = tempfile::tempdir().unwrap();
        let scratch = dir.path().join(crate::SCRATCH_MARK).join("shared");
        fs::create_dir_all(&scratch).unwrap();
        fs::write(scratch.join("keep.txt"), "keep").unwrap();
        let mut store = ThreadStore::load(dir.path().to_path_buf());
        let mut trash = ThreadTrashStore::load(&dir.path().to_path_buf());
        let source = Thread::new(scratch.to_string_lossy().into_owned(), AgentKind::Lyra, None, None, None, true);
        store.threads.push(source.clone());
        let fork = store.fork_thread(&source.id, "fork").unwrap();
        // 退出时删临时源会话，不能连带清掉持久副本的工作目录。
        let removed = store.purge_ephemeral();
        store.cleanup_unused_scratch_dirs(&removed, &trash);
        assert!(scratch.join("keep.txt").exists());
        // 副本切到原工作区子目录后删除，父目录中的源会话也必须受到保护。
        let nested = scratch.join("sub");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("keep.txt"), "keep").unwrap();
        let mut nested_fork = fork.clone();
        nested_fork.cwd = nested.to_string_lossy().into_owned();
        store.threads = vec![source.clone()];
        store.cleanup_unused_scratch_dirs(&[nested_fork], &trash);
        assert!(nested.join("keep.txt").exists());
        store.cleanup_unused_scratch_dirs(&[fork.clone()], &trash);
        assert!(scratch.join("keep.txt").exists());
        trash.move_to_trash(vec![source], now_ms()).unwrap();
        store.threads.clear();
        store.cleanup_unused_scratch_dirs(&[fork.clone()], &trash);
        assert!(scratch.join("keep.txt").exists());
        trash.entries.clear();
        store.cleanup_unused_scratch_dirs(&[fork], &trash);
        assert!(!scratch.exists());
    }

    #[test]
    fn only_lyra_is_an_executable_agent_kind() {
        for raw in ["lyra", " LYRA ", "alkaid"] {
            assert_eq!(AgentKind::from_str(raw), Some(AgentKind::Lyra));
            assert_eq!(serde_json::from_value::<AgentKind>(serde_json::json!(raw)).unwrap(), AgentKind::Lyra);
        }
        assert_eq!(serde_json::to_value(AgentKind::Lyra).unwrap(), "lyra");
        for raw in ["", "unknown-backend", "unsupported"] {
            assert_eq!(AgentKind::from_str(raw), None);
            let kind: AgentKind = serde_json::from_value(serde_json::json!(raw)).unwrap();
            assert_eq!(kind, AgentKind::Unsupported);
            assert_eq!(kind.as_str(), "unsupported");
            assert_eq!(kind.label(), "不支持的后端");
        }
    }

    #[test]
    fn legacy_agent_sources_roundtrip_without_becoming_executable() {
        for source in ["claude", "claudecode", "codex", "codexplus", "devin", "codebuddy", "codebuddyplus", "workbuddy", "opencode", "opencodeplus"] {
            let raw = format!(" {} ", source.to_uppercase());
            let kind: AgentKind = serde_json::from_value(serde_json::json!(raw)).unwrap();
            assert_eq!(kind, AgentKind::Legacy(source.into()));
            assert_eq!(kind.as_str(), source);
            assert_eq!(kind.label(), "待迁移后端");
            assert_eq!(serde_json::to_value(&kind).unwrap(), source);
            assert_eq!(AgentKind::from_str(&raw), None);
        }
    }

    #[test]
    fn migration_preserves_inputs_until_commit_and_seeds_history_once() {
        let mut source = Thread::new("project".into(), AgentKind::Legacy("codex".into()), Some("auto".into()), Some("plan".into()), Some("high".into()), false);
        source.auto_routed_model = Some("original-model:high".into());
        source.auto_route_selection = Some("auto".into());
        source.acp_session_id = Some("old-session".into());
        source.push_user("保留这段上下文".into(), Vec::new());
        source.plan = Some(serde_json::json!([{ "content": "待完成工作", "status": "pending" }]));
        source.updated_at = 12_345;
        let raw = serde_json::to_value(&source).unwrap();
        let mut thread: Thread = serde_json::from_value(raw.clone()).unwrap();
        assert_eq!(serde_json::to_value(&thread).unwrap(), raw);

        assert!(thread.migrate_to_lyra(Some("local-provider/original-model:high".into())));
        assert_eq!(thread.agent_kind, AgentKind::Lyra);
        assert_eq!(thread.model.as_deref(), Some("local-provider/original-model:high"));
        assert!(thread.reasoning_effort.is_none());
        assert_eq!(thread.created_at, source.created_at);
        assert_eq!(thread.updated_at, source.updated_at);
        assert_eq!(thread.mode, source.mode);
        assert_eq!(thread.plan, source.plan);
        assert_eq!(serde_json::to_value(&thread.items).unwrap(), raw["items"]);
        assert!(thread.acp_session_id.is_none());
        assert!(thread.auto_routed_model.is_none() && thread.auto_route_selection.is_none());
        let migrated = serde_json::to_value(&thread).unwrap();
        assert!(migrated.get("autoRoutedModel").is_none());
        assert!(migrated.get("autoRouteSelection").is_none());
        assert!(thread.take_prompt_context("Lyra").unwrap().contains("保留这段上下文"));
        assert!(thread.take_prompt_context("Lyra").is_none());

        thread.acp_session_id = Some("lyra-session".into());
        let before = serde_json::to_value(&thread).unwrap();
        assert!(!thread.migrate_to_lyra(None));
        assert_eq!(serde_json::to_value(&thread).unwrap(), before);
        let mut unsupported = Thread::new("project".into(), AgentKind::Unsupported, None, None, None, false);
        assert!(!unsupported.migrate_to_lyra(None));
    }

    #[test]
    fn legacy_threads_json_is_migrated_to_one_file_per_thread() {
        let data_dir = temp_thread_store_dir();
        let mut first = Thread::new(
            "/tmp/project-a".into(),
            AgentKind::Lyra,
            None,
            None,
            None,
            false,
        );
        first.title = "First".into();
        let mut second = Thread::new(
            "/tmp/project-b".into(),
            AgentKind::Lyra,
            None,
            None,
            None,
            false,
        );
        second.title = "Second".into();
        let ids = [first.id.clone(), second.id.clone()];
        fs::write(
            data_dir.join("threads.json"),
            serde_json::to_string(&StoreFile {
                threads: vec![first, second],
            })
            .unwrap(),
        )
        .unwrap();

        let store = ThreadStore::load(data_dir.clone());

        assert_eq!(store.threads.len(), 2);
        assert!(!data_dir.join("threads.json").exists());
        assert!(data_dir.join("threads.json.backup").exists());
        for id in &ids {
            assert!(data_dir
                .join("threads")
                .join(ThreadStore::thread_file_name(id))
                .exists());
        }
        let reloaded = ThreadStore::load(data_dir.clone());
        assert_eq!(reloaded.threads.len(), 2);
        assert!(ids.iter().all(|id| reloaded.get(id).is_some()));

        fs::remove_dir_all(data_dir).unwrap();
    }

    #[test]
    fn incremental_snapshot_writes_only_dirty_thread_and_keeps_later_updates() {
        let data_dir = temp_thread_store_dir();
        let mut store = ThreadStore::load(data_dir.clone());
        let mut first = Thread::new(
            "/tmp/project-a".into(),
            AgentKind::Lyra,
            None,
            None,
            None,
            false,
        );
        first.title = "First".into();
        let first_id = first.id.clone();
        let mut second = Thread::new(
            "/tmp/project-b".into(),
            AgentKind::Lyra,
            None,
            None,
            None,
            false,
        );
        second.title = "Second".into();
        let second_id = second.id.clone();
        store.threads = vec![first, second];
        store.save_now();

        let second_path = data_dir
            .join("threads")
            .join(ThreadStore::thread_file_name(&second_id));
        let second_before = fs::read_to_string(&second_path).unwrap();
        store.get_mut(&first_id).unwrap().title = "First v1".into();
        store.save_thread(&first_id);
        let mut first_snapshot = store.take_persist_snapshot().unwrap();
        assert!(!first_snapshot.full);
        assert_eq!(first_snapshot.threads.len(), 1);

        // 旧快照写盘期间到达的新更新必须保留为下一批，而不是被 take 清掉。
        store.get_mut(&first_id).unwrap().title = "First v2".into();
        store.save_thread(&first_id);
        ThreadStore::write_persist_snapshot(&mut first_snapshot).unwrap();
        let mut second_snapshot = store.take_persist_snapshot().unwrap();
        ThreadStore::write_persist_snapshot(&mut second_snapshot).unwrap();

        assert_eq!(fs::read_to_string(&second_path).unwrap(), second_before);
        let reloaded = ThreadStore::load(data_dir.clone());
        assert_eq!(reloaded.get(&first_id).unwrap().title, "First v2");
        assert_eq!(reloaded.get(&second_id).unwrap().title, "Second");
        fs::remove_dir_all(data_dir).unwrap();
    }

    #[test]
    fn split_thread_store_removes_deleted_thread_files() {
        let data_dir = temp_thread_store_dir();
        let mut store = ThreadStore::load(data_dir.clone());
        let first = Thread::new(
            "/tmp/project-a".into(),
            AgentKind::Lyra,
            None,
            None,
            None,
            false,
        );
        let first_id = first.id.clone();
        let second = Thread::new(
            "/tmp/project-b".into(),
            AgentKind::Lyra,
            None,
            None,
            None,
            false,
        );
        let second_id = second.id.clone();
        store.threads = vec![first, second];
        store.save_now();

        let threads_dir = data_dir.join("threads");
        assert!(threads_dir
            .join(ThreadStore::thread_file_name(&first_id))
            .exists());
        assert!(threads_dir
            .join(ThreadStore::thread_file_name(&second_id))
            .exists());

        store.threads.retain(|thread| thread.id != first_id);
        store.save_now();

        assert!(!threads_dir
            .join(ThreadStore::thread_file_name(&first_id))
            .exists());
        assert!(threads_dir
            .join(ThreadStore::thread_file_name(&second_id))
            .exists());
        fs::remove_dir_all(data_dir).unwrap();
    }

    #[test]
    fn duplicate_tool_output_is_removed_from_raw_output() {
        let repeated = "same tool output";
        let mut call = ToolCall {
            tool_call_id: "call-1".into(),
            title: "read_files".into(),
            kind: "read".into(),
            status: "completed".into(),
            content: vec![serde_json::json!({
                "type": "content",
                "content": { "type": "text", "text": repeated }
            })],
            locations: Vec::new(),
            raw_input: None,
            raw_output: Some(serde_json::json!({
                "content": [{ "type": "text", "text": repeated }],
                "details": { "count": 3 }
            })),
        };

        deduplicate_tool_output(&mut call);

        assert_eq!(
            call.raw_output,
            Some(serde_json::json!({ "details": { "count": 3 } }))
        );
        let serialized = serde_json::to_string(&call).unwrap();
        assert_eq!(serialized.matches(repeated).count(), 1);
    }

    #[test]
    fn duplicate_command_output_keeps_structured_metadata() {
        let mut call = ToolCall {
            tool_call_id: "call-2".into(),
            title: "cargo check".into(),
            kind: "execute".into(),
            status: "completed".into(),
            content: vec![serde_json::json!({
                "type": "content",
                "content": { "type": "text", "text": "Finished" }
            })],
            locations: Vec::new(),
            raw_input: None,
            raw_output: Some(serde_json::json!({
                "aggregatedOutput": "Finished",
                "exitCode": 0
            })),
        };

        deduplicate_tool_output(&mut call);

        assert_eq!(call.raw_output, Some(serde_json::json!({ "exitCode": 0 })));
    }

    #[test]
    fn distinct_raw_output_is_preserved() {
        let mut call = ToolCall {
            tool_call_id: "call-3".into(),
            title: "tool".into(),
            kind: "other".into(),
            status: "completed".into(),
            content: vec![serde_json::json!({
                "type": "content",
                "content": { "type": "text", "text": "preview" }
            })],
            locations: Vec::new(),
            raw_input: None,
            raw_output: Some(serde_json::json!({ "result": "different detail" })),
        };

        deduplicate_tool_output(&mut call);

        assert_eq!(
            call.raw_output,
            Some(serde_json::json!({ "result": "different detail" }))
        );
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
                context_tokens: None,
                output_tokens: None,
                cache_read_tokens: None,
                cache_write_tokens: None,
                stop_reason: "end_turn".into(),
            });
        }

        let context = render_handoff_context(&items, None, "Lyra", "Lyra").unwrap();

        assert!(context.contains("提示 1"));
        assert!(context.contains("结论 10"));
        assert!(!context.contains("工具轨迹"));
    }

    #[test]
    fn handoff_keeps_reasoning_from_an_interrupted_turn() {
        let items = vec![
            Item::User {
                id: 1,
                text: "检查并修复问题".into(),
                ts: now_ms(),
                images: Vec::new(),
            },
            Item::Thought {
                id: 2,
                text: "需要先读取配置文件".into(),
                ts: now_ms(),
            },
            Item::Tool {
                id: 3,
                ts: now_ms(),
                call: ToolCall {
                    tool_call_id: "read-1".into(),
                    title: "read".into(),
                    kind: "read".into(),
                    status: "completed".into(),
                    content: vec![serde_json::json!({ "type": "content", "content": "关键配置" })],
                    locations: Vec::new(),
                    raw_input: Some(serde_json::json!({ "path": "config.json" })),
                    raw_output: None,
                },
            },
        ];

        let context = render_handoff_context(&items, None, "Lyra", "Lyra").unwrap();

        assert!(context.contains("Lyra 推理：\n需要先读取配置文件"));
        assert!(context.contains("检查并修复问题"));
        assert!(context.contains("read"));
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
    fn edited_lyra_prompt_replays_retained_history_once() {
        let mut thread = Thread::new(
            "D:/project".into(),
            AgentKind::Lyra,
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
        thread.handoff_from = Some(AgentKind::Lyra);

        let context = thread
            .take_prompt_context("Lyra")
            .expect("重编辑后应把截断点前的历史交给新的 Lyra 会话");

        assert!(context.contains("用户：\n先定位问题"));
        assert!(context.contains("Lyra：\n问题位于会话恢复逻辑。"));
        assert!(thread.take_prompt_context("Lyra").is_none());

        thread.items.clear();
        thread.handoff_from = None;
        assert!(thread.take_prompt_context("Lyra").is_none());
    }

    #[test]
    fn missing_backend_is_legacy_devin_not_lyra() {
        let thread = Thread::new(String::new(), AgentKind::Lyra, None, None, None, false);
        let mut value = serde_json::to_value(thread).unwrap();
        value.as_object_mut().unwrap().remove("agentKind");
        let restored: Thread = serde_json::from_value(value).unwrap();
        assert_eq!(restored.agent_kind, AgentKind::Legacy("devin".into()));
    }

    #[test]
    fn thread_defaults_optional_persistence_fields() {
        let thread = Thread::new(String::new(), AgentKind::Lyra, None, None, None, false);
        let mut value = serde_json::to_value(thread).expect("线程应可序列化");
        let object = value.as_object_mut().expect("线程应序列化为对象");
        object.remove("starred");
        object.remove("unreadTurns");
        object.remove("taskbarSeenAt");

        let restored: Thread = serde_json::from_value(value).expect("旧线程应可反序列化");
        assert!(!restored.starred);
        assert_eq!(restored.unread_turns, 0);
        assert_eq!(restored.taskbar_seen_at, None);
    }
}

/// 按字符数从中间截断，保留首尾，避免单条过长撑爆预算
fn tool_output_text(content: &[Value]) -> String {
    let mut parts = Vec::new();
    for item in content {
        match item["type"].as_str() {
            Some("content") => {
                if let Some(text) = item["content"]["text"].as_str() {
                    if !text.trim().is_empty() {
                        parts.push(text.to_string());
                    }
                }
            }
            Some("diff") => {
                if let Some(path) = item["path"].as_str() {
                    parts.push(format!("（修改文件：{path}）"));
                }
            }
            _ => {}
        }
    }
    parts.join("\n")
}

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
