//! Native Devin inference. The CLI supplies its catalog/version; Rust owns HTTP and streaming.
//! Without a usable CLI the bundled catalog snapshot (devin_models.json, CLI 3000.11.205) is used.
//! ponytail: private Connect/Protobuf fields verified against CLI 3000.11.205;
//! re-run the protocol checks and live tool round trip when Devin changes its wire format.
use super::config::ResolvedModel;
use super::provider::{self, StreamEvent, StreamResult};
use base64::Engine;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{atomic::AtomicBool, Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::io::AsyncReadExt;

const LIMIT: usize = 32 * 1024 * 1024;
const RPC: &str = "/exa.api_server_pb.ApiServerService/GetChatMessage";
/// ponytail: 离线快照不会自动更新；Devin 上新模型后用 `devin models list --format json` 重新生成。
const SNAPSHOT_VERSION: &str = "3000.11.205";
const SNAPSHOT: &str = include_str!("devin_models.json");

fn read_config(path: &Path) -> Result<String, String> {
    let file = std::fs::File::open(path).map_err(|_| "无法读取 Devin 配置，请先运行 devin auth login")?;
    let mut text = String::new();
    file.take(1024 * 1024 + 1).read_to_string(&mut text).map_err(|_| "Devin 配置不是有效 UTF-8")?;
    if text.len() > 1024 * 1024 { return Err("Devin 配置超过 1 MiB".into()); }
    Ok(text)
}

pub(super) fn load(root: &Path, env: &HashMap<String, String>) -> Result<Value, String> {
    let config_root = if cfg!(windows) {
        env.get("APPDATA").map(PathBuf::from)
    } else {
        env.get("XDG_CONFIG_HOME").map(PathBuf::from)
            .or_else(|| env.get("HOME").map(|home| Path::new(home).join(".config")))
    }.ok_or("无法定位 Devin 配置目录")?;
    let credentials: toml::Value = toml::from_str(&read_config(&config_root.join("devin/credentials.toml"))?)
        .map_err(|_| "Devin credentials.toml 格式无效")?;
    let key = credentials.get("windsurf_api_key").and_then(toml::Value::as_str)
        .filter(|s| !s.trim().is_empty()).ok_or("Devin 登录缺少 windsurf_api_key，请先运行 devin auth login")?;
    let url = credentials.get("api_server_url").and_then(toml::Value::as_str).unwrap_or("https://server.codeium.com");
    let parsed = reqwest::Url::parse(url).map_err(|_| "Devin API 地址无效")?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none()
        || !parsed.username().is_empty() || parsed.password().is_some() || parsed.query().is_some() || parsed.fragment().is_some() {
        return Err("Devin API 地址必须为不含认证或查询参数的 HTTP(S) 地址".into());
    }
    let model_path = env.get("DEVIN_CONFIG_HOME").map(PathBuf::from).unwrap_or(config_root).join("devin/config.json");
    let default_model = if let Some(model) = env.get("DEVIN_MODEL").filter(|s| !s.trim().is_empty()) {
        model.clone()
    } else if model_path.exists() {
        let config: Value = serde_json::from_str(&read_config(&model_path)?).map_err(|_| "Devin config.json 格式无效")?;
        config.pointer("/agent/model").and_then(Value::as_str).unwrap_or("adaptive").to_string()
    } else { "adaptive".to_string() };
    let settings = crate::settings::Settings::load(&root.to_path_buf());
    Ok(json!({ "name": "Devin（本地 API）", "api": "devin", "options": {
        "baseURL": url, "apiKey": key, "devinCli": settings.devin_path,
        "devinModel": default_model,
        "supportsReasoningEffort": false, "supportsThinkingToggle": false
    }, "models": {} }))
}

#[derive(Clone)]
struct Catalog { families: Vec<Value>, version: String, loaded: Instant }

async fn cli(binary: &str, args: &[&str]) -> Result<Vec<u8>, String> {
    let program = crate::acp::resolve_program_on_path(if binary.trim().is_empty() { "devin" } else { binary })
        .ok_or("未找到 Devin CLI，请检查 Devin 路径设置")?;
    let mut command = tokio::process::Command::new(program);
    command.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x0800_0000);
    tokio::time::timeout(Duration::from_secs(30), async {
        let mut child = command.spawn().map_err(|_| "无法启动 Devin CLI")?;
        let mut bytes = Vec::new();
        child.stdout.take().ok_or("无法读取 Devin CLI 输出")?.take(LIMIT as u64 + 1)
            .read_to_end(&mut bytes).await.map_err(|_| "无法读取 Devin CLI 输出")?;
        if bytes.len() > LIMIT { return Err("Devin CLI 输出超过 32 MiB".into()); }
        if !child.wait().await.map_err(|_| "无法等待 Devin CLI")?.success() {
            return Err("无法读取 Devin 模型目录/版本，请检查 CLI 登录和代理设置".into());
        }
        Ok(bytes)
    }).await.map_err(|_| "读取 Devin 模型目录/版本超时")?
}

