mod acp;
mod agent_config;
mod lyra_complete;
mod browser;
mod browser_agent;
mod cli_manager;
mod clipboard;
mod clues;
mod codex;
mod codex_radar;
mod context_service;
mod credential_roaming;
mod experience;
mod gitwt;
mod http_stream;
mod lyra;
mod model_cache;
mod nova_tools_native;
mod opencode_sdk;
mod path_env;
mod quota;
mod relay;
mod remote;
mod sdk_adapters;
mod sdk_runtime;
mod server;
mod settings;
mod signature;
mod skills;
mod sleep_inhibitor;
mod sys_notify;
mod threads;
mod time_machine;
mod updater;
#[cfg(windows)]
mod windows_shell_shim;

/// 临时会话目录的统一父目录名（前端据此识别并显示「临时会话」）
pub const SCRATCH_MARK: &str = "Nova-scratch";
/// Native global shortcut event for creating a new session.
const EV_NEW_SESSION_SHORTCUT: &str = "session-shortcut:new-session";

pub use path_env::init_process_path;
pub use server::configure_from_args as configure_server_mode;
pub use server::maybe_run_management_command as maybe_run_server_command;

use acp::AcpManager;
use codex::CodexManager;
use credential_roaming::{BorrowedManager, BorrowedRuntime};
use opencode_sdk::OpenCodeSdkManager;
use relay::{RelayManager, Share, WorkflowShare};
use sdk_adapters::{ClaudeAdapter, CodexAdapter, CursorAdapter, LyraAdapter};
use sdk_runtime::SdkManager;
use serde_json::{json, Value};
use settings::Settings;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tauri::{Emitter, Listener, Manager, State};
use threads::{
    now_ms, render_stage_context, session_cleanup_is_expired, AgentKind, Item, ProjectStore,
    PromptImage, RoamingStore, Thread, ThreadMeta, ThreadStore, ThreadTrashStore, Worktree,
    WorktreeRecord, WorktreeStore,
};

pub struct AppState {
    pub store: Mutex<ThreadStore>,
    /// 自动清理会话的延迟删除回收站（thread-trash.json）。
    pub thread_trash: Mutex<ThreadTrashStore>,
    /// 证据链本地存储：未配置 Relay 时是真相来源；配置后作为团队数据缓存。
    pub clues: Mutex<clues::ClueStore>,
    pub projects: Mutex<ProjectStore>,
    pub settings: Mutex<Settings>,
    /// 应用启动时读取的 Windows shell shim 开关；运行中改设置只会在下次重启时生效。
    pub windows_shell_shim_enabled: bool,
    pub roaming: Mutex<RoamingStore>,
    /// 已创建的 git worktree 记录（独立持久化，供设置面板手动清理）
    pub worktrees: Mutex<WorktreeStore>,
    /// Native resident polaris service shared by every external bridge. Lyra bypasses the
    /// socket and calls the same in-process engine directly.
    pub(crate) context_service: context_service::ContextService,
    pub acp: Arc<AcpManager>,
    /// OpenCode 官方 SDK 对应的 HTTP API 后端，不经过 ACP。
    pub opencodeplus: Arc<OpenCodeSdkManager>,
    pub codex: Arc<CodexManager>,
    /// CodeBuddy 后端（官方 HTTP 传输：`codebuddy --serve` + /api/v1/acp JSON-RPC over SSE）。
    pub codebuddy: Arc<AcpManager>,
    /// Lyra Rust 原生 agent 后端（进程内运行，不经 Node bridge）。
    pub lyra: Arc<SdkManager>,
    /// Codex 官方 TypeScript SDK 后端，不经过 app-server 集成层。
    pub codexplus: Arc<SdkManager>,
    pub claudeplus: Arc<SdkManager>,
    pub cursorplus: Arc<SdkManager>,
    /// 额度租借会话的独立后端进程与临时凭证目录（只在本次运行内存在）。
    pub borrowed_runtimes: Mutex<HashMap<String, BorrowedRuntime>>,
    /// 重启后按需重新申请额度凭证时的线程级互斥，避免连续发送重复创建隔离运行时。
    pub restoring_borrowed_runtimes: Mutex<HashSet<String>>,
    pub relay: Arc<RelayManager>,
    pub config_dir: PathBuf,
    /// 用户最近一次操作的时间戳（ms）。前端把鼠标/键盘等交互节流上报到这里，
    /// 静默升级据此判断是否「一段时间没有操作」。
    pub last_activity_ms: Mutex<i64>,
    /// 前端当前打开的会话 id（None = 停在主页）。升级重启前写入恢复标记，重启后据此恢复显示。
    pub active_thread: Mutex<Option<String>>,
    /// 后端可用性检测结果（agent_kind → 是否可用）。
    pub backend_availability: Mutex<HashMap<String, bool>>,
    /// 编辑历史消息后正在后台 restore/fork、等待自动重发的会话。
    pub pending_prompt_restores: Mutex<HashSet<String>>,
    /// 仓库世界线的创建/恢复互斥；同一进程内禁止两个恢复事务交叉写文件。
    pub time_machine_lock: Mutex<()>,
    /// 网页远控尚待处理的权限请求（requestKey -> 原始事件）。
    pub remote_permissions: Mutex<HashMap<String, Value>>,
    /// 有会话运行时阻止系统因空闲自动休眠；最后一个会话结束后自动释放。
    pub sleep_inhibitor: sleep_inhibitor::SleepInhibitor,
    /// 内嵌浏览器（Playwright 录制进程）状态。
    pub browser: browser::BrowserManager,
    /// 截图分析临时会话与计划运行管理。
    pub browser_agent: browser_agent::BrowserAgentState,
}

impl AppState {
    /// Devin 是唯一使用 ACP 的后端。
    pub fn acp_for(&self, kind: &AgentKind) -> Option<Arc<AcpManager>> {
        match kind {
            AgentKind::Devin => Some(self.acp.clone()),
            _ => None,
        }
    }

    pub fn borrowed_runtime(&self, thread_id: &str) -> Option<BorrowedRuntime> {
        self.borrowed_runtimes
            .lock()
            .unwrap()
            .get(thread_id)
            .cloned()
    }

    /// 某后端在设置里是否启用（关闭的后端不参与标题/分享等自动路由）。
    pub fn agent_enabled(&self, kind: &AgentKind) -> bool {
        let s = self.settings.lock().unwrap();
        match kind {
            AgentKind::Lyra => s.lyra_enabled,
            AgentKind::Devin => s.devin_enabled,
            AgentKind::Codex => s.codex_enabled,
            AgentKind::CodexPlus => s.codexplus_enabled,
            AgentKind::CodeBuddy => s.codebuddy_enabled,
            AgentKind::CodeBuddyPlus => s.codebuddyplus_enabled,
            AgentKind::ClaudeCode => s.claudecode_enabled,
            AgentKind::Cursor => s.cursor_enabled,
            AgentKind::OpenCode => s.opencode_enabled,
            AgentKind::OpenCodePlus => s.opencodeplus_enabled,
        }
    }

    /// ACP 标题生成只由 Devin 提供；OpenCode/Codex SDK 有自己的标题入口。
    fn title_fallback_mgr(&self, _origin: &AgentKind) -> Arc<AcpManager> {
        self.acp.clone()
    }

    /// 统一的会话标题生成入口：优先路由到设置里的轻量级模型。
    /// - 配置后端已启用且有 ACP 管理器（Codex 无）：用它并下发标题模型；
    /// - 否则回退到线程自身后端（origin），此时不下发模型（模型 id 与后端绑定、不通用）。
    /// origin 为触发标题的线程所在后端，仅在回退时使用。
    pub fn generate_title(
        &self,
        origin: &AgentKind,
        thread_id: String,
        prompt: String,
        fallback: String,
    ) {
        // 工作流会话保留 [WF] 前缀（阶段导航依赖它识别链）；超长的阶段名兜底标题截短。
        let fallback = if fallback.starts_with("[WF]") {
            if fallback.chars().count() > 28 {
                format!(
                    "[WF] {}",
                    fallback[4..].chars().take(24).collect::<String>()
                )
            } else {
                fallback
            }
        } else {
            fallback
        };
        let (agent_raw, model) = {
            let s = self.settings.lock().unwrap();
            (
                s.lightweight_model_agent.trim().to_string(),
                s.lightweight_model.trim().to_string(),
            )
        };
        if AgentKind::from_str(&agent_raw) == Some(AgentKind::OpenCode)
            && self.agent_enabled(&AgentKind::OpenCode)
        {
            self.opencodeplus
                .generate_title_async(thread_id, prompt, fallback, model);
            return;
        }
        if agent_raw.is_empty()
            && origin == &AgentKind::OpenCode
            && self.agent_enabled(&AgentKind::OpenCode)
        {
            self.opencodeplus
                .generate_title_async(thread_id, prompt, fallback, String::new());
            return;
        }
        if AgentKind::from_str(&agent_raw) == Some(AgentKind::Cursor)
            && self.agent_enabled(&AgentKind::Cursor)
        {
            self.cursorplus
                .generate_title_async(thread_id, prompt, fallback, model);
            return;
        }
        if agent_raw.is_empty()
            && origin == &AgentKind::Cursor
            && self.agent_enabled(&AgentKind::Cursor)
        {
            self.cursorplus
                .generate_title_async(thread_id, prompt, fallback, String::new());
            return;
        }
        if AgentKind::from_str(&agent_raw) == Some(AgentKind::Lyra)
            && self.agent_enabled(&AgentKind::Lyra)
        {
            self.lyra
                .generate_title_async(thread_id, prompt, fallback, model);
            return;
        }
        if agent_raw.is_empty()
            && origin == &AgentKind::Lyra
            && self.agent_enabled(&AgentKind::Lyra)
        {
            self.lyra
                .generate_title_async(thread_id, prompt, fallback, String::new());
            return;
        }
        if matches!(
            AgentKind::from_str(&agent_raw),
            Some(AgentKind::Codex | AgentKind::CodexPlus)
        ) && self.agent_enabled(&AgentKind::Codex)
        {
            self.codexplus
                .generate_title_async(thread_id, prompt, fallback, model);
            return;
        }
        if agent_raw.is_empty()
            && matches!(origin, AgentKind::Codex | AgentKind::CodexPlus)
            && self.agent_enabled(origin)
        {
            self.codexplus
                .generate_title_async(thread_id, prompt, fallback, String::new());
            return;
        }
        if matches!(
            AgentKind::from_str(&agent_raw),
            Some(AgentKind::CodeBuddy | AgentKind::CodeBuddyPlus)
        ) && self.agent_enabled(&AgentKind::CodeBuddy)
        {
            self.codebuddy
                .generate_title_async(thread_id, prompt, fallback, model);
            return;
        }
        if agent_raw.is_empty()
            && matches!(origin, AgentKind::CodeBuddy | AgentKind::CodeBuddyPlus)
            && self.agent_enabled(&AgentKind::CodeBuddy)
        {
            self.codebuddy
                .generate_title_async(thread_id, prompt, fallback, String::new());
            return;
        }
        let (mgr, model) = match AgentKind::from_str(&agent_raw) {
            Some(AgentKind::ClaudeCode) => (self.acp.clone(), String::new()),
            Some(kind) if self.agent_enabled(&kind) => match self.acp_for(&kind) {
                Some(mgr) => (mgr, model),
                None => (self.title_fallback_mgr(origin), String::new()),
            },
            _ => (self.title_fallback_mgr(origin), String::new()),
        };
        mgr.generate_title_async(thread_id, prompt, fallback, model);
    }
}

pub(crate) fn is_running(state: &AppState, thread: &Thread) -> bool {
    if state
        .pending_prompt_restores
        .lock()
        .unwrap()
        .contains(&thread.id)
    {
        return true;
    }
    if thread.is_roaming_guest() {
        return state.relay.is_guest_running(&thread.id);
    }
    if thread.is_quota_borrowed() {
        return state
            .borrowed_runtime(&thread.id)
            .map(|runtime| runtime.is_running(&thread.id))
            .unwrap_or(false);
    }
    match thread.agent_kind {
        AgentKind::Lyra => state.lyra.is_running(&thread.id),
        AgentKind::Devin => state.acp.is_running(&thread.id),
        AgentKind::Codex | AgentKind::CodexPlus => state.codexplus.is_running(&thread.id),
        AgentKind::CodeBuddy | AgentKind::CodeBuddyPlus => state.codebuddy.is_running(&thread.id),
        AgentKind::ClaudeCode => state.claudeplus.is_running(&thread.id),
        AgentKind::Cursor => state.cursorplus.is_running(&thread.id),
        AgentKind::OpenCode | AgentKind::OpenCodePlus => state.opencodeplus.is_running(&thread.id),
    }
}

fn running_by_id(state: &AppState, thread_id: &str) -> bool {
    let store = state.store.lock().unwrap();
    let Some(thread) = store.get(thread_id) else {
        return false;
    };
    is_running(state, thread)
}

fn is_starrable_thread(thread: &Thread) -> bool {
    thread.roaming_role.is_none()
}

fn is_normal_thread_for_auto_cleanup(thread: &Thread) -> bool {
    // 漫游会话（host 替别人执行 / guest 收看别人执行）与普通会话一样参与自动清理；
    // 仍豁免：额度租借会话、经验训练会话和星标会话。运行中的会话由调用方另行排除。
    thread.quota_peer.is_none() && !thread.experience_thread && !thread.starred
}

fn thread_is_expired(updated_at: i64, now: i64, hours: u32) -> bool {
    session_cleanup_is_expired(updated_at, now, hours)
}

const EXPERIENCE_THREAD_RETENTION_MS: i64 = 24 * 60 * 60 * 1000;

fn experience_thread_is_expired(thread: &Thread, now: i64) -> bool {
    thread.experience_thread
        && thread.updated_at < now.saturating_sub(EXPERIENCE_THREAD_RETENTION_MS)
}

fn run_experience_thread_cleanup(app: &tauri::AppHandle) -> usize {
    let state = app.state::<AppState>();
    let now = now_ms();
    let candidates: Vec<String> = {
        let store = state.store.lock().unwrap();
        let mut ids: Vec<String> = store
            .threads
            .iter()
            .filter(|thread| experience_thread_is_expired(thread, now))
            .map(|thread| thread.id.clone())
            .collect();
        // 训练与世代演进为同一批内容在多个时点各开一条会话；只保留每个时点组
        // 里最新的一条，其余无论是否到 24h 都清掉，避免列表被同批次会话刷屏。
        ids.extend(stale_experience_run_thread_ids(&store));
        ids
    };
    let deletable = candidates
        .into_iter()
        .filter(|id| !running_by_id(&state, id))
        .collect::<Vec<_>>();
    if deletable.is_empty() {
        return 0;
    }
    remove_threads(app, &state, deletable).len()
}

/// 一次训练/演进批次 = 首个会话（无 parent）+ 其后代（parent 指向它）。
/// 返回除每批最新一条外的所有会话 id，保持训练视图只剩最近时点。
fn stale_experience_run_thread_ids(store: &crate::threads::ThreadStore) -> Vec<String> {
    let experience: Vec<&Thread> = store
        .threads
        .iter()
        .filter(|thread| thread.experience_thread)
        .collect();
    let mut child_parent: HashMap<&str, &str> = HashMap::new();
    for thread in &experience {
        if let Some(parent) = thread
            .parent_thread_id
            .as_deref()
            .filter(|parent| experience.iter().any(|t| t.id == *parent))
        {
            child_parent.insert(thread.id.as_str(), parent);
        }
    }
    let root_of = |thread: &Thread| -> String {
        child_parent
            .get(thread.id.as_str())
            .map(|parent| (*parent).to_string())
            .unwrap_or_else(|| thread.id.clone())
    };
    // 演进审核会话没有 parent，但按「同项目同标题前缀」聚成一组，同样只留最新。
    let mut groups: HashMap<String, Vec<&Thread>> = HashMap::new();
    for thread in &experience {
        let parent = child_parent.get(thread.id.as_str());
        let key = if parent.is_some() {
            format!("run:{}", root_of(thread))
        } else if thread.title.starts_with("世代演进审核") {
            format!("evolve:{}:{}", thread.cwd, thread.title)
        } else {
            format!("run:{}", root_of(thread))
        };
        groups.entry(key).or_default().push(*thread);
    }
    let mut stale = Vec::new();
    for (_, members) in groups {
        if members.len() <= 1 {
            continue;
        }
        let newest = members
            .iter()
            .max_by_key(|thread| thread.updated_at)
            .map(|thread| thread.id.as_str());
        for member in members {
            if Some(member.id.as_str()) != newest {
                stale.push(member.id.clone());
            }
        }
    }
    stale
}

fn run_session_auto_cleanup(app: &tauri::AppHandle) -> usize {
    let experience_removed = run_experience_thread_cleanup(app);
    let state = app.state::<AppState>();
    let hours = {
        let settings = state.settings.lock().unwrap();
        if !settings.session_auto_cleanup_enabled {
            return experience_removed;
        }
        settings.session_auto_cleanup_hours
    };
    let now = now_ms();
    let permanently_removed = state.thread_trash.lock().unwrap().purge_expired(now, hours);
    let retained_session_ids = state
        .store
        .lock()
        .unwrap()
        .threads
        .iter()
        .filter_map(|thread| thread.acp_session_id.clone())
        .chain(state.thread_trash.lock().unwrap().session_ids())
        .collect();
    cleanup_lyra_session_files(&state.config_dir, &permanently_removed, &retained_session_ids);
    for thread in permanently_removed {
        if thread.cwd.contains(SCRATCH_MARK) {
            let _ = std::fs::remove_dir_all(thread.cwd);
        }
    }
    let thread_ids = {
        let store = state.store.lock().unwrap();
        store
            .threads
            .iter()
            .filter(|thread| {
                is_normal_thread_for_auto_cleanup(thread)
                    && thread_is_expired(thread.updated_at, now, hours)
            })
            .map(|thread| thread.id.clone())
            .collect()
    };
    let deletable = collect_deletable_thread_ids(&state, thread_ids, true);
    let threads: Vec<Thread> = {
        let store = state.store.lock().unwrap();
        store
            .threads
            .iter()
            .filter(|thread| deletable.contains(&thread.id))
            .cloned()
            .collect()
    };
    if threads.is_empty() {
        return experience_removed;
    }
    if let Err(error) = state
        .thread_trash
        .lock()
        .unwrap()
        .move_to_trash(threads, now)
    {
        eprintln!("[session-cleanup] 移入回收站失败：{error}");
        return experience_removed;
    }
    experience_removed + remove_threads(app, &state, deletable).len()
}

/// 是否有任意会话正在运行（本地 Devin/Codex、漫游 guest、被别人漫游的 host 均算）。
/// 静默升级的前置条件之一：没有任何会话在跑才允许自动替换重启。
fn any_session_running(state: &AppState) -> bool {
    let store = state.store.lock().unwrap();
    store.threads.iter().any(|t| is_running(state, t))
}

pub(crate) async fn shutdown_agent_processes(state: &AppState) {
    state.acp.kill_conn().await;
    state.codex.kill_conn().await;
    state.codexplus.shutdown();
    state.codebuddy.shutdown();
    state.claudeplus.shutdown();
    state.cursorplus.shutdown();
    state.opencodeplus.shutdown();
    let borrowed: Vec<BorrowedRuntime> = state
        .borrowed_runtimes
        .lock()
        .unwrap()
        .drain()
        .map(|(_, runtime)| runtime)
        .collect();
    for runtime in borrowed {
        runtime.shutdown().await;
    }
}

fn agent_kind_for_thread(state: &AppState, thread_id: &str) -> Result<AgentKind, String> {
    let store = state.store.lock().unwrap();
    store
        .get(thread_id)
        .map(|t| t.agent_kind.clone())
        .ok_or_else(|| "线程不存在".into())
}

fn cleanup_borrowed_runtime(state: &AppState, thread_id: &str) {
    let runtime = state.borrowed_runtimes.lock().unwrap().remove(thread_id);
    if let Some(runtime) = runtime {
        tauri::async_runtime::spawn(async move {
            runtime.shutdown().await;
        });
    }
}

#[cfg(test)]
mod session_auto_cleanup_tests {
    use super::{
        cleanup_lyra_session_files, experience_thread_is_expired,
        is_normal_thread_for_auto_cleanup, is_starrable_thread, thread_is_expired,
        tree_contains_starred_thread, AgentKind, Thread, EXPERIENCE_THREAD_RETENTION_MS,
    };
    use std::collections::HashSet;

    #[test]
    fn experience_threads_expire_after_24_continuous_hours_only() {
        let now = 100 * 60 * 60 * 1000;
        let mut training = Thread::new(String::new(), AgentKind::Devin, None, None, None, false);
        training.experience_thread = true;
        training.updated_at = now - EXPERIENCE_THREAD_RETENTION_MS;
        assert!(!experience_thread_is_expired(&training, now));

        training.updated_at -= 1;
        assert!(experience_thread_is_expired(&training, now));
        training.experience_thread = false;
        assert!(!experience_thread_is_expired(&training, now));
    }

    #[test]
    fn thread_is_expired_only_after_the_configured_retention() {
        const HOUR_MS: i64 = 60 * 60 * 1000;
        let now = 10 * HOUR_MS;

        assert!(!thread_is_expired(now - 3 * HOUR_MS, now, 3));
        assert!(thread_is_expired(now - 3 * HOUR_MS - 1, now, 3));
        assert!(thread_is_expired(now - HOUR_MS - 1, now, 0));
    }

    #[test]
    fn thread_expiration_does_not_count_weekends() {
        use chrono::TimeZone;

        let friday = chrono::Local
            .with_ymd_and_hms(2026, 7, 17, 17, 0, 0)
            .single()
            .unwrap()
            .timestamp_millis();
        let monday = chrono::Local
            .with_ymd_and_hms(2026, 7, 20, 9, 0, 0)
            .single()
            .unwrap()
            .timestamp_millis();
        let tuesday = chrono::Local
            .with_ymd_and_hms(2026, 7, 21, 17, 0, 0)
            .single()
            .unwrap()
            .timestamp_millis();

        assert!(!thread_is_expired(friday, monday, 48));
        assert!(!thread_is_expired(friday, tuesday, 48));
        assert!(thread_is_expired(friday, tuesday + 1, 48));
    }

    #[test]
    fn special_threads_are_not_auto_cleanup_candidates() {
        let mut thread = Thread::new(String::new(), AgentKind::Devin, None, None, None, false);
        assert!(is_normal_thread_for_auto_cleanup(&thread));

        thread.ephemeral = true;
        assert!(is_normal_thread_for_auto_cleanup(&thread));
        thread.starred = true;
        assert!(!is_normal_thread_for_auto_cleanup(&thread));
        thread.starred = false;
        thread.ephemeral = false;
        thread.experience_thread = true;
        assert!(!is_normal_thread_for_auto_cleanup(&thread));
        thread.experience_thread = false;
        // 漫游会话（两种角色）不豁免自动清理
        thread.roaming_role = Some("host".into());
        assert!(is_normal_thread_for_auto_cleanup(&thread));
        thread.roaming_role = Some("guest".into());
        assert!(is_normal_thread_for_auto_cleanup(&thread));
        thread.roaming_role = None;
        // 额度租借会话仍豁免
        thread.quota_peer = Some("peer-token".into());
        assert!(!is_normal_thread_for_auto_cleanup(&thread));
    }

    #[test]
    fn quota_threads_support_starring() {
        let mut thread = Thread::new(String::new(), AgentKind::Devin, None, None, None, false);
        thread.quota_peer = Some("peer".into());
        assert!(is_starrable_thread(&thread));

        thread.roaming_role = Some("guest".into());
        assert!(!is_starrable_thread(&thread));
    }

    #[test]
    fn starred_descendant_protects_its_tree() {
        let tree = vec!["parent".to_string(), "child".to_string()];
        let starred = HashSet::from(["child".to_string()]);

        assert!(tree_contains_starred_thread(&tree, &starred));
    }

