//! Slim memory —— 忠实移植自 `scripts/alkaid-slim-memory.mjs` 的纯函数部分。
use serde_json::{Map, Value};
use std::collections::BTreeMap;

pub fn create_slim_memory() -> Value {
    serde_json::json!({
        "digests": [],
        "preservedUserPrompts": [],
        "turns": [],
        "pendingMessages": [],
        "fullMessages": [],
        "contextTokens": 0,
        "contextStage": "full",
        "contextTier": "normal",
        "rewriteVersion": 0,
        "systemPromptSnapshot": "",
        "systemFingerprint": "",
        "systemPromptHash": "",
        "toolSchemaHash": "",
        "lastShapeRewriteVersion": 0,
        "consecutiveCompactions": 0,
        "compactStuck": false
    })
}

fn text_content(content: &Value) -> String {
    if let Value::String(s) = content {
        return s.trim().to_string();
    }
    let Some(arr) = content.as_array() else { return String::new(); };
    arr.iter()
        .filter_map(|part| {
            if part.get("type").and_then(Value::as_str) == Some("text") {
                Some(part.get("text").and_then(Value::as_str).unwrap_or("").trim().to_string())
            } else {
                None
            }
        })
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn append_slim_turn(memory: &mut Value, user_prompt: &str) {
    let prompt = user_prompt.trim();
    if !prompt.is_empty() {
        let turns = memory.get_mut("turns").and_then(Value::as_array_mut).unwrap();
        turns.push(serde_json::json!({ "userPrompts": [prompt], "conclusion": "" }));
    }
}

pub fn set_latest_conclusion(memory: &mut Value, content: &Value) {
    let conclusion = text_content(content);
    if conclusion.is_empty() {
        return;
    }
    let turns = memory.get_mut("turns").and_then(Value::as_array_mut).unwrap();
    let needs_new = turns.last().map(|t| t.get("conclusion").and_then(Value::as_str).unwrap_or("").is_empty()).unwrap_or(true);
    if needs_new {
        turns.push(serde_json::json!({ "userPrompts": [], "conclusion": "" }));
    }
    if let Some(last) = turns.last_mut() {
        last["conclusion"] = Value::String(conclusion);
    }
}

pub fn normalize_slim_memory(memory: &mut Value) {
    let mut normalized: Vec<Value> = Vec::new();
    let mut pending_prompts: Vec<String> = Vec::new();
    let turns = memory.get("turns").and_then(Value::as_array).cloned().unwrap_or_default();
    for raw in &turns {
        let prompts: Vec<String> = raw.get("userPrompts").and_then(Value::as_array).map(|arr| {
            arr.iter().filter_map(|v| v.as_str().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())).collect()
        }).unwrap_or_else(|| {
            raw.get("userPrompt").and_then(Value::as_str).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).map(|s| vec![s]).unwrap_or_default()
        });
        pending_prompts.extend(prompts);
        let conclusion = raw.get("conclusion").and_then(Value::as_str).unwrap_or("").trim().to_string();
        if !conclusion.is_empty() {
            normalized.push(serde_json::json!({ "userPrompts": pending_prompts, "conclusion": conclusion }));
            pending_prompts = Vec::new();
        }
    }
    if !pending_prompts.is_empty() {
        normalized.push(serde_json::json!({ "userPrompts": pending_prompts, "conclusion": "" }));
    }
    let legacy_summary = memory.get("summary").and_then(Value::as_str).unwrap_or("").trim().to_string();
    let digests: Vec<String> = memory.get("digests").and_then(Value::as_array).map(|arr| {
        arr.iter().filter_map(|v| v.as_str().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())).collect()
    }).unwrap_or_else(|| if legacy_summary.is_empty() { Vec::new() } else { vec![legacy_summary] });
    let preserved: Vec<String> = memory.get("preservedUserPrompts").and_then(Value::as_array).map(|arr| {
        arr.iter().filter_map(|v| v.as_str().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())).collect()
    }).unwrap_or_default();
    memory["digests"] = Value::Array(digests.into_iter().map(Value::String).collect());
    memory["preservedUserPrompts"] = Value::Array(preserved.into_iter().map(Value::String).collect());
    memory["turns"] = Value::Array(normalized);
    if let Some(obj) = memory.as_object_mut() {
        obj.remove("summary");
    }
}