async fn catalog(options: &Value, force: bool) -> Result<Catalog, String> {
    static CACHE: OnceLock<Mutex<HashMap<String, Catalog>>> = OnceLock::new();
    let binary = options["devinCli"].as_str().unwrap_or("devin");
    let key = binary.to_string();
    let cache = CACHE.get_or_init(Default::default);
    if !force {
        if let Some(cached) = cache.lock().unwrap().get(&key).filter(|c| c.loaded.elapsed() < Duration::from_secs(60)) {
            return Ok(cached.clone());
        }
    }
    let result = match cli_catalog(binary).await {
        Ok(result) => result,
        Err(error) => {
            eprintln!("[lyra] Devin CLI 不可用，使用内置模型目录：{error}");
            snapshot()
        }
    };
    cache.lock().unwrap().insert(key, result.clone());
    Ok(result)
}

async fn cli_catalog(binary: &str) -> Result<Catalog, String> {
    let parsed: Value = serde_json::from_slice(&cli(binary, &["models", "list", "--format", "json"]).await?)
        .map_err(|_| "Devin 模型目录不是有效 JSON")?;
    let families = parsed["families"].as_array().filter(|v| !v.is_empty()).ok_or("Devin 模型目录缺少 families")?
        .iter().filter(|family| family["slug"] != "fusion").cloned().collect();
    let version = cli(binary, &["version"]).await?;
    let version = regex::Regex::new(r"\d+\.\d+\.\d+").unwrap().find(&String::from_utf8_lossy(&version))
        .ok_or("无法解析 Devin CLI 版本")?.as_str().to_string();
    Ok(Catalog { families, version, loaded: Instant::now() })
}

fn snapshot() -> Catalog {
    let parsed: Value = serde_json::from_str(SNAPSHOT).expect("bundled Devin catalog");
    Catalog { families: parsed["families"].as_array().cloned().unwrap_or_default(), version: SNAPSHOT_VERSION.into(), loaded: Instant::now() }
}

fn catalog_models(catalog: &Catalog) -> Map<String, Value> {
    let mut models = Map::new();
    for variant in catalog.families.iter().filter_map(|f| f["variants"].as_array()).flatten() {
        let Some(id) = variant["model_uid"].as_str().filter(|s| !s.is_empty()) else { continue };
        models.insert(id.into(), json!({
            "name": variant["label"].as_str().unwrap_or(id),
            "limit": { "context": variant["max_context_tokens"].as_u64().unwrap_or(128000),
                "output": variant["max_output_tokens"].as_u64().unwrap_or(128000).min(128000) },
            "options": { "supportsReasoningEffort": false, "supportsThinkingToggle": false }
        }));
    }
    models
}

pub(super) async fn models(options: &Value, force: bool) -> Result<Map<String, Value>, String> {
    let catalog = catalog(options, force).await?;
    let mut models = catalog_models(&catalog);
    let id = resolve_model(&catalog, options["devinModel"].as_str().unwrap_or("adaptive"), None)?;
    let mut default = models.get(&id).cloned().ok_or("Devin 模型目录为空")?;
    default["name"] = json!("Devin（跟随 CLI 默认模型）");
    models.insert("devin".into(), default);
    Ok(models)
}

fn resolve_model(catalog: &Catalog, requested: &str, effort: Option<&str>) -> Result<String, String> {
    let exists = |id: &str| catalog.families.iter().filter_map(|f| f["variants"].as_array()).flatten()
        .any(|v| v["model_uid"] == id);
    let mut id = requested.to_string();
    if !exists(&id) {
        if let Some(family) = catalog.families.iter().find(|f| f["slug"] == id || f["aliases"].as_array().is_some_and(|a| a.iter().any(|v| v == &id))) {
            id = family["variants"][0]["model_uid"].as_str().unwrap_or("").to_string();
        }
    }
    if let Some(effort) = effort {
        if !matches!(effort, "none" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max") {
            return Err("Devin 思考档位无效，请选择对应模型 ID".into());
        }
        let suffix = if id.ends_with("-priority") { "-priority" } else { "" };
        let base = id.strip_suffix("-priority").unwrap_or(&id);
        let (name, previous) = base.rsplit_once('-').ok_or("Devin 模型不支持切换思考档位")?;
        if !matches!(previous, "none" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max") {
            return Err("Devin 模型不支持切换思考档位，请选择对应模型 ID".into());
        }
        id = format!("{name}-{effort}{suffix}");
    }
    if !exists(&id) { return Err(format!("Devin 模型或档位不存在：{id}，请刷新模型列表")); }
    Ok(id)
}

