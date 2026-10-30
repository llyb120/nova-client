//! 会话历史（Codex ContextManager 的等价物）：内存中的规范消息列表，
//! 记录时追加到 rollout；发给模型前做配对修复（缺结果补 aborted、孤儿结果丢弃）。

use crate::lyra::rollout::Rollout;
use serde_json::{json, Value};

/// 规范历史：只追加，从不改写。上下文投影（压缩）在 context.rs 里另存。
#[derive(Default)]
pub struct History {
    items: Vec<Value>,
    rollout: Option<Rollout>,
}

impl History {
    pub fn new(items: Vec<Value>, rollout: Option<Rollout>) -> Self {
        Self { items, rollout }
    }

    pub fn items(&self) -> &[Value] {
        &self.items
    }

    pub fn record(&mut self, message: Value) {
        if let Some(rollout) = self.rollout.as_mut() {
            rollout.append(&message);
        }
        self.items.push(message);
    }

    /// 为没有结果的工具调用补上 aborted 结果并落盘（Codex：中断时合成 aborted 输出）。
    pub fn close_pending_calls(&mut self, text: &str) {
        for (id, name) in pending_calls(&self.items) {
            self.record(json!({
                "role": "toolResult",
                "toolCallId": id,
                "toolName": name,
                "content": [{ "type": "text", "text": text }],
                "isError": true,
                "timestamp": now_ms(),
            }));
        }
    }

    /// 发给 provider 的规范化视图：`completed_before` 之前的轮次视为已完成，
    /// OpenAI 系列去掉其 reasoning；最后统一修复工具配对。
    pub fn for_prompt(messages: &[Value], completed_before: usize, strip_reasoning: bool) -> Vec<Value> {
        let split = completed_before.min(messages.len());
        let mut out = if strip_reasoning {
            strip_completed_openai_reasoning(&messages[..split])
        } else {
            messages[..split].to_vec()
        };
        out.extend_from_slice(&messages[split..]);
        let mut out = repair_interrupted_tool_pairs(&out);
        trim_browser_dom(&mut out);
        out
    }
}

/// Only the model projection rolls off old DOM; canonical history and documentPath stay intact.
fn trim_browser_dom(messages: &mut [Value]) {
    let mut seen = std::collections::HashMap::<String, usize>::new();
    for message in messages.iter_mut().rev() {
        if message["role"] != "toolResult" || message["toolName"] != "chrome" { continue; }
        let Some(parts) = message["content"].as_array_mut() else { continue; };
        let mut omitted = false;
        for part in parts.iter_mut().filter(|p| p["type"] == "text") {
            let Some(text) = part["text"].as_str() else { continue; };
            // ACP tags the first text part before provider normalization.
            let tagged = text.split_once("] ").filter(|(tag, _)| tag.strip_prefix("[m")
                .is_some_and(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit())));
            let (prefix, body) = tagged.map_or(("", text), |(tag, body)| (&text[..tag.len() + 2], body));
            let Ok(mut result) = serde_json::from_str::<Value>(body) else { continue; };
            let Some(tab) = result["tabTag"].as_str().filter(|s| !s.is_empty()) else { continue; };
            if !result["pages"].as_array().is_some_and(|p| !p.is_empty()) { continue; }
            let count = seen.entry(tab.to_owned()).or_default();
            *count += 1;
            if *count <= 2 { continue; }
            result.as_object_mut().unwrap().remove("pages");
            result["domOmitted"] = json!("该标签仅保留最近两次 DOM；旧观察正文已省略，完整记录见 documentPath。");
            part["text"] = json!(format!("{prefix}{result}"));
            omitted = true;
        }
        if omitted {
            if let Some(details) = message["details"].as_object_mut() { details.remove("pages"); }
        }
    }
}

/// provider 尚未返回 usage 时的估算：ASCII 约 4 字符/token，非 ASCII 约 1 字符/token。
pub(crate) fn estimate_text_tokens(text: &str) -> u64 {
    crate::lyra::tokens::TokenCounter::default().count_text(text)
}

