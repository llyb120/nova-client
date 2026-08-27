//! Reasonix 精简上下文：只追加的 slim memory（用户提示原文 + 助手结论），
//! 压力分层（warn/snip/elide/force）下先压缩旧工具结果，再把完成的历史折叠为冻结摘要。
//! 文件格式沿用原 sessions/<id>.slim.json（version 3）。

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use tokio::sync::{mpsc, oneshot};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct SlimTurn {
    pub user_prompts: Vec<String>,
    pub conclusion: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct SlimMemory {
    pub digests: Vec<String>,
    pub preserved_user_prompts: Vec<String>,
    pub turns: Vec<SlimTurn>,
    pub pending_messages: Vec<Value>,
    pub full_messages: Vec<Value>,
    pub context_tokens: u64,
    pub context_stage: String,
    pub context_tier: String,
    pub rewrite_version: u64,
    pub system_prompt_snapshot: String,
    pub system_fingerprint: String,
    pub system_prompt_hash: String,
    pub tool_schema_hash: String,
    pub last_shape_rewrite_version: u64,
    pub consecutive_compactions: u64,
    pub compact_stuck: bool,
}

impl SlimMemory {
    pub fn new() -> Self {
        SlimMemory {
            context_stage: "full".into(),
            context_tier: "normal".into(),
            ..Default::default()
        }
    }

    /// 从磁盘加载（兼容 v2 summary 字段与缺失字段）。
    pub fn load(root: &Path, session_id: &str) -> Self {
        let Ok(text) = std::fs::read_to_string(slim_path(root, session_id)) else {
            return Self::new();
        };
        let Ok(mut value) = serde_json::from_str::<Value>(&text) else {
            return Self::new();
        };
        // v2 兼容：summary 字符串并入 digests。
        if value.get("digests").is_none() {
            if let Some(summary) = value.get("summary").and_then(Value::as_str) {
                let summary = summary.trim();
                if !summary.is_empty() {
                    value["digests"] = json!([summary]);
                }
            }
        }
        let mut memory: Self = serde_json::from_value(value).unwrap_or_else(|_| Self::new());
        memory.normalize();
        memory
    }

    pub fn save(&self, root: &Path, session_id: &str) -> Result<(), String> {
        std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
        let mut value = serde_json::to_value(self).map_err(|e| e.to_string())?;
        value["version"] = json!(3);
        let path = slim_path(root, session_id);
        let temp = path.with_extension("slim.tmp");
        std::fs::write(
            &temp,
            serde_json::to_string(&value).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        std::fs::rename(&temp, &path).map_err(|e| e.to_string())
    }

    /// 取消的轮次没有结论，其提示并入下一条完成的结论；中断会留下多条原文提示在一轮里。
    pub fn normalize(&mut self) {
        let mut normalized: Vec<SlimTurn> = Vec::new();
        let mut pending: Vec<String> = Vec::new();
        for turn in std::mem::take(&mut self.turns) {
            pending.extend(
                turn.user_prompts
                    .into_iter()
                    .map(|p| p.trim().to_string())
                    .filter(|p| !p.is_empty()),
            );
            let conclusion = turn.conclusion.trim().to_string();
            if !conclusion.is_empty() {
                normalized.push(SlimTurn {
                    user_prompts: std::mem::take(&mut pending),
                    conclusion,
                });
            }
        }
        if !pending.is_empty() {
            normalized.push(SlimTurn {
                user_prompts: pending,
                conclusion: String::new(),
            });
        }
        self.turns = normalized;
        self.digests = std::mem::take(&mut self.digests)
            .into_iter()
            .map(|d| d.trim().to_string())
            .filter(|d| !d.is_empty())
            .collect();
        self.preserved_user_prompts = std::mem::take(&mut self.preserved_user_prompts)
            .into_iter()
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty())
            .collect();
    }

    pub fn append_turn(&mut self, prompt: &str) {
        let prompt = prompt.trim();
        if !prompt.is_empty() {
            self.turns.push(SlimTurn {
                user_prompts: vec![prompt.to_string()],
                conclusion: String::new(),
            });
        }
    }

    pub fn set_latest_conclusion(&mut self, content: &Value) {
        let conclusion = text_content(content);
        if conclusion.is_empty() {
            return;
        }
        let needs_new = match self.turns.last() {
            Some(turn) => !turn.conclusion.is_empty(),
            None => true,
        };
        if needs_new {
            self.turns.push(SlimTurn::default());
        }
        if let Some(turn) = self.turns.last_mut() {
            turn.conclusion = conclusion;
        }
    }

    /// 克隆并去掉当前轮：中断轮整体由原生消息承载时连提示一起去掉，否则只去掉新提示。
    pub fn without_current(&self, pending_messages: bool) -> SlimMemory {
        let mut memory = self.clone();
        memory.normalize();
        let drop_last_prompt = matches!(memory.turns.last(), Some(t) if t.conclusion.is_empty());
        if drop_last_prompt {
            if pending_messages {
                memory.turns.pop();
            } else if let Some(last) = memory.turns.last_mut() {
                last.user_prompts.pop();
            }
        }
        if matches!(memory.turns.last(), Some(t) if t.user_prompts.is_empty() && t.conclusion.is_empty())
        {
            memory.turns.pop();
        }
        memory
    }

    pub fn format(&self) -> String {
        let mut memory = self.clone();
        memory.normalize();
        let mut sections = vec![
            "请使用下面的只追加会话记录继续工作。不要要求用户重复之前的要求。".to_string(),
            String::new(),
            "## Conversation".to_string(),
        ];
        if !memory.preserved_user_prompts.is_empty() {
            sections.push(String::new());
            sections.push("### Preserved user requests".into());
            for prompt in &memory.preserved_user_prompts {
                sections.push(format!("User:\n{prompt}"));
            }
        }
        if !memory.digests.is_empty() {
            sections.push(String::new());
            sections.push("### Frozen digests".into());
            for (index, digest) in memory.digests.iter().enumerate() {
                sections.push(format!("Digest {}:\n{digest}", index + 1));
            }
        }
        for turn in &memory.turns {
            for prompt in &turn.user_prompts {
                sections.push(String::new());
                sections.push(format!("User:\n{prompt}"));
            }
            if !turn.conclusion.is_empty() {
                sections.push(format!("Assistant:\n{}", turn.conclusion));
            }
        }
        sections.join("\n")
    }

    /// 超出容量时把最旧的完成轮次折叠为一条冻结摘要；最新完成轮（含其后的中断提示）受保护。
    pub async fn compact<F, Fut>(
        &mut self,
        current_tokens: u64,
        max_tokens: u64,
        summarize: F,
    ) -> bool
    where
        F: FnOnce(String) -> Fut,
        Fut: std::future::Future<Output = Result<String, String>>,
    {
        self.normalize();
        let formatted = SlimMemory {
            digests: self.digests.clone(),
            turns: self.turns.clone(),
            ..SlimMemory::new()
        }
        .format();
        if current_tokens < max_tokens {
            self.consecutive_compactions = 0;
            self.compact_stuck = false;
            return false;
        }
        if self.compact_stuck || self.turns.len() < 2 || self.consecutive_compactions >= 2 {
            if self.consecutive_compactions >= 2 {
                self.compact_stuck = true;
            }
            return false;
        }
        let last_has_conclusion = matches!(self.turns.last(), Some(t) if !t.conclusion.is_empty());
        let protected = if last_has_conclusion {
            1
        } else {
            2.min(self.turns.len())
        };
        let split = self.turns.len() - protected;
        if split == 0 {
            return false;
        }
        let compacted: Vec<SlimTurn> = self.turns[..split].to_vec();
        let preserved: Vec<String> = compacted
            .iter()
            .flat_map(|turn| turn.user_prompts.clone())
            .filter(|prompt| prompt.len() <= 2_000 && !self.preserved_user_prompts.contains(prompt))
            .collect();
        let earlier = SlimMemory {
            turns: compacted,
            ..SlimMemory::new()
        }
        .format();
        let Ok(digest) = summarize(earlier).await else {
            return false;
        };
        let digest = digest.trim().to_string();
        if digest.is_empty() {
            return false;
        }
        let _ = formatted; // 保持与 JS 相同的提前返回语义（formatted 仅用于限额判断）
        self.preserved_user_prompts.extend(preserved);
        self.digests.push(digest);
        self.turns = self.turns[split..].to_vec();
        self.rewrite_version += 1;
        self.consecutive_compactions += 1;
        true
    }

    /// 从旧的完整原生会话（sessions/<id>.json）播种。
    pub fn seed_from_messages(&mut self, messages: &[Value]) {
        for message in messages {
            match message.get("role").and_then(Value::as_str) {
                Some("user") => {
                    let prompt = text_content(message.get("content").unwrap_or(&Value::Null));
                    self.append_turn(&prompt);
                }
                Some("assistant")
                    if message.get("stopReason").and_then(Value::as_str) != Some("error") =>
                {
                    self.set_latest_conclusion(message.get("content").unwrap_or(&Value::Null));
                }
                _ => {}
            }
        }
        self.full_messages = messages.to_vec();
        self.context_tokens = context_tokens_from_messages(messages);
        self.context_stage = "full".into();
        self.normalize();
    }
}

/// fresh turn 已进入 slim stage 时，用只追加记忆替换已完成的原生轨迹。
/// 当前用户提示已由 append_turn 记入 memory，因此格式化时先去掉当前提示再显式追加一次。
pub fn message_with_slim_memory(text: &str, memory: &SlimMemory) -> String {
    format!(
        "{}\n\nUser:\n{}",
        memory.without_current(false).format(),
        text
    )
}

fn slim_path(root: &Path, session_id: &str) -> PathBuf {
    root.join(format!("{session_id}.slim.json"))
}

fn pending_checkpoint_path(root: &Path, session_id: &str) -> PathBuf {
    root.join(format!("{session_id}.pending.json"))
}

/// 后台 checkpoint 单写者。调用线程只克隆并发送最新 snapshot；序列化、写盘和
/// rename 全部在 spawn_blocking 中完成。worker 合并 300ms 内的连续更新，避免工具批次
/// 对同一份不断增长的历史反复全量写盘。
pub struct PendingCheckpointWriter {
    tx: mpsc::UnboundedSender<CheckpointCommand>,
    task: tokio::task::JoinHandle<()>,
}

enum CheckpointCommand {
    Snapshot(Vec<Value>),
    Close {
        flush: bool,
        done: oneshot::Sender<()>,
    },
}

impl PendingCheckpointWriter {
    pub fn spawn(root: PathBuf, session_id: String) -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            let mut latest: Option<Vec<Value>> = None;
            loop {
                let command = if latest.is_some() {
                    tokio::select! {
                        command = rx.recv() => command,
                        _ = tokio::time::sleep(std::time::Duration::from_millis(300)) => {
                            if let Some(messages) = latest.take() {
                                write_pending_checkpoint_blocking(root.clone(), session_id.clone(), messages).await;
                            }
                            continue;
                        }
                    }
                } else {
                    rx.recv().await
                };
                match command {
                    Some(CheckpointCommand::Snapshot(messages)) => latest = Some(messages),
                    Some(CheckpointCommand::Close { flush, done }) => {
                        if flush {
                            if let Some(messages) = latest.take() {
                                write_pending_checkpoint_blocking(
                                    root.clone(),
                                    session_id.clone(),
                                    messages,
                                )
                                .await;
                            }
                        }
                        let _ = done.send(());
                        break;
                    }
                    None => {
                        if let Some(messages) = latest.take() {
                            write_pending_checkpoint_blocking(
                                root.clone(),
                                session_id.clone(),
                                messages,
                            )
                            .await;
                        }
                        break;
                    }
                }
            }
        });
        Self { tx, task }
    }

    pub fn checkpoint(&self, messages: &[Value]) {
        let _ = self.tx.send(CheckpointCommand::Snapshot(messages.to_vec()));
    }

    /// flush=true 用于取消/失败：先确保最新活动轨迹已落盘，再保存 slim memory。
    /// 正常完成时 slim memory 即将完整保存，可丢弃尚未写出的冗余 snapshot。
    pub async fn close(self, flush: bool) {
        let (done_tx, done_rx) = oneshot::channel();
        let _ = self.tx.send(CheckpointCommand::Close {
            flush,
            done: done_tx,
        });
        let _ = done_rx.await;
        let _ = self.task.await;
    }
}

