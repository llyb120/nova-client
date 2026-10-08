//! 自适应 provider：config.jsonc 里只写 `"preset"` 与 apiKey，Base URL、模型列表与
//! 每个模型的协议自动获得。模型列表拉取后缓存到 alkaid/models-cache.json，
//! load_config 时合并进 provider.models（手写的同名模型覆盖缓存），下游解析无需感知。

use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[path = "local_provider.rs"]
mod local_provider;

/// 导入为普通 provider，后续由现有设置编辑/保存；不把本机配置引用写入可共享配置。
pub(crate) async fn import_local(http: &reqwest::Client, source: &str) -> Result<Value, String> {
    let (provider, selected) = local_provider::load(source, &super::config::process_env())?;
    let explicit = provider["models"].as_object().cloned().unwrap_or_default();
    let base = provider["options"]["baseURL"].as_str().unwrap_or("").trim_end_matches('/');
    let url = reqwest::Url::parse(base).map_err(|_| "本地 API 的 Base URL 无效")?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("本地 API 的 Base URL 必须使用 HTTP 或 HTTPS".into());
    }
    let anthropic = provider["api"] == "anthropic-messages";
    let models_url = if anthropic && !base.ends_with("/v1") { format!("{base}/v1/models") } else { format!("{base}/models") };
    let mut request = http.get(models_url).timeout(Duration::from_secs(30));
    let key = provider["options"]["apiKey"].as_str().unwrap_or("");
    if !key.is_empty() {
        request = if anthropic { request.header("x-api-key", key) } else { request.bearer_auth(key) };
    }
    if anthropic { request = request.header("anthropic-version", "2023-06-01"); }
    if let Some(headers) = provider["options"]["headers"].as_object() {
        for (name, value) in headers {
            let value = value.as_str().ok_or("本地 API 请求头必须是字符串")?;
            let name = reqwest::header::HeaderName::from_bytes(name.as_bytes()).map_err(|_| "本地 API 请求头名称无效")?;
            let value = reqwest::header::HeaderValue::from_str(value).map_err(|_| "本地 API 请求头值无效")?;
            request = request.header(name, value);
        }
    }
    let (list, catalog) = tokio::join!(async {
        // 优先导入用户已配置的模型；未指定时才查询端点，兼容不提供 /models 的网关。
        if !explicit.is_empty() { return Some(json!({ "data": [] })); }
        request.send().await.ok()?.error_for_status().ok()?.json::<Value>().await.ok()
    }, get_json(http, "https://models.dev/api.json", ""));
    convert_local(provider, &selected, &list.unwrap_or(Value::Null), catalog.as_ref().ok())
}

fn convert_local(mut provider: Value, selected: &str, list: &Value, catalog: Option<&Value>) -> Result<Value, String> {
    let explicit = provider["models"].as_object().cloned().unwrap_or_default();
    let mut entries = list["data"].as_array().cloned().unwrap_or_default();
    for id in explicit.keys() {
        if !entries.iter().any(|entry| entry["id"] == id.as_str()) { entries.push(json!({ "id": id })); }
    }
    let mut models = convert(&json!({ "data": entries }), None, catalog);
    for (id, local) in explicit {
        let model = models.entry(id).or_insert_with(|| json!({}));
        for field in ["options", "variants"] {
            if let Some(values) = local[field].as_object() {
                let mut merged = model[field].as_object().cloned().unwrap_or_default();
                merged.extend(values.clone());
                model[field] = json!(merged);
            }
        }
        if let Some(reasoning) = local.get("reasoning") { model["reasoning"] = reasoning.clone(); }
    }
    if models.is_empty() {
        return Err("本地配置未指定模型，且 API 未返回可用模型；请在本地设置中指定完整模型 ID 后重试".into());
    }
    // 本地 CLI 明确选择了协议，所有导入模型沿用该协议。
    for model in models.values_mut() { model.as_object_mut().unwrap().remove("api"); }
    provider["models"] = json!(models);
    let config = json!({ "model": format!("local/{selected}"), "provider": { "local": provider } });
    let model = super::config::default_model(&config)?;
    Ok(json!({ "provider": config["provider"]["local"], "model": model.strip_prefix("local/").unwrap() }))
}

