//! Read-only CLI subscription credentials. Call at request time, not to decide whether models exist.
//! Never persist/refresh tokens or execute apiKeyHelper. Callers must pin endpoint(), disable
//! redirects, keep credentials out of config/logs, and sanitize upstream authentication errors.
use base64::{engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD}, Engine as _};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::{collections::HashMap, io::Read, path::{Path, PathBuf}, time::{SystemTime, UNIX_EPOCH}};

pub(super) const CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";
pub(super) const CLAUDE_BASE_URL: &str = "https://api.anthropic.com";
pub(super) const CLAUDE_OAUTH_BETA: &str = "oauth-2025-04-20";
pub(super) const CODEX_RESPONSES_BETA: &str = "responses=experimental";
const MAX_BYTES: u64 = 1024 * 1024;
const CLOCK_SKEW_MS: u64 = 60_000;
const CODEX_LOGIN: &str = "Codex 登录无效、过期或即将过期；请先在 Codex CLI 更新登录（codex login），再重试";
const CLAUDE_LOGIN: &str = "Claude Code 登录无效、过期或即将过期；请先在 Claude Code CLI 更新登录（/login 或 claude auth login），再重试";

// Intentionally neither Debug nor Serialize: errors/config must never include the key.
pub(super) struct Credentials {
    pub key: String,
    pub headers: Map<String, Value>,
}

pub(super) fn endpoint(source: &str) -> Result<&'static str, String> {
    match source {
        "codex" => Ok(CODEX_BASE_URL),
        "claude-code" => Ok(CLAUDE_BASE_URL),
        _ => Err("不支持的本地登录来源".into()),
    }
}

pub(super) fn credentials(source: &str, env: &HashMap<String, String>) -> Result<Credentials, String> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|_| "系统时间无效")?;
    credentials_with(source, env, now.as_millis().try_into().map_err(|_| "系统时间无效")?,
        cfg!(target_os = "macos"), &read_keychain)
}

fn text(value: &Value) -> Option<&str> {
    value.as_str().map(str::trim).filter(|s| !s.is_empty())
}

fn variable<'a>(env: &'a HashMap<String, String>, key: &str) -> Option<&'a str> {
    env.get(key).map(String::as_str).map(str::trim).filter(|s| !s.is_empty())
}

fn root(env: &HashMap<String, String>, name: &str, folder: &str) -> Result<PathBuf, String> {
    if let Some(path) = variable(env, name) { return Ok(PathBuf::from(path)); }
    variable(env, "USERPROFILE").or_else(|| variable(env, "HOME"))
        .map(|path| PathBuf::from(path).join(folder)).ok_or_else(|| "无法确定本地 CLI 配置目录".into())
}

fn read_file(path: &Path) -> Result<Option<Vec<u8>>, String> {
    // Check before opening too: do not block indefinitely on a pipe masquerading as a config file.
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("无法读取本地 CLI 配置或登录文件".into()),
    };
    if !metadata.is_file() || metadata.len() > MAX_BYTES {
        return Err("本地 CLI 配置或登录文件类型/大小无效".into());
    }
    let file = std::fs::File::open(path).map_err(|_| "无法读取本地 CLI 配置或登录文件")?;
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1).read_to_end(&mut bytes).map_err(|_| "无法读取本地 CLI 配置或登录文件")?;
    if bytes.len() as u64 > MAX_BYTES { return Err("本地 CLI 配置或登录文件过大".into()); }
    Ok(Some(bytes))
}

fn object(bytes: &[u8]) -> Result<Value, String> {
    if bytes.len() as u64 > MAX_BYTES { return Err("本地 CLI 配置或登录数据过大".into()); }
    serde_json::from_slice::<Value>(bytes).ok().filter(Value::is_object)
        .ok_or_else(|| "本地 CLI 配置或登录数据不是有效 JSON 对象".into())
}

fn header_text(value: &str) -> bool {
    !value.is_empty() && value.len() <= 64 * 1024 && value.bytes().all(|b| b.is_ascii_graphic())
}

