use super::{canonical_usage, LaunchConfig, SdkAdapter};
use crate::settings::Settings;
use crate::threads::{AgentKind, CodexUsageSnapshot};
use serde_json::Value;

pub struct CodeBuddyAdapter;

impl SdkAdapter for CodeBuddyAdapter {
    fn agent_kind(&self) -> AgentKind {
        AgentKind::CodeBuddy
    }

    fn label(&self) -> &'static str {
        "CodeBuddy+"
    }

    fn bridge(&self) -> (&'static str, &'static [u8]) {
        (
            "codebuddy-bridge.cjs",
            include_bytes!("../../resources/codebuddy-bridge.cjs"),
        )
    }

    fn launch_config(&self, settings: &Settings) -> LaunchConfig {
        LaunchConfig {
            program: settings.codebuddy_path.clone(),
            proxy: settings.codebuddy_proxy.clone(),
            path_env: "NOVA_CODEBUDDY_PATH",
            api_key: None,
        }
    }

    fn permission_prefix(&self) -> &'static str {
        "cbp"
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
}
