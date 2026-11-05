//! CodeBuddy login/catalog adapter; inference reuses Lyra's native OpenAI SSE transport.
use super::config::ResolvedModel;
use super::provider::{self, StreamEvent, StreamResult};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{atomic::AtomicBool, Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn json_file(path: &Path) -> Result<Value, String> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(_) => return Err("无法读取 CodeBuddy 配置或登录，请在 Lyra 设置里登录".into()),
    };
    let mut bytes = Vec::new();
    file.take(4 * 1024 * 1024 + 1).read_to_end(&mut bytes).map_err(|_| "读取 CodeBuddy 配置失败")?;
    if bytes.len() > 4 * 1024 * 1024 { return Err("CodeBuddy 配置超过 4 MiB".into()); }
    serde_json::from_slice(&bytes).map_err(|_| "CodeBuddy 配置或登录格式无效".into())
}

pub(super) fn load(root: &Path, env: &HashMap<String, String>, source: &str) -> Result<Value, String> {
    let workbuddy = source == "workbuddy";
    let prefix = source.to_ascii_uppercase();
    let label = if workbuddy { "WorkBuddy" } else { "CodeBuddy" };
    let explicit_key = env.get(&format!("{prefix}_AUTH_TOKEN")).or_else(|| env.get(&format!("{prefix}_API_KEY")))
        .map(String::as_str).filter(|s| !s.trim().is_empty());
    // 两个产品只用各自的 Nova 登录；本机 CLI/桌面端不能改变原生直连行为。
    let session = if explicit_key.is_some() { json!({}) } else { nova_session(root, source)? };
    let version = if workbuddy { WORKBUDDY_VERSION } else { CODEBUDDY_VERSION };
    let key = explicit_key.or_else(|| session["auth"]["accessToken"].as_str()).filter(|s| !s.trim().is_empty())
        .ok_or("CodeBuddy 登录缺失或无效，请在 Lyra 设置里登录")?;
    let endpoint = env.get(&format!("NOVA_{prefix}_ENDPOINT")).cloned().unwrap_or_else(|| format!("https://{}", session["auth"]["domain"].as_str().unwrap_or(if workbuddy { "www.workbuddy.cn" } else { "www.codebuddy.ai" })));
    let url = reqwest::Url::parse(&endpoint).map_err(|_| "CodeBuddy API 地址无效")?;
    if !(url.scheme() == "https" || (url.scheme() == "http" && matches!(url.host_str(), Some("127.0.0.1" | "[::1]" | "localhost"))))
        || !url.username().is_empty() || url.password().is_some() || url.query().is_some() || url.fragment().is_some() || url.path() != "/" {
        return Err("CodeBuddy API 必须为 HTTPS origin，本机测试允许 HTTP 回环地址".into());
    }
    let mut headers = json!({ "User-Agent": format!("CLI/{version} {label}/{version}"), "X-IDE-Type": if workbuddy { "WorkBuddy" } else { "CLI" }, "X-IDE-Version": version,
        "X-Product": label, "X-Agent-Intent": "craft", "X-Agent-Runtime": "local-agent" });
    for (header, field) in [("X-User-Id", "uid"), ("X-Enterprise-Id", "enterpriseId")] {
        if let Some(value) = session["account"].get(field).filter(|v| !v.is_null()) {
            headers[header] = json!(value.as_str().map(str::to_string).unwrap_or_else(|| value.to_string()));
        }
    }
    Ok(json!({ "name": "CodeBuddy / WorkBuddy", "api": "codebuddy", "options": {
        "baseURL": format!("{}/v2", url.origin().ascii_serialization()), "apiKey": key, "headers": headers,
        "codebuddySource": source,
        "codebuddyModel": env.get(&format!("{prefix}_MODEL")),
        "requiresReasoningContentOnAssistantMessages": true
    }, "models": {} }))
}

// shortcut: 固定已验证的协议版本，上游请求头要求变化时更新，不读取本机 CLI。
const CODEBUDDY_VERSION: &str = "2.169.0";
const WORKBUDDY_VERSION: &str = "5.8.2";

fn now_ms() -> u64 { SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64 }

// ---------- 网页登录（不依赖 CLI 或桌面端） ----------
// 与 CodeBuddy CLI 2.174.0 相同的外链登录：申请 state → 浏览器登录 → 轮询 token → 取账号。
// 会话按 CLI 的 {auth, account} 形状存在 ~/.nova/<source>-login.json，复用上面的解析。

fn login_file(root: &Path, source: &str) -> PathBuf { root.join(format!("{source}-login.json")) }

