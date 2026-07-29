//! Agent message construction, ported from the private helpers in
//! `pi-agent-core/dist/agent-loop.js` (`createErrorToolResult`,
//! `createToolResultMessage`).
//!
//! Messages are represented as `serde_json::Value` to match the node bridge's
//! JSON exactly. Field presence (not order) is what matters for the protocol:
//! `details` is omitted when the tool result has none, and `addedToolNames`
//! only appears when non-empty.

use serde_json::{json, Map, Value};

/// Port of `createErrorToolResult(message)`: a text-only error result with an
/// empty `details` object.
pub fn create_error_tool_result(message: &str) -> Value {
    json!({
        "content": [{ "type": "text", "text": message }],
        "details": {},
    })
}

/// Port of `createToolResultMessage(finalized)`. `timestamp` is injected
/// because node stamps messages with the non-deterministic `Date.now()`;
/// callers doing parity checks pass a fixed value.
pub fn create_tool_result_message(
    tool_call_id: &str,
    tool_name: &str,
    result: &Value,
    is_error: bool,
    timestamp: u64,
) -> Value {
    let content = result
        .get("content")
        .cloned()
        .unwrap_or_else(|| json!([]));
    let mut map = Map::new();
    map.insert("role".to_string(), json!("toolResult"));
    map.insert("toolCallId".to_string(), json!(tool_call_id));
    map.insert("toolName".to_string(), json!(tool_name));
    map.insert("content".to_string(), content);
    // node: `details: finalized.result.details` — an absent field stays absent
    // (undefined is dropped by JSON.stringify); an explicit null is preserved.
    if let Some(details) = result.get("details") {
        map.insert("details".to_string(), details.clone());
    }
    if let Some(added) = result.get("addedToolNames").and_then(Value::as_array) {
        if !added.is_empty() {
            map.insert("addedToolNames".to_string(), Value::Array(added.clone()));
        }
    }
    map.insert("isError".to_string(), json!(is_error));
    map.insert("timestamp".to_string(), json!(timestamp));
    Value::Object(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_result_shape() {
        let value = create_error_tool_result("Tool foo not found");
        assert_eq!(
            value,
            json!({ "content": [{ "type": "text", "text": "Tool foo not found" }], "details": {} })
        );
    }

    #[test]
    fn tool_result_omits_absent_details() {
        let value = create_tool_result_message("c2", "read", &json!({ "content": [] }), false, 42);
        assert!(value.get("details").is_none());
        assert_eq!(value["content"], json!([]));
    }

    #[test]
    fn tool_result_null_content_becomes_empty() {
        let value = create_tool_result_message("c3", "x", &json!({}), true, 42);
        assert_eq!(value["content"], json!([]));
        assert_eq!(value["isError"], json!(true));
    }

    #[test]
    fn tool_result_added_tool_names_only_when_nonempty() {
        let empty = create_tool_result_message(
            "c4",
            "x",
            &json!({ "content": [], "addedToolNames": [] }),
            false,
            42,
        );
        assert!(empty.get("addedToolNames").is_none());
        let present = create_tool_result_message(
            "c5",
            "x",
            &json!({ "content": [], "addedToolNames": ["newtool"] }),
            false,
            42,
        );
        assert_eq!(present["addedToolNames"], json!(["newtool"]));
    }
}
