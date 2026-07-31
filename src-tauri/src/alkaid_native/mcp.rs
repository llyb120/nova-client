use async_trait::async_trait;
use pi::sdk::{ContentBlock, TextContent, Tool, ToolOutput};
use pi::tools::ToolEffects;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[derive(Clone, Deserialize)]
pub struct ServerConfig {
    command: String,
    #[serde(default)] args: Vec<String>,
    #[serde(default)] env: HashMap<String, String>,
}

#[derive(Clone)]
pub struct McpToolSpec {
    name: String,
    tool: String,
    description: String,
    parameters: Value,
    config: ServerConfig,
    cwd: PathBuf,
}

pub fn load(cwd: &Path, root: &Path) -> Result<Vec<McpToolSpec>, String> {
    let value = match std::env::var("ALKAID_MCP_SERVERS") {
        Ok(value) if !value.trim().is_empty() => value,
        _ => match std::fs::read_to_string(root.join("mcp.json")) {
            Ok(value) => value,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(format!("读取 Vega MCP 配置失败：{error}")),
        },
    };
    let servers: HashMap<String, ServerConfig> = serde_json::from_str(&value)
        .map_err(|error| format!("Vega MCP 配置无效：{error}"))?;
    let mut result = Vec::new();
    for (server, config) in servers {
        let mut client = Client::start(&config, cwd)?;
        let listed = client.request("tools/list", json!({}))?;
        for tool in listed.pointer("/result/tools").and_then(Value::as_array).into_iter().flatten() {
            let Some(name) = tool.get("name").and_then(Value::as_str) else { continue };
            result.push(McpToolSpec {
                name: format!("mcp__{server}__{name}"),
                tool: name.into(),
                description: tool.get("description").and_then(Value::as_str).unwrap_or("MCP tool").into(),
                parameters: tool.get("inputSchema").cloned().unwrap_or_else(|| json!({"type":"object"})),
                config: config.clone(), cwd: cwd.to_path_buf(),
            });
        }
        client.shutdown();
    }
    Ok(result)
}

pub fn boxed_tools(specs: &[McpToolSpec]) -> Vec<Box<dyn Tool>> {
    specs.iter().cloned().map(|spec| Box::new(McpTool { spec }) as Box<dyn Tool>).collect()
}

struct Client {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl Client {
    fn start(config: &ServerConfig, cwd: &Path) -> Result<Self, String> {
        let mut command = Command::new(&config.command);
        command.args(&config.args).current_dir(cwd).envs(&config.env)
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        #[cfg(windows)] command.creation_flags(0x0800_0000);
        let mut child = command.spawn().map_err(|e| format!("启动 MCP {} 失败：{e}", config.command))?;
        let stdin = child.stdin.take().ok_or("MCP stdin 不可用")?;
        let stdout = BufReader::new(child.stdout.take().ok_or("MCP stdout 不可用")?);
        let mut client = Self { child, stdin, stdout, next_id: 1 };
        client.request("initialize", json!({
            "protocolVersion":"2025-03-26",
            "capabilities":{},
            "clientInfo":{"name":"nova-vega-native","version":"0.1.0"}
        }))?;
        client.notify("notifications/initialized", json!({}))?;
        Ok(client)
    }

    fn write(&mut self, value: &Value) -> Result<(), String> {
        serde_json::to_writer(&mut self.stdin, value).map_err(|e| e.to_string())?;
        self.stdin.write_all(b"\n").and_then(|_| self.stdin.flush()).map_err(|e| e.to_string())
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<(), String> {
        self.write(&json!({"jsonrpc":"2.0","method":method,"params":params}))
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id; self.next_id += 1;
        self.write(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))?;
        loop {
            let mut line = String::new();
            if self.stdout.read_line(&mut line).map_err(|e| e.to_string())? == 0 { return Err(format!("MCP 在响应 {method} 前退出")); }
            let Ok(value) = serde_json::from_str::<Value>(&line) else { continue };
            if value.get("id").and_then(Value::as_u64) != Some(id) { continue; }
            if let Some(error) = value.get("error") { return Err(format!("MCP {method} 失败：{error}")); }
            return Ok(value);
        }
    }

    fn shutdown(&mut self) { let _ = self.child.kill(); let _ = self.child.wait(); }
}

impl Drop for Client { fn drop(&mut self) { self.shutdown(); } }

struct McpTool { spec: McpToolSpec }

#[async_trait]
impl Tool for McpTool {
    fn name(&self) -> &str { &self.spec.name }
    fn label(&self) -> &str { &self.spec.name }
    fn description(&self) -> &str { &self.spec.description }
    fn parameters(&self) -> Value { self.spec.parameters.clone() }
    fn effects(&self) -> ToolEffects { ToolEffects::write() }

    async fn execute(&self, _id: &str, input: Value, _update: Option<Box<dyn Fn(pi::sdk::ToolUpdate) + Send + Sync>>) -> pi::sdk::Result<ToolOutput> {
        let mut client = Client::start(&self.spec.config, &self.spec.cwd).map_err(|e| pi::sdk::Error::tool(self.name(), e))?;
        let value = client.request("tools/call", json!({"name":self.spec.tool,"arguments":input}))
            .map_err(|e| pi::sdk::Error::tool(self.name(), e))?;
        let result = value.get("result").cloned().unwrap_or(Value::Null);
        let content = result.get("content").and_then(Value::as_array).into_iter().flatten().map(|part| {
            ContentBlock::Text(TextContent::new(part.get("text").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| part.to_string())))
        }).collect::<Vec<_>>();
        let is_error = result.get("isError").and_then(Value::as_bool).unwrap_or(false);
        Ok(ToolOutput { content: if content.is_empty(){vec![ContentBlock::Text(TextContent::new("MCP 工具执行完成"))]}else{content}, details: Some(result), is_error })
    }
}