pub(crate) fn login_origin(source: &str, site: &str) -> &'static str {
    match (source, site) { ("workbuddy", _) => "https://www.workbuddy.cn", (_, "cn") => "https://www.codebuddy.cn", _ => "https://www.codebuddy.ai" }
}

/// 读取 Nova 登录会话，快过期时用 refreshToken 续期并写回。
fn nova_session(root: &Path, source: &str) -> Result<Value, String> {
    let path = login_file(root, source);
    let mut session = json_file(&path)?;
    if !session["auth"]["accessToken"].is_string() { return Err("登录会话无效".into()); }
    if session["auth"]["expiresAt"].as_u64().is_some_and(|expiry| expiry <= now_ms() + 5 * 60_000) {
        let origin = session["origin"].as_str().unwrap_or("").to_string();
        let auth = session["auth"].clone();
        // load 是同步的且可能运行在 tokio 线程上，blocking client 只能在独立线程里用。
        let refreshed = std::thread::spawn(move || refresh_blocking(&origin, &auth)).join().map_err(|_| "续期线程异常")??;
        session["auth"] = refreshed;
        save_session(&path, &session)?;
    }
    Ok(session)
}

fn refresh_blocking(origin: &str, auth: &Value) -> Result<Value, String> {
    let error = "登录已过期且续期失败，请在 Lyra 设置里重新登录";
    let refresh = auth["refreshToken"].as_str().ok_or(error)?;
    let response: Value = reqwest::blocking::Client::builder().timeout(Duration::from_secs(20)).build().map_err(|_| error)?
        .post(format!("{origin}/v2/plugin/auth/token/refresh")).json(&json!({}))
        .bearer_auth(auth["accessToken"].as_str().unwrap_or("")).header("X-Refresh-Token", refresh).header("X-Auth-Refresh-Source", "plugin")
        .header("X-Domain", auth["domain"].as_str().unwrap_or(""))
        .send().and_then(|r| r.error_for_status()).and_then(|r| r.json()).map_err(|_| error)?;
    normalize_auth(&response["data"], auth["domain"].as_str().unwrap_or("")).ok_or_else(|| error.into())
}

/// 与 CLI 一致：只有 expiresIn 时换算成绝对时间；补上 domain 供后续请求定位站点。
fn normalize_auth(data: &Value, domain: &str) -> Option<Value> {
    let mut auth = data.clone();
    auth["accessToken"].as_str().filter(|s| !s.is_empty())?;
    let now = now_ms();
    if auth["expiresAt"].is_null() { if let Some(s) = auth["expiresIn"].as_u64() { auth["expiresAt"] = json!(now + s * 1000); } }
    if auth["refreshExpiresAt"].is_null() { if let Some(s) = auth["refreshExpiresIn"].as_u64() { auth["refreshExpiresAt"] = json!(now + s * 1000); } }
    if auth["domain"].as_str().is_none_or(str::is_empty) { auth["domain"] = json!(domain); }
    Some(auth)
}

fn save_session(path: &Path, session: &Value) -> Result<(), String> {
    write_private(path, &serde_json::to_vec_pretty(session).unwrap())
}

/// 登录凭据原子写入并仅当前用户可读。
pub(super) fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    crate::thread_storage::atomic_write(path, bytes).map_err(|e| format!("保存登录凭据失败：{e}"))?;
    #[cfg(unix)]
    { use std::os::unix::fs::PermissionsExt; let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)); }
    Ok(())
}

fn no_auth(request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    request.header("X-No-Authorization", "true").header("X-No-User-Id", "true").header("X-No-Enterprise-Id", "true").header("X-No-Department-Info", "true")
}

/// 申请登录：返回要在浏览器打开的登录页和轮询用的 state。
pub(crate) async fn login_start(source: &str, site: &str) -> Result<Value, String> {
    let origin = login_origin(source, site);
    let response: Value = no_auth(provider::local_api_client().post(format!("{origin}/v2/plugin/auth/state?platform=CLI")).json(&json!({})))
        .timeout(Duration::from_secs(20)).send().await.and_then(|r| r.error_for_status()).map_err(|_| "无法连接登录服务，请检查网络")?
        .json().await.map_err(|_| "登录服务返回格式无效")?;
    let (state, url) = (response["data"]["state"].as_str(), response["data"]["authUrl"].as_str());
    let (Some(state), Some(url)) = (state, url) else { return Err("登录服务未返回登录地址".into()) };
    // 只打开本站的 HTTPS 登录页，防止被响应引导到任意地址。
    if reqwest::Url::parse(url).ok().map(|u| u.origin().ascii_serialization()).as_deref() != Some(origin) { return Err("登录地址不是官方站点".into()); }
    Ok(json!({ "state": state, "url": url }))
}

