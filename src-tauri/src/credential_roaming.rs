//! 额度租借：采集登录凭证、一次性端到端加密传输，并在借用方创建隔离运行目录。
//!
//! 明文凭证不会写入 threads/relay 日志；只在借用会话存活期间落到 Nova 专属临时目录，
//! 后端进程通过 CODEX_HOME / CURSOR_CONFIG_DIR / XDG_* 等环境变量读取，不覆盖本机账号。

use crate::acp::AcpManager;
use crate::opencode_sdk::OpenCodeSdkManager;
use crate::sdk_adapters::{ClaudeAdapter, CodeBuddyAdapter, CodexAdapter, CursorAdapter};
use crate::sdk_runtime::SdkManager;
use crate::threads::AgentKind;
use crate::AppState;
use base64::Engine;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use hkdf::Hkdf;
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use tauri::{AppHandle, Manager};
use tokio::process::Command;
use x25519_dalek::{PublicKey, StaticSecret};

const MAX_BUNDLE_BYTES: usize = 8 * 1024 * 1024;
const MAX_BUNDLE_FILES: usize = 256;

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialFile {
    pub path: String,
    pub data: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialBundle {
    pub version: u8,
    pub agent_kind: AgentKind,
    pub files: Vec<CredentialFile>,
    #[serde(default)]
    pub env: HashMap<String, String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EncryptedGrant {
    pub public_key: String,
    pub nonce: String,
    pub ciphertext: String,
}

#[derive(Clone)]
pub enum BorrowedManager {
    Acp(Arc<AcpManager>),
    Sdk(Arc<SdkManager>),
    OpenCode(Arc<OpenCodeSdkManager>),
}

#[derive(Clone)]
pub struct BorrowedRuntime {
    pub manager: BorrowedManager,
    root: PathBuf,
}

impl BorrowedRuntime {
    pub fn is_running(&self, thread_id: &str) -> bool {
        match &self.manager {
            BorrowedManager::Acp(manager) => manager.is_running(thread_id),
            BorrowedManager::Sdk(manager) => manager.is_running(thread_id),
            BorrowedManager::OpenCode(manager) => manager.is_running(thread_id),
        }
    }

    pub fn has_pending_permission(&self, request_key: &str) -> bool {
        match &self.manager {
            BorrowedManager::Acp(manager) => manager.has_pending_permission(request_key),
            BorrowedManager::Sdk(manager) => manager.has_pending_permission(request_key),
            BorrowedManager::OpenCode(manager) => manager.has_pending_permission(request_key),
        }
    }

    pub async fn respond_permission(
        &self,
        request_key: &str,
        option_id: &str,
    ) -> Result<(), String> {
        match &self.manager {
            BorrowedManager::Acp(manager) => {
                manager.respond_permission(request_key, option_id).await
            }
            BorrowedManager::Sdk(manager) => {
                manager.respond_permission(request_key, option_id).await
            }
            BorrowedManager::OpenCode(manager) => {
                manager.respond_permission(request_key, option_id).await
            }
        }
    }

    pub async fn shutdown(self) {
        match &self.manager {
            BorrowedManager::Acp(manager) => manager.kill_conn().await,
            BorrowedManager::Sdk(manager) => manager.shutdown(),
            BorrowedManager::OpenCode(manager) => manager.shutdown(),
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

pub fn new_request_key() -> (StaticSecret, String) {
    let secret = StaticSecret::random_from_rng(OsRng);
    let public = PublicKey::from(&secret);
    (
        secret,
        base64::engine::general_purpose::STANDARD.encode(public.as_bytes()),
    )
}

pub fn encrypt_bundle(
    peer_public_key: &str,
    request_id: &str,
    bundle: &CredentialBundle,
) -> Result<EncryptedGrant, String> {
    let peer = decode_public_key(peer_public_key)?;
    let secret = StaticSecret::random_from_rng(OsRng);
    let public = PublicKey::from(&secret);
    let key = derive_key(&secret, &peer, request_id)?;
    let cipher = ChaCha20Poly1305::new((&key).into());
    let mut nonce = [0u8; 12];
    OsRng.fill_bytes(&mut nonce);
    let plain = serde_json::to_vec(bundle).map_err(|e| format!("凭证序列化失败：{e}"))?;
    let ciphertext = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: &plain,
                aad: request_id.as_bytes(),
            },
        )
        .map_err(|_| "凭证加密失败".to_string())?;
    Ok(EncryptedGrant {
        public_key: base64::engine::general_purpose::STANDARD.encode(public.as_bytes()),
        nonce: base64::engine::general_purpose::STANDARD.encode(nonce),
        ciphertext: base64::engine::general_purpose::STANDARD.encode(ciphertext),
    })
}

pub fn decrypt_bundle(
    secret: StaticSecret,
    request_id: &str,
    grant: &EncryptedGrant,
) -> Result<CredentialBundle, String> {
    let peer = decode_public_key(&grant.public_key)?;
    let nonce = base64::engine::general_purpose::STANDARD
        .decode(grant.nonce.as_bytes())
        .map_err(|_| "凭证 nonce 无效".to_string())?;
    if nonce.len() != 12 {
        return Err("凭证 nonce 长度无效".into());
    }
    let ciphertext = base64::engine::general_purpose::STANDARD
        .decode(grant.ciphertext.as_bytes())
        .map_err(|_| "凭证密文无效".to_string())?;
    let key = derive_key(&secret, &peer, request_id)?;
    let cipher = ChaCha20Poly1305::new((&key).into());
    let plain = cipher
        .decrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: &ciphertext,
                aad: request_id.as_bytes(),
            },
        )
        .map_err(|_| "凭证解密失败：密文损坏或会话密钥不匹配".to_string())?;
    let bundle: CredentialBundle =
        serde_json::from_slice(&plain).map_err(|_| "凭证包格式无效".to_string())?;
    if bundle.version != 1
        || (bundle.files.is_empty() && bundle.env.is_empty())
        || bundle.files.len() > MAX_BUNDLE_FILES
    {
        return Err("凭证包版本或文件数量无效".into());
    }
    Ok(bundle)
}

fn decode_public_key(encoded: &str) -> Result<PublicKey, String> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded.as_bytes())
        .map_err(|_| "设备公钥无效".to_string())?;
    let bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|_| "设备公钥长度无效".to_string())?;
    Ok(PublicKey::from(bytes))
}

