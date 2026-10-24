//! Real mock stdio tests. Requires Node (already used by this repository), no npm/SDK setup.
//! Included by mcp.rs so the integrator's `cargo test --lib lyra` runs these unchanged.
use super::*;
use std::sync::atomic::AtomicUsize;

struct Fixture {
    dir: tempfile::TempDir,
    config: McpConfig,
}
impl Fixture {
    fn new(mode: &str, read_tools: &[&str]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("mock.mjs");
        std::fs::write(&script, include_str!("mock.mjs")).unwrap();
        let config = config_from_root(&json!({"mcpServers":{"test":{
            "enabled":true,"command":"node","args":[script,mode,dir.path().join("trace.jsonl")],
            "startup_timeout_ms":3000,"request_timeout_ms":1500,"readOnlyTools":read_tools
        }}})).unwrap();
        Self { dir, config }
    }
    fn session(&self, read_only: bool) -> McpSession {
        McpSession::new(self.config.clone(), self.dir.path().to_owned(), read_only).unwrap()
    }
    fn events(&self) -> Vec<Value> {
        std::fs::read_to_string(self.dir.path().join("trace.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
    fn count(&self, method: &str) -> usize {
        self.events()
            .iter()
            .filter(|v| v["method"] == method)
            .count()
    }
    async fn wait_for(&self, method: &str) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while self.count(method) == 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("mock method was not reached");
    }
}

pub(super) fn uncancelled() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
}
pub(super) fn value(out: &ToolOutcome) -> Value {
    serde_json::from_str(out.content[0]["text"].as_str().unwrap()).unwrap()
}
pub(super) async fn run(session: &mut McpSession, args: Value) -> ToolOutcome {
    session.execute(args, &uncancelled(), |out| out).await
}
pub(super) async fn describe(session: &mut McpSession, tool: &str) -> ToolOutcome {
    run(
        session,
        json!({"op":"describe","server":"test","tool":tool}),
    )
    .await
}
pub(super) async fn call(session: &mut McpSession, tool: &str) -> ToolOutcome {
    run(
        session,
        json!({"op":"call","server":"test","tool":tool,"arguments":{}}),
    )
    .await
}
pub(super) fn assert_error(out: &ToolOutcome, code: &str) {
    assert!(out.is_error, "expected {code}: {}", value(out));
    assert_eq!(value(out)["error"]["code"], code, "{}", value(out));
}

#[tokio::test]
async fn progressive_initialize_pagination_describe_and_call() {
    let fixture = Fixture::new("normal", &[]);
    let mut session = fixture.session(false);
    assert!(session.is_enabled());
    let wrapper = tool();
    assert_eq!(wrapper.name, "mcp");
    assert!(!wrapper
        .parameters
        .to_string()
        .contains("LOOKUP_SCHEMA_ONLY"));
    let servers = run(&mut session, json!({"op":"list"})).await;
    assert_eq!(value(&servers)["servers"], json!(["test"]));
    assert!(
        fixture.events().is_empty(),
        "discovery must not launch services"
    );
    assert_error(&call(&mut session, "lookup").await, "not_described");
    assert!(fixture.events().is_empty());

    let first = run(&mut session, json!({"op":"list","server":"test","limit":1})).await;
    assert!(!first.is_error, "{}", value(&first));
    assert_eq!(value(&first)["tools"][0]["name"], "lookup");
    assert!(!json!(first.content).to_string().contains("Schema"));
    let second = run(
        &mut session,
        json!({"op":"list","server":"test","limit":1,"cursor":value(&first)["nextCursor"]}),
    )
    .await;
    assert_eq!(value(&second)["tools"][0]["name"], "mutate");
    assert_eq!(
        fixture.count("tools/list"),
        1,
        "local pagination must not refetch a page"
    );
    let third = run(
        &mut session,
        json!({"op":"list","server":"test","cursor":value(&second)["nextCursor"]}),
    )
    .await;
    assert_eq!(value(&third)["tools"][0]["name"], "huge");
    assert!(value(&third)["nextCursor"].is_null());
    assert!(json!(third.content).to_string().len() < 1000);
    assert_eq!(fixture.count("initialize"), 1);
    assert_eq!(fixture.count("notifications/initialized"), 1);
    assert_eq!(fixture.count("tools/list"), 2);

    let description = describe(&mut session, "lookup").await;
    assert!(!description.is_error, "{}", value(&description));
    assert!(json!(description.content)
        .to_string()
        .contains("LOOKUP_SCHEMA_ONLY"));
    assert!(!json!(description.content)
        .to_string()
        .contains("WRITE_SCHEMA_HIDDEN"));
    let result = call(&mut session, "lookup").await;
    assert!(!result.is_error, "{}", value(&result));
    assert_eq!(fixture.count("tools/call"), 1);
    assert_eq!(
        fixture.count("tools/list"),
        4,
        "call revalidates current inventory"
    );
    assert_error(&describe(&mut session, "missing").await, "unknown_tool");
    assert_error(&describe(&mut session, "huge").await, "schema_too_large");
    assert_error(&call(&mut session, "huge").await, "not_described");
}

#[tokio::test]
async fn read_only_ignores_annotations_and_requires_explicit_local_permission() {
    let fixture = Fixture::new("normal", &["lookup"]);
    let mut session = fixture.session(true);
    assert!(!describe(&mut session, "mutate").await.is_error);
    assert_error(&call(&mut session, "mutate").await, "read_only_denied");
    assert_eq!(fixture.count("tools/call"), 0);
    assert!(!describe(&mut session, "lookup").await.is_error);
    assert!(!call(&mut session, "lookup").await.is_error);
    assert_eq!(fixture.count("tools/call"), 1);
    let default = Fixture::new("normal", &[]);
    let mut denied = default.session(true);
    assert_error(&call(&mut denied, "lookup").await, "read_only_denied");
    assert!(default.events().is_empty());
}

#[tokio::test]
async fn server_requests_receive_valid_replies_without_deadlock() {
    let fixture = Fixture::new("client_request", &[]);
    let result = describe(&mut fixture.session(false), "lookup").await;
    assert!(!result.is_error, "{}", value(&result));
    let replies: Vec<_> = fixture
        .events()
        .into_iter()
        .filter(|v| v["event"] == "client_reply")
        .collect();
    assert_eq!(replies.len(), 2);
    assert!(replies.iter().all(|v| v["valid"] == true));
}

#[tokio::test]
async fn fail_closed_for_invalidated_changed_missing_and_oversized_tools() {
    for (mode, expected) in [
        ("changed", "tool_changed"),
        ("notification", "tool_changed"),
        ("disappeared", "unknown_tool"),
    ] {
        let fixture = Fixture::new(mode, &[]);
        let mut session = fixture.session(false);
        assert!(!describe(&mut session, "lookup").await.is_error);
        assert_error(&call(&mut session, "lookup").await, expected);
        assert_eq!(fixture.count("tools/call"), 0);
        assert_error(&call(&mut session, "lookup").await, "not_described");
    }
    for (mode, expected) in [
        ("oversize", "protocol_error"),
        ("bad_id", "protocol_error"),
        ("disconnect", "disconnected"),
        ("cycle", "pagination_limit"),
        ("bad_version", "protocol_version"),
    ] {
        let fixture = Fixture::new(mode, &[]);
        assert_error(
            &describe(&mut fixture.session(false), "lookup").await,
            expected,
        );
    }
}

#[tokio::test]
async fn list_pages_and_describe_have_hard_output_budgets() {
    let fixture = Fixture::new("wide_page", &[]);
    let mut session = fixture.session(false);
    let mut cursor = Value::Null;
    let mut names = BTreeSet::new();
    loop {
        let mut args = json!({"op":"list","server":"test","limit":50});
        if !cursor.is_null() {
            args["cursor"] = cursor;
        }
        let out = run(&mut session, args).await;
        assert!(!out.is_error, "{}", value(&out));
        assert!(out.content[0]["text"].as_str().unwrap().len() <= MAX_DISCLOSURE_BYTES);
        let page = value(&out);
        for tool in page["tools"].as_array().unwrap() {
            assert!(names.insert(tool["name"].as_str().unwrap().to_string()));
        }
        cursor = page["nextCursor"].clone();
        if cursor.is_null() {
            break;
        }
    }
    assert_eq!(names.len(), 55);
    assert_eq!(fixture.count("tools/list"), 1);
    let empty = Fixture::new("empty_page", &[]);
    let mut session = empty.session(false);
    let first = run(&mut session, json!({"op":"list","server":"test"})).await;
    assert_eq!(value(&first)["tools"], json!([]));
    let second = run(
        &mut session,
        json!({"op":"list","server":"test","cursor":value(&first)["nextCursor"]}),
    )
    .await;
    assert_eq!(value(&second)["tools"][0]["name"], "lookup");
}

#[tokio::test]
async fn errors_timeouts_and_no_automatic_call_replay() {
    for mode in ["tool_error", "rpc_error", "hang"] {
        let mut fixture = Fixture::new(mode, &[]);
        fixture
            .config
            .servers
            .get_mut("test")
            .unwrap()
            .request_timeout_ms = 300;
        let mut session = fixture.session(false);
        assert!(!describe(&mut session, "lookup").await.is_error);
        let out = call(&mut session, "lookup").await;
        match mode {
            "tool_error" => {
                assert!(out.is_error);
                assert_eq!(value(&out)["isError"], true);
            }
            "rpc_error" => assert_error(&out, "rpc_error"),
            _ => assert_error(&out, "timeout"),
        }
        assert_eq!(fixture.count("tools/call"), 1);
        match mode {
            // A JSON-RPC error is a complete exchange: connection and describe grant survive.
            "rpc_error" => assert_error(&call(&mut session, "lookup").await, "rpc_error"),
            "hang" => assert_error(&call(&mut session, "lookup").await, "not_described"),
            _ => {}
        }
        assert_eq!(fixture.count("tools/call"), 1 + usize::from(mode == "rpc_error"));
        assert_eq!(fixture.count("initialize"), 1);
    }
    let mut fixture = Fixture::new("init_hang", &[]);
    fixture
        .config
        .servers
        .get_mut("test")
        .unwrap()
        .startup_timeout_ms = 150;
    assert_error(
        &describe(&mut fixture.session(false), "lookup").await,
        "timeout",
    );
}

#[tokio::test]
async fn isolation_cursors_close_and_validation() {
    let fixture = Fixture::new("normal", &[]);
    let mut a = fixture.session(false);
    let mut b = fixture.session(false);
    assert!(!describe(&mut a, "lookup").await.is_error);
    assert_error(&call(&mut b, "lookup").await, "not_described");
    let page = run(&mut a, json!({"op":"list","server":"test","limit":1})).await;
    assert_error(
        &run(
            &mut b,
            json!({"op":"list","server":"test","cursor":value(&page)["nextCursor"]}),
        )
        .await,
        "invalid_cursor",
    );
    assert_error(
        &run(&mut a, json!({"op":"list","server":"test","limit":0})).await,
        "invalid_limit",
    );
    assert_error(
        &run(&mut a, json!({"op":"list","server":"absent"})).await,
        "unknown_server",
    );
    assert_error(
        &run(
            &mut a,
            json!({"op":"call","server":"test","tool":"lookup","arguments":[]}),
        )
        .await,
        "invalid_arguments",
    );
    assert_error(
        &run(
            &mut a,
            json!({"op":"call","server":"test","tool":"lookup","approved":true}),
        )
        .await,
        "invalid_arguments",
    );
    a.close();
    assert_error(&call(&mut a, "lookup").await, "not_described");
}

#[test]
fn config_is_opt_in_strict_and_http_is_lazy() {
    let config = config_from_root(&json!({"mcpServers":{"off":{"enabled":false,"url":"https://example.invalid/mcp"}}})).unwrap();
    let session = McpSession::new(config.clone(), PathBuf::from("."), false).unwrap();
    assert!(!session.is_enabled());
    let mut enabled = config;
    enabled.servers.get_mut("off").unwrap().enabled = true;
    assert!(McpSession::new(enabled, PathBuf::from("."), false)
        .unwrap()
        .is_enabled());
    assert!(validate_config(&json!({"mcpServers":{"bad":{"enabled":true,"command":"node","unknown":true}}})).is_err());
    let mut fixture = Fixture::new("normal", &[]);
    fixture
        .config
        .servers
        .get_mut("test")
        .unwrap()
        .request_timeout_ms = 0;
    assert!(McpSession::new(fixture.config, PathBuf::from("."), false).is_err());
}

#[tokio::test]
async fn secrets_are_redacted_before_mandatory_governance_and_archival() {
    let key = format!("LYRA_MCP_TEST_{}", uuid::Uuid::new_v4().simple());
    let secret = "top-secret-token-中文-123";
    std::env::set_var(&key, secret);
    for mode in ["secret", "rpc_error"] {
        let mut fixture = Fixture::new(mode, &[]);
        let Transport::Stdio { env, .. } =
            &mut fixture.config.servers.get_mut("test").unwrap().transport
        else {
            unreachable!()
        };
        env.insert("MCP_TEST_SECRET".into(), format!("{{env:{key}}}"));
        let mut session = fixture.session(false);
        let out = describe(&mut session, "lookup").await;
        assert!(!out.is_error, "{}", value(&out));
        assert!(!json!(out.content).to_string().contains(secret));
        let invoked = AtomicUsize::new(0);
        let out = session
            .execute(
                json!({"op":"call","server":"test","tool":"lookup"}),
                &uncancelled(),
                |out| {
                    invoked.fetch_add(1, Ordering::SeqCst);
                    let text = json!(out.content).to_string();
                    assert!(!text.contains(secret));
                    assert!(text.contains("[REDACTED]"));
                    out
                },
            )
            .await;
        assert_eq!(invoked.load(Ordering::SeqCst), 1);
        assert_eq!(out.is_error, mode == "rpc_error");
    }
    std::env::remove_var(key);
    assert_eq!(
        redact_string("abc", &["abc".into(), "REDACTED".into()]),
        "[REDACTED]"
    );
    let fixture = Fixture::new("large_result", &[]);
    let mut session = fixture.session(false);
    assert!(!describe(&mut session, "lookup").await.is_error);
    let out = session
        .execute(
            json!({"op":"call","server":"test","tool":"lookup"}),
            &uncancelled(),
            |mut out| {
                assert!(out.content[0]["text"].as_str().unwrap().len() > 32000);
                out.content = vec![json!({"type":"text","text":"governed"})];
                out
            },
        )
        .await;
    assert_eq!(out.content[0]["text"], "governed");
}

#[tokio::test]
async fn cancellation_closes_connection_and_does_not_replay() {
    let fixture = Fixture::new("hang", &[]);
    let mut session = fixture.session(false);
    assert!(!describe(&mut session, "lookup").await.is_error);
    let cancel = uncancelled();
    let signal = async {
        fixture.wait_for("tools/call").await;
        cancel.store(true, Ordering::SeqCst);
    };
    let (out, _) = tokio::join!(
        session.execute(
            json!({"op":"call","server":"test","tool":"lookup"}),
            &cancel,
            |out| out
        ),
        signal
    );
    assert_error(&out, "cancelled");
    assert_error(&call(&mut session, "lookup").await, "not_described");
    assert_eq!(fixture.count("tools/call"), 1);
    let early = Fixture::new("normal", &[]);
    let mut session = early.session(false);
    let out = session
        .execute(
            json!({"op":"list","server":"test"}),
            &Arc::new(AtomicBool::new(true)),
            |out| out,
        )
        .await;
    assert_error(&out, "cancelled");
    assert!(early.events().is_empty());
}

#[test]
fn flat_config_validates_without_io_and_resolves_only_at_session_creation() {
    let key = format!("LYRA_MCP_CONFIG_{}", uuid::Uuid::new_v4().simple());
    let root = json!({"unrelated":true,"mcpServers":{
        "local":{"enabled":true,"command":"does-not-exist","args":["arg"],"cwd":std::env::temp_dir(),
            "env":{"TOKEN":format!("prefix-{{env:{key}}}")},"readOnlyTools":["lookup"]},
        "remote":{"enabled":true,"url":"https://example.invalid/mcp","headers":{"Authorization":format!("Bearer {{env:{key}}}")}}
    }});
    validate_config(&root).unwrap();
    // A missing {env:X} degrades only the affected servers; the session still starts.
    let mut degraded = McpSession::new(config_from_root(&root).unwrap(), PathBuf::from("."), true).unwrap();
    let listed = tokio::runtime::Runtime::new().unwrap().block_on(async {
        let listed = run(&mut degraded, json!({"op":"list"})).await;
        let unavailable = run(&mut degraded, json!({"op":"list","server":"local"})).await;
        assert_error(&unavailable, "server_unavailable");
        assert!(value(&unavailable)["error"]["message"].as_str().unwrap().contains(&key));
        value(&listed)
    });
    assert_eq!(listed["servers"], json!(["local", "remote"]));
    assert!(listed["unavailable"]["remote"].as_str().unwrap().contains(&key));
    std::env::set_var(&key, "not-in-history");
    let config = config_from_root(&root).unwrap();
    let session = McpSession::new(config, PathBuf::from("."), true).unwrap();
    assert_eq!(
        session.servers["local"].env.as_ref().unwrap()["TOKEN"],
        "prefix-not-in-history"
    );
    assert_eq!(
        session.servers["remote"].env.as_ref().unwrap()["Authorization"],
        "Bearer not-in-history"
    );
    assert!(session.servers["local"]
        .config
        .read_only_tools
        .contains("lookup"));
    assert!(session.secrets.contains(&"not-in-history".to_string()));
    assert!(session.servers.values().all(|s| s.connection.is_none()));
    std::env::remove_var(&key);
    for entry in [
        json!({"command":"x","url":"https://example.invalid"}),
        json!({"url":"file:///tmp/x"}),
        json!({"url":"https://example.invalid","headers":{"Mcp-Session-Id":"spoof"}}),
        json!({"command":"x","enabled":"true"}),
        json!({"command":"x","readOnlyTools":true}),
        json!({"command":"x","env":{"T":"{env:broken"}}),
        json!({"command":"x","env":{"T":12}}),
        json!({"command":"x","request_timeout_ms":0}),
        json!({"command":"x"}),
        json!({"enabled":true,"command":"x","cwd":"relative"}),
        json!({"enabled":true,"command":"x","cwd":""}),
        json!({"enabled":true,"command":"x","cwd":null}),
        json!({"enabled":true,"command":null,"url":"https://example.invalid"}),
        json!({"enabled":true,"command":"x","headers":{}}),
        json!({"enabled":true,"url":"https://example.invalid","args":[]}),
        json!({"enabled":true,"command":"x","readOnlyTools":[" "]}),
        json!({"enabled":true,"url":"http://example.invalid/mcp"}),
        json!({"enabled":true,"url":"http://10.0.0.1/mcp"}),
    ] {
        assert!(validate_config(&json!({"mcpServers":{"bad":entry}})).is_err());
    }
    for url in ["http://localhost:1/mcp", "http://127.0.0.1/mcp", "http://[::1]:8/mcp", "https://example.invalid/mcp"] {
        assert!(validate_config(&json!({"mcpServers":{"ok":{"enabled":true,"url":url}}})).is_ok(), "{url}");
    }
    assert!(validate_config(&json!({})).is_ok());
    assert!(validate_config(&json!({"mcpServers":null})).is_err());
}

#[tokio::test]
async fn search_is_substring_bounded_and_reports_unscanned_pages() {
    let fixture = Fixture::new("normal", &[]);
    let mut session = fixture.session(false);
    let out = run(
        &mut session,
        json!({"op":"list","server":"test","query":"WRITES"}),
    )
    .await;
    assert_eq!(value(&out)["tools"][0]["name"], "mutate");
    assert_eq!(value(&out)["moreAvailable"], true);
    assert!(!json!(out.content)
        .to_string()
        .contains("WRITE_SCHEMA_HIDDEN"));
    let next = run(
        &mut session,
        json!({"op":"list","server":"test","query":"WRITES","cursor":value(&out)["nextCursor"]}),
    )
    .await;
    assert_eq!(value(&next)["tools"], json!([]));
    assert_eq!(value(&next)["moreAvailable"], false);
    let fixture = Fixture::new("search_pages", &[]);
    let mut session = fixture.session(false);
    let first = run(
        &mut session,
        json!({"op":"list","server":"test","query":"NEEDLE"}),
    )
    .await;
    assert!(!first.is_error, "{}", value(&first));
    assert_eq!(value(&first)["scannedPages"], 32);
    assert_eq!(value(&first)["scanLimitReached"], true);
    assert_eq!(value(&first)["moreAvailable"], true);
    assert_eq!(value(&first)["tools"], json!([]));
    let second = run(
        &mut session,
        json!({"op":"list","server":"test","query":"needle","cursor":value(&first)["nextCursor"]}),
    )
    .await;
    assert_eq!(value(&second)["tools"][0]["name"], "lookup");
    assert_eq!(value(&second)["moreAvailable"], false);
    assert_eq!(value(&second)["scannedPages"], 2);
    assert!(!describe(&mut session, "lookup").await.is_error);
    assert!(!call(&mut session, "lookup").await.is_error);
}

#[cfg(target_os = "linux")]
async fn assert_processes_stopped(fixture: &Fixture) {
    let pids: Vec<_> = fixture
        .events()
        .iter()
        .filter_map(|v| v["pid"].as_u64())
        .collect();
    assert!(!pids.is_empty());
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let alive = pids.iter().any(|pid| {
                std::fs::read_to_string(format!("/proc/{pid}/stat"))
                    .ok()
                    .is_some_and(|s| {
                        // A killed orphan may remain a zombie briefly until its parent/init reaps it.
                        !s.rsplit_once(')').unwrap().1.trim_start().starts_with('Z')
                    })
            });
            if !alive {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("MCP process tree survived cleanup");
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn timeout_abort_and_idle_drop_kill_process_trees() {
    let mut fixture = Fixture::new("descendant", &[]);
    fixture
        .config
        .servers
        .get_mut("test")
        .unwrap()
        .request_timeout_ms = 200;
    let mut session = fixture.session(false);
    assert!(!describe(&mut session, "lookup").await.is_error);
    assert_error(&call(&mut session, "lookup").await, "timeout");
    assert_processes_stopped(&fixture).await;

    let fixture = Fixture::new("descendant", &[]);
    let mut session = fixture.session(false);
    assert!(!describe(&mut session, "lookup").await.is_error);
    let task = tokio::spawn(async move { call(&mut session, "lookup").await });
    fixture.wait_for("tools/call").await;
    task.abort();
    assert!(task.await.err().unwrap().is_cancelled());
    assert_processes_stopped(&fixture).await;

    let fixture = Fixture::new("descendant", &[]);
    let mut session = fixture.session(false);
    assert!(!describe(&mut session, "lookup").await.is_error);
    drop(session);
    assert_processes_stopped(&fixture).await;
}

#[test]
fn only_expanded_long_values_are_secrets_and_keys_never_collide() {
    let key = format!("LYRA_MCP_SHORT_{}", uuid::Uuid::new_v4().simple());
    std::env::set_var(&key, "1");
    let mut secrets = Vec::new();
    configuration::resolve(&config_from_root(&json!({"mcpServers":{"s":{"enabled":true,"command":"x",
        "env":{"A":"demo","B":"literal-but-long","C":format!("{{env:{key}}}"),"D":format!("x{{env:{key}}}-suffix")}}}})).unwrap().servers["s"], &mut secrets).unwrap();
    std::env::remove_var(&key);
    assert_eq!(secrets, vec!["x1-suffix".to_string()]);
    let mut v = json!({"a-secret-1":1,"a-secret-2":2,"[REDACTED]":3});
    redact(&mut v, &["a-secret-1".into(), "a-secret-2".into()]);
    assert_eq!(v.as_object().unwrap().len(), 3, "{v}");
}

#[test]
fn unset_cwd_is_pinned_to_the_initial_directory() {
    let mut config = config_from_root(&json!({"mcpServers":{"s":{"enabled":true,"command":"x"}}})).unwrap();
    let root = std::env::temp_dir();
    config.pin_cwd(&root);
    let Transport::Stdio { cwd, .. } = &config.servers["s"].transport else { unreachable!() };
    assert_eq!(cwd.as_deref(), Some(root.as_path()));
}

#[tokio::test]
async fn log_notifications_do_not_exhaust_limit_and_batch_version_is_refused() {
    let fixture = Fixture::new("logs", &[]);
    let out = describe(&mut fixture.session(false), "lookup").await;
    assert!(!out.is_error, "{}", value(&out));
    let fixture = Fixture::new("batch_version", &[]);
    assert_error(&describe(&mut fixture.session(false), "lookup").await, "protocol_version");
}