async fn write_pending_checkpoint_blocking(
    root: PathBuf,
    session_id: String,
    messages: Vec<Value>,
) {
    let result = tokio::task::spawn_blocking(move || {
        write_pending_checkpoint(&root, &session_id, &messages)
    })
    .await;
    match result {
        Ok(Ok(())) => {}
        Ok(Err(error)) => eprintln!("lyra: 中断轨迹 checkpoint 写盘失败：{error}"),
        Err(error) => eprintln!("lyra: 中断轨迹 checkpoint worker 失败：{error}"),
    }
}

/// 原子 checkpoint 写入。只允许从后台 blocking worker 调用，避免阻塞 async executor。
fn write_pending_checkpoint(
    root: &Path,
    session_id: &str,
    messages: &[Value],
) -> Result<(), String> {
    std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
    let path = pending_checkpoint_path(root, session_id);
    let temp = path.with_extension("pending.tmp");
    std::fs::write(
        &temp,
        serde_json::to_string(messages).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    std::fs::rename(&temp, &path).map_err(|e| e.to_string())
}

/// 轮末写盘没机会执行（强杀/panic/退出）时，checkpoint 保留了截至中断点的完整轨迹。
pub fn load_pending_checkpoint(root: &Path, session_id: &str) -> Option<Vec<Value>> {
    std::fs::read_to_string(pending_checkpoint_path(root, session_id))
        .ok()
        .and_then(|text| serde_json::from_str::<Vec<Value>>(&text).ok())
        .filter(|messages| !messages.is_empty())
}

pub fn clear_pending_checkpoint(root: &Path, session_id: &str) {
    let _ = std::fs::remove_file(pending_checkpoint_path(root, session_id));
}

pub fn messages_path(root: &Path, session_id: &str) -> PathBuf {
    root.join(format!("{session_id}.json"))
}

pub fn load_legacy_messages(root: &Path, session_id: &str) -> Vec<Value> {
    std::fs::read_to_string(messages_path(root, session_id))
        .ok()
        .and_then(|text| serde_json::from_str::<Vec<Value>>(&text).ok())
        .unwrap_or_default()
}

pub fn text_content(content: &Value) -> String {
    if let Some(text) = content.as_str() {
        return text.trim().to_string();
    }
    let Some(parts) = content.as_array() else {
        return String::new();
    };
    parts
        .iter()
        .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// 已完成的 OpenAI 轮次不需要重放 reasoning；Responses 工具调用去掉 item-id 后缀，
/// 保留与工具结果配对的 call id。只用于完成的轮次，中断轨迹保持原样。
pub fn strip_completed_openai_reasoning(messages: &[Value]) -> Vec<Value> {
    let normalized: Vec<Value> = messages
        .iter()
        .map(|message| {
            if message.get("role").and_then(Value::as_str) != Some("assistant") {
                return message.clone();
            }
            let Some(content) = message.get("content").and_then(Value::as_array) else {
                return message.clone();
            };
            let mut changed = false;
            let mut next = Vec::with_capacity(content.len());
            for block in content {
                match block.get("type").and_then(Value::as_str) {
                    Some("thinking") => changed = true,
                    Some("toolCall") => {
                        let id = block.get("id").and_then(Value::as_str).unwrap_or_default();
                        if id.contains('|') {
                            changed = true;
                            let mut block = block.clone();
                            block["id"] = json!(id.split('|').next().unwrap_or(id));
                            next.push(block);
                        } else {
                            next.push(block.clone());
                        }
                    }
                    _ => next.push(block.clone()),
                }
            }
            if !changed {
                return message.clone();
            }
            let mut message = message.clone();
            message["content"] = Value::Array(next);
            message
        })
        .collect();
    sanitize_completed_tool_pairs(&normalized)
}

/// Provider 要求 completed assistant toolCall 与 toolResult 完整配对。旧会话可能因强杀、
/// 早期 bridge bug 或 call-id 后缀迁移留下孤儿。这里只清理已完成历史；pending 活动轨迹
/// 不调用本函数，仍逐字保留供恢复。
pub fn sanitize_completed_tool_pairs(messages: &[Value]) -> Vec<Value> {
    let result_ids = messages
        .iter()
        .filter(|message| message.get("role").and_then(Value::as_str) == Some("toolResult"))
        .filter_map(|message| message.get("toolCallId").and_then(Value::as_str))
        .map(|id| id.split('|').next().unwrap_or(id).to_string())
        .collect::<std::collections::HashSet<_>>();
    let call_ids = messages
        .iter()
        .filter(|message| message.get("role").and_then(Value::as_str) == Some("assistant"))
        .flat_map(|message| {
            message
                .get("content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .filter(|part| part.get("type").and_then(Value::as_str) == Some("toolCall"))
        .filter_map(|part| part.get("id").and_then(Value::as_str))
        .map(|id| id.split('|').next().unwrap_or(id).to_string())
        .collect::<std::collections::HashSet<_>>();

    messages
        .iter()
        .filter_map(
            |message| match message.get("role").and_then(Value::as_str) {
                Some("assistant") => {
                    let Some(content) = message.get("content").and_then(Value::as_array) else {
                        return Some(message.clone());
                    };
                    let next = content
                        .iter()
                        .filter_map(|part| {
                            if part.get("type").and_then(Value::as_str) != Some("toolCall") {
                                return Some(part.clone());
                            }
                            let id = part
                                .get("id")
                                .and_then(Value::as_str)
                                .map(|id| id.split('|').next().unwrap_or(id))?;
                            if !result_ids.contains(id) {
                                return None;
                            }
                            let mut part = part.clone();
                            part["id"] = json!(id);
                            Some(part)
                        })
                        .collect::<Vec<_>>();
                    if next.is_empty() {
                        None
                    } else {
                        let mut message = message.clone();
                        message["content"] = Value::Array(next);
                        Some(message)
                    }
                }
                Some("toolResult") => message
                    .get("toolCallId")
                    .and_then(Value::as_str)
                    .map(|id| id.split('|').next().unwrap_or(id))
                    .filter(|id| call_ids.contains(*id))
                    .map(|id| {
                        let mut message = message.clone();
                        message["toolCallId"] = json!(id);
                        message
                    }),
                _ => Some(message.clone()),
            },
        )
        .collect()
}

/// 中断轨迹不能删除活动 toolCall；为旧 checkpoint 中缺失的结果补一个明确的中断占位，
/// 使 provider 校验通过并让模型决定是否重跑。孤儿 toolResult 和空 assistant 安全丢弃。
pub fn repair_interrupted_tool_pairs(messages: &[Value]) -> Vec<Value> {
    let call_ids = messages
        .iter()
        .filter(|message| message.get("role").and_then(Value::as_str) == Some("assistant"))
        .flat_map(|message| {
            message
                .get("content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .filter(|part| part.get("type").and_then(Value::as_str) == Some("toolCall"))
        .filter_map(|part| part.get("id").and_then(Value::as_str))
        .map(str::to_string)
        .collect::<std::collections::HashSet<_>>();
    let result_ids = messages
        .iter()
        .filter(|message| message.get("role").and_then(Value::as_str) == Some("toolResult"))
        .filter_map(|message| message.get("toolCallId").and_then(Value::as_str))
        .map(str::to_string)
        .collect::<std::collections::HashSet<_>>();
    let mut out = Vec::new();
    for message in messages {
        if message.get("role").and_then(Value::as_str) == Some("toolResult") {
            if message
                .get("toolCallId")
                .and_then(Value::as_str)
                .is_some_and(|id| call_ids.contains(id))
            {
                out.push(message.clone());
            }
            continue;
        }
        let content = message.get("content").and_then(Value::as_array);
        if message.get("role").and_then(Value::as_str) == Some("assistant")
            && content.is_some_and(|parts| parts.is_empty())
        {
            continue;
        }
        out.push(message.clone());
        if message.get("role").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        for part in content.into_iter().flatten() {
            if part.get("type").and_then(Value::as_str) != Some("toolCall") {
                continue;
            }
            let Some(id) = part.get("id").and_then(Value::as_str) else {
                continue;
            };
            if result_ids.contains(id) {
                continue;
            }
            out.push(json!({
                "role": "toolResult",
                "toolCallId": id,
                "toolName": part.get("name").and_then(Value::as_str).unwrap_or_default(),
                "content": [{ "type": "text", "text": "[interrupted tool call — no result was persisted; re-run if needed]" }],
                "isError": true,
            }));
        }
    }
    out
}

/// 每个 assistant 请求都上报当时的上下文大小；取最大（最近一次），跨工具调用不求和。
pub fn context_tokens_from_messages(messages: &[Value]) -> u64 {
    let mut tokens = 0u64;
    for message in messages {
        if message.get("role").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let Some(usage) = message.get("usage") else {
            continue;
        };
        let total = usage
            .get("totalTokens")
            .or_else(|| usage.get("total_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let output = usage
            .get("output")
            .or_else(|| usage.get("outputTokens"))
            .or_else(|| usage.get("output_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let input = usage.get("input").and_then(Value::as_u64).unwrap_or(0);
        let cached = usage
            .get("cacheRead")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .saturating_add(usage.get("cacheWrite").and_then(Value::as_u64).unwrap_or(0));
        // 有的 provider 把 cached 计入 input，有的单列。
        let measured = if total > output {
            total - output
        } else if input >= cached {
            input
        } else {
            input + cached
        };
        tokens = tokens.max(measured);
    }
    tokens
}

pub fn pressure_tier(current_tokens: u64, context_window: u64) -> &'static str {
    if current_tokens == 0 || context_window == 0 {
        return "normal";
    }
    let ratio = current_tokens as f64 / context_window as f64;
    if ratio >= 0.9 {
        "force"
    } else if ratio >= 0.8 {
        "elide"
    } else if ratio >= 0.6 {
        "snip"
    } else if ratio >= 0.5 {
        "warn"
    } else {
        "normal"
    }
}

fn compact_tool_text(text: &str, tier: &str, tool_call_id: Option<&str>) -> String {
    if text.contains("[elided tool result") {
        return text.to_string();
    }
    let bytes = text.len();
    if tier == "snip" && bytes <= 8 * 1024 {
        return text.to_string();
    }
    let id = tool_call_id
        .filter(|id| !id.is_empty())
        .map(|id| format!(" {id}"))
        .unwrap_or_default();
    if tier == "elide" || tier == "force" {
        return format!("[elided tool result{id} — {bytes} bytes; re-run the tool if needed]");
    }
    let head: String = text.chars().take(3_000).collect();
    let tail: String = text
        .chars()
        .rev()
        .take(2_000)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("{head}\n\n…[snipped tool result{id} — {bytes} bytes]…\n\n{tail}")
}

/// 摘要之前先对较旧的原生工具结果做单向、稳定的压缩；最近 6 条保持完整。
fn compact_native_tool_results_with_preserve(
    messages: &[Value],
    tier: &str,
    preserve_recent: usize,
) -> (Vec<Value>, bool) {
    if !matches!(tier, "snip" | "elide" | "force") {
        return (messages.to_vec(), false);
    }
    let cutoff = messages.len().saturating_sub(preserve_recent);
    let mut changed = false;
    let next: Vec<Value> = messages
        .iter()
        .enumerate()
        .map(|(index, message)| {
            if index >= cutoff || message.get("role").and_then(Value::as_str) != Some("toolResult")
            {
                return message.clone();
            }
            let Some(content) = message.get("content").and_then(Value::as_array) else {
                return message.clone();
            };
            let tool_call_id = message.get("toolCallId").and_then(Value::as_str);
            let mut content_changed = false;
            let next_content: Vec<Value> = content
                .iter()
                .map(|part| {
                    if part.get("type").and_then(Value::as_str) != Some("text") {
                        return part.clone();
                    }
                    let text = part.get("text").and_then(Value::as_str).unwrap_or_default();
                    let compacted = compact_tool_text(text, tier, tool_call_id);
                    if compacted == text {
                        return part.clone();
                    }
                    content_changed = true;
                    let mut part = part.clone();
                    part["text"] = json!(compacted);
                    part
                })
                .collect();
            if !content_changed {
                return message.clone();
            }
            changed = true;
            let mut message = message.clone();
            message["content"] = Value::Array(next_content);
            message
        })
        .collect();
    if changed {
        (next, true)
    } else {
        (messages.to_vec(), false)
    }
}

/// 摘要之前先对较旧的原生工具结果做单向、稳定的压缩；最近 6 条保持完整。
pub fn compact_native_tool_results(messages: &[Value], tier: &str) -> (Vec<Value>, bool) {
    compact_native_tool_results_with_preserve(messages, tier, 6)
}

/// Provider 已明确拒绝上下文时不再保留最近工具结果，优先确保当前任务能继续。
pub fn compact_all_native_tool_results(messages: &[Value]) -> (Vec<Value>, bool) {
    compact_native_tool_results_with_preserve(messages, "force", 0)
}

pub fn should_use_full_context(memory: &SlimMemory, max_tokens: u64, max_chars: usize) -> bool {
    if !memory.pending_messages.is_empty() {
        return true;
    }
    if memory.context_stage == "slim" {
        return false;
    }
    if memory.full_messages.is_empty() {
        return memory.turns.is_empty();
    }
    if memory.context_tokens > 0 {
        memory.context_tokens < max_tokens
    } else {
        serde_json::to_string(&memory.full_messages)
            .map(|text| text.len() < max_chars)
            .unwrap_or(false)
    }
}

/// 把已完成的历史替换为精简记忆，保留活动用户轮及其后的 assistant/tool 消息。
/// 完成的历史是缓存重置区，进行中的工作逐字保留。
pub fn rebase_native_context(
    messages: &[Value],
    active_turn_start: i64,
    memory: &SlimMemory,
) -> (Vec<Value>, bool) {
    let start = active_turn_start;
    if start <= 0
        || start as usize >= messages.len()
        || messages[start as usize].get("role").and_then(Value::as_str) != Some("user")
    {
        return (messages.to_vec(), false);
    }
    let compact_context = memory.without_current(true).format();
    let mut current = messages[start as usize].clone();
    if let Some(text) = current.get("content").and_then(Value::as_str) {
        current["content"] = json!(format!("{compact_context}\n\nUser:\n{text}"));
    } else if let Some(parts) = current.get("content").and_then(Value::as_array) {
        let mut parts = parts.clone();
        let first_text = parts
            .iter()
            .position(|part| part.get("type").and_then(Value::as_str) == Some("text"));
        match first_text {
            Some(index) => {
                let text = parts[index]
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                parts[index]["text"] = json!(format!("{compact_context}\n\nUser:\n{text}"));
            }
            None => parts.insert(0, json!({ "type": "text", "text": compact_context })),
        }
        current["content"] = Value::Array(parts);
    } else {
        current["content"] = json!([{ "type": "text", "text": compact_context }]);
    }
    let mut next = vec![current];
    next.extend(messages[start as usize + 1..].iter().cloned());
    (next, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn append_and_format_roundtrip() {
        let mut memory = SlimMemory::new();
        memory.append_turn("修复登录 bug");
        memory.set_latest_conclusion(&json!([{ "type": "text", "text": "已修复" }]));
        memory.append_turn("再加个测试");
        let text = memory.without_current(false).format();
        assert!(text.contains("修复登录 bug"));
        assert!(text.contains("已修复"));
        assert!(!text.contains("再加个测试"));
    }

    #[test]
    fn slim_prompt_replaces_completed_native_history() {
        let mut memory = SlimMemory::new();
        memory.append_turn("旧要求");
        memory.set_latest_conclusion(&json!([{ "type": "text", "text": "旧结论" }]));
        memory.append_turn("当前要求");
        let text = message_with_slim_memory("当前要求", &memory);
        assert!(text.contains("旧要求"));
        assert!(text.contains("旧结论"));
        assert_eq!(text.matches("当前要求").count(), 1);
        assert!(text.ends_with("User:\n当前要求"));
    }

    #[test]
    fn pressure_tiers() {
        assert_eq!(pressure_tier(0, 100), "normal");
        assert_eq!(pressure_tier(50, 100), "warn");
        assert_eq!(pressure_tier(60, 100), "snip");
        assert_eq!(pressure_tier(80, 100), "elide");
        assert_eq!(pressure_tier(90, 100), "force");
    }

    #[test]
    fn removes_orphaned_completed_tool_pairs() {
        let messages = vec![
            json!({ "role": "assistant", "content": [
                { "type": "text", "text": "keep" },
                { "type": "toolCall", "id": "paired|fc_1", "name": "read", "arguments": {} },
                { "type": "toolCall", "id": "orphan|fc_2", "name": "read", "arguments": {} }
            ]}),
            json!({ "role": "toolResult", "toolCallId": "paired|fc_1", "content": [{ "type": "text", "text": "ok" }] }),
            json!({ "role": "toolResult", "toolCallId": "result-only", "content": [{ "type": "text", "text": "bad" }] }),
        ];
        let out = strip_completed_openai_reasoning(&messages);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0]["content"].as_array().unwrap().len(), 2);
        assert_eq!(out[0]["content"][1]["id"], "paired");
        assert_eq!(out[1]["toolCallId"], "paired");
    }

    #[test]
    fn repairs_interrupted_missing_tool_result() {
        let messages = vec![json!({ "role": "assistant", "content": [
            { "type": "toolCall", "id": "call|fc", "name": "read", "arguments": {} }
        ]})];
        let out = repair_interrupted_tool_pairs(&messages);
        assert_eq!(out.len(), 2);
        assert_eq!(out[1]["toolCallId"], "call|fc");
        assert_eq!(out[1]["isError"], true);
    }

    #[test]
    fn compacts_old_tool_results_only() {
        let big = "x".repeat(16 * 1024);
        let mut messages: Vec<Value> = (0..8)
            .map(|i| {
                json!({
                    "role": "toolResult", "toolCallId": format!("call-{i}"),
                    "content": [{ "type": "text", "text": big }]
                })
            })
            .collect();
        messages.push(json!({ "role": "assistant", "content": [] }));
        let (out, changed) = compact_native_tool_results(&messages, "elide");
        assert!(changed);
        assert!(out[0]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("[elided tool result call-0"));
        // 最近 6 条保持完整
        assert_eq!(out[3]["content"][0]["text"].as_str().unwrap(), big);
    }

    #[test]
    fn rebase_requires_active_user_turn() {
        let memory = SlimMemory::new();
        let messages = vec![
            json!({ "role": "user", "content": "old" }),
            json!({ "role": "assistant", "content": [{ "type": "text", "text": "done" }] }),
            json!({ "role": "user", "content": "new task" }),
        ];
        let (next, changed) = rebase_native_context(&messages, 2, &memory);
        assert!(changed);
        assert_eq!(next.len(), 1);
        assert!(next[0]["content"]
            .as_str()
            .unwrap()
            .contains("## Conversation"));
    }

    #[test]
    fn seed_builds_turns_and_stage() {
        let mut memory = SlimMemory::new();
        memory.seed_from_messages(&[
            json!({ "role": "user", "content": [{ "type": "text", "text": "目标" }] }),
            json!({ "role": "assistant", "content": [{ "type": "text", "text": "完成" }], "stopReason": "stop" }),
        ]);
        assert_eq!(memory.turns.len(), 1);
        assert_eq!(memory.turns[0].conclusion, "完成");
        assert_eq!(memory.context_stage, "full");
    }

    #[test]
    fn overflow_compaction_elides_even_recent_tool_results() {
        let messages = vec![json!({
            "role": "toolResult",
            "toolCallId": "call-1",
            "content": [{ "type": "text", "text": "x".repeat(10_000) }]
        })];
        let (normal, normal_changed) = compact_native_tool_results(&messages, "force");
        assert!(!normal_changed);
        assert_eq!(normal, messages);

        let (recovered, changed) = compact_all_native_tool_results(&messages);
        assert!(changed);
        assert!(recovered[0]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("[elided tool result call-1"));
    }
    #[tokio::test]
    async fn checkpoint_writer_flushes_latest_interrupted_snapshot() {
        let root =
            std::env::temp_dir().join(format!("nova-checkpoint-writer-{}", uuid::Uuid::new_v4()));
        let session = "cancelled";
        let writer = PendingCheckpointWriter::spawn(root.clone(), session.into());
        writer.checkpoint(&[json!({ "role": "user", "content": "first" })]);
        writer.checkpoint(&[
            json!({ "role": "user", "content": "first" }),
            json!({ "role": "assistant", "content": [{ "type": "toolCall", "id": "call-1", "name": "read" }] }),
        ]);
        // 模拟取消/失败收尾：debounce 尚未到期也必须等待最新轨迹落盘。
        writer.close(true).await;
        let loaded = load_pending_checkpoint(&root, session).expect("checkpoint 应存在");
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[1]["content"][0]["id"], "call-1");
        let _ = std::fs::remove_dir_all(root);
    }
}
