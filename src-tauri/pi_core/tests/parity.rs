//! Differential parity tests against golden vectors produced by the real node
//! implementation (`scripts/alkaid-core.mjs`). Regenerate with the oracle in the
//! milestone docs; `cargo test` itself needs no node.

use base64::Engine;
use pi_core::{
    agent::{run_agent_loop, LoopConfig, LoopContext, StreamTurn},
    apply_smart_edits, build_system_prompt, clamp_openai_payload_tool_outputs,
    clamp_prompt_cache_key, clamp_tool_output_text, decode_text_buffer, format_alkaid_skills_prompt,
    format_size, govern_text, inject_openai_prompt_cache_key, ls_tool, merge_usage, normalize_path,
    read_files_one, resolve_to_cwd, truncate_head, truncate_line, truncate_tail, write_tool,
    NormalizeOptions, ReadRequest, ShellConfig, Skill, OPENAI_TOOL_OUTPUT_SAFE_MAX_CHARS,
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
        let mut config = LoopConfig {
            get_steering_messages: None,
            get_follow_up_messages: None,
            timestamp: 9999,
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
