//! 多 agent（Codex multi_agents_v2 的精简版）：主 agent 用 spawn_agent 派生进程内子 agent，
//! 用 send_message / followup_task / wait_agent / list_agents / interrupt_agent / close_agent 协作。
//! 子 agent 继承主 agent 的模型、工具与工作目录，独立 rollout/context；重启后仅显式 followup 恢复，深度上限 1。
//! Nova 中额外把任务和执行事件投影到只读 Stage，展示记录落盘，不参与调度。
//! 兼容旧场景 Stage 记录的显式 followup；新场景工作在父会话内临时切模型，不再创建 Stage。

use crate::lyra::context::ContextWindow;
use crate::lyra::history::{text_content, user_message, History};
use crate::lyra::rollout::{valid_task_name, AgentRecord, AgentStore, Rollout};
use std::path::{Path, PathBuf};
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
    // tasks 锁是投递、出队、完成、停止的共同边界；pending 包含正在执行的一项。
    tasks: Mutex<Option<tokio::sync::mpsc::UnboundedSender<Task>>>,
    task_revision: AtomicU64,
    pending: AtomicU64,
    running: AtomicBool,
    completion: AtomicU64,
    delivered: AtomicU64,
    record: Option<AgentRecord>,
    store: Option<Arc<AgentStore>>,
    steer: Arc<Mutex<VecDeque<Value>>>,
    cancelled: Arc<AtomicBool>,
    stage: Option<Stage>,
    /// 场景转交子任务所在的 Nova 会话（失效切换后会换成新会话）；进程内子 agent 为 None。
    scene_thread: Mutex<Option<String>>,
}

struct Task {
    revision: u64,
    input: Value,
    settings: Option<ChildSettings>,
}

/// 只在内存中的当前授权快照，绝不写入 manifest；每次 followup 都重新继承父配置。
struct ChildSettings {
    http: reqwest::Client,
    model: crate::lyra::config::Resolved,
    summarizer: crate::lyra::config::Resolved,
    system_prompt: String,
    tools: Vec<Tool>,
    read_only: bool,
    cwd: PathBuf,
    shell: Option<crate::lyra::prompt::ShellConfig>,
    counter: crate::lyra::tokens::TokenCounter,
    mcp_config: crate::lyra::mcp::McpConfig,
}

impl ChildSettings {
    fn from_parent(parent: &Session, record: Option<&AgentRecord>) -> Result<Self, String> {
        let read_only = parent.read_only || record.is_some_and(|r| r.read_only);
        let tools = parent.tools.iter().filter(|t| {
            !is_agent_tool(t.name) && t.name != crate::lyra::scene::TOOL && record.is_none_or(|r| r.tools.iter().any(|name| name == t.name))
                && (!read_only || matches!(t.name, "read" | "polaris" | "recall" | "context_budget" | "change_working_directory" | "mcp"))
        }).map(|t| Tool { name: t.name, description: t.description.clone(), parameters: t.parameters.clone() }).collect();
        Ok(Self {
            http: parent.http.clone(), model: parent.model.clone(), summarizer: parent.summarizer.clone(),
            system_prompt: parent.system_prompt.clone(), tools, read_only, cwd: parent.cwd.clone(),
            shell: (!read_only).then(|| parent.shell.clone()).flatten(),
            counter: Default::default(),
            mcp_config: parent.mcp_config.clone(),
        })
    }

    fn apply(self, session: &mut Session) -> Result<(), String> {
        let mcp = crate::lyra::mcp::McpSession::new(self.mcp_config.clone(), self.cwd.clone(), self.read_only)?;
        session.mcp_config = self.mcp_config;
        session.mcp = tokio::sync::Mutex::new(mcp);
        session.http = self.http;
        session.model = self.model;
        session.summarizer = self.summarizer;
        session.system_prompt = self.system_prompt;
        session.tools = self.tools;
        session.read_only = self.read_only;
        session.cwd = self.cwd;
        session.shell = self.shell;
        session.context.set_window(session.model.model.context_window);
        configure_context(&mut session.context, self.counter, &session.system_prompt, &session.tools, session.model.model.max_output_tokens);
        Ok(())
    }
}

fn configure_context(context: &mut ContextWindow, counter: crate::lyra::tokens::TokenCounter, system: &str, tools: &[Tool], output: u64) {
    let tools = Value::Array(tools.iter().map(|t| json!({"name":t.name, "description":t.description, "parameters":t.parameters})).collect());
    context.configure_tokens(counter, system, &tools, output);
}

impl Child {
    fn cancel_commands(&self) {
        if let Some(record) = &self.record { crate::lyra::tools::exec::cancel_owner(&record.session_id); }
    }

    // 调用方持有 tasks 锁。shutdown 是终态，迟到的 worker 结果不得将其复活。
    fn set_status(&self, status: &str, message: Option<String>) -> Result<(), String> {
        let mut current = self.status.lock().unwrap();
        if current.0 == "shutdown" { return Ok(()); }
        if let (Some(store), Some(record)) = (&self.store, &self.record) { store.status(&record.session_id, status)?; }
        *current = (status.into(), message);
        Ok(())
    }

