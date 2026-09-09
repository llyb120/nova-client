use super::*;
use std::sync::{Mutex, OnceLock};

fn fixture() -> &'static Path {
    static EXE: OnceLock<PathBuf> = OnceLock::new();
    EXE.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("nova-codex-fixture-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join(if cfg!(windows) { "peer.exe" } else { "peer" });
        let output = std::process::Command::new("rustc")
            .args(["--edition=2021", "--crate-name", "codex_stdio_fixture"])
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/codex_app_server/fixture.rs"))
            .arg("-o")
            .arg(&exe)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        exe
    })
}

fn options() -> Options {
    Options {
        ponytail: false,
        polaris: None,
    }
}
fn request() -> Value {
    json!({"action":"prompt","cwd":std::env::temp_dir(),"parts":[{"type":"text","text":"hello"}],"model":"test-model","reasoningEffort":"high"})
}

async fn exercise(
    scenario: &str,
    request: Value,
    early: Option<Value>,
) -> (Result<Value, String>, Vec<Value>, Vec<Value>) {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("rpc.jsonl");
    let mut command = Command::new(fixture());
    command.env("SCENARIO", scenario).env("RPC_LOG", &log);
    let (tx, rx) = mpsc::unbounded_channel();
    if let Some(control) = early {
        tx.send(control.to_string()).unwrap();
    }
    let events = Mutex::new(Vec::new());
    let result = tokio::time::timeout(Duration::from_secs(10), run(command, request, options(), rx, |value| {
        if value["type"] == "permission" { tx.send(json!({"action":"permission","requestId":value["permission"]["id"],"reply":"reject"}).to_string()).unwrap(); }
        events.lock().unwrap().push(value);
    })).await.expect("transport hung");
    let calls = std::fs::read_to_string(log)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    (result, events.into_inner().unwrap(), calls)
}

#[tokio::test]
async fn stdio_stream_and_early_completion() {
    let (result, events, calls) = exercise("stream", request(), None).await;
    let result = result.unwrap();
    assert_eq!(result["usage"]["input_tokens"], 100);
    assert_eq!(result["cancelled"], false);
    assert!(events.iter().any(|e| e["item"]["text"] == "hello world"));
    assert!(events.iter().any(|e| e["item"]["text"] == "thinking"));
    assert!(events
        .iter()
        .any(|e| e["plan"][0]["status"] == "in_progress"));
    assert_eq!(calls[0]["method"], "initialize");
    assert_eq!(calls[1]["method"], "initialized");
    assert_eq!(calls[2]["method"], "thread/start");
    assert_eq!(calls[3]["params"]["effort"], "high");
}

#[tokio::test]
async fn initialization_retains_cancel_and_resume_steer() {
    let (result, _, calls) = exercise("cancel", request(), Some(json!({"action":"cancel"}))).await;
    assert_eq!(result.unwrap()["cancelled"], true);
    assert!(calls
        .iter()
        .any(|c| c["method"] == "turn/interrupt" && c["params"]["turnId"] == "turn-1"));
    let mut req = request();
    req["sessionId"] = json!("previous");
    let (result, _, calls) = exercise(
        "steer",
        req,
        Some(json!({"action":"steer","parts":[{"type":"text","text":"next"}]})),
    )
    .await;
    assert!(result.is_ok());
    assert!(calls
        .iter()
        .any(|c| c["method"] == "thread/resume" && c["params"]["threadId"] == "previous"));
    assert!(calls.iter().any(|c| c["method"] == "turn/steer"
        && c["params"]["expectedTurnId"] == "turn-1"
        && c["params"]["input"][0]["text"] == "next"));
}

#[tokio::test]
async fn server_approval_id_does_not_collide_with_client_id() {
    let (result, events, calls) = exercise("approval", request(), None).await;
    assert!(result.is_ok());
    assert!(events.iter().any(|e| e["permission"]["id"] == "3"));
    assert!(calls
        .iter()
        .any(|c| c["id"] == 3 && c["result"]["decision"] == "decline"));
}

