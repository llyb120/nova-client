//! polaris 的轻量模型检索实现：把 task/keywords/files 交给轻量模型，
//! 由它直接驱动 rg/read 检索；同一轮里的多个内部工具调用并发执行，
//! 命中足够即把含完整代码正文的上下文一次返回（调用方无需再读文件）。
//! 只在工具派发线程内运行，不占用主会话历史；取消、轮次与输出都有上限兜底。

use crate::lyra::config::Resolved;
use crate::lyra::history::user_message;
use crate::lyra::prompt::ShellConfig;
use crate::lyra::provider::{stream_chat, StreamEvent};
use crate::lyra::tools::Tool;
use futures_util::future::join_all;
use serde_json::{json, Value};
use sha2::Digest;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// 单条内部工具输出上限，避免一次把整个文件读爆。
const INNER_TOOL_MAX_CHARS: usize = 24_000;
/// 注入到检索模型的仓库笔记上限（越长越拖慢检索）。
const NOTES_MAX_CHARS: usize = 4_000;
const NOTES_OPEN: &str = "<!-- polaris-notes";
const NOTES_CLOSE: &str = "-->";

/// 每次检索沉淀的仓库笔记：检索模型在下一次运行前读到，越用越熟。
fn notes_path(cwd: &Path) -> std::path::PathBuf {
    let key = format!(
        "{:x}",
        sha2::Sha256::digest(cwd.to_string_lossy().as_bytes())
    );
    crate::lyra::config::nova_root()
        .join("polaris-notes")
        .join(&key[..16])
        .with_extension("md")
}

fn load_notes(cwd: &Path) -> String {
    let Ok(text) = std::fs::read_to_string(notes_path(cwd)) else {
        return String::new();
    };
    clamp(&text, NOTES_MAX_CHARS)
}

fn save_notes(cwd: &Path, notes: &str) {
    let path = notes_path(cwd);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, clamp(notes, NOTES_MAX_CHARS));
}

/// 把最终回复拆成（正文, 仓库笔记）：模型在末尾附 <!-- polaris-notes --> 块。
fn split_notes(text: &str) -> (String, Option<String>) {
    let Some(start) = text.find(NOTES_OPEN) else {
        return (text.to_string(), None);
    };
    let body = &text[start + NOTES_OPEN.len()..];
    let end = body.find(NOTES_CLOSE).map(|i| start + NOTES_OPEN.len() + i);
    let notes = match end {
        Some(end) => text[start + NOTES_OPEN.len()..end].trim().to_string(),
        None => body.trim().to_string(),
    };
    let clean = text[..start].trim_end().to_string();
    if notes.is_empty() {
        (clean, None)
    } else {
        (clean, Some(notes))
    }
}

fn inner_tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "rg",
            description: "在工作目录内用 ripgrep 检索。pattern 为正则或字面词；返回 path:line 命中（-n）。默认大小写敏感、遵守 .gitignore。同一轮可发多个独立检索并发执行。".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "检索词或正则" },
                    "path": { "type": "string", "description": "限定子路径（可选，默认整个工作目录）" },
                    "context": { "type": "integer", "description": "命中行上下文行数（-C），默认 0" },
                    "files_only": { "type": "boolean", "description": "只列出命中文件（-l）" }
                },
                "required": ["pattern"]
            }),
        },
        Tool {
            name: "read",
            description: "读取单个文件的连续行段。返回 行号|内容。默认 400 行；要读完整段函数/结构体就调大 limit。".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "offset": { "type": "integer", "minimum": 1 },
                    "limit": { "type": "integer", "minimum": 1 }
                },
                "required": ["path"]
            }),
        },
    ]
}

