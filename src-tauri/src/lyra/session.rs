//! 主子 agent 调度（一次性分身）：主 agent 用 spawn_agent 派生进程内分身跑一件说得清的任务，
//! 用 interrupt_agent 中途叫停；分身跑完把结论交回父会话后即从注册表移除（跑完即销毁）。
//! 子 agent 继承主 agent 的模型、工具与工作目录，独立历史与上下文，深度上限 1，
//! 只在当前进程内存在；执行过程额外投影到只读 Stage（界面里叫「分身」）供回看。
//!
//! 上下文与缓存：`spawn_agent` 的 `context` 只共享父会话可见历史的**前缀**
//! （none/turn:<n>）；子会话的 system prompt、工具定义与父会话完全相同，并用
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
    /// 已结束分身的结果，等父 turn 在无工具出口交付一次。
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

pub fn agent_tools() -> Vec<Tool> {
    let target = json!({ "type": "string", "description": "task_name used when spawning it" });
    let target_only = json!({ "type": "object", "properties": { "target": target }, "required": ["target"] });
    let context = json!({
        "type": "string",
        "description": "Optional 1-based user-turn number to share as a leading prefix, e.g. \"turn:3\" or \"3\": the sub-agent sees everything from the very start through the end of that turn, which reuses your prompt cache. Omit (or \"none\") for a fresh session; an out-of-range value shares the whole visible history."
    });
    vec![
        Tool {
            name: "spawn_agent",
            description: format!("Start a one-shot sub-agent on a task you can state in one message; it runs in the background with your tools and working directory and returns immediately, its result arriving automatically once you stop calling tools. Spawn only when the work truly parallelizes and the saving beats the overhead: start independent tasks with multiple native spawn_agent calls in the same turn, then keep doing your own work. The agent is discarded when it finishes (only its report comes back, not its own exploration), so to continue the same area spawn again with the same task_name plus the conclusions you got back. Failures come back as status errored with the error text. At most {MAX_ACTIVE} run at once; sub-agents cannot spawn further agents."),
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
    /// 用户取消父 turn 的统一入口，适用于 UI 和无 thread 的原生会话。
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

    fn has_pending(&self) -> bool {
        self.registry.agents.lock().unwrap().values().any(|child| child.active())
    }

    /// 主 turn 在最终无工具出口调用：先等手头分身跑完，再把它们的结果交回一次。
    /// 取消/steer 提前返回且不吞结果；用户已取消本轮时结果不再交付。
    pub async fn await_pending(&self, cancelled: &AtomicBool, steer: &Mutex<VecDeque<Value>>) -> Option<Value> {
        if !self.has_pending() && self.registry.results.lock().unwrap().is_empty() {
            return None;
        }
        while self.has_pending() && !cancelled.load(Ordering::SeqCst) && steer.lock().unwrap().is_empty() {
            tokio::select! {
                _ = self.registry.changed.notified() => {},
                _ = tokio::time::sleep(Duration::from_millis(50)) => {},
            }
        }
        let deliver = !cancelled.load(Ordering::SeqCst) && steer.lock().unwrap().is_empty();
        let mut results = self.registry.results.lock().unwrap();
        if results.is_empty() {
            return None;
        }
        Some(Value::Array(if deliver { results.drain(..).collect() } else { results.iter().cloned().collect() }))
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

pub async fn call(parent: &Session, name: &str, args: &Value) -> ToolOutcome {
    // 桌面归属键是 thread:<id>，不同于模型 session_id；只使用父会话持有的句柄。
    let Some(handle) = parent.agents.clone() else {
        return text_outcome(format!("{name} 仅在主会话中可用；分身不能再派生分身"), true);
    };
    let result = match name {
        "spawn_agent" => spawn(&handle, parent, args),
        "interrupt_agent" => control(&handle, args),
        _ => Err(format!("unknown agent tool: {name}")),
    };
    match result {
        Ok(value) => text_outcome(value.to_string(), false),
        Err(error) => text_outcome(error, true),
    }
}

fn control(handle: &AgentHandle, args: &Value) -> Result<Value, String> {
    let target = args.get("target").and_then(Value::as_str).unwrap_or_default();
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
    if agents.get(&path).is_some_and(|child| child.active()) {
        return Err(format!("{path} is still running; interrupt_agent first"));
    }
    if agents.values().filter(|child| child.active()).count() >= MAX_ACTIVE {
        return Err(format!("{MAX_ACTIVE} agents are already running; wait for them to finish first"));
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

fn spawn(handle: &AgentHandle, parent: &Session, args: &Value) -> Result<Value, String> {
    let (task_name, message, path, mut agents) = reserve(handle, args)?;
    let context = args.get("context").and_then(Value::as_str).unwrap_or("");
    let view = parent.context.view(parent.history.items());
    let users = user_turns(&view);
    let forked = inherit_view(&view, &users, context);
    let stage = handle.stage_parent.as_ref().and_then(|(app, parent_id)| {
        Stage::new(app, parent_id, task_name, parent.cwd.to_string_lossy().into_owned(), parent.model.model.id.clone())
    });
    let child = new_child(task_name, stage);
    let session = match child_session(parent, &child, forked) {
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
    tokio::spawn(child_loop(child, session, user_message(message, &[]), handle.registry.clone()));
    Ok(json!({ "path": path, "status": "pending_init", "sessionId": session_id }))
}

/// 子 agent 只继承当前授权快照；system prompt 与工具与父会话完全一致（护缓存），
/// 历史只可能是父会话可见视图的前缀。
fn child_session(parent: &Session, child: &Child, forked: Vec<Value>) -> Result<Session, String> {
    let mut settings = ChildSettings::from_parent(parent);
    // 工具与 system 原样沿用父会话只为命中前缀缓存；在可变段说清分身不能再派分身。
    settings.dynamic_prompt.push_str("\n\n你是父会话派出的分身：工具表里的 spawn_agent / interrupt_agent 是为共享父会话的 system 前缀缓存而原样保留的，你不能再派生或调度分身，不要调用它们；把结论直接写在回复里，父会话会收到；失败就把失败原因写清楚。");
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
        steer: Arc::new(Mutex::new(VecDeque::new())),
        agents: None,
        scenes: None,
        // 分身继承父会话的开关，计数各自独立。
        governor: parent.governor.as_ref().map(|_| crate::lyra::turn::EffortGovernor::default()),
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
    }

    #[tokio::test]
    async fn invalid_spawns_start_nothing() {
        let mut parent = test_session("http://unused".into(), std::env::temp_dir());
        parent.agents = Some(isolated_handle());
        assert!(call(&parent, "spawn_agent", &json!({ "task_name": "Bad Name", "message": "x" })).await.is_error);
        assert!(call(&parent, "spawn_agent", &json!({ "task_name": "ok", "message": "  " })).await.is_error);
    }
}
