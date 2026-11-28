//! Antigravity `session/new` / `session/resume` / `session/load` 的 `_meta.antigravity` 构造。
//!
//! - 仅在 `initialize` 确认 `supports_system_prompt_append == true` 时注入 `systemPrompt.append`
//!   （说明仓库工具 `mcp_nova_polaris` / `mcp_nova_code_map`、自动化工具 `nova_automation` 与全局指令）；
//! - 挂载 `nova-automation` 时若 `initialize` 宣告 `supports_disabled_tools == true`，
//!   通过 `_meta.antigravity.disabledTools` 禁用 23 个内置 `browser_*` 工具，网页任务统一走 Nova Chrome；
//! - 通过 `_meta.antigravity.eagerMcpServers` 宣告 `["nova", "nova-automation"]`，确保建会话即拉起 MCP 工具桥。

use super::protocol::AgentCapabilities;
use serde_json::{json, Map, Value};

/// Nova 暴露给 `agy` 的固定 MCP server 名称（模型侧工具名：`mcp_nova_<tool>`）。
pub const NOVA_MCP_SERVER_NAME: &str = "nova";
/// Nova 外部/内置浏览器与剑来自动化 MCP server 名称（工具：`nova_automation`）。
pub const NOVA_AUTOMATION_MCP_SERVER_NAME: &str = "nova-automation";

/// `agy` 内置的 23 个浏览器工具名（产品决策禁用，统一走 `nova_automation` 的 Chrome 能力）。
pub const BUILTIN_BROWSER_TOOLS: &[&str] = &[
    "browser_click_element",
    "browser_drag_pixel_to_pixel",
    "browser_get_dom",
    "browser_get_network_request",
    "browser_input",
    "browser_list_network_requests",
    "browser_mouse_down",
    "browser_mouse_up",
    "browser_move_mouse",
    "browser_press_key",
    "browser_refresh_page",
    "browser_resize_window",
    "browser_scroll",
    "browser_scroll_dom",
    "browser_select_option",
    "browser_subagent",
    "capture_browser_console_logs",
    "capture_browser_screenshot",
    "click_browser_pixel",
    "execute_browser_javascript",
    "list_browser_pages",
    "open_browser_url",
    "read_browser_page",
];

/// 构造 `systemPrompt.append` 正文（保持中性 Nova 措辞，不含第三方 harness 特征词）。
pub fn build_system_prompt_append(
    polaris_enabled: bool,
    automation_enabled: bool,
    global_instructions: Option<&str>,
) -> String {
    let mut parts = Vec::new();
    parts.push("Use mcp_nova_code_map for a directory overview. Use one targeted rg when you already know the exact symbol or literal to find. Read a known file directly when no search is needed.".to_owned());
    if polaris_enabled {
        parts.push("For fuzzy repository searches, unknown locations, or questions requiring several searches to gather context, delegate the whole objective to mcp_nova_polaris in one call to keep cost low. Do not scout first with shell or speculative reads, or repeat a search that Polaris answered. Call it directly, e.g. {\"task\":\"Locate the implementation of Lyra\"}. For analysis or explanation questions, answer directly from Polaris's conclusions and verified snippets when they already cover the question—do not re-read files just to double-check. Only when editing code or filling an explicit evidence gap should you read the exact source file ranges listed in Polaris's map in one parallel batch. If Polaris is unavailable or misses necessary evidence, use a narrowly scoped fallback.".to_owned());
    }
    if automation_enabled {
        parts.push("For browser (Chrome) or native desktop (Jianlai) automation tasks, delegate the objective to the nova_automation MCP tool (`scope`: \"chrome\" | \"desktop\" | \"both\", reusing the returned `session_id` across follow-up turns). Always use nova_automation for web tasks instead of agy's built-in browser.".to_owned());
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
    automation_enabled: bool,
    global_instructions: Option<&str>,
) -> Option<Value> {
    let mut agy_meta = Map::new();

    let mut eager = Vec::new();
    if include_mcp {
        eager.push(NOVA_MCP_SERVER_NAME);
    }
    if automation_enabled {
        eager.push(NOVA_AUTOMATION_MCP_SERVER_NAME);
    }
    if !eager.is_empty() {
        agy_meta.insert("eagerMcpServers".into(), json!(eager));
    }

    if caps.supports_system_prompt_append {
        let append =
            build_system_prompt_append(polaris_enabled, automation_enabled, global_instructions);
        agy_meta.insert(
            "systemPrompt".into(),
            json!({ "append": append }),
        );
    }

    if automation_enabled && caps.supports_disabled_tools {
        agy_meta.insert("disabledTools".into(), json!(BUILTIN_BROWSER_TOOLS));
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
    fn configures_automation_mcp_and_disables_builtin_browser_when_enabled() {
        let caps = AgentCapabilities {
            supports_system_prompt_append: true,
            supports_disabled_tools: true,
            ..Default::default()
        };
        assert!(!build_system_prompt_append(false, false, None)
            .to_lowercase()
            .contains("polaris"));

        let meta_with_auto = build_session_meta(&caps, true, true, true, None).unwrap();
        let agy = &meta_with_auto["antigravity"];
        assert_eq!(agy["eagerMcpServers"], json!(["nova", "nova-automation"]));
        let disabled = agy["disabledTools"].as_array().unwrap();
        assert_eq!(disabled.len(), BUILTIN_BROWSER_TOOLS.len());
        for tool in BUILTIN_BROWSER_TOOLS {
            assert!(disabled.iter().any(|v| v.as_str() == Some(*tool)));
        }
        let append = agy["systemPrompt"]["append"].as_str().unwrap();
        assert!(append.contains("mcp_nova_polaris"));
        assert!(append.contains("answer directly from Polaris's conclusions"));
        assert!(append.contains("mcp_nova_code_map"));
        assert!(append.contains("nova_automation"));
        assert!(append.contains("Chrome"));
        assert!(append.contains("desktop"));

        let meta_without_auto = build_session_meta(&caps, true, true, false, None).unwrap();
        let agy_no_auto = &meta_without_auto["antigravity"];
        assert!(agy_no_auto.get("disabledTools").is_none());
        assert_eq!(agy_no_auto["eagerMcpServers"], json!(["nova"]));
        assert!(!agy_no_auto["systemPrompt"]["append"]
            .as_str()
            .unwrap()
            .contains("nova_automation"));
    }
}
