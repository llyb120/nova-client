//! Single-turn Devin decision transport. Never use the interactive ACP manager here.
//! The caller owns `cwd` (a fresh private temporary directory); this module owns the child.
use super::{apply_proxy_env, assign_to_agent_job, codebuddy_command, kill_process_tree, split_model_effort};
use base64::Engine;
use serde_json::{json, Value};
use std::{collections::HashMap, fs, path::{Path, PathBuf}, process::Stdio, time::Duration};
use tokio::{io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader}, process::Child};

const INPUT_LIMIT: usize = 32 * 1024 * 1024;
const OUTPUT_LIMIT: usize = 2 * 1024 * 1024;

fn env_value(env: &HashMap<String, String>, name: &str) -> Option<String> {
    env.iter().find(|(key, _)| key.eq_ignore_ascii_case(name)).map(|(_, v)| v.clone())
        .or_else(|| std::env::var(name).ok()).filter(|s| !s.is_empty())
}

fn user_home(env: &HashMap<String, String>) -> Result<PathBuf, String> {
    env_value(env, if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from).ok_or_else(|| "无法定位后端认证目录".into())
}

fn write(path: &Path, data: impl AsRef<[u8]>) -> Result<(), String> {
    fs::create_dir_all(path.parent().ok_or("无效的隔离目录")?).map_err(|_| "创建隔离配置目录失败")?;
    fs::write(path, data).map_err(|_| "写入隔离配置失败".into())
}

fn read_optional(path: &Path) -> Result<Option<String>, String> {
    use std::io::Read;
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("读取后端认证/模型配置失败".into()),
    };
    let mut text = String::new();
    file.take(INPUT_LIMIT as u64 + 1).read_to_string(&mut text).map_err(|_| "读取后端认证/模型配置失败")?;
    if text.len() > INPUT_LIMIT { return Err("后端配置超过大小限制".into()); }
    Ok(Some(text))
}

fn devin_config(text: &str) -> Result<String, String> {
    let value: Value = serde_json::from_str(text).map_err(|_| "Devin 模型配置不是有效 JSON")?;
    let mut result = json!({"version":1});
    if let Some(version) = value.get("version") { result["version"] = version.clone(); }
    for (section, keys) in [("agent", &["model", "preferred_family_models", "thinking"][..]), ("devin", &["org_id"][..])] {
        for key in keys {
            if let Some(v) = value[section].get(*key) { result[section][*key] = v.clone(); }
        }
    }
    Ok(result.to_string())
}

fn devin_credentials(text: &str) -> Result<String, String> {
    let mut value: toml::Value = toml::from_str(text).map_err(|_| "Devin 凭据不是有效 TOML")?;
    let table = value.as_table_mut().ok_or("Devin 凭据必须是表")?;
    // Only the static API-key format is copied. Never copy/rotate an OAuth refresh
    // token in a disposable home; a future OAuth format must share its live store.
    if !table.get("windsurf_api_key").and_then(toml::Value::as_str).is_some_and(|key| !key.is_empty()) {
        return Err("Devin 助理需要现有 API-key 登录；当前凭据格式不能安全隔离".into());
    }
    table.retain(|k, _| matches!(k, "windsurf_api_key" | "api_server_url" | "devin_webapp_host" | "devin_api_url"));
    toml::to_string(&value).map_err(|_| "编码 Devin 隔离凭据失败".into())
}

fn prompt_blocks(prompt: String, images: &[Value]) -> Result<Vec<Value>, String> {
    let mut size = prompt.len();
    if size > INPUT_LIMIT { return Err("无工具生成输入超过 32 MiB".into()); }
    let mut blocks = vec![json!({"type":"text", "text":prompt})];
    for image in images {
        let data = image["data"].as_str().filter(|s| !s.is_empty()).ok_or("图片缺少内联数据")?;
        size = size.checked_add(data.len()).ok_or("输入过大")?;
        if size > INPUT_LIMIT { return Err("无工具生成输入超过 32 MiB".into()); }
        if image["type"] != "image" || !matches!(image["mimeType"].as_str(), Some("image/png" | "image/jpeg" | "image/gif" | "image/webp")) {
            return Err("无工具生成仅接受内联 PNG/JPEG/GIF/WebP 图片".into());
        }
        base64::engine::general_purpose::STANDARD.decode(data).map_err(|_| "图片 base64 数据无效")?;
        // Do not forward uri/resource references: there is no filesystem callback.
        blocks.push(json!({"type":"image", "mimeType":image["mimeType"], "data":data}));
    }
    if serde_json::to_vec(&blocks).map_err(|_| "编码输入失败")?.len() > INPUT_LIMIT {
        return Err("无工具生成编码后输入超过 32 MiB".into());
    }
    Ok(blocks)
}

struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        // An already reaped child no longer owns its PID.
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            if let Some(pid) = self.0.id() { kill_process_tree(pid); }
            let _ = self.0.start_kill();
        }
    }
}

/// `cwd` must be a fresh caller-owned temporary directory, never a project directory.
pub(super) async fn complete(
    program: &str, model: &str, prompt: String, images: &[Value], cwd: &Path,
    launch_env: &HashMap<String, String>, proxy: &str, deadline: Duration,
) -> Result<String, String> {
    let blocks = prompt_blocks(prompt, images)?;
    let original_home = user_home(launch_env)?;
    let home = cwd.join("home");
    fs::create_dir(&home).map_err(|_| "助理隔离目录必须为空且可写")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).map_err(|_| "无法保护助理隔离目录")?;
    }
    // Windows Known Folder APIs expand USERPROFILE/AppData, ignoring an arbitrary
    // APPDATA override. Match that layout or Devin fails before ACP initialization.
    let config = home.join(if cfg!(windows) { "AppData/Roaming" } else { "config" });
    let data = home.join(if cfg!(windows) { "AppData/Local" } else { "data" });
    for dir in [&config, &data, &home.join("cache")] {
        fs::create_dir_all(dir).map_err(|_| "创建后端隔离数据目录失败")?;
    }
    let (base_model, effort) = split_model_effort(Some(model.trim()));
    let args = {
        let platform_config = if cfg!(windows) {
            env_value(launch_env, "APPDATA").map(PathBuf::from).ok_or("APPDATA 不可用")?
        } else {
            env_value(launch_env, "XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| original_home.join(".config"))
        };
        // DEVIN_CONFIG_HOME is the parent of devin/, not devin/ itself.
        let original = env_value(launch_env, "DEVIN_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| platform_config.clone()).join("devin");
        write(&config.join("devin/config.json"), devin_config(&read_optional(&original.join("config.json"))?.unwrap_or_else(|| "{}".into()))?)?;
        if let Some(credentials) = read_optional(&platform_config.join("devin/credentials.toml"))? {
            write(&config.join("devin/credentials.toml"), devin_credentials(&credentials)?)?;
        }
        let mut args = vec!["acp".into(), "--agent-type".into(), "summarizer".into()];
        if let Some(model) = &base_model { args.extend(["--model".into(), model.clone()]); }
        args
    };
    let (_, mut command) = codebuddy_command(program, &args.iter().map(String::as_str).collect::<Vec<_>>());
    command.current_dir(cwd).envs(launch_env);
    for key in std::env::vars_os().map(|(k, _)| k).chain(launch_env.keys().map(std::ffi::OsString::from)) {
        if key.to_string_lossy().to_ascii_uppercase().starts_with("NOVA_") { command.env_remove(key); }
    }
    command.env("HOME", &home).env("USERPROFILE", &home)
        .env("APPDATA", &config).env("LOCALAPPDATA", &data)
        .env("XDG_CONFIG_HOME", &config).env("XDG_DATA_HOME", &data)
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env("DEVIN_CONFIG_HOME", &config)
        .env_remove("NODE_OPTIONS").env_remove("DEVIN_REFUSAL_FALLBACK")
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    apply_proxy_env(&mut command, proxy);
    #[cfg(windows)]
    command.creation_flags(0x0800_0000);
    #[cfg(unix)]
    command.process_group(0);
    let mut process = Process(command.spawn().map_err(|_| "无法启动受限 CLI，请检查已配置的后端路径")?);
    assign_to_agent_job(&process.0);
    let mut stdin = process.0.stdin.take().ok_or("助理 stdin 不可用")?;
    let mut stdout = BufReader::new(process.0.stdout.take().ok_or("助理 stdout 不可用")?);
    let stderr = process.0.stderr.take().ok_or("助理 stderr 不可用")?;
    let result = tokio::time::timeout(deadline, async {
        tokio::select! {
            result = exchange(&mut stdin, &mut stdout, cwd, effort.as_deref(), &blocks) => result,
            _ = async { let mut bytes = Vec::new(); stderr.take(256 * 1024 + 1).read_to_end(&mut bytes).await } =>
                Err("受限 CLI 提前退出或诊断输出超限；请检查登录、模型及 CLI 版本".into()),
        }
    }).await.map_err(|_| "受限助理生成超时，已终止进程树，未重试".to_string())?;
    // ACP servers stay alive after a turn. Drop also handles cancellation/error paths.
    drop(process);
    result
}

async fn request<W: AsyncWrite + Unpin, R: AsyncRead + Unpin>(
    stdin: &mut W, stdout: &mut BufReader<R>, id: u64, method: &str, params: Value,
    size: &mut usize, text: &mut String, session: Option<&str>,
) -> Result<Value, String> {
    let wire = format!("{}\n", json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params}));
    stdin.write_all(wire.as_bytes()).await.map_err(|_| "写入受限 ACP 请求失败")?;
    stdin.flush().await.map_err(|_| "刷新受限 ACP 请求失败")?;
    loop {
        let mut line = String::new();
        let count = (&mut *stdout).take((OUTPUT_LIMIT - *size + 1) as u64).read_line(&mut line).await.map_err(|_| "读取受限 ACP 输出失败")?;
        *size += count;
        if *size > OUTPUT_LIMIT { return Err("受限 ACP 输出超过 2 MiB".into()); }
        if count == 0 { return Err("受限 ACP 提前结束，未采用部分回复".into()); }
        let message: Value = serde_json::from_str(&line).map_err(|_| "受限 ACP 返回无效 JSON")?;
        if message.get("method").is_some() && message.get("id").is_some() {
            // Including fs/*, terminal/* and permission requests: never delegate to AcpManager.
            let denied = format!("{}\n", json!({"jsonrpc":"2.0", "id":message["id"], "error":{"code":-32601,"message":"Restricted assistant has no client capabilities"}}));
            let _ = stdin.write_all(denied.as_bytes()).await;
            return Err("受限助理请求了未授权客户端能力，已拒绝".into());
        }
        if message["method"] == "session/update" {
            if session.is_some_and(|sid| message["params"]["sessionId"] != sid) { return Err("受限 ACP 会话不匹配".into()); }
            let update = &message["params"]["update"];
            match update["sessionUpdate"].as_str() {
                Some("tool_call" | "tool_call_update") => return Err("无工具助理出现工具调用，已拒绝".into()),
                Some("agent_message_chunk") => {
                    if update["content"]["type"] != "text" { return Err("无工具助理返回非文本结果".into()); }
                    text.push_str(update["content"]["text"].as_str().ok_or("助理文本块无效")?);
                }
                _ => {}
            }
        } else if message["id"] == id {
            if message.get("error").is_some() { return Err("受限 ACP 请求失败；请检查登录、模型及受限 CLI 支持".into()); }
            return message.get("result").cloned().ok_or_else(|| "受限 ACP 缺少结果".into());
        }
    }
}

