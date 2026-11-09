//! 并行子 agent（Reasonix `spawn_agents` 的批量模型 + 结果摘要契约）：
//! 一次调用带 1-8 个互不依赖的任务，各自在**全新**子会话里并行跑完，父会话只拿回
//! 标题、状态、答案开头与引用；完整答案用 `read_subagent_result` 按字节分页读取。
//! 子 agent 看不到本对话，也不能再生子 agent；执行过程投影到只读 Stage 供回看。

use crate::lyra::bridge::{turn_event_sink, Emit};
use crate::lyra::context::ContextWindow;
use crate::lyra::history::{text_content, user_message, History};
use crate::lyra::stage::Stage;
use crate::lyra::tools::{Tool, ToolOutcome};
use crate::lyra::turn::{run_turn, text_outcome, Session};
use futures_util::future::join_all;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use tokio::sync::Semaphore;

/// 单次调用最多派出的任务数。
const MAX_TASKS: usize = 8;
/// 同时运行的子 agent 上限（跨调用共享）。
const MAX_ACTIVE: usize = 6;
/// 全部任务共享的开头预算；按任务数均分后夹在这个区间里。
const HEAD_BUDGET: usize = 8000;
const HEAD_MIN: usize = 600;
const HEAD_MAX: usize = 2000;
/// 归档只保留最近这么多份完整答案，避免长会话吃内存。
const RESULT_KEEP: usize = 32;
const RESULT_DEFAULT_BYTES: usize = 12 * 1024;
const RESULT_MAX_BYTES: usize = 24 * 1024;

/// 只在这些模型上开放子 agent（模型 id 子串，大小写不敏感）：小模型开并行只是多花 token，
/// 换不来更好的探索。默认值可在设置页「Lyra → 子 Agent」改。
/// 模型 id 命中任一条规则才带上 `spawn_agents`；一条规则里的多个词（空格分隔）必须同时命中。
/// 每个词支持 `*` 通配（任意字符，含空），例：`gpt-5* *codex`、`deepseek-*-pro`。
/// 规则由用户在设置页「Lyra → 子 Agent」里填，默认空 = 全不启用。
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

#[derive(Default)]
struct Registry {
    /// 正在运行的子任务取消开关；用户取消父轮时一次性置位。
    running: Vec<Arc<AtomicBool>>,
    /// 归档的完整答案（ref, 文本），按插入顺序保留最近 RESULT_KEEP 份。
    results: Vec<(String, String)>,
    seq: u64,
}

/// 主 agent 持有的控制面句柄（每个 Nova 会话一份）。
#[derive(Clone)]
pub struct AgentHandle {
    registry: Arc<Mutex<Registry>>,
    stage_parent: Option<(tauri::AppHandle, String)>,
}

static REGISTRIES: OnceLock<Mutex<std::collections::HashMap<String, Arc<Mutex<Registry>>>>> = OnceLock::new();

/// 用户取消父轮时终止所有正在运行的子任务。
pub(crate) fn stop_for_thread(thread_id: &str) {
    root_handle(&format!("thread:{thread_id}")).cancel_all();
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

const AGENT_TOOLS: [&str; 2] = ["spawn_agents", "read_subagent_result"];

pub fn is_agent_tool(name: &str) -> bool {
    AGENT_TOOLS.contains(&name)
}

pub fn agent_tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "spawn_agents",
            description: format!(
                "Run independent tasks in parallel, each in its own fresh session that does not see this conversation. A sub-agent has your tools but cannot spawn further agents. Give each task a short title and a self-contained task. Every answer is archived; you get back its title, status, a reference, the opening of its answer, and the files it read and wrote. When an answer is cut off, read the rest with read_subagent_result using the reference. Do not delegate reading a file you are about to edit: read it yourself. At most {MAX_TASKS} tasks per call and {MAX_ACTIVE} running at once."
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "tasks": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": MAX_TASKS,
                        "description": "Independent tasks to run at the same time.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "title": { "type": "string", "description": "Short label for the task, shown to the user." },
                                "task": { "type": "string", "description": "What to find out. Be specific about the evidence or summary to return." }
                            },
                            "required": ["title", "task"]
                        }
                    }
                },
                "required": ["tasks"]
            }),
        },
        Tool {
            name: "read_subagent_result",
            description: "Read a completed sub-agent's full final answer by the reference returned from spawn_agents. Answers are paged by UTF-8 byte offset so a large answer stays lossless without overflowing one tool result.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "ref": { "type": "string", "description": "The sa_... value from a spawn_agents result." },
                    "offset_bytes": { "type": "integer", "minimum": 0, "description": "UTF-8 byte offset to start from. Omit for the beginning; copy next_offset_bytes from the previous page (any value is rounded down to a character boundary)." },
                    "limit_bytes": { "type": "integer", "minimum": 1, "maximum": RESULT_MAX_BYTES, "description": format!("Maximum UTF-8 bytes to return. Defaults to {RESULT_DEFAULT_BYTES} and is capped at {RESULT_MAX_BYTES}.") }
                },
                "required": ["ref"]
            }),
        },
    ]
}

