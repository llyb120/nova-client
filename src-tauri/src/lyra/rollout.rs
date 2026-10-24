//! 会话 rollout：`sessions/<id>.jsonl` 只追加日志（Codex rollout 的等价物），每条消息入史即落盘，
//! 恢复时逐行重放。首次打开旧会话时从 `.slim.json` / `.pending.json` / `<id>.json` 一次性迁移，
//! 旧文件只读不回写。

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::Mutex;
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
        let existing = if resume {
            match std::fs::read(&path) {
                Ok(bytes) => Some(String::from_utf8_lossy(&bytes).into_owned()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => return Err(format!("读取会话日志 {} 失败：{e}", path.display())),
            }
        } else { None };
        let torn_tail = existing.as_ref().is_some_and(|text| !text.is_empty() && !text.ends_with('\n'));
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
        // 隔开崩溃留下的半行，否则恢复后的第一条新消息也会被拼进坏行而丢失。
        if torn_tail { rollout.file.write_all(b"\n").map_err(|e| e.to_string())?; }
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

/// 普通子 agent 的安全元数据；不序列化 Session、模型配置、凭据、消息或任务队列。
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct AgentRecord {
    pub task_name: String,
    pub session_id: String,
    pub status: String,
    pub read_only: bool,
    pub tools: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize)]
struct AgentManifest {
    version: u8,
    owner: String,
    agents: Vec<AgentRecord>,
}

pub(super) fn valid_task_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 64
        && name.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
}

pub(super) struct AgentStore {
    pub root: PathBuf,
    manifest: Mutex<AgentManifest>,
}

impl AgentStore {
    pub fn directory(sessions_root: &Path, owner: &str) -> PathBuf {
        sessions_root.join("agents").join(format!("{:x}", Sha256::digest(owner.as_bytes())))
    }

    pub fn open(root: PathBuf, owner: &str) -> Result<Self, String> {
        std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
        let path = root.join("manifest.json");
        let empty = || AgentManifest { version: 1, owner: owner.into(), agents: Vec::new() };
        let parsed = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<AgentManifest>(&bytes).map_err(|e| e.to_string())
                .and_then(|m| if m.version == 1 && m.owner == owner { Ok(m) } else { Err("owner/version mismatch".into()) }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(empty()),
            Err(e) => return Err(e.to_string()),
        };
        // 坏清单不能让整个会话打不开：留档改名后以空清单继续（子 agent 记录丢失，但主会话可用）。
        let mut manifest = parsed.unwrap_or_else(|error| {
            let bad = root.join(format!("manifest.json.bad.{}", super::history::now_ms()));
            eprintln!("lyra: warn: agent manifest {} 无效（{error}），已移至 {} 并以空清单继续", path.display(), bad.display());
            let _ = std::fs::rename(&path, &bad);
            empty()
        });
        let mut names = std::collections::HashSet::new();
        let mut ids = std::collections::HashSet::new();
        let count = manifest.agents.len();
        let mut interrupted = false;
        manifest.agents.retain(|agent| valid_task_name(&agent.task_name) && names.insert(agent.task_name.clone())
            && uuid::Uuid::parse_str(&agent.session_id).ok().map(|id| id.to_string()).as_deref() == Some(agent.session_id.as_str())
            && ids.insert(agent.session_id.clone())
            && matches!(agent.status.as_str(), "pending_init" | "running" | "completed" | "errored" | "interrupted" | "shutdown"));
        if manifest.agents.len() != count { eprintln!("lyra: warn: agent manifest {} 跳过 {} 条非法条目", path.display(), count - manifest.agents.len()); }
        for agent in &mut manifest.agents {
            if matches!(agent.status.as_str(), "pending_init" | "running") {
                agent.status = "interrupted".into();
                interrupted = true;
            }
        }
        let changed = interrupted || manifest.agents.len() != count;
        let store = Self { root, manifest: Mutex::new(manifest) };
        if changed { store.write(&store.manifest.lock().unwrap())?; }
        Ok(store)
    }

