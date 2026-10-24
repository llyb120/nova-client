//! One-way conversion of retired agent selections; no retired runtime is retained.
use crate::lyra::backend_migration::Migration;
use crate::settings::Settings;
use crate::threads::{AgentKind, Thread, ThreadStore};
use serde_json::{json, Value};
use std::path::Path;

/// 迁移读取 CLI 配置时使用的环境：进程环境叠加用户自定义变量（启动迁移、时间机器恢复与 UI 偏好迁移共用）。
pub(crate) fn env(settings: &Settings) -> std::collections::HashMap<String, String> {
    let mut env = crate::lyra::config::process_env();
    env.extend(settings.custom_env_vars.clone());
    env
}

/// 自动迁移失败的旧会话由用户在模型选择器中显式选择 Lyra 模型改绑；不猜测映射，历史原样保留。
pub(crate) fn bind_thread(thread: &mut Thread, model: &str) -> Result<crate::threads::Item, String> {
    let model = model.trim();
    if model.is_empty() || !model.contains('/') || model.chars().any(char::is_control) {
        return Err("请选择一个具体的 Lyra 模型".into());
    }
    if thread.is_roaming_guest() { return Err("漫游会话由执行端迁移，不能在本机改绑".into()); }
    let source = thread.agent_kind.as_str().to_owned();
    if !thread.migrate_to_lyra(Some(model.into())) {
        return Err(format!("不支持的后端：{source}，只有待迁移的旧会话可以改绑到 Lyra"));
    }
    Ok(thread.push_system(format!("已将原 {source} 会话改绑到 Lyra 模型 {model}，历史已保留，下一条消息会携带此前上下文。"), "info"))
}

fn retired(source: &str) -> bool {
    serde_json::from_value::<AgentKind>(json!(source)).is_ok_and(|kind| matches!(kind, AgentKind::Legacy(_)))
}

/// 启动迁移与时间机器恢复必须选择同一真实模型，不能把未解析 Auto 当成默认选择。
pub(crate) fn thread_target(root: &Path, migration: &mut Migration, thread: &Thread) -> Result<String, String> {
    let selected = thread.auto_routed_model.as_deref().map(str::trim).filter(|s| !s.is_empty())
        .or(thread.model.as_deref().map(str::trim).filter(|s| !s.is_empty()));
    if selected.is_some_and(|model| ["__nova_auto_", "__auto", "__codex_auto", "__cursor_auto"].iter().any(|prefix| model.starts_with(*prefix)))
        || (selected.is_none() && thread.auto_route_selection.is_some()) {
        return Err("旧 Auto 会话缺少缓存的实际模型，不能改用默认模型".into());
    }
    let source = thread.agent_kind.as_str();
    if matches!(source, "codex" | "codexplus") {
        if let Some(selected) = selected {
            for key in [source, "codex"] {
                let Some(cache) = crate::model_cache::load(root, key) else { continue };
                let metadata_effort = cache["configOptions"].as_array()
                    .and_then(|configs| configs.iter().find(|config| config["id"] == "model"))
                    .and_then(|config| config["options"].as_array())
                    .and_then(|options| options.iter().find(|option| option["value"].as_str() == Some(selected)))
                    .and_then(|option| option["_meta"]["codex.ai/effort"].as_str())
                    .map(str::trim).filter(|effort| !effort.is_empty());
                if let Some(effort) = metadata_effort {
                    // 元数据优先于组合 value 的档位；只剥离已知档位，保留模型 ID 内其他冒号。
                    let model = selected.rsplit_once(':')
                        .filter(|(_, suffix)| matches!(*suffix, "none" | "minimal" | "default" | "low" | "medium" | "high" | "xhigh" | "max" | "ultra"))
                        .map_or(selected, |(model, _)| model);
                    return migration.target(source, Some(model), Some(effort));
                }
            }
        }
    }
    migration.target(source, selected, thread.reasoning_effort.as_deref())
}

fn selection_key(migration: &mut Migration, key: &str) -> Result<String, String> {
    let Some((source, model)) = key.split_once(':') else { return Ok(key.into()) };
    if !retired(source) { return Ok(key.into()); }
    let model = crate::threads::percent_decode(model);
    let model = migration.target(source, Some(&model), None)?;
    // encodeURIComponent's safe characters; unlike form encoding, a space must not become '+'.
    let encoded: String = model.bytes().map(|byte| {
        if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) { (byte as char).to_string() }
        else { format!("%{byte:02X}") }
    }).collect();
    Ok(format!("lyra:{encoded}"))
}

