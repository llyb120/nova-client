//! Antigravity CLI（`agy`）多账号凭据管理与一键切换。
//!
//! 工作机制：
//! - `agy` 将当前登录凭据保存在系统原生凭据库中：
//!   - Windows：Credential Manager（`CRED_TYPE_GENERIC`，目标名 `gemini:antigravity`）
//!   - macOS：Keychain（service=`gemini`, account=`antigravity`）
//!   - Linux：Secret Service（`secret-tool`，service=`gemini`, username=`antigravity`）
//! - Nova 将每个已保存账号的原始凭据加密存放于系统凭据库（`nova:antigravity-account:<id>`，
//!   Linux 无 Secret Service 时回退 AES-256-GCM 加密文件），仅在 `~/.nova/antigravity-accounts.json`
//!   保存非敏感元数据（邮箱、别名、订阅档位、最近使用时间）。
//! - 切号或新增登录前自动回写当前活跃账号的最新凭据，保留 `agy` 刷新后的 OAuth 令牌；
//!   新增登录被取消或失败时自动回滚旧凭据，避免用户掉线。

use base64::{
    engine::general_purpose::{STANDARD as B64_STANDARD, URL_SAFE_NO_PAD},
    Engine,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use uuid::Uuid;

const ACCOUNTS_FILE_NAME: &str = "antigravity-accounts.json";
const ACTIVE_TARGET_WINDOWS: &str = "gemini:antigravity";
const ACTIVE_USER_WINDOWS: &str = "antigravity";
const SAVED_TARGET_PREFIX_WINDOWS: &str = "nova:antigravity-account:";

#[cfg(any(target_os = "macos", target_os = "linux"))]
const ACTIVE_SERVICE: &str = "gemini";
#[cfg(any(target_os = "macos", target_os = "linux"))]
const ACTIVE_ACCOUNT: &str = "antigravity";
#[cfg(any(target_os = "macos", target_os = "linux"))]
const SAVED_SERVICE: &str = "nova-antigravity-account";

const MAX_LABEL_CHARS: usize = 48;
const TERMINAL_TOKEN_FILE: &str = "antigravity-oauth-token";
const EXPORT_BUNDLE_FORMAT: &str = "nova-antigravity-accounts";
const EXPORT_BUNDLE_VERSION: u32 = 1;
const MAX_IMPORT_FILE_BYTES: usize = 4 * 1024 * 1024;
const MAX_EXPORT_ACCOUNTS: usize = 200;
const MAX_CREDENTIAL_BLOB_BYTES: usize = 64 * 1024;

pub(crate) fn terminal_account_home(data_dir: &Path, account_id: &str) -> Result<PathBuf, String> {
    let id = Uuid::parse_str(account_id).map_err(|_| "无效的反重力账号标识")?;
    Ok(data_dir.join("terminal-agy-profiles").join(id.to_string()))
}

pub(crate) fn terminal_cli_dir(home: &Path) -> PathBuf {
    home.join(".gemini").join("antigravity-cli")
}

/// 为指定账号生成隔离的 `--app_data_dir` 目录名（如 `antigravity-acp-3f8a9c12`），
/// 使不同账号拥有独立的本地会话库、`installation_id`、缓存与日志目录。
pub fn app_data_dir_for_account(account_id: &str) -> String {
    let clean: String = account_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(8)
        .map(|c| c.to_ascii_lowercase())
        .collect();
    if clean.is_empty() {
        super::core::process::DEFAULT_APP_DATA_DIR.to_string()
    } else {
        format!("{}-{clean}", super::core::process::DEFAULT_APP_DATA_DIR)
    }
}

/// 对外暴露的单个 Antigravity 账号元数据（不含任何敏感令牌）。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AntigravityAccount {
    pub id: String,
    pub email: String,
    pub label: String,
    pub auth_method: String,
    pub app_data_dir: String,
    pub tier_name: Option<String>,
    pub tier_id: Option<String>,
    pub added_at: i64,
    pub last_used_at: i64,
}

/// 推给前端的完整账号状态快照。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AntigravityAccountsSnapshot {
    pub accounts: Vec<AntigravityAccount>,
    pub active_account_id: Option<String>,
    pub current_email: Option<String>,
    pub current_auth_method: Option<String>,
    pub credential_supported: bool,
    pub can_interactive_login: bool,
}

/// 批量导出反重力账号的结果。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AntigravityAccountsExportResult {
    pub exported_count: usize,
    pub skipped_count: usize,
    pub warnings: Vec<String>,
    pub path: String,
}

/// 批量导入反重力账号的结果。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AntigravityAccountsImportResult {
    pub snapshot: AntigravityAccountsSnapshot,
    pub imported_count: usize,
    pub updated_count: usize,
    pub skipped_count: usize,
    pub warnings: Vec<String>,
    pub active_updated: bool,
    #[serde(skip)]
    pub updated_account_ids: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
