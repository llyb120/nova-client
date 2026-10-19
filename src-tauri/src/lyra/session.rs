//! 多 agent（Codex multi_agents_v2 的精简版）：主 agent 用 spawn_agent 派生进程内子 agent，
//! 用 send_message / followup_task / wait_agent / list_agents / interrupt_agent / close_agent 协作。
//! 子 agent 继承主 agent 的模型、工具与工作目录，模型历史只在内存里，深度上限 1。
//! Nova 中额外把任务和执行事件投影到只读 Stage，展示记录落盘，不参与调度。
//! 场景转交子任务的 Stage 会话本身落盘：注册表里找不到时（如 Nova 重启后）按 task_name 从会话存储找回继续用。

use crate::lyra::context::ContextWindow;
use crate::lyra::history::{text_content, user_message, History};
use crate::lyra::tools::{Tool, ToolOutcome};
use crate::lyra::turn::{run_turn, text_outcome, BoxFuture, Session};
use crate::lyra::bridge::{turn_event_sink, Emit};
use crate::lyra::stage::Stage;
use crate::settings::Scene;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

const MAX_ACTIVE: usize = 6;
const ROOT: &str = "/root";

struct Child {
    /// pending_init / running / completed / errored / interrupted / shutdown
    status: Mutex<(String, Option<String>)>,
    tasks: Mutex<Option<tokio::sync::mpsc::UnboundedSender<(u64, Value)>>>,
    task_revision: AtomicU64,
    steer: Arc<Mutex<VecDeque<Value>>>,
    cancelled: Arc<AtomicBool>,
    stage: Option<Stage>,
    /// 场景转交子任务所在的 Nova 会话（失效切换后会换成新会话）；进程内子 agent 为 None。
    scene_thread: Mutex<Option<String>>,
}

impl Child {
    fn status(&self) -> (String, Option<String>) {
        self.status.lock().unwrap().clone()
    }

    fn active(&self) -> bool {
        matches!(self.status().0.as_str(), "pending_init" | "running")
    }

    fn row(&self, path: &str) -> Value {
        let (status, message) = self.status();
        let mut row = json!({ "path": path, "status": status, "message": message });
        if let Some(thread) = self.scene_thread.lock().unwrap().clone() {
            row["threadId"] = thread.into();
        }
        row
    }
}

#[derive(Default)]
struct Registry {
    agents: Mutex<HashMap<String, Arc<Child>>>,
    stopped: AtomicBool,
    changed: tokio::sync::Notify,
}

/// 主 agent 持有的控制面句柄。
#[derive(Clone)]
pub struct AgentHandle {
    registry: Arc<Registry>,
    stage_parent: Option<(tauri::AppHandle, String)>,
}

static REGISTRIES: OnceLock<Mutex<HashMap<String, Arc<Registry>>>> = OnceLock::new();

/// 停止调度器和已排队的工作；后端的停止命令由公共 cancel_turn 并行发出。
pub(crate) fn stop_for_thread(thread_id: &str) -> Vec<String> {
    let handle = root_handle(&format!("thread:{thread_id}"));
    let agents = handle.registry.agents.lock().unwrap();
    handle.registry.stopped.store(true, Ordering::SeqCst);
    let mut scenes = Vec::new();
    for child in agents.values() {
        let _sender = child.tasks.lock().unwrap();
        child.task_revision.fetch_add(1, Ordering::SeqCst);
        child.cancelled.store(true, Ordering::SeqCst);
        if let Some(id) = child.scene_thread.lock().unwrap().clone() { scenes.push(id); }
    }
    handle.registry.changed.notify_waiters();
    scenes
}

/// 只有新一轮明确投递的任务才重新开放调度；停止中的旧工具调用不得自行恢复。
pub(crate) fn resume_for_thread(thread_id: &str) {
    root_handle(&format!("thread:{thread_id}")).registry.stopped.store(false, Ordering::SeqCst);
}

// ponytail: 注册表按根会话常驻进程内存、不回收；进程内子 agent 随 close_agent 或进程退出结束，场景子任务可从落盘的 Stage 会话找回。
pub fn root_handle(session_id: &str) -> AgentHandle {
    let registry = REGISTRIES
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .entry(session_id.to_string())
        .or_default()
        .clone();
    AgentHandle { registry, stage_parent: None }
}

