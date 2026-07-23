use super::{canonical_usage, LaunchConfig, SdkAdapter};
use crate::settings::Settings;
use crate::threads::{AgentKind, CodexUsageSnapshot, ToolCall};
use serde_json::{json, Value};

pub struct AlkaidAdapter;

impl SdkAdapter for AlkaidAdapter {
    fn agent_kind(&self) -> AgentKind {
        AgentKind::Alkaid
    }

    fn label(&self) -> &'static str {
        "Vega"
    }

    fn bridge(&self) -> (&'static str, &'static [u8]) {
        (
            "alkaid-bridge.mjs",
            include_bytes!("../../resources/alkaid-bridge.mjs"),
        )
    }

    fn launch_config(&self, _settings: &Settings) -> LaunchConfig {
        LaunchConfig {
            program: "node".into(),
            proxy: String::new(),
            path_env: "ALKAID_RUNTIME",
            api_key: None,
        }
    }

    fn permission_prefix(&self) -> &'static str {
        "alk"
    }

    fn generates_title(&self) -> bool {
        true
    }

    fn supports_native_steer(&self) -> bool {
        true
    }

    fn cancel_grace_attempts(&self) -> usize {
        20
    }

    fn done_is_cancelled(&self, event: &Value) -> bool {
        event.get("cancelled").and_then(Value::as_bool) == Some(true)
    }

    fn map_tool_call(&self, value: &Value) -> Option<ToolCall> {
        Some(alkaid_tool_call(value))
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
        // PI exposes uncached input separately; cached reads/writes are still input tokens.
        let cached_input = usage
            .get("cacheRead")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .saturating_add(usage.get("cacheWrite").and_then(Value::as_u64).unwrap_or(0));
        (
            Some(canonical_usage(input.saturating_add(cached_input), output)),
            None,
        )
    }
}

fn alkaid_tool_call(value: &Value) -> ToolCall {
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
        .unwrap_or("Vega");
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
            "read" | "read_files" | "load_skill" => "read",
            "edit" | "write" | "edit_files" => "edit",
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
        "read_files" => format!("{} files", arguments.get("paths")?.as_array()?.len()),
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

    #[test]
    fn tools_preserve_arguments_outputs_and_locations() {
        let read = alkaid_tool_call(&json!({
            "id": "read", "type": "mcp_tool_call", "server": "Vega", "tool": "read_files",
            "arguments": { "paths": ["src/a.ts", { "path": "src/b.ts", "offset": 20 }] },
            "status": "completed", "result": { "content": [{ "type": "text", "text": "content" }] }
        }));
        assert_eq!(read.kind, "read");
        assert_eq!(read.locations[0]["path"], "src/a.ts");
        assert_eq!(read.locations[1]["path"], "src/b.ts");
        assert_eq!(read.raw_input.as_ref().unwrap()["paths"][1]["offset"], 20);
        assert_eq!(read.content[0]["content"]["text"], "content");
    }
}