impl AgentHandle {
    pub fn cancel_all(&self) {
        for cancel in &self.registry.lock().unwrap().running {
            cancel.store(true, Ordering::SeqCst);
        }
    }

    pub(super) fn with_stage(mut self, parent: Option<(tauri::AppHandle, String)>) -> Self {
        if let Some((_, thread_id)) = &parent {
            REGISTRIES.get_or_init(Default::default).lock().unwrap()
                .insert(format!("thread:{thread_id}"), self.registry.clone());
        }
        self.stage_parent = parent;
        self
    }

    fn track(&self, cancel: &Arc<AtomicBool>) {
        self.registry.lock().unwrap().running.push(cancel.clone());
    }

    fn untrack(&self, cancel: &Arc<AtomicBool>) {
        self.registry.lock().unwrap().running.retain(|item| !Arc::ptr_eq(item, cancel));
    }

    /// 完整答案入库并返回引用；只保留最近若干份。
    fn archive(&self, answer: String) -> String {
        let mut registry = self.registry.lock().unwrap();
        registry.seq += 1;
        let reference = format!("sa_{}", registry.seq);
        registry.results.push((reference.clone(), answer));
        let overflow = registry.results.len().saturating_sub(RESULT_KEEP);
        registry.results.drain(..overflow);
        reference
    }

    fn result(&self, reference: &str) -> Option<String> {
        self.registry.lock().unwrap().results.iter()
            .find(|(key, _)| key == reference)
            .map(|(_, answer)| answer.clone())
    }
}

pub async fn call(parent: &Session, name: &str, args: &Value) -> ToolOutcome {
    let Some(handle) = parent.agents.clone() else {
        return text_outcome(format!("{name} 仅在主 agent 中可用"), true);
    };
    let result = match name {
        "spawn_agents" => spawn(handle, parent, args).await,
        "read_subagent_result" => read_result(handle, args),
        _ => return text_outcome(format!("unknown agent tool: {name}"), true),
    };
    match result {
        Ok(value) => text_outcome(value, false),
        Err(error) => text_outcome(error, true),
    }
}

fn read_result(handle: AgentHandle, args: &Value) -> Result<String, String> {
    let reference = args.get("ref").and_then(Value::as_str).unwrap_or_default().trim();
    if reference.is_empty() {
        return Err("ref 必填".into());
    }
    let answer = handle.result(reference)
        .ok_or_else(|| format!("没有 {reference} 的结果；它可能来自更早的会话或已被淘汰"))?;
    let requested = args.get("offset_bytes").and_then(Value::as_u64).unwrap_or(0) as usize;
    let limit = args.get("limit_bytes").and_then(Value::as_u64)
        .map(|value| value as usize)
        .unwrap_or(RESULT_DEFAULT_BYTES)
        .clamp(1, RESULT_MAX_BYTES);
    // 模型常把字符数/估算值当字节偏移传：静默对齐到最近的字符边界，不因分页失败。
    let mut offset = requested.min(answer.len());
    while offset > 0 && !answer.is_char_boundary(offset) {
        offset -= 1;
    }
    if offset >= answer.len() {
        return Ok(format!("{reference} 已读完（共 {} 字节）", answer.len()));
    }
    let mut end = (offset + limit).min(answer.len());
    while end > offset && end < answer.len() && !answer.is_char_boundary(end) {
        end -= 1;
    }
    let mut text = format!("{reference}（共 {} 字节，本次 {offset}-{end}）：\n{}", answer.len(), &answer[offset..end]);
    if end < answer.len() {
        text.push_str(&format!("\n\n还有剩余：next_offset_bytes={end}"));
    }
    Ok(text)
}

