//! Differential parity tests against golden vectors produced by the real node
//! implementation (`scripts/alkaid-core.mjs`). Regenerate with the oracle in the
//! milestone docs; `cargo test` itself needs no node.

use base64::Engine;
use pi_core::{
    agent::{run_agent_loop, Agent, AgentState, LoopConfig, LoopContext, StreamTurn},
    aggregated_output, apply_edits_to_normalized_content, apply_smart_edits, build_system_prompt,
    clamp_openai_payload_tool_outputs, clamp_prompt_cache_key, clamp_tool_output_text,
    completed_tool_item, decode_text_buffer, format_alkaid_skills_prompt, format_size, govern_text,
    inject_openai_prompt_cache_key, ls_tool, merge_compat_defaults, merge_config, merge_usage,
    normalize_path, parse_jsonc, read_files_one, resolve_model, resolve_to_cwd, started_tool_item,
    truncate_head, truncate_line, truncate_tail, write_tool, NormalizeOptions, ReadRequest,
    ShellConfig, Skill, OPENAI_TOOL_OUTPUT_SAFE_MAX_CHARS,
};
use pi_core::{
    compact_native_tool_results, compact_slim_memory, context_pressure_tier,
    context_tokens_from_messages, estimate_context_tokens, format_slim_memory, memory_without_current,
    normalize_value, rebase_native_context_for_slim_memory, seed_slim_memory_from_messages,
    should_use_full_context, strip_completed_openai_reasoning, CompactOptions, SlimMemory,
};
use serde_json::{json, Value};

const GOLDEN: &str = include_str!("../testdata/golden.json");

fn golden() -> Value {
    serde_json::from_str(GOLDEN).expect("golden.json parses")
}

/// Mimic JS `String(x)` coercion for scalar JSON inputs.
fn coerce_str(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        other => other.to_string(),
    }
}

fn opt_str(value: &Value) -> Option<&str> {
    value.as_str()
}

#[test]
fn parity_clamp_tool_output_text() {
    for case in golden()["clampToolOutputText"].as_array().unwrap() {
        let text = coerce_str(&case["input"]["text"]);
        let max = case["input"]["maxChars"].as_u64().unwrap() as usize;
        let got = clamp_tool_output_text(Some(&text), max);
        assert_eq!(
            got,
            case["expected"].as_str().unwrap(),
            "clampToolOutputText input {:?}",
            case["input"]
        );
    }
}

#[test]
fn parity_clamp_tool_output_text_default_max() {
    // The default maxChars path must agree with the constant's JS value.
    assert_eq!(OPENAI_TOOL_OUTPUT_SAFE_MAX_CHARS, 10_485_248);
    let short = "hello";
    assert_eq!(
        clamp_tool_output_text(Some(short), OPENAI_TOOL_OUTPUT_SAFE_MAX_CHARS),
        short
    );
}

#[test]
fn parity_govern_tool_result() {
    for case in golden()["governToolResult"].as_array().unwrap() {
        let text = case["input"]["text"].as_str().unwrap();
        let max_bytes = case["input"]["maxBytes"].as_u64().unwrap() as usize;
        let expected_text = case["expected"]["text"].as_str().unwrap();
        match govern_text(text, max_bytes, None) {
            Some(governed) => {
                assert_eq!(
                    governed.text, expected_text,
                    "govern text maxBytes={max_bytes} input_len={}",
                    text.len()
                );
                assert_eq!(
                    governed.original_bytes,
                    case["expected"]["details"]["originalBytes"].as_u64().unwrap() as usize,
                    "govern originalBytes maxBytes={max_bytes}"
                );
                assert!(governed.archived_path.is_none());
            }
            None => {
                // Unchanged path: node returns the original text verbatim.
                assert_eq!(
                    text, expected_text,
                    "govern unchanged maxBytes={max_bytes}"
                );
            }
        }
    }
}

#[test]
fn parity_clamp_prompt_cache_key() {
    for case in golden()["clampPromptCacheKey"].as_array().unwrap() {
        let key = opt_str(&case["input"]["key"]);
        let got = clamp_prompt_cache_key(key);
        let expected = &case["expected"];
        match got {
            Some(value) => assert_eq!(
                &Value::String(value),
                expected,
                "clampPromptCacheKey {:?}",
                case["input"]
            ),
            None => assert!(
                expected.is_null(),
                "clampPromptCacheKey expected null for {:?}",
                case["input"]
            ),
        }
    }
}

