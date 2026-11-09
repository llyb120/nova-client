//! 主子 agent 调度（Codex multi_agents_v2 的精简版）：主 agent 用 spawn_agent 派生进程内
//! 子 agent，用 send_message / followup_task / wait_agent / list_agents / interrupt_agent /
//! close_agent 协作。子 agent 继承主 agent 的模型、工具与工作目录，独立历史与上下文，
//! 深度上限 1，只在当前进程内存在；任务与事件额外投影到只读 Stage（界面里叫「分身」）。
//!
//! 上下文与缓存：`spawn_agent`/`followup_task` 的 `context` 只共享父会话可见历史的**前缀**
//! （none/prefix/all/turn:<n>）；子会话的 system prompt、工具定义与父会话完全相同，并用
//! `cache_key` 锁父会话的 `prompt_cache_key`，所以继承的前缀能命中父会话已写入的前缀缓存。
//! 子会话自身仍是独立 `session_id`，Responses 续接与后台命令作用域不会互相清。

use crate::lyra::bridge::{turn_event_sink, Emit};
use crate::lyra::context::ContextWindow;
use crate::lyra::history::{text_content, user_message, History};
use crate::lyra::stage::Stage;
use crate::lyra::tools::{Tool, ToolOutcome};
use crate::lyra::turn::{run_turn, text_outcome, BoxFuture, Session};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

const MAX_ACTIVE: usize = 6;
const ROOT: &str = "/root";

/// 模型 id 命中任一条规则才带上分身工具；一条规则里的多个词（空格分隔）必须同时命中。
/// 每个词支持 `*` 通配（任意字符，含空），例：`gpt-5* *codex`、`deepseek-*-pro`。
/// 规则由用户在设置页「Lyra → 分身」里填，默认空 = 全不启用。
pub fn enabled(model_id: &str, keywords: &[String]) -> bool {
    let id = model_id.to_ascii_lowercase();
    keywords.iter().any(|rule| {
        let terms: Vec<&str> = rule.split_whitespace().collect();
        !terms.is_empty() && terms.iter().all(|term| glob_match(&term.to_ascii_lowercase(), &id))
    })
}

/// 只认 `*` 的 glob 匹配：首段必须前缀、末段必须后缀，中间段按顺序出现。
fn glob_match(pattern: &str, text: &str) -> bool {
    if !pattern.contains('*') {
        return pattern == text;
    }
    let parts: Vec<&str> = pattern.split('*').collect();
    let mut rest = text;
    if let Some(head) = parts.first().filter(|head| !head.is_empty()) {
        match rest.strip_prefix(head) {
            Some(remainder) => rest = remainder,
            None => return false,
        }
    }
    let Some((tail, middle)) = parts.split_last() else { return false };
    let Some(mut body) = rest.strip_suffix(tail) else { return false };
    for part in middle.iter().skip(1) {
        if part.is_empty() {
            continue;
        }
        match body.find(part) {
            Some(position) => body = &body[position + part.len()..],
            None => return false,
        }
    }
    true
}

fn valid_task_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 64
        && name.chars().all(|character| character.is_ascii_lowercase() || character.is_ascii_digit() || character == '_')
}

struct Child {
    name: String,
    /// 独立 identity：日志、归档子目录与 provider 会话隔离用，不落盘。
    session_id: String,
    /// pending_init / running / completed / errored / interrupted / shutdown
    status: Mutex<(String, Option<String>)>,
    // tasks 锁是投递、出队、完成、停止的共同边界；pending 包含正在执行的一项。
    tasks: Mutex<Option<tokio::sync::mpsc::UnboundedSender<Task>>>,
    task_revision: AtomicU64,
    pending: AtomicU64,
    running: AtomicBool,
    completion: AtomicU64,
    delivered: AtomicU64,
    steer: Arc<Mutex<VecDeque<Value>>>,
    cancelled: Arc<AtomicBool>,
    stage: Option<Stage>,
}

struct Task {
    revision: u64,
    input: Value,
}