fn derive_key(
    secret: &StaticSecret,
    peer: &PublicKey,
    request_id: &str,
) -> Result<[u8; 32], String> {
    let shared = secret.diffie_hellman(peer);
    let hk = Hkdf::<Sha256>::new(Some(request_id.as_bytes()), shared.as_bytes());
    let mut key = [0u8; 32];
    hk.expand(b"nova-quota-credential-roaming-v1", &mut key)
        .map_err(|_| "会话密钥派生失败".to_string())?;
    Ok(key)
}

pub fn collect_credentials(
    app: &AppHandle,
    agent_kind: AgentKind,
    model: &str,
) -> Result<CredentialBundle, String> {
    let mut files = Vec::new();
    let mut env = HashMap::new();
    match &agent_kind {
        AgentKind::Alkaid => return Err("Vega 暂不支持额度凭据共享".into()),
        AgentKind::Devin => collect_file(
            &devin_credentials_path()?,
            "appdata/devin/credentials.toml",
            &mut files,
        )?,
        AgentKind::Codex | AgentKind::CodexPlus => {
            collect_file(
                &configured_home("CODEX_HOME", ".codex").join("auth.json"),
                "codex-home/auth.json",
                &mut files,
            )?;
        }
        AgentKind::CodeBuddy | AgentKind::CodeBuddyPlus => {
            let root = configured_home("CODEBUDDY_CONFIG_DIR", ".codebuddy");
            collect_directory(
                &root.join("local_storage"),
                "profile/.codebuddy/local_storage",
                &mut files,
            )?;
            collect_optional_file(
                &root.join("instances.json"),
                "profile/.codebuddy/instances.json",
                &mut files,
            )?;
        }
        AgentKind::ClaudeCode => {
            let configured = app
                .state::<AppState>()
                .settings
                .lock()
                .unwrap()
                .claudecode_sdk_api_key
                .clone();
            collect_secret_env("ANTHROPIC_API_KEY", &configured, &mut env);
            collect_secret_env("CLAUDE_CODE_OAUTH_TOKEN", "", &mut env);
        }
        AgentKind::Cursor => {
            let configured = app
                .state::<AppState>()
                .settings
                .lock()
                .unwrap()
                .cursor_sdk_api_key
                .clone();
            collect_secret_env("CURSOR_API_KEY", &configured, &mut env);
        }
        AgentKind::OpenCode | AgentKind::OpenCodePlus => {
            let data = configured_home("XDG_DATA_HOME", ".local/share");
            let provider = model
                .split('/')
                .next()
                .filter(|value| !value.is_empty())
                .ok_or("OpenCode 共享模型缺少 Provider 标识")?;
            collect_json_entry(
                &data.join("opencode").join("auth.json"),
                "opencode-data/opencode/auth.json",
                provider,
                &mut files,
            )?;
        }
    }
    if files.is_empty() && env.is_empty() {
        return Err(format!(
            "未找到 {} 可安全共享的登录凭证，请先配置该后端的 API Key 或完成本地登录",
            agent_kind.label()
        ));
    }
    let total = files
        .iter()
        .filter_map(|file: &CredentialFile| {
            base64::engine::general_purpose::STANDARD
                .decode(file.data.as_bytes())
                .ok()
                .map(|data| data.len())
        })
        .sum::<usize>()
        + env.values().map(String::len).sum::<usize>();
    if total > MAX_BUNDLE_BYTES {
        return Err("凭证包过大，已拒绝发送".into());
    }
    Ok(CredentialBundle {
        version: 1,
        agent_kind,
        files,
        env,
    })
}

