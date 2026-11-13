//! 会话正常结束后，用同一个会话再请求一次来整理项目记忆。
//! 消息与下一轮用户消息同一形状：prompt_view 的字节不动（记忆前缀和已落盘的历史），
//! 末尾只加一条 user。动态段本来就在缓存前缀之外，整理指令并进这一条；再另起一条 user
//! 会在要求角色交替的端点上 400。思考档位沿用会话自己的：关掉会拒绝回放带签名的 thinking 块。
//! 指令不落盘、不进 UI。

use crate::acp::EV_MEMORY_DREAM;
use crate::lyra::context::ContextWindow;
use crate::lyra::history::{text_content, user_message};
use crate::lyra::memory::{self, distill_instruction, Snapshot};
use crate::lyra::prompt::attach_dynamic;
use crate::lyra::provider::{self, stream_chat, StreamResult};
use crate::lyra::tokens::TokenCounter;
use crate::lyra::turn::Session;
use serde_json::Value;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tauri::Emitter;

fn emit_dream(app: &tauri::AppHandle, thread_id: &str, dreaming: bool) {
    let _ = app.emit(
        EV_MEMORY_DREAM,
        serde_json::json!({ "threadId": thread_id, "dreaming": dreaming }),
    );
}

/// 会话里有可整理的文字（纯工具轮次没有）。记忆和对话都在前缀里，不用再摘录一份。
fn has_text(items: &[Value]) -> bool {
    items.iter().any(|item| {
        matches!(item.get("role").and_then(Value::as_str), Some("user" | "assistant"))
            && !text_content(&item["content"]).trim().is_empty()
    })
}

/// 与下一轮同形：视图原样，末尾只一条 user。动态段和指令都在这条里，不改视图里已经缓存的字节。
fn request_messages(
    context: &ContextWindow,
    canonical: &[Value],
    dynamic: &str,
    instruction: &str,
) -> Vec<Value> {
    let mut messages = context.prompt_view(canonical);
    let tail = if dynamic.is_empty() {
        instruction.to_string()
    } else {
        format!("{dynamic}\n\n{instruction}")
    };
    attach_dynamic(&mut messages, &tail);
    messages
}

/// 只收正常结束、且没有工具调用的正文。截断或工具调用都丢掉，半行不能进浅层。
fn accepted_distill(result: &StreamResult) -> Result<String, &str> {
    if let Some(error) = result.error_message.as_deref() {
        return Err(error);
    }
    if result.stop_reason != "stop" {
        return Err(result.stop_reason.as_str());
    }
    if result.content.iter().any(|part| part["type"] == "toolCall") {
        return Err("toolCall");
    }
    Ok(result
        .content
        .iter()
        .filter(|part| part["type"] == "text")
        .filter_map(|part| part["text"].as_str())
        .collect())
}