#[test]
fn parity_inject_openai_prompt_cache_key() {
    for case in golden()["injectOpenAIPromptCacheKey"].as_array().unwrap() {
        let payload = &case["input"]["payload"];
        let session_id = opt_str(&case["input"]["sessionId"]);
        let got = inject_openai_prompt_cache_key(payload, session_id);
        let expected = &case["expected"];
        match got {
            Some(value) => assert_eq!(
                &value, expected,
                "injectOpenAIPromptCacheKey {:?}",
                case["input"]
            ),
            None => assert!(
                expected.is_null(),
                "injectOpenAIPromptCacheKey expected null for {:?}",
                case["input"]
            ),
        }
    }
}

#[test]
fn parity_clamp_openai_payload_tool_outputs() {
    for case in golden()["clampOpenAIPayloadToolOutputs"].as_array().unwrap() {
        let payload = &case["input"]["payload"];
        let max = case["input"]["maxChars"].as_u64().unwrap() as usize;
        let got = clamp_openai_payload_tool_outputs(payload, max);
        let expected = &case["expected"];
        match got {
            Some(value) => assert_eq!(
                &value, expected,
                "clampOpenAIPayloadToolOutputs {:?}",
                case["input"]
            ),
            None => assert!(
                expected.is_null(),
                "clampOpenAIPayloadToolOutputs expected null for {:?}",
                case["input"]
            ),
        }
    }
}

#[test]
fn parity_merge_usage() {
    for case in golden()["mergeAlkaidUsage"].as_array().unwrap() {
        let total = case["input"]["total"].clone();
        let usage = case["input"]["usage"].clone();
        let total_ref = if total.is_null() { None } else { Some(&total) };
        let usage_ref = if usage.is_null() { None } else { Some(&usage) };
        let got = merge_usage(total_ref, usage_ref);
        let expected = &case["expected"];
        match got {
            Some(value) => assert_eq!(&value, expected, "mergeAlkaidUsage {:?}", case["input"]),
            None => assert!(
                expected.is_null(),
                "mergeAlkaidUsage expected null for {:?}",
                case["input"]
            ),
        }
    }
}

fn parse_normalize_options(value: &Value) -> NormalizeOptions {
    let obj = value.as_object();
    let get = |key: &str| obj.and_then(|o| o.get(key));
    NormalizeOptions {
        trim: get("trim").and_then(Value::as_bool).unwrap_or(false),
        normalize_unicode_spaces: get("normalizeUnicodeSpaces")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        strip_at_prefix: get("stripAtPrefix").and_then(Value::as_bool).unwrap_or(false),
        expand_tilde: get("expandTilde").and_then(Value::as_bool).unwrap_or(true),
        home_dir: get("homeDir").and_then(Value::as_str).map(String::from),
    }
}

/// Mirror of the node generator's `buildStreamEvents`: turn a scripted response
/// entry into a `StreamTurn`. An entry with `deltas` produces `start` +
/// `text_delta` events (with an accumulating partial) + `done`; a plain message
/// produces `start` + `done`.
fn build_stream_turn(entry: &Value) -> StreamTurn {
    if let (Some(deltas), Some(final_message)) = (
        entry.get("deltas").and_then(Value::as_array),
        entry.get("final"),
    ) {
        let mut events = Vec::new();
        let mut text = String::new();
        let partial = |text: &str| -> Value {
            let mut message = final_message.clone();
            message["content"] = json!([{ "type": "text", "text": text }]);
            message
        };
        events.push(json!({ "type": "start", "partial": partial("") }));
        for delta in deltas {
            text.push_str(delta.as_str().unwrap_or(""));
            events.push(json!({ "type": "text_delta", "delta": delta, "partial": partial(&text) }));
        }
        events.push(json!({ "type": "done" }));
        StreamTurn {
            events,
            result: final_message.clone(),
        }
    } else {
        StreamTurn {
            events: vec![
                json!({ "type": "start", "partial": entry }),
                json!({ "type": "done" }),
            ],
            result: entry.clone(),
        }
    }
}

fn strip_timestamps(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(strip_timestamps).collect()),
        Value::Object(map) => {
            let mut cleaned = serde_json::Map::new();
            for (key, item) in map {
                if key == "timestamp" {
                    continue;
                }
                cleaned.insert(key.clone(), strip_timestamps(item));
            }
            Value::Object(cleaned)
        }
        other => other.clone(),
    }
}

#[test]
fn parity_parse_jsonc() {
    for case in golden()["parseJsonc"].as_array().unwrap() {
        let text = case["input"]["text"].as_str().unwrap();
        let got = parse_jsonc(text).unwrap_or_else(|e| panic!("parseJsonc failed: {e}"));
        assert_eq!(&got, &case["expected"], "parseJsonc {:?}", text);
    }
}