    #[test]
    fn removing_lyra_thread_cleans_its_session_files_only() {
        let data_dir = std::env::temp_dir().join(format!(
            "nova-lyra-session-cleanup-{}",
            uuid::Uuid::new_v4()
        ));
        let sessions = data_dir.join("alkaid").join("sessions");
        let tool_results = data_dir.join("alkaid").join("tool-results");
        std::fs::create_dir_all(tool_results.join("session-a")).unwrap();
        std::fs::create_dir_all(tool_results.join("session-b")).unwrap();
        for name in ["session-a.json", "session-a.slim.json", "session-b.json"] {
            std::fs::create_dir_all(&sessions).unwrap();
            std::fs::write(sessions.join(name), "{}").unwrap();
        }
        let mut removed = Thread::new(String::new(), AgentKind::Lyra, None, None, None, false);
        removed.acp_session_id = Some("session-a".into());

        cleanup_lyra_session_files(
            &data_dir,
            std::slice::from_ref(&removed),
            &HashSet::from(["session-a".into()]),
        );
        assert!(sessions.join("session-a.json").exists());
        assert!(tool_results.join("session-a").exists());

        cleanup_lyra_session_files(&data_dir, &[removed], &HashSet::new());
        assert!(!sessions.join("session-a.json").exists());
        assert!(!sessions.join("session-a.slim.json").exists());
        assert!(!tool_results.join("session-a").exists());
        assert!(sessions.join("session-b.json").exists());
        assert!(tool_results.join("session-b").exists());
        std::fs::remove_dir_all(data_dir).unwrap();
    }
}

#[cfg(windows)]
fn wide_null(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
fn spawn_single_instance_focus_listener(app: &tauri::AppHandle) {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
    const FOCUS_EVENT: &str = "Local\\NovaDesktopFocusEvent";
    const INFINITE: u32 = 0xFFFF_FFFF;
    const WAIT_OBJECT_0: u32 = 0;

    let app = app.clone();
    std::thread::spawn(move || {
        let name = wide_null(FOCUS_EVENT);
        let event = unsafe { CreateEventW(std::ptr::null(), 0, 0, name.as_ptr()) };
        if event.is_null() {
            return;
        }
        loop {
            let wait = unsafe { WaitForSingleObject(event, INFINITE) };
            if wait != WAIT_OBJECT_0 {
                break;
            }
            if let Some(win) = app
                .get_webview_window("main")
                .or_else(|| app.webview_windows().into_values().next())
            {
                let _ = win.show();
                let _ = win.unminimize();
                let _ = win.set_focus();
            }
        }
        unsafe {
            CloseHandle(event);
        }
    });
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn sync_global_session_shortcuts(app: &tauri::AppHandle) {
    use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

    if server::is_headless() {
        return;
    }
    if let Err(error) = app.global_shortcut().unregister_all() {
        eprintln!("[shortcut] 清理全局快捷键失败：{error}");
    }

    let mut seen = HashSet::new();
    let state = app.state::<AppState>();
    let shortcuts = {
        let settings = state.settings.lock().unwrap();
        settings
            .session_shortcuts
            .iter()
            .filter(|shortcut| shortcut.action == "newSession")
            .filter_map(|shortcut| {
                let keys = normalize_global_shortcut(&shortcut.keys);
                if keys.is_empty() || !seen.insert(keys.to_ascii_lowercase()) {
                    return None;
                }
                let parsed = match keys.parse::<tauri_plugin_global_shortcut::Shortcut>() {
                    Ok(value) => value,
                    Err(_) => {
                        eprintln!("[shortcut] 忽略无法注册的快捷键：{}", shortcut.keys);
                        return None;
                    }
                };
                Some(parsed)
            })
            .collect::<Vec<_>>()
    };

    if shortcuts.is_empty() {
        return;
    }
    for shortcut in shortcuts {
        let label = shortcut.to_string();
        if let Err(error) = app
            .global_shortcut()
            .on_shortcut(shortcut, |app, _shortcut, event| {
                if event.state == ShortcutState::Pressed {
                    // A global shortcut is often used while Nova is behind another window or
                    // minimized. Bring it back before delivering the event so the user can see
                    // the newly opened session page.
                    if let Some(window) = app
                        .get_webview_window("main")
                        .or_else(|| app.webview_windows().into_values().next())
                    {
                        let _ = window.show();
                        let _ = window.unminimize();
                        let _ = window.set_focus();
                    }
                    let _ = app.emit(EV_NEW_SESSION_SHORTCUT, ());
                }
            })
        {
            eprintln!("[shortcut] 注册新建会话全局快捷键失败（{label}）：{error}");
        } else {
            eprintln!("[shortcut] 已注册新建会话全局快捷键：{label}");
        }
    }
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn normalize_global_shortcut(keys: &str) -> String {
    keys.split('+')
        .map(|part| {
            if part.trim().eq_ignore_ascii_case("meta") {
                "Super".to_string()
            } else {
                part.trim().to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("+")
}

/// 后端可用性检测完成事件：payload = { availability: { devin: bool, codex: bool, ... } }
pub const EV_BACKENDS: &str = "backends:availability";

/// 判断后端是否应出现在可选列表。
/// 内置 SDK bridge 后端不依赖设置里的旧 CLI 路径；真正启动 bridge 时仍会检查
/// Node.js / 凭据并返回明确错误。其余后端按 CLI 路径检测。
fn backend_is_available(kind: &AgentKind, program: &str) -> bool {
    matches!(
        kind,
        AgentKind::Lyra | AgentKind::ClaudeCode | AgentKind::Cursor
    ) || acp::resolve_program_on_path(program).is_some()
}

#[cfg(test)]
mod backend_availability_tests {
    use super::{backend_is_available, AgentKind};

    #[test]
    fn sdk_backends_are_available_without_legacy_cli_paths() {
        let missing = "__nova_missing_backend_executable__";
        for kind in [AgentKind::Lyra, AgentKind::ClaudeCode, AgentKind::Cursor] {
            assert!(backend_is_available(&kind, missing), "{kind:?}");
        }
        assert!(!backend_is_available(&AgentKind::Devin, missing));
        // CodeBuddy 走 ACP 模式时直接启动 CLI（`codebuddy --acp`），必须真实可解析。
        assert!(!backend_is_available(&AgentKind::CodeBuddy, missing));
    }
}

/// 并发检测各后端 CLI 是否可用：只在 PATH / 具体路径上解析可执行文件（不拉起进程、零成本，
/// 对 Cursor 也不会产生任何用量）。结果写入 state 并广播，启动后与保存设置后各触发一次。
fn spawn_backend_availability_check(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        let paths: Vec<(AgentKind, String)> = {
            let state = app.state::<AppState>();
            let s = state.settings.lock().unwrap();
            vec![
                // Lyra 是随 Nova 提供的内置后端；占位 program 不参与其可用性判定。
                (AgentKind::Lyra, String::new()),
                (AgentKind::Devin, s.devin_path.clone()),
                (AgentKind::Codex, s.codex_path.clone()),
                (AgentKind::CodeBuddy, s.codebuddy_path.clone()),
                (AgentKind::ClaudeCode, s.claudecode_path.clone()),
                (AgentKind::Cursor, s.cursor_path.clone()),
                (AgentKind::OpenCode, s.opencode_path.clone()),
            ]
        };
        // PATH 扫描是同步文件 IO：各自丢进 blocking 线程并发跑，全部完成后一次性汇总
        let checks: Vec<_> = paths
            .into_iter()
            .map(|(kind, path)| {
                tauri::async_runtime::spawn_blocking(move || {
                    let available = backend_is_available(&kind, &path);
                    (kind, available)
                })
            })
            .collect();
        let mut result = HashMap::new();
        for c in checks {
            if let Ok((kind, ok)) = c.await {
                result.insert(kind.as_str().to_string(), ok);
            }
        }
        let state = app.state::<AppState>();
        *state.backend_availability.lock().unwrap() = result.clone();
        let _ = app.emit(EV_BACKENDS, json!({ "availability": result }));
    });
}

/// 前端拉取后端可用性（启动早期事件可能已错过，用它兜底同步一次）。
/// 空 map = 尚未检测完成，前端此时先按「全部可用」显示，避免闪烁。
#[tauri::command]
fn get_backend_availability(state: State<'_, AppState>) -> HashMap<String, bool> {
    state.backend_availability.lock().unwrap().clone()
}

#[tauri::command]
async fn get_cli_statuses(settings: Settings) -> Vec<cli_manager::CliStatus> {
    cli_manager::statuses(&settings).await
}

#[tauri::command]
fn list_threads(state: State<'_, AppState>) -> Vec<ThreadMeta> {
    thread_metas(&state)
}

fn thread_metas(state: &AppState) -> Vec<ThreadMeta> {
    // 会话自身没带 worktree 标注、但 cwd 正好是某条已知 worktree 工作目录时
    // （在项目选择器里直接选中 worktree 目录开的会话、员工 worktree 会话等），
    // 按 worktree 记录表现场补齐标注，避免左侧列表把 uuid 目录名当分组标题展示。
    let wt_by_path: HashMap<String, Worktree> = {
        let worktrees = state.worktrees.lock().unwrap();
        worktrees
            .worktrees
            .iter()
            .map(|w| {
                (
                    w.path.clone(),
                    Worktree {
                        repo: w.repo.clone(),
                        path: w.path.clone(),
                        branch: w.branch.clone(),
                    },
                )
            })
            .collect()
    };
    let store = state.store.lock().unwrap();
    let mut metas: Vec<ThreadMeta> = store
        .threads
        .iter()
        .map(|t| ThreadMeta {
            id: t.id.clone(),
            title: t.title.clone(),
            cwd: t.cwd.clone(),
            agent_kind: t.agent_kind.clone(),
            model: t.model.clone(),
            created_at: t.created_at,
            updated_at: t.updated_at,
            running: is_running(&state, t),
            ephemeral: t.ephemeral,
            starred: t.starred,
            roaming_role: t.roaming_role.clone(),
            roaming_peer_name: t.roaming_peer_name.clone(),
            quota_peer_name: t.quota_peer_name.clone(),
            worktree: t
                .worktree
                .clone()
                .or_else(|| wt_by_path.get(&t.cwd).cloned()),
            experience_thread: t.experience_thread,
            browser_thread: t.browser_thread,
            parent_thread_id: t.parent_thread_id.clone(),
            stage_source_thread_id: t.stage_source_thread_id.clone(),
            active_clue_card_id: t.active_clue_card_id.clone(),
        })
        .collect();
    metas.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    metas
}

#[tauri::command]
async fn list_clue_groups(
    state: State<'_, AppState>,
    space: Option<String>,
) -> Result<Vec<clues::ClueNodeGroup>, String> {
    if !state.relay.is_configured() {
        return Err("云端证据链需要先配置团队中转站".into());
    }
    state
        .relay
        .clue_list(space.as_deref().unwrap_or("personal"))
        .await
}

#[tauri::command]
async fn get_clue_context(
    state: State<'_, AppState>,
    card_id: String,
) -> Result<clues::ClueContextSnapshot, String> {
    if state.relay.is_configured() {
        state.relay.clue_context(&card_id).await
    } else {
        state.clues.lock().unwrap().snapshot(&card_id)
    }
}

#[tauri::command]
async fn capture_clue(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    thread_id: Option<String>,
    title: String,
    content: String,
    placement: String,
    target_card_id: Option<String>,
    mention_tokens: Vec<String>,
    attachments: Vec<clues::ClueAttachment>,
    space: Option<String>,
) -> Result<clues::CaptureClueResult, String> {
    if let Some(thread_id) = thread_id.as_deref() {
        let store = state.store.lock().unwrap();
        if store.get(thread_id).is_none() {
            return Err("线程不存在".into());
        }
    }
    if !state.relay.is_configured() {
        return Err("云端证据链需要先配置团队中转站".into());
    }
    let clue_space = space.as_deref().unwrap_or("personal");
    let result = state
        .relay
        .clue_capture(
            thread_id.as_deref(),
            &title,
            &content,
            &placement,
            target_card_id.as_deref(),
            &mention_tokens,
            &attachments,
            clue_space,
        )
        .await?;
    if let Some(thread_id) = thread_id {
        let mut store = state.store.lock().unwrap();
        if let Some(thread) = store.get_mut(&thread_id) {
            thread.active_clue_card_id = Some(result.card.id.clone());
            thread.updated_at = now_ms();
            store.save();
        }
        let _ = app.emit(acp::EV_THREADS, json!({}));
    }
    let _ = app.emit(clues::EV_CLUES, json!({}));
    Ok(result)
}

#[tauri::command]
async fn add_clue_comment(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    card_id: String,
    content: String,
    parent_comment_id: Option<String>,
    mention_tokens: Vec<String>,
    space: Option<String>,
) -> Result<(), String> {
    if !state.relay.is_configured() {
        return Err("云端证据链需要先配置团队中转站".into());
    }
    state
        .relay
        .clue_comment(
            &card_id,
            &content,
            parent_comment_id.as_deref(),
            &mention_tokens,
            space.as_deref().unwrap_or("personal"),
        )
        .await?;
    let _ = app.emit(clues::EV_CLUES, json!({}));
    Ok(())
}

#[tauri::command]
async fn associate_clues(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    before_card_id: String,
    after_card_id: String,
    space: Option<String>,
) -> Result<clues::ClueNodeGroup, String> {
    let group = state
        .relay
        .clue_associate(
            &before_card_id,
            &after_card_id,
            space.as_deref().unwrap_or("personal"),
        )
        .await?;
    let _ = app.emit(clues::EV_CLUES, json!({}));
    Ok(group)
}

#[tauri::command]
async fn disassociate_clues(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    before_card_id: String,
    after_card_id: String,
    space: Option<String>,
) -> Result<clues::ClueNodeGroup, String> {
    let group = state
        .relay
        .clue_disassociate(
            &before_card_id,
            &after_card_id,
            space.as_deref().unwrap_or("personal"),
        )
        .await?;
    let _ = app.emit(clues::EV_CLUES, json!({}));
    Ok(group)
}

#[tauri::command]
async fn split_clue(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    card_id: String,
    space: Option<String>,
) -> Result<clues::ClueNodeGroup, String> {
    let group = state
        .relay
        .clue_split(&card_id, space.as_deref().unwrap_or("personal"))
        .await?;
    let _ = app.emit(clues::EV_CLUES, json!({}));
    Ok(group)
}

#[tauri::command]
async fn stack_clues(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    card_ids: Vec<String>,
    space: Option<String>,
) -> Result<clues::ClueNodeGroup, String> {
    let group = state
        .relay
        .clue_stack(&card_ids, space.as_deref().unwrap_or("personal"))
        .await?;
    let _ = app.emit(clues::EV_CLUES, json!({}));
    Ok(group)
}

#[tauri::command]
async fn delete_clue(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    card_id: String,
    space: Option<String>,
) -> Result<(), String> {
    state
        .relay
        .clue_delete(&card_id, space.as_deref().unwrap_or("personal"))
        .await?;
    let mut store = state.store.lock().unwrap();
    if store.clear_active_clue_card(&card_id) {
        store.save();
        let _ = app.emit(acp::EV_THREADS, json!({}));
    }
    drop(store);
    let _ = app.emit(clues::EV_CLUES, json!({}));
    Ok(())
}

#[tauri::command]
fn load_threads(state: State<'_, AppState>) -> (Vec<ThreadMeta>, Vec<Thread>) {
    let metas = thread_metas(&state);
    let store = state.store.lock().unwrap();
    let by_id: HashMap<&str, &Thread> = store
        .threads
        .iter()
        .map(|thread| (thread.id.as_str(), thread))
        .collect();
    let threads = metas
        .iter()
        .filter_map(|meta| by_id.get(meta.id.as_str()).map(|thread| (*thread).clone()))
        .collect();
    (metas, threads)
}

#[tauri::command]
fn get_thread(state: State<'_, AppState>, thread_id: String) -> Result<Thread, String> {
    let store = state.store.lock().unwrap();
    store
        .get(&thread_id)
        .cloned()
        .ok_or_else(|| "线程不存在".into())
}

/// 项目选择器里的一条最近项目。worktree 非空表示该目录其实是某次会话创建的
/// git worktree（目录名是随机 uuid），前端据此显示「仓库名 ⎇ 分支」而不是 uuid。
#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ProjectEntry {
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    worktree: Option<ProjectWorktreeInfo>,
}

#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ProjectWorktreeInfo {
    repo: String,
    branch: String,
}

fn local_project_entries(state: &AppState) -> Vec<ProjectEntry> {
    // 会话用过的目录也并入列表（此前由前端合并，挪到后端统一做标注/过滤）。
    // guest 漫游会话的 cwd 指向对方机器，不能当本地项目。
    let (guest_cwds, thread_cwds): (HashSet<String>, Vec<String>) = {
        let store = state.store.lock().unwrap();
        let guests = store
            .threads
            .iter()
            .filter(|t| t.is_roaming_guest())
            .map(|t| t.cwd.clone())
            .collect();
        let cwds = store
            .threads
            .iter()
            .filter(|t| !t.is_roaming_guest())
            .map(|t| t.cwd.clone())
            .filter(|c| !c.is_empty() && !c.contains(SCRATCH_MARK))
            .collect();
        (guests, cwds)
    };
    let mut seen: HashSet<String> = HashSet::new();
    let mut paths: Vec<String> = Vec::new();
    {
        let projects = state.projects.lock().unwrap();
        for p in projects.projects.iter() {
            if !guest_cwds.contains(p) && seen.insert(p.clone()) {
                paths.push(p.clone());
            }
        }
    }
    for c in thread_cwds {
        if seen.insert(c.clone()) {
            paths.push(c);
        }
    }
    // worktree 标注：目录被删（remove_worktree / 手动清理）后 is_dir 过滤，项目条目随之消失
    let wt_by_path: HashMap<String, ProjectWorktreeInfo> = {
        let worktrees = state.worktrees.lock().unwrap();
        worktrees
            .worktrees
            .iter()
            .map(|w| {
                (
                    w.path.clone(),
                    ProjectWorktreeInfo {
                        repo: w.repo.clone(),
                        branch: w.branch.clone(),
                    },
                )
            })
            .collect()
    };
    paths
        .into_iter()
        .filter(|p| std::path::Path::new(p).is_dir())
        .map(|p| {
            let worktree = wt_by_path.get(&p).cloned();
            ProjectEntry { path: p, worktree }
        })
        .collect()
}

#[tauri::command]
fn list_projects(state: State<'_, AppState>) -> Vec<ProjectEntry> {
    local_project_entries(state.inner())
}

fn project_path_key(path: &str) -> String {
    #[cfg(windows)]
    {
        path.replace('/', "\\").to_lowercase()
    }
    #[cfg(not(windows))]
    {
        path.to_string()
    }
}

fn match_existing_project_folders(projects: Vec<String>, folders: Vec<String>) -> Vec<String> {
    let projects: HashMap<String, String> = projects
        .into_iter()
        .map(|path| (project_path_key(&path), path))
        .collect();
    let mut seen = HashSet::new();
    folders
        .into_iter()
        .filter_map(|folder| {
            let key = project_path_key(folder.trim());
            if key.is_empty() || !seen.insert(key.clone()) {
                return None;
            }
            projects.get(&key).cloned()
        })
        .collect()
}

pub(crate) fn restrict_roaming_folders_to_projects(
    state: &AppState,
    folders: Vec<String>,
) -> Vec<String> {
    let projects = local_project_entries(state)
        .into_iter()
        .map(|project| project.path)
        .collect();
    match_existing_project_folders(projects, folders)
}

pub(crate) fn current_roaming_project_folders(state: &AppState) -> Vec<String> {
    let current = state.roaming.lock().unwrap().folders.clone();
    let folders = restrict_roaming_folders_to_projects(state, current.clone());
    if folders != current {
        let mut roaming = state.roaming.lock().unwrap();
        if roaming.folders == current {
            roaming.folders = folders.clone();
            roaming.save();
        }
    }
    folders
}

#[cfg(test)]
mod roaming_folder_selection_tests {
    use super::match_existing_project_folders;

    #[test]
    fn roaming_folders_only_keep_existing_projects() {
        let projects = vec!["/projects/alpha".to_string(), "/projects/beta".to_string()];
        let folders = vec![
            " /projects/alpha ".to_string(),
            "/tmp/arbitrary".to_string(),
            "/projects/alpha".to_string(),
        ];

        assert_eq!(
            match_existing_project_folders(projects, folders),
            vec!["/projects/alpha".to_string()]
        );
    }
}

#[tauri::command]
fn remove_project(state: State<'_, AppState>, cwd: String) {
    state.projects.lock().unwrap().remove(&cwd);
    state.relay.publish_folders();
}

/// 预热某个项目目录的 agent（草稿页选定项目/模型/模式时调用）：
/// CodeBuddy 启动官方 one-shot 预热进程；Cursor 预热 idle bridge 与 Agent.create。
#[tauri::command]
fn prewarm(
    state: State<'_, AppState>,
    cwd: String,
    agent_kind: Option<AgentKind>,
    model: Option<String>,
    mode: Option<String>,
) {
    if !std::path::Path::new(&cwd).is_dir() {
        return;
    }
    let agent_kind = agent_kind.unwrap_or(AgentKind::Devin);
    if !state.agent_enabled(&agent_kind) {
        return;
    }
    let mode = {
        let default_mode = state.settings.lock().unwrap().default_mode.clone();
        mode.filter(|s| !s.is_empty())
            .or(Some(default_mode).filter(|s| !s.is_empty()))
    };
    if matches!(
        agent_kind,
        AgentKind::Devin | AgentKind::CodeBuddy | AgentKind::CodeBuddyPlus
    ) {
        let mgr = if agent_kind == AgentKind::Devin {
            state.acp.clone()
        } else {
            state.codebuddy.clone()
        };
        tauri::async_runtime::spawn(async move {
            mgr.prewarm(cwd).await;
        });
        return;
    }
    // SDK 后端：把 Node bridge 启动与 Agent.create 提前到空闲期，首轮不再冷启动。
    let manager: Option<Arc<SdkManager>> = match agent_kind {
        AgentKind::Cursor => Some(state.cursorplus.clone()),
        _ => None,
    };
    if let Some(manager) = manager {
        manager.prewarm_idle(cwd, model.unwrap_or_default(), mode.unwrap_or_default());
    }
}

fn clean_frontmatter_value(value: &str) -> String {
    let trimmed = value.trim();
    trimmed
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .or_else(|| {
            trimmed
                .strip_prefix('\'')
                .and_then(|v| v.strip_suffix('\''))
        })
        .unwrap_or(trimmed)
        .trim()
        .to_string()
}

fn frontmatter_value(contents: &str, key: &str) -> Option<String> {
    let mut lines = contents.lines();
    if lines.next()?.trim() != "---" {
        return None;
    }
    for line in lines {
        let line = line.trim();
        if line == "---" {
            break;
        }
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        if k.trim() == key {
            let value = clean_frontmatter_value(v);
            if !value.is_empty() {
                return Some(value);
            }
        }
    }
    None
}

fn collect_skill_files(dir: &Path, depth: usize, files: &mut Vec<PathBuf>) {
    if depth == 0 || !dir.is_dir() {
        return;
    }
    let skill = dir.join("SKILL.md");
    if skill.is_file() {
        files.push(skill);
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') && name != ".system" {
            continue;
        }
        collect_skill_files(&path, depth - 1, files);
    }
}

fn codex_skill_roots(config_dir: &Path) -> Vec<PathBuf> {
    let mut roots = skills::backend_skill_roots();
    roots.push(skills::skills_dir(config_dir));
    roots.sort();
    roots.dedup();
    roots
}

fn list_skill_commands(
    roots: Vec<PathBuf>,
    fallback_description: &str,
    name_prefix: &str,
    input_prefix: &str,
) -> Vec<Value> {
    let mut files = Vec::new();
    for root in roots {
        collect_skill_files(&root, 4, &mut files);
    }

    let mut skills: HashMap<String, Value> = HashMap::new();
    for file in files {
        let Ok(contents) = std::fs::read_to_string(&file) else {
            continue;
        };
        let fallback = file
            .parent()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let name = frontmatter_value(&contents, "name").unwrap_or(fallback);
        if name.is_empty() {
            continue;
        }
        let description = frontmatter_value(&contents, "description")
            .unwrap_or_else(|| fallback_description.to_string());
        let command_name = format!("{name_prefix}{name}");
        skills.entry(command_name.clone()).or_insert_with(|| {
            json!({
                "name": command_name,
                "description": description,
                "kind": "skill",
                "input": format!("{input_prefix}{name} ")
            })
        });
    }

    let mut values: Vec<_> = skills.into_values().collect();
    values.sort_by(|a, b| {
        a["name"]
            .as_str()
            .unwrap_or_default()
            .cmp(b["name"].as_str().unwrap_or_default())
    });
    values
}

fn list_codex_skill_commands(config_dir: &Path) -> Vec<Value> {
    list_skill_commands(codex_skill_roots(config_dir), "Codex skill", "", "$")
}

fn list_lyra_skill_commands(config_dir: &Path) -> Vec<Value> {
    list_skill_commands(
        vec![config_dir.join("alkaid").join("skills")],
        "Lyra skill",
        "skill:",
        "/skill:",
    )
}

#[cfg(test)]
mod slash_skill_command_tests {
    use super::*;

    #[test]
    fn lyra_skills_use_pi_slash_command_syntax() {
        let root = std::env::temp_dir().join(format!("nova-lyra-slash-{}", uuid::Uuid::new_v4()));
        let skill_dir = root.join("alkaid").join("skills").join("review");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: review\ndescription: Review code\n---\n",
        )
        .unwrap();

        let commands = list_lyra_skill_commands(&root);
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0]["name"], "skill:review");
        assert_eq!(commands[0]["input"], "/skill:review ");
        assert_eq!(commands[0]["kind"], "skill");
        let _ = std::fs::remove_dir_all(root);
    }
}

