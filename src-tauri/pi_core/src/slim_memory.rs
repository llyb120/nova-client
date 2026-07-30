//! Reasonix slim-memory context management, ported from
//! `scripts/alkaid-slim-memory.mjs`.
//!
//! This is Vega's canonical session model: an append-only conversation log
//! (`turns`), frozen summaries of old turns (`digests`), preserved short user
//! prompts, and a native pi-message trajectory (`fullMessages` /
//! `pendingMessages`) used while the context still fits the window. Every
//! function here is deterministic and golden-tested except the `summarize`
//! callback passed to [`compact_slim_memory`], which is the single LLM boundary
//! (the caller supplies it; tests use a fixed stub digest).
//!
//! Parity boundaries (documented, matching the project's honest-limits policy):
//! - `compact_tool_text` head/tail slicing and the `JSON.stringify(...).length`
//!   capacity check in [`should_use_full_context`] use JS UTF-16 code units;
//!   this port uses Unicode scalars / UTF-8 bytes, which is exact for ASCII and
//!   BMP text and differs only at astral-emoji slice boundaries (a cosmetic
//!   difference in a compaction heuristic).

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

/// One conversation turn: the verbatim user prompts that led to a conclusion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Turn {
    pub user_prompts: Vec<String>,
    pub conclusion: String,
}

/// The normalized, prompt-facing view of a memory (the shape JS
/// `normalizeSlimMemory` / `memoryWithoutCurrent` return).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NormalizedMemory {
    pub digests: Vec<String>,
    pub preserved_user_prompts: Vec<String>,
    pub turns: Vec<Turn>,
}

/// The full Reasonix slim-memory state, persisted per session as
/// `{session}.slim.json`. Field names match the JS object (camelCase).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SlimMemory {
    pub digests: Vec<String>,
    pub preserved_user_prompts: Vec<String>,
    pub turns: Vec<Turn>,
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

impl Default for SlimMemory {
    fn default() -> Self {
        SlimMemory::new()
    }
}

impl SlimMemory {
    /// Port of `createSlimMemory()`.
    pub fn new() -> Self {
        SlimMemory {
            digests: Vec::new(),
            preserved_user_prompts: Vec::new(),
            turns: Vec::new(),
            pending_messages: Vec::new(),
            full_messages: Vec::new(),
            context_tokens: 0,
            context_stage: "full".to_string(),
            context_tier: "normal".to_string(),
            rewrite_version: 0,
            system_prompt_snapshot: String::new(),
            system_fingerprint: String::new(),
            system_prompt_hash: String::new(),
            tool_schema_hash: String::new(),
            last_shape_rewrite_version: 0,
            consecutive_compactions: 0,
            compact_stuck: false,
        }
    }

    /// Port of `appendSlimTurn`.
    pub fn append_turn(&mut self, user_prompt: &str) {
        let prompt = user_prompt.trim();
        if !prompt.is_empty() {
            self.turns.push(Turn {
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
            self.turns.push(Turn {
                user_prompts: Vec::new(),
                conclusion: String::new(),
            });
        }
        if let Some(last) = self.turns.last_mut() {
            last.conclusion = conclusion;
        }
    }

    /// Normalize this memory's `digests`/`preserved_user_prompts`/`turns` in
    /// place (the typed-struct counterpart of JS `normalizeSlimMemory`, which
    /// also strips the legacy `summary`/singular `userPrompt` shapes — those
    /// cannot occur in a typed struct).
    pub fn normalize_in_place(&mut self) {
        self.digests = trim_filter(&self.digests);
        self.preserved_user_prompts = trim_filter(&self.preserved_user_prompts);
        self.turns = merge_turns(std::mem::take(&mut self.turns));
    }

    /// The normalized view of this memory.
    pub fn normalized(&self) -> NormalizedMemory {
        NormalizedMemory {
            digests: trim_filter(&self.digests),
            preserved_user_prompts: trim_filter(&self.preserved_user_prompts),
            turns: merge_turns(self.turns.clone()),
        }
    }

    /// Port of `formatSlimMemory` applied to this memory.
    pub fn format(&self) -> String {
        format_normalized(&self.normalized())
    }
}

/// Port of JS `textContent`: string → trimmed; array → trimmed non-empty text
/// parts joined by newlines.
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

fn trim_filter(values: &[String]) -> Vec<String> {
    values
        .iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect()
}

/// The core of `normalizeSlimMemory`: fold dangling user prompts forward into
/// the next completed conclusion; a trailing promptless, conclusionless turn is
/// kept only if it still has prompts.
fn merge_turns(turns: Vec<Turn>) -> Vec<Turn> {
    let mut normalized: Vec<Turn> = Vec::new();
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
            normalized.push(Turn {
                user_prompts: std::mem::take(&mut pending),
                conclusion,
            });
        }
    }
    if !pending.is_empty() {
        normalized.push(Turn {
            user_prompts: pending,
            conclusion: String::new(),
        });
    }
    normalized
}

