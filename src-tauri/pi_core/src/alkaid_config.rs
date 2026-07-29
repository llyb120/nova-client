//! Vega/Alkaid provider configuration resolution, ported from
//! `scripts/alkaid-config.mjs` (`parseJsonc`, `mergeAlkaidConfig`, `providerApi`,
//! `resolveEnv`, `resolveAlkaidModel`, `mergeAlkaidCompatDefaults`).
//!
//! All functions are deterministic and golden-tested against the node source.
//! They turn the merged Vega config plus a `provider/model[/variant/level]`
//! selection into a resolved model descriptor (api, baseUrl, apiKey, …) that the
//! native provider transport consumes.

use std::collections::HashMap;
use std::sync::OnceLock;

use serde_json::{json, Map, Value};

/// Port of `stripJsonComments`: remove `//` and `/* */` comments outside
/// strings, preserving newlines.
pub fn strip_json_comments(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut result = String::new();
    let mut in_string = false;
    let mut escaped = false;
    let mut index = 0usize;
    while index < chars.len() {
        let char = chars[index];
        let next = chars.get(index + 1).copied().unwrap_or('\0');
        if in_string {
            result.push(char);
            if escaped {
                escaped = false;
            } else if char == '\\' {
                escaped = true;
            } else if char == '"' {
                in_string = false;
            }
        } else if char == '"' {
            in_string = true;
            result.push(char);
        } else if char == '/' && next == '/' {
            while index < chars.len() && chars[index] != '\n' {
                index += 1;
            }
            result.push('\n');
        } else if char == '/' && next == '*' {
            index += 2;
            while index < chars.len() && !(chars[index] == '*' && chars.get(index + 1) == Some(&'/'))
            {
                if chars[index] == '\n' {
                    result.push('\n');
                }
                index += 1;
            }
            index += 1;
        } else {
            result.push(char);
        }
        index += 1;
    }
    result
}

/// Port of `stripTrailingCommas`: drop commas whose next non-space char is
/// `}` or `]`, outside strings.
pub fn strip_trailing_commas(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut result = String::new();
    let mut in_string = false;
    let mut escaped = false;
    let mut index = 0usize;
    while index < chars.len() {
        let char = chars[index];
        if in_string {
            result.push(char);
            if escaped {
                escaped = false;
            } else if char == '\\' {
                escaped = true;
            } else if char == '"' {
                in_string = false;
            }
            index += 1;
            continue;
        }
        if char == '"' {
            in_string = true;
        }
        if char == ',' {
            let mut next = index + 1;
            while next < chars.len() && chars[next].is_whitespace() {
                next += 1;
            }
            if matches!(chars.get(next), Some('}') | Some(']')) {
                index += 1;
                continue;
            }
        }
        result.push(char);
        index += 1;
    }
    result
}

/// Port of `parseJsonc`.
pub fn parse_jsonc(text: &str) -> Result<Value, String> {
    let stripped = strip_trailing_commas(&strip_json_comments(text));
    serde_json::from_str(&stripped).map_err(|error| error.to_string())
}

fn is_plain_object(value: &Value) -> bool {
    matches!(value, Value::Object(_))
}

/// Port of `mergeAlkaidConfig`: server config as baseline, local overrides
/// recursively; arrays and scalars are replaced wholesale.
pub fn merge_config(server_config: &Value, local_config: &Value) -> Value {
    if !is_plain_object(server_config) {
        return local_config.clone();
    }
    if !is_plain_object(local_config) {
        return server_config.clone();
    }
    let server = server_config.as_object().unwrap();
    let local = local_config.as_object().unwrap();
    let mut merged: Map<String, Value> = server.clone();
    for (key, local_value) in local {
        let existing = merged.get(key);
        let value = match (existing, local_value) {
            (Some(existing), local_value) if is_plain_object(existing) && is_plain_object(local_value) => {
                merge_config(existing, local_value)
            }
            _ => local_value.clone(),
        };
        merged.insert(key.clone(), value);
    }
    Value::Object(merged)
}

/// Port of `resolveEnv`: substitute `{env:NAME}` from the environment, erroring
/// when a referenced variable is absent.
pub fn resolve_env(value: &Value, env: &HashMap<String, String>) -> Result<Value, String> {
    let Some(text) = value.as_str() else {
        return Ok(value.clone());
    };
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"\{env:([A-Za-z_][A-Za-z0-9_]*)\}").unwrap());
    let mut result = String::new();
    let mut last = 0usize;
    for captures in re.captures_iter(text) {
        let whole = captures.get(0).unwrap();
        let name = captures.get(1).unwrap().as_str();
        result.push_str(&text[last..whole.start()]);
        match env.get(name) {
            Some(resolved) => result.push_str(resolved),
            None => return Err(format!("Vega 配置引用的环境变量 {name} 未注入 Nova 进程")),
        }
        last = whole.end();
    }
    result.push_str(&text[last..]);
    Ok(Value::String(result))
}

