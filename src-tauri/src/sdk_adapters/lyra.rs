//! Lyra：Rust 原生 agent，不走 Node bridge。主运行时与借用额度运行时都在应用进程内
//! 以 tokio 任务直接运行（runs_inprocess；借用额度只换数据根即不同凭证，无需进程隔离）；
//! `nova lyra` 子命令保留作命令行调试入口，stdio 协议与旧 alkaid-bridge 兼容；
//! 配置/会话/技能目录沿用 ~/.nova/alkaid/。

use super::{canonical_usage, LaunchConfig, SdkAdapter};
use crate::settings::Settings;
use crate::threads::{AgentKind, CodexUsageSnapshot, ToolCall};
use serde_json::{json, Value};

pub struct LyraAdapter;

impl SdkAdapter for LyraAdapter {
    fn agent_kind(&self) -> AgentKind {
        AgentKind::Lyra
    }

    fn label(&self) -> &'static str {
        "Lyra"
    }

    fn bridge(&self) -> (&'static str, &'static [u8]) {
        // 原生 agent 无内嵌 bridge 脚本；spawn 走 native_subcommand。
        ("lyra-bridge.mjs", &[])
    }

    fn native_subcommand(&self) -> Option<&'static str> {
        Some("lyra")
    }

    fn runs_inprocess(&self) -> bool {
        true
    }

    fn launch_config(&self, settings: &Settings) -> LaunchConfig {
        LaunchConfig {
            program: "lyra".into(),
            proxy: settings.lyra_proxy.clone(),
            path_env: "LYRA_RUNTIME",
            api_key: None,
            extra_env: vec![
                (
                    "NOVA_AUTO_CHANGE_PROJECT",
                    if settings.auto_change_project_enabled {
                        "1"
                    } else {
                        "0"
                    }
                    .into(),
                ),
                (
                    "NOVA_FAST_CONTEXT",
                    if settings.context_tools_enabled() {
                        "1"
                    } else {
                        "0"
                    }
                    .into(),
                ),
                (
                    "NOVA_CONTEXT_RETRIEVAL_MODE",
                    settings.context_retrieval_mode.as_str().into(),
                ),
                (
                    "NOVA_POWERSHELL_UTF8",
                    if settings.powershell_utf8_enabled {
                        "1"
                    } else {
                        "0"
                    }
                    .into(),
                ),
            ],
        }
    }

    fn permission_prefix(&self) -> &'static str {
        "lyr"
    }

    fn generates_title(&self) -> bool {
        true
    }

    fn supports_native_steer(&self) -> bool {
        true
    }

    fn supports_browser_debug(&self) -> bool {
        true
    }

    fn uses_text_deltas(&self) -> bool {
        true
    }

    fn cancel_grace_attempts(&self) -> usize {
        20
    }

    fn done_is_cancelled(&self, event: &Value) -> bool {
        event.get("cancelled").and_then(Value::as_bool) == Some(true)
    }

    fn map_tool_call(&self, value: &Value) -> Option<ToolCall> {
        Some(lyra_tool_call(value))
    }

    fn normalize_usage(
        &self,
        usage: Option<&Value>,
        _codex_baseline: Option<&CodexUsageSnapshot>,
        _session_id: Option<&str>,
    ) -> (Option<Value>, Option<CodexUsageSnapshot>) {
        let Some(usage) = usage else {
            return (None, None);
        };
        let Some(input) = usage.get("input").and_then(Value::as_u64) else {
            return (None, None);
        };
        let Some(output) = usage.get("output").and_then(Value::as_u64) else {
            return (None, None);
        };
        // input 聚合未缓存 + 缓存读写，同时转发缓存明细。
        let cache_read = usage.get("cacheRead").and_then(Value::as_u64);
        let cache_write = usage.get("cacheWrite").and_then(Value::as_u64);
        let cached_input = cache_read
            .unwrap_or(0)
            .saturating_add(cache_write.unwrap_or(0));
        let mut normalized = canonical_usage(
            input.saturating_add(cached_input),
            output,
            cache_read,
            cache_write,
        );
        if let Some(context_tokens) = usage.get("contextTokens").and_then(Value::as_u64) {
            normalized["contextTokens"] = context_tokens.into();
        }
        (Some(normalized), None)
    }
}

