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

    /// 对齐 Codex：保留 reasoning、签名和原生 ID，只修复工具配对并裁剪旧 DOM。
    pub fn for_prompt(messages: &[Value]) -> Vec<Value> {
        let mut out = repair_interrupted_tool_pairs(messages);
        trim_browser_dom(&mut out);
        out
    }
}

/// Only the model projection rolls off old DOM/UIA control lists; canonical history and documentPath stay intact.
fn trim_browser_dom(messages: &mut [Value]) {
    let mut seen = std::collections::HashMap::<String, usize>::new();
    for message in messages.iter_mut().rev() {
        if message["role"] != "toolResult" { continue; }
        let Some(tool_name) = message["toolName"].as_str() else { continue; };
        let is_browser = crate::browser::tools::is_tool(tool_name);
        let is_jianlai = crate::jianlai_tools::is_tool(tool_name);
        if !is_browser && !is_jianlai { continue; }
        let Some(parts) = message["content"].as_array_mut() else { continue; };
        let mut omitted = false;
        for part in parts.iter_mut().filter(|p| p["type"] == "text") {
            let Some(text) = part["text"].as_str() else { continue; };
            // ACP tags the first text part before provider normalization.
            let tagged = text.split_once("] ").filter(|(tag, _)| tag.strip_prefix("[m")
                .is_some_and(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit())));
            let (prefix, body) = tagged.map_or(("", text), |(tag, body)| (&text[..tag.len() + 2], body));
            let Ok(mut result) = serde_json::from_str::<Value>(body) else { continue; };
            if is_browser {
                let Some(tab) = result["tab"].as_str().or_else(|| result["tabTag"].as_str()).filter(|s| !s.is_empty()) else { continue; };
                let has_old_dom = result["pages"].as_array().is_some_and(|p| !p.is_empty());
                let has_new_dom = result["elements"].is_string() || result["text"].is_string();
                if !has_old_dom && !has_new_dom { continue; }
                let count = seen.entry(format!("browser:{tab}")).or_default();
                *count += 1;
                if *count <= 2 { continue; }
                if let Some(obj) = result.as_object_mut() {
                    obj.remove("pages");
                    obj.remove("elements");
                    obj.remove("text");
                }
                result["domOmitted"] = json!("该标签仅保留最近两次 DOM；旧观察正文已省略。");
            } else {
                let has_controls = result["elements"].is_string()
                    || result["items"].as_array().is_some_and(|a| !a.is_empty());
                if !has_controls { continue; }
                let stream = result["images"]
                    .as_array()
                    .and_then(|imgs| imgs.first())
                    .and_then(|i| i["frameStream"].as_str().or_else(|| i["imageId"].as_str()))
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .or_else(|| result["windowId"].as_u64().map(|w| format!("win-{w}")))
                    .unwrap_or_else(|| "desktop".to_string());
                let count = seen.entry(format!("jianlai:{stream}")).or_default();
                *count += 1;
                if *count <= 2 { continue; }
                if let Some(obj) = result.as_object_mut() {
                    obj.remove("elements");
                    obj.remove("items");
                }
                result["elementsOmitted"] = json!("仅保留最近两次桌面控件清单；旧控件列表已省略。");
            }
            part["text"] = json!(format!("{prefix}{result}"));
            omitted = true;
        }
        if omitted {
            if let Some(details) = message["details"].as_object_mut() {
                details.remove("pages");
                details.remove("elements");
                details.remove("items");
                details.remove("text");
            }
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
            ("chrome_inspect", observed("C2", "other-tab")),
            ("jianlai", observed("C1", "other-tool")),
            ("chrome_inspect", observed("C1", "middle")),
            ("chrome_evaluate", json!({"tabTag":"C1","status":"executed","value":"readback"})),
            ("chrome_act", observed("C1", "new")),
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
        let projected = History::for_prompt(&canonical);
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
        assert_eq!(History::for_prompt(&projected), projected);
        let mut tagged = canonical.clone();
        for i in (1..tagged.len()).step_by(2) {
            tagged[i]["content"][0]["text"] = json!(format!("[m{i:05}] {}", tagged[i]["content"][0]["text"].as_str().unwrap()));
        }
        let tagged_view = History::for_prompt(&tagged);
        assert_eq!(tagged_view[1]["content"][0]["text"], format!("[m00001] {old}"));
        assert_eq!(tagged_view[2..], tagged[2..]);
        let mut malformed = canonical.clone();
        malformed[1]["content"][0]["text"] = json!("not JSON");
        assert_eq!(History::for_prompt(&malformed), malformed);
    }

    #[test]
    fn prompt_rolls_off_stale_jianlai_control_lists_per_stream() {
        let obs = |stream: &str, id: &str| json!({
            "snapshotId": id,
            "status": "executed",
            "elements": format!("[1] Button \"{id}\" @(100,80)"),
            "images": [{"imageId": stream, "frameStream": stream, "path": format!("{id}.jpg")}]
        });
        let results = [
            ("jianlai_view", obs("monitor-1", "s1")),
            ("jianlai_view", obs("monitor-2", "s2-other")),
            ("jianlai_act", obs("monitor-1", "s2")),
            ("jianlai_act", obs("monitor-1", "s3")),
        ];
        let mut canonical = Vec::new();
        for (i, (tool, r)) in results.iter().enumerate() {
            canonical.push(json!({"role":"assistant","content":[{"type":"toolCall","id":i.to_string(),"name":tool,"arguments":{}}]}));
            canonical.push(json!({"role":"toolResult","toolCallId":i.to_string(),"toolName":tool,
                "content":[{"type":"text","text":r.to_string()}],"details":r}));
        }
        let projected = History::for_prompt(&canonical);
        let oldest: Value = serde_json::from_str(projected[1]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert!(oldest.get("elements").is_none() && oldest["elementsOmitted"].is_string());
        assert_eq!(oldest["snapshotId"], "s1");
        assert!(projected[1]["details"].get("elements").is_none());
        assert_eq!(projected[2..], canonical[2..], "other monitors and latest two control lists stay intact");
    }

    #[test]
    fn prompt_preserves_reasoning_and_native_ids_across_user_turns() {
        let mut messages = vec![
            user_message("read a", &[]),
            json!({ "role": "assistant", "content": [
                { "type": "thinking", "thinking": "reasoning", "thinkingSignature": "opaque",
                  "reasoningDetails": [{ "type": "reasoning.encrypted", "data": "encrypted" }] },
                { "type": "toolCall", "id": "call_1|fc_1", "name": "read", "arguments": { "path": "a" } }
            ] }),
            json!({ "role": "toolResult", "toolCallId": "call_1|fc_1", "content": [{ "type": "text", "text": "ok" }] }),
        ];
        let previous = History::for_prompt(&messages);
        messages.push(user_message("next turn", &[]));
        let prompt = History::for_prompt(&messages);
        assert_eq!(prompt, messages, "request normalization must not rewrite reasoning or native IDs");
        assert_eq!(&prompt[..previous.len()], previous, "a new user turn only appends to the previous prefix");
    }

    #[test]
    fn prompt_repairs_tool_pairs_without_rewriting_native_ids() {
        let messages = vec![
            json!({ "role": "assistant", "content": [
                { "type": "text", "text": "keep" },
                { "type": "toolCall", "id": "paired|fc_1", "name": "read", "arguments": {} },
                { "type": "toolCall", "id": "orphan|fc_2", "name": "read", "arguments": {} }
            ]}),
            json!({ "role": "toolResult", "toolCallId": "paired|fc_1", "content": [{ "type": "text", "text": "ok" }] }),
            json!({ "role": "toolResult", "toolCallId": "result-only", "content": [{ "type": "text", "text": "bad" }] }),
        ];
        let out = History::for_prompt(&messages);
        assert_eq!(out.len(), 3);
        assert_eq!(out[0], messages[0]);
        assert_eq!(out[1]["toolCallId"], "orphan|fc_2");
        assert_eq!(out[1]["isError"], true);
        assert_eq!(out[2], messages[1]);
        assert_eq!(History::for_prompt(&out), out, "repair is idempotent");
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
