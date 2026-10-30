//! 密码本：`~/.nova/vault.json` 保存自由文本凭据（账号密码、密钥等），每条都带“使用条件”。
//! 凭据落盘时用本机随机密钥（`vault.key`）AES-256-GCM 加密，员工读文件也只看到密文，只能经 `employee` 工具的 vault_* 取用。
//! 远程员工要本机密码本里的凭据时，用 X25519 密钥协商加密传输（中转站只见密文）；收到的凭据只作为临时条目存放，任务结束即删。
//! shortcut: 密钥文件与密文同在 ~/.nova，防的是误读与误传而非本机恶意程序；要更强再换系统凭据库（DPAPI / Keychain）。
//! shortcut: 密钥协商没有身份认证，信任中转站不换公钥；要防恶意中转站就得带外核对指纹。

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use x25519_dalek::{PublicKey, StaticSecret};

const MAX_ENTRIES: usize = 200;
const MAX_FIELD_CHARS: usize = 500;
const MAX_CONTENT_CHARS: usize = 16_000;
/// 收到的临时凭据超过这么久还在（任务没正常收尾）就清掉。
const TEMP_TTL_MS: i64 = 24 * 3600 * 1000;

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
struct Entry {
    id: String,
    name: String,
    /// 使用条件：什么情况下员工才可以用这条凭据（必填）。
    condition: String,
    /// 兼容旧条目的账号；新凭据整体加密存在 secret 中。
    #[serde(skip_serializing_if = "String::is_empty")]
    username: String,
    /// 凭据内容密文（base64 的 nonce‖密文）。
    secret: String,
    /// 允许经 StarLink 加密发给远程员工。
    share: bool,
    /// 非空：远程员工经加密传来的临时凭据，值是为哪条待办收的；该任务收尾时删除。
    temp_for: String,
    created_at: i64,
    used_at: i64,
    /// 最近一次取用的用途。
    used_for: String,
}

#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
struct Book {
    entries: Vec<Entry>,
    /// 向对方请求凭据时生成的临时私钥（本机密钥加密），按请求（待办）id 存放。
    requests: HashMap<String, String>,
}

static LOCK: Mutex<()> = Mutex::new(());

fn dir() -> PathBuf {
    crate::lyra::config::nova_root()
}

fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn text(v: &Value, key: &str, max: usize) -> Result<String, String> {
    let s = v[key].as_str().map(str::trim).unwrap_or_default();
    if s.chars().count() > max { return Err(format!("{key} 太长（最多 {max} 字）")); }
    Ok(s.to_string())
}

// ---------- 加解密 ----------

fn encrypt(key: &[u8; 32], aad: &[u8], plain: &[u8]) -> Result<String, String> {
    let mut nonce = [0u8; 12];
    OsRng.fill_bytes(&mut nonce);
    let ct = Aes256Gcm::new(key.into()).encrypt(Nonce::from_slice(&nonce), Payload { msg: plain, aad }).map_err(|_| "加密失败")?;
    Ok(B64.encode([nonce.as_slice(), &ct].concat()))
}

fn decrypt(key: &[u8; 32], aad: &[u8], blob: &str) -> Result<Vec<u8>, String> {
    let raw = B64.decode(blob).ok().filter(|r| r.len() > 12).ok_or("密文格式不对")?;
    let (nonce, ct) = raw.split_at(12);
    Aes256Gcm::new(key.into()).decrypt(Nonce::from_slice(nonce), Payload { msg: ct, aad }).map_err(|_| "解密失败（密钥不对或内容被改动）".into())
}

fn shared_key(secret: [u8; 32], pk: [u8; 32]) -> [u8; 32] {
    let shared = StaticSecret::from(secret).diffie_hellman(&PublicKey::from(pk));
    Sha256::new().chain_update(b"nova-vault-v1").chain_update(shared.as_bytes()).finalize().into()
}

fn b32(s: &str) -> Result<[u8; 32], String> {
    B64.decode(s).ok().and_then(|b| b.try_into().ok()).ok_or_else(|| "公钥格式不对".into())
}

