//! Bridge protocol translation, ported from `scripts/alkaid-bridge-common.mjs`
//! (`startedToolItem`) and the `tool_execution_end` handler in
//! `scripts/alkaid-context-reasonix.mjs`.
//!
//! These build the line-JSON `item` payloads that the Tauri `AlkaidAdapter`
//! parses (`command_execution` / `file_change` / `mcp_tool_call`). Field order
//! follows the node object literals and `undefined` fields are omitted, matching
//! `JSON.stringify`.

use serde_json::{json, Map, Value};

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
