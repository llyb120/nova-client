//! 会话 rollout：`sessions/<id>.jsonl` 只追加日志（Codex rollout 的等价物），每条消息入史即落盘，
//! 恢复时逐行重放。首次打开旧会话时从 `.slim.json` / `.pending.json` / `<id>.json` 一次性迁移，
//! 旧文件只读不回写。

use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};

pub struct Rollout {
    file: std::fs::File,
    path: PathBuf,
}

pub fn rollout_path(root: &Path, session_id: &str) -> PathBuf {
    root.join(format!("{session_id}.jsonl"))
}

impl Rollout {
    /// 打开（或创建）会话日志并返回已记录的消息。`resume` 为 false 表示全新会话，不读任何旧文件。
    pub fn open(
        root: &Path,
        session_id: &str,
        resume: bool,
        restore_at: Option<&str>,
    ) -> Result<(Self, Vec<Value>), String> {
        std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
        let path = rollout_path(root, session_id);
        let existing = resume.then(|| std::fs::read_to_string(&path).ok()).flatten();
        let (messages, migrated) = match existing {
            Some(text) => (replay(&text), false),
            None if resume => (migrate_legacy(root, session_id, restore_at), true),
            None => (Vec::new(), true),
        };
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| format!("打开会话日志 {} 失败：{e}", path.display()))?;
        let mut rollout = Self { file, path };
        if migrated {
            rollout.write_line(&json!({ "type": "session_meta", "version": 1, "sessionId": session_id }));
            for message in &messages {
                rollout.append(message);
            }
        }
        Ok((rollout, messages))
    }

    pub fn append(&mut self, message: &Value) {
        self.write_line(&json!({ "type": "message", "message": message }));
    }

    // ponytail: 同步追加写，单条消息通常很小；若大图/大输出拖慢热路径再改后台写者。
    fn write_line(&mut self, value: &Value) {
        let mut line = value.to_string();
        line.push('\n');
        if let Err(error) = self.file.write_all(line.as_bytes()) {
            eprintln!("lyra: 写会话日志 {} 失败：{error}", self.path.display());
        }
    }
}

/// 逐行重放；强杀留下的半行直接跳过。
fn replay(text: &str) -> Vec<Value> {
    text.lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|line| line.get("type").and_then(Value::as_str) == Some("message"))
        .filter_map(|mut line| line.get_mut("message").map(Value::take))
        .collect()
}

fn read_json(path: PathBuf) -> Option<Value> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
}

fn non_empty_array(value: Option<&Value>) -> Option<Vec<Value>> {
    value
        .and_then(Value::as_array)
        .filter(|items| !items.is_empty())
        .cloned()
}

pub fn truncate_at_restore(messages: Vec<Value>, restore_at: Option<&str>) -> Vec<Value> {
    let Some(restore_at) = restore_at.filter(|r| !r.is_empty()) else {
        return messages;
    };
    match messages
        .iter()
        .position(|m| m.get("timestamp").map(|t| t.to_string()).as_deref() == Some(restore_at))
    {
        Some(index) => messages[..=index].to_vec(),
        None => messages,
    }
}

/// 旧格式优先级与旧 bridge 一致：中断轨迹 > 完整原生历史 > slim 记忆 > `<id>.json`。
fn migrate_legacy(root: &Path, session_id: &str, restore_at: Option<&str>) -> Vec<Value> {
    let slim = read_json(root.join(format!("{session_id}.slim.json"))).unwrap_or(Value::Null);
    if let Some(pending) = non_empty_array(slim.get("pendingMessages")).or_else(|| {
        read_json(root.join(format!("{session_id}.pending.json")))
            .and_then(|v| non_empty_array(Some(&v)))
    }) {
        return pending;
    }
    if let Some(full) = non_empty_array(slim.get("fullMessages")) {
        return full;
    }
    if let Some(summary) = slim_summary(&slim) {
        return vec![json!({
            "role": "user",
            "content": [{ "type": "text", "text": format!("<compaction-summary>\n{summary}\n</compaction-summary>") }],
            "timestamp": crate::lyra::history::now_ms(),
        })];
    }
    truncate_at_restore(
        read_json(root.join(format!("{session_id}.json")))
            .and_then(|v| non_empty_array(Some(&v)))
            .unwrap_or_default(),
        restore_at,
    )
}

