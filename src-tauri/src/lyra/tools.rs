//! 基础工具：read / bash / edit / write + polaris。
//! polaris 始终进程内 Rust 直连，无 Node 依赖；学习增强
//! 是锦上添花：edit 反馈/settle 发往全局 context service（不在则静默丢弃），
//! 反馈响应顺带回全局模型快照注入本地，使本地检索的 blend 排序与全局模型一致。

use crate::lyra::prompt::{
    clamp_tool_output_text, govern_tool_text, POLARIS_OUTPUT_MAX_BYTES,
    TOOL_OUTPUT_CONTEXT_MAX_BYTES,
};
use crate::lyra::{edit as native_edit, read as native_read};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

const POLARIS_DESCRIPTION: &str = "任务涉及跨文件查找或修改、或需要阅读多个文件正文时先调用：按 keywords+task+files 打包完整编辑单元、依赖和 IMPACT，并自动使用 task（缺省时回退 keywords）检索相关的猎户座经验、记忆与守则，一并返回。若返回训练知识，本轮结束前调用 feedback_memory。目标行段已明确时直接 read。";

const READ_DESCRIPTION: &str = "读取文件内容。支持 offset（起始行，1 起始）与 limit（行数）分段读取；返回 `行号|内容` 格式的带行号文本与 hasMore/nextOffset 等分段信息。";
const BASH_DESCRIPTION: &str = "在 shell 中执行命令并返回 stdout/stderr。命令在会话工作目录下运行；长任务请设置 timeout（秒，默认 120，最大 600）。禁止无排除的递归搜索（grep -r 等）。";
const EDIT_DESCRIPTION: &str = "Edit a single file using exact text replacement. Every edits[].oldText must match a unique, non-overlapping region of the original file. If two changes affect the same block or nearby lines, merge them into one edit instead of emitting overlapping edits. Do not include large unchanged regions just to connect distant changes.";
const WRITE_DESCRIPTION: &str =
    "创建或覆盖文件（自动创建父目录）。仅用于新文件或整体重写；局部修改用 edit。";

pub struct Tool {
    pub name: &'static str,
    pub description: String,
    pub parameters: Value,
}

fn schema(value: Value) -> Value {
    value
}

