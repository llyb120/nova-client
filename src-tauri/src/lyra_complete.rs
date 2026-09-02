//! 输入框补全：Rust 直连 Lyra provider HTTP，不再为每次补全冷启 Node bridge。

use crate::http_stream::SseDecoder;
use regex::Regex;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime};

const COMPLETION_MAX_TOKENS: u32 = 48;
/// 流式收到足够续写后立刻返回并断开连接，缩短体感等待。
const COMPLETION_EARLY_MIN_CHARS: usize = 24;
const COMPLETION_EARLY_HARD_CHARS: usize = 64;

#[derive(Debug, Clone)]
struct ResolvedCompletionTarget {
    api: String,
    model_id: String,
    base_url: String,
    api_key: String,
    headers: Map<String, Value>,
    /// deepseek / zai 等用 thinking.type；responses 用 reasoning.effort
    thinking_format: Option<String>,
    max_tokens_field: &'static str,
    reasoning: bool,
    service_tier: Option<String>,
    temperature: Option<f64>,
    top_p: Option<f64>,
}

struct CachedConfig {
    path: PathBuf,
    mtime: Option<SystemTime>,
    config: Value,
}

fn config_cache() -> &'static Mutex<Option<CachedConfig>> {
    static CACHE: OnceLock<Mutex<Option<CachedConfig>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

/// 配置文件发生变化后由外部文件监听器调用，避免直连补全继续使用旧配置。
pub fn invalidate_config_cache() {
    *config_cache().lock().unwrap() = None;
}

/// 用已有 reqwest 连接池直调补全；仅支持 openai-completions / openai-responses。
pub async fn complete_direct(
    http: &reqwest::Client,
    data_dir: &Path,
    env: &HashMap<String, String>,
    model_selection: &str,
    prompt: &str,
) -> Result<String, String> {
    let config = load_config(data_dir)?;
    let target = resolve_target(&config, model_selection, env)?;
    match target.api.as_str() {
        "openai-completions" => complete_openai_completions(http, &target, prompt).await,
        "openai-responses" => complete_openai_responses(http, &target, prompt).await,
        other => Err(format!("补全暂不支持直连协议：{other}")),
    }
}

async fn complete_openai_completions(
    http: &reqwest::Client,
    target: &ResolvedCompletionTarget,
    prompt: &str,
) -> Result<String, String> {
    let url = join_url(&target.base_url, "chat/completions");
    let mut body = json!({
        "model": target.model_id,
        "messages": [{ "role": "user", "content": prompt }],
        "stream": true,
    });
    body[target.max_tokens_field] = json!(COMPLETION_MAX_TOKENS);
    if let Some(service_tier) = &target.service_tier {
        body["service_tier"] = json!(service_tier);
    }
    if let Some(temperature) = target.temperature {
        body["temperature"] = json!(temperature);
    }
    if let Some(top_p) = target.top_p {
        body["top_p"] = json!(top_p);
    }
    apply_thinking_disabled_completions(&mut body, target);
    let mut response = post_stream(http, &url, &target.api_key, &target.headers, body).await?;
    read_completions_sse(&mut response).await
}

async fn complete_openai_responses(
    http: &reqwest::Client,
    target: &ResolvedCompletionTarget,
    prompt: &str,
) -> Result<String, String> {
    let url = join_url(&target.base_url, "responses");
    let mut body = json!({
        "model": target.model_id,
        "input": [{ "role": "user", "content": prompt }],
        "stream": true,
        "store": false,
        "max_output_tokens": COMPLETION_MAX_TOKENS.max(16),
    });
    if let Some(service_tier) = &target.service_tier {
        body["service_tier"] = json!(service_tier);
    }
    if let Some(temperature) = target.temperature {
        body["temperature"] = json!(temperature);
    }
    if let Some(top_p) = target.top_p {
        body["top_p"] = json!(top_p);
    }
    if target.reasoning {
        body["reasoning"] = json!({ "effort": "none" });
    }
    let mut response = post_stream(http, &url, &target.api_key, &target.headers, body).await?;
    read_responses_sse(&mut response).await
}