pub fn user_message(text: &str, images: &[Value]) -> Value {
    let mut parts = Vec::new();
    if !text.is_empty() {
        parts.push(json!({ "type": "text", "text": text }));
    }
    parts.extend(images.iter().cloned());
    json!({ "role": "user", "content": parts, "timestamp": now_ms() })
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 仍缺结果的工具调用（id, name），按出现顺序。
pub fn pending_calls(messages: &[Value]) -> Vec<(String, String)> {
    let results = messages
        .iter()
        .filter(|m| m.get("role").and_then(Value::as_str) == Some("toolResult"))
        .filter_map(|m| m.get("toolCallId").and_then(Value::as_str))
        .collect::<std::collections::HashSet<_>>();
    messages
        .iter()
        .filter(|m| m.get("role").and_then(Value::as_str) == Some("assistant"))
        .flat_map(|m| m.get("content").and_then(Value::as_array).into_iter().flatten())
        .filter(|p| p.get("type").and_then(Value::as_str) == Some("toolCall"))
        .filter_map(|p| {
            let id = p.get("id").and_then(Value::as_str)?;
            (!results.contains(id)).then(|| {
                (id.to_string(), p.get("name").and_then(Value::as_str).unwrap_or_default().to_string())
            })
        })
        .collect()
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


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_keeps_two_dom_observations_per_tab_without_losing_action_results() {
        let observed = |tab: &str, id: &str| json!({"tabTag":tab,"snapshotId":id,"documentPath":format!("{id}.json"),
            "pages":[{"elements":format!("{id}.1 button Save"),"text":"old DOM","tables":[[1]]}],
            "status":"needs_review","reason":"partial input","completedActions":1,"value":{"count":7},
            "images":[{"imageId":id,"path":format!("{id}.webp"),"frameStream":format!("{tab}:viewport")} ]});
        let results = vec![
            ("chrome", observed("C1", "old")),
            ("chrome", observed("C2", "other-tab")),
            ("jianlai", observed("C1", "other-tool")),
            ("chrome", observed("C1", "middle")),
            ("chrome", json!({"tabTag":"C1","status":"executed","value":"readback"})),
            ("chrome", observed("C1", "new")),
            ("chrome", json!({"pages":[{"text":"unbound"}]})),
            ("chrome", json!(["not an observation"])),
        ];
        let mut canonical = Vec::new();
        for (i, (tool, result)) in results.iter().enumerate() {
            canonical.push(json!({"role":"assistant","content":[{"type":"toolCall","id":i.to_string(),"name":tool,"arguments":{}}]}));
            canonical.push(json!({"role":"toolResult","toolCallId":i.to_string(),"toolName":tool,
                "content":[{"type":"text","text":result.to_string()}],"details":result}));
        }
        let before = canonical.clone();
        let projected = History::for_prompt(&canonical, 0, false);
        assert_eq!(canonical, before);
        assert_eq!(projected.len(), canonical.len());
        let old: Value = serde_json::from_str(projected[1]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert!(old.get("pages").is_none() && old["domOmitted"].is_string());
        for key in ["tabTag","snapshotId","documentPath","status","reason","completedActions","value","images"] {
            assert_eq!(old[key], results[0].1[key], "{key}");
        }
        assert!(projected[1]["details"].get("pages").is_none());
        assert_eq!(projected[1]["details"]["images"], canonical[1]["details"]["images"]);
        assert_eq!(projected[2..], canonical[2..], "other tabs/tools, latest two DOMs and script values stay intact");
        assert_eq!(History::for_prompt(&projected, 0, false), projected);
        let mut tagged = canonical.clone();
        for i in (1..tagged.len()).step_by(2) {
            tagged[i]["content"][0]["text"] = json!(format!("[m{i:05}] {}", tagged[i]["content"][0]["text"].as_str().unwrap()));
        }
        let tagged_view = History::for_prompt(&tagged, 0, false);
        assert_eq!(tagged_view[1]["content"][0]["text"], format!("[m00001] {old}"));
        assert_eq!(tagged_view[2..], tagged[2..]);
        let mut malformed = canonical.clone();
        malformed[1]["content"][0]["text"] = json!("not JSON");
        assert_eq!(History::for_prompt(&malformed, 0, false), malformed);
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
    fn closes_pending_calls_once() {
        let mut history = History::new(
            vec![json!({ "role": "assistant", "content": [
                { "type": "toolCall", "id": "a", "name": "bash", "arguments": {} },
                { "type": "toolCall", "id": "b", "name": "read", "arguments": {} }
            ]}), json!({ "role": "toolResult", "toolCallId": "a", "content": [] })],
            None,
        );
        history.close_pending_calls("aborted");
        history.close_pending_calls("aborted");
        assert_eq!(history.items().len(), 3);
        assert_eq!(history.items()[2]["toolCallId"], "b");
    }
}