fn jwt(token: &str) -> Option<Value> {
    // Unverified claims are only expiry/routing hints, never authority to choose an upstream.
    if !header_text(token) { return None; }
    let mut parts = token.split('.');
    let header = parts.next()?;
    let payload = parts.next()?;
    let signature = parts.next()?;
    if header.is_empty() || signature.is_empty() || parts.next().is_some() { return None; }
    let bytes = URL_SAFE_NO_PAD.decode(payload).or_else(|_| URL_SAFE.decode(payload)).ok()?;
    serde_json::from_slice::<Value>(&bytes).ok().filter(Value::is_object)
}

fn codex_token(auth: &Value, now: u64) -> Result<Credentials, String> {
    if auth.get("auth_mode").is_some_and(|v| !v.is_null() && v != "chatgpt")
        || text(&auth["OPENAI_API_KEY"]).is_some() {
        return Err("Codex 当前不是 ChatGPT 订阅登录；API Key 请使用普通 API provider".into());
    }
    let tokens = &auth["tokens"];
    let key = text(&tokens["access_token"]).ok_or(CODEX_LOGIN)?;
    let claims = jwt(key).ok_or(CODEX_LOGIN)?;
    let expires = claims["exp"].as_u64().and_then(|s| s.checked_mul(1000)).ok_or(CODEX_LOGIN)?;
    if expires <= now.saturating_add(CLOCK_SKEW_MS) { return Err(CODEX_LOGIN.into()); }
    let id_claims = text(&tokens["id_token"]).and_then(jwt).unwrap_or(Value::Null);
    let access_auth = &claims["https://api.openai.com/auth"];
    let id_auth = &id_claims["https://api.openai.com/auth"];
    if access_auth["chatgpt_account_is_fedramp"] == true || id_auth["chatgpt_account_is_fedramp"] == true {
        return Err("此 Codex 账户要求专用上游，不能使用通用 ChatGPT 端点".into());
    }
    let account = text(&tokens["account_id"]).or_else(|| text(&access_auth["chatgpt_account_id"]))
        .or_else(|| text(&id_auth["chatgpt_account_id"])).filter(|s| header_text(s)).ok_or(CODEX_LOGIN)?;
    Ok(Credentials { key: key.into(), headers: Map::from_iter([
        ("chatgpt-account-id".into(), json!(account)),
        ("OpenAI-Beta".into(), json!(CODEX_RESPONSES_BETA)),
        ("originator".into(), json!("nova")),
    ]) })
}

fn claude_token(token: &str) -> Result<Credentials, String> {
    if !header_text(token) || !token.starts_with("sk-ant-oat") { return Err(CLAUDE_LOGIN.into()); }
    Ok(Credentials { key: token.into(), headers: Map::from_iter([
        ("anthropic-beta".into(), json!(CLAUDE_OAUTH_BETA)),
    ]) })
}

fn codex_account(root: &Path) -> String {
    let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    format!("cli|{}", &format!("{:x}", Sha256::digest(canonical.to_string_lossy().as_bytes()))[..16])
}

fn claude_service(env: &HashMap<String, String>) -> String {
    let suffix = env.get("CLAUDE_CONFIG_DIR").filter(|s| !s.is_empty())
        .map(|path| format!("-{}", &format!("{:x}", Sha256::digest(path.as_bytes()))[..8])).unwrap_or_default();
    format!("Claude Code-credentials{suffix}")
}

