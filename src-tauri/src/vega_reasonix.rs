//! Reasonix session orchestration helpers for the native Vega path.
//!
//! The deterministic context-management primitives live in
//! `pi_core::slim_memory` (golden-tested). This module adds the session IO and
//! the small glue the bridge performs around a turn: the per-session
//! `.slim.json` persistence (`{data_dir}/alkaid/sessions/<id>.slim.json`),
//! `stableHash` fingerprints, the pending-prompt checkpoint, and the
//! slim-memory prompt prefix. The async per-turn decision flow (capacity
//! tiering, full/slim switching, compaction, conclusion persistence) is wired
//! in `sdk_runtime::run_prompt_native` on top of these helpers.
//!
//! Session ids are validated against the same pattern the node bridge enforces
//! (`/^[A-Za-z0-9_-]+$/`) to keep the on-disk layout interchangeable.
#![allow(dead_code)]

use pi_core::{
    format_normalized, memory_without_current, SlimMemory,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

/// Validate a Vega session id (port of the bridge's `sessionPath` guard).
pub fn is_valid_session_id(session_id: &str) -> bool {
    !session_id.is_empty()
        && session_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn sessions_root(data_dir: &Path) -> PathBuf {
    data_dir.join("alkaid").join("sessions")
}

/// `{data_dir}/alkaid/sessions/<id>.slim.json`.
pub fn slim_memory_path(data_dir: &Path, session_id: &str) -> Result<PathBuf, String> {
    if !is_valid_session_id(session_id) {
        return Err("非法 Vega session id".to_string());
    }
    Ok(sessions_root(data_dir).join(format!("{session_id}.slim.json")))
}

/// `{data_dir}/alkaid/sessions/<id>.json` (legacy native-message transcript).
pub fn legacy_messages_path(data_dir: &Path, session_id: &str) -> Result<PathBuf, String> {
    if !is_valid_session_id(session_id) {
        return Err("非法 Vega session id".to_string());
    }
    Ok(sessions_root(data_dir).join(format!("{session_id}.json")))
}

/// Port of `stableHash`: sha256 hex of the JSON (or string), first 16 chars.
pub fn stable_hash(value: &Value) -> String {
    let bytes = match value {
        Value::String(s) => s.as_bytes().to_vec(),
        other => other.to_string().into_bytes(),
    };
    let digest = Sha256::digest(&bytes);
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    hex[..16].to_string()
}

/// Port of `messagesWithPendingAlkaidPrompt`: append the pending user turn.
pub fn messages_with_pending_prompt(
    messages: &[Value],
    text: &str,
    images: &[Value],
    timestamp: u64,
) -> Vec<Value> {
    let mut content: Vec<Value> = Vec::new();
    if !text.is_empty() {
        content.push(json!({ "type": "text", "text": text }));
    }
    content.extend_from_slice(images);
    let mut result = messages.to_vec();
    result.push(json!({ "role": "user", "content": content, "timestamp": timestamp }));
    result
}

/// Port of `messageWithSlimMemory`: prefix the prompt with the formatted
/// append-only record (excluding the current turn).
pub fn message_with_slim_memory(text: &str, memory: &SlimMemory) -> String {
    let memory_value = serde_json::to_value(memory).unwrap_or(Value::Null);
    let pending = !memory.pending_messages.is_empty();
    let context = format_normalized(&memory_without_current(&memory_value, pending));
    format!("{context}\n\nUser:\n{text}")
}

/// Load the per-session slim memory; absent/corrupt files yield a fresh memory.
pub fn load_slim_memory(data_dir: &Path, session_id: &str) -> SlimMemory {
    let Ok(path) = slim_memory_path(data_dir, session_id) else {
        return SlimMemory::new();
    };
    let Ok(text) = fs::read_to_string(&path) else {
        return SlimMemory::new();
    };
    let Ok(parsed) = serde_json::from_str::<Value>(&text) else {
        return SlimMemory::new();
    };
    // Stored files carry an extra `version` key (ignored by serde) and the full
    // memory shape. Require a `turns` array, mirroring the bridge's guard.
    if parsed.get("turns").and_then(Value::as_array).is_some() {
        serde_json::from_value::<SlimMemory>(parsed).unwrap_or_else(|_| SlimMemory::new())
    } else {
        SlimMemory::new()
    }
}

/// Load a legacy native-message transcript (`<id>.json`), or an empty list.
pub fn load_legacy_messages(data_dir: &Path, session_id: &str) -> Vec<Value> {
    let Ok(path) = legacy_messages_path(data_dir, session_id) else {
        return Vec::new();
    };
    let Ok(text) = fs::read_to_string(&path) else {
        return Vec::new();
    };
    serde_json::from_str::<Vec<Value>>(&text).unwrap_or_default()
}

/// Atomically persist the slim memory as `{version:3, ...memory}`.
pub fn save_slim_memory(data_dir: &Path, session_id: &str, memory: &SlimMemory) -> Result<(), String> {
    let path = slim_memory_path(data_dir, session_id)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建会话目录失败：{e}"))?;
    }
    let mut stored = serde_json::to_value(memory).map_err(|e| e.to_string())?;
    if let Some(obj) = stored.as_object_mut() {
        obj.insert("version".to_string(), json!(3));
    }
    let body = serde_json::to_string(&stored).map_err(|e| e.to_string())?;
    let temp = path.with_extension("json.tmp");
    fs::write(&temp, &body).map_err(|e| format!("写入会话失败：{e}"))?;
    fs::rename(&temp, &path).map_err(|e| format!("保存会话失败：{e}"))?;
    Ok(())
}

/// Persist a native-message transcript to the legacy `<id>.json` path.
pub fn save_legacy_messages(data_dir: &Path, session_id: &str, messages: &[Value]) -> Result<(), String> {
    let path = legacy_messages_path(data_dir, session_id)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建会话目录失败：{e}"))?;
    }
    let body = serde_json::to_string(messages).map_err(|e| e.to_string())?;
    let temp = path.with_extension("json.tmp");
    fs::write(&temp, &body).map_err(|e| format!("写入会话失败：{e}"))?;
    fs::rename(&temp, &path).map_err(|e| format!("保存会话失败：{e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_session_ids() {
        assert!(is_valid_session_id("abc-123_XYZ"));
        assert!(!is_valid_session_id(""));
        assert!(!is_valid_session_id("bad/id"));
        assert!(!is_valid_session_id("bad id"));
        assert!(!is_valid_session_id("../escape"));
    }

    #[test]
    fn stable_hash_is_16_hex_chars_and_deterministic() {
        let value = json!({ "cwd": "/tmp/x", "mode": "agent" });
        let hash = stable_hash(&value);
        assert_eq!(hash.len(), 16);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(hash, stable_hash(&value));
        assert_ne!(hash, stable_hash(&json!({ "cwd": "/tmp/x", "mode": "plan" })));
    }

    #[test]
    fn builds_pending_checkpoint() {
        let prior = vec![json!({ "role": "user", "content": [] })];
        let images = vec![json!({ "type": "image", "data": "x", "mimeType": "image/png" })];
        let result = messages_with_pending_prompt(&prior, "hello", &images, 1234);
        assert_eq!(result.len(), 2);
        let last = &result[1];
        assert_eq!(last["role"], "user");
        assert_eq!(last["timestamp"], 1234);
        assert_eq!(last["content"][0]["text"], "hello");
        assert_eq!(last["content"][1]["type"], "image");
    }

    #[test]
    fn message_with_slim_memory_prefixes_record() {
        let mut memory = SlimMemory::new();
        memory.append_turn("earlier question");
        memory.set_latest_conclusion(&json!("earlier answer"));
        memory.append_turn("current question");
        let wrapped = message_with_slim_memory("current question", &memory);
        assert!(wrapped.contains("## Conversation"));
        assert!(wrapped.contains("earlier question"));
        assert!(wrapped.contains("earlier answer"));
        // The current (conclusionless) turn is excluded from the record...
        assert!(!wrapped.contains("current question\nAssistant"));
        // ...and re-appended as the live user turn.
        assert!(wrapped.ends_with("User:\ncurrent question"));
    }

    #[test]
    fn round_trips_slim_memory_through_disk() {
        let dir = std::env::temp_dir().join(format!(
            "vega-reasonix-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        fs::create_dir_all(&dir).unwrap();
        let mut memory = SlimMemory::new();
        memory.append_turn("q");
        memory.set_latest_conclusion(&json!("a"));
        memory.rewrite_version = 5;
        save_slim_memory(&dir, "sess-1", &memory).unwrap();
        let loaded = load_slim_memory(&dir, "sess-1");
        assert_eq!(loaded, memory);
        // A fresh session loads an empty memory.
        assert_eq!(load_slim_memory(&dir, "missing"), SlimMemory::new());
        let _ = fs::remove_dir_all(&dir);
    }
}
