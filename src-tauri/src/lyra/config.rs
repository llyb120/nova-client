//! 本地配置加载：~/.nova/alkaid/config.jsonc（JSONC），
//! 并解析其中的 {env:NAME} 占位符。

use crate::lyra_complete::{
    detect_max_tokens_field, detect_thinking_format, load_config, provider_api, resolve_env_string,
};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// GUI 进程内运行时的数据根（启动时由 AppState 注入一次）；
/// stdio 子进程模式不设置，走 NOVA_DATA_DIR 环境变量。
static NOVA_ROOT_OVERRIDE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

pub fn set_nova_root(dir: PathBuf) {
    let _ = NOVA_ROOT_OVERRIDE.set(dir);
}

/// Nova 数据根目录：进程内注入优先，其次 NOVA_DATA_DIR，最后 ~/.nova。
pub fn nova_root() -> PathBuf {
    if let Some(dir) = NOVA_ROOT_OVERRIDE.get() {
        return dir.clone();
    }
    if let Ok(dir) = std::env::var("NOVA_DATA_DIR") {
        if !dir.trim().is_empty() {
            return PathBuf::from(dir);
        }
    }
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".nova")
}

/// 一次 Lyra 运行的数据根：主运行时为全局 nova_root()，借用额度运行时为隔离根目录
/// （凭证仅是该目录下的 alkaid/config.jsonc，进程内直接按根加载，无需子进程环境隔离）。
#[derive(Debug, Clone)]
pub struct Roots(PathBuf);

impl Roots {
    pub fn global() -> Self {
        Roots(nova_root())
    }

    pub fn borrowed(root: PathBuf) -> Self {
        Roots(root)
    }

    pub fn nova(&self) -> &Path {
        &self.0
    }

    pub fn data(&self) -> PathBuf {
        self.0.join("alkaid")
    }

    pub fn sessions(&self) -> PathBuf {
        self.data().join("sessions")
    }

    pub fn skills(&self) -> PathBuf {
        self.data().join("skills")
    }

    /// 从当前数据根加载本地 Lyra 配置。服务端同步已停用，保留参数仅兼容桥协议。
    pub fn load_config(&self, _server_config: Option<Value>) -> Result<Value, String> {
        load_config(&self.0)
    }
}

pub fn process_env() -> HashMap<String, String> {
    std::env::vars().collect()
}

#[derive(Debug, Clone)]
pub struct ResolvedModel {
    pub provider: String,
    pub id: String,
    pub api: String,
    pub base_url: String,
    pub headers: Map<String, Value>,
    pub reasoning: bool,
    pub thinking_format: Option<String>,
    pub max_tokens_field: &'static str,
    pub context_window: u64,
    pub max_output_tokens: u64,
    pub service_tier: Option<String>,
    /// 采样/推理开关等可选参数统一写在 options（model 覆盖 provider）；非内置键
    /// 原样透传进请求体顶层。采样参数按 variant > model.options > provider.options。
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    /// 当前模型是否声明支持图片输入；旧会话中的图片对 text-only 模型应降级为占位文本。
    pub supports_images: bool,
    /// deepseek 等要求 assistant 消息回传 reasoning_content。
    pub requires_reasoning_content: bool,
    /// 非官方 OpenAI 兼容代理默认发送会话亲和头，提高前缀缓存命中。
    pub session_affinity_headers: bool,
    pub session_affinity_format: String,
    /// 官方 OpenAI 或显式开启时支持 24h 长缓存保持。
    pub supports_long_cache_retention: bool,
    /// 控制思考强度的协议字段。deepseek/zai 发 thinking.type；GLM-5.2+ / deepseek 同时发
    /// reasoning_effort。端点不支持时可用 options.supportsReasoningEffort 关掉。
    pub supports_reasoning_effort: bool,
    /// 是否下发 thinking.clear_thinking（控制服务端是否抹掉历史 reasoning_content）。
    /// None 表示不下发、由端点默认值决定（GLM 标准端点默认 true=清除）；
    /// Some(false) 开启 Preserved Thinking，需同时回传 reasoning_content。
    pub clear_thinking: Option<bool>,
    /// options 里非内置键原样透传进请求体顶层（model.options 覆盖 provider.options），
    /// 厂商特有字段（tool_stream、thinking_budget、preserve_thinking 等）无需逐个接线。
    pub extra_options: Map<String, Value>,
}

