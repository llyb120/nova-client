//! MCP stdio adapter for the existing Rust context service. No separate index or Node runtime.
use serde_json::{json, Value};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

#[derive(Clone)]
pub(super) struct Config {
    pub endpoint: String,
    pub token: String,
    pub root: String,
}

fn tool() -> Value {
    json!({"name":"polaris","description":"需要跨文件查找、分析修改位置或读取多个文件时先调用：按 keywords/task/files 打包完整代码单元、依赖和 IMPACT 调用方。已展示范围视为已读，只补 coverage gaps / next_reads，不重复搜索相同关键词。返回 CTX MISS 时按 next 提示修正符号或传 files 重试。",
        "inputSchema":{"type":"object","properties":{
            "keywords":{"anyOf":[{"type":"array","minItems":1,"items":{"type":"string","minLength":1}},{"type":"string","minLength":1}],"description":"关键词或符号名，最多取前 5 项"},
            "query":{"type":"string","minLength":1,"description":"简短检索词"},
            "task":{"type":"string","minLength":1,"description":"自然语言任务描述"},
            "files":{"type":"array","minItems":1,"items":{"type":"string","minLength":1}},
            "maxChars":{"type":"integer","minimum":4000,"maximum":80000},
            "budget":{"type":"integer","minimum":100,"maximum":4000},
            "coupling":{"type":"boolean"}
        },"anyOf":[{"required":["keywords"]},{"required":["query"]},{"required":["task"]},{"required":["files"]}],"additionalProperties":false}})
}

fn normalize(mut args: Value) -> Result<Value, String> {
    if !args.is_object() {
        return Err("Polaris arguments must be an object".into());
    }
    fn strings(value: &Value, limit: usize) -> Vec<String> {
        let values = match value {
            Value::Array(a) => a.clone(),
            Value::String(_) => vec![value.clone()],
            _ => Vec::new(),
        };
        let mut output = Vec::new();
        for value in values {
            if let Some(s) = value.as_str().map(str::trim).filter(|s| !s.is_empty()) {
                if !output.iter().any(|v| v == s) {
                    output.push(s.to_string());
                }
                if output.len() == limit {
                    break;
                }
            }
        }
        output
    }
    let query = args["query"].as_str().unwrap_or_default().trim();
    let task = args["task"].as_str().unwrap_or_default().trim();
    let task = if task.is_empty() && query.contains(' ') {
        query
    } else {
        task
    }
    .to_string();
    let mut keywords = strings(&args["keywords"], 5);
    if keywords.is_empty() && !query.is_empty() && task.is_empty() {
        keywords.push(query.to_string());
    }
    let files = strings(&args["files"], 6);
    if keywords.is_empty() && task.is_empty() && files.is_empty() {
        return Err("Provide keywords, query, task or files".into());
    }
    args["keywords"] = json!(keywords);
    args["task"] = json!(task);
    args["files"] = json!(files);
    Ok(args)
}

async fn exchange<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    config: &Config,
    args: Value,
) -> Result<Value, String> {
    let message = json!({"token":config.token,"method":"polaris","root":config.root,"params":args});
    stream
        .write_all(format!("{message}\n").as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    let mut line = String::new();
    BufReader::new(stream)
        .read_line(&mut line)
        .await
        .map_err(|e| e.to_string())?;
    let value: Value = serde_json::from_str(&line).map_err(|e| e.to_string())?;
    if value["ok"] != true {
        return Err(value["error"]
            .as_str()
            .unwrap_or("Context service failed")
            .into());
    }
    Ok(value["result"].clone())
}

async fn call(config: Config, args: Value) -> Result<Value, String> {
    let args = normalize(args)?;
    tokio::time::timeout(Duration::from_secs(120), async {
        let until = tokio::time::Instant::now() + Duration::from_secs(3);
        loop {
            #[cfg(windows)]
            let stream =
                tokio::net::windows::named_pipe::ClientOptions::new().open(&config.endpoint);
            #[cfg(unix)]
            let stream = tokio::net::UnixStream::connect(&config.endpoint).await;
            match stream {
                Ok(stream) => return exchange(stream, &config, args).await,
                Err(error) if tokio::time::Instant::now() >= until => {
                    return Err(format!("连接 Rust 上下文服务失败：{error}"))
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
            }
        }
    })
    .await
    .map_err(|_| "Polaris request timed out".to_string())?
}

fn response(id: Value, result: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"result":result})
}
fn error(id: Value, code: i32, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}

