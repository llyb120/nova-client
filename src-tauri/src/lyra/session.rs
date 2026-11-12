//! 主子 agent 调度（一次性分身）：主 agent 用 spawn_agent 派生进程内分身跑一件说得清的任务，
//! 用 interrupt_agent 中途叫停；分身跑完把结论交回父会话后即从注册表移除（跑完即销毁）。
//! 子 agent 继承主 agent 的模型、工具与工作目录，独立历史与上下文，深度上限 1，
//! 只在当前进程内存在；执行过程额外投影到只读 Stage（界面里叫「分身」）供回看。
//!
//! 上下文与缓存：`spawn_agent` 的 `context` 只共享父会话可见历史的**前缀**
//! （省略=当前可见历史，none=空白，turn:<n>=截到该轮）。末尾尚未返回的工具调用整条丢掉。
//! 子会话的 system prompt、工具定义与父会话完全相同，并用
//! `cache_key` 锁父会话的 `prompt_cache_key`，所以继承的前缀能命中父会话已写入的前缀缓存。
//! 子会话自身仍是独立 `session_id`，Responses 续接与后台命令作用域不会互相清。

use crate::lyra::bridge::{turn_event_sink, Emit};
use crate::lyra::context::ContextWindow;
use crate::lyra::history::{pending_calls, text_content, user_message, History};
use crate::lyra::stage::Stage;
use crate::lyra::tools::{Tool, ToolOutcome};
use crate::lyra::turn::{run_turn, text_outcome, BoxFuture, Session};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

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
    /// pending_init / running / completed / errored / interrupted
    status: Mutex<(String, Option<String>)>,
    running: AtomicBool,
    cancelled: Arc<AtomicBool>,
    stage: Option<Stage>,
}

/// 每次 spawn 时重新继承的父授权快照；只在内存里，不写入任何存档。
#[derive(Clone)]
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
        configure_context(&mut session.context, &session.system_prompt, &session.dynamic_prompt, &session.tools, session.model.model.max_output_tokens);
        Ok(())
    }
}

fn configure_context(context: &mut ContextWindow, system: &str, dynamic: &str, tools: &[Tool], output: u64) {
    let schema = Value::Array(tools.iter().map(|tool| json!({
        "name": tool.name, "description": tool.description, "parameters": tool.parameters
    })).collect());
    context.configure_tokens(Default::default(), system, dynamic, &schema, output);
}

impl Child {
    fn set_status(&self, status: &str, message: Option<String>) {
        *self.status.lock().unwrap() = (status.into(), message);
    }

    fn status(&self) -> (String, Option<String>) {
        self.status.lock().unwrap().clone()
    }

