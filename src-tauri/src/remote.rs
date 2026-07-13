//! server 侧远程会话客户端（重构版）。
//!
//! 只有一个统一端点 `POST /v1/remote/sync`：一次请求既上传变化，也领取待办命令。
//! 上传内容分三块，各自按需发送、且尽量少传：
//!   - catalog（projects + models）：低频，仅在项目/模型可用性变化时上传。
//!   - threads（会话列表元数据）：小体积，任何标题/运行状态/结构变化时上传，不含正文。
//!   - focuses（被浏览器打开的会话正文）：只对「当前被查看」的会话流式上传，且只发尾部增量。
//! 服务端从不重建正文，只按下标拼接不透明条目，避免语义合并出错。
//! 空闲且无人查看时带 wait=true 让服务端长轮询命令；有活动时按 ACTIVE_INTERVAL 主动推送。
//! 所有请求体均 gzip，响应由 reqwest 自动解 gzip。

use crate::relay::{gzip_json, resolve_relay_server};
use crate::threads::{AgentKind, Item, Thread};
use crate::{is_running, AppState, SCRATCH_MARK};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};
use tokio::time::sleep;

const ACTIVE_INTERVAL: Duration = Duration::from_millis(400);
/// 处理完命令后，即使会话暂时还没登记 running 也继续主动推送这么久，
/// 避免异步启动窗口内漏掉会话开头的输出。
const COMMAND_WATCH_DURATION: Duration = Duration::from_secs(30);
const REMOTE_SCRATCH_PATH: &str = "__nova_scratch__";