/// 本机密钥：首次使用时随机生成。
fn local_key() -> Result<[u8; 32], String> {
    let path = dir().join("vault.key");
    if let Ok(raw) = std::fs::read_to_string(&path) {
        return b32(raw.trim()).map_err(|_| "vault.key 损坏：删除它会让已存的凭据无法解密".into());
    }
    let mut key = [0u8; 32];
    OsRng.fill_bytes(&mut key);
    std::fs::create_dir_all(dir()).and_then(|_| std::fs::write(&path, B64.encode(key))).map_err(|e| format!("写入 vault.key 失败：{e}"))?;
    Ok(key)
}

// ---------- 存取 ----------

fn path() -> PathBuf {
    dir().join("vault.json")
}

fn load() -> Result<Book, String> {
    match std::fs::read_to_string(path()) {
        Ok(raw) => serde_json::from_str(&raw).map_err(|e| format!("vault.json 解析失败：{e}")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Book::default()),
        Err(e) => Err(format!("读取 vault.json 失败：{e}")),
    }
}

/// 读改写一次，写入走临时文件再替换，避免半截文件。
fn update<T>(f: impl FnOnce(&mut Book) -> Result<T, String>) -> Result<T, String> {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut book = load()?;
    let out = f(&mut book)?;
    let tmp = path().with_extension("json.tmp");
    let json = serde_json::to_string_pretty(&book).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(dir()).and_then(|_| std::fs::write(&tmp, json)).and_then(|_| std::fs::rename(&tmp, path()))
        .map_err(|e| format!("保存 vault.json 失败：{e}"))?;
    Ok(out)
}

/// 给界面和模型看的视图：永远不含凭据内容。
fn view(e: &Entry) -> Value {
    json!({"id": e.id, "name": e.name, "condition": e.condition, "share": e.share,
        "temp": !e.temp_for.is_empty(), "createdAt": e.created_at, "usedAt": e.used_at, "usedFor": e.used_for})
}

fn decrypt_content(e: &Entry, key: &[u8; 32]) -> Result<String, String> {
    let content = String::from_utf8(decrypt(key, b"vault", &e.secret)?).map_err(|_| "凭据内容损坏")?;
    Ok(if e.username.is_empty() { content } else { format!("账号：{}\n密码：{content}", e.username) })
}

pub(crate) fn list() -> Result<Vec<Value>, String> {
    let t = now();
    // 过期的临时凭据顺手清掉。
    if load()?.entries.iter().any(|e| !e.temp_for.is_empty() && t - e.created_at > TEMP_TTL_MS) {
        update(|b| { b.entries.retain(|e| e.temp_for.is_empty() || t - e.created_at <= TEMP_TTL_MS); Ok(()) })?;
    }
    Ok(load()?.entries.iter().map(view).collect())
}

