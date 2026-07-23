use super::{canonical_usage, LaunchConfig, SdkAdapter};
use crate::settings::Settings;
use crate::threads::{AgentKind, CodexUsageSnapshot};
use serde_json::Value;

pub struct CodexAdapter;

impl SdkAdapter for CodexAdapter {
    fn agent_kind(&self) -> AgentKind {
        AgentKind::Codex
    }

    fn label(&self) -> &'static str {
        "Codex+"
    }

    fn bridge(&self) -> (&'static str, &'static [u8]) {
        (
            "codex-bridge.mjs",
            include_bytes!("../../resources/codex-bridge.mjs"),
        )
    }

    fn launch_config(&self, settings: &Settings) -> LaunchConfig {
        LaunchConfig {
            program: settings.codex_path.clone(),
            proxy: settings.codex_proxy.clone(),
            path_env: "NOVA_CODEX_PATH",
            api_key: None,
        }
    }

    fn permission_prefix(&self) -> &'static str {
        "cdp"
    }

    fn uses_codex_model_routing(&self) -> bool {
        true
    }

    fn generates_title(&self) -> bool {
        true
    }

    fn accepts_data_image(&self, _mime_type: &str) -> bool {
        true
    }

    fn uses_text_deltas(&self) -> bool {
        true
    }

    fn normalize_usage(
        &self,
        usage: Option<&Value>,
        codex_baseline: Option<&CodexUsageSnapshot>,
        session_id: Option<&str>,
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
        let cache_read = usage
            .get("cached_input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let cache_write = usage
            .get("cache_creation_input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let snapshot = CodexUsageSnapshot {
            session_id: session_id.map(str::to_string),
            input_tokens: input,
            output_tokens: output,
            cache_read_tokens: cache_read,
            cache_write_tokens: cache_write,
        };
        let Some(previous) = codex_baseline.filter(|previous| {
            previous.session_id.as_deref() == session_id
                && input >= previous.input_tokens
                && output >= previous.output_tokens
        }) else {
            return (None, Some(snapshot));
        };
        (
            Some(canonical_usage(
                input - previous.input_tokens,
                output - previous.output_tokens,
                (cache_read >= previous.cache_read_tokens)
                    .then_some(cache_read - previous.cache_read_tokens),
                (cache_write >= previous.cache_write_tokens)
                    .then_some(cache_write - previous.cache_write_tokens),
            )),
            Some(snapshot),
        )
    }
}