#[derive(Clone, PartialEq, Eq)]
struct RemoteConfig {
    server: String,
    token: String,
    name: String,
    proxy: String,
    device_id: String,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct RemoteProject {
    path: String,
    name: String,
}

#[derive(Serialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct RemoteThreadMeta {
    id: String,
    title: String,
    cwd: String,
    agent_kind: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    model: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    mode: String,
    updated_at: i64,
    running: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CatalogOut {
    version: i64,
    projects: Vec<RemoteProject>,
    models: HashMap<String, Value>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ThreadsOut {
    version: i64,
    list: Vec<RemoteThreadMeta>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FocusOut {
    id: String,
    running: bool,
    base: usize,
    count: usize,
    version: i64,
    // plan 随每次正文更新一并携带（体积很小），服务端原样透传给浏览器渲染计划卡片。
    plan: Value,
    items: Vec<Value>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SyncRequest {
    device_name: String,
    ack_id: i64,
    wait: bool,
    results: Vec<CommandResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    catalog: Option<CatalogOut>,
    #[serde(skip_serializing_if = "Option::is_none")]
    threads: Option<ThreadsOut>,
    focuses: Vec<FocusOut>,
}

#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct RemoteCommand {
    id: i64,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    thread_id: String,
    #[serde(default)]
    cwd: String,
    #[serde(default)]
    agent_kind: String,
    #[serde(default)]
    model: String,
    #[serde(default)]
    mode: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    path: String,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct CommandResult {
    id: i64,
    ok: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    error: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    thread_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SyncResponse {
    #[serde(default)]
    commands: Vec<RemoteCommand>,
    #[serde(default)]
    focus_ids: Vec<String>,
    #[serde(default)]
    need_catalog: bool,
    #[serde(default)]
    need_threads: bool,
    #[serde(default)]
    need_full: Vec<String>,
}

/// 桌面端已上传到服务端的某个 focus 会话正文基线，用于计算下一次的尾部增量。
struct FocusState {
    items: Vec<Value>,
    version: i64,
}

pub fn start(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        run(app).await;
    });
}

async fn run(app: AppHandle) {
    let mut last_cfg: Option<RemoteConfig> = None;
    let mut client = Client::new();

    let mut catalog_version = 0i64;
    let mut threads_version = 0i64;
    let mut last_catalog_sig = String::new();
    let mut last_threads_sig = String::new();
    let mut need_catalog = true;
    let mut need_threads = true;

    // 服务端要求推送正文的会话集合（=浏览器当前查看的会话，通常只有一个）。
    let mut focus_ids: HashSet<String> = HashSet::new();
    // 需要重发整包（服务端缺基线）的会话。
    let mut need_full: HashSet<String> = HashSet::new();
    // 每个 focus 会话已上传的正文基线。
    let mut focus_state: HashMap<String, FocusState> = HashMap::new();

    let mut ack_id = 0i64;
    let mut processed_id = 0i64;
    let mut results: Vec<CommandResult> = Vec::new();
    // 处理命令后的一段主动推送窗口，覆盖异步启动会话的开头输出。
    let mut active_until = Instant::now();

    let reset = |catalog_version: &mut i64,
                 threads_version: &mut i64,
                 last_catalog_sig: &mut String,
                 last_threads_sig: &mut String,
                 need_catalog: &mut bool,
                 need_threads: &mut bool,
                 focus_ids: &mut HashSet<String>,
                 need_full: &mut HashSet<String>,
                 focus_state: &mut HashMap<String, FocusState>,
                 ack_id: &mut i64,
                 processed_id: &mut i64,
                 results: &mut Vec<CommandResult>| {
        *catalog_version = 0;
        *threads_version = 0;
        last_catalog_sig.clear();
        last_threads_sig.clear();
        *need_catalog = true;
        *need_threads = true;
        focus_ids.clear();
        need_full.clear();
        focus_state.clear();
        *ack_id = 0;
        *processed_id = 0;
        results.clear();
    };

    loop {
        let Some(cfg) = config(&app) else {
            last_cfg = None;
            reset(
                &mut catalog_version,
                &mut threads_version,
                &mut last_catalog_sig,
                &mut last_threads_sig,
                &mut need_catalog,
                &mut need_threads,
                &mut focus_ids,
                &mut need_full,
                &mut focus_state,
                &mut ack_id,
                &mut processed_id,
                &mut results,
            );
            sleep(Duration::from_secs(10)).await;
            continue;
        };
        if last_cfg.as_ref() != Some(&cfg) {
            client = build_client(&cfg.proxy);
            last_cfg = Some(cfg.clone());
            reset(
                &mut catalog_version,
                &mut threads_version,
                &mut last_catalog_sig,
                &mut last_threads_sig,
                &mut need_catalog,
                &mut need_threads,
                &mut focus_ids,
                &mut need_full,
                &mut focus_state,
                &mut ack_id,
                &mut processed_id,
                &mut results,
            );
        }

        // --- 组装本轮要上传的变化 ---
        let catalog_sig = catalog_signature(&app);
        let metas = thread_metas(&app);
        let threads_sig = threads_signature(&metas);

        let mut catalog_out = None;
        if need_catalog || catalog_sig != last_catalog_sig {
            catalog_version += 1;
            catalog_out = Some(CatalogOut {
                version: catalog_version,
                projects: projects(&app),
                models: models(&app),
            });
        }
        let mut threads_out = None;
        if need_threads || threads_sig != last_threads_sig {
            threads_version += 1;
            threads_out = Some(ThreadsOut {
                version: threads_version,
                list: metas.clone(),
            });
        }

        let focus_threads = collect_focus_threads(&app, &focus_ids);
        let mut focus_running = false;
        let mut focuses: Vec<FocusOut> = Vec::new();
        // 记录本轮实际发送的 focus 基线，成功后再提交。
        let mut sent_focus: HashMap<String, (Vec<Value>, i64)> = HashMap::new();
        for id in &focus_ids {
            let Some(thread) = focus_threads.get(id) else {
                continue;
            };
            let running = thread_running(&app, thread);
            if running {
                focus_running = true;
            }
            let items = compact_items(thread);
            let want_full = need_full.contains(id) || !focus_state.contains_key(id);
            let (base, tail) = if want_full {
                (0usize, items.clone())
            } else {
                let prev = focus_state.get(id).map(|s| &s.items);
                let first = first_diff(prev.map(|v| v.as_slice()).unwrap_or(&[]), &items);
                let unchanged = prev.map(|p| p.len()) == Some(items.len()) && first == items.len();
                if unchanged {
                    continue;
                }
                (first, items[first..].to_vec())
            };
            let version = focus_state.get(id).map(|s| s.version + 1).unwrap_or(1);
            focuses.push(FocusOut {
                id: id.clone(),
                running,
                base,
                count: items.len(),
                version,
                plan: thread.plan.clone().unwrap_or(Value::Null),
                items: tail,
            });
            sent_focus.insert(id.clone(), (items, version));
        }

        let did_push = catalog_out.is_some()
            || threads_out.is_some()
            || !focuses.is_empty()
            || !results.is_empty();
        let active = focus_running || Instant::now() < active_until;
        let wait = !did_push && !active;

        let sent_catalog = catalog_out.is_some();
        let sent_threads = threads_out.is_some();
        let request = SyncRequest {
            device_name: cfg.name.clone(),
            ack_id,
            wait,
            results: results.clone(),
            catalog: catalog_out,
            threads: threads_out,
            focuses,
        };
        let body = match serde_json::to_value(&request) {
            Ok(v) => v,
            Err(e) => {
                sleep(error_backoff(&e.to_string())).await;
                continue;
            }
        };

        match sync(&client, &cfg, &body).await {
            Ok(resp) => {
                if sent_catalog {
                    last_catalog_sig = catalog_sig;
                }
                if sent_threads {
                    last_threads_sig = threads_sig;
                }
                for (id, (items, version)) in sent_focus {
                    focus_state.insert(id, FocusState { items, version });
                }
                results.clear();

                focus_ids = resp.focus_ids.into_iter().collect();
                need_catalog = resp.need_catalog;
                need_threads = resp.need_threads;
                need_full = resp.need_full.into_iter().collect();
                focus_state.retain(|id, _| focus_ids.contains(id));

                let processed = process_commands(
                    &app,
                    resp.commands,
                    &mut processed_id,
                    &mut ack_id,
                    &mut results,
                )
                .await;
                if processed {
                    active_until = Instant::now() + COMMAND_WATCH_DURATION;
                }
            }
            Err(e) => {
                sleep(error_backoff(&e)).await;
                continue;
            }
        }

        // 命令刚产生结果时立即再跑一轮把结果送出；否则按活动/空闲决定节奏。
        if !results.is_empty() {
            continue;
        }
        if did_push || active || !wait {
            sleep(ACTIVE_INTERVAL).await;
        }
        // wait==true 时请求已在服务端阻塞，返回后直接进入下一轮，无需额外 sleep。
    }
}

fn error_backoff(error: &str) -> Duration {
    if error.contains("423") || error.contains("409") {
        Duration::from_secs(60)
    } else {
        Duration::from_secs(3)
    }
}

/// 返回 a、b 首个不同下标；相同前缀越长，需要重传的尾部越短。
fn first_diff(a: &[Value], b: &[Value]) -> usize {
    let mut i = 0;
    while i < a.len() && i < b.len() && a[i] == b[i] {
        i += 1;
    }
    i
}

fn compact_items(thread: &Thread) -> Vec<Value> {
    thread.items.iter().map(remote_item_value).collect()
}

/// 目录（projects + models）廉价签名：只看内存里的项目路径、会话目录与各后端启用/可用状态，
/// 避免每拍做文件系统扫描或重建 model 选项。真正上传时才构造完整 catalog。
fn catalog_signature(app: &AppHandle) -> String {
    let state = app.state::<AppState>();
    let mut paths: Vec<String> = state.projects.lock().unwrap().projects.clone();
    {
        let store = state.store.lock().unwrap();
        paths.extend(
            store
                .threads
                .iter()
                .filter(|t| eligible(t))
                .map(|t| t.cwd.clone()),
        );
    }
    paths.sort();
    paths.dedup();
    let mut agents: Vec<String> = Vec::new();
    for kind in [
        AgentKind::Devin,
        AgentKind::Codex,
        AgentKind::CodeBuddy,
        AgentKind::ClaudeCode,
        AgentKind::Cursor,
        AgentKind::OpenCode,
    ] {
        let enabled = state.agent_enabled(&kind);
        let available = state
            .backend_availability
            .lock()
            .unwrap()
            .get(kind.as_str())
            .copied()
            .unwrap_or(true);
        agents.push(format!("{}:{}:{}", kind.as_str(), enabled, available));
    }
    format!("{paths:?}|{agents:?}")
}

/// 会话列表签名刻意排除 updated_at：流式过程中该字段每拍都变，排除它可避免列表被反复重传。
/// 运行状态 running 仍纳入签名，运行开始/结束时列表会刷新一次，保证前端小圆点及时更新。
fn threads_signature(metas: &[RemoteThreadMeta]) -> String {
    let rows: Vec<_> = metas
        .iter()
        .map(|m| {
            (
                &m.id,
                &m.title,
                &m.cwd,
                &m.agent_kind,
                &m.model,
                &m.mode,
                m.running,
            )
        })
        .collect();
    serde_json::to_string(&rows).unwrap_or_default()
}

fn collect_focus_threads(app: &AppHandle, ids: &HashSet<String>) -> HashMap<String, Thread> {
    if ids.is_empty() {
        return HashMap::new();
    }
    let state = app.state::<AppState>();
    let store = state.store.lock().unwrap();
    store
        .threads
        .iter()
        .filter(|t| eligible(t) && ids.contains(&t.id))
        .map(|t| (t.id.clone(), t.clone()))
        .collect()
}

fn config(app: &AppHandle) -> Option<RemoteConfig> {
    let state = app.state::<AppState>();
    let s = state.settings.lock().unwrap().clone();
    let server = resolve_relay_server(&s.relay_server);
    let token = s.relay_token.trim().to_string();
    if server.is_empty() || token.is_empty() {
        return None;
    }
    let name = if s.relay_name.trim().is_empty() {
        std::env::var("COMPUTERNAME")
            .or_else(|_| std::env::var("HOSTNAME"))
            .unwrap_or_else(|_| "Nova".into())
    } else {
        s.relay_name.trim().to_string()
    };
    let proxy = [
        &s.devin_proxy,
        &s.codex_proxy,
        &s.codebuddy_proxy,
        &s.claudecode_proxy,
        &s.cursor_proxy,
        &s.opencode_proxy,
    ]
    .into_iter()
    .map(|p| p.trim())
    .find(|p| !p.is_empty())
    .unwrap_or("")
    .to_string();
    Some(RemoteConfig {
        server,
        token,
        name,
        proxy,
        device_id: state.relay.device_id().to_string(),
    })
}

fn build_client(proxy: &str) -> Client {
    let mut builder = Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(35));
    if !proxy.is_empty() {
        let normalized = if proxy.contains("://") {
            proxy.to_string()
        } else {
            format!("http://{proxy}")
        };
        if let Ok(p) = reqwest::Proxy::all(&normalized) {
            builder = builder.proxy(p);
        }
    }
    builder.build().unwrap_or_default()
}

async fn sync(
    client: &Client,
    cfg: &RemoteConfig,
    value: &Value,
) -> Result<SyncResponse, String> {
    let body = gzip_json(value)?;
    let resp = client
        .post(format!("{}/v1/remote/sync", cfg.server))
        .header("Authorization", format!("Bearer {}", cfg.token))
        .header("X-Relay-Name", &cfg.name)
        .header("X-Relay-Device", &cfg.device_id)
        .header("Content-Type", "application/json")
        .header("Content-Encoding", "gzip")
        .body(body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    resp.json().await.map_err(|e| e.to_string())
}

fn eligible(t: &Thread) -> bool {
    t.roaming_role.is_none() && t.employee_id.is_none() && !t.mind_thread
}

fn thread_metas(app: &AppHandle) -> Vec<RemoteThreadMeta> {
    let state = app.state::<AppState>();
    let store = state.store.lock().unwrap();
    let mut items: Vec<_> = store
        .threads
        .iter()
        .filter(|t| eligible(t))
        .map(|t| RemoteThreadMeta {
            id: t.id.clone(),
            title: t.title.clone(),
            cwd: t.cwd.clone(),
            agent_kind: t.agent_kind.as_str().to_string(),
            model: t.model.clone().unwrap_or_default(),
            mode: t.mode.clone().unwrap_or_default(),
            updated_at: t.updated_at,
            running: is_running(&state, t),
        })
        .collect();
    items.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    items
}

fn projects(app: &AppHandle) -> Vec<RemoteProject> {
    let state = app.state::<AppState>();
    let mut paths = state.projects.lock().unwrap().projects.clone();
    {
        let store = state.store.lock().unwrap();
        paths.extend(
            store
                .threads
                .iter()
                .filter(|t| eligible(t))
                .map(|t| t.cwd.clone()),
        );
    }
    let mut seen = HashSet::new();
    paths
        .into_iter()
        .filter(|p| !p.contains(SCRATCH_MARK) && std::path::Path::new(p).is_dir())
        .filter(|p| seen.insert(p.clone()))
        .map(|path| RemoteProject {
            name: basename(&path),
            path,
        })
        .collect()
}

fn models(app: &AppHandle) -> HashMap<String, Value> {
    let state = app.state::<AppState>();
    let mut out = HashMap::new();
    for kind in [
        AgentKind::Devin,
        AgentKind::Codex,
        AgentKind::CodeBuddy,
        AgentKind::ClaudeCode,
        AgentKind::Cursor,
        AgentKind::OpenCode,
    ] {
        if !state.agent_enabled(&kind) {
            continue;
        }
        let available = state
            .backend_availability
            .lock()
            .unwrap()
            .get(kind.as_str())
            .copied()
            .unwrap_or(true);
        if !available {
            continue;
        }
        let value = match state.acp_for(&kind) {
            Some(mgr) => mgr.get_model_options(),
            None => state.codex.get_model_options(),
        }
        .unwrap_or_else(|| json!({ "configOptions": [], "modes": null }));
        out.insert(kind.as_str().to_string(), value);
    }
    out
}

fn thread_running(app: &AppHandle, thread: &Thread) -> bool {
    let state = app.state::<AppState>();
    is_running(&state, thread)
}

fn remote_item_value(item: &Item) -> Value {
    let mut item = item.clone();
    compact_remote_item(&mut item);
    serde_json::to_value(item).unwrap_or(Value::Null)
}

// 远控会话只展示工具做了什么，不传输命令输出、原始入参/出参和文件 diff。
// title 已包含执行命令；文件编辑保留 locations 中的路径即可。
fn compact_remote_item(item: &mut Item) {
    match item {
        Item::User { images, .. } => {
            for image in images {
                image.data = None;
                image.uri = None;
            }
        }
        Item::Tool { call, .. } => {
            call.content.clear();
            call.raw_input = None;
            call.raw_output = None;
            for location in &mut call.locations {
                let Some(path) = location.get("path").cloned() else {
                    *location = Value::Null;
                    continue;
                };
                *location = json!({ "path": path });
            }
            call.locations.retain(|location| !location.is_null());
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_tool_items_only_keep_summary_and_file_paths() {
        let item: Item = serde_json::from_value(json!({
            "type": "tool",
            "id": 7,
            "ts": 1,
            "toolCallId": "call-1",
            "title": "修改 src/main.rs",
            "kind": "edit",
            "status": "completed",
            "content": [{"type": "diff", "oldText": "secret old", "newText": "secret new"}],
            "locations": [{"path": "src/main.rs", "line": 42, "extra": "detail"}],
            "rawInput": {"patch": "internal"},
            "rawOutput": {"diff": "internal"}
        }))
        .unwrap();

        let value = remote_item_value(&item);
        assert_eq!(value["title"], "修改 src/main.rs");
        assert_eq!(value["kind"], "edit");
        assert_eq!(value["status"], "completed");
        assert_eq!(value["locations"], json!([{"path": "src/main.rs"}]));
        assert_eq!(value["content"], json!([]));
        assert!(value.get("rawInput").is_none());
        assert!(value.get("rawOutput").is_none());
    }

    #[test]
    fn internal_tool_output_does_not_count_as_remote_change() {
        let before: Item = serde_json::from_value(json!({
            "type": "tool", "id": 7, "ts": 1, "toolCallId": "call-1",
            "title": "cargo test", "kind": "execute", "status": "in_progress",
            "content": [{"type": "content", "content": {"type": "text", "text": "line 1"}}],
            "locations": [], "rawOutput": {"text": "line 1"}
        }))
        .unwrap();
        let after: Item = serde_json::from_value(json!({
            "type": "tool", "id": 7, "ts": 1, "toolCallId": "call-1",
            "title": "cargo test", "kind": "execute", "status": "in_progress",
            "content": [{"type": "content", "content": {"type": "text", "text": "line 1\nline 2"}}],
            "locations": [], "rawOutput": {"text": "line 1\nline 2"}
        }))
        .unwrap();
        assert_eq!(remote_item_value(&before), remote_item_value(&after));
    }

    #[test]
    fn first_diff_finds_common_prefix() {
        let a = vec![json!({"id": 1}), json!({"id": 2})];
        let b = vec![json!({"id": 1}), json!({"id": 2}), json!({"id": 3})];
        assert_eq!(first_diff(&a, &b), 2);

        let c = vec![json!({"id": 1}), json!({"id": 9})];
        assert_eq!(first_diff(&a, &c), 1);

        assert_eq!(first_diff(&a, &a), 2);
    }
}

/// 执行服务端下发的命令并收集结果。返回是否处理了至少一个新命令，
/// 供上层进入命令后的主动推送窗口。
async fn process_commands(
    app: &AppHandle,
    commands: Vec<RemoteCommand>,
    processed_id: &mut i64,
    ack_id: &mut i64,
    results: &mut Vec<CommandResult>,
) -> bool {
    let mut processed_any = false;
    for cmd in commands {
        if cmd.id <= *processed_id {
            continue;
        }
        let result = execute_command(app, &cmd).await;
        *processed_id = cmd.id;
        *ack_id = cmd.id;
        results.push(result);
        processed_any = true;
    }
    processed_any
}

async fn execute_command(app: &AppHandle, cmd: &RemoteCommand) -> CommandResult {
    let fail = |error: String| CommandResult {
        id: cmd.id,
        ok: false,
        error,
        thread_id: String::new(),
        data: None,
    };
    let ok_thread = |thread_id: String| CommandResult {
        id: cmd.id,
        ok: true,
        error: String::new(),
        thread_id,
        data: None,
    };
    match cmd.kind.as_str() {
        "create" => match create_thread(app, cmd).and_then(|thread| {
            send_prompt(app, &thread.id, &cmd.text)?;
            Ok(thread.id)
        }) {
            Ok(id) => ok_thread(id),
            Err(e) => fail(e),
        },
        "send" => match send_prompt(app, &cmd.thread_id, &cmd.text) {
            Ok(()) => ok_thread(cmd.thread_id.clone()),
            Err(e) => fail(e),
        },
        "stop" => match stop_thread(app, &cmd.thread_id).await {
            Ok(()) => ok_thread(cmd.thread_id.clone()),
            Err(e) => fail(e),
        },
        "git_status" => match remote_git_status(app, &cmd.cwd) {
            Ok(data) => CommandResult {
                id: cmd.id,
                ok: true,
                error: String::new(),
                thread_id: String::new(),
                data: Some(data),
            },
            Err(e) => fail(e),
        },
        "git_file" => match remote_git_file(app, &cmd.cwd, &cmd.path) {
            Ok(data) => CommandResult {
                id: cmd.id,
                ok: true,
                error: String::new(),
                thread_id: String::new(),
                data: Some(data),
            },
            Err(e) => fail(e),
        },
        _ => fail("不支持的远程操作".into()),
    }
}

fn ensure_remote_git_cwd(app: &AppHandle, cwd: &str) -> Result<String, String> {
    let cwd = cwd.trim();
    if cwd.is_empty() {
        return Err("缺少项目目录".into());
    }
    if !projects(app).iter().any(|p| p.path == cwd)
        && !{
            let state = app.state::<AppState>();
            let store = state.store.lock().unwrap();
            store.threads.iter().any(|t| eligible(t) && t.cwd == cwd)
        }
    {
        return Err("只能查看电脑端已有项目的 git 变化".into());
    }
    if !crate::gitwt::is_repo(cwd) {
        return Err("该目录不是 git 仓库".into());
    }
    crate::gitwt::repo_root(cwd)
}

fn remote_git_status(app: &AppHandle, cwd: &str) -> Result<Value, String> {
    let root = ensure_remote_git_cwd(app, cwd)?;
    let branch = crate::gitwt::run(&root, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_else(|_| "HEAD".into());
    let porcelain = crate::gitwt::run(
        &root,
        &["status", "--porcelain=v1", "-uall", "--ignore-submodules=dirty"],
    )?;
    let mut files = Vec::new();
    for line in porcelain.lines() {
        if line.len() < 3 {
            continue;
        }
        let x = line.as_bytes()[0] as char;
        let y = line.as_bytes()[1] as char;
        let rest = &line[3..];
        // rename: "R  old -> new"
        let path = if let Some((_, new_path)) = rest.split_once(" -> ") {
            new_path
        } else {
            rest
        };
        let status = match (x, y) {
            ('?', '?') => "untracked",
            ('A', _) | (_, 'A') => "added",
            ('D', _) | (_, 'D') => "deleted",
            ('R', _) | (_, 'R') => "renamed",
            _ => "modified",
        };
        files.push(json!({
            "path": path,
            "index": x.to_string(),
            "worktree": y.to_string(),
            "status": status,
        }));
    }
    Ok(json!({
        "repo": root,
        "branch": branch,
        "files": files,
    }))
}

fn remote_git_file(app: &AppHandle, cwd: &str, path: &str) -> Result<Value, String> {
    let root = ensure_remote_git_cwd(app, cwd)?;
    let path = path.trim().trim_start_matches(['/', '\\']);
    if path.is_empty() || path.contains("..") {
        return Err("文件路径无效".into());
    }
    let abs = std::path::Path::new(&root).join(path);
    let old_text = crate::gitwt::run(&root, &["show", &format!("HEAD:{path}")]).unwrap_or_default();
    let new_text = if abs.is_file() {
        std::fs::read_to_string(&abs).unwrap_or_default()
    } else {
        String::new()
    };
    const LIMIT: usize = 400_000;
    let mut truncated = false;
    let mut old = old_text;
    let mut new = new_text;
    if old.len() > LIMIT {
        old.truncate(LIMIT);
        truncated = true;
    }
    if new.len() > LIMIT {
        new.truncate(LIMIT);
        truncated = true;
    }
    // 粗判二进制：含 NUL
    if old.contains('\0') || new.contains('\0') {
        return Ok(json!({
            "path": path,
            "binary": true,
            "oldText": "",
            "newText": "",
            "truncated": false,
        }));
    }
    Ok(json!({
        "path": path,
        "binary": false,
        "oldText": old,
        "newText": new,
        "truncated": truncated,
    }))
}

fn create_thread(app: &AppHandle, cmd: &RemoteCommand) -> Result<Thread, String> {
    let scratch = cmd.cwd == REMOTE_SCRATCH_PATH;
    if !scratch && !projects(app).iter().any(|p| p.path == cmd.cwd) {
        return Err("只能选择电脑端已有项目".into());
    }
    let kind = AgentKind::from_str(&cmd.agent_kind).ok_or("模型后端无效")?;
    let state = app.state::<AppState>();
    if !state.agent_enabled(&kind) {
        return Err(format!("{} 后端已关闭", kind.label()));
    }
    let cwd = if scratch {
        make_scratch_dir()?
    } else {
        cmd.cwd.clone()
    };
    let mut thread = Thread::new(
        cwd.clone(),
        kind,
        Some(cmd.model.clone()).filter(|s| !s.is_empty()),
        Some(cmd.mode.clone()).filter(|s| !s.is_empty()),
        None,
        scratch,
    );
    // 远程入口不支持创建目录/worktree，只把已存在目录记录为最近项目。
    thread.updated_at = crate::threads::now_ms();
    {
        let mut store = state.store.lock().unwrap();
        store.threads.push(thread.clone());
        store.save();
    }
    if !scratch {
        state.projects.lock().unwrap().touch(&cwd);
        state.relay.publish_folders();
    }
    let _ = app.emit(crate::acp::EV_THREADS, json!({}));
    Ok(thread)
}

fn make_scratch_dir() -> Result<String, String> {
    let name = format!(
        "remote-{}-{}",
        chrono::Local::now().format("%m%d-%H%M%S"),
        &uuid::Uuid::new_v4().to_string()[..4]
    );
    let dir = std::env::temp_dir().join(SCRATCH_MARK).join(name);
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建临时目录失败：{e}"))?;
    Ok(dir.to_string_lossy().to_string())
}

fn send_prompt(app: &AppHandle, thread_id: &str, text: &str) -> Result<(), String> {
    let text = text.trim().to_string();
    if text.is_empty() {
        return Err("内容不能为空".into());
    }
    let state = app.state::<AppState>();
    let kind = {
        let store = state.store.lock().unwrap();
        let thread = store.get(thread_id).ok_or("会话不存在")?;
        if !eligible(thread) {
            return Err("该会话不支持远程操作".into());
        }
        thread.agent_kind.clone()
    };
    match kind {
        AgentKind::CodeBuddy | AgentKind::Cursor => {
            let mgr = state.acp_for(&kind).ok_or("后端不可用")?;
            let id = thread_id.to_string();
            tauri::async_runtime::spawn(async move { mgr.run_prompt(id, text, Vec::new()).await });
        }
        AgentKind::Codex => {
            let mgr = state.codex.clone();
            let id = thread_id.to_string();
            if mgr.is_running(&id) {
                tauri::async_runtime::spawn(
                    async move { mgr.steer_prompt(id, text, Vec::new()).await },
                );
            } else {
                tauri::async_runtime::spawn(
                    async move { mgr.run_prompt(id, text, Vec::new()).await },
                );
            }
        }
        _ => {
            let mgr = state.acp_for(&kind).ok_or("后端不可用")?;
            let id = thread_id.to_string();
            if mgr.is_running(&id) {
                tauri::async_runtime::spawn(
                    async move { mgr.steer_prompt(id, text, Vec::new()).await },
                );
            } else {
                tauri::async_runtime::spawn(
                    async move { mgr.run_prompt(id, text, Vec::new()).await },
                );
            }
        }
    }
    Ok(())
}

async fn stop_thread(app: &AppHandle, thread_id: &str) -> Result<(), String> {
    let state = app.state::<AppState>();
    let kind = {
        let store = state.store.lock().unwrap();
        let thread = store.get(thread_id).ok_or("会话不存在")?;
        if !eligible(thread) {
            return Err("该会话不支持远程操作".into());
        }
        thread.agent_kind.clone()
    };
    if kind == AgentKind::Cursor {
        let mgr = state.acp_for(&kind).ok_or("后端不可用")?;

        // 远程 create/send 是异步启动的：停止命令可能紧跟着到达，而 run_prompt 还没来得及
        // 登记 running。短暂等它进入启动窗口，避免第一次 cancel 被当成空闲直接忽略。
        for _ in 0..40 {
            if mgr.is_running(thread_id) {
                break;
            }
            sleep(Duration::from_millis(50)).await;
        }
        if !mgr.is_running(thread_id) {
            return Ok(());
        }
        mgr.cancel(thread_id).await;
        return Ok(());
    }
    match state.acp_for(&kind) {
        Some(mgr) => mgr.cancel(thread_id).await,
        None => state.codex.cancel(thread_id).await,
    };
    Ok(())
}

fn basename(path: &str) -> String {
    path.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(path)
        .to_string()
}