    pub fn records(&self) -> Vec<AgentRecord> {
        self.manifest.lock().unwrap().agents.clone()
    }

    pub fn insert(&self, record: AgentRecord) -> Result<(), String> {
        let mut manifest = self.manifest.lock().unwrap();
        let mut next = manifest.clone();
        next.agents.retain(|a| a.task_name != record.task_name);
        next.agents.push(record);
        self.write(&next)?;
        *manifest = next;
        Ok(())
    }

    pub fn status(&self, session_id: &str, status: &str) -> Result<(), String> {
        let mut manifest = self.manifest.lock().unwrap();
        let mut next = manifest.clone();
        // 同名任务关闭后可新建；旧 worker 退出不得覆盖新实例的状态。
        let Some(agent) = next.agents.iter_mut().find(|a| a.session_id == session_id) else { return Ok(()) };
        agent.status = status.into();
        self.write(&next)?;
        *manifest = next;
        Ok(())
    }

    // ponytail: 每次状态变化原子重写小 manifest；任务累积很多时改为分记录存储。
    fn write(&self, manifest: &AgentManifest) -> Result<(), String> {
        let path = self.root.join("manifest.json");
        let tmp = self.root.join("manifest.json.tmp");
        let bytes = serde_json::to_vec(manifest).map_err(|e| e.to_string())?;
        let mut file = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
        file.write_all(&bytes).and_then(|_| file.sync_all()).map_err(|e| e.to_string())?;
        drop(file);
        std::fs::rename(&tmp, &path).map_err(|e| format!("write agent manifest: {e}"))
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
        let (mut resumed, messages) = Rollout::open(&root, "s", true, None).unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["content"], "a");
        resumed.append(&json!({ "role": "user", "content": "after restart" }));
        drop(resumed);
        assert_eq!(Rollout::open(&root, "s", true, None).unwrap().1.len(), 3);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn agent_manifest_is_owner_bound_and_rejects_unsafe_entries() {
        let dir = tempfile::tempdir().unwrap();
        let root = AgentStore::directory(dir.path(), "owner");
        let store = AgentStore::open(root.clone(), "owner").unwrap();
        let mut record = AgentRecord { task_name: "safe".into(), session_id: uuid::Uuid::new_v4().to_string(),
            status: "pending_init".into(), read_only: true, tools: vec!["read".into()] };
        store.insert(record.clone()).unwrap();
        drop(store);
        let reopened = AgentStore::open(root.clone(), "owner").unwrap();
        assert_eq!(reopened.records()[0].status, "interrupted");
        assert!(reopened.records()[0].read_only);
        drop(reopened);
        let good = record.clone();
        // 非法条目被跳过，合法条目保留。
        for (name, id) in [("../escape", record.session_id.clone()), ("safe", "../../escape".into())] {
            record.task_name = name.into();
            record.session_id = id;
            let manifest = AgentManifest { version: 1, owner: "owner".into(), agents: vec![record.clone(), AgentRecord { task_name: "ok".into(), ..good.clone() }] };
            std::fs::write(root.join("manifest.json"), serde_json::to_vec(&manifest).unwrap()).unwrap();
            let names: Vec<String> = AgentStore::open(root.clone(), "owner").unwrap().records().into_iter().map(|r| r.task_name).collect();
            assert_eq!(names, ["ok"]);
        }
        // owner 不符或文件损坏：改名留档，以空清单打开。
        let bad_count = || std::fs::read_dir(&root).unwrap().filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().starts_with("manifest.json.bad.")).count();
        assert!(AgentStore::open(root.clone(), "other-owner").unwrap().records().is_empty());
        std::thread::sleep(std::time::Duration::from_millis(2));
        std::fs::write(root.join("manifest.json"), b"{not json").unwrap();
        assert!(AgentStore::open(root.clone(), "owner").unwrap().records().is_empty());
        assert_eq!(bad_count(), 2);
        assert!(valid_task_name("probe_1"));
        for name in ["", ".", "..", "/absolute", "a/b", "a\\b"] { assert!(!valid_task_name(name)); }
        assert!(!valid_task_name(&"a".repeat(65)));
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