fn string_from_value(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Port of `normalizeSlimMemory` over a raw JSON value, including the legacy
/// `summary` field and the singular `userPrompt` turn shape.
pub fn normalize_value(input: &Value) -> NormalizedMemory {
    let digests = match input.get("digests") {
        Some(Value::Array(items)) => items
            .iter()
            .map(string_from_value)
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .collect(),
        _ => {
            let legacy = input
                .get("summary")
                .map(string_from_value)
                .unwrap_or_default()
                .trim()
                .to_string();
            if legacy.is_empty() {
                Vec::new()
            } else {
                vec![legacy]
            }
        }
    };
    let preserved_user_prompts = match input.get("preservedUserPrompts") {
        Some(Value::Array(items)) => items
            .iter()
            .map(string_from_value)
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .collect(),
        _ => Vec::new(),
    };
    let raw_turns: Vec<Turn> = input
        .get("turns")
        .and_then(Value::as_array)
        .map(|turns| {
            turns
                .iter()
                .map(|raw| {
                    let user_prompts = match raw.get("userPrompts") {
                        Some(Value::Array(items)) => items
                            .iter()
                            .map(string_from_value)
                            .map(|value| value.trim().to_string())
                            .filter(|value| !value.is_empty())
                            .collect(),
                        _ => {
                            let singular = raw
                                .get("userPrompt")
                                .map(string_from_value)
                                .unwrap_or_default()
                                .trim()
                                .to_string();
                            if singular.is_empty() {
                                Vec::new()
                            } else {
                                vec![singular]
                            }
                        }
                    };
                    let conclusion = raw
                        .get("conclusion")
                        .map(string_from_value)
                        .unwrap_or_default()
                        .trim()
                        .to_string();
                    Turn {
                        user_prompts,
                        conclusion,
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    NormalizedMemory {
        digests,
        preserved_user_prompts,
        turns: merge_turns(raw_turns),
    }
}

/// Port of `formatSlimMemory`.
pub fn format_normalized(memory: &NormalizedMemory) -> String {
    let mut sections: Vec<String> = vec![
        "请使用下面的只追加会话记录继续工作。不要要求用户重复之前的要求。".to_string(),
        String::new(),
        "## Conversation".to_string(),
    ];
    if !memory.preserved_user_prompts.is_empty() {
        sections.push(String::new());
        sections.push("### Preserved user requests".to_string());
        for prompt in &memory.preserved_user_prompts {
            sections.push(format!("User:\n{prompt}"));
        }
    }
    if !memory.digests.is_empty() {
        sections.push(String::new());
        sections.push("### Frozen digests".to_string());
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

/// Port of `formatSlimMemory(memory)` over a raw value.
pub fn format_slim_memory(memory: &Value) -> String {
    format_normalized(&normalize_value(memory))
}

/// Port of `memoryWithoutCurrent`.
pub fn memory_without_current(memory: &Value, pending_messages: bool) -> NormalizedMemory {
    let subset = json!({
        "digests": memory.get("digests").cloned().unwrap_or_else(|| json!([])),
        "preservedUserPrompts": memory.get("preservedUserPrompts").cloned().unwrap_or_else(|| json!([])),
        "turns": memory.get("turns").cloned().unwrap_or_else(|| json!([])),
    });
    let mut normalized = normalize_value(&subset);
    // Mirror JS: `latest` is a reference captured before any pop.
    let mut popped: Option<Turn> = None;
    if let Some(last) = normalized.turns.last() {
        if last.conclusion.is_empty() {
            if pending_messages {
                popped = Some(last.clone());
            }
        }
    }
    if pending_messages && popped.is_some() {
        normalized.turns.pop();
    } else if let Some(last) = normalized.turns.last_mut() {
        if last.conclusion.is_empty() {
            last.user_prompts.pop();
        }
    }
    let latest_ref: Option<&Turn> = if pending_messages {
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

/// Port of `contextPressureTier`.
pub fn context_pressure_tier(current_tokens: f64, context_window: f64) -> &'static str {
    if !(current_tokens > 0.0) || !(context_window > 0.0) {
        return "normal";
    }
    let ratio = current_tokens / context_window;
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

fn num_or_zero(value: Option<&Value>) -> u64 {
    match value {
        Some(Value::Number(n)) => n
            .as_u64()
            .or_else(|| n.as_i64().and_then(|i| u64::try_from(i).ok()))
            .unwrap_or_else(|| {
                n.as_f64()
                    .map(|f| if f.is_nan() || f < 0.0 { 0.0 } else { f })
                    .unwrap_or(0.0) as u64
            }),
        _ => 0,
    }
}

/// Return the first present (non-null) value among `keys`.
fn first_present<'a>(obj: &'a Map<String, Value>, keys: &[&str]) -> Option<&'a Value> {
    keys.iter()
        .find_map(|key| obj.get(*key))
        .filter(|value| !value.is_null())
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
        let total = num_or_zero(first_present(usage, &["totalTokens", "total_tokens"]));
        let output = num_or_zero(first_present(usage, &["output", "outputTokens", "output_tokens"]));
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

/// Port of `stripCompletedOpenAIReasoning`.
pub fn strip_completed_openai_reasoning(messages: &[Value]) -> Vec<Value> {
    messages
        .iter()
        .map(|message| {
            if message.get("role").and_then(Value::as_str) != Some("assistant") {
                return message.clone();
            }
            let content = match message.get("content").and_then(Value::as_array) {
                Some(content) => content,
                None => return message.clone(),
            };
            let mut changed = false;
            let mut new_content: Vec<Value> = Vec::new();
            for block in content {
                let block_type = block.get("type").and_then(Value::as_str);
                if block_type == Some("thinking") {
                    changed = true;
                    continue;
                }
                if block_type == Some("toolCall") {
                    if let Some(id) = block.get("id").and_then(Value::as_str) {
                        if id.contains('|') {
                            changed = true;
                            let mut new_block = block.clone();
                            let prefix = id.split('|').next().unwrap_or("").to_string();
                            new_block["id"] = Value::String(prefix);
                            new_content.push(new_block);
                            continue;
                        }
                    }
                }
                new_content.push(block.clone());
            }
            if changed {
                let mut new_message = message.clone();
                new_message["content"] = Value::Array(new_content);
                new_message
            } else {
                message.clone()
            }
        })
        .collect()
}

fn compact_tool_text(text: &str, tier: &str, tool_call_id: Option<&str>) -> String {
    if text.contains("[elided tool result") {
        return text.to_string();
    }
    let bytes = text.len();
    if tier == "snip" && bytes <= 8 * 1024 {
        return text.to_string();
    }
    let id = tool_call_id.map(|id| format!(" {id}")).unwrap_or_default();
    if tier == "elide" || tier == "force" {
        return format!("[elided tool result{id} — {bytes} bytes; re-run the tool if needed]");
    }
    let head: String = text.chars().take(3_000).collect();
    let tail: String = text.chars().rev().take(2_000).collect::<Vec<_>>().into_iter().rev().collect();
    format!("{head}\n\n…[snipped older tool result{id} — {bytes} bytes]…\n\n{tail}")
}

/// Port of `compactNativeToolResults`. Returns `(messages, changed)`.
pub fn compact_native_tool_results(
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
            if index >= cutoff
                || message.get("role").and_then(Value::as_str) != Some("toolResult")
            {
                return message.clone();
            }
            let content = match message.get("content").and_then(Value::as_array) {
                Some(content) => content,
                None => return message.clone(),
            };
            let tool_call_id = message
                .get("toolCallId")
                .and_then(Value::as_str)
                .map(|id| id.to_string());
            let mut content_changed = false;
            let new_content: Vec<Value> = content
                .iter()
                .map(|part| {
                    if part.get("type").and_then(Value::as_str) != Some("text") {
                        return part.clone();
                    }
                    let text = part.get("text").and_then(Value::as_str).unwrap_or("");
                    let compacted = compact_tool_text(text, tier, tool_call_id.as_deref());
                    if compacted == text {
                        return part.clone();
                    }
                    content_changed = true;
                    let mut new_part = part.clone();
                    new_part["text"] = Value::String(compacted);
                    new_part
                })
                .collect();
            if !content_changed {
                return message.clone();
            }
            changed = true;
            let mut new_message = message.clone();
            new_message["content"] = Value::Array(new_content);
            new_message
        })
        .collect();
    if changed {
        (next, true)
    } else {
        (messages.to_vec(), false)
    }
}

/// Port of `shouldUseFullContext`.
pub fn should_use_full_context(
    memory: &SlimMemory,
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

/// Port of `rebaseNativeContextForSlimMemory`. Returns `(messages, changed)`.
pub fn rebase_native_context_for_slim_memory(
    messages: &[Value],
    active_turn_start: i64,
    memory: &Value,
) -> (Vec<Value>, bool) {
    let start = active_turn_start;
    if start <= 0
        || start as usize >= messages.len()
        || messages[start as usize].get("role").and_then(Value::as_str) != Some("user")
    {
        return (messages.to_vec(), false);
    }
    let start = start as usize;
    let compact_context = format_normalized(&memory_without_current(memory, true));
    let mut current = messages[start].clone();
    match current.get("content").cloned() {
        Some(Value::String(text)) => {
            current["content"] = Value::String(format!("{compact_context}\n\nUser:\n{text}"));
        }
        Some(Value::Array(mut parts)) => {
            let first_text = parts
                .iter()
                .position(|part| part.get("type").and_then(Value::as_str) == Some("text"));
            if let Some(index) = first_text {
                let old_text = parts[index]
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                parts[index]["text"] = Value::String(format!("{compact_context}\n\nUser:\n{old_text}"));
                current["content"] = Value::Array(parts);
            } else {
                let mut new_parts = vec![json!({ "type": "text", "text": compact_context })];
                new_parts.extend(parts);
                current["content"] = Value::Array(new_parts);
            }
        }
        _ => {
            current["content"] = Value::Array(vec![json!({ "type": "text", "text": compact_context })]);
        }
    }
    let mut result = vec![current];
    result.extend_from_slice(&messages[start + 1..]);
    (result, true)
}

/// Port of `seedSlimMemoryFromMessages`.
pub fn seed_slim_memory_from_messages(memory: &mut SlimMemory, messages: &[Value]) {
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

/// Options for [`compact_slim_memory`]; `None` limits mean "no limit"
/// (JS `Number.POSITIVE_INFINITY`).
#[derive(Debug, Clone, Copy)]
pub struct CompactOptions {
    pub max_turns: Option<usize>,
    pub max_chars: Option<usize>,
    pub current_tokens: u64,
    pub max_tokens: Option<u64>,
}

/// Port of `compactSlimMemory`. The `summarize` callback is the LLM boundary:
/// it receives the formatted earlier-turns text and returns a digest. Returns
/// whether a compaction happened.
pub fn compact_slim_memory<F>(memory: &mut SlimMemory, options: CompactOptions, mut summarize: F) -> bool
where
    F: FnMut(&str) -> String,
{
    memory.normalize_in_place();
    // JS computes the character limit from a format of only {digests, turns}.
    let formatted = format_slim_memory(&json!({
        "digests": memory.digests,
        "turns": memory.turns,
    }));
    let within_turn_limit = match options.max_turns {
        Some(max) => memory.turns.len() <= max,
        None => true,
    };
    let below_character_limit = match options.max_chars {
        Some(max) => formatted.len() < max,
        None => true,
    };
    let below_token_limit = match options.max_tokens {
        Some(max) => options.current_tokens < max,
        None => true,
    };
    if within_turn_limit && below_character_limit && below_token_limit {
        memory.consecutive_compactions = 0;
        memory.compact_stuck = false;
        return false;
    }
    if memory.compact_stuck || memory.turns.len() < 2 {
        return false;
    }
    if memory.consecutive_compactions >= 2 {
        memory.compact_stuck = true;
        return false;
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
        return false;
    }
    let compacted_turns: Vec<Turn> = memory.turns[..split].to_vec();
    let mut preserved_prompts: Vec<String> = Vec::new();
    for turn in &compacted_turns {
        for prompt in &turn.user_prompts {
            if prompt.chars().count() <= 2_000 && !memory.preserved_user_prompts.contains(prompt) {
                preserved_prompts.push(prompt.clone());
            }
        }
    }
    let earlier = NormalizedMemory {
        digests: Vec::new(),
        preserved_user_prompts: Vec::new(),
        turns: compacted_turns.clone(),
    };
    let digest = summarize(&format_normalized(&earlier)).trim().to_string();
    if digest.is_empty() {
        return false;
    }
    memory.preserved_user_prompts.extend(preserved_prompts);
    memory.digests.push(digest);
    memory.turns = memory.turns.split_off(split);
    memory.rewrite_version += 1;
    memory.consecutive_compactions += 1;
    true
}

/// A planned compaction awaiting an LLM digest. Produced by
/// [`plan_compaction`], consumed by [`apply_compaction`].
#[derive(Debug, Clone)]
pub struct CompactionPlan {
    /// The formatted earlier-turns text to send to the summary model.
    pub earlier_text: String,
    preserved_prompts: Vec<String>,
    split: usize,
}

/// The decision half of [`compact_slim_memory`], split out so the caller can
/// run the async LLM `summarize` between planning and applying. Normalizes the
/// memory and returns `Some(plan)` when a compaction is warranted (carrying the
/// text to summarize), or `None` when no compaction is needed/possible (having
/// already reset the stuck/consecutive counters where the JS does).
pub fn plan_compaction(memory: &mut SlimMemory, options: CompactOptions) -> Option<CompactionPlan> {
    memory.normalize_in_place();
    let formatted = format_slim_memory(&json!({
        "digests": memory.digests,
        "turns": memory.turns,
    }));
    let within_turn_limit = match options.max_turns {
        Some(max) => memory.turns.len() <= max,
        None => true,
    };
    let below_character_limit = match options.max_chars {
        Some(max) => formatted.len() < max,
        None => true,
    };
    let below_token_limit = match options.max_tokens {
        Some(max) => options.current_tokens < max,
        None => true,
    };
    if within_turn_limit && below_character_limit && below_token_limit {
        memory.consecutive_compactions = 0;
        memory.compact_stuck = false;
        return None;
    }
    if memory.compact_stuck || memory.turns.len() < 2 {
        return None;
    }
    if memory.consecutive_compactions >= 2 {
        memory.compact_stuck = true;
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
    let compacted_turns: Vec<Turn> = memory.turns[..split].to_vec();
    let mut preserved_prompts: Vec<String> = Vec::new();
    for turn in &compacted_turns {
        for prompt in &turn.user_prompts {
            if prompt.chars().count() <= 2_000 && !memory.preserved_user_prompts.contains(prompt) {
                preserved_prompts.push(prompt.clone());
            }
        }
    }
    let earlier = NormalizedMemory {
        digests: Vec::new(),
        preserved_user_prompts: Vec::new(),
        turns: compacted_turns,
    };
    Some(CompactionPlan {
        earlier_text: format_normalized(&earlier),
        preserved_prompts,
        split,
    })
}

/// Apply a digest to a planned compaction (the mutation half of
/// [`compact_slim_memory`]). Returns whether the compaction was applied (false
/// if the digest was empty, matching the JS).
pub fn apply_compaction(memory: &mut SlimMemory, plan: &CompactionPlan, digest: &str) -> bool {
    let digest = digest.trim().to_string();
    if digest.is_empty() {
        return false;
    }
    memory.preserved_user_prompts.extend(plan.preserved_prompts.clone());
    memory.digests.push(digest);
    memory.turns = memory.turns.split_off(plan.split);
    memory.rewrite_version += 1;
    memory.consecutive_compactions += 1;
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory_with_turns(n: usize) -> SlimMemory {
        let mut m = SlimMemory::new();
        for i in 1..=n {
            m.turns.push(Turn {
                user_prompts: vec![format!("prompt {i}")],
                conclusion: format!("conclusion {i}"),
            });
        }
        m
    }

    #[test]
    fn plan_apply_matches_compact_slim_memory() {
        let opts = CompactOptions {
            max_turns: Some(3),
            max_chars: None,
            current_tokens: 0,
            max_tokens: None,
        };
        // Reference: the monolithic function with a stub digest.
        let mut reference = memory_with_turns(5);
        let ref_compacted = compact_slim_memory(&mut reference, opts, |_| "STUB".to_string());

        // Split: plan, then apply with the same stub digest.
        let mut split = memory_with_turns(5);
        let plan = plan_compaction(&mut split, opts);
        let split_compacted = match &plan {
            Some(plan) => apply_compaction(&mut split, plan, "STUB"),
            None => false,
        };

        assert_eq!(ref_compacted, split_compacted);
        assert_eq!(reference, split);
        // The plan's earlier_text is non-empty and contains the folded turns.
        let plan = plan.unwrap();
        assert!(plan.earlier_text.contains("prompt 1"));
        assert!(plan.earlier_text.contains("conclusion 1"));
    }

    #[test]
    fn plan_returns_none_when_within_limits() {
        let mut m = memory_with_turns(2);
        let opts = CompactOptions {
            max_turns: Some(10),
            max_chars: None,
            current_tokens: 0,
            max_tokens: None,
        };
        assert!(plan_compaction(&mut m, opts).is_none());
        // Counters reset like the JS.
        assert_eq!(m.consecutive_compactions, 0);
        assert!(!m.compact_stuck);
    }

    #[test]
    fn apply_with_empty_digest_is_noop() {
        let mut m = memory_with_turns(5);
        let opts = CompactOptions {
            max_turns: Some(3),
            max_chars: None,
            current_tokens: 0,
            max_tokens: None,
        };
        let plan = plan_compaction(&mut m, opts).unwrap();
        let before = m.clone();
        assert!(!apply_compaction(&mut m, &plan, "   "));
        assert_eq!(m, before);
    }
}