struct Preset {
    id: &'static str,
    name: &'static str,
    base_url: &'static str,
    /// id 同时是 models.dev 的 provider id：多数厂商的 /models 不带协议、上下文与思考能力，靠它补齐。
    models_dev: bool,
}

const fn preset(id: &'static str, name: &'static str, base_url: &'static str, models_dev: bool) -> Preset {
    Preset { id, name, base_url, models_dev }
}

/// Base URL 取自 models.dev 各 provider 的 api 字段，离线时也能直接用。
const PRESETS: &[Preset] = &[
    preset("commandcode", "Command Code", "https://api.commandcode.ai/provider/v1", false),
    preset("opencode", "OpenCode Zen", "https://opencode.ai/zen/v1", true),
    preset("opencode-go", "OpenCode Go", "https://opencode.ai/zen/go/v1", true),
    preset("deepseek", "DeepSeek", "https://api.deepseek.com", true),
    preset("zhipuai", "智谱 GLM", "https://open.bigmodel.cn/api/paas/v4", true),
    preset("zhipuai-coding-plan", "智谱 GLM Coding Plan", "https://open.bigmodel.cn/api/coding/paas/v4", true),
    preset("zai", "Z.AI", "https://api.z.ai/api/paas/v4", true),
    preset("zai-coding-plan", "Z.AI Coding Plan", "https://api.z.ai/api/coding/paas/v4", true),
    preset("moonshotai-cn", "Kimi（Moonshot）", "https://api.moonshot.cn/v1", true),
    preset("moonshotai", "Kimi（Moonshot 国际）", "https://api.moonshot.ai/v1", true),
    preset("kimi-code-plan-cn", "Kimi For Coding", "https://api.kimi.com/coding/v1", true),
    preset("alibaba-cn", "阿里云百炼", "https://dashscope.aliyuncs.com/compatible-mode/v1", true),
    preset("alibaba-coding-plan-cn", "阿里云百炼 Coding Plan", "https://coding.dashscope.aliyuncs.com/v1", true),
    preset("alibaba", "阿里云百炼（国际）", "https://dashscope-intl.aliyuncs.com/compatible-mode/v1", true),
    preset("tencent-coding-plan", "腾讯云 Coding Plan", "https://api.lkeap.cloud.tencent.com/coding/v3", true),
    preset("tencent-token-plan", "腾讯云 Token Plan", "https://api.lkeap.cloud.tencent.com/plan/v3", true),
    preset("tencent-tokenhub", "腾讯 TokenHub", "https://tokenhub.tencentmaas.com/v1", true),
    preset("volcengine", "火山方舟", "https://ark.cn-beijing.volces.com/api/v3", true),
    preset("volcengine-coding-plan", "火山方舟 Coding Plan", "https://ark.cn-beijing.volces.com/api/coding/v3", true),
    preset("minimax-cn", "MiniMax", "https://api.minimax.cn/anthropic/v1", true),
    preset("siliconflow-cn", "硅基流动", "https://api.siliconflow.cn/v1", true),
    preset("openrouter", "OpenRouter", "https://openrouter.ai/api/v1", true),
    preset("openai-compatible", "OpenAI 兼容", "", false),
];

/// 设置页的预设下拉：[{ id, name, baseURL }]。
pub(crate) fn list() -> Value {
    PRESETS.iter().map(|p| json!({ "id": p.id, "name": p.name, "baseURL": p.base_url })).collect()
}

const CACHE_TTL_SECS: u64 = 6 * 3600;
const DEFAULT_API: &str = "openai-completions";

fn preset_of(provider: &Value) -> Option<&'static Preset> {
    let id = provider.get("preset")?.as_str()?;
    PRESETS.iter().find(|preset| preset.id == id)
}

pub(crate) fn cache_path(nova_root: &Path) -> PathBuf {
    nova_root.join("alkaid").join("models-cache.json")
}