pub fn tool_set(
    read_only: bool,
    polaris: bool,
    memory_enabled: bool,
    auto_change_project: bool,
) -> Vec<Tool> {
    let mut tools = Vec::new();
    if auto_change_project {
        tools.push(Tool {
            name: "change_working_directory",
            description: "改变本会话后续工具调用的工作目录，并通知 Nova 切换到已有项目或自动创建项目。相对路径基于当前工作目录解析；目录必须已存在。请单独调用，不要与依赖新目录的工具并行调用。".into(),
            parameters: schema(json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "新的工作目录（相对当前工作目录或绝对路径）" }
                },
                "required": ["path"]
            })),
        });
    }
    if polaris {
        tools.push(Tool {
            name: "polaris",
            description: POLARIS_DESCRIPTION.into(),
            parameters: schema(json!({
                "type": "object",
                "properties": {
                    "keywords": {
                        "anyOf": [
                            { "type": "array", "items": { "type": "string" }, "minItems": 1 },
                            { "type": "string", "minLength": 1 }
                        ],
                        "description": "关键词或符号名；字符串自动转单项数组，超过 5 项默认取前 5 项"
                    },
                    "task": { "type": "string", "description": "一句话任务描述，用于补充检索词和排序" },
                    "files": { "type": "array", "items": { "type": "string" }, "maxItems": 6, "description": "已知必看文件，可与 keywords/task 同用" },
                    "budget": { "type": "integer", "minimum": 100, "maximum": 1200, "description": "完整代码单元行预算，默认 600" },
                    "maxBytes": { "type": "integer", "minimum": 8192, "maximum": 65536, "description": "输出硬预算，默认 32768；仅按完整文件/单元边界收敛" },
                    "coupling": { "type": "boolean", "description": "开启后附 git 共改耦合提示（近 120 次提交的高频共改文件）" }
                }
            })),
        });
    }
    tools.push(Tool {
        name: "read",
        description: READ_DESCRIPTION.into(),
        parameters: schema(json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "文件路径（相对会话工作目录或绝对路径）" },
                "offset": { "type": "integer", "minimum": 1, "description": "起始行（1 起始），默认 1" },
                "limit": { "type": "integer", "minimum": 1, "description": "读取行数，默认 2000" }
            },
            "required": ["path"]
        })),
    });
    if !read_only {
        tools.push(Tool {
            name: "bash",
            description: BASH_DESCRIPTION.into(),
            parameters: schema(json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "要执行的 shell 命令" },
                    "timeout": { "type": "integer", "description": "超时秒数，默认 120，最大 600" }
                },
                "required": ["command"]
            })),
        });
        tools.push(Tool {
            name: "edit",
            description: EDIT_DESCRIPTION.into(),
            parameters: schema(json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path to the file to edit (relative or absolute)" },
                    "edits": {
                        "type": "array",
                        "minItems": 1,
                        "description": "One or more targeted replacements. Each edit is matched against the original file, not incrementally. Do not include overlapping or nested edits. If two changes touch the same block or nearby lines, merge them into one edit instead.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "oldText": { "type": "string", "description": "Exact text for one targeted replacement. It must be unique in the original file and must not overlap with any other edits[].oldText in the same call." },
                                "newText": { "type": "string", "description": "Replacement text for this targeted edit." }
                            },
                            "required": ["oldText", "newText"]
                        }
                    }
                },
                "required": ["path", "edits"]
            })),
        });
        tools.push(Tool {
            name: "write",
            description: WRITE_DESCRIPTION.into(),
            parameters: schema(json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "content": { "type": "string" }
                },
                "required": ["path", "content"]
            })),
        });
    }

    // feedback_memory 暂时禁用：内部记账调用会打断转录的结论切分，且闭环提醒会多跑一次模型。
    let _ = memory_enabled;

    tools
}

/// 工具执行结果：内容块列表 + details（归档信息等）。
pub struct ToolOutcome {
    pub content: Vec<Value>,
    pub details: Option<Value>,
    pub is_error: bool,
}

impl ToolOutcome {
    fn text(text: impl Into<String>) -> Self {
        let text = text.into();
        ToolOutcome {
            content: if text.trim().is_empty() {
                Vec::new()
            } else {
                vec![json!({ "type": "text", "text": text })]
            },
            details: None,
            is_error: false,
        }
    }

    fn error(message: impl Into<String>) -> Self {
        ToolOutcome {
            content: vec![json!({ "type": "text", "text": message.into() })],
            details: None,
            is_error: true,
        }
    }
}

