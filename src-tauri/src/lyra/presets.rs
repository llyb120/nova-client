//! 自适应 provider：config.jsonc 里只写 `"preset"` 与 apiKey，Base URL、模型列表与
//! 每个模型的协议自动获得。模型列表拉取后缓存到 alkaid/models-cache.json，
//! load_config 时合并进 provider.models（手写的同名模型覆盖缓存），下游解析无需感知。

use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[path = "local_provider.rs"]
mod local_provider;

/// 订阅凭据只进入官方请求；不能沿用调用方可能跟随重定向的通用客户端。
fn local_models_request(http: &reqwest::Client, provider: &Value, env: &std::collections::HashMap<String, String>) -> Result<reqwest::RequestBuilder, String> {
    let source = provider["localAuth"].as_str();
    let base = match source {
        Some(source) => super::local_auth::endpoint(source)?,
        None => provider["options"]["baseURL"].as_str().unwrap_or(""),
    }.trim_end_matches('/');
    let url = reqwest::Url::parse(base).map_err(|_| "本地 API 的 Base URL 无效")?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("本地 API 的 Base URL 必须使用 HTTP 或 HTTPS".into());
    }
    let anthropic = provider["api"] == "anthropic-messages";
    let models_url = if anthropic && !base.ends_with("/v1") { format!("{base}/v1/models") } else { format!("{base}/models") };
    let mut request = http.get(models_url).timeout(Duration::from_secs(30));
    if source == Some("codex") {
        // ponytail: Codex 目录按 CLI 版本返回；上游变更契约时同步升级此版本及离线样例。
        request = request.query(&[("client_version", "0.134.0")]);
    }
    let key = auth.as_ref().map(|a| a.key.as_str()).unwrap_or_else(|| provider["options"]["apiKey"].as_str().unwrap_or(""));
    if !key.is_empty() {
        request = if anthropic && auth.is_none() { request.header("x-api-key", key) } else { request.bearer_auth(key) };
    }
    if anthropic { request = request.header("anthropic-version", "2023-06-01"); }
    let headers = auth.as_ref().map(|a| &a.headers).or_else(|| provider["options"]["headers"].as_object());
    if let Some(headers) = headers {
        for (name, value) in headers {
            if value.is_null() { continue; }
            let value = value.as_str().ok_or("本地 API 请求头必须是字符串")?;
            let name = reqwest::header::HeaderName::from_bytes(name.as_bytes()).map_err(|_| "本地 API 请求头名称无效")?;
            let value = reqwest::header::HeaderValue::from_str(value).map_err(|_| "本地 API 请求头值无效")?;
            request = request.header(name, value);
        }
    }
    Ok(request)
}

async fn local_model_pages(request: reqwest::RequestBuilder, codex: bool) -> Result<Value, String> {
    let mut entries = Vec::new();
    let mut cursors = std::collections::HashSet::new();
    let mut cursor = None;
    let mut remaining_bytes = 16_usize * 1024 * 1024;
    let mut pages = 0;
    loop {
        if pages == 1000 { return Err("模型列表超过 1000 页上限".into()); }
        pages += 1;
        let mut page = request.try_clone().ok_or("无法复制模型列表请求")?;
        if let Some(cursor) = &cursor { page = page.query(&[("after_id", cursor)]); }
        let mut response = page.send().await.map_err(|_| "无法连接本地配置的模型 API")?;
        // error_for_status 不拒绝 3xx；禁止把重定向或错误正文当目录，也不回显可能含凭据的 URL/正文。
        if !response.status().is_success() { return Err(format!("模型 API 返回 {}", response.status())); }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| "无法读取模型 API 响应")? {
            // 按解压后的实际字节累计，不能信任 Content-Length，也不能先 json()/bytes() 全量分配。
            remaining_bytes = remaining_bytes.checked_sub(chunk.len()).ok_or("模型列表累计响应超过 16 MiB 上限")?;
            body.extend_from_slice(&chunk);
        }
        let page: Value = serde_json::from_slice(&body).map_err(|_| "模型 API 返回的 JSON 无效")?;
        if codex {
            for model in page["models"].as_array().ok_or("模型 API 缺少 models 列表")? {
                if model["visibility"] == "hide" { continue; }
                let mut entry = json!({ "id": model["slug"], "name": model["display_name"],
                    "context_length": model["context_window"], "architecture": { "input_modalities": model["input_modalities"] } });
                if let Some(levels) = model["supported_reasoning_levels"].as_array() {
                    let efforts: Vec<&str> = levels.iter().filter_map(|level| level["effort"].as_str()).filter(|s| !s.trim().is_empty()).collect();
                    entry["reasoning"] = json!(!efforts.is_empty());
                    entry["reasoning_options"] = if efforts.is_empty() { json!([]) } else { json!([{ "type": "effort", "values": efforts }]) };
                }
                entries.push(entry);
            }
            break;
        }
        entries.extend(page["data"].as_array().ok_or("模型 API 缺少 data 列表")?.iter().cloned().map(|mut entry| {
            if entry["name"].is_null() && entry["display_name"].is_string() { entry["name"] = entry["display_name"].clone(); }
            entry
        }));
        if page["has_more"] != true { break; }
        let next = page["last_id"].as_str().filter(|id| !id.is_empty()).ok_or("模型列表分页缺少 last_id")?;
        if !cursors.insert(next.to_string()) { return Err("模型列表分页游标重复".to_string()); }
        cursor = Some(next.to_string());
    }
    Ok(json!({ "data": entries }))
}

