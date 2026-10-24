//! Offline migration only: callers must save providers successfully before changing targets.
use serde_json::{json, Value};
use std::{cell::RefCell, collections::HashMap, fs, io::{Read, Write}, path::{Path, PathBuf}};

const EFFORTS: &[&str] = &["none", "minimal", "default", "low", "medium", "high", "xhigh", "max", "ultra"];

pub(crate) struct Migration {
    root: PathBuf,
    env: HashMap<String, String>,
    config: Value,
    saved: RefCell<(Option<Vec<u8>>, Value)>,
}

fn read_optional(path: &Path) -> Result<Option<Vec<u8>>, String> {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(format!("无法读取迁移配置：{}", path.display())),
    };
    let mut bytes = Vec::new();
    file.take(4 * 1024 * 1024 + 1).read_to_end(&mut bytes).map_err(|_| "读取迁移配置失败")?;
    if bytes.len() > 4 * 1024 * 1024 { return Err("迁移配置超过 4 MiB".into()); }
    Ok(Some(bytes))
}

fn parse(bytes: &[u8]) -> Result<Value, String> {
    std::str::from_utf8(bytes).ok().and_then(|s| crate::lyra_complete::parse_jsonc(s).ok())
        .filter(Value::is_object).ok_or_else(|| "迁移配置必须是有效 JSONC 对象".into())
}

fn json_file(path: &Path) -> Result<Value, String> {
    read_optional(path)?.map(|bytes| parse(&bytes)).transpose().map(|v| v.unwrap_or_else(|| json!({})))
}

fn text(value: &Value) -> Option<&str> { value.as_str().map(str::trim).filter(|s| !s.is_empty()) }

impl Migration {
    pub(crate) fn load(root: &Path, env: &HashMap<String, String>) -> Result<Self, String> {
        let raw = read_optional(&root.join("alkaid/config.jsonc"))?;
        let config = raw.as_deref().map(parse).transpose()?.unwrap_or_else(|| json!({}));
        if config.get("provider").is_some_and(|p| !p.is_object()) {
            return Err("Lyra provider 必须是对象，未执行迁移".into());
        }
        Ok(Self { root: root.into(), env: env.clone(),
            saved: RefCell::new((raw, config.clone())), config })
    }

