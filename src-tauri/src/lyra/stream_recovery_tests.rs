use super::*;
use super::tests::test_model;
use std::io::Write;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn sse(events: &[Value]) -> String {
    events.iter().map(|event| format!("data: {event}\n\n")).collect()
}

fn completed(api: &str, tool: bool) -> Vec<Value> {
    match api {
        "openai-responses" => vec![
            json!({"type":"response.output_text.delta","delta":"ok"}),
            json!({"type":"response.completed","response":{"status":"completed","usage":{"input_tokens":11,"output_tokens":7},
                "output":if tool { json!([{"type":"function_call","call_id":"call","id":"fc","name":"bash","arguments":"{\"command\":\"echo safe\"}"}]) }
                else { json!([{"type":"message","content":[{"type":"output_text","text":"ok"}]}]) }}}),
        ],
        "anthropic-messages" => vec![
            json!({"type":"content_block_start","index":0,"content_block":if tool {
                json!({"type":"tool_use","id":"call","name":"bash","input":{}})
            } else { json!({"type":"text","text":""}) }}),
            json!({"type":"content_block_delta","index":0,"delta":if tool {
                json!({"type":"input_json_delta","partial_json":"{\"command\":\"echo safe\"}"})
            } else { json!({"type":"text_delta","text":"ok"}) }}),
            json!({"type":"content_block_stop","index":0}),
            json!({"type":"message_delta","delta":{"stop_reason":if tool { "tool_use" } else { "end_turn" }},"usage":{"output_tokens":7}}),
            json!({"type":"message_stop"}),
        ],
        _ => vec![
            json!({"choices":[{"index":0,"delta":if tool {
                json!({"tool_calls":[{"index":0,"id":"call","type":"function","function":{"name":"bash","arguments":"{\"command\":\"echo safe\"}"}}]})
            } else { json!({"content":"ok"}) },"finish_reason":null}]}),
            json!({"choices":[{"index":0,"delta":{},"finish_reason":if tool { "tool_calls" } else { "stop" }}]}),
        ],
    }
}

// Acknowledge each body segment before breaking the transport, so the test does not depend on TCP coalescing.
async fn invoke(api: &str, segments: Vec<String>, tail: &str, cancel_on_text: bool) -> Result<StreamResult, String> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (activity, mut received) = tokio::sync::mpsc::unbounded_channel();
    let gzip = tail == "gzip";
    let hold = tail == "hold";
    let tail = tail.to_string();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        loop {
            let mut bytes = [0; 8192];
            let count = socket.read(&mut bytes).await.unwrap();
            assert!(count > 0);
            request.extend_from_slice(&bytes[..count]);
            if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&request[..end]);
                let size = head.lines().find_map(|line| {
                    let (key, value) = line.split_once(':')?;
                    key.eq_ignore_ascii_case("content-length").then(|| value.trim().parse::<usize>().unwrap())
                }).unwrap_or(0);
                if request.len() >= end + 4 + size { break; }
            }
        }
        let encoding = if gzip { "Content-Encoding: gzip\r\n" } else { "" };
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n{encoding}Connection: close\r\n\r\n").as_bytes()).await.unwrap();
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        for segment in segments {
            let bytes = if gzip {
                encoder.write_all(segment.as_bytes()).unwrap();
                encoder.flush().unwrap();
                std::mem::take(encoder.get_mut())
            } else { segment.into_bytes() };
            socket.write_all(format!("{:x}\r\n", bytes.len()).as_bytes()).await.unwrap();
            socket.write_all(&bytes).await.unwrap();
            socket.write_all(b"\r\n").await.unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(3), received.recv()).await.unwrap();
        }
        match tail.as_str() {
            "hold" => std::future::pending::<()>().await,
            // Finish HTTP without the gzip footer: reqwest must report a decompression error.
            "normal" | "gzip" => socket.write_all(b"0\r\n\r\n").await.unwrap(),
            "invalid-utf8" => socket.write_all(b"9\r\ndata: \xff\n\n\r\n0\r\n\r\n").await.unwrap(),
            "disconnect" => {}, // Missing terminating HTTP chunk.
            _ => panic!("unknown fixture tail"),
        }
    });
    let mut model = test_model(api);
    model.base_url = format!("http://{address}");
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let mut on_event = |event| {
        if matches!(event, StreamEvent::Activity) { let _ = activity.send(()); }
        if cancel_on_text && matches!(event, StreamEvent::TextDelta(_)) { cancel.store(true, Ordering::SeqCst); }
    };
    let request = async {
        match api {
            "openai-responses" => stream_responses(&http, &model, "", json!({}), None, &cancel, &mut on_event).await,
            "anthropic-messages" => stream_anthropic(&http, &model, "", json!({}), None, &cancel, &mut on_event).await,
            _ => stream_completions(&http, &model, "", json!({}), None, &cancel, &mut on_event).await,
        }
    };
    let result = tokio::time::timeout(std::time::Duration::from_secs(3), request).await;
    if hold || result.is_err() { server.abort(); } else { server.await.unwrap(); }
    result.expect("protocol terminal must not wait for connection close")
}