async fn fetch_local_models(http: &reqwest::Client, provider: Value, catalog: &mut Option<Value>, env: &std::collections::HashMap<String, String>) -> Result<Map<String, Value>, String> {
    // 凭据读取是文件 IO/macOS 钥匙串（最长约 5s），不能阻塞 async 运行时。
    let request = {
        let (http, provider, env) = (http.clone(), provider.clone(), env.clone());
        tokio::task::spawn_blocking(move || local_models_request(&http, &provider, &env)).await
            .map_err(|_| "读取本地登录失败")??
    };
    let (list, meta) = tokio::join!(local_model_pages(request, provider["localAuth"] == "codex"), async {
        if catalog.is_none() { get_json(http, "https://models.dev/api.json", "").await.ok() } else { None }
    });
    if let Some(meta) = meta { *catalog = Some(meta); }
    // 失败交给统一刷新流程保留旧缓存，不能把完整列表缩回本地选中的一个模型。
    Ok(convert_local(provider, "", &list?, catalog.as_ref())?["provider"]["models"].as_object().unwrap().clone())
}

fn claude_alias_family(id: &str) -> Option<&'static str> {
    match id.strip_suffix("[1m]").unwrap_or(id) {
        "sonnet" => Some("sonnet"),
        "opus" | "opusplan" => Some("opus"),
        "haiku" => Some("haiku"),
        _ => None,
    }
}