#[tokio::test]
async fn title_and_fork_use_native_transport() {
    let mut req = request();
    req["action"] = json!("title");
    req["prompt"] = json!("name it");
    let (result, events, calls) = exercise("stream", req, None).await;
    assert_eq!(result.unwrap(), "hello world");
    assert!(events.is_empty());
    assert_eq!(calls[2]["params"]["ephemeral"], true);
    assert_eq!(calls[3]["params"]["sandboxPolicy"]["type"], "readOnly");
    let mut req = request();
    req["action"] = json!("fork");
    req["sessionId"] = json!("source");
    req["retainedTurns"] = json!(2);
    let (result, _, calls) = exercise("stream", req, None).await;
    assert_eq!(result.unwrap(), "fork-1");
    assert_eq!(calls[3]["params"]["lastTurnId"], "second");
}

#[tokio::test]
async fn failures_surface_and_image_files_are_removed() {
    for scenario in ["failed", "exit", "malformed"] {
        let mut req = request();
        req["parts"] = json!([{"type":"image_data","data":"aGVsbG8=","name":"test.png"}]);
        let (result, _, calls) = exercise(scenario, req, None).await;
        assert!(result.is_err(), "{scenario}");
        if let Some(start) = calls.iter().find(|c| c["method"] == "turn/start") {
            let path = start["params"]["input"][0]["path"].as_str().unwrap();
            assert!(!Path::new(path).exists());
            assert!(!Path::new(path).parent().unwrap().exists());
        }
    }
}

#[test]
fn options_preserve_polaris_ponytail_and_plan_mode() {
    let mut req = request();
    req["mode"] = json!("plan");
    let options = Options {
        ponytail: true,
        polaris: Some(
            json!({"command":"nova","args":["__codex-mcp"],"env":{"NOVA_CONTEXT_SERVICE_TOKEN":"test"}}),
        ),
    };
    let value = thread_options(&req, &options);
    assert_eq!(value["sandbox"], "read-only");
    assert_eq!(
        value["config"]["mcp_servers.nova-tools"]["env"]["NOVA_TOOLS_READ_ONLY"],
        "1"
    );
    let instructions = value["developerInstructions"].as_str().unwrap();
    assert!(
        instructions.contains("polaris")
            && instructions.contains("ponytail:")
            && instructions.contains("do not modify files")
    );
    req["action"] = json!("title");
    let value = thread_options(&req, &options);
    assert_eq!(value["developerInstructions"], "");
    assert_eq!(value["config"]["mcp_servers.nova-tools"]["enabled"], false);
}

#[test]
fn tool_normalization_preserves_output_diff_and_errors() {
    let item = normalize_item(&json!({"id":"c","type":"commandExecution","status":"inProgress","aggregatedOutput":"out","exitCode":1})).unwrap();
    assert_eq!(item["aggregated_output"], "out");
    assert_eq!(item["exit_code"], 1);
    assert_eq!(item["status"], "in_progress");
    let item = normalize_item(
        &json!({"type":"fileChange","changes":[{"kind":{"type":"update"},"diff":"+new"}]}),
    )
    .unwrap();
    assert_eq!(item["changes"][0]["kind"], "update");
    assert_eq!(item["changes"][0]["diff"], "+new");
    let item = normalize_item(&json!({"type":"mcpToolCall","error":{"message":"bad"}})).unwrap();
    assert_eq!(item["error"]["message"], "bad");
}

#[test]
fn image_bytes_and_cleanup_survive_partial_failure() {
    let path;
    {
        let mut images = Images::default();
        let parts = images
            .input(&json!({"parts":[{"type":"image_data","data":"aGVsbG8=","name":"hello.jpg"}]}))
            .unwrap();
        path = PathBuf::from(parts[0]["path"].as_str().unwrap());
        assert_eq!(std::fs::read(&path).unwrap(), b"hello");
        assert!(images
            .input(&json!({"parts":[{"type":"image_data","data":"!"}]}))
            .is_err());
    }
    assert!(!path.exists());
}

