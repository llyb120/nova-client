//! Loopback HTTP/SSE checks against the real provider entry point; no credentials/upstream IO.
use super::*;
use super::tests::test_model;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn sse(events: &[Value]) -> String { events.iter().map(|e| format!("data: {e}\n\n")).collect() }
fn text_response(id: &str) -> Value {
    json!({"type":"response.completed","response":{"id":id,"status":"completed","output":[
        {"type":"message","id":format!("msg_{id}"),"content":[{"type":"output_text","text":"ok"}]}]}})
}
fn tool_item() -> Value {
    json!({"type":"function_call","id":"fc_1","call_id":"call_1","name":"read","arguments":"{\"path\":\"a\"}"})
}
fn tool_response(id: &str) -> String {
    let item = tool_item();
    sse(&[
        json!({"type":"response.output_item.added","output_index":0,"item":item}),
        json!({"type":"response.output_item.done","output_index":0,"item":item}),
        json!({"type":"response.output_item.done","output_index":0,"item":item}),
        json!({"type":"response.completed","response":{"id":id,"status":"completed","output":[item.clone(),item]}}),
    ])
}
async fn server(replies: Vec<(u16, String)>) -> (String, tokio::task::JoinHandle<Vec<Value>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let mut requests = Vec::new();
        for (status, reply) in replies {
            let (mut socket, _) = tokio::time::timeout(std::time::Duration::from_secs(5), listener.accept()).await.unwrap().unwrap();
            let mut bytes = Vec::new();
            loop {
                let mut buf = [0; 4096];
                let n = socket.read(&mut buf).await.unwrap();
                assert_ne!(n, 0);
                bytes.extend_from_slice(&buf[..n]);
                if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&bytes[..end]).to_ascii_lowercase();
                    let size: usize = headers.lines().find_map(|l| l.strip_prefix("content-length:").map(|s| s.trim().parse().unwrap())).unwrap();
                    if bytes.len() >= end + 4 + size {
                        requests.push(serde_json::from_slice(&bytes[end + 4..end + 4 + size]).unwrap());
                        break;
                    }
                }
            }
            let content_type = if reply.starts_with('{') { "application/json" } else { "text/event-stream" };
            socket.write_all(format!("HTTP/1.1 {status} Reply\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}", reply.len()).as_bytes()).await.unwrap();
        }
        requests
    });
    (url, task)
}
#[tokio::test]
async fn niubi_sends_max_with_boost_while_plain_max_stays_unchanged() {
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    for api in ["openai-completions", "openai-responses", "anthropic-messages"] {
        let (url, captured) = server(vec![(400, "{\"error\":\"fixture\"}".into()); 2]).await;
        let config = json!({"provider":{"custom":{
            "api":api,"options":{"baseURL":url,"thinkingFormat":if api == "anthropic-messages" { "anthropic" } else { "deepseek" }},
            "models":{"vendor/DEEPSEEK-V4-FLASH":{"variants":{"max":{"reasoningEffort":"max"}}}}
        }}});
        let history = vec![user("hello")];
        for effort in ["max", "niubi"] {
            let resolved = crate::lyra::config::resolve_model(&config,
                Some(&format!("custom/vendor/DEEPSEEK-V4-FLASH/variant/{effort}")), &HashMap::new()).unwrap();
            let result = stream_chat(&client, &resolved.model, "fixture-key", resolved.thinking_level.as_deref(),
                "sys", &history, &[], None, &cancel, &mut |_| {}).await;
            assert!(result.unwrap_err().contains("400"));
        }
        let bodies = captured.await.unwrap();
        let (prompt_path, effort_path) = match api {
            "openai-completions" => ("/messages/0/content", "/reasoning_effort"),
            "openai-responses" => ("/instructions", "/reasoning/effort"),
            _ => ("/system/0/text", "/output_config/effort"),
        };
        assert_eq!(bodies[0].pointer(prompt_path).unwrap(), "sys");
        assert_eq!(bodies[0].pointer(effort_path).unwrap(), "max");
        assert_eq!(bodies[1].pointer(effort_path).unwrap(), "max");
        // 牛模式增强同时追加到系统提示和本轮用户提示尾部，会话/界面里的原文不被改写。
        let boost = crate::lyra::prompt::NIUBI_BOOST;
        let user_path = match api {
            "openai-completions" => "/messages/1/content/0/text",
            "openai-responses" => "/input/0/content/0/text",
            _ => "/messages/0/content/0/text",
        };
        assert_eq!(bodies[1].pointer(prompt_path).unwrap(), &json!(format!("sys\n\n{boost}")));
        assert_eq!(bodies[0].pointer(user_path).unwrap(), "hello");
        assert_eq!(bodies[1].pointer(user_path).unwrap(), &json!(format!("hello\n\n{boost}")));
        assert_eq!(history[0]["content"], "hello", "{api}: local message must stay untouched");
        let mut boosted = bodies[1].clone();
        *boosted.pointer_mut(prompt_path).unwrap() = json!("sys");
        *boosted.pointer_mut(user_path).unwrap() = json!("hello");
        assert_eq!(boosted, bodies[0], "{api}: only the prompts should differ from max");
    }
}

