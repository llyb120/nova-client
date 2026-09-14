use base64::Engine;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Config {
    #[serde(rename = "baseURL", alias = "baseUrl")]
    base_url: String,
    api_key: String,
    models: Vec<String>,
}

fn load_config(path: &Path) -> Result<Config, String> {
    let text = std::fs::read_to_string(path).map_err(|_| "尚未配置图片服务，请先使用 /setup-image")?;
    let config: Config = serde_json::from_str(&text).map_err(|_| "图片配置格式无效，请使用 /setup-image 修正")?;
    let url = reqwest::Url::parse(&config.base_url).map_err(|_| "图片 API 地址无效")?;
    if !matches!(url.scheme(), "http" | "https") || config.api_key.trim().is_empty()
        || config.models.is_empty() || config.models.iter().any(|id| id.trim().is_empty()) {
        return Err("图片配置需要 HTTP(S) API 地址、API Token 和非空 models 数组".into());
    }
    Ok(config)
}

/// 只向会话提供路径和模型列表，不把 Token 放入提示词。
#[tauri::command]
pub fn image_command_context(app: tauri::AppHandle, configured: bool) -> Result<Value, String> {
    let path = crate::nova_data_dir(&app).join("image-generation.json");
    let models = if configured { load_config(&path)?.models } else { Vec::new() };
    Ok(json!({ "configPath": path, "executable": std::env::current_exe().map_err(|e| e.to_string())?, "models": models }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Request {
    prompt: String,
    model: String,
    #[serde(default = "auto_size")]
    size: String,
    output_dir: PathBuf,
}

fn auto_size() -> String { "auto".into() }

async fn generate(http: &reqwest::Client, config: &Config, request: &Request) -> Result<PathBuf, String> {
    if request.prompt.trim().is_empty() || request.prompt.len() > 32_000 {
        return Err("图片提示词不能为空且不能超过 32000 字节".into());
    }
    if !config.models.contains(&request.model) { return Err("所选图片模型未配置".into()); }
    if !matches!(request.size.as_str(), "auto" | "1024x1024" | "1024x1536" | "1536x1024") {
        return Err("不支持的图片尺寸".into());
    }
    let output_dir = request.output_dir.canonicalize().map_err(|_| "图片输出目录不存在")?;
    if !output_dir.is_dir() { return Err("图片输出路径必须是目录".into()); }
    let mut response = http.post(format!("{}/images/generations", config.base_url.trim_end_matches('/')))
        .bearer_auth(&config.api_key)
        .json(&json!({ "model": request.model, "prompt": request.prompt, "size": request.size, "n": 1, "output_format": "png" }))
        .send().await.map_err(|e| format!("图片请求失败：{}", e.without_url()))?;
    let status = response.status();
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| "图片响应读取失败")? {
        if bytes.len() + chunk.len() > 64 * 1024 * 1024 { return Err("图片响应超过 64 MiB".into()); }
        bytes.extend_from_slice(&chunk);
    }
    if !status.is_success() {
        // 不回显上游原文，避免兼容网关在错误消息中反射凭据。
        return Err(format!("图片 API 返回 HTTP {status}，请检查 Token、模型和接口配置"));
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| "图片 API 未返回有效 JSON")?;
    let data = value.pointer("/data/0/b64_json").and_then(Value::as_str).ok_or("接口未返回 Base64 图片，请确认支持 OpenAI Images API")?;
    let data = base64::engine::general_purpose::STANDARD.decode(data).map_err(|_| "图片 Base64 无效")?;
    if !data.starts_with(b"\x89PNG\r\n\x1a\n") { return Err("接口未返回 PNG 图片".into()); }
    let path = output_dir.join(format!("nova-image-{}.png", uuid::Uuid::new_v4()));
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&path).map_err(|e| format!("保存图片失败：{e}"))?;
    if let Err(error) = file.write_all(&data).and_then(|_| file.sync_all()) {
        drop(file);
        let _ = std::fs::remove_file(&path);
        return Err(format!("保存图片失败：{error}"));
    }
    Ok(path)
}

/// 由任意 Agent 的命令行工具调用；必须先于 GUI 单实例检查运行。
pub fn maybe_run() -> bool {
    let args: Vec<_> = std::env::args_os().collect();
    if args.get(1).and_then(|v| v.to_str()) != Some("__generate_image") { return false; }
    let run = || -> Result<PathBuf, String> {
        if args.len() != 4 { return Err("用法：Nova __generate_image <配置文件> <请求 JSON 文件>".into()); }
        let config = load_config(Path::new(&args[2]))?;
        let text = std::fs::read_to_string(&args[3]).map_err(|_| "无法读取生图请求文件")?;
        let request: Request = serde_json::from_str(&text).map_err(|_| "生图请求需要 prompt、model、outputDir 字段")?;
        let http = reqwest::Client::builder().connect_timeout(std::time::Duration::from_secs(15))
            .timeout(std::time::Duration::from_secs(600)).build().map_err(|e| e.to_string())?;
        tokio::runtime::Runtime::new().map_err(|e| e.to_string())?.block_on(generate(&http, &config, &request))
    };
    match run() {
        Ok(path) => println!("{}", json!({ "path": path.to_string_lossy().trim_start_matches(r"\\?\").replace('\\', "/") })),
        Err(error) => { eprintln!("{error}"); std::process::exit(1); }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn image_generation_validates_model_and_saves_only_valid_output() {
        use std::io::{Read, Write};
        let png = base64::engine::general_purpose::STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aX1sAAAAASUVORK5CYII=").unwrap();
        for (status, body, success) in [
            ("200 OK", json!({ "data": [{ "b64_json": base64::engine::general_purpose::STANDARD.encode(&png) }] }), true),
            ("200 OK", json!({ "data": [{ "b64_json": "invalid" }] }), false),
            ("200 OK", json!({ "data": [] }), false),
            ("401 Unauthorized", json!({ "error": { "message": "secret-token" } }), false),
        ] {
            let root = tempfile::tempdir().unwrap();
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let server = std::thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                socket.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
                let mut bytes = Vec::new();
                let mut chunk = [0; 4096];
                let end = loop {
                    let n = socket.read(&mut chunk).unwrap();
                    assert!(n > 0);
                    bytes.extend_from_slice(&chunk[..n]);
                    if let Some(i) = bytes.windows(4).position(|v| v == b"\r\n\r\n") { break i + 4; }
                };
                let headers = String::from_utf8_lossy(&bytes[..end]).to_lowercase();
                let length: usize = headers.lines().find_map(|v| v.strip_prefix("content-length: ")).unwrap().parse().unwrap();
                while bytes.len() < end + length {
                    let n = socket.read(&mut chunk).unwrap();
                    assert!(n > 0);
                    bytes.extend_from_slice(&chunk[..n]);
                }
                assert!(headers.starts_with("post /v1/images/generations "));
                assert!(headers.contains("authorization: bearer secret-token"));
                let request: Value = serde_json::from_slice(&bytes[end..end + length]).unwrap();
                assert_eq!(request["model"], "second-image");
                assert_eq!(request["n"], 1);
                let body = body.to_string();
                write!(socket, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            });
            let config = Config { base_url: format!("http://{address}/v1"), api_key: "secret-token".into(), models: vec!["first-image".into(), "second-image".into()] };
            let mut request = Request { prompt: "test".into(), model: "unlisted".into(), size: "auto".into(), output_dir: root.path().into() };
            let http = reqwest::Client::builder().no_proxy().build().unwrap();
            assert!(generate(&http, &config, &request).await.unwrap_err().contains("未配置"));
            request.model = "second-image".into();
            let result = generate(&http, &config, &request).await;
            server.join().unwrap();
            assert_eq!(result.is_ok(), success);
            match result {
                Ok(path) => assert_eq!(std::fs::read(path).unwrap(), png),
                Err(error) => {
                    assert!(!error.contains("secret-token"));
                    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
                }
            }
        }
    }

    #[test]
    fn image_generation_config_requires_token_and_models() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("image-generation.json");
        for (models, valid) in [(json!([]), false), (json!([""]), false), (json!(["a", "b"]), true)] {
            std::fs::write(&path, json!({ "baseURL": "https://example.com/v1", "apiKey": "token", "models": models }).to_string()).unwrap();
            assert_eq!(load_config(&path).is_ok(), valid);
        }
        std::fs::write(&path, json!({ "baseURL": "https://example.com/v1", "apiKey": "", "models": ["a"] }).to_string()).unwrap();
        assert!(load_config(&path).is_err());
    }
}