async fn spawn(handle: AgentHandle, parent: &Session, args: &Value) -> Result<String, String> {
    let tasks = args.get("tasks").and_then(Value::as_array)
        .ok_or("tasks 必须是任务数组")?;
    if tasks.is_empty() || tasks.len() > MAX_TASKS {
        return Err(format!("tasks 需要 1-{MAX_TASKS} 项；整批未执行"));
    }
    let mut specs = Vec::with_capacity(tasks.len());
    for (index, task) in tasks.iter().enumerate() {
        let title = task.get("title").and_then(Value::as_str).unwrap_or_default().trim();
        let text = task.get("task").and_then(Value::as_str).unwrap_or_default().trim();
        if text.is_empty() {
            return Err(format!("第 {} 项没有 task 文本；整批未执行", index + 1));
        }
        specs.push((title.chars().take(80).collect::<String>(), text.to_string()));
    }
    let head = (HEAD_BUDGET / specs.len()).clamp(HEAD_MIN, HEAD_MAX);
    let semaphore = Arc::new(Semaphore::new(MAX_ACTIVE));
    let jobs: Vec<_> = specs.into_iter().enumerate().map(|(index, (title, task))| {
        let cancel = Arc::new(AtomicBool::new(false));
        handle.track(&cancel);
        let stage = handle.stage_parent.as_ref().and_then(|(app, parent_id)| {
            Stage::new(app, parent_id, &title, parent.cwd.to_string_lossy().into_owned(), parent.model.model.id.clone())
        });
        let child = child_session(parent, cancel);
        let semaphore = semaphore.clone();
        let tracker = handle.clone();
        async move {
            let mut child = match child {
                Ok(child) => child,
                Err(error) => return (index, title, Err(error), Vec::new(), Vec::new()),
            };
            let permit = match semaphore.acquire_owned().await {
                Ok(permit) => permit,
                Err(error) => {
                    tracker.untrack(&child.cancelled);
                    return (index, title, Err(format!("并发名额不可用：{error}")), Vec::new(), Vec::new());
                }
            };
            if let Some(stage) = &stage {
                stage.begin(&json!({ "content": [{ "type": "text", "text": task }] }));
            }
            let emit: Emit = stage.as_ref().map(Stage::emitter).unwrap_or_else(|| Arc::new(|_| {}));
            let mut usage = json!({});
            let session_id = child.session_id.clone();
            let mut on_event = turn_event_sink(&emit, &session_id, &mut usage);
            let outcome = run_turn(&mut child, Some(user_message(&task, &[])), &mut on_event).await;
            drop(on_event);
            if let Some(stage) = &stage {
                stage.finish(&outcome, usage);
            }
            let answer = child.history.items().iter().rev()
                .find(|message| message.get("role").and_then(Value::as_str) == Some("assistant"))
                .map(|message| text_content(&message["content"]).trim().to_string())
                .filter(|text| !text.is_empty());
            let (read, wrote) = touched_paths(child.history.items());
            tracker.untrack(&child.cancelled);
            drop(permit);
            let result = match (outcome.cancelled, outcome.error) {
                (true, _) => Err("已取消".to_string()),
                (_, Some(error)) => Err(error),
                _ => answer.ok_or_else(|| "子 agent 没有给出最终回答".to_string()),
            };
            (index, title, result, read, wrote)
        }
    }).collect();
    let finished = join_all(jobs).await;
    let mut rows = Vec::with_capacity(finished.len());
    for (index, title, result, read, wrote) in finished {
        let mut row = json!({ "title": title, "index": index + 1, "read": read, "wrote": wrote });
        match result {
            Ok(answer) => {
                let chars = answer.chars().count();
                row["status"] = json!("completed");
                row["chars"] = json!(chars);
                row["head"] = json!(truncate(&answer, head));
                row["more"] = json!(chars > head);
                row["ref"] = json!(handle.archive(answer));
            }
            Err(error) => {
                row["status"] = json!("failed");
                row["error"] = json!(error);
            }
        }
        rows.push(row);
    }
    Ok(json!({ "agents": rows }).to_string())
}

