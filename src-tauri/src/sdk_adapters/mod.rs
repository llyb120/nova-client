mod alkaid;
mod claude;
mod codebuddy;
mod codex;
mod cursor;

pub use alkaid::AlkaidAdapter;
pub use claude::ClaudeAdapter;
pub use codebuddy::CodeBuddyAdapter;
pub use codex::CodexAdapter;
pub use cursor::CursorAdapter;

use crate::settings::Settings;
use crate::threads::{AgentKind, CodexUsageSnapshot, ToolCall};
use serde_json::{json, Value};

pub struct LaunchConfig {
    pub program: String,
    pub proxy: String,
    pub path_env: &'static str,
    pub api_key: Option<(&'static str, String)>,
}

pub trait SdkAdapter: Send + Sync {
    fn agent_kind(&self) -> AgentKind;
    fn label(&self) -> &'static str;
    fn bridge(&self) -> (&'static str, &'static [u8]);
    fn launch_config(&self, settings: &Settings) -> LaunchConfig;
    fn permission_prefix(&self) -> &'static str;

    fn uses_codex_model_routing(&self) -> bool {
        false
    }

    fn generates_title(&self) -> bool {
        false
    }

    fn keeps_bridge_alive(&self) -> bool {
        false
    }

    fn supports_native_steer(&self) -> bool {
        false
    }

    fn accepts_data_image(&self, mime_type: &str) -> bool {
        mime_type.starts_with("image/")
    }

    fn uses_text_deltas(&self) -> bool {
        false
    }

    fn cancel_grace_attempts(&self) -> usize {
        2
    }

    fn done_is_cancelled(&self, _event: &Value) -> bool {
        false
    }

    fn map_tool_call(&self, _value: &Value) -> Option<ToolCall> {
        None
    }

    fn empty_model_options(&self) -> Value {
        json!({
            "configOptions": [{
                "id": "model",
                "name": "Model",
                "currentValue": "",
                "options": [],
            }],
            "modes": null,
        })
    }

    fn normalize_usage(
        &self,
        usage: Option<&Value>,
        _codex_baseline: Option<&CodexUsageSnapshot>,
        _session_id: Option<&str>,
    ) -> (Option<Value>, Option<CodexUsageSnapshot>);
}

fn canonical_usage(input: u64, output: u64) -> Value {
    json!({
        "inputTokens": input,
        "outputTokens": output,
        "totalTokens": input.saturating_add(output),
    })
}