pub fn memory_without_current(memory: &Value, pending_messages: bool) -> Value {
    let mut clone = serde_json::json!({
        "digests": memory.get("digests").cloned().unwrap_or(Value::Array(vec![])),
        "preservedUserPrompts": memory.get("preservedUserPrompts").cloned().unwrap_or(Value::Array(vec![])),
        "turns": memory.get("turns").cloned().unwrap_or(Value::Array(vec![]))
    });
    normalize_slim_memory(&mut clone);
    let turns = clone.get_mut("turns").and_then(Value::as_array_mut).unwrap();
    if let Some(last_idx) = turns.len().checked_sub(1) {
        let conclusion_empty = turns[last_idx].get("conclusion").and_then(Value::as_str).unwrap_or("").is_empty();
        if conclusion_empty {
            if pending_messages {
                turns.pop();
            } else if let Some(prompts) = turns[last_idx].get_mut("userPrompts").and_then(Value::as_array_mut) {
                prompts.pop();
            }
        }
    }
    if let Some(last_idx) = turns.len().checked_sub(1) {
        let no_prompts = turns[last_idx].get("userPrompts").and_then(Value::as_array).map(|a| a.is_empty()).unwrap_or(true);
        let no_conclusion = turns[last_idx].get("conclusion").and_then(Value::as_str).unwrap_or("").is_empty();
        if no_prompts && no_conclusion {
            turns.pop();
        }
    }
    clone
}

pub fn format_slim_memory(memory: &Value) -> String {
    let mut normalized = memory.clone();
    normalize_slim_memory(&mut normalized);
    let mut sections: Vec<String> = vec![
        "请使用下面的只追加会话记录继续工作。不要要求用户重复之前的要求。".into(),
        "".into(),
        "## Conversation".into(),
    ];
    let preserved: Vec<String> = normalized.get("preservedUserPrompts").and_then(Value::as_array).map(|arr| {
        arr.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()
    }).unwrap_or_default();
    if !preserved.is_empty() {
        sections.push("".into());
        sections.push("### Preserved user requests".into());
        for p in &preserved {
            sections.push(format!("User:\n{p}"));
        }
    }
    let digests: Vec<String> = normalized.get("digests").and_then(Value::as_array).map(|arr| {
        arr.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()
    }).unwrap_or_default();
    if !digests.is_empty() {
        sections.push("".into());
        sections.push("### Frozen digests".into());
        for (i, d) in digests.iter().enumerate() {
            sections.push(format!("Digest {}:\n{}", i + 1, d));
        }
    }
    let turns = normalized.get("turns").and_then(Value::as_array).cloned().unwrap_or_default();
    for turn in turns {
        let prompts: Vec<String> = turn.get("userPrompts").and_then(Value::as_array).map(|arr| {
            arr.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()
        }).unwrap_or_default();
        for p in prompts {
            sections.push("".into());
            sections.push(format!("User:\n{p}"));
        }
        let conclusion = turn.get("conclusion").and_then(Value::as_str).unwrap_or("");
        if !conclusion.is_empty() {
            sections.push(format!("Assistant:\n{conclusion}"));
        }
    }
    sections.join("\n")
}

pub fn estimate_context_tokens(text: &str) -> usize {
    let mut ascii = 0usize;
    let mut non_ascii = 0usize;
    for c in text.chars() {
        if (c as u32) <= 0x7f {
            ascii += 1;
        } else {
            non_ascii += 1;
        }
    }
    (ascii / 4) + non_ascii + if ascii % 4 != 0 { 1 } else { 0 }
}