/// worktree 工作目录的根：优先设置里的自定义路径，为空则回退应用数据目录下的 worktrees/。
fn worktree_base(state: &AppState) -> PathBuf {
    let custom = state
        .settings
        .lock()
        .unwrap()
        .worktree_dir
        .trim()
        .to_string();
    if custom.is_empty() {
        state.config_dir.join("worktrees")
    } else {
        PathBuf::from(custom)
    }
}

/// 校验 worktree 的分支参数（branch=新分支名，可空；base=基于的分支/提交）：
/// - branch 非空：走新建分支路径，需通过 git 合法性校验且不与已有分支冲突；
/// - branch 空：直接检出 base 指定的已有分支（不新建），base 必填且不能已被其它工作树检出。
/// 通过后返回 owned_branch（分支是否由 Nova 新建）。
fn precheck_worktree_branch(repo: &str, branch: &str, base: &str) -> Result<bool, String> {
    if branch.is_empty() {
        if base.is_empty() {
            return Err("请填写新分支名，或选择要直接使用的分支".into());
        }
        if let Some(at) = gitwt::branch_checked_out(repo, base) {
            return Err(format!(
                "分支「{base}」已在 {at} 检出，git 不允许同一分支同时检出到两个工作目录。请换一个分支或填写新分支名。"
            ));
        }
        return Ok(false);
    }
    if !gitwt::valid_branch(branch) {
        return Err(format!("分支名不合法：{branch}"));
    }
    if let Some(msg) = gitwt::branch_conflict(repo, branch) {
        return Err(msg);
    }
    Ok(true)
}

/// 同一仓库和分支已有 Nova 管理的 worktree 时直接复用，避免再次 `git worktree add`。
/// 分支已被 git 检出到链接工作树但 Nova 未登记（如外部手动 `git worktree add`）时，
/// 静默登记该工作目录并复用；检出目录是主工作区或已不存在时返回 None，由调用方报错。
/// 返回 (worktree, 是否为本次新登记)。
fn reuse_worktree(
    state: &AppState,
    repo: &str,
    branch: &str,
    thread_id: Option<String>,
    roaming: bool,
) -> Option<(Worktree, bool)> {
    if branch.is_empty() {
        return None;
    }
    let checked_out = gitwt::branch_checked_out(repo, branch)?;
    let mut store = state.worktrees.lock().unwrap();
    let record = store.worktrees.iter_mut().find(|record| {
        (record.repo == repo || Path::new(&record.path) == Path::new(repo))
            && record.branch == branch
            && Path::new(&record.path).is_dir()
            && Path::new(&record.path) == Path::new(&checked_out)
    });
    if let Some(record) = record {
        record.thread_id = thread_id;
        record.roaming = roaming;
        let worktree = Worktree {
            repo: record.repo.clone(),
            path: record.path.clone(),
            branch: record.branch.clone(),
        };
        store.save();
        return Some((worktree, false));
    }
    // 未登记：静默接管已存在的链接工作树（主工作区不接管）
    if Path::new(&checked_out) == Path::new(repo) || !Path::new(&checked_out).is_dir() {
        return None;
    }
    let worktree = Worktree {
        repo: repo.to_string(),
        path: checked_out.clone(),
        branch: branch.to_string(),
    };
    store.add(WorktreeRecord {
        id: uuid::Uuid::new_v4().to_string(),
        repo: repo.to_string(),
        path: checked_out,
        branch: branch.to_string(),
        thread_id,
        roaming,
        owned_branch: false,
        created_at: now_ms(),
    });
    Some((worktree, true))
}

/// 为 dir 所在 git 仓库创建一个 worktree 并登记到 WorktreeStore：
/// branch 非空 = 基于 base（空则 HEAD）新建分支；branch 空 = 直接检出 base 所选分支。
/// 返回 worktree 信息（含工作目录 path）；roaming=true 表示漫游 host 侧代建。
pub fn create_worktree_for(
    state: &AppState,
    dir: &str,
    branch: Option<&str>,
    base_branch: Option<&str>,
    thread_id: Option<String>,
    roaming: bool,
) -> Result<Worktree, String> {
    if !gitwt::is_repo(dir) {
        return Err(format!("不是 git 仓库，无法创建 worktree：{dir}"));
    }
    let repo = gitwt::repo_root(dir)?;
    let branch = branch.map(|s| s.trim()).unwrap_or("").to_string();
    // 基于哪个分支/提交创建（空 = 当前 HEAD，仅新建分支时允许为空）
    let base_branch = base_branch.map(|s| s.trim()).unwrap_or("").to_string();
    let requested_branch = if branch.is_empty() {
        &base_branch
    } else {
        &branch
    };
    if let Some((worktree, _adopted)) =
        reuse_worktree(state, &repo, requested_branch, thread_id.clone(), roaming)
    {
        return Ok(worktree);
    }
    let owned_branch = precheck_worktree_branch(&repo, &branch, &base_branch)?;
    // 展示/记录用的分支名：新建用新分支，直接检出用所选分支
    let display_branch = if owned_branch {
        branch.clone()
    } else {
        base_branch.clone()
    };
    let id = uuid::Uuid::new_v4().to_string();
    let root = worktree_base(state);
    std::fs::create_dir_all(&root).map_err(|e| format!("创建 worktree 根目录失败：{e}"))?;
    // 用 uuid 作目录名：分支名可能含「/」不适合直接做目录名
    let path_str = root.join(&id).to_string_lossy().to_string();
    gitwt::add(&repo, &path_str, &branch, &base_branch)?;
    state.worktrees.lock().unwrap().add(WorktreeRecord {
        id,
        repo: repo.clone(),
        path: path_str.clone(),
        branch: display_branch.clone(),
        thread_id,
        roaming,
        owned_branch,
        created_at: now_ms(),
    });
    Ok(Worktree {
        repo,
        path: path_str,
        branch: display_branch,
    })
}

#[tauri::command]
async fn create_thread(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    cwd: String,
    agent_kind: Option<AgentKind>,
    model: Option<String>,
    mode: Option<String>,
    reasoning_effort: Option<String>,
    ephemeral: Option<bool>,
    worktree: Option<bool>,
    worktree_branch: Option<String>,
    worktree_base: Option<String>,
    clue_card_id: Option<String>,
    parent_thread_id: Option<String>,
) -> Result<Thread, String> {
    let dir = std::path::Path::new(&cwd);
    if !dir.is_dir() {
        return Err(format!("目录不存在：{cwd}"));
    }
    // 未显式指定模式时落到设置中的默认会话模式
    let default_mode = {
        let s = state.settings.lock().unwrap();
        s.default_mode.clone()
    };
    let agent_kind = agent_kind.unwrap_or(AgentKind::Devin);
    let mut thread = Thread::new(
        cwd.clone(),
        agent_kind,
        model.filter(|s| !s.is_empty()),
        mode.filter(|s| !s.is_empty())
            .or(Some(default_mode).filter(|s| !s.is_empty())),
        reasoning_effort.filter(|s| !s.is_empty()),
        ephemeral.unwrap_or(false),
    );
    thread.parent_thread_id = parent_thread_id.filter(|id| !id.trim().is_empty());
    if let Some(card_id) = clue_card_id.filter(|value| !value.trim().is_empty()) {
        let snapshot = if state.relay.is_configured() {
            state.relay.clue_context(&card_id).await?
        } else {
            state.clues.lock().unwrap().snapshot(&card_id)?
        };
        thread.active_clue_card_id = Some(card_id);
        thread.clue_context = Some(snapshot);
    }
    // worktree：在独立工作目录 + 分支中执行，不动主工作区。
    // 大仓库 `git worktree add` 很慢，改为后台创建：会话先落库返回、前端立即进入，
    // 就绪后再把 cwd 切到 worktree 并由前端补发首条提示词，避免卡住界面。
    if worktree.unwrap_or(false) {
        // 同步快速预检（失败立即返回，不产生半成品会话）
        if !gitwt::is_repo(&cwd) {
            return Err(format!("不是 git 仓库，无法创建 worktree：{cwd}"));
        }
        let repo = gitwt::repo_root(&cwd)?;
        let branch = worktree_branch
            .as_deref()
            .map(|s| s.trim())
            .unwrap_or("")
            .to_string();
        let base = worktree_base
            .as_deref()
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        let requested_branch = if branch.is_empty() { &base } else { &branch };
        if let Some((worktree, adopted)) = reuse_worktree(
            state.inner(),
            &repo,
            requested_branch,
            Some(thread.id.clone()),
            false,
        ) {
            thread.cwd = worktree.path.clone();
            thread.worktree = Some(worktree);
            thread.push_system(
                if adopted {
                    format!("已静默添加现有工作目录并切换：{}，开始执行", thread.cwd)
                } else {
                    "已复用现有 git worktree，开始执行".to_string()
                },
                "info",
            );
            {
                let mut store = state.store.lock().unwrap();
                store.threads.push(thread.clone());
                store.save();
            }
            if !repo.contains(SCRATCH_MARK) {
                state.projects.lock().unwrap().touch(&repo);
                state.relay.publish_folders();
            }
            let _ = app.emit(acp::EV_THREADS, json!({}));
            return Ok(thread);
        }
        let owned_branch = precheck_worktree_branch(&repo, &branch, &base)?;
        let display_branch = if owned_branch {
            branch.clone()
        } else {
            base.clone()
        };
        let wt_id = uuid::Uuid::new_v4().to_string();
        let root = crate::worktree_base(state.inner());
        std::fs::create_dir_all(&root).map_err(|e| format!("创建 worktree 根目录失败：{e}"))?;
        let path_str = root.join(&wt_id).to_string_lossy().to_string();

        // 会话先落库返回：cwd 暂用源仓库（有效目录），worktree 记录最终路径。
        thread.worktree = Some(Worktree {
            repo: repo.clone(),
            path: path_str.clone(),
            branch: display_branch.clone(),
        });
        thread.push_system(
            if owned_branch {
                format!("⏳ 正在后台创建 git worktree（新分支 {display_branch}）…")
            } else {
                format!("⏳ 正在后台创建 git worktree（直接使用分支 {display_branch}）…")
            },
            "info",
        );
        let thread_id = thread.id.clone();
        {
            let mut store = state.store.lock().unwrap();
            store.threads.push(thread.clone());
            store.save();
        }
        if !repo.contains(SCRATCH_MARK) {
            state.projects.lock().unwrap().touch(&repo);
            state.relay.publish_folders();
        }
        let _ = app.emit(acp::EV_THREADS, json!({}));

        // 后台执行耗时的 git worktree add，完成/失败回写会话并通知前端
        let app_bg = app.clone();
        std::thread::spawn(move || {
            let state = app_bg.state::<AppState>();
            match gitwt::add(&repo, &path_str, &branch, &base) {
                Ok(()) => {
                    state.worktrees.lock().unwrap().add(WorktreeRecord {
                        id: wt_id,
                        repo: repo.clone(),
                        path: path_str.clone(),
                        branch: display_branch.clone(),
                        thread_id: Some(thread_id.clone()),
                        roaming: false,
                        owned_branch,
                        created_at: now_ms(),
                    });
                    let item = {
                        let mut store = state.store.lock().unwrap();
                        let it = store.get_mut(&thread_id).map(|t| {
                            t.cwd = path_str.clone();
                            t.push_system("✅ worktree 就绪，开始执行".into(), "info")
                        });
                        store.save();
                        it
                    };
                    if let Some(it) = item {
                        let _ = app_bg.emit(
                            acp::EV_UPDATE,
                            json!({ "threadId": thread_id, "op": { "t": "upsert", "item": it } }),
                        );
                    }
                    let _ = app_bg.emit("acp:worktree-ready", json!({ "threadId": thread_id }));
                }
                Err(e) => {
                    let item = {
                        let mut store = state.store.lock().unwrap();
                        let it = store.get_mut(&thread_id).map(|t| {
                            // 失败：清 worktree 标记、cwd 回退到源仓库，避免指向不存在目录
                            t.worktree = None;
                            t.cwd = repo.clone();
                            t.push_system(format!("❌ 创建 worktree 失败：{e}"), "error")
                        });
                        store.save();
                        it
                    };
                    if let Some(it) = item {
                        let _ = app_bg.emit(
                            acp::EV_UPDATE,
                            json!({ "threadId": thread_id, "op": { "t": "upsert", "item": it } }),
                        );
                    }
                    let _ = app_bg.emit(
                        "acp:worktree-failed",
                        json!({ "threadId": thread_id, "error": e }),
                    );
                }
            }
            let _ = app_bg.emit(acp::EV_THREADS, json!({}));
        });

        return Ok(thread);
    }

    // 普通会话：直接落库
    let project_dir = thread.cwd.clone();
    {
        let mut store = state.store.lock().unwrap();
        store.threads.push(thread.clone());
        store.save();
    }
    // 临时会话目录不进入最近项目列表
    if !project_dir.contains(SCRATCH_MARK) {
        state.projects.lock().unwrap().touch(&project_dir);
        // 项目列表变化后同步广播，供在线用户漫游选择
        state.relay.publish_folders();
    }
    let _ = app.emit(acp::EV_THREADS, json!({}));
    Ok(thread)
}

#[tauri::command]
fn create_stage_thread(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    source_thread_id: String,
    stage_index: usize,
    inherit_source_model: bool,
) -> Result<Thread, String> {
    let (cwd, source_id, source_agent_kind, source_model) = {
        let store = state.store.lock().unwrap();
        let source = store.get(&source_thread_id).ok_or("源会话不存在")?;
        (
            source.cwd.clone(),
            source.id.clone(),
            source.agent_kind.clone(),
            source.model.clone(),
        )
    };
    let (agent_kind, model) = if inherit_source_model {
        (source_agent_kind, source_model)
    } else {
        let settings = state.settings.lock().unwrap();
        let target = settings.stage_models.get(stage_index).ok_or_else(|| {
            if settings.stage_models.is_empty() {
                "尚未配置 Stage 模型，请先在设置中添加".to_string()
            } else {
                format!(
                    "未配置 /stage{} 对应的模型，当前共有 {} 个 Stage 模型",
                    stage_index + 1,
                    settings.stage_models.len()
                )
            }
        })?;
        let kind = AgentKind::from_str(target.agent_kind.trim())
            .ok_or_else(|| format!("Stage 模型后端无效：{}", target.agent_kind))?;
        let model = (!target.model.trim().is_empty()).then(|| target.model.trim().to_string());
        (kind, model)
    };
    if !state.agent_enabled(&agent_kind) {
        return Err(format!("Stage 模型后端 {} 已关闭", agent_kind.label()));
    }
    let mut thread = Thread::new(cwd, agent_kind, model, Some("build".into()), None, false);
    thread.parent_thread_id = Some(source_id.clone());
    thread.stage_source_thread_id = Some(source_id);
    thread.title = "[Stage] 新会话".into();
    {
        let mut store = state.store.lock().unwrap();
        store.threads.push(thread.clone());
        store.save();
    }
    let _ = app.emit(acp::EV_THREADS, json!({}));
    Ok(thread)
}

/// 为「不使用项目」的会话新建一个空的临时目录
#[tauri::command]
fn scratch_dir() -> Result<String, String> {
    let name = format!(
        "{}-{}",
        chrono::Local::now().format("%m%d-%H%M%S"),
        &uuid::Uuid::new_v4().to_string()[..4]
    );
    let dir = std::env::temp_dir().join(SCRATCH_MARK).join(name);
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建临时目录失败：{e}"))?;
    Ok(dir.to_string_lossy().to_string())
}

#[tauri::command]
fn directory_exists(path: String) -> bool {
    Path::new(path.trim()).is_dir()
}

/// 资源管理器复制的文件绝对路径。Ctrl+Shift+V 粘贴路径用；WebView2 的 JS 剪贴板通常拿不到。
#[tauri::command]
fn clipboard_file_paths() -> Vec<String> {
    clipboard::file_paths()
}

/// 判断目录是否 git 仓库：前端据此决定「在 worktree 中执行」开关是否可用。
/// git 探测涉及进程启动和磁盘访问，必须离开 Tauri 命令线程，避免选择项目时卡住界面。
#[tauri::command]
async fn is_git_repo(path: String) -> bool {
    tauri::async_runtime::spawn_blocking(move || gitwt::is_repo(&path))
        .await
        .unwrap_or(false)
}

/// worktree「基于分支」下拉的数据：当前分支 + 本地分支列表
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct BranchList {
    current: String,
    branches: Vec<String>,
}

/// 列出目录所在 git 仓库的本地分支 + 当前分支（本地会话的「基于分支」下拉用）
#[tauri::command]
fn list_branches(path: String) -> Result<BranchList, String> {
    let branches = gitwt::list_branches(&path)?;
    let current = gitwt::current_branch(&path);
    Ok(BranchList { current, branches })
}

/// guest：请求对端某目录的本地分支列表（漫游会话的「基于分支」下拉，用对方仓库分支）。
/// 结果经 relay:peer-branches 事件异步回传前端。
#[tauri::command]
fn request_peer_branches(state: State<'_, AppState>, peer_token: String, folder: String) {
    state.relay.request_peer_branches(peer_token, folder);
}

/// 列出所有已创建的 worktree 记录（设置面板手动管理用）
#[tauri::command]
fn list_worktrees(state: State<'_, AppState>) -> Vec<WorktreeRecord> {
    let mut list = state.worktrees.lock().unwrap().list();
    list.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    list
}

/// 移除一条 worktree：删除工作目录（best-effort），可选连同分支一起删，最后清掉记录。
/// git 操作 best-effort：目录可能已被手动删/移动，不阻断记录清理。
/// 属于该工作目录的会话历史一并删除（目录都没了，留着只会指向不存在的路径）；
/// 有会话正在运行时拒绝移除，避免拔掉正在执行的 agent 的工作目录。
#[tauri::command]
fn remove_worktree(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    id: String,
    delete_branch: bool,
) -> Result<(), String> {
    let rec = state
        .worktrees
        .lock()
        .unwrap()
        .get(&id)
        .cloned()
        .ok_or("worktree 记录不存在")?;
    // 归属判定：会话工作目录就是该 worktree 目录，或会话的 worktree 标注指向它
    let doomed: Vec<String> = {
        let store = state.store.lock().unwrap();
        store
            .threads
            .iter()
            .filter(|t| {
                t.cwd == rec.path || t.worktree.as_ref().is_some_and(|w| w.path == rec.path)
            })
            .map(|t| t.id.clone())
            .collect()
    };
    if doomed.iter().any(|tid| running_by_id(&state, tid)) {
        return Err("该 worktree 关联的会话正在运行，请先停止再移除".into());
    }
    let _ = gitwt::remove(&rec.repo, &rec.path);
    // 直接检出用户已有分支的 worktree（owned_branch=false）不删分支：那不是 Nova 建的
    if delete_branch && rec.owned_branch {
        let _ = gitwt::delete_branch(&rec.repo, &rec.branch);
    }
    state.worktrees.lock().unwrap().remove(&id);
    if !doomed.is_empty() {
        remove_threads(&app, &state, doomed);
    }
    // worktree 目录没了，项目列表里对应条目也要跟着消失（可能曾被当项目选择过）
    state.projects.lock().unwrap().remove(&rec.path);
    state.relay.publish_folders();
    let _ = app.emit("projects:changed", json!({}));
    Ok(())
}

/// worktree 会话：把该会话的分支合并到目标分支。
/// 合并在「检出了目标分支的工作树」里执行（目标未被任何工作树检出时，先在主仓库检出它）。
/// 干净合并直接完成；出现冲突时**不回滚**，把冲突现场交给该会话的 AI 解决并完成合并提交。
/// 返回 "merged"（已合并）或 "conflict"（有冲突，已交给 AI 处理）。
#[tauri::command]
async fn merge_worktree_thread(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    thread_id: String,
    target_branch: String,
) -> Result<String, String> {
    let target = target_branch.trim().to_string();
    if target.is_empty() {
        return Err("请选择目标分支".into());
    }
    let (wt, agent_kind, is_guest) = {
        let store = state.store.lock().unwrap();
        let t = store.get(&thread_id).ok_or("会话不存在")?;
        (
            t.worktree.clone().ok_or("该会话没有关联 worktree")?,
            t.agent_kind.clone(),
            t.is_roaming_guest(),
        )
    };
    if is_guest || wt.path.is_empty() {
        return Err("漫游会话的 worktree 在对方机器上，无法在本机合并".into());
    }
    if running_by_id(&state, &thread_id) {
        return Err("会话正在运行，请先等它完成或手动停止，再合并".into());
    }
    if target == wt.branch {
        return Err("目标分支不能是 worktree 自己的分支".into());
    }
    // worktree 里有未提交改动 → 这些改动不会进入合并，先提醒用户处理，避免「合并了却少东西」。
    if !gitwt::is_clean(&wt.path)? {
        return Err(format!(
            "worktree 中还有未提交的改动（分支 {}）。请先让会话里的 AI 提交这些改动，再合并。",
            wt.branch
        ));
    }
    // 合并须在检出了目标分支的工作树里执行：已检出的直接用；未检出的在主仓库切过去（需干净）。
    let merge_dir = match gitwt::branch_checked_out(&wt.repo, &target) {
        Some(dir) => {
            if !gitwt::is_clean(&dir)? {
                return Err(format!(
                    "目标分支「{target}」检出在 {dir}，但该工作树有未提交改动。请先提交或暂存，再合并。"
                ));
            }
            dir
        }
        None => {
            if !gitwt::is_clean(&wt.repo)? {
                return Err(format!(
                    "需要先在主仓库检出目标分支「{target}」，但主工作区有未提交改动。请先提交或暂存，再合并。"
                ));
            }
            gitwt::checkout(&wt.repo, &target)?;
            wt.repo.clone()
        }
    };
    // git merge 可能较慢（大仓库），放到阻塞线程池执行
    let merge_dir2 = merge_dir.clone();
    let branch = wt.branch.clone();
    let merge_result =
        tauri::async_runtime::spawn_blocking(move || gitwt::merge(&merge_dir2, &branch))
            .await
            .map_err(|e| format!("合并任务异常：{e}"))?;
    match merge_result {
        Ok(()) => {
            let item = {
                let mut store = state.store.lock().unwrap();
                let it = store.get_mut(&thread_id).map(|t| {
                    t.push_system(
                        format!("✅ 已将分支 {} 合并到 {target}（{merge_dir}）", wt.branch),
                        "info",
                    )
                });
                store.save();
                it
            };
            if let Some(item) = item {
                let _ = app.emit(
                    acp::EV_UPDATE,
                    json!({ "threadId": thread_id, "op": { "t": "upsert", "item": item } }),
                );
            }
            Ok("merged".into())
        }
        Err(e) => {
            if !gitwt::has_conflicts(&merge_dir) {
                // 非冲突性失败（如网络/对象损坏）：确保不留半截合并现场
                gitwt::merge_abort(&merge_dir);
                return Err(format!("合并失败：{e}"));
            }
            // 有冲突：保留现场，交给该会话的 AI 解决并完成合并提交。
            let prompt = format!(
                "我刚在目录 {merge_dir} 执行了 `git merge {}`（把你这个 worktree 的分支合并到 {target}），出现了合并冲突，合并尚未提交。请你解决全部冲突并完成这次合并：\n\
                 1. 用 `git -C \"{merge_dir}\" status` 与 `git -C \"{merge_dir}\" diff` 查看冲突文件与两边改动；\n\
                 2. 逐个冲突文件结合两边改动的意图正确合并（不要无脑取单边，除非确认另一边的改动已无意义）；\n\
                 3. 解决后在该目录 `git add` 全部冲突文件并 `git commit` 完成合并（用默认合并提交信息即可）；\n\
                 4. 最后简要汇报每处冲突你是如何取舍的。\n\
                 注意：不要 rebase、不要强制推送、不要改动与本次冲突无关的内容。",
                wt.branch
            );
            match agent_kind {
                AgentKind::Lyra => {
                    let mgr = state.lyra.clone();
                    tauri::async_runtime::spawn(async move {
                        mgr.run_prompt(thread_id, prompt, vec![]).await;
                    });
                }
                AgentKind::Devin => {
                    let mgr = state.acp.clone();
                    tauri::async_runtime::spawn(async move {
                        mgr.run_prompt(thread_id, prompt, vec![]).await;
                    });
                }
                AgentKind::Codex | AgentKind::CodexPlus => {
                    let mgr = state.codexplus.clone();
                    tauri::async_runtime::spawn(async move {
                        mgr.run_prompt(thread_id, prompt, vec![]).await;
                    });
                }
                AgentKind::CodeBuddy | AgentKind::CodeBuddyPlus => {
                    let mgr = state.codebuddy.clone();
                    tauri::async_runtime::spawn(async move {
                        mgr.run_prompt(thread_id, prompt, vec![]).await;
                    });
                }
                AgentKind::ClaudeCode => {
                    let mgr = state.claudeplus.clone();
                    tauri::async_runtime::spawn(async move {
                        mgr.run_prompt(thread_id, prompt, vec![]).await;
                    });
                }
                AgentKind::Cursor => {
                    let mgr = state.cursorplus.clone();
                    tauri::async_runtime::spawn(async move {
                        mgr.run_prompt(thread_id, prompt, vec![]).await;
                    });
                }
                AgentKind::OpenCode | AgentKind::OpenCodePlus => {
                    let mgr = state.opencodeplus.clone();
                    tauri::async_runtime::spawn(async move {
                        mgr.run_prompt(thread_id, prompt, vec![]).await;
                    });
                }
            }
            Ok("conflict".into())
        }
    }
}