fn resolve_path(root: &Path, input: &str) -> PathBuf {
    let path = Path::new(input);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

fn text_of(value: &Value) -> String {
    value
        .get("content")
        .and_then(Value::as_array)
        .map(|parts| {
            parts
                .iter()
                .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

/// 应用 Reasonix 工具结果治理：超限归档 + 首尾截断 + OpenAI 硬上限。
fn govern(
    outcome: ToolOutcome,
    name: &str,
    call_id: &str,
    archive_dir: Option<&Path>,
) -> ToolOutcome {
    let text = text_of(&json!({ "content": outcome.content }));
    let max_bytes = if name == "polaris" {
        POLARIS_OUTPUT_MAX_BYTES
    } else {
        TOOL_OUTPUT_CONTEXT_MAX_BYTES
    };
    if text.len() <= max_bytes {
        return outcome;
    }
    let (governed, archive_path, original_bytes) =
        govern_tool_text(&text, max_bytes, archive_dir, call_id, name);
    let mut details = outcome.details.unwrap_or_else(|| json!({}));
    if let Some(path) = archive_path {
        details["archivedToolOutput"] = json!(path);
    }
    details["originalBytes"] = json!(original_bytes);
    let mut content: Vec<Value> = vec![json!({ "type": "text", "text": governed })];
    content.extend(
        outcome
            .content
            .into_iter()
            .filter(|part| part.get("type").and_then(Value::as_str) != Some("text")),
    );
    ToolOutcome {
        content,
        details: Some(details),
        is_error: outcome.is_error,
    }
}

/// 进程树守卫：bash future 被取消（abort）丢弃时，同步强杀整棵进程树，
/// 避免进程内模式下孤儿 shell 及其孙进程泄漏（等价于子进程模式的 kill 语义）。
struct PidGuard(Option<u32>);

impl PidGuard {
    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for PidGuard {
    fn drop(&mut self) {
        if let Some(pid) = self.0.take() {
            crate::acp::kill_process_tree(pid);
        }
    }
}

fn rewrite_with_embedded_rtk(command: &str, shell: &crate::lyra::prompt::ShellConfig) -> String {
    use crate::lyra::prompt::ShellKind;
    let Some(rewritten) = rtk::rewrite_command(command) else {
        return command.to_string();
    };
    let Ok(exe) = std::env::current_exe() else {
        return command.to_string();
    };
    let path = exe.to_string_lossy();
    let prefix = match shell.kind {
        ShellKind::PowerShell => format!("& '{}' __rtk ", path.replace('\'', "''")),
        ShellKind::Bash => format!("'{}' __rtk ", path.replace('\'', "'\\''")),
    };
    rewritten.replace("rtk ", &prefix)
}

async fn run_bash(
    root: &Path,
    shell: &crate::lyra::prompt::ShellConfig,
    command: &str,
    timeout_secs: u64,
    cancelled: Option<Arc<AtomicBool>>,
) -> Result<String, String> {
    use crate::lyra::prompt::ShellKind;
    let timeout_secs = timeout_secs.clamp(1, 600);
    let command = rewrite_with_embedded_rtk(command, shell);
    let mut process = match shell.kind {
        ShellKind::PowerShell => {
            let mut p = tokio::process::Command::new(&shell.program);
            p.arg("-c").arg(&command);
            p
        }
        ShellKind::Bash => {
            let mut p = tokio::process::Command::new(&shell.program);
            p.arg("-c").arg(&command);
            p
        }
    };
    process
        .current_dir(root)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .stdin(std::process::Stdio::null())
        // future 被 drop 时至少强杀直接子进程并交由运行时回收；整棵树由 PidGuard 兜底。
        .kill_on_drop(true);
    #[cfg(unix)]
    {
        // 独立进程组：超时/取消时整组 SIGKILL，孙进程一并清理（kill_process_tree 依赖此约定）。
        process.process_group(0);
    }
    #[cfg(windows)]
    {
        // 避免 Windows 下弹出控制台窗口
        use std::os::windows::process::CommandExt;
        process.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    let mut child = process
        .spawn()
        .map_err(|e| format!("启动 shell 失败：{e}"))?;
    let mut guard = PidGuard(child.id());
    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();
    let read_out = tokio::spawn(async move {
        let mut buf = Vec::new();
        if let Some(mut pipe) = stdout_pipe.take() {
            use tokio::io::AsyncReadExt;
            let _ = pipe.read_to_end(&mut buf).await;
        }
        buf
    });
    let read_err = tokio::spawn(async move {
        let mut buf = Vec::new();
        if let Some(mut pipe) = stderr_pipe.take() {
            use tokio::io::AsyncReadExt;
            let _ = pipe.read_to_end(&mut buf).await;
        }
        buf
    });
    enum Wait {
        Exited(std::io::Result<std::process::ExitStatus>),
        Cancelled,
    }
    // PI 语义：取消信号穿透到工具，bash 轮询取消标志，命中即强杀整棵进程树。
    let wait = tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), async {
        loop {
            tokio::select! {
                result = child.wait() => break Wait::Exited(result),
                _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => {
                    if cancelled
                        .as_ref()
                        .is_some_and(|flag| flag.load(Ordering::SeqCst))
                    {
                        break Wait::Cancelled;
                    }
                }
            }
        }
    })
    .await;
    let status = match wait {
        Ok(Wait::Exited(result)) => {
            guard.disarm();
            result.map_err(|e| format!("执行命令失败：{e}"))?
        }
        Ok(Wait::Cancelled) => {
            // 与超时一致的清理：显式强杀整棵进程树（含孙进程）并回收，管道随之 EOF。
            if let Some(pid) = guard.0.take() {
                crate::acp::kill_process_tree(pid);
            }
            let _ = child.wait().await;
            return Err(cancel_message(
                &read_out.await.unwrap_or_default(),
                &read_err.await.unwrap_or_default(),
            ));
        }
        Err(_) => {
            // 显式强杀整棵进程树（含孙进程）并回收，管道随之 EOF。
            if let Some(pid) = guard.0.take() {
                crate::acp::kill_process_tree(pid);
            }
            let _ = child.wait().await;
            return Err(timeout_message(
                timeout_secs,
                &read_out.await.unwrap_or_default(),
                &read_err.await.unwrap_or_default(),
            ));
        }
    };
    let output_stdout = read_out.await.unwrap_or_default();
    let output_stderr = read_err.await.unwrap_or_default();
    let mut text = String::new();
    text.push_str(&String::from_utf8_lossy(&output_stdout));
    if !output_stderr.is_empty() {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&String::from_utf8_lossy(&output_stderr));
    }
    let code = status.code().unwrap_or(-1);
    if code != 0 {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&format!("Command exited with code {code}"));
    }
    Ok(if text.trim().is_empty() {
        "(no output)".into()
    } else {
        text
    })
}

/// 超时错误：附带已产生的部分输出，便于模型判断现场。
fn timeout_message(timeout_secs: u64, stdout: &[u8], stderr: &[u8]) -> String {
    let mut text = String::new();
    text.push_str(&String::from_utf8_lossy(stdout));
    if !stderr.is_empty() {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&String::from_utf8_lossy(stderr));
    }
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(&format!("命令超过 {timeout_secs}s 未结束，已终止"));
    text
}

/// 取消错误：附带已产生的部分输出，便于模型判断现场（与超时一致）。
fn cancel_message(stdout: &[u8], stderr: &[u8]) -> String {
    let mut text = String::new();
    text.push_str(&String::from_utf8_lossy(stdout));
    if !stderr.is_empty() {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&String::from_utf8_lossy(stderr));
    }
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str("命令已被取消，已终止");
    text
}