    fn finish(&self, revision: u64, status: &str, message: Option<String>) {
        let sender = self.tasks.lock().unwrap();
        self.running.store(false, Ordering::SeqCst);
        if revision == self.task_revision.load(Ordering::SeqCst) {
            let mut remaining = self.pending.load(Ordering::SeqCst).saturating_sub(1);
            // steer 可能恰好到达 run_turn 最后一次检查之后；不能把它留在无人唤醒的 idle 队列。
            if remaining == 0 && !self.cancelled.load(Ordering::SeqCst) {
                if let Some(input) = self.steer.lock().unwrap().pop_front() {
                    if sender.as_ref().is_some_and(|tx| tx.send(Task { revision, input, settings: None }).is_ok()) {
                        remaining = 1;
                    }
                }
            }
            self.pending.store(remaining, Ordering::SeqCst);
        }
        let status = if self.pending.load(Ordering::SeqCst) > 0 { "pending_init" }
            else if revision != self.task_revision.load(Ordering::SeqCst) { "interrupted" } else { status };
        if let Err(error) = self.set_status(status, message) {
            // 不继续执行队列：状态无法可靠落盘时失败关闭，下一次显式恢复再处理。
            self.task_revision.fetch_add(1, Ordering::SeqCst);
            self.pending.store(0, Ordering::SeqCst);
            self.steer.lock().unwrap().clear();
            self.cancel_commands();
            *self.status.lock().unwrap() = ("errored".into(), Some(error));
        }
        if self.pending.load(Ordering::SeqCst) == 0 && self.status().0 != "shutdown" {
            self.completion.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn status(&self) -> (String, Option<String>) {
        self.status.lock().unwrap().clone()
    }

    fn active(&self) -> bool {
        self.pending.load(Ordering::SeqCst) > 0 || self.running.load(Ordering::SeqCst)
    }

    fn row(&self, path: &str) -> Value {
        self.result_row(path, false)
    }

    fn result_row(&self, path: &str, deliver: bool) -> Value {
        let _sender = self.tasks.lock().unwrap();
        if deliver && !self.active() { self.delivered.store(self.completion.load(Ordering::SeqCst), Ordering::SeqCst); }
        let (status, message) = self.status();
        let pending = self.pending.load(Ordering::SeqCst);
        let running = self.running.load(Ordering::SeqCst);
        let current = running && !self.cancelled.load(Ordering::SeqCst);
        let message = message.map(|m| {
            let mut chars = m.chars();
            let mut text: String = chars.by_ref().take(4000).collect();
            if chars.next().is_some() { text.push_str("\n[truncated]"); }
            text
        });
        let mut row = json!({ "path": path, "status": status, "message": message,
            "pending": pending, "running": running, "queued": pending.saturating_sub(u64::from(current)),
            "steering": self.steer.lock().unwrap().len() });
        if let Some(record) = &self.record {
            row["sessionId"] = record.session_id.clone().into();
            row["task_name"] = record.task_name.clone().into();
            row["resumable"] = (status != "shutdown" && self.store.is_some()).into();
        }
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
    store: Mutex<Option<Arc<AgentStore>>>,
}

/// 主 agent 持有的控制面句柄。
#[derive(Clone)]
pub struct AgentHandle {
    registry: Arc<Registry>,
    owner: String,
    stage_parent: Option<(tauri::AppHandle, String)>,
}

static REGISTRIES: OnceLock<Mutex<HashMap<String, Arc<Registry>>>> = OnceLock::new();

/// 停止调度器和已排队的工作；后端的停止命令由公共 cancel_turn 并行发出。
pub(crate) fn stop_for_thread(thread_id: &str) -> Vec<String> {
    root_handle(&format!("thread:{thread_id}")).cancel_all()
}

/// 只有新一轮明确投递的任务才重新开放调度；停止中的旧工具调用不得自行恢复。
pub(crate) fn resume_for_thread(thread_id: &str) {
    root_handle(&format!("thread:{thread_id}")).resume();
}

// ponytail: 注册表按根会话常驻内存；会话很多时需增加 idle registry 淘汰，磁盘子会话可按需恢复。
pub fn root_handle(session_id: &str) -> AgentHandle {
    let registry = REGISTRIES
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .entry(session_id.to_string())
        .or_default()
        .clone();
    AgentHandle { registry, owner: session_id.into(), stage_parent: None }
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

pub fn agent_tools() -> Vec<Tool> {
    let target = json!({ "type": "string", "description": "agent path from spawn_agent/list_agents, or its task_name" });
    let target_and_message = json!({
        "type": "object",
        "properties": { "target": target, "message": { "type": "string" } },
        "required": ["target", "message"]
    });
    let target_only = json!({ "type": "object", "properties": { "target": target }, "required": ["target"] });
    let spawn = Tool {
        name: "spawn_agent",
        description: format!("Start a sub-agent on a self-contained task; it runs in the background with your tools and working directory. Start independent tasks with multiple native spawn_agent calls in the same turn, then continue other independent work; use wait_agent only when its results are needed. At most {MAX_ACTIVE} run at once; sub-agents cannot spawn further agents."),
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
    vec![
        spawn,
        Tool {
            name: "send_message",
            description: "Send input to an agent: steer a running agent, or start a new turn when idle. After restart use followup_task to explicitly resume.".into(),
            parameters: target_and_message.clone(),
        },
        Tool {
            name: "followup_task",
            description: "Give an agent a new task after its current task finishes. Explicitly resumes persisted history/context after restart; never replays old queued work. Closed agents cannot resume.".into(),
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
    /// 用户取消父 turn 的统一入口，适用于 UI 和无 thread 的原生会话。
    pub fn cancel_all(&self) -> Vec<String> {
        let agents = self.registry.agents.lock().unwrap();
        self.registry.stopped.store(true, Ordering::SeqCst);
        let mut scenes = Vec::new();
        for child in agents.values() {
            let _sender = child.tasks.lock().unwrap();
            let active = child.active();
            child.cancelled.store(true, Ordering::SeqCst);
            child.task_revision.fetch_add(1, Ordering::SeqCst);
            child.pending.store(0, Ordering::SeqCst);
            child.steer.lock().unwrap().clear();
            child.cancel_commands();
            if active {
                if let Err(error) = child.set_status("interrupted", None) { eprintln!("lyra: {error}"); }
            }
            if let Some(id) = child.scene_thread.lock().unwrap().clone() { scenes.push(id); }
        }
        self.registry.changed.notify_waiters();
        scenes
    }

    /// 只由新一轮用户输入调用；旧工具、队列恢复不能自行重新开放调度。
    pub fn resume(&self) {
        let _agents = self.registry.agents.lock().unwrap();
        self.registry.stopped.store(false, Ordering::SeqCst);
    }

    pub(super) fn with_stage(mut self, parent: Option<(tauri::AppHandle, String)>) -> Self {
        if let Some((_, thread_id)) = &parent {
            REGISTRIES.get_or_init(Default::default).lock().unwrap()
                .insert(format!("thread:{thread_id}"), self.registry.clone());
        }
        self.stage_parent = parent;
        self
    }

    /// 仅加载元数据；不创建 worker、不打开历史、不投递任务。
    pub fn with_persistence(self, sessions_root: &Path) -> Result<Self, String> {
        let mut agents = self.registry.agents.lock().unwrap();
        let mut slot = self.registry.store.lock().unwrap();
        let root = AgentStore::directory(sessions_root, &self.owner);
        if let Some(store) = slot.as_ref() {
            if store.root != root { return Err("agent persistence root changed".into()); }
        } else {
            let store = Arc::new(AgentStore::open(root, &self.owner)?);
            let records = store.records();
            if records.iter().any(|r| agents.contains_key(&format!("{ROOT}/{}", r.task_name))) {
                return Err("bind persistence before spawning agents".into());
            }
            for record in records {
                let path = format!("{ROOT}/{}", record.task_name);
                let (mut child, _) = new_child(None, None, None)?;
                let child_mut = Arc::get_mut(&mut child).unwrap();
                *child_mut.tasks.get_mut().unwrap() = None;
                *child_mut.status.get_mut().unwrap() = (record.status.clone(), None);
                child_mut.store = Some(store.clone());
                child_mut.record = Some(record);
                agents.insert(path, child);
            }
            *slot = Some(store);
        }
        drop(slot);
        drop(agents);
        Ok(self)
    }

    pub fn has_pending(&self) -> bool {
        self.registry.agents.lock().unwrap().values().any(|child| {
            let _sender = child.tasks.lock().unwrap();
            child.active()
        })
    }

    /// 主 turn 在最终无工具出口调用：正常后台任务继续跑，但父 turn 尚不能 done。
    /// 等待已有工作，或交付最后模型请求期间完成的未读结果一次；不重试失败。
    /// wait_agent 与此处共享交付序号，取消/steer 提前返回不吞结果。
    pub async fn await_pending(&self, cancelled: &AtomicBool, steer: &Mutex<VecDeque<Value>>) -> Option<Value> {
        if !self.has_pending() && !self.registry.agents.lock().unwrap().values().any(|child| {
            let _sender = child.tasks.lock().unwrap();
            child.status().0 != "shutdown" && child.completion.load(Ordering::SeqCst) > child.delivered.load(Ordering::SeqCst)
        }) { return None; }
        while self.has_pending() && !cancelled.load(Ordering::SeqCst) && steer.lock().unwrap().is_empty() {
            tokio::select! {
                _ = self.registry.changed.notified() => {},
                _ = tokio::time::sleep(Duration::from_millis(50)) => {},
            }
        }
        let deliver = !cancelled.load(Ordering::SeqCst) && steer.lock().unwrap().is_empty();
        Some(self.report_results(None, deliver))
    }

    fn resolve(&self, target: &str) -> Result<(String, Arc<Child>), String> {
        let name = target.strip_prefix("/root/").unwrap_or(target);
        if !valid_task_name(name) { return Err("invalid agent target".into()); }
        let path = format!("{ROOT}/{name}");
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
        self.report_results(prefix, false)
    }

    fn report_results(&self, prefix: Option<&str>, deliver: bool) -> Value {
        let agents = self.registry.agents.lock().unwrap();
        let mut rows: Vec<Value> = agents
            .iter()
            .filter(|(path, child)| child.status().0 != "shutdown" && prefix.is_none_or(|p| path.starts_with(p)))
            .map(|(path, child)| child.result_row(path, deliver))
            .collect();
        rows.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
        Value::Array(rows)
    }
}

pub async fn call(handle: &AgentHandle, parent: &Session, name: &str, args: &Value) -> ToolOutcome {
    if handle.owner != parent.session_id { return text_outcome("agent owner must match parent session_id", true); }
    if let Some(root) = &parent.sessions_root {
        if let Err(error) = handle.clone().with_persistence(root) { return text_outcome(error, true); }
    }
    let result = match name {
        "spawn_agent" if args.get("scene").and_then(Value::as_str).is_some_and(|s| !s.is_empty()) => {
            Err("场景不再创建子会话：使用 switch_scene 在本会话临时切模型；没有该工具时请直接完成。".into())
        }
        "spawn_agent" => spawn(handle, parent, args),
        "list_agents" => Ok(handle.report(args.get("path_prefix").and_then(Value::as_str))),
        "wait_agent" => wait(handle, Some(&parent.cancelled), args, 600_000).await,
        _ => control_with_parent(handle, Some(parent), name, args),
    };
    match result {
        Ok(value) => text_outcome(value.to_string(), false),
        Err(error) => text_outcome(error, true),
    }
}


fn control(handle: &AgentHandle, name: &str, args: &Value) -> Result<Value, String> {
    control_with_parent(handle, None, name, args)
}

fn control_with_parent(handle: &AgentHandle, parent: Option<&Session>, name: &str, args: &Value) -> Result<Value, String> {
    let target = args.get("target").and_then(Value::as_str).unwrap_or_default();
    let message = args.get("message").and_then(Value::as_str).unwrap_or_default();
    handle.resolve(target).and_then(|(path, child)| {
        let stop_scene = matches!(name, "interrupt_agent" | "close_agent") && child.active();
        match name {
            "send_message" if child.scene_thread.lock().unwrap().is_some() => {
                return Err("场景子任务独立闭环，不接收中途插话；需要追加工作请用 followup_task".into());
            }
            "send_message" | "followup_task" => {
                let agents = handle.registry.agents.lock().unwrap();
                if handle.registry.stopped.load(Ordering::SeqCst) { return Err("本轮调度已停止，请等待用户继续任务".into()); }
                let mut sender = child.tasks.lock().unwrap();
                if child.status().0 == "shutdown" { return Err(format!("{path} is shut down")); }
                if name == "send_message" && child.running.load(Ordering::SeqCst) {
                    if child.cancelled.load(Ordering::SeqCst) { return Err("agent is stopping; use followup_task".into()); }
                    // 已运行任务不能凭一条 steer 更换授权，收紧模式请先 interrupt 再 followup。
                    if parent.is_some_and(|p| p.read_only) && !child.record.as_ref().is_some_and(|r| r.read_only) {
                        return Err("read-only mode requires interrupt_agent then followup_task".into());
                    }
                    child.steer.lock().unwrap().push_back(user_message(message, &[]));
                } else {
                    if !child.active() && agents.values().filter(|c| c.active()).count() >= MAX_ACTIVE {
                        return Err(format!("{MAX_ACTIVE} agents are already running; wait_agent or close_agent first"));
                    }
                    let ordinary = child.scene_thread.lock().unwrap().is_none();
                    let mut settings = if ordinary {
                        Some(ChildSettings::from_parent(parent.ok_or("ordinary agents require the owning Lyra session")?, child.record.as_ref())?)
                    } else { None };
                    if sender.is_none() {
                        if name != "followup_task" { return Err("use followup_task to explicitly resume this agent".into()); }
                        if child.store.is_none() { return Err("agent has no persisted history to resume".into()); }
                        let parent = parent.ok_or("resume requires the owning Lyra session")?;
                        // 恢复会话直接使用本次授权快照，避免再 apply 一次重建 McpSession。
                        let session = child_session(parent, &child, None, settings.take())?;
                        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
                        *sender = Some(tx);
                        tokio::spawn(child_loop(child.clone(), session, rx, handle.registry.clone()));
                    }
                    let status = if child.running.load(Ordering::SeqCst) { "running" } else { "pending_init" };
                    child.set_status(status, None)?;
                    child.pending.fetch_add(1, Ordering::SeqCst);
                    let task = Task { revision: child.task_revision.load(Ordering::SeqCst), input: user_message(message, &[]), settings };
                    if sender.as_ref().unwrap().send(task).is_err() {
                        child.pending.store(0, Ordering::SeqCst);
                        child.task_revision.fetch_add(1, Ordering::SeqCst);
                        child.steer.lock().unwrap().clear();
                        sender.take();
                        child.set_status("errored", Some("agent worker stopped; use followup_task".into()))?;
                        return Err("agent worker stopped; use followup_task".into());
                    }
                }
                if let Some(stage) = &child.stage { stage.note(format!("主会话已提交后续输入：\n{message}")); }
            }
            "interrupt_agent" | "close_agent" => {
                let _agents = handle.registry.agents.lock().unwrap();
                let mut sender = child.tasks.lock().unwrap();
                child.cancelled.store(true, Ordering::SeqCst);
                child.task_revision.fetch_add(1, Ordering::SeqCst);
                child.pending.store(0, Ordering::SeqCst);
                child.steer.lock().unwrap().clear();
                child.cancel_commands();
                let status = if name == "close_agent" { "shutdown" } else { "interrupted" };
                child.set_status(status, None)?;
                if name == "close_agent" {
                    sender.take();
                    // tombstone 保留到下次显式 spawn，阻止 revive；退出中的 worker 仍占并发名额。
                }
                if let Some(stage) = &child.stage { stage.note(format!("主会话已请求 {status}。")); }
            }
            _ => return Err(format!("unknown agent tool: {name}")),
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
        handle.registry.changed.notify_waiters();
        Ok(child.row(&path))
    })
}

type Agents<'a> = std::sync::MutexGuard<'a, HashMap<String, Arc<Child>>>;

/// 校验 task_name 并占住注册表槽位：返回 (task_name, message, path, 已加锁的注册表)。
fn reserve<'a>(handle: &'a AgentHandle, args: &'a Value) -> Result<(&'a str, &'a str, String, Agents<'a>), String> {
    let task_name = args.get("task_name").and_then(Value::as_str).unwrap_or_default();
    if !valid_task_name(task_name) {
        return Err("task_name must be 1-64 lowercase letters, digits or underscores".into());
    }
    let message = args.get("message").and_then(Value::as_str).unwrap_or_default();
    let path = format!("{ROOT}/{task_name}");
    let mut agents = handle.registry.agents.lock().unwrap();
    if handle.registry.stopped.load(Ordering::SeqCst) { return Err("本轮调度已停止，请等待用户继续任务".into()); }
    if handle.lookup(&mut agents, &path).is_some_and(|c| c.status().0 != "shutdown" || c.active()) {
        return Err(format!("{path} already exists; use followup_task or pick another task_name"));
    }
    if agents.values().filter(|c| c.active()).count() >= MAX_ACTIVE {
        return Err(format!("{MAX_ACTIVE} agents are already running; wait_agent or close_agent first"));
    }
    Ok((task_name, message, path, agents))
}

fn new_child(message: Option<&str>, stage: Option<Stage>, scene_thread: Option<String>) -> Result<(Arc<Child>, tokio::sync::mpsc::UnboundedReceiver<Task>), String> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    if let Some(message) = message {
        tx.send(Task { revision: 0, input: user_message(message, &[]), settings: None }).map_err(|e| e.to_string())?;
    }
    let child = Arc::new(Child {
        status: Mutex::new(("pending_init".into(), None)),
        tasks: Mutex::new(Some(tx)),
        task_revision: AtomicU64::new(0),
        pending: AtomicU64::new(u64::from(message.is_some())),
        running: AtomicBool::new(false),
        completion: AtomicU64::new(0),
        delivered: AtomicU64::new(0),
        record: None,
        store: None,
        steer: Arc::new(Mutex::new(VecDeque::new())),
        cancelled: Arc::new(AtomicBool::new(false)),
        stage,
        scene_thread: Mutex::new(scene_thread),
    });
    Ok((child, rx))
}

async fn scene_loop(
    child: Arc<Child>,
    app: tauri::AppHandle,
    parent_id: String,
    scene: Scene,
    name: String,
    cwd: Option<String>,
    mut tasks: tokio::sync::mpsc::UnboundedReceiver<Task>,
    registry: Arc<Registry>,
) {
    while let Some(Task { revision, input: task, .. }) = tasks.recv().await {
        {
            let sender = child.tasks.lock().unwrap();
            if sender.is_none() { break; }
            if revision != child.task_revision.load(Ordering::SeqCst) { continue; }
            child.cancelled.store(false, Ordering::SeqCst);
            child.running.store(true, Ordering::SeqCst);
            let _ = child.set_status("running", None);
            if let Some(id) = child.scene_thread.lock().unwrap().as_deref() { resume_for_thread(id); }
        }
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
        child.finish(revision, status.0, status.1);
        registry.changed.notify_waiters();
    }
    {
        let _sender = child.tasks.lock().unwrap();
        child.running.store(false, Ordering::SeqCst);
        child.pending.store(0, Ordering::SeqCst);
        let _ = child.set_status("shutdown", None);
    }
    registry.changed.notify_waiters();
}

/// 投递到场景会话并等它跑完，返回本轮新增的最后一条助手回复。
async fn run_scene_turn(app: &tauri::AppHandle, thread_id: &str, text: &str, cancelled: &AtomicBool) -> Result<Option<String>, String> {
    use tauri::Manager;
    let state = app.state::<crate::AppState>();
    let before = state.store.lock().unwrap().get(thread_id).map_or(0, |t| t.items.len());
    if cancelled.load(Ordering::SeqCst) { return Ok(None); }
    // 数字员工的剑来会话是单独的电脑任务：排到桌面（必要时先问用户）才起跑，这一轮结束即让出。
    let _desktop = match crate::employee::acquire_desktop(app, thread_id, cancelled).await {
        Ok(lease) => lease,
        Err(reason) => return Ok(Some(reason)),
    };
    let text = _desktop.as_ref().map_or_else(|| text.to_string(), |lease| format!("{}{text}", lease.note()));
    crate::dispatch_scene_prompt(app, thread_id.to_string(), text, Vec::new())?;
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
    let session_id = uuid::Uuid::new_v4().to_string();
    let record = AgentRecord {
        task_name: task_name.into(), session_id: session_id.clone(), status: "pending_init".into(),
        read_only: parent.read_only,
        tools: parent.tools.iter().filter(|t| !is_agent_tool(t.name)).map(|t| t.name.to_string()).collect(),
    };
    let (mut child, rx) = new_child(Some(message), None, None)?;
    Arc::get_mut(&mut child).unwrap().record = Some(record.clone());
    Arc::get_mut(&mut child).unwrap().store = handle.registry.store.lock().unwrap().clone();
    let session = child_session(parent, &child, Some((session_id.clone(), forked)), None)?;
    if let Some(store) = &child.store { store.insert(record)?; }
    Arc::get_mut(&mut child).unwrap().stage = handle.stage_parent.as_ref().and_then(|(app, parent_id)| {
        Stage::new(app, parent_id, task_name, parent.cwd.to_string_lossy().into_owned(), parent.model.model.id.clone())
    });
    agents.insert(path.clone(), child.clone());
    drop(agents);
    tokio::spawn(child_loop(child, session, rx, handle.registry.clone()));
    Ok(json!({ "path": path, "status": "pending_init", "sessionId": session_id }))
}

/// 只在 spawn 或明确 followup 时读取历史。未完成调用只补 aborted，绝不重新派发。
fn child_session(parent: &Session, child: &Child, fresh: Option<(String, Vec<Value>)>, settings: Option<ChildSettings>) -> Result<Session, String> {
    let resume = fresh.is_none();
    let session_id = fresh.as_ref().map(|(id, _)| id.clone())
        .or_else(|| child.record.as_ref().map(|r| r.session_id.clone())).ok_or("agent has no saved session")?;
    let (mut history, mut context, archive_dir) = if let Some(store) = &child.store {
        if resume && !crate::lyra::rollout::rollout_path(&store.root, &session_id).is_file() {
            return Err("saved agent rollout is missing; refusing to start with empty history".into());
        }
        let (rollout, items) = Rollout::open(&store.root, &session_id, resume, None)?;
        let mut history = History::new(items, Some(rollout));
        if let Some((_, forked)) = fresh { for message in forked { history.record(message); } }
        let context = ContextWindow::load(&store.root, &session_id, parent.context.window(), parent.context.overhead());
        (history, context, Some(store.root.join("tool-results").join(&session_id)))
    } else {
        (History::new(fresh.map(|(_, items)| items).unwrap_or_default(), None),
            ContextWindow::ephemeral(parent.context.window(), parent.context.overhead()),
            parent.archive_dir.as_ref().map(|root| root.join("agents").join(&session_id)))
    };
    history.close_pending_calls(if resume {
        "（子 agent 已中断；旧调用结果未知，不自动重放副作用或排队任务，仅执行本次明确输入）"
    } else { "（已分叉给子 agent，结果交由主 agent）" });
    let settings = match settings { Some(s) => s, None => ChildSettings::from_parent(parent, child.record.as_ref())? };
    configure_context(&mut context, settings.counter, &settings.system_prompt, &settings.tools, settings.model.model.max_output_tokens);
    let mcp_config = settings.mcp_config;
    let mcp = tokio::sync::Mutex::new(crate::lyra::mcp::McpSession::new(mcp_config.clone(), settings.cwd.clone(), settings.read_only)?);
    Ok(Session {
        http: settings.http, model: settings.model, summarizer: settings.summarizer,
        system_prompt: settings.system_prompt, tools: settings.tools, read_only: settings.read_only,
        cwd: settings.cwd, shell: settings.shell, session_id, sessions_root: parent.sessions_root.clone(),
        archive_dir, history, context, mcp_config, mcp,
        cancelled: child.cancelled.clone(), steer: child.steer.clone(), agents: None, scenes: None,
    })
}

fn child_loop(
    child: Arc<Child>,
    mut session: Session,
    mut tasks: tokio::sync::mpsc::UnboundedReceiver<Task>,
    registry: Arc<Registry>,
) -> BoxFuture<'static, ()> {
    Box::pin(async move {
        while let Some(Task { revision, input: task, settings }) = tasks.recv().await {
            {
                let sender = child.tasks.lock().unwrap();
                if sender.is_none() { break; }
                if revision != child.task_revision.load(Ordering::SeqCst) { continue; }
                if let Err(error) = child.set_status("running", None) {
                    child.pending.store(0, Ordering::SeqCst);
                    child.task_revision.fetch_add(1, Ordering::SeqCst);
                    child.cancel_commands();
                    *child.status.lock().unwrap() = ("errored".into(), Some(error));
                    child.completion.fetch_add(1, Ordering::SeqCst);
                    registry.changed.notify_waiters();
                    continue;
                }
                session.cancelled.store(false, Ordering::SeqCst);
                child.running.store(true, Ordering::SeqCst);
            }
            if let Some(settings) = settings {
                if let Err(error) = settings.apply(&mut session) {
                    child.finish(revision, "errored", Some(error));
                    registry.changed.notify_waiters();
                    continue;
                }
            }
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
            let (status, message) = match (outcome.cancelled, outcome.error) {
                (true, _) => ("interrupted", last),
                (_, Some(error)) => ("errored", Some(error)),
                _ => ("completed", last),
            };
            child.finish(revision, status, message);
            registry.changed.notify_waiters();
        }
        {
            let _sender = child.tasks.lock().unwrap();
            child.running.store(false, Ordering::SeqCst);
            child.pending.store(0, Ordering::SeqCst);
            if let Err(error) = child.set_status("shutdown", None) { eprintln!("lyra: {error}"); }
        }
        child.cancel_commands();
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
            .filter(|(_, child)| child.status().0 != "shutdown")
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
    let rows: Vec<Value> = targets.iter().map(|(path, child)| child.result_row(path, !cancelled.is_some_and(|c| c.load(Ordering::SeqCst)))).collect();
    let timed_out = targets.iter().all(|(_, child)| child.active());
    Ok(json!({ "agents": rows, "timedOut": timed_out }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lyra::turn::tests::{capturing_server, scripted_server, test_session, text_reply};

    fn isolated_handle(owner: &str) -> AgentHandle {
        AgentHandle { registry: Arc::new(Registry::default()), owner: owner.into(), stage_parent: None }
    }

    #[tokio::test]
    async fn readonly_child_retains_mcp_only_within_the_recorded_tool_allowlist() {
        let mut parent = test_session("http://unused".into(), std::env::temp_dir());
        parent.read_only = true;
        parent.tools.push(Tool { name: "mcp", description: String::new(), parameters: json!({}) });
        let mut record = AgentRecord { task_name: "probe".into(), session_id: uuid::Uuid::new_v4().to_string(),
            status: "completed".into(), read_only: false, tools: vec!["mcp".into()] };
        let settings = ChildSettings::from_parent(&parent, Some(&record)).unwrap();
        assert!(settings.read_only);
        assert_eq!(settings.tools.iter().map(|t| t.name).collect::<Vec<_>>(), vec!["mcp"]);
        record.tools.clear();
        assert!(ChildSettings::from_parent(&parent, Some(&record)).unwrap().tools.is_empty());
    }

    #[tokio::test]
    async fn rebuild_registry_resumes_history_and_folds_but_not_old_work_or_authority() {
        let root = tempfile::tempdir().unwrap();
        let (url, bodies) = capturing_server(vec![text_reply("resumed safely")]).await;
        let mut parent = test_session(url, root.path().into());
        parent.session_id = "owner-a".into();
        parent.model.model.context_window = 20_000;
        parent.sessions_root = Some(root.path().into());
        parent.context = ContextWindow::ephemeral(20_000, 500);
        parent.tools.extend(crate::lyra::tools::tool_set(false, false, false));
        let handle = isolated_handle(&parent.session_id).with_persistence(root.path()).unwrap();
        let store = handle.registry.store.lock().unwrap().clone().unwrap();
        let record = AgentRecord { task_name: "probe".into(), session_id: uuid::Uuid::new_v4().to_string(),
            status: "running".into(), read_only: false, tools: parent.tools.iter().map(|t| t.name.to_string()).collect() };
        let (mut child, rx) = new_child(Some("old queued task must not run"), None, None).unwrap();
        Arc::get_mut(&mut child).unwrap().store = Some(store.clone());
        Arc::get_mut(&mut child).unwrap().record = Some(record.clone());
        child.steer.lock().unwrap().push_back(user_message("old steer must not run", &[]));
        let mut session = child_session(&parent, &child, Some((record.session_id.clone(), vec![user_message("earlier goal", &[])])), None).unwrap();
        for i in 0..12 {
            session.history.record(json!({"role":"assistant", "content":[{"type":"toolCall", "id":format!("r{i}"), "name":"read", "arguments":{"path":format!("f{i}")}}]}));
            session.history.record(json!({"role":"toolResult", "toolCallId":format!("r{i}"), "toolName":"read", "content":[{"type":"text", "text":"alpha beta gamma delta ".repeat(1000)}]}));
        }
        session.history.record(user_message("previous turn", &[]));
        session.history.record(json!({"role":"assistant", "content":[{"type":"text", "text":"old conclusion"}]}));
        let receipt = session.context.compact(session.history.items(), Some(session.history.items().len() - 2),
            crate::lyra::context::Trigger::Auto, |_, _| async { Ok("## Goal\n- earlier goal\n## Progress\n- inspected files".into()) }).await.unwrap();
        assert_eq!(receipt.status, "applied");
        let generation = session.context.generation();
        let sentinel = root.path().join("must-not-exist");
        session.history.record(json!({"role":"assistant", "content":[{"type":"toolCall", "id":"pending-write", "name":"write", "arguments":{"path":sentinel, "content":"unsafe replay"}}]}));
        store.insert(record.clone()).unwrap();
        drop((session, child, rx, handle, store)); // 模拟进程内 registry/队列全部丢失，不运行旧任务。

        let handle = isolated_handle(&parent.session_id).with_persistence(root.path()).unwrap();
        assert_eq!(handle.report(None)[0]["status"], "interrupted");
        assert_eq!(handle.report(None)[0]["pending"], 0);
        assert!(!handle.has_pending());
        assert!(bodies.lock().unwrap().is_empty(), "list/load must not start a model");
        assert!(handle.await_pending(&parent.cancelled, &parent.steer).await.is_none());
        assert!(isolated_handle("owner-b").with_persistence(root.path()).unwrap().report(None).as_array().unwrap().is_empty());
        assert!(handle.resolve("../probe").is_err());
        assert!(reserve(&handle, &json!({"task_name":"../../escape"})).is_err());

        parent.read_only = true;
        parent.model.model.id = "current-parent-model".into();
        let (_, child) = handle.resolve("probe").unwrap();
        let restored = child_session(&parent, &child, None, None).unwrap();
        assert!(restored.read_only && restored.shell.is_none());
        assert!(!restored.tools.iter().any(|t| t.name == "write" || is_agent_tool(t.name)));
        assert_eq!(restored.model.model.id, "current-parent-model");
        assert_eq!(restored.context.generation(), generation);
        assert!(restored.context.view(restored.history.items()).len() < restored.history.items().len());
        assert!(restored.history.items().iter().any(|m| m["toolCallId"] == "pending-write" && m["isError"] == true));
        assert!(restored.agents.is_none());
        assert_eq!(restored.session_id, record.session_id);
        assert_ne!(restored.archive_dir, parent.archive_dir);
        assert!(restored.archive_dir.as_ref().unwrap().ends_with(&record.session_id));
        let mut readonly_record = record.clone();
        readonly_record.read_only = true;
        parent.read_only = false;
        assert!(ChildSettings::from_parent(&parent, Some(&readonly_record)).unwrap().read_only, "a writable parent cannot upgrade a readonly child");
        parent.read_only = true;
        drop(restored);
        let result = call(&handle, &parent, "followup_task", &json!({"target":"probe", "message":"explicit new task"})).await;
        assert!(!result.is_error, "{:?}", result.content);
        assert!(handle.has_pending());
        let report = tokio::time::timeout(Duration::from_secs(5), handle.await_pending(&parent.cancelled, &parent.steer)).await.unwrap().unwrap();
        assert_eq!(report[0]["message"], "resumed safely");
        assert_eq!(report[0]["pending"], 0);
        assert!(!sentinel.exists());
        let requests = bodies.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].contains("explicit new task") && requests[0].contains("earlier goal"));
        assert!(!requests[0].contains("old queued task") && !requests[0].contains("old steer"));
        drop(requests);
        control(&handle, "close_agent", &json!({"target":"probe"})).unwrap();
        let rebuilt = isolated_handle(&parent.session_id).with_persistence(root.path()).unwrap();
        assert!(!rebuilt.has_pending());
        assert!(rebuilt.report(None).as_array().unwrap().is_empty());
        assert!(control_with_parent(&rebuilt, Some(&parent), "followup_task", &json!({"target":"probe", "message":"do not resurrect"})).is_err());
        let manifest = std::fs::read_to_string(AgentStore::directory(root.path(), &parent.session_id).join("manifest.json")).unwrap();
        assert!(!manifest.contains("api_key") && !manifest.contains("headers") && !manifest.contains("explicit new task"));
    }

    #[tokio::test]
    async fn parent_final_waits_for_child_then_summarizes_its_result_once() {
        let (url, bodies) = capturing_server(vec![text_reply("intermediate parent answer"), text_reply("final summary with child result")]).await;
        let mut parent = test_session(url, std::env::temp_dir());
        let handle = isolated_handle(&parent.session_id);
        parent.agents = Some(handle.clone());
        let (child, _rx) = new_child(Some("pending child work"), None, None).unwrap();
        handle.registry.agents.lock().unwrap().insert("/root/probe".into(), child.clone());
        let seen = Arc::new(AtomicBool::new(false));
        let first_answer = seen.clone();
        let mut worker = tokio::spawn(async move {
            let mut sink = move |event| {
                if matches!(event, crate::lyra::turn::TurnEvent::MessageEnd { .. }) {
                    first_answer.store(true, Ordering::SeqCst);
                }
            };
            let outcome = run_turn(&mut parent, Some(user_message("summarize after the child", &[])), &mut sink).await;
            (outcome, parent)
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            while !seen.load(Ordering::SeqCst) { tokio::task::yield_now().await; }
        }).await.unwrap();
        assert!(tokio::time::timeout(Duration::from_millis(30), &mut worker).await.is_err(), "parent must not finish while child is pending");
        assert_eq!(bodies.lock().unwrap().len(), 1);
        child.finish(0, "completed", Some("child delivered result".into()));
        handle.registry.changed.notify_waiters();
        let (outcome, parent) = tokio::time::timeout(Duration::from_secs(5), worker).await.unwrap().unwrap();
        assert!(!outcome.cancelled && outcome.error.is_none());
        let requests = bodies.lock().unwrap();
        assert_eq!(requests.len(), 2, "delivery causes one summary request, not a loop");
        assert!(requests[1].contains("child delivered result"));
        assert!(parent.history.items().iter().any(|m| text_content(&m["content"]) == "final summary with child result"));
    }

    #[tokio::test]
    async fn queued_work_blocks_parent_completion_and_interrupt_discards_it() {
        let parent = test_session("http://unused".into(), std::env::temp_dir());
        let handle = isolated_handle(&parent.session_id);
        let (child, mut rx) = new_child(None, None, None).unwrap();
        child.set_status("completed", Some("old result".into())).unwrap();
        handle.registry.agents.lock().unwrap().insert("/root/probe".into(), child.clone());
        // idle send_message 必须唤醒；入队当刻已不能被 wait 当成 completed。
        control_with_parent(&handle, Some(&parent), "send_message", &json!({"target":"probe","message":"one"})).unwrap();
        control_with_parent(&handle, Some(&parent), "followup_task", &json!({"target":"probe","message":"two"})).unwrap();
        assert_eq!(child.row("/root/probe")["queued"], 2);
        assert_eq!(child.status().0, "pending_init");
        let first = rx.recv().await.unwrap();
        child.running.store(true, Ordering::SeqCst);
        child.finish(first.revision, "completed", Some("one done".into()));
        assert!(handle.has_pending());
        assert_eq!(child.status().0, "pending_init");
        assert_eq!(child.row("/root/probe")["pending"], 1);
        assert!(tokio::time::timeout(Duration::from_millis(20), handle.await_pending(&parent.cancelled, &parent.steer)).await.is_err());
        // 主模型已给出无工具文本仍等队列，不把正常后台任务取消。
        let waiting = handle.clone();
        let (started, started_rx) = tokio::sync::oneshot::channel();
        let done = tokio::spawn(async move {
            started.send(()).unwrap();
            waiting.await_pending(&AtomicBool::new(false), &Mutex::new(VecDeque::new())).await
        });
        started_rx.await.unwrap();
        assert!(!done.is_finished());
        let second = rx.recv().await.unwrap();
        child.running.store(true, Ordering::SeqCst);
        child.finish(second.revision, "completed", Some("two done".into()));
        handle.registry.changed.notify_waiters();
        let report = done.await.unwrap().unwrap();
        assert_eq!(report[0]["message"], "two done");
        assert!(!handle.has_pending());
        assert!(handle.await_pending(&parent.cancelled, &parent.steer).await.is_none());
        // 子任务在主模型最后一次请求期间已完成，最终出口仍需交付一次。
        child.pending.store(1, Ordering::SeqCst);
        child.finish(0, "completed", Some("fast result".into()));
        assert!(!handle.has_pending());
        assert_eq!(handle.await_pending(&parent.cancelled, &parent.steer).await.unwrap()[0]["message"], "fast result");
        assert!(handle.await_pending(&parent.cancelled, &parent.steer).await.is_none());
        child.pending.store(1, Ordering::SeqCst);
        child.finish(0, "completed", Some("already waited".into()));
        wait(&handle, None, &json!({}), 1000).await.unwrap();
        assert!(handle.await_pending(&parent.cancelled, &parent.steer).await.is_none(), "wait_agent already delivered this result");

        control_with_parent(&handle, Some(&parent), "followup_task", &json!({"target":"probe","message":"discard"})).unwrap();
        child.steer.lock().unwrap().push_back(user_message("discard steer", &[]));
        parent.steer.lock().unwrap().push_back(user_message("parent steer", &[]));
        assert!(handle.await_pending(&parent.cancelled, &parent.steer).await.is_some());
        parent.steer.lock().unwrap().clear();
        parent.cancelled.store(true, Ordering::SeqCst);
        assert!(handle.await_pending(&parent.cancelled, &parent.steer).await.is_some());
        control(&handle, "interrupt_agent", &json!({"target":"probe"})).unwrap();
        assert!(!handle.has_pending());
        assert!(child.steer.lock().unwrap().is_empty());
        assert_ne!(rx.recv().await.unwrap().revision, child.task_revision.load(Ordering::SeqCst));
        assert_eq!(child.status().0, "interrupted");
        // 完成边界刚到的 steer 也不会悬挂在 idle 子会话。
        child.pending.store(1, Ordering::SeqCst);
        child.cancelled.store(false, Ordering::SeqCst);
        child.steer.lock().unwrap().push_back(user_message("late input", &[]));
        child.finish(child.task_revision.load(Ordering::SeqCst), "completed", None);
        assert!(handle.has_pending());
        assert_eq!(text_content(&rx.recv().await.unwrap().input["content"]), "late input");
        control(&handle, "close_agent", &json!({"target":"probe"})).unwrap();
        child.finish(0, "completed", None);
        assert_eq!(child.status().0, "shutdown");
    }

    #[tokio::test]
    async fn resumed_agents_share_the_six_worker_limit_and_closed_workers_keep_their_slot() {
        let parent = test_session("http://unused".into(), std::env::temp_dir());
        let handle = isolated_handle(&parent.session_id);
        let mut receivers = Vec::new();
        for i in 0..MAX_ACTIVE {
            let (child, rx) = new_child(Some("queued"), None, None).unwrap();
            handle.registry.agents.lock().unwrap().insert(format!("/root/a{i}"), child);
            receivers.push(rx);
        }
        let (idle, rx) = new_child(None, None, None).unwrap();
        idle.set_status("completed", None).unwrap();
        handle.registry.agents.lock().unwrap().insert("/root/idle".into(), idle);
        receivers.push(rx);
        assert!(reserve(&handle, &json!({"task_name":"seventh"})).is_err());
        assert!(control_with_parent(&handle, Some(&parent), "followup_task", &json!({"target":"idle","message":"no"})).is_err());
        let (_, stopping) = handle.resolve("a0").unwrap();
        stopping.running.store(true, Ordering::SeqCst);
        control(&handle, "close_agent", &json!({"target":"a0"})).unwrap();
        assert!(reserve(&handle, &json!({"task_name":"a0"})).is_err());
        assert!(reserve(&handle, &json!({"task_name":"seventh"})).is_err());
        stopping.running.store(false, Ordering::SeqCst);
        assert!(reserve(&handle, &json!({"task_name":"a0"})).is_ok());
        handle.cancel_all();
        assert!(reserve(&handle, &json!({"task_name":"new"})).is_err());
        handle.resume();
        assert!(reserve(&handle, &json!({"task_name":"new"})).is_ok());
    }

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
            {
                let sender = child.tasks.lock().unwrap();
                child.pending.store(1, Ordering::SeqCst);
                child.set_status("pending_init", None).unwrap();
                sender.as_ref().unwrap().send(Task { revision: 1, input: user_message("continue", &[]), settings: None }).unwrap();
            }
            tokio::time::timeout(Duration::from_secs(5), async {
                while child.status().1.as_deref() != Some("resumed") { tokio::task::yield_now().await; }
            }).await.expect("an explicit followup after stopping must still work");
            child.tasks.lock().unwrap().take();
            worker.await.unwrap();
        }
    }

    #[tokio::test]
    async fn spawned_agent_completes_and_wait_reports_its_message() {
        let url = scripted_server(vec![text_reply("child done"), text_reply("idle message done"), text_reply("followup done")]).await;
        let mut parent = test_session(url, std::env::temp_dir());
        let handle = root_handle("session-test-root");
        parent.session_id = "session-test-root".into();
        parent.agents = Some(handle.clone());
        parent.history.record(user_message("earlier", &[]));
        let spawned = call(&handle, &parent, "spawn_agent", &json!({ "task_name": "probe", "message": "do it", "fork_turns": "all" })).await;
        assert!(!spawned.is_error, "{:?}", spawned.content);
        let again = call(&handle, &parent, "spawn_agent", &json!({ "task_name": "probe", "message": "x" })).await;
        assert!(again.is_error, "duplicate task names are rejected");
        let waited = call(&handle, &parent, "wait_agent", &json!({ "timeout_ms": 5000 })).await;
        let text = waited.content[0]["text"].as_str().unwrap();
        assert!(text.contains("\"completed\"") && text.contains("child done") && text.contains("/root/probe"), "{text}");
        assert!(handle.await_pending(&parent.cancelled, &parent.steer).await.is_none());
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
