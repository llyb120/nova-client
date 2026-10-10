//! 自适应 provider：config.jsonc 里只写 `"preset"` 与 apiKey，Base URL、模型列表与
//! 每个模型的协议自动获得。模型列表拉取后缓存到 alkaid/models-cache.json，
//! load_config 时合并进 provider.models（手写的同名模型覆盖缓存），下游解析无需感知。

use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[path = "local_provider.rs"]
mod local_provider;

/// 导入为普通 provider，后续由现有设置编辑/保存；不把本机配置引用写入可共享配置。
async fn fetch_local_models(http: &reqwest::Client, provider: Value, catalog: &mut Option<Value>) -> Result<Map<String, Value>, String> {
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
            if value.is_null() { continue; }
            let value = value.as_str().ok_or("本地 API 请求头必须是字符串")?;
            let name = reqwest::header::HeaderName::from_bytes(name.as_bytes()).map_err(|_| "本地 API 请求头名称无效")?;
            let value = reqwest::header::HeaderValue::from_str(value).map_err(|_| "本地 API 请求头值无效")?;
            request = request.header(name, value);
        }
    }
    let list = async {
        let mut entries = Vec::new();
        let mut cursors = std::collections::HashSet::new();
        let mut cursor = None;
        loop {
            let mut page = request.try_clone().ok_or("无法复制模型列表请求")?;
            if let Some(cursor) = &cursor { page = page.query(&[("after_id", cursor)]); }
            let page: Value = page.send().await.map_err(|_| "无法连接本地配置的模型 API")?
                .error_for_status().map_err(|e| format!("模型 API 返回 {}", e.status().unwrap()))?
                .json().await.map_err(|_| "模型 API 返回的 JSON 无效")?;
            entries.extend(page["data"].as_array().ok_or("模型 API 缺少 data 列表")?.iter().cloned());
            if page["has_more"] != true { break; }
            let next = page["last_id"].as_str().filter(|id| !id.is_empty()).ok_or("模型列表分页缺少 last_id")?;
            if !cursors.insert(next.to_string()) { return Err("模型列表分页游标重复".to_string()); }
            cursor = Some(next.to_string());
        }
        Ok::<_, String>(json!({ "data": entries }))
    };
    let (list, meta) = tokio::join!(list, async {
        if catalog.is_none() { get_json(http, "https://models.dev/api.json", "").await.ok() } else { None }
    });
    if let Some(meta) = meta { *catalog = Some(meta); }
    // 失败交给统一刷新流程保留旧缓存，不能把完整列表缩回本地选中的一个模型。
    Ok(convert_local(provider, "", &list?, catalog.as_ref())?["provider"]["models"].as_object().unwrap().clone())
}

