//! Bridge protocol translation, ported from `scripts/alkaid-bridge-common.mjs`
//! (`startedToolItem`) and the `tool_execution_end` handler in
//! `scripts/alkaid-context-reasonix.mjs`.
//!
//! These build the line-JSON `item` payloads that the Tauri `AlkaidAdapter`
//! parses (`command_execution` / `file_change` / `mcp_tool_call`). Field order
//! follows the node object literals and `undefined` fields are omitted, matching
//! `JSON.stringify`.

use serde_json::{json, Map, Value};

use crate::payload::merge_usage;

/// Port of `startedToolItem(event)`: classify a `tool_execution_start` event
/// into an in-progress protocol item.
pub fn started_tool_item(event: &Value) -> Value {
    let tool_name = event.get("toolName").and_then(Value::as_str).unwrap_or("");
    let args = match event.get("args") {
        Some(Value::Object(_)) => event.get("args").cloned().unwrap(),
        _ => json!({}),
    };
    let file_change = tool_name == "edit" || tool_name == "write" || tool_name == "edit_files";

    let mut item_type = "mcp_tool_call";
    let mut command: Option<Value> = None;
    let mut server = "Vega".to_string();
    let mut tool = tool_name.to_string();
    let mut changes: Option<Value> = None;

    if tool_name == "bash" {
        item_type = "command_execution";
        command = args.get("command").cloned();
    } else if let Some(rest) = tool_name.strip_prefix("mcp__") {
        let parts: Vec<&str> = rest.split("__").collect();
        // node: `[, server, tool] = toolName.split("__")`
        if let Some(s) = parts.first() {
            server = s.to_string();
        }
        if let Some(t) = parts.get(1) {
            tool = t.to_string();
        }
    } else if file_change {
        item_type = "file_change";
        let files: Vec<Value> = if tool_name == "edit_files" {
            args.get("files")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
        } else {
            vec![args.clone()]
        };
        let mapped: Vec<Value> = files
            .iter()
            .filter_map(|file| {
                let path = file.get("path").and_then(Value::as_str)?;
                Some(json!({ "path": path, "kind": "update" }))
            })
            .collect();
        changes = Some(Value::Array(mapped));
    }

    // Insertion order matches the node literal; absent (undefined) fields are
    // omitted to mirror JSON.stringify.
    let mut map = Map::new();
    map.insert(
        "id".to_string(),
        event.get("toolCallId").cloned().unwrap_or(Value::Null),
    );
    map.insert("type".to_string(), json!(item_type));
    map.insert("status".to_string(), json!("in_progress"));
    map.insert("arguments".to_string(), event.get("args").cloned().unwrap_or(Value::Null));
    if let Some(command) = command {
        map.insert("command".to_string(), command);
    }
    map.insert("server".to_string(), json!(server));
    map.insert("tool".to_string(), json!(tool));
    if let Some(changes) = changes {
        map.insert("changes".to_string(), changes);
    }
    Value::Object(map)
}