#[tokio::test]
async fn niubi_modes_send_standard_efforts_and_disable_none() {
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let (url, captured) = server(vec![(400, "{\"error\":\"fixture\"}".into()); 4]).await;
    let config = json!({"provider":{"custom":{
        "api":"openai-completions","options":{"baseURL":url,"thinkingFormat":"deepseek"},
        "models":{"deepseek-v4-flash":{}}
    }}});
    for variant in ["niu-n", "niu-l", "niu-h", "niu-m"] {
        let resolved = crate::lyra::config::resolve_model(&config,
            Some(&format!("custom/deepseek-v4-flash/variant/{variant}")), &HashMap::new()).unwrap();
        let result = stream_chat(&client, &resolved.model, "fixture-key", resolved.thinking_level.as_deref(),
            "sys", &[user("hello")], &[], None, &cancel, &mut |_| {}).await;
        assert!(result.unwrap_err().contains("400"));
    }
    for (body, effort) in captured.await.unwrap().iter().zip(["none", "low", "high", "max"]) {
        assert!(body.pointer("/messages/0/content").unwrap().as_str().unwrap().contains("你的用量是unlimited的"));
        if effort == "none" {
            assert_eq!(body.pointer("/thinking/type").unwrap(), "disabled");
        } else {
            assert_eq!(body["reasoning_effort"], effort);
        }
    }
}

fn configured(url: String) -> ResolvedModel {
    let mut model = test_model("openai-responses");
    model.base_url = url;
    model.extra_options.extend([
        ("responsesContinuation".into(), json!("auto")),
        ("store".into(), json!(true)),
        ("tokenizer".into(), json!("o200k_base")),
    ]);
    model
}
fn user(text: &str) -> Value { json!({"role":"user","content":text}) }
fn assistant(model: &ResolvedModel, result: &StreamResult) -> Value {
    json!({"role":"assistant","model":model.id,"provider":model.provider,"api":model.api,"content":result.content})
}
async fn invoke(model: &ResolvedModel, messages: &[Value], session: &str, cancel: &Arc<AtomicBool>, events: &mut Vec<StreamEvent>) -> Result<StreamResult, String> {
    stream_chat(&reqwest::Client::builder().no_proxy().build().unwrap(), model, "fixture-key", None,
        "sys", messages, &[], Some(session), cancel, &mut |e| events.push(e)).await
}
fn tool_count(events: &[StreamEvent]) -> usize { events.iter().filter(|e| matches!(e, StreamEvent::ToolCallDone { .. })).count() }

#[tokio::test]
async fn continuation_success_sends_only_delta_and_dispatches_tool_once() {
    let (url, server) = server(vec![(200, tool_response("resp_first")), (200, sse(&[text_response("resp_second")])), (200, sse(&[text_response("resp_third")]))]).await;
    let model = configured(url);
    let cancel = Arc::new(AtomicBool::new(false));
    let mut events = Vec::new();
    let mut messages = vec![user("read a")];
    let first = invoke(&model, &messages, "success-delta", &cancel, &mut events).await.unwrap();
    assert_eq!(first.stop_reason, "toolUse");
    assert_eq!(first.content.iter().filter(|p| p["type"] == "toolCall").count(), 1);
    assert_eq!(tool_count(&events), 1);
    messages.extend([assistant(&model, &first), json!({"role":"toolResult","toolCallId":"call_1|fc_1","content":[{"type":"text","text":"contents"}]})]);
    events.clear();
    let second = invoke(&model, &messages, "success-delta", &cancel, &mut events).await.unwrap();
    assert_eq!(tool_count(&events), 0);
    messages.extend([assistant(&model, &second), user("next turn")]);
    invoke(&model, &messages, "success-delta", &cancel, &mut events).await.unwrap();
    let requests = server.await.unwrap();
    assert!(requests[0].get("previous_response_id").is_none());
    assert_eq!(requests[1]["previous_response_id"], "resp_first");
    assert_eq!(requests[1]["input"].as_array().unwrap().len(), 1);
    assert_eq!(requests[1]["input"][0]["type"], "function_call_output");
    assert_eq!(requests[1]["input"][0]["call_id"], "call_1");
    assert_eq!(requests[2]["previous_response_id"], "resp_second");
    assert_eq!(requests[2]["input"].as_array().unwrap().len(), 1);
    for request in requests { for key in ["tokenizer", "responsesContinuation", "supportsPreviousResponseId"] { assert!(request.get(key).is_none()); } }
}