#[derive(Debug, Clone)]
pub struct Resolved {
    pub model: ResolvedModel,
    pub api_key: String,
    pub thinking_level: Option<String>,
}

fn variant_label(variant: &str) -> String {
    match variant {
        "minimal" => "Minimal".into(),
        "low" => "Low".into(),
        "medium" => "Medium".into(),
        "high" => "High".into(),
        "xhigh" => "XHigh".into(),
        "max" => "Max".into(),
        other => other.into(),
    }
}

fn is_model_config(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    object.is_empty()
        || [
            "name",
            "reasoning",
            "modalities",
            "limit",
            "options",
            "variants",
        ]
        .iter()
        .any(|key| object.contains_key(*key))
}

fn collect_model_entries<'a>(
    models: &'a Map<String, Value>,
    prefix: &str,
    out: &mut Vec<(String, &'a Value)>,
) {
    for (id, value) in models {
        let id = if prefix.is_empty() {
            id.clone()
        } else {
            format!("{prefix}/{id}")
        };
        if is_model_config(value) {
            out.push((id, value));
        } else if let Some(children) = value.as_object() {
            collect_model_entries(children, &id, out);
        }
    }
}

fn model_entries(provider: &Value) -> Vec<(String, &Value)> {
    let mut out = Vec::new();
    if let Some(models) = provider.get("models").and_then(Value::as_object) {
        collect_model_entries(models, "", &mut out);
    }
    out
}

fn model_by_id<'a>(provider: &'a Value, model_id: &str) -> Option<&'a Value> {
    let models = provider.get("models")?;
    if let Some(model) = models.get(model_id).filter(|value| is_model_config(value)) {
        return Some(model);
    }
    let model = model_id
        .split('/')
        .try_fold(models, |current, segment| current.get(segment))?;
    is_model_config(model).then_some(model)
}

/// 模型选择器枚举，与 alkaidModelOptions 输出保持一致。
pub fn model_options(config: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    let Some(providers) = config.get("provider").and_then(Value::as_object) else {
        return out;
    };
    for (provider_id, provider) in providers {
        for (model_id, model) in model_entries(provider) {
            let value = format!("{provider_id}/{model_id}");
            let name = format!(
                "{} / {}",
                provider
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or(provider_id),
                model
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or(&model_id)
            );
            let supports_images = model
                .pointer("/modalities/input")
                .and_then(Value::as_array)
                .map(|input| input.iter().any(|v| v.as_str() == Some("image")))
                .unwrap_or(false);
            let context_window = model
                .pointer("/limit/context")
                .and_then(Value::as_u64)
                .unwrap_or(128_000);
            let meta = json!({
                "codex.ai/supportsImages": supports_images,
                "contextWindow": context_window,
            });
            let variants: Vec<(&String, &Value)> = model
                .get("variants")
                .and_then(Value::as_object)
                .map(|v| v.iter().collect())
                .unwrap_or_default();
            if variants.is_empty() {
                out.push(json!({ "value": value, "name": name, "_meta": meta }));
            } else {
                for (variant, variant_config) in variants {
                    let label = variant_config
                        .get("name")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .unwrap_or_else(|| variant_label(variant));
                    out.push(json!({
                        "value": format!("{value}/variant/{variant}"),
                        "name": format!("{name} · {label}"),
                        "_meta": meta,
                    }));
                }
            }
        }
    }
    out
}

