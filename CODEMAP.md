# CODEMAP — nova-client (generated 2026-07-31)

## File tree (LOC)
   121  scripts/alkaid-bridge-common.mjs
     8  scripts/alkaid-bridge.mjs
   250  scripts/alkaid-config.mjs
   564  scripts/alkaid-context-reasonix.mjs
   209  scripts/alkaid-context-super-memory.mjs
   428  scripts/alkaid-context-super.mjs
  1070  scripts/alkaid-context-super.test.mjs
   911  scripts/alkaid-core.mjs
    42  scripts/alkaid-diagnostics.mjs
   327  scripts/alkaid-slim-memory.mjs
   267  scripts/alkaid-smart-edit.mjs
    57  scripts/alkaid.mjs
  1159  scripts/alkaid.test.mjs
    92  scripts/audit-context-parity.mjs
    99  scripts/build-alkaid-bridge.mjs
   201  scripts/claude-bridge.mjs
    60  scripts/claude-bridge.test.mjs
   242  scripts/codebuddy-bridge.mjs
    63  scripts/codebuddy-bridge.test.mjs
   191  scripts/codex-bridge.mjs
   269  scripts/cursor-bridge-common.mjs
    15  scripts/cursor-bridge.mjs
   844  scripts/cursor-bridge.test.mjs
  1285  scripts/cursor-context-reasonix.mjs
  1167  scripts/cursor-context-super.mjs
   676  scripts/cursor-context-super.test.mjs
   277  scripts/cursor-filesystem-tools.mjs
    16  scripts/fixtures/mcp-echo-server.mjs
   529  scripts/legacy-context/pre-reasonix-4582ebf/alkaid-bridge.mjs
   209  scripts/legacy-context/pre-reasonix-4582ebf/alkaid-slim-memory.mjs
  1370  scripts/legacy-context/pre-reasonix-4582ebf/cursor-bridge.mjs
   383  scripts/opencode-bridge.mjs
   198  scripts/opencode-bridge.test.mjs
   861  scripts/pi-golden.mjs
   133  scripts/probe-acp.mjs
   139  scripts/probe-big.mjs
   140  scripts/probe-load.mjs
   144  scripts/probe-steer.mjs
   129  scripts/ui-screenshot.mjs
    55  src-tauri/build.rs
   343  src-tauri/pi_core/src/agent/agent.rs
   101  src-tauri/pi_core/src/agent/messages.rs
    16  src-tauri/pi_core/src/agent/mod.rs
   371  src-tauri/pi_core/src/agent/run_loop.rs
   414  src-tauri/pi_core/src/alkaid_config.rs
   285  src-tauri/pi_core/src/bridge.rs
   389  src-tauri/pi_core/src/edit_diff.rs
    81  src-tauri/pi_core/src/encoding.rs
    85  src-tauri/pi_core/src/lib.rs
   103  src-tauri/pi_core/src/ls.rs
   153  src-tauri/pi_core/src/paths.rs
   162  src-tauri/pi_core/src/payload.rs
   115  src-tauri/pi_core/src/prompt.rs
   401  src-tauri/pi_core/src/provider.rs
   554  src-tauri/pi_core/src/provider_anthropic.rs
   416  src-tauri/pi_core/src/provider_google.rs
   473  src-tauri/pi_core/src/provider_responses.rs
   161  src-tauri/pi_core/src/read.rs
   106  src-tauri/pi_core/src/skills.rs
   270  src-tauri/pi_core/src/skills_discovery.rs
   927  src-tauri/pi_core/src/slim_memory.rs
   576  src-tauri/pi_core/src/smart_edit.rs
   202  src-tauri/pi_core/src/text.rs
   582  src-tauri/pi_core/src/tools.rs
   276  src-tauri/pi_core/src/transform.rs
   253  src-tauri/pi_core/src/truncate.rs
    21  src-tauri/pi_core/src/write.rs
  1037  src-tauri/pi_core/tests/parity.rs
  3156  src-tauri/src/acp.rs
   530  src-tauri/src/agent_config.rs
   349  src-tauri/src/cli.rs
   868  src-tauri/src/cli_manager.rs
  1295  src-tauri/src/clues.rs
  2746  src-tauri/src/codex.rs
   470  src-tauri/src/codex_radar.rs
   849  src-tauri/src/credential_roaming.rs
  7909  src-tauri/src/employees.rs
   222  src-tauri/src/gitwt.rs
   146  src-tauri/src/http_stream.rs
  5745  src-tauri/src/lib.rs
   299  src-tauri/src/main.rs
   366  src-tauri/src/marks.rs
   358  src-tauri/src/mcp.rs
  1636  src-tauri/src/mind.rs
    31  src-tauri/src/model_cache.rs
  1601  src-tauri/src/notice.rs
  1425  src-tauri/src/opencode_sdk.rs
   432  src-tauri/src/path_env.rs
   341  src-tauri/src/quota.rs
  4655  src-tauri/src/relay.rs
  2213  src-tauri/src/remote.rs
   263  src-tauri/src/sdk_adapters/alkaid.rs
    70  src-tauri/src/sdk_adapters/claude.rs
    69  src-tauri/src/sdk_adapters/codebuddy.rs
   103  src-tauri/src/sdk_adapters/codex.rs
    94  src-tauri/src/sdk_adapters/cursor.rs
   111  src-tauri/src/sdk_adapters/mod.rs
  2695  src-tauri/src/sdk_runtime.rs
   197  src-tauri/src/semantic.rs
   610  src-tauri/src/server.rs
   409  src-tauri/src/settings.rs
    46  src-tauri/src/signature.rs
   544  src-tauri/src/skills.rs
    70  src-tauri/src/sleep_inhibitor.rs
   201  src-tauri/src/sys_notify.rs
  2068  src-tauri/src/threads.rs
  1120  src-tauri/src/time_machine.rs
  1323  src-tauri/src/updater.rs
   623  src-tauri/src/vega_native.rs
   341  src-tauri/src/vega_provider.rs
   224  src-tauri/src/vega_reasonix.rs
   327  src-tauri/src/windows_shell_shim.rs
   197  src-tauri/windows-shell-shim.rs
   102  src/App.tsx
    51  src/components/AchievementBadge.tsx
   122  src/components/AchievementsModal.tsx
  2706  src/components/CanvasTranscript.tsx
  1250  src/components/ChatView.tsx
    92  src/components/CliOperationModal.tsx
   319  src/components/ClueCaptureModal.tsx
   730  src/components/Composer.tsx
   369  src/components/ConfigSelects.tsx
   889  src/components/DecisionWorkbench.tsx
   140  src/components/EditedFilesCard.tsx
  1387  src/components/EmployeesView.tsx
    50  src/components/EngravedNumberMark.tsx
  1483  src/components/EvidenceChainView.tsx
    75  src/components/ExclusiveChatMark.tsx
    63  src/components/FileContextMenu.tsx
  1068  src/components/HomeView.tsx
   206  src/components/ImageAttachmentStrip.tsx
   339  src/components/Markdown.tsx
   168  src/components/MentionPicker.tsx
   105  src/components/NoteFlow.tsx
   158  src/components/PermissionCard.tsx
    22  src/components/PlanActionCard.tsx
    32  src/components/PlanCard.tsx
   230  src/components/ProjectPicker.tsx
   176  src/components/RoamRequestModal.tsx
   553  src/components/SearchSelect.tsx
  2100  src/components/SettingsModal.tsx
   190  src/components/ShareInboxModal.tsx
   207  src/components/ShareModal.tsx
   858  src/components/Sidebar.tsx
   124  src/components/SignatureSplash.tsx
   375  src/components/ToolCallCard.tsx
   225  src/components/TranscriptItem.tsx
   258  src/components/TurnGroup.tsx
    54  src/components/TypewriterText.tsx
   107  src/components/UpdateModal.tsx
   152  src/components/icons.tsx
    10  src/components/signatureOverlay.ts
    19  src/components/slashMenuLayout.ts
    55  src/components/slashSuggestions.ts
    13  src/fonts.d.ts
    11  src/index.tsx
   433  src/ipc.ts
    22  src/promptDraft.ts
   138  src/promptQueue.ts
  2516  src/store.ts
    65  src/threadDisplay.ts
   970  src/types.ts
    79  src/utils.ts
    19  vite.config.ts

## Rust symbols

### src-tauri/build.rs
1:fn build_windows_shell_shim()
44:fn main()