const AGENT_TOOLS: [&str; 7] = [
    "spawn_agent",
    "send_message",
    "followup_task",
    "wait_agent",
    "list_agents",
    "interrupt_agent",
    "close_agent",
];

pub fn is_agent_tool(name: &str) -> bool {
    AGENT_TOOLS.contains(&name)
}

/// 自动模式会话可按场景转交：spawn_agent 增加 scene 参数，只列出模型不同且不在上游链上的场景。
fn scene_param(handoff: &[Scene]) -> (String, Value) {
    let scenes = handoff.iter().map(|s| format!("{}={}（{}）", s.key(), s.label(), s.when())).collect::<Vec<_>>().join("；");
    (
        " Auto mode: when a whole chunk of work fits one of the scene options, pass scene to hand it off to that scene's model in its own Stage session (it sees this conversation and closes the loop itself); then wait_agent for the final result instead of directing it step by step. To continue or correct an existing scene agent (even from an earlier turn), use followup_task on its task_name instead of spawning another.".into(),
        json!({ "type": "string", "enum": handoff.iter().map(|s| s.key()).collect::<Vec<_>>(), "description": format!("optional handoff scene: {scenes}. Omit for an in-process helper on this model.") }),
    )
}

fn cwd_param() -> Value {
    json!({ "type": "string", "description": "with scene: absolute project directory for the handoff session; default is this session's directory" })
}

pub fn agent_tools(handoff: Option<Vec<Scene>>) -> Vec<Tool> {
    let target = json!({ "type": "string", "description": "agent path from spawn_agent/list_agents, or its task_name" });
    let target_and_message = json!({
        "type": "object",
        "properties": { "target": target, "message": { "type": "string" } },
        "required": ["target", "message"]
    });
    let target_only = json!({ "type": "object", "properties": { "target": target }, "required": ["target"] });
    let mut spawn = Tool {
        name: "spawn_agent",
        description: format!("Start a sub-agent on a self-contained task; it runs in the background with your tools and working directory. Use for independent work that can proceed in parallel, then wait_agent for results. At most {MAX_ACTIVE} run at once; sub-agents cannot spawn further agents."),
        parameters: json!({
            "type": "object",
            "properties": {
                "task_name": { "type": "string", "pattern": "^[a-z0-9_]+$", "description": "short unique name, lowercase letters, digits, underscores" },
                "message": { "type": "string", "description": "the full task; the agent sees only this plus any forked history" },
                "fork_turns": { "type": "string", "description": "history to give the agent: \"none\" (default), \"all\", or a number of recent user turns" }
            },
            "required": ["task_name", "message"]
        }),
    };
    if let Some(handoff) = handoff {
        let (hint, param) = scene_param(&handoff);
        spawn.description.push_str(&hint);
        spawn.parameters["properties"]["scene"] = param;
        spawn.parameters["properties"]["cwd"] = cwd_param();
    }
    vec![
        spawn,
        Tool {
            name: "send_message",
            description: "Queue a message for an agent without starting a new task; a running agent sees it before its next model request.".into(),
            parameters: target_and_message.clone(),
        },
        Tool {
            name: "followup_task",
            description: "Give an agent a new task; it starts after the agent's current task finishes.".into(),
            parameters: target_and_message,
        },
        Tool {
            name: "wait_agent",
            description: "Wait until any of the target agents (default: all of yours) is no longer running, or until the timeout. Returns every target's status and final message.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "targets": { "type": "array", "items": { "type": "string" } },
                    "timeout_ms": { "type": "integer", "minimum": 1000, "maximum": 600000, "description": "default 30000" }
                }
            }),
        },
        Tool {
            name: "list_agents",
            description: "List sub-agents with their status and final message.".into(),
            parameters: json!({ "type": "object", "properties": { "path_prefix": { "type": "string" } } }),
        },
        Tool {
            name: "interrupt_agent",
            description: "Cancel an agent's current task; the agent stays available for followup_task.".into(),
            parameters: target_only.clone(),
        },
        Tool {
            name: "close_agent",
            description: "Cancel and shut down an agent.".into(),
            parameters: target_only,
        },
    ]
}

