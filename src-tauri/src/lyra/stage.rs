//! 子 Agent 的只读 Stage 投影。直接写 Nova 会话，不依赖主 Agent 轮次仍在运行。
use super::bridge::Emit;
use super::history::text_content;
use super::turn::TurnOutcome;
use crate::sdk_runtime::SdkManager;
use crate::threads::{AgentKind, Thread};
use crate::AppState;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tauri::{Emitter, Manager};

#[derive(Clone)]
pub(super) struct Stage {
    app: tauri::AppHandle,
    manager: Arc<SdkManager>,
    thread_id: String,
}

fn stage_thread(source: &Thread, name: &str, cwd: String, model: String) -> Thread {
    let mut thread = Thread::new(cwd, AgentKind::Lyra, Some(model), source.mode.clone(), None, source.ephemeral);
    thread.title = format!("[Agent] {name}");
    thread.subagent = true;
    thread.parent_thread_id = Some(source.id.clone());
    thread.worktree = source.worktree.clone();
    thread.employee_thread = source.employee_thread;
    thread.experience_thread = source.experience_thread;
    // 不设置 stage_source_thread_id：展示不能引入 /stage 的动态上下文或改变 fork_turns。
    thread
}

impl Stage {
    pub(super) fn new(app: &tauri::AppHandle, parent: &str, name: &str, cwd: String, model: String) -> Option<Self> {
        let state = app.state::<AppState>();
        let thread_id = {
            let mut store = state.store.lock().unwrap();
            let thread = stage_thread(store.get(parent)?, name, cwd, model);
            let id = thread.id.clone();
            store.threads.push(thread);
            store.save();
            id
        };
        let stage = Self { app: app.clone(), manager: state.lyra.clone(), thread_id };
        stage.note("子 Agent 执行记录（只读），由主会话调度，可与其他子 Agent 并行执行。".into());
        let _ = app.emit(crate::acp::EV_THREADS, json!({}));
        Some(stage)
    }

    pub(super) fn note(&self, text: String) {
        self.manager.push_system(&self.thread_id, text, "info");
    }

    fn user(&self, text: String) {
        let state = self.app.state::<AppState>();
        let mut store = state.store.lock().unwrap();
        if let Some(thread) = store.get_mut(&self.thread_id) {
            let item = thread.push_user(text, vec![]);
            let _ = self.manager.emit_update(&self.thread_id, &item);
            store.save_thread(&self.thread_id);
        }
    }

    pub(super) fn begin(&self, task: &Value) {
        self.user(text_content(&task["content"]));
        self.manager.set_running(&self.thread_id, true, None);
    }

    pub(super) fn finish(&self, outcome: &TurnOutcome, usage: Value) {
        if let Some(error) = &outcome.error {
            self.manager.push_system(&self.thread_id, error.clone(), "error");
        }
        let reason = if outcome.cancelled { "cancelled" } else if outcome.error.is_some() { "error" } else { "end_turn" };
        self.manager.finish_turn(&self.thread_id, reason, Some(usage));
    }

    pub(super) fn emitter(&self) -> Emit {
        let stage = self.clone();
        // 每项任务独立编号映射：followup 的工具/文本 id 可以复用，不覆盖上次记录。
        let ids = Mutex::new(HashMap::new());
        Arc::new(move |event| match event["type"].as_str() {
            Some("item") => stage.manager.apply_item(&stage.thread_id, &event["item"], &mut ids.lock().unwrap()),
            Some("steer") => stage.user(format!("补充消息（已交付）：\n{}", text_content(&event["message"]["content"]))),
            Some("working_directory_changed") => {
                if let Some(cwd) = event["cwd"].as_str() {
                    let state = stage.app.state::<AppState>();
                    let mut store = state.store.lock().unwrap();
                    if let Some(thread) = store.get_mut(&stage.thread_id) {
                        thread.cwd = cwd.to_owned();
                        store.save_thread(&stage.thread_id);
                    }
                    // 仅更新子 Stage，不触发主会话/当前项目切换。
                    let _ = stage.app.emit(crate::acp::EV_THREADS, json!({}));
                }
            }
            Some("timing") => match event["phase"].as_str() {
                Some("provider_retry" | "context_overflow_recovery") => {
                    stage.note(format!("正在重试：{}", event["error"].as_str().unwrap_or_default()));
                }
                Some("context_compaction") => stage.note("已进行上下文折叠。".into()),
                _ => {}
            },
            _ => {}
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_is_readonly_sibling_without_dynamic_context_or_provider_session() {
        let mut root = Thread::new("root-dir".into(), AgentKind::Lyra, None, Some("build".into()), None, true);
        root.employee_thread = true;
        root.acp_session_id = Some("parent-provider-session".into());
        root.push_user("parent-only context".into(), vec![]);
        let first = stage_thread(&root, "first", "child-dir".into(), "actual-model".into());
        let second = stage_thread(&root, "second", "root-dir".into(), "actual-model".into());
        assert!(first.subagent && first.ephemeral && first.employee_thread);
        assert_eq!(first.parent_thread_id.as_deref(), Some(root.id.as_str()));
        assert_eq!(first.parent_thread_id, second.parent_thread_id);
        assert_ne!(first.id, second.id);
        assert_eq!(first.cwd, "child-dir");
        assert_eq!(first.model.as_deref(), Some("actual-model"));
        assert!(first.stage_source_thread_id.is_none() && first.acp_session_id.is_none());
        assert!(first.items.is_empty());
        let restored: Thread = serde_json::from_value(serde_json::to_value(&first).unwrap()).unwrap();
        assert!(restored.subagent);
    }
}
