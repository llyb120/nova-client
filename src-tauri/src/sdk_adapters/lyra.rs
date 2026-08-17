//! Lyra：Rust 原生 agent，不走 Node bridge。主运行时与借用额度运行时都在应用进程内
//! 以 tokio 任务直接运行（runs_inprocess；借用额度只换数据根即不同凭证，无需进程隔离）；
//! `nova lyra` 子命令保留作命令行调试入口，stdio 协议与 alkaid-bridge 兼容；
//! 配置/会话/技能目录与 Vega 共用。

use super::alkaid::alkaid_tool_call;
use super::{canonical_usage, LaunchConfig, SdkAdapter};
use crate::settings::Settings;
use crate::threads::{AgentKind, CodexUsageSnapshot, ToolCall};
use serde_json::Value;

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
            proxy: settings.vega_proxy.clone(),
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
        // 与 Vega 一致：input 聚合未缓存 + 缓存读写，同时转发缓存明细。
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn maps_tool_items_like_vega() {
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