// Only the protobuf wire types needed by this endpoint; unknown fields remain skippable.
fn varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 128 { out.push(value as u8 | 128); value >>= 7; }
    out.push(value as u8);
}
fn number(out: &mut Vec<u8>, field: u64, value: u64) { varint(out, field << 3); varint(out, value); }
fn bytes(out: &mut Vec<u8>, field: u64, value: impl AsRef<[u8]>) {
    let value = value.as_ref(); varint(out, field << 3 | 2); varint(out, value.len() as u64); out.extend_from_slice(value);
}
fn double(out: &mut Vec<u8>, field: u64, value: f64) { varint(out, field << 3 | 1); out.extend_from_slice(&value.to_le_bytes()); }
fn envelope(payload: &[u8], flags: u8) -> Vec<u8> {
    let mut out = vec![flags]; out.extend_from_slice(&(payload.len() as u32).to_be_bytes()); out.extend_from_slice(payload); out
}

fn encode_request(body: &Value, key: &str, version: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::new(); let mut metadata = Vec::new();
    let platform = if cfg!(windows) { "win32" } else if cfg!(target_os = "macos") { "darwin" } else { "linux" };
    for (field, value) in [(1,"nova-openai"), (2,version), (3,key), (4,"en"), (5,platform), (7,version), (12,"chisel"), (28,"chisel")] {
        bytes(&mut metadata, field, value);
    }
    bytes(&mut out, 1, metadata);
    let mut systems = Vec::new();
    for message in body["messages"].as_array().ok_or("Devin 请求缺少 messages")? {
        let mut msg = Vec::new(); let mut texts = Vec::new();
        bytes(&mut msg, 1, uuid::Uuid::new_v4().to_string());
        let role = message["role"].as_str().unwrap_or("");
        number(&mut msg, 2, match role { "user" => 1, "assistant" => 2, "tool" => 4, "system" | "developer" => 0, _ => return Err("Devin 消息角色无效".into()) });
        if let Some(parts) = message["content"].as_array() {
            for part in parts {
                match part["type"].as_str() {
                    Some("text") => texts.push(part["text"].as_str().ok_or("Devin 文本内容无效")?.to_string()),
                    Some("image_url") => {
                        let source = part["image_url"]["url"].as_str().unwrap_or("");
                        let (mime, data) = source.strip_prefix("data:").and_then(|s| s.split_once(";base64,")).ok_or("Devin 图片必须为 base64 data URL")?;
                        if !matches!(mime, "image/png" | "image/jpeg" | "image/gif" | "image/webp") || data.is_empty() || data.len() > LIMIT {
                            return Err("Devin 图片格式或大小无效".into());
                        }
                        let decoded = base64::engine::general_purpose::STANDARD.decode(data).map_err(|_| "Devin 图片 base64 无效")?;
                        if decoded.len() > LIMIT / 2 { return Err("Devin 图片超过 16 MiB".into()); }
                        let mut image = Vec::new(); bytes(&mut image, 1, data); bytes(&mut image, 2, mime); bytes(&mut msg, 10, image);
                    }
                    _ => return Err("Devin 不支持此消息内容类型".into()),
                }
            }
        } else if let Some(text) = message["content"].as_str() { texts.push(text.to_string()); }
        if matches!(role, "system" | "developer") { systems.push(texts.join("\n")); continue; }
        bytes(&mut msg, 3, texts.join("\n"));
        if let Some(calls) = message["tool_calls"].as_array() {
            for call in calls {
                let mut tool = Vec::new();
                for (field, value) in [(1, &call["id"]), (2, &call["function"]["name"]), (3, &call["function"]["arguments"])] {
                    bytes(&mut tool, field, value.as_str().ok_or("Devin 历史工具调用无效")?);
                }
                bytes(&mut msg, 6, tool);
            }
        }
        if let Some(id) = message["tool_call_id"].as_str() { bytes(&mut msg, 7, id); }
        bytes(&mut out, 3, msg);
        if out.len() > LIMIT { return Err("Devin 请求超过 32 MiB".into()); }
    }
    bytes(&mut out, 2, systems.join("\n\n")); number(&mut out, 7, 5);
    let mut generation = Vec::new();
    number(&mut generation, 1, 1);
    number(&mut generation, 2, body["max_completion_tokens"].as_u64().or_else(|| body["max_tokens"].as_u64()).unwrap_or(128000).clamp(32,128000));
    number(&mut generation, 3, 400);
    let temperature = body["temperature"].as_f64().unwrap_or(1.0);
    let top_p = body["top_p"].as_f64().unwrap_or(0.95);
    if !(0.0..=2.0).contains(&temperature) || !(0.0..=1.0).contains(&top_p) { return Err("Devin 采样参数无效".into()); }
    double(&mut generation, 5, temperature); number(&mut generation, 7, 40); double(&mut generation, 8, top_p);
    bytes(&mut out, 8, generation);
    if let Some(tools) = body["tools"].as_array() {
        for tool in tools {
            let tool = &tool["function"]; let mut definition = Vec::new();
            bytes(&mut definition, 1, tool["name"].as_str().ok_or("Devin 工具缺少名称")?);
            bytes(&mut definition, 2, tool["description"].as_str().unwrap_or(""));
            bytes(&mut definition, 3, tool["parameters"].to_string()); bytes(&mut out, 10, definition);
        }
    }
    let mut context = Vec::new(); bytes(&mut context, 1, uuid::Uuid::new_v4().to_string()); number(&mut context, 3, 4); number(&mut context, 4, 14);
    bytes(&mut out, 15, context); bytes(&mut out, 16, uuid::Uuid::new_v4().to_string()); number(&mut out, 20, 1);
    bytes(&mut out, 21, body["model"].as_str().ok_or("Devin 请求缺少模型")?);
    bytes(&mut out, 22, uuid::Uuid::new_v4().to_string()); bytes(&mut out, 28, "nova-api");
    if out.len() > LIMIT { return Err("Devin 请求超过 32 MiB".into()); }
    Ok(envelope(&out, 0))
}

