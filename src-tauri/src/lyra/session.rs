//! 多 agent（Codex multi_agents_v2 的精简版）：主 agent 用 spawn_agent 派生进程内子 agent，
//! 用 send_message / followup_task / wait_agent / list_agents / interrupt_agent / close_agent 协作。
//! 子 agent 继承主 agent 的模型、工具与工作目录，历史只在内存里（不落盘），深度上限 1。

use crate::lyra::context::ContextWindow;
use crate::lyra::history::{text_content, user_message, History};
use crate::lyra::tools::{Tool, ToolOutcome};
use crate::lyra::turn::{run_turn, text_outcome, BoxFuture, Session, TurnEvent};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

const MAX_ACTIVE: usize = 6;
const ROOT: &str = "/root";

struct Child {
    /// pending_init / running / completed / errored / interrupted / shutdown
    status: Mutex<(String, Option<String>)>,
    tasks: Mutex<Option<tokio::sync::mpsc::UnboundedSender<Value>>>,
    steer: Arc<Mutex<VecDeque<Value>>>,
    cancelled: Arc<AtomicBool>,
}

impl Child {
    fn status(&self) -> (String, Option<String>) {
        self.status.lock().unwrap().clone()
    }

    fn active(&self) -> bool {
        matches!(self.status().0.as_str(), "pending_init" | "running")
    }
}

#[derive(Default)]
struct Registry {
    agents: Mutex<HashMap<String, Arc<Child>>>,
    changed: tokio::sync::Notify,
}

/// 主 agent 持有的控制面句柄。
#[derive(Clone)]
pub struct AgentHandle {
    registry: Arc<Registry>,
}