pub(super) async fn serve<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    reader: R,
    mut writer: W,
    config: Config,
) -> Result<(), String> {
    let mut lines = BufReader::new(reader).lines();
    let mut tasks = tokio::task::JoinSet::new();
    loop {
        let reply = tokio::select! {
            completed = tasks.join_next(), if !tasks.is_empty() => Some(completed.ok_or("MCP worker missing")?.map_err(|e| e.to_string())?),
            line = lines.next_line() => {
                let Some(line) = line.map_err(|e| e.to_string())? else { break; };
                let message: Value = match serde_json::from_str(&line) {
                    Ok(value) => value,
                    Err(_) => { writer.write_all(format!("{}\n", error(Value::Null, -32700, "Invalid JSON")).as_bytes()).await.map_err(|e| e.to_string())?; continue; }
                };
                let Some(id) = message.get("id").cloned() else { continue; };
                if message["jsonrpc"] != "2.0" { Some(error(id, -32600, "Invalid JSON-RPC request")) }
                else { match message["method"].as_str().unwrap_or_default() {
                    "initialize" => {
                        let requested = message["params"]["protocolVersion"].as_str().unwrap_or_default();
                        let version = match requested { "2024-11-05" | "2025-03-26" | "2025-06-18" | "2025-11-25" => requested, _ => "2025-06-18" };
                        Some(response(id, json!({"protocolVersion":version,"capabilities":{"tools":{}},"serverInfo":{"name":"nova-tools","version":env!("CARGO_PKG_VERSION")}})))
                    }
                    "ping" => Some(response(id, json!({}))),
                    "tools/list" => Some(response(id, json!({"tools":[tool()]}))),
                    "tools/call" => {
                        if tasks.len() >= 16 { Some(error(id, -32000, "Too many concurrent tool calls")) }
                        else {
                            let config = config.clone(); let params = message["params"].clone();
                            tasks.spawn(async move {
                                let result = if params["name"] == "polaris" { call(config, params.get("arguments").cloned().unwrap_or_else(|| json!({}))).await } else { Err("Unknown tool".into()) };
                                let (text, failed) = match result { Ok(value) => (value.as_str().map(str::to_string).unwrap_or_else(|| value.to_string()), false), Err(error) => (error, true) };
                                response(id, json!({"isError":failed,"content":[{"type":"text","text":text}]}))
                            });
                            None
                        }
                    }
                    _ => Some(error(id, -32601, "Method not found")),
                } }
            }
        };
        if let Some(reply) = reply {
            writer
                .write_all(format!("{reply}\n").as_bytes())
                .await
                .map_err(|e| e.to_string())?;
            writer.flush().await.map_err(|e| e.to_string())?;
        }
    }
    // EOF aborts outstanding socket requests; no detached context-client tasks.
    tasks.abort_all();
    Ok(())
}

pub(crate) fn maybe_run() -> bool {
    if std::env::args_os().nth(1).as_deref() != Some(std::ffi::OsStr::new("__codex-mcp")) {
        return false;
    }
    let result = (|| {
        let config = Config {
            endpoint: std::env::var("NOVA_CONTEXT_SERVICE_ENDPOINT")
                .map_err(|_| "Missing context endpoint".to_string())?,
            token: std::env::var("NOVA_CONTEXT_SERVICE_TOKEN")
                .map_err(|_| "Missing context token".to_string())?,
            root: std::env::var("NOVA_TOOLS_CWD")
                .map_err(|_| "Missing tool workspace".to_string())?,
        };
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?
            .block_on(serve(tokio::io::stdin(), tokio::io::stdout(), config))
    })();
    if let Err(error) = result {
        eprintln!("Nova native MCP: {error}");
        std::process::exit(1);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn native_mcp_negotiates_lists_tools_and_reports_errors() {
        let (client, server) = tokio::io::duplex(65536);
        let (reader, writer) = tokio::io::split(server);
        let task = tokio::spawn(serve(
            reader,
            writer,
            Config {
                endpoint: "unused".into(),
                token: "unused".into(),
                root: "unused".into(),
            },
        ));
        let (reader, mut writer) = tokio::io::split(client);
        let mut lines = BufReader::new(reader).lines();
        for (id, method, params) in [
            (1, "initialize", json!({"protocolVersion":"2025-06-18"})),
            (2, "tools/list", json!({})),
            (3, "tools/call", json!({"name":"missing"})),
        ] {
            writer
                .write_all(
                    format!(
                        "{}\n",
                        json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            let value: Value =
                serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(value["id"], id);
            match id {
                1 => assert_eq!(value["result"]["protocolVersion"], "2025-06-18"),
                2 => assert_eq!(value["result"]["tools"][0]["name"], "polaris"),
                _ => assert_eq!(value["result"]["isError"], true),
            }
        }
        drop(writer);
        drop(lines);
        task.await.unwrap().unwrap();
    }
    #[test]
    fn query_compatibility_and_bounds() {
        assert_eq!(
            normalize(json!({"query":"Widget"})).unwrap()["keywords"],
            json!(["Widget"])
        );
        assert_eq!(
            normalize(json!({"query":"find Widget"})).unwrap()["task"],
            "find Widget"
        );
        assert_eq!(
            normalize(json!({"keywords":["a","a","b","c","d","e","f"]})).unwrap()["keywords"],
            json!(["a", "b", "c", "d", "e"])
        );
        assert!(normalize(json!({})).is_err());
    }
}