#[tokio::test]
async fn protocol_terminals_finish_without_waiting_for_http_close() {
    for api in ["openai-responses", "anthropic-messages"] {
        for tail in ["hold", "disconnect", "gzip"] {
            for tool in [false, true] {
                let result = invoke(api, vec![sse(&completed(api, tool))], tail, false).await.unwrap();
                assert_eq!(result.stop_reason, if tool { "toolUse" } else { "stop" }, "{api} {tail}");
                assert_eq!(result.usage["output"], 7, "{api} {tail}");
                if tool {
                    let calls: Vec<_> = result.content.iter().filter(|part| part["type"] == "toolCall").collect();
                    assert_eq!(calls.len(), 1);
                    assert_eq!(calls[0]["arguments"]["command"], "echo safe");
                } else {
                    assert_eq!(result.content[0]["text"], "ok");
                }
            }
        }
    }
}

#[tokio::test]
async fn completions_preserves_trailing_usage_and_recovers_only_transport_failures() {
    for tail in ["normal", "disconnect", "gzip"] {
        for tool in [false, true] {
            let mut usage = sse(&[json!({"choices":[],"usage":{"prompt_tokens":11,"completion_tokens":7}})]);
            if tail == "normal" { usage.push_str("data: [DONE]\n\n"); }
            let result = invoke("openai-completions", vec![sse(&completed("openai-completions", tool)), usage], tail, false).await.unwrap();
            assert_eq!(result.stop_reason, if tool { "toolUse" } else { "stop" });
            assert_eq!(result.usage["output"], 7);
            assert_eq!(result.content.iter().filter(|part| part["type"] == "toolCall").count(), usize::from(tool));
        }
    }
    let error = invoke("openai-completions", vec![sse(&completed("openai-completions", false))], "invalid-utf8", false).await.unwrap_err();
    assert!(error.contains("UTF-8"), "must not swallow SSE decoder errors: {error}");
}

#[tokio::test]
async fn incomplete_streams_and_explicit_errors_keep_existing_retry_path() {
    for api in ["openai-responses", "anthropic-messages", "openai-completions"] {
        let mut events = completed(api, false);
        events.pop();
        for tail in ["disconnect", "gzip"] {
            let error = invoke(api, vec![sse(&events)], tail, false).await.unwrap_err();
            assert!(is_retryable_stream_error(&error) || crate::lyra::prompt::is_retryable_provider_error(&error), "{api}: {error}");
        }
    }
    let mut events = completed("openai-completions", false);
    events.push(json!({"error":{"message":"Upstream stream ended prematurely; safe to retry"}}));
    let error = invoke("openai-completions", vec![sse(&events)], "normal", false).await.unwrap_err();
    assert!(error.contains("Upstream stream ended prematurely"));
    assert!(crate::lyra::prompt::is_retryable_provider_error(&error));
}

#[tokio::test]
async fn recovery_does_not_accept_invalid_or_unfinished_tools() {
    let mut events = completed("openai-completions", true);
    events[0]["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"] = json!("{broken");
    assert!(invoke("openai-completions", vec![sse(&events)], "disconnect", false).await.is_err());

    let mut events = completed("openai-responses", true);
    events[1]["response"]["output"][0]["arguments"] = json!("{broken");
    assert!(invoke("openai-responses", vec![sse(&events)], "disconnect", false).await.is_err());

    let mut events = completed("anthropic-messages", true);
    events[1]["delta"]["partial_json"] = json!("{broken");
    assert!(invoke("anthropic-messages", vec![sse(&events)], "disconnect", false).await.is_err());
    let mut events = completed("anthropic-messages", true);
    events.remove(2); // No content_block_stop: message_stop alone is not sufficient.
    assert!(invoke("anthropic-messages", vec![sse(&events)], "disconnect", false).await.is_err());
    let mut events = completed("anthropic-messages", false);
    events.remove(3); // Missing message_delta / stop_reason.
    assert!(invoke("anthropic-messages", vec![sse(&events)], "disconnect", false).await.is_err());
}

#[tokio::test]
async fn terminal_recovery_preserves_cancellation_and_incomplete_status() {
    for api in ["openai-responses", "anthropic-messages", "openai-completions"] {
        let result = invoke(api, vec![sse(&completed(api, false))], "disconnect", true).await.unwrap();
        assert_eq!(result.stop_reason, "aborted", "{api}");
    }
    let mut events = completed("openai-responses", true);
    events[1]["type"] = json!("response.incomplete");
    events[1]["response"]["status"] = json!("incomplete");
    let result = invoke("openai-responses", vec![sse(&events)], "hold", false).await.unwrap();
    assert_eq!(result.stop_reason, "length");
    assert!(result.content.iter().all(|part| part["type"] != "toolCall"));
}

#[tokio::test]
async fn codebuddy_keeps_strict_done_requirement() {
    let events = sse(&completed("codebuddy", false));
    for tail in ["normal", "disconnect", "gzip"] {
        assert!(invoke("codebuddy", vec![events.clone()], tail, false).await.is_err(), "{tail}");
    }
    let result = invoke("codebuddy", vec![format!("{events}data: [DONE]\n\n")], "normal", false).await.unwrap();
    assert_eq!(result.stop_reason, "stop");
}
