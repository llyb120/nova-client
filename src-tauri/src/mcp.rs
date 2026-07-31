//! Minimal MCP (Model Context Protocol) stdio client for the native Vega path,
//! porting the bridge's `connectMcpServers` + `mcpResult` (alkaid-core.mjs).
//!
//! Each configured server (`{data_dir}/alkaid/mcp.json`, or the
//! `ALKAID_MCP_SERVERS` env override) is spawned as a child process speaking
//! newline-delimited JSON-RPC 2.0 over stdio. The hub performs the `initialize`
//! handshake, lists tools (exposed to the model as `mcp__<server>__<tool>`), and
//! dispatches `tools/call`. Tool results are converted to pi format with output
//! clamping, matching `mcpResult`.
//!
//! Verification status: compiles and is wired into the native turn, but cannot
//! be exercised here without a live MCP server; it is pending live verification
//! alongside the provider transports.

use pi_core::text::{clamp_tool_output_text, OPENAI_TOOL_OUTPUT_SAFE_MAX_CHARS};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{oneshot, Mutex};

const MCP_PROTOCOL_VERSION: &str = "2024-11-05";

/// One configured MCP server (a subset of the JSON shape in `mcp.json`).
#[derive(Debug, Clone)]
pub struct McpServerConfig {
    pub command: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
}