fn apply_thinking_disabled_completions(body: &mut Value, target: &ResolvedCompletionTarget) {
    if !target.reasoning {
        return;
    }
    match target.thinking_format.as_deref() {
        Some("deepseek") | Some("zai") => {
            body["thinking"] = json!({ "type": "disabled" });
        }
        Some("qwen") => {
            body["enable_thinking"] = json!(false);
        }
        _ => {}
    }
}

async fn post_stream(
    http: &reqwest::Client,
    url: &str,
    api_key: &str,
    headers: &Map<String, Value>,
    body: Value,
) -> Result<reqwest::Response, String> {
    let mut req = http
        .post(url)
        .header("content-type", "application/json")
        .header("accept", "text/event-stream")
        .json(&body)
        .timeout(Duration::from_secs(12));
    if !api_key.is_empty() {
        req = req.bearer_auth(api_key);
    }
    for (key, value) in headers {
        if let Some(text) = value.as_str() {
            req = req.header(key.as_str(), text);
        }
    }
    let response = req.send().await.map_err(|e| format!("补全请求失败：{e}"))?;
    let status = response.status();
    if !status.is_success() {
        let text = response.text().await.unwrap_or_default();
        let message = text.trim();
        return Err(if message.is_empty() {
            format!("补全 HTTP {status}")
        } else {
            format!("补全 HTTP {status}：{message}")
        });
    }
    Ok(response)
}

async fn read_completions_sse(response: &mut reqwest::Response) -> Result<String, String> {
    let mut decoder = SseDecoder::new();
    let mut out = String::new();
    loop {
        let chunk = response
            .chunk()
            .await
            .map_err(|e| format!("读取补全流失败：{e}"))?;
        let (events, finished) = match chunk {
            Some(chunk) => (
                decoder
                    .push(&chunk)
                    .map_err(|e| format!("读取补全流失败：{e}"))?,
                false,
            ),
            None => (
                decoder
                    .finish()
                    .map_err(|e| format!("读取补全流失败：{e}"))?,
                true,
            ),
        };
        for data in events {
            if data == "[DONE]" {
                return Ok(out.trim().to_string());
            }
            let Ok(value) = serde_json::from_str::<Value>(&data) else {
                continue;
            };
            // 非流式回退：偶发代理把 stream 请求仍按整包 JSON 返回
            if value.get("choices").is_some() && value.pointer("/choices/0/delta").is_none() {
                return extract_completions_text(&value);
            }
            if let Some(delta) = value
                .pointer("/choices/0/delta/content")
                .and_then(Value::as_str)
            {
                out.push_str(delta);
                if should_early_stop(&out) {
                    return Ok(out.trim().to_string());
                }
            }
        }
        if finished {
            return Ok(out.trim().to_string());
        }
    }
}

async fn read_responses_sse(response: &mut reqwest::Response) -> Result<String, String> {
    let mut decoder = SseDecoder::new();
    let mut out = String::new();
    loop {
        let chunk = response
            .chunk()
            .await
            .map_err(|e| format!("读取补全流失败：{e}"))?;
        let (events, finished) = match chunk {
            Some(chunk) => (
                decoder
                    .push(&chunk)
                    .map_err(|e| format!("读取补全流失败：{e}"))?,
                false,
            ),
            None => (
                decoder
                    .finish()
                    .map_err(|e| format!("读取补全流失败：{e}"))?,
                true,
            ),
        };
        for data in events {
            if data == "[DONE]" {
                return Ok(out.trim().to_string());
            }
            let Ok(value) = serde_json::from_str::<Value>(&data) else {
                continue;
            };
            // 非流式整包
            if value.get("output").is_some() || value.get("output_text").is_some() {
                return extract_responses_text(&value);
            }
            let kind = value
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let delta = match kind {
                "response.output_text.delta" | "response.text.delta" => value
                    .get("delta")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                _ => value
                    .pointer("/delta/text")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            };
            if let Some(delta) = delta {
                out.push_str(&delta);
                if should_early_stop(&out) {
                    return Ok(out.trim().to_string());
                }
            }
        }
        if finished {
            return Ok(out.trim().to_string());
        }
    }
}