fn save_settings(root: &Path, settings: &Settings) -> Result<(), String> {
    let path = root.join("settings.json");
    let original = match std::fs::read(&path) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(format!("读取迁移设置失败：{error}")),
    };
    let mut raw: Value = original.as_deref().map(serde_json::from_slice).transpose()
        .map_err(|_| "设置不是有效 JSON；未覆盖原文件")?.unwrap_or_else(|| json!({}));
    let object = raw.as_object_mut().ok_or("设置必须是 JSON 对象；未覆盖原文件")?;
    let migrated = serde_json::to_value(settings).map_err(|e| e.to_string())?;
    // Preserve fields owned by other versions/features; migration is not a settings reset.
    for field in ["sceneModels", "modelFavorites", "sessionShortcuts", "lyraEnabled"] {
        object.insert(field.into(), migrated[field].clone());
    }
    if let Some(bytes) = original {
        let backup = root.join("settings.json.before-backend-migration");
        if !backup.exists() { crate::thread_storage::atomic_write(&backup, &bytes)?; }
    }
    crate::thread_storage::atomic_write(&path, &serde_json::to_vec_pretty(&raw).map_err(|e| e.to_string())?)
}

pub(crate) fn migrate(root: &Path, store: &mut ThreadStore, settings: &mut Settings) -> Result<Vec<String>, String> {
    let mut migration = Migration::load(root, &env(settings))?;
    let mut warnings = Vec::new();
    let mut next = settings.clone();
    let mut settings_changed = false;
    let scenes = &mut next.scene_models;
    for target in scenes.lightweight.iter_mut().chain(&mut scenes.coding).chain(&mut scenes.computer) {
        if !retired(&target.agent_kind) { continue; }
        match migration.target(&target.agent_kind, Some(&target.model), None) {
            Ok(model) => { target.agent_kind = "lyra".into(); target.model = model; settings_changed = true; }
            Err(error) => warnings.push(format!("场景模型 {}：{error}", target.agent_kind)),
        }
    }
    for key in &mut next.model_favorites {
        match selection_key(&mut migration, key) {
            Ok(migrated) if migrated != *key => { *key = migrated; settings_changed = true; }
            Ok(_) => {}
            Err(error) => warnings.push(format!("模型收藏：{error}")),
        }
    }
    for shortcut in &mut next.session_shortcuts {
        if shortcut.action != "selectModel" { continue; }
        match selection_key(&mut migration, &shortcut.target) {
            Ok(migrated) if migrated != shortcut.target => { shortcut.target = migrated; settings_changed = true; }
            Ok(_) => {}
            Err(error) => warnings.push(format!("模型快捷键 {}：{error}", shortcut.keys)),
        }
    }
    let mut plans = Vec::new();
    let mut failures = Vec::new();
    for thread in &store.threads {
        if !matches!(thread.agent_kind, AgentKind::Legacy(_)) { continue; }
        if thread.is_roaming_guest() { continue; } // The execution host owns its provider configuration.
        match thread_target(root, &mut migration, thread) {
            Ok(model) => plans.push((thread.id.clone(), model)),
            Err(error) => {
                let message = format!("模型自动迁移未完成：{error}。原模型和会话已保留；请在 Lyra 配置对应 provider，然后在此会话的模型选择器中选择一个 Lyra 模型完成改绑。");
                warnings.push(format!("会话 {}：{error}", thread.title));
                failures.push((thread.id.clone(), message));
            }
        }
    }
    // Commit providers first: a crash can leave an unused provider, never a dangling selection.
    migration.save()?;
    if settings_changed || !plans.is_empty() {
        next.lyra_enabled = true;
        save_settings(root, &next)?;
        *settings = next;
    }
    for (id, model) in plans {
        if let Some(thread) = store.get_mut(&id) { thread.migrate_to_lyra(Some(model)); }
        store.save_thread(&id);
    }
    for (id, message) in failures {
        if let Some(thread) = store.get_mut(&id) {
            if !thread.items.iter().any(|item| matches!(item, crate::threads::Item::System { text, .. } if text == &message)) {
                let updated = thread.updated_at;
                thread.push_system(message, "warn");
                thread.updated_at = updated;
                store.save_thread(&id);
            }
        }
    }
    warnings.sort(); warnings.dedup();
    Ok(warnings)
}