pub fn materialize_runtime(
    app: AppHandle,
    thread_id: &str,
    expected_kind: &AgentKind,
    bundle: CredentialBundle,
) -> Result<BorrowedRuntime, String> {
    if &bundle.agent_kind != expected_kind {
        return Err("对方返回的凭证后端与请求不一致".into());
    }
    let base = std::env::temp_dir()
        .join("Nova-borrowed-credentials")
        .join(std::process::id().to_string());
    let root = base.join(thread_id);
    if root.exists() {
        std::fs::remove_dir_all(&root).map_err(|e| format!("清理旧租借目录失败：{e}"))?;
    }
    std::fs::create_dir_all(&root).map_err(|e| format!("创建租借目录失败：{e}"))?;
    restrict_dir(&root);

    let CredentialBundle { files, env, .. } = bundle;
    let mut total = 0usize;
    for file in files {
        if !credential_path_allowed(expected_kind, &file.path) {
            let _ = std::fs::remove_dir_all(&root);
            return Err(format!(
                "{} 凭证文件路径不在允许范围内",
                expected_kind.label()
            ));
        }
        let relative = safe_relative_path(&file.path)?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(file.data.as_bytes())
            .map_err(|_| "凭证文件编码无效".to_string())?;
        total = total.saturating_add(bytes.len());
        if total > MAX_BUNDLE_BYTES {
            let _ = std::fs::remove_dir_all(&root);
            return Err("凭证包过大，已拒绝落盘".into());
        }
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("创建凭证目录失败：{e}"))?;
            restrict_dir(parent);
        }
        std::fs::write(&path, bytes).map_err(|e| format!("写入隔离凭证失败：{e}"))?;
        restrict_file(&path);
    }

    let mut launch_env = launch_env(expected_kind, &root)?;
    for (name, value) in env {
        if !credential_env_allowed(expected_kind, &name) {
            let _ = std::fs::remove_dir_all(&root);
            return Err(format!(
                "{} 凭证环境变量不在允许范围内",
                expected_kind.label()
            ));
        }
        launch_env.insert(name, value);
    }
    crate::agent_config::sync_backend_with_env(
        &crate::nova_data_dir(&app),
        expected_kind,
        &launch_env,
    )?;
    stage_local_skills(&app, expected_kind, &launch_env)?;
    let manager = match expected_kind {
        AgentKind::Alkaid => return Err("Vega 暂不支持额度凭据共享".into()),
        AgentKind::Devin => BorrowedManager::Acp(AcpManager::new_with_env(
            app,
            AgentKind::Devin,
            launch_env,
            format!("quota-{thread_id}-"),
        )),
        AgentKind::Codex | AgentKind::CodexPlus => {
            BorrowedManager::Sdk(SdkManager::new_with_env(app, CodexAdapter, launch_env))
        }
        AgentKind::CodeBuddy | AgentKind::CodeBuddyPlus => {
            BorrowedManager::Sdk(SdkManager::new_with_env(app, CodeBuddyAdapter, launch_env))
        }
        AgentKind::ClaudeCode => {
            BorrowedManager::Sdk(SdkManager::new_with_env(app, ClaudeAdapter, launch_env))
        }
        AgentKind::Cursor => {
            BorrowedManager::Sdk(SdkManager::new_with_env(app, CursorAdapter, launch_env))
        }
        AgentKind::OpenCode | AgentKind::OpenCodePlus => {
            BorrowedManager::OpenCode(OpenCodeSdkManager::new_with_env(app, launch_env))
        }
    };
    Ok(BorrowedRuntime { manager, root })
}