impl AgentHandle {
    pub(super) fn with_stage(mut self, parent: Option<(tauri::AppHandle, String)>) -> Self {
        self.stage_parent = parent;
        self
    }

    fn resolve(&self, target: &str) -> Result<(String, Arc<Child>), String> {
        let path = if target.starts_with('/') { target.to_string() } else { format!("{ROOT}/{target}") };
        let mut agents = self.registry.agents.lock().unwrap();
        self.lookup(&mut agents, &path)
            .map(|child| (path, child))
            .ok_or_else(|| format!("not_found: no agent {target}"))
    }

    /// 注册表优先；没有时从会话存储找回同名场景子任务并登记。
    fn lookup(&self, agents: &mut HashMap<String, Arc<Child>>, path: &str) -> Option<Arc<Child>> {
        if let Some(child) = agents.get(path) {
            return Some(child.clone());
        }
        let child = self.revive(path)?;
        agents.insert(path.to_string(), child.clone());
        Some(child)
    }

    /// 找回父会话下最近一个同名场景 Stage 会话（失效切换会新建同名会话，取最新），接回 scene_loop。
    fn revive(&self, path: &str) -> Option<Arc<Child>> {
        use tauri::Manager;
        let (app, parent_id) = self.stage_parent.as_ref()?;
        let name = path.strip_prefix(ROOT)?.strip_prefix('/')?;
        let state = app.state::<crate::AppState>();
        let (thread_id, scene, cwd, reply) = {
            let store = state.store.lock().unwrap();
            let parent_cwd = store.get(parent_id)?.cwd.clone();
            let (thread, scene) = store
                .threads
                .iter()
                .filter(|t| t.stage_source_thread_id.as_deref() == Some(parent_id.as_str()))
                .filter_map(|t| t.auto_scene.as_deref().and_then(Scene::parse).map(|s| (t, s)))
                .filter(|(t, s)| t.title == format!("[{}] {name}", s.label()))
                .max_by_key(|(t, _)| t.created_at)?;
            let reply = thread.items.iter().rev().find_map(|item| match item {
                crate::threads::Item::Assistant { text, .. } if !text.trim().is_empty() => Some(text.trim().to_string()),
                _ => None,
            });
            // 与父会话同目录说明当初没指定 cwd：失效切换时继续沿用父会话（含 worktree）。
            let cwd = (thread.cwd != parent_cwd).then(|| thread.cwd.clone());
            (thread.id.clone(), scene, cwd, reply)
        };
        let (child, rx) = new_child(None, None, Some(thread_id)).ok()?;
        *child.status.lock().unwrap() = ("completed".into(), reply);
        tauri::async_runtime::spawn(scene_loop(child.clone(), app.clone(), parent_id.clone(), scene, name.into(), cwd, rx, self.registry.clone()));
        Some(child)
    }

    fn report(&self, prefix: Option<&str>) -> Value {
        let agents = self.registry.agents.lock().unwrap();
        let mut rows: Vec<Value> = agents
            .iter()
            .filter(|(path, _)| prefix.is_none_or(|p| path.starts_with(p)))
            .map(|(path, child)| child.row(path))
            .collect();
        rows.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
        Value::Array(rows)
    }
}

pub async fn call(handle: &AgentHandle, parent: &Session, name: &str, args: &Value) -> ToolOutcome {
    let result = match name {
        "spawn_agent" if args.get("scene").and_then(Value::as_str).is_some_and(|s| !s.is_empty()) => {
            match &handle.stage_parent {
                Some((app, parent_id)) => scene_spawn(handle, app, parent_id, args),
                None => Err("scene handoff requires a Nova session".into()),
            }
        }
        "spawn_agent" => spawn(handle, parent, args),
        "list_agents" => Ok(handle.report(args.get("path_prefix").and_then(Value::as_str))),
        "wait_agent" => wait(handle, Some(&parent.cancelled), args, 600_000).await,
        _ => control(handle, name, args),
    };
    match result {
        Ok(value) => text_outcome(value.to_string(), false),
        Err(error) => text_outcome(error, true),
    }
}