pub async fn execute(
    root: &Path,
    name: &str,
    args: &Value,
    shell: Option<&crate::lyra::prompt::ShellConfig>,
    archive_dir: Option<&Path>,
    call_id: &str,
    cancelled: Option<&Arc<AtomicBool>>,
) -> ToolOutcome {
    let outcome = execute_inner(root, name, args, shell, cancelled).await;
    govern(outcome, name, call_id, archive_dir)
}

async fn execute_inner(
    root: &Path,
    name: &str,
    args: &Value,
    shell: Option<&crate::lyra::prompt::ShellConfig>,
    cancelled: Option<&Arc<AtomicBool>>,
) -> ToolOutcome {
    match name {
        "polaris" => {
            let code_root = root.to_path_buf();
            let memory_root = root.to_path_buf();
            let mut args = args.clone();
            if let Some(object) = args.as_object_mut() {
                let raw = object.get("keywords").cloned().unwrap_or(Value::Null);
                let values = match raw {
                    Value::Array(values) => values,
                    Value::String(value) => vec![Value::String(value)],
                    _ => Vec::new(),
                };
                let mut seen = std::collections::HashSet::new();
                let keywords = values
                    .into_iter()
                    .filter_map(|value| value.as_str().map(str::trim).map(str::to_string))
                    .filter(|value| !value.is_empty() && seen.insert(value.to_lowercase()))
                    .take(5)
                    .map(Value::String)
                    .collect();
                object.insert("keywords".into(), Value::Array(keywords));
            }
            let memory_query = args
                .get("task")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .or_else(|| {
                    args.get("keywords")
                        .and_then(Value::as_array)
                        .map(|values| {
                            values
                                .iter()
                                .filter_map(Value::as_str)
                                .collect::<Vec<_>>()
                                .join(" ")
                        })
                })
                .unwrap_or_default();
            // 代码上下文与训练知识是独立数据源，同轮并行，附加召回不会串行拖慢 polaris。
            let code_job = tokio::task::spawn_blocking(move || {
                crate::nova_tools_native::context::polaris(&code_root, args)
            });
            let memory_job = tokio::task::spawn_blocking(move || {
                let enabled = crate::settings::Settings::load(&crate::lyra::config::nova_root())
                    .experience_training_enabled;
                if !enabled || memory_query.is_empty() {
                    None
                } else {
                    crate::experience::load_trained_memory(
                        &memory_root.to_string_lossy(),
                        &memory_query,
                        8,
                    )
                    .ok()
                }
            });
            let (code_result, memory_result) = tokio::join!(code_job, memory_job);
            match code_result {
                Ok(Ok(text)) => {
                    let mut text = clamp_tool_output_text(&text);
                    let memory = memory_result.ok().flatten();
                    if let Some(rows) = memory
                        .as_ref()
                        .and_then(|value| value.get("experiences"))
                        .and_then(Value::as_array)
                        .filter(|rows| !rows.is_empty())
                    {
                        let rendered = rows
                            .iter()
                            .map(|item| {
                                let id = item.get("id").and_then(Value::as_str).unwrap_or("");
                                let kind = item
                                    .get("kind")
                                    .and_then(Value::as_str)
                                    .unwrap_or("experience");
                                let knowledge_scope = item
                                    .get("knowledgeScope")
                                    .and_then(Value::as_str)
                                    .unwrap_or("project");
                                let scope_label = if knowledge_scope == "universal" {
                                    "泛用"
                                } else {
                                    "项目独有"
                                };
                                let trigger =
                                    item.get("trigger").and_then(Value::as_str).unwrap_or("");
                                let action =
                                    item.get("action").and_then(Value::as_str).unwrap_or("");
                                format!(
                                    "- [{scope_label}/{kind}] id={id} 条件/上下文：{trigger}\n  内容：{action}"
                                )
                            })
                            .collect::<Vec<_>>()
                            .join("\n");
                        let activated = memory
                            .as_ref()
                            .and_then(|value| value.get("activatedExperts"))
                            .cloned()
                            .unwrap_or_else(|| json!([]));
                        let project_root = memory
                            .as_ref()
                            .and_then(|value| value.get("projectRoot"))
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        text.push_str(&format!("\n\n# TRAINED KNOWLEDGE\nprojectRoot={project_root}\nactivatedExperts={activated}\n{rendered}\n# FEEDBACK REQUIRED\n最终回复前调用 feedback_memory；采用并验证用 ±1，未采用或无法验证用 0。"));
                    }
                    ToolOutcome::text(text)
                }
                Ok(Err(error)) => ToolOutcome::error(error),
                Err(e) => ToolOutcome::error(format!("polaris 执行失败：{e}")),
            }
        }
        "read" => {
            let Some(path) = args.get("path").and_then(Value::as_str) else {
                return ToolOutcome::error("read 缺少 path");
            };
            let offset = args
                .get("offset")
                .and_then(Value::as_u64)
                .map(|n| n as usize);
            let limit = args
                .get("limit")
                .and_then(Value::as_u64)
                .map(|n| n as usize);
            let root = root.to_path_buf();
            let path = path.to_string();
            let numbered = true;
            match tokio::task::spawn_blocking(move || {
                native_read::read(&root, &path, offset, limit, numbered)
            })
            .await
            {
                Ok(Ok(content)) => ToolOutcome {
                    content,
                    details: None,
                    is_error: false,
                },
                Ok(Err(error)) => ToolOutcome::error(error),
                Err(e) => ToolOutcome::error(format!("read 执行失败：{e}")),
            }
        }
        "bash" => {
            let Some(command) = args.get("command").and_then(Value::as_str) else {
                return ToolOutcome::error("bash 缺少 command");
            };
            let Some(shell) = shell else {
                return ToolOutcome::error("当前为只读模式，bash 不可用");
            };
            let timeout = args.get("timeout").and_then(Value::as_u64).unwrap_or(120);
            match run_bash(root, shell, command, timeout, cancelled.cloned()).await {
                Ok(text) => ToolOutcome::text(clamp_tool_output_text(&text)),
                Err(error) => ToolOutcome::error(error),
            }
        }
        "edit" => {
            let Some(path) = args.get("path").and_then(Value::as_str) else {
                return ToolOutcome::error("edit 缺少 path");
            };
            let Some(edits) = args.get("edits").and_then(Value::as_array) else {
                return ToolOutcome::error("edit 缺少 edits");
            };
            for edit in edits {
                if edit.get("oldText").and_then(Value::as_str).is_none()
                    || edit.get("newText").and_then(Value::as_str).is_none()
                {
                    return ToolOutcome::error("edit 的每项都需要 oldText 与 newText");
                }
            }
            let root_buf = root.to_path_buf();
            let path = path.to_string();
            let edits = Value::Array(edits.clone());
            let result =
                tokio::task::spawn_blocking(move || native_edit::edit(&root_buf, &path, edits))
                    .await;
            match result {
                Ok(Ok(value)) => {
                    let message = value
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("编辑完成");
                    let mut outcome = ToolOutcome::text(message);
                    outcome.details = value.get("details").cloned();
                    outcome
                }
                Ok(Err(error)) => ToolOutcome::error(error),
                Err(e) => ToolOutcome::error(format!("edit 执行失败：{e}")),
            }
        }
        "write" => {
            let Some(path) = args.get("path").and_then(Value::as_str) else {
                return ToolOutcome::error("write 缺少 path");
            };
            let content = args
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let target = resolve_path(root, path);
            if let Some(parent) = target.parent() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    return ToolOutcome::error(format!("创建目录失败：{e}"));
                }
            }
            match std::fs::write(&target, content) {
                Ok(()) => ToolOutcome::text(format!("已写入 {}（{} 字节）", path, content.len())),
                Err(e) => ToolOutcome::error(format!("写入 {path} 失败：{e}")),
            }
        }
        "change_working_directory" => {
            let Some(path) = args.get("path").and_then(Value::as_str).map(str::trim) else {
                return ToolOutcome::error("change_working_directory 缺少 path");
            };
            if path.is_empty() {
                return ToolOutcome::error("change_working_directory 缺少 path");
            }
            let next = resolve_path(root, path);
            match std::fs::metadata(&next) {
                Ok(metadata) if metadata.is_dir() => ToolOutcome {
                    content: vec![
                        json!({ "type": "text", "text": format!("Current working directory: {}", next.display()) }),
                    ],
                    details: Some(json!({ "workingDirectory": next })),
                    is_error: false,
                },
                _ => ToolOutcome::error(format!("工作目录不存在或不是目录：{}", next.display())),
            }
        }
        "feedback_memory" => ToolOutcome::error("feedback_memory 已暂时禁用"),
        other => ToolOutcome::error(format!("未知工具：{other}")),
    }
}

