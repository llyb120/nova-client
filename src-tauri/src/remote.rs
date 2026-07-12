//! server 侧远程会话客户端。
//!
//! 连上中转站后先推一次快照；空闲期只维持低流量命令长轮询，不持续上传。
//! 网页端刚打开（bootstrap state）时服务端会 refreshRequested，唤醒桌面补发。
//! 每个运行会话独立维护基线：首包快照，中间与收尾只发变化条目（约 400ms 一拍）。
//! 打开历史会话走一次性 kind=threads 轻量包（不重传 models/projects）；仅首连预热最新会话。
//! 所有请求体均 gzip，响应由 reqwest 自动解 gzip。

use crate::relay::{gzip_json, resolve_relay_server};
use crate::threads::{AgentKind, Item, Thread};
use crate::{is_running, AppState, SCRATCH_MARK};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};
use tokio::time::sleep;

const ACTIVE_INTERVAL: Duration = Duration::from_millis(400);
const REMOTE_SCRATCH_PATH: &str = "__nova_scratch__";
/// 首次连接只预热最新会话；其余历史按点击请求，兼顾首屏速度与流量。
const PREFETCH_RECENT: usize = 1;

#[derive(Clone, PartialEq, Eq)]
struct RemoteConfig {
    server: String,
    token: String,
    name: String,
    proxy: String,
    device_id: String,
}

#[derive(Serialize)]
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
struct RemoteSnapshot {
    projects: Vec<RemoteProject>,
    threads: Vec<RemoteThreadMeta>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thread: Option<Value>,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    thread_snapshots: HashMap<String, Value>,
    models: HashMap<String, Value>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RemoteThreadDelta {
    id: String,
    title: String,
    cwd: String,
    agent_kind: String,
    model: String,
    mode: String,
    updated_at: i64,
    running: bool,
    item_count: usize,
    items: Vec<Value>,
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
struct ServerResponse {
    #[serde(default)]
    commands: Vec<RemoteCommand>,
    #[serde(default)]
    requested_thread_ids: Vec<String>,
    #[serde(default)]
    need_full: bool,
    #[serde(default)]
    revision: i64,
}

pub fn start(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        run(app).await;
    });
}