fn read_cache(nova_root: &Path) -> Value {
    std::fs::read_to_string(cache_path(nova_root))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}))
}

/// 补齐预设 provider 的默认值并合并缓存的模型（同步，供 load_config 调用）。
pub(crate) fn expand(config: &mut Value, nova_root: &Path) {
    let Some(providers) = config.get_mut("provider").and_then(Value::as_object_mut) else {
        return;
    };
    let cache = read_cache(nova_root);
    for (id, provider) in providers.iter_mut() {
        let Some(preset) = preset_of(provider) else { continue };
        let Some(object) = provider.as_object_mut() else { continue };
        object.entry("name").or_insert_with(|| json!(preset.name));
        object.entry("api").or_insert_with(|| json!(DEFAULT_API));
        let options = object.entry("options").or_insert_with(|| json!({}));
        if !preset.base_url.is_empty()
            && options.get("baseURL").or_else(|| options.get("baseUrl")).and_then(Value::as_str).unwrap_or("").is_empty()
        {
            options["baseURL"] = json!(preset.base_url);
        }
        let mut models = cache.pointer(&format!("/{id}/models")).and_then(Value::as_object).cloned().unwrap_or_default();
        if let Some(explicit) = object.get("models").and_then(Value::as_object) {
            models.extend(explicit.clone());
        }
        object.insert("models".into(), Value::Object(models));
    }
}

fn fingerprint(parts: &[&str]) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    parts.hash(&mut hasher);
    format!("{:x}", hasher.finish())
}

/// 拉取过期（超过 TTL 或 Base URL/Key 变化）的预设 provider 模型列表并写回缓存，失败保留旧缓存。
/// force（设置页「获取模型」）无视 TTL 立即拉取，并把失败原因返回给调用方；返回缓存是否有更新。
pub(crate) async fn refresh(http: &reqwest::Client, nova_root: &Path, config: &Value, force: bool) -> Result<bool, String> {
    let Some(providers) = config.get("provider").and_then(Value::as_object) else { return Ok(false) };
    let env = super::config::process_env();
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let mut cache = read_cache(nova_root);
    let mut models_dev: Option<Value> = None;
    let mut changed = false;
    let mut errors = Vec::new();
    for (id, provider) in providers {
        let Some(preset) = preset_of(provider) else { continue };
        let option = |key: &str| {
            provider.pointer(&format!("/options/{key}")).and_then(Value::as_str).unwrap_or("")
        };
        let base_url = crate::lyra_complete::resolve_env_string(
            Some(option("baseURL")).filter(|s| !s.is_empty()).unwrap_or(option("baseUrl")),
            &env,
        )
        .unwrap_or_default();
        let base_url = if base_url.is_empty() { preset.base_url.to_string() } else { base_url };
        let api_key = crate::lyra_complete::resolve_env_string(option("apiKey"), &env).unwrap_or_default();
        // 转换规则升级：旧缓存缺少思考能力，不能继续沿用六小时。
        let print = fingerprint(&["2", preset.id, &base_url, &api_key]);
        let entry = &cache[id.as_str()];
        if !force
            && entry["fingerprint"] == print.as_str()
            && now.saturating_sub(entry["fetchedAt"].as_u64().unwrap_or(0)) < CACHE_TTL_SECS
        {
            continue;
        }
        let proxied;
        let http = match option("proxy").trim() {
            "" => http,
            proxy => {
                proxied = super::provider::client_for_proxy(proxy);
                &proxied
            }
        };
        match fetch_models(http, preset, &base_url, &api_key, &mut models_dev).await {
            Ok(models) => {
                cache[id.as_str()] = json!({ "fingerprint": print, "fetchedAt": now, "models": models });
                changed = true;
            }
            Err(error) if force => errors.push(format!("{id}：{error}")),
            Err(error) => eprintln!("[lyra] 拉取 {id} 模型列表失败：{error}"),
        }
    }
    if changed {
        let path = cache_path(nova_root);
        let tmp = path.with_extension("json.tmp");
        let written = std::fs::create_dir_all(path.parent().unwrap())
            .and_then(|_| std::fs::write(&tmp, cache.to_string()))
            .and_then(|_| std::fs::rename(&tmp, &path));
        written.map_err(|error| format!("写入模型缓存失败：{error}"))?;
    }
    if errors.is_empty() { Ok(changed) } else { Err(errors.join("；")) }
}