#[cfg(test)]
mod embedded_rtk_tests {
    use super::rewrite_with_embedded_rtk;
    use crate::lyra::prompt::{ShellConfig, ShellKind};

    #[test]
    fn rewrites_supported_commands_without_external_rtk_binary() {
        let shell = ShellConfig {
            program: if cfg!(windows) {
                "powershell.exe"
            } else {
                "bash"
            }
            .into(),
            kind: if cfg!(windows) {
                ShellKind::PowerShell
            } else {
                ShellKind::Bash
            },
        };
        let rewritten = rewrite_with_embedded_rtk("git status", &shell);
        assert!(rewritten.contains("__rtk git status"), "{rewritten}");
        assert!(!rewritten.starts_with("rtk "), "{rewritten}");
        for unsupported in [
            "echo hello",
            "git rev-parse HEAD",
            "go version",
            "npm install",
        ] {
            assert_eq!(rewrite_with_embedded_rtk(unsupported, &shell), unsupported);
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::run_bash;
    use crate::lyra::prompt::{ShellConfig, ShellKind};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    fn shell() -> ShellConfig {
        ShellConfig {
            program: "bash".into(),
            kind: ShellKind::Bash,
        }
    }

    fn pid_alive(pid: u32) -> bool {
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }

    fn temp_case_dir(tag: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("nova-lyra-bash-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn wait_pid_exit(pid: u32) -> bool {
        for _ in 0..100 {
            if !pid_alive(pid) {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        false
    }

    /// 超时：整棵进程树（含孙进程）必须被强杀，不能留孤儿。
    #[tokio::test]
    async fn bash_timeout_kills_process_tree() {
        let dir = temp_case_dir("timeout");
        let err = run_bash(
            &dir,
            &shell(),
            "sleep 300 & echo $! > child.pid; wait",
            1,
            None,
        )
        .await
        .expect_err("必须超时");
        assert!(err.contains("已终止"), "err={err}");
        let grandchild: u32 = std::fs::read_to_string(dir.join("child.pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert!(
            wait_pid_exit(grandchild),
            "超时后孙进程 {grandchild} 未被清理"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 取消（abort 丢弃 future）：等价于子进程模式的 kill，整组清理。
    #[tokio::test]
    async fn bash_future_drop_kills_process_tree() {
        let dir = temp_case_dir("drop");
        let work_dir = dir.clone();
        let task = tokio::spawn(async move {
            run_bash(
                &work_dir,
                &shell(),
                "sleep 300 & echo $! > child.pid; wait",
                600,
                None,
            )
            .await
        });
        // 等孙进程起来
        let mut grandchild = None;
        for _ in 0..200 {
            if let Ok(text) = std::fs::read_to_string(dir.join("child.pid")) {
                if let Ok(pid) = text.trim().parse::<u32>() {
                    grandchild = Some(pid);
                    break;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let grandchild = grandchild.expect("孙进程未启动");
        assert!(pid_alive(grandchild));

        task.abort();
        let _ = task.await;

        assert!(
            wait_pid_exit(grandchild),
            "取消后孙进程 {grandchild} 未被清理"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 取消标志：运行中置位后 bash 必须尽快退出并清理整棵进程树（PI 的 signal 语义）。
    #[tokio::test]
    async fn bash_cancel_flag_kills_process_tree() {
        let dir = temp_case_dir("cancel");
        let flag = Arc::new(AtomicBool::new(false));
        let work_dir = dir.clone();
        let cancel = flag.clone();
        let task = tokio::spawn(async move {
            run_bash(
                &work_dir,
                &shell(),
                "sleep 300 & echo $! > child.pid; wait",
                600,
                Some(cancel),
            )
            .await
        });
        // 等孙进程起来
        let mut grandchild = None;
        for _ in 0..200 {
            if let Ok(text) = std::fs::read_to_string(dir.join("child.pid")) {
                if let Ok(pid) = text.trim().parse::<u32>() {
                    grandchild = Some(pid);
                    break;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let grandchild = grandchild.expect("孙进程未启动");
        assert!(pid_alive(grandchild));

        flag.store(true, Ordering::SeqCst);
        let err = task
            .await
            .expect("任务 panic")
            .expect_err("取消后必须返回错误");
        assert!(err.contains("已被取消"), "err={err}");
        assert!(
            wait_pid_exit(grandchild),
            "取消后孙进程 {grandchild} 未被清理"
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