async fn run(app: AppHandle) {
    let mut last_cfg: Option<RemoteConfig> = None;
    let mut client = Client::new();
    let mut requested: HashSet<String> = HashSet::new();
    let mut revision = 0i64;
    // 只保留正在流式同步的逐会话基线。历史会话按需发一次快照后立即释放，
    // 不把 server 缓存误当成永久订阅，也不让一个会话的基线影响另一个会话。
    let mut previous: HashMap<String, (Thread, bool)> = HashMap::new();
    let mut force_full = false;
    let mut ack_id = 0i64;
    let mut processed_id = 0i64;
    let mut results: Vec<CommandResult> = Vec::new();
    let mut catalog_signature = String::new();

    loop {
        let Some(cfg) = config(&app) else {
            last_cfg = None;
            requested.clear();
            revision = 0;
            previous.clear();
            force_full = false;
            catalog_signature.clear();
            sleep(Duration::from_secs(10)).await;
            continue;
        };
        if last_cfg.as_ref() != Some(&cfg) {
            client = build_client(&cfg.proxy);
            last_cfg = Some(cfg.clone());
            requested.clear();
            revision = 0;
            previous.clear();
            // 连上中转站后立刻推一次快照，避免空闲长轮询时网页端打开仍是空列表。
            force_full = true;
            ack_id = 0;
            processed_id = 0;
            results.clear();
            catalog_signature.clear();
        }

        // requested 是 server 的一次性 cache-miss 队列；另外保留上一拍仍在运行的会话，
        // 确保 running=false 与最后一段内容一定能作为收尾增量送达。
        let mut sync_ids = requested.clone();
        sync_ids.extend(
            previous
                .iter()
                .filter_map(|(id, (_, running))| (*running).then(|| id.clone())),
        );
        if force_full && revision == 0 {
            for id in recent_thread_ids(&app, PREFETCH_RECENT) {
                sync_ids.insert(id);
            }
        }

        let current = sync_threads(&app, &sync_ids);
        let running_now: HashMap<String, bool> = current
            .iter()
            .map(|(id, thread)| (id.clone(), thread_running(&app, thread)))
            .collect();
        let any_running = running_now.values().any(|running| *running);
        let metas = thread_metas(&app);
        let next_catalog_signature = catalog_signature_for(&metas);
        let catalog_changed = next_catalog_signature != catalog_signature;
        let needs_snapshot = current.keys().any(|id| !previous.contains_key(id));
        let dirty_ids: HashSet<String> = current
            .iter()
            .filter_map(|(id, thread)| {
                let (old, old_running) = previous.get(id)?;
                let running = running_now.get(id).copied().unwrap_or(false);
                thread_changed(old, *old_running, thread, running).then(|| id.clone())
            })
            .collect();
        let want_upload = force_full
            || catalog_changed
            || needs_snapshot
            || !dirty_ids.is_empty()
            || !results.is_empty();

        if !want_upload {
            match pull(&client, &cfg).await {
                Ok(resp) => {
                    revision = resp.revision;
                    if resp.need_full {
                        force_full = true;
                    }
                    requested = resp.requested_thread_ids.into_iter().collect();
                    process_commands(
                        &app,
                        resp.commands,
                        &mut processed_id,
                        &mut ack_id,
                        &mut results,
                        &mut requested,
                    )
                    .await;
                }
                Err(e) => sleep(error_backoff(&e)).await,
            }
            continue;
        }

        let next_revision = revision.saturating_add(1).max(1);
        let snapshot_threads: HashMap<String, Thread> = current
            .iter()
            .filter_map(|(id, thread)| {
                (!previous.contains_key(id)).then(|| (id.clone(), thread.clone()))
            })
            .collect();
        let (body, sent_ids, sent_catalog) = if force_full || revision == 0 {
            (
                json!({
                    "deviceName": cfg.name,
                    "ackId": ack_id,
                    "results": results,
                    "kind": "full",
                    "baseRevision": revision,
                    "revision": next_revision,
                    "snapshot": full_snapshot(&app, &current),
                }),
                current.keys().cloned().collect::<HashSet<_>>(),
                true,
            )
        } else if catalog_changed || needs_snapshot || dirty_ids.is_empty() {
            // 新会话/目录变化/单纯命令结果走轻量包。只附带缺基线的会话快照；
            // 已有基线的运行会话留到下一拍继续发增量，避免被另一个历史会话拖成全量。
            (
                json!({
                    "deviceName": cfg.name,
                    "ackId": ack_id,
                    "results": results,
                    "kind": "threads",
                    "baseRevision": revision,
                    "revision": next_revision,
                    "snapshot": threads_pack_with_metas(metas.clone(), &snapshot_threads),
                }),
                snapshot_threads.keys().cloned().collect::<HashSet<_>>(),
                true,
            )
        } else {
            let mut deltas = Vec::with_capacity(dirty_ids.len());
            let mut delta_ok = true;
            for id in &dirty_ids {
                let Some(current_thread) = current.get(id) else {
                    delta_ok = false;
                    break;
                };
                let Some((old, _)) = previous.get(id) else {
                    delta_ok = false;
                    break;
                };
                let Some(delta) = make_delta(old, current_thread, &app) else {
                    delta_ok = false;
                    break;
                };
                deltas.push(delta);
            }
            if !delta_ok {
                // 只把本次涉及的会话改走快照，不清空其他会话基线。
                for id in &dirty_ids {
                    previous.remove(id);
                }
                continue;
            }
            (
                json!({
                    "deviceName": cfg.name,
                    "ackId": ack_id,
                    "results": results,
                    "kind": "delta",
                    "baseRevision": revision,
                    "revision": next_revision,
                    "delta": { "threads": deltas },
                }),
                dirty_ids.clone(),
                false,
            )
        };

        match sync(&client, &cfg, &body).await {
            Ok(resp) => {
                revision = resp.revision;
                if !resp.need_full {
                    for id in &sent_ids {
                        if let Some(thread) = current.get(id) {
                            previous.insert(
                                id.clone(),
                                (
                                    thread.clone(),
                                    running_now.get(id).copied().unwrap_or(false),
                                ),
                            );
                        }
                    }
                    if sent_catalog {
                        catalog_signature = next_catalog_signature;
                    }
                }
                force_full = resp.need_full;
                results.clear();
                requested = resp.requested_thread_ids.into_iter().collect();
                process_commands(
                    &app,
                    resp.commands,
                    &mut processed_id,
                    &mut ack_id,
                    &mut results,
                    &mut requested,
                )
                .await;
                if !force_full {
                    // 非运行、也不再被 server 点名的历史快照已经落到 server，立即释放大对象。
                    previous.retain(|id, (_, running)| *running || requested.contains(id));
                }
            }
            Err(e) => sleep(error_backoff(&e)).await,
        }
        if any_running || force_full || !results.is_empty() {
            sleep(ACTIVE_INTERVAL).await;
        }
    }
}

