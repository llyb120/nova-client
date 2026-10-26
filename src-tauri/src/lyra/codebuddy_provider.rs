//! CodeBuddy login/catalog adapter; inference reuses Lyra's native OpenAI SSE transport.
use super::config::ResolvedModel;
use super::provider::{self, StreamEvent, StreamResult};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{atomic::AtomicBool, Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn json_file(path: &Path, optional: bool) -> Result<Value, String> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if optional && e.kind() == std::io::ErrorKind::NotFound => return Ok(json!({})),
        Err(_) => return Err("无法读取 CodeBuddy 配置或登录，请先在 CodeBuddy CLI 登录".into()),
    };
    let mut bytes = Vec::new();
    file.take(4 * 1024 * 1024 + 1).read_to_end(&mut bytes).map_err(|_| "读取 CodeBuddy 配置失败")?;
    if bytes.len() > 4 * 1024 * 1024 { return Err("CodeBuddy 配置超过 4 MiB".into()); }
    serde_json::from_slice(&bytes).map_err(|_| "CodeBuddy 配置或登录格式无效".into())
}

fn installed(env: &HashMap<String, String>) -> (Option<PathBuf>, String) {
    let mut roots = Vec::new();
    if let Some(path) = env.get("NOVA_CODEBUDDY_PACKAGE") { roots.push(PathBuf::from(path)); }
    if let Some(path) = env.get("APPDATA") { roots.push(Path::new(path).join("npm/node_modules/@tencent-ai/codebuddy-code")); }
    if let Some(path) = env.get("PATH").or_else(|| env.get("Path")) {
        for path in std::env::split_paths(path) {
            roots.push(path.join("node_modules/@tencent-ai/codebuddy-code"));
            roots.push(path.join("../lib/node_modules/@tencent-ai/codebuddy-code"));
        }
    }
    for root in roots {
        if let Ok(pkg) = json_file(&root.join("package.json"), true) {
            if pkg["name"] == "@tencent-ai/codebuddy-code" {
                if let Some(version) = pkg["version"].as_str() { return (Some(root), version.to_string()); }
            }
        }
    }
    // ponytail: native installs lack npm metadata; update this verified version when CLI headers change.
    (None, "2.169.0".into())
}

fn workbuddy_token(auth_path: &Path, exe: &Path, encrypted: &Value, env: &HashMap<String, String>) -> Result<String, String> {
    use wait_timeout::ChildExt;
    static CACHE: OnceLock<Mutex<Option<(String, String)>>> = OnceLock::new();
    let modified = std::fs::metadata(exe).and_then(|m| m.modified()).map_err(|_| "未找到 WorkBuddy 桌面程序，无法读取加密登录")?;
    let identity = format!("{}:{modified:?}:{encrypted}", exe.display());
    let cache = CACHE.get_or_init(Default::default);
    if let Some((_, key)) = cache.lock().unwrap().as_ref().filter(|(id, _)| id == &identity) { return Ok(key.clone()); }
    let mut command = std::process::Command::new(exe);
    command.args(["-e", include_str!("../../../scripts/workbuddy-login.cjs"), "--", "--nova-workbuddy-login"]).arg(auth_path)
        .envs(env).env("ELECTRON_RUN_AS_NODE", "1").stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null());
    #[cfg(windows)]
    { use std::os::windows::process::CommandExt; command.creation_flags(0x0800_0000); }
    let error = "无法解密 WorkBuddy 登录，请打开 WorkBuddy 重新登录";
    let mut child = command.spawn().map_err(|_| error)?;
    match child.wait_timeout(Duration::from_secs(15)) {
        Ok(Some(status)) if status.success() => {}
        _ => { let _ = child.kill(); let _ = child.wait(); return Err(error.into()); }
    }
    let mut bytes = Vec::new();
    child.stdout.take().ok_or(error)?.take(65537).read_to_end(&mut bytes).map_err(|_| error)?;
    if bytes.len() > 65536 { return Err(error.into()); }
    let data: Value = serde_json::from_slice(&bytes).map_err(|_| error)?;
    if data["envelope"] != *encrypted { return Err("WorkBuddy 登录在读取期间已更新，请重试".into()); }
    let key = data["accessToken"].as_str().filter(|s| !s.trim().is_empty()).ok_or(error)?.to_string();
    *cache.lock().unwrap() = Some((identity, key.clone()));
    Ok(key)
}