/// 查询 devin 剩余额度（日/周限额百分比等）
#[tauri::command]
async fn get_quota() -> Result<Value, String> {
    quota::fetch_quota().await
}

/// 查询模型费用信息（积分倍率/厂商/视觉支持），按 modelUid 索引
#[tauri::command]
async fn get_model_costs() -> Result<Value, String> {
    quota::fetch_model_costs().await
}

/// 检查更新：返回 { current, latest, hasUpdate, staged, size }
#[tauri::command]
async fn check_update(app: tauri::AppHandle) -> Result<Value, String> {
    updater::check(&app).await
}

/// 静默下载并暂存更新（进度走 update:progress 事件），不替换、不重启。
/// 已暂存同版本则直接返回 ready，避免重复下载。
#[tauri::command]
async fn download_staged_update(app: tauri::AppHandle) -> Result<Value, String> {
    updater::download_and_stage(app).await
}

/// 应用已暂存的更新：替换 exe 并重启
#[tauri::command]
async fn apply_staged_update(app: tauri::AppHandle) -> Result<(), String> {
    updater::apply_staged(app).await
}

/// 前端上报用户活动：记录最近操作时间与当前打开的会话，供静默升级判定空闲与恢复会话。
#[tauri::command]
fn report_activity(state: State<'_, AppState>, thread_id: Option<String>) {
    *state.last_activity_ms.lock().unwrap() = now_ms();
    *state.active_thread.lock().unwrap() = thread_id;
}

/// 前端在可靠主题已经写入 DOM 后调用；窗口此前始终隐藏，避免启动时露出默认底色。
#[tauri::command]
fn show_main_window(app: tauri::AppHandle) {
    updater::restore_window_on_launch(&app);
}

/// 读取并清除「升级重启前正在查看的会话」标记，返回需要自动恢复打开的会话 id。
/// 普通启动返回 null；仅升级（手动/静默）重启后才会返回上次的会话。
#[tauri::command]
fn take_restore_thread(app: tauri::AppHandle) -> Option<String> {
    updater::take_restore_thread(&app)
}

/// 前端启动时查询待签身份；无专属身份返回 null。每次启动（含升级重启）都签。
#[tauri::command]
fn signature_pending(app: tauri::AppHandle) -> Option<signature::SignatureIdentity> {
    let state = app.state::<AppState>();
    let settings = state.settings.lock().unwrap();
    signature::identity_for_token(&settings.relay_token)
}

fn expand_thread_tree_ids(state: &AppState, roots: &[String]) -> Vec<String> {
    let store = state.store.lock().unwrap();
    let mut seen: HashSet<String> = roots.iter().cloned().collect();
    let mut changed = true;
    while changed {
        changed = false;
        for t in &store.threads {
            if let Some(parent) = &t.parent_thread_id {
                if seen.contains(parent) && seen.insert(t.id.clone()) {
                    changed = true;
                }
            }
        }
    }
    seen.into_iter().collect()
}

#[tauri::command]
fn delete_thread(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    thread_id: String,
) -> Result<(), String> {
    let delete_ids = expand_thread_tree_ids(&state, std::slice::from_ref(&thread_id));
    if delete_ids.iter().any(|id| running_by_id(&state, id)) {
        return Err("会话树中有运行中的会话，请先停止".into());
    }
    remove_threads(&app, &state, delete_ids);
    Ok(())
}

fn tree_contains_starred_thread(tree: &[String], starred_thread_ids: &HashSet<String>) -> bool {
    tree.iter().any(|id| starred_thread_ids.contains(id))
}

/// 过滤掉运行中的会话树，按需保留含星标节点的完整树，返回可安全移除的完整树 id 集合。
fn collect_deletable_thread_ids(
    state: &AppState,
    thread_ids: Vec<String>,
    preserve_starred: bool,
) -> Vec<String> {
    let starred_thread_ids: HashSet<String> = if preserve_starred {
        let store = state.store.lock().unwrap();
        store
            .threads
            .iter()
            .filter(|thread| thread.starred)
            .map(|thread| thread.id.clone())
            .collect()
    } else {
        HashSet::new()
    };
    let roots: Vec<String> = thread_ids
        .into_iter()
        .filter(|id| !running_by_id(&state, id))
        .collect();
    let mut delete_set: HashSet<String> = HashSet::new();
    for root in roots {
        if delete_set.contains(&root) {
            continue;
        }
        let tree = expand_thread_tree_ids(&state, std::slice::from_ref(&root));
        if tree.iter().any(|id| running_by_id(&state, id)) {
            continue;
        }
        if preserve_starred && tree_contains_starred_thread(&tree, &starred_thread_ids) {
            continue;
        }
        delete_set.extend(tree);
    }
    delete_set.into_iter().collect()
}

fn valid_lyra_session_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
}

fn cleanup_lyra_session_files(
    data_dir: &Path,
    removed: &[Thread],
    retained_session_ids: &HashSet<String>,
) {
    let session_root = data_dir.join("alkaid").join("sessions");
    let tool_results_root = data_dir.join("alkaid").join("tool-results");
    let session_ids: HashSet<&str> = removed
        .iter()
        .filter(|thread| thread.agent_kind == AgentKind::Lyra)
        .filter_map(|thread| thread.acp_session_id.as_deref())
        .filter(|id| valid_lyra_session_id(id) && !retained_session_ids.contains(*id))
        .collect();
    for id in session_ids {
        for file_name in [format!("{id}.json"), format!("{id}.slim.json")] {
            let path = session_root.join(file_name);
            if let Err(error) = std::fs::remove_file(&path) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    eprintln!("[threads] 清理 Lyra session 文件 {} 失败：{error}", path.display());
                }
            }
        }
        let tool_results = tool_results_root.join(id);
        if let Err(error) = std::fs::remove_dir_all(&tool_results) {
            if error.kind() != std::io::ErrorKind::NotFound {
                eprintln!(
                    "[threads] 清理 Lyra session 工具结果 {} 失败：{error}",
                    tool_results.display()
                );
            }
        }
    }
}

/// 从正常会话存储移除，并清理关联的运行时与 Lyra session；返回完整会话快照供回收站持久化。
fn remove_threads(app: &tauri::AppHandle, state: &AppState, deletable: Vec<String>) -> Vec<Thread> {
    for id in &deletable {
        state.relay.notify_host_thread_deleted(id);
    }
    let (removed, retained_session_ids) = {
        let mut store = state.store.lock().unwrap();
        let (removed, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut store.threads)
            .into_iter()
            .partition(|thread| deletable.contains(&thread.id));
        let retained_session_ids: Vec<_> = kept
            .iter()
            .filter_map(|thread| thread.acp_session_id.clone())
            .collect();
        store.threads = kept;
        store.save();
        (removed, retained_session_ids)
    };
    for thread in &removed {
        let id = &thread.id;
        state.acp.forget_session_of_thread(id);
        state.codex.forget_session_of_thread(id);
        state.lyra.forget_session_of_thread(id);
        state.codexplus.forget_session_of_thread(id);
        state.codebuddy.forget_session_of_thread(id);
        state.claudeplus.forget_session_of_thread(id);
        state.cursorplus.forget_session_of_thread(id);
        state.opencodeplus.forget_session_of_thread(id);
        cleanup_borrowed_runtime(state, id);
    }
    // 自动清理的普通会话仍保留在回收站一个周期，session 要等彻底过期再删；
    // 手动删除、worktree 删除及训练会话清理则没有回收站，立即清理。
    let trashed_session_ids: HashSet<String> = state
        .thread_trash
        .lock()
        .unwrap()
        .session_ids()
        .into_iter()
        .collect();
    let retained_session_ids = retained_session_ids
        .into_iter()
        .chain(trashed_session_ids)
        .collect();
    cleanup_lyra_session_files(&state.config_dir, &removed, &retained_session_ids);
    let _ = app.emit(acp::EV_THREADS, json!({}));
    removed
}

/// 批量删除会话；运行中的自动跳过，返回实际删除数量。
fn delete_threads_impl(
    app: &tauri::AppHandle,
    state: &AppState,
    thread_ids: Vec<String>,
    preserve_starred: bool,
) -> usize {
    let deletable = collect_deletable_thread_ids(state, thread_ids, preserve_starred);
    remove_threads(app, state, deletable).len()
}

#[tauri::command]
fn delete_threads(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    thread_ids: Vec<String>,
) -> Result<usize, String> {
    Ok(delete_threads_impl(&app, &state, thread_ids, false))
}

/// 项目侧栏的一键清理：运行中及星标会话（含其所在树）均保留。
#[tauri::command]
fn delete_project_threads(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    thread_ids: Vec<String>,
) -> Result<usize, String> {
    Ok(delete_threads_impl(&app, &state, thread_ids, true))
}

/// 用配置的编辑器打开文件（可带行号）。
/// 临时会话只开文件；正式项目连同项目目录一起打开（vscode 系用 --goto，zed 用 path:line）。
#[tauri::command]
fn open_in_editor(
    state: State<'_, AppState>,
    thread_id: String,
    path: String,
    line: Option<u32>,
) -> Result<(), String> {
    let cwd = {
        let store = state.store.lock().unwrap();
        store.get(&thread_id).ok_or("线程不存在")?.cwd.clone()
    };
    let editor = {
        let s = state.settings.lock().unwrap();
        s.editor.trim().to_string()
    };
    if editor.is_empty() {
        return Err("未配置编辑器，请在设置中填写（如 cursor / code / zed）".into());
    }
    // 相对路径按线程工作目录解析
    let abs = {
        let p = std::path::Path::new(&path);
        if p.is_absolute() {
            path.clone()
        } else {
            std::path::Path::new(&cwd)
                .join(p)
                .to_string_lossy()
                .to_string()
        }
    };
    if !std::path::Path::new(&abs).exists() {
        return Err(format!("文件不存在：{abs}"));
    }
    let scratch = cwd.contains(SCRATCH_MARK);
    let in_project = std::fs::canonicalize(&abs)
        .ok()
        .zip(std::fs::canonicalize(&cwd).ok())
        .is_some_and(|(file, project)| file.starts_with(project));
    let loc = match line {
        Some(l) => format!("{abs}:{l}"),
        None => abs.clone(),
    };
    let mut args: Vec<String> = Vec::new();
    if editor.to_lowercase().contains("zed") {
        if !scratch && in_project {
            args.push(cwd.clone());
        }
        args.push(loc);
    } else {
        // vscode / cursor / windsurf 同源，支持 --goto file:line
        if !scratch && in_project {
            args.push(cwd.clone());
        }
        args.push("--goto".into());
        args.push(loc);
    }
    // Windows 下编辑器 CLI 多为 .cmd 垫片，必须经 cmd 启动
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let mut cmd = std::process::Command::new("cmd");
        cmd.arg("/C").arg(&editor).args(&args);
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        cmd.spawn().map_err(|e| format!("启动编辑器失败：{e}"))?;
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new(&editor)
            .args(&args)
            .spawn()
            .map_err(|e| format!("启动编辑器失败：{e}"))?;
    }
    Ok(())
}

fn open_path_default(abs: &std::path::Path) -> Result<(), String> {
    if !abs.is_file() {
        return Err(format!("文件不存在：{}", abs.to_string_lossy()));
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let mut cmd = std::process::Command::new("rundll32.exe");
        cmd.arg("url.dll,FileProtocolHandler").arg(abs);
        cmd.creation_flags(0x0800_0000);
        cmd.spawn().map_err(|e| format!("打开文件失败：{e}"))?;
    }
    #[cfg(target_os = "macos")]
    std::process::Command::new("open")
        .arg(abs)
        .spawn()
        .map_err(|e| format!("打开文件失败：{e}"))?;
    #[cfg(all(unix, not(target_os = "macos")))]
    std::process::Command::new("xdg-open")
        .arg(abs)
        .spawn()
        .map_err(|e| format!("打开文件失败：{e}"))?;
    Ok(())
}

/// 用系统默认程序打开线程工作目录中的文件，图片通常由图片查看器或浏览器处理。
#[tauri::command]
fn open_file_default(
    state: State<'_, AppState>,
    thread_id: String,
    path: String,
) -> Result<(), String> {
    let cwd = {
        let store = state.store.lock().unwrap();
        store.get(&thread_id).ok_or("线程不存在")?.cwd.clone()
    };
    let path = std::path::PathBuf::from(path);
    let abs = if path.is_absolute() {
        path
    } else {
        std::path::Path::new(&cwd).join(path)
    };
    open_path_default(&abs)
}

/// 打开线索附件。粘贴附件先还原到临时目录，路径附件直接交给系统默认程序。
#[tauri::command]
fn open_clue_attachment(
    name: String,
    data: Option<String>,
    path: Option<String>,
) -> Result<(), String> {
    let abs = if let Some(data) = data.filter(|value| !value.is_empty()) {
        use base64::Engine as _;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data)
            .map_err(|error| format!("附件数据损坏：{error}"))?;
        let safe_name = std::path::Path::new(&name)
            .file_name()
            .and_then(|value| value.to_str())
            .filter(|value| !value.is_empty())
            .unwrap_or("attachment");
        let dir = std::env::temp_dir().join("nova-clue-attachments");
        std::fs::create_dir_all(&dir).map_err(|error| format!("创建附件临时目录失败：{error}"))?;
        let abs = dir.join(format!("{}-{safe_name}", uuid::Uuid::new_v4()));
        std::fs::write(&abs, bytes).map_err(|error| format!("还原附件失败：{error}"))?;
        abs
    } else if let Some(path) = path.filter(|value| !value.is_empty()) {
        std::path::PathBuf::from(path)
    } else {
        return Err("附件没有可打开的数据".into());
    };
    open_path_default(&abs)
}

/// 按扩展名猜测附件 MIME 类型（选择本地文件作附件时使用）。
fn guess_attachment_mime_type(name: &str) -> String {
    let ext = std::path::Path::new(name)
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase())
        .unwrap_or_default();
    let mime = match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        "json" => "application/json",
        "md" => "text/markdown",
        "txt" => "text/plain",
        "html" => "text/html",
        "css" => "text/css",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        _ => "application/octet-stream",
    };
    mime.to_string()
}

/// 读取本地文件并转换为内嵌（base64）线索附件，供线索弹窗「添加附件」选择任意文件。
#[tauri::command]
fn read_local_attachment(path: String) -> Result<clues::ClueAttachment, String> {
    const MAX_SIZE: u64 = 20 * 1024 * 1024;
    let abs = std::path::PathBuf::from(&path);
    let meta = std::fs::metadata(&abs).map_err(|error| format!("读取文件失败：{error}"))?;
    if !meta.is_file() {
        return Err("只能选择文件作为附件".into());
    }
    if meta.len() > MAX_SIZE {
        return Err("附件过大：单个附件不能超过 20MB".into());
    }
    let bytes = std::fs::read(&abs).map_err(|error| format!("读取文件失败：{error}"))?;
    let name = abs
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("attachment")
        .to_string();
    use base64::Engine as _;
    let data = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Ok(clues::ClueAttachment {
        mime_type: guess_attachment_mime_type(&name),
        name,
        data: Some(data),
        uri: None,
        size: Some(meta.len()),
    })
}

/// 把线索附件保存到指定路径（下载）：内嵌 base64 数据先还原，路径附件直接复制。
#[tauri::command]
fn save_clue_attachment(
    data: Option<String>,
    path: Option<String>,
    target: String,
) -> Result<(), String> {
    let bytes = if let Some(data) = data.filter(|value| !value.is_empty()) {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD
            .decode(data)
            .map_err(|error| format!("附件数据损坏：{error}"))?
    } else if let Some(path) = path.filter(|value| !value.is_empty()) {
        std::fs::read(path).map_err(|error| format!("读取附件原文件失败：{error}"))?
    } else {
        return Err("附件没有可下载的数据".into());
    };
    let target = std::path::PathBuf::from(target);
    if let Some(parent) = target.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("创建下载目录失败：{error}"))?;
        }
    }
    std::fs::write(&target, bytes).map_err(|error| format!("保存附件失败：{error}"))
}

/// 撤销目标：一个文件回滚到本轮编辑前的内容
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RevertChange {
    pub path: String,
    /// 编辑前内容；None 表示文件原本不存在（撤销 = 删除）
    pub old_text: Option<String>,
    /// 期望的当前内容（最后一次编辑后的结果），用于冲突检测
    pub new_text: String,
}

/// 撤销一批文件改动（codex 风格撤销）。
/// 只有当前磁盘内容与编辑后内容一致才回滚，避免覆盖用户/后续轮次的修改。
#[tauri::command]
fn revert_file_changes(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    thread_id: String,
    changes: Vec<RevertChange>,
) -> Result<Value, String> {
    let cwd = {
        let store = state.store.lock().unwrap();
        store.get(&thread_id).ok_or("线程不存在")?.cwd.clone()
    };
    let norm = |s: &str| s.replace("\r\n", "\n");
    let mut reverted: Vec<String> = Vec::new();
    let mut conflicts: Vec<String> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    for ch in &changes {
        let p = std::path::Path::new(&ch.path);
        let abs = if p.is_absolute() {
            p.to_path_buf()
        } else {
            std::path::Path::new(&cwd).join(p)
        };
        let name = ch.path.clone();
        let current = std::fs::read_to_string(&abs).unwrap_or_default();
        // 行尾差异不算冲突（diff 文本与磁盘 CRLF/LF 可能不一致）
        if current != ch.new_text && norm(&current) != norm(&ch.new_text) {
            conflicts.push(name);
            continue;
        }
        let result = match &ch.old_text {
            None => std::fs::remove_file(&abs),
            Some(text) => std::fs::write(&abs, text),
        };
        match result {
            Ok(()) => reverted.push(name),
            Err(e) => errors.push(format!("{name}: {e}")),
        }
    }
    // 结果落一条系统消息，对话里可见
    {
        let mut msg = format!("已撤销 {} 个文件的改动", reverted.len());
        if !conflicts.is_empty() {
            msg.push_str(&format!(
                "；{} 个文件因已被后续修改跳过：{}",
                conflicts.len(),
                conflicts.join("、")
            ));
        }
        if !errors.is_empty() {
            msg.push_str(&format!("；失败：{}", errors.join("、")));
        }
        let level = if conflicts.is_empty() && errors.is_empty() {
            "info"
        } else {
            "warn"
        };
        let mut store = state.store.lock().unwrap();
        if let Some(thread) = store.get_mut(&thread_id) {
            let item = thread.push_system(msg, level);
            store.save();
            let _ = app.emit(
                acp::EV_UPDATE,
                json!({ "threadId": thread_id, "op": { "t": "upsert", "item": item } }),
            );
        }
    }
    let _ = app.emit(acp::EV_THREADS, json!({}));
    Ok(json!({ "reverted": reverted, "conflicts": conflicts, "errors": errors }))
}

/// 在资源管理器 / Finder 中打开目录，或在目录中选中文件
#[tauri::command]
fn open_in_explorer(path: String) -> Result<(), String> {
    let path = std::path::PathBuf::from(path);
    #[cfg(windows)]
    {
        if path.is_dir() {
            std::process::Command::new("explorer")
                .arg(&path)
                .spawn()
                .map_err(|e| format!("打开资源管理器失败：{e}"))?;
            return Ok(());
        }
        if path.is_file() {
            std::process::Command::new("explorer")
                .arg(format!("/select,{}", path.to_string_lossy()))
                .spawn()
                .map_err(|e| format!("打开资源管理器失败：{e}"))?;
            return Ok(());
        }
        if let Some(parent) = path.parent().filter(|p| p.is_dir()) {
            std::process::Command::new("explorer")
                .arg(parent)
                .spawn()
                .map_err(|e| format!("打开资源管理器失败：{e}"))?;
            return Ok(());
        }
        return Err(format!("路径不存在：{}", path.to_string_lossy()));
    }
    #[cfg(target_os = "macos")]
    {
        if path.is_file() {
            std::process::Command::new("open")
                .args(["-R", &path.to_string_lossy()])
                .spawn()
                .map_err(|e| format!("打开 Finder 失败：{e}"))?;
            return Ok(());
        }
        let target = if path.is_dir() {
            path.as_path()
        } else if let Some(parent) = path.parent().filter(|p| p.is_dir()) {
            parent
        } else {
            return Err(format!("路径不存在：{}", path.to_string_lossy()));
        };
        std::process::Command::new("open")
            .arg(target)
            .spawn()
            .map_err(|e| format!("打开 Finder 失败：{e}"))?;
        return Ok(());
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let target = if path.is_dir() {
            path.as_path()
        } else if path.is_file() {
            path.parent().unwrap_or(path.as_path())
        } else if let Some(parent) = path.parent().filter(|p| p.is_dir()) {
            parent
        } else {
            return Err(format!("路径不存在：{}", path.to_string_lossy()));
        };
        std::process::Command::new("xdg-open")
            .arg(target)
            .spawn()
            .map_err(|e| format!("打开文件管理器失败：{e}"))?;
        Ok(())
    }
}

/// 在终端中打开目录：Windows 优先 Windows Terminal；macOS 用 Terminal.app
#[tauri::command]
fn open_in_terminal(path: String) -> Result<(), String> {
    if !std::path::Path::new(&path).is_dir() {
        return Err(format!("目录不存在：{path}"));
    }
    #[cfg(windows)]
    {
        if std::process::Command::new("wt.exe")
            .args(["-d", &path])
            .spawn()
            .is_ok()
        {
            return Ok(());
        }
        let mut cmd = std::process::Command::new("cmd");
        cmd.args(["/C", "start", "cmd", "/K", "cd", "/d", &path]);
        // 外层 cmd 不要弹出自己的控制台窗口，只留 start 打开的那个
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        cmd.spawn().map_err(|e| format!("打开终端失败：{e}"))?;
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    {
        // AppleScript：在目标目录开新 Terminal 窗口并激活
        let escaped = path.replace('\\', "\\\\").replace('"', "\\\"");
        let script = format!(
            "tell application \"Terminal\" to do script \"cd \\\"{escaped}\\\" && clear\"\ntell application \"Terminal\" to activate"
        );
        std::process::Command::new("osascript")
            .args(["-e", &script])
            .spawn()
            .map_err(|e| format!("打开终端失败：{e}"))?;
        return Ok(());
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        for term in ["x-terminal-emulator", "gnome-terminal", "konsole", "xterm"] {
            let mut cmd = std::process::Command::new(term);
            if term == "gnome-terminal" {
                cmd.args(["--working-directory", &path]);
            } else {
                cmd.current_dir(&path);
            }
            if cmd.spawn().is_ok() {
                return Ok(());
            }
        }
        Err("未找到可用终端".into())
    }
}

/// 用系统默认浏览器打开外部链接，避免 WebView 被导航到外部页面
#[tauri::command]
fn open_url(url: String) -> Result<(), String> {
    let url = url.trim().to_string();
    let lower = url.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return Err("只支持打开 http/https 链接".into());
    }
    if url.chars().any(|c| c.is_control()) {
        return Err("链接包含非法字符".into());
    }

    #[cfg(windows)]
    {
        let mut cmd = std::process::Command::new("rundll32.exe");
        cmd.args(["url.dll,FileProtocolHandler", &url]);
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        cmd.spawn().map_err(|e| format!("打开浏览器失败：{e}"))?;
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(&url)
            .spawn()
            .map_err(|e| format!("打开浏览器失败：{e}"))?;
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::process::Command::new("xdg-open")
            .arg(&url)
            .spawn()
            .map_err(|e| format!("打开浏览器失败：{e}"))?;
    }
    Ok(())
}