#[derive(Debug)]
enum Wire<'a> { Number(u64), Bytes(&'a [u8]), Fixed }
impl Wire<'_> {
    fn bytes(&self) -> Result<&[u8], String> { match self { Self::Bytes(b) => Ok(b), _ => Err("Devin protobuf 字段类型无效".into()) } }
    fn text(&self) -> Result<&str, String> { std::str::from_utf8(self.bytes()?).map_err(|_| "Devin protobuf UTF-8 无效".into()) }
    fn number(&self) -> Result<u64, String> { match self { Self::Number(n) => Ok(*n), _ => Err("Devin protobuf 数值类型无效".into()) } }
}
fn decode(mut input: &[u8]) -> Result<Vec<(u64, Wire<'_>)>, String> {
    fn read(input: &mut &[u8]) -> Result<u64, String> {
        let mut value = 0;
        for shift in (0..70).step_by(7) {
            let (&byte, rest) = input.split_first().ok_or("Devin protobuf 被截断")?; *input = rest;
            if shift == 63 && byte > 1 { break; }
            value |= (byte as u64 & 127) << shift;
            if byte < 128 { return Ok(value); }
        }
        Err("Devin protobuf varint 溢出".into())
    }
    let mut fields = Vec::new();
    while !input.is_empty() {
        let tag = read(&mut input)?; let field = tag >> 3; let wire = tag & 7;
        if field == 0 || field >= 1 << 29 { return Err("Devin protobuf 字段编号无效".into()); }
        if wire == 0 { fields.push((field, Wire::Number(read(&mut input)?))); continue; }
        let size = match wire { 1 => 8, 5 => 4, 2 => read(&mut input)?, _ => return Err("Devin protobuf wire type 无效".into()) };
        if size > input.len() as u64 { return Err("Devin protobuf 字段被截断".into()); }
        let (data, rest) = input.split_at(size as usize); input = rest;
        fields.push((field, if wire == 2 { Wire::Bytes(data) } else { Wire::Fixed }));
    }
    Ok(fields)
}

#[derive(Default)]
struct ConnectDecoder { buffer: Vec<u8>, total: usize, expanded: usize, ended: bool }
impl ConnectDecoder {
    fn push(&mut self, chunk: &[u8]) -> Result<Vec<Vec<u8>>, String> {
        self.total += chunk.len();
        if self.total > LIMIT { return Err("Devin 响应超过 32 MiB".into()); }
        self.buffer.extend_from_slice(chunk);
        let mut offset = 0; let mut frames = Vec::new();
        while self.buffer.len() - offset >= 5 {
            let flags = self.buffer[offset];
            let size = u32::from_be_bytes(self.buffer[offset+1..offset+5].try_into().unwrap()) as usize;
            if flags > 3 || size > LIMIT || self.ended { return Err("Devin Connect 帧无效".into()); }
            if self.buffer.len() - offset < size + 5 { break; }
            let mut payload = self.buffer[offset+5..offset+5+size].to_vec(); offset += 5 + size;
            if flags & 1 != 0 {
                let mut expanded = Vec::new();
                flate2::read::GzDecoder::new(payload.as_slice()).take((LIMIT - self.expanded + 1) as u64)
                    .read_to_end(&mut expanded).map_err(|_| "Devin gzip 帧无效")?;
                payload = expanded;
            }
            self.expanded += payload.len();
            if self.expanded > LIMIT { return Err("Devin 解压响应超过 32 MiB".into()); }
            if flags & 2 != 0 {
                let trailer: Value = serde_json::from_slice(&payload).map_err(|_| "Devin Connect trailer 无效")?;
                if !trailer.is_object() { return Err("Devin Connect trailer 无效".into()); }
                if !trailer["error"].is_null() {
                    let code = match trailer["error"]["code"].as_str() {
                        Some("unauthenticated") => "unauthenticated (401)", Some("permission_denied") => "permission_denied (403)",
                        Some("resource_exhausted") => "resource_exhausted (429)", Some("invalid_argument") => "invalid_argument (400)", _ => "upstream_error"
                    };
                    return Err(format!("Devin 拒绝请求：{code}，请检查登录、模型权限和参数"));
                }
                self.ended = true;
            } else { frames.push(payload); }
        }
        self.buffer.drain(..offset);
        Ok(frames)
    }
    fn finish(&self) -> Result<(), String> {
        if !self.ended || !self.buffer.is_empty() { return Err("Devin 响应中断：缺少完整 Connect trailer".into()); }
        Ok(())
    }
}