/// 每次 spawn / 显式 followup 时重新继承的父授权快照；只在内存里，不写入任何存档。
struct ChildSettings {
    http: Option<reqwest::Client>,
    model: crate::lyra::config::Resolved,
    summarizer: crate::lyra::config::Resolved,
    system_prompt: String,
    dynamic_prompt: String,
    tools: Vec<Tool>,
    read_only: bool,
    cwd: std::path::PathBuf,
    shell: Option<crate::lyra::prompt::ShellConfig>,
    mcp_config: crate::lyra::mcp::McpConfig,
    cache_key: Option<String>,
}

impl ChildSettings {
    /// 子 agent 的 system prompt、工具与父会话**完全相同**（护住前缀缓存），
    /// 会话身份独立、缓存键锁父的。
    fn from_parent(parent: &Session) -> Self {
        Self {
            http: parent.http.clone(),
            model: parent.model.clone(),
            summarizer: parent.summarizer.clone(),
            system_prompt: parent.system_prompt.clone(),
            dynamic_prompt: parent.dynamic_prompt.clone(),
            tools: parent.tools.iter()
                .map(|tool| Tool { name: tool.name, description: tool.description.clone(), parameters: tool.parameters.clone() })
                .collect(),
            read_only: parent.read_only,
            cwd: parent.cwd.clone(),
            shell: parent.shell.clone(),
            mcp_config: parent.mcp_config.clone(),
            cache_key: Some(parent.cache_key.clone().unwrap_or_else(|| parent.session_id.clone())),
        }
    }

    fn apply(self, session: &mut Session) -> Result<(), String> {
        let mcp = crate::lyra::mcp::McpSession::new(self.mcp_config.clone(), self.cwd.clone(), self.read_only)?;
        session.mcp_config = self.mcp_config;
        session.mcp = tokio::sync::Mutex::new(mcp);
        session.http = self.http;
        session.model = self.model;
        session.summarizer = self.summarizer;
        session.system_prompt = self.system_prompt;
        session.dynamic_prompt = self.dynamic_prompt;
        session.tools = self.tools;
        session.read_only = self.read_only;
        session.cwd = self.cwd;
        session.shell = self.shell;
        session.cache_key = self.cache_key;
        session.context.set_window(session.model.model.context_window);
        configure_context(&mut session.context, &session.system_prompt, &session.tools, session.model.model.max_output_tokens);
        Ok(())
    }
}

fn configure_context(context: &mut ContextWindow, system: &str, tools: &[Tool], output: u64) {
    let schema = Value::Array(tools.iter().map(|tool| json!({
        "name": tool.name, "description": tool.description, "parameters": tool.parameters
    })).collect());
    context.configure_tokens(Default::default(), system, &schema, output);
}

impl Child {
    /// shutdown 是终态，迟到的 worker 结果不得将其复活。
    fn set_status(&self, status: &str, message: Option<String>) {
        let mut current = self.status.lock().unwrap();
        if current.0 != "shutdown" {
            *current = (status.into(), message);
        }
    }

    fn status(&self) -> (String, Option<String>) {
        self.status.lock().unwrap().clone()
    }

    /// 取消当前任务并丢弃排队工作，保留 agent 本身供 followup_task 复用。
    fn stop_work(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.task_revision.fetch_add(1, Ordering::SeqCst);
        self.pending.store(0, Ordering::SeqCst);
        self.steer.lock().unwrap().clear();
    }