    /// Returns the actual provider key, never a guessed vendor/model mapping.
    pub(crate) fn target(&mut self, source: &str, model: Option<&str>, effort: Option<&str>) -> Result<String, String> {
        let source = source.trim().to_ascii_lowercase();
        if matches!(source.as_str(), "lyra" | "alkaid") { return Ok(model.unwrap_or_default().into()); }
        if matches!(source.as_str(), "opencode" | "opencodeplus") {
            let selected = model.filter(|s| !s.is_empty()).ok_or("OpenCode 未指定模型，无法推断 provider")?;
            let (base, variant) = selected.rsplit_once("/variant/").map(|(b, v)| (b, Some(v))).unwrap_or((selected, None));
            let (provider, id) = base.split_once('/').ok_or("OpenCode 模型缺少 provider")?;
            let existing = &self.config["provider"][provider]["models"][id];
            if !existing.is_object() || variant.is_some_and(|v| !existing["variants"][v].is_object()) {
                return Err("OpenCode provider/model 尚未在 Lyra 配置，不能猜测等价服务".into());
            }
            if effort.is_some_and(|e| !e.is_empty() && e != "default") {
                return Err("OpenCode 独立 effort 无已确认映射，请保留原选择".into());
            }
            return Ok(selected.into());
        }
        let (canonical, preset) = match source.as_str() {
            "codex" | "codexplus" => ("codex", "local-codex"),
            "claude" | "claudecode" => ("claude", "local-claude-code"),
            "devin" => ("devin", "devin"),
            "codebuddy" | "codebuddyplus" => ("codebuddy", "codebuddy"),
            "workbuddy" => ("workbuddy", "workbuddy"),
            _ => return Err(format!("后端 {source} 没有已确认的 Lyra 等价 provider")),
        };
        let (selection, configured_effort) = match model.map(str::trim).filter(|s| !s.is_empty()) {
            Some(model) => (model.to_owned(), None),
            None => self.default_selection(&source, canonical)?,
        };
        let (id, suffix) = selection.rsplit_once(':').filter(|(id, e)| !id.is_empty() && EFFORTS.contains(e))
            .map(|(id, e)| (id, Some(e))).unwrap_or((&selection, None));
        if id.starts_with("__codex_auto") || id.starts_with("__cursor_auto") {
            return Err("旧 Auto 选择没有已确认的实际模型绑定，不能作为模型 ID 迁移".into());
        }
        if canonical == "claude" && id.strip_suffix("[1m]").unwrap_or(id) == "opusplan" {
            // 规划用 Opus、执行用 Sonnet 的组合语义在 Lyra 中没有已确认等价模型，不猜测为纯 Opus。
            return Err("Claude opusplan 没有已确认的 Lyra 等价模型，保留原选择待手动处理".into());
        }
        let alias;
        let id = if canonical == "claude" {
            // API users may bind CLI aliases to custom models; subscription aliases stay for preset catalog resolution.
            alias = self.claude_alias(id)?;
            alias.as_str()
        } else { id };
        let effort = suffix.or(effort.filter(|s| !s.is_empty())).or(configured_effort.as_deref());
        if effort.is_some_and(|e| !EFFORTS.contains(&e)) { return Err("无法迁移未知的思考档位".into()); }
        let effort = effort.filter(|e| *e != "default");
        if id.is_empty() || id.contains("/variant/") || id.chars().any(char::is_control) {
            return Err("旧模型 ID 无法无损编码为 Lyra selector".into());
        }
        let variant = effort.map(|e| json!({ "reasoningEffort": e }));
        // Catalog disables effort emission; migrated explicit choices must reach native UID validation.
        let devin_flags = json!({ "supportsReasoningEffort": true, "supportsThinkingToggle": true });
        if self.config.get("provider").is_none() { self.config["provider"] = json!({}); }
        let providers = self.config["provider"].as_object_mut().unwrap();
        let reusable = providers.iter().find(|(key, p)| {
            !key.is_empty() && !key.contains('/') && !key.chars().any(char::is_control)
                && p["preset"] == preset && p.as_object().is_some_and(|p| p.keys().all(|k| matches!(k.as_str(), "preset" | "name" | "models" | "options")))
                && p.get("options").is_none_or(|o| o.as_object().is_some_and(|o| o.keys().all(|k| k == "proxy")))
                && p.get("models").is_none_or(Value::is_object)
                && p["models"].get(id).is_none_or(|m| m.as_object().is_some_and(|m| {
                    // Reuse metadata, but not model-specific routing/default overrides or conflicting variants.
                    m.keys().all(|k| matches!(k.as_str(), "name" | "limit" | "cost" | "modalities" | "reasoning" | "variants")
                        || (k == "options" && canonical == "devin" && m[k] == devin_flags))
                        && (effort.is_none() || m.get("reasoning") != Some(&json!(false)))
                        && m.get("variants").is_none_or(Value::is_object)
                        && effort.is_none_or(|e| m.get("variants").and_then(|v| v.get(e)).is_none_or(|v| Some(v) == variant.as_ref()))
                }))
        }).map(|(key, _)| key.clone());
        let key = reusable.unwrap_or_else(|| {
            let mut key = preset.to_string();
            let mut n = 1;
            while providers.contains_key(&key) { n += 1; key = format!("{preset}-migration-{n}"); }
            // Lyra 跟随系统代理，不继承已退休后端的代理设置。
            providers.insert(key.clone(), json!({ "preset": preset, "models": {} }));
            key
        });
        let provider = &mut providers[&key];
        if provider.get("models").is_none() { provider["models"] = json!({}); }
        let entry = provider["models"].as_object_mut().unwrap().entry(id.to_owned()).or_insert_with(|| json!({}));
        if let (Some(effort), Some(variant)) = (effort, variant) {
            if canonical == "devin" { entry["options"] = devin_flags; }
            if entry.get("variants").is_none() { entry["variants"] = json!({}); }
            entry["variants"].as_object_mut().unwrap().entry(effort.to_owned()).or_insert(variant);
        }
        Ok(format!("{key}/{id}{}", effort.map(|e| format!("/variant/{e}")).unwrap_or_default()))
    }

    fn variable(&self, name: &str) -> Option<&str> {
        self.env.get(name).map(String::as_str).map(str::trim).filter(|s| !s.is_empty())
    }

    fn cli_root(&self, variable: &str, folder: &str) -> Option<PathBuf> {
        self.variable(variable).map(PathBuf::from).or_else(|| self.variable("USERPROFILE").or_else(|| self.variable("HOME")).map(|h| Path::new(h).join(folder)))
    }