/// Port of `providerApi`: explicit `api`, else inferred from the `npm` package.
pub fn provider_api(provider: &Value) -> Result<String, String> {
    if let Some(api) = provider.get("api").and_then(Value::as_str) {
        return Ok(api.to_string());
    }
    let npm = provider.get("npm").and_then(Value::as_str).unwrap_or("");
    if npm.contains("anthropic") {
        return Ok("anthropic-messages".to_string());
    }
    if npm.contains("google") {
        return Ok("google-generative-ai".to_string());
    }
    if npm.contains("openai-compatible") {
        return Ok("openai-completions".to_string());
    }
    if npm.contains("openai") {
        return Ok("openai-responses".to_string());
    }
    Err("Vega provider 缺少 api，且无法从 npm 推导协议".to_string())
}

fn regex(pattern: &str) -> &'static regex::Regex {
    // Leak-free caching for the small fixed set of compat patterns.
    Box::leak(Box::new(regex::Regex::new(pattern).unwrap()))
}

/// Port of `mergeAlkaidCompatDefaults`: fill cache/routing compat flags without
/// overriding explicit values.
pub fn merge_compat_defaults(
    api: &str,
    model_id: &str,
    base_url: &str,
    existing: Option<&Value>,
) -> Value {
    let mut compat: Map<String, Value> = match existing {
        Some(Value::Object(obj)) => obj.clone(),
        _ => Map::new(),
    };
    let id = model_id.to_lowercase();
    let url = base_url.to_lowercase();
    let is_official_openai = url.contains("api.openai.com");

    let set_if_absent = |compat: &mut Map<String, Value>, key: &str, value: Value| {
        if !compat.contains_key(key) {
            compat.insert(key.to_string(), value);
        }
    };

    if api == "openai-completions" && !is_official_openai {
        set_if_absent(&mut compat, "sendSessionAffinityHeaders", json!(true));
    }
    if api == "anthropic-messages" && !url.contains("api.anthropic.com") {
        set_if_absent(&mut compat, "sendSessionAffinityHeaders", json!(true));
    }

    static DEEPSEEK: OnceLock<&'static regex::Regex> = OnceLock::new();
    if DEEPSEEK.get_or_init(|| regex(r"\bdeepseek\b")).is_match(&id) {
        set_if_absent(&mut compat, "thinkingFormat", json!("deepseek"));
        set_if_absent(
            &mut compat,
            "requiresReasoningContentOnAssistantMessages",
            json!(true),
        );
    }

    static K3: OnceLock<&'static regex::Regex> = OnceLock::new();
    if K3.get_or_init(|| regex(r"\bk3\b|kimi-for-coding|kimi-k3")).is_match(&id) {
        set_if_absent(&mut compat, "forceAdaptiveThinking", json!(true));
        set_if_absent(&mut compat, "allowEmptySignature", json!(true));
    }

    static CLAUDE: OnceLock<&'static regex::Regex> = OnceLock::new();
    static CLAUDE_ADAPTIVE: OnceLock<&'static regex::Regex> = OnceLock::new();
    let claude = CLAUDE.get_or_init(|| regex("claude"));
    let adaptive = CLAUDE_ADAPTIVE.get_or_init(|| {
        regex(r"opus-4(?:\.|-)6|sonnet-4(?:\.|-)6|sonnet-5|fable-5|claude-sonnet-5")
    });
    if claude.is_match(&id) && adaptive.is_match(&id) {
        set_if_absent(&mut compat, "forceAdaptiveThinking", json!(true));
    }

    if compat.is_empty() {
        existing.cloned().unwrap_or(Value::Null)
    } else {
        Value::Object(compat)
    }
}