/// 缓存中某 provider 的模型（设置页「获取模型」后展示）。
pub(crate) fn cached_models(nova_root: &Path, id: &str) -> Value {
    read_cache(nova_root).get(id).map(|entry| entry["models"].clone()).unwrap_or_else(|| json!({}))
}

async fn get_json(http: &reqwest::Client, url: &str, api_key: &str) -> Result<Value, String> {
    let mut request = http.get(url).timeout(Duration::from_secs(30));
    if !api_key.is_empty() {
        request = request.bearer_auth(api_key);
    }
    let response = request.send().await.map_err(|e| format!("{url}：{e}"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("{url} 返回 {status}"));
    }
    response.json().await.map_err(|e| format!("{url}：{e}"))
}

async fn fetch_models(
    http: &reqwest::Client,
    preset: &Preset,
    base_url: &str,
    api_key: &str,
    models_dev: &mut Option<Value>,
) -> Result<Map<String, Value>, String> {
    if base_url.is_empty() {
        return Err("缺少 Base URL".into());
    }
    let list = get_json(http, &format!("{}/models", base_url.trim_end_matches('/')), api_key).await;
    if models_dev.is_none() {
        match get_json(http, "https://models.dev/api.json", "").await {
            Ok(meta) => *models_dev = Some(meta),
            Err(error) if preset.models_dev => return Err(error),
            // 兼容端点以自己的 /models 为准，补充能力信息失败不能使列表不可用。
            Err(error) => eprintln!("[lyra] 模型能力信息暂不可用：{error}"),
        }
    }
    if !preset.models_dev {
        return Ok(convert(&list?, None, models_dev.as_ref()));
    }
    let meta = models_dev.as_ref().and_then(|all| all.get(preset.id)).cloned().unwrap_or_default();
    // Coding Plan、Anthropic 协议端点等常不提供 /models，此时整份用 models.dev 的列表。
    let list = list.unwrap_or_else(|error| {
        eprintln!("[lyra] {}：{error}，改用 models.dev 模型列表", preset.id);
        Value::Null
    });
    Ok(convert(&list, Some(&meta), models_dev.as_ref()))
}

/// 中转站模型名可能带厂商前缀；按完整 ID / canonical ID 匹配，优先原厂能力。
fn catalog_model<'a>(catalog: Option<&'a Value>, id: &str) -> Option<&'a Value> {
    // ponytail: 每次刷新按模型扫描目录；若目录规模使刷新变慢，再建立 ID 索引。
    let catalog = catalog?.as_object()?;
    catalog.iter()
        .flat_map(|(provider, info)| info["models"].as_object().into_iter().flatten()
            .map(move |(key, model)| (provider, key, model)))
        .filter_map(|(provider, key, model)| {
            let qualified = format!("{provider}/{key}");
            let canonical = model["canonical_model_id"].as_str().unwrap_or("");
            if id.eq_ignore_ascii_case(&qualified) {
                Some((0, model))
            } else if id.eq_ignore_ascii_case(key) || id.eq_ignore_ascii_case(canonical) {
                let native = canonical.split_once('/').is_some_and(|(owner, _)| owner.eq_ignore_ascii_case(provider));
                // 旧的原厂条目可能没有 canonical 字段；中转条目的 canonical 仍可指回它。
                let origin = canonical.split_once('/')
                    .and_then(|(owner, key)| catalog.get(owner)?.get("models")?.get(key));
                Some((if native || origin.is_some() { 1 } else { 2 }, origin.unwrap_or(model)))
            } else {
                None
            }
        })
        .min_by_key(|(rank, _)| *rank)
        .map(|(_, model)| model)
}