/// UI-local preferences use the same mapper; per-target failures leave the original selection intact.
pub(crate) fn selections(root: &Path, env: &std::collections::HashMap<String, String>, targets: &[Value]) -> Result<Vec<Value>, String> {
    let mut migration = Migration::load(root, env)?;
    let results = targets.iter().map(|target| {
        match migration.target(target["agentKind"].as_str().unwrap_or(""), target["model"].as_str(), target["reasoningEffort"].as_str()) {
            Ok(model) => json!({"model":model}),
            Err(error) => json!({"error":error}),
        }
    }).collect();
    migration.save()?;
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn thread_selection_rejects_unresolved_auto_and_preserves_cached_effort_priority() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("model-options")).unwrap();
        // 即使缓存存在明确默认模型，未解析 Auto 也不能借它迁移。
        crate::model_cache::save(root.path(), "codex", &json!({"configOptions":[{
            "id":"model", "currentValue":"unrelated-default", "options":[
                {"value":"actual-model:high", "_meta":{"codex.ai/effort":"low"}}
            ]
        }]}));
        let mut migration = Migration::load(root.path(), &std::collections::HashMap::new()).unwrap();
        let mut thread = Thread::new(".".into(), AgentKind::Legacy("codex".into()), None, None, Some("medium".into()), false);
        thread.auto_route_selection = Some("__nova_auto_iq__".into());
        thread.auto_routed_model = Some(" \t".into());
        for selected in [None, Some(" \n"), Some(" __nova_auto_iq__ "), Some("__auto_value__"), Some("__codex_auto_fast__")] {
            thread.model = selected.map(str::to_owned);
            let before = serde_json::to_value(&thread).unwrap();
            let error = thread_target(root.path(), &mut migration, &thread).unwrap_err();
            assert!(error.contains("缺少缓存的实际模型"), "{error}");
            assert_eq!(serde_json::to_value(&thread).unwrap(), before);
            assert!(!migration.changed());
        }
        thread.model = Some("__nova_auto_iq__".into());
        thread.auto_routed_model = Some(" actual-model:high ".into());
        assert!(thread_target(root.path(), &mut migration, &thread).unwrap().ends_with("/actual-model/variant/low"));
        // 空的实际缓存不能吞掉显式原模型；无精确元数据时后缀优先于独立 effort。
        thread.auto_routed_model = Some(" ".into());
        thread.model = Some("uncached-model:high".into());
        assert!(thread_target(root.path(), &mut migration, &thread).unwrap().ends_with("/uncached-model/variant/high"));
        thread.model = Some("uncached-model".into());
        assert!(thread_target(root.path(), &mut migration, &thread).unwrap().ends_with("/uncached-model/variant/medium"));
    }

    #[test]
    fn manual_binding_converts_only_legacy_threads_and_keeps_history() {
        let mut thread = Thread::new(".".into(), AgentKind::Legacy("codex".into()), Some("__nova_auto_iq__".into()), None, Some("high".into()), false);
        thread.push_user("旧历史".into(), Vec::new());
        assert!(bind_thread(&mut thread, " ").is_err());
        assert!(bind_thread(&mut thread, "no-provider").is_err());
        assert_eq!(thread.agent_kind, AgentKind::Legacy("codex".into()));
        bind_thread(&mut thread, " p/model ").unwrap();
        assert_eq!(thread.agent_kind, AgentKind::Lyra);
        assert_eq!(thread.model.as_deref(), Some("p/model"));
        assert!(thread.take_prompt_context("Lyra").unwrap().contains("旧历史"));
        assert!(bind_thread(&mut thread, "p/other").unwrap_err().contains("不支持的后端"));
        let mut unknown = Thread::new(".".into(), AgentKind::Unsupported, None, None, None, false);
        assert!(bind_thread(&mut unknown, "p/model").is_err());
        assert_eq!(unknown.agent_kind, AgentKind::Unsupported);
    }

    #[test]
    fn settings_and_history_migrate_together_without_changing_model_or_text() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("settings.json"), r#"{"customField":true}"#).unwrap();
        let mut settings = Settings::default();
        settings.scene_models.coding.push(crate::settings::ModelTarget { agent_kind: "codex".into(), model: "gpt-test:high".into() });
        settings.model_favorites = vec!["codex:gpt-test%3Ahigh".into()];
        settings.session_shortcuts.push(crate::settings::SessionShortcut { id:"s".into(), keys:"Ctrl+1".into(), action:"selectModel".into(), target:"codex:gpt-test%3Ahigh".into() });
        settings.session_shortcuts.push(crate::settings::SessionShortcut { id:"t".into(), keys:"Ctrl+2".into(), action:"insertText".into(), target:"codex:gpt-test%3Ahigh".into() });
        let mut store = ThreadStore::load(root.path().to_path_buf());
        let mut thread = crate::threads::Thread::new(".".into(), AgentKind::Legacy("codex".into()), Some("gpt-test".into()), None, Some("high".into()), false);
        thread.push_system("retained".into(), "info");
        thread.acp_session_id = Some("old-session".into());
        store.threads.push(thread);
        assert!(migrate(root.path(), &mut store, &mut settings).unwrap().is_empty());
        let model = settings.scene_models.coding[0].model.clone();
        assert!(model.ends_with("/gpt-test/variant/high"));
        assert_eq!(settings.scene_models.coding[0].agent_kind, "lyra");
        assert_eq!(store.threads[0].agent_kind, AgentKind::Lyra);
        assert_eq!(store.threads[0].model.as_deref(), Some(model.as_str()));
        assert!(store.threads[0].acp_session_id.is_none());
        assert_eq!(settings.session_shortcuts[1].target, "codex:gpt-test%3Ahigh");
        assert_eq!(settings.session_shortcuts[0].target, settings.model_favorites[0]);
        let saved: Value = serde_json::from_slice(&std::fs::read(root.path().join("settings.json")).unwrap()).unwrap();
        assert_eq!(saved["customField"], true);
        assert!(root.path().join("settings.json.before-backend-migration").is_file());
        store.threads[0].acp_session_id = Some("new-lyra-session".into());
        assert!(migrate(root.path(), &mut store, &mut settings).unwrap().is_empty());
        assert_eq!(store.threads[0].acp_session_id.as_deref(), Some("new-lyra-session"));
    }
}