    fn claude_alias(&self, id: &str) -> Result<String, String> {
        let variable = match id.strip_suffix("[1m]").unwrap_or(id) {
            "sonnet" => "ANTHROPIC_DEFAULT_SONNET_MODEL",
            "opus" => "ANTHROPIC_DEFAULT_OPUS_MODEL",
            "haiku" => "ANTHROPIC_DEFAULT_HAIKU_MODEL",
            _ => return Ok(id.into()),
        };
        let settings = self.cli_root("CLAUDE_CONFIG_DIR", ".claude").map(|r| json_file(&r.join("settings.json"))).transpose()?.unwrap_or_else(|| json!({}));
        let mapped = settings["env"].get(variable).and_then(Value::as_str).or_else(|| self.env.get(variable).map(String::as_str))
            .map(str::trim).filter(|s| !s.is_empty()).unwrap_or(id);
        Ok(format!("{mapped}{}", if id.ends_with("[1m]") && !mapped.ends_with("[1m]") { "[1m]" } else { "" }))
    }

    fn default_selection(&self, source: &str, canonical: &str) -> Result<(String, Option<String>), String> {
        let (model, effort) = match canonical {
            "codex" => {
                let raw = self.cli_root("CODEX_HOME", ".codex").map(|r| read_optional(&r.join("config.toml"))).transpose()?.flatten();
                let mut config = match raw {
                    Some(raw) => {
                        let config: toml::Value = std::str::from_utf8(&raw).ok().and_then(|s| toml::from_str(s).ok()).ok_or("Codex 模型配置不是有效 TOML")?;
                        serde_json::to_value(config).map_err(|_| "无法解析 Codex 模型配置")?
                    }
                    None => json!({}),
                };
                if let Some(profile) = text(&config["profile"]) {
                    let values = config["profiles"][profile].as_object().cloned().ok_or("Codex profile 不存在")?;
                    config.as_object_mut().unwrap().extend(values);
                }
                (text(&config["model"]).map(str::to_owned), text(&config["model_reasoning_effort"]).map(str::to_owned))
            }
            "claude" => {
                let settings = self.cli_root("CLAUDE_CONFIG_DIR", ".claude").map(|r| json_file(&r.join("settings.json"))).transpose()?.unwrap_or_else(|| json!({}));
                let variable = |key: &str| settings["env"].get(key).and_then(Value::as_str).or_else(|| self.env.get(key).map(String::as_str)).map(str::trim).filter(|s| !s.is_empty());
                (variable("ANTHROPIC_MODEL").or_else(|| text(&settings["model"])).or_else(|| variable("ANTHROPIC_DEFAULT_MODEL")).map(str::to_owned),
                    variable("CLAUDE_CODE_EFFORT_LEVEL").or_else(|| text(&settings["effortLevel"])).map(str::to_owned))
            }
            "devin" => {
                let root = self.variable("DEVIN_CONFIG_HOME").map(PathBuf::from).or_else(|| {
                    if cfg!(windows) { self.variable("APPDATA").map(PathBuf::from) }
                    else { self.variable("XDG_CONFIG_HOME").map(PathBuf::from).or_else(|| self.variable("HOME").map(|h| Path::new(h).join(".config"))) }
                });
                let config = root.map(|r| json_file(&r.join("devin/config.json"))).transpose()?.unwrap_or_else(|| json!({}));
                (self.variable("DEVIN_MODEL").or_else(|| text(&config["agent"]["model"])).map(str::to_owned), None)
            }
            _ => {
                let prefix = canonical.to_ascii_uppercase();
                let settings = self.cli_root(&format!("{prefix}_CONFIG_DIR"), &format!(".{canonical}")).map(|r| json_file(&r.join("settings.json"))).transpose()?.unwrap_or_else(|| json!({}));
                (self.variable(&format!("{prefix}_MODEL")).or_else(|| text(&settings["model"])).map(str::to_owned), None)
            }
        };
        if let Some(model) = model { return Ok((model, effort)); }
        for key in [source, canonical] {
            let cache = json_file(&self.root.join("model-options").join(format!("{key}.json")))?;
            let Some(option) = cache["configOptions"].as_array().and_then(|v| v.iter().find(|v| v["id"] == "model")) else { continue };
            if let Some(model) = text(&option["currentValue"]) { return Ok((model.into(), effort)); }
            let defaults: Vec<_> = option["options"].as_array().into_iter().flatten()
                .filter(|v| v["_meta"]["codex.ai/default"] == true).filter_map(|v| text(&v["value"])).collect();
            if defaults.len() == 1 { return Ok((defaults[0].into(), effort)); }
        }
        Err(format!("{source} 未指定模型，且本地模型配置/缓存没有明确选择；未猜测默认模型"))
    }