pub(super) fn load(root: &Path, env: &HashMap<String, String>, source: &str) -> Result<Value, String> {
    let workbuddy = source == "workbuddy";
    let prefix = source.to_ascii_uppercase();
    let label = if workbuddy { "WorkBuddy" } else { "CodeBuddy" };
    let home = env.get("USERPROFILE").or_else(|| env.get("HOME")).map(PathBuf::from).ok_or("无法定位 CodeBuddy 用户目录")?;
    let config_dir = env.get(&format!("{prefix}_CONFIG_DIR")).map(PathBuf::from).unwrap_or_else(|| home.join(format!(".{source}")));
    let data_dir = if cfg!(windows) { home.join("AppData/Local") } else if cfg!(target_os = "macos") { home.join("Library/Application Support") } else { home.join(".local/share") };
    let auth_path = env.get(&format!("NOVA_{prefix}_AUTH_FILE")).map(PathBuf::from)
        .unwrap_or_else(|| data_dir.join(format!("CodeBuddyExtension/Data/Public/auth/{}.info", if workbuddy { "workbuddy-desktop" } else { "Tencent-Cloud.coding-copilot" })));
    let settings = json_file(&config_dir.join("settings.json"), true)?;
    let variable = |name: &str| settings["env"][name].as_str().or_else(|| env.get(name).map(String::as_str)).filter(|s| !s.trim().is_empty());
    let explicit_key = if workbuddy {
        env.get("WORKBUDDY_AUTH_TOKEN").or_else(|| env.get("WORKBUDDY_API_KEY")).map(String::as_str)
            .or_else(|| settings["env"]["CODEBUDDY_AUTH_TOKEN"].as_str()).or_else(|| settings["env"]["CODEBUDDY_API_KEY"].as_str())
    } else { variable("CODEBUDDY_AUTH_TOKEN").or_else(|| variable("CODEBUDDY_API_KEY")) }.filter(|s| !s.trim().is_empty());
    let nova_settings = crate::settings::Settings::load(&root.to_path_buf());
    let workbuddy_package = || -> Result<(PathBuf, Value), String> {
        let cli = crate::workbuddy::program(env.get("WORKBUDDY_EXE").map(String::as_str).unwrap_or(&nova_settings.workbuddy_path));
        let package = env.get("NOVA_WORKBUDDY_PACKAGE").map(PathBuf::from)
            .or_else(|| Path::new(&cli).parent().and_then(Path::parent).map(Path::to_path_buf)).ok_or("未找到 WorkBuddy 内置 CLI")?;
        let product = json_file(&package.join("product.json"), false)?;
        if product["authentication"]["id"] != "workbuddy-desktop" { return Err("请选择 WorkBuddy 内置 CLI，而不是全局 CodeBuddy".into()); }
        Ok((package, product))
    };
    // 优先用可用的 CLI/桌面端登录；没有时用在 Nova 里网页登录保存的会话。
    let cli_session = || -> Result<(Value, Option<PathBuf>, String), String> {
        let mut marker = auth_path.as_os_str().to_os_string(); marker.push(".logged-out");
        if Path::new(&marker).exists() { return Err("CodeBuddy 已注销，请先在 CLI 登录".into()); }
        let mut session = json_file(&auth_path, false)?;
        if session["auth"]["expiresAt"].as_u64().is_some_and(|expiry| expiry <= now_ms()) {
            return Err("CodeBuddy 登录已过期，请打开 CLI 刷新登录".into());
        }
        if !workbuddy { let (package, version) = installed(env); return Ok((session, package, version)); }
        let (package, product) = workbuddy_package()?;
        if session["auth"]["accessToken"].is_object() {
            let exe = env.get("WORKBUDDY_EXE").map(PathBuf::from).unwrap_or_else(|| package.join("../../../WorkBuddy.exe"));
            session["auth"]["accessToken"] = json!(workbuddy_token(&auth_path, &exe, &session["auth"]["accessToken"], env)?);
        }
        Ok((session, Some(package), product["genieVersion"].as_str().unwrap_or(WORKBUDDY_VERSION).to_string()))
    };
    let (session, package, version) = if explicit_key.is_some() {
        let (package, version) = if workbuddy { workbuddy_package().map(|(p, product)| (Some(p), product["genieVersion"].as_str().unwrap_or(WORKBUDDY_VERSION).to_string()))? } else { installed(env) };
        (json!({}), package, version)
    } else {
        match cli_session() {
            Ok(found) => found,
            Err(error) => {
                let session = nova_session(root, source).map_err(|_| format!("{error}；也可以在 Lyra 设置里登录 {label}"))?;
                let fallback = if workbuddy { (None, WORKBUDDY_VERSION.to_string()) } else { installed(env) };
                (session, fallback.0, fallback.1)
            }
        }
    };
    let key = explicit_key.or_else(|| session["auth"]["accessToken"].as_str()).filter(|s| !s.trim().is_empty())
        .ok_or("CodeBuddy 登录缺失或已加密，请先在 CLI 登录")?;
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
    Ok(json!({ "name": format!("{label}（本地 API）"), "api": "codebuddy", "options": {
        "baseURL": format!("{}/v2", url.origin().ascii_serialization()), "apiKey": key, "headers": headers,
        "codebuddySource": source, "codebuddyPackage": package,
        "codebuddyModel": env.get(&format!("{prefix}_MODEL")).map(String::as_str).or_else(|| settings["model"].as_str()),
        "requiresReasoningContentOnAssistantMessages": true
    }, "models": {} }))
}