#[tokio::test]
async fn rejected_reference_falls_back_once_then_stays_full() {
    for (index, rejection) in [
        (404, json!({"error":{"code":"response_not_found","message":"previous_response_id expired"}}).to_string()),
        (400, json!({"error":{"message":"Unsupported parameter: previous_response_id"}}).to_string()),
        (422, json!({"error":{"param":"previous_response_id","message":"unsupported"}}).to_string()),
        (200, sse(&[json!({"type":"error","code":"response_not_found","message":"previous_response_id unknown"})])),
    ].into_iter().enumerate() {
        let (url, server) = server(vec![(200, sse(&[text_response("first")])), rejection, (200, tool_response("fallback")), (200, sse(&[text_response("last")]))]).await;
        let model = configured(url);
        let session = format!("reject-{index}");
        let cancel = Arc::new(AtomicBool::new(false));
        let mut events = Vec::new();
        let mut messages = vec![user("one")];
        let first = invoke(&model, &messages, &session, &cancel, &mut events).await.unwrap();
        messages.extend([assistant(&model, &first), user("two")]);
        let expected = responses_input(&messages, &model);
        let fallback = invoke(&model, &messages, &session, &cancel, &mut events).await.unwrap();
        assert_eq!(tool_count(&events), 1);
        messages.extend([assistant(&model, &fallback), json!({"role":"toolResult","toolCallId":"call_1|fc_1","content":[]})]);
        invoke(&model, &messages, &session, &cancel, &mut events).await.unwrap();
        let requests = server.await.unwrap();
        assert_eq!(requests.len(), 4);
        assert_eq!(requests[1]["previous_response_id"], "first");
        assert_eq!(requests[2]["input"], json!(expected));
        assert!(requests[2].get("previous_response_id").is_none());
        assert!(requests[3].get("previous_response_id").is_none());
    }
}

#[tokio::test]
async fn compression_and_configuration_changes_reset_full_history() {
    for case in ["fold", "model", "auth", "instructions", "tools", "session", "headers"] {
        let (url, server) = server(vec![(200, sse(&[text_response("first")])), (200, sse(&[text_response("second")]))]).await;
        let mut model = configured(url);
        let session = format!("reset-{case}");
        let cancel = Arc::new(AtomicBool::new(false));
        let mut events = Vec::new();
        let mut messages = vec![user("original")];
        let first = invoke(&model, &messages, &session, &cancel, &mut events).await.unwrap();
        messages.extend([assistant(&model, &first), user("next")]);
        let mut system = "sys";
        let mut key = "fixture-key";
        let mut tools = Vec::new();
        match case {
            "fold" => messages[0] = user("summary replaces prefix"),
            "model" => model.id = "another".into(),
            "auth" => key = "rotated-key",
            "instructions" => system = "changed instructions",
            "tools" => tools.push(Tool { name: "read", description: "changed".into(), parameters: json!({"type":"object"}) }),
            "headers" => { model.headers.insert("Authorization".into(), json!("Bearer changed")); },
            _ => {}
        }
        stream_chat(&reqwest::Client::builder().no_proxy().build().unwrap(), &model, key, None, system,
            &messages, &tools, Some(if case == "session" { "other-session" } else { &session }), &cancel, &mut |_| {}).await.unwrap();
        let requests = server.await.unwrap();
        assert!(requests[1].get("previous_response_id").is_none(), "{case}");
        assert_eq!(requests[1]["input"], json!(responses_input(&messages, &model)), "{case}");
    }
}