### src-tauri/pi_core/src/agent/agent.rs
14:pub struct PendingMessageQueue
19:impl PendingMessageQueue
20:    pub fn new(mode: &str) -> Self
27:    pub fn enqueue(&mut self, message: Value)
31:    pub fn has_items(&self) -> bool
35:    pub fn drain(&mut self) -> Vec<Value>
46:    pub fn clear(&mut self)
53:pub struct AgentState
65:impl AgentState
66:    pub fn new(system_prompt: &str, model: Value, tools: Vec<Value>, messages: Vec<Value>) -> Self
81:pub struct Agent
94:impl Agent
95:    pub fn new(state: AgentState, steering_mode: &str, follow_up_mode: &str) -> Self
107:    pub fn subscribe(&mut self, listener: Box<dyn FnMut(&Value)>)
111:    pub fn steer(&mut self, message: Value)
115:    pub fn follow_up(&mut self, message: Value)
119:    pub fn clear_steering_queue(&mut self)
123:    pub fn clear_follow_up_queue(&mut self)
127:    pub fn has_queued_messages(&self) -> bool
133:    pub fn normalize_prompt_input(&self, input: &Value, images: &[Value], timestamp: u64) -> Vec<Value>
147:    fn process_event(&mut self, event: &Value)
192:    pub fn prompt(
206:    pub fn run_continuation(
217:    fn run_prompt_messages(
286:    fn queue_drain_all()
298:    fn queue_drain_one_at_a_time()
308:    fn normalize_prompt_input_string()
322:    fn normalize_prompt_input_string_with_images()
334:    fn normalize_prompt_input_array_passthrough()

### src-tauri/pi_core/src/agent/messages.rs
14:pub fn create_error_tool_result(message: &str) -> Value
24:pub fn create_tool_result_message(
60:    fn error_result_shape()
69:    fn tool_result_omits_absent_details()
76:    fn tool_result_null_content_becomes_empty()
83:    fn tool_result_added_tool_names_only_when_nonempty()

### src-tauri/pi_core/src/agent/run_loop.rs
18:pub struct LoopContext
26:pub type ToolFn<'a> = dyn FnMut(&str, &Value) -> (Value, bool) + 'a;
31:pub struct StreamTurn
38:pub type StreamFn<'a> = dyn FnMut(usize, &Value) -> StreamTurn + 'a;
41:pub type QueueFn<'a> = dyn FnMut() -> Vec<Value> + 'a;
49:pub type PrepareNextTurnFn<'a> = dyn FnMut(&Value, &[Value], &mut LoopContext) + Send + 'a;
51:pub struct LoopConfig<'a>
63:fn drain(queue: &mut Option<Box<QueueFn>>) -> Vec<Value>
67:fn tool_calls_of(message: &Value) -> Vec<Value>
81:fn has_tool(context: &LoopContext, name: &str) -> bool
89:fn default_convert_to_llm(messages: &[Value]) -> Vec<Value>
106:fn stream_assistant_response(
177:struct FinalizedTool
184:fn emit_tool_start(emit: &mut dyn FnMut(Value), tool_call: &Value)
193:fn run_tool_call(context: &LoopContext, tool_call: &Value, tool_fn: &mut ToolFn) -> FinalizedTool
205:fn emit_tool_end(emit: &mut dyn FnMut(Value), finalized: &FinalizedTool)
210:fn emit_tool_result_message(
228:fn execute_one_tool(
243:pub fn run_agent_loop(
267:fn run_loop_body(

### src-tauri/pi_core/src/alkaid_config.rs
17:pub fn strip_json_comments(text: &str) -> String
63:pub fn strip_trailing_commas(text: &str) -> String
103:pub fn parse_jsonc(text: &str) -> Result<Value, String>
108:fn is_plain_object(value: &Value) -> bool
114:pub fn merge_config(server_config: &Value, local_config: &Value) -> Value
139:pub fn resolve_env(value: &Value, env: &HashMap<String, String>) -> Result<Value, String>
162:pub fn provider_api(provider: &Value) -> Result<String, String>
182:fn regex(pattern: &str) -> &'static regex::Regex
189:pub fn merge_compat_defaults(
251:pub fn resolve_model(

### src-tauri/pi_core/src/bridge.rs
16:pub fn started_tool_item(event: &Value) -> Value
85:pub fn aggregated_output(result: Option<&Value>) -> String
106:pub fn completed_tool_item(started: &Value, end_event: &Value) -> Value
134:pub struct ProtocolAccumulator
145:impl ProtocolAccumulator
146:    pub fn new() -> Self
151:    pub fn on_event(&mut self, event: &Value)
232:    pub fn finish(self) -> Vec<Value>
242:    fn accumulates_text_and_tool_items()
264:    fn merges_usage_across_turns()
273:    fn thinking_accumulates_separately_from_text()

### src-tauri/pi_core/src/edit_diff.rs
13:pub fn detect_line_ending(content: &str) -> &'static str
30:pub fn normalize_to_lf(text: &str) -> String
35:pub fn restore_line_endings(text: &str, ending: &str) -> String
44:fn js_trim_end(line: &str) -> &str
54:fn fold_fuzzy_char(c: char) -> char
67:pub fn normalize_for_fuzzy_match(text: &str) -> String
78:pub struct FuzzyMatch
87:pub fn fuzzy_find_text(content: &str, old_text: &str) -> FuzzyMatch
118:pub fn strip_bom(content: &str) -> (&str, &str)
125:fn count_occurrences(content: &str, old_text: &str) -> usize
132:struct MatchedEdit
141:fn apply_replacements(content: &str, replacements: &[MatchedEdit], offset: usize) -> String
156:fn split_lines_with_endings(content: &str) -> Vec<&str>
172:struct LineSpan
177:fn get_line_spans(content: &str) -> Vec<LineSpan>
192:fn get_replacement_line_range(
220:fn apply_replacements_preserving_unchanged_lines(
234:    struct Group
276:pub struct EditResult
281:fn not_found_error(path: &str, edit_index: usize, total_edits: usize) -> String
289:fn duplicate_error(path: &str, edit_index: usize, total_edits: usize, occurrences: usize) -> String
297:fn empty_old_text_error(path: &str, edit_index: usize, total_edits: usize) -> String
305:fn no_change_error(path: &str, total_edits: usize) -> String
315:pub fn apply_edits_to_normalized_content(

### src-tauri/pi_core/src/encoding.rs
6:pub enum Encoding
14:pub fn detect_text_encoding(buffer: &[u8]) -> (Encoding, usize)
52:pub fn swap_utf16_bytes(buffer: &[u8]) -> Vec<u8>
62:fn decode_utf16le_bytes(buffer: &[u8]) -> String
73:pub fn decode_text_buffer(buffer: &[u8]) -> String

### src-tauri/pi_core/src/ls.rs
17:pub struct LsOutput
25:pub fn ls_tool(cwd: &str, path: Option<&str>, limit: Option<usize>) -> Result<LsOutput, String>

### src-tauri/pi_core/src/paths.rs
12:pub struct NormalizeOptions
20:impl Default for NormalizeOptions
21:    fn default() -> Self
34:fn is_unicode_space(c: char) -> bool
44:fn default_home_dir() -> String
50:fn normalize_components(path: &str) -> String
66:fn file_url_to_path(url: &str) -> String
71:pub fn normalize_path(input: &str, options: &NormalizeOptions) -> String
109:fn is_absolute(path: &str) -> bool
114:pub fn dirname(path: &str) -> String
131:pub fn resolve_path(input: &str, base_dir: &str, options: &NormalizeOptions) -> String
143:pub fn resolve_to_cwd(file_path: &str, cwd: &str) -> String

### src-tauri/pi_core/src/payload.rs
13:pub fn clamp_prompt_cache_key(key: Option<&str>) -> Option<String>
28:pub fn inject_openai_prompt_cache_key(payload: &Value, session_id: Option<&str>) -> Option<Value>
50:pub fn clamp_openai_payload_tool_outputs(payload: &Value, max_chars: usize) -> Option<Value>
124:pub fn merge_usage(total: Option<&Value>, usage: Option<&Value>) -> Option<Value>
154:fn json_number(value: f64) -> Value

### src-tauri/pi_core/src/prompt.rs
12:pub enum ShellKind
18:pub struct ShellConfig
26:fn optimize(stable_parts: &[String], dynamic_parts: &[String]) -> String
52:pub fn build_system_prompt(

### src-tauri/pi_core/src/provider.rs
16:pub fn pi_message_to_openai(message: &Value) -> Option<Value>
107:pub fn pi_tools_to_openai(tools: &[Value]) -> Vec<Value>
124:pub fn build_openai_chat_request(
153:pub fn map_finish_reason(finish_reason: Option<&str>) -> &'static str
163:struct ToolCallAcc
172:pub struct OpenAiChatAccumulator
184:impl OpenAiChatAccumulator
185:    pub fn new(model: &str, provider: &str, api: &str) -> Self
194:    fn current_partial(&self) -> Value
198:    fn build_message(&self, stop_reason: &str) -> Value
223:    fn pi_usage(&self) -> Value
245:    pub fn add_chunk(&mut self, chunk: &Value)
297:    pub fn finish(mut self) -> StreamTurn
316:    fn converts_user_text_message()
325:    fn converts_assistant_tool_call()
342:    fn converts_tool_result()
351:    fn builds_request_with_system_and_tools()
363:    fn accumulates_text_stream()
380:    fn accumulates_tool_call_stream()
395:    fn maps_finish_reasons()

### src-tauri/pi_core/src/provider_anthropic.rs
15:fn normalize_tool_call_id(id: &str) -> String
24:pub fn pi_tools_to_anthropic(tools: &[Value]) -> Vec<Value>
44:fn convert_content_blocks(content: &[Value]) -> Value
81:fn convert_messages(transformed: &[Value], cache_control: Option<&Value>) -> Vec<Value>
237:pub fn build_anthropic_request(
322:pub fn map_anthropic_stop_reason(reason: &str) -> (&'static str, Option<String>)
337:pub struct AnthropicAccumulator
355:struct ToolCallAcc
361:impl AnthropicAccumulator
362:    pub fn new(model: &str, provider: &str, api: &str) -> Self
372:    fn build_message(&self, stop_reason: &str) -> Value
410:    fn current_partial(&self) -> Value
415:    pub fn add_event(&mut self, event: &Value)
525:    fn merge_usage(&mut self, usage: &Value)
542:    pub fn finish(mut self) -> StreamTurn

### src-tauri/pi_core/src/provider_google.rs
15:fn normalize_tool_call_id(id: &str) -> String
23:fn requires_tool_call_id(model_id: &str) -> bool
28:pub fn pi_tools_to_google(tools: &[Value]) -> Value
42:fn convert_google_messages(
191:pub fn build_google_request(
234:pub fn map_google_stop_reason(reason: &str) -> &'static str
245:pub struct GoogleAccumulator
263:struct GoogleToolCall
269:impl GoogleAccumulator
270:    pub fn new(model: &str, provider: &str, api: &str) -> Self
280:    fn build_message(&self, stop_reason: &str) -> Value
313:    fn current_partial(&self) -> Value
317:    fn ensure_started(&mut self)
325:    pub fn add_chunk(&mut self, chunk: &Value)
387:    fn merge_usage(&mut self, usage: &Value)
404:    pub fn finish(mut self) -> StreamTurn

### src-tauri/pi_core/src/provider_responses.rs
15:fn normalize_id_part(part: &str) -> String
24:fn normalize_tool_call_id(id: &str) -> String
39:pub fn pi_tools_to_responses(tools: &[Value]) -> Vec<Value>
55:fn convert_responses_messages(
200:pub fn build_openai_responses_request(
246:fn map_responses_stop_reason(status: Option<&str>) -> &'static str
257:pub struct ResponsesAccumulator
274:struct ToolCallSlot
281:impl ResponsesAccumulator
282:    pub fn new(model: &str, provider: &str, api: &str) -> Self
292:    fn build_message(&self, stop_reason: &str) -> Value
331:    fn current_partial(&self) -> Value
335:    fn ensure_started(&mut self)
343:    pub fn add_event(&mut self, event: &Value)
438:    fn merge_usage(&mut self, usage: &Value)
461:    pub fn finish(mut self) -> StreamTurn

### src-tauri/pi_core/src/read.rs
24:fn split_lines_readline(text: &str) -> Vec<String>
51:pub struct ReadLines
60:pub fn read_text_lines(text: &str, offset: usize, limit: usize, max_bytes: usize) -> ReadLines
106:pub fn read_file_text(bytes: &[u8]) -> String
117:fn decode_utf16le(bytes: &[u8]) -> String
127:pub struct ReadRequest
137:pub fn read_files_one(root: &Path, request: &ReadRequest) -> Value

### src-tauri/pi_core/src/skills.rs
18:pub struct Skill
26:fn escape_xml(value: &str) -> String
36:pub fn format_skills_for_prompt(skills: &[Skill]) -> String
67:pub fn format_skills_for_prompt_compressed(skills: &[Skill]) -> String
97:pub fn format_alkaid_skills_prompt(skills: &[Skill]) -> String

### src-tauri/pi_core/src/skills_discovery.rs
29:fn clean_value(value: &str) -> String
43:pub fn parse_frontmatter(content: &str) -> HashMap<String, String>
68:fn load_skill_from_file(path: &Path) -> Option<Skill>
97:fn load_skills_internal(dir: &Path, include_root_files: bool) -> Vec<Skill>
141:pub fn load_skills_from_dir(dir: &Path) -> Vec<Skill>
146:pub fn strip_skill_frontmatter(content: &str) -> String
163:pub fn expand_skill_command(text: &str, skills: &[Skill]) -> String
195:    fn parses_frontmatter_fields()
204:    fn frontmatter_falls_back_to_parent_name_and_requires_description()
223:    fn skill_md_stops_recursion_and_root_md_loaded()
245:    fn strips_frontmatter()
252:    fn expands_skill_command()

### src-tauri/pi_core/src/slim_memory.rs
25:pub struct Turn
34:pub struct NormalizedMemory
44:pub struct SlimMemory
63:impl Default for SlimMemory
64:    fn default() -> Self
69:impl SlimMemory
71:    pub fn new() -> Self
93:    pub fn append_turn(&mut self, user_prompt: &str)
104:    pub fn set_latest_conclusion(&mut self, content: &Value)
128:    pub fn normalize_in_place(&mut self)
135:    pub fn normalized(&self) -> NormalizedMemory
144:    pub fn format(&self) -> String
151:pub fn text_content(content: &Value) -> String
166:fn trim_filter(values: &[String]) -> Vec<String>
177:fn merge_turns(turns: Vec<Turn>) -> Vec<Turn>
205:fn string_from_value(value: &Value) -> String
215:pub fn normalize_value(input: &Value) -> NormalizedMemory
296:pub fn format_normalized(memory: &NormalizedMemory) -> String
329:pub fn format_slim_memory(memory: &Value) -> String
334:pub fn memory_without_current(memory: &Value, pending_messages: bool) -> NormalizedMemory
371:pub fn estimate_context_tokens(text: &str) -> u64
385:pub fn context_pressure_tier(current_tokens: f64, context_window: f64) -> &'static str
403:fn num_or_zero(value: Option<&Value>) -> u64
418:fn first_present<'a>(obj: &'a Map<String, Value>, keys: &[&str]) -> Option<&'a Value>
425:pub fn context_tokens_from_messages(messages: &[Value]) -> u64
452:pub fn strip_completed_openai_reasoning(messages: &[Value]) -> Vec<Value>
496:fn compact_tool_text(text: &str, tier: &str, tool_call_id: Option<&str>) -> String
514:pub fn compact_native_tool_results(
576:pub fn should_use_full_context(
603:pub fn rebase_native_context_for_slim_memory(
650:pub fn seed_slim_memory_from_messages(memory: &mut SlimMemory, messages: &[Value])
675:pub struct CompactOptions
685:pub fn compact_slim_memory<F>(memory: &mut SlimMemory, options: CompactOptions, mut summarize: F) -> bool
762:pub struct CompactionPlan
774:pub fn plan_compaction(memory: &mut SlimMemory, options: CompactOptions) -> Option<CompactionPlan>
842:pub fn apply_compaction(memory: &mut SlimMemory, plan: &CompactionPlan, digest: &str) -> bool
859:    fn memory_with_turns(n: usize) -> SlimMemory
871:    fn plan_apply_matches_compact_slim_memory()
899:    fn plan_returns_none_when_within_limits()
914:    fn apply_with_empty_digest_is_noop()

### src-tauri/pi_core/src/smart_edit.rs
22:fn map_unicode(c: char) -> char
37:fn normalize_unicode(value: &str) -> String
41:fn line_indent(line: &str) -> &str
50:fn relative_indent_lines(lines: &[String]) -> Vec<String>
69:enum Mode
75:impl Mode
76:    fn name(self) -> &'static str
84:    fn map(self, line: &str) -> String
93:fn find_occurrences(content: &[u16], needle: &[u16]) -> Vec<usize>
112:struct Table
117:fn build_line_table(units: &[u16]) -> Table
137:fn build_index(lines: &[String]) -> HashMap<String, Vec<usize>>
148:struct Candidates
154:fn candidate_starts(target: &[String], pattern: &[String], mode: Mode) -> Candidates
193:fn same_sequence(lines: &[String], pattern: &[String], start: usize) -> bool
202:struct Span
208:fn span_for_lines(table: &Table, start: usize, pattern: &[String]) -> Span
222:fn token_set(value: &str) -> HashSet<String>
229:fn jaccard(left: &HashSet<String>, right: &HashSet<String>) -> f64
237:fn line_similarity(left: &str, right: &str) -> f64
263:fn score_candidate(target: &[String], pattern: &[String], start: usize) -> f64
275:fn rebase_indent(new_text: &str, old_text: &str, matched_text: &str) -> String
301:fn fuzzy_candidates(target: &[String], pattern: &[String]) -> Vec<usize>
323:struct Located
335:fn locate_edit(
501:pub struct SmartMatch
509:pub struct SmartResult
517:pub fn apply_smart_edits(

### src-tauri/pi_core/src/text.rs
13:pub fn utf8_byte_len(units: &[u16]) -> usize
47:pub fn utf16_len(text: &str) -> usize
51:fn units(text: &str) -> Vec<u16>
58:fn from_units(units: &[u16]) -> String
65:pub fn truncate_utf8_to_bytes(text: &str, max_bytes: usize) -> String
82:pub fn truncate_utf8_tail_to_bytes(text: &str, max_bytes: usize) -> String
101:pub fn head_tail_utf8(text: &str, max_bytes: usize, notice: &str) -> String
117:pub fn safe_archive_segment(value: Option<&str>) -> String
153:pub fn clamp_tool_output_text(text: Option<&str>, max_chars: usize) -> String
171:pub struct Governed
180:pub fn govern_text(text: &str, max_bytes: usize, archive_path: Option<&str>) -> Option<Governed>

### src-tauri/pi_core/src/tools.rs
26:fn text_result(text: &str) -> Value
30:fn error_result(message: &str) -> Value
35:pub struct NativeTools
41:impl NativeTools
42:    pub fn new(cwd: impl Into<PathBuf>) -> Self
49:    fn cwd_str(&self) -> String
55:    pub fn execute(&self, name: &str, args: &Value) -> (Value, bool)
71:    fn tool_read(&self, args: &Value) -> (Value, bool)
165:    fn tool_read_files(&self, args: &Value) -> (Value, bool)
198:    fn tool_edit(&self, args: &Value) -> (Value, bool)
239:    fn tool_edit_files(&self, args: &Value) -> (Value, bool)
318:    fn tool_write(&self, args: &Value) -> (Value, bool)
328:    fn tool_ls(&self, args: &Value) -> (Value, bool)
345:    fn tool_bash(&self, args: &Value) -> (Value, bool)
377:    fn tool_grep(&self, args: &Value) -> (Value, bool)
397:    fn tool_find(&self, args: &Value) -> (Value, bool)
410:fn parse_edits(args: &Value) -> Option<Vec<(String, String)>>
442:fn run_capture(mut cmd: Command) -> (Value, bool)
461:pub fn tool_fn_for(tools: &NativeTools) -> impl FnMut(&str, &Value) -> (Value, bool) + '_
469:    fn setup() -> (tempfile::TempDir, NativeTools)
476:    fn read_basic_and_offset()
490:    fn read_offset_beyond_end_errors()
498:    fn read_files_batch()
510:    fn edit_single_exact_and_fuzzy()
532:    fn edit_files_batch()
550:    fn write_creates_and_reports_utf16_length()
565:    fn ls_sorted_with_dir_suffix()
576:    fn unknown_tool_errors()

### src-tauri/pi_core/src/transform.rs
15:fn replace_images_with_placeholder(content: &[Value], placeholder: &str) -> Vec<Value>
33:fn downgrade_unsupported_images(messages: &[Value], model_supports_image: bool) -> Vec<Value>
65:fn is_same_model(msg: &Value, model_provider: &str, model_api: &str, model_id: &str) -> bool
73:pub fn transform_messages<F>(

### src-tauri/pi_core/src/truncate.rs
17:pub fn format_size(bytes: usize) -> String
29:fn to_fixed_1_div(bytes: usize, divisor: usize) -> String
41:fn split_lines_for_counting(content: &str) -> Vec<&str>
53:pub struct Truncation
76:fn no_truncation(content: &str, total_lines: usize, total_bytes: usize, max_lines: usize, max_bytes: usize) -> Truncation
95:pub fn truncate_head(content: &str, max_lines: Option<usize>, max_bytes: Option<usize>) -> Truncation
161:pub fn truncate_tail(content: &str, max_lines: Option<usize>, max_bytes: Option<usize>) -> Truncation
218:fn truncate_string_to_bytes_from_end(value: &str, max_bytes: usize) -> String
231:pub struct TruncateLineResult
239:pub fn truncate_line(line: &str, max_chars: Option<usize>) -> TruncateLineResult

### src-tauri/pi_core/src/write.rs
14:pub fn write_tool(cwd: &str, path: &str, content: &str) -> Result<String, String>

### src-tauri/pi_core/tests/parity.rs
26:fn golden() -> Value
31:fn coerce_str(value: &Value) -> String
41:fn opt_str(value: &Value) -> Option<&str>
46:fn parity_clamp_tool_output_text()
61:fn parity_clamp_tool_output_text_default_max()
72:fn parity_govern_tool_result()
103:fn parity_clamp_prompt_cache_key()
125:fn parity_inject_openai_prompt_cache_key()
147:fn parity_clamp_openai_payload_tool_outputs()
169:fn parity_merge_usage()
188:fn parse_normalize_options(value: &Value) -> NormalizeOptions
206:fn build_stream_turn(entry: &Value) -> StreamTurn
239:fn strip_timestamps(value: &Value) -> Value
257:fn parity_parse_jsonc()
266:fn parity_merge_config()
279:fn env_map(config: &Value) -> std::collections::HashMap<String, String>
292:fn parity_resolve_model()
313:fn parity_compat_defaults()
327:fn parity_edit_diff()
375:fn parity_bridge_tool_item()
400:fn parity_agent_class()
463:fn parity_agent_loop()
526:fn parity_skills_prompt()
549:fn parity_write_tool()
571:fn parity_ls_tool()
622:fn parity_resolve_to_cwd()
636:fn parity_normalize_path()
650:fn parity_format_size()
661:fn truncation_options(case: &Value) -> (Option<usize>, Option<usize>)
670:fn parity_truncate_head()
680:fn parity_truncate_tail()
690:fn parity_truncate_line()
700:fn parity_read_files()
742:fn parity_apply_smart_edits()
815:fn parity_build_system_prompt()
842:fn parity_decode_text_buffer()
859:fn parity_slim_memory()

### src-tauri/src/acp.rs
34:pub struct PendingPermission
42:struct Route
48:fn permission_request_key(permission_scope: &str, id: &Value) -> String
52:struct TitleJob
58:pub struct AcpConn
69:impl AcpConn
70:    fn send_raw(&self, msg: Value) -> Result<(), String>
76:    pub async fn request(
104:    pub fn notify(&self, method: &str, params: Value)
112:    pub fn respond_ok(&self, id: Value, result: Value)
116:    pub fn respond_err(&self, id: Value, code: i64, message: String)
124:    pub fn kill(&self)
147:pub(crate) fn kill_process_tree(pid: u32)
211:fn wait_pid_exit(pid: u32, timeout_ms: u32)
225:pub(crate) fn kill_process_tree(pid: u32)
232:    fn collect_children(parent: u32, out: &mut Vec<u32>)
268:pub(crate) fn assign_to_agent_job(child: &tokio::process::Child)
277:    struct JobHandle(isize);
309:pub(crate) fn assign_to_agent_job(_child: &tokio::process::Child)
311:pub struct AcpManager
349:impl AcpManager
350:    pub fn new(app: AppHandle, kind: AgentKind) -> Arc<Self>
354:    pub fn new_with_env(
387:    pub fn is_running(&self, thread_id: &str) -> bool
392:    fn mcp_servers_for_thread(&self, _thread_id: Option<&str>) -> Value
396:    fn conn_key_for_thread(&self, _thread_id: &str) -> String
400:    fn aux_key(&self) -> String
406:    fn slot(&self, key: &str) -> Arc<TokioMutex<Option<Arc<AcpConn>>>>
415:    fn slot_opt(&self, key: &str) -> Option<Arc<TokioMutex<Option<Arc<AcpConn>>>>>
419:    pub fn get_logs(&self) -> Vec<String>
423:    pub fn get_model_options(&self) -> Option<Value>
428:    pub fn seed_model_options(&self, v: Value)
432:    fn persist_model_options(&self, v: &Value)
437:    pub async fn refresh_model_options(self: &Arc<Self>) -> Result<Value, String>
448:    pub fn spawn_revalidate_model_options(self: &Arc<Self>)
467:    pub async fn ensure_model_options(self: &Arc<Self>) -> Result<Value, String>
492:    fn backend_mode_id(&self, mode: &str) -> String
501:    fn proxy_of<'a>(&self, settings: &'a Settings) -> &'a str
506:    fn known_mode_ids(&self) -> Option<Vec<String>>
522:    fn known_model_values(&self) -> Option<Vec<String>>
536:    pub fn get_commands(&self) -> Option<Value>
540:    pub async fn fetch_commands(self: &Arc<Self>) -> Result<Value, String>
571:    pub async fn fetch_model_options(self: &Arc<Self>) -> Result<Value, String>
579:    async fn fetch_model_options_from_agent(self: &Arc<Self>) -> Result<Value, String>
601:    pub async fn connected(&self) -> bool
605:    fn push_log(&self, line: String)
618:    pub async fn kill_conn(&self)
636:    fn clear_sessions_of_key(&self, conn_key: &str)
662:    pub async fn restart(self: &Arc<Self>)
684:    fn thread_lock(&self, thread_id: &str) -> Arc<TokioMutex<()>>
697:    async fn ensure_conn_for(
720:    async fn conn_for_key(&self, conn_key: &str) -> Option<Arc<AcpConn>>
729:    async fn spawn_conn(
881:    async fn on_conn_closed(&self, conn: &Arc<AcpConn>)
954:    fn broadcast_if_all_closed(&self)
963:    fn handle_line(self: &Arc<Self>, conn: &Arc<AcpConn>, line: &str)
1002:    fn handle_server_request(self: &Arc<Self>, conn: &Arc<AcpConn>, msg: &Value)
1132:    fn emit_update(&self, thread_id: &str, op: Value)
1158:    fn on_session_update(self: &Arc<Self>, params: &Value)
1335:    fn mark_plan_interrupted(&self, thread_id: &str, status: &str, include_pending: bool)
1370:    fn clear_plan(&self, thread_id: &str)
1394:    fn emit_proposed_plan(&self, thread_id: &str, text: Option<String>)
1398:    fn set_running(&self, thread_id: &str, running: bool, stop_reason: Option<String>)
1422:    fn finish_turn(&self, thread_id: &str, stop_reason: String, usage: Option<Value>)
1447:    fn maybe_emit_plan_action(&self, thread_id: &str, stop_reason: &str)
1480:    fn notify_done(&self, thread_id: &str, stop_reason: &str)
1500:    fn capture_options(self: &Arc<Self>, result: &Value)
1504:        fn models_to_config_options(models: Option<&Value>) -> Value
1539:        fn modes_from_config_options(config_options: &Value) -> Value
1599:    fn capture_commands(&self, update: &Value)
1613:    pub async fn prewarm(self: &Arc<Self>, cwd: String)
1660:    async fn take_prewarmed_session(&self, cwd: &str) -> Option<String>
1674:    async fn ensure_session(self: &Arc<Self>, thread_id: &str) -> Result<String, String>
1796:    fn mark_model_applied_with_warn(&self, sid: &str, model: &str, warn: String)
1815:    async fn apply_session_config(
1925:    pub fn generate_title_async(
1947:    async fn generate_title(
2009:    fn capture_title_update(&self, session_id: &str, update: &Value) -> bool
2021:    fn complete_title_job(&self, session_id: &str)
2054:    pub async fn sync_thread_config(self: &Arc<Self>, thread_id: &str)
2077:    async fn new_session_for(
2163:    pub async fn run_prompt(
2280:    fn build_prompt_blocks(text: &str, images: &[PromptImage]) -> Vec<Value>
2343:    fn build_user_prompt_blocks(
2360:    pub async fn steer_prompt(
2452:    async fn drive_prompt(
2632:    fn clear_thread_session_for_respawn(&self, thread_id: &str)
2644:    async fn force_finish(&self, thread_id: &str, msg: &str)
2659:    pub async fn cancel(self: &Arc<Self>, thread_id: &str)
2717:    pub async fn respond_permission(
2742:    pub fn has_pending_permission(&self, request_key: &str) -> bool
2749:    pub fn forget_session_of_thread(self: &Arc<Self>, thread_id: &str)
2762:pub(crate) fn resolve_program_on_path(name: &str) -> Option<std::path::PathBuf>
2791:pub(crate) fn resolve_program_on_path(name: &str) -> Option<std::path::PathBuf>
2811:fn build_acp_command(program: &str, args_str: &str) -> tokio::process::Command
2839:pub(crate) fn apply_proxy_env(cmd: &mut tokio::process::Command, proxy: &str)
2866:fn is_retriable_rpc_error(err: &str) -> bool
2887:fn is_process_exit_error(err: &str) -> bool
2891:fn prompt_conn_needs_rebuild(conn_alive: Option<bool>, last_err: &str) -> bool
2896:fn retriable_backoff_ms(attempt: u32) -> u64
2901:fn is_full_permission_mode(mode: &str) -> bool
2907:fn unify_mode_id(mode: &str) -> String
2918:fn pick_fallback_mode_id(unified: &str, known: &[String]) -> Option<String>
2933:fn devin_runtime_guidance() -> Option<&'static str>
2940:fn devin_runtime_guidance() -> Option<&'static str>
2945:fn save_prompt_image(img: &PromptImage) -> Option<String>
2965:fn prompt_image_data(img: &PromptImage) -> Option<String>
2976:fn attachment_size(img: &PromptImage) -> Option<u64>
2986:fn file_uri_to_path(uri: &str) -> Option<String>
2999:fn percent_decode(s: &str) -> String
3019:fn derive_title(text: &str, has_images: bool) -> String
3033:fn extract_text(content: &Value) -> String
3041:fn tool_call_from_update(tc_id: &str, update: &Value) -> ToolCall
3063:fn merge_tool_call(call: &mut ToolCall, update: &Value)
3087:fn compact_tool_values(values: &[Value]) -> Vec<Value>
3091:fn compact_tool_value(value: &Value) -> Value
3106:fn limit_display_text(text: &str) -> String
3121:fn normalize_generated_title(raw: &str, fallback: &str) -> String
3141:fn complete_pending_tools(thread: &mut Thread, except_tool_call_id: Option<&str>) -> Vec<Item>

### src-tauri/src/agent_config.rs
18:pub struct AgentInstructionTarget
29:pub struct GlobalAgentInstructions
37:struct AdapterPreferences
42:enum TargetFormat
47:struct Target
54:pub fn get_global_instructions(config_dir: &Path) -> GlobalAgentInstructions
71:pub fn set_global_instructions(
129:pub fn sync_global_instructions(config_dir: &Path) -> Result<(), String>
143:pub fn sync_backend_with_env(
160:fn central_path(config_dir: &Path) -> PathBuf
164:fn read_adapter_preferences(config_dir: &Path) -> AdapterPreferences
171:fn write_adapter_preferences(
181:fn normal_targets(config_dir: &Path) -> Result<Vec<Target>, String>
201:fn target_for(kind: &AgentKind, overrides: &HashMap<String, String>) -> Result<Target, String>
279:fn configured_dir(overrides: &HashMap<String, String>, name: &str) -> Option<PathBuf>
291:fn user_home_dir() -> Option<PathBuf>
297:fn sync_target(target: &Target, content: &str, enabled: bool) -> AgentInstructionTarget
308:fn inspect_target(target: &Target, active: bool, enabled: bool) -> AgentInstructionTarget
352:fn target_status(
368:fn sync_markdown(path: &Path, content: &str) -> Result<(&'static str, String), String>
401:fn sync_cursor_rule(path: &Path, content: &str) -> Result<(&'static str, String), String>
428:fn managed_block(content: &str) -> String
432:fn upsert_managed_block(existing: &str, content: &str) -> String
441:fn remove_managed_block(existing: &str) -> (String, bool)
459:fn cursor_rule_content(content: &str) -> String
466:fn ensure_parent(path: &Path) -> Result<(), String>
473:fn is_symlink(path: &Path) -> bool
484:    fn managed_block_preserves_existing_content_and_updates_in_place()
495:    fn removing_managed_block_keeps_user_content()
503:    fn disabling_markdown_adapter_keeps_target_file()
516:    fn cursor_adapter_is_always_apply_and_disabling_keeps_target_file()

### src-tauri/src/cli.rs
23:fn is_known(cmd: &str) -> bool
41:pub fn maybe_run() -> bool
60:fn parse_flags(args: &[String]) -> (HashMap<String, String>, Vec<String>)
85:fn cmd_search(flags: &HashMap<String, String>, positional: &[String]) -> String
101:fn cmd_ledger_list(flags: &HashMap<String, String>) -> String
112:fn run_search(data_dir: &str, employee: &str, query: &str, k: usize) -> String
163:fn cmd_write(kind: &str, flags: &HashMap<String, String>) -> String
277:fn semantic_search<'a>(
341:fn one_line(s: &str, max: usize) -> String

### src-tauri/src/cli_manager.rs
19:pub struct CliStatus
28:struct CliSpec
41:fn powershell_script_installer(url: &str, elevated: bool) -> (String, Vec<String>)
72:fn script_installer(url: &str) -> (String, Vec<String>)
77:fn script_installer(url: &str) -> (String, Vec<String>)
84:fn npm_installer(package: &str) -> (String, Vec<String>)
91:fn configured_cli_program(configured: &str, expected_names: &[&str], fallback: &str) -> String
104:fn spec_for(kind: &AgentKind, settings: &Settings) -> CliSpec
219:fn all_specs(settings: &Settings) -> Vec<CliSpec>
236:fn build_command(program: &str, args: &[String]) -> tokio::process::Command
267:fn build_command(program: &str, args: &[String]) -> tokio::process::Command
273:async fn run_command(
301:fn emit_operation_progress(
323:async fn read_progress_stream<R>(
363:async fn terminate_child(child: &mut tokio::process::Child, pid: Option<u32>)
371:async fn run_command_with_progress(
467:fn command_output(stdout: &[u8], stderr: &[u8]) -> String
478:fn strip_ansi(input: &str) -> String
496:async fn status_for(spec: CliSpec) -> CliStatus
552:pub async fn statuses(settings: &Settings) -> Vec<CliStatus>
566:async fn stop_backend(state: &AppState, kind: &AgentKind)
582:fn devin_process_running() -> bool
620:fn devin_process_running() -> bool
629:pub async fn upgrade(
749:pub fn cancel(state: &AppState, operation_id: &str) -> bool
763:pub fn is_installed(kind: &AgentKind, settings: &Settings) -> bool
772:pub async fn ensure_installed(
792:fn refresh_cli_search_path()
839:    fn cursor_is_sdk_only_and_skips_cli_install()
848:    fn windows_script_installers_open_visible_powershell()

### src-tauri/src/clues.rs
11:pub(crate) fn deserialize_vec_or_default<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
21:pub struct ClueMention
28:pub struct ClueAttachment
41:pub struct ClueCardVersion
58:pub struct ClueComment
73:pub struct ClueCard
84:impl ClueCard
85:    pub fn current_version(&self) -> Option<&ClueCardVersion>
96:pub struct ClueNodeGroup
108:pub struct ClueContextCard
119:pub struct ClueContextSnapshot
129:pub struct CaptureClueResult
136:struct ClueFile
141:pub struct ClueStore
146:impl ClueStore
147:    pub fn load(dir: &PathBuf) -> Self
157:    pub fn list(&self) -> Vec<ClueNodeGroup>
163:    pub fn replace(&mut self, groups: Vec<ClueNodeGroup>) -> Result<(), String>
168:    pub fn save(&self) -> Result<(), String>
184:    pub fn capture(
204:    pub fn capture_with_mentions(
226:    pub fn capture_with_attachments(
330:    pub fn add_comment(
389:    pub fn associate(
440:    pub fn disassociate(
464:    pub fn split_card(&mut self, card_id: &str) -> Result<ClueNodeGroup, String>
488:    pub fn stack_cards(&mut self, card_ids: &[String]) -> Result<ClueNodeGroup, String>
587:    pub fn delete(&mut self, card_id: &str) -> Result<(), String>
607:    pub fn snapshot(&self, root_card_id: &str) -> Result<ClueContextSnapshot, String>
642:    fn visit_card(
682:    fn card_location(&self, card_id: &str) -> Option<(usize, usize)>
695:    fn is_reachable(&self, from_card_id: &str, target_card_id: &str) -> bool
719:fn new_version(
740:fn new_card(
768:fn normalize_mentions(
796:    fn store() -> ClueStore
804:    fn update_parallel_and_new_have_distinct_shapes()
857:    fn snapshot_orders_parent_before_child()
879:    fn association_splits_only_the_selected_parallel_card()
904:    fn association_rejects_cycles()
924:    fn disassociation_removes_shared_group_connection()
944:    fn split_and_stack_only_move_selected_cards()
990:    fn stacking_rejects_different_parent_relationships()
1012:    fn stacking_parents_collapses_duplicate_child_edges()
1055:    fn stacking_siblings_keeps_single_parent()
1090:    fn null_arrays_from_older_relay_are_treated_as_empty()
1131:    fn mentions_belong_to_the_published_version_and_are_deduplicated()
1164:    fn comments_and_replies_persist_and_reply_mentions_the_parent_author()
1263:    fn delete_keeps_downstream_and_removes_parent_reference()

### src-tauri/src/codex.rs
27:fn path_has_separator(path: &str) -> bool
32:fn find_on_path(name: &str) -> Option<PathBuf>
52:fn codex_npm_shim_script(path: &Path) -> Option<PathBuf>
68:fn codex_npm_package_root(path: &Path) -> Option<PathBuf>
82:fn codex_native_binary_from_npm_root(root: &Path) -> Option<PathBuf>
111:fn resolve_codex_native_binary(codex_path: &str) -> Option<PathBuf>
128:fn resolve_codex_npm_shim(codex_path: &str) -> Option<(PathBuf, PathBuf)>
144:struct PendingCodexPermission
149:struct CodexTurnOutcome
156:pub struct CodexConn
164:impl CodexConn
165:    fn send_raw(&self, msg: Value) -> Result<(), String>
171:    pub async fn request(
199:    pub fn respond_ok(&self, id: Value, result: Value)
203:    pub fn respond_err(&self, id: Value, code: i64, message: String)
211:    pub fn kill(&self)
232:pub struct CodexManager
266:impl CodexManager
267:    pub fn new(app: AppHandle) -> Arc<Self>
271:    pub fn new_with_env(
319:    fn touch_conn(&self)
325:    async fn reap_if_idle(self: &Arc<Self>)
360:    pub fn is_running(&self, thread_id: &str) -> bool
366:    pub fn remount_for_config(&self, thread_id: &str)
373:    pub fn get_model_options(&self) -> Option<Value>
377:    pub fn seed_model_options(&self, v: Value)
381:    fn persist_model_options(&self, v: &Value)
385:    pub async fn refresh_model_options(self: &Arc<Self>) -> Result<Value, String>
396:    pub fn spawn_revalidate_model_options(self: &Arc<Self>)
416:    pub async fn ensure_model_options(self: &Arc<Self>) -> Result<Value, String>
437:    pub fn get_logs(&self) -> Vec<String>
441:    pub async fn connected(&self) -> bool
450:    fn push_log(&self, line: String)
465:    pub async fn kill_conn(&self)
488:    pub async fn restart(self: &Arc<Self>)
505:    fn thread_lock(&self, thread_id: &str) -> Arc<TokioMutex<()>>
514:    pub async fn ensure_conn(self: &Arc<Self>) -> Result<Arc<CodexConn>, String>
534:    async fn current_conn(&self) -> Option<Arc<CodexConn>>
542:    async fn spawn_conn(self: &Arc<Self>, settings: &Settings) -> Result<Arc<CodexConn>, String>
698:    fn on_conn_closed(&self, conn: &Arc<CodexConn>)
735:    fn handle_line(self: &Arc<Self>, conn: &Arc<CodexConn>, line: &str)
761:    fn handle_server_request(self: &Arc<Self>, conn: &Arc<CodexConn>, msg: &Value)
775:    fn emit_permission_request(
860:    fn handle_notification(&self, msg: &Value)
886:    fn local_thread_id(&self, remote_thread_id: &str) -> Option<String>
890:    fn on_turn_started(&self, params: &Value)
903:    fn on_turn_completed(&self, params: &Value)
950:    fn on_error(&self, params: &Value)
963:    fn on_token_usage(&self, params: &Value)
975:    fn on_item(&self, params: &Value, completed: bool)
983:    fn on_text_delta(&self, params: &Value, thought: bool)
1005:    fn on_tool_output_delta(&self, params: &Value)
1018:    fn on_patch_updated(&self, params: &Value)
1033:    fn on_plan_updated(&self, params: &Value)
1062:    fn emit_update(&self, thread_id: &str, op: Value)
1083:    fn append_text_item(&self, thread_id: &str, remote_item_id: &str, text: String)
1125:    fn append_tool_output(&self, thread_id: &str, remote_item_id: &str, delta: &str)
1157:    fn upsert_codex_item(
1213:    fn local_text_item(&self, thread_id: &str, remote_item_id: &str) -> Option<String>
1229:    fn set_text_item(&self, thread_id: &str, remote_item_id: &str, text: String)
1268:    fn upsert_tool_item(
1307:    fn tool_item_from_codex(&self, thread_id: &str, item: &Value) -> Item
1447:    async fn default_model_id(self: &Arc<Self>) -> Option<String>
1473:    pub async fn fetch_model_options(self: &Arc<Self>) -> Result<Value, String>
1480:    async fn fetch_model_options_from_agent(self: &Arc<Self>) -> Result<Value, String>
1583:    pub async fn prewarm(
1613:    async fn take_prewarmed_thread(&self, key: &str) -> Option<String>
1626:    async fn ensure_thread(self: &Arc<Self>, thread_id: &str) -> Result<String, String>
1718:    async fn start_thread(
1734:    async fn start_remote_thread(
1757:    pub async fn run_prompt(
1833:    async fn drive_prompt(
1919:    pub async fn steer_prompt(
1989:    pub async fn cancel(self: &Arc<Self>, thread_id: &str)
2021:    async fn force_finish(&self, thread_id: &str, msg: &str)
2036:    pub async fn respond_permission(
2068:    pub fn has_pending_permission(&self, request_key: &str) -> bool
2075:    fn set_running(&self, thread_id: &str, running: bool, stop_reason: Option<String>)
2100:    fn finish_turn(&self, thread_id: &str, stop_reason: String, usage: Option<Value>)
2128:    fn maybe_emit_plan_action(&self, thread_id: &str, stop_reason: &str)
2160:    fn mark_plan_interrupted(&self, thread_id: &str, status: &str, include_pending: bool)
2192:    fn clear_plan(&self, thread_id: &str)
2216:    fn emit_proposed_plan(&self, thread_id: &str, text: Option<String>)
2220:    fn notify_done(&self, thread_id: &str, stop_reason: &str)
2242:    pub async fn compact(self: &Arc<Self>, thread_id: String)
2290:    fn finish_manual_compaction(&self, thread_id: &str)
2300:    fn set_compaction_item(&self, thread_id: &str, remote_item_id: &str, done: bool)
2353:    pub fn forget_session_of_thread(&self, thread_id: &str)
2361:fn apply_windows_sandbox_fallback(args: &mut Vec<String>)
2380:fn codex_thread_params(
2406:fn split_model_effort(model: Option<&str>) -> (Option<String>, Option<String>)
2418:fn codex_warm_key(cwd: &str, model: Option<&str>, mode: Option<&str>) -> String
2429:fn effort_label(effort: &str) -> String
2444:fn codex_policy(mode: Option<&str>) -> (&'static str, &'static str)
2453:fn codex_modes() -> Vec<Value>
2460:fn build_user_input(text: &str, images: &[PromptImage]) -> Vec<Value>
2498:fn prompt_image_path(img: &PromptImage) -> Option<String>
2507:fn save_prompt_image(img: &PromptImage) -> Option<String>
2527:fn file_uri_to_path(uri: &str) -> Option<String>
2540:fn percent_decode(s: &str) -> String
2560:fn text_delta(params: &Value) -> String
2579:fn format_codex_error(err: &Value) -> Option<String>
2591:fn codex_result_content(result: Option<&Value>, error: Option<&Value>) -> Vec<Value>
2610:fn limit_display_text(text: &str) -> String
2642:fn strip_omission_notice(text: &str) -> &str
2653:    fn omission_payload(text: &str) -> &str
2658:    fn limits_tool_output_to_last_500_lines()
2673:    fn limits_tool_output_to_last_10000_unicode_characters()
2684:    fn applies_the_smaller_of_the_line_and_character_limits()
2698:fn normalize_tool_status(status: &str) -> String
2708:fn normalize_plan_status(status: &str) -> &'static str
2717:fn complete_pending_tools(thread: &mut Thread, except_tool_call_id: Option<&str>) -> Vec<Item>
2734:fn derive_title(text: &str, has_images: bool) -> String

### src-tauri/src/codex_radar.rs
13:struct RadarModel
22:pub struct ResolvedAutoModel
27:pub fn is_auto_model(model: &str) -> bool
34:pub fn selection_label(model: &str) -> &'static str
42:pub async fn resolve_auto_model(
91:async fn fetch_text(client: &reqwest::Client, url: &str) -> Result<String, String>
105:fn radar_models(summary: &Value) -> Result<Vec<RadarModel>, String>
130:fn radar_model(latest: &Value, key: Option<&str>) -> Option<RadarModel>
144:fn latest_iq_winner(entries: &[RadarModel]) -> Result<RadarModel, String>
158:fn latest_value_winner(entries: &[RadarModel], html: &str) -> Result<RadarModel, String>
198:fn community_rating_winner(ratings: &Value) -> Result<RadarModel, String>
245:fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str>
251:fn match_available_model(options: &Value, winner: &RadarModel, open_code: bool) -> Option<String>
269:fn model_options(options: &Value) -> impl Iterator<Item = &Value>
280:fn has_gpt_model(options: &Value) -> bool
289:fn matches_codex(option: &Value, value: &str, winner: &RadarModel) -> bool
300:fn matches_opencode(value: &str, winner: &RadarModel) -> bool
326:fn same_model(available: &str, radar: &str) -> bool
332:fn normalize_key(value: &str) -> String
349:fn compact_key(value: &str) -> String
363:    fn picks_latest_iq_and_value_winners()
390:    fn picks_highest_community_rating_and_excludes_ultra()
408:    fn matches_codex_and_opencode_effort_variants()
429:    fn does_not_map_radar_max_to_opencode_xhigh_variant()
453:    fn matches_opencode_flat_model_ids_with_embedded_effort()

### src-tauri/src/credential_roaming.rs
31:pub struct CredentialFile
38:pub struct CredentialBundle
48:pub struct EncryptedGrant
55:pub enum BorrowedManager
62:pub struct BorrowedRuntime
67:impl BorrowedRuntime
68:    pub fn is_running(&self, thread_id: &str) -> bool
76:    pub fn has_pending_permission(&self, request_key: &str) -> bool
84:    pub async fn respond_permission(
102:    pub async fn shutdown(self)
112:pub fn new_request_key() -> (StaticSecret, String)
121:pub fn encrypt_bundle(
150:pub fn decrypt_bundle(
187:fn decode_public_key(encoded: &str) -> Result<PublicKey, String>
197:fn derive_key(
210:pub fn collect_credentials(
307:pub fn materialize_runtime(
398:fn launch_env(kind: &AgentKind, root: &Path) -> Result<HashMap<String, String>, String>
472:fn stage_local_skills(
507:pub fn isolate_borrowed_command(command: &mut Command)
531:fn credential_path_allowed(kind: &AgentKind, raw: &str) -> bool
546:fn credential_env_allowed(kind: &AgentKind, name: &str) -> bool
557:fn home_dir() -> PathBuf
564:fn configured_home(env: &str, fallback: &str) -> PathBuf
570:fn devin_credentials_path() -> Result<PathBuf, String>
587:fn collect_file(path: &Path, target: &str, files: &mut Vec<CredentialFile>) -> Result<(), String>
604:fn collect_optional_file(
615:fn collect_json_entry(
643:fn collect_directory(
688:fn collect_secret_env(name: &str, configured: &str, env: &mut HashMap<String, String>)
699:fn safe_relative_path(raw: &str) -> Result<PathBuf, String>
712:fn restrict_dir(path: &Path)
718:fn restrict_dir(_path: &Path)
721:fn restrict_file(path: &Path)
727:fn restrict_file(_path: &Path)
734:    fn encrypted_bundle_round_trip()
756:    fn rejects_credential_path_escape()
779:    fn opencode_credentials_only_include_requested_provider()
806:    fn cursor_launch_env_uses_isolated_sdk_data_dir()
817:    fn devin_launch_env_uses_isolated_user_profile()

### src-tauri/src/employees.rs
55:fn task_memory_usage() -> &'static std::sync::Mutex<HashMap<String, HashSet<i64>>>
61:fn task_memory_usage_key(employee_id: &str, task_id: &str) -> String
65:fn remember_task_memory_usage(employee_id: &str, task_id: Option<&str>, used_ts: &[i64])
78:fn take_task_memory_usage(employee_id: &str, task_id: &str) -> Vec<i64>
88:fn apply_task_memory_evidence(
107:fn default_heartbeat() -> u64
111:fn default_true() -> bool
115:fn default_origin() -> String
122:pub struct Employee
196:pub struct WorkHours
207:fn parse_hm(s: &str) -> Option<u32>
219:fn within_work_hours(emp: &Employee) -> bool
248:pub struct Partner
262:pub struct Decision
308:pub struct Task
329:pub struct Workflow
353:pub struct JournalEntry
414:pub struct InboxCommand
465:pub(crate) fn commands_path(data_dir: &std::path::Path) -> PathBuf
470:pub(crate) fn append_command(data_dir: &str, cmd: &InboxCommand) -> Result<(), String>
489:struct EmployeesFile
493:pub struct EmployeeStore
498:impl EmployeeStore
499:    pub fn load(dir: &PathBuf) -> Self
509:    pub fn save(&self)
521:    pub fn get(&self, id: &str) -> Option<&Employee>
525:    pub fn get_mut(&mut self, id: &str) -> Option<&mut Employee>
531:struct TasksFile
535:pub struct TaskStore
540:impl TaskStore
541:    pub fn load(dir: &PathBuf) -> Self
557:    pub fn save(&self)
569:    pub fn get_mut(&mut self, id: &str) -> Option<&mut Task>
575:struct WorkflowsFile
579:pub struct WorkflowStore
584:impl WorkflowStore
585:    pub fn load(dir: &PathBuf) -> Self
595:    pub fn save(&self)
607:    pub fn get_mut_by_task(
618:    pub fn active_for_employee(&self, employee_id: &str, limit: usize) -> Vec<Workflow>
630:    pub fn close_by_thread_id(&mut self, thread_id: &str) -> bool
645:    pub fn close_by_worktree(&mut self, repo: &str, path: &str, branch: &str) -> bool
660:    pub fn detach_threads(&mut self, thread_ids: &[String]) -> bool
680:struct MemoryFile
684:pub struct MemoryStore
689:impl MemoryStore
690:    pub fn load(dir: &PathBuf) -> Self
704:    pub fn save(&self)
716:    pub fn append(&mut self, employee_id: &str, entry: JournalEntry)
760:    pub fn append_unique_managed(&mut self, employee_id: &str, entry: JournalEntry) -> bool
781:    fn prune_duplicate_managed_knowledge(&mut self) -> bool
822:    pub fn all(&self, employee_id: &str) -> Vec<JournalEntry>
826:    pub fn mark_used(&mut self, employee_id: &str, ts_list: &[i64])
846:    pub fn apply_usage_evidence(
892:    pub fn update_entry(&mut self, employee_id: &str, ts: i64, summary: &str)
901:    pub fn delete_entry(&mut self, employee_id: &str, ts: i64)
908:    pub fn forget_managed(&mut self, employee_id: &str, ts: i64) -> bool
935:    pub fn challenge_lesson(
967:    pub fn set_pinned(&mut self, employee_id: &str, ts: i64, pinned: bool)
978:    pub fn set_feedback(&mut self, employee_id: &str, ts: i64, feedback: i8) -> bool
1024:    pub fn verify_lesson(
1054:    pub fn mark_superseded(&mut self, employee_id: &str, ts_list: &[i64], superseded_by: i64)
1070:    pub fn downgrade_memory(
1120:fn normalized_memory_key(text: &str) -> String
1128:fn is_memory_key_separator(c: char) -> bool
1159:fn memory_dedup_class(entry: &JournalEntry) -> &'static str
1175:fn memory_keys_similar(a: &str, b: &str) -> bool
1192:fn char_bigrams(chars: &[char]) -> HashSet<String>
1200:struct DecisionsFile
1205:pub struct DecisionStore
1210:impl DecisionStore
1211:    pub fn load(dir: &PathBuf) -> Self
1221:    pub fn save(&self)
1233:    pub fn list(&self) -> Vec<Decision>
1246:    fn prune(&mut self)
1265:    pub fn add(&mut self, d: Decision)
1271:    pub fn add_or_merge_pending(&mut self, d: Decision) -> bool
1303:    pub fn pending_for(&self, employee_id: &str, scope: &str, key: &str) -> bool
1314:    pub fn pending_on(&self, scope: &str, key: &str) -> bool
1321:    pub fn actionable_for_employee(&self, employee_id: &str) -> Vec<Decision>
1337:    pub fn take_actionable(&mut self, employee_id: &str, scope: &str, key: &str) -> Vec<Decision>
1358:    pub fn withdraw_for(&mut self, scope: &str, key: &str) -> usize
1376:    pub fn resolve(&mut self, id: &str, answer: &str) -> bool
1393:    pub fn mark_read(&mut self, id: &str) -> bool
1410:    pub fn review_report(&mut self, id: &str, answer: &str) -> bool
1428:    pub fn shelve(&mut self, id: &str) -> bool
1445:    pub fn reject(&mut self, id: &str, answer: &str) -> bool
1461:    pub fn remove(&mut self, id: &str)
1469:pub fn list_employees(app: &AppHandle) -> Vec<Employee>
1477:pub fn create_employee(
1543:pub fn update_employee(app: &AppHandle, mut emp: Employee) -> Result<(), String>
1575:pub fn delete_employee(app: &AppHandle, id: &str)
1608:pub fn set_employee_enabled(app: &AppHandle, id: &str, enabled: bool)
1621:pub fn list_tasks(app: &AppHandle) -> Vec<Task>
1628:pub fn assign_task(
1658:fn thread_running(app: &AppHandle, thread_id: &str) -> bool
1667:pub fn delete_task(app: &AppHandle, id: &str) -> Result<(), String>
1693:pub fn delete_tasks_for_threads(app: &AppHandle, thread_ids: &[String]) -> usize
1718:pub fn employee_memory(app: &AppHandle, id: &str) -> Vec<JournalEntry>
1725:pub fn add_memory(
1780:pub fn update_memory_entry(app: &AppHandle, id: &str, ts: i64, summary: String)
1790:pub fn delete_memory_entry(app: &AppHandle, id: &str, ts: i64)
1796:pub fn set_memory_pinned(app: &AppHandle, id: &str, ts: i64, pinned: bool)
1807:pub fn set_memory_feedback(app: &AppHandle, id: &str, ts: i64, feedback: i8) -> Result<(), String>
1821:pub fn run_now(app: &AppHandle, id: &str) -> Result<(), String>
1857:pub fn heartbeat_tick(app: &AppHandle)
1910:pub async fn process_command_inbox(app: &AppHandle)
1941:async fn exec_inbox_command(app: &AppHandle, cmd: InboxCommand) -> Result<(), String>
2005:fn memo_title(cmd: &InboxCommand) -> String
2018:pub fn summon_employee(app: &AppHandle, emp_id: &str)
2029:pub fn notify_decision_toast(app: &AppHandle, emp_name: &str, question: &str)
2033:pub fn notice_append_event(
2045:pub async fn notice_claim_mark(
2078:pub async fn notice_fail_mark(
2110:pub async fn notice_release_mark(
2147:pub fn find_employee(app: &AppHandle, id_or_name: &str) -> Option<Employee>
2162:fn relay_configured(app: &AppHandle) -> bool
2167:fn cmd_scope_or(emp: &Employee, cmd: &InboxCommand) -> String
2178:async fn ledger_assign(
2205:async fn exec_relay(app: &AppHandle, actor: &Employee, cmd: InboxCommand) -> Result<(), String>
2365:fn list_employee_names(app: &AppHandle) -> String
2375:fn list_relay_peer_names(app: &AppHandle, include_disabled: bool) -> Vec<String>
2393:fn resolve_relay_target(app: &AppHandle, actor: &Employee, to: &str) -> Result<Employee, String>
2419:async fn exec_relay_decision(app: &AppHandle, actor: &Employee, cmd: &InboxCommand)
2491:fn notify_decision(app: &AppHandle, emp_name: &str, question: &str)
2496:fn normalize_decision_category(raw: &str) -> String
2507:fn stable_error_kind(text: &str) -> &'static str
2525:fn decision_signature(scope: &str, key: &str, category: &str, text: &str) -> String
2535:fn workflow_decision_options(question: &str) -> Vec<String>
2555:fn proposed_action_for_workflow_error(question: &str) -> String
2566:async fn exec_done(app: &AppHandle, actor: &Employee, cmd: &InboxCommand)
2611:async fn exec_blocked(app: &AppHandle, actor: &Employee, cmd: &InboxCommand)
2662:fn forget_memory(app: &AppHandle, employee_id: &str, ts: i64)
2674:fn dev_rounds() -> &'static std::sync::Mutex<HashMap<String, u32>>
2680:fn rounds_key(scope: &str, key: &str) -> String
2687:fn blocked_cooloff() -> &'static std::sync::Mutex<HashSet<String>>
2692:fn cooloff_key(scope: &str, key: &str, emp_id: &str) -> String
2696:fn cooloff_add(scope: &str, key: &str, emp_id: &str)
2703:fn cooloff_hit(scope: &str, key: &str, emp_id: &str) -> bool
2711:pub fn cooloff_clear(scope: &str, key: &str)
2719:fn dev_thread_get(scope: &str, key: &str) -> Option<String>
2726:fn bump_dev_rounds(scope: &str, key: &str) -> u32
2734:fn reset_dev_rounds(scope: &str, key: &str)
2740:fn reset_dev_round_counter(scope: &str, key: &str)
2744:fn repo_locks() -> &'static std::sync::Mutex<HashSet<String>>
2749:struct RepoLockGuard
2753:impl Drop for RepoLockGuard
2754:    fn drop(&mut self)
2761:fn try_lock_repo(repo: &str) -> Option<RepoLockGuard>
2774:struct WorkspaceReady
2781:struct WorkPreflight
2811:struct WakeDecision
2852:impl WakeDecision
2853:    fn intent_key(&self) -> &str
2862:    fn workspace_plan(&self) -> WorkPreflight
2882:enum WakeRun
2904:async fn run_prompt_for(kind: &AgentKind, app: &AppHandle, thread_id: String, prompt: String)
2908:async fn run_prompt_for_images(
2968:pub(crate) async fn run_employee_prompt(
2978:fn emp_scope(emp: &Employee) -> String
2986:pub(crate) fn employee_scope(emp: &Employee) -> String
2990:pub(crate) fn mind_agent_kind(emp: &Employee) -> AgentKind
2997:pub(crate) fn new_mind_thread(app: &AppHandle, emp: &Employee, title: &str) -> String
3001:fn new_mind_thread_with_parent(
3029:fn set_thread_parent(app: &AppHandle, thread_id: &str, parent_thread_id: Option<&str>)
3046:fn thread_chain_root(app: &AppHandle, thread_id: &str) -> String
3069:fn resolve_chain_parent(
3111:fn chain_anchor_thread(app: &AppHandle, origin_thread_id: &str) -> Option<String>
3119:pub(crate) fn mark_mind_completed(app: &AppHandle, employee_id: &str)
3129:pub(crate) fn mind_employee_idle(app: &AppHandle, emp: &Employee) -> bool
3170:pub(crate) async fn cancel_employee_thread(app: &AppHandle, thread_id: &str)
3200:pub(crate) async fn cancel_deleted_ledger_thread(app: &AppHandle, thread_id: &str)
3220:fn task_title_of(key: &str, title: &str) -> String
3228:fn attachment_text(images: &[PromptImage]) -> String
3247:fn shared_note_with_attachments(note: &str, images: &[PromptImage]) -> String
3264:fn restore_mark_attachments(mut mark: Mark) -> Mark
3288:async fn ledger_marks(app: &AppHandle, use_shared: bool, scope: &str) -> Vec<Mark>
3302:async fn ledger_digest(app: &AppHandle, use_shared: bool, scope: &str) -> Result<String, String>
3314:async fn ledger_claim_one(
3338:async fn ledger_set_status(
3359:async fn delete_work_mark(app: &AppHandle, use_shared: bool, scope: &str, key: &str)
3376:async fn reject_work_mark(
3399:async fn ledger_set_thread(
3415:fn thread_cancelled(app: &AppHandle, thread_id: &str) -> bool
3425:pub(crate) fn employee_thread_cancelled(app: &AppHandle, thread_id: &str) -> bool
3429:fn clear_thread_cancelled(app: &AppHandle, thread_id: &str)
3443:pub(crate) fn clear_employee_thread_cancelled(app: &AppHandle, thread_id: &str)
3449:fn employee_has_running_thread(app: &AppHandle, employee_id: &str) -> bool
3474:async fn abort_on_stop(
3519:pub async fn delete_work_by_thread(app: &AppHandle, thread_id: &str)
3645:fn runtime_cwd(app: &AppHandle, preferred: Option<&str>) -> Option<String>
3666:fn employee_at_cwd(emp: &Employee, cwd: Option<&str>) -> Employee
3674:fn employee_for_mark(app: &AppHandle, emp: &Employee, scope: &str, mark: &Mark) -> Employee
3695:fn run_cycle(app: AppHandle, mut emp: Employee, manual: bool)
3905:pub(crate) fn finalize_wake_thread_decision(app: &AppHandle, dec: &Decision)
3975:async fn run_notice_injections(
4056:async fn run_pickup_open(app: &AppHandle, emp: &Employee, scope: &str, use_shared: bool) -> bool
4132:fn find_ascii_case_insensitive(haystack: &str, needle: &str) -> Option<usize>
4148:fn parse_scout_actions(text: &str, scope: &str, actor_id: &str) -> Vec<InboxCommand>
4228:fn is_idle_scout(text: &str) -> bool
4239:async fn develop_and_conclude(
4717:fn file_report(
4738:fn withdraw_decisions(app: &AppHandle, scope: &str, key: &str)
4743:async fn ledger_status(app: &AppHandle, use_shared: bool, scope: &str, key: &str) -> String
4753:fn has_done_marker(text: &str) -> bool
4770:async fn exec_discuss(
4859:async fn run_partner_reply(
4883:fn resolve_peer_token(app: &AppHandle, peer_name: &str) -> Option<String>
4903:pub fn on_remote_discuss(
4981:pub fn on_remote_discuss_reply(app: &AppHandle, data: serde_json::Value)
5024:fn marks_from_values(vals: Vec<serde_json::Value>) -> Vec<Mark>
5032:fn parse_outcome(s: &str) -> ClaimOutcome
5042:fn new_thread(app: &AppHandle, emp: &Employee, title: &str) -> String
5047:fn new_thread_with_model(
5066:fn new_dev_thread(
5131:fn new_thread_full(
5156:fn append_necessary_lesson(app: &AppHandle, emp: &Employee, key: &str, title: &str, lesson: &str)
5172:async fn run_wake(
5469:fn build_wake_prompt(
5571:fn wake_context_blocks(prior_note: &str, extra: &str) -> (String, String, String)
5627:fn parse_wake_decision(text: &str) -> Option<WakeDecision>
5678:fn parse_wake_decision_json(text: &str) -> Option<WakeDecision>
5696:fn parse_work_preflight(text: &str) -> Option<WorkPreflight>
5700:fn parse_work_preflight_json(text: &str) -> Option<WorkPreflight>
5717:fn preflight_memory_block(preflight: &WorkPreflight) -> String
5766:fn preflight_extra_block(preflight: Option<&WorkPreflight>) -> String
5821:fn record_preflight_prepare_result(
5852:fn record_preflight_prepare_failure(
5871:fn append_thread_system(app: &AppHandle, thread_id: &str, text: &str, level: &str)
5892:fn apply_workspace(
6005:fn normalize_preflight_plan(
6030:fn default_employee_branch(emp: &Employee, key: &str) -> String
6038:fn branch_component(s: &str, fallback: &str) -> String
6068:fn unique_manual_key(content: &str) -> String
6075:fn current_branch_label(repo: &str) -> String
6084:fn attach_workflow_thread(app: &AppHandle, workflow: &Workflow, thread_id: &str)
6105:fn mark_workflow_waiting(app: &AppHandle, emp_id: &str, scope: &str, key: &str)
6115:fn mark_workflow_blocked(app: &AppHandle, emp_id: &str, scope: &str, key: &str)
6126:fn route_key_from_decision(dec: &Decision) -> Option<String>
6141:fn workflow_context(workflow: &Workflow, _emp: &Employee, startup: bool) -> String
6173:fn commit_workflow_if_needed(
6225:fn file_workflow_decision(
6252:fn discard_thread(app: &AppHandle, thread_id: &str)
6270:pub(crate) fn discard_employee_thread(app: &AppHandle, thread_id: &str)
6275:fn rename_thread(app: &AppHandle, thread_id: &str, title: &str)
6288:pub(crate) fn rename_employee_thread(app: &AppHandle, thread_id: &str, title: &str)
6292:fn create_task(
6328:fn finish_task(app: &AppHandle, task_id: &str, status: &str, result: Option<String>)
6345:async fn ledger_register_open(
6408:pub fn migrate_tasks_to_ledger(app: &AppHandle)
6456:pub async fn register_ledger_item(
6563:pub async fn delegate_employee_work(
6609:fn thread_alive(app: &AppHandle, thread_id: &str) -> bool
6615:fn thread_is_mind(app: &AppHandle, thread_id: &str) -> bool
6624:fn supervision_forces_do(prior_note: &str, extra: &str) -> bool
6634:fn thread_has_assistant(app: &AppHandle, thread_id: &str) -> bool
6645:fn extract_last_assistant(app: &AppHandle, thread_id: &str) -> Option<String>
6655:pub(crate) fn last_employee_assistant(app: &AppHandle, thread_id: &str) -> Option<String>
6661:fn thread_has_error(app: &AppHandle, thread_id: &str) -> bool
6673:pub(crate) fn employee_thread_has_error(app: &AppHandle, thread_id: &str) -> bool
6677:fn append_journal(
6688:fn append_journal_full(
6711:fn append_event(
6746:fn append_journal_kind(
6793:fn fmt_ts(ts: i64) -> String
6803:fn sanitize_name(name: &str) -> String
6826:fn export_memory_files(app: &AppHandle, emp: &Employee) -> Option<String>
6915:fn cleanup_legacy_memory_files(emp: &Employee, safe_name: &str)
6927:fn memory_hint(_mem_dir: &str) -> String
6932:fn tool_ctx(app: &AppHandle) -> Option<(String, String)>
6946:fn tools_manual(
6956:fn tools_manual_cli(
7035:async fn retrieve_memory(
7194:async fn semantic_rank<'a>(
7296:fn evidence_delta(e: &JournalEntry) -> i32
7300:fn evidence_boost_f32(e: &JournalEntry) -> f32
7304:fn evidence_boost_f64(e: &JournalEntry) -> f64
7308:fn is_cjk(ch: char) -> bool
7312:fn push_ascii(buf: &mut String, out: &mut Vec<String>)
7320:fn push_cjk(buf: &mut Vec<char>, out: &mut Vec<String>)
7332:fn tokenize_terms(text: &str) -> Vec<String>
7358:pub(crate) fn bm25_rank<'a>(
7438:fn one_line(s: &str, max: usize) -> String
7451:enum ParsedNextAction
7456:fn extract_json_object(s: &str) -> Option<&str>
7487:fn json_str(v: &serde_json::Value, key: &str) -> String
7495:fn json_options(v: &serde_json::Value) -> Vec<String>
7511:fn find_last_next_action_marker(text: &str) -> Option<usize>
7527:fn parse_next_action(
7654:fn parse_blocked(text: &str) -> Option<String>
7666:fn charter_or_default(charter: &str) -> &str
7674:fn build_scout_prompt(
7724:fn build_dev_prompt(
7772:fn build_dev_followup(_app: &AppHandle, _emp: &Employee, task_title: &str, extra: &str) -> String
7785:fn followup_extra_with_note(prior_note: &str, extra: &str) -> String
7797:fn build_reply_prompt(
7826:    fn parse_work_preflight_accepts_bare_json()
7849:    fn parse_work_preflight_accepts_marked_json()
7867:    fn wake_context_blocks_surfaces_edict()
7878:    fn supervision_forces_do_detects_批示()
7888:    fn parse_wake_decision_accepts_nested_workspace()
7901:    fn parse_wake_decision_escalate()

### src-tauri/src/gitwt.rs
12:pub fn run(repo: &str, args: &[&str]) -> Result<String, String>
18:pub fn run_raw(repo: &str, args: &[&str]) -> Result<String, String>
23:fn git_stdout(repo: &str, args: &[&str]) -> Result<String, String>
42:pub fn is_repo(dir: &str) -> bool
52:pub fn repo_root(dir: &str) -> Result<String, String>
69:pub fn valid_branch(branch: &str) -> bool
88:pub fn branch_conflict(repo: &str, branch: &str) -> Option<String>
105:pub fn list_branches(dir: &str) -> Result<Vec<String>, String>
120:pub fn current_branch(dir: &str) -> String
127:pub fn add(repo: &str, path: &str, branch: &str, base: &str) -> Result<(), String>
141:pub fn branch_checked_out(repo: &str, branch: &str) -> Option<String>
158:pub fn is_clean(dir: &str) -> Result<bool, String>
163:pub fn checkout(dir: &str, branch: &str) -> Result<(), String>
169:pub fn checkout_new_branch(dir: &str, branch: &str, base: &str) -> Result<(), String>
180:pub fn has_changes(dir: &str) -> Result<bool, String>
185:pub fn commit_all(dir: &str, message: &str) -> Result<bool, String>
195:pub fn merge(dir: &str, branch: &str) -> Result<(), String>
201:pub fn has_conflicts(dir: &str) -> bool
208:pub fn merge_abort(dir: &str)
213:pub fn remove(repo: &str, path: &str) -> Result<(), String>
219:pub fn delete_branch(repo: &str, branch: &str) -> Result<(), String>

### src-tauri/src/http_stream.rs
10:pub struct SseDecoder
16:impl SseDecoder
17:    pub fn new() -> Self
25:    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<String>, String>
46:    pub fn finish(&mut self) -> Result<Vec<String>, String>
59:    fn consume_line(&mut self, line: &[u8], events: &mut Vec<String>) -> Result<(), String>
83:    fn finish_event(&mut self, events: &mut Vec<String>)
94:pub fn decode_sse_json(text: &str) -> Result<Value, String>
127:    fn parses_chunked_multiline_sse_and_heartbeats()
135:    fn decodes_per_event_gzip_envelope()

### src-tauri/src/lib.rs
66:pub struct AppState
138:impl AppState
140:    pub fn acp_for(&self, kind: &AgentKind) -> Option<Arc<AcpManager>>
147:    pub fn borrowed_runtime(&self, thread_id: &str) -> Option<BorrowedRuntime>
156:    pub fn agent_enabled(&self, kind: &AgentKind) -> bool
173:    fn title_fallback_mgr(&self, _origin: &AgentKind) -> Arc<AcpManager>
181:    pub fn generate_title(
286:pub(crate) fn is_running(state: &AppState, thread: &Thread) -> bool
317:fn running_by_id(state: &AppState, thread_id: &str) -> bool
325:fn is_ordinary_thread(thread: &Thread) -> bool
332:fn is_starrable_thread(thread: &Thread) -> bool
336:fn is_normal_thread_for_auto_cleanup(thread: &Thread) -> bool
340:fn thread_is_expired(updated_at: i64, now: i64, hours: u32) -> bool
344:fn run_session_auto_cleanup(app: &tauri::AppHandle) -> usize
399:fn any_session_running(state: &AppState) -> bool
406:fn any_employee_working(state: &AppState) -> bool
411:pub(crate) async fn shutdown_agent_processes(state: &AppState)
432:fn agent_kind_for_thread(state: &AppState, thread_id: &str) -> Result<AgentKind, String>
440:fn cleanup_borrowed_runtime(state: &AppState, thread_id: &str)
458:    fn thread_is_expired_only_after_the_configured_retention()
468:    fn thread_expiration_does_not_count_weekends()
493:    fn special_threads_are_not_auto_cleanup_candidates()
511:    fn quota_threads_support_starring()
521:    fn starred_descendant_protects_its_tree()
530:fn wide_null(s: &str) -> Vec<u16>
535:fn spawn_single_instance_focus_listener(app: &tauri::AppHandle)
575:fn backend_is_available(kind: &AgentKind, program: &str) -> bool
587:    fn sdk_backends_are_available_without_legacy_cli_paths()
603:fn spawn_backend_availability_check(app: tauri::AppHandle)
644:fn get_backend_availability(state: State<'_, AppState>) -> HashMap<String, bool>
649:async fn get_cli_statuses(settings: Settings) -> Vec<cli_manager::CliStatus>
654:async fn upgrade_cli(
667:fn cancel_cli_operation(state: State<'_, AppState>, operation_id: String) -> bool
672:fn list_threads(state: State<'_, AppState>) -> Vec<ThreadMeta>
726:async fn list_clue_groups(state: State<'_, AppState>) -> Result<Vec<clues::ClueNodeGroup>, String>
737:async fn get_clue_context(
748:fn local_clue_author_name() -> String
756:async fn capture_clue(
817:async fn add_clue_comment(
854:async fn associate_clues(
881:async fn disassociate_clues(
908:async fn split_clue(
927:async fn stack_clues(
946:async fn delete_clue(
970:fn get_thread(state: State<'_, AppState>, thread_id: String) -> Result<Thread, String>
982:struct ProjectEntry
990:struct ProjectWorktreeInfo
995:fn local_project_entries(state: &AppState) -> Vec<ProjectEntry>
1058:fn list_projects(state: State<'_, AppState>) -> Vec<ProjectEntry>
1062:fn project_path_key(path: &str) -> String
1073:fn match_existing_project_folders(projects: Vec<String>, folders: Vec<String>) -> Vec<String>
1091:pub(crate) fn restrict_roaming_folders_to_projects(
1102:pub(crate) fn current_roaming_project_folders(state: &AppState) -> Vec<String>
1120:    fn roaming_folders_only_keep_existing_projects()
1136:fn remove_project(state: State<'_, AppState>, cwd: String)
1143:fn prewarm(
1172:fn clean_frontmatter_value(value: &str) -> String
1187:fn frontmatter_value(contents: &str, key: &str) -> Option<String>
1210:fn collect_skill_files(dir: &Path, depth: usize, files: &mut Vec<PathBuf>)
1235:fn codex_skill_roots(config_dir: &Path) -> Vec<PathBuf>
1243:fn list_skill_commands(
1291:fn list_codex_skill_commands(config_dir: &Path) -> Vec<Value>
1295:fn list_alkaid_skill_commands(config_dir: &Path) -> Vec<Value>
1309:    fn alkaid_skills_use_pi_slash_command_syntax()
1329:fn worktree_base(state: &AppState) -> PathBuf
1348:fn precheck_worktree_branch(repo: &str, branch: &str, base: &str) -> Result<bool, String>
1370:fn reuse_worktree(
1402:pub fn create_worktree_for(
1458:async fn create_thread(
1662:fn scratch_dir() -> Result<String, String>
1674:fn directory_exists(path: String) -> bool
1681:async fn is_git_repo(path: String) -> bool
1690:struct BranchList
1697:fn list_branches(path: String) -> Result<BranchList, String>
1706:fn request_peer_branches(state: State<'_, AppState>, peer_token: String, folder: String)
1712:fn list_worktrees(state: State<'_, AppState>) -> Vec<WorktreeRecord>
1723:fn remove_worktree(
1792:async fn merge_worktree_thread(
1947:async fn get_quota() -> Result<Value, String>
1953:async fn get_model_costs() -> Result<Value, String>
1959:async fn check_update(app: tauri::AppHandle) -> Result<Value, String>
1966:async fn download_staged_update(app: tauri::AppHandle) -> Result<Value, String>
1972:async fn apply_staged_update(app: tauri::AppHandle) -> Result<(), String>
1978:fn report_activity(state: State<'_, AppState>, thread_id: Option<String>)
1985:fn show_main_window(app: tauri::AppHandle)
1992:fn take_restore_thread(app: tauri::AppHandle) -> Option<String>
1998:fn signature_pending(app: tauri::AppHandle) -> Option<signature::SignatureIdentity>
2004:fn expand_thread_tree_ids(state: &AppState, roots: &[String]) -> Vec<String>
2022:fn delete_thread(
2055:fn tree_contains_starred_thread(tree: &[String], starred_thread_ids: &HashSet<String>) -> bool
2060:fn collect_deletable_thread_ids(
2098:fn remove_threads(app: &tauri::AppHandle, state: &AppState, deletable: Vec<String>) -> Vec<Thread>
2128:fn delete_threads_impl(
2139:fn delete_threads(
2149:fn delete_project_threads(
2160:fn open_in_editor(
2234:fn open_path_default(abs: &std::path::Path) -> Result<(), String>
2261:fn open_file_default(
2281:fn open_clue_attachment(
2312:pub struct RevertChange
2323:fn revert_file_changes(
2394:fn open_in_explorer(path: String) -> Result<(), String>
2464:fn open_in_terminal(path: String) -> Result<(), String>
2519:fn open_url(url: String) -> Result<(), String>
2555:fn set_prompt_queue_pending(thread_id: String, pending: bool)
2560:fn notify_fire_done(
2584:fn create_time_machine_checkpoint(
2601:fn get_time_machine_timeline(
2610:fn get_time_machine_checkpoint_preview(
2620:fn restore_time_machine_checkpoint(
2659:fn delete_time_machine_context(
2738:fn rename_thread(
2759:fn set_thread_model(
2820:fn set_thread_mode(
2861:fn set_thread_reasoning_effort(
2874:fn set_thread_starred(
2897:fn set_thread_agent(
2991:async fn get_model_options(
3017:async fn get_slash_commands(
3041:fn list_skills(state: State<'_, AppState>) -> Vec<skills::SkillInfo>
3046:fn get_skills_dir(state: State<'_, AppState>) -> String
3053:fn install_skill(state: State<'_, AppState>, path: String) -> Result<skills::SkillInfo, String>
3058:fn remove_skill(state: State<'_, AppState>, name: String) -> Result<(), String>
3063:fn sync_skills(state: State<'_, AppState>) -> Result<(), String>
3068:fn send_prompt(
3077:fn append_thread_error(app: &tauri::AppHandle, thread_id: &str, error: String)
3090:pub(crate) fn dispatch_prompt(
3274:fn truncate_thread(
3491:async fn cancel_turn(
3610:async fn compact_thread(state: State<'_, AppState>, thread_id: String) -> Result<(), String>
3637:async fn respond_permission(
3695:fn get_settings(state: State<'_, AppState>) -> Settings
3700:fn refresh_environment_variables() -> Result<usize, String>
3705:fn get_global_agent_instructions(
3712:fn set_global_agent_instructions(
3721:async fn set_settings(
3731:async fn apply_runtime_settings(
3858:fn start_headless_config_watcher(app: tauri::AppHandle)
3909:fn get_relay_status(state: State<'_, AppState>) -> Value
3915:async fn verify_relay(
3924:fn get_relay_peers(state: State<'_, AppState>) -> Value
3930:async fn list_achievements(state: State<'_, AppState>) -> Result<Vec<relay::Achievement>, String>
3935:fn get_relay_inbox(state: State<'_, AppState>) -> Vec<Share>
3941:fn share_thread(state: State<'_, AppState>, thread_id: String, to: String) -> Result<(), String>
3947:fn advanced_share(
3966:struct ClueAiSummary
3973:async fn summarize_clue(
4063:fn parse_clue_ai_summary(raw: &str, fallback_title: &str) -> ClueAiSummary
4108:fn accept_share(
4120:fn decline_share(state: State<'_, AppState>, id: String)
4125:fn list_roaming_folders(state: State<'_, AppState>) -> Vec<String>
4130:fn is_folder_roaming(state: State<'_, AppState>, cwd: String) -> bool
4138:fn set_folder_roaming(state: State<'_, AppState>, cwd: String, allowed: bool) -> bool
4160:fn set_roaming_folders(state: State<'_, AppState>, folders: Vec<String>) -> Vec<String>
4173:async fn create_roaming_thread(
4218:async fn create_quota_thread(
4250:async fn prepare_quota_lease(
4263:fn cancel_quota_roaming(state: State<'_, AppState>, operation_id: String) -> bool
4272:fn recall_roaming_thread(state: State<'_, AppState>, thread_id: String) -> Result<(), String>
4279:fn request_peer_models(state: State<'_, AppState>, peer_token: String)
4285:fn respond_roam_request(
4312:async fn restart_devin(state: State<'_, AppState>) -> Result<(), String>
4325:async fn get_status(state: State<'_, AppState>) -> Result<Value, String>
4350:fn get_logs(state: State<'_, AppState>) -> Vec<String>
4359:fn list_employees(app: tauri::AppHandle) -> Vec<employees::Employee>
4365:fn create_employee(
4414:fn update_employee(app: tauri::AppHandle, employee: employees::Employee) -> Result<(), String>
4419:fn delete_employee(app: tauri::AppHandle, id: String)
4424:fn set_employee_enabled(app: tauri::AppHandle, id: String, enabled: bool)
4429:fn get_employee_mind(app: tauri::AppHandle, id: String) -> mind::MindSnapshot
4434:fn set_employee_mind_enabled(app: tauri::AppHandle, id: String, enabled: bool)
4439:fn resume_employee_mind(app: tauri::AppHandle, id: String)
4445:fn run_employee_now(app: tauri::AppHandle, id: String) -> Result<(), String>
4450:fn list_employee_tasks(app: tauri::AppHandle) -> Vec<employees::Task>
4455:fn assign_task(
4465:fn delete_task(app: tauri::AppHandle, id: String) -> Result<(), String>
4471:async fn register_ledger_item(
4492:async fn delegate_employee_work(
4510:fn get_employee_memory(app: tauri::AppHandle, id: String) -> Vec<employees::JournalEntry>
4516:fn add_employee_memory(
4527:fn update_employee_memory(app: tauri::AppHandle, id: String, ts: i64, summary: String)
4532:fn delete_employee_memory(app: tauri::AppHandle, id: String, ts: i64)
4537:fn set_employee_memory_pinned(app: tauri::AppHandle, id: String, ts: i64, pinned: bool)
4542:fn set_employee_memory_feedback(
4554:fn list_marks(state: State<'_, AppState>, scope: Option<String>) -> Vec<marks::Mark>
4560:fn release_mark(app: tauri::AppHandle, state: State<'_, AppState>, scope: String, key: String)
4567:async fn reset_mark(
4590:fn set_mark(
4614:fn list_decisions(app: tauri::AppHandle) -> Vec<employees::Decision>
4619:fn list_notices(app: tauri::AppHandle) -> Vec<notice::Notice>
4625:fn resolve_decision(app: tauri::AppHandle, id: String, answer: String) -> Result<(), String>
4650:fn legacy_resolve_decision(app: &tauri::AppHandle, id: &str, answer: &str) -> Result<(), String>
4676:fn reject_decision(app: tauri::AppHandle, id: String, answer: String) -> Result<(), String>
4700:fn legacy_reject_decision(app: &tauri::AppHandle, id: &str, answer: &str) -> Result<(), String>
4726:fn read_report(app: tauri::AppHandle, id: String)
4758:fn review_report(app: tauri::AppHandle, id: String, answer: String) -> Result<(), String>
4808:fn dismiss_decision(app: tauri::AppHandle, id: String)
4854:fn delete_decision(app: tauri::AppHandle, id: String) -> Result<(), String>
4877:fn relay_of(app: &tauri::AppHandle) -> Arc<RelayManager>
4883:async fn list_shared_marks(
4896:async fn release_shared_mark(
4910:async fn reset_shared_mark(
4926:async fn set_shared_mark(
4943:fn embed_cfg(app: &tauri::AppHandle) -> (String, String, String)
4955:async fn semantic_status(app: tauri::AppHandle) -> Result<Value, String>
4967:async fn semantic_pull(app: tauri::AppHandle, model: Option<String>) -> Result<(), String>
4979:fn semantic_rebuild(state: State<'_, AppState>, employee_id: Option<String>)
4990:fn register_roaming_forwarders(app: &tauri::AppHandle, relay: Arc<RelayManager>)
5055:fn register_remote_permission_capture(app: &tauri::AppHandle)
5108:    fn build_profiles_use_separate_data_directories()
5117:pub fn nova_data_dir(app: &tauri::AppHandle) -> PathBuf
5140:fn migrate_data_to_home(app: &tauri::AppHandle, new_dir: &Path)
5172:fn dir_has_entries(dir: &Path) -> bool
5179:fn copy_dir_all(from: &Path, to: &Path) -> std::io::Result<()>
5196:pub fn maybe_run_cli() -> bool
5201:pub fn maybe_run_update_helper() -> bool
5206:pub fn run()

### src-tauri/src/main.rs
12:fn wait_for_restart_parent()
34:struct SingleInstanceGuard
40:impl Drop for SingleInstanceGuard
41:    fn drop(&mut self)
55:fn wide_null(s: &str) -> Vec<u16>
60:fn signal_existing_gui_instance()
78:fn acquire_single_instance() -> Option<SingleInstanceGuard>
122:fn should_enforce_single_instance() -> bool
127:struct SingleInstanceGuard
134:fn signal_existing_gui_instance()
147:fn acquire_single_instance() -> Option<SingleInstanceGuard>
189:fn dirs_nova_lock_path() -> std::path::PathBuf
197:fn should_enforce_single_instance() -> bool
201:fn main()
264:struct VirtualDisplay(std::process::Child);
267:impl Drop for VirtualDisplay
268:    fn drop(&mut self)
275:fn start_virtual_display_if_needed() -> Result<Option<VirtualDisplay>, String>

### src-tauri/src/marks.rs
19:pub struct Mark
64:pub enum ClaimOutcome
76:struct MarksFile
80:pub struct MarkStore
85:impl MarkStore
86:    pub fn load(dir: &PathBuf) -> Self
96:    pub fn save(&self)
108:    fn idx(&self, scope: &str, key: &str) -> Option<usize>
114:    pub fn get(&self, scope: &str, key: &str) -> Option<&Mark>
119:    pub fn claim(
187:    pub fn set_status(
215:    pub fn set_thread(&mut self, scope: &str, key: &str, thread_id: &str)
229:    pub fn release(&mut self, scope: &str, key: &str)
235:    pub fn register_open(
298:    pub fn remove(&mut self, scope: &str, key: &str)
303:    pub fn list(&self, scope: Option<&str>) -> Vec<Mark>
318:    pub fn digest(&self, scope: &str) -> String
330:pub fn render_digest(marks: &[Mark]) -> String

### src-tauri/src/mcp.rs
29:pub struct McpServerConfig
37:pub fn load_mcp_config(data_dir: &Path) -> HashMap<String, McpServerConfig>
88:type PendingMap = Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>;
91:struct McpConnection
99:impl McpConnection
102:    async fn connect(config: &McpServerConfig, cwd: &str) -> Result<Self, String>
177:    async fn send_line(&self, value: &Value) -> Result<(), String>
189:    async fn notify(&self, method: &str, params: Value) -> Result<(), String>
194:    async fn request(&self, method: &str, params: Value) -> Result<Value, String>
212:    async fn list_tools(&self) -> Result<Vec<Value>, String>
221:    async fn call_tool(&self, name: &str, arguments: &Value) -> Result<Value, String>
231:fn mcp_result(result: &Value) -> (Value, bool)
265:struct McpTool
270:pub struct McpHub
275:impl McpHub
279:    pub async fn connect(
325:    pub fn tool_definitions(&self) -> Vec<Value>
329:    pub fn is_empty(&self) -> bool
335:    pub async fn call_tool(&self, mcp_name: &str, arguments: &Value) -> (Value, bool)

### src-tauri/src/mind.rs
24:fn default_true() -> bool
30:pub struct MindEvent
48:pub struct MindHandoff
62:pub struct AttentionPlan
86:pub struct MindState
124:impl MindState
125:    fn new(employee_id: &str) -> Self
151:pub struct MindSnapshot
172:struct MindAudit
184:struct MindFile
197:pub struct MindStore
206:impl MindStore
207:    pub fn load(dir: &PathBuf) -> Self
235:    pub fn save(&self)
256:    fn state_mut(&mut self, employee_id: &str) -> &mut MindState
262:    fn add_event(&mut self, mut event: MindEvent) -> bool
286:    fn pending_events(&self, employee_id: &str) -> Vec<MindEvent>
304:    fn pending_count(&self, employee_id: &str) -> usize
316:    fn latest_seq(&self, employee_id: &str) -> u64
324:    fn add_audit(&mut self, audit: MindAudit)
333:    fn snapshot(&mut self, employee_id: &str) -> MindSnapshot
359:struct MindOutput
382:struct MindKnowledgeAction
393:struct MindLessonAction
400:struct DreamMergeAction
413:struct DreamDowngradeAction
423:struct MindVerifyAction
431:struct MindChallengeAction
439:impl MindOutput
440:    fn has_progress(&self) -> bool
455:struct MindEscalation
466:pub fn record_journal_event(
506:pub fn invalidate_brief(app: &AppHandle, employee_id: &str, reason: &str)
544:pub fn remove_employee(app: &AppHandle, employee_id: &str)
554:pub fn snapshot(app: &AppHandle, employee_id: &str) -> MindSnapshot
572:pub fn check_memory_pressure(app: &AppHandle, employee_id: &str)
607:pub fn set_enabled(app: &AppHandle, employee_id: &str, enabled: bool)
642:pub fn resume(app: &AppHandle, employee_id: &str)
657:pub fn is_active_thread(app: &AppHandle, thread_id: &str) -> bool
665:pub fn manual_stop(app: &AppHandle, employee_id: &str, thread_id: &str)
682:pub fn preempt_for_work(app: &AppHandle, employee_id: &str)
706:pub fn tick(app: &AppHandle)
787:async fn run_once(
859:async fn apply_output(
934:fn finish_cancelled(app: &AppHandle, employee_id: &str, run_id: &str)
959:fn finish_failure(app: &AppHandle, emp: &Employee, run_id: &str, error: &str)
1004:fn recover_expired_runs(app: &AppHandle)
1031:fn sync_legacy_events(app: &AppHandle)
1074:fn build_context(app: &AppHandle, emp: &Employee, events: &[MindEvent]) -> String
1186:fn build_prompt(emp: &Employee, context: &str) -> String
1222:fn apply_memory_actions(
1476:fn parse_output(text: &str) -> Option<MindOutput>
1489:fn create_mind_decision(
1532:fn create_system_escalation(
1553:pub fn on_decision(app: &AppHandle, decision: &Decision)
1592:fn event_severity(kind: &str) -> u8
1602:fn event_fingerprint(events: &[MindEvent]) -> String
1613:fn stable_hash(text: &str) -> String
1619:fn cap(text: &str, max_chars: usize) -> String
1628:fn cap_vec(items: Vec<String>, max_items: usize, max_chars: usize) -> Vec<String>

### src-tauri/src/model_cache.rs
8:fn cache_dir(config_dir: &Path) -> PathBuf
12:fn cache_path(config_dir: &Path, agent_kind: &str) -> PathBuf
17:pub fn load(config_dir: &Path, agent_kind: &str) -> Option<Value>
23:pub fn save(config_dir: &Path, agent_kind: &str, options: &Value)

### src-tauri/src/notice.rs
23:pub struct ActorRef
31:impl ActorRef
32:    pub fn user() -> Self
39:    pub fn system() -> Self
46:    pub fn employee(id: &str, name: &str) -> Self
53:    pub fn peer(name: &str) -> Self
60:    pub fn is_user(&self) -> bool
63:    pub fn employee_id(&self) -> Option<&str>
74:pub struct NoticeTopic
84:pub struct NoticeOption
91:pub struct NoticeBody
102:pub struct NoticeHold
110:pub enum Action
168:pub struct NoticeExpect
183:pub struct NoticeResponse
194:pub struct NoticeMeta
207:pub struct Notice
232:struct NoticesFile
239:pub struct NoticeStore
246:impl NoticeStore
247:    pub fn load(dir: &PathBuf) -> Self
261:    pub fn save(&self)
274:    fn prune(&mut self)
293:    pub fn list(&self) -> Vec<Notice>
311:    pub fn get(&self, id: &str) -> Option<&Notice>
315:    pub fn get_mut(&mut self, id: &str) -> Option<&mut Notice>
319:    pub fn add(&mut self, n: Notice)
326:    pub fn add_or_merge_pending(&mut self, n: Notice) -> bool
373:    pub fn pending_hold_for(&self, employee_id: &str, scope: &str, key: &str) -> bool
382:    pub fn pending_hold_on(&self, scope: &str, key: &str) -> bool
391:    pub fn withdraw_for(&mut self, scope: &str, key: &str) -> usize
411:    pub fn retain_employee(&mut self, employee_id: &str)
420:    pub fn remove(&mut self, id: &str) -> bool
431:    fn injection_key(employee_id: &str, scope: &str, key: &str) -> String
435:    pub fn put_injection(&mut self, employee_id: &str, scope: &str, key: &str, text: &str)
449:    pub fn take_injection(&mut self, employee_id: &str, scope: &str, key: &str) -> Option<String>
459:    pub fn has_injection_for(&self, employee_id: &str) -> bool
467:fn opt_id_label(label: &str) -> NoticeOption
483:pub fn options_from_labels(labels: &[String]) -> Vec<NoticeOption>
492:pub fn template_decision(
552:pub fn template_work(
591:pub fn template_discuss(
668:pub fn template_report(sender: &Employee, key: &str, title: &str) -> NoticeExpect
686:pub struct EmitParams
698:pub fn emit_notice(app: &AppHandle, params: EmitParams) -> Option<Notice>
779:pub struct RespondParams
785:pub fn respond_notice(
879:fn fill_action_placeholders(
1025:pub fn withdraw_notices(app: &AppHandle, scope: &str, key: &str) -> usize
1037:pub fn pending_hold_for(app: &AppHandle, employee_id: &str, scope: &str, key: &str) -> bool
1043:pub fn pending_hold_on(app: &AppHandle, scope: &str, key: &str) -> bool
1050:pub fn pending_discuss_id(
1073:pub fn take_injection(
1084:pub fn has_pending_work_signal(app: &AppHandle, employee_id: &str) -> bool
1090:fn emit_changed(app: &AppHandle)
1096:fn notify_user_notice(app: &AppHandle, notice: &Notice)
1111:fn execute_actions(
1249:pub fn notice_to_decision(n: &Notice) -> Decision
1327:pub fn list_as_decisions(app: &AppHandle) -> Vec<Decision>
1361:pub fn migrate_from_decisions(app: &AppHandle)
1516:pub fn emit_decision(
1563:pub fn emit_report(

### src-tauri/src/opencode_sdk.rs
25:struct RunningBridge
30:enum PendingRequestKind
35:struct PendingPermission
41:pub struct OpenCodeSdkManager
55:impl OpenCodeSdkManager
56:    pub fn new(app: AppHandle) -> Arc<Self>
60:    pub fn new_with_env(app: AppHandle, launch_env: HashMap<String, String>) -> Arc<Self>
76:    pub fn is_running(&self, thread_id: &str) -> bool
80:    pub fn has_pending_permission(&self, request_key: &str) -> bool
87:    pub fn get_model_options(&self) -> Option<Value>
91:    pub fn seed_model_options(&self, value: Value)
95:    pub fn spawn_revalidate_model_options(self: &Arc<Self>)
113:    async fn refresh_model_options(&self) -> Result<Value, String>
129:    pub async fn ensure_model_options(self: &Arc<Self>) -> Result<Value, String>
150:    pub async fn fetch_commands(&self) -> Result<Vec<Value>, String>
170:    pub async fn run_prompt(
183:    async fn run_new_prompt(
350:    pub async fn steer_prompt(
362:    async fn interrupt_for_steer(&self, thread_id: &str)
393:    pub async fn cancel(&self, thread_id: &str)
413:    pub async fn fork_session(
435:    pub fn forget_session_of_thread(&self, thread_id: &str)
442:    pub fn shutdown(&self)
449:    pub fn generate_title_async(
490:    async fn run_prompt_bridge(
527:    async fn run_bridge(&self, cwd: &str, request: Value) -> Result<Value, String>
534:    async fn read_prompt_events(
586:    fn spawn_bridge(&self, cwd: &str) -> Result<Child, String>
628:    fn save_session_id(&self, thread_id: &str, session_id: &str)
637:    fn save_checkpoint(&self, thread_id: &str, user_item_id: u64, event: &Value)
656:    fn apply_part(&self, thread_id: &str, part: &Value, part_items: &mut HashMap<String, u64>)
705:    fn apply_plan(&self, thread_id: &str, plan: &Value)
720:    fn handle_permission(&self, thread_id: &str, permission: &Value)
757:    fn handle_question(&self, thread_id: &str, question: &Value)
786:    pub async fn respond_permission(
822:    fn clear_permissions(&self, thread_id: &str)
840:    fn push_system(&self, thread_id: &str, text: String, level: &str)
850:    fn set_running(&self, thread_id: &str, running: bool, stop_reason: Option<&str>)
873:    fn finish_turn(&self, thread_id: &str, stop_reason: &str)
899:    fn is_current_run(&self, thread_id: &str, run_epoch: u64) -> bool
907:    fn finish_turn_if_current(&self, thread_id: &str, run_epoch: u64, stop_reason: &str)
913:    fn emit_update(&self, thread_id: &str, item: &Item) -> Result<(), tauri::Error>
923:fn complete_pending_tools(thread: &mut crate::threads::Thread) -> Vec<Item>
937:fn prompt_attachment_part(image: PromptImage) -> Option<Value>
952:impl Drop for OpenCodeSdkManager
953:    fn drop(&mut self)
958:fn bridge_path(app: &AppHandle) -> Result<PathBuf, String>
969:async fn write_request(child: &mut Child, request: &Value) -> Result<(), String>
978:async fn write_line(
990:fn parse_bridge_output(output: String) -> Result<Value, String>
1004:fn kill_child(child: &mut Child)
1011:fn current_dir() -> Result<String, String>
1017:fn split_model_variant(model: &str) -> Option<(&str, &str, Option<&str>)>
1030:fn with_command(mut request: Value, text: &str) -> Value
1049:fn normalize_title(output: &str, fallback: &str) -> String
1062:fn provider_options(value: Value) -> Result<Value, String>
1113:fn supports_images(model: &Value) -> bool
1120:fn variant_label(variant: &str) -> &str
1133:fn text_item(id: u64, part: &Value, thought: bool) -> Option<Item>
1150:fn tool_call(part: &Value) -> ToolCall
1250:fn compact_tool_detail(value: &str) -> String
1261:fn question_command(request_id: &str, answer: &str) -> Result<Value, String>
1270:fn derive_title(text: &str, has_images: bool) -> String
1293:    fn title_fallback_uses_first_prompt_line_or_image()
1303:    fn builds_question_reply_and_reject_commands()
1320:    fn ordinary_attachments_become_readable_paths()
1345:    fn splits_sdk_model_identifier()
1358:    fn maps_provider_models_to_nova_options()
1384:    fn recognizes_sdk_slash_commands()
1399:    fn maps_read_and_todo_tools_for_display()

### src-tauri/src/path_env.rs
16:pub fn init_process_path()
26:pub fn refresh_process_environment() -> Result<usize, String>
38:fn merge_paths<'a>(groups: impl IntoIterator<Item = &'a OsStr>) -> Option<OsString>
56:fn init_windows_process_path()
72:fn fallback_windows_path(home: Option<PathBuf>) -> Option<OsString>
79:struct RegistryEnvironmentValue
86:fn refresh_windows_process_environment() -> Result<usize, String>
161:fn read_registry_environment(
186:fn expand_windows_environment_value(
224:fn extract_marked_path(output: &[u8]) -> Option<OsString>
244:fn bytes_to_os_string(value: &[u8]) -> OsString
250:fn bytes_to_os_string(value: &[u8]) -> OsString
255:fn init_macos_process_path()
278:fn login_shell_path() -> PathBuf
286:fn read_shell_path(shell: &std::path::Path) -> Option<OsString>
337:fn fallback_macos_path() -> Option<OsString>
374:fn append_matching_dirs(paths: &mut Vec<PathBuf>, root: PathBuf, suffix: &str)
394:    fn extracts_path_while_ignoring_shell_startup_output()
404:    fn merge_keeps_priority_and_removes_duplicates()
422:    fn windows_fallback_includes_claude_and_opencode_native_bins()

### src-tauri/src/quota.rs
8:fn home_dir() -> Option<PathBuf>
15:fn devin_data_dir() -> Option<PathBuf>
32:fn devin_cache_dir() -> Option<PathBuf>
43:fn credentials_path() -> Option<PathBuf>
48:fn read_credentials() -> Result<(String, String), String>
67:fn find<'a>(v: &'a Value, key: &str) -> Option<&'a Value>
80:fn as_f64(v: &Value) -> Option<f64>
86:fn as_i64(v: &Value) -> Option<i64>
93:fn devin_version() -> Option<String>
100:fn model_prices(m: &Value) -> Value
145:    fn parses_model_dimension_prices()
160:pub async fn fetch_model_costs() -> Result<Value, String>
245:pub async fn fetch_quota() -> Result<Value, String>

### src-tauri/src/relay.rs
64:struct RoamGuest
72:struct PendingRoam
96:struct PendingQuotaClient
104:struct QuotaLeaseKey
110:impl QuotaLeaseKey
111:    fn new(peer: String, agent_kind: AgentKind, model: &str) -> Result<Self, String>
134:type QuotaLeaseResult = Result<CredentialBundle, String>;
135:type QuotaLeaseWaiter = oneshot::Sender<QuotaLeaseResult>;
137:struct QuotaOperation
142:impl QuotaOperation
143:    fn new() -> Self
150:    fn cancel(&self) -> bool
167:    fn commit(&self) -> bool
178:    fn ensure_active(&self) -> Result<(), String>
186:    async fn cancelled(&self)
194:fn quota_model_key(kind: &AgentKind, model: &str) -> String
198:fn ensure_quota_backend_supported(kind: &AgentKind) -> Result<(), String>
213:fn shared_quota_model_keys(shared_options: &Value) -> HashSet<String>
239:fn quota_model_is_shared(settings: &Settings, kind: &AgentKind, model: &str) -> bool
259:fn shared_model_options(
294:fn is_publishable_roaming_path(path: &str) -> bool
298:fn is_allowed_roaming_path(allowed: &[String], path: &str) -> bool
304:fn host_prompt_is_current(prompt_epoch: &(Arc<AtomicU64>, u64)) -> bool
311:pub struct Achievement
324:struct AchievementsResponse
331:pub struct Share
348:fn accepted_share_mode(recall: bool, default_mode: &str) -> Option<String>
355:struct InEnvelope
369:struct RelayClueList
374:struct RelayClueAssociate
378:pub struct RelayManager
437:impl RelayManager
438:    pub fn new(app: AppHandle, config_dir: PathBuf) -> Arc<Self>
519:    fn enqueue(&self, to: String, kind: &str, data: Value)
525:    fn cfg(&self) -> Option<(String, String, String)>
544:    fn groups_csv(&self) -> String
550:    pub fn enabled(&self) -> bool
554:    pub fn device_id(&self) -> &str
558:    pub fn status(&self) -> Value
565:    pub fn peers(&self) -> Value
570:    pub async fn list_achievements(&self) -> Result<Vec<Achievement>, String>
593:    pub fn inbox_list(&self) -> Vec<Share>
597:    fn emit_status(&self)
601:    fn clear_quota_leases(&self)
605:    fn invalidate_quota_leases_for_peer(&self, peer: &str)
612:    fn retain_online_quota_leases(&self, peers: &Value)
626:    fn retain_shared_quota_leases(&self, peer: &str, shared_options: &Value)
644:    fn set_connected(&self, on: bool)
655:    pub fn restart(self: &Arc<Self>)
668:    async fn run_loop(self: Arc<Self>)
704:    fn clear_v2(&self)
708:    async fn connect_once(&self, server: &str, token: &str, name: &str) -> Result<(), String>
772:    fn consume_v2_sse_events(&self, events: Vec<String>) -> Result<(), String>
779:    fn on_v2_frame(&self, frame: Value)
825:    fn dispatch(&self, env: InEnvelope)
880:    fn apply_alkaid_config(&self, data: &Value)
895:    fn on_clue_mentioned(&self, env: &InEnvelope)
921:    pub fn spawn_send(self: &Arc<Self>, to: String, kind: &str, data: Value)
927:    fn assign_out_seq(&self, kind: &str, data: &mut Value)
954:    async fn send_with_retry(&self, to: &str, kind: &str, data: Value)
974:    pub async fn send(&self, to: &str, kind: &str, data: Value) -> Result<Value, String>
979:    async fn wait_v2_ready(&self) -> Result<(), String>
998:    async fn send_once(
1009:    async fn send_v2(
1063:    pub async fn ledger_claim(
1104:    pub async fn ledger_set(
1138:    pub async fn ledger_remove(&self, scope: &str, key: &str) -> Result<(), String>
1162:    pub async fn ledger_list(&self, scope: &str) -> Result<Vec<Value>, String>
1186:    fn clue_request(
1203:    pub async fn clue_list(&self) -> Result<Vec<ClueNodeGroup>, String>
1212:    pub async fn clue_capture(
1241:    pub async fn clue_comment(
1263:    pub async fn clue_associate(
1282:    pub async fn clue_disassociate(
1301:    pub async fn clue_split(&self, card_id: &str) -> Result<ClueNodeGroup, String>
1313:    pub async fn clue_stack(&self, card_ids: &[String]) -> Result<ClueNodeGroup, String>
1325:    pub async fn clue_delete(&self, card_id: &str) -> Result<(), String>
1336:    pub async fn clue_context(&self, card_id: &str) -> Result<ClueContextSnapshot, String>
1347:    pub fn is_configured(&self) -> bool
1352:    pub fn publish_folders(&self)
1409:    pub fn share_thread(&self, thread_id: &str, to: &str) -> Result<(), String>
1414:    fn send_share(&self, thread_id: &str, to: &str, recall: bool) -> Result<(), String>
1445:    fn on_share(&self, env: &InEnvelope)
1467:    fn emit_inbox(&self)
1472:    pub fn accept_share(
1529:    pub fn decline_share(&self, id: &str)
1540:    pub fn advanced_share(
1666:    pub fn finish_advanced_if_any(self: &Arc<Self>, thread_id: &str) -> Option<String>
1672:    pub fn is_advanced(&self, thread_id: &str) -> bool
1678:    fn emit_quota_progress(&self, operation_id: &str, stage: &str, message: impl Into<String>)
1691:    pub async fn restore_quota_runtime(self: &Arc<Self>, thread_id: &str) -> Result<(), String>
1743:    pub async fn prepare_quota_lease(
1762:    fn quota_lease_ready(&self, key: &QuotaLeaseKey) -> bool
1766:    async fn wait_quota_lease_flight(
1781:    async fn acquire_quota_lease(
1825:    async fn request_quota_bundle(
1890:    pub async fn create_quota_thread(
1937:    async fn create_quota_thread_inner(
2059:    pub fn cancel_quota_roaming(&self, operation_id: &str) -> bool
2072:    fn spawn_quota_send(&self, to: String, kind: &str, data: Value)
2080:    fn on_quota_request(&self, env: &InEnvelope)
2135:    fn on_quota_granted(&self, env: &InEnvelope)
2161:    fn on_quota_rejected(&self, env: &InEnvelope)
2176:    pub async fn create_roaming_thread(
2260:    pub fn is_guest_running(&self, thread_id: &str) -> bool
2264:    fn set_guest_running(&self, thread_id: &str, running: bool)
2278:    pub fn guest_send_prompt(
2352:    pub fn guest_truncate(self: &Arc<Self>, thread_id: &str, item_id: u64) -> Result<(), String>
2373:    pub fn guest_cancel(self: &Arc<Self>, thread_id: &str) -> Result<(), String>
2407:    pub fn recall_roaming_thread(self: &Arc<Self>, thread_id: &str) -> Result<(), String>
2434:    pub fn guest_sync_config(self: &Arc<Self>, thread_id: &str)
2457:    pub fn request_peer_models(self: &Arc<Self>, peer_token: String)
2465:    pub fn notify_peer_models_changed(self: &Arc<Self>)
2486:    fn on_roaming_models_changed(&self, env: &InEnvelope)
2496:    fn on_roaming_models_request(&self, env: &InEnvelope)
2583:    fn on_roaming_models(&self, env: &InEnvelope)
2597:    pub fn request_peer_branches(self: &Arc<Self>, peer_token: String, folder: String)
2610:    fn on_roaming_branches_request(&self, env: &InEnvelope)
2635:    fn on_roaming_branches(&self, env: &InEnvelope)
2648:    pub fn guest_respond_permission(self: &Arc<Self>, request_key: &str, option_id: &str) -> bool
2664:    fn roaming_route(&self, thread_id: &str) -> Result<(String, String), String>
2675:    fn request_resync(&self, thread_id: &str)
2695:    fn resync_guest_threads(&self)
2719:    fn send_blocking(&self, to: &str, kind: &str, data: Value) -> Result<(), String>
2727:    fn on_roaming_create(&self, env: &InEnvelope)
2832:    pub fn respond_roam_request(
2896:    fn finish_roam_accept(self: &Arc<Self>, req_id: String, pending: PendingRoam)
2980:    fn notify_roam_request(&self, from_name: &str, folder_name: &str)
2984:    fn roaming_folder_allowed(&self, folder: &str) -> bool
2990:    fn on_roaming_truncate(&self, env: &InEnvelope)
3055:    fn on_roaming_prompt(&self, env: &InEnvelope)
3125:    fn run_roaming_prompt(&self, host_thread_id: String, text: String, images: Vec<PromptImage>)
3224:    fn on_roaming_cancel(&self, env: &InEnvelope)
3286:    fn on_roaming_recall(&self, env: &InEnvelope)
3344:    fn on_roaming_permission_response(&self, env: &InEnvelope)
3382:    fn on_roaming_config(&self, env: &InEnvelope)
3416:    fn on_roaming_resync(&self, env: &InEnvelope)
3432:    fn send_snapshot(&self, host_thread_id: &str, guest: &RoamGuest)
3469:    fn touch_guest_activity(&self, thread_id: &str)
3481:    fn resync_running_guests(&self)
3537:    pub fn forward_local_update(&self, thread_id: &str, op: &Value)
3555:    pub fn forward_local_title(&self, thread_id: &str)
3575:    fn on_roaming_title(&self, env: &InEnvelope)
3600:    pub fn forward_local_turn(&self, thread_id: &str, running: bool, stop_reason: &Value)
3618:    pub fn forward_local_permission(&self, thread_id: &str, payload: &Value)
3629:    pub fn forward_local_permission_resolved(&self, request_key: &str)
3641:    pub fn is_hosted(&self, thread_id: &str) -> bool
3647:    pub fn notify_host_thread_deleted(&self, thread_id: &str)
3666:    fn ensure_hosted(&self, host_thread_id: &str) -> bool
3697:    pub fn rebuild_hosted(&self)
3713:    fn spawn_send_now(&self, to: String, kind: &str, data: Value)
3719:    fn on_roaming_created(&self, env: &InEnvelope)
3796:    fn on_roaming_update(&self, env: &InEnvelope)
3874:    fn on_roaming_turn(&self, env: &InEnvelope)
3900:    fn on_roaming_permission(&self, env: &InEnvelope)
3915:    fn on_roaming_permission_resolved(&self, env: &InEnvelope)
3926:    fn on_roaming_snapshot(&self, env: &InEnvelope)
4001:    fn on_roaming_error(&self, env: &InEnvelope)
4023:    fn emit_update(&self, thread_id: &str, op: Value)
4039:    fn persist_seq(&self, force: bool)
4052:    fn log(&self, line: String)
4056:    pub fn log_line(&self, line: String)
4063:fn apply_op_to_thread(thread: &mut Thread, op: &Value)
4103:fn item_user_text(item: &Item) -> Option<&str>
4110:fn append_item_text(item: &mut Item, text: &str)
4120:fn derive_title(text: &str) -> String
4125:fn first_line(s: &str, n: usize) -> String
4136:fn maybe_decompress(data: &mut Value)
4153:pub(crate) fn gzip_json(value: &Value) -> Result<Vec<u8>, String>
4162:fn coalesce_outbound(batch: Vec<(String, String, Value)>) -> Vec<(String, String, Value)>
4190:pub(crate) fn build_transcript(items: &[Item]) -> String
4210:fn make_scratch_dir() -> Result<String, String>
4217:fn basename(p: &str) -> String
4226:pub fn normalize_groups_csv(raw: &str) -> String
4242:pub fn resolve_relay_server(raw: &str) -> String
4252:pub async fn probe_relay(server: &str, token: &str, groups: &str) -> Result<i64, String>
4295:async fn decode_relay_json<T: DeserializeOwned>(response: reqwest::Response) -> Result<T, String>
4309:fn relay_display_name(s: &Settings) -> String
4321:fn relay_token_username(token: &str) -> Option<&str>
4325:fn relay_sender_name(token: &str, fallback: &str) -> String
4329:fn relay_display_peers(mut peers: Value) -> Value
4346:pub(crate) fn urlencode(s: &str) -> String
4360:fn resolve_relay_asset_url(server: &str, url: Option<String>) -> Option<String>
4377:fn sse_url(server: &str, since: i64, server_epoch: &str) -> Result<String, String>
4388:fn relay_thread_id(data: &Value) -> &str
4395:fn read_relay_state(dir: &PathBuf) -> (i64, String)
4409:fn read_or_create_device_id(dir: &PathBuf) -> String
4423:fn write_relay_state(dir: &PathBuf, seq: i64, server_epoch: &str)
4431:fn read_inbox(dir: &PathBuf) -> Vec<Share>
4438:fn persist_inbox(dir: &PathBuf, inbox: &[Share])
4450:    fn v2_sse_url_keeps_cursor_and_server_epoch()
4462:    fn v2_http_gzip_round_trips()
4473:    fn relay_token_username_is_backward_compatible()
4484:    fn relay_peer_names_hide_random_token_suffixes()
4495:    fn recalled_share_uses_local_default_mode()
4502:    fn filters_peer_models_to_explicit_quota_shares()
4524:    fn quota_request_requires_current_exact_model_share()
4547:    fn quota_lease_key_is_per_peer_and_backend()
4557:    fn opencode_quota_lease_is_scoped_to_provider()
4572:    fn quota_runtime_supports_every_frontend_backend()
4586:    fn shared_quota_model_keys_follow_peer_payload()
4605:    async fn quota_operation_cancel_wakes_waiter_and_blocks_commit()
4619:    fn committed_quota_operation_rejects_late_cancel()
4626:    fn temporary_sessions_are_not_published_as_roaming_projects()
4634:    fn roaming_path_requires_whitelist_and_existing_directory()
4645:    fn roaming_cancel_invalidates_host_prompt_that_has_not_started()

### src-tauri/src/remote.rs
39:struct RemoteConfig
49:struct RemoteProject
56:struct RemoteThreadMeta
74:struct RemoteSnapshot
87:struct RemoteThreadDelta
108:struct ThreadCheckpoint
115:struct RemoteCommand
145:struct CommandResult
159:struct ServerResponse
175:struct RemoteTransport
182:impl RemoteTransport
183:    async fn sync(&self, body: Value) -> Result<ServerResponse, String>
212:    async fn pull(&self) -> Result<ServerResponse, String>
221:    fn start_command_sse(&self) -> tauri::async_runtime::JoinHandle<()>
236:pub(crate) fn publish_main_sse(value: Value)
245:fn start_transport(cfg: RemoteConfig) -> RemoteTransport
269:async fn remote_sse_loop(
288:async fn remote_sse_once(
340:fn parse_remote_response(value: Value) -> Result<ServerResponse, String>
360:fn remote_sync_url(server: &str) -> Result<String, String>
364:fn remote_events_url(server: &str) -> Result<String, String>
368:fn remote_http_url(server: &str, path: &str) -> Result<String, String>
377:fn deserialize_null_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
385:pub fn start(app: AppHandle)
391:async fn run(app: AppHandle)
751:fn spawn_pull(
765:async fn apply_pull_response(
800:fn error_backoff(error: &str) -> Duration
808:fn config(app: &AppHandle) -> Option<RemoteConfig>
850:fn remote_control_enabled(app: &AppHandle) -> bool
858:async fn pull(transport: &RemoteTransport) -> Result<ServerResponse, String>
862:async fn sync(transport: &RemoteTransport, value: &Value) -> Result<ServerResponse, String>
866:fn eligible(t: &Thread) -> bool
873:fn thread_metas(app: &AppHandle) -> Vec<RemoteThreadMeta>
897:fn projects(app: &AppHandle) -> Vec<RemoteProject>
922:fn models(app: &AppHandle) -> HashMap<String, Value>
967:fn model_signature_for(models: &HashMap<String, Value>) -> String
975:fn sync_threads(app: &AppHandle, requested: &HashSet<String>) -> HashMap<String, Thread>
987:fn thread_running(app: &AppHandle, thread: &Thread) -> bool
992:fn full_snapshot(app: &AppHandle, synced: &HashMap<String, Thread>) -> RemoteSnapshot
1007:fn threads_pack_with_metas(
1027:fn remote_permissions(app: &AppHandle) -> Vec<Value>
1039:fn catalog_signature_for(metas: &[RemoteThreadMeta]) -> String
1058:fn thread_changed(old: &Thread, old_running: bool, current: &Thread, running: bool) -> bool
1074:fn recent_thread_ids(app: &AppHandle, limit: usize) -> Vec<String>
1086:fn remote_thread_value(thread: &Thread) -> Value
1097:fn remote_item_value(item: &Item) -> Value
1103:fn thread_checkpoint(thread: &Thread) -> ThreadCheckpoint
1119:fn checkpoints_for(threads: &HashMap<String, Thread>) -> HashMap<String, ThreadCheckpoint>
1126:fn reconcile_response(
1146:fn response_confirms_thread(
1157:fn compact_remote_item(item: &mut Item)
1175:fn make_delta(previous: &Thread, current: &Thread, app: &AppHandle) -> Option<RemoteThreadDelta>
1218:fn remote_delta_updated_at(previous: &Thread, current: &Thread, running: bool) -> i64
1231:    fn fire_command_detection_matches_frontend()
1242:    fn fire_input_validation_requires_goal_and_single_target()
1252:    fn model_signature_does_not_depend_on_hashmap_order()
1266:    fn remote_tool_items_only_keep_summary()
1293:    fn internal_tool_output_does_not_count_as_remote_change()
1312:    fn checkpoint_covers_visible_text_and_plan()
1325:    fn remote_thread_snapshot_omits_clue_context()
1342:    fn server_response_accepts_null_collections()
1358:    fn remote_command_accepts_prompt_attachments()
1380:    fn running_delta_keeps_sort_timestamp_stable_until_completion()
1391:    fn remote_http_urls_use_dedicated_endpoints()
1403:    fn remote_response_accepts_http_and_sse_envelopes()
1419:    fn remote_file_path_normalizes_slash_prefixed_windows_drive()
1436:    fn remote_file_path_accepts_linux_file_uris_and_url_encoding()
1448:    fn equivalent_remote_cwds_match_after_canonicalization()
1461:async fn process_commands(
1490:async fn execute_command(app: &AppHandle, cmd: &RemoteCommand) -> CommandResult
1590:fn rename_remote_thread(app: &AppHandle, thread_id: &str, title: &str) -> Result<(), String>
1607:fn configure_remote_thread(app: &AppHandle, cmd: &RemoteCommand) -> Result<(), String>
1646:async fn respond_remote_permission(
1697:fn remote_file(app: &AppHandle, thread_id: &str, cwd: &str, path: &str) -> Result<Value, String>
1739:fn normalize_remote_file_path(path: &str) -> String
1759:fn remote_cwd_matches(thread_cwd: &str, command_cwd: &str) -> bool
1776:fn ensure_remote_git_cwd(app: &AppHandle, cwd: &str) -> Result<String, String>
1794:fn remote_git_status(app: &AppHandle, cwd: &str) -> Result<Value, String>
1855:fn looks_binary(sample: &[u8]) -> bool
1859:fn truncate_str(mut s: String, limit: usize) -> (String, bool)
1872:fn remote_git_file(app: &AppHandle, cwd: &str, path: &str) -> Result<Value, String>
1994:fn create_thread(app: &AppHandle, cmd: &RemoteCommand) -> Result<Thread, String>
2032:fn make_scratch_dir() -> Result<String, String>
2043:fn send_prompt(
2068:pub(crate) fn route_fire_command(
2111:fn is_fire_command(text: &str) -> bool
2119:fn is_target_only_command(text: &str) -> bool
2127:fn validate_fire_input(input: &str) -> Result<(), String>
2165:fn strip_fire_prefix(input: &str) -> &str
2183:async fn stop_thread(app: &AppHandle, thread_id: &str) -> Result<(), String>
2207:fn basename(path: &str) -> String

### src-tauri/src/sdk_adapters/alkaid.rs
6:pub struct AlkaidAdapter;
8:impl SdkAdapter for AlkaidAdapter
9:    fn agent_kind(&self) -> AgentKind
13:    fn label(&self) -> &'static str
17:    fn bridge(&self) -> (&'static str, &'static [u8])
24:    fn bridge_sidecars(&self) -> &'static [(&'static str, &'static [u8])]
33:    fn launch_config(&self, settings: &Settings) -> LaunchConfig
43:    fn permission_prefix(&self) -> &'static str
47:    fn generates_title(&self) -> bool
51:    fn supports_native_steer(&self) -> bool
55:    fn cancel_grace_attempts(&self) -> usize
59:    fn done_is_cancelled(&self, event: &Value) -> bool
63:    fn map_tool_call(&self, value: &Value) -> Option<ToolCall>
67:    fn normalize_usage(
101:fn alkaid_tool_call(value: &Value) -> ToolCall
195:fn tool_detail(tool: &str, arguments: Option<&Value>) -> Option<String>
207:fn argument_paths(arguments: Option<&Value>) -> Vec<Value>
227:fn result_text(result: &Value) -> Option<String>
238:fn text_content(text: &str) -> Vec<Value>
251:    fn tools_preserve_arguments_outputs_and_locations()

### src-tauri/src/sdk_adapters/claude.rs
6:pub struct ClaudeAdapter;
8:impl SdkAdapter for ClaudeAdapter
9:    fn agent_kind(&self) -> AgentKind
13:    fn label(&self) -> &'static str
17:    fn bridge(&self) -> (&'static str, &'static [u8])
24:    fn launch_config(&self, settings: &Settings) -> LaunchConfig
35:    fn permission_prefix(&self) -> &'static str
39:    fn normalize_usage(
49:fn normalize_claude_usage(usage: Option<&Value>) -> (Option<Value>, Option<CodexUsageSnapshot>)

### src-tauri/src/sdk_adapters/codebuddy.rs
6:pub struct CodeBuddyAdapter;
8:impl SdkAdapter for CodeBuddyAdapter
9:    fn agent_kind(&self) -> AgentKind
13:    fn label(&self) -> &'static str
17:    fn bridge(&self) -> (&'static str, &'static [u8])
24:    fn launch_config(&self, settings: &Settings) -> LaunchConfig
34:    fn permission_prefix(&self) -> &'static str
38:    fn generates_title(&self) -> bool
42:    fn normalize_usage(

### src-tauri/src/sdk_adapters/codex.rs
6:pub struct CodexAdapter;
8:impl SdkAdapter for CodexAdapter
9:    fn agent_kind(&self) -> AgentKind
13:    fn label(&self) -> &'static str
17:    fn bridge(&self) -> (&'static str, &'static [u8])
24:    fn launch_config(&self, settings: &Settings) -> LaunchConfig
34:    fn permission_prefix(&self) -> &'static str
38:    fn uses_codex_model_routing(&self) -> bool
42:    fn generates_title(&self) -> bool
46:    fn accepts_data_image(&self, _mime_type: &str) -> bool
50:    fn uses_text_deltas(&self) -> bool
54:    fn normalize_usage(

### src-tauri/src/sdk_adapters/cursor.rs
6:pub struct CursorAdapter;
8:impl SdkAdapter for CursorAdapter
9:    fn agent_kind(&self) -> AgentKind
13:    fn label(&self) -> &'static str
17:    fn bridge(&self) -> (&'static str, &'static [u8])
24:    fn launch_config(&self, settings: &Settings) -> LaunchConfig
43:    fn permission_prefix(&self) -> &'static str
47:    fn generates_title(&self) -> bool
51:    fn keeps_bridge_alive(&self) -> bool
55:    fn empty_model_options(&self) -> Value
67:    fn normalize_usage(

### src-tauri/src/sdk_adapters/mod.rs
17:pub struct LaunchConfig
25:pub trait SdkAdapter: Send + Sync
26:    fn agent_kind(&self) -> AgentKind;
27:    fn label(&self) -> &'static str;
28:    fn bridge(&self) -> (&'static str, &'static [u8]);
29:    fn launch_config(&self, settings: &Settings) -> LaunchConfig;
30:    fn permission_prefix(&self) -> &'static str;
33:    fn bridge_sidecars(&self) -> &'static [(&'static str, &'static [u8])]
37:    fn uses_codex_model_routing(&self) -> bool
41:    fn generates_title(&self) -> bool
45:    fn keeps_bridge_alive(&self) -> bool
49:    fn supports_native_steer(&self) -> bool
53:    fn accepts_data_image(&self, mime_type: &str) -> bool
57:    fn uses_text_deltas(&self) -> bool
61:    fn cancel_grace_attempts(&self) -> usize
65:    fn done_is_cancelled(&self, _event: &Value) -> bool
69:    fn map_tool_call(&self, _value: &Value) -> Option<ToolCall>
73:    fn empty_model_options(&self) -> Value
85:    fn normalize_usage(
93:fn canonical_usage(

### src-tauri/src/sdk_runtime.rs
23:fn is_codex_model_resume_warning(value: &Value) -> bool
38:struct RunningBridge
43:struct IdleBridge
51:enum ReadEventsOutcome
56:pub struct SdkManager
75:impl SdkManager
76:    pub fn new<A: SdkAdapter + 'static>(app: AppHandle, adapter: A) -> Arc<Self>
80:    pub fn new_with_env<A: SdkAdapter + 'static>(
104:    pub fn is_running(&self, thread_id: &str) -> bool
108:    pub fn has_pending_permission(&self, request_key: &str) -> bool
115:    pub async fn run_prompt(
384:    pub async fn cancel(&self, thread_id: &str)
428:    pub async fn steer_prompt(
444:    async fn native_steer_prompt(
498:    async fn interrupt_for_steer(&self, thread_id: &str)
534:    pub fn forget_session_of_thread(&self, thread_id: &str)
545:    pub async fn fork_session(
568:    pub fn shutdown(&self)
579:    pub fn seed_model_options(&self, value: Value)
587:    pub fn refresh_model_options_soon(self: &Arc<Self>)
602:    pub fn set_alkaid_server_config(self: &Arc<Self>, config: Option<Value>)
629:    fn with_alkaid_server_config(&self, mut request: Value) -> Value
639:    pub fn get_model_options(&self) -> Option<Value>
643:    pub fn spawn_revalidate_model_options(self: &Arc<Self>)
669:    pub async fn ensure_model_options(self: &Arc<Self>) -> Result<Value, String>
678:    fn empty_model_options(&self) -> Value
684:    fn pending_model_options(&self) -> Value
692:    async fn refresh_model_options(&self) -> Result<Value, String>
718:    pub fn generate_title_async(
769:    async fn run_bridge(&self, cwd: &str, request: Value) -> Result<Value, String>
792:    async fn run_prompt_native(
1196:    async fn run_prompt_bridge(
1323:    fn spawn_idle_bridge(&self, cwd: &str) -> Result<IdleBridge, String>
1362:    async fn read_events(
1432:    fn spawn_bridge(&self, cwd: &str) -> Result<Child, String>
1484:    fn save_session_id(&self, thread_id: &str, session_id: &str)
1505:    fn clear_session_id(&self, thread_id: &str)
1517:    fn save_checkpoint(&self, thread_id: &str, user_item_id: u64, event: &Value)
1536:    fn apply_item(&self, thread_id: &str, value: &Value, ids: &mut HashMap<String, u64>)
1649:    fn apply_plan(&self, thread_id: &str, plan: &Value)
1665:    fn emit_permission(&self, thread_id: &str, permission: &Value)
1692:    pub async fn respond_permission(
1718:    fn push_system(&self, thread_id: &str, text: String, level: &str)
1728:    fn set_running(&self, thread_id: &str, running: bool, stop_reason: Option<&str>)
1750:    fn finish_turn(&self, thread_id: &str, stop_reason: &str, usage: Option<Value>)
1784:    fn is_current_run(&self, thread_id: &str, run_epoch: u64) -> bool
1792:    fn finish_turn_if_current(
1804:    fn emit_update(&self, thread_id: &str, item: &Item) -> Result<(), tauri::Error>
1808:    fn emit_op(&self, thread_id: &str, op: Value) -> Result<(), tauri::Error>
1814:impl Drop for SdkManager
1815:    fn drop(&mut self)
1820:fn bridge_path(app: &AppHandle, adapter: &dyn SdkAdapter) -> Result<PathBuf, String>
1841:fn prompt_parts(adapter: &dyn SdkAdapter, text: &str, images: &[PromptImage]) -> Vec<Value>
1868:fn image_mime_from_path(path: &str) -> Option<&'static str>
1881:async fn write_line(
1893:fn kill_child(child: &mut Child)
1900:fn parse_bridge_output(output: &str, label: &str) -> Result<Value, String>
1919:fn normalize_title(output: &str, fallback: &str) -> String
1932:fn resolve_codex_model(
1975:fn split_codex_effort(value: &str) -> Option<(&str, &str)>
1981:fn derive_title(text: &str, has_images: bool) -> String
1999:fn complete_pending_tools(thread: &mut crate::threads::Thread) -> Vec<Item>
2014:enum TextSnapshotChange<'a>
2022:fn text_snapshot_change<'a>(previous: &Item, next: &'a Item) -> TextSnapshotChange<'a>
2042:fn codex_todo_plan(value: &Value) -> Option<Value>
2081:    fn title_fallback_uses_first_prompt_line_or_image()
2091:    fn codex_model_resolution_splits_combined_values()
2124:    fn codex_model_resume_warning_is_nonfatal()
2136:    fn codex_usage_is_the_delta_between_cumulative_snapshots()
2171:    fn codex_usage_without_a_matching_baseline_is_not_counted()
2189:    fn alkaid_usage_includes_pi_cached_input()
2211:    fn cursor_usage_maps_disjoint_bridge_usage_and_includes_cached_input()
2233:    fn claude_style_usage_includes_cached_input_and_rejects_partial_data()
2260:    fn parses_and_normalizes_title_response()
2271:    fn turn_completion_finishes_pending_sdk_tools()
2297:    fn codex_text_snapshots_become_deltas_when_they_only_append()
2330:    fn codex_todo_list_becomes_the_shared_plan_shape()
2354:    fn cursor_tools_show_the_specific_operation()
2391:    fn sdk_tools_preserve_available_details()
2433:fn compact_tool_detail(value: &str) -> String
2444:fn text_content(text: String) -> Vec<Value>
2452:fn display_file_change(kind: &str, path: &str) -> String
2462:fn mcp_result_text(result: &Value) -> Option<String>
2476:fn tool_call(value: &Value) -> ToolCall

### src-tauri/src/semantic.rs
18:fn base_of(endpoint: &str) -> String
25:pub async fn embed(
70:pub async fn probe(
81:pub async fn ollama_pull(
103:pub fn cosine(a: &[f32], b: &[f32]) -> f32
120:fn one_line(s: &str, max: usize) -> String
133:struct VectorsFile
139:pub struct VectorStore
147:impl VectorStore
148:    pub fn load(dir: &PathBuf) -> Self
161:    pub fn save(&self)
174:    pub fn get(&self, employee_id: &str, ts: i64) -> Option<&Vec<f32>>
180:    pub fn put(&mut self, employee_id: &str, ts: i64, v: Vec<f32>)
188:    pub fn set_model(&mut self, model: &str)
194:    pub fn clear_employee(&mut self, employee_id: &str)

### src-tauri/src/server.rs
31:struct ServerFile
75:pub fn is_headless() -> bool
79:pub fn data_dir_override() -> Option<PathBuf>
85:fn data_dir() -> PathBuf
96:fn server_file_path() -> PathBuf
100:fn reload_marker_path() -> PathBuf
104:pub fn reload_marker() -> String
108:fn notify_runtime() -> Result<(), String>
124:fn load_server_file() -> ServerFile
131:fn save_server_file(value: &ServerFile) -> Result<(), String>
148:pub fn maybe_run_management_command() -> Option<Result<(), String>>
172:fn extract_data_dir_arg(args: &mut Vec<String>)
190:fn manage_projects(args: &[String]) -> Result<(), String>
245:fn manage_config(args: &[String]) -> Result<(), String>
265:fn show_config(show_token: bool) -> Result<(), String>
314:fn parse_bool(value: &str) -> Result<bool, String>
322:fn set_config(key: &str, value: &str) -> Result<(), String>
376:fn validate_environment_name(name: &str) -> Result<(), String>
390:fn apply_server_environment()
396:pub fn sync_server_environment() -> bool
430:fn mask_secret(value: &str) -> String
447:pub fn configure_from_args() -> Result<bool, String>
501:pub fn apply_settings(settings: &mut Settings)
514:pub fn configured_projects() -> Vec<PathBuf>
526:pub fn replace_configured_projects(projects: &[String])
532:pub fn path_allowed(path: &str) -> bool
554:pub fn configured_name() -> Option<String>
564:pub fn configured_proxy() -> Option<String>
579:    fn configured_projects_uses_unit_separator()
589:    fn token_masking_does_not_expose_full_secret()
596:    fn boolean_config_values_are_human_friendly()
603:    fn validates_environment_variable_names()

### src-tauri/src/settings.rs
10:pub struct CursorModelContextRule
19:pub struct Settings
137:impl Default for Settings
138:    fn default() -> Self
211:    fn windows_shell_shim_is_disabled_by_default()
216:    fn legacy_title_model_migrates_to_lightweight_model()
225:    fn checkpoint_file_restore_is_disabled_by_default()
230:    fn cursor_model_context_rules_round_trip()
241:    fn context_modes_default_and_round_trip()
253:    fn only_alkaid_is_enabled_by_default()
265:    fn remote_control_is_disabled_by_default()
272:    fn model_favorites_survive_reload()
286:    fn quota_shared_models_survive_reload_for_all_backends()
307:    fn missing_history_display_mode_defaults_to_project()
313:    fn missing_chat_view_render_defaults_to_canvas()
319:    fn sdk_integration_defaults_match_backend_policy()
329:    fn load_forces_persisted_integrations_to_sdk()
355:impl Settings
356:    pub fn load(dir: &PathBuf) -> Self
403:    pub fn save(&self, dir: &PathBuf)

### src-tauri/src/signature.rs
7:pub struct SignatureIdentity
23:pub fn identity_for_token(token: &str) -> Option<SignatureIdentity>
41:    fn matches_nie_youlin_case_insensitively_and_preserves_token_case()

### src-tauri/src/skills.rs
11:pub struct SkillInfo
17:pub fn skills_dir(config_dir: &Path) -> PathBuf
21:pub fn ensure_skills_dir(config_dir: &Path) -> PathBuf
27:fn user_home_dir() -> Option<PathBuf>
42:fn clean_frontmatter_value(value: &str) -> String
57:fn frontmatter_value(contents: &str, key: &str) -> Option<String>
79:fn read_skill_meta(skill_md: &Path) -> (String, String)
93:fn is_skill_dir(dir: &Path) -> bool
98:pub fn backend_skill_roots() -> Vec<PathBuf>
116:fn paths_equal(a: &Path, b: &Path) -> bool
130:fn resolve_link_target(link: &Path) -> Option<PathBuf>
139:fn is_managed_link(path: &Path, expected: &Path) -> bool
146:fn is_any_symlink(path: &Path) -> bool
153:fn link_dir(original: &Path, link: &Path) -> io::Result<()>
158:fn link_dir(original: &Path, link: &Path) -> io::Result<()>
184:fn remove_path_quiet(path: &Path)
196:fn sanitize_skill_name(name: &str) -> String
214:fn copy_dir_all(src: &Path, dst: &Path) -> io::Result<()>
232:pub fn copy_skills_to_runtime(config_dir: &Path, destination: &Path) -> Result<(), String>
249:fn find_skill_md(dir: &Path, depth: usize) -> Option<PathBuf>
272:fn install_from_skill_root(config_dir: &Path, skill_root: &Path) -> Result<SkillInfo, String>
308:pub fn list_skills(config_dir: &Path) -> Vec<SkillInfo>
340:pub fn install_skill_path(config_dir: &Path, path: &Path) -> Result<SkillInfo, String>
366:pub fn install_skill_zip(config_dir: &Path, zip_path: &Path) -> Result<SkillInfo, String>
417:pub fn remove_skill(config_dir: &Path, name: &str) -> Result<(), String>
439:pub fn sync_skills_from_home()
450:pub fn sync_skills_to_backends(config_dir: &Path) -> Result<(), String>
526:    fn copies_local_skills_into_isolated_runtime()

### src-tauri/src/sleep_inhibitor.rs
9:pub struct SleepInhibitor
13:struct Inner
18:impl SleepInhibitor
19:    pub fn new() -> Self
57:    pub fn set_running(&self, thread_id: &str, running: bool)

### src-tauri/src/sys_notify.rs
8:fn prompt_queued_threads() -> &'static Mutex<HashSet<String>>
14:pub fn set_prompt_queue_pending(thread_id: &str, pending: bool)
24:pub fn focus_main_window(app: &AppHandle)
36:fn main_window_focused(app: &AppHandle) -> bool
45:pub fn show(
96:pub fn notify_thread_done(app: &AppHandle, thread_id: &str, title: &str, body: &str, event: &str)
105:pub fn notify_thread_done_unfiltered(
128:pub fn notify_decision(app: &AppHandle, emp_name: &str, question: &str, event: &str)
144:pub fn notify_roam_request(app: &AppHandle, from_name: &str, folder_name: &str)
163:pub fn notify_clue_mention(
199:fn escape_applescript(s: &str) -> String

### src-tauri/src/threads.rs
13:pub fn now_ms() -> i64
18:pub fn session_cleanup_is_expired(timestamp: i64, now: i64, hours: u32) -> bool
53:pub enum AgentKind
66:impl Default for AgentKind
67:    fn default() -> Self
72:impl AgentKind
73:    pub fn as_str(&self) -> &'static str
89:    pub fn from_str(s: &str) -> Option<AgentKind>
106:    pub fn label(&self) -> &'static str
124:pub struct ToolCall
142:fn default_kind() -> String
145:fn default_status() -> String
149:fn raw_value_text(value: &Value) -> Option<String>
165:fn same_output_text(value: &Value, display: &str) -> bool
171:fn strip_duplicate_raw_output(value: Value, display: &str) -> Option<Value>
214:fn deduplicate_tool_output(call: &mut ToolCall)
224:fn deduplicate_thread_outputs(thread: &mut Thread)
235:pub struct PromptImage
253:pub fn embed_attachment_data(images: &mut [PromptImage])
283:pub fn save_attachment_to_temp(img: &PromptImage) -> Option<String>
299:fn sanitize_filename(name: &str) -> String
313:pub fn embed_items_attachments(items: &mut [Item])
322:pub fn file_uri_to_local_path(uri: &str) -> Option<String>
335:pub(crate) fn percent_decode(s: &str) -> String
361:pub enum Item
414:impl Item
415:    pub fn id(&self) -> u64
432:pub struct Worktree
444:pub struct ProviderCheckpoint
453:pub struct PendingNativeRestore
460:pub struct CodexUsageSnapshot
473:pub struct Thread
564:impl Thread
565:    pub fn cached_auto_model(&self, selection: &str) -> Option<String>
571:    pub fn clear_auto_route(&mut self)
577:    pub fn new(
624:    pub fn is_roaming_guest(&self) -> bool
628:    pub fn is_quota_borrowed(&self) -> bool
632:    pub fn next_item_id(&self) -> u64
639:    pub fn next_local_item_id(&self) -> u64
650:    pub fn push_user(&mut self, text: String, images: Vec<PromptImage>) -> Item
662:    pub fn record_provider_checkpoint(
679:    pub fn checkpoint_before(&self, item_id: u64) -> Option<ProviderCheckpoint>
690:    pub fn push_user_local(&mut self, text: String, images: Vec<PromptImage>) -> Item
702:    pub fn push_system(&mut self, text: String, level: &str) -> Item
715:    pub fn push_system_local(&mut self, text: String, level: &str) -> Item
727:    pub fn push_turn(
767:    pub fn take_handoff_context(&mut self, to_label: &str) -> Option<String>
775:    pub fn take_prompt_context(&mut self, to_label: &str) -> Option<String>
801:pub struct ThreadMeta
838:struct ProjectsFile
842:pub struct ProjectStore
847:impl ProjectStore
848:    pub fn load(dir: &PathBuf) -> Self
858:    pub fn save(&self)
871:    pub fn touch(&mut self, cwd: &str)
878:    pub fn remove(&mut self, cwd: &str)
886:struct RoamingFile
890:pub struct RoamingStore
895:impl RoamingStore
896:    pub fn load(dir: &PathBuf) -> Self
906:    pub fn save(&self)
918:    pub fn is_allowed(&self, cwd: &str) -> bool
923:    pub fn toggle(&mut self, cwd: &str) -> bool
935:    pub fn set(&mut self, cwd: &str, allowed: bool)
951:pub struct WorktreeRecord
972:fn default_true() -> bool
977:struct WorktreeFile
981:pub struct WorktreeStore
986:impl WorktreeStore
987:    pub fn load(dir: &PathBuf) -> Self
997:    pub fn save(&self)
1009:    pub fn add(&mut self, rec: WorktreeRecord)
1014:    pub fn get(&self, id: &str) -> Option<&WorktreeRecord>
1018:    pub fn remove(&mut self, id: &str) -> Option<WorktreeRecord>
1025:    pub fn list(&self) -> Vec<WorktreeRecord>
1031:struct StoreFile
1036:struct ThreadTrashFile
1042:pub struct TrashedThread
1048:pub struct ThreadTrashStore
1053:impl ThreadTrashStore
1054:    pub fn load(dir: &PathBuf) -> Self
1064:    fn save(&self) -> Result<(), String>
1078:    pub fn move_to_trash(&mut self, threads: Vec<Thread>, trashed_at: i64) -> Result<(), String>
1093:    pub fn purge_expired(&mut self, now: i64, hours: u32) -> Vec<Thread>
1106:pub struct ThreadStore
1115:impl ThreadStore
1116:    pub fn load(data_dir: PathBuf) -> Self
1169:    fn load_split_threads(dir: &Path) -> Vec<Thread>
1192:    fn thread_file_name(id: &str) -> String
1208:    fn serialize_threads(
1224:    pub fn purge_ephemeral(&mut self) -> Vec<Thread>
1237:    pub fn save(&self)
1243:    pub fn take_dirty(&self) -> bool
1248:    pub fn save_notify_handle(&self) -> Arc<Notify>
1253:    pub fn serialize_files(&mut self) -> Option<Vec<(PathBuf, String)>>
1257:    pub fn directory_path(&self) -> PathBuf
1262:    pub fn write_files(dir: &Path, files: &[(PathBuf, String)]) -> Result<(), String>
1291:    pub fn save_now(&mut self)
1300:    pub fn get(&self, id: &str) -> Option<&Thread>
1304:    pub fn get_mut(&mut self, id: &str) -> Option<&mut Thread>
1308:    pub fn clear_active_clue_card(&mut self, card_id: &str) -> bool
1319:    pub fn thread_by_session_mut(&mut self, session_id: &str) -> Option<&mut Thread>
1338:pub fn render_handoff_context(
1468:fn render_handoff_plan(plan: Option<&Value>) -> String
1493:fn render_handoff_tool(call: &ToolCall) -> String
1533:    fn temp_thread_store_dir() -> PathBuf
1541:    fn legacy_threads_json_is_migrated_to_one_file_per_thread()
1590:    fn split_thread_store_removes_deleted_thread_files()
1635:    fn duplicate_tool_output_is_removed_from_raw_output()
1665:    fn duplicate_command_output_keeps_structured_metadata()
1689:    fn distinct_raw_output_is_preserved()
1713:    fn auto_route_cache_is_reused_and_written_to_turns()
1742:    fn handoff_to_opencode_preserves_history_once()
1773:    fn handoff_uses_two_stage_context_compaction()
1823:    fn handoff_keeps_reasoning_from_an_interrupted_turn()
1860:    fn handoff_budget_prioritizes_an_interrupted_tool_trajectory()
1878:    fn edited_opencode_prompt_replays_retained_history_once()
1909:    fn checkpoint_before_uses_latest_position_from_current_backend()
1936:    fn recording_checkpoint_replaces_same_turn_position()
1954:    fn legacy_thread_defaults_new_persistence_fields()
1972:fn tool_output_text(content: &[Value]) -> String
1994:fn truncate_middle(s: &str, max: usize) -> String
2010:fn clip_blocks_to_budget(

### src-tauri/src/time_machine.rs
24:pub struct PatchEntry
33:struct Checkpoint
48:struct Timeline
62:struct StoreFile
69:impl Default for StoreFile
70:    fn default() -> Self
78:fn store_version() -> u32
84:pub struct PromptSummary
91:pub struct CheckpointSummary
104:pub struct TimelineView
113:pub struct RestoreResult
118:fn time_machine_dir(data_dir: &Path) -> PathBuf
122:fn store_path(data_dir: &Path) -> PathBuf
126:fn load_store(data_dir: &Path) -> Result<StoreFile, String>
140:fn save_store(data_dir: &Path, store: &StoreFile) -> Result<(), String>
169:fn object_path(data_dir: &Path, hash: &str) -> PathBuf
177:fn put_blob(data_dir: &Path, bytes: &[u8]) -> Result<String, String>
197:fn get_blob(data_dir: &Path, hash: &str) -> Result<Vec<u8>, String>
201:fn workspace_root(cwd: &Path) -> Result<PathBuf, String>
210:fn safe_relative(path: &str) -> Result<PathBuf, String>
224:fn executable(metadata: &fs::Metadata) -> bool
230:fn executable(_metadata: &fs::Metadata) -> bool
234:fn directory_files(root: &Path) -> Result<Vec<String>, String>
235:    fn visit(root: &Path, directory: &Path, paths: &mut Vec<String>) -> Result<(), String>
277:fn capture_manifest(data_dir: &Path, root: &Path) -> Result<Vec<PatchEntry>, String>
294:fn view_for(timeline: &Timeline, thread_id: &str) -> TimelineView
331:fn timeline_index(store: &StoreFile, thread_id: &str) -> Option<usize>
342:fn latest_prompt_title(thread: &Thread) -> String
354:fn append_checkpoint(
404:pub fn create_checkpoint(data_dir: &Path, thread: &Thread) -> Result<TimelineView, String>
425:pub fn get_timeline(data_dir: &Path, thread_id: &str) -> Result<Option<TimelineView>, String>
430:pub fn checkpoint_preview(
445:fn prompt_signature(item: &Item) -> Option<PromptSummary>
457:pub fn remove_prompt_turns(thread: &mut Thread, prompts: &[PromptSummary]) -> usize
480:fn checkpoint_prompt_path(checkpoint: &Checkpoint) -> Vec<PromptSummary>
491:pub fn rewrite_after_context_edit(
554:pub fn record_edit_fork(
605:fn set_executable(path: &Path, value: bool) -> Result<(), String>
623:fn remove_path(path: &Path) -> Result<(), String>
634:fn write_blob(
655:fn restore_manifest(data_dir: &Path, checkpoint: &Checkpoint) -> Result<(), String>
715:pub fn restore_checkpoint(
811:    fn rejects_paths_that_escape_repository()
818:    fn removes_complete_prompt_turns_and_keeps_neighboring_context()
854:    fn old_worldline_store_is_discarded_without_migration()
874:    fn managed_snapshot_ignores_generated_directories()
891:    fn snapshots_and_restores_a_directory_without_git()
929:    fn roaming_guest_timeline_does_not_require_local_workspace()
962:    fn crossing_timeline_without_checkpoint_restore_keeps_workspace_files()
993:    fn editing_a_prompt_creates_a_named_branch_tree()
1033:    fn jumping_to_the_current_branch_point_preserves_workspace_as_a_child()
1073:    fn restores_files_and_forks_from_the_selected_checkpoint()

### src-tauri/src/updater.rs
27:fn github_repo() -> &'static str
31:fn github_api_latest() -> String
38:fn asset_name_for(version: &str) -> String
45:fn update_channel() -> &'static str
75:fn compiled_app_version() -> String
83:pub fn run_server_update(check_only: bool) -> Result<(), String>
88:async fn run_server_update_async(check_only: bool) -> Result<(), String>
225:struct StagedMarker
233:struct WindowState
248:fn default_true() -> bool
252:impl WindowState
255:    fn geometry_valid(&self) -> bool
263:struct RestoreMarker
270:fn parse_ver(s: &str) -> Option<(u64, u64, u64)>
278:fn marker_path(app: &AppHandle) -> Option<PathBuf>
282:fn read_marker(app: &AppHandle) -> Option<StagedMarker>
288:fn write_marker(app: &AppHandle, marker: &StagedMarker)
299:fn remove_file_if_exists(path: &Path)
304:fn read_u16(bytes: &[u8], offset: usize) -> Option<u16>
311:fn read_u32(bytes: &[u8], offset: usize) -> Option<u32>
318:fn expected_pe_machine() -> Option<u16>
338:fn pe_machine_name(machine: u16) -> &'static str
348:fn validate_pe_image(bytes: &[u8]) -> Result<(), String>
421:fn expected_mach_o_cputype() -> Option<u32>
437:fn validate_mach_o_image(bytes: &[u8]) -> Result<(), String>
477:fn validate_staged_exe(path: &Path) -> Result<(), String>
508:fn validate_elf_image(bytes: &[u8], expected_machine: u16) -> Result<(), String>
533:    fn validates_elf_header_and_architecture()
546:fn restore_path(app: &AppHandle) -> Option<PathBuf>
550:fn write_restore(app: &AppHandle, marker: &RestoreMarker)
563:fn capture_window_state(app: &AppHandle) -> Option<WindowState>
587:pub fn write_restore_state(app: &AppHandle, thread_id: Option<&str>)
599:pub fn take_restore_thread(app: &AppHandle) -> Option<String>
615:fn main_window(app: &AppHandle) -> Option<tauri::WebviewWindow>
621:fn take_restore_window(app: &AppHandle) -> Option<WindowState>
637:fn apply_window_state(win: &tauri::WebviewWindow, ws: &WindowState)
668:pub fn restore_window_on_launch(app: &AppHandle)
682:fn valid_staged(app: &AppHandle) -> Option<StagedMarker>
701:pub fn staged_upgrade_version(app: &AppHandle) -> Option<String>
710:fn current_exe_name() -> String
728:fn update_http_client(
743:pub async fn check(app: &AppHandle) -> Result<Value, String>
810:fn find_exe(dir: &Path, name: &str) -> Option<PathBuf>
842:fn copy_extras(from: &Path, to: &Path, skip: &Path)
867:fn ensure_install_dir_writable(target: &Path) -> Result<(), String>
879:fn arg_value(args: &[String], key: &str) -> Option<String>
894:fn restore_old_exe(target: &Path, old: &Path)
899:fn spawn_installed_app(target: &Path) -> Result<(), String>
920:fn restart_after_failed_update(args: &[String]) -> Result<(), String>
931:fn apply_update_from_helper_args(args: &[String]) -> Result<(), String>
1009:pub fn maybe_run_apply_helper() -> bool
1027:fn spawn_update_helper(
1062:pub async fn download_and_stage(app: AppHandle) -> Result<Value, String>
1173:fn install_headless_in_place(
1224:pub async fn apply_staged(app: AppHandle) -> Result<(), String>
1309:pub fn cleanup_old()

### src-tauri/src/vega_native.rs
32:pub struct NativeTurnConfig
47:pub struct NativeTurnOutput
61:pub fn run_native_turn(
103:fn error_turn(error: &str, provider: &ProviderConfig) -> StreamTurn
131:pub async fn run_native_turn_async(
204:pub fn resolve_provider_config(
227:pub async fn run_summary_turn_async(
274:pub fn load_alkaid_config(data_dir: &Path, server_config: Option<&Value>) -> Result<Value, String>
295:fn now_millis() -> u64
305:pub fn native_tool_definitions(read_only: bool) -> Vec<Value>
493:pub struct NativeVegaSetup
502:fn load_agent_instructions(path: &Path) -> String
513:fn detect_shell_config() -> ShellConfig
534:fn find_windows_powershell() -> Option<String>
557:pub fn prepare_native_turn(

### src-tauri/src/vega_provider.rs
30:pub struct ProviderConfig
41:pub async fn stream_turn(
69:async fn stream_openai_chat(
138:async fn stream_anthropic(
204:async fn stream_openai_responses(
268:async fn stream_google(

### src-tauri/src/vega_reasonix.rs
25:pub fn is_valid_session_id(session_id: &str) -> bool
32:fn sessions_root(data_dir: &Path) -> PathBuf
37:pub fn slim_memory_path(data_dir: &Path, session_id: &str) -> Result<PathBuf, String>
45:pub fn legacy_messages_path(data_dir: &Path, session_id: &str) -> Result<PathBuf, String>
53:pub fn stable_hash(value: &Value) -> String
64:pub fn messages_with_pending_prompt(
82:pub fn message_with_slim_memory(text: &str, memory: &SlimMemory) -> String
90:pub fn load_slim_memory(data_dir: &Path, session_id: &str) -> SlimMemory
110:pub fn load_legacy_messages(data_dir: &Path, session_id: &str) -> Vec<Value>
121:pub fn save_slim_memory(data_dir: &Path, session_id: &str, memory: &SlimMemory) -> Result<(), String>
138:pub fn save_legacy_messages(data_dir: &Path, session_id: &str, messages: &[Value]) -> Result<(), String>
155:    fn validates_session_ids()
164:    fn stable_hash_is_16_hex_chars_and_deterministic()
174:    fn builds_pending_checkpoint()
187:    fn message_with_slim_memory_prefixes_record()
203:    fn round_trips_slim_memory_through_disk()

### src-tauri/src/windows_shell_shim.rs
30:struct ShellShim
42:fn system32() -> PathBuf
50:fn real_cmd() -> PathBuf
59:pub(crate) fn real_powershell() -> PathBuf
71:fn find_executable_on_path(name: &str, path: &std::ffi::OsStr) -> Option<PathBuf>
77:fn find_bash_on_path(path: &std::ffi::OsStr) -> Option<PathBuf>
88:fn real_bash(launch_env: &HashMap<String, String>) -> Option<PathBuf>
111:fn content_key() -> String
118:fn write_helper(dir: &Path) -> Result<PathBuf, String>
133:fn ensure_alias(helper: &Path, dir: &Path, name: &str) -> Result<(), String>
148:fn cursor_compatible_bash_shim_dir(shim_dir: &Path) -> PathBuf
152:fn init(app: &AppHandle, launch_env: &HashMap<String, String>) -> Result<ShellShim, String>
187:pub(crate) fn apply(
242:    fn finds_git_bash_from_path_before_fixed_install_locations()
259:    fn embedded_helper_is_windows_executable()
265:    fn helper_aliases_share_one_payload()
284:    fn matches_cursor_git_bash_filter(path: &str) -> bool
294:    fn cursor_compatible_bash_shim_path_matches_git_bash_filter()
315:    fn installs_cursor_compatible_bash_alias_under_git_bin()

### src-tauri/windows-shell-shim.rs
23:type Handle = *mut c_void;
26:struct StartupInfoW
48:struct ProcessInformation
57:    fn GetCommandLineW() -> *const u16;
58:    fn GetConsoleCP() -> u32;
59:    fn GetStdHandle(handle: u32) -> Handle;
60:    fn CreateProcessW(
72:    fn WaitForSingleObject(handle: Handle, milliseconds: u32) -> u32;
73:    fn GetExitCodeProcess(process: Handle, exit_code: *mut u32) -> i32;
74:    fn CloseHandle(handle: Handle) -> i32;
77:fn is_command_line_space(ch: u16) -> bool
81:fn command_line_tail() -> OsString
114:fn real_shell_env() -> Option<&'static str>
130:fn quote_program(program: &std::ffi::OsStr) -> Vec<u16>
137:fn run_hidden(real_shell: &std::ffi::OsStr, tail: &std::ffi::OsStr) -> Option<u32>
188:fn main()

## TS/TSX exports

### scripts/alkaid-bridge-common.mjs
7:export function send(value)
11:export const dataRoot
14:export function sessionPath(sessionId)
19:export async function mcpServers()
26:export async function loadMessages(sessionId)
31:export async function saveJson(path, value)
43:export function saveMessages(sessionId, messages)
47:export function startedToolItem(event)
102:export async function runAlkaidBridge(handlePrompt)

### scripts/alkaid-config.mjs
5:export function alkaidDataRoot(home
65:export function parseJsonc(text)
93:export function mergeAlkaidConfig(serverConfig, localConfig)
105:export async function loadAlkaidConfig(
123:export function defaultAlkaidModel(config)
141:export function mergeAlkaidCompatDefaults(api, modelId, baseUrl, existing
184:export function resolveAlkaidModel(config, selection
235:export function alkaidModelOptions(config)

### scripts/alkaid-context-reasonix.mjs
562:export function runContextBridge()

### scripts/alkaid-context-super-memory.mjs
1:export const VEGA_SLIM_MEMORY_TURNS
3:export function createSlimMemory()
24:export function appendSlimTurn(memory, userPrompt)
30:export function setLatestConclusion(memory, content)
46:export function normalizeSlimMemory(memory)
66:export function memoryWithoutCurrent(memory,
82:export function formatSlimMemory(memory)
95:export async function compactSlimMemory(
135:export function stripCompletedOpenAIReasoning(messages)
156:export function estimateContextTokens(text)
169:export function contextTokensFromMessages(messages)
184:export function shouldUseFullContext(memory, maxContextTokens, maxContextChars
198:export function seedSlimMemoryFromMessages(memory, messages)

### scripts/alkaid-context-super.mjs
426:export function runContextBridge()

### scripts/alkaid-core.mjs
25:export const TOOL_OUTPUT_CONTEXT_MAX_BYTES
27:export const OPENAI_TOOL_OUTPUT_MAX_CHARS
29:export const OPENAI_TOOL_OUTPUT_SAFE_MAX_CHARS
31:export const ALKAID_PROVIDER_IDLE_TIMEOUT_MS
33:export const ALKAID_PROVIDER_IDLE_TIMEOUT_ENABLED
50:export function clampToolOutputText(text, maxChars
82:export async function governToolResult(result, options
137:export function clampOpenAIPayloadToolOutputs(payload, maxChars
193:export function alkaidSkillsRoot(home
197:export async function alkaidPromptInput(parts
217:export function messagesWithPendingAlkaidPrompt(messages, input, timestamp
231:export async function alkaidUserMessage(parts
236:export class AlkaidProviderIdleTimeoutError extends Error
243:export function isRetryableAlkaidProviderError(error)
269:export function restoreAlkaidSteeringForRetry(agent, steeringMessages
284:export function createAlkaidIdleTimeout(options
340:export function mergeAlkaidUsage(total, usage)
349:export async function runAlkaidPromptWithRetry(agent, input, images, options
443:export function decodeTextBuffer(buffer)
505:export function createFilesystemTools(cwd, editTool
599:export function loadAlkaidSkills(root
612:export async function expandAlkaidSkillCommand(text, skills)
649:export function formatAlkaidSkillsPrompt(skills)
658:export function optimizeAlkaidSystemPrompt(stableParts, dynamicParts)
666:export async function loadAlkaidAgentInstructions(path
673:export function buildAlkaidSystemPrompt(options
713:export function clampPromptCacheKey(key)
721:export function injectOpenAIPromptCacheKey(payload, sessionId)
739:export function findWindowsPowerShell(env
755:export function detectAlkaidShellConfig(env
761:export function resolveAlkaidShellConfig(shellConfig, env
778:export async function connectMcpServers(servers
809:export async function createAlkaidAgent(options

### scripts/alkaid-diagnostics.mjs
4:export const ALKAID_PROVIDER_DIAGNOSTIC_LOG
10:export function createAlkaidDiagnosticLog(root)
36:export function alkaidDiagnosticEndpoint(baseUrl)

### scripts/alkaid-slim-memory.mjs
1:export function createSlimMemory()
32:export function appendSlimTurn(memory, userPrompt)
38:export function setLatestConclusion(memory, content)
54:export function normalizeSlimMemory(memory)
81:export function memoryWithoutCurrent(memory,
98:export function formatSlimMemory(memory)
120:export async function compactSlimMemory(
177:export function stripCompletedOpenAIReasoning(messages)
198:export function estimateContextTokens(text)
211:export function contextTokensFromMessages(messages)
229:export function contextPressureTier(currentTokens, contextWindow)
254:export function compactNativeToolResults(messages, tier,
275:export function shouldUseFullContext(memory, maxContextTokens, maxContextChars
290:export function rebaseNativeContextForSlimMemory(messages, activeTurnStart, memory)
316:export function seedSlimMemoryFromMessages(memory, messages)

### scripts/alkaid-smart-edit.mjs
235:export function applySmartEdits(content, edits, path)

### scripts/cursor-bridge-common.mjs
13:export function cursorShellProgram(program, env
34:export function createMessageState()
45:export function appendText(state, runId, type, text)
62:export function isEditFilesTool(name)
67:export function isMcpEnvelope(value)
72:export function mapTool(state, callId, name, status, args, result)
113:export function mapMessage(message, state)
136:export function mapDelta(update, state, runId)
160:export function completePendingTools(state)
176:export function cursorTodoPlan(toolCall)
188:export function modelSelection(selected)
213:export function encodeModelVariant(model, variant)
230:export function cursorModelOptions(models)
244:export async function modelOptions()

### scripts/cursor-filesystem-tools.mjs
99:export function createCursorFilesystemTools(cwd, options
246:export function cursorBatchToolPolicy(options
270:export function cursorCavemanPolicy()
275:export function cursorPromptPrefix(options

### src/App.tsx
36:export default function App()

### src/components/AchievementBadge.tsx
4:export function AchievementBadge(props:

### src/components/AchievementsModal.tsx
6:export function AchievementsModal(props:

### src/components/CanvasTranscript.tsx
12:export interface CanvasTranscriptHandle
685:export function CanvasTranscript(props: CanvasTranscriptProps)

### src/components/ChatView.tsx
218:export function ChatView()

### src/components/CliOperationModal.tsx
6:export function CliOperationModal()

### src/components/ClueCaptureModal.tsx
17:export function ClueCaptureModal(props:

### src/components/Composer.tsx
45:export function Composer()

### src/components/ConfigSelects.tsx
17:export type ModelOptionsSource
19:export interface QuotaModelPeer
24:export interface SharedModelSource
135:export function modelOptionsOf(
169:export function groupedModelOptions(
190:export function ModelPicker(props:
306:export function ConfigSelects(props:

### src/components/DecisionWorkbench.tsx
147:export function DecisionWorkbench()

### src/components/EditedFilesCard.tsx
46:export function relPath(p: string): string
56:export function EditedFilesCard(props:

### src/components/EmployeesView.tsx
224:export function EmployeesView()

### src/components/EngravedNumberMark.tsx
14:export function EngravedNumberMark(props: EngravedNumberMarkProps)

### src/components/EvidenceChainView.tsx
189:export function EvidenceChainView()

### src/components/ExclusiveChatMark.tsx
15:export interface ExclusiveChatIdentity
20:export function exclusiveIdentityForToken(token: string): ExclusiveChatIdentity | undefined
30:export function exclusiveNumberForToken(token: string): string | undefined
34:export function ExclusiveChatMark(props:

### src/components/FileContextMenu.tsx
13:export function absolutePath(path: string)
18:export function createFileContextMenu()

### src/components/HomeView.tsx
48:export function HomeView()

### src/components/ImageAttachmentStrip.tsx
77:export function fileUriPath(uri: string)
82:export function attachmentPreviewSrc(image: PromptImage)
89:export function createImageAttachments(
168:export function ImageAttachmentStrip(props:

### src/components/Markdown.tsx
207:export function Markdown(props:

### src/components/MentionPicker.tsx
5:export function MentionPicker(props:

### src/components/NoteFlow.tsx
14:export type NoteFlow
19:export function createNoteFlow(running?: ()

### src/components/PermissionCard.tsx
29:export function PermissionCard(props:

### src/components/PlanActionCard.tsx
5:export function PlanActionCard()

### src/components/PlanCard.tsx
5:export function PlanCard(props:

### src/components/ProjectPicker.tsx
14:export function projectDisplayName(p: ProjectEntry): string
19:export function ProjectPicker(props:

### src/components/RoamRequestModal.tsx
12:export function RoamRequestModal()

### src/components/SearchSelect.tsx
9:export interface SelectOption
35:export function SearchSelect(props:

### src/components/SettingsModal.tsx
153:export function SettingsModal(props:

### src/components/ShareInboxModal.tsx
28:export function ShareInboxModal(props:

### src/components/ShareModal.tsx
13:export function ShareModal(props:

### src/components/Sidebar.tsx
62:export function Sidebar(props:

### src/components/SignatureSplash.tsx
21:export function SignatureSplash()

### src/components/ToolCallCard.tsx
232:export function ToolCallCard(props:

### src/components/TranscriptItem.tsx
148:export function TranscriptItem(props:

### src/components/TurnGroup.tsx
9:export interface Group
50:export function groupItems(items: Item[], prev: Group[]
94:export function fmtDuration(ms: number): string
101:export function fmtTokens(n: number): string
108:export function turnTokenTitle(t: TurnItem | undefined | null): string | undefined
138:export function TurnGroup(props:

### src/components/TypewriterText.tsx
3:export function TypewriterText(props:

### src/components/UpdateModal.tsx
18:export function UpdateModal(props:

### src/components/icons.tsx
23:export const IconPlus
24:export const IconFolder
26:export const IconGear
34:export const IconStop
35:export const IconStopwatch
37:export const IconSend
38:export const IconTrash
40:export const IconChevron
42:export const IconTerminal
43:export const IconFile
45:export const IconPencil
47:export const IconSearch
48:export const IconGlobe
50:export const IconBrain
52:export const IconWrench
54:export const IconMove
56:export const IconCheck
57:export const IconStar
65:export const IconThumbUp
67:export const IconThumbDown
69:export const IconMerge
79:export const IconClue
89:export const IconEye
91:export const IconCopy
93:export const IconUndo
95:export const IconX
96:export const IconShare
98:export const IconBell
100:export const IconDownload
102:export const IconUsers
104:export const IconBroadcast
107:export const IconLogo
119:export const IconTrophy
128:export const IconCompress
131:export function toolIcon(kind: string, size

### src/components/signatureOverlay.ts
7:export const [signatureProgress, setSignatureProgress]
10:export const [signatureVisible, setSignatureVisible]

### src/components/slashMenuLayout.ts
7:export function fitSlashMenuHeight(

### src/components/slashSuggestions.ts
4:export type SlashSuggestion
24:export function getSlashSuggestions(

### src/ipc.ts
51:export const api

### src/promptDraft.ts
10:export function rememberPromptDraft(text: string, images: PromptImage[])
18:export function takePromptDraft(): PromptDraft | null

### src/promptQueue.ts
6:export type QueuedPrompt
22:export function holdPromptQueue(threadId: string | null | undefined)
32:export function releasePromptQueue(threadId: string | null | undefined)
42:export function enqueuePrompt(threadId: string, text: string, images: PromptImage[])
57:export function removeQueuedPrompt(itemId: string): QueuedPrompt | null
81:export async function dispatchQueuedPrompt(item: QueuedPrompt, steerNow

### src/store.ts
48:export type ThemePref
71:export const [modelFavoriteIds, setModelFavoriteIds]
73:export function toggleModelFavorite(id: string)
89:export function initTheme()
187:export const [state, setState]
281:export function setTheme(theme: ThemePref)
292:export function isExpanded(key: number | string, fallback
300:export function toggleExpanded(key: number | string, value?: boolean)
308:export function modelChoices(
337:export function resolveAvailableModel(
354:export const UNIFIED_MODES: ModeChoice[]
361:export function modeChoices(
369:export function normalizeUnifiedMode(m?: string | null): "build" | "plan" | undefined
387:export function reasoningEffortChoices(
426:export async function ensureModelOptions(agentKind: AgentKind)
447:export const ALL_AGENT_KINDS: AgentKind[]
481:export function enabledAgentKinds(): AgentKind[]
488:export function resolveEnabledAgentKind(kind: AgentKind): AgentKind
493:export async function refreshThreads()
506:export async function refreshProjects()
510:export async function refreshQuota()
518:export async function refreshModelCosts()
538:export async function refreshRelayStatus()
546:export function clueMentionPeers(): Peer[]
561:export async function refreshInbox()
615:export async function refreshAchievements(reloadImages
654:export function markAchievementsSeen()
662:export async function refreshRoamingFolders()
673:export function setView(view: "home" | "clues" | "employees" | "workbench")
677:export function clueCurrentVersion(card: ClueCard)
681:export function clueCardById(cardId: string | null | undefined): ClueCard | undefined
690:export async function refreshClueGroups()
695:export async function captureClue(
717:export async function addClueComment(
728:export async function summarizeClue(threadId: string)
732:export async function associateClues(beforeCardId: string, afterCardId: string)
737:export async function disassociateClues(beforeCardId: string, afterCardId: string)
742:export async function splitClue(cardId: string)
747:export async function stackClues(cardIds: string[])
752:export async function deleteClue(cardId: string)
757:export function startSessionFromClue(card: ClueCard)
764:export function clearPendingClueCard()
768:export function openClueCard(cardId: string)
777:export function clearClueOpenRequest(cardId: string)
781:export function markClueMentionRead(cardId: string)
785:export async function refreshEmployees()
793:export async function refreshEmployeeTasks()
801:export async function refreshMarks()
809:export async function refreshDecisions()
818:export function pendingDecisionCount(): number
824:export function roamingPeers(): Peer[]
831:export function ensurePeerModels(token: string, force
840:export function peerBranchKey(token: string, folder: string): string
846:export function ensurePeerBranches(token: string, folder: string)
858:export function preloadPeerModels(force
867:export async function checkAndStageUpdate(): Promise<string>
889:export async function applyStagedUpdate()
901:export function reportActivity(force
923:export async function createRoamingThread(
973:export async function respondRoamRequest(
986:export async function createQuotaThread(
1028:export function clearQuotaRoamingProgress()
1105:export async function openThread(id: string)
1159:export function closeThread()
1179:export async function createThread(
1232:export const lastUsed
1282:export async function setThreadModel(model: string)
1291:export async function pickThreadModel(agentKind: AgentKind, model: string)
1311:export async function setThreadMode(mode: string)
1320:export async function implementProposedPlan()
1328:export function dismissProposedPlan()
1332:export async function setThreadReasoningEffort(reasoningEffort: string)
1340:export async function deleteThread(id: string)
1348:export async function deleteThreads(ids: string[]): Promise<number>
1357:export async function deleteProjectThreads(ids: string[]): Promise<number>
1366:export async function sendPrompt(
1419:export function assertBuiltinPrompt(text: string, images: PromptImage[]
1716:export async function startFireRelay(
1775:export function setTimeMachineEditTarget(
1780:export function chatScrollToBottomSignal()
1783:export function timeMachineChangedSignal()
1792:export function stashWorktreePrompt(threadId: string, text: string, images: PromptImage[])
1802:export async function sendPromptTo(threadId: string, text: string, images: PromptImage[])
1809:export async function editUserMessage(itemId: number, text: string, images: PromptImage[]
1857:export async function cancelTurn(stopReason?: string, deleteWork
1868:export async function compactThread()
1880:export async function respondPermission(requestKey: string, optionId: string)
2079:export async function refreshSlashCommands(agentKind: AgentKind)
2089:export const [restoreSettled, setRestoreSettled]
2091:export async function initStore()

### src/threadDisplay.ts
19:export function latestFireStage(
39:export function firstWakeDoChild(
49:export function firstWakeDoPairForThread(

### src/types.ts
1:export type AgentKind
3:export interface SlashCommand
11:export interface Worktree
18:export interface ProjectEntry
23:export interface ClueMention
28:export interface ClueAttachment
37:export interface ClueCardVersion
48:export interface ClueComment
58:export interface ClueCard
68:export interface ClueNodeGroup
76:export interface ClueContextCard
84:export interface ClueContextSnapshot
91:export interface CaptureClueResult
97:export interface ClueAiSummary
102:export interface ThreadMeta
134:export interface TimeMachinePrompt
139:export interface TimeMachineCheckpoint
150:export interface TimeMachineTimeline
157:export interface TimeMachineRestoreResult
162:export interface PromptImage
170:export interface UserItem
178:export interface AssistantItem
185:export interface ThoughtItem
192:export interface ToolItem
206:export interface SystemItem
215:export interface TurnItem
231:export type Item
233:export type ToolContent
238:export interface PlanEntry
244:export interface Thread
283:export interface ModelChoice
292:export interface EffortChoice
298:export interface ModeChoice
304:export interface ModelOptions
321:export interface PermissionOption
327:export interface QuestionInfo
335:export interface PermissionRequest
350:export interface CursorModelContextRule
357:export interface Settings
444:export interface AgentInstructionTarget
453:export interface GlobalAgentInstructions
460:export interface SkillInfo
466:export interface CliStatus
475:export interface CliOperationProgress
485:export interface BranchList
491:export interface WorktreeRecord
504:export interface RoamingFolder
511:export interface PeerModels
521:export interface Peer
532:export interface RelayStatus
538:export interface Achievement
551:export interface IncomingRoamRequest
572:export interface QuotaRoamingProgress
579:export interface IncomingShare
594:export interface RevertChange
602:export interface RevertResult
608:export interface Status
614:export interface ModelCost
626:export interface UpdateInfo
637:export interface UpdateProgress
645:export interface Quota
654:export type UpdateOp
662:export interface TurnEvent
669:export interface Partner
679:export interface Employee
732:export interface WorkHours
742:export type DecisionStatus
754:export interface Decision
784:export type NoticeLabel
785:export type NoticeStatus
793:export interface ActorRef
799:export interface NoticeAction
804:export interface Notice
851:export type EmployeeTaskStatus
854:export interface EmployeeTask
871:export type MarkStatus
874:export interface Mark
897:export interface EmployeeJournalEntry
929:export interface MindHandoff
938:export interface AttentionPlan
952:export interface MindSnapshot

### src/utils.ts
4:export function agentLabel(kind: AgentKind): string
25:export function agentShort(kind: AgentKind): string
50:export function stripAnsi(text: string): string
55:export function displayToolTitle(title: string): string
62:export function isScratch(cwd: string): boolean
67:export function scratchParent(cwd: string): string
74:export function setFileDropBlocked(blocked: boolean)
77:export function isFileDropBlocked()
