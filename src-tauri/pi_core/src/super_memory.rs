//! Super-context slim memory, ported from `scripts/alkaid-context-super-memory.mjs`.
//!
//! This is Vega's *legacy* context mode (selected via `NOVA_CONTEXT_MODE=super`),
//! superseded by Reasonix (`slim_memory.rs`). It differs in shape and policy:
//! a single rolling `summary` string (not frozen digests + preserved prompts),
//! a Chinese prompt format, and a default 10-turn retention that replaces the
//! whole summary on compaction. Kept for parity with the node bridge's super
//! mode; Reasonix is the default and canonical path.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Default recent-turn retention before compaction (`VEGA_SLIM_MEMORY_TURNS`).
pub const VEGA_SLIM_MEMORY_TURNS: usize = 10;

/// One conversation turn (same shape as Reasonix).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SuperTurn {
    pub user_prompts: Vec<String>,
    pub conclusion: String,
}

/// The super-context memory state, persisted per session as `<id>.slim.json`
/// (version 1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SuperMemory {
    pub summary: String,
    pub turns: Vec<SuperTurn>,
    pub pending_messages: Vec<Value>,
    pub full_messages: Vec<Value>,
    pub context_tokens: u64,
    pub context_stage: String,
}

impl Default for SuperMemory {
    fn default() -> Self {
        SuperMemory::new()
    }
}

impl SuperMemory {
    /// Port of `createSlimMemory()`.
    pub fn new() -> Self {
        SuperMemory {
            summary: String::new(),
            turns: Vec::new(),
            pending_messages: Vec::new(),
            full_messages: Vec::new(),
            context_tokens: 0,
            context_stage: "full".to_string(),
        }
    }

    /// Port of `appendSlimTurn`.
    pub fn append_turn(&mut self, user_prompt: &str) {
        let prompt = user_prompt.trim();
        if !prompt.is_empty() {
            self.turns.push(SuperTurn {
                user_prompts: vec![prompt.to_string()],
                conclusion: String::new(),
            });
        }
    }

    /// Port of `setLatestConclusion`.
    pub fn set_latest_conclusion(&mut self, content: &Value) {
        let conclusion = text_content(content);
        if conclusion.is_empty() {
            return;
        }
        let needs_new = match self.turns.last() {
            None => true,
            Some(last) => !last.conclusion.is_empty(),
        };
        if needs_new {
            self.turns.push(SuperTurn {
                user_prompts: Vec::new(),
                conclusion: String::new(),
            });
        }
        if let Some(last) = self.turns.last_mut() {
            last.conclusion = conclusion;
        }
    }

    /// Normalize in place (typed counterpart of JS `normalizeSlimMemory`).
    pub fn normalize_in_place(&mut self) {
        self.summary = self.summary.trim().to_string();
        self.turns = merge_turns(std::mem::take(&mut self.turns));
    }

    /// Port of `formatSlimMemory`.
    pub fn format(&self) -> String {
        format_super_memory(&self.summary, &self.turns)
    }
}