    pub(crate) fn changed(&self) -> bool { self.config != self.saved.borrow().1 }

    pub(crate) fn save(&self) -> Result<(), String> {
        if !self.changed() { return Ok(()); }
        let path = self.root.join("alkaid/config.jsonc");
        let mut saved = self.saved.borrow_mut();
        // ponytail: startup's single writer + optimistic conflict detection; cross-process writers need a shared config lock.
        if read_optional(&path)? != saved.0 { return Err("Lyra 配置在迁移期间被修改，拒绝覆盖；请重新加载迁移".into()); }
        fs::create_dir_all(path.parent().unwrap()).map_err(|_| "无法创建 Lyra 配置目录")?;
        if let Some(raw) = &saved.0 {
            let backup = path.with_file_name("config.jsonc.before-backend-migration");
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)] { use std::os::unix::fs::OpenOptionsExt; options.mode(0o600); }
            match options.open(&backup) {
                Ok(mut file) => {
                    if file.write_all(raw).and_then(|_| file.sync_all()).is_err() {
                        drop(file);
                        let _ = fs::remove_file(&backup);
                        return Err("备份原始 Lyra 配置失败，未保存迁移".into());
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && backup.is_file() => {}
                Err(_) => return Err("无法创建 Lyra 配置迁移备份".into()),
            }
        }
        let mut bytes = serde_json::to_vec_pretty(&self.config).map_err(|_| "无法编码 Lyra 迁移配置")?;
        bytes.push(b'\n');
        if read_optional(&path)? != saved.0 { return Err("Lyra 配置在备份期间被修改，拒绝覆盖".into()); }
        crate::thread_storage::atomic_write(&path, &bytes)?;
        *saved = (Some(bytes), self.config.clone());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup(config: Option<&str>) -> (tempfile::TempDir, Migration) {
        let root = tempfile::tempdir().unwrap();
        if let Some(config) = config { put(root.path(), "alkaid/config.jsonc", config); }
        let migration = Migration::load(root.path(), &HashMap::new()).unwrap();
        (root, migration)
    }
    fn put(root: &Path, file: &str, value: &str) {
        let path = root.join(file);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, value).unwrap();
    }

    #[test]
    fn native_efforts_are_independent_and_defaults_do_not_invent_variants() {
        let (_root, mut m) = setup(None);
        assert_eq!(m.target("devin", Some("swe-medium:max"), Some("low")).unwrap(), "devin/swe-medium/variant/max");
        assert_eq!(m.target("devin", Some("swe-medium:high"), None).unwrap(), "devin/swe-medium/variant/high");
        let model = &m.config["provider"]["devin"]["models"]["swe-medium"];
        assert!(model["options"].get("reasoningEffort").is_none());
        assert_eq!(model["options"]["supportsReasoningEffort"], true);
        assert_eq!(model["variants"]["max"], json!({"reasoningEffort":"max"}));
        assert_eq!(model["variants"]["high"]["reasoningEffort"], "high");
        assert_eq!(m.target("codexplus", Some("gpt:high"), Some("low")).unwrap(), "local-codex/gpt/variant/high");
        assert_eq!(m.target("claudecode", Some("sonnet[1m]:default"), Some("high")).unwrap(), "local-claude-code/sonnet[1m]");
        assert_eq!(m.target("codebuddyplus", Some("hy"), Some("high")).unwrap(), "codebuddy/hy/variant/high");
        assert_eq!(m.target("workbuddy", Some("hy:low"), None).unwrap(), "workbuddy/hy/variant/low");
        assert_eq!(m.target("codex", Some("custom:unknown"), None).unwrap(), "local-codex/custom:unknown");
        assert_eq!(m.target("alkaid", Some("p/a:b/variant/fast"), None).unwrap(), "p/a:b/variant/fast");
    }