#[tokio::test]
#[ignore = "requires an installed Codex CLI; uses only an isolated local model stub"]
async fn live_turn_resume_title_and_fork() {
    use tokio::io::AsyncReadExt;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let provider = tokio::spawn(async move {
        let mut requests = Vec::new();
        while requests.len() < 3 {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = BufReader::new(stream);
            let mut length = 0;
            let mut first = String::new();
            stream.read_line(&mut first).await.unwrap();
            loop {
                let mut line = String::new();
                stream.read_line(&mut line).await.unwrap();
                if line == "\r\n" || line.is_empty() {
                    break;
                }
                if let Some(value) = line.to_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap();
                }
            }
            let mut body = vec![0; length];
            stream.read_exact(&mut body).await.unwrap();
            if !first.contains("/responses ") {
                stream
                    .get_mut()
                    .write_all(
                        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    )
                    .await
                    .unwrap();
                continue;
            }
            requests.push(String::from_utf8(body).unwrap());
            let item = json!({"id":"msg_test","type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"stub response","annotations":[]}]});
            let events = [
                json!({"type":"response.created","response":{"id":"resp_test","status":"in_progress","output":[]}}),
                json!({"type":"response.output_item.added","output_index":0,"item":{"id":"msg_test","type":"message","role":"assistant","status":"in_progress","content":[]}}),
                json!({"type":"response.output_text.delta","item_id":"msg_test","output_index":0,"content_index":0,"delta":"stub response"}),
                json!({"type":"response.output_item.done","output_index":0,"item":item}),
                json!({"type":"response.completed","response":{"id":"resp_test","status":"completed","output":[item],"usage":{"input_tokens":12,"output_tokens":3,"total_tokens":15,"input_tokens_details":{"cached_tokens":0}}}}),
            ];
            let body = events
                .iter()
                .map(|event| {
                    format!(
                        "event: {}\ndata: {event}\n\n",
                        event["type"].as_str().unwrap()
                    )
                })
                .collect::<String>();
            let response = format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            stream
                .get_mut()
                .write_all(response.as_bytes())
                .await
                .unwrap();
        }
        requests
    });
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        format!(
            r#"
model = "test-model"
model_provider = "test"
web_search = "disabled"
[model_providers.test]
name = "Local test"
base_url = "http://{addr}"
wire_api = "responses"
requires_openai_auth = false
"#
        ),
    )
    .unwrap();
    let binary = executable("codex").unwrap();
    let command = || {
        let mut command = Command::new(&binary);
        command
            .arg("app-server")
            .current_dir(home.path())
            .env("CODEX_HOME", home.path())
            .env("OPENAI_API_KEY", "")
            .env("CODEX_API_KEY", "");
        command
    };
    let invoke = |req| {
        let command = command();
        async move {
            let (_tx, controls) = mpsc::unbounded_channel();
            let events = Mutex::new(Vec::new());
            let result = tokio::time::timeout(
                Duration::from_secs(30),
                run(
                    command,
                    req,
                    Options {
                        ponytail: true,
                        polaris: None,
                    },
                    controls,
                    |e| events.lock().unwrap().push(e),
                ),
            )
            .await
            .unwrap()
            .unwrap();
            (result, events.into_inner().unwrap())
        }
    };
    let mut req = request();
    req["cwd"] = json!(home.path());
    let (done, events) = invoke(req.clone()).await;
    assert_eq!(done["usage"]["input_tokens"], 12);
    assert!(events.iter().any(|e| e["item"]["text"] == "stub response"));
    let session = events.iter().find(|e| e["type"] == "ready").unwrap()["sessionId"].clone();
    req["sessionId"] = session.clone();
    req["parts"] = json!([{"type":"text","text":"continue"}]);
    let (done, events) = invoke(req.clone()).await;
    assert_eq!(done["usage"]["input_tokens"], 24);
    assert!(events.iter().any(|e| e["sessionId"] == session));
    req["action"] = json!("fork");
    req["retainedTurns"] = json!(1);
    let (fork, _) = invoke(req.clone()).await;
    assert!(fork
        .as_str()
        .is_some_and(|id| !id.is_empty() && fork != session));
    req["action"] = json!("title");
    req["sessionId"] = Value::Null;
    req["prompt"] = json!("Make a title");
    let (title, events) = invoke(req).await;
    assert_eq!(title, "stub response");
    assert!(events.is_empty());
    let requests = provider.await.unwrap();
    assert!(requests[0].contains("ponytail:"));
    assert!(requests[1].contains("hello") && requests[1].contains("continue"));
}