/// nova-tools MCP 入口：非 Lyra 后端的自动模式会话只能按场景转交（子任务在场景模型的 Stage 会话里闭环）。
pub async fn call_for_thread(app: &tauri::AppHandle, thread_id: &str, name: &str, args: &Value) -> Result<Value, String> {
    let handle = root_handle(&format!("thread:{thread_id}")).with_stage(Some((app.clone(), thread_id.to_string())));
    match name {
        "spawn_agent" => scene_spawn(&handle, app, thread_id, args),
        "list_agents" => Ok(handle.report(args.get("path_prefix").and_then(Value::as_str))),
        // MCP 客户端单次调用约 120s 超时，等待上限留足余量。
        "wait_agent" => wait(&handle, None, args, 90_000).await,
        _ if is_agent_tool(name) => control(&handle, name, args),
        _ => Err(format!("unknown agent tool: {name}")),
    }
}

fn control(handle: &AgentHandle, name: &str, args: &Value) -> Result<Value, String> {
    let target = args.get("target").and_then(Value::as_str).unwrap_or_default();
    let message = args.get("message").and_then(Value::as_str).unwrap_or_default();
    handle.resolve(target).and_then(|(path, child)| {
        let stop_scene = matches!(name, "interrupt_agent" | "close_agent") && child.active();
        match name {
            "send_message" if child.scene_thread.lock().unwrap().is_some() => {
                return Err("场景子任务独立闭环，不接收中途插话；需要追加工作请用 followup_task".into());
            }
            "send_message" => {
                if let Some(stage) = &child.stage {
                    stage.note(format!("补充消息（已排队，下次模型请求前交付）：\n{message}"));
                }
                child.steer.lock().unwrap().push_back(user_message(message, &[]));
            }
            "followup_task" => {
                let _agents = handle.registry.agents.lock().unwrap();
                if handle.registry.stopped.load(Ordering::SeqCst) { return Err("本轮调度已停止，请等待用户继续任务".into()); }
                let sent = child.tasks.lock().unwrap().as_ref().map(|tx|
                    tx.send((child.task_revision.load(Ordering::SeqCst), user_message(message, &[]))));
                if !matches!(sent, Some(Ok(()))) {
                    return Err(format!("{path} is shut down"));
                }
                if let Some(stage) = &child.stage {
                    stage.note(format!("主会话已提交后续任务：\n{message}"));
                }
            }
            "interrupt_agent" => {
                child.cancelled.store(true, Ordering::SeqCst);
                if let Some(stage) = &child.stage {
                    stage.note("主会话已请求中断。".into());
                }
            }
            _ => {
                if let Some(stage) = &child.stage {
                    stage.note("主会话已请求关闭。".into());
                }
                child.cancelled.store(true, Ordering::SeqCst);
                child.tasks.lock().unwrap().take();
                handle.registry.agents.lock().unwrap().remove(&path);
            }
        }
        if stop_scene {
            if let (Some((app, _)), Some(thread_id)) = (&handle.stage_parent, child.scene_thread.lock().unwrap().clone()) {
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    use tauri::Manager;
                    let _ = crate::cancel_turn(app.clone(), app.state(), thread_id, None, None).await;
                });
            }
        }
        Ok(json!({ "path": path, "status": child.status().0 }))
    })
}

type Agents<'a> = std::sync::MutexGuard<'a, HashMap<String, Arc<Child>>>;

/// 校验 task_name 并占住注册表槽位：返回 (task_name, message, path, 已加锁的注册表)。
fn reserve<'a>(handle: &'a AgentHandle, args: &'a Value) -> Result<(&'a str, &'a str, String, Agents<'a>), String> {
    let task_name = args.get("task_name").and_then(Value::as_str).unwrap_or_default();
    if task_name.is_empty() || !task_name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') {
        return Err("task_name must be lowercase letters, digits and underscores".into());
    }
    let message = args.get("message").and_then(Value::as_str).unwrap_or_default();
    let path = format!("{ROOT}/{task_name}");
    let mut agents = handle.registry.agents.lock().unwrap();
    if handle.registry.stopped.load(Ordering::SeqCst) { return Err("本轮调度已停止，请等待用户继续任务".into()); }
    if handle.lookup(&mut agents, &path).is_some_and(|c| c.status().0 != "shutdown") {
        return Err(format!("{path} already exists; use followup_task or pick another task_name"));
    }
    if agents.values().filter(|c| c.active()).count() >= MAX_ACTIVE {
        return Err(format!("{MAX_ACTIVE} agents are already running; wait_agent or close_agent first"));
    }
    Ok((task_name, message, path, agents))
}