fn credentials_with(
    source: &str, env: &HashMap<String, String>, now: u64, macos: bool,
    keychain: &dyn Fn(&str, &str) -> Result<Option<Vec<u8>>, String>,
) -> Result<Credentials, String> {
    endpoint(source)?;
    // ponytail: Read-only access tokens; never rotate another process's refresh token. Add refresh
    // only with CLI-compatible cross-process locking, rotation recovery and atomic secure storage.
    let bytes = if source == "codex" {
        let root = root(env, "CODEX_HOME", ".codex")?;
        let config = match read_file(&root.join("config.toml"))? {
            Some(bytes) => std::str::from_utf8(&bytes).ok().and_then(|s| toml::from_str::<toml::Value>(s).ok())
                .ok_or("Codex 配置不是有效 TOML")?,
            None => toml::Value::Table(Default::default()),
        };
        let mode = match config.get("cli_auth_credentials_store") {
            Some(value) => value.as_str().ok_or("Codex 登录存储配置无效")?,
            None => "file",
        };
        match mode {
            "file" => read_file(&root.join("auth.json"))?,
            "keyring" | "auto" => {
                if !macos || config.get("features").and_then(|v| v.get("secret_auth_storage"))
                    .and_then(toml::Value::as_bool) == Some(true) {
                    return Err("暂不支持此 Codex keyring 后端；请使用 CLI 的 file 登录存储或普通 API provider".into());
                }
                // Fail closed on locked/denied keychains instead of silently selecting a stale file.
                match keychain("Codex Auth", &codex_account(&root)).map_err(|_| "无法读取 Codex keychain，请在 CLI 中检查登录")? {
                    Some(bytes) => Some(bytes),
                    None if mode == "auto" => read_file(&root.join("auth.json"))?,
                    None => None,
                }
            }
            "ephemeral" => return Err("Codex 内存登录不能由 Lyra 导入，请在 CLI 中使用持久登录".into()),
            _ => return Err("不支持的 Codex 登录存储配置".into()),
        }.ok_or(CODEX_LOGIN)?
    } else {
        let root = root(env, "CLAUDE_CONFIG_DIR", ".claude")?;
        let settings = read_file(&root.join("settings.json"))?.map(|bytes| {
            std::str::from_utf8(&bytes).ok().and_then(|raw| crate::lyra_complete::parse_jsonc(raw).ok())
                .filter(Value::is_object).ok_or_else(|| "Claude Code 设置不是有效 JSON 对象".to_string())
        }).transpose()?.unwrap_or_else(|| json!({}));
        let mut effective = env.clone();
        if let Some(values) = settings["env"].as_object() {
            for (name, value) in values {
                if let Some(value) = value.as_str() { effective.insert(name.clone(), value.into()); }
            }
        }
        if text(&settings["apiKeyHelper"]).is_some() {
            return Err("不会执行 Claude Code apiKeyHelper；请使用普通 API provider".into());
        }
        if ["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN"].iter().any(|name| variable(&effective, name).is_some()) {
            return Err("Claude Code 已配置 API 认证，请使用普通 API provider".into());
        }
        if ["CLAUDE_CODE_USE_BEDROCK", "CLAUDE_CODE_USE_VERTEX", "CLAUDE_CODE_USE_FOUNDRY"].iter()
            .any(|name| variable(&effective, name).is_some_and(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))) {
            return Err("Claude Code 当前使用云平台认证，不是订阅登录".into());
        }
        if let Some(token) = variable(&effective, "CLAUDE_CODE_OAUTH_TOKEN") {
            // setup-token/env credentials are opaque and have no expiry metadata. Do not invent an
            // expiry or borrow one from disk; the caller must turn upstream 401 into a login prompt.
            return claude_token(token);
        }
        if variable(&effective, "CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR").is_some() {
            return Err("不读取其它进程的 Claude Code token 文件描述符，请在 CLI 中登录".into());
        }
        let stored = if macos {
            let account = variable(env, "USER").ok_or("无法确定 Claude Code keychain 账户")?;
            keychain(&claude_service(env), account).map_err(|_| "无法读取 Claude Code keychain，请在 CLI 中检查登录")?
        } else { None };
        match stored {
            Some(bytes) => bytes,
            None => read_file(&root.join(".credentials.json"))?.ok_or(CLAUDE_LOGIN)?,
        }
    };
    let auth = object(&bytes)?;
    if source == "codex" { return codex_token(&auth, now); }
    let oauth = &auth["claudeAiOauth"];
    let expires = oauth["expiresAt"].as_u64().ok_or(CLAUDE_LOGIN)?;
    if expires <= now.saturating_add(CLOCK_SKEW_MS) { return Err(CLAUDE_LOGIN.into()); }
    if let Some(scopes) = oauth.get("scopes") {
        if !scopes.as_array().is_some_and(|s| s.iter().any(|v| v == "user:inference")) {
            return Err("Claude Code 登录缺少 user:inference 权限，请在 CLI 中重新登录".into());
        }
    }
    claude_token(text(&oauth["accessToken"]).ok_or(CLAUDE_LOGIN)?)
}