/// 轮询一次：未完成返回 false；完成则取账号、保存会话并返回 true。
pub(crate) async fn login_poll(root: &Path, source: &str, site: &str, state: &str) -> Result<bool, String> {
    let origin = login_origin(source, site);
    let http = provider::local_api_client();
    let url = reqwest::Url::parse_with_params(&format!("{origin}/v2/plugin/auth/token"), [("state", state)]).map_err(|_| "登录状态无效")?;
    let response: Value = no_auth(http.get(url)).timeout(Duration::from_secs(20)).send().await.map_err(|_| "无法连接登录服务")?
        .json().await.map_err(|_| "登录服务返回格式无效")?;
    if response["code"] == 11217 { return Ok(false); }
    let domain = reqwest::Url::parse(origin).unwrap().host_str().unwrap_or("").to_string();
    let auth = normalize_auth(&response["data"], &domain).ok_or_else(|| format!("登录失败：{}", response["msg"].as_str().unwrap_or("未返回凭据")))?;
    let mut account = Value::Null;
    for _ in 0..10 {
        let response: Value = http.get(format!("{origin}/v2/plugin/account")).bearer_auth(auth["accessToken"].as_str().unwrap()).header("X-Domain", &domain)
            .timeout(Duration::from_secs(20)).send().await.map_err(|_| "无法读取账号信息")?.json().await.map_err(|_| "账号信息格式无效")?;
        if response["code"] != 12151 { account = response["data"].clone(); break; }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    if !account.is_object() { return Err("登录成功但读取账号信息失败，请重试".into()); }
    save_session(&login_file(root, source), &json!({ "origin": origin, "auth": auth, "account": account }))?;
    Ok(true)
}

fn merge_catalog(config: &Value) -> Result<Vec<Value>, String> {
    if config["code"] != 0 { return Err("CodeBuddy 模型目录返回错误".into()); }
    let cloud = config["data"]["models"].as_array().ok_or("CodeBuddy 模型目录格式无效")?;
    let mut merged: HashMap<String, Value> = HashMap::new();
    for model in cloud {
        if let Some(id) = model["id"].as_str() {
            merged.entry(id.into()).or_insert_with(|| json!({})).as_object_mut().unwrap().extend(model.as_object().unwrap().clone());
        }
    }
    let allowed = config["data"]["agents"].as_array().and_then(|agents| agents.iter().find(|a| a["name"] == "cli")).and_then(|a| a["models"].as_array());
    let ids: Vec<&str> = match allowed { Some(ids) => ids.iter().filter_map(Value::as_str).collect(), None => cloud.iter().filter_map(|m| m["id"].as_str()).collect() };
    Ok(ids.into_iter().map(|id| merged.get(id).cloned().unwrap_or_else(|| json!({"id":id}))).collect())
}

async fn catalog(http: &reqwest::Client, options: &Value, force: bool) -> Result<Vec<Value>, String> {
    static CACHE: OnceLock<Mutex<HashMap<String, (Instant, Vec<Value>)>>> = OnceLock::new();
    // Account and enterprise identity must invalidate the cached catalog on login changes.
    use sha2::{Digest, Sha256};
    let identity = format!("{:x}", Sha256::digest(options.to_string().as_bytes()));
    let cache = CACHE.get_or_init(Default::default);
    if !force {
        if let Some((_, models)) = cache.lock().unwrap().get(&identity).filter(|(time, _)| time.elapsed() < Duration::from_secs(60)) { return Ok(models.clone()); }
    }
    let url = reqwest::Url::parse(options["baseURL"].as_str().unwrap_or("")).and_then(|u| u.join("/v3/config")).map_err(|_| "CodeBuddy API 地址无效")?;
    let mut request = http.get(url).bearer_auth(options["apiKey"].as_str().unwrap_or("")).timeout(Duration::from_secs(30));
    if let Some(headers) = options["headers"].as_object() { for (name, value) in headers { if let Some(value) = value.as_str() { request = request.header(name, value); } } }
    let mut response = request.send().await.map_err(|_| "CodeBuddy 模型目录请求失败")?;
    if !response.status().is_success() { return Err(format!("CodeBuddy 模型目录 HTTP {}", response.status().as_u16())); }
    let mut data = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| "CodeBuddy 模型目录读取失败")? {
        if data.len() + chunk.len() > 32 * 1024 * 1024 { return Err("CodeBuddy 模型目录超过 32 MiB".into()); }
        data.extend_from_slice(&chunk);
    }
    let config: Value = serde_json::from_slice(&data).map_err(|_| "CodeBuddy 模型目录 JSON 无效")?;
    let models = merge_catalog(&config)?;
    let mut cache = cache.lock().unwrap();
    cache.retain(|_, (time, _)| time.elapsed() < Duration::from_secs(60));
    cache.insert(identity, (Instant::now(), models.clone()));
    Ok(models)
}

