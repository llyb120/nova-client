//! Local-only diagnostics; never feed these records back to the model or frontend events.
use crate::{lyra::config::ResolvedModel, threads::Thread, AppState};
use serde_json::{json, Value};
use std::{fs, io::{Cursor, Read, Write}, path::{Path, PathBuf}, sync::{Mutex, OnceLock}};

tokio::task_local! {
    pub(crate) static SESSION: Option<(PathBuf, String)>;
}

fn sensitive_key(key: &str) -> bool {
    let key: String = key.chars().filter(|c| c.is_ascii_alphanumeric()).flat_map(char::to_lowercase).collect();
    matches!(key.as_str(), "apikey" | "authorization" | "proxyauthorization" | "cookie" | "setcookie"
        | "password" | "passwd" | "accesstoken" | "refreshtoken" | "idtoken" | "token" | "secret"
        | "clientsecret" | "privatekey" | "roamingpeer")
}

fn redact_text(text: &str, secrets: &[&str]) -> String {
    let mut text = text.to_owned();
    for secret in secrets.iter().filter(|s| !s.is_empty()) {
        text = text.replace(secret, "[REDACTED]");
    }
    static AUTH: OnceLock<regex::Regex> = OnceLock::new();
    let auth = AUTH.get_or_init(|| regex::Regex::new(r"(?i)\b(Bearer|Basic)\s+[A-Za-z0-9._~+/=-]+").unwrap());
    text = auth.replace_all(&text, "$1 [REDACTED]").into_owned();
    static FIELD: OnceLock<regex::Regex> = OnceLock::new();
    let field = FIELD.get_or_init(|| regex::Regex::new(
        r#"(?i)(\b(?:api[_-]?key|access[_-]?token|refresh[_-]?token|password|client[_-]?secret|token)\b["']?\s*[:=]\s*["']?)[^\s"'&,;}]+"#,
    ).unwrap());
    field.replace_all(&text, "${1}[REDACTED]").into_owned()
}

fn redact(value: &mut Value, secrets: &[&str]) {
    match value {
        Value::Object(fields) => for (key, value) in fields {
            if sensitive_key(key) { *value = json!("[REDACTED]"); } else { redact(value, secrets); }
        },
        Value::Array(items) => for item in items { redact(item, secrets); },
        Value::String(text) => *text = redact_text(text, secrets),
        _ => {}
    }
}

pub(crate) fn record(model: &ResolvedModel, key: &str, kind: &str, mut data: Value) {
    let _ = SESSION.try_with(|scope| {
        let Some((root, session_id)) = scope else { return };
        if !crate::valid_lyra_session_id(session_id) { return; }
        let mut secrets = vec![key];
        secrets.extend(model.headers.values().filter_map(Value::as_str));
        redact(&mut data, &secrets);
        let text = data.to_string();
        if text.len() > 128 * 1024 {
            let mut end = 128 * 1024;
            while !text.is_char_boundary(end) { end -= 1; }
            data = json!({"preview": &text[..end], "truncated": true});
        }
        let entry = json!({"timestamp": chrono::Utc::now().to_rfc3339(), "sessionId": session_id,
            "provider": model.provider, "model": model.id, "api": model.api, "kind": kind, "data": data});
        static WRITE_LOCK: Mutex<()> = Mutex::new(());
        let _guard = WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let result = (|| -> std::io::Result<()> {
            fs::create_dir_all(root)?;
            let mut options = fs::OpenOptions::new();
            options.create(true).append(true);
            #[cfg(unix)]
            { use std::os::unix::fs::OpenOptionsExt; options.mode(0o600); }
            let mut file = options.open(root.join(format!("{session_id}.errors.jsonl")))?;
            file.write_all(format!("{entry}\n").as_bytes())?;
            file.flush()
        })();
        if let Err(error) = result { eprintln!("[diagnostics] 无法保存会话错误日志：{error}"); }
    });
}