fn new_child(message: Option<&str>, stage: Option<Stage>, scene_thread: Option<String>) -> Result<(Arc<Child>, tokio::sync::mpsc::UnboundedReceiver<(u64, Value)>), String> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    if let Some(message) = message {
        tx.send((0, user_message(message, &[]))).map_err(|e| e.to_string())?;
    }
    let child = Arc::new(Child {
        status: Mutex::new(("pending_init".into(), None)),
        tasks: Mutex::new(Some(tx)),
        task_revision: AtomicU64::new(0),
        steer: Arc::new(Mutex::new(VecDeque::new())),
        cancelled: Arc::new(AtomicBool::new(false)),
        stage,
        scene_thread: Mutex::new(scene_thread),
    });
    Ok((child, rx))
}

/// 场景转交：子任务在场景模型（可跨后端）的 Stage 会话里独立闭环，主 agent 只等最终结果。
fn scene_spawn(handle: &AgentHandle, app: &tauri::AppHandle, parent_id: &str, args: &Value) -> Result<Value, String> {
    let scene = args
        .get("scene")
        .and_then(Value::as_str)
        .and_then(Scene::parse)
        .ok_or("scene must be lightweight, coding or computer")?;
    let cwd = args.get("cwd").and_then(Value::as_str).map(str::trim).filter(|c| !c.is_empty()).map(str::to_string);
    let (task_name, message, path, mut agents) = reserve(handle, args)?;
    let Some(thread_id) = crate::create_scene_stage(app, parent_id, scene, task_name, cwd.as_deref(), 0)? else {
        return Ok(json!({ "ok": false, "next": "该场景的模型与当前会话相同，不要转交，直接在本会话完成。" }));
    };
    let (child, rx) = new_child(Some(message), None, Some(thread_id.clone()))?;
    agents.insert(path.clone(), child.clone());
    drop(agents);
    tauri::async_runtime::spawn(scene_loop(
        child,
        app.clone(),
        parent_id.to_string(),
        scene,
        task_name.to_string(),
        cwd,
        rx,
        handle.registry.clone(),
    ));
    Ok(json!({ "path": path, "status": "pending_init", "threadId": thread_id }))
}

async fn scene_loop(
    child: Arc<Child>,
    app: tauri::AppHandle,
    parent_id: String,
    scene: Scene,
    name: String,
    cwd: Option<String>,
    mut tasks: tokio::sync::mpsc::UnboundedReceiver<(u64, Value)>,
    registry: Arc<Registry>,
) {
    while let Some((revision, task)) = tasks.recv().await {
        {
            let sender = child.tasks.lock().unwrap();
            if sender.is_none() { break; }
            if revision != child.task_revision.load(Ordering::SeqCst) {
                *child.status.lock().unwrap() = ("interrupted".into(), None);
                registry.changed.notify_waiters();
                continue;
            }
            child.cancelled.store(false, Ordering::SeqCst);
            if let Some(id) = child.scene_thread.lock().unwrap().as_deref() { resume_for_thread(id); }
        }
        *child.status.lock().unwrap() = ("running".into(), None);
        let text = text_content(&task["content"]);
        let mut attempt = 0;
        let status = loop {
            let thread_id = child.scene_thread.lock().unwrap().clone().unwrap_or_default();
            let result = run_scene_turn(&app, &thread_id, &text, &child.cancelled).await;
            if child.cancelled.load(Ordering::SeqCst) {
                break ("interrupted", result.ok().flatten());
            }
            let error = match result {
                Ok(Some(reply)) => break ("completed", Some(reply)),
                Ok(None) => "场景模型没有返回内容".to_string(),
                Err(error) => error,
            };
            // 没有任何输出：视为该模型失效，按场景列表顺序换下一个模型在新 Stage 会话重做。
            attempt += 1;
            match crate::create_scene_stage(&app, &parent_id, scene, &name, cwd.as_deref(), attempt) {
                Ok(Some(next)) => *child.scene_thread.lock().unwrap() = Some(next),
                _ => break ("errored", Some(error)),
            }
        };
        *child.status.lock().unwrap() = (status.0.into(), status.1);
        registry.changed.notify_waiters();
    }
    *child.status.lock().unwrap() = ("shutdown".into(), None);
    registry.changed.notify_waiters();
}