#[cfg(all(target_os = "macos", not(test)))]
fn read_keychain(service: &str, account: &str) -> Result<Option<Vec<u8>>, String> {
    use std::{process::{Command, Stdio}, time::Duration};
    use wait_timeout::ChildExt;
    let mut child = Command::new("/usr/bin/security")
        .args(["find-generic-password", "-s", service, "-a", account, "-w"])
        .env_clear().stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null())
        .spawn().map_err(|_| "无法读取系统 keychain")?;
    let stdout = child.stdout.take().ok_or("无法读取系统 keychain")?;
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.take(MAX_BYTES + 1).read_to_end(&mut bytes).map(|_| bytes)
    });
    let status = match child.wait_timeout(Duration::from_secs(5)) {
        Ok(Some(status)) => Some(status),
        _ => { let _ = child.kill(); let _ = child.wait(); None }
    };
    let bytes = reader.join().ok().and_then(Result::ok).ok_or("无法读取系统 keychain")?;
    let status = status.ok_or("系统 keychain 读取超时或失败")?;
    if status.code() == Some(44) { return Ok(None); } // errSecItemNotFound, not permission denied.
    if !status.success() || bytes.len() as u64 > MAX_BYTES { return Err("无法读取系统 keychain".into()); }
    Ok(Some(bytes))
}