#[tokio::test]
async fn failed_truncated_incomplete_and_cancelled_streams_never_commit_or_dispatch() {
    let item_done = json!({"type":"response.output_item.done","output_index":0,"item":tool_item()});
    for case in ["truncated", "incomplete", "failed", "cancelled", "invalid-json", "late-reference-error", "empty-terminal", "truncated-tool"] {
        let mut events = vec![item_done.clone()];
        match case {
            "incomplete" => events.push(json!({"type":"response.incomplete","response":{"id":"bad","status":"incomplete","output":[tool_item()]}})),
            "failed" => events.push(json!({"type":"response.failed","response":{"error":{"message":"fixture failure"}}})),
            "cancelled" => events.extend([json!({"type":"response.output_text.delta","delta":"cancel now"}), text_response("bad")]),
            "late-reference-error" => events.push(json!({"type":"error","code":"response_not_found","message":"previous_response_id invalid"})),
            "empty-terminal" => events.push(json!({"type":"response.completed","response":{"id":"bad","status":"completed","output":[]}})),
            "truncated-tool" => events = vec![json!({"type":"response.output_item.added","output_index":0,"item":tool_item()}), json!({"type":"response.completed","response":{"id":"bad","status":"completed"}})],
            _ => {}
        }
        let mut bad = sse(&events);
        if case == "invalid-json" { bad.push_str("data: {broken\n\n"); }
        let (url, server) = server(vec![(200, sse(&[text_response("first")])), (200, bad), (200, sse(&[text_response("recovered")]))]).await;
        let model = configured(url);
        let session = format!("bad-{case}");
        let cancel = Arc::new(AtomicBool::new(false));
        let mut observed = Vec::new();
        let mut messages = vec![user("start")];
        let first = invoke(&model, &messages, &session, &cancel, &mut observed).await.unwrap();
        messages.extend([assistant(&model, &first), user("next")]);
        observed.clear();
        let result = stream_chat(&reqwest::Client::builder().no_proxy().build().unwrap(), &model, "fixture-key", None, "sys",
            &messages, &[], Some(&session), &cancel, &mut |e| {
                if case == "cancelled" && matches!(e, StreamEvent::TextDelta(_)) { cancel.store(true, Ordering::SeqCst); }
                observed.push(e);
            }).await;
        assert_eq!(tool_count(&observed), 0, "{case}");
        match case {
            "cancelled" => assert_eq!(result.unwrap().stop_reason, "aborted"),
            "incomplete" => assert!(result.unwrap().content.iter().all(|p| p["type"] != "toolCall")),
            "empty-terminal" => assert_eq!(result.unwrap().stop_reason, "error"),
            _ => assert!(result.is_err(), "{case}"),
        }
        cancel.store(false, Ordering::SeqCst);
        invoke(&model, &messages, &session, &cancel, &mut observed).await.unwrap();
        let requests = server.await.unwrap();
        assert_eq!(requests.len(), 3, "no in-call retry after output: {case}");
        assert_eq!(requests[1]["previous_response_id"], "first");
        assert!(requests[2].get("previous_response_id").is_none(), "{case}");
        assert_eq!(requests[2]["input"], json!(responses_input(&messages, &model)));
    }
}

#[tokio::test]
async fn manual_ids_and_store_false_and_codex_never_enter_automatic_cache() {
    for case in ["manual", "store-false", "codex", "off"] {
        let (url, server) = server(vec![(200, sse(&[text_response("first")])), (200, sse(&[text_response("second")]))]).await;
        let mut model = configured(url);
        match case {
            "manual" => { model.extra_options.insert("previous_response_id".into(), json!("manual-id")); },
            "store-false" => { model.extra_options.insert("store".into(), json!(false)); },
            "codex" => model.local_auth = Some("codex".into()),
            "off" => { model.extra_options.remove("responsesContinuation"); },
            _ => unreachable!()
        }
        let session = format!("mode-{case}");
        let cancel = Arc::new(AtomicBool::new(false));
        let mut messages = vec![user("start")];
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        // Bypass only local credential loading: exercise real Codex body/store policy on loopback.
        let first = stream_responses(&http, &model, "fixture-key", responses_body(&model, "sys", &messages, &[], None, Some(&session)), Some(&session), &cancel, &mut |_| {}).await.unwrap();
        messages.extend([assistant(&model, &first), user("next")]);
        stream_responses(&http, &model, "fixture-key", responses_body(&model, "sys", &messages, &[], None, Some(&session)), Some(&session), &cancel, &mut |_| {}).await.unwrap();
        let requests = server.await.unwrap();
        for request in &requests {
            if case == "manual" { assert_eq!(request["previous_response_id"], "manual-id"); }
            else { assert!(request.get("previous_response_id").is_none(), "{case}"); }
        }
        assert_eq!(requests[1]["input"], json!(responses_input(&messages, &model)));
        if case == "codex" { assert_eq!(requests[1]["store"], false); }
    }
}