pub fn default_model(config: &Value) -> Result<String, String> {
    let options = model_options(config);
    let has = |value: &str| {
        options
            .iter()
            .any(|option| option.get("value").and_then(Value::as_str) == Some(value))
    };
    let mut selection = config
        .get("model")
        .and_then(Value::as_str)
        .map(str::to_string);
    if let Some(current) = selection.clone() {
        if !has(&current) {
            // 兼容旧配置：model 无 variant 后缀时按 options.reasoningEffort 补全。
            let (provider_id, model_id) = current.split_once('/').unwrap_or((&current, ""));
            let effort = config
                .get("provider")
                .and_then(|providers| providers.get(provider_id))
                .and_then(|provider| model_by_id(provider, model_id))
                .and_then(|model| model.pointer("/options/reasoningEffort"))
                .and_then(Value::as_str);
            selection = effort
                .map(|effort| format!("{current}/variant/{effort}"))
                .filter(|candidate| has(candidate))
                .or(None);
        }
    }
    let selection = selection
        .or_else(|| {
            options
                .first()
                .and_then(|option| option.get("value").and_then(Value::as_str))
                .map(str::to_string)
        })
        .ok_or_else(|| "Lyra 配置没有可用模型".to_string())?;
    Ok(selection)
}

/// options 中被解析器消费、不应透传到请求体的内置键。
const RESERVED_OPTION_KEYS: &[&str] = &[
    "baseURL",
    "baseUrl",
    "apiKey",
    "headers",
    "temperature",
    "top_p",
    "topP",
    "reasoningEffort",
    "serviceTier",
    "service_tier",
    "thinkingFormat",
    "thinking_format",
    "maxTokensField",
    "sendSessionAffinityHeaders",
    "requiresReasoningContentOnAssistantMessages",
    "supportsLongCacheRetention",
    "supportsReasoningEffort",
    "clearThinking",
];

fn compat_flag(model: &Value, provider: &Value, key: &str) -> Option<bool> {
    // 只有 options 一处入口：model.options 覆盖 provider.options。
    [
        model.pointer(&format!("/options/{key}")),
        provider.pointer(&format!("/options/{key}")),
    ]
    .into_iter()
    .flatten()
    .find_map(Value::as_bool)
}