fn should_early_stop(text: &str) -> bool {
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    if len >= COMPLETION_EARLY_HARD_CHARS {
        return true;
    }
    if len < COMPLETION_EARLY_MIN_CHARS {
        return false;
    }
    matches!(
        chars.last(),
        Some('。' | '！' | '？' | '；' | '，' | '、' | '!' | '?' | ';' | ',' | '\n' | '…')
    )
}

fn extract_completions_text(response: &Value) -> Result<String, String> {
    let content = response
        .pointer("/choices/0/message/content")
        .ok_or_else(|| "补全响应缺少 choices[0].message.content".to_string())?;
    Ok(content_to_text(content).trim().to_string())
}

fn extract_responses_text(response: &Value) -> Result<String, String> {
    if let Some(text) = response.get("output_text").and_then(Value::as_str) {
        return Ok(text.trim().to_string());
    }
    let mut out = String::new();
    let Some(items) = response.get("output").and_then(Value::as_array) else {
        return Ok(String::new());
    };
    for item in items {
        if item.get("type").and_then(Value::as_str) != Some("message") {
            continue;
        }
        let Some(parts) = item.get("content").and_then(Value::as_array) else {
            continue;
        };
        for part in parts {
            let kind = part.get("type").and_then(Value::as_str).unwrap_or_default();
            if kind == "output_text" || kind == "text" {
                if let Some(text) = part.get("text").and_then(Value::as_str) {
                    out.push_str(text);
                }
            }
        }
    }
    Ok(out.trim().to_string())
}

fn content_to_text(content: &Value) -> String {
    if let Some(text) = content.as_str() {
        return text.to_string();
    }
    let Some(parts) = content.as_array() else {
        return String::new();
    };
    parts
        .iter()
        .filter_map(|part| {
            if part.get("type").and_then(Value::as_str) == Some("text") {
                part.get("text").and_then(Value::as_str)
            } else {
                part.as_str()
            }
        })
        .collect::<Vec<_>>()
        .join("")
}

pub(crate) fn load_config(data_dir: &Path) -> Result<Value, String> {
    let path = data_dir.join("alkaid").join("config.jsonc");
    let mtime = std::fs::metadata(&path)
        .ok()
        .and_then(|m| m.modified().ok());
    {
        let cache = config_cache().lock().unwrap();
        if let Some(cached) = cache.as_ref() {
            if cached.path == path && cached.mtime == mtime {
                return Ok(cached.config.clone());
            }
        }
    }

    let config = match std::fs::read_to_string(&path) {
        Ok(text) => parse_jsonc(&text)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!("未找到 Lyra 配置：{}", path.display()));
        }
        Err(error) => return Err(format!("读取 Lyra 配置失败：{error}")),
    };
    if !config
        .get("provider")
        .map(Value::is_object)
        .unwrap_or(false)
    {
        return Err("Lyra 配置缺少 provider".into());
    }
    *config_cache().lock().unwrap() = Some(CachedConfig {
        path,
        mtime,
        config: config.clone(),
    });
    Ok(config)
}