#[tauri::command]
fn set_prompt_queue_pending(thread_id: String, pending: bool) {
    sys_notify::set_prompt_queue_pending(&thread_id, pending);
}

#[tauri::command]
fn notify_fire_done(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    thread_id: String,
    success: bool,
) -> Result<(), String> {
    let title = {
        let store = state.store.lock().unwrap();
        let thread = store.get(&thread_id).ok_or("线程不存在")?;
        if !thread.title.starts_with("[Fire]") {
            return Err("不是 Fire 会话".into());
        }
        thread.title.clone()
    };
    let body = if success {
        "Fire 目标已完成，点击查看结果"
    } else {
        "Fire 已结束，但目标仍未通过验收"
    };
    sys_notify::notify_thread_done_unfiltered(&app, &thread_id, &title, body, acp::EV_NOTIFY_OPEN);
    Ok(())
}

#[tauri::command]
fn notify_workflow_done(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    thread_id: String,
    success: bool,
) -> Result<(), String> {
    let title = {
        let store = state.store.lock().unwrap();
        let thread = store.get(&thread_id).ok_or("线程不存在")?;
        if !(thread.title.starts_with("[Fire]") || thread.title.starts_with("[WF]")) {
            return Err("不是工作流会话".into());
        }
        thread.title.clone()
    };
    let body = if success {
        "工作流已完成，点击查看结果"
    } else {
        "工作流已结束，但未通过最终阶段"
    };
    sys_notify::notify_thread_done_unfiltered(&app, &thread_id, &title, body, acp::EV_NOTIFY_OPEN);
    Ok(())
}

/// 向会话追加一条系统提示（工作流预览、错误提示等由前端渲染的结构化内容）。
#[tauri::command]
fn push_system_item(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    thread_id: String,
    text: String,
    level: String,
) -> Result<(), String> {
    let item = {
        let mut store = state.store.lock().unwrap();
        let thread = store.get_mut(&thread_id).ok_or("会话不存在")?;
        let item = thread.push_system(text, &level);
        store.save();
        item
    };
    let _ = app.emit(
        acp::EV_UPDATE,
        json!({ "threadId": thread_id, "op": { "t": "upsert", "item": item } }),
    );
    Ok(())
}

#[tauri::command]
fn create_time_machine_checkpoint(
    state: State<'_, AppState>,
    thread_id: String,
) -> Result<time_machine::TimelineView, String> {
    let _guard = state.time_machine_lock.lock().unwrap();
    let thread = {
        let store = state.store.lock().unwrap();
        let thread = store.get(&thread_id).ok_or("会话不存在")?;
        if is_running(&state, thread) {
            return Err("请等待当前会话执行结束后再创建时间点".into());
        }
        thread.clone()
    };
    let capture_workspace = state.settings.lock().unwrap().checkpoint_enabled;
    time_machine::create_checkpoint(&state.config_dir, &thread, capture_workspace)
}

#[tauri::command]
fn get_time_machine_timeline(
    state: State<'_, AppState>,
    thread_id: String,
) -> Result<Option<time_machine::TimelineView>, String> {
    let _guard = state.time_machine_lock.lock().unwrap();
    time_machine::get_timeline(&state.config_dir, &thread_id)
}

#[tauri::command]
fn get_time_machine_training_digest(
    state: State<'_, AppState>,
    thread_id: String,
) -> Result<time_machine::TrainingDigest, String> {
    let _guard = state.time_machine_lock.lock().unwrap();
    time_machine::timeline_training_digest(&state.config_dir, &thread_id)
}

#[tauri::command]
fn set_time_machine_checkpoint_outcome(
    state: State<'_, AppState>,
    thread_id: String,
    checkpoint_id: String,
    outcome: Option<String>,
) -> Result<time_machine::TimelineView, String> {
    let _guard = state.time_machine_lock.lock().unwrap();
    time_machine::set_checkpoint_outcome(&state.config_dir, &thread_id, &checkpoint_id, outcome)
}

#[tauri::command]
fn get_time_machine_checkpoint_preview(
    state: State<'_, AppState>,
    thread_id: String,
    checkpoint_id: String,
) -> Result<Thread, String> {
    let _guard = state.time_machine_lock.lock().unwrap();
    time_machine::checkpoint_preview(&state.config_dir, &thread_id, &checkpoint_id)
}

#[tauri::command]
fn restore_time_machine_checkpoint(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    thread_id: String,
    checkpoint_id: String,
) -> Result<time_machine::RestoreResult, String> {
    let _guard = state.time_machine_lock.lock().unwrap();
    let current = {
        let store = state.store.lock().unwrap();
        let thread = store.get(&thread_id).ok_or("会话不存在")?;
        if is_running(&state, thread) {
            return Err("请等待当前会话执行结束后再跳转".into());
        }
        thread.clone()
    };
    // 漫游 guest 的工作目录在对端；本机只恢复会话镜像，文件时间点由 host 管理。
    let restore_files =
        state.settings.lock().unwrap().checkpoint_enabled && !current.is_roaming_guest();
    let (thread, result) = time_machine::restore_checkpoint(
        &state.config_dir,
        &checkpoint_id,
        &current,
        restore_files,
    )?;
    {
        let mut store = state.store.lock().unwrap();
        if current.is_roaming_guest() {
            let existing = store.get_mut(&current.id).ok_or("漫游会话不存在")?;
            *existing = thread;
        } else {
            store.threads.push(thread);
        }
        store.save();
    }
    let _ = app.emit(acp::EV_THREADS, json!({}));
    Ok(result)
}

#[tauri::command]
fn delete_time_machine_context(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    thread_id: String,
    prompts: Vec<time_machine::PromptSummary>,
) -> Result<time_machine::RestoreResult, String> {
    if prompts.is_empty() {
        return Err("没有可删除的上下文节点".into());
    }
    if running_by_id(&state, &thread_id) {
        return Err("会话正在运行，请先停止".into());
    }
    let _guard = state.time_machine_lock.lock().unwrap();
    let capture_workspace = state.settings.lock().unwrap().checkpoint_enabled;
    let (agent_kind, timeline) = {
        let mut store = state.store.lock().unwrap();
        let current = store.get(&thread_id).ok_or("会话不存在")?;
        if current.is_roaming_guest() {
            return Err("漫游会话暂不支持手动删除上下文".into());
        }
        let mut edited = current.clone();
        if time_machine::remove_prompt_turns(&mut edited, &prompts) == 0 {
            return Err("所选上下文节点已不存在，请刷新世界线后重试".into());
        }
        edited.acp_session_id = None;
        edited.provider_checkpoints.clear();
        edited.pending_native_restore = None;
        edited.codex_usage_snapshot = None;
        edited.handoff_from = edited
            .items
            .iter()
            .any(|item| matches!(item, Item::User { .. }))
            .then(|| edited.agent_kind.clone());
        if !edited
            .items
            .iter()
            .any(|item| matches!(item, Item::User { .. }))
        {
            edited.title = "新会话".into();
        }
        let timeline = time_machine::rewrite_after_context_edit(
            &state.config_dir,
            &edited,
            &prompts,
            capture_workspace,
        )?;
        let agent_kind = edited.agent_kind.clone();
        *store.get_mut(&thread_id).ok_or("会话不存在")? = edited;
        store.save();
        (agent_kind, timeline)
    };

    // Lyra / Cursor 的下一轮会从编辑后的 transcript 直接重建精简上下文；其余后端
    // 作废原生 session，并通过一次无感接力在新 session 中继续。旧压缩摘要也随之失效。
    match agent_kind {
        AgentKind::Lyra => state.lyra.forget_session_of_thread(&thread_id),
        AgentKind::Devin => state.acp.forget_session_of_thread(&thread_id),
        AgentKind::Codex | AgentKind::CodexPlus => {
            state.codexplus.forget_session_of_thread(&thread_id)
        }
        AgentKind::CodeBuddy | AgentKind::CodeBuddyPlus => {
            state.codebuddy.forget_session_of_thread(&thread_id)
        }
        AgentKind::ClaudeCode => state.claudeplus.forget_session_of_thread(&thread_id),
        AgentKind::Cursor => state.cursorplus.forget_session_of_thread(&thread_id),
        AgentKind::OpenCode | AgentKind::OpenCodePlus => {
            state.opencodeplus.forget_session_of_thread(&thread_id)
        }
    }
    if let Some(runtime) = state.borrowed_runtime(&thread_id) {
        match runtime.manager {
            BorrowedManager::Acp(manager) => manager.forget_session_of_thread(&thread_id),
            BorrowedManager::Sdk(manager) => manager.forget_session_of_thread(&thread_id),
            BorrowedManager::OpenCode(manager) => manager.forget_session_of_thread(&thread_id),
        }
    }
    let _ = app.emit(acp::EV_THREADS, json!({}));
    Ok(time_machine::RestoreResult {
        thread_id,
        timeline,
    })
}

#[tauri::command]
fn rename_thread(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    thread_id: String,
    title: String,
) -> Result<(), String> {
    let title = title.trim().to_string();
    if title.is_empty() {
        return Err("标题不能为空".into());
    }
    {
        let mut store = state.store.lock().unwrap();
        let thread = store.get_mut(&thread_id).ok_or("线程不存在")?;
        thread.title = title;
        store.save();
    }
    let _ = app.emit(acp::EV_THREADS, json!({}));
    Ok(())
}

/// 工作流阶段会话：让模型按节点任务生成标题。会话先以「[WF] 节点名」兜底创建，
/// 生成成功后替换（[WF] 前缀由后端统一保留）；生成失败则保持兜底标题。
#[tauri::command]
fn generate_thread_title(state: State<'_, AppState>, thread_id: String, prompt: String) {
    let (agent_kind, fallback) = {
        let store = state.store.lock().unwrap();
        let Some(thread) = store.get(&thread_id) else {
            return;
        };
        (thread.agent_kind.clone(), thread.title.clone())
    };
    if fallback.trim().is_empty() || prompt.trim().is_empty() {
        return;
    }
    state.generate_title(&agent_kind, thread_id, prompt, fallback);
}

#[tauri::command]
fn set_thread_model(
    state: State<'_, AppState>,
    thread_id: String,
    model: Option<String>,
) -> Result<(), String> {
    let agent_kind;
    let is_guest;
    let is_quota;
    let was_running;
    {
        let mut store = state.store.lock().unwrap();
        let thread = store.get_mut(&thread_id).ok_or("线程不存在")?;
        // 运行中的 provider/bridge 仍在使用旧模型。此时只保存新选择，让它从
        // 下一轮请求生效；不能 forget 当前 session，否则会杀掉正在输出的进程。
        was_running = is_running(&state, thread);
        let model = model.filter(|s| !s.is_empty());
        if thread.model != model {
            thread.clear_auto_route();
        }
        thread.model = model;
        agent_kind = thread.agent_kind.clone();
        is_guest = thread.is_roaming_guest();
        is_quota = thread.is_quota_borrowed();
        store.save();
    }
    if is_guest {
        state.relay.guest_sync_config(&thread_id);
    } else if let Some(runtime) = state.borrowed_runtime(&thread_id) {
        match runtime.manager {
            BorrowedManager::Acp(manager) => {
                tauri::async_runtime::spawn(async move {
                    manager.sync_thread_config(&thread_id).await;
                });
            }
            BorrowedManager::Sdk(manager) => {
                if !was_running {
                    manager.forget_session_of_thread(&thread_id);
                }
            }
            BorrowedManager::OpenCode(manager) => {
                if !was_running {
                    manager.forget_session_of_thread(&thread_id);
                }
            }
        }
    } else if is_quota {
        return Err("额度凭证已过期，请重新发起租借".into());
    } else if agent_kind == AgentKind::Devin
        || matches!(agent_kind, AgentKind::CodeBuddy | AgentKind::CodeBuddyPlus)
    {
        let mgr = if agent_kind == AgentKind::Devin {
            state.acp.clone()
        } else {
            state.codebuddy.clone()
        };
        tauri::async_runtime::spawn(async move {
            mgr.sync_thread_config(&thread_id).await;
        });
    } else if !was_running {
        match agent_kind {
            AgentKind::Lyra => state.lyra.forget_session_of_thread(&thread_id),
            AgentKind::Codex | AgentKind::CodexPlus => {
                state.codexplus.forget_session_of_thread(&thread_id)
            }
            AgentKind::ClaudeCode => state.claudeplus.forget_session_of_thread(&thread_id),
            AgentKind::Cursor => state.cursorplus.forget_session_of_thread(&thread_id),
            AgentKind::OpenCode | AgentKind::OpenCodePlus => {
                state.opencodeplus.forget_session_of_thread(&thread_id)
            }
            AgentKind::Devin | AgentKind::CodeBuddy | AgentKind::CodeBuddyPlus => {}
        }
    }
    Ok(())
}

#[tauri::command]
fn set_thread_mode(
    state: State<'_, AppState>,
    thread_id: String,
    mode: Option<String>,
) -> Result<(), String> {
    let agent_kind;
    let is_guest;
    let is_quota;
    {
        let mut store = state.store.lock().unwrap();
        let thread = store.get_mut(&thread_id).ok_or("线程不存在")?;
        thread.mode = mode.filter(|s| !s.is_empty());
        agent_kind = thread.agent_kind.clone();
        is_guest = thread.is_roaming_guest();
        is_quota = thread.is_quota_borrowed();
        store.save();
    }
    if is_guest {
        state.relay.guest_sync_config(&thread_id);
    } else if let Some(runtime) = state.borrowed_runtime(&thread_id) {
        match runtime.manager {
            BorrowedManager::Acp(manager) => {
                tauri::async_runtime::spawn(async move {
                    manager.sync_thread_config(&thread_id).await;
                });
            }
            BorrowedManager::Sdk(manager) => manager.forget_session_of_thread(&thread_id),
            BorrowedManager::OpenCode(manager) => manager.forget_session_of_thread(&thread_id),
        }
    } else if is_quota {
        return Err("额度凭证已过期，请重新发起租借".into());
    } else if agent_kind == AgentKind::Devin {
        let mgr = state.acp.clone();
        tauri::async_runtime::spawn(async move {
            mgr.sync_thread_config(&thread_id).await;
        });
    }
    Ok(())
}

#[tauri::command]
fn set_thread_reasoning_effort(
    state: State<'_, AppState>,
    thread_id: String,
    reasoning_effort: Option<String>,
) -> Result<(), String> {
    let mut store = state.store.lock().unwrap();
    let thread = store.get_mut(&thread_id).ok_or("线程不存在")?;
    thread.reasoning_effort = reasoning_effort.filter(|s| !s.is_empty());
    store.save();
    Ok(())
}

#[tauri::command]
fn set_thread_starred(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    thread_id: String,
    starred: bool,
) -> Result<(), String> {
    {
        let mut store = state.store.lock().unwrap();
        let thread = store.get_mut(&thread_id).ok_or("线程不存在")?;
        if !is_starrable_thread(thread) {
            return Err("仅普通会话和额度租借会话支持星标".into());
        }
        thread.starred = starred;
        store.save();
    }
    let _ = app.emit(acp::EV_THREADS, json!({}));
    Ok(())
}

/// 切换会话使用的 agent（Devin ⇄ Codex），同时设置该 agent 下的模型/模式/思考强度。
/// 跨 agent 切换会作废旧的 remote session（两个 agent 的会话相互独立、上下文不互通），
/// 新 agent 从空上下文重新开始；UI 历史保留供参考。运行中/漫游会话禁止切换。
#[tauri::command]
fn set_thread_agent(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    thread_id: String,
    agent_kind: AgentKind,
    model: Option<String>,
    mode: Option<String>,
    reasoning_effort: Option<String>,
) -> Result<(), String> {
    // 同后端切模型允许在运行中进行：set_thread_model 会保留当前运行，
    // 新模型从下一轮生效。跨后端切换涉及运行路由迁移，仍需等待当前轮结束。
    if running_by_id(&state, &thread_id) {
        let current_kind = {
            let store = state.store.lock().unwrap();
            store
                .get(&thread_id)
                .ok_or("线程不存在")?
                .agent_kind
                .clone()
        };
        if current_kind != agent_kind {
            return Err("会话正在运行，请等待当前轮结束后再切换后端".into());
        }
        return set_thread_model(state, thread_id, model);
    }
    let changed;
    let old_kind: AgentKind;
    let switched_item;
    {
        let mut store = state.store.lock().unwrap();
        let thread = store.get_mut(&thread_id).ok_or("线程不存在")?;
        if thread.is_roaming_guest() {
            return Err("漫游会话暂不支持切换 agent".into());
        }
        if thread.is_quota_borrowed() {
            return Err("额度租借会话的后端已绑定；请重新发起租借以切换后端".into());
        }
        old_kind = thread.agent_kind.clone();
        changed = old_kind != agent_kind;
        let model = model.filter(|s| !s.is_empty());
        if changed || thread.model != model {
            thread.clear_auto_route();
        }
        thread.agent_kind = agent_kind.clone();
        thread.model = model;
        thread.mode = mode.filter(|s| !s.is_empty());
        thread.reasoning_effort = reasoning_effort.filter(|s| !s.is_empty());
        switched_item = if changed {
            // 旧 remote session 属于旧 agent，作废；下次发消息时新 agent 重新建会话
            thread.acp_session_id = None;
            thread.pending_native_restore = None;
            thread.provider_checkpoints.clear();
            thread.codex_usage_snapshot = None;
            // 标记上下文接力：仅当已有历史时才有意义（无历史时 take 会返回 None）
            thread.handoff_from = if thread.items.is_empty() {
                None
            } else {
                Some(old_kind.clone())
            };
            let label = agent_kind.label();
            let note = if thread.handoff_from.is_some() {
                format!(
                    "已切换到 {label}。下一条消息会把此前的对话上下文一并交给 {label}，便于无缝接续。"
                )
            } else {
                format!("已切换到 {label}，后续消息将由 {label} 处理。")
            };
            Some(thread.push_system(note, "info"))
        } else {
            None
        };
        thread.updated_at = now_ms();
        store.save();
    }
    if changed {
        // 旧 remote session 只属于切换前的后端。不能清理目标后端：OpenCode 等独占连接
        // 的 manager 会异步杀掉 thread_id 对应连接，若紧接着发送首条接力消息，可能误杀
        // 刚创建的新连接并导致 session not found。
        if old_kind == AgentKind::OpenCodePlus {
            state.opencodeplus.forget_session_of_thread(&thread_id);
        } else if old_kind == AgentKind::CodexPlus {
            state.codexplus.forget_session_of_thread(&thread_id);
        } else if old_kind == AgentKind::CodeBuddyPlus {
            state.codebuddy.forget_session_of_thread(&thread_id);
        } else {
            match old_kind {
                AgentKind::Devin => state.acp.forget_session_of_thread(&thread_id),
                AgentKind::Codex => state.codexplus.forget_session_of_thread(&thread_id),
                AgentKind::CodeBuddy => state.codebuddy.forget_session_of_thread(&thread_id),
                AgentKind::ClaudeCode => state.claudeplus.forget_session_of_thread(&thread_id),
                AgentKind::Cursor => state.cursorplus.forget_session_of_thread(&thread_id),
                AgentKind::OpenCode => state.opencodeplus.forget_session_of_thread(&thread_id),
                _ => {}
            }
        }
        if let Some(item) = switched_item {
            let _ = app.emit(
                acp::EV_UPDATE,
                json!({ "threadId": thread_id, "op": { "t": "upsert", "item": item } }),
            );
        }
        let _ = app.emit(acp::EV_THREADS, json!({}));
    }
    Ok(())
}

#[tauri::command]
async fn get_model_options(
    state: State<'_, AppState>,
    agent_kind: Option<AgentKind>,
) -> Result<Option<Value>, String> {
    let agent_kind = agent_kind.unwrap_or(AgentKind::Devin);
    if !state.agent_enabled(&agent_kind) {
        return Err(format!("{} 后端已关闭", agent_kind.label()));
    }
    match agent_kind {
        AgentKind::Lyra => state.lyra.ensure_model_options().await.map(Some),
        AgentKind::Devin => state.acp.ensure_model_options().await.map(Some),
        AgentKind::Codex | AgentKind::CodexPlus => {
            state.codex.ensure_model_options().await.map(Some)
        }
        AgentKind::OpenCode | AgentKind::OpenCodePlus => {
            state.opencodeplus.ensure_model_options().await.map(Some)
        }
        AgentKind::CodeBuddy | AgentKind::CodeBuddyPlus => {
            state.codebuddy.ensure_model_options().await.map(Some)
        }
        AgentKind::ClaudeCode => state.claudeplus.ensure_model_options().await.map(Some),
        AgentKind::Cursor => state.cursorplus.ensure_model_options().await.map(Some),
    }
}

/// 手动刷新 Lyra 本地配置（`~/.nova/alkaid/config.jsonc`）：清空模型列表缓存、
/// 杀掉预热实例，并由后台重拉模型列表推给前端。用于设置页「刷新配置」按钮；
/// 不打断正在运行的会话（每轮请求本就会重读配置）。
#[tauri::command]
fn refresh_lyra_config(state: State<'_, AppState>) {
    state.lyra.notify_config_changed();
}

#[tauri::command]
async fn get_slash_commands(
    state: State<'_, AppState>,
    agent_kind: Option<AgentKind>,
) -> Result<Vec<Value>, String> {
    let agent_kind = agent_kind.unwrap_or(AgentKind::Devin);
    if !state.agent_enabled(&agent_kind) {
        return Ok(Vec::new());
    }
    match agent_kind {
        AgentKind::Lyra => Ok(list_lyra_skill_commands(&state.config_dir)),
        AgentKind::Devin => {
            let commands = state.acp.fetch_commands().await?;
            Ok(commands.as_array().cloned().unwrap_or_default())
        }
        AgentKind::Codex | AgentKind::CodexPlus => Ok(list_codex_skill_commands(&state.config_dir)),
        AgentKind::OpenCode | AgentKind::OpenCodePlus => state.opencodeplus.fetch_commands().await,
        AgentKind::CodeBuddy
        | AgentKind::CodeBuddyPlus
        | AgentKind::ClaudeCode
        | AgentKind::Cursor => Ok(Vec::new()),
    }
}

#[tauri::command]
fn list_experiences(state: State<'_, AppState>, cwd: String) -> Result<Value, String> {
    let configs = state.settings.lock().unwrap().experience_experts.clone();
    experience::list_memory(&cwd, &configs)
}

#[tauri::command]
fn feedback_experience(
    state: State<'_, AppState>,
    cwd: String,
    experience_id: String,
    reward: f64,
) -> Result<Value, String> {
    let settings = state.settings.lock().unwrap().clone();
    let requested = if reward > 0.0 { 1 } else { -1 };
    experience::set_user_feedback(
        &cwd,
        &experience_id,
        requested,
        &settings.experience_experts,
    )
}

#[tauri::command]
fn delete_experience(cwd: String, experience_id: String) -> Result<Value, String> {
    experience::delete_memory(&cwd, &experience_id)
}

#[tauri::command]
async fn evolve_experiences(app: tauri::AppHandle, cwd: String) -> Result<Value, String> {
    experience::evolve_memory(&app, None, &cwd).await
}

#[tauri::command]
async fn train_experience(app: tauri::AppHandle, cwd: String) -> Result<Value, String> {
    experience::train(&app, &cwd, true).await
}

#[tauri::command]
fn list_skills(state: State<'_, AppState>) -> Vec<skills::SkillInfo> {
    skills::list_skills(&state.config_dir)
}

#[tauri::command]
fn get_skills_dir(state: State<'_, AppState>) -> String {
    skills::ensure_skills_dir(&state.config_dir)
        .to_string_lossy()
        .to_string()
}