fn convert_local(mut provider: Value, selected: &str, list: &Value, catalog: Option<&Value>) -> Result<Value, String> {
    let explicit = provider["models"].as_object().cloned().unwrap_or_default();
    let mut entries = list["data"].as_array().cloned().unwrap_or_default();
    for id in explicit.keys() {
        if !entries.iter().any(|entry| entry["id"] == id.as_str()) { entries.push(json!({ "id": id })); }
    }
    let preset_id = PRESETS.iter().find(|p| provider["options"]["baseURL"].as_str()
        .is_some_and(|url| !p.base_url.is_empty() && url.trim_end_matches('/') == p.base_url)).map(|p| p.id).unwrap_or("");
    let mut models = convert(&json!({ "data": entries }), None, catalog, preset_id);
    for (id, model) in &mut models {
        if model["reasoning"] == true {
            configure_reasoning(model, id, provider["api"].as_str().unwrap_or(DEFAULT_API), preset_id,
                entries.iter().find(|entry| entry["id"] == id.as_str())
                    .and_then(|entry| entry["reasoning_options"].as_array())
                    .or_else(|| reasoning_catalog_model(catalog, id).and_then(|info| info["reasoning_options"].as_array())));
        }
    }
    merge_local_models(&mut models, &explicit);
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

fn merge_local_models(models: &mut Map<String, Value>, explicit: &Map<String, Value>) {
    for (id, local) in explicit {
        let model = models.entry(id.clone()).or_insert_with(|| json!({}));
        for field in ["options", "variants"] {
            if let Some(values) = local[field].as_object() {
                let mut merged = model[field].as_object().cloned().unwrap_or_default();
                merged.extend(values.clone());
                model[field] = json!(merged);
            }
        }
        if let Some(reasoning) = local.get("reasoning") { model["reasoning"] = reasoning.clone(); }
    }
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
    preset("local-codex", "本地 Codex", "", false),
    preset("local-claude-code", "本地 Claude Code", "", false),
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
    PRESETS.iter().map(|p| json!({ "id": p.id, "name": p.name, "baseURL": p.base_url, "local": local_source(p.id).is_some() })).collect()
}

fn local_source(preset: &str) -> Option<&str> {
    match preset {
        "local-codex" => Some("codex"),
        "local-claude-code" => Some("claude-code"),
        _ => None,
    }
}

fn resolve_local(provider: &Value, source: &str, env: &std::collections::HashMap<String, String>) -> Result<Value, String> {
    let (mut local, _) = local_provider::load(source, env)?;
    let mut options = local["options"].as_object().cloned().unwrap_or_default();
    let mut headers = options["headers"].as_object().cloned().unwrap_or_default();
    if let Some(explicit) = provider["options"].as_object() {
        // Key 始终跟随 CLI；其余手写选项仍可覆盖默认值。
        options.extend(explicit.iter().filter(|(key, _)| key.as_str() != "apiKey").map(|(k, v)| (k.clone(), v.clone())));
        if let Some(explicit) = explicit.get("headers").and_then(Value::as_object) { headers.extend(explicit.clone()); }
    }
    options.insert("headers".into(), json!(headers));
    let models = local["models"].clone();
    if let Some(explicit) = provider.as_object() { local.as_object_mut().unwrap().extend(explicit.clone()); }
    local["options"] = json!(options);
    local["models"] = json!(models);
    super::config::resolve_config_env(&local, env)
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
    let env = super::config::process_env();
    for (id, provider) in providers.iter_mut() {
        let Some(preset) = preset_of(provider) else { continue };
        if let Some(source) = local_source(preset.id) {
            let explicit = provider["models"].as_object().cloned().unwrap_or_default();
            match resolve_local(provider, source, &env) {
                Ok(mut local) => {
                    let mut models = cache[id.as_str()]["models"].as_object().cloned().unwrap_or_default();
                    merge_local_models(&mut models, local["models"].as_object().unwrap());
                    models.extend(explicit);
                    local["models"] = json!(models);
                    *provider = local;
                }
                Err(error) => {
                    eprintln!("[lyra] {id}：{error}");
                    provider["models"] = json!({});
                }
            }
            continue;
        }
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
        let local;
        let provider = if let Some(source) = local_source(preset.id) {
            local = match resolve_local(provider, source, &env) {
                Ok(local) => local,
                Err(error) => {
                    if force { errors.push(format!("{id}：{error}")); }
                    else { eprintln!("[lyra] {id}：{error}"); }
                    continue;
                }
            };
            &local
        } else { provider };
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
        // 转换规则升级：旧缓存缺少模型私有参数，不能继续沿用六小时。
        let headers = provider["options"]["headers"].to_string();
        let local_models = if local_source(preset.id).is_some() { provider["models"].to_string() } else { String::new() };
        let print = fingerprint(&["5", preset.id, &base_url, &api_key, &headers, &local_models, provider["api"].as_str().unwrap_or("")]);
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
        let result = if local_source(preset.id).is_some() {
            fetch_local_models(http, provider.clone(), &mut models_dev).await
        } else {
            fetch_models(http, preset, &base_url, &api_key, &mut models_dev).await
        };
        match result {
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
            Err(error) if preset.models_dev && list.is_err() => return Err(error),
            // 兼容端点以自己的 /models 为准，补充能力信息失败不能使列表不可用。
            Err(error) => eprintln!("[lyra] 模型能力信息暂不可用：{error}"),
        }
    }
    if !preset.models_dev {
        return Ok(convert(&list?, None, models_dev.as_ref(), preset.id));
    }
    let meta = models_dev.as_ref().and_then(|all| all.get(preset.id)).cloned().unwrap_or_default();
    // Coding Plan、Anthropic 协议端点等常不提供 /models，此时整份用 models.dev 的列表。
    let list = list.unwrap_or_else(|error| {
        eprintln!("[lyra] {}：{error}，改用 models.dev 模型列表", preset.id);
        Value::Null
    });
    Ok(convert(&list, Some(&meta), models_dev.as_ref(), preset.id))
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

/// 计费/速度/上下文变体对应的原模型 ID。
fn variant_base(id: &str) -> Option<&str> {
    ["[1m]", "-1m", "-fast", "-paid", ":free"].iter().find_map(|s| id.strip_suffix(s))
}

// 目录里只借用基础模型的推理能力；同列表里的原模型由 convert 末尾整体补齐。
fn reasoning_catalog_model<'a>(catalog: Option<&'a Value>, id: &str) -> Option<&'a Value> {
    catalog_model(catalog, id).or_else(|| catalog_model(catalog, variant_base(id)?))
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
fn convert(list: &Value, meta: Option<&Value>, catalog: Option<&Value>, preset_id: &str) -> Map<String, Value> {
    let known = meta.and_then(|m| m["models"].as_object());
    let provider_npm = meta.and_then(|m| m["npm"].as_str()).unwrap_or("");
    let null = Value::Null;
    let mut entries: Vec<(&str, &Value)> = list["data"].as_array().into_iter().flatten()
        .filter_map(|entry| entry["id"].as_str().filter(|id| !id.is_empty()).map(|id| (id, entry)))
        .collect();
    // 成功返回的列表（包括空列表）是可用性依据；目录只能补能力，不能补回无权限模型。
    if !list["data"].is_array() {
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
        let tool_call = entry["supported_parameters"].as_array().map(|p| p.iter().any(|v| v == "tools"))
            .or_else(|| info.and_then(|i| i["tool_call"].as_bool())).unwrap_or(true);
        if !tool_call {
            continue;
        }
        // Command Code 在 supported_endpoints 声明协议，两者都支持时优先 chat/completions。
        let api = match entry["supported_endpoints"].as_array() {
            Some(endpoints) => {
                let has = |path: &str| endpoints.iter().any(|e| e.as_str() == Some(path));
                let responses_tools_only = matches!(id.rsplit('/').next(), Some("gpt-6-astra" | "gpt-6.1-sol"));
                if has("/responses") && responses_tools_only {
                    Some("openai-responses")
                } else if has("/chat/completions") && !responses_tools_only {
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
        let output = entry.pointer("/top_provider/max_completion_tokens").and_then(Value::as_u64)
            .or_else(|| info.and_then(|i| i.pointer("/limit/output")).and_then(Value::as_u64));
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
        let image = entry.pointer("/architecture/input_modalities")
            .or_else(|| info.and_then(|i| i.pointer("/modalities/input"))).and_then(Value::as_array)
            .is_some_and(|input| input.iter().any(|v| v.as_str() == Some("image")));
        if image {
            model["modalities"] = json!({ "input": ["text", "image"] });
        }
        let reasoning_info = info.or_else(|| reasoning_catalog_model(catalog, id));
        if let Some(info) = reasoning_info {
            // DeepSeek、GLM、Kimi 等要求多轮里回传 reasoning_content，否则报错或丢思维链。
            if info.pointer("/interleaved/field") == Some(&json!("reasoning_content")) {
                model["options"] = json!({ "requiresReasoningContentOnAssistantMessages": true });
            }
        }
        // /models 显式声明优先，缺失时才补目录信息；兼容 provider 也走同一条路径。
        let reasoning_options = entry["reasoning_options"].as_array()
            .or_else(|| reasoning_info.and_then(|i| i["reasoning_options"].as_array()));
        let efforts = reasoning_options.into_iter().flatten()
            .find(|o| o["type"] == "effort")
            .and_then(|o| o["values"].as_array());
        // OpenRouter 的 /models 使用 reasoning 对象，而非 models.dev 的 reasoning_options。
        let router_reasoning = entry["reasoning"].as_object().filter(|_| preset_id == "openrouter");
        let router_efforts = json!(["max", "xhigh", "high", "medium", "low", "minimal", "none"]);
        let efforts = if let Some(reasoning) = router_reasoning {
            reasoning.get("supported_efforts").and_then(|v| if v.is_null() { router_efforts.as_array() } else { v.as_array() })
        } else { efforts };
        let mut variants: Map<String, Value> = efforts.into_iter().flatten()
            .filter_map(Value::as_str).filter(|e| !e.trim().is_empty())
            .filter(|e| *e != "none" || router_reasoning.is_none_or(|r| r.get("mandatory") != Some(&json!(true))))
            .map(|e| (e.to_string(), json!({ "reasoningEffort": e })))
            .collect();
        let reasoning = entry["reasoning"].as_bool()
            .or(router_reasoning.map(|_| true))
            .or(entry["reasoning_options"].as_array().filter(|o| !o.is_empty()).map(|_| true))
            .or_else(|| reasoning_info.and_then(|i| i["reasoning"].as_bool()))
            .unwrap_or(!variants.is_empty());
        if reasoning {
            model["reasoning"] = json!(true);
            configure_reasoning(&mut model, id, api, preset_id, reasoning_options);
            if let Some(reasoning) = router_reasoning {
                model["options"]["supportsThinkingToggle"] = json!(reasoning.get("mandatory") != Some(&json!(true)));
                model["options"]["supportsReasoningEffort"] = json!(efforts.is_some());
                if let Some(default) = reasoning.get("default_effort").and_then(Value::as_str).filter(|e| variants.contains_key(*e)) {
                    model["options"]["reasoningEffort"] = json!(default);
                }
            }
            if model.pointer("/options/supportsThinkingToggle") == Some(&json!(false)) { variants.remove("none"); }
            if model.pointer("/options/supportsReasoningEffort") == Some(&json!(false)) { variants.clear(); }
            if !variants.is_empty() {
                model["variants"] = Value::Object(variants);
            }
        }
        out.insert(id.to_string(), model);
    }
    // 变体（如 deepseek-v4-flash-fast）常缺模态、输出上限等元数据，缺什么沿用原模型；1M 变体不继承上下文。
    let ids: Vec<String> = out.keys().cloned().collect();
    for id in ids {
        let Some(base) = variant_base(&id).and_then(|b| out.get(b)).cloned() else { continue };
        let model = out.get_mut(&id).unwrap();
        for field in ["modalities", "reasoning", "options", "variants"] {
            if model.get(field).is_none() {
                if let Some(value) = base.get(field) { model[field] = value.clone(); }
            }
        }
        let long = id.ends_with("[1m]") || id.ends_with("-1m");
        for key in ["context", "output"] {
            if (key == "output" || !long) && model.pointer(&format!("/limit/{key}")).is_none() {
                if let Some(value) = base.pointer(&format!("/limit/{key}")) {
                    if !model["limit"].is_object() { model["limit"] = json!({}); }
                    model["limit"][key] = value.clone();
                }
            }
        }
    }
    out
}

// 厂商参数取决于接入端点，不能把原厂私有字段复制到未知兼容网关。
// 官方文档及未覆盖的端点见 docs/lyra-provider-models.md。
fn configure_reasoning(model: &mut Value, id: &str, api: &str, preset_id: &str, capabilities: Option<&Vec<Value>>) {
    let id = id.to_ascii_lowercase();
    let id = id.strip_suffix("[1m]").or_else(|| id.strip_suffix("-1m")).unwrap_or(&id);
    let format = if api == "anthropic-messages" && id.contains("claude")
        && capabilities.is_some_and(|c| c.iter().any(|c| c["type"] == "effort")
            && (!c.iter().any(|c| c["type"] == "budget_tokens")
                || id.ends_with("opus-4-6") || id.ends_with("sonnet-4-6")
                || id.ends_with("opus-4.6") || id.ends_with("sonnet-4.6"))) {
        Some("anthropic")
    } else { match preset_id {
        "deepseek" => Some("deepseek"),
        "zai" | "zai-coding-plan" | "zhipuai" | "zhipuai-coding-plan" => Some("zai"),
        "moonshotai" | "moonshotai-cn" | "kimi-code-plan-cn" => Some(if id.starts_with("kimi-k2") { "moonshot" } else { "kimi" }),
        "alibaba" | "alibaba-cn" | "alibaba-coding-plan-cn" if id.starts_with("qwen") || id.starts_with("qwq") => Some("qwen"),
        "siliconflow-cn" if matches!(id.rsplit('/').next().unwrap_or(""),
            "qwen3-8b" | "qwen3-14b" | "qwen3-32b" | "qwen3-30b-a3b" | "qwen3-235b-a22b"
            | "hunyuan-a13b-instruct" | "glm-5v-turbo" | "glm-4.6v" | "glm-4.5v"
            | "deepseek-v3.1" | "deepseek-v3.1-terminus" | "deepseek-v3.2-exp" | "deepseek-v3.2") => Some("qwen"),
        "minimax-cn" => Some("minimax"),
        "volcengine" | "volcengine-coding-plan" => Some("volcengine"),
        "openrouter" => Some("openrouter"),
        _ => None,
    } };
    let mut options = model["options"].as_object().cloned().unwrap_or_default();
    if let Some(format) = format {
        options.insert("thinkingFormat".into(), json!(format));
        if !matches!(format, "openrouter" | "volcengine") { options.insert("maxTokensField".into(), json!("max_tokens")); }
    }
    if let Some(capabilities) = capabilities {
        let effort = capabilities.iter().any(|c| c["type"] == "effort");
        let toggle = capabilities.iter().any(|c| c["type"] == "toggle"
            || (api == "anthropic-messages" && c["type"] == "budget_tokens")
            || c["values"].as_array().is_some_and(|v| v.iter().any(|v| v == "none")));
        options.insert("supportsReasoningEffort".into(), json!(effort));
        options.insert("supportsThinkingToggle".into(), json!(toggle));
        if format == Some("anthropic") {
            let default = if id.ends_with("opus-5-5") || id.ends_with("opus-5.5")
                || id.ends_with("haiku-5-5") || id.ends_with("haiku-5.5") { "medium" } else { "high" };
            if capabilities.iter().any(|c| c["type"] == "effort" && c["values"].as_array().is_some_and(|v| v.iter().any(|v| v == default))) {
                options.insert("reasoningEffort".into(), json!(default));
            }
        }
    }
    if matches!(format, Some("deepseek" | "zai" | "moonshot" | "kimi" | "volcengine")) {
        options.insert("requiresReasoningContentOnAssistantMessages".into(), json!(true));
    }
    if format == Some("zai") && id.starts_with("glm-5.3") {
        options.insert("supportsThinkingToggle".into(), json!(false));
    }
    if preset_id == "siliconflow-cn" && id.rsplit('/').next() == Some("deepseek-v3.1") {
        // SiliconFlow 此版本的工具调用仅支持非思考模式。
        options.insert("enable_thinking".into(), json!(false));
        options.insert("supportsThinkingToggle".into(), json!(false));
        options.insert("supportsReasoningEffort".into(), json!(false));
    }
    // K2.x 不接受 reasoning_effort；K3 不接受 K2.x 的 thinking 参数。
    if matches!(format, Some("moonshot")) {
        options.insert("supportsReasoningEffort".into(), json!(false));
        options.insert("supportsThinkingToggle".into(), json!(matches!(id, "kimi-k2.5" | "kimi-k2.6")));
    }
    if format == Some("kimi") { options.insert("supportsThinkingToggle".into(), json!(false)); }
    if matches!(format, Some("moonshot" | "kimi")) {
        // 固定采样值随思考模式变化，交给服务端默认；模型级 null 屏蔽 provider 的全局值。
        options.insert("temperature".into(), Value::Null);
        options.insert("top_p".into(), Value::Null);
        options.insert("topP".into(), Value::Null);
    }
    if format == Some("minimax") {
        options.insert("supportsThinkingToggle".into(), json!(id == "minimax-m3"));
        options.insert("supportsReasoningEffort".into(), json!(id == "minimax-m3.1-flash-preview"));
    }
    if !options.is_empty() { model["options"] = json!(options); }
}

/// load_config 缓存键的一部分：config.jsonc 与模型缓存任一变化都要重新解析。
pub(crate) fn cache_mtime(nova_root: &Path) -> Option<SystemTime> {
    std::fs::metadata(cache_path(nova_root)).ok().and_then(|m| m.modified().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn local_presets_fetch_all_pages_despite_selected_model_and_keep_private_options() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        for source in ["codex", "claude-code"] {
            let dir = tempfile::tempdir().unwrap();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let (variable, api) = if source == "codex" {
                std::fs::write(dir.path().join("config.toml"), format!(r#"
model = "selected"
model_provider = "proxy"
model_reasoning_effort = "high"
[model_providers.proxy]
base_url = "{base}/v1"
env_key = "TEST_KEY"
"#)).unwrap();
                ("CODEX_HOME", "openai-responses")
            } else {
                std::fs::write(dir.path().join("settings.json"), json!({
                    "model": "selected", "effortLevel": "high", "env": {
                        "ANTHROPIC_BASE_URL": base, "ANTHROPIC_AUTH_TOKEN": "test-token"
                    }
                }).to_string()).unwrap();
                ("CLAUDE_CONFIG_DIR", "anthropic-messages")
            };
            let env = std::collections::HashMap::from([
                (variable.into(), dir.path().to_string_lossy().into_owned()),
                ("TEST_KEY".into(), "test-token".into()),
            ]);
            let config = json!({ "preset": format!("local-{source}"), "options": {
                "apiKey": "stale-key", "temperature": 0.5, "headers": {"X-Route": "test"}
            }, "models": { "old-cache-only": {} } });
            let provider = resolve_local(&config, source, &env).unwrap();
            assert_eq!(provider["api"], api);
            assert!(provider["models"].get("old-cache-only").is_none());
            assert_eq!(provider["options"]["temperature"], 0.5);
            assert_eq!(provider["name"], if source == "codex" { "本地 Codex" } else { "本地 Claude Code" });
            let server = tokio::spawn(async move {
                for page in 0..3 {
                    let (mut socket, _) = listener.accept().await.unwrap();
                    let mut request = Vec::new();
                    let mut buf = [0; 1024];
                    while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                        let n = socket.read(&mut buf).await.unwrap();
                        assert!(n > 0);
                        request.extend_from_slice(&buf[..n]);
                    }
                    let request = String::from_utf8(request).unwrap().to_ascii_lowercase();
                    assert!(request.starts_with(if page == 1 { "get /v1/models?after_id=selected " } else { "get /v1/models " }));
                    assert!(request.contains("authorization: bearer test-token\r\n"));
                    assert!(request.contains("x-route: test\r\n"));
                    assert!(!request.contains("stale-key"));
                    if source == "claude-code" { assert!(request.contains("anthropic-version: 2023-06-01\r\n")); }
                    if page == 2 {
                        socket.write_all(b"HTTP/1.1 503 Unavailable\r\ncontent-length: 0\r\nconnection: close\r\n\r\n").await.unwrap();
                        continue;
                    }
                    let body = if page == 0 {
                        json!({"data":[{"id":"selected"}],"has_more":true,"last_id":"selected"})
                    } else {
                        json!({"data":[{"id":"another","name":"另一个模型"}],"has_more":false})
                    }.to_string();
                    socket.write_all(format!("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
                }
            });
            let mut catalog = Some(json!({ "test": { "models": { "selected": {
                "reasoning": true, "limit": {"context": 100000},
                "reasoning_options": [{"type":"effort","values":["low","high"]}]
            } } } }));
            let models = tokio::time::timeout(Duration::from_secs(5), fetch_local_models(
                &reqwest::Client::builder().no_proxy().build().unwrap(), provider.clone(), &mut catalog
            )).await.unwrap().unwrap();
            assert_eq!(models.len(), 2);
            assert_eq!(models["another"]["name"], "另一个模型");
            assert_eq!(models["selected"]["limit"]["context"], 100000);
            assert_eq!(models["selected"]["options"]["reasoningEffort"], "high");
            assert_eq!(models["selected"]["variants"]["low"]["reasoningEffort"], "low");
            assert!(models.values().all(|m| m.get("api").is_none()));
            let mut expanded = models.clone();
            merge_local_models(&mut expanded, provider["models"].as_object().unwrap());
            assert_eq!(expanded, models, "展开本地选择不能丢掉缓存中的能力和档位");
            let failed = tokio::time::timeout(Duration::from_secs(5), fetch_local_models(
                &reqwest::Client::builder().no_proxy().build().unwrap(), provider, &mut catalog
            )).await.unwrap();
            assert!(failed.unwrap_err().contains("503"), "失败时不得返回单模型列表覆盖完整缓存");
            server.await.unwrap();
        }
    }

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
        let models = convert(&cc, None, None, "");
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
        let models = convert(&zen, Some(&meta), None, "");
        assert_eq!(models["gpt-6"], json!({
            "api": "openai-responses", "name": "GPT-6", "limit": { "context": 400000, "output": 128000 },
            "modalities": { "input": ["text", "image"] }, "reasoning": true,
            "options": { "supportsReasoningEffort": true, "supportsThinkingToggle": false },
            "variants": { "low": { "reasoningEffort": "low" }, "high": { "reasoningEffort": "high" } },
        }));
        assert_eq!(models["glm-5.3"], json!({ "name": "GLM-5.3", "reasoning": true,
            "options": { "supportsReasoningEffort": false, "supportsThinkingToggle": true } }));
        assert!(!models.contains_key("gemini-4") && models.contains_key("new-model"));

        // /models 不可用（如 MiniMax 的 Anthropic 端点）：整份采用 models.dev，跳过弃用与不支持工具的模型。
        let minimax = json!({ "npm": "@ai-sdk/anthropic", "models": {
            "m3": { "name": "M3", "interleaved": { "field": "reasoning_content" } },
            "old": { "status": "deprecated" },
            "research": { "tool_call": false },
        }});
        let models = convert(&Value::Null, Some(&minimax), None, "");
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
            (json!({ "reasoning_options": [] }), json!({ "reasoning": true,
                "options": { "supportsReasoningEffort": false, "supportsThinkingToggle": false } })),
            (json!({ "reasoning_options": [{ "type": "effort", "values": ["max", "", null, 1] }] }),
                json!({ "reasoning": true, "options": { "supportsReasoningEffort": true, "supportsThinkingToggle": false },
                    "variants": { "max": { "reasoningEffort": "max" } } })),
        ] {
            let mut entry = fields;
            entry["id"] = json!("m");
            let models = convert(&json!({ "data": [entry] }), Some(&meta), None, "");
            assert_eq!(models["m"], expected);
        }
        let models = convert(&json!({ "data": [{ "id": "custom", "reasoning_options": [
            { "type": "effort", "values": ["medium"] }
        ] }] }), None, None, "");
        assert_eq!(models["custom"]["variants"]["medium"]["reasoningEffort"], "medium");
        assert_eq!(models["custom"]["reasoning"], true);
        let models = convert(&json!({ "data": [{ "id": "custom", "reasoning_options": [
            { "type": "effort", "values": ["high"] }
        ] }] }), Some(&json!({ "models": { "custom": { "reasoning": false } } })), None, "");
        assert_eq!(models["custom"]["variants"]["high"]["reasoningEffort"], "high");
    }

    #[test]
    fn native_model_constraints_are_scoped_to_the_endpoint() {
        let meta = json!({ "models": {
            "kimi-k2.6": { "reasoning": true, "reasoning_options": [{ "type": "toggle" }] },
            "kimi-k2.7-code": { "reasoning": true, "reasoning_options": [] },
            "kimi-k3": { "reasoning": true, "reasoning_options": [{ "type": "effort", "values": ["low", "high", "max"] }] },
            "glm-5.3": { "reasoning": true, "reasoning_options": [{ "type": "effort", "values": ["low", "high", "max"] }] },
            "qwen3.5-plus": { "reasoning": true, "reasoning_options": [{ "type": "toggle" }] },
            "MiniMax-M3": { "reasoning": true, "reasoning_options": [{ "type": "toggle" }] },
            "MiniMax-M3.1-Flash-Preview": { "reasoning": true, "reasoning_options": [{ "type": "effort", "values": ["low", "medium", "high", "xhigh", "max"] }] }
        }});
        for (preset, id, format, toggle, effort) in [
            ("moonshotai", "kimi-k2.6", "moonshot", true, false),
            ("moonshotai-cn", "kimi-k2.7-code", "moonshot", false, false),
            ("moonshotai", "kimi-k3", "kimi", false, true),
            ("zai-coding-plan", "glm-5.3", "zai", false, true),
            ("alibaba-cn", "qwen3.5-plus", "qwen", true, false),
            ("minimax-cn", "MiniMax-M3", "minimax", true, false),
            ("minimax-cn", "MiniMax-M3.1-Flash-Preview", "minimax", false, true),
        ] {
            let list = json!({ "data": [{ "id": id }] });
            let models = convert(&list, Some(&meta), None, preset);
            let config = json!({ "provider": { "p": { "api": DEFAULT_API,
                "options": { "baseURL": "https://example.test", "temperature": 0.7, "topP": 0.3 }, "models": models } } });
            let resolved = super::super::config::resolve_model(&config, Some(&format!("p/{id}")), &Default::default()).unwrap();
            assert_eq!(resolved.model.thinking_format.as_deref(), Some(format), "{id}");
            assert_eq!(resolved.model.supports_thinking_toggle, toggle, "{id}");
            assert_eq!(resolved.model.supports_reasoning_effort, effort, "{id}");
            assert_eq!(resolved.model.max_tokens_field, "max_tokens");
            assert!(!resolved.model.extra_options.contains_key("supportsThinkingToggle"));
            if id.starts_with("kimi") { assert_eq!(resolved.model.temperature, None); assert_eq!(resolved.model.top_p, None); }
            let gateway = convert(&list, Some(&meta), None, "openai-compatible");
            assert!(gateway[id].pointer("/options/thinkingFormat").is_none());
        }
    }

    #[test]
    fn live_list_and_openrouter_capabilities_override_the_catalog() {
        let meta = json!({ "models": { "old": { "reasoning": true }, "fresh": { "tool_call": false } } });
        assert!(convert(&json!({"data":[]}), Some(&meta), None, "openrouter").is_empty());
        let models = convert(&json!({ "data": [
            { "id": "fresh", "supported_parameters": ["tools", "reasoning"], "context_length": 1000000,
                "top_provider": { "max_completion_tokens": 64000 }, "architecture": { "input_modalities": ["text", "image"] },
                "reasoning": { "mandatory": true, "supported_efforts": ["none", "low", "high"], "default_effort": "high" } },
            { "id": "embedding", "supported_parameters": [] }
        ] }), Some(&meta), None, "openrouter");
        assert_eq!(models.len(), 1);
        assert_eq!(models["fresh"]["limit"]["output"], 64000);
        assert_eq!(models["fresh"]["modalities"]["input"], json!(["text", "image"]));
        assert_eq!(models["fresh"]["options"]["thinkingFormat"], "openrouter");
        assert_eq!(models["fresh"]["options"]["supportsThinkingToggle"], false);
        assert_eq!(models["fresh"]["options"]["reasoningEffort"], "high");
        assert!(models["fresh"]["variants"].get("none").is_none());
        assert!(models["fresh"]["variants"].get("high").is_some());
    }

    #[test]
    fn local_claude_1m_models_inherit_reasoning_and_remain_selectable() {
        let catalog = json!({"anthropic":{"models":{
            "claude-haiku-5-5":{"reasoning":true,"reasoning_options":[
                {"type":"effort","values":["low","medium","high","xhigh","max"]}
            ]},
            "claude-sonnet-4-6":{"reasoning":true,"reasoning_options":[
                {"type":"effort","values":["low","medium","high","max"]},{"type":"budget_tokens","min":1024}
            ]},
            "claude-haiku-4-5":{"reasoning":true,"reasoning_options":[{"type":"budget_tokens","min":1024}]}
        }}});
        for base in ["claude-haiku-5-5", "claude-sonnet-4-6", "claude-haiku-4-5"] {
            for suffix in ["[1m]", "-1m"] {
                let id = format!("{base}{suffix}");
                // 复现本地配置独有的 1M ID：/models 只返回原模型，变体由本地配置追加。
                let provider = json!({"api":"anthropic-messages","options":{"baseURL":"https://proxy.example"},
                    "models":{id.clone():{}}});
                let imported = convert_local(provider.clone(), &id, &json!({"data":[{"id":base}]}), Some(&catalog)).unwrap();
                let models = &imported["provider"]["models"];
                for field in ["reasoning", "variants", "options"] {
                    assert_eq!(models[&id][field], models[base][field], "{id}: {field}");
                }
                let config = json!({"provider":{"local":imported["provider"]}});
                let choices = super::super::config::model_options(&config);
                if base == "claude-haiku-4-5" {
                    assert!(models[&id].get("variants").is_none(), "无档位的原模型不能凭空补档位");
                    assert!(choices.iter().any(|o| o["value"] == format!("local/{id}")));
                    continue;
                }
                assert!(choices.iter().any(|o| o["value"] == format!("local/{id}/variant/high")));
                assert_eq!(models[&id]["options"]["thinkingFormat"], "anthropic");
                let default = if base == "claude-haiku-5-5" { "medium" } else { "high" };
                assert_eq!(imported["model"], format!("{id}/variant/{default}"));
                // 普通 provider 走相同继承路径；显式 API 能力优先于原模型。
                let mut entry = json!({"id":id,"supported_endpoints":["/messages"],"context_length":1_000_000});
                let direct = convert(&json!({"data":[entry]}), None, Some(&catalog), "commandcode");
                assert_eq!(direct[&id]["variants"], models[&id]["variants"]);
                assert_eq!(direct[&id]["options"], models[&id]["options"]);
                assert_eq!(direct[&id]["limit"]["context"], 1_000_000);
                entry["reasoning_options"] = json!([{"type":"effort","values":["max"]}]);
                let explicit = convert_local(provider, &id, &json!({"data":[entry]}), Some(&catalog)).unwrap();
                let model = &explicit["provider"]["models"][&id];
                assert_eq!(model["variants"], json!({"max":{"reasoningEffort":"max"}}));
                assert!(model["options"].get("reasoningEffort").is_none(), "不能下发显式能力以外的默认档位");
            }
        }
        let exact = json!({"anthropic":{"models":{
            "claude-haiku-5-5":{"reasoning":true}, "claude-haiku-5-5[1m]":{"reasoning":false}
        }}});
        assert_eq!(reasoning_catalog_model(Some(&exact), "claude-haiku-5-5[1m]").unwrap()["reasoning"], false);
    }

    #[test]
    fn protocols_and_claude_thinking_follow_model_capabilities() {
        let catalog = json!({ "anthropic": { "models": {
            "claude-opus-5-5": { "reasoning": true, "reasoning_options": [{"type":"effort","values":["low","medium","high","xhigh","max"]}] },
            "claude-opus-4-5": { "reasoning": true, "reasoning_options": [{"type":"effort","values":["low","medium","high"]},{"type":"budget_tokens","min":1024}] }
        } } });
        let models = convert(&json!({"data":[
            {"id":"gpt-6-astra","supported_endpoints":["/chat/completions","/responses"]},
            {"id":"gpt-6.1-sol","supported_endpoints":["/chat/completions"]},
            {"id":"claude-opus-5-5","supported_endpoints":["/messages"]},
            {"id":"claude-opus-4-5","supported_endpoints":["/messages"]}
        ]}), None, Some(&catalog), "commandcode");
        let fast = convert(&json!({"data":[{"id":"claude-opus-5-5-fast","supported_endpoints":["/messages"]}]}), None, Some(&catalog), "commandcode");
        assert_eq!(fast["claude-opus-5-5-fast"]["variants"]["high"]["reasoningEffort"], "high");
        let flash = convert(&json!({"data":[
            {"id":"deepseek/deepseek-v4-flash","context_length":1_000_000,"architecture":{"input_modalities":["text","image"]},"top_provider":{"max_completion_tokens":393216}},
            {"id":"deepseek/deepseek-v4-flash-fast","context_length":1_000_000}
        ]}), None, None, "commandcode");
        assert_eq!(flash["deepseek/deepseek-v4-flash-fast"]["modalities"], flash["deepseek/deepseek-v4-flash"]["modalities"]);
        assert_eq!(flash["deepseek/deepseek-v4-flash-fast"]["limit"]["output"], 393216);
        assert_eq!(models["gpt-6-astra"]["api"], "openai-responses");
        assert!(!models.contains_key("gpt-6.1-sol"));
        assert_eq!(models["claude-opus-5-5"]["options"]["thinkingFormat"], "anthropic");
        assert_eq!(models["claude-opus-5-5"]["options"]["reasoningEffort"], "medium");
        assert_eq!(models["claude-opus-5-5"]["options"]["supportsThinkingToggle"], false);
        assert!(models["claude-opus-4-5"].pointer("/options/thinkingFormat").is_none());
        let imported = convert_local(json!({"api":"anthropic-messages","options":{"baseURL":"https://proxy.example"},
            "models":{"claude-opus-5-5":{"options":{"thinkingFormat":"custom"}}}}), "claude-opus-5-5", &Value::Null, Some(&catalog)).unwrap();
        assert_eq!(imported["provider"]["models"]["claude-opus-5-5"]["options"]["thinkingFormat"], "custom");
        assert_eq!(imported["model"], "claude-opus-5-5/variant/medium");
    }
}