/// slim 阶段只剩用户原文、冻结摘要和结论：拼成一条摘要消息。
fn slim_summary(slim: &Value) -> Option<String> {
    let strings = |key: &str| -> Vec<String> {
        slim.get(key)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect()
    };
    let mut digests = strings("digests");
    if let Some(summary) = slim.get("summary").and_then(Value::as_str).filter(|s| !s.trim().is_empty()) {
        digests.push(summary.trim().to_string());
    }
    let mut sections = Vec::new();
    for prompt in strings("preservedUserPrompts") {
        sections.push(format!("User:\n{prompt}"));
    }
    for (i, digest) in digests.iter().enumerate() {
        sections.push(format!("Digest {}:\n{digest}", i + 1));
    }
    for turn in slim.get("turns").and_then(Value::as_array).into_iter().flatten() {
        for prompt in turn.get("userPrompts").and_then(Value::as_array).into_iter().flatten() {
            if let Some(prompt) = prompt.as_str().map(str::trim).filter(|p| !p.is_empty()) {
                sections.push(format!("User:\n{prompt}"));
            }
        }
        if let Some(conclusion) = turn.get("conclusion").and_then(Value::as_str).map(str::trim).filter(|c| !c.is_empty()) {
            sections.push(format!("Assistant:\n{conclusion}"));
        }
    }
    (!sections.is_empty()).then(|| {
        format!("Earlier conversation (migrated):\n\n{}", sections.join("\n\n"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("lyra-rollout-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        root
    }

    #[test]
    fn appends_and_replays_skipping_torn_line() {
        let root = temp_root("replay");
        let (mut rollout, messages) = Rollout::open(&root, "s", false, None).unwrap();
        assert!(messages.is_empty());
        rollout.append(&json!({ "role": "user", "content": "a" }));
        rollout.append(&json!({ "role": "assistant", "content": [] }));
        drop(rollout);
        let path = rollout_path(&root, "s");
        let mut file = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(b"{\"type\":\"message\",\"mess").unwrap();
        let (_, messages) = Rollout::open(&root, "s", true, None).unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["content"], "a");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn migrates_slim_once_without_touching_old_files() {
        let root = temp_root("migrate");
        std::fs::create_dir_all(&root).unwrap();
        let slim = json!({ "version": 3, "digests": ["d1"],
            "turns": [{ "userPrompts": ["hi"], "conclusion": "done" }] });
        std::fs::write(root.join("s.slim.json"), slim.to_string()).unwrap();
        let (_, messages) = Rollout::open(&root, "s", true, None).unwrap();
        assert_eq!(messages.len(), 1);
        let text = messages[0]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("Digest 1:\nd1") && text.contains("Assistant:\ndone"));
        // 第二次打开走 jsonl 重放，不再迁移，也不重复写入。
        let (_, again) = Rollout::open(&root, "s", true, None).unwrap();
        assert_eq!(again, messages);
        assert!(root.join("s.slim.json").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn migration_prefers_pending_then_legacy_with_restore() {
        let root = temp_root("pending");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("p.pending.json"), json!([{ "role": "user", "content": "x" }]).to_string()).unwrap();
        assert_eq!(Rollout::open(&root, "p", true, None).unwrap().1.len(), 1);
        let legacy = json!([
            { "role": "user", "content": "a", "timestamp": 1 },
            { "role": "assistant", "content": [], "timestamp": 2 },
            { "role": "user", "content": "b", "timestamp": 3 }
        ]);
        std::fs::write(root.join("l.json"), legacy.to_string()).unwrap();
        assert_eq!(Rollout::open(&root, "l", true, Some("2")).unwrap().1.len(), 2);
        let _ = std::fs::remove_dir_all(&root);
    }
}
