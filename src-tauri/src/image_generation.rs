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
pub fn image_command_context(app: tauri::AppHandle, configured: bool, images: Option<Vec<crate::threads::PromptImage>>) -> Result<Value, String> {
    let dir = crate::nova_data_dir(&app);
    let path = dir.join("image-generation.json");
    let models = if configured { load_config(&path)?.models } else { Vec::new() };
    let reference_images = reference_image_paths(&images.unwrap_or_default())?;
    Ok(json!({ "configPath": path, "models": models, "referenceImages": reference_images }))
}

fn reference_image_paths(images: &[crate::threads::PromptImage]) -> Result<Vec<String>, String> {
    if images.len() > 16 { return Err("参考图最多 16 张".into()); }
    images.iter().map(|image| {
        if !matches!(image.mime_type.as_str(), "image/png" | "image/jpeg" | "image/webp") {
            return Err("参考图仅支持 PNG、JPEG 和 WebP".into());
        }
        if let Some(data) = &image.data {
            if data.len() > ((crate::threads::MAX_EMBED_BYTES + 2) / 3 * 4) as usize {
                return Err("单张参考图不能超过 25 MiB".into());
            }
            return crate::threads::save_attachment_to_temp(image).ok_or_else(|| "参考图保存失败或 Base64 无效".into());
        }
        image.uri.as_deref().and_then(crate::threads::file_uri_to_local_path)
            .ok_or_else(|| "参考图缺少图片数据或本地文件路径".into())
    }).collect()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Request {
    prompt: String,
    model: String,
    #[serde(default = "auto_size")]
    size: String,
    output_dir: PathBuf,
    #[serde(default)]
    reference_images: Vec<PathBuf>,
}

fn auto_size() -> String { "auto".into() }

pub fn tool_definitions() -> Vec<Value> {
    serde_json::from_str(include_str!("../../scripts/image-tools.json")).expect("valid image tool schemas")
}

fn tool_request(root: &Path, name: &str, args: &Value, config: &Config) -> Result<Request, String> {
    if !matches!(name, "generate_image" | "edit_image") { return Err("未知图片工具".into()); }
    let mut args = args.as_object().cloned().ok_or("图片工具参数必须是对象")?;
    if args.keys().any(|key| !["prompt", "model", "size", "outputDir", "referenceImages"].contains(&key.as_str())) {
        return Err("图片工具含不支持的参数".into());
    }
    args.entry("model").or_insert_with(|| json!(config.models[0]));
    args.entry("outputDir").or_insert_with(|| json!(root));
    let mut request: Request = serde_json::from_value(Value::Object(args)).map_err(|_| "图片工具参数格式无效，请按工具 schema 提供 prompt 和图片路径")?;
    if name == "edit_image" && request.reference_images.is_empty() {
        return Err("edit_image 必须提供原图，referenceImages 首项为待编辑图片".into());
    }
    if request.output_dir.is_relative() { request.output_dir = root.join(&request.output_dir); }
    for path in &mut request.reference_images {
        if path.is_relative() { *path = root.join(&*path); }
    }
    Ok(request)
}

pub async fn execute_tool(config_dir: &Path, root: &Path, name: &str, args: &Value) -> Result<Value, String> {
    let config = load_config(&config_dir.join("image-generation.json"))?;
    let request = tool_request(root, name, args, &config)?;
    let http = reqwest::Client::builder().connect_timeout(std::time::Duration::from_secs(15))
        .timeout(std::time::Duration::from_secs(600)).build().map_err(|e| e.to_string())?;
    let path = generate(&http, &config, &request).await?;
    let path = path.to_string_lossy().trim_start_matches(r"\\?\").replace('\\', "/");
    Ok(json!({ "path": path, "model": request.model, "markdown": format!("![图片](<{path}>)") }))
}

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
    let base_url = config.base_url.trim_end_matches('/');
    let call = if request.reference_images.is_empty() {
        http.post(format!("{base_url}/images/generations"))
            .json(&json!({ "model": request.model, "prompt": request.prompt, "size": request.size, "n": 1, "output_format": "png" }))
    } else {
        use std::io::Read;
        if request.reference_images.len() > 16 { return Err("参考图最多 16 张".into()); }
        let mut form = reqwest::multipart::Form::new()
            .text("model", request.model.clone()).text("prompt", request.prompt.clone())
            .text("size", request.size.clone()).text("n", "1").text("output_format", "png");
        let mut total = 0;
        for (index, path) in request.reference_images.iter().enumerate() {
            if !path.is_absolute() { return Err("参考图需要本地文件绝对路径".into()); }
            let file = std::fs::File::open(path).map_err(|_| "参考图文件无法读取")?;
            if !file.metadata().map_err(|_| "参考图文件信息无法读取")?.is_file() {
                return Err("参考图路径必须是文件".into());
            }
            let mut bytes = Vec::new();
            file.take(crate::threads::MAX_EMBED_BYTES + 1).read_to_end(&mut bytes).map_err(|_| "参考图读取失败")?;
            if bytes.len() as u64 > crate::threads::MAX_EMBED_BYTES { return Err("单张参考图不能超过 25 MiB".into()); }
            total += bytes.len();
            if total > 64 * 1024 * 1024 { return Err("参考图总计不能超过 64 MiB".into()); }
            let (mime, ext) = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") { ("image/png", "png") }
                else if bytes.starts_with(b"\xff\xd8\xff") { ("image/jpeg", "jpg") }
                else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") { ("image/webp", "webp") }
                else { return Err("参考图内容必须为 PNG、JPEG 或 WebP".into()); };
            let part = reqwest::multipart::Part::bytes(bytes).file_name(format!("reference-{index}.{ext}"))
                .mime_str(mime).map_err(|_| "参考图类型无效")?;
            form = form.part(if request.reference_images.len() == 1 { "image" } else { "image[]" }, part);
        }
        http.post(format!("{base_url}/images/edits")).multipart(form)
    };
    let started = std::time::Instant::now();
    let mut response = call.bearer_auth(&config.api_key).send().await
        .map_err(|e| format!("图片请求失败：{}", e.without_url()))?;
    let status = response.status();
    if !status.is_success() {
        // 不回显上游原文，避免兼容网关在错误消息中反射凭据。
        return Err(format!("图片 API 返回 HTTP {status}，请检查 Token、模型和接口配置"));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| {
        let reason = if error.is_timeout() { "读取超时" } else { "传输中断或响应解码失败" };
        format!("图片响应读取失败：{reason}（HTTP {status}，耗时 {:.1}s，已接收 {} 字节）：{}；HTTP 成功不代表图片数据接收完整，未自动重试，请先检查服务端结果以免重复计费",
            started.elapsed().as_secs_f64(), bytes.len(), error.without_url())
    })? {
        if bytes.len() + chunk.len() > 64 * 1024 * 1024 { return Err("图片响应超过 64 MiB".into()); }
        bytes.extend_from_slice(&chunk);
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

    #[test]
    fn image_tools_default_model_and_require_original_for_edits() {
        let root = tempfile::tempdir().unwrap();
        let config = Config { base_url: "http://localhost/v1".into(), api_key: "secret".into(), models: vec!["default-image".into()] };
        let request = tool_request(root.path(), "generate_image", &json!({ "prompt": "cat" }), &config).unwrap();
        assert_eq!(request.model, "default-image");
        assert_eq!(request.output_dir, root.path());
        for args in [json!({ "prompt": "cat" }), json!({ "prompt": "cat", "referenceImages": [] })] {
            assert!(tool_request(root.path(), "edit_image", &args, &config).unwrap_err().contains("原图"));
        }
        let request = tool_request(root.path(), "edit_image", &json!({ "prompt": "blue background", "referenceImages": ["original.png"], "outputDir": "assets" }), &config).unwrap();
        assert_eq!(request.reference_images, vec![root.path().join("original.png")]);
        assert_eq!(request.output_dir, root.path().join("assets"));
        assert!(tool_request(root.path(), "generate_image", &json!({ "prompt": "cat", "apiKey": "leak" }), &config).is_err());
        let tools = tool_definitions();
        assert_eq!(tools[1]["inputSchema"]["properties"]["referenceImages"]["minItems"], 1);
    }

    #[tokio::test]
    async fn image_generation_validates_model_and_saves_only_valid_output() {
        use std::io::{Read, Write};
        let png = base64::engine::general_purpose::STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aX1sAAAAASUVORK5CYII=").unwrap();
        for (reference_count, status, body, success) in [
            (0, "200 OK", json!({ "data": [{ "b64_json": base64::engine::general_purpose::STANDARD.encode(&png) }] }), true),
            (1, "200 OK", json!({ "data": [{ "b64_json": base64::engine::general_purpose::STANDARD.encode(&png) }] }), true),
            (2, "200 OK", json!({ "data": [{ "b64_json": base64::engine::general_purpose::STANDARD.encode(&png) }] }), true),
            (0, "200 OK", json!({ "data": [{ "b64_json": "invalid" }] }), false),
            (0, "200 OK", json!({ "data": [] }), false),
            (0, "401 Unauthorized", json!({ "error": { "message": "secret-token" } }), false),
            (1, "400 Bad Request", json!({ "error": { "message": "secret-token" } }), false),
        ] {
            let root = tempfile::tempdir().unwrap();
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let expected_png = png.clone();
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
                assert!(headers.contains("authorization: bearer secret-token"));
                if reference_count == 0 {
                    assert!(headers.starts_with("post /v1/images/generations "));
                    let request: Value = serde_json::from_slice(&bytes[end..end + length]).unwrap();
                    assert_eq!(request["model"], "second-image");
                    assert_eq!(request["n"], 1);
                } else {
                    assert!(headers.starts_with("post /v1/images/edits "));
                    assert!(headers.contains("multipart/form-data; boundary="));
                    let body = &bytes[end..end + length];
                    let text = String::from_utf8_lossy(body);
                    assert!(text.contains("name=\"model\"\r\n\r\nsecond-image"));
                    assert!(text.contains("name=\"prompt\"\r\n\r\ntest"));
                    let field = if reference_count == 1 { "name=\"image\"" } else { "name=\"image[]\"" };
                    assert_eq!(text.matches(field).count(), reference_count);
                    assert_eq!(body.windows(expected_png.len()).filter(|v| *v == expected_png.as_slice()).count(), reference_count);
                }
                let body = body.to_string();
                write!(socket, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            });
            let config = Config { base_url: format!("http://{address}/v1"), api_key: "secret-token".into(), models: vec!["first-image".into(), "second-image".into()] };
            let mut request = Request { prompt: "test".into(), model: "unlisted".into(), size: "auto".into(), output_dir: root.path().into(), reference_images: vec![] };
            let http = reqwest::Client::builder().no_proxy().build().unwrap();
            assert!(generate(&http, &config, &request).await.unwrap_err().contains("未配置"));
            request.model = "second-image".into();
            let references = tempfile::tempdir().unwrap();
            for index in 0..reference_count {
                let path = references.path().join(format!("参考 {index}.png"));
                std::fs::write(&path, &png).unwrap();
                request.reference_images.push(path);
            }
            let settings = tempfile::tempdir().unwrap();
            std::fs::write(settings.path().join("image-generation.json"), json!({ "baseURL": config.base_url, "apiKey": config.api_key, "models": ["second-image", "first-image"] }).to_string()).unwrap();
            let result = execute_tool(settings.path(), root.path(), if reference_count == 0 { "generate_image" } else { "edit_image" }, &json!({
                "prompt": request.prompt, "referenceImages": request.reference_images,
            })).await;
            server.join().unwrap();
            assert_eq!(result.is_ok(), success);
            match result {
                Ok(value) => {
                    let path = value["path"].as_str().unwrap();
                    assert_eq!(std::fs::read(path).unwrap(), png);
                    assert_eq!(value["model"], "second-image");
                    assert_eq!(value["markdown"], format!("![图片](<{path}>)"));
                    assert!(!value.to_string().contains("secret-token"));
                }
                Err(error) => {
                    assert!(!error.contains("secret-token"));
                    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
                }
            }
        }
    }

    #[tokio::test]
    async fn image_response_errors_distinguish_http_truncation_and_timeout() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        for (status, stall, expected) in [
            ("200 OK", false, "传输中断或响应解码失败"),
            ("200 OK", true, "读取超时"),
            ("502 Bad Gateway", false, "图片 API 返回 HTTP 502"),
        ] {
            let root = tempfile::tempdir().unwrap();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let mut chunk = [0; 4096];
                let end = loop {
                    let n = socket.read(&mut chunk).await.unwrap();
                    assert!(n > 0);
                    request.extend_from_slice(&chunk[..n]);
                    if let Some(i) = request.windows(4).position(|v| v == b"\r\n\r\n") { break i + 4; }
                };
                let headers = String::from_utf8_lossy(&request[..end]).to_lowercase();
                let length: usize = headers.lines().find_map(|v| v.strip_prefix("content-length: ")).unwrap().parse().unwrap();
                while request.len() < end + length {
                    let n = socket.read(&mut chunk).await.unwrap();
                    assert!(n > 0);
                    request.extend_from_slice(&chunk[..n]);
                }
                socket.write_all(format!("HTTP/1.1 {status}\r\nContent-Length: 1000\r\nConnection: close\r\n\r\nsecret-token").as_bytes()).await.unwrap();
                if stall { std::future::pending::<()>().await; }
            });
            let config = Config { base_url: format!("http://{address}/v1"), api_key: "secret-token".into(), models: vec!["image".into()] };
            let request = Request { prompt: "test".into(), model: "image".into(), size: "auto".into(), output_dir: root.path().into(), reference_images: vec![] };
            let http = reqwest::Client::builder().no_proxy().timeout(std::time::Duration::from_secs(2)).build().unwrap();
            let error = generate(&http, &config, &request).await.unwrap_err();
            if stall { server.abort(); } else { server.await.unwrap(); }
            assert!(error.contains(expected), "{error}");
            if status == "200 OK" {
                for detail in ["HTTP 200", "耗时", "已接收", "未自动重试"] {
                    assert!(error.contains(detail), "{error}");
                }
            }
            assert!(!error.contains("secret-token"), "{error}");
            assert!(!error.contains(&address.to_string()), "{error}");
            assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
        }
    }

    #[test]
    fn reference_attachments_preserve_bytes_and_reject_missing_data() {
        let mut image = crate::threads::PromptImage {
            name: "参考.png".into(), mime_type: "image/png".into(),
            data: Some(base64::engine::general_purpose::STANDARD.encode(b"reference bytes")), uri: None, size: None,
        };
        let paths = reference_image_paths(std::slice::from_ref(&image)).unwrap();
        assert_eq!(std::fs::read(&paths[0]).unwrap(), b"reference bytes");
        std::fs::remove_file(&paths[0]).unwrap();
        std::fs::remove_dir(Path::new(&paths[0]).parent().unwrap()).unwrap();
        image.data = None;
        assert!(reference_image_paths(std::slice::from_ref(&image)).is_err());
        image.uri = Some("https://example.com/image.png".into());
        assert!(reference_image_paths(std::slice::from_ref(&image)).is_err());
        image.uri = Some("file:///D:/reference.png".into());
        assert_eq!(reference_image_paths(std::slice::from_ref(&image)).unwrap().len(), 1);
        image.mime_type = "text/plain".into();
        assert!(reference_image_paths(&[image]).is_err());
    }

    #[tokio::test]
    async fn invalid_references_fail_before_network_request() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("not-an-image.png");
        std::fs::write(&path, b"not an image").unwrap();
        let config = Config { base_url: "http://127.0.0.1:1/v1".into(), api_key: "secret-token".into(), models: vec!["image".into()] };
        let mut request: Request = serde_json::from_value(json!({ "prompt": "test", "model": "image", "outputDir": root.path() })).unwrap();
        assert!(request.reference_images.is_empty());
        for (paths, expected) in [
            (vec![path.clone()], "参考图内容"),
            (vec![root.path().join("missing.png")], "无法读取"),
            (vec![PathBuf::from("relative.png")], "绝对路径"),
            (vec![path.clone(); 17], "最多 16 张"),
        ] {
            request.reference_images = paths;
            assert!(generate(&reqwest::Client::new(), &config, &request).await.unwrap_err().contains(expected));
        }
        std::fs::File::create(&path).unwrap().set_len(crate::threads::MAX_EMBED_BYTES + 1).unwrap();
        request.reference_images = vec![path];
        assert!(generate(&reqwest::Client::new(), &config, &request).await.unwrap_err().contains("25 MiB"));
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