pub fn context_tokens_from_messages(messages: &Value) -> u64 {
    let mut tokens = 0u64;
    let Some(arr) = messages.as_array() else { return 0; };
    for message in arr {
        if message.get("role").and_then(Value::as_str) != Some("assistant") || message.get("usage").is_none() {
            continue;
        }
        let usage = &message["usage"];
        let total = usage.get("totalTokens").or_else(|| usage.get("total_tokens")).and_then(Value::as_u64).unwrap_or(0);
        let output = usage.get("output").or_else(|| usage.get("outputTokens")).or_else(|| usage.get("output_tokens")).and_then(Value::as_u64).unwrap_or(0);
        let input = usage.get("input").and_then(Value::as_u64).unwrap_or(0);
        let cached = usage.get("cacheRead").and_then(Value::as_u64).unwrap_or(0) + usage.get("cacheWrite").and_then(Value::as_u64).unwrap_or(0);
        let measured = if total > output { total - output } else if input >= cached { input } else { input + cached };
        tokens = tokens.max(measured);
    }
    tokens
}

pub fn context_pressure_tier(current_tokens: u64, context_window: u64) -> &'static str {
    if !(current_tokens > 0) || !(context_window > 0) {
        return "normal";
    }
    let ratio = current_tokens as f64 / context_window as f64;
    if ratio >= 0.9 { "force" }
    else if ratio >= 0.8 { "elide" }
    else if ratio >= 0.6 { "snip" }
    else if ratio >= 0.5 { "warn" }
    else { "normal" }
}

pub fn should_use_full_context(memory: &Value, max_context_tokens: u64, max_context_chars: u64) -> bool {
    if memory.get("pendingMessages").and_then(Value::as_array).is_some_and(|a| !a.is_empty()) {
        return true;
    }
    if memory.get("contextStage").and_then(Value::as_str) == Some("slim") {
        return false;
    }
    let full_len = memory.get("fullMessages").and_then(Value::as_array).is_some_and(|a| !a.is_empty());
    if !full_len {
        return memory.get("turns").and_then(Value::as_array).is_some_and(|a| a.is_empty());
    }
    let measured = memory.get("contextTokens").and_then(Value::as_u64).unwrap_or(0);
    if measured > 0 {
        measured < max_context_tokens
    } else {
        let serialized = serde_json::to_string(memory.get("fullMessages").unwrap_or(&Value::Null)).unwrap_or_default();
        serialized.len() < max_context_chars as usize
    }
}

pub fn strip_completed_openai_reasoning(messages: &Value) -> Value {
    let Some(arr) = messages.as_array() else { return messages.clone(); };
    let mut out: Vec<Value> = Vec::with_capacity(arr.len());
    for message in arr {
        if message.get("role").and_then(Value::as_str) != Some("assistant") || !message.get("content").and_then(Value::as_array).is_some() {
            out.push(message.clone());
            continue;
        }
        let mut changed = false;
        let mut content: Vec<Value> = Vec::new();
        for block in message["content"].as_array().unwrap() {
            if block.get("type").and_then(Value::as_str) == Some("thinking") {
                changed = true;
                continue;
            }
            if block.get("type").and_then(Value::as_str) == Some("toolCall") {
                if let Some(id) = block.get("id").and_then(Value::as_str) {
                    if id.contains('|') {
                        changed = true;
                        let mut nb = block.clone();
                        let new_id = id.split('|').next().unwrap_or("").to_string();
                        nb["id"] = Value::String(new_id);
                        content.push(nb);
                        continue;
                    }
                }
            }
            content.push(block.clone());
        }
        if changed {
            let mut nm = message.clone();
            nm["content"] = Value::Array(content);
            out.push(nm);
        } else {
            out.push(message.clone());
        }
    }
    Value::Array(out)
}

pub fn compact_native_tool_results(messages: &Value, tier: &str, preserve_recent: usize) -> (Value, bool) {
    if !matches!(tier, "snip" | "elide" | "force") {
        return (messages.clone(), false);
    }
    let arr = match messages.as_array() {
        Some(a) => a,
        None => return (messages.clone(), false),
    };
    let cutoff = arr.len().saturating_sub(preserve_recent);
    let mut changed = false;
    let mut next: Vec<Value> = Vec::with_capacity(arr.len());
    for (i, message) in arr.iter().enumerate() {
        if i >= cutoff || message.get("role").and_then(Value::as_str) != Some("toolResult") || !message.get("content").and_then(Value::as_array).is_some() {
            next.push(message.clone());
            continue;
        }
        let mut content_changed = false;
        let mut content: Vec<Value> = Vec::new();
        for part in message["content"].as_array().unwrap() {
            if part.get("type").and_then(Value::as_str) != Some("text") {
                content.push(part.clone());
                continue;
            }
            let text = part.get("text").and_then(Value::as_str).unwrap_or("");
            let tool_call_id = message.get("toolCallId").and_then(Value::as_str).unwrap_or("");
            let compacted = compact_tool_text(text, tier, tool_call_id);
            if compacted == text {
                content.push(part.clone());
            } else {
                content_changed = true;
                let mut np = part.clone();
                np["text"] = Value::String(compacted);
                content.push(np);
            }
        }
        if content_changed {
            changed = true;
            let mut nm = message.clone();
            nm["content"] = Value::Array(content);
            next.push(nm);
        } else {
            next.push(message.clone());
        }
    }
    (Value::Array(next), changed)
}

