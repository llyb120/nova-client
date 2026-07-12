//! server 侧远程会话客户端。
//!
//! 空闲期只维持一个低流量命令长轮询，不上传会话；会话运行时首包/纠错/收尾发全量，
//! 中间流式阶段只发变化条目。所有请求体均 gzip，响应由 reqwest 自动解 gzip。

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

const ACTIVE_INTERVAL: Duration = Duration::from_secs(2);
const REMOTE_SCRATCH_PATH: &str = "__nova_scratch__";

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

#[derive(Serialize, Clone)]
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
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ServerResponse {
    #[serde(default)]
    commands: Vec<RemoteCommand>,
    #[serde(default)]
    wanted_thread_id: String,
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
    let mut wanted = String::new();
    let mut revision = 0i64;
    let mut previous: HashMap<String, Thread> = HashMap::new();
    let mut force_full = false;
    let mut was_running = false;
    let mut ack_id = 0i64;
    let mut processed_id = 0i64;
    let mut results: Vec<CommandResult> = Vec::new();

    loop {
        let Some(cfg) = config(&app) else {
            last_cfg = None;
            wanted.clear();
            revision = 0;
            previous.clear();
            force_full = false;
            sleep(Duration::from_secs(10)).await;
            continue;
        };
        if last_cfg.as_ref() != Some(&cfg) {
            client = build_client(&cfg.proxy);
            last_cfg = Some(cfg.clone());
            wanted.clear();
            revision = 0;
            previous.clear();
            force_full = false;
            was_running = false;
            ack_id = 0;
            processed_id = 0;
            results.clear();
        }

        let current = sync_threads(&app, &wanted);
        let any_running = threads_running(&app, current.values());
        let tracked_changed = !previous.is_empty()
            && (previous.len() != current.len()
                || previous.keys().any(|id| !current.contains_key(id)));
        if (was_running && !any_running) || tracked_changed {
            force_full = true; // 一轮结束用全量校准，之后停止上传。
        }

        if force_full || any_running || !results.is_empty() {
            let next_revision = revision.saturating_add(1).max(1);
            let (body, sent_full) = if force_full || previous.is_empty() {
                let snap = full_snapshot(&app, &wanted, &current);
                (
                    json!({
                        "deviceName": cfg.name,
                        "ackId": ack_id,
                        "results": results,
                        "kind": "full",
                        "baseRevision": revision,
                        "revision": next_revision,
                        "snapshot": snap,
                    }),
                    true,
                )
            } else {
                let mut deltas = Vec::with_capacity(current.len());
                for (id, current_thread) in &current {
                    let Some(old) = previous.get(id) else {
                        force_full = true;
                        break;
                    };
                    let Some(delta) = make_delta(old, current_thread, &app) else {
                        force_full = true;
                        break;
                    };
                    deltas.push(delta);
                }
                if force_full {
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
                    false,
                )
            };

            match sync(&client, &cfg, &body).await {
                Ok(resp) => {
                    revision = resp.revision.max(next_revision);
                    previous = current;
                    force_full = resp.need_full;
                    results.clear();
                    if resp.wanted_thread_id != wanted {
                        wanted = resp.wanted_thread_id;
                        previous.clear();
                        force_full = true;
                    }
                    process_commands(
                        &app,
                        resp.commands,
                        &mut processed_id,
                        &mut ack_id,
                        &mut results,
                        &mut force_full,
                        &mut wanted,
                    )
                    .await;
                    if sent_full && !any_running && results.is_empty() && !force_full {
                        was_running = false;
                    } else {
                        was_running = any_running;
                    }
                }
                Err(e) => sleep(error_backoff(&e)).await,
            }
            if any_running || force_full || !results.is_empty() {
                sleep(ACTIVE_INTERVAL).await;
            }
            continue;
        }

        // 空闲不上传快照；只用长轮询等待网页命令或 server 请求补全量。
        match pull(&client, &cfg).await {
            Ok(resp) => {
                revision = resp.revision;
                if resp.need_full {
                    force_full = true;
                }
                if resp.wanted_thread_id != wanted {
                    wanted = resp.wanted_thread_id;
                    previous.clear();
                    force_full = true;
                }
                process_commands(
                    &app,
                    resp.commands,
                    &mut processed_id,
                    &mut ack_id,
                    &mut results,
                    &mut force_full,
                    &mut wanted,
                )
                .await;
            }
            Err(e) => sleep(error_backoff(&e)).await,
        }
        was_running = any_eligible_thread_running(&app);
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

fn selected_thread(app: &AppHandle, wanted: &str) -> Option<Thread> {
    let state = app.state::<AppState>();
    let store = state.store.lock().unwrap();
    if !wanted.is_empty() {
        return store.get(wanted).filter(|t| eligible(t)).cloned();
    }
    store
        .threads
        .iter()
        .filter(|t| eligible(t))
        .max_by_key(|t| t.updated_at)
        .cloned()
}

/// server 当前查看的会话始终同步；除此之外，所有正在运行的普通会话也加入同步集合。
/// 这里只决定上传范围，不修改桌面端 active_thread，因此后台会话不会被强制切到前台。
fn sync_threads(app: &AppHandle, wanted: &str) -> HashMap<String, Thread> {
    let state = app.state::<AppState>();
    let store = state.store.lock().unwrap();
    let selected_id = if !wanted.is_empty() && store.get(wanted).is_some_and(eligible) {
        Some(wanted.to_string())
    } else {
        store
            .threads
            .iter()
            .filter(|t| eligible(t))
            .max_by_key(|t| t.updated_at)
            .map(|t| t.id.clone())
    };
    store
        .threads
        .iter()
        .filter(|t| eligible(t))
        .filter(|t| selected_id.as_deref() == Some(t.id.as_str()) || is_running(&state, t))
        .map(|t| (t.id.clone(), t.clone()))
        .collect()
}

fn threads_running<'a>(app: &AppHandle, threads: impl Iterator<Item = &'a Thread>) -> bool {
    let state = app.state::<AppState>();
    threads.into_iter().any(|thread| is_running(&state, thread))
}

fn any_eligible_thread_running(app: &AppHandle) -> bool {
    let state = app.state::<AppState>();
    let store = state.store.lock().unwrap();
    store
        .threads
        .iter()
        .filter(|thread| eligible(thread))
        .any(|thread| is_running(&state, thread))
}

fn full_snapshot(
    app: &AppHandle,
    wanted: &str,
    synced: &HashMap<String, Thread>,
) -> RemoteSnapshot {
    let selected = selected_thread(app, wanted);
    RemoteSnapshot {
        projects: projects(app),
        threads: thread_metas(app),
        thread: selected.as_ref().map(remote_thread_value),
        thread_snapshots: synced
            .iter()
            .map(|(id, thread)| (id.clone(), remote_thread_value(thread)))
            .collect(),
        models: models(app),
    }
}

fn remote_thread_value(thread: &Thread) -> Value {
    let mut thread = thread.clone();
    for item in &mut thread.items {
        if let Item::User { images, .. } = item {
            for image in images {
                image.data = None;
                image.uri = None;
            }
        }
    }
    serde_json::to_value(thread).unwrap_or(Value::Null)
}

fn remote_item_value(item: &Item) -> Value {
    let mut item = item.clone();
    if let Item::User { images, .. } = &mut item {
        for image in images {
            image.data = None;
            image.uri = None;
        }
    }
    serde_json::to_value(item).unwrap_or(Value::Null)
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
            .map(|old| serde_json::to_vec(old).ok() != serde_json::to_vec(item).ok())
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

async fn process_commands(
    app: &AppHandle,
    commands: Vec<RemoteCommand>,
    processed_id: &mut i64,
    ack_id: &mut i64,
    results: &mut Vec<CommandResult>,
    force_full: &mut bool,
    wanted: &mut String,
) {
    for cmd in commands {
        if cmd.id <= *processed_id {
            continue;
        }
        let result = execute_command(app, &cmd).await;
        *processed_id = cmd.id;
        *ack_id = cmd.id;
        results.push(match result {
            Ok(thread_id) => {
                // 创建/发送/停止成功后立即把该会话设为下一次同步目标。
                // 尤其是 create：必须让新会话全量快照与 command result 同包到达 server，
                // 避免 server 先切 selectedThreadId、快照却仍是旧会话而一直显示“正在同步”。
                if !thread_id.is_empty() {
                    *wanted = thread_id.clone();
                }
                CommandResult {
                    id: cmd.id,
                    ok: true,
                    error: String::new(),
                    thread_id,
                }
            }
            Err(error) => CommandResult {
                id: cmd.id,
                ok: false,
                error,
                thread_id: String::new(),
            },
        });
        *force_full = true;
    }
}

async fn execute_command(app: &AppHandle, cmd: &RemoteCommand) -> Result<String, String> {
    match cmd.kind.as_str() {
        "create" => {
            let thread = create_thread(app, cmd)?;
            send_prompt(app, &thread.id, &cmd.text)?;
            Ok(thread.id)
        }
        "send" => {
            send_prompt(app, &cmd.thread_id, &cmd.text)?;
            Ok(cmd.thread_id.clone())
        }
        "stop" => {
            stop_thread(app, &cmd.thread_id).await?;
            Ok(cmd.thread_id.clone())
        }
        _ => Err("不支持的远程操作".into()),
    }
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
