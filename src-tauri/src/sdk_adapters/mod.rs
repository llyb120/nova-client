mod alkaid;
mod claude;
mod codex;
mod cursor;
mod lyra;

pub use alkaid::AlkaidAdapter;
pub use claude::ClaudeAdapter;
pub use codex::CodexAdapter;
pub use cursor::CursorAdapter;
pub use lyra::LyraAdapter;

use crate::settings::Settings;
use crate::threads::{AgentKind, CodexUsageSnapshot, ToolCall};
use serde_json::{json, Value};

pub struct LaunchConfig {
    pub program: String,
    pub proxy: String,
    pub path_env: &'static str,
    pub api_key: Option<(&'static str, String)>,
    pub extra_env: Vec<(&'static str, String)>,
}

pub trait SdkAdapter: Send + Sync {
    fn agent_kind(&self) -> AgentKind;
    fn label(&self) -> &'static str;
    fn bridge(&self) -> (&'static str, &'static [u8]);
    fn launch_config(&self, settings: &Settings) -> LaunchConfig;
    fn permission_prefix(&self) -> &'static str;

    /// Extra files written next to the bridge in `~/.nova/runtime/`.
    fn bridge_sidecars(&self) -> &'static [(&'static str, &'static [u8])] {
        &[]
    }

    /// Rust 原生 agent：以应用自身的 CLI 子命令启动（`nova <sub>`），不经 Node bridge。
    /// 进程内运行的 agent（Lyra）不使用该入口，仅供命令行调试。
    fn native_subcommand(&self) -> Option<&'static str> {
        None
    }

    /// Rust 原生 agent：主运行时在应用进程内以 tokio 任务运行（不 spawn 子进程）。
    /// 借用额度等带 launch_env 的隔离运行时仍回退 native_subcommand 子进程。
    fn runs_inprocess(&self) -> bool {
        false
    }

    fn uses_codex_model_routing(&self) -> bool {
        false
    }

    fn generates_title(&self) -> bool {
        false
    }

    fn keeps_bridge_alive(&self) -> bool {
        false
    }

    /// 是否支持在首条消息前预热 idle bridge 与 Agent（草稿页空闲期调用，
    /// 把进程启动与 Agent.create 移出首轮关键路径）。
    fn supports_idle_prewarm(&self) -> bool {
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

fn canonical_usage(
    input: u64,
    output: u64,
    cache_read: Option<u64>,
    cache_write: Option<u64>,
) -> Value {
    let mut usage = json!({
        "inputTokens": input,
        "outputTokens": output,
        "totalTokens": input.saturating_add(output),
    });
    if let Some(value) = cache_read {
        usage["cacheReadTokens"] = value.into();
    }
    if let Some(value) = cache_write {
        usage["cacheWriteTokens"] = value.into();
    }
    usage
}