fn compact_tool_text(text: &str, tier: &str, tool_call_id: &str) -> String {
    if text.contains("[elided tool result") {
        return text.to_string();
    }
    if tier == "snip" && text.len() <= 8 * 1024 {
        return text.to_string();
    }
    let bytes = text.len();
    let id = if tool_call_id.is_empty() { String::new() } else { format!(" {}", tool_call_id) };
    if tier == "elide" || tier == "force" {
        return format!("[elided tool result{id} — {bytes} bytes; re-run the tool if needed]");
    }
    let head: String = text.chars().take(3_000).collect();
    let tail: String = text.chars().rev().take(2_000).collect::<Vec<_>>().into_iter().rev().collect();
    format!("{head}\n\n…[snipped older tool result{id} — {bytes} bytes]…\n\n{tail}")
}

pub fn seed_slim_memory_from_messages(memory: &mut Value, messages: &Value) {
    if let Some(arr) = messages.as_array() {
        for message in arr {
            if message.get("role").and_then(Value::as_str) == Some("user") {
                append_slim_turn(memory, &text_content(&message["content"]));
            } else if message.get("role").and_then(Value::as_str) == Some("assistant")
                && message.get("stopReason").and_then(Value::as_str) != Some("error")
            {
                set_latest_conclusion(memory, &message["content"]);
            }
        }
    }
    memory["fullMessages"] = messages.clone();
    memory["contextTokens"] = Value::from(context_tokens_from_messages(messages));
    memory["contextStage"] = Value::String("full".into());
    normalize_slim_memory(memory);
}

