//! OpenAI request payload transforms: prompt cache key injection and tool
//! output clamping. Ported from `alkaid-core.mjs`.

use serde_json::{Map, Value};

use crate::text::{clamp_tool_output_text, utf16_len};

/// OpenAI prompt cache keys longer than 64 code points are rejected.
pub const OPENAI_PROMPT_CACHE_KEY_MAX_LENGTH: usize = 64;

/// Port of `clampPromptCacheKey(key)`: trim, drop empties, and cap at 64 *code
/// points* (`Array.from` iterates code points, not UTF-16 units).
pub fn clamp_prompt_cache_key(key: Option<&str>) -> Option<String> {
    let normalized = key?.trim();
    if normalized.is_empty() {
        return None;
    }
    let chars: Vec<char> = normalized.chars().collect();
    if chars.len() <= OPENAI_PROMPT_CACHE_KEY_MAX_LENGTH {
        return Some(normalized.to_string());
    }
    Some(chars[..OPENAI_PROMPT_CACHE_KEY_MAX_LENGTH].iter().collect())
}

/// Port of `injectOpenAIPromptCacheKey(payload, sessionId)`. Returns a new
/// payload with `prompt_cache_key` set, or `None` when nothing should change
/// (non-object payload, an existing non-empty key, or an unusable session id).
pub fn inject_openai_prompt_cache_key(payload: &Value, session_id: Option<&str>) -> Option<Value> {
    let obj = payload.as_object()?;
    if let Some(Value::String(existing)) = obj.get("prompt_cache_key") {
        if !existing.trim().is_empty() {
            return None;
        }
    }
    if let Some(Value::String(existing)) = obj.get("promptCacheKey") {
        if !existing.trim().is_empty() {
            return None;
        }
    }
    let key = clamp_prompt_cache_key(session_id)?;
    let mut next = obj.clone();
    next.insert("prompt_cache_key".to_string(), Value::String(key));
    Some(Value::Object(next))
}

/// Port of `clampOpenAIPayloadToolOutputs(payload, maxChars)`. Clamps oversized
/// tool outputs already present in a Responses `input[].output` or Completions
/// `messages[].content` (role=tool). Returns a new payload when anything
/// changed, otherwise `None`.
pub fn clamp_openai_payload_tool_outputs(payload: &Value, max_chars: usize) -> Option<Value> {
    let obj = payload.as_object()?;
    let mut next = obj.clone();
    let mut changed = false;

    if let Some(Value::Array(items)) = next.get("input") {
        let mut new_items = Vec::with_capacity(items.len());
        for item in items {
            let mut it = item.clone();
            if it.get("type").and_then(Value::as_str) == Some("function_call_output") {
                match it.get("output").cloned() {
                    Some(Value::String(output)) => {
                        if utf16_len(&output) > max_chars {
                            let clamped = clamp_tool_output_text(Some(&output), max_chars);
                            it["output"] = Value::String(clamped);
                            changed = true;
                        }
                    }
                    Some(Value::Array(parts)) => {
                        let mut new_parts = Vec::with_capacity(parts.len());
                        let mut parts_changed = false;
                        for part in parts {
                            let mut p = part.clone();
                            if p.get("type").and_then(Value::as_str) == Some("input_text") {
                                if let Some(text) =
                                    p.get("text").and_then(Value::as_str).map(str::to_string)
                                {
                                    if utf16_len(&text) > max_chars {
                                        let clamped = clamp_tool_output_text(Some(&text), max_chars);
                                        p["text"] = Value::String(clamped);
                                        parts_changed = true;
                                    }
                                }
                            }
                            new_parts.push(p);
                        }
                        if parts_changed {
                            it["output"] = Value::Array(new_parts);
                            changed = true;
                        }
                    }
                    _ => {}
                }
            }
            new_items.push(it);
        }
        next["input"] = Value::Array(new_items);
    }

    if let Some(Value::Array(messages)) = next.get("messages") {
        let mut new_messages = Vec::with_capacity(messages.len());
        for message in messages {
            let mut m = message.clone();
            if m.get("role").and_then(Value::as_str) == Some("tool") {
                if let Some(content) = m.get("content").and_then(Value::as_str).map(str::to_string)
                {
                    if utf16_len(&content) > max_chars {
                        let clamped = clamp_tool_output_text(Some(&content), max_chars);
                        m["content"] = Value::String(clamped);
                        changed = true;
                    }
                }
            }
            new_messages.push(m);
        }
        next["messages"] = Value::Array(new_messages);
    }

    changed.then_some(Value::Object(next))
}

/// Port of `mergeAlkaidUsage(total, usage)`: sum `input`, `output`, `cacheRead`
/// and `cacheWrite`, ignoring non-finite additions. Returns `total` unchanged
/// when `usage` is absent.
pub fn merge_usage(total: Option<&Value>, usage: Option<&Value>) -> Option<Value> {
    let Some(usage) = usage else {
        return total.cloned();
    };
    const KEYS: [&str; 4] = ["input", "output", "cacheRead", "cacheWrite"];
    let mut merged: Map<String, Value> = match total {
        Some(Value::Object(obj)) => obj.clone(),
        _ => {
            let mut fresh = Map::new();
            for key in KEYS {
                fresh.insert(key.to_string(), json_number(0.0));
            }
            fresh
        }
    };
    for key in KEYS {
        let current = merged.get(key).and_then(Value::as_f64).unwrap_or(0.0);
        let addition = usage
            .get(key)
            .and_then(Value::as_f64)
            .filter(|value| value.is_finite())
            .unwrap_or(0.0);
        merged.insert(key.to_string(), json_number(current + addition));
    }
    Some(Value::Object(merged))
}

/// Serialize a float the way `JSON.stringify` prints a JS number: integral
/// values without a decimal point, non-integral values as floats. This keeps
/// parity with golden vectors parsed back into `serde_json::Number`.
fn json_number(value: f64) -> Value {
    if value.fract() == 0.0 && value.abs() < 9.007_199_254_740_992e15 {
        Value::from(value as i64)
    } else {
        serde_json::Number::from_f64(value)
            .map(Value::Number)
            .unwrap_or(Value::Null)
    }
}