/// models.dev 的 npm 包名 → Lyra 协议；Google 原生协议不支持，返回 None。
fn npm_api(npm: &str) -> Option<&'static str> {
    match npm {
        "@ai-sdk/anthropic" => Some("anthropic-messages"),
        "@ai-sdk/openai" => Some("openai-responses"),
        npm if npm.contains("google") => None,
        _ => Some(DEFAULT_API),
    }
}

/// /models 列表 + 可选 models.dev provider 条目 → Lyra models 配置。
/// 有 models.dev 时只保留它认识的模型（滤掉嵌入、绘图等非对话模型），/models 不可用则整份采用。
fn convert(list: &Value, meta: Option<&Value>, catalog: Option<&Value>) -> Map<String, Value> {
    let known = meta.and_then(|m| m["models"].as_object());
    let provider_npm = meta.and_then(|m| m["npm"].as_str()).unwrap_or("");
    let null = Value::Null;
    let mut entries: Vec<(&str, &Value)> = list["data"].as_array().into_iter().flatten()
        .filter_map(|entry| entry["id"].as_str().filter(|id| !id.is_empty()).map(|id| (id, entry)))
        .filter(|(id, _)| known.is_none_or(|k| k.contains_key(*id)))
        .collect();
    if entries.is_empty() {
        if let Some(known) = known {
            entries = known.iter().filter(|(_, info)| info["status"] != "deprecated").map(|(id, _)| (id.as_str(), &null)).collect();
        }
    }
    let mut out = Map::new();
    for (id, entry) in entries {
        // ponytail: jev-* 是 /systemone 决策模型而非对话模型，按前缀排除；上游加新的非对话模型时再扩充。
        if id.starts_with("jev-") {
            continue;
        }
        let provider_info = known.and_then(|m| m.get(id));
        let info = provider_info.or_else(|| catalog_model(catalog, id));
        // Lyra 是 agent，不支持工具调用的模型（深度研究、数学等）选了也用不了。
        if info.is_some_and(|i| i["tool_call"] == false) {
            continue;
        }
        // Command Code 在 supported_endpoints 声明协议，两者都支持时优先 chat/completions。
        let api = match entry["supported_endpoints"].as_array() {
            Some(endpoints) => {
                let has = |path: &str| endpoints.iter().any(|e| e.as_str() == Some(path));
                if has("/chat/completions") {
                    Some(DEFAULT_API)
                } else if has("/responses") {
                    Some("openai-responses")
                } else if has("/messages") {
                    Some("anthropic-messages")
                } else {
                    None
                }
            }
            // 跨 provider 只补能力，不继承原厂协议（如兼容端点上的 Claude / Gemini）。
            None => npm_api(provider_info.and_then(|i| i.pointer("/provider/npm")).and_then(Value::as_str).unwrap_or(provider_npm)),
        };
        let Some(api) = api else { continue };
        let mut model = json!({});
        if api != DEFAULT_API {
            model["api"] = json!(api);
        }
        if let Some(name) = entry["name"].as_str().or_else(|| info.and_then(|i| i["name"].as_str())) {
            model["name"] = json!(name);
        }
        let context = entry["context_length"].as_u64().or_else(|| info.and_then(|i| i.pointer("/limit/context")).and_then(Value::as_u64));
        let output = info.and_then(|i| i.pointer("/limit/output")).and_then(Value::as_u64);
        let mut limit = json!({});
        if let Some(context) = context.filter(|n| *n > 0) {
            limit["context"] = json!(context);
        }
        if let Some(output) = output.filter(|n| *n > 0) {
            limit["output"] = json!(output);
        }
        if limit.as_object().is_some_and(|l| !l.is_empty()) {
            model["limit"] = limit;
        }
        if let Some(info) = info {
            let image = info.pointer("/modalities/input").and_then(Value::as_array)
                .is_some_and(|input| input.iter().any(|v| v.as_str() == Some("image")));
            if image {
                model["modalities"] = json!({ "input": ["text", "image"] });
            }
            // DeepSeek、GLM、Kimi 等要求多轮里回传 reasoning_content，否则报错或丢思维链。
            if info.pointer("/interleaved/field") == Some(&json!("reasoning_content")) {
                model["options"] = json!({ "requiresReasoningContentOnAssistantMessages": true });
            }
        }
        // /models 显式声明优先，缺失时才补目录信息；兼容 provider 也走同一条路径。
        let reasoning_options = entry["reasoning_options"].as_array()
            .or_else(|| info.and_then(|i| i["reasoning_options"].as_array()));
        let efforts = reasoning_options.into_iter().flatten()
            .find(|o| o["type"] == "effort")
            .and_then(|o| o["values"].as_array());
        let variants: Map<String, Value> = efforts.into_iter().flatten()
            .filter_map(Value::as_str).filter(|e| !e.trim().is_empty())
            .map(|e| (e.to_string(), json!({ "reasoningEffort": e })))
            .collect();
        let reasoning = entry["reasoning"].as_bool()
            .or((entry["reasoning_options"].is_array() && !variants.is_empty()).then_some(true))
            .or_else(|| info.and_then(|i| i["reasoning"].as_bool()))
            .unwrap_or(!variants.is_empty());
        if reasoning {
            model["reasoning"] = json!(true);
            if !variants.is_empty() {
                model["variants"] = Value::Object(variants);
            }
        }
        out.insert(id.to_string(), model);
    }
    out
}