fn lyra_tool_call(value: &Value) -> ToolCall {
    let item_type = value.get("type").and_then(Value::as_str).unwrap_or("tool");
    let status = value
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("completed")
        .to_string();
    let arguments = value.get("arguments").cloned();
    let result = value.get("result").or_else(|| value.get("error")).cloned();
    if item_type == "command_execution" {
        let command = value
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or("bash");
        let output = value
            .get("aggregated_output")
            .and_then(Value::as_str)
            .unwrap_or_default();
        return ToolCall {
            tool_call_id: value["id"].as_str().unwrap_or("tool").into(),
            title: command.into(),
            kind: "execute".into(),
            status,
            content: text_content(output),
            locations: Vec::new(),
            raw_input: arguments,
            raw_output: result,
        };
    }
    if item_type == "file_change" {
        let changes = value
            .get("changes")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let locations = changes
            .iter()
            .filter_map(|change| change.get("path").and_then(Value::as_str))
            .map(|path| json!({ "path": path }))
            .collect();
        let title = if changes.len() == 1 {
            format!("修改 {}", changes[0]["path"].as_str().unwrap_or("文件"))
        } else {
            format!("修改 {} 个文件", changes.len())
        };
        return ToolCall {
            tool_call_id: value["id"].as_str().unwrap_or("tool").into(),
            title,
            kind: "edit".into(),
            status,
            content: Vec::new(),
            locations,
            raw_input: arguments,
            raw_output: result,
        };
    }
    let server = value
        .get("server")
        .and_then(Value::as_str)
        .unwrap_or("Lyra");
    let tool = value.get("tool").and_then(Value::as_str).unwrap_or("tool");
    let detail = tool_detail(tool, value.get("arguments"));
    let output = value
        .get("error")
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| value.get("result").and_then(result_text));
    ToolCall {
        tool_call_id: value["id"].as_str().unwrap_or("tool").into(),
        title: format!(
            "{server} / {tool}{}",
            detail
                .map(|detail| format!(" · {detail}"))
                .unwrap_or_default()
        ),
        kind: match tool {
            "bash" | "shell" => "execute",
            "read" | "load_skill" => "read",
            "edit" | "write" => "edit",
            "grep" | "find" | "ls" | "glob" => "search",
            _ => "other",
        }
        .into(),
        status,
        content: output
            .map(|output| text_content(&output))
            .unwrap_or_default(),
        locations: argument_paths(value.get("arguments")),
        raw_input: arguments,
        raw_output: result,
    }
}

fn tool_detail(tool: &str, arguments: Option<&Value>) -> Option<String> {
    let arguments = arguments?;
    let value = match tool {
        "read" | "edit" | "write" | "ls" => arguments.get("path")?.as_str()?.to_string(),
        "grep" | "find" => arguments.get("pattern")?.as_str()?.to_string(),
        "load_skill" => arguments.get("name")?.as_str()?.to_string(),
        _ => return None,
    };
    Some(value.chars().take(160).collect())
}

fn argument_paths(arguments: Option<&Value>) -> Vec<Value> {
    let Some(arguments) = arguments else {
        return Vec::new();
    };
    if let Some(path) = arguments.get("path").and_then(Value::as_str) {
        return vec![json!({ "path": path })];
    }
    arguments
        .get("paths")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|path| {
            path.as_str()
                .or_else(|| path.get("path").and_then(Value::as_str))
                .map(|path| json!({ "path": path }))
        })
        .collect()
}

fn result_text(result: &Value) -> Option<String> {
    let text = result
        .get("content")?
        .as_array()?
        .iter()
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n");
    (!text.is_empty()).then_some(text)
}

fn text_content(text: &str) -> Vec<Value> {
    if text.trim().is_empty() {
        Vec::new()
    } else {
        vec![json!({ "type": "content", "content": { "type": "text", "text": text } })]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn maps_tool_items() {
        let adapter = LyraAdapter;
        let tool = adapter
            .map_tool_call(&json!({
                "id": "call-1", "type": "mcp_tool_call", "server": "Lyra", "tool": "read",
                "arguments": { "path": "src/main.rs" }, "status": "completed",
                "result": { "content": [{ "type": "text", "text": "ok" }] }
            }))
            .unwrap();
        assert_eq!(tool.kind, "read");
        assert!(tool.title.starts_with("Lyra / read"));

        let bash = adapter
            .map_tool_call(&json!({
                "id": "call-2", "type": "command_execution", "status": "completed",
                "command": "ls", "aggregated_output": "a.rs"
            }))
            .unwrap();
        assert_eq!(bash.kind, "execute");
        assert_eq!(bash.content[0]["content"]["text"], "a.rs");
    }
}