    #[test]
    fn providers_are_reused_only_without_routing_overrides_and_never_overwritten() {
        let (root, mut m) = setup(Some(r#"{"provider":{
            "local-codex":{"preset":"local-codex","options":{"baseURL":"https://custom.invalid","apiKey":"keep"}},
            "local-codex-migration-2":{"api":"anthropic-messages"},
            "team":{"preset":"local-claude-code","options":{"proxy":"existing"},"models":{"sonnet[1m]":{"name":"Keep","limit":{"context":1000000}}}}
        }}"#));
        let original = m.config.clone();
        put(root.path(), "settings.json", r#"{"codexProxy":"legacy","claudeProxy":"do-not-overwrite"}"#);
        m = Migration::load(root.path(), &HashMap::new()).unwrap();
        assert_eq!(m.target("codex", Some("gpt:high"), None).unwrap(), "local-codex-migration-3/gpt/variant/high");
        assert!(m.config["provider"]["local-codex-migration-3"].get("options").is_none(), "旧代理设置不应迁入 Lyra");
        assert_eq!(m.config["provider"]["local-codex"], original["provider"]["local-codex"]);
        assert_eq!(m.target("claude", Some("sonnet[1m]:high"), None).unwrap(), "team/sonnet[1m]/variant/high");
        assert_eq!(m.config["provider"]["team"]["options"]["proxy"], "existing");
        assert_eq!(m.config["provider"]["team"]["models"]["sonnet[1m]"]["name"], "Keep");
        m.config["provider"]["team"]["models"]["sonnet[1m]"]["variants"]["low"] = json!({"reasoningEffort":"max"});
        assert!(m.target("claude", Some("sonnet[1m]:low"), None).unwrap().starts_with("local-claude-code/"));
        assert_eq!(m.config["provider"]["team"]["models"]["sonnet[1m]"]["variants"]["low"]["reasoningEffort"], "max");
        m.config["provider"]["codebuddy"] = json!({"preset":"codebuddy","models":{"hy":{"reasoning":false}}});
        assert_eq!(m.target("codebuddy", Some("hy:high"), None).unwrap(), "codebuddy-migration-2/hy/variant/high");
        assert_eq!(m.config["provider"]["codebuddy"]["models"]["hy"]["reasoning"], false);
    }

    #[test]
    fn empty_models_use_only_explicit_noncredential_configuration_or_cache() {
        let (root, _) = setup(None);
        put(root.path(), "cli/config.toml", "model='old'\nprofile='work'\n[profiles.work]\nmodel='configured'\nmodel_reasoning_effort='high'\n");
        // Reading these would fail; no importer/credential loader may run during migration.
        put(root.path(), "cli/auth.json", "not-json-secret");
        put(root.path(), "claude/.credentials.json", "not-json-secret");
        put(root.path(), "claude/settings.json", r#"{"env":{"ANTHROPIC_MODEL":""},"model":"opus[1m]","effortLevel":"max"}"#);
        let env = HashMap::from([
            ("CODEX_HOME".into(), root.path().join("cli").to_string_lossy().into_owned()),
            ("CLAUDE_CONFIG_DIR".into(), root.path().join("claude").to_string_lossy().into_owned()),
            ("ANTHROPIC_MODEL".into(), "must-not-win".into()),
        ]);
        let mut m = Migration::load(root.path(), &env).unwrap();
        assert_eq!(m.target("codex", None, Some("low")).unwrap(), "local-codex/configured/variant/low");
        assert_eq!(m.target("claude", None, None).unwrap(), "local-claude-code/opus[1m]/variant/max");
        assert!(m.target("devin", None, None).is_err());
        put(root.path(), "model-options/devin.json", r#"{"configOptions":[{"id":"model","currentValue":"adaptive","options":[]}]}"#);
        assert_eq!(m.target("devin", None, None).unwrap(), "devin/adaptive");
        let serialized = m.config.to_string();
        assert!(!serialized.contains("secret") && !serialized.contains("apiKey"));
    }

    #[test]
    fn configured_claude_aliases_preserve_context_and_settings_env_precedence() {
        let (root, _) = setup(None);
        put(root.path(), "claude/settings.json", r#"{"env":{"ANTHROPIC_DEFAULT_SONNET_MODEL":"custom-sonnet","ANTHROPIC_DEFAULT_OPUS_MODEL":""}}"#);
        let env = HashMap::from([
            ("CLAUDE_CONFIG_DIR".into(), root.path().join("claude").to_string_lossy().into_owned()),
            ("ANTHROPIC_DEFAULT_SONNET_MODEL".into(), "wrong".into()),
            ("ANTHROPIC_DEFAULT_OPUS_MODEL".into(), "wrong".into()),
        ]);
        let mut m = Migration::load(root.path(), &env).unwrap();
        assert_eq!(m.target("claude", Some("sonnet[1m]:high"), None).unwrap(), "local-claude-code/custom-sonnet[1m]/variant/high");
        for opusplan in ["opusplan:default", "opusplan[1m]:high"] {
            let before = m.config.clone();
            assert!(m.target("claude", Some(opusplan), None).unwrap_err().contains("opusplan"));
            assert_eq!(m.config, before);
        }
    }

    #[test]
    fn absent_config_is_created_once_without_a_fabricated_backup() {
        let (root, mut m) = setup(None);
        let selection = m.target("codex", Some("model"), None).unwrap();
        m.save().unwrap();
        assert!(!m.changed());
        assert!(!root.path().join("alkaid/config.jsonc.before-backend-migration").exists());
        let mut reloaded = Migration::load(root.path(), &HashMap::new()).unwrap();
        assert_eq!(reloaded.target("codex", Some("model"), None).unwrap(), selection);
        assert!(!reloaded.changed());
    }

    #[test]
    fn auto_sentinels_are_not_models_or_guessed_from_unrelated_cached_options() {
        let (root, mut m) = setup(None);
        for id in ["__codex_auto__", "__codex_auto_fast__", "__codex_auto_high:high", "__cursor_auto__"] {
            assert!(m.target("codex", Some(id), None).is_err());
        }
        put(root.path(), "model-options/codex.json", r#"{"configOptions":[{"id":"model","currentValue":"__codex_auto_fast__","options":[{"value":"unrelated","_meta":{"codex.ai/default":true}}]}]}"#);
        assert!(m.target("codex", None, None).is_err());
        assert!(!m.changed());
        put(root.path(), "model-options/codex.json", r#"{"configOptions":[{"id":"model","currentValue":"actual-model:high"}]}"#);
        assert_eq!(m.target("codex", None, None).unwrap(), "local-codex/actual-model/variant/high");
    }

    #[test]
    fn unsupported_and_unconfigured_targets_fail_without_mutation() {
        let (_root, mut m) = setup(Some(r#"{"provider":{"known":{"api":"openai-completions","models":{"nested/id":{"variants":{"high":{"reasoningEffort":"high"}}}}}}}"#));
        for source in ["", "unsupported", "cursor", "kimi", "future"] {
            assert!(m.target(source, Some("gpt"), None).is_err());
        }
        assert!(m.target("opencode", Some("missing/model"), None).is_err());
        assert!(m.target("codex", None, None).is_err());
        assert!(m.target("codex", Some("gpt"), Some("surprise")).is_err());
        assert_eq!(m.target("opencodeplus", Some("known/nested/id/variant/high"), None).unwrap(), "known/nested/id/variant/high");
        assert!(!m.changed());
    }

    #[test]
    fn saves_backup_original_jsonc_once_and_are_idempotent_and_conflict_safe() {
        let original = "{ // preserve original bytes\n\"provider\": {},\n}\n";
        let (root, mut m) = setup(Some(original));
        let path = root.path().join("alkaid/config.jsonc");
        let backup = root.path().join("alkaid/config.jsonc.before-backend-migration");
        m.save().unwrap();
        assert!(!backup.exists());
        let target = m.target("codex", Some("gpt:high"), None).unwrap();
        m.save().unwrap();
        assert!(!m.changed());
        assert_eq!(fs::read_to_string(&backup).unwrap(), original);
        let written = fs::read(&path).unwrap();
        m.save().unwrap();
        let mut again = Migration::load(root.path(), &HashMap::new()).unwrap();
        assert_eq!(again.target("codex", Some("gpt:high"), None).unwrap(), target);
        assert!(!again.changed());
        again.save().unwrap();
        assert_eq!(fs::read(&path).unwrap(), written);
        m.target("devin", Some("adaptive"), None).unwrap();
        m.save().unwrap();
        assert_eq!(fs::read_to_string(&backup).unwrap(), original);
        m.target("workbuddy", Some("another"), None).unwrap();
        fs::write(&path, b"{\"concurrent\":true}").unwrap();
        assert!(m.save().is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "{\"concurrent\":true}");
    }

    #[test]
    fn invalid_configs_and_blocked_backups_never_overwrite_the_original() {
        let (root, mut m) = setup(Some("{}"));
        m.target("codex", Some("gpt"), None).unwrap();
        fs::create_dir(root.path().join("alkaid/config.jsonc.before-backend-migration")).unwrap();
        assert!(m.save().is_err());
        assert_eq!(fs::read_to_string(root.path().join("alkaid/config.jsonc")).unwrap(), "{}");
        for invalid in ["{bad", "[]", "{\"provider\":null}"] {
            put(root.path(), "alkaid/config.jsonc", invalid);
            assert!(Migration::load(root.path(), &HashMap::new()).is_err());
        }
    }
}