#[tokio::test]
async fn nonstream_json_continuation_uses_completed_response_id() {
    let (url, server) = server(vec![(200, text_response("json_first")["response"].to_string()), (200, text_response("json_second")["response"].to_string())]).await;
    let model = configured(url);
    let cancel = Arc::new(AtomicBool::new(false));
    let mut events = Vec::new();
    let mut messages = vec![user("start")];
    let first = invoke(&model, &messages, "json-mode", &cancel, &mut events).await.unwrap();
    messages.extend([assistant(&model, &first), user("next")]);
    invoke(&model, &messages, "json-mode", &cancel, &mut events).await.unwrap();
    let requests = server.await.unwrap();
    assert_eq!(requests[1]["previous_response_id"], "json_first");
    assert_eq!(requests[1]["input"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn dropped_inflight_future_cannot_leave_a_reusable_checkpoint() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let model = configured(format!("http://{}", listener.local_addr().unwrap()));
    let cancel = Arc::new(AtomicBool::new(false));
    let mut seed_events = Vec::new();
    let mut messages = vec![user("start")];
    // Seed a checkpoint using this same endpoint; then stall the next request before headers.
    let (started, observed) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let mut requests = Vec::new();
        for i in 0..3 {
            let (mut socket, _) = tokio::time::timeout(std::time::Duration::from_secs(5), listener.accept()).await.unwrap().unwrap();
            let mut bytes = vec![0; 16384];
            let n = socket.read(&mut bytes).await.unwrap();
            requests.push(String::from_utf8_lossy(&bytes[..n]).into_owned());
            if i == 1 {
                started.send(()).unwrap();
                // The final request can only arrive after the in-flight client future is dropped.
                let (mut last, _) = tokio::time::timeout(std::time::Duration::from_secs(5), listener.accept()).await.unwrap().unwrap();
                let n = last.read(&mut bytes).await.unwrap();
                requests.push(String::from_utf8_lossy(&bytes[..n]).into_owned());
                let payload = sse(&[text_response("last")]);
                last.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}", payload.len()).as_bytes()).await.unwrap();
                break;
            }
            let payload = sse(&[text_response("seed")]);
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}", payload.len()).as_bytes()).await.unwrap();
        }
        requests
    });
    let first = invoke(&model, &messages, "dropped-future", &cancel, &mut seed_events).await.unwrap();
    messages.extend([assistant(&model, &first), user("next")]);
    {
        let mut events = Vec::new();
        let pending = invoke(&model, &messages, "dropped-future", &cancel, &mut events);
        tokio::pin!(pending);
        tokio::select! { _ = observed => {}, result = &mut pending => panic!("request must still be pending: {result:?}") }
    }
    invoke(&model, &messages, "dropped-future", &cancel, &mut seed_events).await.unwrap();
    let requests = server.await.unwrap();
    assert!(requests[1].contains("previous_response_id"));
    assert!(!requests[2].contains("previous_response_id"));
}