    /// 调用方持有 tasks 锁：完成边界与投递竞争同一条队列。
    fn finish(&self, revision: u64, status: &str, message: Option<String>) {
        let sender = self.tasks.lock().unwrap();
        self.running.store(false, Ordering::SeqCst);
        if revision == self.task_revision.load(Ordering::SeqCst) {
            let mut remaining = self.pending.load(Ordering::SeqCst).saturating_sub(1);
            // steer 可能恰好到达 run_turn 最后一次检查之后；不能把它留在无人唤醒的 idle 队列。
            if remaining == 0 && !self.cancelled.load(Ordering::SeqCst) {
                if let Some(input) = self.steer.lock().unwrap().pop_front() {
                    if sender.as_ref().is_some_and(|tx| tx.send(Task { revision, input }).is_ok()) {
                        remaining = 1;
                    }
                }
            }
            self.pending.store(remaining, Ordering::SeqCst);
        }
        let status = if self.pending.load(Ordering::SeqCst) > 0 { "pending_init" }
            else if revision != self.task_revision.load(Ordering::SeqCst) { "interrupted" } else { status };
        self.set_status(status, message);
        if self.pending.load(Ordering::SeqCst) == 0 && self.status().0 != "shutdown" {
            self.completion.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn active(&self) -> bool {
        self.pending.load(Ordering::SeqCst) > 0 || self.running.load(Ordering::SeqCst)
    }

    fn row(&self, path: &str) -> Value {
        self.result_row(path, false)
    }

    fn result_row(&self, path: &str, deliver: bool) -> Value {
        let _sender = self.tasks.lock().unwrap();
        if deliver && !self.active() {
            self.delivered.store(self.completion.load(Ordering::SeqCst), Ordering::SeqCst);
        }
        let (status, message) = self.status();
        let pending = self.pending.load(Ordering::SeqCst);
        let current = self.running.load(Ordering::SeqCst) && !self.cancelled.load(Ordering::SeqCst);
        let message = message.map(|message| {
            let mut chars = message.chars();
            let mut text: String = chars.by_ref().take(4000).collect();
            if chars.next().is_some() {
                text.push_str("\n[truncated]");
            }
            text
        });
        json!({ "path": path, "task_name": self.name, "sessionId": self.session_id, "status": status,
            "message": message, "pending": pending, "running": self.running.load(Ordering::SeqCst),
            "queued": pending.saturating_sub(u64::from(current)),
            "steering": self.steer.lock().unwrap().len() })
    }
}

#[derive(Default)]
struct Registry {
    agents: Mutex<HashMap<String, Arc<Child>>>,
    stopped: AtomicBool,
    changed: tokio::sync::Notify,
}

/// 主 agent 持有的控制面句柄（每个 Nova 会话一份）。
#[derive(Clone)]
pub struct AgentHandle {
    registry: Arc<Registry>,
    stage_parent: Option<(tauri::AppHandle, String)>,
}

static REGISTRIES: OnceLock<Mutex<HashMap<String, Arc<Registry>>>> = OnceLock::new();

/// 停止调度器和已排队的工作；后端 bridge 的停止由公共 cancel_turn 并行发出。
pub(crate) fn stop_for_thread(thread_id: &str) {
    root_handle(&format!("thread:{thread_id}")).cancel_all()
}

/// 只有新一轮明确投递的任务才重新开放调度；停止中的旧工具调用不得自行恢复。
pub(crate) fn resume_for_thread(thread_id: &str) {
    root_handle(&format!("thread:{thread_id}")).resume()
}

// ponytail: 注册表按根会话常驻内存；会话很多时需增加 idle registry 淘汰。
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

pub fn agent_tools() -> Vec<Tool> {
    let target = json!({ "type": "string", "description": "agent path from spawn_agent/list_agents, or its task_name" });
    let target_and_message = json!({
        "type": "object",
        "properties": { "target": target, "message": { "type": "string" } },
        "required": ["target", "message"]
    });
    let target_only = json!({ "type": "object", "properties": { "target": target }, "required": ["target"] });
    let context = json!({
        "type": "string",
        "description": "Optional 1-based user-turn number to share as a leading prefix, e.g. \"turn:3\" or \"3\": the sub-agent sees everything from the very start through the end of that turn, which reuses your prompt cache. Omit (or \"none\") for a fresh session; an out-of-range value shares the whole visible history."
    });
    vec![
        Tool {
            name: "spawn_agent",
            description: format!("Start a sub-agent on a self-contained task; it runs in the background with your tools and working directory. Start independent tasks with multiple native spawn_agent calls in the same turn, then continue other independent work; use wait_agent only when its results are needed. At most {MAX_ACTIVE} run at once; sub-agents cannot spawn further agents."),
            parameters: json!({
                "type": "object",
                "properties": {
                    "task_name": { "type": "string", "pattern": "^[a-z0-9_]+$", "description": "short unique name, lowercase letters, digits, underscores" },
                    "message": { "type": "string", "description": "the full task; the agent sees this plus the shared history prefix" },
                    "context": context
                },
                "required": ["task_name", "message"]
            }),
        },
        Tool {
            name: "send_message",
            description: "Send input to an agent: steer a running agent, or start a new turn when idle.".into(),
            parameters: target_and_message.clone(),
        },
        Tool {
            name: "followup_task",
            description: "Give an idle agent a new task after its current task finishes; queued work does not count as done.".into(),
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
    pub fn cancel_all(&self) {
        let agents = self.registry.agents.lock().unwrap();
        self.registry.stopped.store(true, Ordering::SeqCst);
        for child in agents.values() {
            let _sender = child.tasks.lock().unwrap();
            let active = child.active();
            child.stop_work();
            if active {
                child.set_status("interrupted", None);
            }
        }
        self.registry.changed.notify_waiters();
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
        }) {
            return None;
        }
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
        if !valid_task_name(name) {
            return Err("invalid agent target".into());
        }
        let path = format!("{ROOT}/{name}");
        let mut agents = self.registry.agents.lock().unwrap();
        self.lookup(&mut agents, &path)
            .map(|child| (path, child))
            .ok_or_else(|| format!("not_found: no agent {target}"))
    }

    /// 注册表优先；进程内子 agent 没有持久化，因此不会复活已关闭的实例。
    fn lookup(&self, agents: &mut HashMap<String, Arc<Child>>, path: &str) -> Option<Arc<Child>> {
        agents.get(path).cloned()
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

pub async fn call(parent: &Session, name: &str, args: &Value) -> ToolOutcome {
    // 桌面归属键是 thread:<id>，不同于模型 session_id；只使用父会话持有的句柄。
    let Some(handle) = parent.agents.clone() else {
        return text_outcome(format!("{name} 仅在主会话中可用；分身不能再派生分身"), true);
    };
    let result = match name {
        "spawn_agent" => spawn(&handle, parent, args),
        "list_agents" => Ok(handle.report(args.get("path_prefix").and_then(Value::as_str))),
        "wait_agent" => wait(&handle, Some(&parent.cancelled), args, 600_000).await,
        _ => control(&handle, name, args),
    };
    match result {
        Ok(value) => text_outcome(value.to_string(), false),
        Err(error) => text_outcome(error, true),
    }
}

fn control(handle: &AgentHandle, name: &str, args: &Value) -> Result<Value, String> {
    let target = args.get("target").and_then(Value::as_str).unwrap_or_default();
    let message = args.get("message").and_then(Value::as_str).unwrap_or_default();
    handle.resolve(target).and_then(|(path, child)| {
        match name {
            "send_message" | "followup_task" => {
                let agents = handle.registry.agents.lock().unwrap();
                if handle.registry.stopped.load(Ordering::SeqCst) {
                    return Err("本轮调度已停止，请等待用户继续任务".into());
                }
                let mut sender = child.tasks.lock().unwrap();
                let Some(tx) = sender.as_ref() else { return Err(format!("{path} is shut down")) };
                if name == "send_message" && child.running.load(Ordering::SeqCst) {
                    if child.cancelled.load(Ordering::SeqCst) {
                        return Err("agent is stopping; use followup_task".into());
                    }
                    child.steer.lock().unwrap().push_back(user_message(message, &[]));
                } else {
                    if !child.active() && agents.values().filter(|candidate| candidate.active()).count() >= MAX_ACTIVE {
                        return Err(format!("{MAX_ACTIVE} agents are already running; wait_agent or close_agent first"));
                    }
                    let status = if child.running.load(Ordering::SeqCst) { "running" } else { "pending_init" };
                    child.set_status(status, None);
                    child.pending.fetch_add(1, Ordering::SeqCst);
                    let task = Task { revision: child.task_revision.load(Ordering::SeqCst), input: user_message(message, &[]) };
                    if tx.send(task).is_err() {
                        child.stop_work();
                        sender.take();
                        child.set_status("errored", Some("agent worker stopped; spawn a new agent".into()));
                        return Err("agent worker stopped; spawn a new agent".into());
                    }
                }
                drop(sender);
                drop(agents);
                if let Some(stage) = &child.stage {
                    stage.note(format!("主会话已提交后续输入：\n{message}"));
                }
            }
            "interrupt_agent" | "close_agent" => {
                let _agents = handle.registry.agents.lock().unwrap();
                let mut sender = child.tasks.lock().unwrap();
                child.stop_work();
                let status = if name == "close_agent" { "shutdown" } else { "interrupted" };
                child.set_status(status, None);
                if name == "close_agent" {
                    // 关闭是终态：投递口一并撤掉，退出中的 worker 仍占并发名额。
                    sender.take();
                }
                drop(sender);
                if let Some(stage) = &child.stage {
                    stage.note(format!("主会话已请求 {status}。"));
                }
            }
            _ => return Err(format!("unknown agent tool: {name}")),
        }
        handle.registry.changed.notify_waiters();
        Ok(child.row(&path))
    })
}

type Agents<'a> = MutexGuard<'a, HashMap<String, Arc<Child>>>;

/// 校验 task_name 并占住注册表槽位：返回 (task_name, message, path, 已加锁的注册表)。
fn reserve<'a>(handle: &'a AgentHandle, args: &'a Value) -> Result<(&'a str, &'a str, String, Agents<'a>), String> {
    let task_name = args.get("task_name").and_then(Value::as_str).unwrap_or_default();
    if !valid_task_name(task_name) {
        return Err("task_name must be 1-64 lowercase letters, digits or underscores".into());
    }
    let message = args.get("message").and_then(Value::as_str).unwrap_or_default();
    if message.trim().is_empty() {
        return Err("message must describe the whole task".into());
    }
    let path = format!("{ROOT}/{task_name}");
    let agents = handle.registry.agents.lock().unwrap();
    if handle.registry.stopped.load(Ordering::SeqCst) {
        return Err("本轮调度已停止，请等待用户继续任务".into());
    }
    if agents.get(&path).is_some_and(|child| child.status().0 != "shutdown" || child.active()) {
        return Err(format!("{path} already exists; use followup_task or pick another task_name"));
    }
    if agents.values().filter(|child| child.active()).count() >= MAX_ACTIVE {
        return Err(format!("{MAX_ACTIVE} agents are already running; wait_agent or close_agent first"));
    }
    Ok((task_name, message, path, agents))
}

fn new_child(message: Option<&str>, name: &str, stage: Option<Stage>) -> (Arc<Child>, tokio::sync::mpsc::UnboundedReceiver<Task>) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    if let Some(message) = message {
        let _ = tx.send(Task { revision: 0, input: user_message(message, &[]) });
    }
    let child = Arc::new(Child {
        name: name.into(),
        session_id: uuid::Uuid::new_v4().to_string(),
        status: Mutex::new(("pending_init".into(), None)),
        tasks: Mutex::new(Some(tx)),
        task_revision: AtomicU64::new(0),
        pending: AtomicU64::new(u64::from(message.is_some())),
        running: AtomicBool::new(false),
        completion: AtomicU64::new(0),
        delivered: AtomicU64::new(0),
        steer: Arc::new(Mutex::new(VecDeque::new())),
        cancelled: Arc::new(AtomicBool::new(false)),
        stage,
    });
    (child, rx)
}

fn spawn(handle: &AgentHandle, parent: &Session, args: &Value) -> Result<Value, String> {
    let (task_name, message, path, mut agents) = reserve(handle, args)?;
    let context = args.get("context").and_then(Value::as_str).unwrap_or("");
    let view = parent.context.view(parent.history.items());
    let users = user_turns(&view);
    let forked = inherit_view(&view, &users, context);
    let stage = handle.stage_parent.as_ref().and_then(|(app, parent_id)| {
        Stage::new(app, parent_id, task_name, parent.cwd.to_string_lossy().into_owned(), parent.model.model.id.clone())
    });
    let (child, rx) = new_child(Some(message), task_name, stage);
    let session = child_session(parent, &child, forked)?;
    agents.insert(path.clone(), child.clone());
    drop(agents);
    let session_id = child.session_id.clone();
    tokio::spawn(child_loop(child, session, rx, handle.registry.clone()));
    Ok(json!({ "path": path, "status": "pending_init", "sessionId": session_id }))
}

/// 子 agent 只继承当前授权快照；system prompt 与工具与父会话完全一致（护缓存），
/// 历史只可能是父会话可见视图的前缀。
fn child_session(parent: &Session, child: &Child, forked: Vec<Value>) -> Result<Session, String> {
    let settings = ChildSettings::from_parent(parent);
    let mut history = History::new(forked, None);
    history.close_pending_calls("（已分叉给分身，结果交由主会话）");
    let mut context = ContextWindow::ephemeral(parent.context.window(), parent.context.overhead());
    configure_context(&mut context, &settings.system_prompt, &settings.tools, settings.model.model.max_output_tokens);
    let mcp = tokio::sync::Mutex::new(crate::lyra::mcp::McpSession::new(
        settings.mcp_config.clone(), settings.cwd.clone(), settings.read_only)?);
    Ok(Session {
        http: settings.http,
        model: settings.model,
        summarizer: settings.summarizer,
        system_prompt: settings.system_prompt,
        dynamic_prompt: settings.dynamic_prompt,
        tools: settings.tools,
        read_only: settings.read_only,
        mcp_config: settings.mcp_config,
        mcp,
        cwd: settings.cwd,
        session_id: child.session_id.clone(),
        cache_key: settings.cache_key,
        sessions_root: None,
        archive_dir: parent.archive_dir.as_ref().map(|root| root.join("agents").join(&child.session_id)),
        shell: settings.shell,
        history,
        context,
        cancelled: child.cancelled.clone(),
        steer: child.steer.clone(),
        agents: None,
        scenes: None,
    })
}

fn child_loop(
    child: Arc<Child>,
    mut session: Session,
    mut tasks: tokio::sync::mpsc::UnboundedReceiver<Task>,
    registry: Arc<Registry>,
) -> BoxFuture<'static, ()> {
    Box::pin(async move {
        while let Some(Task { revision, input: task }) = tasks.recv().await {
            {
                let sender = child.tasks.lock().unwrap();
                if sender.is_none() { break; }
                if revision != child.task_revision.load(Ordering::SeqCst) { continue; }
                child.set_status("running", None);
                session.cancelled.store(false, Ordering::SeqCst);
                child.running.store(true, Ordering::SeqCst);
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
            let last = session.history.items().iter().rev()
                .find(|message| message.get("role").and_then(Value::as_str) == Some("assistant"))
                .map(|message| text_content(&message["content"]))
                .filter(|text| !text.is_empty());
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
            child.set_status("shutdown", None);
        }
        if let Some(stage) = &child.stage {
            stage.note("分身已关闭，执行记录保留供回看。".into());
        }
        registry.changed.notify_waiters();
    })
}

async fn wait(handle: &AgentHandle, cancelled: Option<&AtomicBool>, args: &Value, max_ms: u64) -> Result<Value, String> {
    let targets: Vec<(String, Arc<Child>)> = match args.get("targets").and_then(Value::as_array) {
        Some(list) if !list.is_empty() => list
            .iter()
            .map(|target| handle.resolve(target.as_str().unwrap_or_default()))
            .collect::<Result<_, _>>()?,
        _ => handle.registry.agents.lock().unwrap().iter()
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
        && !cancelled.is_some_and(|cancel| cancel.load(Ordering::SeqCst))
    {
        tokio::select! {
            _ = handle.registry.changed.notified() => {}
            _ = tokio::time::sleep(Duration::from_millis(250)) => {}
        }
    }
    let rows: Vec<Value> = targets.iter()
        .map(|(path, child)| child.result_row(path, !cancelled.is_some_and(|cancel| cancel.load(Ordering::SeqCst))))
        .collect();
    let timed_out = targets.iter().all(|(_, child)| child.active());
    Ok(json!({ "agents": rows, "timedOut": timed_out }))
}

/// 父会话可见视图里每个用户轮次的起点。
fn user_turns(view: &[Value]) -> Vec<usize> {
    view.iter().enumerate()
        .filter(|(_, message)| message.get("role").and_then(Value::as_str) == Some("user"))
        .map(|(index, _)| index)
        .collect()
}

/// 把父会话可见历史按 `context` 切成一个**前缀**：只从最开始取到某个用户轮次结束，永不取中间段。
/// 不传或 `none` = 全新会话；其余值按 1 起的轮次号解析，**越界或解析失败视为全部历史**。
fn inherit_view(view: &[Value], users: &[usize], mode: &str) -> Vec<Value> {
    let mode = mode.trim();
    if mode.is_empty() || mode.eq_ignore_ascii_case("none") {
        return Vec::new();
    }
    let number = mode
        .strip_prefix("turn:")
        .or_else(|| mode.strip_prefix("turn"))
        .unwrap_or(mode)
        .trim_start_matches('t');
    match number.parse::<usize>() {
        Ok(turn) if turn > 0 && turn <= users.len() => {
            let end = users.get(turn).copied().unwrap_or(view.len());
            view[..end].to_vec()
        }
        _ => view.to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lyra::turn::tests::{capturing_server, scripted_server, test_session, text_reply};

    fn isolated_handle() -> AgentHandle {
        AgentHandle { registry: Arc::new(Registry::default()), stage_parent: None }
    }

    #[test]
    fn only_configured_rules_open_subagents() {
        let keywords = vec!["*gpt-5*".to_string(), "*claude-opus*".to_string(), "deepseek-*-pro".to_string()];
        assert!(enabled("gpt-5.1-codex", &keywords));
        assert!(enabled("claude-opus-5", &keywords));
        assert!(enabled("deepseek-v4-pro", &keywords));
        assert!(!enabled("deepseek-v4-flash", &keywords));
        assert!(!enabled("gemini-3-pro", &keywords));
        assert!(enabled("GPT-5", &[" gpt-5 ".into()]));
        assert!(enabled("gpt-5.1-codex", &["gpt-5* *codex".into()]));
        assert!(!enabled("gpt-5.1", &["gpt-5* *codex".into()]));
        assert!(!enabled("gpt-5", &["  ".into()]));
        assert!(!enabled("gpt-5", &[]));
    }

    #[test]
    fn inherited_context_is_always_a_leading_prefix() {
        let view: Vec<Value> = vec![
            json!({"role":"user","content":[{"type":"text","text":"one"}]}),
            json!({"role":"assistant","content":[{"type":"text","text":"a1"}]}),
            json!({"role":"user","content":[{"type":"text","text":"two"}]}),
            json!({"role":"assistant","content":[{"type":"text","text":"a2"}]}),
            json!({"role":"user","content":[{"type":"text","text":"three"}]}),
        ];
        let users = vec![0usize, 2, 4];
        assert!(inherit_view(&view, &users, "").is_empty());
        assert!(inherit_view(&view, &users, "none").is_empty());
        assert_eq!(inherit_view(&view, &users, "turn:1").len(), 2);
        assert_eq!(inherit_view(&view, &users, "2").len(), 4);
        assert_eq!(inherit_view(&view, &users, "t3").len(), 5);
        // 越界或解析失败一律视为全部历史，不报错。
        assert_eq!(inherit_view(&view, &users, "turn:9").len(), 5);
        assert_eq!(inherit_view(&view, &users, "0").len(), 5);
        assert_eq!(inherit_view(&view, &users, "prefix").len(), 5);
    }

    #[tokio::test]
    async fn spawned_agent_completes_and_wait_reports_its_message() {
        let url = scripted_server(vec![text_reply("child done")]).await;
        let mut parent = test_session(url, std::env::temp_dir());
        parent.agents = Some(isolated_handle());
        let spawned = call(&parent, "spawn_agent", &json!({ "task_name": "probe", "message": "do it" })).await;
        assert!(!spawned.is_error, "{:?}", spawned.content);
        let waited = call(&parent, "wait_agent", &json!({ "timeout_ms": 5000 })).await;
        let text = waited.content[0]["text"].as_str().unwrap();
        assert!(text.contains("\"completed\"") && text.contains("child done") && text.contains("/root/probe"), "{text}");
        assert!(!call(&parent, "close_agent", &json!({ "target": "probe" })).await.is_error);
        assert_eq!(call(&parent, "list_agents", &json!({})).await.content[0]["text"], "[]");
    }

    #[tokio::test]
    async fn parent_final_waits_for_child_then_summarizes_its_result_once() {
        let (url, bodies) = capturing_server(vec![text_reply("intermediate parent answer"), text_reply("final summary with child result")]).await;
        let mut parent = test_session(url, std::env::temp_dir());
        let handle = isolated_handle();
        parent.agents = Some(handle.clone());
        let (child, _rx) = new_child(Some("pending child work"), "probe", None);
        handle.registry.agents.lock().unwrap().insert("/root/probe".into(), child.clone());
        child.finish(0, "completed", Some("child delivered result".into()));
        handle.registry.changed.notify_waiters();
        let outcome = run_turn(&mut parent, Some(user_message("summarize after the child", &[])), &mut |_| {}).await;
        assert!(!outcome.cancelled && outcome.error.is_none());
        let requests = bodies.lock().unwrap();
        assert_eq!(requests.len(), 2, "delivery causes one summary request, not a loop");
        assert!(requests[1].contains("child delivered result"));
    }

    #[tokio::test]
    async fn spawned_agent_reuses_the_parent_cache_key_and_prefix() {
        let (url, bodies) = capturing_server(vec![text_reply("child answer")]).await;
        let mut parent = test_session(url, std::env::temp_dir());
        parent.agents = Some(isolated_handle());
        parent.history.record(user_message("earlier request", &[]));
        parent.history.record(json!({"role":"assistant","content":[{"type":"text","text":"earlier answer"}]}));
        parent.history.record(user_message("current request", &[]));
        let spawned = call(&parent, "spawn_agent", &json!({
            "task_name": "probe", "message": "do it", "context": "turn:1"
        })).await;
        assert!(!spawned.is_error, "{:?}", spawned.content);
        let waited = call(&parent, "wait_agent", &json!({ "timeout_ms": 5000 })).await;
        assert!(!waited.is_error);
        let requests = bodies.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].contains("earlier request"), "{requests:?}");
        assert!(!requests[0].contains("current request"), "前缀必须在本次任务之前截断");
        assert!(requests[0].contains("\"prompt_cache_key\":\"t\""), "分身必须锁父会话的缓存键");
    }

    #[tokio::test]
    async fn invalid_spawns_start_nothing() {
        let mut parent = test_session("http://unused".into(), std::env::temp_dir());
        parent.agents = Some(isolated_handle());
        assert!(call(&parent, "spawn_agent", &json!({ "task_name": "Bad Name", "message": "x" })).await.is_error);
        assert!(call(&parent, "spawn_agent", &json!({ "task_name": "ok", "message": "  " })).await.is_error);
    }
}