    fn active(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    /// 交回父会话的结果行；分身跑完即销毁，所以没有排队/续派字段。
    fn row(&self, path: &str) -> Value {
        let (status, message) = self.status();
        let message = message.map(|message| {
            let mut chars = message.chars();
            let mut text: String = chars.by_ref().take(4000).collect();
            if chars.next().is_some() {
                text.push_str("\n[truncated]");
            }
            text
        });
        json!({ "path": path, "task_name": self.name, "sessionId": self.session_id,
            "status": status, "message": message })
    }
}

#[derive(Default)]
struct Registry {
    agents: Mutex<HashMap<String, Arc<Child>>>,
    /// 已结束分身的结果，父 turn 在无工具出口按完成先后交付，不必等最慢的。
    results: Mutex<VecDeque<Value>>,
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

const AGENT_TOOLS: [&str; 2] = ["spawn_agent", "interrupt_agent"];

pub fn is_agent_tool(name: &str) -> bool {
    AGENT_TOOLS.contains(&name)
}

pub fn agent_tools(eager: bool) -> Vec<Tool> {
    let target = json!({ "type": "string", "description": "task_name used when spawning it" });
    let target_only = json!({ "type": "object", "properties": { "target": target }, "required": ["target"] });
    let context = json!({
        "type": "string",
        "description": "Leading history prefix. Omit (default) when the child needs files already read or decisions already made; returned read/polaris bodies are included, and the in-flight assistant tool-call is left out. \"none\" when the message is self-contained and parent history is another topic or only noise. \"turn:N\" keeps through the Nth user message you can see (1-based, including that turn's later assistant replies and tool results, stopping before the next user message) and drops later user turns. Count that message first; do not pass the last user message (same as omit); if unsure, omit. A wrong, out-of-range, or 0 value shares the whole visible history, not a blank session. Do not copy an example number. The shared prefix reuses your prompt cache."
    });
    // Legend 只放宽第一波门槛；交回是增量的，交回后仍可再开一波。
    let policy = if eager {
        "Under Legend, if the task splits into 2 or more independent tracks, spawn them together in this turn by default; do not do those tracks yourself, and do not wait until the saving beats the overhead. Still skip spawning when one step finishes it, the steps must share intermediate conclusions, or there is only one track. After spawning, keep doing work that does not depend on the children; do not idle-wait. Completed reports arrive as soon as you stop issuing new tool calls (earliest first). If at least 2 independent tracks remain after a handoff, spawn another same-turn wave instead of doing them yourself."
    } else {
        "Spawn only when the work truly parallelizes and the saving beats the overhead: start independent tasks with multiple native spawn_agent calls in the same turn, then keep doing work that does not depend on the children. Completed reports arrive incrementally (earliest first) once you stop issuing new tool calls. After a handoff, if at least 2 independent tracks remain, spawn another same-turn wave instead of doing them serially yourself."
    };
    vec![
        Tool {
            name: "spawn_agent",
            description: format!("Start a one-shot sub-agent on a task you can state in one message; it runs in the background with your tools and working directory and returns immediately, its result arriving automatically once you stop issuing new tool calls (finished agents are handed off without waiting for slower peers). {policy} The agent is discarded when it finishes (only its report comes back, not its own exploration), so to continue the same area spawn again with the same task_name plus the conclusions you got back. To feed this spawn a same-turn read or polaris result, pass _after pointing at those calls. Failures come back as status errored with the error text; do not auto-retry, explain or take over that task, and keep delegating any remaining independent tracks. At most {MAX_ACTIVE} run at once; sub-agents cannot spawn further agents."),
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
            name: "interrupt_agent",
            description: "Cancel a running sub-agent; it reports back as interrupted and is discarded like any other finish.".into(),
            parameters: target_only,
        },
    ]
}

impl AgentHandle {
    /// 父 turn 提前收尾的统一入口（用户取消、provider 错误、panic）：封住调度并中断全部分身。
    pub fn cancel_all(&self) {
        self.registry.stopped.store(true, Ordering::SeqCst);
        for child in self.registry.agents.lock().unwrap().values() {
            child.cancelled.store(true, Ordering::SeqCst);
            if child.active() {
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
        self.registry.agents.lock().unwrap().values().any(|child| child.active())
    }

    pub fn has_ready(&self) -> bool {
        !self.registry.results.lock().unwrap().is_empty()
    }

    /// 取出当前已完成的结果，不等仍在跑的分身。取消时不消费，留给取消路径处理。
    pub fn take_ready(&self) -> Option<Value> {
        let mut results = self.registry.results.lock().unwrap();
        if results.is_empty() {
            return None;
        }
        Some(Value::Array(results.drain(..).collect()))
    }

    /// 等到至少一条结果入队、全部结束、或取消/steer。不消费队列，避免和其它等待竞态丢结果。
    pub async fn wait_until_ready(&self, cancelled: &AtomicBool, steer: &Mutex<VecDeque<Value>>) {
        loop {
            if cancelled.load(Ordering::SeqCst) || !steer.lock().unwrap().is_empty() {
                return;
            }
            if self.has_ready() || !self.has_pending() {
                return;
            }
            let notified = self.registry.changed.notified();
            if cancelled.load(Ordering::SeqCst) || !steer.lock().unwrap().is_empty() || self.has_ready() || !self.has_pending() {
                return;
            }
            tokio::select! {
                _ = notified => {},
                _ = tokio::time::sleep(Duration::from_millis(50)) => {},
            }
        }
    }

    /// 主 turn 在无工具出口调用：有已完成结果就立刻交回，仍在跑的分身留待下次。
    /// 取消/steer 时不消费队列。
    pub async fn await_pending(&self, cancelled: &AtomicBool, steer: &Mutex<VecDeque<Value>>) -> Option<Value> {
        if cancelled.load(Ordering::SeqCst) || !steer.lock().unwrap().is_empty() {
            return None;
        }
        if let Some(ready) = self.take_ready() {
            return Some(ready);
        }
        if !self.has_pending() {
            return None;
        }
        self.wait_until_ready(cancelled, steer).await;
        if cancelled.load(Ordering::SeqCst) || !steer.lock().unwrap().is_empty() {
            return None;
        }
        self.take_ready()
    }

    /// 流式请求失败时收回尚未被本轮采纳的提前 spawn：中断分身且不把结果交给父会话。
    pub fn discard(&self, path: &str) {
        if let Some(child) = self.registry.agents.lock().unwrap().remove(path) {
            child.cancelled.store(true, Ordering::SeqCst);
            if child.active() {
                child.set_status("interrupted", None);
            }
            child.running.store(false, Ordering::SeqCst);
        }
        self.registry.results.lock().unwrap().retain(|row| row["path"].as_str() != Some(path));
        self.registry.changed.notify_waiters();
    }

    fn resolve(&self, target: &str) -> Result<(String, Arc<Child>), String> {
        let name = target.strip_prefix("/root/").unwrap_or(target);
        if !valid_task_name(name) {
            return Err("invalid agent target".into());
        }
        let path = format!("{ROOT}/{name}");
        let child = self.registry.agents.lock().unwrap().get(&path).cloned();
        // 跑完即销毁，所以已结束的分身不再可寻址。
        child.map(|child| (path, child)).ok_or_else(|| format!("not_found: no running agent {target}"))
    }
}

/// 流式期间父会话只读快照：历史在生成过程中不变，供 spawn_agent 提前派发。
#[derive(Clone)]
pub struct SpawnParent {
    settings: ChildSettings,
    view: Vec<Value>,
    window: u64,
    overhead: u64,
    archive_dir: Option<std::path::PathBuf>,
    agents: Option<AgentHandle>,
    governor: bool,
    project_memory: Option<crate::lyra::context::ProjectMemory>,
}

impl SpawnParent {
    pub fn from_session(parent: &Session) -> Self {
        Self {
            settings: ChildSettings::from_parent(parent),
            view: parent.context.workspace_view(parent.history.items()),
            window: parent.context.window(),
            overhead: parent.context.overhead(),
            archive_dir: parent.archive_dir.clone(),
            agents: parent.agents.clone(),
            governor: parent.governor.is_some(),
            project_memory: parent.context.project_memory_state(),
        }
    }
}

pub async fn call(parent: &Session, name: &str, args: &Value) -> ToolOutcome {
    call_with(parent, name, args, &[]).await
}

/// `batch_suffix` 是同批已完成、且本 spawn 通过 _after 排在其后的工具调用/结果，供分身看见。
pub async fn call_with(
    parent: &Session,
    name: &str,
    args: &Value,
    batch_suffix: &[Value],
) -> ToolOutcome {
    // 桌面归属键是 thread:<id>，不同于模型 session_id；只使用父会话持有的句柄。
    let Some(handle) = parent.agents.clone() else {
        return text_outcome(format!("{name} 仅在主会话中可用；分身不能再派生分身"), true);
    };
    let result = match name {
        "spawn_agent" => spawn(&handle, ParentView::Session(parent), args, batch_suffix),
        "interrupt_agent" => control(&handle, args),
        _ => Err(format!("unknown agent tool: {name}")),
    };
    finish_call(result)
}

/// 流式 ToolCallDone 时同步启动 spawn_agent，不走 execute()。
pub fn call_snapshot(source: &SpawnParent, name: &str, args: &Value) -> ToolOutcome {
    let Some(handle) = source.agents.clone() else {
        return text_outcome(format!("{name} 仅在主会话中可用；分身不能再派生分身"), true);
    };
    let result = match name {
        "spawn_agent" => spawn(&handle, ParentView::Snapshot(source), args, &[]),
        "interrupt_agent" => control(&handle, args),
        _ => Err(format!("unknown agent tool: {name}")),
    };
    finish_call(result)
}

fn finish_call(result: Result<Value, String>) -> ToolOutcome {
    match result {
        Ok(value) => text_outcome(value.to_string(), false),
        Err(error) => text_outcome(error, true),
    }
}

enum ParentView<'a> {
    Session(&'a Session),
    Snapshot(&'a SpawnParent),
}

impl ParentView<'_> {
    fn view(&self) -> Vec<Value> {
        match self {
            Self::Session(parent) => parent.context.workspace_view(parent.history.items()),
            Self::Snapshot(source) => source.view.clone(),
        }
    }

    fn cwd(&self) -> String {
        match self {
            Self::Session(parent) => parent.cwd.to_string_lossy().into_owned(),
            Self::Snapshot(source) => source.settings.cwd.to_string_lossy().into_owned(),
        }
    }

    fn settings(&self) -> ChildSettings {
        match self {
            Self::Session(parent) => ChildSettings::from_parent(parent),
            Self::Snapshot(source) => source.settings.clone(),
        }
    }

    fn window(&self) -> (u64, u64) {
        match self {
            Self::Session(parent) => (parent.context.window(), parent.context.overhead()),
            Self::Snapshot(source) => (source.window, source.overhead),
        }
    }

    fn archive_dir(&self) -> Option<std::path::PathBuf> {
        match self {
            Self::Session(parent) => parent.archive_dir.clone(),
            Self::Snapshot(source) => source.archive_dir.clone(),
        }
    }

    fn governor(&self) -> bool {
        match self {
            Self::Session(parent) => parent.governor.is_some(),
            Self::Snapshot(source) => source.governor,
        }
    }
}

fn control(handle: &AgentHandle, args: &Value) -> Result<Value, String> {
    let target = args
        .get("target")
        .and_then(Value::as_str)
        .unwrap_or_default();
    handle.resolve(target).map(|(path, child)| {
        // 只看一次性任务：中断即让它以 interrupted 收口，随后同样从注册表移除。
        child.cancelled.store(true, Ordering::SeqCst);
        if child.active() {
            child.set_status("interrupted", None);
        }
        if let Some(stage) = &child.stage {
            stage.note("主会话已请求中断。".into());
        }
        handle.registry.changed.notify_waiters();
        child.row(&path)
    })
}

type Agents<'a> = MutexGuard<'a, HashMap<String, Arc<Child>>>;

/// 校验 task_name 并占住注册表槽位：返回 (task_name, message, path, 已加锁的注册表)。
fn reserve<'a>(
    handle: &'a AgentHandle,
    args: &'a Value,
) -> Result<(&'a str, &'a str, String, Agents<'a>), String> {
    let task_name = args
        .get("task_name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !valid_task_name(task_name) {
        return Err("task_name must be 1-64 lowercase letters, digits or underscores".into());
    }
    let message = args
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if message.trim().is_empty() {
        return Err("message must describe the whole task".into());
    }
    let path = format!("{ROOT}/{task_name}");
    let agents = handle.registry.agents.lock().unwrap();
    if handle.registry.stopped.load(Ordering::SeqCst) {
        return Err("本轮调度已停止，请等待用户继续任务".into());
    }
    if agents.get(&path).is_some_and(|child| child.active()) {
        return Err(format!("{path} is still running; interrupt_agent first"));
    }
    if agents.values().filter(|child| child.active()).count() >= MAX_ACTIVE {
        return Err(format!(
            "{MAX_ACTIVE} agents are already running; wait for them to finish first"
        ));
    }
    Ok((task_name, message, path, agents))
}

fn new_child(name: &str, stage: Option<Stage>) -> Arc<Child> {
    Arc::new(Child {
        name: name.into(),
        session_id: uuid::Uuid::new_v4().to_string(),
        status: Mutex::new(("pending_init".into(), None)),
        running: AtomicBool::new(false),
        cancelled: Arc::new(AtomicBool::new(false)),
        stage,
    })
}

fn spawn(
    handle: &AgentHandle,
    parent: ParentView<'_>,
    args: &Value,
    batch_suffix: &[Value],
) -> Result<Value, String> {
    let (task_name, message, path, mut agents) = reserve(handle, args)?;
    let context = args.get("context").and_then(Value::as_str).unwrap_or("");
    let view = parent.view();
    let users = user_turns(&view);
    let mut forked = without_open_tail(inherit_view(&view, &users, context));
    forked.extend(batch_suffix.iter().cloned());
    // 分身线程的模型串由 Stage 从父线程原样继承（含 provider 与 variant）。
    let stage = handle
        .stage_parent
        .as_ref()
        .and_then(|(app, parent_id)| Stage::new(app, parent_id, task_name, parent.cwd()));
    let child = new_child(task_name, stage);
    let session = match child_session(&parent, &child, forked) {
        Ok(session) => session,
        // 启动就失败：注册表未被占用，Stage 留一条失败记录供回看。
        Err(error) => {
            if let Some(stage) = &child.stage {
                stage.note(format!("分身启动失败：{error}"));
            }
            return Err(error);
        }
    };
    // 同名分身此时已结束并被移除，直接占位覆盖。占位即算「在跑」，否则父会话可能在 worker 启动前就认为无事可等。
    child.running.store(true, Ordering::SeqCst);
    agents.insert(path.clone(), child.clone());
    drop(agents);
    let session_id = child.session_id.clone();
    tokio::spawn(child_loop(
        child,
        session,
        user_message(message, &[]),
        handle.registry.clone(),
    ));
    Ok(json!({ "path": path, "status": "pending_init", "sessionId": session_id }))
}

/// 子 agent 只继承当前授权快照；system prompt 与工具与父会话完全一致（护缓存），
/// 历史只可能是父会话可见视图的前缀，外加同批已完成且经 _after 排在本 spawn 之后的结果。
fn child_session(
    parent: &ParentView<'_>,
    child: &Child,
    forked: Vec<Value>,
) -> Result<Session, String> {
    let mut settings = parent.settings();
    // 工具与 system 原样沿用父会话只为命中前缀缓存；在可变段说清分身不能再派分身。
    settings.dynamic_prompt.push_str("\n\n你是父会话派出的分身：工具表里的 spawn_agent / interrupt_agent 是为共享父会话的 system 前缀缓存而原样保留的，你不能再派生或调度分身，不要调用它们；把结论直接写在回复里，父会话会收到；失败就把失败原因写清楚。上文已有的 read / polaris 正文视为已读，同一路径和行段不要再读；上文没有正文、只剩摘要，或文件刚被改过，才读。");
    let mut history = History::new(forked, None);
    history.close_pending_calls("（父会话未完成的工具调用，没有结果）");
    let (window, overhead) = parent.window();
    let mut context = ContextWindow::ephemeral(window, overhead);
    configure_context(
        &mut context,
        &settings.system_prompt,
        &settings.dynamic_prompt,
        &settings.tools,
        settings.model.model.max_output_tokens,
    );
    match parent {
        ParentView::Session(session) => context.copy_project_memory(&session.context),
        ParentView::Snapshot(source) => context.restore_project_memory(source.project_memory.clone()),
    }
    let mcp = tokio::sync::Mutex::new(crate::lyra::mcp::McpSession::new(
        settings.mcp_config.clone(),
        settings.cwd.clone(),
        settings.read_only,
    )?);
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
        archive_dir: parent
            .archive_dir()
            .map(|root| root.join("agents").join(&child.session_id)),
        shell: settings.shell,
        history,
        context,
        cancelled: child.cancelled.clone(),
        steer: Arc::new(Mutex::new(VecDeque::new())),
        agents: None,
        scenes: None,
        // 分身继承父会话的开关，计数各自独立。
        governor: parent
            .governor()
            .then(crate::lyra::turn::EffortGovernor::default),
    })
}

fn child_loop(child: Arc<Child>, session: Session, task: Value, registry: Arc<Registry>) -> BoxFuture<'static, ()> {
    Box::pin(async move {
        let path = format!("{ROOT}/{}", child.name);
        child.set_status("running", None);
        child.running.store(true, Ordering::SeqCst);
        // run_turn 内部 panic 也要收口：否则分身永远停在 running，父 turn 会一直等下去。
        let (status, message) = match tokio::spawn(run_child(child.clone(), session, task)).await {
            Ok(finished) => finished,
            Err(error) => ("errored", Some(format!("分身异常退出：{error}"))),
        };
        finish_child(&child, &registry, &path, status, message);
    })
}