const WORKBUDDY_VERSION: &str = "5.8.2";

fn now_ms() -> u64 { SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64 }

// ---------- 网页登录（本机没有可用 CLI 时） ----------
// 与 CodeBuddy CLI 2.174.0 相同的外链登录：申请 state → 浏览器登录 → 轮询 token → 取账号。
// 会话按 CLI 的 {auth, account} 形状存在 ~/.nova/<source>-login.json，复用上面的解析。

fn login_file(root: &Path, source: &str) -> PathBuf { root.join(format!("{source}-login.json")) }

pub(crate) fn login_origin(source: &str, site: &str) -> &'static str {
    match (source, site) { ("workbuddy", _) => "https://www.workbuddy.cn", (_, "cn") => "https://www.codebuddy.cn", _ => "https://www.codebuddy.ai" }
}

/// 读取 Nova 登录会话，快过期时用 refreshToken 续期并写回。
fn nova_session(root: &Path, source: &str) -> Result<Value, String> {
    let path = login_file(root, source);
    let mut session = json_file(&path, false)?;
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

fn merge_catalog(config: &Value, defaults: &[Value]) -> Result<Vec<Value>, String> {
    if config["code"] != 0 { return Err("CodeBuddy 模型目录返回错误".into()); }
    let cloud = config["data"]["models"].as_array().ok_or("CodeBuddy 模型目录格式无效")?;
    let mut merged: HashMap<String, Value> = HashMap::new();
    for model in defaults.iter().chain(cloud) {
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
    let mut defaults = Vec::new();
    if let Some(package) = options["codebuddyPackage"].as_str() {
        let base = json_file(&Path::new(package).join("product.json"), true)?;
        if let Some(models) = base["models"].as_array() { defaults.extend(models.clone()); }
        let base_url = options["baseURL"].as_str().unwrap_or("");
        if base_url.contains("copilot.tencent.com") || base_url.contains("codebuddy.cn") {
            let regional = json_file(&Path::new(package).join("product.internal.json"), true)?;
            if let Some(models) = regional["models"].as_array() { defaults.extend(models.clone()); }
        }
    }
    let models = merge_catalog(&config, &defaults)?;
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

pub(super) async fn models(options: &Value, force: bool) -> Result<Map<String, Value>, String> {
    let catalog = catalog(&provider::local_api_client(), options, force).await?;
    let mut models: Map<String, Value> = catalog.iter().filter_map(|m| m["id"].as_str().map(|id| (id.into(), convert(m)))).collect();
    let default = options["codebuddyModel"].as_str().or_else(|| catalog.first().and_then(|m| m["id"].as_str())).ok_or("CodeBuddy 没有可用模型")?;
    let mut model = models.get(default).cloned().ok_or("CodeBuddy 默认模型不在当前账号模型列表中")?;
    let source = options["codebuddySource"].as_str().unwrap_or("codebuddy");
    model["name"] = json!(format!("{}（跟随 CLI 默认模型）", if source == "workbuddy" { "WorkBuddy" } else { "CodeBuddy" })); models.insert(source.into(), model);
    Ok(models)
}

pub(super) async fn stream(model: &ResolvedModel, key: &str, mut body: Value, session: Option<&str>,
    cancel: &Arc<AtomicBool>, on_event: &mut (dyn FnMut(StreamEvent) + Send)) -> Result<StreamResult, String> {
    let mut options = json!(model.extra_options);
    options["baseURL"] = json!(model.base_url); options["apiKey"] = json!(key); options["headers"] = json!(model.headers);
    let http = provider::local_api_client();
    let catalog = tokio::select! { result = catalog(&http, &options, false) => result?, _ = provider::wait_cancelled(cancel) => return Err("CodeBuddy 请求已取消".into()) };
    let id = if model.id == options["codebuddySource"].as_str().unwrap_or("codebuddy") { options["codebuddyModel"].as_str().or_else(|| catalog.first().and_then(|m| m["id"].as_str())).unwrap_or("") } else { &model.id };
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
    fn nova_login_session_is_used_when_cli_login_is_missing() {
        assert!(normalize_auth(&json!({"refreshToken":"r"}), "d").is_none());
        let auth = normalize_auth(&json!({"accessToken":"tok","expiresIn":3600}), "www.codebuddy.cn").unwrap();
        assert_eq!(auth["domain"], "www.codebuddy.cn");
        assert!(auth["expiresAt"].as_u64().unwrap() > now_ms() + 3_000_000);
        let root = tempfile::tempdir().unwrap();
        let mut env = HashMap::new();
        env.insert("HOME".to_string(), root.path().to_string_lossy().into_owned());
        env.insert("NOVA_CODEBUDDY_AUTH_FILE".to_string(), root.path().join("missing.info").to_string_lossy().into_owned());
        assert!(load(root.path(), &env, "codebuddy").unwrap_err().contains("Lyra 设置里登录"));
        save_session(&login_file(root.path(), "codebuddy"), &json!({"origin":"https://www.codebuddy.cn","auth":auth,"account":{"uid":"u1"}})).unwrap();
        let provider = load(root.path(), &env, "codebuddy").unwrap();
        assert_eq!(provider["options"]["apiKey"], "tok");
        assert_eq!(provider["options"]["baseURL"], "https://www.codebuddy.cn/v2");
        assert_eq!(provider["options"]["headers"]["X-User-Id"], "u1");
    }

    #[test]
    fn workbuddy_login_stays_separate_from_codebuddy() {
        let dir = tempfile::tempdir().unwrap(); let path = dir.path().to_string_lossy().to_string();
        let auth = dir.path().join("auth.info");
        std::fs::write(dir.path().join("product.json"),r#"{"authentication":{"id":"workbuddy-desktop"},"genieVersion":"5.8.2"}"#).unwrap();
        std::fs::write(&auth,json!({"auth":{"accessToken":"workbuddy-key","expiresAt":u64::MAX,"domain":"www.workbuddy.cn"},"account":{"uid":"wb-user"}}).to_string()).unwrap();
        let mut env = HashMap::from([("USERPROFILE".into(),path.clone()),("WORKBUDDY_CONFIG_DIR".into(),path.clone()),
            ("NOVA_WORKBUDDY_PACKAGE".into(),path),("NOVA_WORKBUDDY_AUTH_FILE".into(),auth.to_string_lossy().to_string()),
            ("CODEBUDDY_AUTH_TOKEN".into(),"wrong-account".into()),("CODEBUDDY_MODEL".into(),"wrong-model".into())]);
        let login = load(dir.path(),&env,"workbuddy").unwrap();
        assert_eq!(login["api"],"codebuddy"); assert_eq!(login["options"]["apiKey"],"workbuddy-key");
        assert_eq!(login["options"]["baseURL"],"https://www.workbuddy.cn/v2");
        assert_eq!(login["options"]["headers"]["X-Product"],"WorkBuddy"); assert_eq!(login["options"]["codebuddySource"],"workbuddy");
        assert!(login["options"]["codebuddyModel"].is_null());
        std::fs::write(dir.path().join("auth.info.logged-out"),"").unwrap(); assert!(load(dir.path(),&env,"workbuddy").is_err());
        env.insert("WORKBUDDY_AUTH_TOKEN".into(),"wb-override".into());
        assert_eq!(load(dir.path(),&env,"workbuddy").unwrap()["options"]["apiKey"],"wb-override");
        std::fs::write(dir.path().join("product.json"),r#"{"authentication":{"id":"codebuddy"}}"#).unwrap();
        assert!(load(dir.path(),&env,"workbuddy").is_err());
    }

    #[test]
    fn login_rotation_expiry_logout_and_model_capabilities() {
        let dir = tempfile::tempdir().unwrap(); let path = dir.path().to_string_lossy().to_string();
        let auth = dir.path().join("auth.info");
        let env = HashMap::from([("USERPROFILE".into(),path.clone()),("CODEBUDDY_CONFIG_DIR".into(),path),
            ("NOVA_CODEBUDDY_AUTH_FILE".into(),auth.to_string_lossy().to_string())]);
        let login = |key: &str, expires: u64| std::fs::write(&auth,json!({"auth":{"accessToken":key,"expiresAt":expires,"domain":"www.codebuddy.ai"},"account":{"uid":"user"}}).to_string()).unwrap();
        login("first",u64::MAX);
        let first = load(dir.path(),&env,"codebuddy").unwrap(); assert_eq!(first["api"],"codebuddy"); assert_eq!(first["options"]["apiKey"],"first");
        login("second",u64::MAX); assert_eq!(load(dir.path(),&env,"codebuddy").unwrap()["options"]["apiKey"],"second");
        login("expired",1); assert!(load(dir.path(),&env,"codebuddy").unwrap_err().contains("过期"));
        login("secret",u64::MAX); std::fs::write(dir.path().join("auth.info.logged-out"),"").unwrap(); assert!(load(dir.path(),&env,"codebuddy").unwrap_err().contains("注销"));
        let mut override_env = env.clone(); override_env.insert("CODEBUDDY_AUTH_TOKEN".into(),"override".into());
        assert_eq!(load(dir.path(),&override_env,"codebuddy").unwrap()["options"]["apiKey"],"override");
        override_env.insert("NOVA_CODEBUDDY_ENDPOINT".into(),"http://example.com".into()); assert!(load(dir.path(),&override_env,"codebuddy").is_err());
        let catalog = merge_catalog(&json!({"code":0,"data":{"agents":[{"name":"cli","models":["allowed"]}],"models":[{"id":"hidden"},{"id":"allowed","maxOutputTokens":1024}]}}),
            &[json!({"id":"allowed","supportsImages":true,"reasoning":{"supportedEfforts":["low","high"],"canDisableThinking":true}})]).unwrap();
        assert_eq!(catalog.len(),1);
        let model = convert(&catalog[0]); assert_eq!(model["limit"]["output"],1024);
        assert_eq!(model["modalities"]["input"],json!(["text","image"])); assert_eq!(model["variants"]["none"]["reasoningEffort"],"none");
        assert!(!dir.path().join("runtime").exists());
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
        for (wire, good, effort, summary) in [
            (success.clone(),true,None,None),
            (success.clone(),true,Some("high"),None),
            (success.clone(),true,Some("none"),None),
            (success,true,Some("high"),Some("detailed")),
            (delta(json!({"reasoning_content":"思考"}))+&repeated+&ending+&usage+"data: [DONE]\r\n\r\n",true,None,None),
            (delta(json!({"reasoning_content":"思考"}))+&wrapped+&ending+&usage+"data: [DONE]\r\n\r\n",true,None,None),
            (calls.clone()+&ending,false,None,None),
            (tool(0,Some("a"),"read","{")+&ending+"data: [DONE]\r\n\r\n",false,None,None),
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
                "requiresReasoningContentOnAssistantMessages":true},"models":{"codebuddy":{}}}}}),Some("codebuddy/codebuddy"),&HashMap::new()).unwrap().model;
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
                assert!(!result.unwrap_err().contains("upstream-secret"));
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