struct ExportedAccountEntry {
    #[serde(default)]
    email: String,
    #[serde(default)]
    label: String,
    #[serde(default)]
    auth_method: String,
    #[serde(default)]
    tier_name: Option<String>,
    #[serde(default)]
    tier_id: Option<String>,
    credential_b64: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
struct AccountExportBundle {
    format: String,
    version: u32,
    #[serde(default)]
    exported_at: i64,
    accounts: Vec<ExportedAccountEntry>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
struct StoredAccount {
    id: String,
    email: String,
    label: String,
    #[serde(default)]
    auth_method: String,
    #[serde(default)]
    app_data_dir: String,
    #[serde(default)]
    tier_name: Option<String>,
    #[serde(default)]
    tier_id: Option<String>,
    #[serde(default)]
    fingerprint: String,
    #[serde(default)]
    added_at: i64,
    #[serde(default)]
    last_used_at: i64,
}

impl From<&StoredAccount> for AntigravityAccount {
    fn from(s: &StoredAccount) -> Self {
        let app_data_dir = if s.app_data_dir.trim().is_empty() {
            app_data_dir_for_account(&s.id)
        } else {
            s.app_data_dir.clone()
        };
        Self {
            id: s.id.clone(),
            email: s.email.clone(),
            label: s.label.clone(),
            auth_method: s.auth_method.clone(),
            app_data_dir,
            tier_name: s.tier_name.clone(),
            tier_id: s.tier_id.clone(),
            added_at: s.added_at,
            last_used_at: s.last_used_at,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
struct AccountsFile {
    version: u32,
    active_account_id: Option<String>,
    /// 用户显式删除了当前活跃账号时记录其指纹，避免列表刷新时自动重新导入。
    /// 当用户手动同步、登录或切换账号时会清除。
    ignored_fingerprint: Option<String>,
    accounts: Vec<StoredAccount>,
}

/// 从 `agy` 凭据 JSON 提取的非敏感身份信息。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CredentialIdentity {
    pub email: Option<String>,
    pub auth_method: Option<String>,
    pub fingerprint: String,
    pub token_expiry_ms: Option<i64>,
}

/// 凭据存取抽象：生产使用系统凭据库，单测使用内存实现避免污染本机凭据。
pub trait CredentialStore: Send + Sync {
    fn is_supported(&self) -> bool;
    fn read_active(&self) -> Result<Option<Vec<u8>>, String>;
    fn write_active(&self, blob: &[u8]) -> Result<(), String>;
    fn delete_active(&self) -> Result<(), String>;
    fn read_saved(&self, id: &str) -> Result<Option<Vec<u8>>, String>;
    fn write_saved(&self, id: &str, user_hint: &str, blob: &[u8]) -> Result<(), String>;
    fn delete_saved(&self, id: &str) -> Result<(), String>;
}

/// 登录前备份的活跃账号信息，用于登录取消/失败时回滚。
pub struct LoginRollbackBackup {
    previous_blob: Option<Vec<u8>>,
    previous_active_id: Option<String>,
}

/// 从 `agy` 凭据原始字节中提取非敏感信息（邮箱、认证方式、指纹、token 过期时间）。
pub fn inspect_credential_blob(blob: &[u8]) -> CredentialIdentity {
    let mut email = None;
    let mut auth_method = None;
    let mut token_expiry_ms = None;

    if let Ok(val) = serde_json::from_slice::<Value>(blob) {
        auth_method = val
            .get("auth_method")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);

        email = val
            .get("id_token_email")
            .or_else(|| val.get("email"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .or_else(|| {
                val.pointer("/token/id_token")
                    .or_else(|| val.get("id_token"))
                    .and_then(Value::as_str)
                    .and_then(extract_email_from_jwt)
            });

        token_expiry_ms = val
            .pointer("/token/expiry")
            .or_else(|| val.get("expiry"))
            .and_then(parse_token_expiry_ms);
    }

    let mut hasher = Sha256::new();
    hasher.update(b"nova-agy-cred-v1:");
    if let Some(ref e) = email {
        hasher.update(e.to_ascii_lowercase().as_bytes());
    } else {
        hasher.update(blob);
    }
    let digest = hasher.finalize();
    let fingerprint = digest.iter().map(|b| format!("{b:02x}")).collect();

    CredentialIdentity {
        email,
        auth_method,
        fingerprint,
        token_expiry_ms,
    }
}

fn parse_token_expiry_ms(val: &Value) -> Option<i64> {
    match val {
        Value::String(s) => chrono::DateTime::parse_from_rfc3339(s.trim())
            .ok()
            .map(|dt| dt.timestamp_millis())
            .filter(|&ms| ms > 0),
        Value::Number(n) => n
            .as_i64()
            .filter(|&v| v > 0)
            .map(|v| if v < 10_000_000_000 { v.saturating_mul(1000) } else { v }),
        _ => None,
    }
}

fn extract_email_from_jwt(jwt: &str) -> Option<String> {
    let payload_b64 = jwt.split('.').nth(1)?;
    let decoded = URL_SAFE_NO_PAD.decode(payload_b64).ok()?;
    let payload: Value = serde_json::from_slice(&decoded).ok()?;
    payload
        .get("email")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn validate_terminal_credential(account: &StoredAccount, blob: &[u8]) -> Result<(), String> {
    // agy 1.5.18 在系统凭据库与文件模式中均使用 StoredToken JSON。
    if !serde_json::from_slice::<Value>(blob).ok().is_some_and(|value| {
        ["/token/refresh_token", "/token/access_token"].iter().any(|key| {
            value.pointer(key).and_then(Value::as_str).is_some_and(|token| !token.is_empty())
        })
    }) {
        return Err("所绑定的反重力账号没有可用登录凭据，请重新登录该账号".into());
    }
    let identity = inspect_credential_blob(blob);
    let matches = if account.email.is_empty() {
        identity.fingerprint == account.fingerprint
    } else {
        identity.email.is_some_and(|email| email.eq_ignore_ascii_case(&account.email))
    };
    if !matches {
        return Err("无法确认独立登录属于所绑定的反重力账号，请重新登录该账号".into());
    }
    Ok(())
}

fn default_label_for(email: &str, auth_method: &str) -> String {
    let trimmed = email.trim();
    if !trimmed.is_empty() {
        return trimmed
            .split('@')
            .next()
            .filter(|local| !local.is_empty())
            .unwrap_or(trimmed)
            .to_string();
    }
    match auth_method.trim() {
        "gemini-api-key" => "Gemini API Key".into(),
        "oauth-business" => "企业账号".into(),
        "agent-platform" => "Agent Platform".into(),
        _ => "Antigravity 账号".into(),
    }
}

fn normalize_label(label: &str) -> Result<String, String> {
    let trimmed = label.trim();
    if trimmed.is_empty() {
        return Err("账号名称不能为空".into());
    }
    if trimmed.chars().count() > MAX_LABEL_CHARS {
        return Err(format!("账号名称不能超过 {MAX_LABEL_CHARS} 个字符"));
    }
    Ok(trimmed.to_string())
}

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

pub struct AccountManager {
    data_dir: PathBuf,
    creds: Box<dyn CredentialStore>,
    lock: Mutex<()>,
}

impl AccountManager {
    pub fn new(data_dir: PathBuf) -> Self {
        Self::with_store(data_dir, Box::new(OsCredentialStore::new()))
    }

    pub fn with_store(data_dir: PathBuf, creds: Box<dyn CredentialStore>) -> Self {
        Self {
            data_dir,
            creds,
            lock: Mutex::new(()),
        }
    }

    fn meta_path(&self) -> PathBuf {
        self.data_dir.join(ACCOUNTS_FILE_NAME)
    }

    fn load_file(&self) -> AccountsFile {
        let path = self.meta_path();
        let Ok(raw) = std::fs::read_to_string(&path) else {
            return AccountsFile {
                version: 1,
                ..AccountsFile::default()
            };
        };
        let mut file = serde_json::from_str::<AccountsFile>(&raw).unwrap_or_else(|_| AccountsFile {
            version: 1,
            ..AccountsFile::default()
        });
        for acc in &mut file.accounts {
            if acc.app_data_dir.trim().is_empty() {
                acc.app_data_dir = app_data_dir_for_account(&acc.id);
            }
        }
        file
    }

    fn save_file(&self, file: &AccountsFile) -> Result<(), String> {
        let path = self.meta_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("创建账号配置目录失败：{e}"))?;
        }
        let body = serde_json::to_string_pretty(file)
            .map_err(|e| format!("序列化账号配置失败：{e}"))?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, &body).map_err(|e| format!("写入账号配置失败：{e}"))?;
        if std::fs::rename(&tmp, &path).is_err() {
            std::fs::write(&path, &body).map_err(|e| format!("保存账号配置失败：{e}"))?;
            let _ = std::fs::remove_file(&tmp);
        }
        Ok(())
    }

    fn find_matching_index(
        accounts: &[StoredAccount],
        identity: &CredentialIdentity,
        preferred_id: Option<&str>,
    ) -> Option<usize> {
        if let Some(ref email) = identity.email {
            if let Some(idx) = accounts
                .iter()
                .position(|a| !a.email.is_empty() && a.email.eq_ignore_ascii_case(email))
            {
                return Some(idx);
            }
        }
        if let Some(idx) = accounts
            .iter()
            .position(|a| !a.fingerprint.is_empty() && a.fingerprint == identity.fingerprint)
        {
            return Some(idx);
        }
        if let Some(pid) = preferred_id {
            if let Some(idx) = accounts.iter().position(|a| a.id == pid) {
                // Only match by preferred_id when neither has a conflicting email.
                if identity.email.is_none() || accounts[idx].email.is_empty() {
                    return Some(idx);
                }
            }
        }
        None
    }

    fn build_snapshot(
        &self,
        file: &AccountsFile,
        current_identity: Option<&CredentialIdentity>,
    ) -> AntigravityAccountsSnapshot {
        AntigravityAccountsSnapshot {
            accounts: file.accounts.iter().map(AntigravityAccount::from).collect(),
            active_account_id: file.active_account_id.clone(),
            current_email: current_identity.and_then(|c| c.email.clone()),
            current_auth_method: current_identity.and_then(|c| c.auth_method.clone()),
            credential_supported: self.creds.is_supported(),
            can_interactive_login: cfg!(windows),
        }
    }

    /// 同步系统凭据库中的当前活跃账号到 Nova 账号库。
    /// - `force_import = false`：若当前指纹位于 `ignored_fingerprint` 中则跳过自动导入；
    /// - `force_import = true`：清除忽略标记并强制导入当前系统登录。
    fn sync_active_locked(
        &self,
        file: &mut AccountsFile,
        force_import: bool,
        preferred_target_id: Option<&str>,
    ) -> Result<Option<CredentialIdentity>, String> {
        if !self.creds.is_supported() {
            return Ok(None);
        }
        let Some(blob) = self.creds.read_active()? else {
            file.active_account_id = None;
            return Ok(None);
        };
        let identity = inspect_credential_blob(&blob);
        if force_import {
            file.ignored_fingerprint = None;
        } else if file
            .ignored_fingerprint
            .as_deref()
            .is_some_and(|fp| fp == identity.fingerprint)
        {
            file.active_account_id = None;
            return Ok(Some(identity));
        }

        let now = now_ms();
        let email = identity.email.clone().unwrap_or_default();
        let auth_method = identity
            .auth_method
            .clone()
            .unwrap_or_else(|| "oauth-personal".into());

        if let Some(idx) = Self::find_matching_index(
            &file.accounts,
            &identity,
            preferred_target_id,
        ) {
            let id = file.accounts[idx].id.clone();
            let user_hint = if !email.is_empty() {
                email.as_str()
            } else {
                id.as_str()
            };
            self.creds.write_saved(&id, user_hint, &blob)?;
            let acc = &mut file.accounts[idx];
            if !email.is_empty() {
                acc.email = email;
            }
            if !auth_method.is_empty() {
                acc.auth_method = auth_method;
            }
            if acc.app_data_dir.trim().is_empty() {
                acc.app_data_dir = app_data_dir_for_account(&id);
            }
            acc.fingerprint = identity.fingerprint.clone();
            file.active_account_id = Some(id);
        } else {
            let id = Uuid::new_v4().to_string();
            let app_data_dir = app_data_dir_for_account(&id);
            let label = default_label_for(&email, &auth_method);
            let user_hint = if !email.is_empty() {
                email.as_str()
            } else {
                id.as_str()
            };
            self.creds.write_saved(&id, user_hint, &blob)?;
            file.accounts.push(StoredAccount {
                id: id.clone(),
                email,
                label,
                auth_method,
                app_data_dir,
                tier_name: None,
                tier_id: None,
                fingerprint: identity.fingerprint.clone(),
                added_at: now,
                last_used_at: now,
            });
            file.active_account_id = Some(id);
        }

        file.version = 1;
        self.save_file(file)?;
        Ok(Some(identity))
    }

    /// 返回当前活跃账号的 `(account_id, app_data_dir, token_expiry_ms)`。
    /// 若尚未同步过系统凭据库，会先执行一次轻量同步以确保隔离目录与活跃账号一致。
    pub fn active_account_context_with_expiry(&self) -> (Option<String>, String, Option<i64>) {
        let _guard = self.lock.lock().unwrap();
        let mut file = self.load_file();
        let current_identity = self.sync_active_locked(&mut file, false, None).ok().flatten();
        let token_expiry_ms = current_identity.and_then(|c| c.token_expiry_ms);
        if let Some(ref active_id) = file.active_account_id {
            if let Some(acc) = file.accounts.iter().find(|a| &a.id == active_id) {
                let dir = if acc.app_data_dir.trim().is_empty() {
                    app_data_dir_for_account(&acc.id)
                } else {
                    acc.app_data_dir.clone()
                };
                return (Some(active_id.clone()), dir, token_expiry_ms);
            }
        }
        (
            None,
            super::core::process::DEFAULT_APP_DATA_DIR.to_string(),
            token_expiry_ms,
        )
    }

    /// 返回当前活跃账号的 `(account_id, app_data_dir)`。
    /// 若尚未同步过系统凭据库，会先执行一次轻量同步以确保隔离目录与活跃账号一致。
    pub fn active_account_context(&self) -> (Option<String>, String) {
        let (account_id, app_data_dir, _) = self.active_account_context_with_expiry();
        (account_id, app_data_dir)
    }

    pub(crate) fn current_terminal_account_id(&self) -> Result<String, String> {
        let snapshot = self.list_and_sync()?;
        snapshot.active_account_id.filter(|_| snapshot.credential_supported).ok_or_else(|| {
            "未检测到可隔离的当前反重力登录。请先登录并在设置中同步终端账号，或选择已保存账号。".into()
        })
    }

    /// 从已保存账号初始化独立 CLI 凭据，不读写共享的活跃凭据槽。
    pub(crate) fn prepare_terminal_home(&self, account_id: &str) -> Result<PathBuf, String> {
        let _guard = self.lock.lock().unwrap();
        let root = dunce::canonicalize(&self.data_dir)
            .map_err(|e| format!("无法打开 Nova 数据目录：{e}"))?;
        let home = terminal_account_home(&root, account_id)?;
        let accounts = self.load_file();
        let account = accounts
            .accounts
            .iter()
            .find(|account| account.id == account_id)
            .ok_or("所绑定的反重力账号已不存在，请选择其他账号")?;
        let cli_dir = terminal_cli_dir(&home);
        let mut dirs = std::fs::DirBuilder::new();
        dirs.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            dirs.mode(0o700);
        }
        dirs.create(&cli_dir)
            .map_err(|e| format!("无法创建反重力账号专用目录：{e}"))?;
        if dunce::canonicalize(&cli_dir).ok().as_ref() != Some(&cli_dir) {
            return Err("反重力账号专用目录已被重定向，无法安全启动".into());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let permissions = std::fs::metadata(&home).map_err(|e| e.to_string())?.permissions();
            if permissions.mode() & 0o077 != 0 {
                return Err("反重力账号专用目录必须仅当前用户可访问".into());
            }
        }

        let token_path = cli_dir.join(TERMINAL_TOKEN_FILE);
        match std::fs::symlink_metadata(&token_path) {
            Ok(meta) if meta.is_file() => {
                // 保留 CLI 刷新后的凭据，但不复用已切换或无法确认归属的登录。
                let blob = std::fs::read(&token_path).map_err(|_| "无法读取反重力独立登录")?;
                validate_terminal_credential(account, &blob)?;
                return Ok(home);
            }
            Ok(_) => return Err("反重力独立凭据路径不是普通文件".into()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err("无法读取反重力独立凭据路径".into()),
        }

        let blob = self.creds.read_saved(account_id)?
            .ok_or("所绑定的反重力账号缺少已保存凭据，请重新登录该账号")?;
        validate_terminal_credential(account, &blob)?;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&token_path).map_err(|e| format!("无法创建反重力独立凭据：{e}"))?;
        if let Err(error) = file.write_all(&blob) {
            drop(file);
            let _ = std::fs::remove_file(&token_path);
            return Err(format!("无法初始化反重力独立登录：{error}"));
        }
        Ok(home)
    }