/// run_turn 之后统一收口：状态、结果交付、从注册表移除（跑完即销毁）。
fn finish_child(child: &Child, registry: &Registry, path: &str, status: &str, message: Option<String>) {
    child.set_status(status, message);
    if let Some(stage) = &child.stage {
        stage.note(format!("分身以 {status} 结束，执行记录保留供回看。"));
    }
    registry.agents.lock().unwrap().remove(path);
    // 用户已经取消本轮时不再交付旧结果；其余情况在父 turn 的无工具出口交付一次。
    if !registry.stopped.load(Ordering::SeqCst) {
        registry.results.lock().unwrap().push_back(child.row(path));
    }
    // 先入队再放开「在跑」，否则父会话可能恰好错在中间：看到无 pending、results 仍空就收尾丢结果。
    child.running.store(false, Ordering::SeqCst);
    registry.changed.notify_waiters();
}

async fn run_child(child: Arc<Child>, mut session: Session, task: Value) -> (&'static str, Option<String>) {
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
    match (outcome.cancelled, outcome.error) {
        (true, _) => ("interrupted", last.or(Some("（被中断，没有文字结论）".into()))),
        // 失败必须带回原因，父会话据此决定重派、自己接手或上报；最后输出可辅助定位。
        (_, Some(error)) => ("errored", Some(match last {
            Some(last) => format!("{error}\n\n[分身最后输出]\n{last}"),
            None => error,
        })),
        _ => ("completed", last.or(Some("（分身没有给出文字结论）".into()))),
    }
}

