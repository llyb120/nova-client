//! server 侧远程会话客户端。
//!
//! 连上中转站后先推一次快照；空闲期复用一条命令长轮询连接，不持续上传。
//! 网页端刚打开（bootstrap state）时服务端会 refreshRequested，唤醒桌面补发。
//! 每个运行会话独立维护基线：首包快照，中间与收尾只发变化条目（约 400ms 一拍）。
//! 服务端会回传当前查看会话的轻量校验点；发现缺包或修订错位时只补对应会话。
//! 打开历史会话走一次性 kind=threads 轻量包（不重传 models/projects）；仅首连预热最新会话。
//! 工具条目只同步展示摘要，所有请求体均 gzip，响应由 reqwest 自动解 gzip。

use crate::relay::{gzip_json, resolve_relay_server};
use crate::threads::{AgentKind, Item, Thread};
use crate::{is_running, AppState, SCRATCH_MARK};
use base64::Engine;
use reqwest::Client;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::mpsc;
use tokio::time::sleep;

const ACTIVE_INTERVAL: Duration = Duration::from_millis(400);
const COMMAND_WATCH_DURATION: Duration = Duration::from_secs(30);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(65);
const REMOTE_SCRATCH_PATH: &str = "__nova_scratch__";
/// 首次连接只预热最新会话；其余历史按点击请求，兼顾首屏速度与流量。
const PREFETCH_RECENT: usize = 1;
const REMOTE_FILE_MAX_BYTES: u64 = 50 * 1024 * 1024;

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
    permissions: Vec<Value>,
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
    plan: Value,
    base: ThreadCheckpoint,
    checkpoint: ThreadCheckpoint,
    item_count: usize,
    items: Vec<Value>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct ThreadCheckpoint {
    item_count: usize,
    hash: String,
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
    #[serde(default)]
    title: String,
    #[serde(default)]
    request_key: String,
    #[serde(default)]
    option_id: String,
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
    #[serde(default, deserialize_with = "deserialize_null_default")]
    commands: Vec<RemoteCommand>,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    requested_thread_ids: Vec<String>,
    #[serde(default)]
    need_full: bool,
    #[serde(default)]
    revision: i64,
    #[serde(default)]
    resync: bool,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    thread_checkpoints: HashMap<String, ThreadCheckpoint>,
}

/// 兼容旧服务端把空集合编码成 `null`。协议升级期间不能因为一个空字段让整条
/// 长轮询响应解析失败，否则历史会话的按需同步请求永远到不了客户端。
fn deserialize_null_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

pub fn start(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        run(app).await;
    });
}