fn resolve_target(
    config: &Value,
    selection: &str,
    env: &HashMap<String, String>,
) -> Result<ResolvedCompletionTarget, String> {
    if selection.is_empty() || !selection.contains('/') {
        return Err("Lyra model 必须是 provider/model 格式".into());
    }
    let marker = "/variant/";
    let base_selection = selection
        .rsplit_once(marker)
        .map(|(base, _)| base)
        .unwrap_or(selection);
    let (provider_id, model_id) = base_selection
        .split_once('/')
        .ok_or_else(|| "Lyra model 必须是 provider/model 格式".to_string())?;
    let provider = config
        .pointer(&format!("/provider/{provider_id}"))
        .ok_or_else(|| format!("Lyra provider 不存在：{provider_id}"))?;
    let model = provider
        .pointer(&format!("/models/{model_id}"))
        .ok_or_else(|| format!("Lyra model 不存在：{base_selection}"))?;
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
    let variant = selection.rsplit_once(marker).map(|(_, value)| value);
    let variant_options = variant
        .and_then(|name| model.pointer(&format!("/variants/{name}")))
        .and_then(Value::as_object);
    let service_tier = variant_options
        .and_then(|value| {
            value
                .get("serviceTier")
                .or_else(|| value.get("service_tier"))
        })
        .or_else(|| model.pointer("/options/serviceTier"))
        .or_else(|| model.pointer("/options/service_tier"))
        .or_else(|| provider.pointer("/options/serviceTier"))
        .or_else(|| provider.pointer("/options/service_tier"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    // 采样参数：variant > model.options > provider.options，camel/snake 均可。
    fn sampling_number(
        variant_options: Option<&Map<String, Value>>,
        model: &Value,
        provider: &Value,
        camel: &str,
        snake: &str,
    ) -> Option<f64> {
        variant_options
            .and_then(|value| value.get(camel).or_else(|| value.get(snake)))
            .or_else(|| model.pointer(&format!("/options/{camel}")))
            .or_else(|| model.pointer(&format!("/options/{snake}")))
            .or_else(|| provider.pointer(&format!("/options/{camel}")))
            .or_else(|| provider.pointer(&format!("/options/{snake}")))
            .and_then(Value::as_f64)
    }
    let temperature = sampling_number(
        variant_options,
        model,
        provider,
        "temperature",
        "temperature",
    );
    let top_p = sampling_number(variant_options, model, provider, "topP", "top_p");
    let reasoning = model
        .get("reasoning")
        .and_then(Value::as_bool)
        .unwrap_or_else(|| {
            model
                .get("variants")
                .and_then(Value::as_object)
                .map(|v| !v.is_empty())
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
    let thinking_format = detect_thinking_format(provider_id, &base_url, model, provider);
    let max_tokens_field = detect_max_tokens_field(&base_url, model, provider);
    Ok(ResolvedCompletionTarget {
        api,
        model_id: model_id.to_string(),
        base_url,
        api_key,
        headers,
        thinking_format,
        max_tokens_field,
        reasoning,
        service_tier,
        temperature,
        top_p,
    })
}

pub(crate) fn provider_api(provider: &Value) -> Result<String, String> {
    if let Some(api) = provider.get("api").and_then(Value::as_str) {
        return Ok(api.to_string());
    }
    let npm = provider
        .get("npm")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if npm.contains("anthropic") {
        return Ok("anthropic-messages".into());
    }
    if npm.contains("google") {
        return Ok("google-generative-ai".into());
    }
    if npm.contains("openai-compatible") {
        return Ok("openai-completions".into());
    }
    if npm.contains("openai") {
        return Ok("openai-responses".into());
    }
    Err("Lyra provider 缺少 api，且无法从 npm 推导协议".into())
}

pub(crate) fn detect_thinking_format(
    provider_id: &str,
    base_url: &str,
    model: &Value,
    provider: &Value,
) -> Option<String> {
    // 与 compat_flag 同优先级：model.options 覆盖 provider.options。
    for scope in [model.get("options"), provider.get("options")]
    .into_iter()
    .flatten()
    {
        if let Some(format) = scope
            .get("thinkingFormat")
            .or_else(|| scope.get("thinking_format"))
            .and_then(Value::as_str)
        {
            return Some(format.to_string());
        }
    }
    let id = format!("{provider_id} {base_url}").to_lowercase();
    if id.contains("deepseek") {
        Some("deepseek".into())
    } else if id.contains("api.z.ai") || id.contains("open.bigmodel.cn") {
        Some("zai".into())
    } else {
        None
    }
}

pub(crate) fn detect_max_tokens_field(
    base_url: &str,
    model: &Value,
    provider: &Value,
) -> &'static str {
    if let Some(field) = model
        .pointer("/options/maxTokensField")
        .or_else(|| provider.pointer("/options/maxTokensField"))
        .and_then(Value::as_str)
    {
        return if field == "max_tokens" {
            "max_tokens"
        } else {
            "max_completion_tokens"
        };
    }
    let url = base_url.to_lowercase();
    if url.contains("moonshot")
        || url.contains("together")
        || url.contains("chutes.ai")
        || url.contains("nvidia")
        || url.contains("ant-ling")
    {
        "max_tokens"
    } else {
        // 与 Lyra/PI 对齐：标准及未知 OpenAI-compatible 默认 max_completion_tokens；
        // 只有 PI 明确识别的兼容服务默认使用旧 max_tokens，配置仍可显式覆盖。
        "max_completion_tokens"
    }
}

pub(crate) fn resolve_env_string(
    value: &str,
    env: &HashMap<String, String>,
) -> Result<String, String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"\{env:([A-Za-z_][A-Za-z0-9_]*)\}").unwrap());
    let mut out = value.to_string();
    for caps in re.captures_iter(value) {
        let full = caps.get(0).unwrap().as_str();
        let name = &caps[1];
        let resolved = env
            .get(name)
            .cloned()
            .or_else(|| std::env::var(name).ok())
            .ok_or_else(|| format!("Lyra 配置引用的环境变量 {name} 未注入 Nova 进程"))?;
        out = out.replace(full, &resolved);
    }
    Ok(out)
}

