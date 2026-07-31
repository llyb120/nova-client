# Context Bundle

- 查询: launch_config
- 生成: 2026-07-31 22:57:21  commit: c414b67
- 预算: 700 行  命中文件: 7  扩展文件: 15

## 命中排名 (命中数 文件)
    1 src-tauri/src/sdk_runtime.rs
    1 src-tauri/src/sdk_adapters/mod.rs
    1 src-tauri/src/sdk_adapters/cursor.rs
    1 src-tauri/src/sdk_adapters/codex.rs
    1 src-tauri/src/sdk_adapters/codebuddy.rs
    1 src-tauri/src/sdk_adapters/claude.rs
    1 src-tauri/src/sdk_adapters/alkaid.rs

# ===== 核心命中文件 =====
----- [HIT] src-tauri/src/sdk_runtime.rs  (2695 行) -----
  ## 符号大纲
fn is_codex_model_resume_warning(value: &Value) -> bool
struct RunningBridge
struct IdleBridge
enum ReadEventsOutcome
pub struct SdkManager
impl SdkManager
    pub fn new<A: SdkAdapter + 'static>(app: AppHandle, adapter: A) -> Arc<Self>
    pub fn new_with_env<A: SdkAdapter + 'static>(
    pub fn is_running(&self, thread_id: &str) -> bool
    pub fn has_pending_permission(&self, request_key: &str) -> bool
    pub async fn run_prompt(
    pub async fn cancel(&self, thread_id: &str)
    pub async fn steer_prompt(
    async fn native_steer_prompt(
    async fn interrupt_for_steer(&self, thread_id: &str)
    pub fn forget_session_of_thread(&self, thread_id: &str)
    pub async fn fork_session(
    pub fn shutdown(&self)
    pub fn seed_model_options(&self, value: Value)
    pub fn refresh_model_options_soon(self: &Arc<Self>)
    pub fn set_alkaid_server_config(self: &Arc<Self>, config: Option<Value>)
    fn with_alkaid_server_config(&self, mut request: Value) -> Value
    pub fn get_model_options(&self) -> Option<Value>
    pub fn spawn_revalidate_model_options(self: &Arc<Self>)
    pub async fn ensure_model_options(self: &Arc<Self>) -> Result<Value, String>
    fn empty_model_options(&self) -> Value
    fn pending_model_options(&self) -> Value
    async fn refresh_model_options(&self) -> Result<Value, String>
    pub fn generate_title_async(
    async fn run_bridge(&self, cwd: &str, request: Value) -> Result<Value, String>
    async fn run_prompt_native(
    async fn run_prompt_bridge(
    fn spawn_idle_bridge(&self, cwd: &str) -> Result<IdleBridge, String>
    async fn read_events(
    fn spawn_bridge(&self, cwd: &str) -> Result<Child, String>
    fn save_session_id(&self, thread_id: &str, session_id: &str)
    fn clear_session_id(&self, thread_id: &str)
    fn save_checkpoint(&self, thread_id: &str, user_item_id: u64, event: &Value)
    fn apply_item(&self, thread_id: &str, value: &Value, ids: &mut HashMap<String, u64>)
    fn apply_plan(&self, thread_id: &str, plan: &Value)
    fn emit_permission(&self, thread_id: &str, permission: &Value)
    pub async fn respond_permission(
    fn push_system(&self, thread_id: &str, text: String, level: &str)
    fn set_running(&self, thread_id: &str, running: bool, stop_reason: Option<&str>)
    fn finish_turn(&self, thread_id: &str, stop_reason: &str, usage: Option<Value>)
    fn is_current_run(&self, thread_id: &str, run_epoch: u64) -> bool
    fn finish_turn_if_current(
    fn emit_update(&self, thread_id: &str, item: &Item) -> Result<(), tauri::Error>
    fn emit_op(&self, thread_id: &str, op: Value) -> Result<(), tauri::Error>
impl Drop for SdkManager
    fn drop(&mut self)
fn bridge_path(app: &AppHandle, adapter: &dyn SdkAdapter) -> Result<PathBuf, String>
fn prompt_parts(adapter: &dyn SdkAdapter, text: &str, images: &[PromptImage]) -> Vec<Value>
fn image_mime_from_path(path: &str) -> Option<&'static str>
async fn write_line(
fn kill_child(child: &mut Child)
fn parse_bridge_output(output: &str, label: &str) -> Result<Value, String>
fn normalize_title(output: &str, fallback: &str) -> String
fn resolve_codex_model(
fn split_codex_effort(value: &str) -> Option<(&str, &str)>
  ## 命中上下文
src-tauri/src/sdk_runtime.rs-1424-                    return Ok(ReadEventsOutcome::Completed);
src-tauri/src/sdk_runtime.rs-1425-                }
src-tauri/src/sdk_runtime.rs-1426-                _ => {}
src-tauri/src/sdk_runtime.rs-1427-            }
src-tauri/src/sdk_runtime.rs-1428-        }
src-tauri/src/sdk_runtime.rs-1429-        Err(format!("{} bridge 意外退出", self.adapter.label()))
src-tauri/src/sdk_runtime.rs-1430-    }
src-tauri/src/sdk_runtime.rs-1431-
src-tauri/src/sdk_runtime.rs-1432-    fn spawn_bridge(&self, cwd: &str) -> Result<Child, String> {
src-tauri/src/sdk_runtime.rs-1433-        let launch = {
src-tauri/src/sdk_runtime.rs-1434-            let state = self.app.state::<AppState>();
src-tauri/src/sdk_runtime.rs-1435-            let settings = state.settings.lock().unwrap();
src-tauri/src/sdk_runtime.rs:1436:            self.adapter.launch_config(&settings)
src-tauri/src/sdk_runtime.rs-1437-        };
src-tauri/src/sdk_runtime.rs-1438-        let program = resolve_program_on_path(&launch.program)
src-tauri/src/sdk_runtime.rs-1439-            .map(|path| path.to_string_lossy().into_owned())
src-tauri/src/sdk_runtime.rs-1440-            .unwrap_or(launch.program);
src-tauri/src/sdk_runtime.rs-1441-        let node = resolve_program_on_path("node").ok_or_else(|| {
src-tauri/src/sdk_runtime.rs-1442-            format!(
src-tauri/src/sdk_runtime.rs-1443-                "未找到 Node.js，{} 需要 Node.js 运行官方 SDK",
src-tauri/src/sdk_runtime.rs-1444-                self.adapter.label()
src-tauri/src/sdk_runtime.rs-1445-            )
src-tauri/src/sdk_runtime.rs-1446-        })?;
src-tauri/src/sdk_runtime.rs-1447-        let bridge = bridge_path(&self.app, self.adapter.as_ref())?;
src-tauri/src/sdk_runtime.rs-1448-        let mut command = Command::new(node);

----- [HIT] src-tauri/src/sdk_adapters/mod.rs  (111 行) -----
     1	mod alkaid;
     2	mod claude;
     3	mod codebuddy;
     4	mod codex;
     5	mod cursor;
     6	
     7	pub use alkaid::AlkaidAdapter;
     8	pub use claude::ClaudeAdapter;
     9	pub use codebuddy::CodeBuddyAdapter;
    10	pub use codex::CodexAdapter;
    11	pub use cursor::CursorAdapter;
    12	
    13	use crate::settings::Settings;
    14	use crate::threads::{AgentKind, CodexUsageSnapshot, ToolCall};
    15	use serde_json::{json, Value};
    16	
    17	pub struct LaunchConfig {
    18	    pub program: String,
    19	    pub proxy: String,
    20	    pub path_env: &'static str,
    21	    pub api_key: Option<(&'static str, String)>,
    22	    pub extra_env: Vec<(&'static str, String)>,
    23	}
    24	
    25	pub trait SdkAdapter: Send + Sync {
    26	    fn agent_kind(&self) -> AgentKind;
    27	    fn label(&self) -> &'static str;
    28	    fn bridge(&self) -> (&'static str, &'static [u8]);
    29	    fn launch_config(&self, settings: &Settings) -> LaunchConfig;
    30	    fn permission_prefix(&self) -> &'static str;
    31	
    32	    /// Extra files written next to the bridge in `~/.nova/runtime/`.
    33	    fn bridge_sidecars(&self) -> &'static [(&'static str, &'static [u8])] {
    34	        &[]
    35	    }
    36	
    37	    fn uses_codex_model_routing(&self) -> bool {
    38	        false
    39	    }
    40	
    41	    fn generates_title(&self) -> bool {
    42	        false
    43	    }
    44	
    45	    fn keeps_bridge_alive(&self) -> bool {
    46	        false
    47	    }
    48	
    49	    fn supports_native_steer(&self) -> bool {
    50	        false
    51	    }
    52	
    53	    fn accepts_data_image(&self, mime_type: &str) -> bool {
    54	        mime_type.starts_with("image/")
    55	    }
    56	
    57	    fn uses_text_deltas(&self) -> bool {
    58	        false
    59	    }
    60	
    61	    fn cancel_grace_attempts(&self) -> usize {
    62	        2
    63	    }
    64	
    65	    fn done_is_cancelled(&self, _event: &Value) -> bool {
    66	        false
    67	    }
    68	
    69	    fn map_tool_call(&self, _value: &Value) -> Option<ToolCall> {
    70	        None
    71	    }
    72	
    73	    fn empty_model_options(&self) -> Value {
    74	        json!({
    75	            "configOptions": [{
    76	                "id": "model",
    77	                "name": "Model",
    78	                "currentValue": "",
    79	                "options": [],
    80	            }],
    81	            "modes": null,
    82	        })
    83	    }
    84	
    85	    fn normalize_usage(
    86	        &self,
    87	        usage: Option<&Value>,
    88	        _codex_baseline: Option<&CodexUsageSnapshot>,
    89	        _session_id: Option<&str>,
    90	    ) -> (Option<Value>, Option<CodexUsageSnapshot>);
    91	}
    92	
    93	fn canonical_usage(
    94	    input: u64,
    95	    output: u64,
    96	    cache_read: Option<u64>,
    97	    cache_write: Option<u64>,
    98	) -> Value {
    99	    let mut usage = json!({
   100	        "inputTokens": input,
   101	        "outputTokens": output,
   102	        "totalTokens": input.saturating_add(output),
   103	    });
   104	    if let Some(value) = cache_read {
   105	        usage["cacheReadTokens"] = value.into();
   106	    }
   107	    if let Some(value) = cache_write {
   108	        usage["cacheWriteTokens"] = value.into();
   109	    }
   110	    usage
   111	}

----- [HIT] src-tauri/src/sdk_adapters/cursor.rs  (94 行) -----
     1	use super::{canonical_usage, LaunchConfig, SdkAdapter};
     2	use crate::settings::Settings;
     3	use crate::threads::{AgentKind, CodexUsageSnapshot};
     4	use serde_json::{json, Value};
     5	
     6	pub struct CursorAdapter;
     7	
     8	impl SdkAdapter for CursorAdapter {
     9	    fn agent_kind(&self) -> AgentKind {
    10	        AgentKind::Cursor
    11	    }
    12	
    13	    fn label(&self) -> &'static str {
    14	        "Cursor+"
    15	    }
    16	
    17	    fn bridge(&self) -> (&'static str, &'static [u8]) {
    18	        (
    19	            "cursor-bridge.mjs",
    20	            include_bytes!("../../resources/cursor-bridge.mjs"),
    21	        )
    22	    }
    23	
    24	    fn launch_config(&self, settings: &Settings) -> LaunchConfig {
    25	        LaunchConfig {
    26	            // Cursor 仅依赖 Node.js 运行官方 SDK bridge，不再读取本机 cursor-agent。
    27	            program: "node".into(),
    28	            proxy: settings.cursor_proxy.clone(),
    29	            path_env: "NOVA_CURSOR_PATH",
    30	            api_key: (!settings.cursor_sdk_api_key.is_empty())
    31	                .then(|| ("CURSOR_API_KEY", settings.cursor_sdk_api_key.clone())),
    32	            extra_env: vec![
    33	                (
    34	                    "NOVA_CURSOR_MODEL_CONTEXTS",
    35	                    serde_json::to_string(&settings.cursor_model_contexts)
    36	                        .unwrap_or_else(|_| "[]".into()),
    37	                ),
    38	                ("NOVA_CONTEXT_MODE", settings.cursor_context_mode.clone()),
    39	            ],
    40	        }
    41	    }
    42	
    43	    fn permission_prefix(&self) -> &'static str {
    44	        "cup"
    45	    }
    46	
    47	    fn generates_title(&self) -> bool {
    48	        true
    49	    }
    50	
    51	    fn keeps_bridge_alive(&self) -> bool {
    52	        true
    53	    }
    54	
    55	    fn empty_model_options(&self) -> Value {
    56	        json!({
    57	            "configOptions": [{
    58	                "id": "model",
    59	                "name": "Model",
    60	                "currentValue": "",
    61	                "options": [{ "value": "", "name": "Auto（Cursor 默认）" }],
    62	            }],
    63	            "modes": null,
    64	        })
    65	    }
    66	
    67	    fn normalize_usage(
    68	        &self,
    69	        usage: Option<&Value>,
    70	        _codex_baseline: Option<&CodexUsageSnapshot>,
    71	        _session_id: Option<&str>,
    72	    ) -> (Option<Value>, Option<CodexUsageSnapshot>) {
    73	        let Some(usage) = usage else {
    74	            return (None, None);
    75	        };
    76	        let Some(input) = usage.get("inputTokens").and_then(Value::as_u64) else {
    77	            return (None, None);
    78	        };
    79	        let Some(output) = usage.get("outputTokens").and_then(Value::as_u64) else {
    80	            return (None, None);
    81	        };
    82	        let cache_read = usage.get("cacheReadTokens").and_then(Value::as_u64);
    83	        let cache_write = usage.get("cacheWriteTokens").and_then(Value::as_u64);
    84	        // The bridge removes Cursor's input/cache-read and output/cache-write overlaps first.
    85	        // Nova stores aggregate inputTokens, so add both disjoint cache categories once here.
    86	        let input = input
    87	            .saturating_add(cache_read.unwrap_or(0))
    88	            .saturating_add(cache_write.unwrap_or(0));
    89	        (
    90	            Some(canonical_usage(input, output, cache_read, cache_write)),
    91	            None,
    92	        )
    93	    }
    94	}

----- [HIT] src-tauri/src/sdk_adapters/codex.rs  (103 行) -----
     1	use super::{canonical_usage, LaunchConfig, SdkAdapter};
     2	use crate::settings::Settings;
     3	use crate::threads::{AgentKind, CodexUsageSnapshot};
     4	use serde_json::Value;
     5	
     6	pub struct CodexAdapter;
     7	
     8	impl SdkAdapter for CodexAdapter {
     9	    fn agent_kind(&self) -> AgentKind {
    10	        AgentKind::Codex
    11	    }
    12	
    13	    fn label(&self) -> &'static str {
    14	        "Codex+"
    15	    }
    16	
    17	    fn bridge(&self) -> (&'static str, &'static [u8]) {
    18	        (
    19	            "codex-bridge.mjs",
    20	            include_bytes!("../../resources/codex-bridge.mjs"),
    21	        )
    22	    }
    23	
    24	    fn launch_config(&self, settings: &Settings) -> LaunchConfig {
    25	        LaunchConfig {
    26	            program: settings.codex_path.clone(),
    27	            proxy: settings.codex_proxy.clone(),
    28	            path_env: "NOVA_CODEX_PATH",
    29	            api_key: None,
    30	            extra_env: Vec::new(),
    31	        }
    32	    }
    33	
    34	    fn permission_prefix(&self) -> &'static str {
    35	        "cdp"
    36	    }
    37	
    38	    fn uses_codex_model_routing(&self) -> bool {
    39	        true
    40	    }
    41	
    42	    fn generates_title(&self) -> bool {
    43	        true
    44	    }
    45	
    46	    fn accepts_data_image(&self, _mime_type: &str) -> bool {
    47	        true
    48	    }
    49	
    50	    fn uses_text_deltas(&self) -> bool {
    51	        true
    52	    }
    53	
    54	    fn normalize_usage(
    55	        &self,
    56	        usage: Option<&Value>,
    57	        codex_baseline: Option<&CodexUsageSnapshot>,
    58	        session_id: Option<&str>,
    59	    ) -> (Option<Value>, Option<CodexUsageSnapshot>) {
    60	        let Some(usage) = usage else {
    61	            return (None, None);
    62	        };
    63	        let Some(input) = usage.get("input_tokens").and_then(Value::as_u64) else {
    64	            return (None, None);
    65	        };
    66	        let Some(output) = usage.get("output_tokens").and_then(Value::as_u64) else {
    67	            return (None, None);
    68	        };
    69	        let cache_read = usage
    70	            .get("cached_input_tokens")
    71	            .and_then(Value::as_u64)
    72	            .unwrap_or(0);
    73	        let cache_write = usage
    74	            .get("cache_creation_input_tokens")
    75	            .and_then(Value::as_u64)
    76	            .unwrap_or(0);
    77	        let snapshot = CodexUsageSnapshot {
    78	            session_id: session_id.map(str::to_string),
    79	            input_tokens: input,
    80	            output_tokens: output,
    81	            cache_read_tokens: cache_read,
    82	            cache_write_tokens: cache_write,
    83	        };
    84	        let Some(previous) = codex_baseline.filter(|previous| {
    85	            previous.session_id.as_deref() == session_id
    86	                && input >= previous.input_tokens
    87	                && output >= previous.output_tokens
    88	        }) else {
    89	            return (None, Some(snapshot));
    90	        };
    91	        (
    92	            Some(canonical_usage(
    93	                input - previous.input_tokens,
    94	                output - previous.output_tokens,
    95	                (cache_read >= previous.cache_read_tokens)
    96	                    .then_some(cache_read - previous.cache_read_tokens),
    97	                (cache_write >= previous.cache_write_tokens)
    98	                    .then_some(cache_write - previous.cache_write_tokens),
    99	            )),
   100	            Some(snapshot),
   101	        )
   102	    }
   103	}

----- [HIT] src-tauri/src/sdk_adapters/codebuddy.rs  (69 行) -----
     1	use super::{canonical_usage, LaunchConfig, SdkAdapter};
     2	use crate::settings::Settings;
     3	use crate::threads::{AgentKind, CodexUsageSnapshot};
     4	use serde_json::Value;
     5	
     6	pub struct CodeBuddyAdapter;
     7	
     8	impl SdkAdapter for CodeBuddyAdapter {
     9	    fn agent_kind(&self) -> AgentKind {
    10	        AgentKind::CodeBuddy
    11	    }
    12	
    13	    fn label(&self) -> &'static str {
    14	        "CodeBuddy+"
    15	    }
    16	
    17	    fn bridge(&self) -> (&'static str, &'static [u8]) {
    18	        (
    19	            "codebuddy-bridge.cjs",
    20	            include_bytes!("../../resources/codebuddy-bridge.cjs"),
    21	        )
    22	    }
    23	
    24	    fn launch_config(&self, settings: &Settings) -> LaunchConfig {
    25	        LaunchConfig {
    26	            program: settings.codebuddy_path.clone(),
    27	            proxy: settings.codebuddy_proxy.clone(),
    28	            path_env: "NOVA_CODEBUDDY_PATH",
    29	            api_key: None,
    30	            extra_env: Vec::new(),
    31	        }
    32	    }
    33	
    34	    fn permission_prefix(&self) -> &'static str {
    35	        "cbp"
    36	    }
    37	
    38	    fn generates_title(&self) -> bool {
    39	        true
    40	    }
    41	
    42	    fn normalize_usage(
    43	        &self,
    44	        usage: Option<&Value>,
    45	        _codex_baseline: Option<&CodexUsageSnapshot>,
    46	        _session_id: Option<&str>,
    47	    ) -> (Option<Value>, Option<CodexUsageSnapshot>) {
    48	        let Some(usage) = usage else {
    49	            return (None, None);
    50	        };
    51	        let Some(input) = usage.get("input_tokens").and_then(Value::as_u64) else {
    52	            return (None, None);
    53	        };
    54	        let Some(output) = usage.get("output_tokens").and_then(Value::as_u64) else {
    55	            return (None, None);
    56	        };
    57	        let cache_read = usage.get("cache_read_input_tokens").and_then(Value::as_u64);
    58	        let cache_write = usage
    59	            .get("cache_creation_input_tokens")
    60	            .and_then(Value::as_u64);
    61	        let input = input
    62	            .saturating_add(cache_read.unwrap_or(0))
    63	            .saturating_add(cache_write.unwrap_or(0));
    64	        (
    65	            Some(canonical_usage(input, output, cache_read, cache_write)),
    66	            None,
    67	        )
    68	    }
    69	}

----- [HIT] src-tauri/src/sdk_adapters/claude.rs  (70 行) -----
     1	use super::{canonical_usage, LaunchConfig, SdkAdapter};
     2	use crate::settings::Settings;
     3	use crate::threads::{AgentKind, CodexUsageSnapshot};
     4	use serde_json::Value;
     5	
     6	pub struct ClaudeAdapter;
     7	
     8	impl SdkAdapter for ClaudeAdapter {
     9	    fn agent_kind(&self) -> AgentKind {
    10	        AgentKind::ClaudeCode
    11	    }
    12	
    13	    fn label(&self) -> &'static str {
    14	        "Claude Code+"
    15	    }
    16	
    17	    fn bridge(&self) -> (&'static str, &'static [u8]) {
    18	        (
    19	            "claude-bridge.mjs",
    20	            include_bytes!("../../resources/claude-bridge.mjs"),
    21	        )
    22	    }
    23	
    24	    fn launch_config(&self, settings: &Settings) -> LaunchConfig {
    25	        LaunchConfig {
    26	            program: settings.claudecode_path.clone(),
    27	            proxy: settings.claudecode_proxy.clone(),
    28	            path_env: "NOVA_CLAUDE_PATH",
    29	            api_key: (!settings.claudecode_sdk_api_key.is_empty())
    30	                .then(|| ("ANTHROPIC_API_KEY", settings.claudecode_sdk_api_key.clone())),
    31	            extra_env: Vec::new(),
    32	        }
    33	    }
    34	
    35	    fn permission_prefix(&self) -> &'static str {
    36	        "clp"
    37	    }
    38	
    39	    fn normalize_usage(
    40	        &self,
    41	        usage: Option<&Value>,
    42	        _codex_baseline: Option<&CodexUsageSnapshot>,
    43	        _session_id: Option<&str>,
    44	    ) -> (Option<Value>, Option<CodexUsageSnapshot>) {
    45	        normalize_claude_usage(usage)
    46	    }
    47	}
    48	
    49	fn normalize_claude_usage(usage: Option<&Value>) -> (Option<Value>, Option<CodexUsageSnapshot>) {
    50	    let Some(usage) = usage else {
    51	        return (None, None);
    52	    };
    53	    let Some(input) = usage.get("input_tokens").and_then(Value::as_u64) else {
    54	        return (None, None);
    55	    };
    56	    let Some(output) = usage.get("output_tokens").and_then(Value::as_u64) else {
    57	        return (None, None);
    58	    };
    59	    let cache_read = usage.get("cache_read_input_tokens").and_then(Value::as_u64);
    60	    let cache_write = usage
    61	        .get("cache_creation_input_tokens")
    62	        .and_then(Value::as_u64);
    63	    let input = input
    64	        .saturating_add(cache_read.unwrap_or(0))
    65	        .saturating_add(cache_write.unwrap_or(0));
    66	    (
    67	        Some(canonical_usage(input, output, cache_read, cache_write)),
    68	        None,
    69	    )
    70	}

----- [HIT] src-tauri/src/sdk_adapters/alkaid.rs  (263 行) -----
  ## 符号大纲
pub struct AlkaidAdapter;
impl SdkAdapter for AlkaidAdapter
    fn agent_kind(&self) -> AgentKind
    fn label(&self) -> &'static str
    fn bridge(&self) -> (&'static str, &'static [u8])
    fn bridge_sidecars(&self) -> &'static [(&'static str, &'static [u8])]
    fn launch_config(&self, settings: &Settings) -> LaunchConfig
    fn permission_prefix(&self) -> &'static str
    fn generates_title(&self) -> bool
    fn supports_native_steer(&self) -> bool
    fn cancel_grace_attempts(&self) -> usize
    fn done_is_cancelled(&self, event: &Value) -> bool
    fn map_tool_call(&self, value: &Value) -> Option<ToolCall>
    fn normalize_usage(
fn alkaid_tool_call(value: &Value) -> ToolCall
fn tool_detail(tool: &str, arguments: Option<&Value>) -> Option<String>
fn argument_paths(arguments: Option<&Value>) -> Vec<Value>
fn result_text(result: &Value) -> Option<String>
fn text_content(text: &str) -> Vec<Value>
    fn tools_preserve_arguments_outputs_and_locations()
  ## 命中上下文
src-tauri/src/sdk_adapters/alkaid.rs-21-        )
src-tauri/src/sdk_adapters/alkaid.rs-22-    }
src-tauri/src/sdk_adapters/alkaid.rs-23-
src-tauri/src/sdk_adapters/alkaid.rs-24-    fn bridge_sidecars(&self) -> &'static [(&'static str, &'static [u8])] {
src-tauri/src/sdk_adapters/alkaid.rs-25-        &[
src-tauri/src/sdk_adapters/alkaid.rs-26-            (
src-tauri/src/sdk_adapters/alkaid.rs-27-                "photon_rs_bg.wasm",
src-tauri/src/sdk_adapters/alkaid.rs-28-                include_bytes!("../../resources/photon_rs_bg.wasm"),
src-tauri/src/sdk_adapters/alkaid.rs-29-            ),
src-tauri/src/sdk_adapters/alkaid.rs-30-        ]
src-tauri/src/sdk_adapters/alkaid.rs-31-    }
src-tauri/src/sdk_adapters/alkaid.rs-32-
src-tauri/src/sdk_adapters/alkaid.rs:33:    fn launch_config(&self, settings: &Settings) -> LaunchConfig {
src-tauri/src/sdk_adapters/alkaid.rs-34-        LaunchConfig {
src-tauri/src/sdk_adapters/alkaid.rs-35-            program: "node".into(),
src-tauri/src/sdk_adapters/alkaid.rs-36-            proxy: settings.vega_proxy.clone(),
src-tauri/src/sdk_adapters/alkaid.rs-37-            path_env: "ALKAID_RUNTIME",
src-tauri/src/sdk_adapters/alkaid.rs-38-            api_key: None,
src-tauri/src/sdk_adapters/alkaid.rs-39-            extra_env: vec![("NOVA_CONTEXT_MODE", settings.vega_context_mode.clone())],
src-tauri/src/sdk_adapters/alkaid.rs-40-        }
src-tauri/src/sdk_adapters/alkaid.rs-41-    }
src-tauri/src/sdk_adapters/alkaid.rs-42-
src-tauri/src/sdk_adapters/alkaid.rs-43-    fn permission_prefix(&self) -> &'static str {
src-tauri/src/sdk_adapters/alkaid.rs-44-        "alk"
src-tauri/src/sdk_adapters/alkaid.rs-45-    }

# ===== 1 跳扩展文件(仅大纲) =====
----- [NEIGHBOR] src-tauri/src/sdk_adapters/codebuddy.rs -----
pub struct CodeBuddyAdapter;
impl SdkAdapter for CodeBuddyAdapter
    fn agent_kind(&self) -> AgentKind
    fn label(&self) -> &'static str
    fn bridge(&self) -> (&'static str, &'static [u8])
    fn launch_config(&self, settings: &Settings) -> LaunchConfig
    fn permission_prefix(&self) -> &'static str
    fn generates_title(&self) -> bool
    fn normalize_usage(

(预算耗尽, 剩余邻居仅列名于下)
# ===== 未展开的大文件(可追加) =====