/// 新增或修改（带 id）：name / condition 必填；修改时 content 留空表示不改。
pub(crate) fn save(args: &Value) -> Result<Value, String> {
    let name = text(args, "name", 100)?;
    let condition = text(args, "condition", MAX_FIELD_CHARS)?;
    if name.is_empty() { return Err("缺少 name（这条凭据叫什么、用在哪）".into()); }
    if condition.is_empty() { return Err("缺少 condition：每条凭据都要写明什么情况下员工才可以使用".into()); }
    let content = match args.get("content") {
        None => "",
        Some(value) => value.as_str().ok_or("content 必须为文本")?,
    };
    if content.chars().count() > MAX_CONTENT_CHARS { return Err(format!("content 太长（最多 {MAX_CONTENT_CHARS} 字）")); }
    let share = args["share"].as_bool();
    let key = local_key()?;
    let id = args["id"].as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
    update(|b| {
        let secret = if content.is_empty() { None } else { Some(encrypt(&key, b"vault", content.as_bytes())?) };
        if let Some(id) = &id {
            let e = b.entries.iter_mut().find(|e| e.id == *id).ok_or("没有这条凭据")?;
            (e.name, e.condition) = (name, condition);
            if let Some(s) = secret { e.secret = s; e.username.clear(); }
            if let Some(v) = share { e.share = v; }
            return Ok(json!({"ok": true, "id": id}));
        }
        let secret = secret.ok_or("新增凭据需要 content")?;
        if b.entries.len() >= MAX_ENTRIES { return Err(format!("密码本最多 {MAX_ENTRIES} 条")); }
        let id = format!("v{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
        b.entries.push(Entry { id: id.clone(), name, condition, secret, share: share.unwrap_or(false), created_at: now(), ..Default::default() });
        Ok(json!({"ok": true, "id": id}))
    })
}

pub(crate) fn delete(id: &str) -> Result<(), String> {
    update(|b| {
        let before = b.entries.len();
        b.entries.retain(|e| e.id != id);
        if before == b.entries.len() { Err("没有这条凭据".into()) } else { Ok(()) }
    })
}

/// 员工取用：返回凭据内容，同时记下用途。使用条件是否满足由调用方（员工）按条件原文判断，随结果一并带回。
pub(crate) fn get(id: &str, purpose: &str) -> Result<Value, String> {
    let key = local_key()?;
    update(|b| {
        let e = b.entries.iter_mut().find(|e| e.id == id).ok_or("没有这条凭据，先 vault_list 查看")?;
        let content = decrypt_content(e, &key)?;
        e.used_at = now();
        e.used_for = purpose.chars().take(100).collect();
        Ok(json!({"name": e.name, "condition": e.condition, "content": content,
            "next": "仅当当前这件事符合上面的使用条件才可以使用；凭据内容只用于当前授权操作，不要写进回复、备注、记忆或交接说明。"}))
    })
}

// ---------- 跨机传输 ----------

/// 请求方：为一次请求（待办 id）生成临时密钥对，私钥加密存本机，返回公钥（base64）发给对方。
pub(crate) fn new_request(req_id: &str) -> Result<String, String> {
    let secret = StaticSecret::random_from_rng(OsRng);
    let sealed = encrypt(&local_key()?, req_id.as_bytes(), secret.as_bytes())?;
    update(|b| { b.requests.insert(req_id.into(), sealed); Ok(()) })?;
    Ok(B64.encode(PublicKey::from(&secret).as_bytes()))
}

/// 公钥格式合法才算数（对方传来的不可信）。
pub(crate) fn valid_pk(pk: &str) -> bool {
    b32(pk).is_ok()
}

/// 响应方：把一条允许共享的凭据用对方公钥加密，返回（条目名，可传输的密文）。
pub(crate) fn seal_for(id: &str, req_id: &str, their_pk: &str) -> Result<(String, Value), String> {
    let their = b32(their_pk)?;
    let key = local_key()?;
    let entry = load()?.entries.into_iter().find(|e| e.id == id).ok_or("没有这条凭据")?;
    if !entry.share { return Err(format!("「{}」没有设为允许发给远程员工，不能发送", entry.name)); }
    if !entry.temp_for.is_empty() { return Err("临时凭据不能再转发".into()); }
    let content = decrypt_content(&entry, &key)?;
    let eph = StaticSecret::random_from_rng(OsRng);
    let k = shared_key(eph.to_bytes(), their);
    // 保留旧传输字段名，让旧版本接收端也能收到完整内容。
    let plain = json!({"name": entry.name, "password": content}).to_string();
    let sealed = json!({"epk": B64.encode(PublicKey::from(&eph).as_bytes()), "data": encrypt(&k, req_id.as_bytes(), plain.as_bytes())?});
    update(|b| {
        if let Some(e) = b.entries.iter_mut().find(|e| e.id == id) { e.used_at = now(); e.used_for = "加密发给远程员工".into(); }
        Ok(())
    })?;
    Ok((entry.name, sealed))
}

/// 请求方收到密文：解开存成临时条目（只许该任务用），返回（条目 id，名称）。
pub(crate) fn receive(req_id: &str, task: &str, sealed: &Value) -> Result<(String, String), String> {
    let key = local_key()?;
    let mine = load()?.requests.get(req_id).cloned().ok_or("没有发出过这个凭据请求")?;
    let secret: [u8; 32] = decrypt(&key, req_id.as_bytes(), &mine)?.try_into().map_err(|_| "私钥损坏")?;
    let k = shared_key(secret, b32(sealed["epk"].as_str().unwrap_or_default())?);
    let plain: Value = serde_json::from_slice(&decrypt(&k, req_id.as_bytes(), sealed["data"].as_str().unwrap_or_default())?).map_err(|_| "凭据内容损坏")?;
    let name = plain["name"].as_str().unwrap_or("凭据").to_string();
    let id = format!("v{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
    let entry = Entry {
        id: id.clone(), name: format!("{name}（对方传来）"), username: plain["username"].as_str().unwrap_or_default().into(),
        condition: format!("仅限任务「{task}」使用，任务结束即删除"),
        secret: encrypt(&key, b"vault", plain["password"].as_str().unwrap_or_default().as_bytes())?,
        temp_for: req_id.into(), created_at: now(), ..Default::default()
    };
    update(|b| { b.requests.remove(req_id); b.entries.push(entry); Ok(()) })?;
    Ok((id, name))
}

/// 任务收尾：删掉它的临时凭据与未用的临时私钥。
pub(crate) fn drop_temp(req_id: &str) {
    // 绝大多数任务没碰过密码本：不为它们创建或改写文件。
    if !load().is_ok_and(|b| b.requests.contains_key(req_id) || b.entries.iter().any(|e| e.temp_for == req_id)) { return; }
    if let Err(error) = update(|b| {
        b.requests.remove(req_id);
        b.entries.retain(|e| e.temp_for != req_id);
        Ok(())
    }) {
        eprintln!("[vault] drop temp failed: {error}");
    }
}

// ---------- 前端命令 ----------

#[tauri::command]
pub fn vault_list() -> Result<Vec<Value>, String> {
    list()
}

#[tauri::command]
pub fn vault_save(entry: Value) -> Result<Value, String> {
    save(&entry)
}

#[tauri::command]
pub fn vault_delete(id: String) -> Result<(), String> {
    delete(&id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sealed_credentials_open_only_with_the_requesters_key_and_request_id() {
        let requester = StaticSecret::random_from_rng(OsRng);
        let eph = StaticSecret::random_from_rng(OsRng);
        let send = shared_key(eph.to_bytes(), PublicKey::from(&requester).to_bytes());
        let recv = shared_key(requester.to_bytes(), PublicKey::from(&eph).to_bytes());
        assert_eq!(send, recv);
        let sealed = encrypt(&send, b"t1", b"hunter2").unwrap();
        assert!(!sealed.contains("hunter2"));
        assert_eq!(decrypt(&recv, b"t1", &sealed).unwrap(), b"hunter2");
        assert!(decrypt(&recv, b"t2", &sealed).is_err(), "绑定请求 id，不能换到别的请求上");
        let other = shared_key(StaticSecret::random_from_rng(OsRng).to_bytes(), PublicKey::from(&eph).to_bytes());
        assert!(decrypt(&other, b"t1", &sealed).is_err());
    }

    #[test]
    fn freeform_content_preserves_whitespace_and_legacy_accounts() {
        let key = [7u8; 32];
        let content = "  -----BEGIN PRIVATE KEY-----\n密钥内容\n-----END PRIVATE KEY-----\n";
        let e = Entry { secret: encrypt(&key, b"vault", content.as_bytes()).unwrap(), ..Default::default() };
        assert_eq!(decrypt_content(&e, &key).unwrap(), content);
        let legacy: Entry = serde_json::from_value(json!({"username": "alice", "secret": encrypt(&key, b"vault", b" p ").unwrap()})).unwrap();
        assert_eq!(decrypt_content(&legacy, &key).unwrap(), "账号：alice\n密码： p ");
        assert!(decrypt_content(&e, &[8u8; 32]).is_err());
    }

    #[test]
    fn entries_never_expose_content_and_validate_inputs() {
        let e = Entry { id: "v1".into(), name: "n".into(), condition: "c".into(), username: "alice".into(), secret: "xxx".into(), ..Default::default() };
        let listed = view(&e);
        assert!(listed.get("username").is_none() && listed.get("content").is_none() && listed.get("secret").is_none());
        assert!(!listed.to_string().contains("xxx"));
        assert!(save(&json!({"name": "n", "content": "p"})).is_err());
        assert!(save(&json!({"name": "n", "condition": "c", "content": 123})).unwrap_err().contains("content 必须为文本"));
        assert!(save(&json!({"name": "n", "condition": "c", "content": "x".repeat(MAX_CONTENT_CHARS + 1)})).unwrap_err().contains("content 太长"));
        assert!(valid_pk(&B64.encode([7u8; 32])) && !valid_pk("abc"));
    }
}