// ponytail: 注册表按根会话常驻进程内存、不回收；子 agent 随 close_agent 或进程退出结束。
pub fn root_handle(session_id: &str) -> AgentHandle {
    static REGISTRIES: OnceLock<Mutex<HashMap<String, Arc<Registry>>>> = OnceLock::new();
    let registry = REGISTRIES
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .entry(session_id.to_string())
        .or_default()
        .clone();
    AgentHandle { registry }
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
    vec![
        Tool {
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
        },
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
    fn resolve(&self, target: &str) -> Result<(String, Arc<Child>), String> {
        let agents = self.registry.agents.lock().unwrap();
        let path = if target.starts_with('/') { target.to_string() } else { format!("{ROOT}/{target}") };
        agents
            .get(&path)
            .map(|child| (path, child.clone()))
            .ok_or_else(|| format!("not_found: no agent {target}"))
    }

    fn report(&self, prefix: Option<&str>) -> Value {
        let agents = self.registry.agents.lock().unwrap();
        let mut rows: Vec<Value> = agents
            .iter()
            .filter(|(path, _)| prefix.is_none_or(|p| path.starts_with(p)))
            .map(|(path, child)| {
                let (status, message) = child.status();
                json!({ "path": path, "status": status, "message": message })
            })
            .collect();
        rows.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
        Value::Array(rows)
    }
}

pub async fn call(handle: &AgentHandle, parent: &Session, name: &str, args: &Value) -> ToolOutcome {
    let result = match name {
        "spawn_agent" => spawn(handle, parent, args),
        "list_agents" => Ok(handle.report(args.get("path_prefix").and_then(Value::as_str))),
        "wait_agent" => wait(handle, parent, args).await,
        _ => {
            let target = args.get("target").and_then(Value::as_str).unwrap_or_default();
            let message = args.get("message").and_then(Value::as_str).unwrap_or_default();
            handle.resolve(target).and_then(|(path, child)| {
                match name {
                    "send_message" => child.steer.lock().unwrap().push_back(user_message(message, &[])),
                    "followup_task" => {
                        let sent = child.tasks.lock().unwrap().as_ref().map(|tx| tx.send(user_message(message, &[])));
                        if !matches!(sent, Some(Ok(()))) {
                            return Err(format!("{path} is shut down"));
                        }
                    }
                    "interrupt_agent" => child.cancelled.store(true, Ordering::SeqCst),
                    _ => {
                        child.cancelled.store(true, Ordering::SeqCst);
                        child.tasks.lock().unwrap().take();
                        handle.registry.agents.lock().unwrap().remove(&path);
                    }
                }
                Ok(json!({ "path": path, "status": child.status().0 }))
            })
        }
    };
    match result {
        Ok(value) => text_outcome(value.to_string(), false),
        Err(error) => text_outcome(error, true),
    }
}

fn spawn(handle: &AgentHandle, parent: &Session, args: &Value) -> Result<Value, String> {
    let task_name = args.get("task_name").and_then(Value::as_str).unwrap_or_default();
    if task_name.is_empty() || !task_name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') {
        return Err("task_name must be lowercase letters, digits and underscores".into());
    }
    let message = args.get("message").and_then(Value::as_str).unwrap_or_default();
    let path = format!("{ROOT}/{task_name}");
    let mut agents = handle.registry.agents.lock().unwrap();
    if agents.get(&path).is_some_and(|c| c.status().0 != "shutdown") {
        return Err(format!("{path} already exists; use followup_task or pick another task_name"));
    }
    if agents.values().filter(|c| c.active()).count() >= MAX_ACTIVE {
        return Err(format!("{MAX_ACTIVE} agents are already running; wait_agent or close_agent first"));
    }

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

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    tx.send(user_message(message, &[])).map_err(|e| e.to_string())?;
    let child = Arc::new(Child {
        status: Mutex::new(("pending_init".into(), None)),
        tasks: Mutex::new(Some(tx)),
        steer: Arc::new(Mutex::new(VecDeque::new())),
        cancelled: Arc::new(AtomicBool::new(false)),
    });
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
    mut tasks: tokio::sync::mpsc::UnboundedReceiver<Value>,
    registry: Arc<Registry>,
) -> BoxFuture<'static, ()> {
    Box::pin(async move {
        while let Some(task) = tasks.recv().await {
            session.cancelled.store(false, Ordering::SeqCst);
            *child.status.lock().unwrap() = ("running".into(), None);
            let outcome = run_turn(&mut session, Some(task), &mut |_: TurnEvent| {}).await;
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
        registry.changed.notify_waiters();
    })
}

async fn wait(handle: &AgentHandle, parent: &Session, args: &Value) -> Result<Value, String> {
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
    let timeout = Duration::from_millis(args.get("timeout_ms").and_then(Value::as_u64).unwrap_or(30_000).clamp(1_000, 600_000));
    let deadline = Instant::now() + timeout;
    // 通知可能在检查与等待之间错过，因此同时按 250ms 轮询（也用于响应主 agent 的取消）。
    while targets.iter().all(|(_, child)| child.active())
        && Instant::now() < deadline
        && !parent.cancelled.load(Ordering::SeqCst)
    {
        tokio::select! {
            _ = handle.registry.changed.notified() => {}
            _ = tokio::time::sleep(Duration::from_millis(250)) => {}
        }
    }
    let rows: Vec<Value> = targets
        .iter()
        .map(|(path, child)| {
            let (status, message) = child.status();
            json!({ "path": path, "status": status, "message": message })
        })
        .collect();
    let timed_out = targets.iter().all(|(_, child)| child.active());
    Ok(json!({ "agents": rows, "timedOut": timed_out }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lyra::turn::tests::{scripted_server, test_session, text_reply};

    #[tokio::test]
    async fn spawned_agent_completes_and_wait_reports_its_message() {
        let url = scripted_server(vec![text_reply("child done")]).await;
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
        let closed = call(&handle, &parent, "close_agent", &json!({ "target": "probe" })).await;
        assert!(!closed.is_error);
        let listed = call(&handle, &parent, "list_agents", &json!({})).await;
        assert_eq!(listed.content[0]["text"], "[]");
    }
}