async fn exchange<W: AsyncWrite + Unpin, R: AsyncRead + Unpin>(
    stdin: &mut W, stdout: &mut BufReader<R>, cwd: &Path, effort: Option<&str>, blocks: &[Value],
) -> Result<String, String> {
    let mut size = 0;
    let mut text = String::new();
    let init = request(stdin, stdout, 1, "initialize", json!({"protocolVersion":1,"clientInfo":{"name":"nova-decision","version":"1"},"clientCapabilities":{"fs":{"readTextFile":false,"writeTextFile":false},"terminal":false}}), &mut size, &mut text, None).await?;
    if init["protocolVersion"] != 1 { return Err("受限 ACP 协议版本不兼容".into()); }
    if blocks.iter().any(|b| b["type"] == "image") && init["agentCapabilities"]["promptCapabilities"]["image"] != true {
        return Err("该后端未声明支持图片，未发送缺失图片的请求".into());
    }
    let session = request(stdin, stdout, 2, "session/new", json!({"cwd":cwd,"mcpServers":[]}), &mut size, &mut text, None).await?;
    let sid = session["sessionId"].as_str().filter(|s| !s.is_empty()).ok_or("受限 ACP 缺少 sessionId")?;
    if let Some(effort) = effort {
        let option = session["configOptions"].as_array().and_then(|xs| xs.iter().find(|x| {
            x["category"] == "thought_level" || matches!(x["id"].as_str(), Some("thinking" | "thought_level" | "reasoning_effort"))
        })).and_then(|x| x["id"].as_str()).ok_or("受限后端未声明所选推理级别，未静默忽略")?;
        request(stdin, stdout, 4, "session/set_config_option", json!({"sessionId":sid,"configId":option,"value":effort}), &mut size, &mut text, Some(sid)).await?;
    }
    text.clear();
    let result = request(stdin, stdout, 5, "session/prompt", json!({"sessionId":sid,"prompt":blocks}), &mut size, &mut text, Some(sid)).await?;
    if result["stopReason"] != "end_turn" || text.trim().is_empty() {
        return Err("受限助理未正常完成，未采用空结果或部分回复".into());
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn isolated_config_keeps_models_not_extensions() {
        let d: Value = serde_json::from_str(&devin_config(r#"{"agent":{"model":"small","hooks":"bad"},"devin":{"org_id":"org"},"mcpServers":{"evil":{}}}"#).unwrap()).unwrap();
        assert_eq!(d["agent"]["model"], "small");
        assert!(d["agent"].get("hooks").is_none() && d.get("mcpServers").is_none());
        let credentials = devin_credentials("windsurf_api_key='test-key'\nrefresh_token='never-copy'\napi_server_url='https://example.test'\n").unwrap();
        assert!(credentials.contains("test-key") && credentials.contains("https://example.test"));
        assert!(!credentials.contains("never-copy") && !credentials.contains("refresh_token"));
        assert!(devin_credentials("refresh_token='never-copy'").is_err());
    }
    #[test]
    fn images_are_inline_or_rejected_not_dropped() {
        let image = json!({"type":"image","mimeType":"image/png","data":"YQ==","uri":"file:///secret"});
        let b = prompt_blocks("decide".into(), &[image]).unwrap();
        assert_eq!(b.len(), 2);
        assert_eq!(b[1]["data"], "YQ==");
        assert!(b[1].get("uri").is_none());
        assert!(prompt_blocks("decide".into(), &[json!({"type":"image","mimeType":"image/png","data":"!"})]).is_err());
    }
    #[tokio::test]
    async fn mock_rpc_refuses_capabilities_tools_and_partial_results() {
        for line in [
            json!({"jsonrpc":"2.0","id":9,"method":"fs/read_text_file","params":{"path":"secret"}}),
            json!({"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"tool_call","title":"read"}}}),
            json!({"id":1,"error":{"message":"credential must never leak"}}),
        ] {
            let bytes = format!("{line}\n");
            let mut reader = BufReader::new(bytes.as_bytes());
            let mut out = Vec::new();
            let mut size = 0;
            let mut text = String::new();
            let err = request(&mut out, &mut reader, 1, "initialize", json!({}), &mut size, &mut text, Some("s")).await.unwrap_err();
            assert!(!err.contains("credential"));
        }
        let wire = concat!(
            "{\"id\":1,\"result\":{\"protocolVersion\":1}}\n",
            "{\"id\":2,\"result\":{\"sessionId\":\"s\"}}\n",
            "{\"method\":\"session/update\",\"params\":{\"sessionId\":\"s\",\"update\":{\"sessionUpdate\":\"agent_message_chunk\",\"content\":{\"type\":\"text\",\"text\":\"OK\"}}}}\n",
            "{\"id\":5,\"result\":{\"stopReason\":\"end_turn\"}}\n");
        let mut out = Vec::new();
        assert_eq!(exchange(&mut out, &mut BufReader::new(wire.as_bytes()), Path::new("/isolated"), None, &[]).await.unwrap(), "OK");
        let sent = String::from_utf8(out).unwrap();
        assert!(sent.contains("\"mcpServers\":[]") && sent.contains("\"readTextFile\":false"));
        let partial = wire.replace("end_turn", "max_tokens");
        assert!(exchange(&mut Vec::new(), &mut BufReader::new(partial.as_bytes()), Path::new("/isolated"), None, &[]).await.is_err());
    }
}
