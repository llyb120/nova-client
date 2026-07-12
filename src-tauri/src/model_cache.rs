//! 各后端模型列表磁盘缓存（`~/.nova/model-options/<agent>.json`）。
//! 启动时先读缓存立刻展示，后台再向 agent 拉最新列表覆盖。

use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

fn cache_dir(config_dir: &Path) -> PathBuf {
    config_dir.join("model-options")
}

fn cache_path(config_dir: &Path, agent_kind: &str) -> PathBuf {
    cache_dir(config_dir).join(format!("{agent_kind}.json"))
}

/// 读取某后端上次落盘的模型选项；损坏/缺失返回 None。
pub fn load(config_dir: &Path, agent_kind: &str) -> Option<Value> {
    let raw = fs::read_to_string(cache_path(config_dir, agent_kind)).ok()?;
    serde_json::from_str(&raw).ok()
}

/// 把最新模型选项写入磁盘（失败静默，不影响主流程）。
pub fn save(config_dir: &Path, agent_kind: &str, options: &Value) {
    let dir = cache_dir(config_dir);
    if fs::create_dir_all(&dir).is_err() {
        return;
    }
    if let Ok(json) = serde_json::to_string(options) {
        let _ = fs::write(cache_path(config_dir, agent_kind), json);
    }
}