fn launch_env(kind: &AgentKind, root: &Path) -> Result<HashMap<String, String>, String> {
    let mut env = HashMap::new();
    let as_string = |path: PathBuf| path.to_string_lossy().to_string();
    match kind {
        AgentKind::Alkaid => {}
        AgentKind::Devin => {
            #[cfg(windows)]
            {
                // Devin 通过 USERPROFILE 解析凭据目录，仅覆盖 APPDATA/LOCALAPPDATA 不会生效。
                let profile = root.join("profile");
                let appdata = profile.join("AppData").join("Roaming");
                let local = profile.join("AppData").join("Local");
                let staged = root.join("appdata").join("devin").join("credentials.toml");
                let credentials_dir = appdata.join("devin");
                let credentials = credentials_dir.join("credentials.toml");
                std::fs::create_dir_all(&credentials_dir).map_err(|e| e.to_string())?;
                std::fs::create_dir_all(&local).map_err(|e| e.to_string())?;
                std::fs::rename(&staged, &credentials)
                    .map_err(|e| format!("准备 Devin 隔离凭据失败：{e}"))?;
                env.insert("USERPROFILE".into(), as_string(profile));
                env.insert("APPDATA".into(), as_string(appdata));
                env.insert("LOCALAPPDATA".into(), as_string(local));
            }
            #[cfg(not(windows))]
            {
                let appdata = root.join("appdata");
                let local = root.join("localappdata");
                std::fs::create_dir_all(&local).map_err(|e| e.to_string())?;
                env.insert("APPDATA".into(), as_string(appdata));
                env.insert("LOCALAPPDATA".into(), as_string(local));
            }
        }
        AgentKind::Codex | AgentKind::CodexPlus => {
            env.insert("CODEX_HOME".into(), as_string(root.join("codex-home")));
        }
        AgentKind::CodeBuddy | AgentKind::CodeBuddyPlus => {
            let profile = root.join("profile");
            let config = profile.join(".codebuddy");
            std::fs::create_dir_all(&config).map_err(|e| e.to_string())?;
            env.insert("USERPROFILE".into(), as_string(profile.clone()));
            env.insert("HOME".into(), as_string(profile));
            env.insert("CODEBUDDY_CONFIG_DIR".into(), as_string(config));
        }
        AgentKind::ClaudeCode => {
            let config = root.join("claude");
            std::fs::create_dir_all(&config).map_err(|e| e.to_string())?;
            env.insert("CLAUDE_CONFIG_DIR".into(), as_string(config.clone()));
            env.insert("CLAUDE_SECURESTORAGE_CONFIG_DIR".into(), as_string(config));
        }
        AgentKind::Cursor => {
            let config = root.join("cursor");
            std::fs::create_dir_all(&config).map_err(|e| e.to_string())?;
            env.insert("CURSOR_CONFIG_DIR".into(), as_string(config.clone()));
            env.insert("CURSOR_DATA_DIR".into(), as_string(config));
        }
        AgentKind::OpenCode | AgentKind::OpenCodePlus => {
            let profile = root.join("profile");
            let config = root.join("opencode-config");
            let data = root.join("opencode-data");
            let cache = root.join("opencode-cache");
            for dir in [&profile, &config, &data, &cache] {
                std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
            }
            env.insert("USERPROFILE".into(), as_string(profile.clone()));
            env.insert("HOME".into(), as_string(profile));
            env.insert("XDG_CONFIG_HOME".into(), as_string(config));
            env.insert("XDG_DATA_HOME".into(), as_string(data));
            env.insert("XDG_CACHE_HOME".into(), as_string(cache));
        }
    }
    env.insert("NOVA_QUOTA_BORROWED".into(), "1".into());
    Ok(env)
}

