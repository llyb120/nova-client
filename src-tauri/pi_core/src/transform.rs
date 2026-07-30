//! Cross-provider message normalization, ported from pi-ai
//! `transform-messages.js`. Shared by all provider request builders.
//!
//! Handles: null-content normalization, image downgrade for non-vision models,
//! thinking-block handling (keep for same model, convert to text cross-model),
//! tool-call ID normalization, error/aborted assistant skipping, and synthetic
//! tool-result insertion for orphaned tool calls.

use serde_json::{json, Value};

const NON_VISION_USER_IMAGE_PLACEHOLDER: &str = "(image omitted: model does not support images)";
const NON_VISION_TOOL_IMAGE_PLACEHOLDER: &str =
    "(tool image omitted: model does not support images)";

fn replace_images_with_placeholder(content: &[Value], placeholder: &str) -> Vec<Value> {
    let mut result: Vec<Value> = Vec::new();
    let mut previous_was_placeholder = false;
    for block in content {
        if block.get("type").and_then(Value::as_str) == Some("image") {
            if !previous_was_placeholder {
                result.push(json!({ "type": "text", "text": placeholder }));
            }
            previous_was_placeholder = true;
            continue;
        }
        result.push(block.clone());
        previous_was_placeholder =
            block.get("text").and_then(Value::as_str) == Some(placeholder);
    }
    result
}

fn downgrade_unsupported_images(messages: &[Value], model_supports_image: bool) -> Vec<Value> {
    if model_supports_image {
        return messages.to_vec();
    }
    messages
        .iter()
        .map(|msg| {
            let role = msg.get("role").and_then(Value::as_str).unwrap_or("");
            let content = msg.get("content").and_then(Value::as_array);
            match (role, content) {
                ("user", Some(_)) => {
                    let content: Vec<Value> = msg["content"].as_array().unwrap().to_vec();
                    let replaced =
                        replace_images_with_placeholder(&content, NON_VISION_USER_IMAGE_PLACEHOLDER);
                    let mut m = msg.clone();
                    m["content"] = Value::Array(replaced);
                    m
                }
                ("toolResult", Some(_)) => {
                    let content: Vec<Value> = msg["content"].as_array().unwrap().to_vec();
                    let replaced =
                        replace_images_with_placeholder(&content, NON_VISION_TOOL_IMAGE_PLACEHOLDER);
                    let mut m = msg.clone();
                    m["content"] = Value::Array(replaced);
                    m
                }
                _ => msg.clone(),
            }
        })
        .collect()
}

fn is_same_model(msg: &Value, model_provider: &str, model_api: &str, model_id: &str) -> bool {
    msg.get("provider").and_then(Value::as_str) == Some(model_provider)
        && msg.get("api").and_then(Value::as_str) == Some(model_api)
        && msg.get("model").and_then(Value::as_str) == Some(model_id)
}

