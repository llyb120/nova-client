//! 将本地 CLI 的 API 设置导入为普通 Lyra provider；不读取订阅登录 token 或执行密钥脚本。
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

fn text(value: &Value) -> Option<&str> {
    value.as_str().map(str::trim).filter(|s| !s.is_empty())
}

fn env_value<'a>(env: &'a HashMap<String, String>, key: &str) -> Option<&'a str> {
    env.get(key).map(String::as_str).map(str::trim).filter(|s| !s.is_empty())
}

fn root(env: &HashMap<String, String>, variable: &str, folder: &str) -> Result<PathBuf, String> {
    if let Some(path) = env_value(env, variable) {
        return Ok(PathBuf::from(path));
    }
    env_value(env, "USERPROFILE").or_else(|| env_value(env, "HOME"))
        .map(|home| PathBuf::from(home).join(folder)).ok_or_else(|| "无法确定用户目录".into())
}

fn read(path: &Path) -> Result<Option<String>, String> {
    match std::fs::read_to_string(path) {
        Ok(value) => Ok(Some(value)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(format!("无法读取 {}", path.display())),
    }
}

fn read_json(path: &Path) -> Result<Value, String> {
    read(path)?.map(|raw| crate::lyra_complete::parse_jsonc(&raw)
        .map_err(|_| format!("{} 不是有效的 JSON 配置", path.display()))
        .and_then(|value| if value.is_object() { Ok(value) } else { Err(format!("{} 必须是 JSON 对象", path.display())) }))
        .unwrap_or_else(|| Ok(json!({})))
}

fn add_model(provider: &mut Value, id: &str, effort: Option<&str>) {
    if id.is_empty() { return; }
    let mut model = json!({});
    if let Some(effort) = effort {
        model["reasoning"] = json!(true);
        model["options"] = json!({ "reasoningEffort": effort });
        model["variants"] = json!({ effort: { "reasoningEffort": effort } });
    }
    provider["models"][id] = model;
}

pub(super) fn load(source: &str, env: &HashMap<String, String>) -> Result<(Value, String), String> {
    match source {
        "codex" => codex(env),
        "claude-code" => claude(env),
        _ => Err("不支持的本地配置来源".into()),
    }
}

fn codex(env: &HashMap<String, String>) -> Result<(Value, String), String> {
    let root = root(env, "CODEX_HOME", ".codex")?;
    let path = root.join("config.toml");
    let mut config = match read(&path)? {
        Some(raw) => {
            // TOML 错误可能包含原始配置行，不把密钥所在行带入 UI/日志。
            let value: toml::Value = toml::from_str(&raw).map_err(|_| format!("{} 不是有效的 TOML 配置", path.display()))?;
            serde_json::to_value(value).map_err(|_| "无法解析 Codex 配置".to_string())?
        }
        None => json!({}),
    };
    if let Some(profile) = text(&config["profile"]) {
        let values = config["profiles"][profile].as_object().cloned()
            .ok_or_else(|| format!("Codex profile 不存在：{profile}"))?;
        config.as_object_mut().ok_or("Codex 配置必须是对象")?.extend(values);
    }
    let provider_id = text(&config["model_provider"]).unwrap_or("openai");
    let settings = &config["model_providers"][provider_id];
    if provider_id != "openai" && !settings.is_object() {
        return Err(format!("Codex model_provider 不存在：{provider_id}"));
    }
    let api = match text(&settings["wire_api"]).unwrap_or("responses") {
        "responses" => "openai-responses",
        "chat" => "openai-completions",
        _ => return Err("Codex provider 使用了不支持的 wire_api".into()),
    };
    let base = text(&settings["base_url"])
        .or_else(|| (provider_id == "openai").then(|| env_value(env, "OPENAI_BASE_URL").unwrap_or("https://api.openai.com/v1")))
        .ok_or("Codex 自定义 provider 缺少 base_url")?;
    if settings["query_params"].as_object().is_some_and(|q| !q.is_empty()) {
        // ponytail: Lyra URL 拼接尚不支持查询参数；支持 Azure 等端点时先统一 URL 构造。
        return Err("该 Codex provider 配置了 query_params，请在 Lyra 中手动配置兼容端点".into());
    }
    let mut headers = settings["http_headers"].as_object().cloned().unwrap_or_default();
    if let Some(names) = settings["env_http_headers"].as_object() {
        for (header, variable) in names {
            let variable = text(variable).ok_or("Codex env_http_headers 必须填写环境变量名")?;
            let value = env_value(env, variable).ok_or_else(|| format!("Codex 缺少环境变量 {variable}"))?;
            headers.insert(header.clone(), json!(value));
        }
    }
    let key = if let Some(variable) = text(&settings["env_key"]) {
        env_value(env, variable).ok_or_else(|| format!("Codex 缺少环境变量 {variable}"))?.to_string()
    } else if let Some(token) = text(&settings["experimental_bearer_token"]) {
        token.to_string()
    } else {
        let auth = read_json(&root.join("auth.json"))?;
        text(&auth["OPENAI_API_KEY"]).or_else(|| env_value(env, "OPENAI_API_KEY")).unwrap_or("").to_string()
    };
    if key.is_empty() && !headers.keys().any(|name| name.eq_ignore_ascii_case("authorization") || name.eq_ignore_ascii_case("api-key")) {
        return Err("本地 Codex 未配置 API Key；订阅登录不能作为 Lyra API provider 导入".into());
    }
    let model = text(&config["model"]).unwrap_or("").to_string();
    let mut provider = json!({ "name": "本地 Codex", "api": api,
        "options": { "baseURL": base, "apiKey": key, "headers": headers }, "models": {} });
    add_model(&mut provider, &model, text(&config["model_reasoning_effort"]));
    Ok((provider, model))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_profile_credentials_headers_and_effort_are_imported() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), r#"
model = "old"
profile = "work"
[profiles.work]
model = "gpt-custom"
model_provider = "proxy"
model_reasoning_effort = "high"
[model_providers.proxy]
base_url = "https://proxy.example/v1"
wire_api = "chat"
env_key = "PROXY_KEY"
http_headers = { "X-Client" = "codex" }
env_http_headers = { "X-Workspace" = "WORKSPACE_ID" }
"#).unwrap();
        let mut env = HashMap::from([
            ("CODEX_HOME".into(), dir.path().to_string_lossy().into_owned()),
            ("PROXY_KEY".into(), "custom-key".into()),
            ("WORKSPACE_ID".into(), "workspace".into()),
            ("OPENAI_API_KEY".into(), "wrong-key".into()),
        ]);
        let (provider, model) = load("codex", &env).unwrap();
        assert_eq!(model, "gpt-custom");
        assert_eq!(provider["api"], "openai-completions");
        assert_eq!(provider["options"]["apiKey"], "custom-key");
        assert_eq!(provider["options"]["headers"]["X-Workspace"], "workspace");
        let config = json!({ "model": "p/gpt-custom", "provider": { "p": provider } });
        let resolved = crate::lyra::config::resolve_model(&config, None, &env).unwrap();
        assert_eq!(resolved.thinking_level.as_deref(), Some("high"));
        assert_eq!(resolved.model.api, "openai-completions");
        env.remove("PROXY_KEY");
        assert!(load("codex", &env).unwrap_err().contains("PROXY_KEY"));
    }

    #[test]
    fn codex_api_login_is_supported_but_subscription_tokens_are_not_imported() {
        let dir = tempfile::tempdir().unwrap();
        let env = HashMap::from([("CODEX_HOME".into(), dir.path().to_string_lossy().into_owned())]);
        std::fs::write(dir.path().join("config.toml"), "model = 'test-model'").unwrap();
        let auth = dir.path().join("auth.json");
        std::fs::write(&auth, r#"{"OPENAI_API_KEY":"api-key"}"#).unwrap();
        let (provider, _) = load("codex", &env).unwrap();
        assert_eq!(provider["api"], "openai-responses");
        assert_eq!(provider["options"]["baseURL"], "https://api.openai.com/v1");
        assert_eq!(provider["options"]["apiKey"], "api-key");
        std::fs::write(&auth, r#"{"tokens":{"access_token":"subscription-secret"}}"#).unwrap();
        let error = load("codex", &env).unwrap_err();
        assert!(error.contains("API Key") && !error.contains("subscription-secret"));
        std::fs::write(dir.path().join("config.toml"), "api_key = 'secret\n").unwrap();
        assert!(!load("codex", &env).unwrap_err().contains("secret"));
    }

    #[test]
    fn claude_settings_override_shell_and_preserve_auth_type_and_model_aliases() {
        let dir = tempfile::tempdir().unwrap();
        let env = HashMap::from([
            ("CLAUDE_CONFIG_DIR".into(), dir.path().to_string_lossy().into_owned()),
            ("ANTHROPIC_AUTH_TOKEN".into(), "shell-token".into()),
            ("ANTHROPIC_API_KEY".into(), "shell-key".into()),
        ]);
        let path = dir.path().join("settings.json");
        let mut settings = json!({ "model": "sonnet", "effortLevel": "high", "env": {
            "ANTHROPIC_BASE_URL": "https://gateway.example/anthropic",
            "ANTHROPIC_API_KEY": "settings-key", "ANTHROPIC_AUTH_TOKEN": "",
            "ANTHROPIC_DEFAULT_SONNET_MODEL": "claude-custom",
            "ANTHROPIC_DEFAULT_HAIKU_MODEL": "claude-small",
            "ANTHROPIC_CUSTOM_HEADERS": "X-Client: local\nX-Route: a:b"
        } });
        std::fs::write(&path, settings.to_string()).unwrap();
        let (provider, model) = load("claude-code", &env).unwrap();
        assert_eq!(model, "claude-custom");
        assert_eq!(provider["api"], "anthropic-messages");
        assert_eq!(provider["options"]["apiKey"], "settings-key");
        assert!(provider["options"]["headers"].get("Authorization").is_none());
        assert_eq!(provider["options"]["headers"]["X-Route"], "a:b");
        assert_eq!(provider["models"]["claude-custom"]["variants"]["high"]["reasoningEffort"], "high");
        assert!(provider["models"].get("claude-small").is_some());
        settings["env"]["ANTHROPIC_AUTH_TOKEN"] = json!("settings-token");
        std::fs::write(&path, settings.to_string()).unwrap();
        let (provider, _) = load("claude-code", &env).unwrap();
        assert_eq!(provider["options"]["apiKey"], "");
        assert_eq!(provider["options"]["headers"]["Authorization"], "Bearer settings-token");
        settings["env"]["CLAUDE_CODE_USE_VERTEX"] = json!("1");
        std::fs::write(&path, settings.to_string()).unwrap();
        assert!(load("claude-code", &env).is_err());
        std::fs::write(&path, r#"{"env":{"CLAUDE_CODE_OAUTH_TOKEN":"subscription-secret"}}"#).unwrap();
        let env = HashMap::from([("CLAUDE_CONFIG_DIR".into(), dir.path().to_string_lossy().into_owned())]);
        assert!(load("claude-code", &env).is_err());
        assert!(load("unknown", &env).is_err());
    }
}

fn claude(env: &HashMap<String, String>) -> Result<(Value, String), String> {
    let settings = read_json(&root(env, "CLAUDE_CONFIG_DIR", ".claude")?.join("settings.json"))?;
    let mut env = env.clone();
    // Claude Code 用户 settings.env 覆盖 shell，空字符串也会取消 shell 中的值。
    if let Some(values) = settings["env"].as_object() {
        for (name, value) in values {
            if let Some(value) = value.as_str() { env.insert(name.clone(), value.to_string()); }
        }
    }
    let get = |key| env_value(&env, key);
    if ["CLAUDE_CODE_USE_BEDROCK", "CLAUDE_CODE_USE_VERTEX", "CLAUDE_CODE_USE_FOUNDRY"]
        .iter().any(|key| get(key).is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))) {
        return Err("本地 Claude Code 使用云平台认证，请配置 Anthropic 兼容 API 后导入".into());
    }
    let mut headers = Map::new();
    if let Some(custom) = get("ANTHROPIC_CUSTOM_HEADERS") {
        for line in custom.lines().filter(|line| !line.trim().is_empty()) {
            let (name, value) = line.split_once(':').ok_or("ANTHROPIC_CUSTOM_HEADERS 格式应为每行 Header: value")?;
            headers.insert(name.trim().to_string(), json!(value.trim()));
        }
    }
    let key = if let Some(token) = get("ANTHROPIC_AUTH_TOKEN") {
        headers.insert("Authorization".into(), json!(format!("Bearer {token}")));
        ""
    } else {
        get("ANTHROPIC_API_KEY").ok_or("本地 Claude Code 未配置 API Key / AUTH_TOKEN；订阅登录不能作为 Lyra API provider 导入")?
    };
    let mut provider = json!({ "name": "本地 Claude Code", "api": "anthropic-messages",
        "options": { "baseURL": get("ANTHROPIC_BASE_URL").unwrap_or("https://api.anthropic.com"),
            "apiKey": key, "headers": headers, "claudeCodeClient": true }, "models": {} });
    let selected = get("ANTHROPIC_MODEL").or_else(|| text(&settings["model"]))
        .or_else(|| get("ANTHROPIC_DEFAULT_MODEL")).unwrap_or("sonnet");
    let alias = match selected {
        "sonnet" => Some("ANTHROPIC_DEFAULT_SONNET_MODEL"),
        "opus" | "opusplan" => Some("ANTHROPIC_DEFAULT_OPUS_MODEL"),
        "haiku" => Some("ANTHROPIC_DEFAULT_HAIKU_MODEL"),
        _ => None,
    };
    let model = alias.map(|key| get(key).unwrap_or("")).unwrap_or(selected).to_string();
    for variable in ["ANTHROPIC_DEFAULT_SONNET_MODEL", "ANTHROPIC_DEFAULT_OPUS_MODEL", "ANTHROPIC_DEFAULT_HAIKU_MODEL"] {
        if let Some(id) = get(variable) { add_model(&mut provider, id, None); }
    }
    add_model(&mut provider, &model, get("CLAUDE_CODE_EFFORT_LEVEL").or_else(|| text(&settings["effortLevel"])));
    Ok((provider, model))
}