fn system_prompt(cwd: &Path, notes: &str) -> String {
    let memory = if notes.is_empty() {
        String::new()
    } else {
        format!("\n仓库记忆（此前检索沉淀，仅供参考，可能过期）：\n{notes}\n")
    };
    format!(
        "你是代码检索助手。用 rg 与 read 在仓库 {cwd} 内定位与任务相关的代码，\
把完整上下文带回给调用方——调用方不会再去读这些文件，所以你返回的内容必须自足。{memory}\n\
规则：\n\
- 先用 rg 召回候选，再用 read 精读把相关代码整段（函数/结构体/配置块）读全；同一轮里把互不依赖的 rg/read 一起发出并发执行。\n\
- 回复输出 Markdown：按文件分组，每处给出 `path:line` 标题和**完整代码正文**（用 ``` 代码块原样贴出，保留行号），不要用一行签名或省略号概括；必要的上下文关系用一句话说明。\n\
- 目标是让调用方仅凭你的返回就能直接动手改代码，不再回头读文件。命中足够即停止调用工具直接输出。\n\
- 回复末尾另起一行输出 {NOTES_OPEN}，换行写下值得记住的仓库结构/符号位置/入口等简短笔记（供下次检索用），再换行输出 {NOTES_CLOSE}。没有新发现就写空。\n\
- 不修改任何文件，不运行会改动状态的命令。找不到就如实说明未覆盖。",
        cwd = cwd.display()
    )
}

fn task_message(args: &Value) -> String {
    let task = args
        .get("task")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let mut keywords: Vec<String> = Vec::new();
    match args.get("keywords") {
        Some(Value::Array(values)) => {
            for value in values {
                if let Some(s) = value.as_str() {
                    if !s.trim().is_empty() {
                        keywords.push(s.trim().to_string());
                    }
                }
            }
        }
        Some(Value::String(s)) => {
            if !s.trim().is_empty() {
                keywords.push(s.trim().to_string());
            }
        }
        _ => {}
    }
    let mut files: Vec<String> = Vec::new();
    if let Some(Value::Array(values)) = args.get("files") {
        for value in values {
            if let Some(s) = value.as_str() {
                if !s.trim().is_empty() {
                    files.push(s.trim().to_string());
                }
            }
        }
    }
    let mut msg = String::new();
    if !task.is_empty() {
        msg.push_str(&format!("任务：{task}\n"));
    }
    if !keywords.is_empty() {
        msg.push_str(&format!("检索词：{}\n", keywords.join(", ")));
    }
    if !files.is_empty() {
        msg.push_str(&format!("已知相关文件：{}\n", files.join(", ")));
    }
    msg.push_str("请检索并返回与任务相关的完整代码上下文。");
    msg
}

fn clamp(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let keep = max.saturating_sub(40);
    let head: String = text.chars().take(keep).collect();
    format!("{head}\n…[truncated]")
}

/// POSIX 单引号转义，防止模型传入的 pattern/path 被 shell 二次解析。
fn sh_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

async fn run_inner_tool(
    cwd: &Path,
    name: &str,
    args: &Value,
    shell: &ShellConfig,
    cancelled: Option<Arc<AtomicBool>>,
) -> Result<String, String> {
    match name {
        "rg" => {
            let pattern = args
                .get("pattern")
                .and_then(Value::as_str)
                .ok_or("rg 缺少 pattern")?;
            if pattern.trim().is_empty() {
                return Err("rg pattern 不能为空".into());
            }
            let mut cmd = String::from("rg -n --no-heading");
            if args
                .get("files_only")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                cmd.push_str(" -l");
            }
            if let Some(c) = args.get("context").and_then(Value::as_u64) {
                if c > 0 {
                    cmd.push_str(&format!(" -C{c}"));
                }
            }
            cmd.push_str(" -- ");
            cmd.push_str(&sh_quote(pattern));
            if let Some(path) = args.get("path").and_then(Value::as_str) {
                if !path.trim().is_empty() {
                    cmd.push(' ');
                    cmd.push_str(&sh_quote(path));
                }
            }
            let (text, _code, _s, _e) =
                crate::lyra::tools::run_bash_collect(cwd, shell, &cmd, 30, cancelled).await?;
            Ok(clamp(&text, INNER_TOOL_MAX_CHARS))
        }
        "read" => {
            let path = args
                .get("path")
                .and_then(Value::as_str)
                .ok_or("read 缺少 path")?;
            let offset = args
                .get("offset")
                .and_then(Value::as_u64)
                .map(|n| n as usize);
            let limit = args
                .get("limit")
                .and_then(Value::as_u64)
                .map(|n| n as usize);
            let cwd = cwd.to_path_buf();
            let path = path.to_string();
            let parts = tokio::task::spawn_blocking(move || {
                crate::lyra::read::read(&cwd, &path, offset, limit, true)
            })
            .await
            .map_err(|e| format!("read 失败：{e}"))??;
            let text = parts
                .iter()
                .filter(|p| p.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|p| p.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n");
            Ok(clamp(&text, INNER_TOOL_MAX_CHARS))
        }
        other => Err(format!("未知工具：{other}")),
    }
}

