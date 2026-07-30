//! `pi_core` — a Rust port of the deterministic core of the node pi-agent that
//! powers the Vega/Alkaid backend. The goal is byte-for-byte output parity with
//! `scripts/alkaid-core.mjs` for every non-LLM computation, verified by the
//! differential golden vectors in `testdata/golden.json`.
//!
//! Scope (milestone M1): text governance/truncation, OpenAI payload transforms,
//! usage merging, and text-encoding detection. The agent loop, tools, and
//! provider streaming land in later milestones.

pub mod agent;
pub mod alkaid_config;
pub mod bridge;
pub mod edit_diff;
pub mod encoding;
pub mod ls;
pub mod paths;
pub mod payload;
pub mod prompt;
pub mod provider;
pub mod provider_anthropic;
pub mod provider_google;
pub mod provider_responses;
pub mod read;
pub mod skills;
pub mod skills_discovery;
pub mod slim_memory;
pub mod smart_edit;
pub mod text;
pub mod tools;
pub mod transform;
pub mod truncate;
pub mod write;

pub use encoding::{decode_text_buffer, detect_text_encoding, swap_utf16_bytes, Encoding};
pub use alkaid_config::{
    merge_compat_defaults, merge_config, parse_jsonc, provider_api, resolve_env, resolve_model,
    strip_json_comments, strip_trailing_commas,
};
pub use bridge::{aggregated_output, completed_tool_item, started_tool_item, ProtocolAccumulator};
pub use edit_diff::{
    apply_edits_to_normalized_content, detect_line_ending, normalize_for_fuzzy_match,
    normalize_to_lf, restore_line_endings, strip_bom, EditResult,
};
pub use ls::{ls_tool, LsOutput};
pub use paths::{normalize_path, resolve_path, resolve_to_cwd, NormalizeOptions};
pub use payload::{
    clamp_openai_payload_tool_outputs, clamp_prompt_cache_key, inject_openai_prompt_cache_key,
    merge_usage, OPENAI_PROMPT_CACHE_KEY_MAX_LENGTH,
};
pub use prompt::{build_system_prompt, ShellConfig, ShellKind};
pub use provider::{
    build_openai_chat_request, map_finish_reason, pi_message_to_openai, pi_tools_to_openai,
    OpenAiChatAccumulator,
};
pub use read::{
    read_file_text, read_files_one, read_text_lines, ReadLines, ReadRequest,
    DEFAULT_BATCH_READ_LINES, READ_FILES_MAX_BYTES,
};
pub use skills::{
    format_alkaid_skills_prompt, format_skills_for_prompt, format_skills_for_prompt_compressed,
    Skill,
};
pub use skills_discovery::{
    expand_skill_command, load_skills_from_dir, parse_frontmatter, strip_skill_frontmatter,
};
pub use slim_memory::{
    apply_compaction, compact_native_tool_results, compact_slim_memory, context_pressure_tier,
    context_tokens_from_messages, estimate_context_tokens, format_normalized, format_slim_memory,
    memory_without_current, normalize_value, plan_compaction, rebase_native_context_for_slim_memory,
    seed_slim_memory_from_messages, should_use_full_context, strip_completed_openai_reasoning,
    text_content, CompactionPlan, CompactOptions, NormalizedMemory, SlimMemory, Turn,
};
pub use smart_edit::{apply_smart_edits, SmartMatch, SmartResult};
pub use tools::{tool_fn_for, NativeTools};
pub use truncate::{
    format_size, truncate_head, truncate_line, truncate_tail, TruncateLineResult, Truncation,
    DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, GREP_MAX_LINE_LENGTH,
};
pub use write::write_tool;
pub use text::{
    clamp_tool_output_text, govern_text, head_tail_utf8, safe_archive_segment,
    truncate_utf8_tail_to_bytes, truncate_utf8_to_bytes, utf16_len, utf8_byte_len, Governed,
    OPENAI_TOOL_OUTPUT_MAX_CHARS, OPENAI_TOOL_OUTPUT_SAFE_MAX_CHARS,
    TOOL_OUTPUT_CONTEXT_MAX_BYTES,
};