fn convert_local(mut provider: Value, selected: &str, list: &Value, catalog: Option<&Value>) -> Result<Value, String> {
    let explicit = provider["models"].as_object().cloned().unwrap_or_default();
    let claude = provider["localAuth"] == "claude-code";
    let mut entries = list["data"].as_array().cloned().unwrap_or_default();
    for id in explicit.keys() {
        // CLI alias 不是 API 模型 ID；只能由成功拉取的真实目录生成映射。
        if claude && claude_alias_family(id).is_some() { continue; }
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
    let mut explicit = explicit;
    if claude {
        for (alias, local) in &mut explicit {
            let Some(family) = claude_alias_family(alias) else { continue };
            // /v1/models 的 created_at 决定新旧，不能从版本号/日期后缀猜测未发布的模型。
            let latest = entries.iter().filter_map(|entry| {
                let id = entry["id"].as_str()?;
                if !id.starts_with("claude-") || !id.split('-').any(|part| part == family) || !models.contains_key(id) { return None; }
                let created = chrono::DateTime::parse_from_rfc3339(entry["created_at"].as_str()?).ok()?;
                Some((created, id))
            }).max_by_key(|(created, _)| *created);
            if let Some((_, id)) = latest {
                let mut model = models[id].clone();
                model["localModel"] = json!(format!("{id}{}", if alias.ends_with("[1m]") { "[1m]" } else { "" }));
                if alias.ends_with("[1m]") {
                    if !model["limit"].is_object() { model["limit"] = json!({}); }
                    model["limit"]["context"] = json!(1_000_000);
                }
                models.insert(alias.clone(), model);
            }
            // 旧缓存或手写 localModel 不能覆盖本次目录产生的映射。
            if let Some(local) = local.as_object_mut() { local.remove("localModel"); }
        }
        explicit.retain(|id, _| claude_alias_family(id).is_none() || models.contains_key(id));
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
        for (field, value) in local.as_object().into_iter().flatten() {
            if let Some(values) = value.as_object() {
                let mut merged = model[field].as_object().cloned().unwrap_or_default();
                merged.extend(values.clone());
                model[field] = json!(merged);
            } else { model[field] = value.clone(); }
        }
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
    preset("server-support", "server support", "", false),
    preset("local-codex", "本地 Codex", "", false),
    preset("local-claude-code", "本地 Claude Code", "", false),
    preset("devin", "Devin（本地 API）", "", false),
    preset("codebuddy", "CodeBuddy（本地 API）", "", false),
    preset("workbuddy", "WorkBuddy（本地 API）", "", false),
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
        "server-support" => Some("server-support"),
        "local-codex" => Some("codex"),
        "local-claude-code" => Some("claude-code"),
        "devin" => Some("devin"),
        "codebuddy" => Some("codebuddy"),
        "workbuddy" => Some("workbuddy"),
        _ => None,
    }
}

fn resolve_local(provider: &Value, source: &str, env: &std::collections::HashMap<String, String>, nova_root: &Path) -> Result<Value, String> {
    let mut local = if source == "server-support" {
        let settings = crate::settings::Settings::load(&nova_root.to_path_buf());
        let server = crate::relay::resolve_relay_server(&settings.relay_server);
        let token = settings.relay_token.trim();
        if server.is_empty() || token.is_empty() {
            return Err("请先在团队设置中保存中转地址和团队 token".into());
        }
        let url = reqwest::Url::parse(&server).map_err(|_| "团队中转地址无效")?;
        if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() || url.query().is_some() || url.fragment().is_some() {
            return Err("团队中转地址必须是 HTTP(S) 地址，不能包含查询参数或片段".into());
        }
        json!({ "name": "server support", "api": DEFAULT_API,
            "options": { "baseURL": format!("{server}/cc"), "apiKey": token }, "models": {} })
    } else if source == "devin" {
        super::devin_provider::load(nova_root, env)?
    } else if matches!(source, "codebuddy" | "workbuddy") {
        super::codebuddy_provider::load(nova_root, env, source)
            .map_err(|error| if source == "workbuddy" { error.replace("CodeBuddy", "WorkBuddy") } else { error })?
    } else {
        local_provider::load(source, env)?.0
    };
    let subscription = local["localAuth"].is_string();
    let mut options = local["options"].as_object().cloned().unwrap_or_default();
    let mut headers = options.get("headers").and_then(Value::as_object).cloned().unwrap_or_default();
    if let Some(explicit) = provider["options"].as_object() {
        // Key 始终跟随本地认证；订阅的官方地址/请求头也只能来自 load。
        options.extend(explicit.iter().filter(|(key, _)| key.as_str() != "apiKey"
            && !(subscription && matches!(key.as_str(), "baseURL" | "baseUrl" | "headers"))).map(|(k, v)| (k.clone(), v.clone())));
        if !subscription {
            if let Some(explicit) = explicit.get("headers").and_then(Value::as_object) { headers.extend(explicit.clone()); }
        }
    }
    options.insert("headers".into(), json!(headers));
    let models = local["models"].clone();
    if let Some(explicit) = provider.as_object() {
        // localAuth 必须由本次 load 决定；切回自定义 API 时丢弃旧标记，不能带着订阅 token 直连。
        local.as_object_mut().unwrap().extend(explicit.iter().filter(|(key, _)| key.as_str() != "localAuth"
            && !(subscription && key.as_str() == "api")).map(|(k, v)| (k.clone(), v.clone())));
    }
    local["options"] = json!(options);
    local["models"] = json!(models);
    if local["localAuth"] == "claude-code" {
        // 手写 alias 也需要目录映射，但不导入其它旧缓存模型作为显式模型。
        if let Some(explicit) = provider["models"].as_object() {
            let aliases = explicit.iter().filter(|(id, _)| claude_alias_family(id).is_some()).map(|(id, model)| (id.clone(), model.clone())).collect();
            merge_local_models(local["models"].as_object_mut().unwrap(), &aliases);
        }
        for model in local["models"].as_object_mut().unwrap().values_mut() {
            if let Some(model) = model.as_object_mut() { model.remove("localModel"); }
        }
    }
    if matches!(source, "devin" | "codebuddy") { local["api"] = json!(source); }
    if source == "workbuddy" { local["api"] = json!("codebuddy"); }
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
            match resolve_local(provider, source, &env, nova_root) {
                Ok(mut local) => {
                    let mut models = cache[id.as_str()]["models"].as_object().cloned().unwrap_or_default();
                    merge_local_models(&mut models, local["models"].as_object().unwrap());
                    if local["localAuth"] == "claude-code" {
                        let mut explicit = explicit;
                        for (id, model) in &mut explicit {
                            if claude_alias_family(id).is_some() {
                                if let Some(model) = model.as_object_mut() { model.remove("localModel"); }
                            }
                        }
                        merge_local_models(&mut models, &explicit);
                        // 未获取目录的 alias 保留选择，但没有 localModel；stream 必须拒绝而非发送 alias。
                    } else {
                        // 迁移项只声明原模型/档位；不能用这个稀疏定义抹掉目录中的模态和限额。
                        merge_local_models(&mut models, &explicit);
                    }
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

fn local_auth_fingerprint(source: &str, env: &std::collections::HashMap<String, String>) -> Result<String, String> {
    let auth = super::local_auth::credentials(source, env)?;
    // 仅哈希进入缓存指纹；账号 routing headers 或登录 token 变化都必须使 TTL 失效。
    Ok(fingerprint(&[&auth.key, &json!(auth.headers).to_string()]))
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
            local = match resolve_local(provider, source, &env, nova_root) {
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
        let auth_print = if let Some(source) = provider["localAuth"].as_str() {
            let (source, env) = (source.to_string(), env.clone());
            match tokio::task::spawn_blocking(move || local_auth_fingerprint(&source, &env)).await
                .unwrap_or_else(|_| Err("读取本地登录失败".into())) {
                Ok(print) => print,
                Err(error) => {
                    if force { errors.push(format!("{id}：{error}")); }
                    else { eprintln!("[lyra] {id}：{error}"); }
                    continue; // 登录失效仍保留旧目录，但不能以 TTL 新鲜为由跳过认证检查。
                }
            }
        } else { String::new() };
        let print = fingerprint(&["10", preset.id, &base_url, &api_key, &headers, &local_models,
            provider["api"].as_str().unwrap_or(""), provider["localAuth"].as_str().unwrap_or(""), &auth_print]);
        if !force && cache_fresh(&cache[id.as_str()], &print, now) {
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
        let result = if preset.id == "devin" {
            super::devin_provider::models(&provider["options"], force).await
        } else if matches!(preset.id, "codebuddy" | "workbuddy") {
            super::codebuddy_provider::models(&provider["options"], force).await
        } else if local_source(preset.id).is_some() && preset.id != "server-support" {
            fetch_local_models(http, provider.clone(), &mut models_dev, &env).await
        } else {
            fetch_models(http, preset, &base_url, &api_key, &mut models_dev).await
        };
        match result {
            Ok(models) => {
                let no_catalog = models_dev.is_none();
                let mut models = models;
                // 目录没拉到时，降级结果不能抹掉之前缓存里的能力（思考强度等）；按同名模型沿用，短 TTL 后重试。
                if no_catalog && cache[id.as_str()]["noCatalog"] != true {
                    for (name, model) in models.iter_mut() {
                        if let Some(old) = cache[id.as_str()]["models"].get(name) {
                            for field in ["reasoning", "options", "variants"] {
                                if model.get(field).is_none() && old.get(field).is_some() { model[field] = old[field].clone(); }
                            }
                        }
                    }
                }
                cache[id.as_str()] = json!({ "fingerprint": print, "fetchedAt": now, "models": models, "noCatalog": no_catalog });
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

const NO_CATALOG_CACHE_TTL_SECS: u64 = 5 * 60;

fn cache_fresh(entry: &Value, print: &str, now: u64) -> bool {
    let ttl = if entry["noCatalog"] == true { NO_CATALOG_CACHE_TTL_SECS } else { CACHE_TTL_SECS };
    entry["fingerprint"] == print && now.saturating_sub(entry["fetchedAt"].as_u64().unwrap_or(0)) < ttl
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
        let catalog_id = if preset.id == "server-support" { "commandcode" } else { preset.id };
        return Ok(convert(&list?, None, models_dev.as_ref(), catalog_id));
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
    // 兼容端点也优先使用自身的目录条目；其他渠道的输出上限不代表当前端点。
    let meta = meta.or_else(|| catalog.and_then(|all| all.get(preset_id)));
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
    // 变体（如 deepseek-v4-flash-fast）常缺模态、输出上限等元数据，缺什么沿用原模型；1M 变体的上下文按变体本身（缺省 1M）。
    let ids: Vec<String> = out.keys().cloned().collect();
    for id in ids {
        let model = out.get_mut(&id).unwrap();
        if (id.ends_with("[1m]") || id.ends_with("-1m")) && model.pointer("/limit/context").is_none() {
            if !model["limit"].is_object() { model["limit"] = json!({}); }
            model["limit"]["context"] = json!(1_000_000);
        }
        let Some(base) = variant_base(&id).and_then(|b| out.get(b)).cloned() else { continue };
        let model = out.get_mut(&id).unwrap();
        for field in ["modalities", "reasoning", "options", "variants"] {
            if model.get(field).is_none() {
                if let Some(value) = base.get(field) { model[field] = value.clone(); }
            }
        }
        for key in ["context", "output"] {
            if model.pointer(&format!("/limit/{key}")).is_none() {
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

    fn subscription_env(source: &str, root: &Path) -> std::collections::HashMap<String, String> {
        let variable = if source == "codex" {
            use base64::Engine as _;
            let expires = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() + 3600;
            let token = format!("e30.{}.subscription-secret", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json!({"exp":expires}).to_string()));
            std::fs::write(root.join("auth.json"), json!({"tokens": {
                "access_token": token, "account_id": "private-account"
            }}).to_string()).unwrap();
            "CODEX_HOME"
        } else {
            std::fs::write(root.join("settings.json"), json!({"env": {
                "CLAUDE_CODE_OAUTH_TOKEN": "sk-ant-oat-subscription-secret"
            }}).to_string()).unwrap();
            "CLAUDE_CONFIG_DIR"
        };
        std::collections::HashMap::from([(variable.into(), root.to_string_lossy().into_owned())])
    }

    #[test]
    fn subscription_merge_pins_transport_and_drops_stale_auth_for_direct_apis() {
        for source in ["codex", "claude-code"] {
            let dir = tempfile::tempdir().unwrap();
            let cli = dir.path().join("cli");
            std::fs::create_dir(&cli).unwrap();
            let env = subscription_env(source, &cli);
            let mut raw = json!({"preset":format!("local-{source}"),"localAuth":"untrusted","api":"openai-completions",
                "options":{"baseURL":"https://untrusted.invalid","baseUrl":"https://alias.invalid", "apiKey":"stale-key",
                    "headers":{"Authorization":"Bearer stale-key","Host":"untrusted.invalid"},"temperature":0.5},
                "models":{"sonnet":{"localModel":"untrusted-model","options":{"reasoningEffort":"high"}}}});
            {
                let local = resolve_local(&raw, source, &env, dir.path()).unwrap();
                assert_eq!(local["localAuth"], source);
                assert_eq!(local["api"], if source == "codex" { "openai-responses" } else { "anthropic-messages" });
                assert_eq!(local["options"]["baseURL"], super::super::local_auth::endpoint(source).unwrap());
                assert!(local["options"]["baseUrl"].is_null());
                assert_eq!(local["options"]["apiKey"], "");
                assert_eq!(local["options"]["headers"], json!({}));
                assert_eq!(local["options"]["temperature"], 0.5);
                assert!(local["options"]["proxy"].is_null(), "Lyra 跟随系统代理，不再固化代理");
                assert!(local["models"]["sonnet"]["localModel"].is_null());
                assert!(!local.to_string().contains("subscription-secret"));
            }
            if source == "codex" {
                std::fs::write(cli.join("config.toml"), "[model_providers.openai]\nbase_url='http://127.0.0.1:1234/v1'\n").unwrap();
            } else {
                std::fs::write(cli.join("settings.json"), r#"{"env":{"ANTHROPIC_BASE_URL":"http://127.0.0.1:1234","ANTHROPIC_API_KEY":"direct-key"}}"#).unwrap();
            }
            raw["localAuth"] = json!(source);
            let direct = resolve_local(&raw, source, &env, dir.path()).unwrap();
            assert!(direct.get("localAuth").is_none(), "切换到直连不得保留订阅标记");
            assert_eq!(direct["api"], "openai-completions");
            assert_eq!(direct["options"]["baseURL"], "https://untrusted.invalid");
            assert_eq!(direct["options"]["headers"]["Host"], "untrusted.invalid");
            assert_ne!(direct["options"]["apiKey"], "stale-key");
        }
    }

    // 先构造生产请求并核对官方 pin，然后仅在测试中改为 loopback；保留生产客户端的代理/redirect 策略。
    fn mock_subscription_request(provider: &Value, env: &std::collections::HashMap<String, String>, address: std::net::SocketAddr) -> reqwest::RequestBuilder {
        let mut untrusted = provider.clone();
        untrusted["api"] = json!("openai-completions");
        untrusted["options"]["baseURL"] = json!("https://untrusted.invalid");
        untrusted["options"]["apiKey"] = json!("stale-key");
        untrusted["options"]["headers"] = json!({"Host":"untrusted.invalid","Authorization":"Bearer stale-key"});
        let (client, request) = local_models_request(&reqwest::Client::new(), &untrusted, env).unwrap().build_split();
        let mut request = request.unwrap();
        let expected = if provider["localAuth"] == "codex" { "https://chatgpt.com/backend-api/codex/models?client_version=0.134.0" }
            else { "https://api.anthropic.com/v1/models" };
        assert_eq!(request.url().as_str(), expected);
        let mut url = reqwest::Url::parse(&format!("http://{address}")).unwrap();
        url.set_path(request.url().path());
        url.set_query(request.url().query());
        *request.url_mut() = url;
        reqwest::RequestBuilder::from_parts(client, request)
    }

    #[tokio::test]
    async fn subscription_catalogs_use_bearer_headers_pagination_and_never_leak_or_redirect() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        for source in ["codex", "claude-code"] {
            let dir = tempfile::tempdir().unwrap();
            let env = subscription_env(source, dir.path());
            let provider = resolve_local(&json!({}), source, &env, dir.path()).unwrap();
            let credentials = super::super::local_auth::credentials(source, &env).unwrap();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let codex = source == "codex";
            let server = tokio::spawn(async move {
                let pages = if codex { vec![json!({"models":[
                    {"slug":"gpt-test","display_name":"GPT Test","context_window":272000,
                        "supported_reasoning_levels":[{"effort":"low"},{"effort":"xhigh"}],"input_modalities":["text","image"],"visibility":"list"},
                    {"slug":"no-thinking","supported_reasoning_levels":[],"visibility":"list"},
                    {"slug":"hidden","visibility":"hide"}
                ]})] } else { vec![
                    json!({"data":[{"id":"claude-sonnet-new","display_name":"Sonnet New","created_at":"2026-01-01T00:00:00Z"}],"has_more":true,"last_id":"claude-sonnet-new"}),
                    json!({"data":[{"id":"claude-sonnet-old","display_name":"Sonnet Old","created_at":"2025-01-01T00:00:00Z"}],"has_more":false})
                ] };
                for page in 0..pages.len() + 3 {
                    let (mut socket, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept()).await.unwrap().unwrap();
                    let mut request = Vec::new();
                    let mut buf = [0; 1024];
                    while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                        let n = socket.read(&mut buf).await.unwrap();
                        assert!(n > 0);
                        request.extend_from_slice(&buf[..n]);
                    }
                    let request = String::from_utf8(request).unwrap().to_ascii_lowercase();
                    let path = if codex { "/backend-api/codex/models?client_version=0.134.0" }
                        else if page == 1 { "/v1/models?after_id=claude-sonnet-new" } else { "/v1/models" };
                    assert!(request.starts_with(&format!("get {path} ")), "unexpected request: {}", request.lines().next().unwrap());
                    assert_eq!(request.matches(&format!("authorization: bearer {}\r\n", credentials.key.to_ascii_lowercase())).count(), 1);
                    assert!(!request.contains("x-api-key:") && !request.contains("stale-key") && !request.contains("untrusted.invalid"));
                    for (name, value) in &credentials.headers {
                        assert!(request.contains(&format!("{}: {}\r\n", name.to_ascii_lowercase(), value.as_str().unwrap().to_ascii_lowercase())));
                    }
                    if !codex {
                        assert_eq!(request.matches("anthropic-version: 2023-06-01\r\n").count(), 1);
                        assert!(request.contains("anthropic-beta:") && request.contains("oauth-"));
                    }
                    let (status, extra, body) = if page < pages.len() {
                        ("200 OK", String::new(), pages[page].to_string())
                    } else if page == pages.len() {
                        ("307 Temporary Redirect", format!("location: http://{address}/leak?token=subscription-secret\r\n"), String::new())
                    } else if page == pages.len() + 1 {
                        ("503 Unavailable", String::new(), "subscription-secret".into())
                    } else { ("200 OK", String::new(), "subscription-secret".into()) };
                    socket.write_all(format!("HTTP/1.1 {status}\r\n{extra}content-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
                }
            });
            let list = tokio::time::timeout(Duration::from_secs(5), local_model_pages(
                mock_subscription_request(&provider, &env, address), codex
            )).await.unwrap().unwrap();
            let catalog = json!({"anthropic":{"models":{"claude-sonnet-new":{
                "reasoning":true,"reasoning_options":[{"type":"effort","values":["low","high"]}]
            }}}});
            let converted = convert_local(provider.clone(), "", &list, Some(&catalog)).unwrap();
            let models = &converted["provider"]["models"];
            if codex {
                assert_eq!(models.as_object().unwrap().len(), 2);
                assert_eq!(models["gpt-test"]["name"], "GPT Test");
                assert_eq!(models["gpt-test"]["limit"]["context"], 272000);
                assert_eq!(models["gpt-test"]["modalities"]["input"], json!(["text","image"]));
                assert_eq!(models["gpt-test"]["variants"]["xhigh"]["reasoningEffort"], "xhigh");
                assert_eq!(models["gpt-test"]["options"]["supportsThinkingToggle"], false);
                assert!(models["no-thinking"]["reasoning"].is_null());
                assert!(models["hidden"].is_null());
            } else {
                assert_eq!(models["claude-sonnet-new"]["name"], "Sonnet New");
                assert!(models["claude-sonnet-old"].is_object());
                assert_eq!(models["sonnet"]["localModel"], "claude-sonnet-new");
                assert_eq!(models["sonnet"]["variants"]["low"]["reasoningEffort"], "low");
            }
            std::fs::create_dir(dir.path().join("alkaid")).unwrap();
            std::fs::write(cache_path(dir.path()), json!({"local":{"models":models}}).to_string()).unwrap();
            for serialized in [provider.to_string(), converted.to_string(), read_cache(dir.path()).to_string()] {
                assert!(!serialized.contains("subscription-secret") && !serialized.contains("private-account"));
            }
            for expected in ["307", "503", "JSON"] {
                let error = tokio::time::timeout(Duration::from_secs(5), local_model_pages(
                    mock_subscription_request(&provider, &env, address), codex
                )).await.unwrap().unwrap_err();
                assert!(error.contains(expected), "{error}");
                assert!(!error.contains("subscription-secret") && !error.contains("/leak"));
            }
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn local_model_pages_enforce_cumulative_bytes_and_page_limits() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        for (pages, padding, expected) in [(2, 8 * 1024 * 1024, "16 MiB"), (1000, 0, "1000 页")] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}/models", listener.local_addr().unwrap());
            let server = tokio::spawn(async move {
                for page in 0..pages {
                    let (mut socket, _) = listener.accept().await.unwrap();
                    let mut request = Vec::new();
                    let mut buf = [0; 1024];
                    while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                        let n = socket.read(&mut buf).await.unwrap();
                        assert!(n > 0);
                        request.extend_from_slice(&buf[..n]);
                    }
                    let body = format!("{}{}", json!({"data":[],"has_more":true,"last_id":page.to_string()}), " ".repeat(padding));
                    // 超限时客户端会提前关闭，write_all 失败是预期行为。
                    let _ = socket.write_all(format!("HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).as_bytes()).await;
                }
            });
            let error = tokio::time::timeout(Duration::from_secs(15), local_model_pages(
                reqwest::Client::builder().no_proxy().build().unwrap().get(url), false
            )).await.unwrap().unwrap_err();
            assert!(error.contains(expected), "{error}");
            server.await.unwrap();
        }
    }

    #[test]
    fn subscription_fingerprint_tracks_login_and_account_without_persisting_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let env = subscription_env("codex", dir.path());
        let path = dir.path().join("auth.json");
        let mut auth: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let original = local_auth_fingerprint("codex", &env).unwrap();
        let cached = json!({"fingerprint":original,"fetchedAt":1000});
        assert!(cache_fresh(&cached, &original, 1001));
        for (field, value) in [("account_id", "other-account".to_string()),
            ("access_token", format!("{}-rotated", auth["tokens"]["access_token"].as_str().unwrap()))] {
            let mut changed = auth.clone();
            changed["tokens"][field] = json!(value);
            std::fs::write(&path, changed.to_string()).unwrap();
            let print = local_auth_fingerprint("codex", &env).unwrap();
            assert!(!cache_fresh(&cached, &print, 1001));
            assert!(!print.contains("subscription-secret") && !print.contains("account"));
        }
        auth["tokens"]["access_token"] = json!("expired");
        std::fs::write(&path, auth.to_string()).unwrap();
        assert!(local_auth_fingerprint("codex", &env).is_err());
        let provider = resolve_local(&json!({}), "codex", &env, dir.path()).unwrap();
        assert_eq!(provider["models"], json!({}), "无 config.model 不生成虚构模型");
        assert!(convert_local(provider, "", &json!({"data":[]}), None).unwrap_err().contains("未返回可用模型"));
    }

    #[test]
    fn claude_subscription_aliases_use_catalog_dates_and_preserve_effort_and_1m() {
        let mut provider = json!({"localAuth":"claude-code","api":"anthropic-messages", "options":{"baseURL":"https://api.anthropic.com"},"models":{}});
        for alias in ["sonnet","opus","opusplan","haiku","sonnet[1m]","opus[1m]","opusplan[1m]","haiku[1m]"] {
            provider["models"][alias] = json!({"localModel":"stale-model", "options":{"reasoningEffort":"high"},
                "variants":{"high":{"reasoningEffort":"high"}}});
        }
        let list = json!({"data":[
            {"id":"claude-sonnet-99","created_at":"2024-01-01T00:00:00Z"},
            {"id":"claude-sonnet-2","created_at":"2025-01-01T00:00:00Z"},
            {"id":"claude-sonnet-100"},
            {"id":"claude-opus-2","created_at":"2025-02-01T00:00:00Z"},
            {"id":"claude-opus-99","created_at":"2024-02-01T00:00:00Z"},
            {"id":"claude-haiku-2","created_at":"2025-01-01T00:00:00Z"}
        ]});
        let catalog = json!({"anthropic":{"models":{"claude-sonnet-2":{
            "reasoning":true,"limit":{"context":200000,"output":64000},"modalities":{"input":["text","image"]},
            "reasoning_options":[{"type":"effort","values":["low","high"]}]
        }}}});
        let converted = convert_local(provider.clone(), "sonnet[1m]", &list, Some(&catalog)).unwrap();
        let models = converted["provider"]["models"].as_object().unwrap();
        for alias in provider["models"].as_object().unwrap().keys() {
            let family = claude_alias_family(alias).unwrap();
            assert_eq!(models[alias]["localModel"], format!("claude-{family}-2{}", if alias.ends_with("[1m]") { "[1m]" } else { "" }));
            assert_eq!(models[alias]["options"]["reasoningEffort"], "high");
        }
        assert_eq!(models["sonnet[1m]"]["limit"], json!({"context":1000000,"output":64000}));
        assert_eq!(models["sonnet"]["variants"]["low"]["reasoningEffort"], "low");
        assert_eq!(models["sonnet"]["modalities"]["input"], json!(["text","image"]));
        let config = json!({"provider":{"local":converted["provider"]}});
        let resolved = super::super::config::resolve_model(&config, Some("local/sonnet[1m]/variant/high"), &Default::default()).unwrap();
        assert_eq!(resolved.model.id, "claude-sonnet-2[1m]");
        assert_eq!(resolved.thinking_level.as_deref(), Some("high"));
        let mut expanded = models.clone();
        let mut explicit = provider["models"].as_object().unwrap().clone();
        for model in explicit.values_mut() { model.as_object_mut().unwrap().remove("localModel"); }
        merge_local_models(&mut expanded, &explicit);
        assert_eq!(expanded, *models, "本地 effort 占位不能抹掉目录映射/能力");
        let unavailable = convert_local(provider.clone(), "", &json!({"data":[{"id":"other"}]}), None).unwrap();
        assert_eq!(unavailable["provider"]["models"], json!({"other":{}}));
        assert!(convert_local(provider, "", &json!({"data":[]}), None).is_err());
        let direct = convert_local(json!({"api":"anthropic-messages","options":{},"models":{"sonnet":{}}}), "", &Value::Null, None).unwrap();
        assert_eq!(direct["provider"]["models"], json!({"sonnet":{}}), "自定义直连 alias 原样保留");
    }

    #[test]
    fn partial_native_model_options_preserve_catalog_capabilities() {
        let mut models = json!({"alias":{"reasoning":true,"limit":{"context":1000000,"output":64000},
            "modalities":{"input":["text","image"]},"options":{"supportsThinkingToggle":false}}}).as_object().unwrap().clone();
        merge_local_models(&mut models,json!({"alias":{"limit":{"output":2048}}}).as_object().unwrap());
        assert_eq!(models["alias"]["limit"]["output"],2048);
        assert_eq!(models["alias"]["limit"]["context"],1000000);
        assert_eq!(models["alias"]["reasoning"],true);
        assert_eq!(models["alias"]["options"]["supportsThinkingToggle"],false);
        assert_eq!(models["alias"]["modalities"]["input"],json!(["text","image"]));
    }

    #[test]
    fn sparse_local_models_keep_cached_capabilities_through_expand() {
        const CHILD: &str = "NOVA_PRESETS_SPARSE_MODEL_TEST_ROOT";
        if std::env::var_os(CHILD).is_none() {
            // expand reads process_env: isolate fixtures without racing other tests or reading user credentials.
            let root = tempfile::tempdir().unwrap();
            let mut child = std::process::Command::new(std::env::current_exe().unwrap());
            child.args(["--exact", "lyra::presets::tests::sparse_local_models_keep_cached_capabilities_through_expand", "--nocapture"])
                .env_clear().env(CHILD, root.path());
            for key in ["HOME", "USERPROFILE", "CODEX_HOME", "CLAUDE_CONFIG_DIR", "NOVA_DATA_DIR"] {
                child.env(key, root.path());
            }
            if let Some(system_root) = std::env::var_os("SystemRoot") { child.env("SystemRoot", system_root); }
            assert!(child.status().unwrap().success());
            return;
        }
        let root = PathBuf::from(std::env::var_os(CHILD).unwrap());
        std::fs::write(root.join("config.toml"), "model_provider='fixture'\n[model_providers.fixture]\nbase_url='http://127.0.0.1:9/v1'\nexperimental_bearer_token='fixture-key'\n").unwrap();
        std::fs::write(root.join("settings.json"), r#"{"env":{"ANTHROPIC_BASE_URL":"http://127.0.0.1:9","ANTHROPIC_API_KEY":"fixture-key"}}"#).unwrap();
        let cached = json!({"m":{
            "limit":{"context":200000,"output":64000}, "modalities":{"input":["text","image"]},
            "reasoning":true, "options":{"supportsThinkingToggle":false},
            "variants":{"low":{"reasoningEffort":"low"}}
        }});
        std::fs::create_dir(root.join("alkaid")).unwrap();
        std::fs::write(cache_path(&root), json!({"local":{"models":cached}}).to_string()).unwrap();
        for source in ["codex", "claude-code"] {
            let mut config = json!({"provider":{"local":{"preset":format!("local-{source}"),
                "models":{"m":{"variants":{"high":{"reasoningEffort":"high"}},"limit":{"output":2048}}}
            }}});
            expand(&mut config, &root);
            let model = &config["provider"]["local"]["models"]["m"];
            assert_eq!(model["limit"], json!({"context":200000,"output":2048}), "{source}");
            for field in ["modalities", "reasoning", "options"] {
                assert_eq!(model[field], cached["m"][field], "{source}: {field}");
            }
            assert_eq!(model["variants"], json!({"low":{"reasoningEffort":"low"},"high":{"reasoningEffort":"high"}}));
            let selected = super::super::config::resolve_model(&config, Some("local/m/variant/high"), &Default::default()).unwrap();
            assert_eq!(selected.model.context_window, 200000);
            assert_eq!(selected.model.max_output_tokens, 2048);
            assert!(selected.model.supports_images);
            assert_eq!(selected.thinking_level.as_deref(), Some("high"));
        }
    }

    #[test]
    fn local_cli_apis_use_native_transport_and_cli_model_limits() {
        for source in ["devin", "codebuddy"] {
            let dir = tempfile::tempdir().unwrap();
            std::fs::create_dir(dir.path().join("devin")).unwrap();
            std::fs::write(dir.path().join("devin/credentials.toml"), "windsurf_api_key = 'native-key'\n").unwrap();
            let path = dir.path().to_string_lossy().to_string();
            let env = std::collections::HashMap::from([
                ("APPDATA".into(), path.clone()), ("XDG_CONFIG_HOME".into(), path.clone()),
                ("HOME".into(), path.clone()), ("USERPROFILE".into(), path.clone()),
                ("CODEBUDDY_CONFIG_DIR".into(), path), ("CODEBUDDY_AUTH_TOKEN".into(), "native-key".into()),
            ]);
            let list = json!({ "data": [{ "id": "gpt-5-5-high", "name": "GPT-5.5 High Thinking",
                "context_length": 272000, "top_provider": { "max_completion_tokens": 128000 }, "reasoning_options": [] }] });
            let catalog = json!({ "openai": { "npm": "@ai-sdk/openai", "models": {
                "gpt-5-5-high": { "reasoning": true, "reasoning_options": [{ "type": "effort", "values": ["low", "high"] }] }
            } } });
            let models = convert(&list, None, Some(&catalog), source);
            let raw = json!({ "provider": { (source): { "preset": source, "options": { "apiKey": "stale-key" }, "models": models } } });
            let mut local = resolve_local(&raw["provider"][source], source, &env, dir.path()).unwrap();
            local["models"] = raw["provider"][source]["models"].clone();
            let config = json!({ "provider": { (source): local } });
            let resolved = super::super::config::resolve_model(&config, Some(&format!("{source}/gpt-5-5-high")), &Default::default()).unwrap();
            assert!(resolved.model.base_url.starts_with("https://"));
            assert_eq!(resolved.model.api, source);
            assert!(!resolved.api_key.is_empty());
            assert_ne!(resolved.api_key, "stale-key");
            if source == "codebuddy" { assert!(resolved.model.requires_reasoning_content); }
            assert_eq!(resolved.model.context_window, 272000);
            assert_eq!(resolved.model.max_output_tokens, 128000);
            assert!(!resolved.model.supports_reasoning_effort);
            assert!(!resolved.model.supports_thinking_toggle);
            assert!(config["provider"][source]["models"]["gpt-5-5-high"].get("variants").is_none());
            assert_eq!(super::list().as_array().unwrap().iter().find(|p| p["id"] == source).unwrap()["local"], true);
            assert_eq!(resolved.api_key, "native-key");
            assert!(!dir.path().join("runtime").exists(), "native loading must not unpack or launch Node");
        }
    }

    #[test]
    fn server_support_follows_team_settings_and_preserves_model_protocols() {
        let dir = tempfile::tempdir().unwrap();
        let raw = json!({ "provider": { "team": { "preset": "server-support", "options": { "apiKey": "stale" } } } });
        let mut config = raw.clone();
        expand(&mut config, dir.path());
        assert_eq!(config["provider"]["team"]["models"], json!({}));
        assert!(resolve_local(&raw["provider"]["team"], "server-support", &Default::default(), dir.path()).is_err());
        std::fs::create_dir_all(dir.path().join("alkaid")).unwrap();
        let model_list = json!({ "data": [
            { "id": "custom-chat", "supported_endpoints": ["/chat/completions"] },
            { "id": "custom-responses", "supported_endpoints": ["/responses"] },
            { "id": "custom-claude", "supported_endpoints": ["/messages"] }
        ] });
        let models = convert(&model_list, None, None, "commandcode");
        std::fs::write(cache_path(dir.path()), json!({ "team": { "models": models } }).to_string()).unwrap();
        for token in ["alice/first", "alice/rotated"] {
            std::fs::write(dir.path().join("settings.json"), json!({ "relayServer": " http://localhost:8320/ ", "relayToken": token }).to_string()).unwrap();
            let mut config = raw.clone();
            expand(&mut config, dir.path());
            let provider = &config["provider"]["team"];
            assert_eq!(provider["options"]["baseURL"], "http://localhost:8320/cc");
            assert_eq!(provider["options"]["apiKey"], token);
            assert_eq!(provider["name"], "server support");
            assert_eq!(provider["models"].as_object().unwrap().len(), 3);
            for (id, api) in [("custom-chat", DEFAULT_API), ("custom-responses", "openai-responses"), ("custom-claude", "anthropic-messages")] {
                let resolved = super::super::config::resolve_model(&config, Some(&format!("team/{id}")), &Default::default()).unwrap();
                assert_eq!(resolved.api_key, token);
                assert_eq!(resolved.model.api, api);
                assert!(resolved.model.command_code);
            }
        }
        assert_eq!(raw["provider"]["team"]["options"]["apiKey"], "stale", "runtime credentials are not persisted into provider config");
        assert!(list().as_array().unwrap().iter().any(|p| p["id"] == "server-support" && p["local"] == true));
    }

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
            let provider = resolve_local(&config, source, &env, dir.path()).unwrap();
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
    fn degraded_cache_without_model_catalog_expires_quickly() {
        let fresh = json!({ "fingerprint": "p", "fetchedAt": 1000, "models": {} });
        assert!(cache_fresh(&fresh, "p", 1000 + CACHE_TTL_SECS - 1));
        assert!(!cache_fresh(&fresh, "p", 1000 + CACHE_TTL_SECS));
        assert!(!cache_fresh(&fresh, "q", 1000));
        let degraded = json!({ "fingerprint": "p", "fetchedAt": 1000, "noCatalog": true, "models": {} });
        assert!(cache_fresh(&degraded, "p", 1000 + NO_CATALOG_CACHE_TTL_SECS - 1));
        assert!(!cache_fresh(&degraded, "p", 1000 + NO_CATALOG_CACHE_TTL_SECS));
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
    fn compatible_provider_uses_its_own_output_limit_before_cross_provider_catalog() {
        let id = "deepseek/deepseek-v4.1-flash";
        let catalog = json!({
            "deepseek": { "models": { "deepseek-v4.1-flash": {
                "limit": { "context": 1_048_576, "output": 1_048_576 }
            } } },
            "commandcode": { "models": { id: {
                "canonical_model_id": "deepseek/deepseek-v4.1-flash",
                "limit": { "context": 1_000_000, "output": 384_000 },
                "reasoning": true, "reasoning_options": [{ "type": "effort", "values": ["high"] }]
            } } }
        });
        let mut list = json!({ "data": [{ "id": id, "context_length": 1_000_000,
            "supported_endpoints": ["/chat/completions"] }] });
        let models = convert(&list, None, Some(&catalog), "commandcode");
        assert_eq!(models[id]["limit"], json!({ "context": 1_000_000, "output": 384_000 }));
        assert!(models[id].get("api").is_none(), "catalog must not switch the live endpoint's protocol");
        assert_eq!(models[id]["variants"]["high"]["reasoningEffort"], "high");
        let config = json!({ "provider": { "cc": { "api": DEFAULT_API, "options": { "baseURL": "https://api.commandcode.ai/provider/v1" }, "models": models } } });
        let resolved = super::super::config::resolve_model(&config, Some(&format!("cc/{id}")), &Default::default()).unwrap();
        assert_eq!(resolved.model.context_window, 1_000_000);
        assert_eq!(resolved.model.max_output_tokens, 384_000);
        // /models 的显式值仍优先，不能被目录中的旧上限压低。
        list["data"][0]["top_provider"] = json!({ "max_completion_tokens": 393_216 });
        assert_eq!(convert(&list, None, Some(&catalog), "commandcode")[id]["limit"]["output"], 393_216);
        assert_eq!(convert(&list, None, Some(&catalog), "unknown")[id]["limit"]["output"], 393_216);
        list["data"][0].as_object_mut().unwrap().remove("top_provider");
        assert_eq!(convert(&list, None, Some(&catalog), "unknown")[id]["limit"]["output"], 1_048_576);
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
                assert_eq!(models[&id]["limit"]["context"], 1_000_000, "{id}: 1M 变体上下文按变体");
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