fn truncate(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}

/// 子 agent 只继承当前授权快照：模型、工具、工作目录、MCP 配置与系统提示都来自父会话，
/// 历史为空（看不到本对话），上下文账本只在内存里。
fn child_session(parent: &Session, cancelled: Arc<AtomicBool>) -> Result<Session, String> {
    let tools: Vec<Tool> = parent.tools.iter()
        .filter(|tool| !is_agent_tool(tool.name) && tool.name != crate::lyra::scene::TOOL)
        .map(|tool| Tool { name: tool.name, description: tool.description.clone(), parameters: tool.parameters.clone() })
        .collect();
    let system_prompt = format!(
        "{}\n\n你是主会话派出的子 agent：只完成本次任务并给出结论，不要向用户提问；你看不到主对话，也不能再派生子 agent。需要主会话知道的证据写进最终回答。",
        parent.system_prompt.replace(crate::lyra::prompt::SUBAGENT_GUIDE, "")
    );
    let mut context = ContextWindow::ephemeral(parent.context.window(), parent.context.overhead());
    let schema = Value::Array(tools.iter().map(|tool| json!({
        "name": tool.name, "description": tool.description, "parameters": tool.parameters
    })).collect());
    context.configure_tokens(Default::default(), &system_prompt, &schema, parent.model.model.max_output_tokens);
    let session_id = uuid::Uuid::new_v4().to_string();
    let mcp = tokio::sync::Mutex::new(crate::lyra::mcp::McpSession::new(
        parent.mcp_config.clone(), parent.cwd.clone(), parent.read_only)?);
    Ok(Session {
        http: parent.http.clone(),
        model: parent.model.clone(),
        summarizer: parent.summarizer.clone(),
        system_prompt,
        dynamic_prompt: parent.dynamic_prompt.clone(),
        tools,
        read_only: parent.read_only,
        mcp_config: parent.mcp_config.clone(),
        mcp,
        cwd: parent.cwd.clone(),
        session_id: session_id.clone(),
        sessions_root: None,
        archive_dir: parent.archive_dir.as_ref().map(|root| root.join("agents").join(&session_id)),
        shell: parent.shell.clone(),
        history: History::new(Vec::new(), None),
        context,
        cancelled,
        steer: Arc::new(Mutex::new(VecDeque::new())),
        agents: None,
        scenes: None,
    })
}