fn convert(model: &Value) -> Value {
    let efforts = model["reasoning"]["supportedEfforts"].as_array();
    let toggle = model["reasoning"]["canDisableThinking"] == true;
    let mut variants = Map::new();
    for effort in efforts.into_iter().flatten().filter_map(Value::as_str).chain(toggle.then_some("none")) {
        variants.insert(effort.into(), json!({ "reasoningEffort": effort }));
    }
    json!({ "name": model["name"].as_str().or_else(|| model["id"].as_str()).unwrap_or("CodeBuddy"),
        "limit": { "context": model["contextWindow"]["defaultLength"].as_u64().or_else(|| model["maxAllowedSize"].as_u64()).or_else(|| model["maxInputTokens"].as_u64()).unwrap_or(128000),
            "output": model["maxOutputTokens"].as_u64().unwrap_or(32000) },
        "modalities": { "input": if model["supportsImages"] == true { vec!["text","image"] } else { vec!["text"] } },
        "reasoning": model["supportsReasoning"] == true || !variants.is_empty(), "variants": variants,
        "options": { "supportsReasoningEffort": efforts.is_some(), "supportsThinkingToggle": toggle }
    })
}

fn default_model<'a>(catalog: &'a [Value], options: &Value) -> Result<&'a str, String> {
    let preferred = options["codebuddyModel"].as_str();
    catalog.iter().filter_map(|m| m["id"].as_str()).find(|id| Some(*id) == preferred)
        .or_else(|| catalog.iter().find_map(|m| m["id"].as_str())).ok_or_else(|| "CodeBuddy 没有可用模型".into())
}

pub(super) async fn models(options: &Value, force: bool) -> Result<Map<String, Value>, String> {
    let catalog = catalog(&provider::local_api_client(), options, force).await?;
    let mut models: Map<String, Value> = catalog.iter().filter_map(|m| m["id"].as_str().map(|id| (id.into(), convert(m)))).collect();
    let default = default_model(&catalog, options)?;
    let mut model = models[default].clone();
    let source = options["codebuddySource"].as_str().unwrap_or("codebuddy");
    model["name"] = json!(if source == "workbuddy" { "WorkBuddy（默认模型）" } else { "CodeBuddy（默认模型）" }); models.insert(source.into(), model);
    Ok(models)
}

