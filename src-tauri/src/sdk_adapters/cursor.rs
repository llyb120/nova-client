use super::{canonical_usage, LaunchConfig, SdkAdapter};
use crate::settings::Settings;
use crate::threads::{AgentKind, CodexUsageSnapshot};
use serde_json::{json, Value};

pub struct CursorAdapter;

impl SdkAdapter for CursorAdapter {
    fn agent_kind(&self) -> AgentKind {
        AgentKind::Cursor
    }

    fn label(&self) -> &'static str {
        "Cursor+"
    }

    fn bridge(&self) -> (&'static str, &'static [u8]) {
        (
            "cursor-bridge.mjs",
            include_bytes!("../../resources/cursor-bridge.mjs"),
        )
    }

    fn launch_config(&self, settings: &Settings) -> LaunchConfig {
        LaunchConfig {
            program: settings.cursor_path.clone(),
            proxy: settings.cursor_proxy.clone(),
            path_env: "NOVA_CURSOR_PATH",
            api_key: (!settings.cursor_sdk_api_key.is_empty())
                .then(|| ("CURSOR_API_KEY", settings.cursor_sdk_api_key.clone())),
        }
    }

    fn permission_prefix(&self) -> &'static str {
        "cup"
    }

    fn generates_title(&self) -> bool {
        true
    }

    fn keeps_bridge_alive(&self) -> bool {
        true
    }

    fn empty_model_options(&self) -> Value {
        json!({
            "configOptions": [{
                "id": "model",
                "name": "Model",
                "currentValue": "",
                "options": [{ "value": "", "name": "Auto（Cursor 默认）" }],
            }],
            "modes": null,
        })
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
        let Some(input) = usage.get("inputTokens").and_then(Value::as_u64) else {
            return (None, None);
        };
        let Some(output) = usage.get("outputTokens").and_then(Value::as_u64) else {
            return (None, None);
        };
        let cache_read = usage.get("cacheReadTokens").and_then(Value::as_u64);
        let cache_write = usage.get("cacheWriteTokens").and_then(Value::as_u64);
        let input = input
            .saturating_add(cache_read.unwrap_or(0))
            .saturating_add(cache_write.unwrap_or(0));
        (
            Some(canonical_usage(input, output, cache_read, cache_write)),
            None,
        )
    }
}