    /// 列出已保存账号，并自动同步当前系统凭据库中的活跃登录。
    pub fn list_and_sync(&self) -> Result<AntigravityAccountsSnapshot, String> {
        let _guard = self.lock.lock().unwrap();
        let mut file = self.load_file();
        let current = self.sync_active_locked(&mut file, false, None)?;
        Ok(self.build_snapshot(&file, current.as_ref()))
    }

    /// 强制导入/同步当前终端中的 `agy` 登录状态（如用户刚在终端执行了 `/logout` + `/login`）。
    pub fn force_sync_current(&self) -> Result<AntigravityAccountsSnapshot, String> {
        let _guard = self.lock.lock().unwrap();
        let mut file = self.load_file();
        let current = self.sync_active_locked(&mut file, true, None)?;
        if let Some(ref active_id) = file.active_account_id {
            if let Some(acc) = file.accounts.iter_mut().find(|a| &a.id == active_id) {
                acc.last_used_at = now_ms();
            }
            self.save_file(&file)?;
        }
        Ok(self.build_snapshot(&file, current.as_ref()))
    }

    /// 一键切换到指定已保存账号：先回写当前活跃账号的最新令牌，再将目标账号凭据写入系统凭据库。
    pub fn activate_account(&self, account_id: &str) -> Result<AntigravityAccountsSnapshot, String> {
        let _guard = self.lock.lock().unwrap();
        if !self.creds.is_supported() {
            return Err("当前系统暂不支持读写 Antigravity 凭据库，请在终端切号后点击「同步终端登录」".into());
        }
        let mut file = self.load_file();
        // 切换前先回写当前活跃凭据，避免丢失 agy 刚刚刷新的 token。
        let _ = self.sync_active_locked(&mut file, false, None);

        let target_idx = file
            .accounts
            .iter()
            .position(|a| a.id == account_id)
            .ok_or_else(|| "未找到该 Antigravity 账号，请刷新列表后重试".to_string())?;

        let blob = self.creds.read_saved(account_id)?.ok_or_else(|| {
            format!(
                "账号「{}」的凭据已丢失，请点击「重新登录」重新授权",
                file.accounts[target_idx].label
            )
        })?;

        self.creds.write_active(&blob)?;
        let identity = inspect_credential_blob(&blob);
        file.accounts[target_idx].last_used_at = now_ms();
        file.accounts[target_idx].fingerprint = identity.fingerprint.clone();
        if let Some(ref email) = identity.email {
            if file.accounts[target_idx].email.is_empty() {
                file.accounts[target_idx].email = email.clone();
            }
        }
        file.active_account_id = Some(account_id.to_string());
        file.ignored_fingerprint = None;
        file.version = 1;
        self.save_file(&file)?;

        Ok(self.build_snapshot(&file, Some(&identity)))
    }

    /// 准备交互式登录新账号：先安全备份当前活跃凭据，再清空活跃槽让 `agy acp --login` 强制弹出 Google 账号选择。
    pub fn prepare_interactive_login(&self) -> Result<LoginRollbackBackup, String> {
        let _guard = self.lock.lock().unwrap();
        let mut file = self.load_file();
        let _ = self.sync_active_locked(&mut file, false, None);
        let previous_blob = if self.creds.is_supported() {
            self.creds.read_active()?
        } else {
            None
        };
        let previous_active_id = file.active_account_id.clone();
        if self.creds.is_supported() {
            self.creds.delete_active()?;
        }
        Ok(LoginRollbackBackup {
            previous_blob,
            previous_active_id,
        })
    }

    /// 交互式登录失败或被用户取消时，恢复登录前的活跃凭据。
    pub fn rollback_interactive_login(&self, backup: LoginRollbackBackup) {
        let _guard = self.lock.lock().unwrap();
        if self.creds.is_supported() {
            if let Some(blob) = backup.previous_blob {
                let _ = self.creds.write_active(&blob);
            }
        }
        let mut file = self.load_file();
        file.active_account_id = backup.previous_active_id;
        let _ = self.save_file(&file);
    }

    /// 交互式登录成功后，保存新登录的凭据并设为当前活跃账号。
    pub fn commit_interactive_login(
        &self,
        target_account_id: Option<&str>,
    ) -> Result<AntigravityAccountsSnapshot, String> {
        let _guard = self.lock.lock().unwrap();
        let mut file = self.load_file();
        let current = self.sync_active_locked(&mut file, true, target_account_id)?;
        if current.is_none() {
            return Err("未检测到新的登录凭据，登录可能未完成".into());
        }
        if let Some(ref active_id) = file.active_account_id {
            if let Some(acc) = file.accounts.iter_mut().find(|a| &a.id == active_id) {
                acc.last_used_at = now_ms();
            }
            self.save_file(&file)?;
        }
        Ok(self.build_snapshot(&file, current.as_ref()))
    }

    /// 更新当前活跃账号的订阅档位（从 `session/new` 的 `_meta.antigravity.tier` 自动提取）。
    pub fn update_active_tier(
        &self,
        tier_name: Option<String>,
        tier_id: Option<String>,
    ) -> Option<AntigravityAccountsSnapshot> {
        if tier_name.is_none() && tier_id.is_none() {
            return None;
        }
        let _guard = self.lock.lock().unwrap();
        let mut file = self.load_file();
        if file.active_account_id.is_none() {
            let _ = self.sync_active_locked(&mut file, false, None);
        }
        let active_id = file.active_account_id.clone()?;
        let acc = file.accounts.iter_mut().find(|a| a.id == active_id)?;
        if acc.tier_name == tier_name && acc.tier_id == tier_id {
            return None;
        }
        acc.tier_name = tier_name;
        acc.tier_id = tier_id;
        let _ = self.save_file(&file);
        let current = self
            .creds
            .read_active()
            .ok()
            .flatten()
            .map(|b| inspect_credential_blob(&b));
        Some(self.build_snapshot(&file, current.as_ref()))
    }

    /// 重命名账号显示名称（例如「主号」「公司号」）。
    pub fn rename_account(
        &self,
        account_id: &str,
        new_label: &str,
    ) -> Result<AntigravityAccountsSnapshot, String> {
        let label = normalize_label(new_label)?;
        let _guard = self.lock.lock().unwrap();
        let mut file = self.load_file();
        let acc = file
            .accounts
            .iter_mut()
            .find(|a| a.id == account_id)
            .ok_or_else(|| "未找到该 Antigravity 账号".to_string())?;
        acc.label = label;
        self.save_file(&file)?;
        let current = self
            .creds
            .read_active()
            .ok()
            .flatten()
            .map(|b| inspect_credential_blob(&b));
        Ok(self.build_snapshot(&file, current.as_ref()))
    }

    /// 从 Nova 账号列表中删除一个已保存账号及其加密凭据。
    pub fn delete_account(&self, account_id: &str) -> Result<AntigravityAccountsSnapshot, String> {
        let _guard = self.lock.lock().unwrap();
        let mut file = self.load_file();
        let idx = file
            .accounts
            .iter()
            .position(|a| a.id == account_id)
            .ok_or_else(|| "未找到该 Antigravity 账号".to_string())?;
        let removed = file.accounts.remove(idx);
        if self.creds.is_supported() {
            let _ = self.creds.delete_saved(&removed.id);
        }

        let current = if self.creds.is_supported() {
            self.creds
                .read_active()
                .ok()
                .flatten()
                .map(|b| inspect_credential_blob(&b))
        } else {
            None
        };

        if file.active_account_id.as_deref() == Some(account_id) {
            file.active_account_id = None;
            if let Some(ref cur) = current {
                if cur.fingerprint == removed.fingerprint
                    || (!removed.email.is_empty()
                        && cur
                            .email
                            .as_deref()
                            .is_some_and(|e| e.eq_ignore_ascii_case(&removed.email)))
                {
                    file.ignored_fingerprint = Some(cur.fingerprint.clone());
                }
            }
        }

        self.save_file(&file)?;
        Ok(self.build_snapshot(&file, current.as_ref()))
    }

    /// 读取指定账号当前可用的最新凭据（优先采用独立终端目录中更新的 OAuth token）。
    fn read_freshest_credential_locked(&self, acc: &StoredAccount) -> Result<Option<Vec<u8>>, String> {
        let saved = self.creds.read_saved(&acc.id)?;
        let term_blob = terminal_account_home(&self.data_dir, &acc.id)
            .ok()
            .map(|home| terminal_cli_dir(&home).join(TERMINAL_TOKEN_FILE))
            .filter(|token_path| {
                std::fs::symlink_metadata(token_path).is_ok_and(|meta| meta.is_file())
            })
            .and_then(|token_path| std::fs::read(token_path).ok())
            .filter(|blob| validate_terminal_credential(acc, blob).is_ok());

        match (saved, term_blob) {
            (Some(s), Some(t)) => {
                let saved_exp = inspect_credential_blob(&s).token_expiry_ms;
                let term_exp = inspect_credential_blob(&t).token_expiry_ms;
                let term_is_newer = match (term_exp, saved_exp) {
                    (Some(te), Some(se)) => te > se,
                    (Some(_), None) => true,
                    _ => false,
                };
                Ok(Some(if term_is_newer { t } else { s }))
            }
            (Some(s), None) => Ok(Some(s)),
            (None, Some(t)) => Ok(Some(t)),
            (None, None) => Ok(None),
        }
    }

    /// 导入更新已有账号后清理独立终端目录中的旧 token，确保下次启动从最新凭据初始化。
    fn remove_stale_terminal_token(&self, account_id: &str) {
        let Ok(home) = terminal_account_home(&self.data_dir, account_id) else {
            return;
        };
        let token_path = terminal_cli_dir(&home).join(TERMINAL_TOKEN_FILE);
        if std::fs::symlink_metadata(&token_path).is_ok_and(|meta| meta.is_file()) {
            let _ = std::fs::remove_file(&token_path);
        }
    }