/// Port of `resolveAlkaidModel`: resolve a `provider/model[/variant/level]`
/// selection against the merged config into a model descriptor.
pub fn resolve_model(
    config: &Value,
    selection: &str,
    env: &HashMap<String, String>,
) -> Result<Value, String> {
    if !selection.contains('/') {
        return Err("Vega model 必须是 provider/model 格式".to_string());
    }
    let marker = "/variant/";
    let variant_index = selection.rfind(marker);
    let (variant, base_selection) = match variant_index {
        Some(index) => (
            Some(&selection[index + marker.len()..]),
            &selection[..index],
        ),
        None => (None, selection),
    };
    let mut parts = base_selection.splitn(2, '/');
    let provider_id = parts.next().unwrap_or("");
    let model_id = parts.next().unwrap_or("").to_string();

    let provider = config
        .get("provider")
        .and_then(|providers| providers.get(provider_id));
    let Some(provider) = provider else {
        return Err(format!("Vega provider 不存在：{provider_id}"));
    };
    let model = provider.get("models").and_then(|models| models.get(&model_id));
    let Some(model) = model else {
        return Err(format!("Vega model 不存在：{base_selection}"));
    };
    if let Some(variant) = variant {
        let variants = model.get("variants").and_then(Value::as_object);
        if !variants.map_or(false, |variants| variants.contains_key(variant)) {
            return Err(format!("Vega model 不支持思考强度：{selection}"));
        }
    }

    let options = provider.get("options").cloned().unwrap_or(json!({}));
    let base_url_value = options
        .get("baseURL")
        .or_else(|| options.get("baseUrl"))
        .cloned()
        .unwrap_or(Value::Null);
    let base_url = match resolve_env(&base_url_value, env)? {
        Value::String(text) => text,
        _ => String::new(),
    };
    if base_url.is_empty() {
        return Err(format!("Vega provider 缺少 options.baseURL：{provider_id}"));
    }
    let api = provider_api(provider)?;
    let api_key = resolve_env(&options.get("apiKey").cloned().unwrap_or(Value::Null), env)?;

    let thinking_level = if let Some(variant) = variant {
        model
            .get("variants")
            .and_then(|variants| variants.get(variant))
            .and_then(|value| value.get("reasoningEffort"))
            .cloned()
            .unwrap_or_else(|| Value::String(variant.to_string()))
    } else {
        model
            .get("options")
            .and_then(|options| options.get("reasoningEffort"))
            .cloned()
            .unwrap_or(Value::Null)
    };

    let reasoning = model
        .get("reasoning")
        .and_then(Value::as_bool)
        .unwrap_or_else(|| {
            model
                .get("variants")
                .and_then(Value::as_object)
                .map_or(false, |variants| !variants.is_empty())
        });

    let input: Vec<Value> = model
        .get("modalities")
        .and_then(|modalities| modalities.get("input"))
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter(|value| matches!(value.as_str(), Some("text") | Some("image")))
                .cloned()
                .collect()
        })
        .filter(|values: &Vec<Value>| !values.is_empty())
        .unwrap_or_else(|| vec![json!("text")]);

    let cost = model
        .get("cost")
        .cloned()
        .unwrap_or_else(|| json!({ "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 }));
    let context_window = model
        .get("limit")
        .and_then(|limit| limit.get("context"))
        .cloned()
        .unwrap_or(json!(128000));
    let max_tokens = model
        .get("limit")
        .and_then(|limit| limit.get("output"))
        .cloned()
        .unwrap_or(json!(32000));

    let compat_existing = model
        .get("compat")
        .or_else(|| provider.get("compat"));
    let compat = merge_compat_defaults(&api, &model_id, &base_url, compat_existing);

    // thinkingLevelMap: variant level -> reasoningEffort (or null), only when
    // the model declares variants.
    let thinking_level_map = model.get("variants").and_then(Value::as_object).map(|variants| {
        let map: Map<String, Value> = variants
            .iter()
            .map(|(level, value)| {
                let effort = value
                    .get("reasoningEffort")
                    .cloned()
                    .unwrap_or(Value::Null);
                (level.clone(), effort)
            })
            .collect();
        map
    });

    let mut model_descriptor = json!({
        "id": model_id,
        "name": model.get("name").cloned().unwrap_or_else(|| json!(model_id)),
        "api": api,
        "provider": provider_id,
        "baseUrl": base_url,
        "reasoning": reasoning,
        "input": input,
        "cost": cost,
        "contextWindow": context_window,
        "maxTokens": max_tokens,
    });
    if let Some(map) = thinking_level_map {
        if !map.is_empty() {
            model_descriptor["thinkingLevelMap"] = Value::Object(map);
        }
    }
    if let Some(headers) = options.get("headers") {
        model_descriptor["headers"] = headers.clone();
    }
    if !compat.is_null() {
        model_descriptor["compat"] = compat;
    }

    // JSON.stringify drops `undefined`; mirror that by omitting null apiKey /
    // thinkingLevel rather than emitting JSON null.
    let mut result = json!({ "model": model_descriptor });
    if !api_key.is_null() {
        result["apiKey"] = api_key;
    }
    if !thinking_level.is_null() {
        result["thinkingLevel"] = thinking_level;
    }
    Ok(result)
}
