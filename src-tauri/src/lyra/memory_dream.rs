//! 会话正常结束后，后台用轻量模型整理项目记忆。
//! 用户看不见这次整理，Butterfly 也不提示成败。

use crate::acp::EV_MEMORY_DREAM;
use crate::lyra::history::text_content;
use crate::lyra::memory::{self, distill_prompt, Snapshot, ACK, ACK_RECENT};
use crate::lyra::tokens::TokenCounter;
use crate::lyra::turn::Session;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use tauri::Emitter;

const TRANSCRIPT_CAP: usize = 12_000;

type PathLock = Arc<tokio::sync::Mutex<()>>;

fn lock_for(path: &Path) -> PathLock {
    static LOCKS: OnceLock<Mutex<HashMap<PathBuf, PathLock>>> = OnceLock::new();
    let mut map = LOCKS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .expect("memory dream locks");
    map.entry(path.to_path_buf())
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

fn emit_dream(app: &tauri::AppHandle, thread_id: &str, dreaming: bool) {
    let _ = app.emit(
        EV_MEMORY_DREAM,
        json!({ "threadId": thread_id, "dreaming": dreaming }),
    );
}

fn transcript_excerpt(items: &[Value], cap: usize) -> String {
    let mut parts = Vec::new();
    for item in items {
        let role = item.get("role").and_then(Value::as_str).unwrap_or("");
        if role != "user" && role != "assistant" {
            continue;
        }
        let text = text_content(&item["content"]);
        let text = text.trim();
        if text.is_empty() || text.contains("<project-memory") || text == ACK || text == ACK_RECENT
        {
            continue;
        }
        parts.push(format!("{role}: {text}"));
    }
    let out = parts.join("\n\n");
    let count = out.chars().count();
    if count <= cap {
        return out;
    }
    out.chars().skip(count - cap).collect()
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
    let excerpt = transcript_excerpt(session.history.items(), TRANSCRIPT_CAP);
    if excerpt.trim().is_empty() && hits.is_empty() {
        return;
    }
    let outline = session.context.memory_outline();
    let cwd = session.cwd.clone();
    let window = session.context.window();
    let reserve = session.context.output_reserve();
    // 桥协议要 provider/model 选择串；ResolvedModel.id 只有模型名，直接传会被 resolve_model 拒掉。
    let model = selection(&session.summarizer);
    let http = http.or_else(|| session.http.clone());
    let thread = thread_id.filter(|id| !id.is_empty()).map(str::to_string);
    let app = app.cloned();

    if let (Some(app), Some(thread)) = (app.as_ref(), thread.as_deref()) {
        emit_dream(app, thread, true);
    }

    tokio::spawn(async move {
        let lock = lock_for(&snapshot_path);
        let _guard = lock.lock().await;
        let distill = match generate(http.as_ref(), &model, &outline, &excerpt).await {
            Ok(text) => text,
            Err(error) => {
                eprintln!("lyra: 整理项目记忆失败：{error}");
                String::new()
            }
        };
        if let Some(mut snap) = Snapshot::load(&snapshot_path) {
            let (prose, _, code, _) = memory::budgets(window, reserve);
            if snap.apply_session_end(&distill, &hits, &cwd, prose, code, &TokenCounter::default())
            {
                snap.save(&snapshot_path);
            }
        }
        if let (Some(app), Some(thread)) = (app.as_ref(), thread.as_deref()) {
            emit_dream(app, thread, false);
        }
    });
}

fn selection(resolved: &crate::lyra::config::Resolved) -> String {
    format!("{}/{}", resolved.model.provider, resolved.model.id)
}

async fn generate(
    http: Option<&reqwest::Client>,
    model: &str,
    outline: &str,
    excerpt: &str,
) -> Result<String, String> {
    if model.trim().is_empty() {
        return Ok(String::new());
    }
    let prompt = distill_prompt(outline, excerpt);
    let request = json!({
        "action": "generate",
        "model": model,
        "prompt": prompt,
    });
    let owned;
    let http = match http {
        Some(http) => http,
        None => {
            owned = reqwest::Client::new();
            &owned
        }
    };
    let output = crate::lyra::run_oneshot(http, &request, None).await?;
    output
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| "整理记忆返回了非文本结果".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn summarizer_selection_round_trips_through_resolve_model() {
        let config = json!({
            "provider": {
                "custom": {
                    "npm": "@ai-sdk/openai-compatible",
                    "options": { "baseURL": "http://127.0.0.1:8317/v1", "apiKey": "key" },
                    "models": { "gpt": { "reasoning": true } }
                }
            }
        });
        let env = HashMap::new();
        let resolved =
            crate::lyra::config::resolve_model(&config, Some("custom/gpt"), &env).unwrap();
        // 回归：只传 model.id（"gpt"）会被"Lyra model 必须是 provider/model 格式"拒掉。
        let selection = selection(&resolved);
        assert_eq!(selection, "custom/gpt");
        let again = crate::lyra::config::resolve_model(&config, Some(&selection), &env).unwrap();
        assert_eq!(again.model.provider, "custom");
        assert_eq!(again.model.id, "gpt");
    }

    #[test]
    fn excerpt_skips_memory_prefix_and_keeps_workspace() {
        let items = vec![
            json!({"role":"user","content":[{"type":"text","text":"<project-memory>\n# 索引\n</project-memory>"}]}),
            json!({"role":"assistant","content":[{"type":"text","text": ACK}]}),
            json!({"role":"user","content":[{"type":"text","text":"请看 src/main.rs"}]}),
            json!({"role":"assistant","content":[{"type":"text","text":"看过了"}]}),
        ];
        let text = transcript_excerpt(&items, 2000);
        assert!(text.contains("请看 src/main.rs"), "{text}");
        assert!(text.contains("看过了"), "{text}");
        assert!(!text.contains("<project-memory>"), "{text}");
        assert!(!text.contains(ACK), "{text}");
    }
}