    fn write_bundle_file(path: &Path, body: &str) -> Result<(), String> {
        if path.is_dir() {
            return Err("导出路径不能是目录，请选择 .json 文件".into());
        }
        if !path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
        {
            return Err("请选择 .json 文件保存反重力账号".into());
        }
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("创建导出目录失败：{e}"))?;
            }
        }
        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("antigravity-accounts.json");
        let tmp = path.with_file_name(format!(".{file_name}.{}.tmp", Uuid::new_v4().simple()));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut tmp_file = options
            .open(&tmp)
            .map_err(|e| format!("创建导出临时文件失败：{e}"))?;
        if let Err(err) = tmp_file.write_all(body.as_bytes()).and_then(|_| tmp_file.flush()) {
            drop(tmp_file);
            let _ = std::fs::remove_file(&tmp);
            return Err(format!("写入导出文件失败：{err}"));
        }
        drop(tmp_file);
        if std::fs::rename(&tmp, path).is_err() {
            let mut direct_opts = std::fs::OpenOptions::new();
            direct_opts.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                direct_opts.mode(0o600);
            }
            let write_res = direct_opts
                .open(path)
                .and_then(|mut f| f.write_all(body.as_bytes()).and_then(|_| f.flush()));
            let _ = std::fs::remove_file(&tmp);
            write_res.map_err(|e| format!("保存导出文件失败：{e}"))?;
        }
        Ok(())
    }

    /// 批量导出选中的 Antigravity 账号及凭据为 JSON 文件，供另一台 Nova 导入。
    pub fn export_accounts_to_path(
        &self,
        account_ids: &[String],
        path: &Path,
    ) -> Result<AntigravityAccountsExportResult, String> {
        if !self.creds.is_supported() {
            return Err("当前系统不支持读取系统凭据库，无法导出反重力账号".into());
        }
        let mut selected_ids = Vec::new();
        let mut seen = HashSet::new();
        for raw_id in account_ids {
            let id = raw_id.trim();
            if !id.is_empty() && seen.insert(id.to_string()) {
                selected_ids.push(id.to_string());
            }
        }
        if selected_ids.is_empty() {
            return Err("请至少选择一个要导出的 Antigravity 账号".into());
        }
        if selected_ids.len() > MAX_EXPORT_ACCOUNTS {
            return Err(format!("单次最多导出 {MAX_EXPORT_ACCOUNTS} 个账号"));
        }

        let _guard = self.lock.lock().unwrap();
        let mut file = self.load_file();
        // 导出前同步当前活跃账号，保证刚刷新的令牌一起导出。
        let _ = self.sync_active_locked(&mut file, false, None);

        let mut entries = Vec::with_capacity(selected_ids.len());
        let mut skipped_count = 0usize;
        let mut warnings = Vec::new();

        for target_id in &selected_ids {
            let Some(acc) = file.accounts.iter().find(|a| &a.id == target_id) else {
                skipped_count += 1;
                warnings.push(format!("未找到账号 {target_id}，已跳过"));
                continue;
            };
            let display = if !acc.label.trim().is_empty() {
                acc.label.trim()
            } else if !acc.email.trim().is_empty() {
                acc.email.trim()
            } else {
                acc.id.as_str()
            };
            let blob = match self.read_freshest_credential_locked(acc) {
                Ok(Some(blob)) => blob,
                Ok(None) => {
                    skipped_count += 1;
                    warnings.push(format!("账号「{display}」缺少已保存凭据，已跳过"));
                    continue;
                }
                Err(err) => {
                    skipped_count += 1;
                    warnings.push(format!("读取账号「{display}」凭据失败（{err}），已跳过"));
                    continue;
                }
            };
            if blob.is_empty() || blob.len() > MAX_CREDENTIAL_BLOB_BYTES {
                skipped_count += 1;
                warnings.push(format!("账号「{display}」凭据大小异常，已跳过"));
                continue;
            }
            entries.push(ExportedAccountEntry {
                email: acc.email.clone(),
                label: acc.label.clone(),
                auth_method: acc.auth_method.clone(),
                tier_name: acc.tier_name.clone(),
                tier_id: acc.tier_id.clone(),
                credential_b64: B64_STANDARD.encode(&blob),
            });
        }

        if entries.is_empty() {
            let reason = warnings
                .first()
                .cloned()
                .unwrap_or_else(|| "所选账号均无可用凭据".into());
            return Err(format!("未导出任何账号：{reason}"));
        }

        let bundle = AccountExportBundle {
            format: EXPORT_BUNDLE_FORMAT.to_string(),
            version: EXPORT_BUNDLE_VERSION,
            exported_at: now_ms(),
            accounts: entries,
        };
        let body = serde_json::to_string_pretty(&bundle)
            .map_err(|e| format!("序列化导出包失败：{e}"))?;
        Self::write_bundle_file(path, &body)?;

        Ok(AntigravityAccountsExportResult {
            exported_count: bundle.accounts.len(),
            skipped_count,
            warnings,
            path: path.to_string_lossy().into_owned(),
        })
    }

    /// 从导出的 JSON 文件批量导入 Antigravity 账号并写入本机凭据库。
    pub fn import_accounts_from_path(
        &self,
        path: &Path,
    ) -> Result<AntigravityAccountsImportResult, String> {
        if !self.creds.is_supported() {
            return Err("当前系统不支持写入系统凭据库，无法导入反重力账号".into());
        }
        let meta = std::fs::metadata(path).map_err(|e| format!("无法读取导入文件：{e}"))?;
        if !meta.is_file() {
            return Err("导入路径不是有效文件".into());
        }
        if meta.len() > MAX_IMPORT_FILE_BYTES as u64 {
            return Err("导入文件体积过大（超过 4MB 上限）".into());
        }
        let raw = std::fs::read(path).map_err(|e| format!("读取导入文件失败：{e}"))?;
        let bundle: AccountExportBundle =
            serde_json::from_slice(&raw).map_err(|e| format!("导入文件 JSON 格式无效：{e}"))?;
        if bundle.format != EXPORT_BUNDLE_FORMAT {
            return Err("不是有效的 Nova 反重力账号导出文件".into());
        }
        if bundle.version != EXPORT_BUNDLE_VERSION {
            return Err(format!(
                "不支持的账号导出版本（v{}），请升级 Nova 后再导入",
                bundle.version
            ));
        }
        if bundle.accounts.is_empty() {
            return Err("导入文件中不包含任何账号".into());
        }
        if bundle.accounts.len() > MAX_EXPORT_ACCOUNTS {
            return Err(format!(
                "单次最多导入 {MAX_EXPORT_ACCOUNTS} 个账号"
            ));
        }

        let _guard = self.lock.lock().unwrap();
        let mut file = self.load_file();
        // 导入前先同步本机当前活跃凭据，避免本机活跃登录未入库就被忽略或丢失。
        let mut current_active = self.sync_active_locked(&mut file, false, None).ok().flatten();

        let now = now_ms();
        let mut imported_ids = HashSet::new();
        let mut updated_ids = HashSet::new();
        let mut updated_account_ids = Vec::new();
        let mut skipped_count = 0usize;
        let mut warnings = Vec::new();
        let mut active_updated = false;

        for (index, entry) in bundle.accounts.iter().enumerate() {
            let entry_hint = if !entry.label.trim().is_empty() {
                entry.label.trim().to_string()
            } else if !entry.email.trim().is_empty() {
                entry.email.trim().to_string()
            } else {
                format!("第 {} 项", index + 1)
            };

            let blob = match B64_STANDARD.decode(entry.credential_b64.trim()) {
                Ok(b) if !b.is_empty() && b.len() <= MAX_CREDENTIAL_BLOB_BYTES => b,
                Ok(_) => {
                    skipped_count += 1;
                    warnings.push(format!("账号「{entry_hint}」凭据为空或过大，已跳过"));
                    continue;
                }
                Err(_) => {
                    skipped_count += 1;
                    warnings.push(format!("账号「{entry_hint}」凭据编码损坏，已跳过"));
                    continue;
                }
            };
            if !matches!(serde_json::from_slice::<Value>(&blob), Ok(Value::Object(_))) {
                skipped_count += 1;
                warnings.push(format!("账号「{entry_hint}」凭据内容不是有效 JSON 对象，已跳过"));
                continue;
            }

            let identity = inspect_credential_blob(&blob);
            let email = identity
                .email
                .clone()
                .unwrap_or_else(|| entry.email.trim().to_string());
            let auth_method = identity
                .auth_method
                .clone()
                .or_else(|| {
                    let m = entry.auth_method.trim();
                    (!m.is_empty()).then(|| m.to_string())
                })
                .unwrap_or_else(|| "oauth-personal".into());
            let label = normalize_label(&entry.label)
                .unwrap_or_else(|_| default_label_for(&email, &auth_method));
            let tier_name = entry
                .tier_name
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            let tier_id = entry
                .tier_id
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string);

            if let Some(idx) = Self::find_matching_index(&file.accounts, &identity, None) {
                let id = file.accounts[idx].id.clone();
                let user_hint = if !email.is_empty() {
                    email.as_str()
                } else {
                    id.as_str()
                };
                if let Err(err) = self.creds.write_saved(&id, user_hint, &blob) {
                    skipped_count += 1;
                    warnings.push(format!("写入账号「{entry_hint}」凭据失败（{err}），已跳过"));
                    continue;
                }

                let is_active_match = file.active_account_id.as_deref() == Some(id.as_str())
                    || current_active
                        .as_ref()
                        .is_some_and(|cur| cur.fingerprint == identity.fingerprint);
                if is_active_match {
                    if let Err(err) = self.creds.write_active(&blob) {
                        warnings.push(format!("更新当前活跃账号「{entry_hint}」系统凭据失败：{err}"));
                    } else {
                        file.active_account_id = Some(id.clone());
                        current_active = Some(identity.clone());
                        active_updated = true;
                    }
                }

                let acc = &mut file.accounts[idx];
                let old_default = default_label_for(&acc.email, &acc.auth_method);
                let incoming_default = default_label_for(&email, &auth_method);
                if !email.is_empty() {
                    acc.email = email;
                }
                if !auth_method.is_empty() {
                    acc.auth_method = auth_method;
                }
                if acc.label.trim().is_empty()
                    || (acc.label == old_default && label != incoming_default)
                {
                    acc.label = label;
                }
                if acc.app_data_dir.trim().is_empty() {
                    acc.app_data_dir = app_data_dir_for_account(&id);
                }
                if let Some(tn) = tier_name {
                    acc.tier_name = Some(tn);
                }
                if let Some(ti) = tier_id {
                    acc.tier_id = Some(ti);
                }
                acc.fingerprint = identity.fingerprint.clone();

                self.remove_stale_terminal_token(&id);
                if file.ignored_fingerprint.as_deref() == Some(identity.fingerprint.as_str()) {
                    file.ignored_fingerprint = None;
                }

                if !imported_ids.contains(&id) {
                    updated_ids.insert(id.clone());
                }
                updated_account_ids.push(id);
            } else {
                let id = Uuid::new_v4().to_string();
                let app_data_dir = app_data_dir_for_account(&id);
                let user_hint = if !email.is_empty() {
                    email.as_str()
                } else {
                    id.as_str()
                };
                if let Err(err) = self.creds.write_saved(&id, user_hint, &blob) {
                    skipped_count += 1;
                    warnings.push(format!("写入账号「{entry_hint}」凭据失败（{err}），已跳过"));
                    continue;
                }

                let is_active_match = current_active
                    .as_ref()
                    .is_some_and(|cur| cur.fingerprint == identity.fingerprint);
                if is_active_match {
                    if let Err(err) = self.creds.write_active(&blob) {
                        warnings.push(format!("更新当前活跃账号「{entry_hint}」系统凭据失败：{err}"));
                    } else {
                        file.active_account_id = Some(id.clone());
                        current_active = Some(identity.clone());
                        active_updated = true;
                    }
                }

                if file.ignored_fingerprint.as_deref() == Some(identity.fingerprint.as_str()) {
                    file.ignored_fingerprint = None;
                }

                file.accounts.push(StoredAccount {
                    id: id.clone(),
                    email,
                    label,
                    auth_method,
                    app_data_dir,
                    tier_name,
                    tier_id,
                    fingerprint: identity.fingerprint,
                    added_at: now,
                    last_used_at: 0,
                });

                imported_ids.insert(id.clone());
                updated_account_ids.push(id);
            }
        }

        if imported_ids.is_empty() && updated_ids.is_empty() {
            let reason = warnings
                .first()
                .cloned()
                .unwrap_or_else(|| "没有可导入的有效账号".into());
            return Err(format!("导入失败：{reason}"));
        }

        file.version = 1;
        self.save_file(&file)?;
        let snapshot = self.build_snapshot(&file, current_active.as_ref());
        Ok(AntigravityAccountsImportResult {
            snapshot,
            imported_count: imported_ids.len(),
            updated_count: updated_ids.len(),
            skipped_count,
            warnings,
            active_updated,
            updated_account_ids,
        })
    }
}

