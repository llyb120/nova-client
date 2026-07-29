//! Vega 配置加载与模型解析 —— 忠实移植自 `scripts/alkaid-config.mjs`。
//!
//! 行为对齐：JSONC 注释/尾逗号剥离、`{env:NAME}` 解析、服务端配置递归覆盖、
//! provider api 推导、compat 默认填充、模型 variant 解析、模型选项枚举。
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::env;
use std::path::PathBuf;

pub fn alkaid_data_root() -> PathBuf {
    let env_root = env::var_os("NOVA_DATA_DIR");
    match env_root {
        Some(dir) => PathBuf::from(dir).join("alkaid"),
        None => dirs_home().join(".nova").join("alkaid"),
    }
}

fn dirs_home() -> PathBuf {
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Strip `//` and `/* */` comments while preserving string literals.
fn strip_json_comments(text: &str) -> String {
    let bytes: Vec<char> = text.chars().collect();
    let mut result = String::with_capacity(bytes.len());
    let mut in_string = false;
    let mut escaped = false;
    let mut i = 0;
    while i < bytes.len() {
        let ch = bytes[i];
        let next = if i + 1 < bytes.len() { Some(bytes[i + 1]) } else { None };
        if in_string {
            result.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if ch == '"' {
            in_string = true;
            result.push(ch);
            i += 1;
            continue;
        }
        if ch == '/' && next == Some('/') {
            while i < bytes.len() && bytes[i] != '\n' {
                i += 1;
            }
            result.push('\n');
            continue;
        }
        if ch == '/' && next == Some('*') {
            i += 2;
            while i < bytes.len() && !(bytes[i] == '*' && i + 1 < bytes.len() && bytes[i + 1] == '/') {
                if bytes[i] == '\n' {
                    result.push('\n');
                }
                i += 1;
            }
            i += 2; // skip closing `*/`
            continue;
        }
        result.push(ch);
        i += 1;
    }
    result
}

/// Strip trailing commas before `}` or `]` while preserving string literals.
fn strip_trailing_commas(text: &str) -> String {
    let bytes: Vec<char> = text.chars().collect();
    let mut result = String::with_capacity(bytes.len());
    let mut in_string = false;
    let mut escaped = false;
    let mut i = 0;
    while i < bytes.len() {
        let ch = bytes[i];
        if in_string {
            result.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if ch == '"' {
            in_string = true;
        }
        if ch == ',' {
            let mut next = i + 1;
            while next < bytes.len() && is_ws(bytes[next]) {
                next += 1;
            }
            if next < bytes.len() && (bytes[next] == '}' || bytes[next] == ']') {
                i += 1;
                continue;
            }
        }
        result.push(ch);
        i += 1;
    }
    result
}

fn is_ws(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0c' | '\x0b')
}

pub fn parse_jsonc(text: &str) -> Result<Value, String> {
    let stripped = strip_trailing_commas(&strip_json_comments(text));
    serde_json::from_str(&stripped).map_err(|e| e.to_string())
}

fn resolve_env(value: &Value, env: &Map<String, Value>) -> Result<Value, String> {
    let s = match value {
        Value::String(s) => s,
        other => return Ok(other.clone()),
    };
    let re = regex::Regex::new(r"\{env:([A-Za-z_][A-Za-z0-9_]*)\}").unwrap();
    let mut error: Option<String> = None;
    let out = re.replace_all(s, |caps: &regex::Captures| {
        let name = &caps[1];
        match env.get(name).and_then(|v| v.as_str()) {
            Some(v) => v.to_string(),
            None => {
                // Fall back to process environment, matching the JS behavior that reads process.env.
                match std::env::var(name) {
                    Ok(v) => v,
                    Err(_) => {
                        error = Some(format!("Vega 配置引用的环境变量 {name} 未注入 Nova 进程"));
                        String::new()
                    }
                }
            }
        }
    });
    if let Some(err) = error {
        return Err(err);
    }
    Ok(Value::String(out.into()))
}

fn provider_api(provider: &Value, provider_id: &str) -> Result<String, String> {
    if let Some(api) = provider.get("api").and_then(Value::as_str) {
        return Ok(api.to_string());
    }
    let npm = provider.get("npm").and_then(Value::as_str).unwrap_or("");
    let name = provider
        .get("name")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| provider_id.to_string());
    let haystacks = [npm.to_ascii_lowercase(), name.to_ascii_lowercase(), provider_id.to_ascii_lowercase()];
    let contains = |needle: &str| haystacks.iter().any(|h| h.contains(needle));
    if contains("anthropic") {
        return Ok("anthropic-messages".into());
    }
    if contains("google") {
        return Ok("google-generative-ai".into());
    }
    if npm.contains("openai-compatible") {
        return Ok("openai-completions".into());
    }
    if contains("openai") {
        return Ok("openai-responses".into());
    }
    Err("Vega provider 缺少 api，且无法从 npm 推导协议".into())
}

fn is_plain_object(v: &Value) -> bool {
    matches!(v, Value::Object(_))
}

/// 服务端配置作基线，本地配置递归覆盖；数组与标量由本地整体替换。
pub fn merge_alkaid_config(server: Option<&Value>, local: &Value) -> Value {
    let server = match server {
        Some(s) if is_plain_object(s) => s.clone(),
        _ => return local.clone(),
    };
    if !is_plain_object(local) {
        return server;
    }
    let mut merged = server;
    if let (Some(merged_obj), Some(local_obj)) = (merged.as_object_mut(), local.as_object()) {
        for (key, local_value) in local_obj {
            let use_merge = is_plain_object(local_value)
                && merged_obj.get(key).map(is_plain_object).unwrap_or(false);
            let next = if use_merge {
                merge_alkaid_config(merged_obj.get(key), local_value)
            } else {
                local_value.clone()
            };
            merged_obj.insert(key.clone(), next);
        }
    }
    merged
}

pub struct LoadedConfig {
    pub root: PathBuf,
    pub env: Map<String, Value>,
    pub value: Value,
}

pub fn load_alkaid_config(
    root: Option<PathBuf>,
    server_config: Option<&Value>,
) -> Result<LoadedConfig, String> {
    let root = root.unwrap_or_else(alkaid_data_root);
    let path = root.join("config.jsonc");
    let local_config = match std::fs::read_to_string(&path) {
        Ok(text) => Some(parse_jsonc(&text).map_err(|e| format!("读取 Vega 配置失败：{e}"))?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("读取 Vega 配置失败：{e}")),
    };
    let local = local_config.unwrap_or(Value::Null);
    if !is_plain_object(&local) && !is_plain_object(&server_config.unwrap_or(&Value::Null)) {
        return Err(format!("未找到 Vega 配置：{}", path.display()));
    }
    let mut config = merge_alkaid_config(server_config, &local);
    if config.get("provider").and_then(|v| v.as_object()).is_none() {
        return Err("Vega 配置缺少 provider".into());
    }
    // Attach env map (process env merged) for downstream {env:} resolution.
    let mut env_map = Map::new();
    for (k, v) in std::env::vars() {
        env_map.insert(k, Value::String(v));
    }
    if let Some(obj) = config.as_object_mut() {
        obj.entry("root").or_insert_with(|| Value::String(root.to_string_lossy().into()));
    }
    let _ = root;
    Ok(LoadedConfig { root, env: env_map, value: config })
}

pub fn default_alkaid_model(config: &Value) -> Result<String, String> {
    let options = alkaid_model_options(config);
    let mut selection = config.get("model").and_then(Value::as_str).map(str::to_string);
    if let Some(sel) = &selection {
        if !options.iter().any(|o| &o.value == sel) {
            let (provider_id, model_parts) = split_provider_model(sel);
            let model_id = model_parts.join("/");
            let effort = config
                .get("provider")
                .and_then(|p| p.get(provider_id))
                .and_then(|p| p.get("models"))
                .and_then(|m| m.get(&model_id))
                .and_then(|m| m.get("options"))
                .and_then(|o| o.get("reasoningEffort"))
                .and_then(Value::as_str)
                .map(str::to_string);
            // Match Node: selection = options.find(value === `${model}/variant/${effort}`)?.value
            // When effort is absent or the candidate isn't found, selection becomes None
            // and falls through to options[0].
            selection = effort
                .and_then(|e| {
                    let candidate = format!("{sel}/variant/{e}");
                    options.iter().find(|o| o.value == candidate).map(|o| o.value.clone())
                });
        }
    }
    if selection.is_none() {
        selection = options.first().map(|o| o.value.clone());
    }
    match selection {
        Some(s) if !s.is_empty() => Ok(s),
        _ => Err("Vega 配置没有可用模型".into()),
    }
}

fn split_provider_model(sel: &str) -> (&str, Vec<&str>) {
    let mut parts = sel.split('/');
    let provider = parts.next().unwrap_or("");
    let model_parts: Vec<&str> = parts.collect();
    (provider, model_parts)
}

pub fn merge_alkaid_compat_defaults(
    api: &str,
    model_id: &str,
    base_url: &str,
    existing: Option<&Value>,
) -> Value {
    let compat = match existing {
        Some(Value::Object(map)) => map.clone(),
        _ => Map::new(),
    };
    let id = model_id.to_lowercase();
    let url = base_url.to_lowercase();
    let is_official_openai = url.contains("api.openai.com");
    let mut compat = compat;

    let set_if = |compat: &mut Map<String, Value>, key: &str, val: Value| {
        if !compat.contains_key(key) {
            compat.insert(key.to_string(), val);
        }
    };

    if api == "openai-completions" && !is_official_openai {
        set_if(&mut compat, "sendSessionAffinityHeaders", Value::Bool(true));
    }
    if api == "anthropic-messages" && !url.contains("api.anthropic.com") {
        set_if(&mut compat, "sendSessionAffinityHeaders", Value::Bool(true));
    }

    if regex::Regex::new(r"\bdeepseek\b").unwrap().is_match(&id) {
        set_if(&mut compat, "thinkingFormat", Value::String("deepseek".into()));
        set_if(&mut compat, "requiresReasoningContentOnAssistantMessages", Value::Bool(true));
    }

    if regex::Regex::new(r"\bk3\b|kimi-for-coding|kimi-k3").unwrap().is_match(&id) {
        set_if(&mut compat, "forceAdaptiveThinking", Value::Bool(true));
        set_if(&mut compat, "allowEmptySignature", Value::Bool(true));
    }

    let claude_adaptive = regex::Regex::new(r"claude").unwrap().is_match(&id)
        && (regex::Regex::new(r"opus-4(?:\.|-)6").unwrap().is_match(&id)
            || regex::Regex::new(r"sonnet-4(?:\.|-)6").unwrap().is_match(&id)
            || regex::Regex::new(r"sonnet-5").unwrap().is_match(&id)
            || regex::Regex::new(r"fable-5").unwrap().is_match(&id)
            || regex::Regex::new(r"claude-sonnet-5").unwrap().is_match(&id));
    if claude_adaptive {
        set_if(&mut compat, "forceAdaptiveThinking", Value::Bool(true));
    }

    if compat.is_empty() {
        existing.cloned().unwrap_or(Value::Null)
    } else {
        Value::Object(compat)
    }
}

pub struct ResolvedModel {
    pub api_key: Option<String>,
    pub thinking_level: Option<String>,
    pub model: ModelInfo,
}

#[derive(Clone)]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    pub api: String,
    pub provider: String,
    pub base_url: String,
    pub reasoning: bool,
    pub thinking_level_map: Option<BTreeMap<String, Option<String>>>,
    pub input: Vec<String>,
    pub cost: Value,
    pub context_window: u64,
    pub max_tokens: u64,
    pub headers: Option<Value>,
    pub compat: Value,
}

pub fn resolve_alkaid_model(config: &Value, selection: Option<&str>) -> Result<ResolvedModel, String> {
    let selection = match selection {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => default_alkaid_model(config)?,
    };
    if !selection.contains('/') {
        return Err("Vega model 必须是 provider/model 格式".into());
    }
    let marker = "/variant/";
    let (variant, base_selection) = match selection.rfind(marker) {
        Some(idx) => (Some(selection[idx + marker.len()..].to_string()), selection[..idx].to_string()),
        None => (None, selection.clone()),
    };
    let (provider_id, model_parts) = split_provider_model(&base_selection);
    let model_id = model_parts.join("/");
    let provider = config.get("provider").and_then(|p| p.get(provider_id)).ok_or_else(|| format!("Vega provider 不存在：{provider_id}"))?;
    let model = provider.get("models").and_then(|m| m.get(&model_id)).ok_or_else(|| format!("Vega model 不存在：{base_selection}"))?;
    if let Some(v) = &variant {
        let has = model.get("variants").and_then(|v| v.as_object()).is_some_and(|o| o.contains_key(v));
        if !has {
            return Err(format!("Vega model 不支持思考强度：{selection}"));
        }
    }
    let options = provider.get("options").cloned().unwrap_or(Value::Object(Map::new()));
    let env_map = config.get("env").and_then(Value::as_object).cloned().unwrap_or_default();
    let env_map_ref: Map<String, Value> = env_map;
    let base_url = {
        let raw = options.get("baseURL").or_else(|| options.get("baseUrl")).cloned().unwrap_or(Value::Null);
        resolve_env(&raw, &env_map_ref)?.as_str().ok_or_else(|| format!("Vega provider 缺少 options.baseURL：{provider_id}"))?.to_string()
    };
    if base_url.is_empty() {
        return Err(format!("Vega provider 缺少 options.baseURL：{provider_id}"));
    }
    let api_key = {
        let raw = options.get("apiKey").cloned().unwrap_or(Value::Null);
        resolve_env(&raw, &env_map_ref)?.as_str().map(str::to_string)
    };
    let variants_map = model.get("variants").and_then(|v| v.as_object());
    let thinking_level = if let Some(v) = &variant {
        variants_map
            .and_then(|m| m.get(v))
            .and_then(|v| v.get("reasoningEffort"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| Some(v.clone()))
    } else {
        model.get("options").and_then(|o| o.get("reasoningEffort")).and_then(Value::as_str).map(str::to_string)
    };
    let thinking_level_map: Option<BTreeMap<String, Option<String>>> = variants_map.map(|m| {
        m.iter()
            .map(|(k, v)| (k.clone(), v.get("reasoningEffort").and_then(Value::as_str).map(str::to_string)))
            .collect()
    });
    let api = provider_api(provider, provider_id)?;
    let input = model.get("modalities").and_then(|m| m.get("input")).and_then(Value::as_array).map(|arr| {
        arr.iter().filter_map(|v| v.as_str().filter(|s| *s == "text" || *s == "image").map(str::to_string)).collect()
    }).unwrap_or_else(|| vec!["text".into()]);
    let cost = model.get("cost").cloned().unwrap_or_else(|| serde_json::json!({ "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 }));
    let context_window = model.get("limit").and_then(|l| l.get("context")).and_then(Value::as_u64).unwrap_or(128_000);
    let max_tokens = model.get("limit").and_then(|l| l.get("output")).and_then(Value::as_u64).unwrap_or(32_000);
    let headers = options.get("headers").cloned();
    let compat_existing = model.get("compat").or_else(|| provider.get("compat"));
    let compat = merge_alkaid_compat_defaults(&api, &model_id, &base_url, compat_existing);
    let reasoning = model.get("reasoning").and_then(Value::as_bool).unwrap_or_else(|| variants_map.is_some_and(|m| !m.is_empty()));
    let name = model.get("name").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| model_id.clone());
    Ok(ResolvedModel {
        api_key,
        thinking_level,
        model: ModelInfo { id: model_id, name, api, provider: provider_id.to_string(), base_url, reasoning, thinking_level_map, input, cost, context_window, max_tokens, headers, compat },
    })
}

pub struct ModelOption {
    pub value: String,
    pub name: String,
    pub meta: Value,
}

fn variant_label(v: &str) -> String {
    match v {
        "minimal" => "Minimal", "low" => "Low", "medium" => "Medium", "high" => "High", "xhigh" => "XHigh", "max" => "Max",
        other => other,
    }.into()
}

pub fn alkaid_model_options(config: &Value) -> Vec<ModelOption> {
    let mut out = Vec::new();
    let providers = config.get("provider").and_then(Value::as_object).cloned().unwrap_or_default();
    // serde_json::Map preserves insertion order when the "preserve_order" feature is enabled,
    // matching JS Object.entries order. Iterate keys directly without sorting.
    let provider_ids: Vec<String> = providers.keys().cloned().collect();
    for provider_id in provider_ids {
        let provider = &providers[&provider_id];
        let models = provider.get("models").and_then(Value::as_object).cloned().unwrap_or_default();
        let model_ids: Vec<String> = models.keys().cloned().collect();
        for model_id in model_ids {
            let model = &models[&model_id];
            let value = format!("{provider_id}/{model_id}");
            let provider_name = provider.get("name").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| provider_id.clone());
            let model_name = model.get("name").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| model_id.clone());
            let name = format!("{provider_name} / {model_name}");
            let supports_images = model.get("modalities").and_then(|m| m.get("input")).and_then(Value::as_array).is_some_and(|arr| arr.iter().any(|v| v.as_str() == Some("image")));
            let meta = serde_json::json!({ "codex.ai/supportsImages": supports_images });
            let variants = model.get("variants").and_then(Value::as_object).cloned().unwrap_or_default();
            if variants.is_empty() {
                out.push(ModelOption { value, name, meta });
            } else {
                let variant_keys: Vec<String> = variants.keys().cloned().collect();
                for variant in variant_keys {
                    out.push(ModelOption {
                        value: format!("{value}/variant/{variant}"),
                        name: format!("{name} · {}", variant_label(&variant)),
                        meta: meta.clone(),
                    });
                }
            }
        }
    }
    out
}

pub fn model_options_json(config: &Value) -> Value {
    let options = alkaid_model_options(config);
    let default = default_alkaid_model(config).unwrap_or_default();
    let arr: Vec<Value> = options
        .iter()
        .map(|o| serde_json::json!({ "value": o.value, "name": o.name, "_meta": o.meta }))
        .collect();
    serde_json::json!({
        "configOptions": [{ "id": "model", "name": "Model", "currentValue": default, "options": arr }],
        "modes": null
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jsonc_strips_comments_and_trailing_commas() {
        let text = r#"{
            // a comment
            "a": 1,
            /* block */
            "b": 2,
            "c": [1, 2,],
        }"#;
        let v = parse_jsonc(text).unwrap();
        assert_eq!(v["a"], 1);
        assert_eq!(v["b"], 2);
        assert_eq!(v["c"][1], 2);
    }

    #[test]
    fn resolve_model_with_variant() {
        let config = serde_json::json!({
            "model": "openai/o3",
            "provider": {
                "openai": {
                    "options": { "baseURL": "https://api.openai.com/v1", "apiKey": "k" },
                    "models": {
                        "o3": { "variants": { "low": { "reasoningEffort": "low" }, "high": { "reasoningEffort": "high" } } }
                    }
                }
            }
        });
        let r = resolve_alkaid_model(&config, Some("openai/o3/variant/high")).unwrap();
        assert_eq!(r.model.id, "o3");
        assert_eq!(r.thinking_level.as_deref(), Some("high"));
        assert_eq!(r.model.api, "openai-responses");
    }

    #[test]
    fn compat_defaults_for_third_party_openai_completions() {
        let compat = merge_alkaid_compat_defaults("openai-completions", "gpt-4o", "https://proxy.example.com/v1", None);
        assert_eq!(compat["sendSessionAffinityHeaders"], Value::Bool(true));
    }
}
