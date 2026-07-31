use serde_json::{json, Map, Value};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone)]
pub struct ResolvedModel {
    pub provider: String,
    pub model: String,
    pub api_key: Option<String>,
    pub thinking: Option<String>,
    pub context_window: u32,
}

pub fn data_root() -> PathBuf {
    let base = std::env::var_os("NOVA_DATA_DIR").map(PathBuf::from).unwrap_or_else(|| {
        dirs::home_dir().unwrap_or_default().join(".nova")
    });
    base.join("alkaid")
}

fn merge(base: &Value, overlay: &Value) -> Value {
    match (base, overlay) {
        (Value::Object(base), Value::Object(overlay)) => {
            let mut result = base.clone();
            for (key, value) in overlay {
                let next = result.get(key).map_or_else(|| value.clone(), |old| merge(old, value));
                result.insert(key.clone(), next);
            }
            Value::Object(result)
        }
        (_, overlay) => overlay.clone(),
    }
}

pub fn load(server: Option<&Value>) -> Result<Value, String> {
    let path = data_root().join("config.jsonc");
    let local = match fs::read_to_string(&path) {
        Ok(text) => json5::from_str(&text).map_err(|e| format!("读取 Vega 配置失败：{e}"))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Value::Object(Map::new()),
        Err(error) => return Err(format!("读取 Vega 配置失败：{error}")),
    };
    if server.is_none() && local.as_object().is_none_or(Map::is_empty) {
        return Err(format!("未找到 Vega 配置：{}", path.display()));
    }
    let config = merge(server.unwrap_or(&Value::Object(Map::new())), &local);
    if !config.get("provider").is_some_and(Value::is_object) {
        return Err("Vega 配置缺少 provider".into());
    }
    Ok(config)
}

fn env_value(value: Option<&Value>) -> Result<Option<String>, String> {
    let Some(value) = value.and_then(Value::as_str) else { return Ok(None) };
    let mut result = value.to_string();
    while let Some(start) = result.find("{env:") {
        let tail = &result[start + 5..];
        let Some(end) = tail.find('}') else { break };
        let name = &tail[..end];
        let resolved = std::env::var(name)
            .map_err(|_| format!("Vega 配置引用的环境变量 {name} 未注入 Nova 进程"))?;
        result.replace_range(start..start + 6 + end, &resolved);
    }
    Ok(Some(result))
}

fn provider_api(provider: &Value) -> Result<String, String> {
    if let Some(api) = provider.get("api").and_then(Value::as_str) { return Ok(api.into()); }
    let npm = provider.get("npm").and_then(Value::as_str).unwrap_or_default();
    if npm.contains("anthropic") { Ok("anthropic-messages".into()) }
    else if npm.contains("google") { Ok("google-generative-ai".into()) }
    else if npm.contains("openai-compatible") { Ok("openai-completions".into()) }
    else if npm.contains("openai") { Ok("openai-responses".into()) }
    else { Err("Vega provider 缺少 api，且无法从 npm 推导协议".into()) }
}

pub fn model_options(config: &Value) -> Result<Vec<Value>, String> {
    let providers = config.get("provider").and_then(Value::as_object).ok_or("Vega 配置缺少 provider")?;
    let mut result = Vec::new();
    for (provider_id, provider) in providers {
        let provider_name = provider.get("name").and_then(Value::as_str).unwrap_or(provider_id);
        let Some(models) = provider.get("models").and_then(Value::as_object) else { continue };
        for (model_id, model) in models {
            let model_name = model.get("name").and_then(Value::as_str).unwrap_or(model_id);
            let images = model.pointer("/modalities/input").and_then(Value::as_array)
                .is_some_and(|items| items.iter().any(|item| item == "image"));
            let variants = model.get("variants").and_then(Value::as_object);
            if variants.is_none_or(Map::is_empty) {
                result.push(json!({"value":format!("{provider_id}/{model_id}"),"name":format!("{provider_name} / {model_name}"),"_meta":{"codex.ai/supportsImages":images}}));
            } else if let Some(variants) = variants {
                for variant in variants.keys() {
                    result.push(json!({"value":format!("{provider_id}/{model_id}/variant/{variant}"),"name":format!("{provider_name} / {model_name} · {variant}"),"_meta":{"codex.ai/supportsImages":images}}));
                }
            }
        }
    }
    Ok(result)
}