/// Port of `transformMessages`. `normalize_tool_call_id` maps an id to its
/// provider-normalized form (or returns it unchanged).
pub fn transform_messages<F>(
    messages: &[Value],
    model_provider: &str,
    model_api: &str,
    model_id: &str,
    model_supports_image: bool,
    normalize_tool_call_id: F,
) -> Vec<Value>
where
    F: Fn(&str) -> String,
{
    // Normalize null content.
    let normalized: Vec<Value> = messages
        .iter()
        .map(|msg| {
            if msg.get("content").map_or(true, Value::is_null) {
                let mut m = msg.clone();
                m["content"] = json!([]);
                m
            } else {
                msg.clone()
            }
        })
        .collect();

    let image_aware = downgrade_unsupported_images(&normalized, model_supports_image);

    // First pass: transform assistant messages, build tool-call ID map.
    let mut tool_call_id_map: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    let transformed: Vec<Value> = image_aware
        .iter()
        .map(|msg| {
            let role = msg.get("role").and_then(Value::as_str).unwrap_or("");
            match role {
                "user" => msg.clone(),
                "toolResult" => {
                    let tool_call_id =
                        msg.get("toolCallId").and_then(Value::as_str).unwrap_or("");
                    if let Some(normalized_id) = tool_call_id_map.get(tool_call_id) {
                        if normalized_id != tool_call_id {
                            let mut m = msg.clone();
                            m["toolCallId"] = Value::String(normalized_id.clone());
                            return m;
                        }
                    }
                    msg.clone()
                }
                "assistant" => {
                    let same = is_same_model(msg, model_provider, model_api, model_id);
                    let content = msg
                        .get("content")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    let mut new_content: Vec<Value> = Vec::new();
                    for block in &content {
                        let block_type = block.get("type").and_then(Value::as_str).unwrap_or("");
                        match block_type {
                            "thinking" => {
                                let redacted =
                                    block.get("redacted").and_then(Value::as_bool).unwrap_or(false);
                                if redacted {
                                    if same {
                                        new_content.push(block.clone());
                                    }
                                    continue;
                                }
                                let has_signature = block
                                    .get("thinkingSignature")
                                    .and_then(Value::as_str)
                                    .map_or(false, |s| !s.is_empty());
                                if same && has_signature {
                                    new_content.push(block.clone());
                                    continue;
                                }
                                let thinking = block
                                    .get("thinking")
                                    .and_then(Value::as_str)
                                    .unwrap_or("");
                                if thinking.trim().is_empty() {
                                    continue;
                                }
                                if same {
                                    new_content.push(block.clone());
                                } else {
                                    new_content.push(json!({ "type": "text", "text": thinking }));
                                }
                            }
                            "text" => {
                                if same {
                                    new_content.push(block.clone());
                                } else {
                                    let text =
                                        block.get("text").and_then(Value::as_str).unwrap_or("");
                                    new_content.push(json!({ "type": "text", "text": text }));
                                }
                            }
                            "toolCall" => {
                                let mut normalized_block = block.clone();
                                if !same {
                                    if normalized_block.get("thoughtSignature").is_some() {
                                        if let Some(obj) = normalized_block.as_object_mut() {
                                            obj.remove("thoughtSignature");
                                        }
                                    }
                                    let id = block
                                        .get("id")
                                        .and_then(Value::as_str)
                                        .unwrap_or("")
                                        .to_string();
                                    let normalized_id = normalize_tool_call_id(&id);
                                    if normalized_id != id {
                                        tool_call_id_map.insert(id, normalized_id.clone());
                                        normalized_block["id"] = Value::String(normalized_id);
                                    }
                                }
                                new_content.push(normalized_block);
                            }
                            _ => new_content.push(block.clone()),
                        }
                    }
                    let mut m = msg.clone();
                    m["content"] = Value::Array(new_content);
                    m
                }
                _ => msg.clone(),
            }
        })
        .collect();

    // Second pass: skip error/aborted assistants, insert synthetic tool results.
    let mut result: Vec<Value> = Vec::new();
    let mut pending_tool_calls: Vec<Value> = Vec::new();
    let mut existing_tool_result_ids: std::collections::HashSet<String> =
        std::collections::HashSet::new();

    let flush_pending = |result: &mut Vec<Value>,
                         pending: &mut Vec<Value>,
                         existing: &mut std::collections::HashSet<String>| {
        if !pending.is_empty() {
            for tc in pending.drain(..) {
                let id = tc.get("id").and_then(Value::as_str).unwrap_or("").to_string();
                if !existing.contains(&id) {
                    let name = tc.get("name").and_then(Value::as_str).unwrap_or("");
                    result.push(json!({
                        "role": "toolResult",
                        "toolCallId": id,
                        "toolName": name,
                        "content": [{ "type": "text", "text": "No result provided" }],
                        "isError": true,
                        "timestamp": 0,
                    }));
                }
            }
            existing.clear();
        }
    };

    for msg in &transformed {
        let role = msg.get("role").and_then(Value::as_str).unwrap_or("");
        match role {
            "assistant" => {
                flush_pending(&mut result, &mut pending_tool_calls, &mut existing_tool_result_ids);
                let stop_reason = msg.get("stopReason").and_then(Value::as_str).unwrap_or("");
                if stop_reason == "error" || stop_reason == "aborted" {
                    continue;
                }
                let tool_calls: Vec<Value> = msg
                    .get("content")
                    .and_then(Value::as_array)
                    .map(|parts| {
                        parts
                            .iter()
                            .filter(|b| b.get("type").and_then(Value::as_str) == Some("toolCall"))
                            .cloned()
                            .collect()
                    })
                    .unwrap_or_default();
                if !tool_calls.is_empty() {
                    pending_tool_calls = tool_calls;
                    existing_tool_result_ids.clear();
                }
                result.push(msg.clone());
            }
            "toolResult" => {
                let id = msg
                    .get("toolCallId")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                existing_tool_result_ids.insert(id);
                result.push(msg.clone());
            }
            "user" => {
                flush_pending(&mut result, &mut pending_tool_calls, &mut existing_tool_result_ids);
                result.push(msg.clone());
            }
            _ => result.push(msg.clone()),
        }
    }
    flush_pending(&mut result, &mut pending_tool_calls, &mut existing_tool_result_ids);
    result
}