async fn run(app: AppHandle) {
    let mut last_cfg: Option<RemoteConfig> = None;
    let mut client = Client::new();
    let (pull_tx, mut pull_rx) = mpsc::unbounded_channel();
    let mut pull_task: Option<tauri::async_runtime::JoinHandle<()>> = None;
    let mut pull_generation = 0u64;
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
    let mut permission_signature = String::new();
    // 远程命令通过异步任务启动。初始快照可能先看到 running=false，随后任务才登记运行；
    // 在这段窗口内主动跟踪命令涉及的会话，避免释放基线后漏掉整个短任务。
    let mut command_watch: HashMap<String, Instant> = HashMap::new();

    loop {
        let Some(cfg) = config(&app) else {
            if let Some(task) = pull_task.take() {
                task.abort();
            }
            pull_generation = pull_generation.wrapping_add(1);
            last_cfg = None;
            requested.clear();
            revision = 0;
            previous.clear();
            force_full = false;
            catalog_signature.clear();
            permission_signature.clear();
            command_watch.clear();
            // 设置页开启后尽快生效；关闭时命令执行入口还会再次校验开关。
            sleep(Duration::from_secs(1)).await;
            continue;
        };
        if last_cfg.as_ref() != Some(&cfg) {
            if let Some(task) = pull_task.take() {
                task.abort();
            }
            pull_generation = pull_generation.wrapping_add(1);
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
            permission_signature.clear();
            command_watch.clear();
        }

        while let Ok((generation, result)) = pull_rx.try_recv() {
            if generation != pull_generation {
                continue;
            }
            pull_task.take();
            if let Ok(response) = result {
                apply_pull_response(
                    &app,
                    response,
                    &mut revision,
                    &mut force_full,
                    &mut previous,
                    &mut processed_id,
                    &mut ack_id,
                    &mut results,
                    &mut requested,
                    &mut command_watch,
                )
                .await;
            }
        }
        if pull_task.is_none() && revision > 0 && !force_full && results.is_empty() {
            pull_task = Some(spawn_pull(
                client.clone(),
                cfg.clone(),
                pull_generation,
                pull_tx.clone(),
            ));
        }

        let now = Instant::now();
        command_watch.retain(|id, until| {
            now < *until || previous.get(id).is_some_and(|(_, running)| *running)
        });
        previous.retain(|id, (_, running)| {
            *running || requested.contains(id) || command_watch.contains_key(id)
        });
        // requested 是 server 的一次性 cache-miss 队列；另外保留上一拍仍在运行的会话，
        // 确保 running=false 与最后一段内容一定能作为收尾增量送达。
        let mut sync_ids = requested.clone();
        sync_ids.extend(command_watch.keys().cloned());
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
        let permissions = remote_permissions(&app);
        let next_permission_signature =
            serde_json::to_string(&permissions).unwrap_or_else(|_| "[]".into());
        let permissions_changed = next_permission_signature != permission_signature;
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
            || permissions_changed
            || !results.is_empty();

        if !want_upload {
            if any_running || !command_watch.is_empty() {
                // 内容同步继续按活动间隔检查；服务端命令由独立长轮询并发领取，
                // 不会再被一个长任务或无可见输出的工具调用挡住。
                sleep(ACTIVE_INTERVAL).await;
                continue;
            }
            if let Some((generation, result)) = pull_rx.recv().await {
                if generation == pull_generation {
                    pull_task.take();
                    if let Ok(response) = result {
                        apply_pull_response(
                            &app,
                            response,
                            &mut revision,
                            &mut force_full,
                            &mut previous,
                            &mut processed_id,
                            &mut ack_id,
                            &mut results,
                            &mut requested,
                            &mut command_watch,
                        )
                        .await;
                    }
                }
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
            let checkpoints = checkpoints_for(&current);
            (
                json!({
                    "deviceName": cfg.name,
                    "ackId": ack_id,
                    "results": results,
                    "kind": "full",
                    "baseRevision": revision,
                    "revision": next_revision,
                    "snapshot": full_snapshot(&app, &current),
                    "threadCheckpoints": checkpoints,
                }),
                current.keys().cloned().collect::<HashSet<_>>(),
                true,
            )
        } else if catalog_changed || needs_snapshot || dirty_ids.is_empty() {
            // 新会话/目录变化/单纯命令结果走轻量包。只附带缺基线的会话快照；
            // 已有基线的运行会话留到下一拍继续发增量，避免被另一个历史会话拖成全量。
            let checkpoints = checkpoints_for(&snapshot_threads);
            (
                json!({
                    "deviceName": cfg.name,
                    "ackId": ack_id,
                    "results": results,
                    "kind": "threads",
                    "baseRevision": revision,
                    "revision": next_revision,
                    "snapshot": threads_pack_with_metas(&app, metas.clone(), &snapshot_threads),
                    "threadCheckpoints": checkpoints,
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
                    "permissions": permissions,
                }),
                dirty_ids.clone(),
                false,
            )
        };

        // 构造同步包期间用户可能刚关闭开关；关闭后不再向 server 上传任何数据。
        if !remote_control_enabled(&app) {
            continue;
        }
        match sync(&client, &cfg, &body).await {
            Ok(resp) => {
                revision = resp.revision;
                let mut next_requested = reconcile_response(&app, &resp, &mut previous);
                if resp.resync {
                    for id in &sent_ids {
                        if !response_confirms_thread(&resp, &current, id) {
                            previous.remove(id);
                            next_requested.insert(id.clone());
                        }
                    }
                }
                if !resp.need_full {
                    for id in &sent_ids {
                        if next_requested.contains(id) {
                            continue;
                        }
                        if resp.resync && !response_confirms_thread(&resp, &current, id) {
                            continue;
                        }
                        if let Some(thread) = current.get(id) {
                            let completed_after_baseline = previous
                                .get(id)
                                .is_some_and(|(_, was_running)| *was_running)
                                && !running_now.get(id).copied().unwrap_or(false);
                            previous.insert(
                                id.clone(),
                                (
                                    thread.clone(),
                                    running_now.get(id).copied().unwrap_or(false),
                                ),
                            );
                            if completed_after_baseline {
                                command_watch.remove(id);
                            }
                        }
                    }
                    if sent_catalog && !resp.resync {
                        catalog_signature = next_catalog_signature;
                    }
                    if !resp.resync {
                        permission_signature = next_permission_signature;
                    }
                }
                force_full = resp.need_full;
                results.clear();
                requested = next_requested;
                process_commands(
                    &app,
                    resp.commands,
                    &mut processed_id,
                    &mut ack_id,
                    &mut results,
                    &mut requested,
                    &mut command_watch,
                )
                .await;
            }
            Err(e) => sleep(error_backoff(&e)).await,
        }
        if any_running || !command_watch.is_empty() || force_full || !results.is_empty() {
            sleep(ACTIVE_INTERVAL).await;
        }
    }
}

fn spawn_pull(
    client: Client,
    cfg: RemoteConfig,
    generation: u64,
    tx: mpsc::UnboundedSender<(u64, Result<ServerResponse, String>)>,
) -> tauri::async_runtime::JoinHandle<()> {
    tauri::async_runtime::spawn(async move {
        let result = pull(&client, &cfg).await;
        if let Err(error) = &result {
            sleep(error_backoff(error)).await;
        }
        let _ = tx.send((generation, result));
    })
}

async fn apply_pull_response(
    app: &AppHandle,
    response: ServerResponse,
    revision: &mut i64,
    force_full: &mut bool,
    previous: &mut HashMap<String, (Thread, bool)>,
    processed_id: &mut i64,
    ack_id: &mut i64,
    results: &mut Vec<CommandResult>,
    requested: &mut HashSet<String>,
    command_watch: &mut HashMap<String, Instant>,
) {
    // pull 与内容上传并发，较早发出的 pull 可能晚于一次 sync 返回。修订号只能前进；
    // 命令 id 自带去重，因此无论响应修订新旧都可以安全处理。
    if response.need_full {
        *revision = response.revision;
        *force_full = true;
        previous.clear();
        requested.extend(reconcile_response(app, &response, previous));
    } else if response.revision >= *revision {
        *revision = response.revision;
        requested.extend(reconcile_response(app, &response, previous));
    }
    process_commands(
        app,
        response.commands,
        processed_id,
        ack_id,
        results,
        requested,
        command_watch,
    )
    .await;
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
    if !s.remote_control_enabled {
        return None;
    }
    let server = resolve_relay_server(&s.relay_server);
    let token = s.relay_token.trim().to_string();
    if server.is_empty() || token.is_empty() {
        return None;
    }
    let name = crate::server::configured_name()
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .filter(|value| !value.trim().is_empty())
        .or_else(|| std::env::var("HOSTNAME").ok())
        .or_else(|| std::env::var("USERNAME").ok())
        .unwrap_or_else(|| "Nova".into());
    let proxy = crate::server::configured_proxy().unwrap_or_else(|| {
        [
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
        .to_string()
    });
    Some(RemoteConfig {
        server,
        token,
        name,
        proxy,
        device_id: state.relay.device_id().to_string(),
    })
}

fn remote_control_enabled(app: &AppHandle) -> bool {
    app.state::<AppState>()
        .settings
        .lock()
        .unwrap()
        .remote_control_enabled
}

fn build_client(proxy: &str) -> Client {
    let mut builder = Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(REQUEST_TIMEOUT)
        .tcp_keepalive(Duration::from_secs(30))
        .pool_idle_timeout(Duration::from_secs(90))
        .pool_max_idle_per_host(2);
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
        .get(format!("{}/v2/remote/pull", cfg.server))
        .header("Authorization", format!("Bearer {}", cfg.token))
        .header("X-Relay-Name-Encoded", crate::relay::urlencode(&cfg.name))
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
        .post(format!("{}/v2/remote/sync", cfg.server))
        .header("Authorization", format!("Bearer {}", cfg.token))
        .header("X-Relay-Name-Encoded", crate::relay::urlencode(&cfg.name))
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
    t.roaming_role.is_none()
        && t.employee_id.is_none()
        && !t.mind_thread
        && crate::server::path_allowed(&t.cwd)
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
        AgentKind::OpenCodePlus,
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
        let value = match kind {
            AgentKind::Devin => state.acp.get_model_options(),
            AgentKind::Codex | AgentKind::CodexPlus => state.codex.get_model_options(),
            AgentKind::OpenCode | AgentKind::OpenCodePlus => state.opencodeplus.get_model_options(),
            _ => None,
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
        permissions: remote_permissions(app),
    }
}

/// 轻量包：只带会话列表 + 指定会话快照，供打开历史/预热，避免重扫 projects 与重传 models。
fn threads_pack_with_metas(
    app: &AppHandle,
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
        permissions: remote_permissions(app),
    }
}

fn remote_permissions(app: &AppHandle) -> Vec<Value> {
    app.state::<AppState>()
        .remote_permissions
        .lock()
        .unwrap()
        .values()
        .cloned()
        .collect()
}

/// 目录签名刻意排除 updated_at/running：这两个高频字段由逐会话增量携带，
/// 避免流式输出时退化成每 400ms 重传完整会话。标题/模型/目录变化仍会触发轻量目录包。
fn catalog_signature_for(metas: &[RemoteThreadMeta]) -> String {
    let rows: Vec<_> = metas
        .iter()
        .map(|m| (&m.id, &m.title, &m.cwd, &m.agent_kind, &m.model, &m.mode))
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
        || old.plan != current.plan
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
    // 远程设备/网页会话暂不传递证据链；漫游会话走 relay.rs 的独立协议。
    thread.active_clue_card_id = None;
    thread.clue_context = None;
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

fn thread_checkpoint(thread: &Thread) -> ThreadCheckpoint {
    let payload = json!({
        "items": thread.items.iter().map(remote_item_value).collect::<Vec<_>>(),
        "plan": thread.plan,
    });
    let mut hash = 0xcbf29ce484222325u64;
    for byte in serde_json::to_vec(&payload).unwrap_or_default() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    ThreadCheckpoint {
        item_count: thread.items.len(),
        hash: format!("{hash:016x}"),
    }
}

fn checkpoints_for(threads: &HashMap<String, Thread>) -> HashMap<String, ThreadCheckpoint> {
    threads
        .iter()
        .map(|(id, thread)| (id.clone(), thread_checkpoint(thread)))
        .collect()
}

fn reconcile_response(
    app: &AppHandle,
    response: &ServerResponse,
    previous: &mut HashMap<String, (Thread, bool)>,
) -> HashSet<String> {
    let mut requested: HashSet<String> = response.requested_thread_ids.iter().cloned().collect();
    let ids: HashSet<String> = response.thread_checkpoints.keys().cloned().collect();
    let local = sync_threads(app, &ids);
    for (id, server_checkpoint) in &response.thread_checkpoints {
        let Some(thread) = local.get(id) else {
            continue;
        };
        if thread_checkpoint(thread) != *server_checkpoint {
            previous.remove(id);
            requested.insert(id.clone());
        }
    }
    requested
}

fn response_confirms_thread(
    response: &ServerResponse,
    current: &HashMap<String, Thread>,
    id: &str,
) -> bool {
    current.get(id).is_some_and(|thread| {
        response.thread_checkpoints.get(id) == Some(&thread_checkpoint(thread))
    })
}

// 远控会话只展示工具做了什么，不传输命令输出、原始入参/出参、文件 diff 和历史定位详情。
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
            call.locations.clear();
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
        plan: current.plan.clone().unwrap_or(Value::Null),
        base: thread_checkpoint(previous),
        checkpoint: thread_checkpoint(current),
        item_count: current.items.len(),
        items: changed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_tool_items_only_keep_summary() {
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
        assert_eq!(value["locations"], json!([]));
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
    fn checkpoint_covers_visible_text_and_plan() {
        let mut thread = Thread::new("C:/work".into(), AgentKind::Codex, None, None, None, false);
        thread.items.push(Item::Assistant {
            id: 1,
            text: "first".into(),
            ts: 1,
        });
        let before = thread_checkpoint(&thread);
        thread.plan = Some(json!([{"step": "test", "status": "pending"}]));
        assert_ne!(before, thread_checkpoint(&thread));
    }

    #[test]
    fn remote_thread_snapshot_omits_clue_context() {
        let mut thread = Thread::new("C:/work".into(), AgentKind::Codex, None, None, None, false);
        thread.active_clue_card_id = Some("card-1".into());
        thread.clue_context = serde_json::from_value(json!({
            "rootCardId": "card-1",
            "cards": [],
            "renderedContext": "secret clue context",
            "createdAt": 1
        }))
        .ok();

        let value = remote_thread_value(&thread);
        assert!(value.get("activeClueCardId").is_none());
        assert!(value.get("clueContext").is_none());
    }

    #[test]
    fn server_response_accepts_null_collections() {
        let response: ServerResponse = serde_json::from_value(json!({
            "commands": null,
            "requestedThreadIds": null,
            "threadCheckpoints": null,
            "revision": 3
        }))
        .unwrap();

        assert!(response.commands.is_empty());
        assert!(response.requested_thread_ids.is_empty());
        assert!(response.thread_checkpoints.is_empty());
        assert_eq!(response.revision, 3);
    }

    #[test]
    fn remote_file_path_normalizes_slash_prefixed_windows_drive() {
        assert_eq!(
            normalize_remote_file_path(" /D:/code/nova/file.rs "),
            "D:/code/nova/file.rs"
        );
        assert_eq!(
            normalize_remote_file_path("/c:\\code\\nova\\file.rs"),
            "c:\\code\\nova\\file.rs"
        );
        assert_eq!(
            normalize_remote_file_path("/home/user/file.rs"),
            "/home/user/file.rs"
        );
        assert_eq!(normalize_remote_file_path("src/file.rs"), "src/file.rs");
    }
}

async fn process_commands(
    app: &AppHandle,
    commands: Vec<RemoteCommand>,
    processed_id: &mut i64,
    ack_id: &mut i64,
    results: &mut Vec<CommandResult>,
    requested: &mut HashSet<String>,
    command_watch: &mut HashMap<String, Instant>,
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
            command_watch.insert(
                result.thread_id.clone(),
                Instant::now() + COMMAND_WATCH_DURATION,
            );
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
    if !remote_control_enabled(app) {
        return fail("本机未允许远程控制".into());
    }
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
        "rename" => match rename_remote_thread(app, &cmd.thread_id, &cmd.title) {
            Ok(()) => ok_thread(cmd.thread_id.clone()),
            Err(e) => fail(e),
        },
        "configure" => match configure_remote_thread(app, cmd) {
            Ok(()) => ok_thread(cmd.thread_id.clone()),
            Err(e) => fail(e),
        },
        "permission" => {
            match respond_remote_permission(app, &cmd.request_key, &cmd.option_id).await {
                Ok(()) => CommandResult {
                    id: cmd.id,
                    ok: true,
                    error: String::new(),
                    thread_id: cmd.thread_id.clone(),
                    data: None,
                },
                Err(e) => fail(e),
            }
        }
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
        "remote_file" => match remote_file(app, &cmd.thread_id, &cmd.cwd, &cmd.path) {
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

fn rename_remote_thread(app: &AppHandle, thread_id: &str, title: &str) -> Result<(), String> {
    let title = title.trim();
    if title.is_empty() {
        return Err("标题不能为空".into());
    }
    let state = app.state::<AppState>();
    let mut store = state.store.lock().unwrap();
    let thread = store.get_mut(thread_id).ok_or("会话不存在")?;
    if !eligible(thread) {
        return Err("该会话不支持远程操作".into());
    }
    thread.title = title.to_string();
    store.save();
    let _ = app.emit(crate::acp::EV_THREADS, json!({}));
    Ok(())
}

fn configure_remote_thread(app: &AppHandle, cmd: &RemoteCommand) -> Result<(), String> {
    let state = app.state::<AppState>();
    let kind = {
        let mut store = state.store.lock().unwrap();
        let thread = store.get_mut(&cmd.thread_id).ok_or("会话不存在")?;
        if !eligible(thread) {
            return Err("该会话不支持远程操作".into());
        }
        if is_running(&state, thread) {
            return Err("请先停止当前轮次再切换模型或模式".into());
        }
        if !cmd.model.trim().is_empty() {
            thread.model = Some(cmd.model.trim().to_string());
        }
        if !cmd.mode.trim().is_empty() {
            thread.mode = Some(cmd.mode.trim().to_string());
        }
        let kind = thread.agent_kind.clone();
        store.save();
        kind
    };
    match kind {
        AgentKind::Devin => state.acp.forget_session_of_thread(&cmd.thread_id),
        AgentKind::Codex | AgentKind::CodexPlus => {
            state.codexplus.forget_session_of_thread(&cmd.thread_id)
        }
        AgentKind::CodeBuddy | AgentKind::CodeBuddyPlus => {
            state.codebuddyplus.forget_session_of_thread(&cmd.thread_id)
        }
        AgentKind::ClaudeCode => state.claudeplus.forget_session_of_thread(&cmd.thread_id),
        AgentKind::Cursor => state.cursorplus.forget_session_of_thread(&cmd.thread_id),
        AgentKind::OpenCode | AgentKind::OpenCodePlus => {
            state.opencodeplus.forget_session_of_thread(&cmd.thread_id)
        }
    }
    Ok(())
}

async fn respond_remote_permission(
    app: &AppHandle,
    request_key: &str,
    option_id: &str,
) -> Result<(), String> {
    if request_key.trim().is_empty() {
        return Err("缺少权限请求标识".into());
    }
    let state = app.state::<AppState>();
    let borrowed = state
        .borrowed_runtimes
        .lock()
        .unwrap()
        .values()
        .find(|runtime| runtime.has_pending_permission(request_key))
        .cloned();
    if let Some(runtime) = borrowed {
        return runtime.respond_permission(request_key, option_id).await;
    }
    if request_key.starts_with("cdp-") {
        state
            .codexplus
            .respond_permission(request_key, option_id)
            .await
    } else if request_key.starts_with("cbp-") {
        state
            .codebuddyplus
            .respond_permission(request_key, option_id)
            .await
    } else if request_key.starts_with("clp-") {
        state
            .claudeplus
            .respond_permission(request_key, option_id)
            .await
    } else if request_key.starts_with("cup-") {
        state
            .cursorplus
            .respond_permission(request_key, option_id)
            .await
    } else if request_key.starts_with("ocp-") {
        state
            .opencodeplus
            .respond_permission(request_key, option_id)
            .await
    } else if request_key.starts_with("codex-") {
        state.codex.respond_permission(request_key, option_id).await
    } else {
        state.acp.respond_permission(request_key, option_id).await
    }
}

fn remote_file(app: &AppHandle, thread_id: &str, cwd: &str, path: &str) -> Result<Value, String> {
    let thread_cwd = {
        let state = app.state::<AppState>();
        let store = state.store.lock().unwrap();
        let thread = store.get(thread_id).ok_or("会话不存在")?;
        if !eligible(thread) || thread.cwd != cwd {
            return Err("文件不属于该会话".into());
        }
        thread.cwd.clone()
    };
    let requested = std::path::Path::new(normalize_remote_file_path(path));
    let abs = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        std::path::Path::new(&thread_cwd).join(requested)
    };
    let abs = std::fs::canonicalize(abs).map_err(|_| "文件不存在")?;
    let meta = abs.metadata().map_err(|e| format!("读取文件失败：{e}"))?;
    if !meta.is_file() {
        return Err("目标不是文件".into());
    }
    if meta.len() > REMOTE_FILE_MAX_BYTES {
        return Err("文件超过 50 MiB，无法通过中转下载".into());
    }
    let bytes = std::fs::read(&abs).map_err(|e| format!("读取文件失败：{e}"))?;
    let name = abs
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("download")
        .to_string();
    Ok(json!({
        "name": name,
        "data": base64::engine::general_purpose::STANDARD.encode(bytes),
    }))
}

// Codex file links on Windows can use /D:/path syntax. Rust treats that as a
// rooted path without a drive prefix, so joining/canonicalizing it fails.
fn normalize_remote_file_path(path: &str) -> &str {
    let path = path.trim();
    let bytes = path.as_bytes();
    if bytes.len() >= 4
        && bytes[0] == b'/'
        && bytes[1].is_ascii_alphabetic()
        && bytes[2] == b':'
        && matches!(bytes[3], b'/' | b'\\')
    {
        &path[1..]
    } else {
        path
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
    // repo_root 本身会失败于非仓库，省掉额外的 is_repo 进程启动（Windows 上很贵）
    crate::gitwt::repo_root(cwd).map_err(|_| "该目录不是 git 仓库".into())
}

fn remote_git_status(app: &AppHandle, cwd: &str) -> Result<Value, String> {
    let root = ensure_remote_git_cwd(app, cwd)?;
    // -b 把分支信息放进同一条 status，少一次 git 进程；
    // 默认 untracked=normal（目录级），比 -uall 枚举全部未跟踪文件快得多。
    let porcelain = crate::gitwt::run(
        &root,
        &[
            "status",
            "--porcelain=v1",
            "-b",
            "--untracked-files=normal",
            "--ignore-submodules=dirty",
        ],
    )?;
    let mut branch = "HEAD".to_string();
    let mut files = Vec::new();
    for line in porcelain.lines() {
        if let Some(rest) = line.strip_prefix("## ") {
            let name = rest
                .split("...")
                .next()
                .unwrap_or(rest)
                .split_whitespace()
                .next()
                .unwrap_or("HEAD");
            branch = name.to_string();
            continue;
        }
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

fn looks_binary(sample: &[u8]) -> bool {
    sample.iter().any(|&b| b == 0)
}

fn truncate_str(mut s: String, limit: usize) -> (String, bool) {
    if s.len() <= limit {
        return (s, false);
    }
    // 尽量按 UTF-8 边界截断，避免 panic
    let mut end = limit;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
    (s, true)
}

fn remote_git_file(app: &AppHandle, cwd: &str, path: &str) -> Result<Value, String> {
    let root = ensure_remote_git_cwd(app, cwd)?;
    let path = path.trim().trim_start_matches(['/', '\\']);
    if path.is_empty() || path.contains("..") {
        return Err("文件路径无效".into());
    }
    let abs = std::path::Path::new(&root).join(path);
    // 已跟踪文件一律走统一 diff（改几行不再传双全文）；未跟踪才传 newText
    const FULL_TEXT_LIMIT: usize = 120_000;
    const UNIFIED_LIMIT: usize = 600_000;
    const SAMPLE: usize = 8_192;

    let in_head = crate::gitwt::run(&root, &["cat-file", "-e", &format!("HEAD:{path}")]).is_ok();
    let new_meta = abs.metadata().ok();
    let new_is_file = new_meta.as_ref().map(|m| m.is_file()).unwrap_or(false);

    // 先抽样判二进制，避免把大二进制读进内存
    if new_is_file {
        if let Ok(mut f) = std::fs::File::open(&abs) {
            use std::io::Read;
            let mut buf = vec![0u8; SAMPLE];
            let n = f.read(&mut buf).unwrap_or(0);
            if looks_binary(&buf[..n]) {
                return Ok(json!({
                    "path": path,
                    "binary": true,
                    "oldText": "",
                    "newText": "",
                    "unified": "",
                    "truncated": false,
                }));
            }
        }
    }

    // 已跟踪：始终用 git 统一 diff，真实改动几行就只传几行
    if in_head {
        let unified = crate::gitwt::run_raw(
            &root,
            &[
                "diff",
                "--no-color",
                "--unified=3",
                "--ignore-cr-at-eol",
                "HEAD",
                "--",
                path,
            ],
        )
        .unwrap_or_default();
        if unified.contains("Binary files ") || looks_binary(unified.as_bytes()) {
            return Ok(json!({
                "path": path,
                "binary": true,
                "oldText": "",
                "newText": "",
                "unified": "",
                "truncated": false,
            }));
        }
        let (unified, truncated) = truncate_str(unified, UNIFIED_LIMIT);
        // 删除文件时 git diff 通常仍有输出；若为空则回退展示旧内容头
        if unified.is_empty() && !new_is_file {
            let old = crate::gitwt::run_raw(&root, &["show", &format!("HEAD:{path}")])
                .unwrap_or_default();
            if looks_binary(old.as_bytes()) {
                return Ok(json!({
                    "path": path,
                    "binary": true,
                    "oldText": "",
                    "newText": "",
                    "unified": "",
                    "truncated": false,
                }));
            }
            let (old, truncated) = truncate_str(old, FULL_TEXT_LIMIT);
            return Ok(json!({
                "path": path,
                "binary": false,
                "oldText": old,
                "newText": "",
                "unified": "",
                "truncated": truncated,
            }));
        }
        return Ok(json!({
            "path": path,
            "binary": false,
            "oldText": "",
            "newText": "",
            "unified": unified,
            "truncated": truncated,
        }));
    }

    // 未跟踪：只传截断后的新内容，前端按整文件新增展示
    let new_text = if new_is_file {
        std::fs::read_to_string(&abs).unwrap_or_default()
    } else {
        String::new()
    };
    if looks_binary(new_text.as_bytes()) {
        return Ok(json!({
            "path": path,
            "binary": true,
            "oldText": "",
            "newText": "",
            "unified": "",
            "truncated": false,
        }));
    }
    let (new_text, truncated) = truncate_str(new_text, FULL_TEXT_LIMIT);
    Ok(json!({
        "path": path,
        "binary": false,
        "oldText": "",
        "newText": new_text,
        "unified": "",
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
        false, // 临时目录只决定 cwd；Server 创建的会话仍是普通持久会话。
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
    if text.trim().is_empty() {
        return Err("内容不能为空".into());
    }
    let state = app.state::<AppState>();
    {
        let store = state.store.lock().unwrap();
        let thread = store.get(thread_id).ok_or("会话不存在")?;
        if !eligible(thread) {
            return Err("该会话不支持远程操作".into());
        }
    }
    // 复用本地聊天的唯一分发入口：运行中的 Codex/Devin 会走 steer_prompt，
    // 其余后端也与桌面输入框保持完全一致，避免远程入口再次出现能力漂移。
    crate::dispatch_prompt(app, thread_id.to_string(), text.to_string(), Vec::new())
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
    match kind {
        AgentKind::Devin => state.acp.cancel(thread_id).await,
        AgentKind::Codex | AgentKind::CodexPlus => state.codexplus.cancel(thread_id).await,
        AgentKind::CodeBuddy | AgentKind::CodeBuddyPlus => {
            state.codebuddyplus.cancel(thread_id).await
        }
        AgentKind::ClaudeCode => state.claudeplus.cancel(thread_id).await,
        AgentKind::Cursor => state.cursorplus.cancel(thread_id).await,
        AgentKind::OpenCode | AgentKind::OpenCodePlus => state.opencodeplus.cancel(thread_id).await,
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