/// Load the MCP server map: `ALKAID_MCP_SERVERS` env first, else
/// `{data_dir}/alkaid/mcp.json` (absent/invalid → empty).
pub fn load_mcp_config(data_dir: &Path) -> HashMap<String, McpServerConfig> {
    let raw = if let Ok(configured) = std::env::var("ALKAID_MCP_SERVERS") {
        configured
    } else {
        let path = data_dir.join("alkaid").join("mcp.json");
        match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(_) => return HashMap::new(),
        }
    };
    let Ok(parsed) = serde_json::from_str::<Value>(&raw) else {
        return HashMap::new();
    };
    let Some(obj) = parsed.as_object() else {
        return HashMap::new();
    };
    let mut servers = HashMap::new();
    for (name, value) in obj {
        let Some(command) = value.get("command").and_then(Value::as_str) else {
            continue;
        };
        let args = value
            .get("args")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let env = value
            .get("env")
            .and_then(Value::as_object)
            .map(|obj| {
                obj.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        servers.insert(
            name.clone(),
            McpServerConfig {
                command: command.to_string(),
                args,
                env,
            },
        );
    }
    servers
}

type PendingMap = Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>;

/// A live connection to one MCP server.
struct McpConnection {
    stdin: Arc<Mutex<tokio::process::ChildStdin>>,
    pending: PendingMap,
    next_id: AtomicU64,
    #[allow(dead_code)]
    child: Arc<Mutex<Child>>,
}

impl McpConnection {
    /// Spawn the server, start the reader task, and run the initialize
    /// handshake.
    async fn connect(config: &McpServerConfig, cwd: &str) -> Result<Self, String> {
        let mut cmd = Command::new(&config.command);
        cmd.args(&config.args)
            .current_dir(cwd)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        if !config.env.is_empty() {
            cmd.envs(&config.env);
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("启动 MCP server 失败 ({}): {e}", config.command))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "MCP server 缺少 stdin".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "MCP server 缺少 stdout".to_string())?;

        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let reader_pending = Arc::clone(&pending);
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let Ok(msg) = serde_json::from_str::<Value>(trimmed) else {
                    continue;
                };
                // Dispatch responses (have an id) to their pending caller.
                if let Some(id) = msg.get("id").and_then(Value::as_u64) {
                    let mut map = reader_pending.lock().await;
                    if let Some(sender) = map.remove(&id) {
                        let _ = sender.send(msg);
                    }
                }
            }
        });

        let connection = McpConnection {
            stdin: Arc::new(Mutex::new(stdin)),
            pending,
            next_id: AtomicU64::new(1),
            child: Arc::new(Mutex::new(child)),
        };

        // initialize handshake.
        let init_result = connection
            .request(
                "initialize",
                json!({
                    "protocolVersion": MCP_PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": { "name": "alkaid", "version": "0.1.0" },
                }),
            )
            .await?;
        if init_result.get("error").is_some() {
            return Err(format!(
                "MCP initialize 失败: {}",
                init_result["error"].get("message").and_then(Value::as_str).unwrap_or("unknown")
            ));
        }
        connection
            .notify("notifications/initialized", json!({}))
            .await?;
        Ok(connection)
    }

    async fn send_line(&self, value: &Value) -> Result<(), String> {
        let mut line = serde_json::to_string(value).map_err(|e| e.to_string())?;
        line.push('\n');
        let mut stdin = self.stdin.lock().await;
        stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| format!("写入 MCP server 失败: {e}"))?;
        stdin.flush().await.map_err(|e| e.to_string())?;
        Ok(())
    }

    async fn notify(&self, method: &str, params: Value) -> Result<(), String> {
        self.send_line(&json!({ "jsonrpc": "2.0", "method": method, "params": params }))
            .await
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        self.send_line(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
            .await?;
        let response = rx
            .await
            .map_err(|_| "MCP server 连接已关闭".to_string())?;
        if let Some(error) = response.get("error") {
            return Err(format!(
                "MCP {method} 错误: {}",
                error.get("message").and_then(Value::as_str).unwrap_or("unknown")
            ));
        }
        Ok(response.get("result").cloned().unwrap_or(Value::Null))
    }

    async fn list_tools(&self) -> Result<Vec<Value>, String> {
        let result = self.request("tools/list", json!({})).await?;
        Ok(result
            .get("tools")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    async fn call_tool(&self, name: &str, arguments: &Value) -> Result<Value, String> {
        self.request(
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        )
        .await
    }
}

/// Convert an MCP `tools/call` result to a pi tool result (port of `mcpResult`).
fn mcp_result(result: &Value) -> (Value, bool) {
    let is_error = result.get("isError").and_then(Value::as_bool).unwrap_or(false);
    let content = result.get("content").and_then(Value::as_array);
    let mut parts: Vec<Value> = Vec::new();
    if let Some(content) = content {
        for part in content {
            match part.get("type").and_then(Value::as_str) {
                Some("text") => {
                    let text = part.get("text").and_then(Value::as_str).unwrap_or("");
                    parts.push(json!({ "type": "text", "text": clamp_tool_output_text(Some(text), OPENAI_TOOL_OUTPUT_SAFE_MAX_CHARS) }));
                }
                Some("image") => {
                    parts.push(json!({
                        "type": "image",
                        "data": part.get("data").and_then(Value::as_str).unwrap_or(""),
                        "mimeType": part.get("mimeType").and_then(Value::as_str).unwrap_or("image/png"),
                    }));
                }
                _ => {
                    parts.push(json!({
                        "type": "text",
                        "text": clamp_tool_output_text(Some(&part.to_string()), OPENAI_TOOL_OUTPUT_SAFE_MAX_CHARS),
                    }));
                }
            }
        }
    }
    if parts.is_empty() {
        parts.push(json!({ "type": "text", "text": "MCP 工具执行完成" }));
    }
    (json!({ "content": parts, "details": result }), is_error)
}

/// An advertised MCP tool definition (`mcp__<server>__<tool>`).
struct McpTool {
    definition: Value,
}

/// All connected MCP servers and their aggregated tools.
pub struct McpHub {
    connections: HashMap<String, Arc<McpConnection>>,
    tools: Vec<McpTool>,
}

impl McpHub {
    /// Connect every configured server and list their tools. A server that
    /// fails to connect is skipped with a stderr warning (the bridge throws,
    /// but degrading gracefully keeps the turn alive).
    pub async fn connect(
        servers: HashMap<String, McpServerConfig>,
        cwd: &str,
    ) -> McpHub {
        let mut connections = HashMap::new();
        let mut tools = Vec::new();
        let mut names: Vec<String> = servers.keys().cloned().collect();
        names.sort();
        for name in names {
            let config = &servers[&name];
            match McpConnection::connect(config, cwd).await {
                Ok(connection) => {
                    let connection = Arc::new(connection);
                    match connection.list_tools().await {
                        Ok(listed) => {
                            for tool in listed {
                                let tool_name =
                                    tool.get("name").and_then(Value::as_str).unwrap_or("").to_string();
                                let definition = json!({
                                    "name": format!("mcp__{name}__{tool_name}"),
                                    "description": tool.get("description").and_then(Value::as_str)
                                        .map(String::from)
                                        .unwrap_or_else(|| format!("MCP {name} / {tool_name}")),
                                    "parameters": tool.get("inputSchema").cloned()
                                        .unwrap_or(json!({ "type": "object", "properties": {} })),
                                });
                                tools.push(McpTool {
                                    definition,
                                });
                            }
                            connections.insert(name.clone(), connection);
                        }
                        Err(error) => {
                            eprintln!("Vega MCP {name} tools/list 失败: {error}");
                        }
                    }
                }
                Err(error) => {
                    eprintln!("Vega MCP {name} 连接失败: {error}");
                }
            }
        }
        McpHub { connections, tools }
    }

    /// Tool definitions to advertise to the model.
    pub fn tool_definitions(&self) -> Vec<Value> {
        self.tools.iter().map(|t| t.definition.clone()).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// Dispatch an `mcp__<server>__<tool>` call. Returns the pi tool result and
    /// an error flag.
    pub async fn call_tool(&self, mcp_name: &str, arguments: &Value) -> (Value, bool) {
        // mcp__<server>__<tool>: split into server and tool (tool may contain __).
        let stripped = mcp_name.strip_prefix("mcp__").unwrap_or(mcp_name);
        let Some((server, tool)) = stripped.split_once("__") else {
            return (
                json!({ "content": [{ "type": "text", "text": format!("未知 MCP 工具: {mcp_name}") }] }),
                true,
            );
        };
        let Some(connection) = self.connections.get(server) else {
            return (
                json!({ "content": [{ "type": "text", "text": format!("MCP server 未连接: {server}") }] }),
                true,
            );
        };
        match connection.call_tool(tool, arguments).await {
            Ok(result) => mcp_result(&result),
            Err(error) => (
                json!({ "content": [{ "type": "text", "text": format!("MCP 调用失败: {error}") }] }),
                true,
            ),
        }
    }
}