/// 投递到场景会话并等它跑完，返回本轮新增的最后一条助手回复。
async fn run_scene_turn(app: &tauri::AppHandle, thread_id: &str, text: &str, cancelled: &AtomicBool) -> Result<Option<String>, String> {
    use tauri::Manager;
    let state = app.state::<crate::AppState>();
    let before = state.store.lock().unwrap().get(thread_id).map_or(0, |t| t.items.len());
    if cancelled.load(Ordering::SeqCst) { return Ok(None); }
    crate::dispatch_scene_prompt(app, thread_id.to_string(), text.to_string(), Vec::new())?;
    let started = Instant::now();
    let mut seen_running = false;
    loop {
        tokio::time::sleep(Duration::from_millis(500)).await;
        if cancelled.load(Ordering::SeqCst) {
            let _ = crate::cancel_single_turn(app, &state, thread_id.to_string()).await;
            break;
        }
        let running = crate::running_by_id(&state, thread_id);
        seen_running |= running;
        // dispatch 异步起跑：给 30s 宽限再判定“没跑起来”。
        if !running && (seen_running || started.elapsed() > Duration::from_secs(30)) {
            break;
        }
    }
    let store = state.store.lock().unwrap();
    Ok(store.get(thread_id).and_then(|thread| {
        thread.items.iter().skip(before).rev().find_map(|item| match item {
            crate::threads::Item::Assistant { text, .. } if !text.trim().is_empty() => Some(text.trim().to_string()),
            _ => None,
        })
    }))
}

fn spawn(handle: &AgentHandle, parent: &Session, args: &Value) -> Result<Value, String> {
    let (task_name, message, path, mut agents) = reserve(handle, args)?;

    // 从主 agent 的模型可见视图分叉（已折叠的部分以摘要形式带入）。
    let view = parent.context.view(parent.history.items());
    let forked = match args.get("fork_turns").and_then(Value::as_str).unwrap_or("none") {
        "none" | "" => Vec::new(),
        "all" => view,
        n => {
            let n: usize = n.parse().map_err(|_| "fork_turns must be \"none\", \"all\" or a number".to_string())?;
            let users: Vec<usize> = view
                .iter()
                .enumerate()
                .filter(|(_, m)| m.get("role").and_then(Value::as_str) == Some("user"))
                .map(|(i, _)| i)
                .collect();
            match n {
                0 => Vec::new(),
                _ => view[users.len().checked_sub(n).map_or(0, |k| users[k])..].to_vec(),
            }
        }
    };
    let mut history = History::new(forked, None);
    history.close_pending_calls("（已分叉给子 agent，结果交由主 agent）");

    let stage = handle.stage_parent.as_ref().and_then(|(app, parent_id)| {
        Stage::new(app, parent_id, task_name, parent.cwd.to_string_lossy().into_owned(), parent.model.model.id.clone())
    });
    let (child, rx) = new_child(Some(message), stage, None)?;
    let session = Session {
        http: parent.http.clone(),
        model: parent.model.clone(),
        summarizer: parent.summarizer.clone(),
        system_prompt: parent.system_prompt.clone(),
        tools: parent
            .tools
            .iter()
            .filter(|t| !is_agent_tool(t.name))
            .map(|t| Tool { name: t.name, description: t.description.clone(), parameters: t.parameters.clone() })
            .collect(),
        cwd: parent.cwd.clone(),
        session_id: format!("{}{path}", parent.session_id),
        archive_dir: parent.archive_dir.clone(),
        shell: parent.shell.clone(),
        history,
        // ponytail: 子 agent 的折叠账本只在内存里。
        context: ContextWindow::ephemeral(parent.context.window(), parent.context.overhead()),
        cancelled: child.cancelled.clone(),
        steer: child.steer.clone(),
        agents: None,
    };
    agents.insert(path.clone(), child.clone());
    drop(agents);
    tokio::spawn(child_loop(child, session, rx, handle.registry.clone()));
    Ok(json!({ "path": path, "status": "pending_init" }))
}