// ============================================================================
// 跨平台系统凭据库实现
// ============================================================================

struct OsCredentialStore;

impl OsCredentialStore {
    fn new() -> Self {
        Self
    }
}

#[cfg(windows)]
mod win_cred {
    use super::{
        CredentialStore, OsCredentialStore, ACTIVE_TARGET_WINDOWS, ACTIVE_USER_WINDOWS,
        SAVED_TARGET_PREFIX_WINDOWS,
    };
    use std::ffi::c_void;

    const CRED_TYPE_GENERIC: u32 = 1;
    const CRED_PERSIST_LOCAL_MACHINE: u32 = 2;
    const CRED_MAX_CREDENTIAL_BLOB_SIZE: usize = 5 * 512;
    const ERROR_NOT_FOUND: i32 = 1168;

    #[repr(C)]
    struct FILETIME {
        dw_low_date_time: u32,
        dw_high_date_time: u32,
    }

    #[repr(C)]
    struct CREDENTIALW {
        flags: u32,
        type_: u32,
        target_name: *mut u16,
        comment: *mut u16,
        last_written: FILETIME,
        credential_blob_size: u32,
        credential_blob: *mut u8,
        persist: u32,
        attribute_count: u32,
        attributes: *mut c_void,
        target_alias: *mut u16,
        user_name: *mut u16,
    }

    #[link(name = "advapi32")]
    extern "system" {
        fn CredReadW(
            target_name: *const u16,
            type_: u32,
            flags: u32,
            credential: *mut *mut CREDENTIALW,
        ) -> i32;
        fn CredWriteW(credential: *const CREDENTIALW, flags: u32) -> i32;
        fn CredDeleteW(target_name: *const u16, type_: u32, flags: u32) -> i32;
        fn CredFree(buffer: *mut c_void);
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn read_generic(target: &str) -> Result<Option<Vec<u8>>, String> {
        let target_w = wide(target);
        let mut ptr: *mut CREDENTIALW = std::ptr::null_mut();
        let ok = unsafe { CredReadW(target_w.as_ptr(), CRED_TYPE_GENERIC, 0, &mut ptr) };
        if ok == 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(ERROR_NOT_FOUND) {
                return Ok(None);
            }
            return Err(format!("读取 Windows 凭据库失败：{err}"));
        }
        let result = unsafe {
            let cred = &*ptr;
            if cred.credential_blob_size == 0 || cred.credential_blob.is_null() {
                Vec::new()
            } else {
                std::slice::from_raw_parts(
                    cred.credential_blob,
                    cred.credential_blob_size as usize,
                )
                .to_vec()
            }
        };
        unsafe { CredFree(ptr as *mut c_void) };
        Ok(Some(result))
    }

    fn write_generic(target: &str, user_name: &str, blob: &[u8]) -> Result<(), String> {
        if blob.len() > CRED_MAX_CREDENTIAL_BLOB_SIZE {
            return Err(format!(
                "凭据体积（{} 字节）超过 Windows 凭据库上限（{CRED_MAX_CREDENTIAL_BLOB_SIZE} 字节）",
                blob.len()
            ));
        }
        let mut target_w = wide(target);
        let mut user_w = wide(user_name);
        let mut buf = blob.to_vec();
        let cred = CREDENTIALW {
            flags: 0,
            type_: CRED_TYPE_GENERIC,
            target_name: target_w.as_mut_ptr(),
            comment: std::ptr::null_mut(),
            last_written: FILETIME {
                dw_low_date_time: 0,
                dw_high_date_time: 0,
            },
            credential_blob_size: buf.len() as u32,
            credential_blob: if buf.is_empty() {
                std::ptr::null_mut()
            } else {
                buf.as_mut_ptr()
            },
            persist: CRED_PERSIST_LOCAL_MACHINE,
            attribute_count: 0,
            attributes: std::ptr::null_mut(),
            target_alias: std::ptr::null_mut(),
            user_name: user_w.as_mut_ptr(),
        };
        let ok = unsafe { CredWriteW(&cred, 0) };
        if ok == 0 {
            let err = std::io::Error::last_os_error();
            return Err(format!("写入 Windows 凭据库失败：{err}"));
        }
        Ok(())
    }

    fn delete_generic(target: &str) -> Result<(), String> {
        let target_w = wide(target);
        let ok = unsafe { CredDeleteW(target_w.as_ptr(), CRED_TYPE_GENERIC, 0) };
        if ok == 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(ERROR_NOT_FOUND) {
                return Ok(());
            }
            return Err(format!("删除 Windows 凭据失败：{err}"));
        }
        Ok(())
    }

    impl CredentialStore for OsCredentialStore {
        fn is_supported(&self) -> bool {
            true
        }

        fn read_active(&self) -> Result<Option<Vec<u8>>, String> {
            read_generic(ACTIVE_TARGET_WINDOWS)
        }

        fn write_active(&self, blob: &[u8]) -> Result<(), String> {
            write_generic(ACTIVE_TARGET_WINDOWS, ACTIVE_USER_WINDOWS, blob)
        }

        fn delete_active(&self) -> Result<(), String> {
            delete_generic(ACTIVE_TARGET_WINDOWS)
        }

        fn read_saved(&self, id: &str) -> Result<Option<Vec<u8>>, String> {
            read_generic(&format!("{SAVED_TARGET_PREFIX_WINDOWS}{id}"))
        }

        fn write_saved(&self, id: &str, user_hint: &str, blob: &[u8]) -> Result<(), String> {
            write_generic(
                &format!("{SAVED_TARGET_PREFIX_WINDOWS}{id}"),
                user_hint,
                blob,
            )
        }

        fn delete_saved(&self, id: &str) -> Result<(), String> {
            delete_generic(&format!("{SAVED_TARGET_PREFIX_WINDOWS}{id}"))
        }
    }
}

#[cfg(target_os = "macos")]
mod mac_cred {
    use super::{CredentialStore, OsCredentialStore, ACTIVE_ACCOUNT, ACTIVE_SERVICE, SAVED_SERVICE};
    use std::io::Write;
    use std::path::Path;
    use std::process::{Command, Stdio};

    fn decode_keychain_output(raw: &[u8]) -> Vec<u8> {
        let trimmed = raw
            .strip_suffix(b"\r\n")
            .or_else(|| raw.strip_suffix(b"\n"))
            .unwrap_or(raw);
        if trimmed.starts_with(b"{") {
            return trimmed.to_vec();
        }
        // macOS `security -w` 在凭据含非 ASCII 或换行时会输出十六进制字符串。
        if trimmed.len() >= 2 && trimmed.len() % 2 == 0 && trimmed.iter().all(|b| b.is_ascii_hexdigit()) {
            if let Ok(hex_str) = std::str::from_utf8(trimmed) {
                let mut decoded = Vec::with_capacity(trimmed.len() / 2);
                let mut ok = true;
                for i in (0..hex_str.len()).step_by(2) {
                    match u8::from_str_radix(&hex_str[i..i + 2], 16) {
                        Ok(byte) => decoded.push(byte),
                        Err(_) => {
                            ok = false;
                            break;
                        }
                    }
                }
                if ok && decoded.starts_with(b"{") {
                    return decoded;
                }
            }
        }
        trimmed.to_vec()
    }

    fn read_generic(service: &str, account: &str) -> Result<Option<Vec<u8>>, String> {
        let out = Command::new("/usr/bin/security")
            .args(["find-generic-password", "-s", service, "-a", account, "-w"])
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("调用 macOS Keychain 失败：{e}"))?;
        if !out.status.success() {
            // 44 = errSecItemNotFound
            if out.status.code() == Some(44) {
                return Ok(None);
            }
            let err = String::from_utf8_lossy(&out.stderr);
            if err.contains("could not be found") {
                return Ok(None);
            }
            return Err("读取 macOS Keychain 失败".into());
        }
        Ok(Some(decode_keychain_output(&out.stdout)))
    }

    fn write_generic(service: &str, account: &str, blob: &[u8]) -> Result<(), String> {
        let hex: String = blob.iter().map(|b| format!("{b:02x}")).collect();
        let mut child = Command::new("/usr/bin/security")
            .arg("-i")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("调用 macOS Keychain 失败：{e}"))?;
        if let Some(mut stdin) = child.stdin.take() {
            let line = format!(
                "add-generic-password -U -s \"{}\" -a \"{}\" -X {}\n",
                service.replace('"', ""),
                account.replace('"', ""),
                hex
            );
            stdin
                .write_all(line.as_bytes())
                .map_err(|e| format!("写入 macOS Keychain 失败：{e}"))?;
        }
        let out = child
            .wait_with_output()
            .map_err(|e| format!("等待 macOS Keychain 失败：{e}"))?;
        if !out.status.success() {
            return Err("写入 macOS Keychain 失败".into());
        }
        Ok(())
    }

    fn delete_generic(service: &str, account: &str) -> Result<(), String> {
        let _ = Command::new("/usr/bin/security")
            .args(["delete-generic-password", "-s", service, "-a", account])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        Ok(())
    }

    impl CredentialStore for OsCredentialStore {
        fn is_supported(&self) -> bool {
            Path::new("/usr/bin/security").is_file()
        }

        fn read_active(&self) -> Result<Option<Vec<u8>>, String> {
            read_generic(ACTIVE_SERVICE, ACTIVE_ACCOUNT)
        }

        fn write_active(&self, blob: &[u8]) -> Result<(), String> {
            write_generic(ACTIVE_SERVICE, ACTIVE_ACCOUNT, blob)
        }

        fn delete_active(&self) -> Result<(), String> {
            delete_generic(ACTIVE_SERVICE, ACTIVE_ACCOUNT)
        }

        fn read_saved(&self, id: &str) -> Result<Option<Vec<u8>>, String> {
            read_generic(SAVED_SERVICE, id)
        }

        fn write_saved(&self, id: &str, _user_hint: &str, blob: &[u8]) -> Result<(), String> {
            write_generic(SAVED_SERVICE, id, blob)
        }

        fn delete_saved(&self, id: &str) -> Result<(), String> {
            delete_generic(SAVED_SERVICE, id)
        }
    }
}