pub(crate) fn join_url(base: &str, path: &str) -> String {
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

pub(crate) fn parse_jsonc(text: &str) -> Result<Value, String> {
    let stripped = strip_trailing_commas(&strip_json_comments(text));
    serde_json::from_str(&stripped).map_err(|e| format!("解析 Lyra 配置失败：{e}"))
}

fn strip_json_comments(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let mut in_string = false;
    let mut escaped = false;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if c == '"' {
            in_string = true;
            out.push(c);
            i += 1;
            continue;
        }
        if c == '/' && bytes.get(i + 1) == Some(&b'/') {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if c == '/' && bytes.get(i + 1) == Some(&b'*') {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                if bytes[i] == b'\n' {
                    out.push('\n');
                }
                i += 1;
            }
            i = i.saturating_add(2);
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

fn strip_trailing_commas(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let mut in_string = false;
    let mut escaped = false;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if c == '"' {
            in_string = true;
            out.push(c);
            i += 1;
            continue;
        }
        if c == ',' {
            let mut next = i + 1;
            while next < bytes.len() && bytes[next].is_ascii_whitespace() {
                next += 1;
            }
            if matches!(bytes.get(next), Some(b'}') | Some(b']')) {
                i += 1;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_jsonc_with_comments_and_trailing_commas() {
        let value = parse_jsonc(
            r#"{
            // comment
            "provider": { "deepseek": { "npm": "@ai-sdk/openai-compatible", }, },
        }"#,
        )
        .unwrap();
        assert_eq!(
            value
                .pointer("/provider/deepseek/npm")
                .and_then(Value::as_str),
            Some("@ai-sdk/openai-compatible")
        );
    }

    #[test]
    fn resolves_deepseek_completion_target() {
        let config = json!({
            "provider": {
                "deepseek": {
                    "npm": "@ai-sdk/openai-compatible",
                    "options": {
                        "baseURL": "https://api.deepseek.com",
                        "apiKey": "sk-test"
                    },
                    "models": {
                        "deepseek-v4-flash": {
                            "reasoning": true,
                            "variants": { "high": { "reasoningEffort": "high" } }
                        }
                    }
                }
            }
        });
        let target = resolve_target(
            &config,
            "deepseek/deepseek-v4-flash/variant/high",
            &HashMap::new(),
        )
        .unwrap();
        assert_eq!(target.api, "openai-completions");
        assert_eq!(target.model_id, "deepseek-v4-flash");
        assert_eq!(target.thinking_format.as_deref(), Some("deepseek"));
        assert_eq!(target.max_tokens_field, "max_completion_tokens");
        assert!(target.reasoning);
    }

    #[test]
    fn extracts_completions_message_text() {
        let response = json!({
            "choices": [{ "message": { "content": "  world  " } }]
        });
        assert_eq!(extract_completions_text(&response).unwrap(), "world");
    }

    #[test]
    fn early_stop_on_sentence_or_hard_cap() {
        assert!(!should_early_stop("短"));
        assert!(!should_early_stop("还不够长的句子。"));
        assert!(should_early_stop(
            "这是一句已经超过最小字数阈值、可以在顿号处早停、"
        ));
        assert!(should_early_stop(
            "这是一句已经超过最小字数阈值，可以在逗号处早停，"
        ));
        let hard: String = "字".repeat(COMPLETION_EARLY_HARD_CHARS);
        assert!(should_early_stop(&hard));
    }
}
