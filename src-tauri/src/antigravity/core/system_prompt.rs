//! Antigravity `session/new` / `session/resume` / `session/load` 的 `_meta.antigravity` 构造。
//!
//! - 仅在 `initialize` 确认 `supports_system_prompt_append == true` 时注入 `systemPrompt.append`
//!   （说明仓库工具 `mcp_nova_polaris` / `mcp_nova_code_map` 与全局指令）；
//! - 保留 `agy` 内置的 `browser_*` 系列工具，不向 `_meta.antigravity.disabledTools` 写入禁用清单；
//! - 始终通过 `_meta.antigravity.eagerMcpServers` 宣告 `["nova"]`，确保建会话即拉起 MCP 工具桥。

use super::protocol::AgentCapabilities;
use serde_json::{json, Map, Value};

/// Nova 暴露给 `agy` 的固定 MCP server 名称（模型侧工具名：`mcp_nova_<tool>`）。
pub const NOVA_MCP_SERVER_NAME: &str = "nova";

/// 构造 `systemPrompt.append` 正文（保持中性 Nova 措辞，不含第三方 harness 特征词）。
pub fn build_system_prompt_append(polaris_enabled: bool, global_instructions: Option<&str>) -> String {
    let mut parts = Vec::new();
    parts.push("Use mcp_nova_code_map for a directory overview. Use one targeted rg when you already know the exact symbol or literal to find. Read a known file directly when no search is needed.".to_owned());
    if polaris_enabled {
        parts.push("For fuzzy repository searches, unknown locations, or questions requiring several searches to gather context, delegate the whole objective to mcp_nova_polaris in one call to keep cost low. Do not scout first with shell or speculative reads, or repeat a search that Polaris answered. Call it directly, e.g. {\"task\":\"Locate the implementation of Lyra\"}. For analysis or explanation questions, answer directly from Polaris's conclusions and verified snippets when they already cover the question—do not re-read files just to double-check. Only when editing code or filling an explicit evidence gap should you read the exact source file ranges listed in Polaris's map in one parallel batch. If Polaris is unavailable or misses necessary evidence, use a narrowly scoped fallback.".to_owned());
    }

    if let Some(extra) = global_instructions
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        parts.push(format!("[Nova Global Instructions]\n{extra}"));
    }

    parts.join("\n\n")
}

/// 按 `initialize` 协商出的能力构造 `_meta`（返回 `None` 表示无需传 `_meta`）。
pub fn build_session_meta(
    caps: &AgentCapabilities,
    include_mcp: bool,
    polaris_enabled: bool,
    global_instructions: Option<&str>,
) -> Option<Value> {
    let mut agy_meta = Map::new();

    if include_mcp {
        agy_meta.insert(
            "eagerMcpServers".into(),
            json!([NOVA_MCP_SERVER_NAME]),
        );
    }

    if caps.supports_system_prompt_append {
        let append = build_system_prompt_append(polaris_enabled, global_instructions);
        agy_meta.insert(
            "systemPrompt".into(),
            json!({ "append": append }),
        );
    }

    if agy_meta.is_empty() {
        None
    } else {
        Some(json!({ "antigravity": Value::Object(agy_meta) }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_builtin_browser_tools_and_only_describes_repo_mcp_tools() {
        let caps = AgentCapabilities {
            supports_system_prompt_append: true,
            supports_disabled_tools: true,
            ..Default::default()
        };
        assert!(!build_system_prompt_append(false, None).to_lowercase().contains("polaris"));
        let meta = build_session_meta(&caps, true, true, None).unwrap();
        let agy = &meta["antigravity"];
        assert!(agy.get("disabledTools").is_none());
        assert_eq!(agy["eagerMcpServers"], json!(["nova"]));
        let append = agy["systemPrompt"]["append"].as_str().unwrap();
        assert!(append.contains("mcp_nova_polaris"));
        assert!(append.contains("answer directly from Polaris's conclusions"));
        assert!(append.contains("mcp_nova_code_map"));
        assert!(!append.contains("mcp_nova_jianlai"));
        assert!(!append.contains("mcp_nova_chrome"));
        assert!(!append.contains("built-in browser"));
    }
}