#[tauri::command]
fn install_skill(state: State<'_, AppState>, path: String) -> Result<skills::SkillInfo, String> {
    skills::install_skill_path(&state.config_dir, Path::new(&path))
}

#[tauri::command]
fn remove_skill(state: State<'_, AppState>, name: String) -> Result<(), String> {
    skills::remove_skill(&state.config_dir, &name)
}

#[tauri::command]
fn sync_skills(state: State<'_, AppState>) -> Result<(), String> {
    skills::sync_skills_to_backends(&state.config_dir)
}

#[tauri::command]
fn send_prompt(
    app: tauri::AppHandle,
    thread_id: String,
    text: String,
    images: Option<Vec<PromptImage>>,
) -> Result<(), String> {
    dispatch_prompt(&app, thread_id, text, images.unwrap_or_default())
}

fn append_thread_error(app: &tauri::AppHandle, thread_id: &str, error: String) {
    let state = app.state::<AppState>();
    {
        let mut store = state.store.lock().unwrap();
        let Some(thread) = store.get_mut(thread_id) else {
            return;
        };
        thread.push_system(error, "error");
        store.save();
    }
    let _ = app.emit(relay::EV_RELAY_RELOAD, json!({ "threadId": thread_id }));
}

pub(crate) fn dispatch_prompt(
    app: &tauri::AppHandle,
    thread_id: String,
    text: String,
    images: Vec<PromptImage>,
) -> Result<(), String> {
    let state = app.state::<AppState>();
    let text = text.trim().to_string();
    if text.is_empty() && images.is_empty() {
        return Err("内容不能为空".into());
    }
    // 内置 /fire：任意入口（IPC、远程、后台重发、worktree 补发若未在前端拦截等）
    // 都交前端编排，禁止把字面量丢给模型。
    if remote::route_fire_command(app, &thread_id, &text, &images)? {
        return Ok(());
    }
    let (agent_kind, is_guest, is_quota) = {
        let store = state.store.lock().unwrap();
        let t = store.get(&thread_id).ok_or("线程不存在")?;
        (
            t.agent_kind.clone(),
            t.is_roaming_guest(),
            t.is_quota_borrowed(),
        )
    };
    // Stage 引用不是一次性快照：每次投递都从源会话最新 items 重建。
    // 猎户座知识不在这里自动注入；只有 Lyra 可通过 load_trained_memory 显式调用。
    {
        let mut store = state.store.lock().unwrap();
        let source_id = store
            .get(&thread_id)
            .and_then(|thread| thread.stage_source_thread_id.clone());
        let stage_context = if let Some(source_id) = source_id {
            Some(
                store
                    .get(&source_id)
                    .map(render_stage_context)
                    .ok_or("Stage 引用的源会话不存在")?,
            )
        } else {
            None
        };
        if let Some(thread) = store.get_mut(&thread_id) {
            thread.pending_stage_context = stage_context;
        }
    }
    // 漫游 guest：本机不执行，转发到对端 host
    if is_guest {
        return state.relay.guest_send_prompt(&thread_id, text, images);
    }
    if is_quota {
        let Some(runtime) = state.borrowed_runtime(&thread_id) else {
            let started = state
                .restoring_borrowed_runtimes
                .lock()
                .unwrap()
                .insert(thread_id.clone());
            if !started {
                return Err("额度会话正在恢复，请稍候再发送".into());
            }
            let restore_app = app.clone();
            let restore_thread_id = thread_id.clone();
            tauri::async_runtime::spawn(async move {
                let state = restore_app.state::<AppState>();
                let result = state.relay.restore_quota_runtime(&restore_thread_id).await;
                state
                    .restoring_borrowed_runtimes
                    .lock()
                    .unwrap()
                    .remove(&restore_thread_id);
                match result {
                    Ok(()) => {
                        if let Err(error) =
                            dispatch_prompt(&restore_app, restore_thread_id.clone(), text, images)
                        {
                            append_thread_error(&restore_app, &restore_thread_id, error);
                        }
                    }
                    Err(error) => append_thread_error(
                        &restore_app,
                        &restore_thread_id,
                        format!("额度会话恢复失败：{error}"),
                    ),
                }
            });
            return Ok(());
        };
        match runtime.manager {
            BorrowedManager::Acp(manager) => {
                if matches!(agent_kind, AgentKind::CodeBuddy | AgentKind::Cursor)
                    || !manager.is_running(&thread_id)
                {
                    tauri::async_runtime::spawn(async move {
                        manager.run_prompt(thread_id, text, images).await;
                    });
                } else {
                    tauri::async_runtime::spawn(async move {
                        manager.steer_prompt(thread_id, text, images).await;
                    });
                }
            }
            BorrowedManager::Sdk(manager) => {
                // Lyra：原生 steer；Cursor 等：打断当前轮后以新 turn / slim memory 继续。
                if manager.is_running(&thread_id) {
                    tauri::async_runtime::spawn(async move {
                        manager.steer_prompt(thread_id, text, images).await;
                    });
                } else {
                    tauri::async_runtime::spawn(async move {
                        manager.run_prompt(thread_id, text, images).await;
                    });
                }
            }
            BorrowedManager::OpenCode(manager) => {
                tauri::async_runtime::spawn(async move {
                    manager.run_prompt(thread_id, text, images).await;
                });
            }
        }
        return Ok(());
    }
    match agent_kind {
        AgentKind::Lyra => {
            let mgr = state.lyra.clone();
            if mgr.is_running(&thread_id) {
                tauri::async_runtime::spawn(async move {
                    mgr.steer_prompt(thread_id, text, images).await;
                });
            } else {
                tauri::async_runtime::spawn(async move {
                    mgr.run_prompt(thread_id, text, images).await;
                });
            }
        }
        AgentKind::Codex | AgentKind::CodexPlus => {
            let mgr = state.codexplus.clone();
            if mgr.is_running(&thread_id) {
                tauri::async_runtime::spawn(async move {
                    mgr.steer_prompt(thread_id, text, images).await;
                });
            } else {
                tauri::async_runtime::spawn(async move {
                    mgr.run_prompt(thread_id, text, images).await;
                });
            }
        }
        AgentKind::CodeBuddy | AgentKind::CodeBuddyPlus => {
            let mgr = state.codebuddy.clone();
            if mgr.is_running(&thread_id) {
                tauri::async_runtime::spawn(async move {
                    mgr.steer_prompt(thread_id, text, images).await;
                });
            } else {
                tauri::async_runtime::spawn(async move {
                    mgr.run_prompt(thread_id, text, images).await;
                });
            }
        }
        AgentKind::OpenCode | AgentKind::OpenCodePlus => {
            let mgr = state.opencodeplus.clone();
            tauri::async_runtime::spawn(async move {
                mgr.run_prompt(thread_id, text, images).await;
            });
        }
        AgentKind::ClaudeCode => {
            let mgr = state.claudeplus.clone();
            tauri::async_runtime::spawn(async move {
                mgr.run_prompt(thread_id, text, images).await;
            });
        }
        AgentKind::Cursor => {
            // Cursor 复用同一 live Agent session；运行中引导仍需静默打断后开新 turn，
            // 因为 Cursor SDK 没有原生 steer。
            let mgr = state.cursorplus.clone();
            if mgr.is_running(&thread_id) {
                tauri::async_runtime::spawn(async move {
                    mgr.steer_prompt(thread_id, text, images).await;
                });
            } else {
                tauri::async_runtime::spawn(async move {
                    mgr.run_prompt(thread_id, text, images).await;
                });
            }
        }
        AgentKind::Devin => {
            let mgr = state.acp.clone();
            if mgr.is_running(&thread_id) {
                tauri::async_runtime::spawn(async move {
                    mgr.steer_prompt(thread_id, text, images).await;
                });
                return Ok(());
            }
            tauri::async_runtime::spawn(async move {
                mgr.run_prompt(thread_id, text, images).await;
            });
        }
    }
    Ok(())
}