#[tokio::test]
async fn run_turn_preserves_reasoning_across_user_turns_and_dispatches_tools_once() {
    use crate::lyra::{history::user_message, turn::{run_turn, tests::test_session, TurnEvent}};
    for stored in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a"), "fixture contents").unwrap();
        let item = tool_item();
        let reasoning = json!({"type":"reasoning","id":"rs_first","encrypted_content":"opaque","summary":[{"type":"summary_text","text":"thinking"}]});
        let tools = sse(&[
            json!({"type":"response.output_item.added","output_index":1,"item":item}),
            json!({"type":"response.output_item.done","output_index":1,"item":item}),
            json!({"type":"response.output_item.done","output_index":1,"item":item}),
            json!({"type":"response.completed","response":{"id":"turn_tools","status":"completed","output":[reasoning,item]}}),
        ]);
        let (url, server) = server(vec![(200, tools), (200, sse(&[text_response("turn_end")])), (200, sse(&[text_response("next_turn")]))]).await;
        let mut session = test_session(url.clone(), dir.path().into());
        session.model.model = configured(url);
        session.model.model.extra_options.insert("store".into(), json!(stored));
        session.model.api_key = "fixture-key".into();
        session.session_id = format!("e2e-reasoning-{stored}");
        session.tools.push(Tool { name: "read", description: "Read file".into(), parameters: json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}) });
        let mut ended = Vec::new();
        let first = run_turn(&mut session, Some(user_message("read a", &[])), &mut |e| {
            if let TurnEvent::ToolEnd { id, .. } = e { ended.push(id); }
        }).await;
        assert!(first.error.is_none() && !first.cancelled, "{first:?}");
        assert_eq!(ended, ["call_1|fc_1"]);
        let results: Vec<_> = session.history.items().iter().filter(|m| m["role"] == "toolResult").collect();
        assert_eq!(results.len(), 1);
        assert!(results[0]["content"].to_string().contains("fixture contents"));
        assert!(session.history.items().iter().any(|m| m["content"].as_array().is_some_and(|parts| parts.iter().any(|p| p["type"] == "thinking"))));
        let next = run_turn(&mut session, Some(user_message("next", &[])), &mut |_| {}).await;
        assert!(next.error.is_none() && !next.cancelled, "{next:?}");
        let requests = server.await.unwrap();
        if stored {
            assert_eq!(requests[1]["previous_response_id"], "turn_tools");
            assert_eq!(requests[1]["input"].as_array().unwrap().len(), 1);
            assert_eq!(requests[1]["input"][0]["type"], "function_call_output");
            assert_eq!(requests[2]["previous_response_id"], "turn_end");
            assert_eq!(requests[2]["input"].as_array().unwrap().len(), 1);
            assert_eq!(requests[2]["input"][0]["content"][0]["text"], "next");
        } else {
            assert!(requests[2].get("previous_response_id").is_none());
            let input = requests[2]["input"].as_array().unwrap();
            let replay_reasoning = input.iter().find(|p| p["type"] == "reasoning").unwrap();
            let mut expected_reasoning = reasoning.clone();
            expected_reasoning.as_object_mut().unwrap().remove("id");
            assert_eq!(replay_reasoning, &expected_reasoning);
            let replay_call = input.iter().find(|p| p["type"] == "function_call").unwrap();
            assert_eq!(replay_call["call_id"], "call_1");
            assert!(replay_call.get("id").is_none(), "store=false omits upstream item IDs, not reasoning state");
            let prefix = requests[1]["input"].as_array().unwrap();
            assert_eq!(&input[..prefix.len()], prefix, "full replay preserves the previous input prefix");
        }
    }
}

#[tokio::test]
async fn run_turn_reuses_checkpoint_across_user_turns_without_history_rewrite() {
    use crate::lyra::{history::user_message, turn::{run_turn, tests::test_session}};
    let dir = tempfile::tempdir().unwrap();
    let (url, server) = server(vec![(200, sse(&[text_response("plain_first")])), (200, sse(&[text_response("plain_next")]))]).await;
    let mut session = test_session(url.clone(), dir.path().into());
    session.model.model = configured(url);
    session.session_id = "e2e-plain".into();
    for input in ["one", "two"] {
        let outcome = run_turn(&mut session, Some(user_message(input, &[])), &mut |_| {}).await;
        assert!(outcome.error.is_none() && !outcome.cancelled, "{outcome:?}");
    }
    let requests = server.await.unwrap();
    assert_eq!(requests[1]["previous_response_id"], "plain_first");
    assert_eq!(requests[1]["input"].as_array().unwrap().len(), 1);
    assert_eq!(requests[1]["input"][0]["content"][0]["text"], "two");
}

#[test]
fn client_options_are_retained_for_counting_but_never_forwarded() {
    let model = configured("https://fixture.invalid".into());
    for body in [completions_body(&model, "sys", &[], &[], None, None), responses_body(&model, "sys", &[], &[], None, None), anthropic_body(&model, "sys", &[], &[], None)] {
        for key in ["tokenizer", "responsesContinuation", "supportsPreviousResponseId"] { assert!(body.get(key).is_none()); }
    }
    let native = native_transport_model(&model);
    assert!(native.extra_options.keys().all(|key| !client_only_option(key)));
    assert_eq!(model.extra_options["tokenizer"], "o200k_base");
    let body = json!({"messages":[{"role":"user","content":"code \\ \"中文\" <|endoftext|>"}],"tools":[{"schema":{"type":"object"}}]});
    assert!(request_input_budget(&body) >= crate::lyra::tokens::TokenCounter.count_json(&body));
}