#[test]
fn parity_merge_config() {
    for case in golden()["mergeConfig"].as_array().unwrap() {
        let server = &case["input"]["server"];
        let local = &case["input"]["local"];
        assert_eq!(
            &merge_config(server, local),
            &case["expected"],
            "mergeConfig {:?}",
            case["input"]
        );
    }
}

fn env_map(config: &Value) -> std::collections::HashMap<String, String> {
    config
        .get("env")
        .and_then(Value::as_object)
        .map(|obj| {
            obj.iter()
                .filter_map(|(key, value)| Some((key.clone(), value.as_str()?.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn parity_resolve_model() {
    for case in golden()["resolveModel"].as_array().unwrap() {
        let config = &case["input"]["config"];
        let selection = case["input"]["selection"].as_str().unwrap();
        let env = env_map(config);
        let got = resolve_model(config, selection, &env);
        let expected = &case["expected"];
        if expected["ok"].as_bool().unwrap() {
            let result = got
                .unwrap_or_else(|e| panic!("resolveModel {selection} expected ok, got: {e}"));
            assert_eq!(&result, &expected["result"], "resolveModel {selection}");
        } else {
            let error = got
                .err()
                .unwrap_or_else(|| panic!("resolveModel {selection} expected error"));
            assert_eq!(error, expected["error"].as_str().unwrap(), "resolveModel {selection}");
        }
    }
}

#[test]
fn parity_compat_defaults() {
    for case in golden()["compatDefaults"].as_array().unwrap() {
        let api = case["input"]["api"].as_str().unwrap();
        let model_id = case["input"]["modelId"].as_str().unwrap();
        let base_url = case["input"]["baseUrl"].as_str().unwrap();
        let existing = case["input"]["existing"].clone();
        let existing_ref = if existing.is_null() { None } else { Some(&existing) };
        let got = merge_compat_defaults(api, model_id, base_url, existing_ref);
        let got = if got.is_null() { Value::Null } else { got };
        assert_eq!(&got, &case["expected"], "compatDefaults {:?}", case["input"]);
    }
}

#[test]
fn parity_edit_diff() {
    for case in golden()["editDiff"].as_array().unwrap() {
        let content = case["input"]["content"].as_str().unwrap();
        let path = case["input"]["path"].as_str().unwrap();
        let edits: Vec<(String, String)> = case["input"]["edits"]
            .as_array()
            .unwrap()
            .iter()
            .map(|pair| {
                let pair = pair.as_array().unwrap();
                (
                    pair[0].as_str().unwrap().to_string(),
                    pair[1].as_str().unwrap().to_string(),
                )
            })
            .collect();
        let got = apply_edits_to_normalized_content(content, &edits, path);
        let expected = &case["expected"];
        if expected["ok"].as_bool().unwrap() {
            let result = got
                .unwrap_or_else(|e| panic!("editDiff {:?} expected ok, got error: {e}", case["input"]));
            assert_eq!(
                result.new_content,
                expected["newContent"].as_str().unwrap(),
                "editDiff newContent {:?}",
                case["input"]
            );
            assert_eq!(
                result.base_content,
                expected["baseContent"].as_str().unwrap(),
                "editDiff baseContent {:?}",
                case["input"]
            );
        } else {
            let error = got
                .err()
                .unwrap_or_else(|| panic!("editDiff {:?} expected error, got ok", case["input"]));
            assert_eq!(
                error,
                expected["error"].as_str().unwrap(),
                "editDiff error {:?}",
                case["input"]
            );
        }
    }
}

#[test]
fn parity_bridge_tool_item() {
    let success_end = json!({ "result": { "content": [{ "type": "text", "text": "line1" }, { "type": "text", "text": "line2" }] }, "isError": false });
    let failed_end = json!({ "result": { "content": [{ "type": "text", "text": "boom" }] }, "isError": true });
    for case in golden()["bridgeToolItem"].as_array().unwrap() {
        let event = &case["input"]["event"];
        let started = started_tool_item(event);
        assert_eq!(&started, &case["expected"]["started"], "started {:?}", event);
        let completed = completed_tool_item(&started, &success_end);
        assert_eq!(
            &completed,
            &case["expected"]["completed"],
            "completed {:?}",
            event
        );
        let failed = completed_tool_item(&started, &failed_end);
        assert_eq!(&failed, &case["expected"]["failed"], "failed {:?}", event);
    }
    assert_eq!(
        aggregated_output(Some(&json!({ "content": [{ "text": "a" }, { "text": "b" }] }))),
        "a\nb"
    );
    assert_eq!(aggregated_output(None), "");
}

#[test]
fn parity_agent_class() {
    for case in golden()["agentClass"].as_array().unwrap() {
        let input = &case["input"];
        let name = input["name"].as_str().unwrap();
        let prompt_text = input["promptText"].as_str().unwrap();
        let responses: Vec<Value> = input["responses"].as_array().unwrap().iter().cloned().collect();
        let tools: Vec<Value> = input["tools"].as_array().unwrap().iter().cloned().collect();
        let tool_results = input["toolResults"].clone();
        let steer_before: Vec<Value> = input["steerBefore"].as_array().unwrap().iter().cloned().collect();

        let state = AgentState::new(
            "You are a test agent.",
            json!({ "id": "test-model", "provider": "test-provider", "api": "test-api" }),
            tools,
            vec![],
        );
        let mut agent = Agent::new(state, "all", "one-at-a-time");
        for message in steer_before {
            agent.steer(message);
        }

        let mut stream_fn = |index: usize, _llm_context: &Value| -> StreamTurn {
            let entry = responses
                .get(index)
                .cloned()
                .unwrap_or_else(|| json!({ "role": "assistant", "content": [], "stopReason": "end_turn" }));
            build_stream_turn(&entry)
        };
        let mut tool_fn = |tool_name: &str, _args: &Value| -> (Value, bool) {
            match tool_results.get(tool_name) {
                Some(result) => (result.clone(), false),
                None => (
                    json!({ "content": [{ "type": "text", "text": format!("ran {tool_name}") }], "details": {} }),
                    false,
                ),
            }
        };

        let events = agent.prompt(&json!(prompt_text), &[], 9999, &mut stream_fn, &mut tool_fn);

        let events_stripped: Vec<Value> = events.iter().map(strip_timestamps).collect();
        let final_stripped: Vec<Value> = agent.state.messages.iter().map(strip_timestamps).collect();

        assert_eq!(
            events_stripped,
            case["expected"]["events"].as_array().unwrap().clone(),
            "agent class events: {name}"
        );
        assert_eq!(
            final_stripped,
            case["expected"]["finalMessages"].as_array().unwrap().clone(),
            "agent class finalMessages: {name}"
        );
        let expected_error = case["expected"]["errorMessage"].as_str();
        assert_eq!(
            agent.state.error_message.as_deref(),
            expected_error,
            "agent class errorMessage: {name}"
        );
    }
}

#[test]
fn parity_agent_loop() {
    for case in golden()["agentLoop"].as_array().unwrap() {
        let input = &case["input"];
        let name = input["name"].as_str().unwrap();
        let system_prompt = input["systemPrompt"].as_str().unwrap().to_string();
        let prompts: Vec<Value> = input["prompts"].as_array().unwrap().iter().cloned().collect();
        let responses: Vec<Value> = input["responses"].as_array().unwrap().iter().cloned().collect();
        let tools: Vec<Value> = input["tools"].as_array().unwrap().iter().cloned().collect();
        let tool_results = input["toolResults"].clone();

        let mut context = LoopContext {
            system_prompt,
            messages: Vec::new(),
            tools,
        };
        let parallel = input["toolExecution"].as_str() == Some("parallel");
        let mut config = LoopConfig {
            get_steering_messages: None,
            get_follow_up_messages: None,
            timestamp: 9999,
            parallel,
        };

        let mut events: Vec<Value> = Vec::new();
        let mut emit = |event: Value| events.push(event);
        let mut stream_fn = |index: usize, _llm_context: &Value| -> StreamTurn {
            let entry = responses
                .get(index)
                .cloned()
                .unwrap_or_else(|| json!({ "role": "assistant", "content": [], "stopReason": "end_turn" }));
            build_stream_turn(&entry)
        };
        let mut tool_fn = |tool_name: &str, _args: &Value| -> (Value, bool) {
            match tool_results.get(tool_name) {
                Some(result) => (result.clone(), false),
                None => (
                    json!({ "content": [{ "type": "text", "text": format!("ran {tool_name}") }], "details": {} }),
                    false,
                ),
            }
        };

        let final_messages =
            run_agent_loop(&prompts, &mut context, &mut config, &mut emit, &mut stream_fn, &mut tool_fn);

        let events_stripped: Vec<Value> = events.iter().map(strip_timestamps).collect();
        let final_stripped: Vec<Value> = final_messages.iter().map(strip_timestamps).collect();

        assert_eq!(
            events_stripped,
            case["expected"]["events"].as_array().unwrap().clone(),
            "agent loop events: {name}"
        );
        assert_eq!(
            final_stripped,
            case["expected"]["finalMessages"].as_array().unwrap().clone(),
            "agent loop finalMessages: {name}"
        );
    }
}

#[test]
fn parity_skills_prompt() {
    for case in golden()["skillsPrompt"].as_array().unwrap() {
        let skills: Vec<Skill> = case["input"]["skills"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| Skill {
                name: s["name"].as_str().unwrap().to_string(),
                description: s["description"].as_str().unwrap().to_string(),
                file_path: s["filePath"].as_str().unwrap().to_string(),
                disable_model_invocation: s["disableModelInvocation"].as_bool().unwrap(),
            })
            .collect();
        assert_eq!(
            format_alkaid_skills_prompt(&skills),
            case["expected"].as_str().unwrap(),
            "skillsPrompt {:?}",
            case["input"]
        );
    }
}

#[test]
fn parity_write_tool() {
    let base = std::env::temp_dir().join(format!("pi_write_test_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    for case in golden()["writeTool"].as_array().unwrap() {
        let path = case["input"]["path"].as_str().unwrap();
        let content = case["input"]["content"].as_str().unwrap();
        let got = write_tool(base.to_str().unwrap(), path, content)
            .unwrap_or_else(|e| panic!("write failed: {e}"));
        assert_eq!(
            got,
            case["expected"]["text"].as_str().unwrap(),
            "write {:?}",
            case["input"]
        );
        let written = std::fs::read_to_string(base.join(path)).unwrap();
        assert_eq!(written, content, "write content {:?}", case["input"]);
    }
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn parity_ls_tool() {
    let base = std::env::temp_dir().join(format!("pi_ls_test_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    for name in ["apple", "Banana", "cherry", "file10", "file2", "afile"] {
        std::fs::write(base.join(name), "x").unwrap();
    }
    for dir in ["Zulu", "alpha", "emptydir", "many"] {
        std::fs::create_dir_all(base.join(dir)).unwrap();
    }
    for i in 1..=5 {
        std::fs::write(base.join("many").join(format!("e{i}")), "x").unwrap();
    }
    let cwd = base.to_str().unwrap().to_string();

    for case in golden()["lsTool"].as_array().unwrap() {
        let expected = &case["expected"];
        let args = &case["input"]["args"];
        let path = args["path"].as_str();
        let limit = args["limit"].as_u64().map(|v| v as usize);
        let got = ls_tool(&cwd, path, limit);
        if expected["ok"].as_bool().unwrap() {
            let out = got.unwrap_or_else(|e| panic!("ls expected ok, got {e}: {:?}", args));
            assert_eq!(out.text, expected["text"].as_str().unwrap(), "ls text {:?}", args);
            match &out.details {
                Some(details) => {
                    assert_eq!(details, &expected["details"], "ls details {:?}", args)
                }
                None => assert!(
                    expected["details"].is_null(),
                    "ls details expected null {:?}",
                    args
                ),
            }
        } else if let Some(prefix) = expected.get("errorPrefix").and_then(Value::as_str) {
            let error = got.err().unwrap_or_else(|| panic!("ls expected error {:?}", args));
            assert!(error.starts_with(prefix), "ls error prefix: {error}");
        } else {
            let error = got.err().unwrap_or_else(|| panic!("ls expected error {:?}", args));
            assert_eq!(
                error,
                expected["error"].as_str().unwrap(),
                "ls error {:?}",
                args
            );
        }
    }
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn parity_resolve_to_cwd() {
    for case in golden()["resolveToCwd"].as_array().unwrap() {
        let input = case["input"]["input"].as_str().unwrap();
        let base = case["input"]["base"].as_str().unwrap();
        assert_eq!(
            resolve_to_cwd(input, base),
            case["expected"].as_str().unwrap(),
            "resolveToCwd {:?}",
            case["input"]
        );
    }
}

#[test]
fn parity_normalize_path() {
    for case in golden()["normalizePath"].as_array().unwrap() {
        let input = case["input"]["input"].as_str().unwrap();
        let options = parse_normalize_options(&case["input"]["options"]);
        assert_eq!(
            normalize_path(input, &options),
            case["expected"].as_str().unwrap(),
            "normalizePath {:?}",
            case["input"]
        );
    }
}

#[test]
fn parity_format_size() {
    for case in golden()["formatSize"].as_array().unwrap() {
        let bytes = case["input"]["bytes"].as_u64().unwrap() as usize;
        assert_eq!(
            format_size(bytes),
            case["expected"].as_str().unwrap(),
            "formatSize {bytes}"
        );
    }
}

fn truncation_options(case: &Value) -> (Option<usize>, Option<usize>) {
    let options = &case["input"]["options"];
    (
        options["maxLines"].as_u64().map(|v| v as usize),
        options["maxBytes"].as_u64().map(|v| v as usize),
    )
}

#[test]
fn parity_truncate_head() {
    for case in golden()["truncateHead"].as_array().unwrap() {
        let content = case["input"]["content"].as_str().unwrap();
        let (max_lines, max_bytes) = truncation_options(case);
        let got = serde_json::to_value(truncate_head(content, max_lines, max_bytes)).unwrap();
        assert_eq!(&got, &case["expected"], "truncateHead {:?}", case["input"]);
    }
}

#[test]
fn parity_truncate_tail() {
    for case in golden()["truncateTail"].as_array().unwrap() {
        let content = case["input"]["content"].as_str().unwrap();
        let (max_lines, max_bytes) = truncation_options(case);
        let got = serde_json::to_value(truncate_tail(content, max_lines, max_bytes)).unwrap();
        assert_eq!(&got, &case["expected"], "truncateTail {:?}", case["input"]);
    }
}

#[test]
fn parity_truncate_line() {
    for case in golden()["truncateLine"].as_array().unwrap() {
        let line = case["input"]["line"].as_str().unwrap();
        let max_chars = case["input"]["maxChars"].as_u64().map(|v| v as usize);
        let got = serde_json::to_value(truncate_line(line, max_chars)).unwrap();
        assert_eq!(&got, &case["expected"], "truncateLine {:?}", case["input"]);
    }
}

#[test]
fn parity_read_files() {
    let engine = base64::engine::general_purpose::STANDARD;
    let base = std::env::temp_dir().join(format!("pi_read_test_{}", std::process::id()));
    std::fs::create_dir_all(&base).unwrap();
    for (i, case) in golden()["readFiles"].as_array().unwrap().iter().enumerate() {
        let dir = base.join(format!("case_{i}"));
        std::fs::create_dir_all(&dir).unwrap();
        let file_name = case["input"]["fileName"].as_str().unwrap();
        if let Some(b64) = case["input"]["fileBase64"].as_str() {
            std::fs::write(dir.join(file_name), engine.decode(b64).unwrap()).unwrap();
        }
        let request = &case["input"]["request"];
        let req = ReadRequest {
            path: request["path"].as_str().unwrap().to_string(),
            offset: request["offset"].as_u64().map(|v| v as usize),
            limit: request["limit"].as_u64().map(|v| v as usize),
        };
        let got = read_files_one(&dir, &req);
        let expected = &case["expected"];
        if expected["ok"].as_bool().unwrap() {
            assert_eq!(
                &got,
                &expected["result"],
                "readFiles case {i} {:?}",
                case["input"]
            );
        } else {
            assert!(
                got.get("error").is_some(),
                "readFiles case {i} expected error, got {got}"
            );
            assert_eq!(
                got.get("path").and_then(Value::as_str),
                Some(req.path.as_str()),
                "readFiles case {i} error path",
            );
        }
    }
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn parity_apply_smart_edits() {
    for case in golden()["smartEdit"].as_array().unwrap() {
        let content = case["input"]["content"].as_str().unwrap();
        let path = case["input"]["path"].as_str().unwrap();
        let edits: Vec<(String, String)> = case["input"]["edits"]
            .as_array()
            .unwrap()
            .iter()
            .map(|pair| {
                let pair = pair.as_array().unwrap();
                (
                    pair[0].as_str().unwrap().to_string(),
                    pair[1].as_str().unwrap().to_string(),
                )
            })
            .collect();
        let got = apply_smart_edits(content, &edits, path);
        let expected = &case["expected"];
        if expected["ok"].as_bool().unwrap() {
            let result = got.unwrap_or_else(|error| {
                panic!("smartEdit {:?} expected ok, got error: {error}", case["input"])
            });
            assert_eq!(
                result.content,
                expected["content"].as_str().unwrap(),
                "smartEdit content {:?}",
                case["input"]
            );
            let exp_matches = expected["matches"].as_array().unwrap();
            assert_eq!(
                result.matches.len(),
                exp_matches.len(),
                "smartEdit matches len {:?}",
                case["input"]
            );
            for (got_match, exp_match) in result.matches.iter().zip(exp_matches) {
                assert_eq!(
                    got_match.edit_index,
                    exp_match["editIndex"].as_u64().unwrap() as usize,
                    "smartEdit editIndex {:?}",
                    case["input"]
                );
                assert_eq!(
                    got_match.mode,
                    exp_match["mode"].as_str().unwrap(),
                    "smartEdit mode {:?}",
                    case["input"]
                );
                let exp_line = exp_match
                    .get("line")
                    .and_then(Value::as_u64)
                    .map(|v| v as usize);
                assert_eq!(
                    got_match.line, exp_line,
                    "smartEdit line {:?}",
                    case["input"]
                );
            }
        } else {
            let error = got.err().unwrap_or_else(|| {
                panic!("smartEdit {:?} expected error, got ok", case["input"])
            });
            assert_eq!(
                error,
                expected["error"].as_str().unwrap(),
                "smartEdit error {:?}",
                case["input"]
            );
        }
    }
}

#[test]
fn parity_build_system_prompt() {
    for case in golden()["buildAlkaidSystemPrompt"].as_array().unwrap() {
        let options = &case["input"]["options"];
        let cwd = options["cwd"].as_str().unwrap();
        let read_only = options["readOnly"].as_bool().unwrap();
        let shell_config: Option<ShellConfig> = if options["shellConfig"].is_null() {
            None
        } else {
            Some(serde_json::from_value(options["shellConfig"].clone()).unwrap())
        };
        let skills = options["skills"].as_array().unwrap();
        assert!(
            skills.is_empty(),
            "M1 golden only covers empty skills; non-empty is M2"
        );
        let system_prompt = options["systemPrompt"].as_str().unwrap_or("");
        let got = build_system_prompt(cwd, read_only, shell_config.as_ref(), "", system_prompt);
        assert_eq!(
            got,
            case["expected"].as_str().unwrap(),
            "buildAlkaidSystemPrompt {:?}",
            options
        );
    }
}

#[test]
fn parity_decode_text_buffer() {
    let engine = base64::engine::general_purpose::STANDARD;
    for case in golden()["decodeTextBuffer"].as_array().unwrap() {
        let bytes = engine
            .decode(case["input"]["base64"].as_str().unwrap())
            .unwrap();
        let got = decode_text_buffer(&bytes);
        assert_eq!(
            got,
            case["expected"].as_str().unwrap(),
            "decodeTextBuffer {:?}",
            case["input"]
        );
    }
}

#[test]
fn parity_slim_memory() {
    let sm = &golden()["slimMemory"];

    for case in sm["estimateContextTokens"].as_array().unwrap() {
        let text = case["input"]["text"].as_str().unwrap();
        assert_eq!(
            estimate_context_tokens(text),
            case["expected"].as_u64().unwrap(),
            "estimateContextTokens {:?}",
            text
        );
    }

    for case in sm["contextPressureTier"].as_array().unwrap() {
        let current = case["input"]["currentTokens"].as_f64().unwrap();
        let window = case["input"]["contextWindow"].as_f64().unwrap();
        assert_eq!(
            context_pressure_tier(current, window),
            case["expected"].as_str().unwrap(),
            "contextPressureTier {:?}",
            case["input"]
        );
    }

    for case in sm["contextTokensFromMessages"].as_array().unwrap() {
        let messages: Vec<Value> = case["input"]["messages"].as_array().unwrap().clone();
        assert_eq!(
            context_tokens_from_messages(&messages),
            case["expected"].as_u64().unwrap(),
            "contextTokensFromMessages {:?}",
            case["input"]
        );
    }

    for case in sm["stripCompletedOpenAIReasoning"].as_array().unwrap() {
        let messages: Vec<Value> = case["input"]["messages"].as_array().unwrap().clone();
        assert_eq!(
            strip_completed_openai_reasoning(&messages),
            *case["expected"].as_array().unwrap(),
            "stripCompletedOpenAIReasoning {:?}",
            case["input"]
        );
    }

    for case in sm["compactNativeToolResults"].as_array().unwrap() {
        let messages: Vec<Value> = case["input"]["messages"].as_array().unwrap().clone();
        let tier = case["input"]["tier"].as_str().unwrap();
        let preserve = case["input"]["preserveRecent"].as_u64().unwrap() as usize;
        let (msgs, changed) = compact_native_tool_results(&messages, tier, preserve);
        assert_eq!(
            msgs,
            *case["expected"]["messages"].as_array().unwrap(),
            "compactNativeToolResults messages {:?}",
            case["input"]
        );
        assert_eq!(
            changed,
            case["expected"]["changed"].as_bool().unwrap(),
            "compactNativeToolResults changed {:?}",
            case["input"]
        );
    }

    assert_eq!(
        serde_json::to_value(SlimMemory::new()).unwrap(),
        sm["createSlimMemory"][0]["expected"],
        "createSlimMemory"
    );

    let mut m = SlimMemory::new();
    m.append_turn("  padded prompt  ");
    m.set_latest_conclusion(&json!("conclusion text"));
    m.set_latest_conclusion(&json!(""));
    m.append_turn("");
    m.set_latest_conclusion(&json!([{ "type": "text", "text": "" }, { "type": "text", "text": "real" }]));
    assert_eq!(
        serde_json::to_value(&m).unwrap(),
        sm["appendAndConclusion"][0]["expected"],
        "appendAndConclusion"
    );

    for case in sm["normalizeSlimMemory"].as_array().unwrap() {
        let normalized = normalize_value(&case["input"]["memory"]);
        assert_eq!(
            serde_json::to_value(&normalized).unwrap(),
            case["expected"],
            "normalizeSlimMemory {:?}",
            case["input"]
        );
    }

    for case in sm["formatSlimMemory"].as_array().unwrap() {
        assert_eq!(
            format_slim_memory(&case["input"]["memory"]),
            case["expected"].as_str().unwrap(),
            "formatSlimMemory {:?}",
            case["input"]
        );
    }

    for case in sm["memoryWithoutCurrent"].as_array().unwrap() {
        let pending = case["input"]["pendingMessages"].as_bool().unwrap();
        let result = memory_without_current(&case["input"]["memory"], pending);
        assert_eq!(
            serde_json::to_value(&result).unwrap(),
            case["expected"],
            "memoryWithoutCurrent {:?}",
            case["input"]
        );
    }

    for case in sm["shouldUseFullContext"].as_array().unwrap() {
        let memory: SlimMemory = serde_json::from_value(case["input"]["memory"].clone()).unwrap();
        let max_tokens = case["input"]["maxContextTokens"].as_u64().unwrap();
        let max_chars = case["input"]["maxContextChars"].as_u64().map(|v| v as usize);
        assert_eq!(
            should_use_full_context(&memory, max_tokens, max_chars),
            case["expected"].as_bool().unwrap(),
            "shouldUseFullContext {:?}",
            case["input"]
        );
    }

    for case in sm["rebaseNativeContextForSlimMemory"].as_array().unwrap() {
        let messages: Vec<Value> = case["input"]["messages"].as_array().unwrap().clone();
        let start = case["input"]["activeTurnStart"].as_i64().unwrap();
        let (msgs, changed) =
            rebase_native_context_for_slim_memory(&messages, start, &case["input"]["memory"]);
        assert_eq!(
            msgs,
            *case["expected"]["messages"].as_array().unwrap(),
            "rebaseNativeContextForSlimMemory messages {:?}",
            case["input"]
        );
        assert_eq!(
            changed,
            case["expected"]["changed"].as_bool().unwrap(),
            "rebaseNativeContextForSlimMemory changed {:?}",
            case["input"]
        );
    }

    let seed_messages: Vec<Value> = sm["seedSlimMemoryFromMessages"][0]["input"]["messages"]
        .as_array()
        .unwrap()
        .clone();
    let mut seeded = SlimMemory::new();
    seed_slim_memory_from_messages(&mut seeded, &seed_messages);
    assert_eq!(
        serde_json::to_value(&seeded).unwrap(),
        sm["seedSlimMemoryFromMessages"][0]["expected"],
        "seedSlimMemoryFromMessages"
    );

    for case in sm["compactSlimMemory"].as_array().unwrap() {
        let mut memory: SlimMemory = serde_json::from_value(case["input"]["memory"].clone()).unwrap();
        let opts = &case["input"]["options"];
        let options = CompactOptions {
            max_turns: Some(opts["maxTurns"].as_u64().unwrap() as usize),
            max_chars: Some(opts["maxChars"].as_u64().unwrap() as usize),
            current_tokens: opts["currentTokens"].as_u64().unwrap(),
            max_tokens: Some(opts["maxTokens"].as_u64().unwrap()),
        };
        let stub = case["input"]["stubDigest"].as_str().unwrap().to_string();
        let compacted = compact_slim_memory(&mut memory, options, move |_| stub.clone());
        assert_eq!(
            compacted,
            case["expected"]["compacted"].as_bool().unwrap(),
            "compactSlimMemory compacted {:?}",
            opts
        );
        assert_eq!(
            serde_json::to_value(&memory).unwrap(),
            case["expected"]["memory"],
            "compactSlimMemory memory {:?}",
            opts
        );
    }
}