fn stage_local_skills(
    app: &AppHandle,
    kind: &AgentKind,
    env: &HashMap<String, String>,
) -> Result<(), String> {
    let root = match kind {
        AgentKind::Alkaid => return Ok(()),
        AgentKind::Devin => return Ok(()),
        AgentKind::Codex | AgentKind::CodexPlus => env
            .get("CODEX_HOME")
            .map(PathBuf::from)
            .map(|path| path.join("skills")),
        AgentKind::CodeBuddy | AgentKind::CodeBuddyPlus => env
            .get("CODEBUDDY_CONFIG_DIR")
            .map(PathBuf::from)
            .map(|path| path.join("skills")),
        AgentKind::ClaudeCode => env
            .get("CLAUDE_CONFIG_DIR")
            .map(PathBuf::from)
            .map(|path| path.join("skills")),
        AgentKind::Cursor => env
            .get("CURSOR_CONFIG_DIR")
            .map(PathBuf::from)
            .map(|path| path.join("skills")),
        AgentKind::OpenCode | AgentKind::OpenCodePlus => env
            .get("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .map(|path| path.join("opencode").join("skills")),
    };
    if let Some(root) = root {
        crate::skills::copy_skills_to_runtime(&crate::nova_data_dir(app), &root)?;
    }
    Ok(())
}

pub fn isolate_borrowed_command(command: &mut Command) {
    for name in [
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "CLAUDE_CODE_OAUTH_TOKEN",
        "CURSOR_API_KEY",
        "DEEPSEEK_API_KEY",
        "GEMINI_API_KEY",
        "GOOGLE_API_KEY",
        "GOOGLE_APPLICATION_CREDENTIALS",
        "GROQ_API_KEY",
        "MISTRAL_API_KEY",
        "OPENAI_API_KEY",
        "OPENROUTER_API_KEY",
        "XAI_API_KEY",
        "AWS_ACCESS_KEY_ID",
        "AWS_SECRET_ACCESS_KEY",
        "AWS_SESSION_TOKEN",
        "AZURE_OPENAI_API_KEY",
    ] {
        command.env_remove(name);
    }
}

fn credential_path_allowed(kind: &AgentKind, raw: &str) -> bool {
    let path = raw.replace('\\', "/");
    match kind {
        AgentKind::Alkaid => false,
        AgentKind::Devin => path == "appdata/devin/credentials.toml",
        AgentKind::Codex | AgentKind::CodexPlus => path == "codex-home/auth.json",
        AgentKind::CodeBuddy | AgentKind::CodeBuddyPlus => {
            path == "profile/.codebuddy/instances.json"
                || path.starts_with("profile/.codebuddy/local_storage/")
        }
        AgentKind::ClaudeCode | AgentKind::Cursor => false,
        AgentKind::OpenCode | AgentKind::OpenCodePlus => path == "opencode-data/opencode/auth.json",
    }
}

fn credential_env_allowed(kind: &AgentKind, name: &str) -> bool {
    match kind {
        AgentKind::ClaudeCode => matches!(
            name,
            "ANTHROPIC_API_KEY" | "ANTHROPIC_AUTH_TOKEN" | "CLAUDE_CODE_OAUTH_TOKEN"
        ),
        AgentKind::Cursor => name == "CURSOR_API_KEY",
        _ => false,
    }
}

fn home_dir() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

fn configured_home(env: &str, fallback: &str) -> PathBuf {
    std::env::var_os(env)
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(fallback))
}