/// Port of the `tool_execution_end` aggregated-output computation:
/// `event.result?.content?.map(part => part.text ?? "").join("\n") ?? ""`.
pub fn aggregated_output(result: Option<&Value>) -> String {
    let Some(result) = result else {
        return String::new();
    };
    let Some(content) = result.get("content").and_then(Value::as_array) else {
        return String::new();
    };
    content
        .iter()
        .map(|part| {
            part.get("text")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Port of the `tool_execution_end` handler: extend the in-progress item with a
/// terminal status, the aggregated output, and `result`/`error`.
pub fn completed_tool_item(started: &Value, end_event: &Value) -> Value {
    let is_error = end_event.get("isError").and_then(Value::as_bool).unwrap_or(false);
    let result = end_event.get("result");
    let output = aggregated_output(result);

    let mut map = started.as_object().cloned().unwrap_or_default();
    map.insert(
        "status".to_string(),
        json!(if is_error { "failed" } else { "completed" }),
    );
    map.insert("aggregated_output".to_string(), json!(output));
    if is_error {
        map.insert("error".to_string(), json!({ "message": output }));
    } else if let Some(result) = result {
        map.insert("result".to_string(), result.clone());
    }
    Value::Object(map)
}

/// Assembles the bridge protocol item stream from agent events, ported from the
/// `subscribe` handler in `scripts/alkaid-context-reasonix.mjs`.
///
/// Accumulates assistant text and thinking across `message_update` deltas,
/// merges per-turn usage, and translates tool execution into in-progress then
/// terminal protocol items. Assistant/thinking ids are generated deterministically
/// (`assistant-N`/`thinking-N`) rather than node's `randomUUID`; parity checks
/// normalize ids regardless.
#[derive(Default)]
pub struct ProtocolAccumulator {
    pub items: Vec<Value>,
    pub usage: Option<Value>,
    text: String,
    thinking: String,
    assistant_id: String,
    thinking_id: String,
    seq: u64,
    tool_items: std::collections::HashMap<String, Value>,
}

impl ProtocolAccumulator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one agent event, appending any resulting protocol items.
    pub fn on_event(&mut self, event: &Value) {
        match event.get("type").and_then(Value::as_str) {
            Some("message_start") => {
                let is_assistant = event
                    .get("message")
                    .and_then(|m| m.get("role"))
                    .and_then(Value::as_str)
                    == Some("assistant");
                if is_assistant {
                    self.text.clear();
                    self.thinking.clear();
                    self.assistant_id = format!("assistant-{}", self.seq);
                    self.thinking_id = format!("thinking-{}", self.seq);
                    self.seq += 1;
                }
            }
            Some("message_update") => {
                let ame = event.get("assistantMessageEvent");
                match ame.and_then(|a| a.get("type")).and_then(Value::as_str) {
                    Some("text_delta") => {
                        if let Some(delta) =
                            ame.and_then(|a| a.get("delta")).and_then(Value::as_str)
                        {
                            self.text.push_str(delta);
                            self.items.push(json!({ "type": "item", "item": {
                                "id": self.assistant_id,
                                "type": "agent_message",
                                "text": self.text,
                            }}));
                        }
                    }
                    Some("thinking_delta") => {
                        if let Some(delta) =
                            ame.and_then(|a| a.get("delta")).and_then(Value::as_str)
                        {
                            self.thinking.push_str(delta);
                            self.items.push(json!({ "type": "item", "item": {
                                "id": self.thinking_id,
                                "type": "reasoning",
                                "text": self.thinking,
                            }}));
                        }
                    }
                    _ => {}
                }
            }
            Some("message_end") => {
                if let Some(message) = event.get("message") {
                    if message.get("role").and_then(Value::as_str) == Some("assistant") {
                        if let Some(usage) = message.get("usage") {
                            self.usage = merge_usage(self.usage.as_ref(), Some(usage));
                        }
                    }
                }
            }
            Some("tool_execution_start") => {
                let item = started_tool_item(event);
                let id = event
                    .get("toolCallId")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                self.tool_items.insert(id, item.clone());
                self.items.push(json!({ "type": "item", "item": item }));
            }
            Some("tool_execution_end") => {
                let id = event
                    .get("toolCallId")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                if let Some(started) = self.tool_items.get(&id) {
                    let completed = completed_tool_item(started, event);
                    self.items.push(json!({ "type": "item", "item": completed }));
                }
            }
            _ => {}
        }
    }

    /// Consume the accumulator and return the assembled protocol items.
    pub fn finish(self) -> Vec<Value> {
        self.items
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accumulates_text_and_tool_items() {
        let mut acc = ProtocolAccumulator::new();
        acc.on_event(&json!({ "type": "message_start", "message": { "role": "assistant" } }));
        acc.on_event(&json!({ "type": "message_update", "assistantMessageEvent": { "type": "text_delta", "delta": "Hel" } }));
        acc.on_event(&json!({ "type": "message_update", "assistantMessageEvent": { "type": "text_delta", "delta": "lo" } }));
        acc.on_event(&json!({ "type": "message_end", "message": { "role": "assistant", "usage": { "input": 5, "output": 3, "cacheRead": 0, "cacheWrite": 0 } } }));
        acc.on_event(&json!({ "type": "tool_execution_start", "toolCallId": "c1", "toolName": "bash", "args": { "command": "ls" } }));
        acc.on_event(&json!({ "type": "tool_execution_end", "toolCallId": "c1", "toolName": "bash", "result": { "content": [{ "type": "text", "text": "file.txt" }] }, "isError": false }));

        let items = acc.finish();
        assert_eq!(items.len(), 4);
        assert_eq!(items[0]["item"]["type"], json!("agent_message"));
        assert_eq!(items[0]["item"]["text"], json!("Hel"));
        assert_eq!(items[1]["item"]["text"], json!("Hello"));
        assert_eq!(items[1]["item"]["id"], items[0]["item"]["id"]);
        assert_eq!(items[2]["item"]["type"], json!("command_execution"));
        assert_eq!(items[2]["item"]["status"], json!("in_progress"));
        assert_eq!(items[3]["item"]["status"], json!("completed"));
        assert_eq!(items[3]["item"]["aggregated_output"], json!("file.txt"));
    }

    #[test]
    fn merges_usage_across_turns() {
        let mut acc = ProtocolAccumulator::new();
        let usage = |i: u64, o: u64| json!({ "type": "message_end", "message": { "role": "assistant", "usage": { "input": i, "output": o, "cacheRead": 0, "cacheWrite": 0 } } });
        acc.on_event(&usage(5, 3));
        acc.on_event(&usage(2, 1));
        assert_eq!(acc.usage, Some(json!({ "input": 7, "output": 4, "cacheRead": 0, "cacheWrite": 0 })));
    }

    #[test]
    fn thinking_accumulates_separately_from_text() {
        let mut acc = ProtocolAccumulator::new();
        acc.on_event(&json!({ "type": "message_start", "message": { "role": "assistant" } }));
        acc.on_event(&json!({ "type": "message_update", "assistantMessageEvent": { "type": "thinking_delta", "delta": "hmm" } }));
        acc.on_event(&json!({ "type": "message_update", "assistantMessageEvent": { "type": "text_delta", "delta": "answer" } }));
        let items = acc.finish();
        assert_eq!(items[0]["item"]["type"], json!("reasoning"));
        assert_eq!(items[0]["item"]["text"], json!("hmm"));
        assert_eq!(items[1]["item"]["type"], json!("agent_message"));
        assert_eq!(items[1]["item"]["text"], json!("answer"));
        assert_ne!(items[0]["item"]["id"], items[1]["item"]["id"]);
    }
}