/// 供 turn 调用：单轻量模型检索循环，同轮内部工具并发，返回整理好的上下文文本。
pub async fn run(
    cwd: &Path,
    args: &Value,
    model: &Resolved,
    http: &reqwest::Client,
    shell: &ShellConfig,
    cancelled: &Arc<AtomicBool>,
) -> Result<String, String> {
    let tools = inner_tools();
    let system = system_prompt(cwd, &load_notes(cwd));
    let mut messages = vec![user_message(&task_message(args), &[])];
    let mut final_text = String::new();

    loop {
        if cancelled.load(Ordering::SeqCst) {
            return Err("已取消".into());
        }
        let mut round_text = String::new();
        let result = stream_chat(
            http,
            &model.model,
            &model.api_key,
            Some("off"),
            &system,
            &messages,
            &tools,
            None,
            None,
            cancelled,
            &mut |event| {
                if let StreamEvent::TextDelta(delta) = event {
                    round_text.push_str(&delta);
                }
            },
        )
        .await
        .map_err(|e| e.to_string())?;

        if cancelled.load(Ordering::SeqCst) {
            return Err("已取消".into());
        }
        // 累积所有文本输出：模型可能在多轮里逐步拼出完整上下文。
        if !round_text.trim().is_empty() {
            if !final_text.is_empty() {
                final_text.push_str("\n");
            }
            final_text.push_str(&round_text);
        }

        // 收集本轮工具调用。
        let mut calls = Vec::new();
        for (index, part) in result.content.iter().enumerate() {
            if part.get("type").and_then(Value::as_str) != Some("toolCall") {
                continue;
            }
            let id = part
                .get("id")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| format!("call-{index}"));
            calls.push((
                id,
                part.get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                part.get("arguments").cloned().unwrap_or_else(|| json!({})),
            ));
        }

        messages.push(json!({
            "role": "assistant",
            "content": result.content,
            "api": model.model.api,
            "timestamp": crate::lyra::history::now_ms(),
        }));

        if calls.is_empty() {
            break;
        }

        // 同一轮里多个内部工具调用并发执行。
        let futures: Vec<_> = calls
            .iter()
            .map(|(_, name, call_args)| {
                run_inner_tool(cwd, name, call_args, shell, Some(cancelled.clone()))
            })
            .collect();
        let outputs = join_all(futures).await;
        for ((id, name, _), out) in calls.into_iter().zip(outputs) {
            let text = match out {
                Ok(t) => t,
                Err(e) => format!("错误：{e}"),
            };
            messages.push(json!({
                "role": "tool",
                "toolCallId": id,
                "toolName": name,
                "content": [{ "type": "text", "text": text }],
                "isError": false,
                "timestamp": crate::lyra::history::now_ms(),
            }));
        }
    }

    if final_text.trim().is_empty() {
        return Err("轻量模型未返回检索结果".into());
    }
    // 拆出仓库笔记沉淀到磁盘，只把正文上下文交给调用方。
    let (clean, notes) = split_notes(&final_text);
    if let Some(notes) = notes {
        save_notes(cwd, &notes);
    }
    Ok(if clean.trim().is_empty() {
        final_text
    } else {
        clean
    })
}