// Test builds cannot invoke the real keychain, even on macOS; all keychain tests inject bytes.
#[cfg(any(not(target_os = "macos"), test))]
fn read_keychain(_: &str, _: &str) -> Result<Option<Vec<u8>>, String> {
    Err("此环境不支持读取系统 keychain".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    const NOW: u64 = 1_800_000_000_000;
    fn token(exp: u64, account: &str) -> String {
        format!("e30.{}.signature", URL_SAFE_NO_PAD.encode(json!({"exp":exp,
            "https://api.openai.com/auth":{"chatgpt_account_id":account}}).to_string()))
    }
    fn codex(exp: u64) -> Value {
        json!({"auth_mode":"chatgpt", "tokens":{"access_token":token(exp, "test-account"),
            "refresh_token":"never-return-refresh", "account_id":"selected-account"}})
    }
    fn claude(exp: u64) -> Value {
        json!({"claudeAiOauth":{"accessToken":"sk-ant-oat-test", "expiresAt":exp,
            "refreshToken":"never-return-refresh", "scopes":["user:inference"]}})
    }
    fn fixture() -> (tempfile::TempDir, HashMap<String, String>) {
        let dir = tempfile::tempdir().unwrap();
        let env = HashMap::from([("CODEX_HOME".into(), dir.path().to_string_lossy().into_owned()),
            ("CLAUDE_CONFIG_DIR".into(), dir.path().to_string_lossy().into_owned()), ("USER".into(), "fixture-user".into())]);
        (dir, env)
    }
    fn file(dir: &Path, name: &str, value: &Value) {
        std::fs::write(dir.join(name), value.to_string()).unwrap();
    }
    fn no_keychain(_: &str, _: &str) -> Result<Option<Vec<u8>>, String> { panic!("unexpected keychain access") }
    fn get(source: &str, env: &HashMap<String, String>) -> Result<Credentials, String> {
        credentials_with(source, env, NOW, false, &no_keychain)
    }

    #[test]
    fn codex_file_expiry_account_and_no_secret_headers() {
        let (dir, env) = fixture();
        let mut auth = codex(NOW / 1000 + 3600);
        file(dir.path(), "auth.json", &auth);
        let result = get("codex", &env).unwrap();
        assert_eq!(result.key, auth["tokens"]["access_token"]);
        assert_eq!(result.headers["chatgpt-account-id"], "selected-account");
        assert!(!json!(&result.headers).to_string().contains("never-return-refresh"));
        assert!(result.headers.get("Authorization").is_none());
        auth["tokens"].as_object_mut().unwrap().remove("account_id");
        assert_eq!(codex_token(&auth, NOW).unwrap().headers["chatgpt-account-id"], "test-account");
        for exp in [0, NOW / 1000, NOW / 1000 + 60] {
            file(dir.path(), "auth.json", &codex(exp));
            assert!(get("codex", &env).err().unwrap().contains("codex login"));
        }
        auth["auth_mode"] = json!("apikey");
        assert!(codex_token(&auth, NOW).is_err());
    }

    #[test]
    fn claude_settings_override_shell_empty_clears_and_files_expire_in_milliseconds() {
        let (dir, mut env) = fixture();
        env.insert("CLAUDE_CODE_OAUTH_TOKEN".into(), "sk-ant-oat-shell".into());
        std::fs::write(dir.path().join("settings.json"), "{ // 用户配置\n\"env\":{\"CLAUDE_CODE_OAUTH_TOKEN\":\"sk-ant-oat-settings\",},}").unwrap();
        assert_eq!(get("claude-code", &env).unwrap().key, "sk-ant-oat-settings");
        file(dir.path(), "settings.json", &json!({"env":{"CLAUDE_CODE_OAUTH_TOKEN":""}}));
        file(dir.path(), ".credentials.json", &claude(NOW + 3600_000));
        assert_eq!(get("claude-code", &env).unwrap().key, "sk-ant-oat-test");
        for exp in [NOW / 1000 + 3600, NOW, NOW + CLOCK_SKEW_MS] {
            file(dir.path(), ".credentials.json", &claude(exp));
            assert!(get("claude-code", &env).err().unwrap().contains("CLI 更新登录"));
        }
        file(dir.path(), "settings.json", &json!({"apiKeyHelper":"do-not-run-secret-command"}));
        let error = get("claude-code", &env).err().unwrap();
        assert!(error.contains("apiKeyHelper") && !error.contains("do-not-run-secret-command"));
    }

    #[test]
    fn keychain_precedence_names_and_fail_closed_are_mocked() {
        let (dir, mut env) = fixture();
        file(dir.path(), ".credentials.json", &claude(NOW + 3600_000));
        let mut value = claude(NOW + 3600_000);
        value["claudeAiOauth"]["accessToken"] = json!("sk-ant-oat-keychain");
        let mock = |service: &str, account: &str| {
            assert_eq!(service, claude_service(&env));
            assert_eq!(account, "fixture-user");
            Ok(Some(value.to_string().into_bytes()))
        };
        assert_eq!(credentials_with("claude-code", &env, NOW, true, &mock).unwrap().key, "sk-ant-oat-keychain");
        assert_eq!(credentials_with("claude-code", &env, NOW, true, &|_, _| Ok(None)).unwrap().key, "sk-ant-oat-test");
        assert!(!credentials_with("claude-code", &env, NOW, true, &|_, _| Err("secret-stderr".into()))
            .err().unwrap().contains("secret-stderr"));
        env.insert("CLAUDE_CODE_OAUTH_TOKEN".into(), "sk-ant-oat-env".into());
        assert_eq!(credentials_with("claude-code", &env, NOW, true, &no_keychain).unwrap().key, "sk-ant-oat-env");
        file(dir.path(), "auth.json", &codex(NOW / 1000 + 3600));
        std::fs::write(dir.path().join("config.toml"), "cli_auth_credentials_store='auto'").unwrap();
        let mock = |service: &str, account: &str| {
            assert_eq!(service, "Codex Auth");
            assert_eq!(account, codex_account(dir.path()));
            Ok(None)
        };
        assert!(credentials_with("codex", &env, NOW, true, &mock).is_ok());
        assert!(credentials_with("codex", &env, NOW, true, &|_, _| Err("secret-stderr".into())).is_err());
        assert!(get("codex", &env).err().unwrap().contains("keyring"));
        std::fs::write(dir.path().join("config.toml"), "cli_auth_credentials_store='keyring'").unwrap();
        assert!(credentials_with("codex", &env, NOW, true, &mock).is_err());
    }

    #[test]
    fn malformed_expiry_scope_and_header_injection_errors_are_redacted() {
        let (dir, env) = fixture();
        for raw in ["{\"accessToken\":\"secret-sentinel\", broken}", "[\"secret-sentinel\"]"] {
            std::fs::write(dir.path().join("auth.json"), raw).unwrap();
            assert!(!get("codex", &env).err().unwrap().contains("secret-sentinel"));
        }
        let mut auth = codex(NOW / 1000 + 3600);
        auth["tokens"]["account_id"] = json!("secret-sentinel\r\nx-injected: yes");
        assert!(!codex_token(&auth, NOW).err().unwrap().contains("secret-sentinel"));
        auth["tokens"]["access_token"] = json!("secret-sentinel");
        assert!(!codex_token(&auth, NOW).err().unwrap().contains("secret-sentinel"));
        let mut auth = claude(NOW + 3600_000);
        auth["claudeAiOauth"]["scopes"] = json!(["user:profile"]);
        file(dir.path(), ".credentials.json", &auth);
        assert!(get("claude-code", &env).err().unwrap().contains("user:inference"));
        auth["claudeAiOauth"].as_object_mut().unwrap().remove("expiresAt");
        file(dir.path(), ".credentials.json", &auth);
        assert!(get("claude-code", &env).is_err());
        assert!(claude_token("sk-ant-oat-secret\nheader").is_err());
        // stream_anthropic 固定加 anthropic-version；凭据头再带一份会被 reqwest 追加成重复头。
        assert!(claude_token("sk-ant-oat-secret").unwrap().headers.get("anthropic-version").is_none());
        assert!(object(&vec![b' '; MAX_BYTES as usize + 1]).is_err());
    }

    #[test]
    fn credentials_are_reread_without_mutation_and_expired_keychain_never_falls_back() {
        let (dir, env) = fixture();
        let auth = codex(NOW / 1000 + 3600);
        file(dir.path(), "auth.json", &auth);
        let before = std::fs::read(dir.path().join("auth.json")).unwrap();
        assert!(get("codex", &env).is_ok());
        assert_eq!(std::fs::read(dir.path().join("auth.json")).unwrap(), before);
        file(dir.path(), "auth.json", &codex(0));
        assert!(get("codex", &env).is_err());
        file(dir.path(), ".credentials.json", &claude(NOW + 3600_000));
        let expired = |_: &str, _: &str| Ok(Some(claude(0).to_string().into_bytes()));
        assert!(credentials_with("claude-code", &env, NOW, true, &expired).is_err());
        std::fs::write(dir.path().join("config.toml"), "cli_auth_credentials_store='auto'\n[features]\nsecret_auth_storage=true").unwrap();
        assert!(credentials_with("codex", &env, NOW, true, &no_keychain).err().unwrap().contains("后端"));
    }

    #[test]
    fn pinned_endpoints_and_default_keychain_service() {
        assert_eq!(endpoint("codex").unwrap(), CODEX_BASE_URL);
        assert_eq!(endpoint("claude-code").unwrap(), CLAUDE_BASE_URL);
        assert!(!endpoint("https://secret-sentinel.test").err().unwrap().contains("secret-sentinel"));
        assert_eq!(claude_service(&HashMap::new()), "Claude Code-credentials");
        assert_eq!(claude_service(&HashMap::from([("CLAUDE_CONFIG_DIR".into(), "abc".into())])),
            "Claude Code-credentials-ba7816bf");
        assert!(read_keychain("must-not-execute", "test").is_err());
        // This public call cannot discover HOME or the real user's stores from the process.
        assert!(credentials("codex", &HashMap::new()).is_err());
    }
}
