use super::{canonical_usage, LaunchConfig, SdkAdapter};
use crate::settings::Settings;
use crate::threads::{AgentKind, CodexUsageSnapshot};
use serde_json::Value;

pub struct ClaudeAdapter;

impl SdkAdapter for ClaudeAdapter {
    fn agent_kind(&self) -> AgentKind {
        AgentKind::ClaudeCode
    }

    fn label(&self) -> &'static str {
        "Claude Code+"
    }

    fn bridge(&self) -> (&'static str, &'static [u8]) {
        (
            "claude-bridge.mjs",
            include_bytes!("../../resources/claude-bridge.mjs"),
        )
    }

    /// nova-tools MCP（polaris）随 bridge 释放到 runtime 目录，
    /// 由 bridge 在 query 时通过 mcpServers 挂载。
    fn bridge_sidecars(&self) -> &'static [(&'static str, &'static [u8])] {
        &[(
            "nova-tools-mcp.mjs",
            include_bytes!("../../resources/nova-tools-mcp.mjs"),
        )]
    }

    fn launch_config(&self, settings: &Settings) -> LaunchConfig {
        LaunchConfig {
            program: settings.claudecode_path.clone(),
            proxy: settings.claudecode_proxy.clone(),
            path_env: "NOVA_CLAUDE_PATH",
            api_key: (!settings.claudecode_sdk_api_key.is_empty())
                .then(|| ("ANTHROPIC_API_KEY", settings.claudecode_sdk_api_key.clone())),
            extra_env: vec![
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
        "clp"
    }

    fn normalize_usage(
        &self,
        usage: Option<&Value>,
        _codex_baseline: Option<&CodexUsageSnapshot>,
        _session_id: Option<&str>,
    ) -> (Option<Value>, Option<CodexUsageSnapshot>) {
        normalize_claude_usage(usage)
    }
}

fn normalize_claude_usage(usage: Option<&Value>) -> (Option<Value>, Option<CodexUsageSnapshot>) {
    let Some(usage) = usage else {
        return (None, None);
    };
    let Some(input) = usage.get("input_tokens").and_then(Value::as_u64) else {
        return (None, None);
    };
    let Some(output) = usage.get("output_tokens").and_then(Value::as_u64) else {
        return (None, None);
    };
    let cache_read = usage.get("cache_read_input_tokens").and_then(Value::as_u64);
    let cache_write = usage
        .get("cache_creation_input_tokens")
        .and_then(Value::as_u64);
    let input = input
        .saturating_add(cache_read.unwrap_or(0))
        .saturating_add(cache_write.unwrap_or(0));
    (
        Some(canonical_usage(input, output, cache_read, cache_write)),
        None,
    )
}