/// 主会话一轮结束后启动整理。失败只写日志，仍可能写入机械命中，并关掉做梦图标。
pub fn maybe_spawn(
    session: &Session,
    app: Option<&tauri::AppHandle>,
    thread_id: Option<&str>,
    http: Option<reqwest::Client>,
) {
    let Some(snapshot_path) = session
        .context
        .project_memory_snapshot_path()
        .map(Path::to_path_buf)
    else {
        return;
    };
    let hits = session.context.project_memory_session_hits();
    if !has_text(session.history.items()) && hits.is_empty() {
        return;
    }
    let evidence = memory::SessionEvidence::new(&session.session_id, hits, session.history.items());
    let candidates = Snapshot::load(&snapshot_path)
        .map(|snap| snap.candidate_index())
        .unwrap_or_default();
    let messages = request_messages(
        &session.context,
        session.history.items(),
        &session.dynamic_prompt,
        &distill_instruction(&candidates),
    );
    // 必须用会话自己的模型：前缀缓存按模型隔离，换轻量模型就整份重算，比原方案还贵。
    let (system_prompt, tools, model) = (
        session.system_prompt.clone(),
        session.tools.clone(),
        session.model.clone(),
    );
    let (session_id, cache_key, cwd) = (
        session.session_id.clone(),
        session.cache_key.clone(),
        session.cwd.clone(),
    );
    let (window, reserve) = (session.context.window(), session.context.output_reserve());
    let http = http.or_else(|| session.http.clone());
    let thread = thread_id.filter(|id| !id.is_empty()).map(str::to_string);
    let app = app.cloned();

    if let (Some(app), Some(thread)) = (app.as_ref(), thread.as_deref()) {
        emit_dream(app, thread, true);
    }

    tokio::spawn(async move {
        let http = http.unwrap_or_else(provider::http_client);
        let cancel = Arc::new(AtomicBool::new(false));
        // 档位必须和会话一致。关掉思考时，历史里的签名 thinking 块会被拒，整次命中也没了。
        let result = stream_chat(
            &http,
            &model.model,
            &model.api_key,
            model.thinking_level.as_deref(),
            &system_prompt,
            &messages,
            &tools,
            Some(&session_id),
            cache_key.as_deref(),
            &cancel,
            &mut |_| {},
        )
        .await;
        let text = match result {
            Ok(result) => match accepted_distill(&result) {
                Ok(text) => text,
                Err(why) => {
                    eprintln!("lyra: 整理项目记忆未采用（{why}）");
                    String::new()
                }
            },
            Err(error) => {
                eprintln!("lyra: 整理项目记忆失败：{error}");
                String::new()
            }
        };
        // 整理模型可以并行跑；只有写回快照时排队，后来的写入会等，不会被丢掉。
        memory::with_snapshot_write(&snapshot_path, || {
            if let Some(mut snap) = Snapshot::load(&snapshot_path) {
                let (_, _, prose, code) = memory::budgets(window, reserve);
                if snap.apply_session_end(&text, &evidence, &cwd, prose, code, &TokenCounter::default()) {
                    snap.save(&snapshot_path);
                }
            }
        });
        if let (Some(app), Some(thread)) = (app.as_ref(), thread.as_deref()) {
            emit_dream(app, thread, false);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distillation_reuses_the_session_prefix() {
        let ctx = ContextWindow::ephemeral(4096, 0);
        let history = vec![user_message("看下 src/a.rs", &[]), user_message("继续", &[])];
        let view = ctx.prompt_view(&history);
        let sent = request_messages(&ctx, &history, "Current working directory: /w", "整理");
        assert_eq!(&sent[..view.len()], &view[..], "视图里的消息一条都没被改写");
        assert_eq!(sent.len(), view.len() + 1, "尾巴只能一条，再多一条 user 就不是上一轮的前缀");
        assert_eq!(
            sent[view.len()]["content"][0]["text"],
            "Current working directory: /w\n\n整理"
        );
    }

    #[test]
    fn instruction_still_appended_when_dynamic_segment_is_empty() {
        let ctx = ContextWindow::ephemeral(4096, 0);
        let history = vec![user_message("看下 src/a.rs", &[])];
        let view = ctx.prompt_view(&history);
        let sent = request_messages(&ctx, &history, "", "整理");
        assert_eq!(&sent[..view.len()], &view[..]);
        assert_eq!(sent.len(), view.len() + 1);
        assert_eq!(sent[view.len()]["content"][0]["text"], "整理");
    }

    #[test]
    fn tool_call_or_truncation_is_not_distill() {
        let stopped = StreamResult {
            stop_reason: "stop".into(),
            content: vec![serde_json::json!({"type": "text", "text": "事实 增加 一条"})],
            ..StreamResult::default()
        };
        assert_eq!(accepted_distill(&stopped).as_deref(), Ok("事实 增加 一条"));

        let called = StreamResult {
            stop_reason: "toolUse".into(),
            content: vec![
                serde_json::json!({"type": "text", "text": "事实 增加 一条"}),
                serde_json::json!({"type": "toolCall", "name": "read"}),
            ],
            ..StreamResult::default()
        };
        assert!(accepted_distill(&called).is_err());

        let truncated = StreamResult {
            stop_reason: "length".into(),
            content: stopped.content.clone(),
            ..StreamResult::default()
        };
        assert!(accepted_distill(&truncated).is_err());
    }

    #[test]
    fn instruction_does_not_resend_the_memory_body() {
        let text = distill_instruction("- [abc123] 已记着的一条");
        assert!(text.contains("abc123") && text.contains("不要调用任何工具"));
        assert!(text.contains("开发模型没看过") && text.contains("证实 <"));
        assert!(text.contains("成功工具") && text.contains("三个不同会话"));
        // 记忆正文在上文前缀里；指令里再贴一遍就白费了这次缓存命中。
        assert!(!text.contains("<project-memory>"));
    }
}