/// load_config 缓存键的一部分：config.jsonc 与模型缓存任一变化都要重新解析。
pub(crate) fn cache_mtime(nova_root: &Path) -> Option<SystemTime> {
    std::fs::metadata(cache_path(nova_root)).ok().and_then(|m| m.modified().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_import_keeps_configured_protocol_and_effort_with_or_without_catalog() {
        let provider = json!({ "api": "openai-responses", "options": {
            "baseURL": "https://proxy.example/v1", "apiKey": "test-key"
        }, "models": { "custom/model": {
            "reasoning": true, "options": { "reasoningEffort": "high" },
            "variants": { "high": { "reasoningEffort": "high" } }
        } } });
        let catalog = json!({ "upstream": { "models": { "custom/model": {
            "reasoning": true, "reasoning_options": [{ "type": "effort", "values": ["low", "high"] }]
        } } } });
        for catalog in [None, Some(&catalog)] {
            let imported = convert_local(provider.clone(), "custom/model", &Value::Null, catalog).unwrap();
            assert_eq!(imported["model"], "custom/model/variant/high");
            assert_eq!(imported["provider"]["api"], "openai-responses");
            assert_eq!(imported["provider"]["options"]["apiKey"], "test-key");
            assert_eq!(imported["provider"]["models"]["custom/model"]["variants"]["low"].is_object(), catalog.is_some());
        }
        let provider = json!({ "api": "anthropic-messages", "options": { "baseURL": "https://api.example" }, "models": {} });
        assert!(convert_local(provider.clone(), "", &Value::Null, None).is_err());
        let imported = convert_local(provider, "", &json!({ "data": [{ "id": "from-api", "supported_endpoints": ["/responses"] }] }), None).unwrap();
        assert_eq!(imported["model"], "from-api");
        assert!(imported["provider"]["models"]["from-api"].get("api").is_none());
    }

    #[test]
    fn converts_commandcode_and_models_dev_lists() {
        let cc = json!({ "data": [
            { "id": "claude-opus-5-5", "name": "Claude Opus 5.5", "context_length": 1000000, "supported_endpoints": ["/messages"] },
            { "id": "deepseek/deepseek-v4-pro", "context_length": 1000000, "supported_endpoints": ["/chat/completions", "/responses"] },
            { "id": "jev-1", "supported_endpoints": ["/systemone"] },
            { "id": "odd", "supported_endpoints": ["/systemone"] },
        ]});
        let models = convert(&cc, None, None);
        assert_eq!(models["claude-opus-5-5"], json!({ "api": "anthropic-messages", "name": "Claude Opus 5.5", "limit": { "context": 1000000 } }));
        assert_eq!(models["deepseek/deepseek-v4-pro"], json!({ "limit": { "context": 1000000 } }));
        assert!(!models.contains_key("jev-1") && !models.contains_key("odd"));

        let zen = json!({ "data": [{ "id": "gpt-6" }, { "id": "glm-5.3" }, { "id": "gemini-4" }, { "id": "new-model" }] });
        let meta = json!({ "npm": "@ai-sdk/openai-compatible", "models": {
            "gpt-6": { "name": "GPT-6", "provider": { "npm": "@ai-sdk/openai" }, "reasoning": true,
                "reasoning_options": [{ "type": "effort", "values": ["low", "high"] }],
                "modalities": { "input": ["text", "image", "pdf"] }, "limit": { "context": 400000, "output": 128000 } },
            "glm-5.3": { "name": "GLM-5.3", "reasoning": true, "reasoning_options": [{ "type": "toggle" }] },
            "gemini-4": { "provider": { "npm": "@ai-sdk/google" } },
        }});
        let models = convert(&zen, Some(&meta), None);
        assert_eq!(models["gpt-6"], json!({
            "api": "openai-responses", "name": "GPT-6", "limit": { "context": 400000, "output": 128000 },
            "modalities": { "input": ["text", "image"] }, "reasoning": true,
            "variants": { "low": { "reasoningEffort": "low" }, "high": { "reasoningEffort": "high" } },
        }));
        assert_eq!(models["glm-5.3"], json!({ "name": "GLM-5.3", "reasoning": true }));
        assert!(!models.contains_key("gemini-4") && !models.contains_key("new-model"));

        // /models 不可用（如 MiniMax 的 Anthropic 端点）：整份采用 models.dev，跳过弃用与不支持工具的模型。
        let minimax = json!({ "npm": "@ai-sdk/anthropic", "models": {
            "m3": { "name": "M3", "interleaved": { "field": "reasoning_content" } },
            "old": { "status": "deprecated" },
            "research": { "tool_call": false },
        }});
        let models = convert(&Value::Null, Some(&minimax), None);
        assert_eq!(Value::Object(models), json!({ "m3": {
            "api": "anthropic-messages", "name": "M3",
            "options": { "requiresReasoningContentOnAssistantMessages": true },
        }}));
    }

    #[test]
    fn expand_fills_preset_defaults_and_merges_cache() {
        let root = std::env::temp_dir().join(format!("lyra-presets-{}", std::process::id()));
        std::fs::create_dir_all(root.join("alkaid")).unwrap();
        std::fs::write(cache_path(&root), json!({ "cc": { "models": { "a": {}, "b": { "name": "B" } } } }).to_string()).unwrap();
        let mut config = json!({ "provider": {
            "cc": { "preset": "commandcode", "options": { "apiKey": "k" }, "models": { "b": { "name": "mine" } } },
            "plain": { "api": "openai-completions", "options": { "baseURL": "x" }, "models": { "m": {} } },
        }});
        expand(&mut config, &root);
        let cc = &config["provider"]["cc"];
        assert_eq!(cc["options"]["baseURL"], "https://api.commandcode.ai/provider/v1");
        assert_eq!(cc["name"], "Command Code");
        assert_eq!(cc["models"], json!({ "a": {}, "b": { "name": "mine" } }));
        assert_eq!(config["provider"]["plain"]["models"], json!({ "m": {} }));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn compatible_presets_fill_reasoning_without_changing_protocol() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let catalog = json!({
            "aaa-relay": { "models": { "claude-opus-5-5": {
                "canonical_model_id": "anthropic/claude-opus-5-5", "reasoning": false
            } } },
            "anthropic": { "npm": "@ai-sdk/anthropic", "models": {
                "claude-opus-5-5": { "reasoning": true,
                    "reasoning_options": [{ "type": "effort", "values": ["low", "high"] }] }
            } },
            "deepseek": { "models": {
                "deepseek-flash": { "canonical_model_id": "deepseek/deepseek-v4.1-flash", "reasoning": true,
                    "reasoning_options": [{ "type": "effort", "values": ["high", "max"] }] }
            } },
            "google": { "npm": "@ai-sdk/google", "models": {
                "gemini-test": { "reasoning": true, "reasoning_options": [{ "type": "effort", "values": ["low", "high"] }] }
            } },
        });
        let list = json!({ "data": [
            { "id": "claude-opus-5-5", "supported_endpoints": ["/messages"] },
            { "id": "deepseek/deepseek-v4.1-flash", "supported_endpoints": ["/chat/completions", "/responses"] },
            { "id": "Google/Gemini-Test" },
            { "id": "unknown" },
        ] });
        for preset_id in ["commandcode", "openai-compatible"] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let body = list.to_string();
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let mut buf = [0; 1024];
                while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                    let n = socket.read(&mut buf).await.unwrap();
                    assert!(n > 0);
                    request.extend_from_slice(&buf[..n]);
                }
                assert!(request.starts_with(b"GET /models "));
                socket.write_all(format!("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            });
            let preset = PRESETS.iter().find(|p| p.id == preset_id).unwrap();
            let models = fetch_models(&reqwest::Client::builder().no_proxy().build().unwrap(), preset,
                &url, "", &mut Some(catalog.clone())).await.unwrap();
            server.await.unwrap();
            assert_eq!(models["claude-opus-5-5"]["api"], "anthropic-messages");
            assert_eq!(models["claude-opus-5-5"]["variants"]["high"]["reasoningEffort"], "high");
            assert_eq!(models["deepseek/deepseek-v4.1-flash"]["variants"]["max"]["reasoningEffort"], "max");
            assert!(models["Google/Gemini-Test"].get("api").is_none());
            assert_eq!(models["Google/Gemini-Test"]["reasoning"], true);
            assert_eq!(models["unknown"], json!({}));
            let config = json!({ "model": "p/deepseek/deepseek-v4.1-flash",
                "provider": { "p": { "api": DEFAULT_API, "options": { "baseURL": url }, "models": models } } });
            assert!(super::super::config::default_model(&config).unwrap().starts_with("p/deepseek/deepseek-v4.1-flash/variant/"));
            let selection = "p/deepseek/deepseek-v4.1-flash/variant/max";
            assert!(super::super::config::model_options(&config).iter().any(|o| o["value"] == selection));
            let resolved = super::super::config::resolve_model(&config, Some(selection), &Default::default()).unwrap();
            assert_eq!(resolved.thinking_level.as_deref(), Some("max"));
            assert!(resolved.model.reasoning);
            assert_eq!(resolved.model.api, DEFAULT_API);
        }
    }

    #[test]
    fn explicit_reasoning_metadata_takes_precedence() {
        let meta = json!({ "models": { "m": { "reasoning": true,
            "reasoning_options": [{ "type": "effort", "values": ["low", "high"] }] } } });
        for (fields, expected) in [
            (json!({ "reasoning": false }), json!({})),
            (json!({ "reasoning_options": [] }), json!({ "reasoning": true })),
            (json!({ "reasoning_options": [{ "type": "effort", "values": ["max", "", null, 1] }] }),
                json!({ "reasoning": true, "variants": { "max": { "reasoningEffort": "max" } } })),
        ] {
            let mut entry = fields;
            entry["id"] = json!("m");
            let models = convert(&json!({ "data": [entry] }), Some(&meta), None);
            assert_eq!(models["m"], expected);
        }
        let models = convert(&json!({ "data": [{ "id": "custom", "reasoning_options": [
            { "type": "effort", "values": ["medium"] }
        ] }] }), None, None);
        assert_eq!(models["custom"]["variants"]["medium"]["reasoningEffort"], "medium");
        assert_eq!(models["custom"]["reasoning"], true);
        let models = convert(&json!({ "data": [{ "id": "custom", "reasoning_options": [
            { "type": "effort", "values": ["high"] }
        ] }] }), Some(&json!({ "models": { "custom": { "reasoning": false } } })), None);
        assert_eq!(models["custom"]["variants"]["high"]["reasoningEffort"], "high");
    }
}