/// Compact older turns into a frozen digest. `summarize` produces the digest text.
/// Returns true if compaction happened.
pub async fn compact_slim_memory<F, Fut>(
    memory: &mut Value,
    summarize: F,
    max_turns: usize,
    max_chars: usize,
    current_tokens: u64,
    max_tokens: u64,
) -> bool
where
    F: Fn(String) -> Fut,
    Fut: std::future::Future<Output = Option<String>>,
{
    normalize_slim_memory(memory);
    let turns = memory.get("turns").and_then(Value::as_array).cloned().unwrap_or_default();
    let earlier = serde_json::json!({
        "digests": [],
        "preservedUserPrompts": [],
        "turns": turns.clone()
    });
    let formatted = format_slim_memory(&earlier);
    let within_turn_limit = turns.len() <= max_turns;
    let below_char_limit = max_chars == usize::MAX || formatted.len() < max_chars;
    let below_token_limit = max_tokens == u64::MAX || current_tokens < max_tokens;
    if within_turn_limit && below_char_limit && below_token_limit {
        memory["consecutiveCompactions"] = Value::from(0u64);
        memory["compactStuck"] = Value::Bool(false);
        return false;
    }
    let compact_stuck = memory.get("compactStuck").and_then(Value::as_bool).unwrap_or(false);
    if compact_stuck || turns.len() < 2 {
        return false;
    }
    let consecutive = memory.get("consecutiveCompactions").and_then(Value::as_u64).unwrap_or(0);
    if consecutive >= 2 {
        memory["compactStuck"] = Value::Bool(true);
        return false;
    }
    let protected_count = if turns.last().and_then(|t| t.get("conclusion").and_then(Value::as_str)).is_some_and(|c| !c.is_empty()) {
        1
    } else {
        turns.len().min(2)
    };
    let split = turns.len().saturating_sub(protected_count);
    if split == 0 {
        return false;
    }
    let compacted_turns: Vec<Value> = turns[..split].to_vec();
    let preserved_existing: Vec<String> = memory.get("preservedUserPrompts").and_then(Value::as_array).map(|a| {
        a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()
    }).unwrap_or_default();
    let preserved_prompts: Vec<String> = compacted_turns.iter().flat_map(|t| {
        t.get("userPrompts").and_then(Value::as_array).map(|arr| {
            arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string()))
                .filter(|s| s.chars().count() <= 2_000 && !preserved_existing.contains(s))
                .collect::<Vec<_>>()
        }).unwrap_or_default()
    }).collect();
    let earlier_only = serde_json::json!({
        "digests": [],
        "preservedUserPrompts": [],
        "turns": compacted_turns.iter().map(|t| serde_json::json!({ "userPrompts": t["userPrompts"], "conclusion": t["conclusion"] })).collect::<Vec<_>>()
    });
    let digest = summarize(format_slim_memory(&earlier_only)).await;
    let digest = digest.map(|d| d.trim().to_string()).filter(|d| !d.is_empty());
    let Some(digest) = digest else { return false; };
    let mut preserved = memory.get("preservedUserPrompts").and_then(Value::as_array).cloned().unwrap_or_default();
    preserved.extend(preserved_prompts.into_iter().map(Value::String));
    memory["preservedUserPrompts"] = Value::Array(preserved);
    let mut digests = memory.get("digests").and_then(Value::as_array).cloned().unwrap_or_default();
    digests.push(Value::String(digest));
    memory["digests"] = Value::Array(digests);
    let remaining: Vec<Value> = turns[split..].to_vec();
    memory["turns"] = Value::Array(remaining);
    let rv = memory.get("rewriteVersion").and_then(Value::as_u64).unwrap_or(0) + 1;
    memory["rewriteVersion"] = Value::from(rv);
    let cc = memory.get("consecutiveCompactions").and_then(Value::as_u64).unwrap_or(0) + 1;
    memory["consecutiveCompactions"] = Value::from(cc);
    true
}

#[allow(dead_code)]
fn _unused_btreemap() -> BTreeMap<String, Value> {
    let _: Map<String, Value> = Map::new();
    BTreeMap::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pressure_tiers() {
        assert_eq!(context_pressure_tier(0, 128_000), "normal");
        assert_eq!(context_pressure_tier(64_000, 128_000), "warn");   // 0.5
        assert_eq!(context_pressure_tier(76_800, 128_000), "snip");   // 0.6
        assert_eq!(context_pressure_tier(102_400, 128_000), "elide"); // 0.8
        assert_eq!(context_pressure_tier(115_200, 128_000), "force"); // 0.9
    }

    #[test]
    fn estimate_tokens_ascii_and_cjk() {
        assert_eq!(estimate_context_tokens("abcd"), 1); // 4 ascii -> 1 token
        assert_eq!(estimate_context_tokens("ab"), 1);   // 2 ascii -> 1 (ceil)
        assert_eq!(estimate_context_tokens("中"), 1);   // 1 cjk -> 1
    }

    #[test]
    fn format_slim_memory_basic() {
        let mut m = create_slim_memory();
        append_slim_turn(&mut m, "hello");
        set_latest_conclusion(&mut m, &Value::String("hi there".into()));
        let text = format_slim_memory(&m);
        assert!(text.contains("## Conversation"));
        assert!(text.contains("User:\nhello"));
        assert!(text.contains("Assistant:\nhi there"));
    }

    #[test]
    fn memory_without_current_drops_latest_prompt() {
        let mut m = create_slim_memory();
        append_slim_turn(&mut m, "first");
        set_latest_conclusion(&mut m, &Value::String("ans1".into()));
        append_slim_turn(&mut m, "second");
        let without = memory_without_current(&m, false);
        let turns = without.get("turns").and_then(Value::as_array).unwrap();
        // "second" had no conclusion and its prompt is dropped -> only first turn remains.
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].get("conclusion").and_then(Value::as_str), Some("ans1"));
    }
}
