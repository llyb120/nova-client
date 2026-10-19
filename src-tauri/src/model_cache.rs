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
    // Lyra 的选项可直接从本地配置与模型缓存生成，避免沿用过期/乱码的显示名。
    if agent_kind == "lyra" {
        let config = crate::lyra_complete::load_config(config_dir).ok()?;
        return Some(serde_json::json!({
            "configOptions": [{ "id": "model", "name": "Model",
                "currentValue": crate::lyra::config::default_model(&config).ok()?,
                "options": crate::lyra::config::model_options(&config) }],
            "modes": Value::Null,
        }));
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn lyra_names_are_rebuilt_from_utf8_config_instead_of_stale_cache() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("alkaid")).unwrap();
        fs::write(root.path().join("alkaid/config.jsonc"), json!({
            "provider": { "local-codex": { "name": "本地 Codex", "models": {
                "m": { "name": "中文模型", "variants": {"high": {}} }
            } } }
        }).to_string()).unwrap();
        let stale = json!({"configOptions":[{"id":"model","options":[{"value":"local-codex/m/variant/high","name":"Ã¦ÂÂ¬Ã¥ÂÂ° Codex"}]}]});
        save(root.path(), "lyra", &stale);
        let loaded = load(root.path(), "lyra").unwrap();
        assert_eq!(loaded["configOptions"][0]["options"][0]["name"], "本地 Codex / 中文模型 · High");
        assert_eq!(loaded["configOptions"][0]["currentValue"], "local-codex/m/variant/high");
        save(root.path(), "codex", &stale);
        assert_eq!(load(root.path(), "codex"), Some(stale));
    }
}