/// 子 agent 读过/写过的文件，让主会话直接打开确切的那几个。
fn touched_paths(items: &[Value]) -> (Vec<String>, Vec<String>) {
    const MAX: usize = 40;
    let mut read = Vec::new();
    let mut wrote = Vec::new();
    for message in items {
        let Some(parts) = message.get("content").and_then(Value::as_array) else { continue };
        for part in parts {
            if part.get("type").and_then(Value::as_str) != Some("toolCall") {
                continue;
            }
            let name = part.get("name").and_then(Value::as_str).unwrap_or_default();
            let path = part.get("arguments").and_then(|args| args.get("path")).and_then(Value::as_str)
                .unwrap_or_default().trim();
            if path.is_empty() {
                continue;
            }
            let list = match name {
                "read" => &mut read,
                "edit" | "write" => &mut wrote,
                _ => continue,
            };
            if list.len() < MAX && !list.iter().any(|seen| seen == path) {
                list.push(path.to_string());
            }
        }
    }
    (read, wrote)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lyra::turn::tests::{scripted_server, test_session, text_reply};

    fn handle() -> AgentHandle {
        AgentHandle { registry: Arc::new(Mutex::new(Registry::default())), stage_parent: None }
    }

    #[test]
    fn only_configured_rules_open_subagents() {
        let keywords = vec!["*gpt-5*".to_string(), "*claude-opus*".to_string(), "deepseek-*-pro".to_string()];
        assert!(enabled("gpt-5.1-codex", &keywords));
        assert!(enabled("claude-opus-5", &keywords));
        assert!(enabled("deepseek-v4-pro", &keywords));
        assert!(!enabled("deepseek-v4-flash", &keywords));
        assert!(!enabled("gemini-3-pro", &keywords));
        // 默认空规则：谁都不开。
        // 大小写不敏感；空白规则忽略；空列表全不启用；同一行多个词必须同时命中；`*` 为通配。
        assert!(enabled("GPT-5", &[" gpt-5 ".into()]));
        assert!(enabled("gpt-5.1-codex", &["gpt-5* *codex".into()]));
        assert!(enabled("gpt-5.1-codex", &["*-codex".into()]));
        assert!(!enabled("gpt-5.1", &["gpt-5* *codex".into()]));
        assert!(!enabled("gpt-5.1-codex", &["gpt-5* *opus".into()]));
        assert!(enabled("deepseek-v4-pro", &["deepseek-*-pro".into()]));
        assert!(!enabled("deepseek-v4-flash", &["deepseek-*-pro".into()]));
        assert!(enabled("gpt-5.1", &["gpt-5* *codex".into(), "gpt-5*".into()]));
        assert!(!enabled("gpt-5", &["  ".into()]));
        assert!(!enabled("gpt-5", &[]));
        assert!(!enabled("gpt-5", &["*codex*".into()]));
    }

    #[test]
    fn archived_results_page_on_char_boundaries_and_stay_bounded() {
        let handle = handle();
        let long = "中文".repeat(RESULT_KEEP + 1);
        let reference = handle.archive(long.clone());
        assert_eq!(handle.result(&reference).as_deref(), Some(long.as_str()));
        for _ in 0..RESULT_KEEP + 4 {
            handle.archive("x".into());
        }
        assert!(handle.result(&reference).is_none(), "old answers are evicted");
        let reference = handle.archive("中文中文".into());
        // 落在字符中间或超出末尾都不报错：对齐到边界 / 直接告知已读完。
        let page = read_result(handle.clone(), &json!({ "ref": reference, "offset_bytes": 1 })).unwrap();
        assert!(page.contains("本次 0-"), "{page}");
        let page = read_result(handle.clone(), &json!({ "ref": reference, "limit_bytes": 6 })).unwrap();
        assert!(page.contains("中文") && page.contains("next_offset_bytes=6"), "{page}");
        let page = read_result(handle, &json!({ "ref": reference, "offset_bytes": 9999 })).unwrap();
        assert!(page.contains("已读完"), "{page}");
    }

    #[tokio::test]
    async fn spawn_agents_runs_tasks_and_returns_heads_with_refs() {
        let url = scripted_server(vec![text_reply("first answer"), text_reply("second answer")]).await;
        let mut parent = test_session(url, std::env::temp_dir());
        parent.agents = Some(handle());
        let outcome = call(&parent, "spawn_agents", &json!({
            "tasks": [{ "title": "one", "task": "do one" }, { "title": "two", "task": "do two" }]
        })).await;
        assert!(!outcome.is_error, "{:?}", outcome.content);
        let text = outcome.content[0]["text"].as_str().unwrap();
        assert!(text.contains("first answer") && text.contains("second answer"), "{text}");
        let value: Value = serde_json::from_str(text).unwrap();
        let rows = value["agents"].as_array().unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|row| row["status"] == "completed" && row["ref"].as_str().is_some_and(|r| r.starts_with("sa_"))));
    }

    #[tokio::test]
    async fn invalid_batches_start_nothing() {
        let parent = {
            let mut parent = test_session("http://unused".into(), std::env::temp_dir());
            parent.agents = Some(handle());
            parent
        };
        let empty = call(&parent, "spawn_agents", &json!({ "tasks": [] })).await;
        assert!(empty.is_error);
        let blank = call(&parent, "spawn_agents", &json!({ "tasks": [{ "title": "x", "task": "  " }] })).await;
        assert!(blank.is_error);
        let unknown = call(&parent, "read_subagent_result", &json!({ "ref": "sa_999" })).await;
        assert!(unknown.is_error);
    }
}