/// 从指定用户消息处截断会话（该消息及其之后的内容全部删除）。支持历史分叉的后端
/// 优先从远端对应位置 fork；失败或不支持时，下一条 prompt 才走文本上下文接力。
#[tauri::command]
fn truncate_thread(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    thread_id: String,
    item_id: u64,
    text: Option<String>,
    images: Option<Vec<PromptImage>>,
) -> Result<(), String> {
    if running_by_id(&state, &thread_id) {
        return Err("会话正在运行，请先停止".into());
    }
    // 与手动恢复保持相同的锁顺序，保证编辑分叉和恢复不会交叉写时间线或项目文件。
    let _time_machine_guard = state.time_machine_lock.lock().unwrap();
    let capture_workspace = state.settings.lock().unwrap().checkpoint_enabled;
    let (agent_kind, cwd, old_session_id, retained_turns, checkpoint, is_quota, is_guest) = {
        let mut store = state.store.lock().unwrap();
        let thread = store.get_mut(&thread_id).ok_or("线程不存在")?;
        let idx = thread
            .items
            .iter()
            .position(|i| i.id() == item_id && matches!(i, Item::User { .. }))
            .ok_or("该消息不存在或不是用户消息")?;
        if !thread.title.starts_with("[Fire]") {
            time_machine::record_edit_fork(&state.config_dir, thread, item_id, capture_workspace)?;
        }
        let is_guest = thread.is_roaming_guest();
        let checkpoint = thread.checkpoint_before(item_id);
        let old_session_id = thread.acp_session_id.clone();
        thread.items.truncate(idx);
        thread.plan = None;
        thread.acp_session_id = None;
        thread.pending_native_restore = None;
        thread.codex_usage_snapshot = None;
        thread
            .provider_checkpoints
            .retain(|checkpoint| checkpoint.user_item_id < item_id);
        // 在原生 fork 确认成功前保留接力上下文，作为唯一一次兜底。
        thread.handoff_from = (!thread.items.is_empty()).then(|| thread.agent_kind.clone());
        if matches!(
            thread.agent_kind,
            AgentKind::CodeBuddy | AgentKind::CodeBuddyPlus | AgentKind::ClaudeCode
        ) {
            if let Some(checkpoint) = checkpoint.as_ref() {
                thread.acp_session_id = Some(checkpoint.session_id.clone());
                thread.pending_native_restore = Some(threads::PendingNativeRestore {
                    session_id: checkpoint.session_id.clone(),
                    position: checkpoint.position.clone(),
                });
            }
        }
        // 截断到开头时重置标题，让编辑后的首条消息重新生成标题
        if idx == 0 {
            thread.title = thread
                .quota_peer_name
                .as_ref()
                .map(|peer| {
                    let name = Path::new(&thread.cwd)
                        .file_name()
                        .and_then(|value| value.to_str())
                        .unwrap_or(&thread.cwd);
                    format!("额度@{peer} · {name}")
                })
                .unwrap_or_else(|| "新会话".into());
        }
        thread.updated_at = now_ms();
        let retained_turns = thread
            .items
            .iter()
            .filter(|item| matches!(item, Item::User { .. }))
            .count();
        let result = (
            thread.agent_kind.clone(),
            thread.cwd.clone(),
            old_session_id,
            retained_turns,
            checkpoint,
            thread.is_quota_borrowed(),
            is_guest,
        );
        store.save();
        result
    };
    // guest 的真实 agent 会话在 host。释放本地 store 锁后再查路由，避免重入锁；
    // 顺序队列保证 truncate 消息先于稍后自动发送的 prompt 到达。
    if is_guest {
        state.relay.guest_truncate(&thread_id, item_id)?;
    }
    state.acp.forget_session_of_thread(&thread_id);
    state.codex.forget_session_of_thread(&thread_id);
    state.codexplus.forget_session_of_thread(&thread_id);
    state.codebuddy.forget_session_of_thread(&thread_id);
    state.claudeplus.forget_session_of_thread(&thread_id);
    state.cursorplus.forget_session_of_thread(&thread_id);
    state.opencodeplus.forget_session_of_thread(&thread_id);
    if let Some(runtime) = state.borrowed_runtime(&thread_id) {
        match runtime.manager {
            BorrowedManager::Acp(manager) => manager.forget_session_of_thread(&thread_id),
            BorrowedManager::Sdk(manager) => manager.forget_session_of_thread(&thread_id),
            BorrowedManager::OpenCode(manager) => manager.forget_session_of_thread(&thread_id),
        }
    }

    let _ = app.emit(acp::EV_THREADS, json!({}));
    let images = images.unwrap_or_default();
    let prompt_text = text.unwrap_or_default().trim().to_string();
    let prompt = (!prompt_text.is_empty() || !images.is_empty()).then_some(prompt_text);
    if prompt.is_some() {
        state
            .pending_prompt_restores
            .lock()
            .unwrap()
            .insert(thread_id.clone());
        state.sleep_inhibitor.set_running(&thread_id, true);
        let _ = app.emit(
            acp::EV_TURN,
            json!({ "threadId": thread_id, "running": true, "stopReason": null }),
        );
    }
    let background_app = app.clone();
    tauri::async_runtime::spawn(async move {
        let forked_session = {
            let state = background_app.state::<AppState>();
            match agent_kind {
                AgentKind::Codex | AgentKind::CodexPlus if retained_turns > 0 => {
                    if let Some(session_id) = old_session_id.as_deref() {
                        let manager = match state.borrowed_runtime(&thread_id) {
                            Some(runtime) => match runtime.manager {
                                BorrowedManager::Sdk(manager) => Some(manager),
                                _ => None,
                            },
                            None if !is_quota => Some(state.codexplus.clone()),
                            None => None,
                        };
                        if let Some(manager) = manager {
                            manager
                                .fork_session(&cwd, session_id, retained_turns)
                                .await
                                .ok()
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                }
                AgentKind::OpenCode | AgentKind::OpenCodePlus => {
                    if let Some(checkpoint) = checkpoint.as_ref() {
                        let manager = match state.borrowed_runtime(&thread_id) {
                            Some(runtime) => match runtime.manager {
                                BorrowedManager::OpenCode(manager) => Some(manager),
                                _ => None,
                            },
                            None if !is_quota => Some(state.opencodeplus.clone()),
                            None => None,
                        };
                        if let Some(manager) = manager {
                            manager
                                .fork_session(&cwd, &checkpoint.session_id, &checkpoint.position)
                                .await
                                .ok()
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                }
                _ => None,
            }
        };
        if let Some(session_id) = forked_session {
            let state = background_app.state::<AppState>();
            let mut store = state.store.lock().unwrap();
            if let Some(thread) = store.get_mut(&thread_id) {
                thread.acp_session_id = Some(session_id);
                thread.handoff_from = None;
                thread.pending_native_restore = None;
            }
            store.save();
        }
        let should_send = prompt.is_some()
            && background_app
                .state::<AppState>()
                .pending_prompt_restores
                .lock()
                .unwrap()
                .remove(&thread_id);
        if prompt.is_some() {
            background_app
                .state::<AppState>()
                .sleep_inhibitor
                .set_running(&thread_id, false);
        }
        if should_send {
            let prompt = prompt.unwrap_or_default();
            if let Err(error) = dispatch_prompt(&background_app, thread_id.clone(), prompt, images)
            {
                let state = background_app.state::<AppState>();
                let mut store = state.store.lock().unwrap();
                if let Some(thread) = store.get_mut(&thread_id) {
                    let item = thread.push_system(format!("编辑后重新发送失败：{error}"), "error");
                    let _ = background_app.emit(
                        acp::EV_UPDATE,
                        json!({ "threadId": thread_id, "op": { "t": "upsert", "item": item } }),
                    );
                }
                store.save();
                let _ = background_app.emit(
                    acp::EV_TURN,
                    json!({ "threadId": thread_id, "running": false, "stopReason": "error" }),
                );
            }
        }
    });
    Ok(())
}

#[tauri::command]
async fn cancel_turn(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    thread_id: String,
    _stop_reason: Option<String>,
    _delete_work: Option<bool>,
) -> Result<(), String> {
    if state
        .pending_prompt_restores
        .lock()
        .unwrap()
        .remove(&thread_id)
    {
        state.sleep_inhibitor.set_running(&thread_id, false);
        let _ = app.emit(
            acp::EV_TURN,
            json!({ "threadId": thread_id, "running": false, "stopReason": "cancelled" }),
        );
        return Ok(());
    }
    let (is_guest, is_quota) = {
        let store = state.store.lock().unwrap();
        store
            .get(&thread_id)
            .map(|t| (t.is_roaming_guest(), t.is_quota_borrowed()))
            .unwrap_or((false, false))
    };
    if is_guest {
        return state.relay.guest_cancel(&thread_id);
    }
    if let Some(runtime) = state.borrowed_runtime(&thread_id) {
        match runtime.manager {
            BorrowedManager::Acp(manager) => manager.cancel(&thread_id).await,
            BorrowedManager::Sdk(manager) => manager.cancel(&thread_id).await,
            BorrowedManager::OpenCode(manager) => manager.cancel(&thread_id).await,
        }
        return Ok(());
    }
    if is_quota {
        return Err("额度凭证已过期，请重新发起租借".into());
    }
    let kind = agent_kind_for_thread(&state, &thread_id)?;
    match kind {
        AgentKind::Lyra => state.lyra.cancel(&thread_id).await,
        AgentKind::Devin => state.acp.cancel(&thread_id).await,
        AgentKind::Codex | AgentKind::CodexPlus => state.codexplus.cancel(&thread_id).await,
        AgentKind::CodeBuddy | AgentKind::CodeBuddyPlus => state.codebuddy.cancel(&thread_id).await,
        AgentKind::ClaudeCode => state.claudeplus.cancel(&thread_id).await,
        AgentKind::Cursor => state.cursorplus.cancel(&thread_id).await,
        AgentKind::OpenCode | AgentKind::OpenCodePlus => {
            state.opencodeplus.cancel(&thread_id).await
        }
    }
    Ok(())
}

/// 手动压缩上下文：把当前会话历史浓缩为摘要，后续轮次仅基于摘要继续，加快长上下文响应。
/// Codex 走原生 thread/compact/start；Devin（ACP）暂无标准压缩接口，暂不支持。
#[tauri::command]
async fn compact_thread(state: State<'_, AppState>, thread_id: String) -> Result<(), String> {
    let (is_guest, is_quota) = {
        let store = state.store.lock().unwrap();
        store
            .get(&thread_id)
            .map(|t| (t.is_roaming_guest(), t.is_quota_borrowed()))
            .unwrap_or((false, false))
    };
    if is_guest {
        return Err("漫游会话暂不支持手动压缩上下文".into());
    }
    if is_quota {
        return Err("额度租借会话暂不支持手动压缩上下文".into());
    }
    match agent_kind_for_thread(&state, &thread_id)? {
        AgentKind::Codex => {
            let mgr = state.codex.clone();
            tauri::async_runtime::spawn(async move {
                mgr.compact(thread_id).await;
            });
            Ok(())
        }
        kind => Err(format!("{} 暂不支持手动压缩上下文", kind.label())),
    }
}

#[tauri::command]
async fn respond_permission(
    state: State<'_, AppState>,
    request_key: String,
    option_id: String,
) -> Result<(), String> {
    // 漫游 guest 的权限请求：回传给对端 host
    if state
        .relay
        .guest_respond_permission(&request_key, &option_id)
    {
        return Ok(());
    }
    let borrowed = state
        .borrowed_runtimes
        .lock()
        .unwrap()
        .values()
        .find(|runtime| runtime.has_pending_permission(&request_key))
        .cloned();
    if let Some(runtime) = borrowed {
        return runtime.respond_permission(&request_key, &option_id).await;
    }
    if request_key.starts_with("cdp-") {
        state
            .codexplus
            .respond_permission(&request_key, &option_id)
            .await
    } else if request_key.starts_with("cbp-") {
        // cbp- 前缀由 CodeBuddy ACP 管理器（kind=CodeBuddy）使用。
        state
            .codebuddy
            .respond_permission(&request_key, &option_id)
            .await
    } else if request_key.starts_with("clp-") {
        state
            .claudeplus
            .respond_permission(&request_key, &option_id)
            .await
    } else if request_key.starts_with("cup-") {
        state
            .cursorplus
            .respond_permission(&request_key, &option_id)
            .await
    } else if request_key.starts_with("ocp-") {
        state
            .opencodeplus
            .respond_permission(&request_key, &option_id)
            .await
    } else if request_key.starts_with("codex-") {
        state
            .codex
            .respond_permission(&request_key, &option_id)
            .await
    } else {
        state.acp.respond_permission(&request_key, &option_id).await
    }
}

#[tauri::command]
fn get_settings(state: State<'_, AppState>) -> Settings {
    state.settings.lock().unwrap().clone()
}

#[tauri::command]
fn refresh_environment_variables() -> Result<usize, String> {
    path_env::refresh_process_environment()
}

#[tauri::command]
fn get_global_agent_instructions(
    state: State<'_, AppState>,
) -> agent_config::GlobalAgentInstructions {
    agent_config::get_global_instructions(&state.config_dir)
}

#[tauri::command]
fn set_global_agent_instructions(
    state: State<'_, AppState>,
    content: String,
    enabled_agent_kinds: Vec<String>,
) -> Result<agent_config::GlobalAgentInstructions, String> {
    agent_config::set_global_instructions(&state.config_dir, &content, &enabled_agent_kinds)
}

#[tauri::command]
async fn set_settings(
    app: tauri::AppHandle,
    _state: State<'_, AppState>,
    settings: Settings,
) -> Result<(), String> {
    apply_runtime_settings(&app, settings, true, false).await
}

/// Apply settings to the live runtime. The headless file watcher uses the same restart rules as
/// the desktop settings command, but does not write the file back after loading it.
async fn apply_runtime_settings(
    app: &tauri::AppHandle,
    mut settings: Settings,
    persist: bool,
    restart_all_agents: bool,
) -> Result<(), String> {
    let state = app.state::<AppState>();
    settings.session_auto_cleanup_hours = settings.session_auto_cleanup_hours.max(1);
    // 只有 agent 启动配置变化才需要重启进程；编辑器等本地偏好直接生效
    let (
        restart_lyra,
        restart_devin,
        restart_codebuddy,
        restart_claudecode,
        restart_cursor,
        restart_opencode,
        restart_codex,
        restart_relay,
        notify_peer_models,
        recheck_availability,
    ) = {
        let mut s = state.settings.lock().unwrap();
        let context_runtime_changed = s.context_retrieval_mode != settings.context_retrieval_mode;

        let auto_change_project_changed =
            s.auto_change_project_enabled != settings.auto_change_project_enabled;
        let experience_tools_changed =
            s.experience_training_enabled != settings.experience_training_enabled;
        if context_runtime_changed || experience_tools_changed {
            settings.apply_context_retrieval_environment();
        }
        let restart_lyra = restart_all_agents
            || context_runtime_changed
            || experience_tools_changed
            || auto_change_project_changed
            || s.lyra_proxy != settings.lyra_proxy
            || s.lyra_enabled != settings.lyra_enabled;
        let restart_devin = restart_all_agents
            || context_runtime_changed
            || s.devin_path != settings.devin_path
            || s.acp_args != settings.acp_args
            || s.devin_proxy != settings.devin_proxy
            || s.devin_enabled != settings.devin_enabled;
        let restart_codebuddy = restart_all_agents
            || context_runtime_changed
            || s.codebuddy_path != settings.codebuddy_path
            || s.codebuddy_proxy != settings.codebuddy_proxy
            || s.codebuddy_enabled != settings.codebuddy_enabled;
        // API Key 也是 bridge 的启动环境：改了必须重启后端并重拉模型列表，
        // 否则新 Key 要等下次开应用才生效。
        let restart_claudecode = restart_all_agents
            || context_runtime_changed
            || s.claudecode_path != settings.claudecode_path
            || s.claudecode_proxy != settings.claudecode_proxy
            || s.claudecode_sdk_api_key != settings.claudecode_sdk_api_key
            || s.claudecode_enabled != settings.claudecode_enabled;
        let restart_cursor = restart_all_agents
            || context_runtime_changed
            || s.cursor_path != settings.cursor_path
            || s.cursor_proxy != settings.cursor_proxy
            || s.cursor_sdk_api_key != settings.cursor_sdk_api_key
            || s.cursor_disable_subagents != settings.cursor_disable_subagents
            || s.cursor_model_contexts != settings.cursor_model_contexts
            || s.cursor_context_mode != settings.cursor_context_mode
            || s.cursor_enabled != settings.cursor_enabled;
        let restart_opencode = restart_all_agents
            || s.opencode_path != settings.opencode_path
            || s.opencode_proxy != settings.opencode_proxy
            || s.opencode_enabled != settings.opencode_enabled;
        let restart_codex = restart_all_agents
            || s.codex_path != settings.codex_path
            || s.codex_args != settings.codex_args
            || s.codex_proxy != settings.codex_proxy
            || s.codex_enabled != settings.codex_enabled;
        let notify_peer_models = s.quota_shared_models != settings.quota_shared_models;
        let restart_relay = s.relay_server != settings.relay_server
            || s.relay_token != settings.relay_token
            || s.relay_groups != settings.relay_groups;
        // 任一后端的路径变化都可能影响「是否可用」，保存后重新并发检测
        let recheck_availability = restart_devin
            || restart_codebuddy
            || restart_claudecode
            || restart_cursor
            || restart_opencode
            || restart_codex;
        *s = settings;
        if persist {
            s.save(&state.config_dir);
        }
        (
            restart_lyra,
            restart_devin,
            restart_codebuddy,
            restart_claudecode,
            restart_cursor,
            restart_opencode,
            restart_codex,
            restart_relay,
            notify_peer_models,
            recheck_availability,
        )
    };
    if restart_lyra {
        state.lyra.shutdown();
        state.lyra.refresh_model_options_soon();
    }
    if restart_devin {
        // 杀掉当前进程，下次发消息时用新配置重启（历史会话靠 session/load 恢复）
        state.acp.kill_conn().await;
    }
    if restart_codebuddy {
        state.codebuddy.shutdown();
    }
    if restart_claudecode {
        state.claudeplus.shutdown();
        state.claudeplus.refresh_model_options_soon();
    }
    if restart_cursor {
        state.cursorplus.shutdown();
        state.cursorplus.refresh_model_options_soon();
    }
    if restart_opencode {
        state.opencodeplus.shutdown();
    }
    if restart_codex {
        state.codexplus.shutdown();
        state.codex.kill_conn().await;
    }
    if restart_relay {
        state.relay.restart();
    } else if notify_peer_models {
        state.relay.notify_peer_models_changed();
    }
    if recheck_availability {
        spawn_backend_availability_check(app.clone());
    }
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    sync_global_session_shortcuts(app);
    run_session_auto_cleanup(app);
    Ok(())
}

/// Management commands run in a short-lived process. Watch their commit marker and reconcile the
/// three persisted server inputs into the already-running headless process.
fn start_headless_config_watcher(app: tauri::AppHandle) {
    if !server::is_headless() {
        return;
    }
    tauri::async_runtime::spawn(async move {
        // 从空基线开始，避免管理命令恰好在 setup 与 watcher 启动之间提交而被漏掉。
        // 已存在的旧 marker 最多导致启动后做一次无害的重复校准。
        let mut marker = String::new();
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            let next = server::reload_marker();
            if next == marker {
                continue;
            }
            marker = next;

            let environment_changed = server::sync_server_environment();
            let dir = app.state::<AppState>().config_dir.clone();
            let mut settings = Settings::load(&dir);
            server::apply_settings(&mut settings);
            if let Err(error) =
                apply_runtime_settings(&app, settings, false, environment_changed).await
            {
                eprintln!("[nova-server] failed to reload settings: {error}");
            }

            let loaded = ProjectStore::load(&dir);
            let changed = {
                let state = app.state::<AppState>();
                let mut projects = state.projects.lock().unwrap();
                if projects.projects == loaded.projects {
                    false
                } else {
                    *projects = loaded;
                    true
                }
            };
            if changed {
                let state = app.state::<AppState>();
                let projects = state.projects.lock().unwrap().projects.clone();
                server::replace_configured_projects(&projects);
                let _ = app.emit("projects:changed", json!({}));
                eprintln!("[nova-server] project whitelist reloaded");
            }
        }
    });
}

// ===== 团队分享 / 漫游 =====

#[tauri::command]
fn get_relay_status(state: State<'_, AppState>) -> Value {
    state.relay.status()
}

/// 验证中转站连通性（用界面上当前填写的 server+token+groups，未保存也能测）
#[tauri::command]
async fn verify_relay(
    server: String,
    token: String,
    groups: Option<String>,
) -> Result<i64, String> {
    relay::probe_relay(&server, &token, &groups.unwrap_or_default()).await
}

#[tauri::command]
fn get_relay_peers(state: State<'_, AppState>) -> Value {
    state.relay.peers()
}

/// 从中转站拉取当前用户已解锁的成就。
#[tauri::command]
async fn list_achievements(state: State<'_, AppState>) -> Result<Vec<relay::Achievement>, String> {
    state.relay.list_achievements().await
}

#[tauri::command]
fn get_relay_inbox(state: State<'_, AppState>) -> Vec<Share> {
    state.relay.inbox_list()
}

/// 把某个会话分享给指定的人
#[tauri::command]
fn share_thread(state: State<'_, AppState>, thread_id: String, to: String) -> Result<(), String> {
    state.relay.share_thread(&thread_id, &to)
}

/// 高级分享：用所选后端 + 模型（默认 Devin swe-1.6）按提示词处理会话，跑完自动分享结果
#[tauri::command]
fn advanced_share(
    state: State<'_, AppState>,
    thread_id: String,
    to: String,
    prompt: String,
    agent: Option<String>,
    model: Option<String>,
) -> Result<Thread, String> {
    state.relay.advanced_share(
        &thread_id,
        to,
        prompt,
        agent.unwrap_or_default(),
        model.unwrap_or_default(),
    )
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ClueAiSummary {
    title: String,
    content: String,
}

async fn run_auxiliary_prompt(
    kind: &AgentKind,
    app: &tauri::AppHandle,
    thread_id: String,
    prompt: String,
) {
    let state = app.state::<AppState>();
    match kind {
        AgentKind::Lyra => {
            state
                .lyra
                .clone()
                .run_prompt(thread_id, prompt, Vec::new())
                .await
        }
        AgentKind::Devin => {
            state
                .acp
                .clone()
                .run_prompt(thread_id, prompt, Vec::new())
                .await
        }
        AgentKind::Codex | AgentKind::CodexPlus => {
            state
                .codexplus
                .clone()
                .run_prompt(thread_id, prompt, Vec::new())
                .await
        }
        AgentKind::CodeBuddy | AgentKind::CodeBuddyPlus => {
            state
                .codebuddy
                .clone()
                .run_prompt(thread_id, prompt, Vec::new())
                .await
        }
        AgentKind::ClaudeCode => {
            state
                .claudeplus
                .clone()
                .run_prompt(thread_id, prompt, Vec::new())
                .await
        }
        AgentKind::Cursor => {
            state
                .cursorplus
                .clone()
                .run_prompt(thread_id, prompt, Vec::new())
                .await
        }
        AgentKind::OpenCode | AgentKind::OpenCodePlus => {
            state
                .opencodeplus
                .clone()
                .run_prompt(thread_id, prompt, Vec::new())
                .await
        }
    }
}

fn last_assistant_text(app: &tauri::AppHandle, thread_id: &str) -> Option<String> {
    let state = app.state::<AppState>();
    let store = state.store.lock().unwrap();
    store
        .get(thread_id)?
        .items
        .iter()
        .rev()
        .find_map(|item| match item {
            Item::Assistant { text, .. } if !text.trim().is_empty() => {
                Some(text.trim().to_string())
            }
            _ => None,
        })
}

/// 优先用轻量级模型总结会话核心内容，失败时回退到原会话模型。
#[tauri::command]
async fn summarize_clue(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    thread_id: String,
) -> Result<ClueAiSummary, String> {
    let (src_cwd, transcript, fallback_title, origin_agent, origin_model) = {
        let store = state.store.lock().unwrap();
        let t = store.get(&thread_id).ok_or("线程不存在")?;
        (
            t.cwd.clone(),
            relay::build_transcript(&t.items),
            t.title.clone(),
            t.agent_kind.clone(),
            t.model.clone(),
        )
    };
    if transcript.trim().is_empty() {
        return Err("会话还没有内容可总结".into());
    }

    let (lightweight_agent, lightweight_model) = {
        let s = state.settings.lock().unwrap();
        (
            AgentKind::from_str(&s.lightweight_model_agent).unwrap_or(AgentKind::Lyra),
            (!s.lightweight_model.trim().is_empty())
                .then(|| s.lightweight_model.trim().to_string()),
        )
    };
    let mut attempts = vec![(lightweight_agent, lightweight_model)];
    if attempts[0].0 != origin_agent || attempts[0].1 != origin_model {
        attempts.push((origin_agent, origin_model));
    }

    let cwd = if std::path::Path::new(&src_cwd).is_dir() {
        src_cwd
    } else {
        let name = format!("clue-sum-{}", &uuid::Uuid::new_v4().to_string()[..8]);
        let dir = std::env::temp_dir().join(SCRATCH_MARK).join(name);
        std::fs::create_dir_all(&dir).map_err(|e| format!("创建临时目录失败：{e}"))?;
        dir.to_string_lossy().to_string()
    };
    let seed = format!(
        "请总结下面这段会话的核心内容，用于保存为「线索」。\n\
         严格按以下格式输出，不要其它前言或解释：\n\n\
         标题：一句话概括（不超过50字）\n\
         内容：\n\
         结论、关键证据/产物、验证结果与下一步（可多段）\n\n\
         ----\n会话记录：\n\n{transcript}"
    );

    let mut raw = None;
    for (agent_kind, model_opt) in attempts {
        let mut thread = Thread::new(cwd.clone(), agent_kind.clone(), model_opt, None, None, true);
        thread.title = "线索 AI 总结".into();
        let run_id = thread.id.clone();
        {
            let mut store = state.store.lock().unwrap();
            store.threads.push(thread);
            store.save();
        }
        let _ = app.emit(acp::EV_THREADS, json!({}));
        run_auxiliary_prompt(&agent_kind, &app, run_id.clone(), seed.clone()).await;
        let output = last_assistant_text(&app, &run_id).filter(|text| !text.trim().is_empty());

        // 每次尝试后都清理辅助会话；轻量模型失败时再用原会话模型重试。
        {
            let mut store = state.store.lock().unwrap();
            store.threads.retain(|t| t.id != run_id);
            store.save();
        }
        state.acp.forget_session_of_thread(&run_id);
        state.codex.forget_session_of_thread(&run_id);
        state.lyra.forget_session_of_thread(&run_id);
        state.codexplus.forget_session_of_thread(&run_id);
        state.codebuddy.forget_session_of_thread(&run_id);
        state.claudeplus.forget_session_of_thread(&run_id);
        state.cursorplus.forget_session_of_thread(&run_id);
        state.opencodeplus.forget_session_of_thread(&run_id);
        let _ = app.emit(acp::EV_THREADS, json!({}));
        if output.is_some() {
            raw = output;
            break;
        }
    }

    let raw = raw.ok_or_else(|| "总结失败：轻量模型和原模型均没有返回内容".to_string())?;
    Ok(parse_clue_ai_summary(&raw, &fallback_title))
}

fn parse_clue_ai_summary(raw: &str, fallback_title: &str) -> ClueAiSummary {
    let text = raw.trim();
    let (mut title, mut content) = if let Some(rest) = text
        .strip_prefix("标题：")
        .or_else(|| text.strip_prefix("标题:"))
    {
        let rest = rest.trim_start();
        if let Some((t, c)) = rest
            .split_once("\n内容：")
            .or_else(|| rest.split_once("\n内容:"))
        {
            (
                t.lines().next().unwrap_or("").trim().to_string(),
                c.trim().to_string(),
            )
        } else if let Some((t, c)) = rest.split_once('\n') {
            (t.trim().to_string(), c.trim().to_string())
        } else {
            (rest.trim().to_string(), String::new())
        }
    } else {
        let mut lines = text.lines();
        let first = lines.next().unwrap_or("").trim().to_string();
        let rest = lines.collect::<Vec<_>>().join("\n").trim().to_string();
        if rest.is_empty() {
            (String::new(), text.to_string())
        } else {
            (first, rest)
        }
    };

    if title.is_empty() {
        title = fallback_title.trim().to_string();
    }
    if title.chars().count() > 100 {
        title = title.chars().take(100).collect();
    }
    if content.is_empty() {
        content = text.to_string();
    }
    ClueAiSummary { title, content }
}

fn take_last_chars(text: &str, max: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max {
        return text.to_string();
    }
    let mut out = String::from("…");
    out.extend(&chars[chars.len() - max..]);
    out
}

/* ===== 工作流连线判断（轻量模型） ===== */

/// 提示词模式连线的候选分支。
#[derive(serde::Deserialize)]
struct RouteJudgeOption {
    id: String,
    label: String,
}

/// 传给判断模型的节点结论字数上限（结论通常在后面，取末尾）。
const ROUTE_JUDGE_CONCLUSION_MAX: usize = 2000;

/// 提示词模式连线判断：用配置的轻量级模型根据节点结论选择下一条连线。
/// 返回选中的连线 id；无法判断返回空串，由前端按「待补充」暂停处理。
#[tauri::command]
async fn judge_workflow_route(
    state: State<'_, AppState>,
    conclusion: String,
    options: Vec<RouteJudgeOption>,
) -> Result<String, String> {
    if options.is_empty() {
        return Ok(String::new());
    }
    let model = {
        let s = state.settings.lock().unwrap();
        let lightweight = s.lightweight_model.trim().to_string();
        let lightweight_agent = s.lightweight_model_agent.trim();
        if (lightweight_agent.is_empty() || lightweight_agent == "lyra")
            && !lightweight.is_empty()
        {
            lightweight
        } else {
            String::new()
        }
    };
    if model.is_empty() {
        return Err("未配置轻量级模型，无法使用提示词判断连线".into());
    }
    let list = options
        .iter()
        .enumerate()
        .map(|(i, o)| format!("{}. {}", i + 1, o.label.trim()))
        .collect::<Vec<_>>()
        .join("\n");
    let seed = format!(
        "你是工作流路由器：根据「节点结论」判断流程接下来应该走哪一条分支。\n\
         规则：只输出所选分支的编号（一个数字），不要输出任何其它内容；没有明显合适的分支时输出 0。\n\n\
         分支：\n{list}\n\n节点结论：\n{}\n\n编号：",
        take_last_chars(conclusion.trim(), ROUTE_JUDGE_CONCLUSION_MAX)
    );
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let fut = state.lyra.complete_once(&cwd, &model, seed);
    let raw = tokio::time::timeout(std::time::Duration::from_secs(20), fut)
        .await
        .map_err(|_| "连线判断超时".to_string())??;
    let text = raw.trim();
    // 1) 编号解析：只看输出中的第一个数字。
    for token in text.split(|c: char| !c.is_ascii_digit()) {
        if token.is_empty() {
            continue;
        }
        if let Ok(n) = token.parse::<usize>() {
            if n >= 1 && n <= options.len() {
                return Ok(options[n - 1].id.clone());
            }
            if n == 0 {
                return Ok(String::new());
            }
        }
        break;
    }
    // 2) 名称兜底：输出里直接包含某分支名。
    for opt in &options {
        let label = opt.label.trim();
        if !label.is_empty() && text.contains(label) {
            return Ok(opt.id.clone());
        }
    }
    Ok(String::new())
}

/// 接收一条分享，在指定目录新建本地会话，返回新会话 id
#[tauri::command]
fn accept_share(
    state: State<'_, AppState>,
    id: String,
    cwd: String,
    ephemeral: Option<bool>,
) -> Result<String, String> {
    state
        .relay
        .accept_share(&id, &cwd, ephemeral.unwrap_or(false))
}

#[tauri::command]
fn decline_share(state: State<'_, AppState>, id: String) {
    state.relay.decline_share(&id);
}

/// 把工作流定义分享给指定队友（对方接收后进入其工作流库）
#[tauri::command]
fn share_workflow(
    state: State<'_, AppState>,
    workflow: Value,
    to: String,
) -> Result<usize, String> {
    state.relay.share_workflow(&workflow, &to)
}

#[tauri::command]
async fn revoke_workflow(state: State<'_, AppState>, workflow_id: String) -> Result<usize, String> {
    state.relay.revoke_workflow(&workflow_id).await
}

#[tauri::command]
fn get_relay_workflow_inbox(state: State<'_, AppState>) -> Vec<WorkflowShare> {
    state.relay.workflow_inbox_list()
}

/// 接收队友分享的工作流：返回定义 JSON，由前端写入本地工作流库
#[tauri::command]
fn accept_relay_workflow_share(state: State<'_, AppState>, id: String) -> Result<Value, String> {
    state.relay.accept_workflow_share(&id)
}

#[tauri::command]
fn decline_relay_workflow_share(state: State<'_, AppState>, id: String) {
    state.relay.decline_workflow_share(&id);
}

#[tauri::command]
fn list_roaming_folders(state: State<'_, AppState>) -> Vec<String> {
    current_roaming_project_folders(state.inner())
}

#[tauri::command]
fn is_folder_roaming(state: State<'_, AppState>, cwd: String) -> bool {
    current_roaming_project_folders(state.inner())
        .iter()
        .any(|folder| project_path_key(folder) == project_path_key(&cwd))
}

/// 切换某目录是否允许漫游，返回切换后的状态
#[tauri::command]
fn set_folder_roaming(state: State<'_, AppState>, cwd: String, allowed: bool) -> bool {
    let cwd = if allowed {
        let Some(cwd) = restrict_roaming_folders_to_projects(state.inner(), vec![cwd])
            .into_iter()
            .next()
        else {
            return false;
        };
        cwd
    } else {
        cwd
    };
    {
        let mut roaming = state.roaming.lock().unwrap();
        roaming.set(&cwd, allowed);
    }
    state.relay.publish_folders();
    allowed
}

/// 批量替换允许漫游目录，设置页一次保存后只广播一遍。
#[tauri::command]
fn set_roaming_folders(state: State<'_, AppState>, folders: Vec<String>) -> Vec<String> {
    let normalized = restrict_roaming_folders_to_projects(state.inner(), folders);
    {
        let mut roaming = state.roaming.lock().unwrap();
        roaming.folders = normalized.clone();
        roaming.save();
    }
    state.relay.publish_folders();
    normalized
}

/// guest：在对端的目录上新建漫游会话
#[tauri::command]
async fn create_roaming_thread(
    state: State<'_, AppState>,
    peer_token: String,
    peer_name: String,
    folder: String,
    agent_kind: Option<AgentKind>,
    model: Option<String>,
    mode: Option<String>,
    first_prompt: Option<String>,
    clue_card_id: Option<String>,
    worktree: Option<bool>,
    worktree_branch: Option<String>,
    worktree_base: Option<String>,
) -> Result<Thread, String> {
    let relay = state.relay.clone();
    let default_mode = {
        let s = state.settings.lock().unwrap();
        s.default_mode.clone()
    };
    let mode = mode
        .filter(|s| !s.is_empty())
        .or(Some(default_mode).filter(|s| !s.is_empty()));
    let clue_context = match clue_card_id.filter(|value| !value.trim().is_empty()) {
        Some(card_id) => Some(state.relay.clue_context(&card_id).await?),
        None => None,
    };
    relay
        .create_roaming_thread(
            peer_token,
            peer_name,
            folder,
            agent_kind.unwrap_or(AgentKind::Devin),
            model.filter(|s| !s.is_empty()),
            mode,
            first_prompt.filter(|s| !s.trim().is_empty()),
            clue_context,
            worktree.unwrap_or(false),
            worktree_branch.filter(|s| !s.trim().is_empty()),
            worktree_base.filter(|s| !s.trim().is_empty()),
        )
        .await
}

/// 借用方：本机目录执行，但临时使用在线队友加密授权的后端额度。
#[tauri::command]
async fn create_quota_thread(
    state: State<'_, AppState>,
    peer_token: String,
    peer_name: String,
    cwd: String,
    agent_kind: Option<AgentKind>,
    model: Option<String>,
    mode: Option<String>,
    clue_card_id: Option<String>,
    operation_id: String,
) -> Result<Thread, String> {
    let clue_context = match clue_card_id.filter(|value| !value.trim().is_empty()) {
        Some(card_id) => Some(state.relay.clue_context(&card_id).await?),
        None => None,
    };
    state
        .relay
        .create_quota_thread(
            peer_token,
            peer_name,
            cwd,
            agent_kind.unwrap_or(AgentKind::Devin),
            model,
            mode,
            clue_context,
            operation_id,
        )
        .await
}

/// 选择共享模型时后台预热应用级额度租约，正式创建会话时直接复用。
#[tauri::command]
async fn prepare_quota_lease(
    state: State<'_, AppState>,
    peer_token: String,
    agent_kind: AgentKind,
    model: String,
) -> Result<(), String> {
    state
        .relay
        .prepare_quota_lease(peer_token, agent_kind, model)
        .await
}

#[tauri::command]
fn cancel_quota_roaming(state: State<'_, AppState>, operation_id: String) -> bool {
    state.relay.cancel_quota_roaming(&operation_id)
}

/// guest：召回漫游会话——请求 host 把完整会话快照 Flow 回来，
/// 收到后在收件箱选择本地项目接收成本地会话
#[tauri::command]
fn recall_roaming_thread(state: State<'_, AppState>, thread_id: String) -> Result<(), String> {
    state.relay.recall_roaming_thread(&thread_id)
}

/// guest：请求对端（host）已启用后端的可选模型/模式列表，
/// 漫游时用对方的模型列表而非本机的（结果经 relay:peer-models 事件回传前端）
#[tauri::command]
fn request_peer_models(state: State<'_, AppState>, peer_token: String) {
    state.relay.request_peer_models(peer_token);
}

/// host：对收到的漫游请求作出应答（接受/拒绝）
#[tauri::command]
fn respond_roam_request(
    state: State<'_, AppState>,
    req_id: String,
    accept: bool,
    prompt: Option<String>,
    folder: Option<String>,
    model: Option<String>,
    mode: Option<String>,
    worktree: Option<bool>,
    worktree_branch: Option<String>,
    worktree_base: Option<String>,
) -> Result<(), String> {
    state.relay.respond_roam_request(
        &req_id,
        accept,
        prompt,
        folder,
        model,
        mode,
        worktree,
        worktree_branch,
        worktree_base,
    )
}

/// 强制重启所有 agent 进程；会话上下文在下次发消息时自动恢复。
#[tauri::command]
async fn restart_devin(state: State<'_, AppState>) -> Result<(), String> {
    state.acp.restart().await;
    state.codex.restart().await;
    state.codexplus.shutdown();
    state.codebuddy.shutdown();
    state.claudeplus.shutdown();
    state.cursorplus.shutdown();
    state.opencodeplus.shutdown();
    Ok(())
}

#[tauri::command]
async fn get_status(state: State<'_, AppState>) -> Result<Value, String> {
    let mut connected = state.acp.connected().await;
    let mut agent = state
        .acp
        .agent_info
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|v| v.get("agentInfo").cloned());
    if state.codex.connected().await {
        connected = true;
        if agent.is_none() {
            agent = state.codex.agent_info.lock().unwrap().as_ref().map(|v| {
                json!({
                    "name": "codex",
                    "title": "Codex",
                    "version": v["userAgent"].as_str().unwrap_or("")
                })
            });
        }
    }
    Ok(json!({ "connected": connected, "agent": agent }))
}

#[tauri::command]
fn get_logs(state: State<'_, AppState>) -> Vec<String> {
    let mut logs = state.acp.get_logs();
    logs.extend(state.codex.get_logs());
    logs
}

/// 注册 Tauri 事件监听：本机 agent 产生的 update/turn/permission 事件，
/// 若属于「被别人漫游」的会话，则原样转发给对应 guest。
fn register_roaming_forwarders(app: &tauri::AppHandle, relay: Arc<RelayManager>) {
    let r = relay.clone();
    app.listen(acp::EV_UPDATE, move |e| {
        if let Ok(v) = serde_json::from_str::<Value>(e.payload()) {
            if let Some(tid) = v["threadId"].as_str() {
                if r.is_hosted(tid) {
                    r.forward_local_update(tid, &v["op"]);
                }
            }
        }
    });

    let r = relay.clone();
    app.listen(acp::EV_TURN, move |e| {
        if let Ok(v) = serde_json::from_str::<Value>(e.payload()) {
            if let Some(tid) = v["threadId"].as_str() {
                let running = v["running"].as_bool().unwrap_or(false);
                if r.is_hosted(tid) {
                    r.forward_local_turn(tid, running, &v["stopReason"]);
                }
                // 高级分享：处理线程跑完，自动把结果分享出去
                if !running && r.is_advanced(tid) {
                    if let Some(to) = r.finish_advanced_if_any(tid) {
                        r.log_line(format!("[relay] 高级分享已发送给 {to}"));
                    }
                }
            }
        }
    });

    let r = relay.clone();
    app.listen(acp::EV_PERMISSION, move |e| {
        if let Ok(v) = serde_json::from_str::<Value>(e.payload()) {
            if let Some(tid) = v["threadId"].as_str() {
                if r.is_hosted(tid) {
                    r.forward_local_permission(tid, &v);
                }
            }
        }
    });

    let r = relay.clone();
    app.listen(acp::EV_PERMISSION_RESOLVED, move |e| {
        if let Ok(v) = serde_json::from_str::<Value>(e.payload()) {
            if let Some(key) = v["requestKey"].as_str() {
                r.forward_local_permission_resolved(key);
            }
        }
    });

    // host 生成 AI 标题后，同步给 guest，让漫游会话标题与本机一致
    let r = relay.clone();
    app.listen(acp::EV_TITLE_GENERATED, move |e| {
        if let Ok(v) = serde_json::from_str::<Value>(e.payload()) {
            if let Some(tid) = v["threadId"].as_str() {
                if r.is_hosted(tid) {
                    r.forward_local_title(tid);
                }
            }
        }
    });
}

/// 权限请求原本只存在于 WebView 的临时状态。Server 没有 WebView，因此在核心状态中保留
/// 一份待审批事件，随远程快照同步给 Nova Web，并在审批完成后删除。
fn register_remote_permission_capture(app: &tauri::AppHandle) {
    let capture_app = app.clone();
    app.listen(acp::EV_PERMISSION, move |event| {
        let Ok(value) = serde_json::from_str::<Value>(event.payload()) else {
            return;
        };
        let Some(key) = value["requestKey"].as_str().map(str::to_string) else {
            return;
        };
        capture_app
            .state::<AppState>()
            .remote_permissions
            .lock()
            .unwrap()
            .insert(key, value);
    });

    let resolve_app = app.clone();
    app.listen(acp::EV_PERMISSION_RESOLVED, move |event| {
        let Ok(value) = serde_json::from_str::<Value>(event.payload()) else {
            return;
        };
        let Some(key) = value["requestKey"].as_str() else {
            return;
        };
        resolve_app
            .state::<AppState>()
            .remote_permissions
            .lock()
            .unwrap()
            .remove(key);
    });
}

pub const fn nova_data_dir_name() -> &'static str {
    data_dir_name(cfg!(debug_assertions))
}

const fn data_dir_name(debug: bool) -> &'static str {
    if debug {
        ".novadev"
    } else {
        ".nova"
    }
}

#[cfg(test)]
mod data_dir_tests {
    #[test]
    fn build_profiles_use_separate_data_directories() {
        assert_eq!(super::data_dir_name(true), ".novadev");
        assert_eq!(super::data_dir_name(false), ".nova");
    }
}

/// 全应用统一数据目录：开发构建使用 `~/.novadev`，正式构建使用 `~/.nova`。
/// 相比 Tauri 默认的 `%APPDATA%/<identifier>`，它跨项目、跨安装位置、跨版本都稳定，
/// 便于用户直接找到；worktree、CLI 工具、会话、记忆等都放在这里。
pub fn nova_data_dir(app: &tauri::AppHandle) -> PathBuf {
    if let Some(dir) = server::data_dir_override() {
        let _ = std::fs::create_dir_all(&dir);
        return dir;
    }
    let name = nova_data_dir_name();
    let dir = app
        .path()
        .home_dir()
        .map(|h| h.join(name))
        // 极端情况下取不到主目录：回退到旧的 app_data_dir，保证永不 panic。
        .unwrap_or_else(|_| {
            app.path()
                .app_data_dir()
                .unwrap_or_else(|_| PathBuf::from(name))
        });
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Lyra agent 原生入口（`nova lyra`）：命中则执行 stdio bridge 协议并退出，不启动 GUI。
pub fn maybe_run_lyra() -> bool {
    lyra::maybe_run()
}

/// 自更新内部 helper 入口：命中则替换旧 exe 并退出，不启动 GUI。
pub fn maybe_run_update_helper() -> bool {
    updater::maybe_run_apply_helper()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default().plugin(tauri_plugin_dialog::init());
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    let builder = builder.plugin(tauri_plugin_global_shortcut::Builder::new().build());

    builder
        .setup(|app| {
            #[cfg(windows)]
            spawn_single_instance_focus_listener(app.handle());

            // 数据目录必须最先确定，后续窗口还原/更新都要读取其中的 marker。
            let dir = nova_data_dir(app.handle());
            nova_tools_native::context::set_data_root(dir.clone());
            if let Err(error) = agent_config::sync_global_instructions(&dir) {
                eprintln!("[agent-config] 启动同步失败：{error}");
            }
            // 上次若异常退出，租借凭证明文目录可能没走正常清理；启动第一时间删除。
            let _ = std::fs::remove_dir_all(std::env::temp_dir().join("Nova-borrowed-credentials"));

            // 清理上次自更新留下的旧 exe
            updater::cleanup_old();

            // GUI 窗口保持隐藏，等前端应用可靠主题后通过 show_main_window 显示；
            // headless 模式没有前端，只记录启动状态。
            if server::is_headless() {
                eprintln!("[nova-server] headless runtime started");
            }

            // 不再在启动时强制替换重启。若本地已有暂存好的新版本：前端会显示「可更新」角标，
            // 后端的空闲提示定时器会在没有任务时弹窗让用户选择是否现在更新（用户主导）。

            let store = ThreadStore::load(dir.clone());
            let thread_trash = ThreadTrashStore::load(&dir);
            let clues = clues::ClueStore::load(&dir);
            let mut projects = ProjectStore::load(&dir);
            // 迁移：项目列表为空时从既有会话提取目录
            if projects.projects.is_empty() && !store.threads.is_empty() {
                let mut sorted: Vec<_> = store.threads.iter().collect();
                sorted.sort_by_key(|t| t.updated_at);
                for t in sorted {
                    if std::path::Path::new(&t.cwd).is_dir() {
                        projects.touch(&t.cwd);
                    }
                }
            }
            let mut settings = Settings::load(&dir);
            server::apply_settings(&mut settings);
            if server::is_headless()
                && (settings.relay_server.trim().is_empty()
                    || settings.relay_token.trim().is_empty())
            {
                return Err(
                    "Nova server 需要 relay server 和 token（命令行参数、环境变量或 settings.json）"
                        .into(),
                );
            }
            let configured_projects = server::configured_projects();
            if !configured_projects.is_empty() {
                // 显式白名单覆盖历史最近项目，防止服务器复用旧数据目录时意外暴露其他路径。
                projects.projects.clear();
            }
            for project in configured_projects {
                if project.is_dir() {
                    projects.touch(&project.to_string_lossy());
                } else {
                    eprintln!(
                        "[nova-server] ignored missing project: {}",
                        project.display()
                    );
                }
            }
            let windows_shell_shim_enabled = settings.windows_shell_shim_enabled;
            // 集中 skills → 各后端全局目录的软链接/联接（启动时先同步一次）
            let _ = skills::sync_skills_to_backends(&dir);
            let roaming = RoamingStore::load(&dir);
            let worktrees = WorktreeStore::load(&dir);
            experience::init(&dir);
            settings.apply_context_retrieval_environment();
            // Load known roots once into the process-wide cache shared by Lyra and every bridge.
            // Include active thread/worktree roots so alternate checkouts do not cold-rebuild.
            let mut preload_roots = projects.projects.clone();
            preload_roots.extend(store.threads.iter().map(|thread| thread.cwd.clone()));
            preload_roots.extend(
                worktrees
                    .worktrees
                    .iter()
                    .flat_map(|worktree| [worktree.repo.clone(), worktree.path.clone()]),
            );
            preload_roots.sort();
            preload_roots.dedup();
            let loaded_indexes = nova_tools_native::context::preload_indexes(&preload_roots);
            eprintln!(
                "[fast-context] shared mmap-loaded {loaded_indexes}/{} workspace indexes",
                preload_roots.len()
            );
            let context_service =
                context_service::ContextService::start(&dir).unwrap_or_else(|error| {
                    eprintln!("[fast-context] native context service disabled: {error}");
                    context_service::ContextService::disabled()
                });
            let acp = AcpManager::new(app.handle().clone(), AgentKind::Devin);
            let codebuddy_acp = AcpManager::new(app.handle().clone(), AgentKind::CodeBuddy);
            let opencodeplus = OpenCodeSdkManager::new(app.handle().clone());
            let codex = CodexManager::new(app.handle().clone());
            let lyra = SdkManager::new(app.handle().clone(), LyraAdapter);
            // Lyra 进程内运行时：会话/技能/配置目录锚定到当前 profile 的数据根
            // （debug 构建不回退到 release 的 ~/.nova）。
            lyra::set_nova_root(dir.clone());
            let codexplus = SdkManager::new(app.handle().clone(), CodexAdapter);
            let claudeplus = SdkManager::new(app.handle().clone(), ClaudeAdapter);
            let cursorplus = SdkManager::new(app.handle().clone(), CursorAdapter);
            let relay = RelayManager::new(app.handle().clone(), dir.clone());

            app.manage(AppState {
                store: Mutex::new(store),
                thread_trash: Mutex::new(thread_trash),
                clues: Mutex::new(clues),
                projects: Mutex::new(projects),
                settings: Mutex::new(settings),
                windows_shell_shim_enabled,
                roaming: Mutex::new(roaming),
                worktrees: Mutex::new(worktrees),
                context_service,
                acp,
                opencodeplus,
                codex,
                codebuddy: codebuddy_acp,
                lyra,
                codexplus,
                claudeplus,
                cursorplus,
                borrowed_runtimes: Mutex::new(HashMap::new()),
                restoring_borrowed_runtimes: Mutex::new(HashSet::new()),
                relay: relay.clone(),
                config_dir: dir.clone(),
                // 启动即视为一次活动，避免刚开机就触发静默升级
                last_activity_ms: Mutex::new(now_ms()),
                active_thread: Mutex::new(None),
                backend_availability: Mutex::new(HashMap::new()),
                pending_prompt_restores: Mutex::new(HashSet::new()),
                time_machine_lock: Mutex::new(()),
                remote_permissions: Mutex::new(HashMap::new()),
                sleep_inhibitor: sleep_inhibitor::SleepInhibitor::new(),
                browser: browser::BrowserManager::new(),
                browser_agent: browser_agent::BrowserAgentState::new(),
            });

            #[cfg(not(any(target_os = "android", target_os = "ios")))]
            sync_global_session_shortcuts(app.handle());
            run_session_auto_cleanup(app.handle());
            let cleanup_app = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                use tokio::time::{sleep, Duration};
                loop {
                    sleep(Duration::from_secs(60 * 60)).await;
                    run_session_auto_cleanup(&cleanup_app);
                }
            });

            // Tool API 已移除，清理旧版本遗留的失效连接信息。
            let _ = std::fs::remove_file(dir.join("tool-api.json"));

            // 启动后并发检测各后端 CLI 可用性（只解析 PATH，零成本），
            // 前端据结果只显示真正可用的后端
            spawn_backend_availability_check(app.handle().clone());

            // 模型列表：先从 ~/.nova/model-options/ 灌入内存，前端 get_model_options 几乎瞬时返回；
            // 再对默认（首个已启用）后端主动 fetch，不等前端 bootstrap 排队。
            {
                let state = app.state::<AppState>();
                let dir = state.config_dir.clone();
                if let Some(v) = model_cache::load(&dir, AgentKind::Devin.as_str()) {
                    state.acp.seed_model_options(v);
                }
                if let Some(v) = model_cache::load(&dir, opencode_sdk::MODEL_CACHE_KEY) {
                    state.opencodeplus.seed_model_options(v);
                }
                state.opencodeplus.spawn_revalidate_model_options();
                if let Some(v) = model_cache::load(&dir, AgentKind::Lyra.as_str()) {
                    state.lyra.seed_model_options(v);
                }
                if let Some(v) = model_cache::load(&dir, "codex") {
                    state.codex.seed_model_options(v);
                }
                // CodeBuddy 模型列表缓存灌入内存。
                if let Some(v) = model_cache::load(&dir, AgentKind::CodeBuddy.as_str()) {
                    state.codebuddy.seed_model_options(v);
                }
                for (kind, manager) in [
                    (AgentKind::ClaudeCode, &state.claudeplus),
                    (AgentKind::Cursor, &state.cursorplus),
                ] {
                    if let Some(v) = model_cache::load(&dir, kind.as_str()) {
                        if kind == AgentKind::Cursor
                            && v.get("novaCursorModelSchema").and_then(Value::as_u64) != Some(2)
                        {
                            continue;
                        }
                        manager.seed_model_options(v);
                    }
                }
                let default_kind = [
                    AgentKind::Devin,
                    AgentKind::Codex,
                    AgentKind::CodeBuddy,
                    AgentKind::ClaudeCode,
                    AgentKind::Cursor,
                    AgentKind::OpenCode,
                ]
                .into_iter()
                .find(|k| state.agent_enabled(k))
                .unwrap_or(AgentKind::Devin);
                // Server 的网页不会像桌面 WebView 那样在打开选择器时调用
                // get_model_options，因此必须主动刷新所有启用后端。否则首次远程快照
                // 发出后 Codex 仍是空列表，只能用 CLI 默认模型对话。
                if server::is_headless() {
                    if state.agent_enabled(&AgentKind::Devin) {
                        state.acp.spawn_revalidate_model_options();
                    }
                    if state.agent_enabled(&AgentKind::Codex) {
                        state.codex.spawn_revalidate_model_options();
                    }
                    if state.agent_enabled(&AgentKind::OpenCode) {
                        state.opencodeplus.spawn_revalidate_model_options();
                    }
                    if state.agent_enabled(&AgentKind::CodeBuddy) {
                        state.codebuddy.spawn_revalidate_model_options();
                    }
                    if state.agent_enabled(&AgentKind::ClaudeCode) {
                        state.claudeplus.spawn_revalidate_model_options();
                    }
                    if state.agent_enabled(&AgentKind::Cursor) {
                        state.cursorplus.spawn_revalidate_model_options();
                    }
                } else {
                    // 与 get_model_options 共用 refreshing 闸门，避免桌面启动时双开探测 session
                    match default_kind {
                        AgentKind::Devin => state.acp.spawn_revalidate_model_options(),
                        AgentKind::Codex | AgentKind::CodexPlus => {
                            state.codex.spawn_revalidate_model_options()
                        }
                        AgentKind::OpenCode | AgentKind::OpenCodePlus => {
                            state.opencodeplus.spawn_revalidate_model_options()
                        }
                        _ => {}
                    }
                }
                // Lyra 进程内拉取零成本，启动时静默刷新，
                // 避免首次打开模型选择器才生成 ~/.nova/model-options/lyra.json。
                if state.agent_enabled(&AgentKind::Lyra) {
                    state.lyra.spawn_revalidate_model_options();
                }
            }

            // 会话后台落盘器：普通更新仅克隆并写入脏会话；删除等结构变化才做全量快照。
            // ThreadStore 锁内不再执行 JSON 序列化，工具完成事件不会被大历史阻塞。
            {
                let save_notify = {
                    let state = app.state::<AppState>();
                    let store = state.store.lock().unwrap();
                    store.save_notify_handle()
                };
                let flush_app = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    loop {
                        // notify_one 在无等待者时会存 permit，启动前的 save 也不会丢
                        save_notify.notified().await;
                        // 防抖窗口：聚合流式期间密集的 save 请求
                        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
                        let snapshot = {
                            let state = flush_app.state::<AppState>();
                            let store = state.store.lock().unwrap();
                            store.take_persist_snapshot()
                        };
                        if let Some(mut snapshot) = snapshot {
                            let result = tauri::async_runtime::spawn_blocking(move || {
                                let result = ThreadStore::write_persist_snapshot(&mut snapshot);
                                (snapshot, result)
                            })
                            .await;
                            match result {
                                Ok((_, Ok(()))) => {}
                                Ok((snapshot, Err(error))) => {
                                    eprintln!("[threads] 后台保存会话失败：{error}");
                                    let state = flush_app.state::<AppState>();
                                    state
                                        .store
                                        .lock()
                                        .unwrap()
                                        .retry_persist_snapshot(&snapshot);
                                }
                                Err(error) => {
                                    // worker panic 时 snapshot 已随任务丢失，保守退回全量持久化。
                                    eprintln!("[threads] 后台保存 worker 失败：{error}");
                                    let state = flush_app.state::<AppState>();
                                    state.store.lock().unwrap().save();
                                }
                            }
                        }
                    }
                });
            }

            // 漫游 host：把本机被漫游会话的更新/轮次/权限事件转发给 guest
            register_roaming_forwarders(app.handle(), relay.clone());
            register_remote_permission_capture(app.handle());
            // 连接中转站（未配置 token 时内部直接返回）
            relay.restart();
            // server 侧远程会话：空闲只做命令长轮询；运行中按全量 + 增量同步。
            remote::start(app.handle().clone());
            // `Nova server config/project ...` 由独立管理进程写盘；运行实例监听提交标记，
            // 无需重启即可同步设置、环境变量与项目白名单。
            start_headless_config_watcher(app.handle().clone());

            // 经验训练调度无需跟随 5 秒员工心跳；每分钟检查一次是否达到用户配置的训练间隔。
            // 手动 /train 不受该检查频率限制。
            let experience_app = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                use tokio::time::{sleep, Duration};
                sleep(Duration::from_secs(60)).await;
                loop {
                    experience::tick(&experience_app);
                    sleep(Duration::from_secs(60)).await;
                }
            });

            // 自动更新：桌面端和无头模式都由后端 tokio 定时检测并静默下载暂存（每 10 分钟）。
            // 放后端而非前端 setInterval：WebView 计时器在窗口最小化/隐藏时会被严重节流甚至暂停；
            // 无头模式则根本没有前端。桌面端下载就绪后额外发事件显示可更新角标。
            let update_app = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                use tokio::time::{sleep, Duration};
                sleep(Duration::from_secs(3)).await; // 稍等，避开启动高峰（拉会话/连中转站）
                loop {
                    if let Ok(info) = updater::check(&update_app).await {
                        if info["hasUpdate"].as_bool().unwrap_or(false) {
                            if info["staged"].as_bool().unwrap_or(false) {
                                if !server::is_headless() {
                                    // 已暂存好（上次会话或本次刚下完）：通知桌面端显示角标
                                    let _ = update_app.emit(updater::EV_AVAILABLE, info);
                                }
                            } else if let Ok(res) =
                                updater::download_and_stage(update_app.clone()).await
                            {
                                if res["ready"].as_bool().unwrap_or(false) && !server::is_headless()
                                {
                                    if let Ok(info2) = updater::check(&update_app).await {
                                        let _ = update_app.emit(updater::EV_AVAILABLE, info2);
                                    }
                                }
                            }
                        }
                    }
                    sleep(Duration::from_secs(10 * 60)).await;
                }
            });

            // 已暂存新版本且当前空闲（没有会话运行、没有员工任务执行）时：桌面端弹窗确认；
            // 无头模式没有可交互前端，因此满足同样条件后直接应用并重启。桌面端每版本只提示一次。
            let prompt_app = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                use tokio::time::{sleep, Duration};
                let mut prompted: Option<String> = None;
                sleep(Duration::from_secs(60)).await;
                loop {
                    if let Some(ver) = updater::staged_upgrade_version(&prompt_app) {
                        let idle = {
                            let state = prompt_app.state::<AppState>();
                            !any_session_running(&state)
                        };
                        if idle {
                            if server::is_headless() {
                                eprintln!("[nova-server] 空闲，正在自动更新至 {ver}");
                                if let Err(error) = updater::apply_staged(prompt_app.clone()).await
                                {
                                    eprintln!("[nova-server] 自动更新失败：{error}");
                                }
                            } else if prompted.as_deref() != Some(ver.as_str()) {
                                if let Ok(info) = updater::check(&prompt_app).await {
                                    // 在线检查可能刚发现比暂存包更高的版本。只有暂存包仍是
                                    // 当前最新版时才提示，绝不把中间版本伪装成最新版应用。
                                    let is_same_staged = info["staged"].as_bool().unwrap_or(false)
                                        && info["latest"].as_str() == Some(ver.as_str());
                                    if is_same_staged {
                                        let _ = prompt_app.emit(updater::EV_PROMPT, info);
                                        prompted = Some(ver);
                                    }
                                }
                            }
                        }
                    }
                    sleep(Duration::from_secs(60)).await;
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_threads,
            load_threads,
            get_thread,
            list_clue_groups,
            get_clue_context,
            capture_clue,
            add_clue_comment,
            associate_clues,
            disassociate_clues,
            split_clue,
            stack_clues,
            delete_clue,
            list_projects,
            remove_project,
            prewarm,
            scratch_dir,
            get_quota,
            get_model_costs,
            check_update,
            download_staged_update,
            apply_staged_update,
            report_activity,
            show_main_window,
            take_restore_thread,
            signature_pending,
            create_thread,
            create_stage_thread,
            delete_thread,
            delete_threads,
            delete_project_threads,
            open_in_editor,
            open_file_default,
            open_clue_attachment,
            read_local_attachment,
            save_clue_attachment,
            revert_file_changes,
            open_in_explorer,
            open_in_terminal,
            open_url,
            create_time_machine_checkpoint,
            get_time_machine_timeline,
            get_time_machine_training_digest,
            set_time_machine_checkpoint_outcome,
            get_time_machine_checkpoint_preview,
            restore_time_machine_checkpoint,
            delete_time_machine_context,
            rename_thread,
            notify_fire_done,
            notify_workflow_done,
            push_system_item,
            set_prompt_queue_pending,
            set_thread_model,
            set_thread_mode,
            set_thread_reasoning_effort,
            set_thread_starred,
            set_thread_agent,
            get_model_options,
            refresh_lyra_config,
            get_slash_commands,
            list_experiences,
            feedback_experience,
            delete_experience,
            evolve_experiences,
            train_experience,
            send_prompt,
            truncate_thread,
            cancel_turn,
            compact_thread,
            respond_permission,
            get_settings,
            set_settings,
            refresh_environment_variables,
            get_global_agent_instructions,
            set_global_agent_instructions,
            get_backend_availability,
            get_cli_statuses,
            restart_devin,
            get_status,
            get_logs,
            get_relay_status,
            verify_relay,
            get_relay_peers,
            list_achievements,
            get_relay_inbox,
            share_thread,
            advanced_share,
            summarize_clue,
            generate_thread_title,
            judge_workflow_route,
            accept_share,
            decline_share,
            share_workflow,
            revoke_workflow,
            get_relay_workflow_inbox,
            accept_relay_workflow_share,
            decline_relay_workflow_share,
            list_roaming_folders,
            is_folder_roaming,
            set_folder_roaming,
            set_roaming_folders,
            create_roaming_thread,
            create_quota_thread,
            prepare_quota_lease,
            cancel_quota_roaming,
            recall_roaming_thread,
            request_peer_models,
            respond_roam_request,
            directory_exists,
            clipboard_file_paths,
            is_git_repo,
            list_branches,
            request_peer_branches,
            list_worktrees,
            remove_worktree,
            merge_worktree_thread,
            list_skills,
            get_skills_dir,
            install_skill,
            remove_skill,
            sync_skills,
            browser::browser_open,
            browser::browser_close,
            browser::browser_navigate,
            browser::browser_info,
            browser::browser_record_start,
            browser::browser_record_stop,
            browser::browser_record_pause,
            browser::browser_record_resume,
            browser::browser_events,
            browser::browser_capture_screenshot,
            browser::browser_capture_region,
            browser::browser_save_shot,
            browser_agent::analyze_screenshot,
            browser_agent::run_plan_with_agent
        ])
        .build(tauri::generate_context!())
        .expect("Nova 启动失败")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                let state = app.state::<AppState>();
                // 临时会话随程序关闭一并删除，并清理其临时工作目录。
                // save 已改为后台节流、每会话独立落盘；退出时必须无条件同步 save_now，
                // 把 flusher 尚未来得及写的脏数据一并落盘。
                let removed = {
                    let mut store = state.store.lock().unwrap();
                    let removed = store.purge_ephemeral();
                    store.save_now();
                    removed
                };
                for t in &removed {
                    if t.cwd.contains(SCRATCH_MARK) {
                        let _ = std::fs::remove_dir_all(&t.cwd);
                    }
                }
                // 退出时杀掉全部后端进程（连同其子进程树）。
                tauri::async_runtime::block_on(shutdown_agent_processes(&state));
            }
        });
}