#[tauri::command]
pub(crate) async fn export_thread_diagnostics(
    app: tauri::AppHandle, state: tauri::State<'_, AppState>, thread_id: String, path: String,
) -> Result<(), String> {
    let path = PathBuf::from(path);
    if !path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("zip")) {
        return Err("请选择 .zip 文件保存诊断包".into());
    }
    let thread = state.store.lock().unwrap().get(&thread_id).cloned().ok_or("会话不存在")?;
    let root = state.config_dir.join("alkaid").join("sessions");
    let version = app.package_info().version.to_string();
    tauri::async_runtime::spawn_blocking(move || {
        let bytes = bundle(&root, &thread, &version)?;
        crate::thread_storage::atomic_write(&path, &bytes)
    }).await.map_err(|e| e.to_string())?
}

fn bundle(root: &Path, thread: &Thread, version: &str) -> Result<Vec<u8>, String> {
    // shortcut: 在内存中组包，超过 256 MiB 时拒绝；需要更大诊断包时改为流式临时文件。
    const MAX: u64 = 256 * 1024 * 1024;
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let mut total = 0u64;
    let mut entries = Vec::new();
    let mut missing = Vec::new();
    let mut warnings = Vec::new();
    let mut add = |name: &str, bytes: &[u8]| -> Result<(), String> {
        total += bytes.len() as u64;
        if total > MAX { return Err("诊断包内容超过 256 MiB，未写入目标文件".into()); }
        zip.start_file(name, zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated))
            .map_err(|e| e.to_string())?;
        zip.write_all(bytes).map_err(|e| e.to_string())?;
        entries.push(name.to_owned());
        Ok(())
    };
    let mut value = serde_json::to_value(thread).map_err(|e| e.to_string())?;
    redact(&mut value, &[]);
    add("thread.json", &serde_json::to_vec_pretty(&value).map_err(|e| e.to_string())?)?;
    if let Some(id) = &thread.acp_session_id {
        if !crate::valid_lyra_session_id(id) { return Err("会话 ID 无效，拒绝读取会话目录外文件".into()); }
        for suffix in ["jsonl", "context.json", "errors.jsonl", "json", "slim.json", "pending.json"] {
            let name = format!("{id}.{suffix}");
            let path = root.join(&name);
            match fs::symlink_metadata(&path) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => { missing.push(name); continue; }
                Err(e) => return Err(format!("读取 {name} 失败：{e}")),
                Ok(meta) if !meta.is_file() || meta.file_type().is_symlink() => return Err(format!("拒绝导出非普通会话文件：{name}")),
                Ok(_) => {}
            }
            let mut text = String::new();
            fs::File::open(&path).and_then(|file| file.take(MAX + 1).read_to_string(&mut text))
                .map_err(|e| format!("读取 {name} 失败：{e}"))?;
            if text.len() as u64 > MAX { return Err(format!("{name} 超过 256 MiB，未写入目标文件")); }
            let mut sanitize = |text: &str| -> String {
                match serde_json::from_str::<Value>(text) {
                    Ok(mut value) => { redact(&mut value, &[]); value.to_string() }
                    Err(_) => {
                        if !warnings.contains(&name) { warnings.push(name.clone()); }
                        redact_text(text, &[])
                    }
                }
            };
            let content = if suffix.ends_with("jsonl") {
                text.lines().map(&mut sanitize).collect::<Vec<_>>().join("\n") + "\n"
            } else { sanitize(&text) };
            add(&format!("sessions/{name}"), content.as_bytes())?;
        }
    }
    drop(add);
    let manifest = json!({"formatVersion":1, "exportedAt":chrono::Utc::now().to_rfc3339(),
        "appVersion": version, "platform": std::env::consts::OS,
        "threadId":thread.id, "sessionId":thread.acp_session_id, "files":entries,
        "missingFiles":missing, "malformedFilesPreserved":warnings,
        "notes":["当前会话本地快照；运行中的输出和上下文可能尚未落盘，各文件不是跨文件原子快照。",
            "thread.json 含完整展示记录（含工具结果），sessions 含规范历史、上下文投影和本地错误日志。",
            "缺失文件可能尚未生成；旧版本未保存的错误原文无法补回。",
            "不包含全局配置、密码本、其他会话、工作目录文件或外部附件/工具归档文件。",
            "已遮盖常见凭据字段和认证文本，但对话、代码、截图及上游错误仍可能含敏感信息，分享前请检查。"]});
    zip.start_file("manifest.json", zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated))
        .map_err(|e| e.to_string())?;
    zip.write_all(&serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    zip.finish().map(|cursor| cursor.into_inner()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn diagnostics_are_session_local_and_redacted() {
        let dir = tempfile::tempdir().unwrap();
        let model = crate::lyra::config::resolve_model(&json!({"provider":{"p":{"api":"openai-completions",
            "options":{"baseURL":"https://example.invalid","apiKey":"fixture-key"},"models":{"m":{}}}}}),
            Some("p/m"), &std::collections::HashMap::new()).unwrap().model;
        record(&model, "fixture-key", "outside_scope", json!({}));
        SESSION.scope(Some((dir.path().to_owned(), "lyra-test".into())), async {
            for attempt in 0..2 {
                record(&model, "fixture-key", "provider_error", json!({"attempt":attempt,
                    "message":"quota exhausted fixture-key", "details":{"retryAfter":60,"apiKey":"secret-value"}}));
            }
        }).await;
        let text = fs::read_to_string(dir.path().join("lyra-test.errors.jsonl")).unwrap();
        assert_eq!(text.lines().count(), 2);
        assert!(text.contains("quota exhausted") && text.contains("retryAfter"));
        assert!(!text.contains("fixture-key") && !text.contains("secret-value") && !text.contains("outside_scope"));
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn zip_contains_full_thread_context_and_only_its_logs() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("lyra-test.jsonl"), "{\"role\":\"user\",\"content\":\"hello\"}\n").unwrap();
        fs::write(dir.path().join("lyra-test.context.json"), r#"{"projection":[{"text":"summary"}],"generation":2}"#).unwrap();
        fs::write(dir.path().join("lyra-test.errors.jsonl"), "{\"message\":\"quota\",\"token\":\"hidden\"}\n{broken\n").unwrap();
        fs::write(dir.path().join("other.errors.jsonl"), "private").unwrap();
        let mut thread = Thread::new(".".into(), crate::AgentKind::Lyra, None, None, None, false);
        thread.acp_session_id = Some("lyra-test".into());
        thread.roaming_peer = Some("private-peer-token".into());
        let mut zip = zip::ZipArchive::new(Cursor::new(bundle(dir.path(), &thread, "test").unwrap())).unwrap();
        let names: Vec<_> = zip.file_names().map(|name| name.unwrap().into_owned()).collect();
        assert!(names.contains(&"thread.json".into()));
        assert!(names.contains(&"sessions/lyra-test.context.json".into()));
        assert!(!names.iter().any(|name| name.contains("other")));
        let mut text = String::new();
        zip.by_name("sessions/lyra-test.errors.jsonl").unwrap().read_to_string(&mut text).unwrap();
        assert!(text.contains("quota") && text.contains("{broken") && !text.contains("hidden"));
        text.clear();
        zip.by_name("thread.json").unwrap().read_to_string(&mut text).unwrap();
        assert!(!text.contains("private-peer-token"));
        text.clear();
        zip.by_name("manifest.json").unwrap().read_to_string(&mut text).unwrap();
        let manifest: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(manifest["malformedFilesPreserved"], json!(["lyra-test.errors.jsonl"]));
        assert!(manifest["missingFiles"].as_array().unwrap().contains(&json!("lyra-test.pending.json")));
        thread.acp_session_id = Some("../private".into());
        assert!(bundle(dir.path(), &thread, "test").unwrap_err().contains("ID 无效"));
    }

    #[test]
    fn common_auth_text_is_redacted_without_hiding_error_details() {
        let text = redact_text("429 token=abc&retry=60 Authorization: Bearer private-token password: hidden", &[]);
        assert!(!text.contains("abc") && !text.contains("private-token") && !text.contains("hidden"));
        assert!(text.contains("429") && text.contains("retry=60"));
    }
}