pub fn default_model(config: &Value) -> Result<String, String> {
    let options = model_options(config)?;
    let configured = config.get("model").and_then(Value::as_str);
    if let Some(configured) = configured {
        if options.iter().any(|option| option.get("value").and_then(Value::as_str) == Some(configured)) {
            return Ok(configured.into());
        }
    }
    options.first().and_then(|option| option.get("value")).and_then(Value::as_str)
        .map(str::to_string).ok_or_else(|| "Vega 配置没有可用模型".into())
}

pub fn resolve(config: &Value, selection: Option<&str>) -> Result<(ResolvedModel, Value), String> {
    let selection = selection.map(str::to_string).unwrap_or(default_model(config)?);
    let (base, variant) = selection.rsplit_once("/variant/").map_or((selection.as_str(), None), |(a,b)|(a,Some(b)));
    let (provider_id, model_id) = base.split_once('/').ok_or("Vega model 必须是 provider/model 格式")?;
    let provider = config.pointer(&format!("/provider/{}", escape(provider_id))).ok_or_else(|| format!("Vega provider 不存在：{provider_id}"))?;
    let model = provider.pointer(&format!("/models/{}", escape(model_id))).ok_or_else(|| format!("Vega model 不存在：{base}"))?;
    if variant.is_some_and(|name| model.pointer(&format!("/variants/{}", escape(name))).is_none()) {
        return Err(format!("Vega model 不支持思考强度：{selection}"));
    }
    let options = provider.get("options").unwrap_or(&Value::Null);
    let base_url = env_value(options.get("baseURL").or_else(|| options.get("baseUrl")))?.ok_or_else(|| format!("Vega provider 缺少 options.baseURL：{provider_id}"))?;
    let api_key = env_value(options.get("apiKey"))?;
    let api = provider_api(provider)?;
    let thinking = variant.map(str::to_string).or_else(|| model.pointer("/options/reasoningEffort").and_then(Value::as_str).map(str::to_string));
    let context_window = model.pointer("/limit/context").and_then(Value::as_u64).unwrap_or(128_000) as u32;
    let max_tokens = model.pointer("/limit/output").and_then(Value::as_u64).unwrap_or(32_000) as u32;
    let input = model.pointer("/modalities/input").cloned().unwrap_or_else(|| json!(["text"]));
    let registry = json!({"providers":{provider_id:{
        "api":api,"baseUrl":base_url,"headers":options.get("headers"),
        "compat":model.get("compat").or_else(||provider.get("compat")),
        "models":[{"id":model_id,"name":model.get("name").and_then(Value::as_str).unwrap_or(model_id),"contextWindow":context_window,"maxTokens":max_tokens,"reasoning":model.get("reasoning").and_then(Value::as_bool).unwrap_or(variant.is_some()),"input":input,"cost":model.get("cost")}]
    }}});
    Ok((ResolvedModel { provider:provider_id.into(), model:model_id.into(), api_key, thinking, context_window }, registry))
}

fn escape(value: &str) -> String { value.replace('~', "~0").replace('/', "~1") }

pub fn install_registry(root: &Path, registry: &Value) -> Result<(), String> {
    fs::create_dir_all(root).map_err(|e| format!("创建 Rust Pi 配置目录失败：{e}"))?;
    fs::write(root.join("models.json"), serde_json::to_vec(registry).map_err(|e| e.to_string())?)
        .map_err(|e| format!("写入 Rust Pi models.json 失败：{e}"))
}
