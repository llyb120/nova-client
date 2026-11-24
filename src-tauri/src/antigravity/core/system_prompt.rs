//! Antigravity `session/new` / `session/resume` / `session/load` 的 `_meta.antigravity` 构造与决策 A 落地。
//!
//! 决策 A：
//! - 通过 `_meta.antigravity.systemPrompt.append` 注入强指令：禁止使用 `agy` 内置的 23 个浏览器工具，
//!   所有网页操作改走 chrome Skill；仅注入入口索引与全局指令；
//! - 仅在 `initialize` 确认 `supports_system_prompt_append == true` 时注入 `systemPrompt.append`；
//! - 若 `initialize` 宣告 `supports_disabled_tools == true`，额外通过 `_meta.antigravity.disabledTools` 禁用 23 个内置浏览器工具；
//! - 始终通过 `_meta.antigravity.eagerMcpServers` 宣告 `["nova"]`，确保建会话即拉起 MCP 工具桥。

use super::protocol::AgentCapabilities;
use serde_json::{json, Map, Value};

/// Nova 暴露给 `agy` 的固定 MCP server 名称（模型侧工具名：`mcp_nova_<tool>`）。
pub const NOVA_MCP_SERVER_NAME: &str = "nova";

/// `agy` 内置的 23 个浏览器工具名（依据 agy 1.5.15 init.tools 实测，60 个内置工具中 browser 相关共 23 个；产品决策 A 禁止使用，统一走 `mcp_nova_chrome`）。
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
pub fn build_system_prompt_append(global_instructions: Option<&str>) -> String {
    let mut parts = Vec::new();
    parts.push("Nova skills: mcp_nova_polaris (repository search), mcp_nova_jianlai (desktop), mcp_nova_chrome (web). When relevant, first call that skill with {} for its SKILL.md. Use chrome exclusively for web tasks, never agy's built-in browser. Do not operate browser and desktop in parallel.".to_owned());

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
        let append = build_system_prompt_append(global_instructions);
        agy_meta.insert(
            "systemPrompt".into(),
            json!({ "append": append }),
        );
    }

    if caps.supports_disabled_tools {
        agy_meta.insert(
            "disabledTools".into(),
            json!(BUILTIN_BROWSER_TOOLS),
        );
    }

    if agy_meta.is_empty() {
        None
    } else {
        Some(json!({ "antigravity": Value::Object(agy_meta) }))
    }
}