#[derive(Default)]
struct ResponseState {
    result: StreamResult,
    calls: Vec<(String, String, String)>,
    indexes: HashMap<String, usize>,
    current: Option<usize>,
    stop: Option<u64>,
}
impl ResponseState {
    fn frame(&mut self, payload: &[u8], on_event: &mut (dyn FnMut(StreamEvent) + Send)) -> Result<(), String> {
        for (field, value) in decode(payload)? {
            match field {
                3 | 9 => provider::push_delta(&mut self.result, if field == 9 { "thinking" } else { "text" }, value.text()?, on_event),
                5 => {
                    let stop = value.number()?;
                    // Claude 系模型经 Devin 正常结束时返回 4（实测 claude-haiku-5-5），等同普通停止。
                    if !matches!(stop, 0 | 1 | 2 | 3 | 4 | 10) { return Err(format!("Unknown Devin stop reason: {stop}")); }
                    // SWE-2 sends 10 before its function name/arguments. Keep reading to the trailer.
                    if matches!(stop, 3 | 10) || self.stop.is_none() { self.stop = Some(stop); }
                }
                6 => {
                    let mut id = String::new(); let mut name = String::new(); let mut arguments = String::new();
                    for (field, value) in decode(value.bytes()?)? {
                        match field { 1 => id.push_str(value.text()?), 2 => name.push_str(value.text()?), 3 => arguments.push_str(value.text()?), _ => {} }
                    }
                    if !id.is_empty() {
                        let index = *self.indexes.entry(id.clone()).or_insert_with(|| { self.calls.push((id, String::new(), String::new())); self.calls.len()-1 });
                        self.current = Some(index);
                    }
                    let index = self.current.ok_or("Devin 工具参数缺少调用 ID")?;
                    self.calls[index].1.push_str(&name); self.calls[index].2.push_str(&arguments);
                }
                7 => {
                    let mut input = None; let mut output = None; let mut cached = 0;
                    for (field, value) in decode(value.bytes()?)? {
                        match field { 2 => input = Some(value.number()?), 3 => output = Some(value.number()?), 5 => cached = value.number()?, _ => {} }
                    }
                    if input.is_some() || output.is_some() {
                        let input = input.unwrap_or(0); let output = output.unwrap_or(0);
                        self.result.usage = json!({ "input": input.saturating_sub(cached), "output": output, "cacheRead": cached,
                            "cacheWrite": 0, "totalTokens": input.saturating_add(output) });
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
    fn finish(mut self, body: &Value, on_event: &mut (dyn FnMut(StreamEvent) + Send)) -> Result<StreamResult, String> {
        if self.stop == Some(10) && self.calls.is_empty() { return Err("Devin 报告工具交接，但没有返回工具调用".into()); }
        if self.stop == Some(3) && !self.calls.is_empty() { return Err("Devin 工具参数被截断，未执行工具".into()); }
        self.result.stop_reason = if self.calls.is_empty() { if self.stop == Some(3) { "length" } else { "stop" } } else { "toolUse" }.into();
        // Validate every call before emitting any ToolCallDone (Lyra can execute tools on that event).
        for (id, name, arguments) in self.calls {
            if !body["tools"].as_array().is_some_and(|tools| tools.iter().any(|t| t["function"]["name"] == name)) {
                return Err("Devin 返回了未提供的工具名称".into());
            }
            let arguments: Value = serde_json::from_str(&arguments).map_err(|_| "Devin 工具参数不是完整 JSON，未执行工具")?;
            if !arguments.is_object() { return Err("Devin 工具参数必须是 JSON 对象".into()); }
            self.result.content.push(json!({ "type": "toolCall", "id": id, "name": name, "arguments": arguments }));
        }
        for part in &self.result.content {
            if part["type"] == "toolCall" {
                on_event(StreamEvent::ToolCallDone { id: part["id"].as_str().unwrap().into(), name: part["name"].as_str().unwrap().into(), arguments: part["arguments"].clone() });
            }
        }
        Ok(self.result)
    }
}

async fn inference(http: &reqwest::Client, model: &ResolvedModel, key: &str, body: &Value, version: &str,
    cancel: &Arc<AtomicBool>, on_event: &mut (dyn FnMut(StreamEvent) + Send)) -> Result<StreamResult, String> {
    let url = reqwest::Url::parse(&model.base_url).and_then(|u| u.join(RPC)).map_err(|_| "Devin API 地址无效")?;
    let mut request = http.post(url).body(encode_request(body, key, version)?);
    for (name, value) in &model.headers { if let Some(value) = value.as_str() { request = request.header(name, value); } }
    request = request.header("content-type", "application/connect+proto").header("connect-protocol-version", "1").header("connect-accept-encoding", "gzip");
    let mut response = provider::send_request(request, cancel).await?;
    if !response.status().is_success() { return Err(format!("Devin upstream HTTP {}，请检查登录和模型权限", response.status().as_u16())); }
    if !response.headers().get("content-type").and_then(|v| v.to_str().ok()).is_some_and(|s| s.starts_with("application/connect+proto")) {
        return Err("Devin 响应类型不是 Connect/Protobuf".into());
    }
    let mut decoder = ConnectDecoder::default(); let mut state = ResponseState::default();
    loop {
        let chunk = tokio::select! {
            chunk = tokio::time::timeout(Duration::from_secs(180), response.chunk()) => chunk.map_err(|_| "Devin 响应连续 180s 无数据")?.map_err(|_| "Devin 响应流读取失败")?,
            _ = provider::wait_cancelled(cancel) => return Err("Devin 请求已取消".into()),
        };
        let Some(chunk) = chunk else { break };
        if !chunk.is_empty() { on_event(StreamEvent::Activity); }
        for frame in decoder.push(&chunk)? { state.frame(&frame, on_event)?; }
    }
    decoder.finish()?;
    state.finish(body, on_event)
}

pub(super) async fn stream(model: &ResolvedModel, key: &str, mut body: Value,
    cancel: &Arc<AtomicBool>, on_event: &mut (dyn FnMut(StreamEvent) + Send)) -> Result<StreamResult, String> {
    for option in model.extra_options.keys() {
        // devinProxy 为旧配置遗留键：已忽略（跟随系统代理），保留放行避免旧配置报错。
        if !matches!(option.as_str(), "devinCli" | "devinProxy" | "devinModel" | "cacheRetention" | "reasoning_effort") {
            return Err(format!("Devin 原生协议不支持 options.{option}"));
        }
    }
    if model.service_tier.is_some() { return Err("Devin 请直接选择 priority 模型 ID，不支持 serviceTier 参数".into()); }
    let options = json!(model.extra_options);
    let catalog = tokio::select! { result = catalog(&options, false) => result?, _ = provider::wait_cancelled(cancel) => return Err("Devin 请求已取消".into()) };
    let requested = if model.id == "devin" { options["devinModel"].as_str().unwrap_or("adaptive") } else { &model.id };
    let id = resolve_model(&catalog, requested, body["reasoning_effort"].as_str())?;
    let limit = catalog_models(&catalog)[&id]["limit"]["output"].as_u64().unwrap_or(128000);
    body["model"] = json!(id);
    body[model.max_tokens_field] = json!(body[model.max_tokens_field].as_u64().unwrap_or(128000).min(limit));
    let http = provider::local_api_client();
    inference(&http, model, key, &body, &catalog.version, cancel, on_event).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn value<'a>(fields: &'a [(u64, Wire<'a>)], field: u64) -> &'a Wire<'a> {
        &fields.iter().find(|(id, _)| *id == field).unwrap().1
    }
    fn model(url: &str) -> ResolvedModel {
        super::super::config::resolve_model(&json!({"provider":{"devin":{"api":"devin",
            "options":{"baseURL":url},"models":{"test":{"modalities":{"input":["text","image"]}}}}}}),
            Some("devin/test"), &HashMap::new()).unwrap().model
    }
    fn tool_body() -> Value { json!({"tools":[{"function":{"name":"read"}}]}) }
    fn call(id: &str, name: &str, args: &str) -> Vec<u8> {
        let mut payload = Vec::new(); let mut call = Vec::new();
        bytes(&mut call, 1, id); bytes(&mut call, 2, name); bytes(&mut call, 3, args); bytes(&mut payload, 6, call); payload
    }

    #[test]
    fn native_request_preserves_history_tools_images_and_sampling() {
        let mut body = provider::completions_body(&model("https://example.com"), "system", &[
            json!({"role":"user","content":[{"type":"text","text":"你好"},{"type":"image","mimeType":"image/png","data":"AQID"}]}),
            json!({"role":"assistant","content":[{"type":"toolCall","id":"call-a","name":"read","arguments":{"path":"a"}}]}),
            json!({"role":"toolResult","toolCallId":"call-a","content":[{"type":"text","text":"contents"}]}),
        ], &[], None, None);
        body["tools"] = json!([{"function":{"name":"read","parameters":{"type":"object"}}}]);
        body["temperature"] = json!(0.5);
        let request = encode_request(&body, "secret", "3000.11.205").unwrap();
        assert_eq!(request[0], 0);
        let fields = decode(&request[5..]).unwrap();
        assert_eq!(value(&fields, 2).text().unwrap(), "system");
        let metadata = decode(value(&fields, 1).bytes().unwrap()).unwrap();
        assert_eq!(value(&metadata, 3).text().unwrap(), "secret");
        let messages: Vec<_> = fields.iter().filter(|(id,_)| *id==3).map(|(_,v)| decode(v.bytes().unwrap()).unwrap()).collect();
        assert_eq!(messages.len(), 3);
        assert_eq!(value(&messages[0], 3).text().unwrap(), "你好");
        let image = decode(value(&messages[0], 10).bytes().unwrap()).unwrap();
        assert_eq!(value(&image, 1).text().unwrap(), "AQID");
        let call = decode(value(&messages[1], 6).bytes().unwrap()).unwrap();
        assert_eq!(value(&call, 1).text().unwrap(), value(&messages[2], 7).text().unwrap());
        assert_eq!(value(&messages[2], 2).number().unwrap(), 4);
        assert_eq!(value(&messages[2], 3).text().unwrap(), "contents");
        let tools = decode(value(&fields, 10).bytes().unwrap()).unwrap();
        assert_eq!(value(&tools, 1).text().unwrap(), "read");
    }

    #[test]
    fn connect_split_gzip_parallel_tools_and_stop_ten() {
        let mut payload = Vec::new(); number(&mut payload, 5, 10); bytes(&mut payload, 9, "思考"); bytes(&mut payload, 3, "正文");
        payload.extend(call("a", "read", "{\"path\":")); payload.extend(call("b", "read", "{\"path\":"));
        payload.extend(call("a", "", "\"a\"}")); payload.extend(call("b", "", "\"b\"}"));
        let mut usage = Vec::new(); number(&mut usage, 2, 20); number(&mut usage, 3, 8); number(&mut usage, 5, 5); bytes(&mut payload, 7, usage);
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default()); gzip.write_all(&payload).unwrap();
        let wire = [envelope(&gzip.finish().unwrap(), 1), envelope(b"{}", 2)].concat();
        let mut decoder = ConnectDecoder::default(); let mut state = ResponseState::default(); let mut events = Vec::new();
        for chunk in wire.chunks(3) { for frame in decoder.push(chunk).unwrap() { state.frame(&frame, &mut |e| events.push(e)).unwrap(); } }
        decoder.finish().unwrap();
        assert!(!events.iter().any(|e| matches!(e, StreamEvent::ToolCallDone{..})));
        let result = state.finish(&tool_body(), &mut |e| events.push(e)).unwrap();
        assert_eq!(result.stop_reason, "toolUse"); assert_eq!(result.usage["input"], 15); assert_eq!(result.usage["totalTokens"], 28);
        let tools: Vec<_> = result.content.iter().filter(|p| p["type"]=="toolCall").collect();
        assert_eq!(tools[0]["arguments"]["path"], "a"); assert_eq!(tools[1]["id"], "b");
        assert_eq!(events.iter().filter(|e| matches!(e, StreamEvent::ToolCallDone{..})).count(), 2);
    }

    #[test]
    fn malformed_or_truncated_stream_never_dispatches_tools() {
        for (stop, call) in [(10, vec![]), (3, call("a","read","{}")), (10, call("a","read","{")), (10, call("a","unknown","{}"))] {
            let mut state = ResponseState::default(); let mut frame = Vec::new(); number(&mut frame, 5, stop); frame.extend(call);
            state.frame(&frame, &mut |_| panic!("no early tool event")).unwrap();
            assert!(state.finish(&tool_body(), &mut |_| panic!("invalid call dispatched")).is_err());
        }
        for bad in [vec![0], vec![8,128], vec![10,3,1], vec![8,255,255,255,255,255,255,255,255,255,2]] { assert!(decode(&bad).is_err()); }
        let mut state = ResponseState::default(); assert!(state.frame(&[40,99], &mut |_| {}).unwrap_err().contains("99"));
        let mut state = ResponseState::default(); state.frame(&[40,4], &mut |_| {}).unwrap();
        assert_eq!(state.finish(&tool_body(), &mut |_| {}).unwrap().stop_reason, "stop");
        let mut decoder = ConnectDecoder::default(); decoder.push(&envelope(b"",0)).unwrap(); assert!(decoder.finish().is_err());
        let mut decoder = ConnectDecoder::default(); decoder.push(&envelope(b"{}",2)).unwrap(); assert!(decoder.push(&envelope(b"",0)).is_err());
        let mut decoder = ConnectDecoder::default();
        let error = decoder.push(&envelope(br#"{"error":{"code":"secret","message":"secret"}}"#,2)).unwrap_err(); assert!(!error.contains("secret"));
    }

    #[test]
    fn bundled_snapshot_resolves_default_and_effort_models() {
        let catalog = snapshot();
        assert!(catalog.families.len() > 10 && catalog.families.iter().all(|f| f["slug"] != "fusion"));
        assert_eq!(resolve_model(&catalog, "swe-2", None).unwrap(), "swe-2-high");
        assert!(catalog_models(&catalog).contains_key("claude-opus-4-8-high"));
    }

    #[test]
    fn catalog_aliases_limits_and_priority_effort() {
        let catalog = Catalog { loaded: Instant::now(), version: "1.2.3".into(), families: vec![json!({"slug":"gpt","aliases":["alias"],"variants":[
            {"model_uid":"gpt-low-priority","max_context_tokens":272000,"max_output_tokens":200000}, {"model_uid":"gpt-high-priority"}]})] };
        assert_eq!(resolve_model(&catalog,"alias",Some("high")).unwrap(), "gpt-high-priority");
        assert!(resolve_model(&catalog,"gpt",Some("max")).is_err());
        assert_eq!(catalog_models(&catalog)["gpt-low-priority"]["limit"]["output"],128000);
    }

    #[test]
    fn migrated_efforts_reach_request_body_and_native_uid_resolver() {
        let dir = tempfile::tempdir().unwrap();
        let mut migration = super::super::backend_migration::Migration::load(dir.path(), &HashMap::new()).unwrap();
        let choices: Vec<_> = ["high", "low", "none"].into_iter().map(|effort| {
            (effort, migration.target("devin", Some(&format!("gpt-low-priority:{effort}")), None).unwrap())
        }).collect();
        let default = migration.target("devin", Some("gpt-low-priority"), None).unwrap();
        migration.save().unwrap();
        let mut config: Value = serde_json::from_slice(&std::fs::read(dir.path().join("alkaid/config.jsonc")).unwrap()).unwrap();
        let catalog = Catalog { loaded: Instant::now(), version: "1.2.3".into(), families: vec![json!({"slug":"gpt","variants":[
            {"model_uid":"gpt-low-priority"}, {"model_uid":"gpt-high-priority"}, {"model_uid":"gpt-none-priority"}]})] };
        let provider = &mut config["provider"]["devin"];
        provider["api"] = json!("devin");
        provider["options"] = json!({"baseURL":"https://example.invalid","supportsReasoningEffort":false,"supportsThinkingToggle":false});
        // Fixture for expanded native config: catalog defaults, then explicit per-model overrides.
        // The assertions below exercise real config resolution, request construction and UID resolution.
        let mut models = catalog_models(&catalog);
        for (id, model) in provider["models"].as_object().unwrap() {
            models[id]["variants"] = model["variants"].clone();
            models[id]["options"].as_object_mut().unwrap().extend(model["options"].as_object().unwrap().clone());
        }
        provider["models"] = json!(models);
        for (effort, selected) in choices {
            let resolved = super::super::config::resolve_model(&config, Some(&selected), &HashMap::new()).unwrap();
            assert_eq!(resolved.model.id, "gpt-low-priority");
            let body = provider::completions_body(&resolved.model, "test", &[], &[], resolved.thinking_level.as_deref(), None);
            assert_eq!(body["reasoning_effort"], effort);
            assert_eq!(resolve_model(&catalog, &resolved.model.id, body["reasoning_effort"].as_str()).unwrap(), format!("gpt-{effort}-priority"));
        }
        let resolved = super::super::config::resolve_model(&config, Some(&default), &HashMap::new()).unwrap();
        let body = provider::completions_body(&resolved.model, "test", &[], &[], resolved.thinking_level.as_deref(), None);
        assert!(body.get("reasoning_effort").is_none(), "explicit variants must not set a shared default effort");
    }

    #[tokio::test]
    async fn native_http_request_and_cancellation() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap(); let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut socket,_) = listener.accept().unwrap(); socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut data = Vec::new(); let mut byte = [0];
            while !data.ends_with(b"\r\n\r\n") { socket.read_exact(&mut byte).unwrap(); data.push(byte[0]); }
            let headers = String::from_utf8(data).unwrap(); assert!(headers.starts_with(&format!("POST {RPC} "))); assert!(headers.contains("application/connect+proto"));
            let length: usize = headers.lines().find_map(|l| l.to_lowercase().strip_prefix("content-length: ").map(str::to_string)).unwrap().parse().unwrap();
            let mut body = vec![0;length]; socket.read_exact(&mut body).unwrap(); assert!(decode(&body[5..]).is_ok());
            let mut payload = Vec::new(); bytes(&mut payload,3,"hello"); number(&mut payload,5,1);
            let wire = [envelope(&payload,0),envelope(b"{}",2)].concat();
            write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: application/connect+proto\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",wire.len()).unwrap(); socket.write_all(&wire).unwrap();
        });
        let http = provider::local_api_client(); let model = model(&format!("http://{address}"));
        let body = json!({"model":"test","messages":[{"role":"user","content":"hello"}]});
        let result = inference(&http,&model,"secret",&body,"1.2.3",&Arc::new(AtomicBool::new(false)),&mut |_|{}).await.unwrap();
        assert_eq!(result.content[0]["text"],"hello"); server.join().unwrap();
        let error = tokio::time::timeout(Duration::from_secs(1),inference(&http,&model,"secret",&body,"1.2.3",&Arc::new(AtomicBool::new(true)),&mut |_|{})).await.unwrap();
        assert!(error.is_err());
    }
}