fn devin_credentials_path() -> Result<PathBuf, String> {
    #[cfg(windows)]
    {
        let appdata = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home_dir().join("AppData").join("Roaming"));
        Ok(appdata.join("devin").join("credentials.toml"))
    }
    #[cfg(not(windows))]
    {
        let data = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home_dir().join(".local").join("share"));
        Ok(data.join("devin").join("credentials.toml"))
    }
}

fn collect_file(path: &Path, target: &str, files: &mut Vec<CredentialFile>) -> Result<(), String> {
    let data = std::fs::read(path).map_err(|_| {
        format!(
            "未找到 {} 登录凭证，请先在额度提供方完成登录",
            path.display()
        )
    })?;
    if data.is_empty() {
        return Err(format!("登录凭证为空：{}", path.display()));
    }
    files.push(CredentialFile {
        path: target.into(),
        data: base64::engine::general_purpose::STANDARD.encode(data),
    });
    Ok(())
}

fn collect_optional_file(
    path: &Path,
    target: &str,
    files: &mut Vec<CredentialFile>,
) -> Result<(), String> {
    if path.is_file() {
        collect_file(path, target, files)?;
    }
    Ok(())
}

fn collect_json_entry(
    path: &Path,
    target: &str,
    key: &str,
    files: &mut Vec<CredentialFile>,
) -> Result<(), String> {
    let data = std::fs::read(path).map_err(|_| {
        format!(
            "未找到 {} 登录凭证，请先在额度提供方完成登录",
            path.display()
        )
    })?;
    let value: serde_json::Value = serde_json::from_slice(&data)
        .map_err(|_| format!("登录凭证格式无效：{}", path.display()))?;
    let credential = value
        .get(key)
        .cloned()
        .ok_or_else(|| format!("OpenCode Provider {key} 尚未登录，无法共享该模型额度"))?;
    let mut filtered = serde_json::Map::new();
    filtered.insert(key.to_string(), credential);
    let filtered = serde_json::to_vec(&filtered).map_err(|e| format!("凭证序列化失败：{e}"))?;
    files.push(CredentialFile {
        path: target.into(),
        data: base64::engine::general_purpose::STANDARD.encode(filtered),
    });
    Ok(())
}

fn collect_directory(
    source: &Path,
    target: &str,
    files: &mut Vec<CredentialFile>,
) -> Result<(), String> {
    if !source.is_dir() {
        return Err(format!(
            "未找到 {} 登录凭证，请先在额度提供方完成登录",
            source.display()
        ));
    }
    let initial_file_count = files.len();
    let mut pending = vec![source.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let file_type = entry.file_type().map_err(|e| e.to_string())?;
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                pending.push(entry.path());
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            let relative = entry
                .path()
                .strip_prefix(source)
                .map_err(|_| "凭证目录结构无效".to_string())?
                .to_string_lossy()
                .replace('\\', "/");
            collect_file(&entry.path(), &format!("{target}/{relative}"), files)?;
            if files.len() > MAX_BUNDLE_FILES {
                return Err("凭证文件数量过多，已拒绝发送".into());
            }
        }
    }
    if files.len() == initial_file_count {
        return Err(format!("登录凭证目录为空：{}", source.display()));
    }
    Ok(())
}

fn collect_secret_env(name: &str, configured: &str, env: &mut HashMap<String, String>) {
    let value = if configured.trim().is_empty() {
        std::env::var(name).unwrap_or_default()
    } else {
        configured.trim().to_string()
    };
    if !value.is_empty() {
        env.insert(name.to_string(), value);
    }
}

fn safe_relative_path(raw: &str) -> Result<PathBuf, String> {
    let path = Path::new(raw);
    if path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err("凭证文件路径无效".into());
    }
    Ok(path.to_path_buf())
}

#[cfg(unix)]
fn restrict_dir(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
}

#[cfg(not(unix))]
fn restrict_dir(_path: &Path) {}