fn child_loop(
    child: Arc<Child>,
    mut session: Session,
    mut tasks: tokio::sync::mpsc::UnboundedReceiver<(u64, Value)>,
    registry: Arc<Registry>,
) -> BoxFuture<'static, ()> {
    Box::pin(async move {
        while let Some((revision, task)) = tasks.recv().await {
            {
                let sender = child.tasks.lock().unwrap();
                if sender.is_none() { break; }
                if revision != child.task_revision.load(Ordering::SeqCst) {
                    *child.status.lock().unwrap() = ("interrupted".into(), None);
                    registry.changed.notify_waiters();
                    continue;
                }
                session.cancelled.store(false, Ordering::SeqCst);
            }
            *child.status.lock().unwrap() = ("running".into(), None);
            if let Some(stage) = &child.stage {
                stage.begin(&task);
            }
            let emit: Emit = child.stage.as_ref().map(Stage::emitter).unwrap_or_else(|| Arc::new(|_| {}));
            let mut usage = json!({});
            let session_id = session.session_id.clone();
            let mut on_event = turn_event_sink(&emit, &session_id, &mut usage);
            let outcome = run_turn(&mut session, Some(task), &mut on_event).await;
            drop(on_event);
            if let Some(stage) = &child.stage {
                stage.finish(&outcome, usage);
            }
            let last = session
                .history
                .items()
                .iter()
                .rev()
                .find(|m| m.get("role").and_then(Value::as_str) == Some("assistant"))
                .map(|m| text_content(&m["content"]))
                .filter(|t| !t.is_empty());
            *child.status.lock().unwrap() = match (outcome.cancelled, outcome.error) {
                (true, _) => ("interrupted".into(), last),
                (_, Some(error)) => ("errored".into(), Some(error)),
                _ => ("completed".into(), last),
            };
            registry.changed.notify_waiters();
        }
        *child.status.lock().unwrap() = ("shutdown".into(), None);
        if let Some(stage) = &child.stage {
            stage.note("子 Agent 已关闭，执行记录保留供回看。".into());
        }
        registry.changed.notify_waiters();
    })
}