#[cfg(target_os = "linux")]
mod linux_cred {
    use super::{CredentialStore, OsCredentialStore, ACTIVE_ACCOUNT, ACTIVE_SERVICE, SAVED_SERVICE};
    use std::io::Write;
    use std::process::{Command, Stdio};

    fn has_secret_tool() -> bool {
        Command::new("secret-tool")
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok()
    }

    fn read_generic(service: &str, username: &str) -> Result<Option<Vec<u8>>, String> {
        let out = Command::new("secret-tool")
            .args(["lookup", "service", service, "username", username])
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("调用 secret-tool 失败：{e}"))?;
        if !out.status.success() || out.stdout.is_empty() {
            return Ok(None);
        }
        Ok(Some(out.stdout))
    }

    fn write_generic(service: &str, username: &str, label: &str, blob: &[u8]) -> Result<(), String> {
        let mut child = Command::new("secret-tool")
            .args([
                "store",
                "--label",
                label,
                "service",
                service,
                "username",
                username,
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("调用 secret-tool 失败：{e}"))?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(blob)
                .map_err(|e| format!("写入系统凭据库失败：{e}"))?;
        }
        let out = child
            .wait_with_output()
            .map_err(|e| format!("等待 secret-tool 失败：{e}"))?;
        if !out.status.success() {
            return Err("写入 Linux Secret Service 失败".into());
        }
        Ok(())
    }

    fn delete_generic(service: &str, username: &str) -> Result<(), String> {
        let _ = Command::new("secret-tool")
            .args(["clear", "service", service, "username", username])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        Ok(())
    }

    impl CredentialStore for OsCredentialStore {
        fn is_supported(&self) -> bool {
            has_secret_tool()
        }

        fn read_active(&self) -> Result<Option<Vec<u8>>, String> {
            read_generic(ACTIVE_SERVICE, ACTIVE_ACCOUNT)
        }

        fn write_active(&self, blob: &[u8]) -> Result<(), String> {
            write_generic(ACTIVE_SERVICE, ACTIVE_ACCOUNT, "gemini:antigravity", blob)
        }

        fn delete_active(&self) -> Result<(), String> {
            delete_generic(ACTIVE_SERVICE, ACTIVE_ACCOUNT)
        }

        fn read_saved(&self, id: &str) -> Result<Option<Vec<u8>>, String> {
            read_generic(SAVED_SERVICE, id)
        }

        fn write_saved(&self, id: &str, user_hint: &str, blob: &[u8]) -> Result<(), String> {
            write_generic(
                SAVED_SERVICE,
                id,
                &format!("Nova Antigravity ({user_hint})"),
                blob,
            )
        }

        fn delete_saved(&self, id: &str) -> Result<(), String> {
            delete_generic(SAVED_SERVICE, id)
        }
    }
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
impl CredentialStore for OsCredentialStore {
    fn is_supported(&self) -> bool {
        false
    }
    fn read_active(&self) -> Result<Option<Vec<u8>>, String> {
        Ok(None)
    }
    fn write_active(&self, _blob: &[u8]) -> Result<(), String> {
        Err("当前系统不支持凭据库读写".into())
    }
    fn delete_active(&self) -> Result<(), String> {
        Ok(())
    }
    fn read_saved(&self, _id: &str) -> Result<Option<Vec<u8>>, String> {
        Ok(None)
    }
    fn write_saved(&self, _id: &str, _user_hint: &str, _blob: &[u8]) -> Result<(), String> {
        Err("当前系统不支持凭据库读写".into())
    }
    fn delete_saved(&self, _id: &str) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[derive(Default)]
    struct MemoryStore {
        active: Mutex<Option<Vec<u8>>>,
        saved: Mutex<HashMap<String, Vec<u8>>>,
    }

    impl MemoryStore {
        fn with_active(blob: &[u8]) -> Self {
            Self {
                active: Mutex::new(Some(blob.to_vec())),
                saved: Mutex::new(HashMap::new()),
            }
        }
    }

    impl CredentialStore for MemoryStore {
        fn is_supported(&self) -> bool {
            true
        }
        fn read_active(&self) -> Result<Option<Vec<u8>>, String> {
            Ok(self.active.lock().unwrap().clone())
        }
        fn write_active(&self, blob: &[u8]) -> Result<(), String> {
            *self.active.lock().unwrap() = Some(blob.to_vec());
            Ok(())
        }
        fn delete_active(&self) -> Result<(), String> {
            *self.active.lock().unwrap() = None;
            Ok(())
        }
        fn read_saved(&self, id: &str) -> Result<Option<Vec<u8>>, String> {
            Ok(self.saved.lock().unwrap().get(id).cloned())
        }
        fn write_saved(&self, id: &str, _user_hint: &str, blob: &[u8]) -> Result<(), String> {
            self.saved.lock().unwrap().insert(id.to_string(), blob.to_vec());
            Ok(())
        }
        fn delete_saved(&self, id: &str) -> Result<(), String> {
            self.saved.lock().unwrap().remove(id);
            Ok(())
        }
    }

    fn cred_json(email: &str, token: &str) -> Vec<u8> {
        serde_json::json!({
            "token": { "access_token": token, "refresh_token": format!("rt-{token}") },
            "auth_method": "oauth-personal",
            "id_token_email": email,
        })
        .to_string()
        .into_bytes()
    }

    #[test]
    fn terminal_profiles_use_saved_accounts_and_keep_refreshes() {
        let dir = tempfile::tempdir().unwrap();
        let alice = cred_json("alice@example.com", "a1");
        let bob = cred_json("bob@example.com", "b1");
        let manager = AccountManager::with_store(
            dir.path().to_path_buf(),
            Box::new(MemoryStore::with_active(&alice)),
        );
        let alice_id = manager.current_terminal_account_id().unwrap();
        let alice_home = manager.prepare_terminal_home(&alice_id).unwrap();
        manager.creds.write_active(&bob).unwrap();
        let bob_id = manager.current_terminal_account_id().unwrap();
        let bob_home = manager.prepare_terminal_home(&bob_id).unwrap();
        assert_eq!(manager.prepare_terminal_home(&alice_id).unwrap(), alice_home);
        assert_eq!(manager.current_terminal_account_id().unwrap(), bob_id);
        assert_ne!(alice_home, bob_home);
        let alice_token = terminal_cli_dir(&alice_home).join(TERMINAL_TOKEN_FILE);
        let bob_token = terminal_cli_dir(&bob_home).join(TERMINAL_TOKEN_FILE);
        assert_eq!(std::fs::read(&alice_token).unwrap(), alice);
        assert_eq!(std::fs::read(&bob_token).unwrap(), bob);
        assert_eq!(manager.creds.read_active().unwrap().as_deref(), Some(bob.as_slice()));

        let refreshed = cred_json("alice@example.com", "a2");
        std::fs::write(&alice_token, &refreshed).unwrap();
        assert_eq!(manager.prepare_terminal_home(&alice_id).unwrap(), alice_home);
        assert_eq!(std::fs::read(&alice_token).unwrap(), refreshed);
        std::fs::write(&alice_token, &bob).unwrap();
        assert!(manager.prepare_terminal_home(&alice_id).is_err());
        std::fs::remove_file(&alice_token).unwrap();
        manager.creds.write_saved(&alice_id, "alice", &bob).unwrap();
        assert!(manager.prepare_terminal_home(&alice_id).is_err(), "a stale ID must not import another account's credentials");
        assert!(!alice_token.exists());
        assert!(manager.prepare_terminal_home(&Uuid::new_v4().to_string()).is_err());
        assert!(manager.prepare_terminal_home("../default").is_err());
        manager.creds.delete_saved(&bob_id).unwrap();
        std::fs::remove_file(bob_token).unwrap();
        assert!(manager.prepare_terminal_home(&bob_id).is_err());
        manager.creds.delete_active().unwrap();
        assert!(manager.current_terminal_account_id().is_err(), "do not fall back to the previously active account");
    }

    #[test]
    fn terminal_snapshot_never_overwrites_an_account_with_unidentified_login() {
        let dir = tempfile::tempdir().unwrap();
        let alice = cred_json("alice@example.com", "a1");
        let manager = AccountManager::with_store(
            dir.path().to_path_buf(),
            Box::new(MemoryStore::with_active(&alice)),
        );
        let alice_id = manager.current_terminal_account_id().unwrap();
        manager.creds.write_active(&cred_json("", "unknown-identity")).unwrap();
        assert_ne!(manager.current_terminal_account_id().unwrap(), alice_id);
        assert_eq!(manager.creds.read_saved(&alice_id).unwrap(), Some(alice));
    }

    #[test]
    fn auto_imports_active_and_switches_preserving_refreshed_tokens() {
        let dir = tempfile::tempdir().unwrap();
        let store = std::sync::Arc::new(MemoryStore::with_active(&cred_json("alice@example.com", "a1")));
        struct SharedStore(std::sync::Arc<MemoryStore>);
        impl CredentialStore for SharedStore {
            fn is_supported(&self) -> bool { self.0.is_supported() }
            fn read_active(&self) -> Result<Option<Vec<u8>>, String> { self.0.read_active() }
            fn write_active(&self, blob: &[u8]) -> Result<(), String> { self.0.write_active(blob) }
            fn delete_active(&self) -> Result<(), String> { self.0.delete_active() }
            fn read_saved(&self, id: &str) -> Result<Option<Vec<u8>>, String> { self.0.read_saved(id) }
            fn write_saved(&self, id: &str, u: &str, b: &[u8]) -> Result<(), String> { self.0.write_saved(id, u, b) }
            fn delete_saved(&self, id: &str) -> Result<(), String> { self.0.delete_saved(id) }
        }

        let mgr = AccountManager::with_store(dir.path().to_path_buf(), Box::new(SharedStore(store.clone())));

        // 1. 首次列出自动导入 alice
        let snap1 = mgr.list_and_sync().unwrap();
        assert_eq!(snap1.accounts.len(), 1);
        assert_eq!(snap1.accounts[0].email, "alice@example.com");
        assert_eq!(snap1.accounts[0].label, "alice");
        let alice_id = snap1.accounts[0].id.clone();
        assert_eq!(snap1.active_account_id.as_deref(), Some(alice_id.as_str()));

        // 2. 模拟 agy 刷新了 alice 的 token
        store.write_active(&cred_json("alice@example.com", "a2-refreshed")).unwrap();

        // 3. 登录第二个账号 bob
        let backup = mgr.prepare_interactive_login().unwrap();
        assert!(store.read_active().unwrap().is_none());
        // 验证回滚后再重来
        mgr.rollback_interactive_login(backup);
        assert_eq!(
            store.read_active().unwrap().unwrap(),
            cred_json("alice@example.com", "a2-refreshed")
        );

        let _backup2 = mgr.prepare_interactive_login().unwrap();
        store.write_active(&cred_json("bob@example.com", "b1")).unwrap();
        let snap2 = mgr.commit_interactive_login(None).unwrap();
        assert_eq!(snap2.accounts.len(), 2);
        let bob_id = snap2.active_account_id.clone().unwrap();
        assert_ne!(bob_id, alice_id);
        let alice_dir = snap2.accounts.iter().find(|a| a.id == alice_id).unwrap().app_data_dir.clone();
        let bob_dir = snap2.accounts.iter().find(|a| a.id == bob_id).unwrap().app_data_dir.clone();
        assert!(alice_dir.starts_with("antigravity-acp-"));
        assert!(bob_dir.starts_with("antigravity-acp-"));
        assert_ne!(alice_dir, bob_dir);
        assert_eq!(mgr.active_account_context(), (Some(bob_id.clone()), bob_dir));

        // 4. 一键切回 alice：拿到的必须是刷新后的 a2-refreshed，且上下文目录切回 alice 独立目录
        let snap3 = mgr.activate_account(&alice_id).unwrap();
        assert_eq!(snap3.active_account_id.as_deref(), Some(alice_id.as_str()));
        assert_eq!(mgr.active_account_context(), (Some(alice_id.clone()), alice_dir));
        assert_eq!(
            store.read_active().unwrap().unwrap(),
            cred_json("alice@example.com", "a2-refreshed")
        );

        // 5. 重命名和更新 tier
        let snap4 = mgr.rename_account(&alice_id, "主号").unwrap();
        assert_eq!(snap4.accounts.iter().find(|a| a.id == alice_id).unwrap().label, "主号");
        let snap5 = mgr
            .update_active_tier(Some("Google AI Pro".into()), Some("g1-pro-tier".into()))
            .unwrap();
        assert_eq!(
            snap5.accounts.iter().find(|a| a.id == alice_id).unwrap().tier_name.as_deref(),
            Some("Google AI Pro")
        );

        // 6. 删除当前活跃账号 alice 后刷新不会自动复活
        let snap6 = mgr.delete_account(&alice_id).unwrap();
        assert_eq!(snap6.accounts.len(), 1);
        assert!(snap6.active_account_id.is_none());
        let snap7 = mgr.list_and_sync().unwrap();
        assert_eq!(snap7.accounts.len(), 1);
        assert!(snap7.active_account_id.is_none());

        // 7. 切换到 bob 正常恢复并清除忽略指纹
        let snap8 = mgr.activate_account(&bob_id).unwrap();
        assert_eq!(snap8.active_account_id.as_deref(), Some(bob_id.as_str()));
        assert_eq!(
            store.read_active().unwrap().unwrap(),
            cred_json("bob@example.com", "b1")
        );
    }