/// Port of JS `textContent`.
pub fn text_content(content: &Value) -> String {
    match content {
        Value::String(s) => s.trim().to_string(),
        Value::Array(parts) => parts
            .iter()
            .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .map(|text| text.trim().to_string())
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn merge_turns(turns: Vec<SuperTurn>) -> Vec<SuperTurn> {
    let mut normalized: Vec<SuperTurn> = Vec::new();
    let mut pending: Vec<String> = Vec::new();
    for turn in turns {
        let prompts: Vec<String> = turn
            .user_prompts
            .iter()
            .map(|prompt| prompt.trim().to_string())
            .filter(|prompt| !prompt.is_empty())
            .collect();
        pending.extend(prompts);
        let conclusion = turn.conclusion.trim().to_string();
        if !conclusion.is_empty() {
            normalized.push(SuperTurn {
                user_prompts: std::mem::take(&mut pending),
                conclusion,
            });
        }
    }
    if !pending.is_empty() {
        normalized.push(SuperTurn {
            user_prompts: pending,
            conclusion: String::new(),
        });
    }
    normalized
}

/// Port of `formatSlimMemory` (Chinese section layout).
pub fn format_super_memory(summary: &str, turns: &[SuperTurn]) -> String {
    let normalized_turns = merge_turns(turns.to_vec());
    let summary = summary.trim();
    let mut sections: Vec<String> = Vec::new();
    if !summary.is_empty() {
        sections.push("## 更早轮次摘要".to_string());
        sections.push(summary.to_string());
    }
    if !normalized_turns.is_empty() {
        sections.push("## 最近轮次".to_string());
    }
    for (index, turn) in normalized_turns.iter().enumerate() {
        sections.push(format!("### 轮次 {}", index + 1));
        for prompt in &turn.user_prompts {
            sections.push(format!("用户提示：{prompt}"));
        }
        if !turn.conclusion.is_empty() {
            sections.push(format!("结论：{}", turn.conclusion));
        }
    }
    sections.join("\n")
}

/// Port of super's `messageWithSlimMemory`: prefix the prompt with the
/// formatted record (excluding the current turn).
pub fn message_with_super_memory(text: &str, memory: &SuperMemory) -> String {
    let pending = !memory.pending_messages.is_empty();
    let context = format_super_memory(
        &memory_without_current(memory, pending).summary,
        &memory_without_current(memory, pending).turns,
    );
    format!("{context}\n\nUser:\n{text}")
}

/// The normalized view excluding the current turn (port of
/// `memoryWithoutCurrent`).
pub fn memory_without_current(memory: &SuperMemory, pending_messages: bool) -> SuperMemory {
    let mut normalized = SuperMemory {
        summary: memory.summary.clone(),
        turns: memory.turns.clone(),
        pending_messages: Vec::new(),
        full_messages: Vec::new(),
        context_tokens: 0,
        context_stage: "full".to_string(),
    };
    normalized.normalize_in_place();
    // Mirror JS: capture `latest` before any pop.
    let mut popped: Option<SuperTurn> = None;
    if let Some(last) = normalized.turns.last() {
        if last.conclusion.is_empty() && pending_messages {
            popped = Some(last.clone());
        }
    }
    if pending_messages && popped.is_some() {
        normalized.turns.pop();
    } else if let Some(last) = normalized.turns.last_mut() {
        if last.conclusion.is_empty() {
            last.user_prompts.pop();
        }
    }
    let latest_ref: Option<&SuperTurn> = if pending_messages {
        popped.as_ref()
    } else {
        normalized.turns.last()
    };
    if let Some(latest) = latest_ref {
        if latest.user_prompts.is_empty() && latest.conclusion.is_empty() {
            normalized.turns.pop();
        }
    }
    normalized
}

/// Port of `estimateContextTokens`.
pub fn estimate_context_tokens(text: &str) -> u64 {
    let mut ascii: u64 = 0;
    let mut non_ascii: u64 = 0;
    for ch in text.chars() {
        if (ch as u32) <= 0x7f {
            ascii += 1;
        } else {
            non_ascii += 1;
        }
    }
    (ascii + 3) / 4 + non_ascii
}

/// Port of `shouldUseFullContext`.
pub fn should_use_full_context(
    memory: &SuperMemory,
    max_context_tokens: u64,
    max_context_chars: Option<usize>,
) -> bool {
    if !memory.pending_messages.is_empty() {
        return true;
    }
    if memory.context_stage == "slim" {
        return false;
    }
    if memory.full_messages.is_empty() {
        return memory.turns.is_empty();
    }
    let measured = memory.context_tokens;
    if measured > 0 {
        measured < max_context_tokens
    } else {
        let serialized = serde_json::to_string(&memory.full_messages).unwrap_or_default();
        match max_context_chars {
            Some(max) => serialized.len() < max,
            None => true,
        }
    }
}

/// Port of `seedSlimMemoryFromMessages`.
pub fn seed_super_memory_from_messages(memory: &mut SuperMemory, messages: &[Value]) {
    for message in messages {
        match message.get("role").and_then(Value::as_str) {
            Some("user") => {
                let text = text_content(&message.get("content").cloned().unwrap_or(Value::Null));
                memory.append_turn(&text);
            }
            Some("assistant") => {
                if message.get("stopReason").and_then(Value::as_str) != Some("error") {
                    let content = message.get("content").cloned().unwrap_or(Value::Null);
                    memory.set_latest_conclusion(&content);
                }
            }
            _ => {}
        }
    }
    memory.full_messages = messages.to_vec();
    memory.context_tokens = context_tokens_from_messages(messages);
    memory.context_stage = "full".to_string();
    memory.normalize_in_place();
}

fn num_or_zero(value: Option<&Value>) -> u64 {
    match value {
        Some(Value::Number(n)) => n.as_u64().unwrap_or_else(|| {
            n.as_f64()
                .map(|f| if f.is_nan() || f < 0.0 { 0.0 } else { f })
                .unwrap_or(0.0) as u64
        }),
        _ => 0,
    }
}

/// Port of `contextTokensFromMessages`.
pub fn context_tokens_from_messages(messages: &[Value]) -> u64 {
    let mut tokens: u64 = 0;
    for message in messages {
        if message.get("role").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let usage = match message.get("usage").and_then(Value::as_object) {
            Some(usage) => usage,
            None => continue,
        };
        let total = num_or_zero(usage.get("totalTokens").or_else(|| usage.get("total_tokens")));
        let output = num_or_zero(
            usage
                .get("output")
                .or_else(|| usage.get("outputTokens"))
                .or_else(|| usage.get("output_tokens")),
        );
        let input = num_or_zero(usage.get("input"));
        let cached = num_or_zero(usage.get("cacheRead")) + num_or_zero(usage.get("cacheWrite"));
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

/// Options for [`compact_super_memory`]; `None` limits mean "no limit".
#[derive(Debug, Clone, Copy)]
pub struct SuperCompactOptions {
    pub max_turns: Option<usize>,
    pub max_chars: Option<usize>,
    pub current_tokens: u64,
    pub max_tokens: Option<u64>,
}

/// A planned super compaction awaiting an LLM summary.
#[derive(Debug, Clone)]
pub struct SuperCompactionPlan {
    /// The formatted earlier-turns text (including the prior summary) to send
    /// to the summary model.
    pub earlier_text: String,
    split: usize,
}

/// The decision half of `compactSlimMemory` (super variant). Returns the text to
/// summarize when a compaction is warranted, else `None`.
pub fn plan_super_compaction(
    memory: &mut SuperMemory,
    options: SuperCompactOptions,
) -> Option<SuperCompactionPlan> {
    memory.normalize_in_place();
    let formatted = format_super_memory(&memory.summary, &memory.turns);
    let max_turns = options.max_turns.unwrap_or(VEGA_SLIM_MEMORY_TURNS);
    let within_turn_limit = memory.turns.len() <= max_turns;
    let below_character_limit = match options.max_chars {
        Some(max) => formatted.len() < max,
        None => true,
    };
    let below_token_limit = match options.max_tokens {
        Some(max) => options.current_tokens < max,
        None => true,
    };
    if within_turn_limit && below_character_limit && below_token_limit {
        return None;
    }
    let protected_count = if memory
        .turns
        .last()
        .map(|turn| !turn.conclusion.is_empty())
        .unwrap_or(false)
    {
        1
    } else {
        memory.turns.len().min(2)
    };
    let split = memory.turns.len() - protected_count;
    if split == 0 {
        return None;
    }
    let earlier_turns: Vec<SuperTurn> = memory.turns[..split].to_vec();
    Some(SuperCompactionPlan {
        earlier_text: format_super_memory(&memory.summary, &earlier_turns),
        split,
    })
}

/// Apply a summary to a planned super compaction (replaces the rolling summary).
pub fn apply_super_compaction(
    memory: &mut SuperMemory,
    plan: &SuperCompactionPlan,
    summary: &str,
) -> bool {
    let summary = summary.trim().to_string();
    if summary.is_empty() {
        return false;
    }
    memory.summary = summary;
    memory.turns = memory.turns.split_off(plan.split);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn memory_with_turns(n: usize) -> SuperMemory {
        let mut m = SuperMemory::new();
        for i in 1..=n {
            m.turns.push(SuperTurn {
                user_prompts: vec![format!("prompt {i}")],
                conclusion: format!("conclusion {i}"),
            });
        }
        m
    }

    #[test]
    fn format_uses_chinese_layout() {
        let mut m = memory_with_turns(2);
        m.summary = "earlier".to_string();
        let formatted = m.format();
        assert!(formatted.contains("## 更早轮次摘要"));
        assert!(formatted.contains("earlier"));
        assert!(formatted.contains("## 最近轮次"));
        assert!(formatted.contains("### 轮次 1"));
        assert!(formatted.contains("用户提示：prompt 1"));
        assert!(formatted.contains("结论：conclusion 1"));
    }

    #[test]
    fn plan_apply_replaces_summary() {
        let mut m = memory_with_turns(12);
        let opts = SuperCompactOptions {
            max_turns: Some(10),
            max_chars: None,
            current_tokens: 0,
            max_tokens: None,
        };
        let plan = plan_super_compaction(&mut m, opts).expect("should plan");
        assert!(plan.earlier_text.contains("prompt 1"));
        assert!(apply_super_compaction(&mut m, &plan, "NEW SUMMARY"));
        assert_eq!(m.summary, "NEW SUMMARY");
        // Protected: latest conclusion turn kept; 12 - 1 = 11 turns compacted.
        assert_eq!(m.turns.len(), 1);
        assert_eq!(m.turns[0].conclusion, "conclusion 12");
    }

    #[test]
    fn no_plan_within_turn_limit() {
        let mut m = memory_with_turns(5);
        let opts = SuperCompactOptions {
            max_turns: None,
            max_chars: None,
            current_tokens: 0,
            max_tokens: None,
        };
        assert!(plan_super_compaction(&mut m, opts).is_none());
    }

    #[test]
    fn memory_without_current_drops_current_prompt() {
        let mut m = memory_with_turns(1);
        m.append_turn("current");
        let without = memory_without_current(&m, false);
        // The dangling "current" prompt (no conclusion) is removed.
        assert!(without.format().contains("conclusion 1"));
        assert!(!without.format().contains("用户提示：current"));
    }

    #[test]
    fn seeds_from_messages() {
        let messages = vec![
            json!({ "role": "user", "content": [{ "type": "text", "text": "q1" }] }),
            json!({ "role": "assistant", "content": [{ "type": "text", "text": "a1" }], "stopReason": "end_turn", "usage": { "totalTokens": 100, "output": 20 } }),
        ];
        let mut m = SuperMemory::new();
        seed_super_memory_from_messages(&mut m, &messages);
        assert_eq!(m.turns.len(), 1);
        assert_eq!(m.turns[0].user_prompts, vec!["q1".to_string()]);
        assert_eq!(m.turns[0].conclusion, "a1");
        assert_eq!(m.context_tokens, 80);
    }
}
