//! Isolated inference transports. They have no desktop/filesystem/MCP tools.
use super::{DecisionInput, SYSTEM};
use crate::lyra::config::Resolved;
use serde_json::{json, Value};
use std::path::PathBuf;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
};

pub struct Scratch(pub PathBuf);
impl Scratch {
    pub fn new() -> Result<Self, String> {
        let p = std::env::temp_dir().join(format!("nova-operator-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&p).map_err(|e| e.to_string())?;
        Ok(Self(p))
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        if let Some(pid) = self.0.id() {
            crate::acp::kill_process_tree(pid);
            let _ = self.0.start_kill();
        }
    }
}
fn spawn(mut command: Command) -> Result<Process, String> {
    command
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    // Decision processes cannot call the parent service even through ambient MCP.
    for name in [
        "NOVA_OPERATOR_SCOPE",
        "NOVA_CONTEXT_SERVICE_TOKEN",
        "NOVA_CONTEXT_SERVICE_ENDPOINT",
    ] {
        command.env_remove(name);
    }
    #[cfg(windows)]
    command.creation_flags(0x0800_0000);
    #[cfg(unix)]
    command.process_group(0);
    let child = command
        .spawn()
        .map_err(|e| format!("Operator inference process failed to start: {e}"))?;
    Ok(Process(child))
}

pub async fn lyra(
    http: reqwest::Client,
    resolved: Resolved,
    input: DecisionInput,
) -> Result<String, String> {
    if !input.images.is_empty() && !resolved.model.supports_images {
        return Err("Inherited model does not support screenshots; no model fallback".into());
    }
    let mut parts = vec![json!({"type":"text","text":input.context.to_string()})];
    parts.extend(
        input
            .images
            .iter()
            .map(|image| json!({"type":"image","data":image.data,"mimeType":image.mime_type})),
    );
    let response = crate::lyra::provider::stream_chat(
        &http,
        &resolved.model,
        &resolved.api_key,
        resolved.thinking_level.as_deref(),
        SYSTEM,
        &[json!({"role":"user","content":parts})],
        &[],
        None,
        &input.cancelled,
        &mut |_| {},
    )
    .await?;
    if response.stop_reason == "aborted" {
        return Err("Operator inference cancelled".into());
    }
    let text = response
        .content
        .iter()
        .filter(|p| p["type"] == "text")
        .filter_map(|p| p["text"].as_str())
        .collect::<Vec<_>>()
        .join("");
    if text.is_empty() {
        return Err("Inherited model returned no decision".into());
    }
    Ok(text)
}

struct Rpc {
    stdin: ChildStdin,
    lines: tokio::io::Lines<BufReader<ChildStdout>>,
    next: u64,
    text: String,
}
impl Rpc {
    async fn write(&mut self, value: Value) -> Result<(), String> {
        self.stdin
            .write_all(format!("{value}\n").as_bytes())
            .await
            .map_err(|e| e.to_string())
    }
    async fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.next += 1;
        let id = self.next;
        self.write(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
            .await?;
        loop {
            let line=self.lines.next_line().await.map_err(|e|e.to_string())?.ok_or("Operator ACP exited before response (check CLI version, model access and isolated-tool flags)")?;
            if line.len() > 4 * 1024 * 1024 {
                return Err("ACP response exceeds limit".into());
            }
            if line.trim().is_empty() {
                continue;
            }
            let value: Value = serde_json::from_str(&line).map_err(|e| e.to_string())?;
            if value["method"] == "session/update" {
                let update = &value["params"]["update"];
                if matches!(
                    update["sessionUpdate"].as_str(),
                    Some("tool_call" | "tool_call_update")
                ) {
                    return Err(
                        "Decision agent attempted a tool; isolation contract violated".into(),
                    );
                }
                if update["sessionUpdate"] == "agent_message_chunk" {
                    if let Some(text) = update["content"]["text"].as_str() {
                        self.text.push_str(text);
                        if self.text.len() > 48_000 {
                            return Err("Decision exceeds limit".into());
                        }
                    }
                }
            } else if value.get("method").is_some() && value.get("id").is_some() {
                let reply = if value["method"] == "session/request_permission" {
                    json!({"outcome":{"outcome":"cancelled"}})
                } else {
                    Value::Null
                };
                if !reply.is_null() {
                    self.write(json!({"jsonrpc":"2.0","id":value["id"],"result":reply}))
                        .await?;
                } else {
                    self.write(json!({"jsonrpc":"2.0","id":value["id"],"error":{"code":-32601,"message":"Operator decision sessions have no filesystem, terminal or tool access"}})).await?;
                }
            } else if value["id"] == id {
                if let Some(error) = value.get("error") {
                    return Err(format!(
                        "Operator ACP {method} failed: {}",
                        error["message"].as_str().unwrap_or("protocol error")
                    ));
                }
                return Ok(value["result"].clone());
            }
        }
    }
}
/// A fresh ACP process/session for this bounded decision. Never load/resume/fork
/// the parent's session, and never reuse a title/lightweight model.
pub async fn codebuddy(
    command: Command,
    model: String,
    effort: Option<String>,
    input: DecisionInput,
    scratch: Scratch,
) -> Result<String, String> {
    let mut child = spawn(command)?;
    let mut rpc = Rpc {
        stdin: child.0.stdin.take().ok_or("ACP stdin missing")?,
        lines: BufReader::new(child.0.stdout.take().ok_or("ACP stdout missing")?).lines(),
        next: 0,
        text: String::new(),
    };
    let init=rpc.request("initialize",json!({"protocolVersion":1,"clientInfo":{"name":"nova-operator","version":"1"},"clientCapabilities":{}})).await?;
    if !input.images.is_empty() && init["agentCapabilities"]["promptCapabilities"]["image"] != true
    {
        return Err("Inherited ACP agent did not advertise image input support".into());
    }
    let new = rpc
        .request("session/new", json!({"cwd":scratch.0,"mcpServers":[]}))
        .await?;
    let sid = new["sessionId"]
        .as_str()
        .ok_or("ACP did not return sessionId")?
        .to_string();
    let selected = rpc
        .request(
            "session/set_config_option",
            json!({"sessionId":sid,"configId":"model","value":model}),
        )
        .await?;
    if selected["configOptions"]
        .as_array()
        .and_then(|a| a.iter().find(|o| o["id"] == "model"))
        .and_then(|o| o["currentValue"].as_str())
        != Some(model.as_str())
    {
        return Err("ACP did not confirm the inherited model; refusing a silent fallback".into());
    }
    if let Some(effort) = effort {
        let selected = rpc
            .request(
                "session/set_config_option",
                json!({"sessionId":sid,"configId":"thought_level","value":effort}),
            )
            .await?;
        if selected["configOptions"]
            .as_array()
            .and_then(|a| a.iter().find(|o| o["id"] == "thought_level"))
            .and_then(|o| o["currentValue"].as_str())
            != Some(effort.as_str())
        {
            return Err("ACP did not confirm inherited reasoning effort".into());
        }
    }
    let mut prompt = vec![json!({"type":"text","text":input.context.to_string()})];
    prompt.extend(
        input
            .images
            .iter()
            .map(|image| json!({"type":"image","data":image.data,"mimeType":image.mime_type})),
    );
    let done = rpc
        .request("session/prompt", json!({"sessionId":sid,"prompt":prompt}))
        .await?;
    if done["stopReason"] != "end_turn" {
        return Err("ACP decision did not complete normally".into());
    }
    Ok(rpc.text.clone())
}

/// Cursor's existing bundled bridge dispatches this action BEFORE its normal
/// code-context/Reasonix loop. Parent credentials/environment are inherited by
/// the caller's existing spawn path; only the decision prompt is sent here.
pub async fn cursor(
    mut child: Child,
    model: String,
    effort: Option<String>,
    input: DecisionInput,
    scratch: Scratch,
) -> Result<String, String> {
    let mut stdin = child.stdin.take().ok_or("Cursor stdin missing")?;
    let stdout = child.stdout.take().ok_or("Cursor stdout missing")?;
    // Drain errors without retaining credential-bearing server output.
    if let Some(stderr) = child.stderr.take() {
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while lines.next_line().await.ok().flatten().is_some() {}
        });
    }
    let _child = Process(child);
    let request = json!({"action":"operator_decide","cwd":scratch.0,"model":model,"reasoningEffort":effort,"context":input.context,"images":input.images,"system":SYSTEM});
    stdin
        .write_all(format!("{request}\n").as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    let mut lines = BufReader::new(stdout).lines();
    while let Some(line) = lines.next_line().await.map_err(|e| e.to_string())? {
        if line.len() > 64_000 {
            return Err("Cursor decision response too large".into());
        }
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if value["operatorDecision"] == true {
            return if value["ok"] == true {
                value["text"]
                    .as_str()
                    .map(str::to_owned)
                    .ok_or("Cursor returned no decision".into())
            } else {
                Err(value["error"]
                    .as_str()
                    .unwrap_or("Cursor isolated inference failed")
                    .into())
            };
        }
    }
    Err("Cursor bridge exited without an Operator decision".into())
}