    #[test]
    fn duplicate_email_updates_existing_entry_without_duplicating() {
        let dir = tempfile::tempdir().unwrap();
        let store = std::sync::Arc::new(MemoryStore::with_active(&cred_json("Alice@Example.com", "v1")));
        struct SharedStore(std::sync::Arc<MemoryStore>);
        impl CredentialStore for SharedStore {
            fn is_supported(&self) -> bool { self.0.is_supported() }
            fn read_active(&self) -> Result<Option<Vec<u8>>, String> { self.0.read_active() }
            fn write_active(&self, blob: &[u8]) -> Result<(), String> { self.0.write_active(blob) }
            fn delete_active(&self) -> Result<(), String> { self.0.delete_active() }
            fn read_saved(&self, id: &str) -> Result<Option<Vec<u8>>, String> { self.0.read_saved(id) }
            fn write_saved(&self, id: &str, u: &str, b: &[u8]) -> Result<(), String> { self.0.write_saved(id, u, b) }
            fn delete_saved(&self, id: &str) -> Result<(), String> { self.0.delete_saved(id) }
        }
        let mgr = AccountManager::with_store(dir.path().to_path_buf(), Box::new(SharedStore(store.clone())));
        let s1 = mgr.list_and_sync().unwrap();
        assert_eq!(s1.accounts.len(), 1);
        let id = s1.accounts[0].id.clone();

        let _b = mgr.prepare_interactive_login().unwrap();
        store.write_active(&cred_json("alice@example.com", "v2")).unwrap();
        let s2 = mgr.commit_interactive_login(None).unwrap();
        assert_eq!(s2.accounts.len(), 1);
        assert_eq!(s2.accounts[0].id, id);
        assert_eq!(store.read_saved(&id).unwrap().unwrap(), cred_json("alice@example.com", "v2"));
    }

    #[test]
    fn extracts_token_expiry_from_credential_blob_and_tracks_refresh() {
        let dir = tempfile::tempdir().unwrap();
        let initial = serde_json::json!({
            "token": {
                "access_token": "a1",
                "refresh_token": "rt-a1",
                "expiry": "2026-11-25T18:02:34.5970551+08:00"
            },
            "auth_method": "consumer",
            "id_token_email": "alice@example.com"
        })
        .to_string()
        .into_bytes();
        let expected_initial_ms = chrono::DateTime::parse_from_rfc3339("2026-11-25T18:02:34.5970551+08:00")
            .unwrap()
            .timestamp_millis();
        assert_eq!(
            inspect_credential_blob(&initial).token_expiry_ms,
            Some(expected_initial_ms)
        );

        let sec_blob = serde_json::json!({ "expiry": 1_795_597_000i64 }).to_string().into_bytes();
        assert_eq!(
            inspect_credential_blob(&sec_blob).token_expiry_ms,
            Some(1_795_597_000_000)
        );
        let ms_blob = serde_json::json!({ "expiry": 1_795_597_000_123i64 }).to_string().into_bytes();
        assert_eq!(
            inspect_credential_blob(&ms_blob).token_expiry_ms,
            Some(1_795_597_000_123)
        );

        let store = std::sync::Arc::new(MemoryStore::with_active(&initial));
        struct SharedStore(std::sync::Arc<MemoryStore>);
        impl CredentialStore for SharedStore {
            fn is_supported(&self) -> bool { self.0.is_supported() }
            fn read_active(&self) -> Result<Option<Vec<u8>>, String> { self.0.read_active() }
            fn write_active(&self, blob: &[u8]) -> Result<(), String> { self.0.write_active(blob) }
            fn delete_active(&self) -> Result<(), String> { self.0.delete_active() }
            fn read_saved(&self, id: &str) -> Result<Option<Vec<u8>>, String> { self.0.read_saved(id) }
            fn write_saved(&self, id: &str, u: &str, b: &[u8]) -> Result<(), String> { self.0.write_saved(id, u, b) }
            fn delete_saved(&self, id: &str) -> Result<(), String> { self.0.delete_saved(id) }
        }
        let mgr = AccountManager::with_store(dir.path().to_path_buf(), Box::new(SharedStore(store.clone())));
        let (acc_id, _, exp1) = mgr.active_account_context_with_expiry();
        assert!(acc_id.is_some());
        assert_eq!(exp1, Some(expected_initial_ms));

        let refreshed = serde_json::json!({
            "token": {
                "access_token": "a2",
                "refresh_token": "rt-a1",
                "expiry": "2026-11-25T19:02:34.1230000+08:00"
            },
            "auth_method": "consumer",
            "id_token_email": "alice@example.com"
        })
        .to_string()
        .into_bytes();
        let expected_refreshed_ms = chrono::DateTime::parse_from_rfc3339("2026-11-25T19:02:34.1230000+08:00")
            .unwrap()
            .timestamp_millis();
        store.write_active(&refreshed).unwrap();
        let (acc_id2, _, exp2) = mgr.active_account_context_with_expiry();
        assert_eq!(acc_id2, acc_id);
        assert_eq!(exp2, Some(expected_refreshed_ms));
    }

    fn make_shared_store(
        store: std::sync::Arc<MemoryStore>,
    ) -> Box<dyn CredentialStore> {
        struct SharedStore(std::sync::Arc<MemoryStore>);
        impl CredentialStore for SharedStore {
            fn is_supported(&self) -> bool { self.0.is_supported() }
            fn read_active(&self) -> Result<Option<Vec<u8>>, String> { self.0.read_active() }
            fn write_active(&self, blob: &[u8]) -> Result<(), String> { self.0.write_active(blob) }
            fn delete_active(&self) -> Result<(), String> { self.0.delete_active() }
            fn read_saved(&self, id: &str) -> Result<Option<Vec<u8>>, String> { self.0.read_saved(id) }
            fn write_saved(&self, id: &str, u: &str, b: &[u8]) -> Result<(), String> { self.0.write_saved(id, u, b) }
            fn delete_saved(&self, id: &str) -> Result<(), String> { self.0.delete_saved(id) }
        }
        Box::new(SharedStore(store))
    }

    #[test]
    fn selective_export_and_import_into_another_nova_works() {
        let src_dir = tempfile::tempdir().unwrap();
        let dst_dir = tempfile::tempdir().unwrap();
        let bundle_dir = tempfile::tempdir().unwrap();
        let bundle_path = bundle_dir.path().join("accounts.json");

        let src_store = std::sync::Arc::new(MemoryStore::with_active(&cred_json("alice@example.com", "a1")));
        let src_mgr = AccountManager::with_store(src_dir.path().to_path_buf(), make_shared_store(src_store.clone()));
        let s1 = src_mgr.list_and_sync().unwrap();
        let alice_id = s1.accounts[0].id.clone();
        src_mgr.rename_account(&alice_id, "主号").unwrap();
        src_mgr.update_active_tier(Some("Google AI Ultra".into()), Some("g1-ultra".into())).unwrap();

        let _b1 = src_mgr.prepare_interactive_login().unwrap();
        src_store.write_active(&cred_json("bob@example.com", "b1")).unwrap();
        let s2 = src_mgr.commit_interactive_login(None).unwrap();
        let bob_id = s2.active_account_id.clone().unwrap();

        let _b2 = src_mgr.prepare_interactive_login().unwrap();
        src_store.write_active(&cred_json("carol@example.com", "c1")).unwrap();
        let s3 = src_mgr.commit_interactive_login(None).unwrap();
        let carol_id = s3.active_account_id.clone().unwrap();

        // 仅导出 alice 与 carol（3 选 2）
        let export_res = src_mgr
            .export_accounts_to_path(&[alice_id.clone(), carol_id.clone(), alice_id.clone()], &bundle_path)
            .unwrap();
        assert_eq!(export_res.exported_count, 2);
        assert_eq!(export_res.skipped_count, 0);
        assert!(bundle_path.is_file());

        // 导入到另一台全新的 Nova 实例
        let dst_store = std::sync::Arc::new(MemoryStore::default());
        let dst_mgr = AccountManager::with_store(dst_dir.path().to_path_buf(), make_shared_store(dst_store.clone()));
        let import_res = dst_mgr.import_accounts_from_path(&bundle_path).unwrap();
        assert_eq!(import_res.imported_count, 2);
        assert_eq!(import_res.updated_count, 0);
        assert_eq!(import_res.skipped_count, 0);
        assert!(!import_res.active_updated);
        assert_eq!(import_res.snapshot.accounts.len(), 2);
        assert!(import_res.snapshot.active_account_id.is_none());

        let imported_alice = import_res
            .snapshot
            .accounts
            .iter()
            .find(|a| a.email == "alice@example.com")
            .unwrap();
        let imported_carol = import_res
            .snapshot
            .accounts
            .iter()
            .find(|a| a.email == "carol@example.com")
            .unwrap();
        assert_ne!(imported_alice.id, alice_id);
        assert_ne!(imported_carol.id, carol_id);
        assert_ne!(imported_alice.app_data_dir, imported_carol.app_data_dir);
        assert_eq!(imported_alice.label, "主号");
        assert_eq!(imported_alice.tier_name.as_deref(), Some("Google AI Ultra"));
        assert!(import_res.snapshot.accounts.iter().all(|a| a.id != bob_id && a.email != "bob@example.com"));

        // 导入后可立即一键切换并创建独立终端 home
        let activated = dst_mgr.activate_account(&imported_alice.id).unwrap();
        assert_eq!(activated.active_account_id.as_deref(), Some(imported_alice.id.as_str()));
        assert_eq!(dst_store.read_active().unwrap().unwrap(), cred_json("alice@example.com", "a1"));
        let term_home = dst_mgr.prepare_terminal_home(&imported_carol.id).unwrap();
        assert_eq!(
            std::fs::read(terminal_cli_dir(&term_home).join(TERMINAL_TOKEN_FILE)).unwrap(),
            cred_json("carol@example.com", "c1")
        );
    }