pub(super) async fn stream(model: &ResolvedModel, key: &str, mut body: Value, session: Option<&str>,
    cancel: &Arc<AtomicBool>, on_event: &mut (dyn FnMut(StreamEvent) + Send)) -> Result<StreamResult, String> {
    let mut options = json!(model.extra_options);
    options["baseURL"] = json!(model.base_url); options["apiKey"] = json!(key); options["headers"] = json!(model.headers);
    let http = provider::local_api_client();
    let catalog = tokio::select! { result = catalog(&http, &options, false) => result?, _ = provider::wait_cancelled(cancel) => return Err("CodeBuddy 请求已取消".into()) };
    let id = if model.id == options["codebuddySource"].as_str().unwrap_or("codebuddy") { default_model(&catalog, &options)? } else { &model.id };
    let info = catalog.iter().find(|m| m["id"] == id).ok_or("CodeBuddy 模型不可用，请刷新模型列表")?;
    if let Some(effort) = body["reasoning_effort"].as_str() {
        if info["reasoning"]["supportedEfforts"].as_array().is_some_and(|efforts| !efforts.iter().any(|v| v == effort))
            && !(effort == "none" && info["reasoning"]["canDisableThinking"] == true) {
            return Err("CodeBuddy 模型不支持此思考档位".into());
        }
    }
    // Like the CLI, request visible reasoning summaries without overriding an explicit opt-out.
    if body["reasoning_summary"].is_null()
        && !matches!(body["reasoning_effort"].as_str(), Some("none" | "off"))
        && (info["supportsReasoning"] == true || body["reasoning_effort"].is_string()) {
        body["reasoning_summary"] = json!("auto");
    }
    if info["supportsToolCall"] == false && body["tools"].as_array().is_some_and(|t| !t.is_empty()) { return Err("CodeBuddy 当前模型不支持工具调用".into()); }
    if info["supportsImages"] == false && body["messages"].as_array().is_some_and(|messages| messages.iter().any(|m| m["content"].as_array().is_some_and(|parts| parts.iter().any(|p| p["type"] == "image_url")))) {
        return Err("CodeBuddy 当前模型不支持图片".into());
    }
    body["model"] = json!(id);
    body["max_tokens"] = json!(body[model.max_tokens_field].as_u64().unwrap_or(32000).min(info["maxOutputTokens"].as_u64().unwrap_or(u64::MAX)));
    for field in ["max_completion_tokens", "codebuddySource", "codebuddyModel", "codebuddyPackage", "codebuddyProxy", "prompt_cache_key", "prompt_cache_retention", "store", "metadata"] { body.as_object_mut().unwrap().remove(field); }
    let mut model = model.clone();
    model.headers.insert("X-Request-ID".into(), json!(uuid::Uuid::new_v4().to_string()));
    model.headers.insert("X-Conversation-ID".into(), json!(session.map(str::to_string).unwrap_or_else(|| uuid::Uuid::new_v4().to_string())));
    provider::stream_completions(&http, &model, key, body, session, cancel, on_event).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn buddy_providers_use_nova_login_with_or_without_cli() {
        assert!(normalize_auth(&json!({"refreshToken":"r"}), "d").is_none());
        let auth = normalize_auth(&json!({"accessToken":"tok","expiresIn":3600}), "www.codebuddy.cn").unwrap();
        assert_eq!(auth["domain"], "www.codebuddy.cn");
        assert!(auth["expiresAt"].as_u64().unwrap() > now_ms() + 3_000_000);
        for source in ["codebuddy", "workbuddy"] {
        let root = tempfile::tempdir().unwrap();
        let prefix = source.to_ascii_uppercase();
        let mut env = HashMap::new();
        assert!(load(root.path(), &env, source).unwrap_err().contains("Lyra 设置里登录"));
        save_session(&login_file(root.path(), source), &json!({"origin":"https://www.codebuddy.cn","auth":auth,"account":{"uid":"u1"}})).unwrap();
        let provider = load(root.path(), &env, source).unwrap();
        assert_eq!(provider["options"]["apiKey"], "tok");
        assert_eq!(provider["options"]["baseURL"], "https://www.codebuddy.cn/v2");
        assert_eq!(provider["options"]["headers"]["X-User-Id"], "u1");
        let path = root.path().to_string_lossy().to_string();
        env.extend([("HOME".into(), path.clone()), (format!("{prefix}_CONFIG_DIR"), path.clone()),
            (format!("NOVA_{prefix}_PACKAGE"), path.clone()), ("WORKBUDDY_EXE".into(), path),
            (format!("NOVA_{prefix}_AUTH_FILE"), root.path().join("auth.info").to_string_lossy().to_string())]);
        std::fs::write(root.path().join("auth.info"), r#"{"auth":{"accessToken":"cli-token","expiresAt":18446744073709551615}}"#).unwrap();
        std::fs::write(root.path().join("settings.json"), r#"{"model":"retired","env":{"CODEBUDDY_AUTH_TOKEN":"cli-key"}}"#).unwrap();
        std::fs::write(root.path().join("package.json"), r#"{"name":"@tencent-ai/codebuddy-code","version":"999.0.0"}"#).unwrap();
        std::fs::write(root.path().join("product.json"), "invalid json").unwrap();
        assert_eq!(load(root.path(), &env, source).unwrap(), provider);
        std::fs::write(root.path().join("settings.json"), "invalid json").unwrap();
        std::fs::write(root.path().join("auth.info.logged-out"), "").unwrap();
        assert_eq!(load(root.path(), &env, source).unwrap(), provider);
        std::fs::remove_file(login_file(root.path(), source)).unwrap();
        assert!(load(root.path(), &env, source).unwrap_err().contains("Lyra 设置里登录"));
        }
    }

    #[test]
    fn workbuddy_login_stays_separate_from_codebuddy() {
        let dir = tempfile::tempdir().unwrap();
        let auth = login_file(dir.path(), "workbuddy");
        save_session(&auth, &json!({"auth":{"accessToken":"workbuddy-key","expiresAt":u64::MAX,"domain":"www.workbuddy.cn"},"account":{"uid":"wb-user"}})).unwrap();
        save_session(&login_file(dir.path(), "codebuddy"), &json!({"auth":{"accessToken":"wrong-account"}})).unwrap();
        let mut env = HashMap::from([("CODEBUDDY_AUTH_TOKEN".into(),"wrong-account".into()),("CODEBUDDY_MODEL".into(),"wrong-model".into())]);
        let login = load(dir.path(),&env,"workbuddy").unwrap();
        assert_eq!(login["api"],"codebuddy"); assert_eq!(login["options"]["apiKey"],"workbuddy-key");
        assert_eq!(login["options"]["baseURL"],"https://www.workbuddy.cn/v2");
        assert_eq!(login["options"]["headers"]["X-Product"],"WorkBuddy"); assert_eq!(login["options"]["codebuddySource"],"workbuddy");
        assert!(login["options"]["codebuddyModel"].is_null());
        std::fs::remove_file(&auth).unwrap(); assert!(load(dir.path(),&env,"workbuddy").is_err());
        env.insert("WORKBUDDY_AUTH_TOKEN".into(),"wb-override".into());
        assert_eq!(load(dir.path(),&env,"workbuddy").unwrap()["options"]["apiKey"],"wb-override");
    }

    #[test]
    fn login_rotation_expiry_logout_and_model_capabilities() {
        let dir = tempfile::tempdir().unwrap(); let path = dir.path().to_string_lossy().to_string();
        let auth = login_file(dir.path(), "codebuddy");
        let env = HashMap::from([("USERPROFILE".into(),path)]);
        let login = |key: &str, expires: u64| std::fs::write(&auth,json!({"auth":{"accessToken":key,"expiresAt":expires,"domain":"www.codebuddy.ai"},"account":{"uid":"user"}}).to_string()).unwrap();
        login("first",u64::MAX);
        let first = load(dir.path(),&env,"codebuddy").unwrap(); assert_eq!(first["api"],"codebuddy"); assert_eq!(first["options"]["apiKey"],"first");
        login("second",u64::MAX); assert_eq!(load(dir.path(),&env,"codebuddy").unwrap()["options"]["apiKey"],"second");
        login("expired",1); assert!(load(dir.path(),&env,"codebuddy").unwrap_err().contains("过期"));
        std::fs::remove_file(&auth).unwrap(); assert!(load(dir.path(),&env,"codebuddy").unwrap_err().contains("登录"));
        let mut override_env = env.clone(); override_env.insert("CODEBUDDY_AUTH_TOKEN".into(),"override".into());
        assert_eq!(load(dir.path(),&override_env,"codebuddy").unwrap()["options"]["apiKey"],"override");
        override_env.insert("NOVA_CODEBUDDY_ENDPOINT".into(),"http://example.com".into()); assert!(load(dir.path(),&override_env,"codebuddy").is_err());
        let catalog = merge_catalog(&json!({"code":0,"data":{"agents":[{"name":"cli","models":["allowed"]}],"models":[{"id":"hidden"},
            {"id":"allowed","maxOutputTokens":1024,"supportsImages":true,"reasoning":{"supportedEfforts":["low","high"],"canDisableThinking":true}}]}})).unwrap();
        assert_eq!(catalog.len(),1);
        let model = convert(&catalog[0]); assert_eq!(model["limit"]["output"],1024);
        assert_eq!(model["modalities"]["input"],json!(["text","image"])); assert_eq!(model["variants"]["none"]["reasoningEffort"],"none");
        assert!(!dir.path().join("runtime").exists());
    }

    #[tokio::test]
    async fn stale_default_does_not_hide_cloud_models_or_read_cli_package() {
        let catalog = vec![json!({"id":"first"}), json!({"id":"second"})];
        for (preferred, expected) in [(Value::Null, "first"), (json!("retired"), "first"), (json!("second"), "second")] {
            assert_eq!(default_model(&catalog, &json!({"codebuddyModel":preferred})).unwrap(), expected);
        }
        assert!(default_model(&[], &json!({})).is_err());
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("product.json"), "invalid json").unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut bytes = Vec::new(); let mut byte = [0];
            while !bytes.ends_with(b"\r\n\r\n") { socket.read_exact(&mut byte).unwrap(); bytes.push(byte[0]); }
            let response = json!({"code":0,"data":{"models":catalog}}).to_string();
            write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).unwrap();
        });
        let models = models(&json!({"baseURL":format!("http://{address}/v2"), "codebuddyModel":"retired", "codebuddyPackage":dir.path()}), true).await.unwrap();
        server.join().unwrap();
        assert_eq!(models.len(), 3);
        assert!(models.contains_key("first") && models.contains_key("second"));
        assert_eq!(models["codebuddy"]["limit"], models["first"]["limit"]);
    }

    fn sse(value: Value) -> String { format!("data: {value}\r\n\r\n") }
    fn delta(value: Value) -> String { sse(json!({"choices":[{"index":0,"delta":value}]})) }
    fn tool(index: u64, id: Option<&str>, name: &str, args: &str) -> String {
        let mut call = json!({"index":index,"function":{"name":name,"arguments":args}});
        if let Some(id) = id { call["id"] = json!(id); }
        delta(json!({"tool_calls":[call]}))
    }

    #[tokio::test]
    async fn native_catalog_sse_tools_history_and_failures() {
        let ending = sse(json!({"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}));
        let calls = tool(0,Some("a"),"read","{\"path\":") + &tool(1,Some("b"),"read","{\"path\":")
            + &tool(0,None,"","\"a\"}") + &tool(1,None,"","\"b\"}");
        let usage = sse(json!({"choices":[],"usage":{"prompt_tokens":20,"completion_tokens":8,"prompt_cache_hit_tokens":5}}));
        let success = delta(json!({"reasoning_content":"思考","content":"你好"})) + &calls + &ending + &usage + "data: [DONE]\r\n\r\n";
        // 与 CLI 的 normalizeStreamingToolCallIds 一致：index 归并分片，重复 ID 不判冲突。
        let repeated = tool(0,Some("a"),"read","{\"path\":")
            + &tool(1,Some("a"),"read","{\"path\":")
            + &tool(0,Some("chunk-a"),"read","\"a\"}")
            + &tool(1,Some("chunk-b"),"read","\"b\"}");
        let wrapped = tool(0,Some("a"),"re",&serde_json::to_string("{\"path\":\"a\"}").unwrap())
            + &tool(0,Some("later-id"),"ad","")
            + &tool(1,Some("b"),"read","{\"path\":\"b\"}");
        let nested = tool(0,Some("a"),"read",r#"{"meta":"#)
            + &tool(0,None,"",r#"{"meta":"safe"}"#)
            + &tool(0,None,"",r#", "path":"a"}"#)
            + &tool(1,Some("b"),"read",r#"{"path":"b"}"#);
        let snapshot = |path: &str| delta(json!({"tool_calls":[{"index":0,"function":{"arguments":{"path":path}}}]}));
        let snapshots = tool(0,Some("a"),"read",r#"{"path":"#)
            + &tool(0,None,"",r#"{"path":"a"}"#)
            + &tool(0,None,"",r#"{"path":"a"}"#)
            + &snapshot("a")
            + &tool(1,Some("b"),"read",r#"{"path":"b"}"#);
        let conflict = tool(0,Some("a"),"read",r#"{"path":"a"}"#) + &snapshot("other");
        for (wire, good, effort, summary) in [
            (success.clone(),true,None,None),
            (success.clone(),true,Some("high"),None),
            (success.clone(),true,Some("none"),None),
            (success,true,Some("high"),Some("detailed")),
            (delta(json!({"reasoning_content":"思考"}))+&repeated+&ending+&usage+"data: [DONE]\r\n\r\n",true,None,None),
            (delta(json!({"reasoning_content":"思考"}))+&wrapped+&ending+&usage+"data: [DONE]\r\n\r\n",true,None,None),
            (delta(json!({"reasoning_content":"思考"}))+&nested+&ending+&usage+"data: [DONE]\r\n\r\n",true,None,None),
            (delta(json!({"reasoning_content":"思考"}))+&snapshots+&ending+&usage+"data: [DONE]\r\n\r\n",true,None,None),
            (conflict+&ending+"data: [DONE]\r\n\r\n",false,None,None),
            (calls.clone()+&ending,false,None,None),
            (tool(0,Some("a"),"read","{")+&ending+"data: [DONE]\r\n\r\n",false,None,None),
            (tool(0,Some("a"),"read",r#"{"path":"a"}"#)+&tool(1,Some("b"),"read","{")+&ending+"data: [DONE]\r\n\r\n",false,None,None),
            (sse(json!({"error":{"message":"upstream-secret"}}))+"data: [DONE]\r\n\r\n",false,None,None),
            ("data: malformed\r\n\r\ndata: [DONE]\r\n\r\n".into(),false,None,None),
        ] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap(); let address = listener.local_addr().unwrap();
            let server = std::thread::spawn(move || {
                let mut captured = None;
                for i in 0..2 {
                    let (mut socket,_) = listener.accept().unwrap(); socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                    let mut bytes = Vec::new(); let mut byte = [0];
                    while !bytes.ends_with(b"\r\n\r\n") { socket.read_exact(&mut byte).unwrap(); bytes.push(byte[0]); }
                    let headers = String::from_utf8(bytes).unwrap(); assert!(headers.to_lowercase().contains("authorization: bearer secret"));
                    let length: usize = headers.lines().find_map(|l| l.to_lowercase().strip_prefix("content-length: ").map(str::to_string)).unwrap_or("0".into()).parse().unwrap();
                    let mut body = vec![0;length]; socket.read_exact(&mut body).unwrap();
                    let (content_type, response) = if i==0 {
                        assert!(headers.starts_with("GET /v3/config "));
                        ("application/json",json!({"code":0,"data":{"models":[{"id":"test","maxOutputTokens":1024,"supportsToolCall":true,
                            "supportsReasoning":true,"reasoning":{"supportedEfforts":["high"],"canDisableThinking":true}}]}}).to_string())
                    } else {
                        assert!(headers.starts_with("POST /v2/chat/completions "));
                        captured = Some(serde_json::from_slice::<Value>(&body).unwrap()); ("text/event-stream",wire.clone())
                    };
                    write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",response.len()).unwrap();
                    for chunk in response.as_bytes().chunks(7) { socket.write_all(chunk).unwrap(); }
                }
                captured.unwrap()
            });
            let model = super::super::config::resolve_model(&json!({"provider":{"codebuddy":{"api":"codebuddy","options":{"baseURL":format!("http://{address}/v2"),
                "codebuddyModel":"retired", "requiresReasoningContentOnAssistantMessages":true},"models":{"codebuddy":{}}}}}),Some("codebuddy/codebuddy"),&HashMap::new()).unwrap().model;
            let mut body = provider::completions_body(&model,"system",&[
                json!({"role":"assistant","api":model.api,"provider":model.provider,"model":model.id,"content":[{"type":"thinking","thinking":"先读文件"},{"type":"toolCall","id":"prior","name":"read","arguments":{"path":"prior"}}]}),
                json!({"role":"toolResult","toolCallId":"prior","content":[{"type":"text","text":"previous output"}]}),
            ],&[],None,Some("session"));
            body["tools"] = json!([{"function":{"name":"read","parameters":{"type":"object"}}}]);
            if let Some(effort) = effort { body["reasoning_effort"] = json!(effort); }
            if let Some(summary) = summary { body["reasoning_summary"] = json!(summary); }
            let mut events = Vec::new();
            let result = stream(&model,"secret",body,Some("session"),&Arc::new(AtomicBool::new(false)),&mut |e|events.push(e)).await;
            if good {
                let result = result.unwrap(); assert_eq!(result.stop_reason,"toolUse"); assert_eq!(result.usage["input"],15);
                assert!(events.iter().any(|e|matches!(e,StreamEvent::ThinkingDelta(text) if text == "思考")));
                assert!(result.content.iter().any(|part| *part == json!({"type":"thinking","thinking":"思考"})));
                let calls: Vec<_> = result.content.iter().filter(|p|p["type"]=="toolCall").collect();
                assert_eq!(calls.len(),2); assert_eq!(calls[0]["arguments"]["path"],"a"); assert_eq!(calls[1]["arguments"]["path"],"b");
                assert_ne!(calls[0]["id"], calls[1]["id"]);
                assert_eq!(events.iter().filter(|e|matches!(e,StreamEvent::ToolCallDone{..})).count(),2);
            } else {
                let error = result.unwrap_err();
                assert!(!error.contains("upstream-secret"));
                if error.contains("工具 JSON 参数无效") { assert!(super::super::prompt::is_retryable_provider_error(&error)); }
                assert!(!events.iter().any(|e|matches!(e,StreamEvent::ToolCallDone{..})));
            }
            let captured = server.join().unwrap(); assert_eq!(captured["model"],"test"); assert_eq!(captured["max_tokens"],1024);
            assert_eq!(captured["reasoning_effort"],json!(effort));
            assert_eq!(captured["reasoning_summary"],json!(summary.or(if effort == Some("none") { None } else { Some("auto") })));
            assert!(captured.get("codebuddyModel").is_none()); assert!(captured.get("prompt_cache_key").is_none());
            assert_eq!(captured["messages"][1]["reasoning_content"],"先读文件"); assert_eq!(captured["messages"][2]["tool_call_id"],"prior");
        }
    }
}