async fn wait(handle: &AgentHandle, cancelled: Option<&AtomicBool>, args: &Value, max_ms: u64) -> Result<Value, String> {
    let targets: Vec<(String, Arc<Child>)> = match args.get("targets").and_then(Value::as_array) {
        Some(list) if !list.is_empty() => list
            .iter()
            .map(|t| handle.resolve(t.as_str().unwrap_or_default()))
            .collect::<Result<_, _>>()?,
        _ => handle
            .registry
            .agents
            .lock()
            .unwrap()
            .iter()
            .map(|(path, child)| (path.clone(), child.clone()))
            .collect(),
    };
    if targets.is_empty() {
        return Err("no agents to wait for".into());
    }
    let timeout = Duration::from_millis(args.get("timeout_ms").and_then(Value::as_u64).unwrap_or(30_000).clamp(1_000, max_ms));
    let deadline = Instant::now() + timeout;
    // 通知可能在检查与等待之间错过，因此同时按 250ms 轮询（也用于响应主 agent 的取消）。
    while targets.iter().all(|(_, child)| child.active())
        && Instant::now() < deadline
        && !cancelled.is_some_and(|c| c.load(Ordering::SeqCst))
    {
        tokio::select! {
            _ = handle.registry.changed.notified() => {}
            _ = tokio::time::sleep(Duration::from_millis(250)) => {}
        }
    }
    let rows: Vec<Value> = targets.iter().map(|(path, child)| child.row(path)).collect();
    let timed_out = targets.iter().all(|(_, child)| child.active());
    Ok(json!({ "agents": rows, "timedOut": timed_out }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lyra::turn::tests::{scripted_server, test_session, text_reply};

    #[tokio::test]
    async fn auto_cancel_stops_schedulers_and_discards_queued_work() {
        let thread_id = "auto-cancel-thread-test";
        let lyra = root_handle(&format!("thread:{thread_id}"));
        let mcp = root_handle(&format!("thread:{thread_id}"));
        let mut children = Vec::new();
        for handle in [&lyra, &mcp] {
            let (child, rx) = new_child(Some("must not run"), None, None).unwrap();
            let path = format!("/root/pending_{}", children.len());
            handle.registry.agents.lock().unwrap().insert(path, child.clone());
            children.push((child, rx, handle.registry.clone()));
        }
        stop_for_thread(thread_id);
        assert!(reserve(&mcp, &json!({"task_name":"late", "message":"must not spawn"})).is_err());
        assert!(control(&mcp, "followup_task", &json!({"target":"pending_0", "message":"must not queue"})).is_err());
        resume_for_thread(thread_id);
        assert!(reserve(&mcp, &json!({"task_name":"new_turn", "message":"allowed"})).is_ok());
        for (child, rx, registry) in children {
            assert!(child.cancelled.load(Ordering::SeqCst));
            assert_eq!(child.task_revision.load(Ordering::SeqCst), 1);
            let url = scripted_server(vec![text_reply("resumed")]).await;
            let mut session = test_session(url, std::env::temp_dir());
            session.cancelled = child.cancelled.clone();
            let worker = tokio::spawn(child_loop(child.clone(), session, rx, registry));
            tokio::time::timeout(Duration::from_secs(1), async {
                while child.status().0 != "interrupted" { tokio::task::yield_now().await; }
            }).await.expect("queued work must be discarded without starting a model");
            child.tasks.lock().unwrap().as_ref().unwrap().send((1, user_message("continue", &[]))).unwrap();
            tokio::time::timeout(Duration::from_secs(5), async {
                while child.status().1.as_deref() != Some("resumed") { tokio::task::yield_now().await; }
            }).await.expect("an explicit followup after stopping must still work");
            child.tasks.lock().unwrap().take();
            worker.await.unwrap();
        }
    }

    #[tokio::test]
    async fn spawned_agent_completes_and_wait_reports_its_message() {
        let url = scripted_server(vec![text_reply("child done"), text_reply("followup done")]).await;
        let mut parent = test_session(url, std::env::temp_dir());
        let handle = root_handle("session-test-root");
        parent.agents = Some(handle.clone());
        parent.history.record(user_message("earlier", &[]));
        let spawned = call(&handle, &parent, "spawn_agent", &json!({ "task_name": "probe", "message": "do it", "fork_turns": "all" })).await;
        assert!(!spawned.is_error, "{:?}", spawned.content);
        let again = call(&handle, &parent, "spawn_agent", &json!({ "task_name": "probe", "message": "x" })).await;
        assert!(again.is_error, "duplicate task names are rejected");
        let waited = call(&handle, &parent, "wait_agent", &json!({ "timeout_ms": 5000 })).await;
        let text = waited.content[0]["text"].as_str().unwrap();
        assert!(text.contains("\"completed\"") && text.contains("child done") && text.contains("/root/probe"), "{text}");
        // 主会话下一轮仍能使用原来的子 Agent；只读展示不创建另一套执行会话。
        let next_handle = root_handle("session-test-root");
        let sent = call(&next_handle, &parent, "send_message", &json!({"target":"probe","message":"extra context"})).await;
        assert!(!sent.is_error);
        let followup = call(&next_handle, &parent, "followup_task", &json!({"target":"probe","message":"continue"})).await;
        assert!(!followup.is_error);
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let (_, child) = next_handle.resolve("probe").unwrap();
                if child.status().1.as_deref() == Some("followup done") {
                    assert!(child.steer.lock().unwrap().is_empty());
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.expect("followup completes on the existing agent");
        let closed = call(&handle, &parent, "close_agent", &json!({ "target": "probe" })).await;
        assert!(!closed.is_error);
        let listed = call(&handle, &parent, "list_agents", &json!({})).await;
        assert_eq!(listed.content[0]["text"], "[]");
    }
}