    #[test]
    fn export_prefers_freshest_active_and_terminal_tokens() {
        let dir = tempfile::tempdir().unwrap();
        let bundle_dir = tempfile::tempdir().unwrap();
        let bundle_path = bundle_dir.path().join("fresh.json");

        let init_alice = serde_json::json!({
            "token": { "access_token": "a1", "refresh_token": "rt-a1", "expiry": "2026-11-25T18:00:00+08:00" },
            "auth_method": "oauth-personal",
            "id_token_email": "alice@example.com"
        }).to_string().into_bytes();
        let init_bob = serde_json::json!({
            "token": { "access_token": "b1", "refresh_token": "rt-b1", "expiry": "2026-11-25T18:00:00+08:00" },
            "auth_method": "oauth-personal",
            "id_token_email": "bob@example.com"
        }).to_string().into_bytes();

        let store = std::sync::Arc::new(MemoryStore::with_active(&init_alice));
        let mgr = AccountManager::with_store(dir.path().to_path_buf(), make_shared_store(store.clone()));
        let alice_id = mgr.list_and_sync().unwrap().accounts[0].id.clone();

        // 为 alice 准备独立终端凭据并写入更新的 token
        let alice_home = mgr.prepare_terminal_home(&alice_id).unwrap();
        let newer_alice = serde_json::json!({
            "token": { "access_token": "a2-terminal", "refresh_token": "rt-a2", "expiry": "2026-11-25T20:00:00+08:00" },
            "auth_method": "oauth-personal",
            "id_token_email": "alice@example.com"
        }).to_string().into_bytes();
        std::fs::write(terminal_cli_dir(&alice_home).join(TERMINAL_TOKEN_FILE), &newer_alice).unwrap();

        // 登录 bob 为活跃账号，并在活跃槽写入刷新后的 bob token
        let _b = mgr.prepare_interactive_login().unwrap();
        store.write_active(&init_bob).unwrap();
        let bob_id = mgr.commit_interactive_login(None).unwrap().active_account_id.unwrap();
        let newer_bob = serde_json::json!({
            "token": { "access_token": "b2-active", "refresh_token": "rt-b2", "expiry": "2026-11-25T21:00:00+08:00" },
            "auth_method": "oauth-personal",
            "id_token_email": "bob@example.com"
        }).to_string().into_bytes();
        store.write_active(&newer_bob).unwrap();

        let res = mgr.export_accounts_to_path(&[alice_id, bob_id], &bundle_path).unwrap();
        assert_eq!(res.exported_count, 2);

        let bundle: AccountExportBundle = serde_json::from_slice(&std::fs::read(&bundle_path).unwrap()).unwrap();
        let alice_blob = B64_STANDARD.decode(&bundle.accounts[0].credential_b64).unwrap();
        let bob_blob = B64_STANDARD.decode(&bundle.accounts[1].credential_b64).unwrap();
        assert_eq!(alice_blob, newer_alice);
        assert_eq!(bob_blob, newer_bob);
    }

    #[test]
    fn import_updates_existing_active_clears_ignored_and_stale_terminal_token() {
        let dir = tempfile::tempdir().unwrap();
        let bundle_dir = tempfile::tempdir().unwrap();
        let bundle_path = bundle_dir.path().join("update.json");

        let store = std::sync::Arc::new(MemoryStore::with_active(&cred_json("Alice@Example.com", "old-a")));
        let mgr = AccountManager::with_store(dir.path().to_path_buf(), make_shared_store(store.clone()));
        let s1 = mgr.list_and_sync().unwrap();
        let alice_id = s1.accounts[0].id.clone();
        mgr.rename_account(&alice_id, "本地主号").unwrap();
        let alice_home = mgr.prepare_terminal_home(&alice_id).unwrap();
        let alice_term_token = terminal_cli_dir(&alice_home).join(TERMINAL_TOKEN_FILE);
        assert!(alice_term_token.is_file());

        // 登录 bob 再删除 bob，使其处于 ignored_fingerprint
        let _b = mgr.prepare_interactive_login().unwrap();
        store.write_active(&cred_json("bob@example.com", "old-b")).unwrap();
        let bob_id = mgr.commit_interactive_login(None).unwrap().active_account_id.unwrap();
        mgr.delete_account(&bob_id).unwrap();

        // 切回 alice 作为活跃账号
        mgr.activate_account(&alice_id).unwrap();

        // 构造包含新版 alice 与 bob 的导出包
        let new_alice = cred_json("alice@example.com", "new-a");
        let new_bob = cred_json("bob@example.com", "new-b");
        let bundle = AccountExportBundle {
            format: EXPORT_BUNDLE_FORMAT.into(),
            version: EXPORT_BUNDLE_VERSION,
            exported_at: now_ms(),
            accounts: vec![
                ExportedAccountEntry {
                    email: "alice@example.com".into(),
                    label: "远端名称".into(),
                    auth_method: "oauth-personal".into(),
                    tier_name: Some("Pro".into()),
                    tier_id: None,
                    credential_b64: B64_STANDARD.encode(&new_alice),
                },
                ExportedAccountEntry {
                    email: "bob@example.com".into(),
                    label: "Bob号".into(),
                    auth_method: "oauth-personal".into(),
                    tier_name: None,
                    tier_id: None,
                    credential_b64: B64_STANDARD.encode(&new_bob),
                },
            ],
        };
        std::fs::write(&bundle_path, serde_json::to_string_pretty(&bundle).unwrap()).unwrap();

        let res = mgr.import_accounts_from_path(&bundle_path).unwrap();
        assert_eq!(res.imported_count, 1);
        assert_eq!(res.updated_count, 1);
        assert!(res.active_updated);
        assert_eq!(res.snapshot.accounts.len(), 2);

        // alice 保留原有本地 ID 与自定义备注，活跃槽和已保存槽同步更新，旧终端 token 被清理
        let updated_alice = res.snapshot.accounts.iter().find(|a| a.id == alice_id).unwrap();
        assert_eq!(updated_alice.label, "本地主号");
        assert_eq!(updated_alice.tier_name.as_deref(), Some("Pro"));
        assert_eq!(store.read_saved(&alice_id).unwrap().unwrap(), new_alice);
        assert_eq!(store.read_active().unwrap().unwrap(), new_alice);
        assert!(!alice_term_token.exists());

        // bob 解除 ignored_fingerprint 并成功入库
        assert!(mgr.load_file().ignored_fingerprint.is_none());
        let bob_acc = res.snapshot.accounts.iter().find(|a| a.email == "bob@example.com").unwrap();
        assert_eq!(bob_acc.label, "Bob号");
    }

    #[test]
    fn export_and_import_validate_errors_and_skip_invalid_entries() {
        let dir = tempfile::tempdir().unwrap();
        let bundle_dir = tempfile::tempdir().unwrap();
        let store = std::sync::Arc::new(MemoryStore::with_active(&cred_json("alice@example.com", "a1")));
        let mgr = AccountManager::with_store(dir.path().to_path_buf(), make_shared_store(store.clone()));
        let alice_id = mgr.list_and_sync().unwrap().accounts[0].id.clone();

        // 空选择 / 非 json 路径
        assert!(mgr.export_accounts_to_path(&[], &bundle_dir.path().join("a.json")).is_err());
        assert!(mgr.export_accounts_to_path(&[alice_id.clone()], &bundle_dir.path().join("a.txt")).is_err());

        // 缺失凭据跳过，若全部缺失则返回错误
        store.delete_saved(&alice_id).unwrap();
        store.delete_active().unwrap();
        assert!(mgr.export_accounts_to_path(&[alice_id.clone()], &bundle_dir.path().join("a.json")).is_err());

        // 损坏 JSON / 错误 format / 高版本
        let bad_json = bundle_dir.path().join("bad.json");
        std::fs::write(&bad_json, "not-json").unwrap();
        assert!(mgr.import_accounts_from_path(&bad_json).is_err());

        let bad_format = bundle_dir.path().join("bad-format.json");
        std::fs::write(&bad_format, r#"{"format":"other","version":1,"accounts":[]}"#).unwrap();
        assert!(mgr.import_accounts_from_path(&bad_format).is_err());

        let bad_ver = bundle_dir.path().join("bad-ver.json");
        std::fs::write(
            &bad_ver,
            format!(r#"{{"format":"{EXPORT_BUNDLE_FORMAT}","version":99,"accounts":[]}}"#),
        ).unwrap();
        assert!(mgr.import_accounts_from_path(&bad_ver).is_err());

        // 混合有效与无效条目：跳过无效，导入有效
        let mixed = bundle_dir.path().join("mixed.json");
        let valid_blob = cred_json("carol@example.com", "c1");
        let bundle = AccountExportBundle {
            format: EXPORT_BUNDLE_FORMAT.into(),
            version: EXPORT_BUNDLE_VERSION,
            exported_at: now_ms(),
            accounts: vec![
                ExportedAccountEntry {
                    email: "bad@example.com".into(),
                    label: "坏条目".into(),
                    auth_method: "oauth-personal".into(),
                    tier_name: None,
                    tier_id: None,
                    credential_b64: "!!!not-base64!!!".into(),
                },
                ExportedAccountEntry {
                    email: "carol@example.com".into(),
                    label: "Carol".into(),
                    auth_method: "oauth-personal".into(),
                    tier_name: None,
                    tier_id: None,
                    credential_b64: B64_STANDARD.encode(&valid_blob),
                },
            ],
        };
        std::fs::write(&mixed, serde_json::to_string(&bundle).unwrap()).unwrap();
        let res = mgr.import_accounts_from_path(&mixed).unwrap();
        assert_eq!(res.imported_count, 1);
        assert_eq!(res.skipped_count, 1);
        assert_eq!(res.warnings.len(), 1);
    }
}