/// 解析 provider/model[/variant/x] 选择，返回可直接发请求的目标模型。
pub fn resolve_model(
    config: &Value,
    selection: Option<&str>,
    env: &HashMap<String, String>,
) -> Result<Resolved, String> {
    let owned;
    let selection = match selection {
        Some(value) if value.contains('/') => value,
        _ => {
            owned = default_model(config)?;
            &owned
        }
    };
    let marker = "/variant/";
    let (base_selection, variant) = selection
        .rsplit_once(marker)
        .map(|(base, variant)| (base, Some(variant)))
        .unwrap_or((selection, None));
    let (provider_id, model_id) = base_selection
        .split_once('/')
        .ok_or_else(|| "Lyra model 必须是 provider/model 格式".to_string())?;
    if model_id.is_empty() {
        return Err("Lyra model 必须是 provider/model 格式".into());
    }
    let provider = config
        .get("provider")
        .and_then(|providers| providers.get(provider_id))
        .ok_or_else(|| format!("Lyra provider 不存在：{provider_id}"))?;
    let model = model_by_id(provider, model_id)
        .ok_or_else(|| format!("Lyra model 不存在：{base_selection}"))?;
    if let Some(variant) = variant {
        if model
            .get("variants")
            .and_then(|variants| variants.get(variant))
            .is_none()
        {
            return Err(format!("Lyra model 不支持思考强度：{selection}"));
        }
    }
    let options = provider
        .get("options")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let base_url = resolve_env_string(
        options
            .get("baseURL")
            .or_else(|| options.get("baseUrl"))
            .and_then(Value::as_str)
            .unwrap_or_default(),
        env,
    )?;
    if base_url.is_empty() {
        return Err(format!("Lyra provider 缺少 options.baseURL：{provider_id}"));
    }
    let api_key = resolve_env_string(
        options
            .get("apiKey")
            .and_then(Value::as_str)
            .unwrap_or_default(),
        env,
    )?;
    let api = provider_api(provider)?;
    if !matches!(
        api.as_str(),
        "openai-completions" | "openai-responses" | "anthropic-messages"
    ) {
        return Err(format!(
            "Lyra 暂不支持协议 {api}（仅 openai-completions / openai-responses / anthropic-messages）"
        ));
    }
    let variant_options = variant
        .and_then(|name| {
            model
                .get("variants")
                .and_then(|variants| variants.get(name))
        })
        .cloned()
        .unwrap_or(Value::Null);
    let service_tier = variant_options
        .get("serviceTier")
        .or_else(|| variant_options.get("service_tier"))
        .or_else(|| model.pointer("/options/serviceTier"))
        .or_else(|| model.pointer("/options/service_tier"))
        .or_else(|| provider.pointer("/options/serviceTier"))
        .or_else(|| provider.pointer("/options/service_tier"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|value| !value.trim().is_empty());
    // 采样参数：variant > model.options > provider.options，camel/snake 均可。
    fn sampling_number(
        variant_options: &Value,
        model: &Value,
        provider: &Value,
        camel: &str,
        snake: &str,
    ) -> Option<f64> {
        variant_options
            .get(camel)
            .or_else(|| variant_options.get(snake))
            .or_else(|| model.pointer(&format!("/options/{camel}")))
            .or_else(|| model.pointer(&format!("/options/{snake}")))
            .or_else(|| provider.pointer(&format!("/options/{camel}")))
            .or_else(|| provider.pointer(&format!("/options/{snake}")))
            .and_then(Value::as_f64)
    }
    let temperature = sampling_number(
        &variant_options,
        model,
        provider,
        "temperature",
        "temperature",
    );
    let top_p = sampling_number(&variant_options, model, provider, "topP", "top_p");
    let thinking_level = variant.and_then(|name| {
        model
            .get("variants")
            .and_then(|variants| variants.get(name))
            .and_then(|variant| variant.get("reasoningEffort"))
            .and_then(Value::as_str)
            .map(str::to_string)
    });
    let reasoning = model
        .get("reasoning")
        .and_then(Value::as_bool)
        .unwrap_or_else(|| {
            model
                .get("variants")
                .and_then(Value::as_object)
                .map(|variants| !variants.is_empty())
                .unwrap_or(false)
        });
    let mut headers = Map::new();
    if let Some(object) = options.get("headers").and_then(Value::as_object) {
        for (key, value) in object {
            if let Some(text) = value.as_str() {
                headers.insert(key.clone(), json!(resolve_env_string(text, env)?));
            }
        }
    }
    let url_lower = base_url.to_lowercase();
    let official_openai = url_lower.contains("api.openai.com");
    // 默认不注入会话亲和 header；仅 provider/model 显式声明时发送。
    // 自建 OpenAI 兼容代理可能错误地把这些 header 固定到失效上游，形成半开 SSE。
    let session_affinity_headers =
        compat_flag(model, provider, "sendSessionAffinityHeaders").unwrap_or(false);
    let id_lower = model_id.to_lowercase();
    let requires_reasoning_content = compat_flag(
        model,
        provider,
        "requiresReasoningContentOnAssistantMessages",
    )
    .unwrap_or(id_lower.contains("deepseek"));
    let supports_long_cache_retention =
        compat_flag(model, provider, "supportsLongCacheRetention").unwrap_or(official_openai);
    let thinking_format = detect_thinking_format(provider_id, &base_url, model, provider);
    // deepseek / zai 在 thinking.type 之外同时发 reasoning_effort 控制思考深度
    // （GLM-5.2+ 已支持，可用 options.supportsReasoningEffort 显式关闭）。
    let supports_reasoning_effort =
        compat_flag(model, provider, "supportsReasoningEffort").unwrap_or(true);
    // 三态：不下发（沿用端点默认）/ 强制清除 / 强制保留（Preserved Thinking）。
    // GLM 标准端点默认清除、Coding Plan 端点默认保留，因此默认保持沉默交给端点。
    let clear_thinking = compat_flag(model, provider, "clearThinking");
    // options 里的非内置键原样透传进请求体顶层（厂商特有字段如 tool_stream、
    // thinking_budget、preserve_thinking 无需逐个接线）；model.options 覆盖 provider.options。
    let mut extra_options = Map::new();
    for scope in [
        Some(&options),
        model.get("options"),
    ]
    .into_iter()
    .flatten()
    {
        if let Some(object) = scope.as_object() {
            for (key, value) in object {
                if !RESERVED_OPTION_KEYS.contains(&key.as_str()) {
                    extra_options.insert(key.clone(), value.clone());
                }
            }
        }
    }
    // 保留历史思维链必须同时回传 reasoning_content，否则服务端拼不回原序列；
    // 反向若确定要清除，回传只是白烧 input token。
    let requires_reasoning_content = match clear_thinking {
        Some(true) => false,
        Some(false) => true,
        None => requires_reasoning_content,
    };
    Ok(Resolved {
        model: ResolvedModel {
            provider: provider_id.to_string(),
            id: model_id.to_string(),
            api,
            thinking_format,
            max_tokens_field: detect_max_tokens_field(&base_url, model, provider),
            base_url,
            headers,
            reasoning,
            context_window: model
                .pointer("/limit/context")
                .and_then(Value::as_u64)
                .unwrap_or(128_000),
            max_output_tokens: model
                .pointer("/limit/output")
                .and_then(Value::as_u64)
                .unwrap_or(32_000),
            service_tier,
            temperature,
            top_p,
            supports_images: model
                .pointer("/modalities/input")
                .and_then(Value::as_array)
                .is_some_and(|input| input.iter().any(|value| value.as_str() == Some("image"))),
            requires_reasoning_content,
            session_affinity_headers,
            session_affinity_format: if url_lower.contains("openrouter") {
                "openrouter".into()
            } else {
                "openai".into()
            },
            supports_long_cache_retention,
            supports_reasoning_effort,
            clear_thinking,
            extra_options,
        },
        api_key,
        thinking_level,
    })
}

/// 额度共享导出：把配置中的 {env:NAME} 占位符解析为字面量。
pub fn resolve_config_env(value: &Value, env: &HashMap<String, String>) -> Result<Value, String> {
    match value {
        Value::String(text) => Ok(json!(resolve_env_string(text, env)?)),
        Value::Array(items) => Ok(Value::Array(
            items
                .iter()
                .map(|item| resolve_config_env(item, env))
                .collect::<Result<Vec<_>, _>>()?,
        )),
        Value::Object(map) => {
            let mut out = Map::new();
            for (key, item) in map {
                out.insert(key.clone(), resolve_config_env(item, env)?);
            }
            Ok(Value::Object(out))
        }
        other => Ok(other.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_openai_proxy_defaults_match_legacy_pi() {
        let config = json!({
            "provider": {
                "custom": {
                    "npm": "@ai-sdk/openai-compatible",
                    "options": { "baseURL": "http://127.0.0.1:8317/v1", "apiKey": "key" },
                    "models": {
                        "gpt": {
                            "reasoning": true,
                            "variants": { "medium": { "reasoningEffort": "medium" } }
                        }
                    }
                }
            }
        });
        let resolved =
            resolve_model(&config, Some("custom/gpt/variant/medium"), &HashMap::new()).unwrap();
        assert_eq!(resolved.model.max_tokens_field, "max_completion_tokens");
        assert!(!resolved.model.session_affinity_headers);
    }

    #[test]
    fn sampling_params_resolve_variant_over_model_over_provider() {
        let config = json!({
            "provider": {
                "custom": {
                    "npm": "@ai-sdk/openai-compatible",
                    "options": {
                        "baseURL": "http://127.0.0.1:8317/v1",
                        "apiKey": "key",
                        "temperature": 1,
                        "top_p": 0.9
                    },
                    "models": {
                        "gpt": {
                            "options": { "topP": 0.95 },
                            "variants": {
                                "medium": { "reasoningEffort": "medium", "temperature": 0.2 }
                            }
                        }
                    }
                }
            }
        });
        let base = resolve_model(&config, Some("custom/gpt"), &HashMap::new()).unwrap();
        assert_eq!(base.model.temperature, Some(1.0));
        assert_eq!(base.model.top_p, Some(0.95));
        let variant =
            resolve_model(&config, Some("custom/gpt/variant/medium"), &HashMap::new()).unwrap();
        assert_eq!(variant.model.temperature, Some(0.2));
        assert_eq!(variant.model.top_p, Some(0.95));
    }

    #[test]
    fn model_ids_with_slashes_are_listed_and_resolved() {
        let config = json!({
            "model": "custom/qwen/qwen3.8-flash",
            "provider": {
                "custom": {
                    "name": "Command Code GOAT",
                    "npm": "@ai-sdk/openai-compatible",
                    "options": { "baseURL": "http://127.0.0.1:8317/v1", "apiKey": "key" },
                    "models": {
                        "qwen": {
                            "qwen3.8-flash": {
                                "name": "Qwen/Qwen 3.8 Flash",
                                "options": { "reasoningEffort": "max" },
                                "variants": {
                                    "high": { "reasoningEffort": "high" },
                                    "max": { "reasoningEffort": "max" }
                                }
                            }
                        }
                    }
                }
            }
        });

        assert_eq!(
            default_model(&config).unwrap(),
            "custom/qwen/qwen3.8-flash/variant/max"
        );
        assert!(model_options(&config).iter().any(|option| {
            option.get("value").and_then(Value::as_str)
                == Some("custom/qwen/qwen3.8-flash/variant/max")
                && option.get("name").and_then(Value::as_str)
                    == Some("Command Code GOAT / Qwen/Qwen 3.8 Flash · Max")
        }));
        let resolved = resolve_model(&config, None, &HashMap::new()).unwrap();
        assert_eq!(resolved.model.id, "qwen/qwen3.8-flash");
        assert_eq!(resolved.thinking_level.as_deref(), Some("max"));
    }

    #[test]
    fn explicit_proxy_compat_overrides_pi_defaults() {
        let config = json!({
            "provider": {
                "custom": {
                    "npm": "@ai-sdk/openai-compatible",
                    "options": {
                        "baseURL": "http://127.0.0.1:8317/v1",
                        "apiKey": "key",
                        "maxTokensField": "max_tokens",
                        "sendSessionAffinityHeaders": true,
                        "clearThinking": true
                    },
                    "models": { "gpt": {} }
                }
            }
        });
        let resolved = resolve_model(&config, Some("custom/gpt"), &HashMap::new()).unwrap();
        assert_eq!(resolved.model.max_tokens_field, "max_tokens");
        assert!(resolved.model.session_affinity_headers);
        assert_eq!(resolved.model.clear_thinking, Some(true));
    }

    #[test]
    fn unknown_options_pass_through_but_reserved_keys_are_consumed() {
        let config = json!({
            "provider": {
                "custom": {
                    "npm": "@ai-sdk/openai-compatible",
                    "options": {
                        "baseURL": "https://api.z.ai/api/paas/v4",
                        "apiKey": "key",
                        "tool_stream": true,
                        "thinking_format": "deepseek"
                    },
                    "models": {
                        "glm": {
                            "options": { "thinking_budget": 2048, "clearThinking": false }
                        }
                    }
                }
            }
        });
        let resolved = resolve_model(&config, Some("custom/glm"), &HashMap::new()).unwrap();
        // 未知键透传；model.options 覆盖 provider.options。
        assert_eq!(resolved.model.extra_options["tool_stream"], json!(true));
        assert_eq!(resolved.model.extra_options["thinking_budget"], json!(2048));
        // 内置键被消费，不会重复下发；snake 风格的 thinking_format 同样生效。
        assert!(!resolved.model.extra_options.contains_key("clearThinking"));
        assert!(!resolved.model.extra_options.contains_key("thinking_format"));
        assert_eq!(resolved.model.thinking_format.as_deref(), Some("deepseek"));
        assert_eq!(resolved.model.clear_thinking, Some(false));
    }
}