#[tokio::test]
#[ignore = "requires Codex CLI and cargo build --bin nova; no model request"]
async fn live_native_mcp_calls_rust_polaris() {
    let home = tempfile::tempdir().unwrap();
    let workspace = home.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::write(
        workspace.join("widget.rs"),
        "pub fn native_polaris_probe() -> &'static str { \"native context works\" }\n",
    )
    .unwrap();
    crate::nova_tools_native::context::set_data_root(home.path().join("nova-data"));
    let service = crate::context_service::ContextService::start(home.path()).unwrap();
    let exe = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join(if cfg!(windows) { "nova.exe" } else { "nova" });
    assert!(
        exe.is_file(),
        "Build nova before running the native MCP integration test"
    );
    let mut command = Command::new(executable("codex").unwrap());
    command
        .arg("app-server")
        .env("CODEX_HOME", home.path())
        .current_dir(&workspace)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x0800_0000);
    #[cfg(unix)]
    command.process_group(0);
    let mut child = ServerChild(command.spawn().unwrap());
    let mut stdin = child.0.stdin.take().unwrap();
    let mut lines = BufReader::new(child.0.stdout.take().unwrap()).lines();
    async fn rpc(
        stdin: &mut ChildStdin,
        lines: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
        id: u64,
        method: &str,
        params: Value,
    ) -> Value {
        write(stdin, json!({"id":id,"method":method,"params":params}))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let message: Value =
                    serde_json::from_str(&lines.next_line().await.unwrap().expect("Codex EOF"))
                        .unwrap();
                if message["id"] == id && message.get("method").is_none() {
                    assert!(message.get("error").is_none(), "{message}");
                    return message["result"].clone();
                }
            }
        })
        .await
        .expect("native MCP RPC timed out")
    }
    rpc(&mut stdin,&mut lines,1,"initialize",json!({"clientInfo":{"name":"nova_native_test","version":"1"},"capabilities":{"experimentalApi":true}})).await;
    write(&mut stdin, json!({"method":"initialized","params":{}}))
        .await
        .unwrap();
    let mut req = request();
    req["cwd"] = json!(workspace);
    let options = Options {
        ponytail: false,
        polaris: Some(
            json!({"command":exe,"args":["__codex-mcp"],"enabled":true,"required":true,"env":{
                "NOVA_CONTEXT_SERVICE_ENDPOINT":service.endpoint(),"NOVA_CONTEXT_SERVICE_TOKEN":service.token()
            }}),
        ),
    };
    let mut params = thread_options(&req, &options);
    params["ephemeral"] = json!(true);
    let thread =
        rpc(&mut stdin, &mut lines, 2, "thread/start", params).await["thread"]["id"].clone();
    let inventory = rpc(
        &mut stdin,
        &mut lines,
        3,
        "mcpServerStatus/list",
        json!({"threadId":thread}),
    )
    .await;
    assert!(
        inventory["data"].as_array().unwrap().iter().any(
            |server| server["name"] == "nova-tools" && server["tools"].get("polaris").is_some()
        ),
        "{inventory}"
    );
    let result=rpc(&mut stdin,&mut lines,4,"mcpServer/tool/call",json!({"threadId":thread,"server":"nova-tools","tool":"polaris","arguments":{"query":"native_polaris_probe"}})).await;
    assert!(
        result.to_string().contains("native context works"),
        "{result}"
    );
    drop(stdin);
    tokio::time::timeout(Duration::from_secs(5), child.0.wait())
        .await
        .unwrap()
        .unwrap();
}