#[cfg(unix)]
fn restrict_file(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn restrict_file(_path: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypted_bundle_round_trip() {
        let (secret, public_key) = new_request_key();
        let bundle = CredentialBundle {
            version: 1,
            agent_kind: AgentKind::Codex,
            files: vec![CredentialFile {
                path: "codex-home/auth.json".into(),
                data: base64::engine::general_purpose::STANDARD.encode(b"secret"),
            }],
            env: HashMap::from([("OPENAI_API_KEY".into(), "test-key".into())]),
        };
        let grant = encrypt_bundle(&public_key, "request-1", &bundle).unwrap();
        let decoded = decrypt_bundle(secret, "request-1", &grant).unwrap();
        assert_eq!(decoded.agent_kind, AgentKind::Codex);
        assert_eq!(decoded.files[0].path, "codex-home/auth.json");
        assert_eq!(
            decoded.env.get("OPENAI_API_KEY").map(String::as_str),
            Some("test-key")
        );
    }

    #[test]
    fn rejects_credential_path_escape() {
        assert!(safe_relative_path("../auth.json").is_err());
        assert!(safe_relative_path("/tmp/auth.json").is_err());
        assert!(safe_relative_path("codex-home/auth.json").is_ok());
        assert!(credential_path_allowed(
            &AgentKind::Codex,
            "codex-home/auth.json"
        ));
        assert!(!credential_path_allowed(
            &AgentKind::Codex,
            "codex-home/config.toml"
        ));
        assert!(credential_env_allowed(
            &AgentKind::ClaudeCode,
            "ANTHROPIC_API_KEY"
        ));
        assert!(!credential_env_allowed(
            &AgentKind::ClaudeCode,
            "NODE_OPTIONS"
        ));
    }

    #[test]
    fn opencode_credentials_only_include_requested_provider() {
        let root = std::env::temp_dir().join(format!(
            "nova-opencode-credential-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let auth = root.join("auth.json");
        std::fs::write(
            &auth,
            br#"{"anthropic":{"token":"a"},"openai":{"token":"b"}}"#,
        )
        .unwrap();

        let mut files = Vec::new();
        collect_json_entry(&auth, "opencode/auth.json", "openai", &mut files).unwrap();
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(files[0].data.as_bytes())
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&decoded).unwrap();
        assert!(value.get("openai").is_some());
        assert!(value.get("anthropic").is_none());

        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    #[test]
    fn cursor_launch_env_uses_isolated_sdk_data_dir() {
        let root = std::env::temp_dir().join("nova-cursor-launch-env-test");
        let env = launch_env(&AgentKind::Cursor, &root).unwrap();
        let cursor = root.join("cursor").to_string_lossy().to_string();
        assert_eq!(env.get("CURSOR_CONFIG_DIR"), Some(&cursor));
        assert_eq!(env.get("CURSOR_DATA_DIR"), Some(&cursor));
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    #[test]
    fn devin_launch_env_uses_isolated_user_profile() {
        let root = std::env::temp_dir().join(format!(
            "nova-devin-launch-env-test-{}",
            uuid::Uuid::new_v4()
        ));
        let staged = root.join("appdata").join("devin").join("credentials.toml");
        std::fs::create_dir_all(staged.parent().unwrap()).unwrap();
        std::fs::write(&staged, b"secret").unwrap();

        let env = launch_env(&AgentKind::Devin, &root).unwrap();
        let profile = root.join("profile");
        let appdata = profile.join("AppData").join("Roaming");
        let local = profile.join("AppData").join("Local");
        let credentials = appdata.join("devin").join("credentials.toml");

        assert_eq!(
            env.get("USERPROFILE"),
            Some(&profile.to_string_lossy().to_string())
        );
        assert_eq!(
            env.get("APPDATA"),
            Some(&appdata.to_string_lossy().to_string())
        );
        assert_eq!(
            env.get("LOCALAPPDATA"),
            Some(&local.to_string_lossy().to_string())
        );
        assert_eq!(std::fs::read(credentials).unwrap(), b"secret");
        assert!(!staged.exists());

        std::fs::remove_dir_all(root).unwrap();
    }
}