/// 父会话可见视图里每个用户轮次的起点。
fn user_turns(view: &[Value]) -> Vec<usize> {
    view.iter().enumerate()
        .filter(|(_, message)| message.get("role").and_then(Value::as_str) == Some("user"))
        .map(|(index, _)| index)
        .collect()
}

/// 把父会话可见历史按 `context` 切成一个**前缀**：只从最开始取到某个用户轮次结束，永不取中间段。
/// 不传 = 当前可见历史；`none` = 空白会话；其余值按 1 起的轮次号解析，越界或解析失败视为全部历史。
fn inherit_view(view: &[Value], users: &[usize], mode: &str) -> Vec<Value> {
    let mode = mode.trim();
    if mode.eq_ignore_ascii_case("none") {
        return Vec::new();
    }
    if mode.is_empty() {
        return view.to_vec();
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

/// 末尾那条还没有结果的 assistant 工具调用整段去掉。补一条假失败会让分身把父会话正在读的文件再读一遍。
fn without_open_tail(mut view: Vec<Value>) -> Vec<Value> {
    let pending = pending_calls(&view);
    let opens = view.last().is_some_and(|last| {
        last.get("role").and_then(Value::as_str) == Some("assistant")
            && last.get("content").and_then(Value::as_array).is_some_and(|parts| {
                parts.iter().any(|part| {
                    part.get("type").and_then(Value::as_str) == Some("toolCall")
                        && part.get("id").and_then(Value::as_str).is_some_and(|id| {
                            pending.iter().any(|(pending_id, _)| pending_id == id)
                        })
                })
            })
    });
    if opens {
        view.pop();
    }
    view
}

#[cfg(test)]
pub(crate) fn test_handle() -> AgentHandle {
    AgentHandle { registry: Arc::new(Registry::default()), stage_parent: None }
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::lyra::turn::tests::{capturing_server, scripted_server, test_session, text_reply};

    fn isolated_handle() -> AgentHandle {
        test_handle()
    }

    #[test]
    fn legend_spawn_tool_does_not_wait_for_a_saving() {
        let plain = agent_tools(false);
        let legend = agent_tools(true);
        assert!(plain[0].description.contains("saving beats the overhead") && plain[0].description.contains("Spawn only when"));
        assert!(legend[0].description.contains("do not wait until the saving beats the overhead"));
        assert!(!legend[0].description.contains("Spawn only when"));
        let schema = plain[0].parameters.to_string();
        assert!(schema.contains("turn:N") && !schema.contains("turn:3"));
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
        assert_eq!(inherit_view(&view, &users, "").len(), 5, "省略 context 共享当前可见历史");
        assert!(inherit_view(&view, &users, "none").is_empty());
        assert_eq!(inherit_view(&view, &users, "turn:1").len(), 2);
        assert_eq!(inherit_view(&view, &users, "2").len(), 4);
        assert_eq!(inherit_view(&view, &users, "t3").len(), 5);
        // 越界或解析失败一律视为全部历史，不报错。
        assert_eq!(inherit_view(&view, &users, "turn:9").len(), 5);
        assert_eq!(inherit_view(&view, &users, "0").len(), 5);
        assert_eq!(inherit_view(&view, &users, "prefix").len(), 5);
    }

    #[test]
    fn open_tool_call_is_dropped_but_returned_reads_stay() {
        let view = vec![
            json!({"role":"user","content":[{"type":"text","text":"one"}]}),
            json!({"role":"assistant","content":[{"type":"toolCall","id":"c1","name":"read","arguments":{"path":"kept.rs"}}]}),
            json!({"role":"toolResult","toolCallId":"c1","toolName":"read","content":[{"type":"text","text":"kept file body"}]}),
            json!({"role":"assistant","content":[{"type":"toolCall","id":"c2","name":"read","arguments":{"path":"inflight.rs"}}]}),
        ];
        let users = user_turns(&view);
        let forked = without_open_tail(inherit_view(&view, &users, ""));
        let text = serde_json::to_string(&forked).unwrap();
        assert!(text.contains("kept file body"), "{text}");
        assert!(!text.contains("inflight.rs"), "{text}");
        assert!(without_open_tail(inherit_view(&view, &users, "none")).is_empty());
    }

    /// 父会话在无工具出口等一次：结果交付一次后即消费，分身同时从注册表移除。
    #[tokio::test]
    async fn spawned_agent_reports_once_then_is_discarded() {
        let url = scripted_server(vec![text_reply("child done")]).await;
        let mut parent = test_session(url, std::env::temp_dir());
        let handle = isolated_handle();
        parent.agents = Some(handle.clone());
        let spawned = call(&parent, "spawn_agent", &json!({ "task_name": "probe", "message": "do it" })).await;
        assert!(!spawned.is_error, "{:?}", spawned.content);
        let report = handle.await_pending(&AtomicBool::new(false), &Mutex::new(VecDeque::new())).await
            .expect("finished agent must be delivered");
        let text = report.to_string();
        assert!(text.contains("\"completed\"") && text.contains("child done") && text.contains("/root/probe"), "{text}");
        assert!(handle.await_pending(&AtomicBool::new(false), &Mutex::new(VecDeque::new())).await.is_none(), "结果只交付一次");
        assert!(handle.registry.agents.lock().unwrap().is_empty(), "跑完即销毁");
        // 已结束的分身不再可寻址：要继续就重新 spawn。
        assert!(call(&parent, "interrupt_agent", &json!({ "target": "probe" })).await.is_error);
    }

    /// 失败同样要回流：父会话必须拿到 errored 与错误原因，注册表也不能留僵尸。
    #[tokio::test]
    async fn failed_agent_reports_its_error_and_leaves_no_zombie() {
        let handle = isolated_handle();
        let child = new_child("probe", None);
        handle.registry.agents.lock().unwrap().insert("/root/probe".into(), child.clone());
        finish_child(&child, &handle.registry, "/root/probe", "errored", Some("HTTP 503 Service Unavailable".into()));
        let report = handle.await_pending(&AtomicBool::new(false), &Mutex::new(VecDeque::new())).await
            .expect("失败也要交付");
        let text = report.to_string();
        assert!(text.contains("\"errored\"") && text.contains("503") && text.contains("/root/probe"), "{text}");
        assert!(handle.registry.agents.lock().unwrap().is_empty());
    }

    /// 主 turn 因 provider 错误退出时，仍在跑的分身必须一起中断：本轮已没有出口交付结果，
    /// 继续跑只会继续烧额度、继续改盘；用户续发新一轮时才重新开放调度。
    #[tokio::test]
    async fn parent_turn_error_interrupts_still_running_agents() {
        let url = scripted_server(vec![Some(vec![json!({"error":{"message":"boom"}})])]).await;
        let mut parent = test_session(url, std::env::temp_dir());
        let handle = isolated_handle();
        parent.agents = Some(handle.clone());
        let child = new_child("probe", None);
        child.running.store(true, Ordering::SeqCst);
        handle.registry.agents.lock().unwrap().insert("/root/probe".into(), child.clone());
        let outcome = run_turn(&mut parent, Some(user_message("go", &[])), &mut |_| {}).await;
        assert!(outcome.error.as_deref().is_some_and(|error| error.contains("boom")), "{outcome:?}");
        assert!(child.cancelled.load(Ordering::SeqCst), "分身必须被中断");
        assert_eq!(child.status().0, "interrupted");
        assert!(handle.registry.stopped.load(Ordering::SeqCst), "错误收尾后调度必须封住");
    }

    /// 同名可重派：上一个已结束并移除，不必改名。
    #[tokio::test]
    async fn spawn_again_with_the_same_name_after_it_finished() {
        let url = scripted_server(vec![text_reply("first"), text_reply("second")]).await;
        let mut parent = test_session(url, std::env::temp_dir());
        let handle = isolated_handle();
        parent.agents = Some(handle.clone());
        let spawned = call(&parent, "spawn_agent", &json!({ "task_name": "probe", "message": "round one" })).await;
        assert!(!spawned.is_error, "{:?}", spawned.content);
        assert!(handle.await_pending(&AtomicBool::new(false), &Mutex::new(VecDeque::new())).await.is_some());
        let again = call(&parent, "spawn_agent", &json!({ "task_name": "probe", "message": "round two" })).await;
        assert!(!again.is_error, "{:?}", again.content);
    }

    #[tokio::test]
    async fn parent_final_waits_for_child_then_summarizes_its_result_once() {
        let (url, bodies) = capturing_server(vec![text_reply("intermediate parent answer"), text_reply("final summary with child result")]).await;
        let mut parent = test_session(url, std::env::temp_dir());
        let handle = isolated_handle();
        parent.agents = Some(handle.clone());
        let child = new_child("probe", None);
        finish_child(&child, &handle.registry, "/root/probe", "completed", Some("child delivered result".into()));
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
        let handle = isolated_handle();
        parent.agents = Some(handle.clone());
        parent.history.record(user_message("earlier request", &[]));
        parent.history.record(json!({"role":"assistant","content":[{"type":"text","text":"earlier answer"}]}));
        parent.history.record(user_message("current request", &[]));
        let spawned = call(&parent, "spawn_agent", &json!({
            "task_name": "probe", "message": "do it", "context": "turn:1"
        })).await;
        assert!(!spawned.is_error, "{:?}", spawned.content);
        assert!(handle.await_pending(&AtomicBool::new(false), &Mutex::new(VecDeque::new())).await.is_some());
        let requests = bodies.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].contains("earlier request"), "{requests:?}");
        assert!(!requests[0].contains("current request"), "前缀必须在本次任务之前截断");
        assert!(requests[0].contains("\"prompt_cache_key\":\"t\""), "分身必须锁父会话的缓存键");
        assert!(requests[0].contains("不能再派生或调度分身"), "分身工具为缓存保留，可变段必须说明不能用");
        assert!(requests[0].contains("视为已读"), "分身必须把上文已读正文当作已读");
    }

    #[tokio::test]
    async fn omitted_context_shares_returned_reads_and_drops_the_open_call() {
        let (url, bodies) = capturing_server(vec![text_reply("child answer")]).await;
        let mut parent = test_session(url, std::env::temp_dir());
        let handle = isolated_handle();
        parent.agents = Some(handle.clone());
        parent.history.record(user_message("read the file", &[]));
        parent.history.record(json!({"role":"assistant","content":[{"type":"toolCall","id":"c1","name":"read","arguments":{"path":"kept.rs"}}]}));
        parent.history.record(json!({"role":"toolResult","toolCallId":"c1","toolName":"read","content":[{"type":"text","text":"kept file body"}]}));
        parent.history.record(json!({"role":"assistant","content":[{"type":"toolCall","id":"c2","name":"read","arguments":{"path":"inflight.rs"}}]}));
        let spawned = call(&parent, "spawn_agent", &json!({ "task_name": "probe", "message": "use what you have" })).await;
        assert!(!spawned.is_error, "{:?}", spawned.content);
        assert!(handle.await_pending(&AtomicBool::new(false), &Mutex::new(VecDeque::new())).await.is_some());
        let body = bodies.lock().unwrap()[0].clone();
        assert!(body.contains("kept file body"), "{body}");
        assert!(!body.contains("inflight.rs"), "{body}");
        assert!(!body.contains("已分叉给分身"), "{body}");
    }

    #[tokio::test]
    async fn invalid_spawns_start_nothing() {
        let mut parent = test_session("http://unused".into(), std::env::temp_dir());
        parent.agents = Some(isolated_handle());
        assert!(call(&parent, "spawn_agent", &json!({ "task_name": "Bad Name", "message": "x" })).await.is_error);
        assert!(call(&parent, "spawn_agent", &json!({ "task_name": "ok", "message": "  " })).await.is_error);
    }

    /// 先完成的分身立刻交回，不必等更慢的那个；队列只消费一次。
    #[tokio::test]
    async fn first_finished_agent_is_delivered_without_waiting_for_a_slower_peer() {
        let handle = isolated_handle();
        let fast = new_child("fast", None);
        let slow = new_child("slow", None);
        slow.running.store(true, Ordering::SeqCst);
        handle
            .registry
            .agents
            .lock()
            .unwrap()
            .insert("/root/slow".into(), slow.clone());
        finish_child(
            &fast,
            &handle.registry,
            "/root/fast",
            "completed",
            Some("fast done".into()),
        );
        let first = tokio::time::timeout(
            Duration::from_millis(200),
            handle.await_pending(&AtomicBool::new(false), &Mutex::new(VecDeque::new())),
        )
        .await
        .expect("must not wait for the slower agent");
        let text = first.expect("fast result").to_string();
        assert!(
            text.contains("fast done") && text.contains("/root/fast"),
            "{text}"
        );
        assert!(!text.contains("/root/slow"), "{text}");
        assert!(handle.take_ready().is_none(), "结果只交付一次");
        assert!(
            tokio::time::timeout(
                Duration::from_millis(80),
                handle.await_pending(&AtomicBool::new(false), &Mutex::new(VecDeque::new())),
            )
            .await
            .is_err(),
            "慢的还在跑时不应返回"
        );
        finish_child(
            &slow,
            &handle.registry,
            "/root/slow",
            "completed",
            Some("slow done".into()),
        );
        let second = handle
            .await_pending(&AtomicBool::new(false), &Mutex::new(VecDeque::new()))
            .await
            .expect("slow result after it finishes");
        let text = second.to_string();
        assert!(
            text.contains("slow done") && !text.contains("fast done"),
            "{text}"
        );
        assert!(handle
            .await_pending(&AtomicBool::new(false), &Mutex::new(VecDeque::new()))
            .await
            .is_none());
    }

    /// 增量交付后父会话再 spawn，同名可重派，且不会把上一波结果再交一次。
    #[tokio::test]
    async fn second_wave_spawn_after_incremental_handoff() {
        let (url, bodies) = capturing_server(vec![
            text_reply("ack first wave"),
            Some(vec![json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"s2",
                "function":{"name":"spawn_agent","arguments":"{\"task_name\":\"probe\",\"message\":\"round two\"}"}}]},
                "finish_reason":"tool_calls"}]})]),
            text_reply("child wave two"),
            text_reply("after spawn"),
            text_reply("final after second wave"),
        ]).await;
        let mut parent = test_session(url, std::env::temp_dir());
        let handle = isolated_handle();
        parent.agents = Some(handle.clone());
        parent.tools.extend(agent_tools(false));
        parent.read_only = false;
        let child = new_child("probe", None);
        finish_child(
            &child,
            &handle.registry,
            "/root/probe",
            "completed",
            Some("wave one result".into()),
        );
        let outcome = run_turn(
            &mut parent,
            Some(user_message("continue the work", &[])),
            &mut |_| {},
        )
        .await;
        assert!(!outcome.cancelled && outcome.error.is_none(), "{outcome:?}");
        let history = serde_json::to_string(parent.history.items()).unwrap();
        assert_eq!(
            history.matches("wave one result").count(),
            1,
            "wave one must not be double-delivered: {history}"
        );
        assert!(
            history.contains("round two") || history.contains("child wave two"),
            "{history}"
        );
        let requests = bodies.lock().unwrap();
        assert!(
            requests.iter().any(|body| body.contains("wave one result")),
            "{requests:?}"
        );
        assert!(
            requests
                .iter()
                .any(|body| body.contains("child wave two") || body.contains("round two")),
            "{requests:?}"
        );
    }

    #[tokio::test]
    async fn same_batch_completed_read_is_visible_to_a_later_spawn() {
        let (url, bodies) = capturing_server(vec![text_reply("child used the read")]).await;
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("kept.rs"), "VISIBLE_BATCH_BODY").unwrap();
        let mut parent = test_session(url, dir.path().into());
        let handle = isolated_handle();
        parent.agents = Some(handle.clone());
        parent.read_only = false;
        parent
            .tools
            .extend(crate::lyra::tools::tool_set(false, false, false));
        parent.tools.extend(agent_tools(false));
        parent.history.record(user_message("read then spawn", &[]));
        let calls = vec![
            ("c1".into(), "read".into(), json!({"path":"kept.rs"})),
            (
                "c2".into(),
                "spawn_agent".into(),
                json!({"task_name":"probe","message":"use the file","_after":[1]}),
            ),
        ];
        let results = crate::lyra::turn::run_tools_for_tests(&mut parent, &calls).await;
        assert_eq!(
            results
                .iter()
                .map(|r| r["isError"].as_bool().unwrap())
                .collect::<Vec<_>>(),
            [false, false],
            "{results:?}"
        );
        assert!(handle
            .await_pending(&AtomicBool::new(false), &Mutex::new(VecDeque::new()))
            .await
            .is_some());
        let body = bodies.lock().unwrap()[0].clone();
        assert!(
            body.contains("VISIBLE_BATCH_BODY"),
            "same-batch read must reach the child: {body}"
        );
    }

    #[tokio::test]
    async fn in_flight_same_batch_read_is_not_leaked_without_after() {
        let (url, bodies) = capturing_server(vec![text_reply("child started concurrently")]).await;
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("secret.rs"), "LEAKED_BATCH_BODY").unwrap();
        let mut parent = test_session(url, dir.path().into());
        let handle = isolated_handle();
        parent.agents = Some(handle.clone());
        parent.read_only = false;
        parent
            .tools
            .extend(crate::lyra::tools::tool_set(false, false, false));
        parent.tools.extend(agent_tools(false));
        parent
            .history
            .record(user_message("spawn beside a read", &[]));
        // 无 _after：spawn 立即启动，不能看到尚未入史的同批读取。
        let calls = vec![
            ("c1".into(), "read".into(), json!({"path":"secret.rs"})),
            (
                "c2".into(),
                "spawn_agent".into(),
                json!({"task_name":"probe","message":"do not look at secret"}),
            ),
        ];
        let results = crate::lyra::turn::run_tools_for_tests(&mut parent, &calls).await;
        assert!(results.iter().all(|r| r["isError"] == false), "{results:?}");
        assert!(handle
            .await_pending(&AtomicBool::new(false), &Mutex::new(VecDeque::new()))
            .await
            .is_some());
        let body = bodies.lock().unwrap()[0].clone();
        assert!(
            !body.contains("LEAKED_BATCH_BODY"),
            "in-flight read must not leak: {body}"
        );
    }
}