fn error_backoff(error: &str) -> Duration {
    if error.contains("423") || error.contains("409") {
        Duration::from_secs(60)
    } else {
        Duration::from_secs(3)
    }
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

async fn pull(client: &Client, cfg: &RemoteConfig) -> Result<ServerResponse, String> {
    let resp = client
        .get(format!("{}/v1/remote/pull", cfg.server))
        .header("Authorization", format!("Bearer {}", cfg.token))
        .header("X-Relay-Name", &cfg.name)
        .header("X-Relay-Device", &cfg.device_id)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    resp.json().await.map_err(|e| e.to_string())
}

async fn sync(
    client: &Client,
    cfg: &RemoteConfig,
    value: &Value,
) -> Result<ServerResponse, String> {
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

/// 浏览器按需读取过的会话 + 所有正在运行的普通会话加入同步集合。
/// 浏览器选择只形成只读快照请求，不修改桌面端 active_thread，也不影响其他浏览器。
fn sync_threads(app: &AppHandle, requested: &HashSet<String>) -> HashMap<String, Thread> {
    let state = app.state::<AppState>();
    let store = state.store.lock().unwrap();
    store
        .threads
        .iter()
        .filter(|t| eligible(t))
        .filter(|t| requested.contains(&t.id) || is_running(&state, t))
        .map(|t| (t.id.clone(), t.clone()))
        .collect()
}

fn thread_running(app: &AppHandle, thread: &Thread) -> bool {
    let state = app.state::<AppState>();
    is_running(&state, thread)
}

fn full_snapshot(app: &AppHandle, synced: &HashMap<String, Thread>) -> RemoteSnapshot {
    RemoteSnapshot {
        projects: projects(app),
        threads: thread_metas(app),
        thread: None,
        thread_snapshots: synced
            .iter()
            .map(|(id, thread)| (id.clone(), remote_thread_value(thread)))
            .collect(),
        models: models(app),
    }
}

/// 轻量包：只带会话列表 + 指定会话快照，供打开历史/预热，避免重扫 projects 与重传 models。
fn threads_pack_with_metas(
    metas: Vec<RemoteThreadMeta>,
    synced: &HashMap<String, Thread>,
) -> RemoteSnapshot {
    RemoteSnapshot {
        projects: Vec::new(),
        threads: metas,
        thread: None,
        thread_snapshots: synced
            .iter()
            .map(|(id, thread)| (id.clone(), remote_thread_value(thread)))
            .collect(),
        models: HashMap::new(),
    }
}

/// 目录签名刻意排除 updated_at/running：这两个高频字段由逐会话增量携带，
/// 避免流式输出时退化成每 400ms 重传完整会话。标题/模型/目录变化仍会触发轻量目录包。
fn catalog_signature_for(metas: &[RemoteThreadMeta]) -> String {
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
            )
        })
        .collect();
    serde_json::to_string(&rows).unwrap_or_default()
}

fn thread_changed(old: &Thread, old_running: bool, current: &Thread, running: bool) -> bool {
    old_running != running
        || old.title != current.title
        || old.cwd != current.cwd
        || old.agent_kind != current.agent_kind
        || old.model != current.model
        || old.mode != current.mode
        || old.items.len() != current.items.len()
        || old
            .items
            .iter()
            .zip(current.items.iter())
            .any(|(before, after)| remote_item_value(before) != remote_item_value(after))
}

fn recent_thread_ids(app: &AppHandle, limit: usize) -> Vec<String> {
    let state = app.state::<AppState>();
    let store = state.store.lock().unwrap();
    let mut items: Vec<_> = store.threads.iter().filter(|t| eligible(t)).collect();
    items.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    items
        .into_iter()
        .take(limit)
        .map(|t| t.id.clone())
        .collect()
}

fn remote_thread_value(thread: &Thread) -> Value {
    let mut thread = thread.clone();
    for item in &mut thread.items {
        compact_remote_item(item);
    }
    serde_json::to_value(thread).unwrap_or(Value::Null)
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

fn make_delta(previous: &Thread, current: &Thread, app: &AppHandle) -> Option<RemoteThreadDelta> {
    if previous.id != current.id || current.items.len() < previous.items.len() {
        return None;
    }
    for (old, new) in previous.items.iter().zip(current.items.iter()) {
        if old.id() != new.id() {
            return None;
        }
    }
    let mut changed = Vec::new();
    for (index, item) in current.items.iter().enumerate() {
        let differs = previous
            .items
            .get(index)
            .map(|old| remote_item_value(old) != remote_item_value(item))
            .unwrap_or(true);
        if differs {
            changed.push(remote_item_value(item));
        }
    }
    let state = app.state::<AppState>();
    Some(RemoteThreadDelta {
        id: current.id.clone(),
        title: current.title.clone(),
        cwd: current.cwd.clone(),
        agent_kind: current.agent_kind.as_str().to_string(),
        model: current.model.clone().unwrap_or_default(),
        mode: current.mode.clone().unwrap_or_default(),
        updated_at: current.updated_at,
        running: is_running(&state, current),
        item_count: current.items.len(),
        items: changed,
    })
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
}

async fn process_commands(
    app: &AppHandle,
    commands: Vec<RemoteCommand>,
    processed_id: &mut i64,
    ack_id: &mut i64,
    results: &mut Vec<CommandResult>,
    requested: &mut HashSet<String>,
) {
    for cmd in commands {
        if cmd.id <= *processed_id {
            continue;
        }
        let result = execute_command(app, &cmd).await;
        *processed_id = cmd.id;
        *ack_id = cmd.id;
        if result.ok && !result.thread_id.is_empty() {
            requested.insert(result.thread_id.clone());
        }
        // create/send/stop 的会话 id 已加入一次性请求集；下一拍会按该会话独立选择
        // 快照或增量，不再把一个命令升级成全局 full。
        results.push(result);
    }
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
