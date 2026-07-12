mod acp;
mod cli;
mod cli_manager;
mod tool_api;
mod codex;
mod employees;
mod gitwt;
mod marks;
mod mind;
mod model_cache;
mod notice;
mod pxpipe;
mod quota;
mod relay;
mod remote;
mod semantic;
mod settings;
mod skills;
mod sys_notify;
mod threads;
mod updater;

/// 临时会话目录的统一父目录名（前端据此识别并显示「临时会话」）
pub const SCRATCH_MARK: &str = "Nova-scratch";

use acp::AcpManager;
use codex::CodexManager;
use relay::{RelayManager, Share};
use serde_json::{json, Value};
use settings::Settings;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tauri::{Emitter, Listener, Manager, State};
use threads::{
    now_ms, AgentKind, Item, ProjectStore, PromptImage, RoamingStore, Thread, ThreadMeta,
    ThreadStore, Worktree, WorktreeRecord, WorktreeStore,
};

pub struct AppState {
    pub store: Mutex<ThreadStore>,
    pub projects: Mutex<ProjectStore>,
    pub settings: Mutex<Settings>,
    pub roaming: Mutex<RoamingStore>,
    /// 已创建的 git worktree 记录（独立持久化，供设置面板手动清理）
    pub worktrees: Mutex<WorktreeStore>,
    /// 数字员工：配置 / 任务收件箱 / 工作记忆
    pub employees: Mutex<employees::EmployeeStore>,
    pub tasks: Mutex<employees::TaskStore>,
    pub workflows: Mutex<employees::WorkflowStore>,
    pub memory: Mutex<employees::MemoryStore>,
    /// 数字员工 Mind：事件、注意力、运行租约与自愈状态
    pub mind: Mutex<mind::MindStore>,
    /// 奏折（御书房）：兼容旧存档；新协作走 notices
    pub decisions: Mutex<employees::DecisionStore>,
    /// 统一协作 Notice（发送方声明 PendingIntent）
    pub notices: Mutex<notice::NoticeStore>,
    /// 共享标记账本：员工协作时对外部实体（需求单等）做去重/互斥/接力
    pub marks: Mutex<marks::MarkStore>,
    pub vectors: Mutex<semantic::VectorStore>,
    pub acp: Arc<AcpManager>,
    /// CodeBuddy（腾讯云代码助手）：与 acp 同实现、不同实例（独立进程/会话/模型列表）
    pub codebuddy: Arc<AcpManager>,
    /// Claude Code（@zed-industries/claude-code-acp）：同为标准 ACP，多路复用，行为同 Devin，
    /// 仅启动命令/权限前缀/日志前缀不同，故复用 AcpManager 另起一个实例。
    pub claudecode: Arc<AcpManager>,
    /// Cursor（cursor-agent acp）：标准 ACP，多路复用；session/load 有已知缺陷故进程常驻不回收
    pub cursor: Arc<AcpManager>,
    /// OpenCode（opencode acp 模式）：标准 ACP，多路复用，空闲整树回收
    pub opencode: Arc<AcpManager>,
    pub codex: Arc<CodexManager>,
    pub relay: Arc<RelayManager>,
    pub config_dir: PathBuf,
    /// 用户最近一次操作的时间戳（ms）。前端把鼠标/键盘等交互节流上报到这里，
    /// 静默升级据此判断是否「一段时间没有操作」。
    pub last_activity_ms: Mutex<i64>,
    /// 前端当前打开的会话 id（None = 停在主页）。升级重启前写入恢复标记，重启后据此恢复显示。
    pub active_thread: Mutex<Option<String>>,
    /// 被用户手动停止的数字员工会话 id 集合：`cancel_turn` 命中员工会话时置入。
    /// 必须按会话隔离，否则新交办唤起下一轮时会误清旧会话的停止信号。
    pub cancelled_employee_threads: Mutex<HashSet<String>>,
    /// 用户停止普通数字员工工作会话时填写的原因。空字符串表示仅仅不想继续运行。
    pub employee_stop_reasons: Mutex<HashMap<String, String>>,
    /// 后端可用性检测结果（agent_kind → 是否可用）。启动后并发按需检测（解析 PATH，
    /// 不拉起进程），前端据此只显示真正可用的后端。空 map 表示尚未检测完成。
    pub backend_availability: Mutex<HashMap<String, bool>>,
    /// CLI 升级串行执行，避免两个包管理器/自更新器同时改写 PATH 下的文件。
    pub cli_upgrade_lock: tokio::sync::Mutex<()>,
    /// 进程内 Tool API（localhost）；员工工具优先走此通道。
    pub tool_api: Mutex<Option<tool_api::ToolApiInfo>>,
}

impl AppState {
    /// 按 AgentKind 取对应的 ACP manager 实例（Codex 走原生协议，返回 None）。
    pub fn acp_for(&self, kind: &AgentKind) -> Option<Arc<AcpManager>> {
        match kind {
            AgentKind::Devin => Some(self.acp.clone()),
            AgentKind::CodeBuddy => Some(self.codebuddy.clone()),
            AgentKind::ClaudeCode => Some(self.claudecode.clone()),
            AgentKind::Cursor => Some(self.cursor.clone()),
            AgentKind::OpenCode => Some(self.opencode.clone()),
            AgentKind::Codex => None,
        }
    }

    /// 全部 ACP 实例，用于「对所有后端广播」的场合（忘路由/杀进程/收集日志等）。
    pub fn all_acp(&self) -> [Arc<AcpManager>; 5] {
        [
            self.acp.clone(),
            self.codebuddy.clone(),
            self.claudecode.clone(),
            self.cursor.clone(),
            self.opencode.clone(),
        ]
    }

    /// 某后端在设置里是否启用（关闭的后端不参与标题/分享等自动路由）。
    pub fn agent_enabled(&self, kind: &AgentKind) -> bool {
        let s = self.settings.lock().unwrap();
        match kind {
            AgentKind::Devin => s.devin_enabled,
            AgentKind::Codex => s.codex_enabled,
            AgentKind::CodeBuddy => s.codebuddy_enabled,
            AgentKind::ClaudeCode => s.claudecode_enabled,
            AgentKind::Cursor => s.cursor_enabled,
            AgentKind::OpenCode => s.opencode_enabled,
        }
    }

    /// 标题生成的兜底后端：配置的标题后端不可用时用线程自身后端；线程自身是 Cursor/Codex
    /// 时改用 Devin（Cursor 按用量计费、不为标题额外发轮；Codex 无 ACP 标题能力）。
    fn title_fallback_mgr(&self, origin: &AgentKind) -> Arc<AcpManager> {
        match origin {
            AgentKind::Cursor | AgentKind::Codex => self.acp.clone(),
            other => self.acp_for(other).unwrap_or_else(|| self.acp.clone()),
        }
    }

    /// 统一的会话标题生成入口：把标题任务路由到设置里的「标题后端 + 标题模型」。
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
        let (agent_raw, model) = {
            let s = self.settings.lock().unwrap();
            (
                s.title_model_agent.trim().to_string(),
                s.title_model.trim().to_string(),
            )
        };
        let (mgr, model) = match AgentKind::from_str(&agent_raw) {
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
    if thread.is_roaming_guest() {
        return state.relay.is_guest_running(&thread.id);
    }
    match state.acp_for(&thread.agent_kind) {
        Some(mgr) => mgr.is_running(&thread.id),
        None => state.codex.is_running(&thread.id),
    }
}

fn running_by_id(state: &AppState, thread_id: &str) -> bool {
    let store = state.store.lock().unwrap();
    let Some(thread) = store.get(thread_id) else {
        return false;
    };
    is_running(state, thread)
}

/// 是否有任意会话正在运行（本地 Devin/Codex、漫游 guest、被别人漫游的 host 均算）。
/// 静默升级的前置条件之一：没有任何会话在跑才允许自动替换重启。
fn any_session_running(state: &AppState) -> bool {
    let store = state.store.lock().unwrap();
    store.threads.iter().any(|t| is_running(state, t))
}

/// 是否有任何员工任务正在执行（working）。用于判断「空闲（没有任务）」：
/// 空闲弹窗更新要求既没有会话在跑，也没有员工任务在做。
fn any_employee_working(state: &AppState) -> bool {
    let tasks = state.tasks.lock().unwrap();
    tasks.tasks.iter().any(|t| t.status == "working")
}

pub(crate) async fn shutdown_agent_processes(state: &AppState) {
    let managers = state.all_acp();
    let codex = state.codex.clone();
    for mgr in managers {
        mgr.kill_conn().await;
    }
    codex.kill_conn().await;
}

fn agent_kind_for_thread(state: &AppState, thread_id: &str) -> Result<AgentKind, String> {
    let store = state.store.lock().unwrap();
    store
        .get(thread_id)
        .map(|t| t.agent_kind.clone())
        .ok_or_else(|| "线程不存在".into())
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

/// 后端可用性检测完成事件：payload = { availability: { devin: bool, codex: bool, ... } }
pub const EV_BACKENDS: &str = "backends:availability";

/// 并发检测各后端 CLI 是否可用：只在 PATH / 具体路径上解析可执行文件（不拉起进程、零成本，
/// 对 Cursor 也不会产生任何用量）。结果写入 state 并广播，启动后与保存设置后各触发一次。
fn spawn_backend_availability_check(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        let paths: Vec<(AgentKind, String)> = {
            let state = app.state::<AppState>();
            let s = state.settings.lock().unwrap();
            vec![
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
                    (kind, acp::resolve_program_on_path(&path).is_some())
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
async fn upgrade_cli(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    agent_kind: AgentKind,
    settings: Settings,
) -> Result<cli_manager::CliStatus, String> {
    let status = cli_manager::upgrade(&state, agent_kind, &settings).await?;
    spawn_backend_availability_check(app);
    Ok(status)
}

#[tauri::command]
fn list_threads(state: State<'_, AppState>) -> Vec<ThreadMeta> {
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
            created_at: t.created_at,
            updated_at: t.updated_at,
            running: is_running(&state, t),
            ephemeral: t.ephemeral,
            roaming_role: t.roaming_role.clone(),
            roaming_peer_name: t.roaming_peer_name.clone(),
            worktree: t
                .worktree
                .clone()
                .or_else(|| wt_by_path.get(&t.cwd).cloned()),
            employee_id: t.employee_id.clone(),
            mind_thread: t.mind_thread,
            parent_thread_id: t.parent_thread_id.clone(),
        })
        .collect();
    metas.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    metas
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

#[tauri::command]
fn list_projects(state: State<'_, AppState>) -> Vec<ProjectEntry> {
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
fn remove_project(state: State<'_, AppState>, cwd: String) {
    state.projects.lock().unwrap().remove(&cwd);
    state.relay.publish_folders();
}

/// 预热某个项目目录的 agent session（草稿页选定项目时调用）
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
    match state.acp_for(&agent_kind) {
        Some(mgr) => {
            tauri::async_runtime::spawn(async move {
                mgr.prewarm(cwd).await;
            });
        }
        None => {
            let mgr = state.codex.clone();
            tauri::async_runtime::spawn(async move {
                mgr.prewarm(cwd, model.filter(|s| !s.is_empty()), mode)
                    .await;
            });
        }
    }
}

fn user_home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .or_else(|| {
            let drive = std::env::var_os("HOMEDRIVE")?;
            let path = std::env::var_os("HOMEPATH")?;
            Some(PathBuf::from(format!(
                "{}{}",
                drive.to_string_lossy(),
                path.to_string_lossy()
            )))
        })
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

fn list_codex_skill_commands(config_dir: &Path) -> Vec<Value> {
    let mut files = Vec::new();
    for root in codex_skill_roots(config_dir) {
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
            .unwrap_or_else(|| "Codex skill".to_string());
        skills.entry(name.clone()).or_insert_with(|| {
            json!({
                "name": name,
                "description": description,
                "kind": "skill",
                "input": format!("${name} ")
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
fn create_thread(
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

/// 判断目录是否 git 仓库：前端据此决定「在 worktree 中执行」开关是否可用
#[tauri::command]
fn is_git_repo(path: String) -> bool {
    gitwt::is_repo(&path)
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
    state
        .workflows
        .lock()
        .unwrap()
        .close_by_worktree(&rec.repo, &rec.path, &rec.branch);
    state.worktrees.lock().unwrap().remove(&id);
    if !doomed.is_empty() {
        {
            let mut store = state.store.lock().unwrap();
            store.threads.retain(|t| !doomed.contains(&t.id));
            store.save();
        }
        for tid in &doomed {
            for mgr in state.all_acp() {
                mgr.forget_session_of_thread(tid);
            }
            state.codex.forget_session_of_thread(tid);
        }
        let _ = app.emit(acp::EV_THREADS, json!({}));
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
            state
                .workflows
                .lock()
                .unwrap()
                .close_by_thread_id(&thread_id);
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
                AgentKind::Codex => {
                    let mgr = state.codex.clone();
                    tauri::async_runtime::spawn(async move {
                        mgr.run_prompt(thread_id, prompt, vec![]).await;
                    });
                }
                AgentKind::CodeBuddy => {
                    let mgr = state.codebuddy.clone();
                    tauri::async_runtime::spawn(async move {
                        mgr.run_prompt(thread_id, prompt, vec![]).await;
                    });
                }
                kind => {
                    let mgr = state.acp_for(&kind).expect("ACP 后端必有 manager");
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

/// 查看当前由 Nova 托管的 pxpipe 服务状态。
#[tauri::command]
fn get_pxpipe_service_status(settings: Settings) -> Result<Value, String> {
    serde_json::to_value(pxpipe::service_status(&settings)).map_err(|e| e.to_string())
}

/// 重启当前 pxpipe 服务；若尚未启动，则按当前设置启动一个实例。
#[tauri::command]
fn restart_pxpipe_service(settings: Settings) -> Result<Value, String> {
    serde_json::to_value(pxpipe::restart_service(&settings)?).map_err(|e| e.to_string())
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

/// 读取并清除「升级重启前正在查看的会话」标记，返回需要自动恢复打开的会话 id。
/// 普通启动返回 null；仅升级（手动/静默）重启后才会返回上次的会话。
#[tauri::command]
fn take_restore_thread(app: tauri::AppHandle) -> Option<String> {
    updater::take_restore_thread(&app)
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
    for id in &delete_ids {
        state.relay.notify_host_thread_deleted(id);
    }
    {
        let mut store = state.store.lock().unwrap();
        store.threads.retain(|t| !delete_ids.contains(&t.id));
        store.save();
    }
    employees::delete_tasks_for_threads(&app, &delete_ids);
    state.workflows.lock().unwrap().detach_threads(&delete_ids);
    for id in &delete_ids {
        for mgr in state.all_acp() {
            mgr.forget_session_of_thread(id);
        }
        state.codex.forget_session_of_thread(id);
    }
    let _ = app.emit(acp::EV_THREADS, json!({}));
    Ok(())
}

/// 批量删除会话；运行中的自动跳过，返回实际删除数量
#[tauri::command]
fn delete_threads(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    thread_ids: Vec<String>,
) -> Result<usize, String> {
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
        delete_set.extend(tree);
    }
    let deletable: Vec<String> = delete_set.into_iter().collect();
    for id in &deletable {
        state.relay.notify_host_thread_deleted(id);
    }
    let deleted;
    {
        let mut store = state.store.lock().unwrap();
        let before = store.threads.len();
        store.threads.retain(|t| !deletable.contains(&t.id));
        deleted = before - store.threads.len();
        store.save();
    }
    employees::delete_tasks_for_threads(&app, &deletable);
    state.workflows.lock().unwrap().detach_threads(&deletable);
    for id in &deletable {
        for mgr in state.all_acp() {
            mgr.forget_session_of_thread(id);
        }
        state.codex.forget_session_of_thread(id);
    }
    let _ = app.emit(acp::EV_THREADS, json!({}));
    Ok(deleted)
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
    let loc = match line {
        Some(l) => format!("{abs}:{l}"),
        None => abs.clone(),
    };
    let mut args: Vec<String> = Vec::new();
    if editor.to_lowercase().contains("zed") {
        if !scratch {
            args.push(cwd.clone());
        }
        args.push(loc);
    } else {
        // vscode / cursor / windsurf 同源，支持 --goto file:line
        if !scratch {
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

#[tauri::command]
fn set_thread_model(
    state: State<'_, AppState>,
    thread_id: String,
    model: Option<String>,
) -> Result<(), String> {
    let agent_kind;
    let is_guest;
    {
        let mut store = state.store.lock().unwrap();
        let thread = store.get_mut(&thread_id).ok_or("线程不存在")?;
        thread.model = model.filter(|s| !s.is_empty());
        agent_kind = thread.agent_kind.clone();
        is_guest = thread.is_roaming_guest();
        store.save();
    }
    if is_guest {
        state.relay.guest_sync_config(&thread_id);
    } else if let Some(mgr) = state.acp_for(&agent_kind) {
        // ACP 后端需把模型/模式同步到已挂载的 session；Codex 不需要
        tauri::async_runtime::spawn(async move {
            mgr.sync_thread_config(&thread_id).await;
        });
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
    {
        let mut store = state.store.lock().unwrap();
        let thread = store.get_mut(&thread_id).ok_or("线程不存在")?;
        thread.mode = mode.filter(|s| !s.is_empty());
        agent_kind = thread.agent_kind.clone();
        is_guest = thread.is_roaming_guest();
        store.save();
    }
    if is_guest {
        state.relay.guest_sync_config(&thread_id);
    } else if let Some(mgr) = state.acp_for(&agent_kind) {
        // ACP 后端需把模型/模式同步到已挂载的 session
        tauri::async_runtime::spawn(async move {
            mgr.sync_thread_config(&thread_id).await;
        });
    } else {
        // Codex 的模式落在 thread/start|resume 的 approvalPolicy/sandbox 上：
        // 卸载已挂载的远端线程（未运行时），下次发消息经 thread/resume 重新应用新策略。
        state.codex.remount_for_config(&thread_id);
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
    if running_by_id(&state, &thread_id) {
        return Err("会话正在运行，请先停止再切换模型".into());
    }
    let changed;
    let switched_item;
    {
        let mut store = state.store.lock().unwrap();
        let thread = store.get_mut(&thread_id).ok_or("线程不存在")?;
        if thread.is_roaming_guest() {
            return Err("漫游会话暂不支持切换 agent".into());
        }
        let old_kind = thread.agent_kind.clone();
        changed = old_kind != agent_kind;
        thread.agent_kind = agent_kind.clone();
        thread.model = model.filter(|s| !s.is_empty());
        thread.mode = mode.filter(|s| !s.is_empty());
        thread.reasoning_effort = reasoning_effort.filter(|s| !s.is_empty());
        switched_item = if changed {
            // 旧 remote session 属于旧 agent，作废；下次发消息时新 agent 重新建会话
            thread.acp_session_id = None;
            // 标记上下文接力：仅当已有历史时才有意义（无历史时 take 会返回 None）
            thread.handoff_from = if thread.items.is_empty() {
                None
            } else {
                Some(old_kind)
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
        // 所有 manager 都解除该线程的 remote 会话路由，确保下次按新 agent 新建
        for mgr in state.all_acp() {
            mgr.forget_session_of_thread(&thread_id);
        }
        state.codex.forget_session_of_thread(&thread_id);
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
    match state.acp_for(&agent_kind) {
        Some(mgr) => mgr.ensure_model_options().await.map(Some),
        None => state.codex.ensure_model_options().await.map(Some),
    }
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
    match state.acp_for(&agent_kind) {
        Some(mgr) => {
            let commands = mgr.fetch_commands().await?;
            Ok(commands.as_array().cloned().unwrap_or_default())
        }
        None => Ok(list_codex_skill_commands(&state.config_dir)),
    }
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
    state: State<'_, AppState>,
    thread_id: String,
    text: String,
    images: Option<Vec<PromptImage>>,
) -> Result<(), String> {
    let text = text.trim().to_string();
    let images = images.unwrap_or_default();
    if text.is_empty() && images.is_empty() {
        return Err("内容不能为空".into());
    }
    let (agent_kind, is_guest, employee_id, mind_thread) = {
        let store = state.store.lock().unwrap();
        let t = store.get(&thread_id).ok_or("线程不存在")?;
        (
            t.agent_kind.clone(),
            t.is_roaming_guest(),
            t.employee_id.clone(),
            t.mind_thread,
        )
    };
    if employee_id.is_some() && !mind_thread {
        return Err(
            "数字员工会话不能直接发送 prompt；请通过员工交办/账本流程，让 Dream 先做开工预检。"
                .into(),
        );
    }
    // 漫游 guest：本机不执行，转发到对端 host
    if is_guest {
        return state.relay.guest_send_prompt(&thread_id, text, images);
    }
    match agent_kind {
        AgentKind::CodeBuddy | AgentKind::Cursor => {
            // 每线程独占一条连接，无法像 Devin 那样把追问并发「注入」当前轮次（会串台/
            // 冲掉，Cursor 还会在仍活跃的 turn 上报内部错误）。一律走 run_prompt——经串行
            // 闸门排到当前轮次之后顺序执行；停止后立刻重发也靠闸门等旧轮次收尾完成。
            let mgr = state.acp_for(&agent_kind).expect("ACP 后端必有 manager");
            tauri::async_runtime::spawn(async move {
                mgr.run_prompt(thread_id, text, images).await;
            });
        }
        AgentKind::Codex => {
            let mgr = state.codex.clone();
            if state.codex.is_running(&thread_id) {
                tauri::async_runtime::spawn(async move {
                    mgr.steer_prompt(thread_id, text, images).await;
                });
                return Ok(());
            }
            tauri::async_runtime::spawn(async move {
                mgr.run_prompt(thread_id, text, images).await;
            });
        }
        // Devin / ClaudeCode / OpenCode：多路复用，运行中可注入「引导」当前轮次
        kind => {
            let mgr = state.acp_for(&kind).expect("ACP 后端必有 manager");
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

/// 从指定用户消息处截断会话（该消息及其之后的内容全部删除），
/// 用于「编辑并从此处重新开始」。旧 ACP session 上下文已不一致，一并丢弃，
/// 之后由前端走正常发送流程重新发起。
#[tauri::command]
fn truncate_thread(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    thread_id: String,
    item_id: u64,
) -> Result<(), String> {
    if running_by_id(&state, &thread_id) {
        return Err("会话正在运行，请先停止".into());
    }
    {
        let mut store = state.store.lock().unwrap();
        let thread = store.get_mut(&thread_id).ok_or("线程不存在")?;
        let idx = thread
            .items
            .iter()
            .position(|i| i.id() == item_id && matches!(i, Item::User { .. }))
            .ok_or("该消息不存在或不是用户消息")?;
        thread.items.truncate(idx);
        thread.plan = None;
        thread.acp_session_id = None;
        // 截断到开头时重置标题，让编辑后的首条消息重新生成标题
        if idx == 0 {
            thread.title = "新会话".into();
        }
        thread.updated_at = now_ms();
        store.save();
    }
    for mgr in state.all_acp() {
        mgr.forget_session_of_thread(&thread_id);
    }
    state.codex.forget_session_of_thread(&thread_id);
    let _ = app.emit(acp::EV_THREADS, json!({}));
    Ok(())
}

#[tauri::command]
async fn cancel_turn(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    thread_id: String,
    stop_reason: Option<String>,
    delete_work: Option<bool>,
) -> Result<(), String> {
    let is_guest = {
        let store = state.store.lock().unwrap();
        store
            .get(&thread_id)
            .map(|t| t.is_roaming_guest())
            .unwrap_or(false)
    };
    if is_guest {
        return state.relay.guest_cancel(&thread_id);
    }
    // 数字员工后台会话：用户手动停止 = 中止整轮自主编排（不只停当前一轮 turn），
    // 并把心跳计时重置，避免下一次 tick 立刻又把它唤起。
    let (employee_id, mind_thread, thread_title) = {
        let store = state.store.lock().unwrap();
        store
            .get(&thread_id)
            .map(|t| (t.employee_id.clone(), t.mind_thread, t.title.clone()))
            .unwrap_or((None, false, String::new()))
    };
    if let Some(eid) = employee_id {
        let is_mind = mind::is_active_thread(&app, &thread_id);
        state
            .cancelled_employee_threads
            .lock()
            .unwrap()
            .insert(thread_id.clone());
        if is_mind {
            mind::manual_stop(&app, &eid, &thread_id);
        } else {
            if !mind_thread {
                let reason = stop_reason.unwrap_or_default().trim().to_string();
                state
                    .employee_stop_reasons
                    .lock()
                    .unwrap()
                    .insert(thread_id.clone(), reason.clone());
                let scope = employees::find_employee(&app, &eid)
                    .map(|employee| employees::employee_scope(&employee))
                    .unwrap_or_else(|| format!("emp:{eid}"));
                let summary = if reason.is_empty() {
                    "【用户】手动停止了这轮工作，没有填写具体原因，只是不想继续运行了。Dream 应结合会话过程判断是否需要沉淀。".to_string()
                } else {
                    format!(
                        "【用户】手动停止了这轮工作，原因：{reason}。Dream 应结合会话过程复盘任务方向、执行范围和上奏时机。"
                    )
                };
                mind::record_journal_event(
                    &app,
                    &eid,
                    &scope,
                    &thread_id,
                    &thread_title,
                    &summary,
                    "outcome:stopped",
                    now_ms(),
                );
            }
            let mut emps = state.employees.lock().unwrap();
            if let Some(e) = emps.get_mut(&eid) {
                e.last_heartbeat_ms = now_ms();
                e.updated_at = now_ms();
            }
            emps.save();
        }
    }
    match state.acp_for(&agent_kind_for_thread(&state, &thread_id)?) {
        Some(mgr) => mgr.cancel(&thread_id).await,
        None => state.codex.cancel(&thread_id).await,
    }
    if delete_work.unwrap_or(false) {
        employees::delete_work_by_thread(&app, &thread_id).await;
    }
    Ok(())
}

/// 手动压缩上下文：把当前会话历史浓缩为摘要，后续轮次仅基于摘要继续，加快长上下文响应。
/// Codex 走原生 thread/compact/start；Devin（ACP）暂无标准压缩接口，暂不支持。
#[tauri::command]
async fn compact_thread(state: State<'_, AppState>, thread_id: String) -> Result<(), String> {
    let is_guest = {
        let store = state.store.lock().unwrap();
        store
            .get(&thread_id)
            .map(|t| t.is_roaming_guest())
            .unwrap_or(false)
    };
    if is_guest {
        return Err("漫游会话暂不支持手动压缩上下文".into());
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
    if request_key.starts_with("codex-") {
        state
            .codex
            .respond_permission(&request_key, &option_id)
            .await
    } else if request_key.starts_with("cb-") {
        state
            .codebuddy
            .respond_permission(&request_key, &option_id)
            .await
    } else if request_key.starts_with("cc-") {
        state
            .claudecode
            .respond_permission(&request_key, &option_id)
            .await
    } else if request_key.starts_with("cs-") {
        state
            .cursor
            .respond_permission(&request_key, &option_id)
            .await
    } else if request_key.starts_with("oc-") {
        state
            .opencode
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
async fn set_settings(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    settings: Settings,
) -> Result<(), String> {
    // 只有 agent 启动配置变化才需要重启进程；编辑器等本地偏好直接生效
    let (
        restart_devin,
        restart_codebuddy,
        restart_claudecode,
        restart_cursor,
        restart_opencode,
        restart_codex,
        restart_relay,
        recheck_availability,
        pxpipe_changed,
    ) = {
        let mut s = state.settings.lock().unwrap();
        let pxpipe_changed = s.pxpipe_experimental != settings.pxpipe_experimental
            || s.pxpipe_models != settings.pxpipe_models
            || s.devin_proxy != settings.devin_proxy
            || s.codebuddy_proxy != settings.codebuddy_proxy
            || s.claudecode_proxy != settings.claudecode_proxy
            || s.cursor_proxy != settings.cursor_proxy
            || s.opencode_proxy != settings.opencode_proxy
            || s.codex_proxy != settings.codex_proxy;
        let restart_devin = s.devin_path != settings.devin_path
            || s.acp_args != settings.acp_args
            || s.devin_proxy != settings.devin_proxy
            || s.devin_enabled != settings.devin_enabled
            || pxpipe_changed;
        let restart_codebuddy = s.codebuddy_path != settings.codebuddy_path
            || s.codebuddy_args != settings.codebuddy_args
            || s.codebuddy_proxy != settings.codebuddy_proxy
            || s.codebuddy_enabled != settings.codebuddy_enabled
            || pxpipe_changed;
        let restart_claudecode = s.claudecode_path != settings.claudecode_path
            || s.claudecode_args != settings.claudecode_args
            || s.claudecode_proxy != settings.claudecode_proxy
            || s.claudecode_enabled != settings.claudecode_enabled
            || pxpipe_changed;
        let restart_cursor = s.cursor_path != settings.cursor_path
            || s.cursor_args != settings.cursor_args
            || s.cursor_proxy != settings.cursor_proxy
            || s.cursor_enabled != settings.cursor_enabled
            || pxpipe_changed;
        let restart_opencode = s.opencode_path != settings.opencode_path
            || s.opencode_args != settings.opencode_args
            || s.opencode_proxy != settings.opencode_proxy
            || s.opencode_enabled != settings.opencode_enabled
            || pxpipe_changed;
        let restart_codex = s.codex_path != settings.codex_path
            || s.codex_args != settings.codex_args
            || s.codex_proxy != settings.codex_proxy
            || s.codex_enabled != settings.codex_enabled
            || pxpipe_changed;
        let restart_relay = s.relay_server != settings.relay_server
            || s.relay_token != settings.relay_token
            || s.relay_groups != settings.relay_groups
            || s.relay_name != settings.relay_name;
        // 任一后端的路径变化都可能影响「是否可用」，保存后重新并发检测
        let recheck_availability = restart_devin
            || restart_codebuddy
            || restart_claudecode
            || restart_cursor
            || restart_opencode
            || restart_codex;
        *s = settings;
        s.save(&state.config_dir);
        (
            restart_devin,
            restart_codebuddy,
            restart_claudecode,
            restart_cursor,
            restart_opencode,
            restart_codex,
            restart_relay,
            recheck_availability,
            pxpipe_changed,
        )
    };
    if pxpipe_changed {
        pxpipe::shutdown();
        let settings = state.settings.lock().unwrap().clone();
        if settings.pxpipe_experimental {
            pxpipe::restart_service(&settings)?;
        }
    }
    if restart_devin {
        // 杀掉当前进程，下次发消息时用新配置重启（历史会话靠 session/load 恢复）
        state.acp.kill_conn().await;
    }
    if restart_codebuddy {
        state.codebuddy.kill_conn().await;
    }
    if restart_claudecode {
        state.claudecode.kill_conn().await;
    }
    if restart_cursor {
        state.cursor.kill_conn().await;
    }
    if restart_opencode {
        state.opencode.kill_conn().await;
    }
    if restart_codex {
        state.codex.kill_conn().await;
    }
    if restart_relay {
        state.relay.restart();
    }
    if recheck_availability {
        spawn_backend_availability_check(app);
    }
    Ok(())
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

/// 主动联网刷新在线名单（供前端定时兜底）：直接查服务端 roster 并更新缓存/通知前端，
/// 不依赖 SSE presence 推送，能自愈丢失的 presence（「别人看不到你 / 少一个人」）。
#[tauri::command]
async fn refresh_relay_peers(state: State<'_, AppState>) -> Result<Value, String> {
    state.relay.refresh_peers().await;
    Ok(state.relay.peers())
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

#[tauri::command]
fn list_roaming_folders(state: State<'_, AppState>) -> Vec<String> {
    state.roaming.lock().unwrap().folders.clone()
}

#[tauri::command]
fn is_folder_roaming(state: State<'_, AppState>, cwd: String) -> bool {
    state.roaming.lock().unwrap().is_allowed(&cwd)
}

/// 切换某目录是否允许漫游，返回切换后的状态
#[tauri::command]
fn set_folder_roaming(state: State<'_, AppState>, cwd: String, allowed: bool) -> bool {
    {
        let mut roaming = state.roaming.lock().unwrap();
        roaming.set(&cwd, allowed);
    }
    state.relay.publish_folders();
    allowed
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
    relay
        .create_roaming_thread(
            peer_token,
            peer_name,
            folder,
            agent_kind.unwrap_or(AgentKind::Devin),
            model.filter(|s| !s.is_empty()),
            mode,
            first_prompt.filter(|s| !s.trim().is_empty()),
            worktree.unwrap_or(false),
            worktree_branch.filter(|s| !s.trim().is_empty()),
            worktree_base.filter(|s| !s.trim().is_empty()),
        )
        .await
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
) -> Result<(), String> {
    state.relay.respond_roam_request(&req_id, accept)
}

/// 强制重启 devin 进程：杀进程后所有挂起的轮次会立即失败返回，
/// 会话上下文在下次发消息时通过 session/load 自动恢复
#[tauri::command]
async fn restart_devin(state: State<'_, AppState>) -> Result<(), String> {
    for mgr in state.all_acp() {
        mgr.restart().await;
    }
    state.codex.restart().await;
    Ok(())
}

#[tauri::command]
async fn get_status(state: State<'_, AppState>) -> Result<Value, String> {
    // 依次找第一个已连接的 ACP 后端，取它的 agentInfo；都没有则看 Codex
    let mut connected = false;
    let mut agent: Option<Value> = None;
    for mgr in state.all_acp() {
        if mgr.connected().await {
            connected = true;
            if agent.is_none() {
                agent = mgr
                    .agent_info
                    .lock()
                    .unwrap()
                    .as_ref()
                    .and_then(|v| v.get("agentInfo").cloned());
            }
        }
    }
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
    let mut logs = Vec::new();
    for mgr in state.all_acp() {
        logs.extend(mgr.get_logs());
    }
    logs.extend(state.codex.get_logs());
    logs
}

// ===== 数字员工 =====

#[tauri::command]
fn list_employees(app: tauri::AppHandle) -> Vec<employees::Employee> {
    employees::list_employees(&app)
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
fn create_employee(
    app: tauri::AppHandle,
    name: String,
    agent_kind: Option<AgentKind>,
    model: Option<String>,
    heartbeat_agent_kind: Option<AgentKind>,
    heartbeat_model: Option<String>,
    mind_agent_kind: Option<AgentKind>,
    mind_model: Option<String>,
    mode: Option<String>,
    charter: String,
    cwd: String,
    heartbeat_enabled: Option<bool>,
    heartbeat_secs: Option<u64>,
    work_hours: Option<employees::WorkHours>,
    enabled: Option<bool>,
    self_directed: Option<bool>,
    allow_worktree: Option<bool>,
    directive: Option<String>,
    mark_scope: Option<String>,
    shared_ledger: Option<bool>,
    partners: Option<Vec<employees::Partner>>,
) -> Result<employees::Employee, String> {
    employees::create_employee(
        &app,
        name,
        agent_kind,
        model,
        heartbeat_agent_kind,
        heartbeat_model,
        mind_agent_kind,
        mind_model,
        mode,
        charter,
        cwd,
        heartbeat_enabled,
        heartbeat_secs,
        work_hours,
        enabled,
        self_directed,
        allow_worktree,
        directive,
        mark_scope,
        shared_ledger,
        partners,
    )
}

#[tauri::command]
fn update_employee(app: tauri::AppHandle, employee: employees::Employee) -> Result<(), String> {
    employees::update_employee(&app, employee)
}

#[tauri::command]
fn delete_employee(app: tauri::AppHandle, id: String) {
    employees::delete_employee(&app, &id);
}

#[tauri::command]
fn set_employee_enabled(app: tauri::AppHandle, id: String, enabled: bool) {
    employees::set_employee_enabled(&app, &id, enabled);
}

#[tauri::command]
fn get_employee_mind(app: tauri::AppHandle, id: String) -> mind::MindSnapshot {
    mind::snapshot(&app, &id)
}

#[tauri::command]
fn set_employee_mind_enabled(app: tauri::AppHandle, id: String, enabled: bool) {
    mind::set_enabled(&app, &id, enabled);
}

#[tauri::command]
fn resume_employee_mind(app: tauri::AppHandle, id: String) {
    mind::resume(&app, &id);
}

/// 立即让某员工干一轮（忽略心跳节奏，方便手动触发）
#[tauri::command]
fn run_employee_now(app: tauri::AppHandle, id: String) -> Result<(), String> {
    employees::run_now(&app, &id)
}

#[tauri::command]
fn list_employee_tasks(app: tauri::AppHandle) -> Vec<employees::Task> {
    employees::list_tasks(&app)
}

#[tauri::command]
fn assign_task(
    app: tauri::AppHandle,
    employee_id: String,
    title: String,
    brief: String,
) -> Result<employees::Task, String> {
    employees::assign_task(&app, employee_id, title, brief)
}

#[tauri::command]
fn delete_task(app: tauri::AppHandle, id: String) -> Result<(), String> {
    employees::delete_task(&app, &id)
}

/// 交办：把一个具体单子登记到该员工账本的「待处理」，员工唤起后自行侦察认领。
#[tauri::command]
async fn register_ledger_item(
    app: tauri::AppHandle,
    employee_id: String,
    title: String,
    brief: String,
    images: Option<Vec<PromptImage>>,
) -> Result<(), String> {
    employees::register_ledger_item(&app, employee_id, title, brief, images.unwrap_or_default())
        .await
}

#[tauri::command]
fn get_employee_memory(app: tauri::AppHandle, id: String) -> Vec<employees::JournalEntry> {
    employees::employee_memory(&app, &id)
}

/// 手动新增一条记忆/知识（pinned=true 为长期知识，不会被自动截断）
#[tauri::command]
fn add_employee_memory(
    app: tauri::AppHandle,
    id: String,
    title: String,
    summary: String,
    pinned: bool,
) -> Result<employees::JournalEntry, String> {
    employees::add_memory(&app, &id, title, summary, pinned)
}

#[tauri::command]
fn update_employee_memory(app: tauri::AppHandle, id: String, ts: i64, summary: String) {
    employees::update_memory_entry(&app, &id, ts, summary)
}

#[tauri::command]
fn delete_employee_memory(app: tauri::AppHandle, id: String, ts: i64) {
    employees::delete_memory_entry(&app, &id, ts)
}

#[tauri::command]
fn set_employee_memory_pinned(app: tauri::AppHandle, id: String, ts: i64, pinned: bool) {
    employees::set_memory_pinned(&app, &id, ts, pinned)
}

#[tauri::command]
fn set_employee_memory_feedback(
    app: tauri::AppHandle,
    id: String,
    ts: i64,
    feedback: i8,
) -> Result<(), String> {
    employees::set_memory_feedback(&app, &id, ts, feedback)
}

// ===== 协作标记账本 =====

#[tauri::command]
fn list_marks(state: State<'_, AppState>, scope: Option<String>) -> Vec<marks::Mark> {
    state.marks.lock().unwrap().list(scope.as_deref())
}

/// 释放认领：状态回到 open，供他人接手（不删除历史与备注）
#[tauri::command]
fn release_mark(app: tauri::AppHandle, state: State<'_, AppState>, scope: String, key: String) {
    state.marks.lock().unwrap().release(&scope, &key);
    let _ = app.emit(marks::EV_MARKS, json!({}));
}

/// 删除标记：该单子可被重新发现处理（用于「重做」）
#[tauri::command]
async fn reset_mark(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    scope: String,
    key: String,
) -> Result<(), String> {
    let thread_id = state
        .marks
        .lock()
        .unwrap()
        .get(&scope, &key)
        .and_then(|mark| mark.thread_id.clone());
    if let Some(thread_id) = thread_id.as_deref() {
        employees::cancel_deleted_ledger_thread(&app, thread_id).await;
    }
    state.marks.lock().unwrap().remove(&scope, &key);
    employees::cooloff_clear(&scope, &key); // 主管手动复位 = 明确要求可以再做
    let _ = app.emit(marks::EV_MARKS, json!({}));
    Ok(())
}

/// 手动改标记状态（open / claimed / done / failed）；置 open 时一并释放认领。
#[tauri::command]
fn set_mark(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    scope: String,
    key: String,
    status: String,
    note: Option<String>,
) {
    let release = status == "open";
    state
        .marks
        .lock()
        .unwrap()
        .set_status(&scope, &key, &status, note, release);
    if release {
        employees::cooloff_clear(&scope, &key); // 主管手动放回待处理 = 允许任何人（含原认领者）再领
    }
    let _ = app.emit(marks::EV_MARKS, json!({}));
}

// ===== 御书房（员工上奏、主管朱批）—— 统一走 Notice 广播 =====

/// 列出全部奏折（从 Notice 投影，兼容旧存档）。
#[tauri::command]
fn list_decisions(app: tauri::AppHandle) -> Vec<employees::Decision> {
    notice::list_as_decisions(&app)
}

#[tauri::command]
fn list_notices(app: tauri::AppHandle) -> Vec<notice::Notice> {
    app.state::<AppState>().notices.lock().unwrap().list()
}

/// 朱批准奏：respond Notice（choice=approve）并执行发送方预声明的 ActionPlan。
#[tauri::command]
fn resolve_decision(app: tauri::AppHandle, id: String, answer: String) -> Result<(), String> {
    // 优先 Notice；旧 Decision 存档兜底
    if app
        .state::<AppState>()
        .notices
        .lock()
        .unwrap()
        .get(&id)
        .is_some()
    {
        notice::respond_notice(
            &app,
            &id,
            notice::ActorRef::user(),
            notice::RespondParams {
                choice_id: Some("approve".into()),
                text: Some(answer),
                reject: false,
            },
        )?;
        return Ok(());
    }
    legacy_resolve_decision(&app, &id, &answer)
}

fn legacy_resolve_decision(app: &tauri::AppHandle, id: &str, answer: &str) -> Result<(), String> {
    let decision = {
        let state = app.state::<AppState>();
        let mut d = state.decisions.lock().unwrap();
        if !d.resolve(id, answer) {
            return Err("奏折不存在或已批阅".into());
        }
        d.decisions.iter().find(|x| x.id == id).cloned()
    };
    if let Some(decision) = decision {
        if decision.source == "mind" {
            mind::on_decision(app, &decision);
        } else {
            employees::finalize_wake_thread_decision(app, &decision);
            if decision.source != "wake" {
                mind::preempt_for_work(app, &decision.employee_id);
            }
            employees::summon_employee(app, &decision.employee_id);
        }
    }
    let _ = app.emit(employees::EV_DECISIONS, json!({}));
    let _ = app.emit(employees::EV_EMPLOYEES, json!({}));
    Ok(())
}

#[tauri::command]
fn reject_decision(app: tauri::AppHandle, id: String, answer: String) -> Result<(), String> {
    if app
        .state::<AppState>()
        .notices
        .lock()
        .unwrap()
        .get(&id)
        .is_some()
    {
        notice::respond_notice(
            &app,
            &id,
            notice::ActorRef::user(),
            notice::RespondParams {
                choice_id: Some("reject".into()),
                text: Some(answer),
                reject: true,
            },
        )?;
        return Ok(());
    }
    legacy_reject_decision(&app, &id, &answer)
}

fn legacy_reject_decision(app: &tauri::AppHandle, id: &str, answer: &str) -> Result<(), String> {
    let decision = {
        let state = app.state::<AppState>();
        let mut d = state.decisions.lock().unwrap();
        if !d.reject(id, answer) {
            return Err("奏折不存在或已批阅".into());
        }
        d.decisions.iter().find(|x| x.id == id).cloned()
    };
    if let Some(decision) = decision {
        if decision.source == "mind" {
            mind::on_decision(app, &decision);
        } else {
            employees::finalize_wake_thread_decision(app, &decision);
            if decision.source != "wake" {
                mind::preempt_for_work(app, &decision.employee_id);
            }
            employees::summon_employee(app, &decision.employee_id);
        }
    }
    let _ = app.emit(employees::EV_DECISIONS, json!({}));
    let _ = app.emit(employees::EV_EMPLOYEES, json!({}));
    Ok(())
}

#[tauri::command]
fn read_report(app: tauri::AppHandle, id: String) {
    if app
        .state::<AppState>()
        .notices
        .lock()
        .unwrap()
        .get(&id)
        .is_some()
    {
        let _ = notice::respond_notice(
            &app,
            &id,
            notice::ActorRef::user(),
            notice::RespondParams {
                choice_id: Some("ack".into()),
                text: Some("ack".into()),
                reject: false,
            },
        );
        return;
    }
    let changed = {
        let state = app.state::<AppState>();
        let mut d = state.decisions.lock().unwrap();
        d.mark_read(&id)
    };
    if changed {
        let _ = app.emit(employees::EV_DECISIONS, json!({}));
    }
}

#[tauri::command]
fn review_report(app: tauri::AppHandle, id: String, answer: String) -> Result<(), String> {
    let answer = answer.trim();
    if answer.is_empty() {
        return Err("批阅内容不能为空".into());
    }
    if app
        .state::<AppState>()
        .notices
        .lock()
        .unwrap()
        .get(&id)
        .is_some()
    {
        notice::respond_notice(
            &app,
            &id,
            notice::ActorRef::user(),
            notice::RespondParams {
                choice_id: Some("review".into()),
                text: Some(answer.to_string()),
                reject: false,
            },
        )?;
        return Ok(());
    }
    let decision = {
        let state = app.state::<AppState>();
        let mut decisions = state.decisions.lock().unwrap();
        if !decisions.review_report(&id, answer) {
            return Err("完工汇报不存在或已归档".into());
        }
        decisions.decisions.iter().find(|d| d.id == id).cloned()
    };
    if let Some(decision) = decision {
        mind::record_journal_event(
            &app,
            &decision.employee_id,
            &decision.scope,
            &decision.mark_key,
            &decision.task_title,
            &format!("完工汇报：{}\n【用户】批阅：{}", decision.question, answer),
            "supervision",
            now_ms(),
        );
    }
    let _ = app.emit(employees::EV_DECISIONS, json!({}));
    Ok(())
}

#[tauri::command]
fn dismiss_decision(app: tauri::AppHandle, id: String) {
    if app
        .state::<AppState>()
        .notices
        .lock()
        .unwrap()
        .get(&id)
        .is_some()
    {
        let _ = notice::respond_notice(
            &app,
            &id,
            notice::ActorRef::user(),
            notice::RespondParams {
                choice_id: Some("shelve".into()),
                text: Some("留中不发".into()),
                reject: false,
            },
        );
        return;
    }
    let decision = {
        let state = app.state::<AppState>();
        let mut d = state.decisions.lock().unwrap();
        if !d.shelve(&id) {
            return;
        }
        d.decisions.iter().find(|x| x.id == id).cloned()
    };
    if let Some(decision) = decision {
        if decision.source == "mind" {
            mind::on_decision(&app, &decision);
        } else {
            employees::finalize_wake_thread_decision(&app, &decision);
            if decision.source != "wake" {
                mind::preempt_for_work(&app, &decision.employee_id);
            }
            employees::summon_employee(&app, &decision.employee_id);
        }
    }
    let _ = app.emit(employees::EV_DECISIONS, json!({}));
    let _ = app.emit(employees::EV_EMPLOYEES, json!({}));
}

/// 从御书房物理删除一条奏折/汇报记录（含已批阅归档）。
#[tauri::command]
fn delete_decision(app: tauri::AppHandle, id: String) -> Result<(), String> {
    let state = app.state::<AppState>();
    let notice_removed = {
        let mut notices = state.notices.lock().unwrap();
        notices.remove(&id)
    };
    let legacy_removed = {
        let mut decisions = state.decisions.lock().unwrap();
        let existed = decisions.decisions.iter().any(|d| d.id == id);
        if existed {
            decisions.remove(&id);
        }
        existed
    };
    if !notice_removed && !legacy_removed {
        return Err("奏折不存在".into());
    }
    let _ = app.emit(employees::EV_DECISIONS, json!({}));
    Ok(())
}

// ===== 共享协作账本（跨机器，经中转站中心仲裁）=====

fn relay_of(app: &tauri::AppHandle) -> Arc<RelayManager> {
    app.state::<AppState>().relay.clone()
}

/// 列出共享账本某 scope 下的全部单子（从中转站拉取）。
#[tauri::command]
async fn list_shared_marks(
    app: tauri::AppHandle,
    scope: String,
) -> Result<Vec<marks::Mark>, String> {
    let vals = relay_of(&app).ledger_list(&scope).await?;
    Ok(vals
        .into_iter()
        .filter_map(|v| serde_json::from_value(v).ok())
        .collect())
}

/// 释放共享账本里的认领（状态回到 open，供同组队友接手）。
#[tauri::command]
async fn release_shared_mark(
    app: tauri::AppHandle,
    scope: String,
    key: String,
) -> Result<(), String> {
    relay_of(&app)
        .ledger_set(&scope, &key, "open", None, true)
        .await?;
    let _ = app.emit(marks::EV_MARKS, json!({}));
    Ok(())
}

/// 从共享账本删除一个单子（可被重新发现处理）。
#[tauri::command]
async fn reset_shared_mark(
    app: tauri::AppHandle,
    scope: String,
    key: String,
    thread_id: Option<String>,
) -> Result<(), String> {
    if let Some(thread_id) = thread_id.as_deref() {
        employees::cancel_deleted_ledger_thread(&app, thread_id).await;
    }
    relay_of(&app).ledger_remove(&scope, &key).await?;
    let _ = app.emit(marks::EV_MARKS, json!({}));
    Ok(())
}

/// 手动改共享账本里某单子的状态（open / claimed / done / failed）。
#[tauri::command]
async fn set_shared_mark(
    app: tauri::AppHandle,
    scope: String,
    key: String,
    status: String,
    note: Option<String>,
) -> Result<(), String> {
    let release = status == "open";
    relay_of(&app)
        .ledger_set(&scope, &key, &status, note.as_deref(), release)
        .await?;
    let _ = app.emit(marks::EV_MARKS, json!({}));
    Ok(())
}

// ===== 语义检索（外置 embedding 引擎）=====

fn embed_cfg(app: &tauri::AppHandle) -> (String, String, String) {
    let state = app.state::<AppState>();
    let s = state.settings.lock().unwrap();
    (
        s.embed_endpoint.clone(),
        s.embed_model.clone(),
        s.embed_api_key.clone(),
    )
}

/// 探测 embedding 服务是否可用，返回向量维度。
#[tauri::command]
async fn semantic_status(app: tauri::AppHandle) -> Result<Value, String> {
    let (endpoint, model, key) = embed_cfg(&app);
    if endpoint.trim().is_empty() || model.trim().is_empty() {
        return Err("未配置 embedding 服务地址或模型".into());
    }
    let client = reqwest::Client::new();
    let dim = semantic::probe(&client, &endpoint, &model, &key).await?;
    Ok(json!({ "ok": true, "dim": dim }))
}

/// 触发本地 Ollama 拉取模型（"点按钮手动下载"）。
#[tauri::command]
async fn semantic_pull(app: tauri::AppHandle, model: Option<String>) -> Result<(), String> {
    let (endpoint, cfg_model, _key) = embed_cfg(&app);
    let model = model.filter(|s| !s.trim().is_empty()).unwrap_or(cfg_model);
    if model.trim().is_empty() {
        return Err("未指定要下载的模型".into());
    }
    let client = reqwest::Client::new();
    semantic::ollama_pull(&client, &endpoint, &model).await
}

/// 重建向量索引：清空后下次检索惰性补算（employeeId 为空则清全部）。
#[tauri::command]
fn semantic_rebuild(state: State<'_, AppState>, employee_id: Option<String>) {
    let mut vs = state.vectors.lock().unwrap();
    match employee_id {
        Some(id) if !id.is_empty() => vs.clear_employee(&id),
        _ => vs.set_model(""),
    }
    vs.save();
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

/// 旧的 Tauri app_data_dir 标识（按新→旧排列），仅用于一次性迁移到 ~/.nova。
/// com.nova.desktop：更名 Nova 后的目录；com.fuckdevin.desktop：更早的品牌目录。
const LEGACY_IDENTIFIERS: &[&str] = &["com.nova.desktop", "com.fuckdevin.desktop"];

/// 全应用统一数据目录：**用户主目录下的 `.nova`**。
/// 相比 Tauri 默认的 `%APPDATA%/<identifier>`，它跨项目、跨安装位置、跨版本都稳定，
/// 便于用户直接找到；worktree、CLI 工具、会话、记忆等都放在这里。
pub fn nova_data_dir(app: &tauri::AppHandle) -> PathBuf {
    let dir = app
        .path()
        .home_dir()
        .map(|h| h.join(".nova"))
        // 极端情况下取不到主目录：回退到旧的 app_data_dir，保证永不 panic。
        .unwrap_or_else(|_| {
            app.path()
                .app_data_dir()
                .unwrap_or_else(|_| PathBuf::from(".nova"))
        });
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// 首次以 `~/.nova` 启动、且该目录尚无数据时，从旧的 Tauri 数据目录整体拷贝过来
///（优先 com.nova.desktop，其次更早的 com.fuckdevin.desktop），实现老用户数据无缝延续。
/// 用拷贝而非移动：万一在新旧版本间来回切换也不会丢数据。
fn migrate_data_to_home(app: &tauri::AppHandle, new_dir: &Path) {
    // 新目录已有任何数据 → 已在用 .nova，直接用它，不再从旧目录复制，以免覆盖现有数据/设置。
    if dir_has_entries(new_dir) {
        return;
    }
    // 旧目录都在 app_data_dir 的同级：<roaming>/<identifier>。用当前 app_data_dir 的父目录推导。
    let Ok(app_data) = app.path().app_data_dir() else {
        return;
    };
    let Some(parent) = app_data.parent() else {
        return;
    };
    for id in LEGACY_IDENTIFIERS {
        let legacy = parent.join(id);
        if legacy.as_path() == new_dir || !dir_has_entries(&legacy) {
            continue;
        }
        match copy_dir_all(&legacy, new_dir) {
            Ok(_) => {
                eprintln!(
                    "[nova] 已迁移旧数据目录 {} -> {}",
                    legacy.display(),
                    new_dir.display()
                );
                return;
            }
            Err(e) => eprintln!("[nova] 旧数据目录迁移失败（不影响启动）: {e}"),
        }
    }
}

/// 目录存在且至少含一个条目（用于判断数据目录是否已有内容）。
fn dir_has_entries(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .map(|mut it| it.next().is_some())
        .unwrap_or(false)
}

/// 递归拷贝目录内容（尽力而为，供旧数据迁移使用）。
fn copy_dir_all(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let src = entry.path();
        let dst = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_all(&src, &dst)?;
        } else {
            std::fs::copy(&src, &dst)?;
        }
    }
    Ok(())
}

/// 命令行子工具入口（如 `nova mem-search ...`）：命中则执行并返回 true，调用方随后退出、不启动 GUI。
/// 供数字员工的 agent 用自带 shell 调用记忆检索工具。
pub fn maybe_run_cli() -> bool {
    cli::maybe_run()
}

/// 自更新内部 helper 入口：命中则替换旧 exe 并退出，不启动 GUI。
pub fn maybe_run_update_helper() -> bool {
    updater::maybe_run_apply_helper()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            #[cfg(windows)]
            spawn_single_instance_focus_listener(app.handle());

            // 统一数据目录为 ~/.nova（跨项目/安装位置/版本稳定）。必须最先执行——后续窗口还原/
            // 更新都要读该目录里的 marker。首启从旧 Tauri 数据目录（com.nova → com.fuckdevin）整体迁移。
            let dir = nova_data_dir(app.handle());
            migrate_data_to_home(app.handle(), &dir);

            // 清理上次自更新留下的旧 exe
            updater::cleanup_old();

            // 窗口在配置里以 visible:false 创建：这里统一决定如何呈现。
            // 升级重启会还原成「更新前的样子」（位置/多屏/最大化/最小化/是否前台），
            // 普通启动则正常显示并聚焦。
            updater::restore_window_on_launch(app.handle());

            // 不再在启动时强制替换重启。若本地已有暂存好的新版本：前端会显示「可更新」角标，
            // 后端的空闲提示定时器会在没有任务时弹窗让用户选择是否现在更新（用户主导）。

            let store = ThreadStore::load(dir.clone());
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
            let settings = Settings::load(&dir);
            // 集中 skills → 各后端全局目录的软链接/联接（启动时先同步一次）
            let _ = skills::sync_skills_to_backends(&dir);
            let roaming = RoamingStore::load(&dir);
            let worktrees = WorktreeStore::load(&dir);
            let employees = employees::EmployeeStore::load(&dir);
            let tasks = employees::TaskStore::load(&dir);
            let workflows = employees::WorkflowStore::load(&dir);
            let memory = employees::MemoryStore::load(&dir);
            let mind = mind::MindStore::load(&dir);
            let decisions = employees::DecisionStore::load(&dir);
            let notices = notice::NoticeStore::load(&dir);
            let marks = marks::MarkStore::load(&dir);
            let vectors = semantic::VectorStore::load(&dir);
            let acp = AcpManager::new(app.handle().clone(), AgentKind::Devin);
            let codebuddy = AcpManager::new(app.handle().clone(), AgentKind::CodeBuddy);
            let claudecode = AcpManager::new(app.handle().clone(), AgentKind::ClaudeCode);
            let cursor = AcpManager::new(app.handle().clone(), AgentKind::Cursor);
            let opencode = AcpManager::new(app.handle().clone(), AgentKind::OpenCode);
            let codex = CodexManager::new(app.handle().clone());
            let relay = RelayManager::new(app.handle().clone(), dir.clone());

            app.manage(AppState {
                store: Mutex::new(store),
                projects: Mutex::new(projects),
                settings: Mutex::new(settings),
                roaming: Mutex::new(roaming),
                worktrees: Mutex::new(worktrees),
                employees: Mutex::new(employees),
                tasks: Mutex::new(tasks),
                workflows: Mutex::new(workflows),
                memory: Mutex::new(memory),
                mind: Mutex::new(mind),
                decisions: Mutex::new(decisions),
                notices: Mutex::new(notices),
                marks: Mutex::new(marks),
                vectors: Mutex::new(vectors),
                acp,
                codebuddy,
                claudecode,
                cursor,
                opencode,
                codex,
                relay: relay.clone(),
                config_dir: dir.clone(),
                // 启动即视为一次活动，避免刚开机就触发静默升级
                last_activity_ms: Mutex::new(now_ms()),
                active_thread: Mutex::new(None),
                cancelled_employee_threads: Mutex::new(HashSet::new()),
                employee_stop_reasons: Mutex::new(HashMap::new()),
                backend_availability: Mutex::new(HashMap::new()),
                cli_upgrade_lock: tokio::sync::Mutex::new(()),
                tool_api: Mutex::new(None),
            });

            // 旧奏折 → Notice（一次性迁移 pending/待领旨）
            notice::migrate_from_decisions(app.handle());
            // 进程内 Tool API：员工工具走 HTTP
            let _ = tool_api::start(app.handle().clone(), dir);

            // 启动后并发检测各后端 CLI 可用性（只解析 PATH，零成本），
            // 前端据结果只显示真正可用的后端
            spawn_backend_availability_check(app.handle().clone());

            // 模型列表：先从 ~/.nova/model-options/ 灌入内存，前端 get_model_options 几乎瞬时返回；
            // 再对默认（首个已启用）后端主动 fetch，不等前端 bootstrap 排队。
            {
                let state = app.state::<AppState>();
                let dir = state.config_dir.clone();
                for kind in [
                    AgentKind::Devin,
                    AgentKind::CodeBuddy,
                    AgentKind::ClaudeCode,
                    AgentKind::Cursor,
                    AgentKind::OpenCode,
                ] {
                    if let Some(v) = model_cache::load(&dir, kind.as_str()) {
                        if let Some(mgr) = state.acp_for(&kind) {
                            mgr.seed_model_options(v);
                        }
                    }
                }
                if let Some(v) = model_cache::load(&dir, "codex") {
                    state.codex.seed_model_options(v);
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
                // 与 get_model_options 共用 refreshing 闸门，避免启动时双开探测 session
                match state.acp_for(&default_kind) {
                    Some(mgr) => mgr.spawn_revalidate_model_options(),
                    None => state.codex.spawn_revalidate_model_options(),
                }
            }

            // threads.json 后台落盘器：store.save() 只置脏标记，这里以 600ms 防抖合并、
            // 序列化（紧凑 JSON、借用不拷贝）后在阻塞线程写盘。写盘不再发生在流式热路径
            // 且频率受控，消除「每个工具完成都全量序列化十几 MB」带来的出字卡顿与内存碎片。
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
                        let payload = {
                            let state = flush_app.state::<AppState>();
                            let store = state.store.lock().unwrap();
                            if !store.take_dirty() {
                                continue;
                            }
                            store.serialize_json().map(|json| (store.file_path(), json))
                        };
                        if let Some((path, json)) = payload {
                            // 写盘放阻塞线程，不占用 tokio worker
                            let _ = tauri::async_runtime::spawn_blocking(move || {
                                ThreadStore::write_json(&path, &json);
                            })
                            .await;
                        }
                    }
                });
            }

            // 迁移旧收件箱：历史 queued/working 任务转成员工账本的待处理单子（新模型下自主认领）
            employees::migrate_tasks_to_ledger(app.handle());

            // 漫游 host：把本机被漫游会话的更新/轮次/权限事件转发给 guest
            register_roaming_forwarders(app.handle(), relay.clone());
            // 连接中转站（未配置 token 时内部直接返回）
            relay.restart();
            // server 侧远程会话：空闲只做命令长轮询；运行中按全量 + 增量同步。
            remote::start(app.handle().clone());

            // 数字员工心跳：每 5 秒 tick 一次，到点的在岗员工自动唤起干一轮（续做在手单子或找新单子）。
            // 放后端 tokio 定时器（同更新检测）：窗口最小化/隐藏也不受 WebView 节流影响。
            let hb_app = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                use tokio::time::{sleep, Duration};
                sleep(Duration::from_secs(5)).await;
                loop {
                    // 先处理 agent 工具写入的收件箱（接力/记忆/收尾等），再心跳。
                    // 顺序很重要：让「done/接力」在本轮心跳前生效，避免重复开发/漏接力。
                    employees::process_command_inbox(&hb_app).await;
                    employees::heartbeat_tick(&hb_app);
                    mind::tick(&hb_app);
                    sleep(Duration::from_secs(5)).await;
                }
            });

            // 自动更新：检测 + 静默下载暂存放在后端 tokio 定时器里跑（每 10 分钟，不只启动时）。
            // 放后端而非前端 setInterval：WebView 计时器在窗口最小化/隐藏时会被严重节流甚至暂停，
            // 表现为「只有启动时才检测、之后角标一直不出现」。tokio 定时器不受影响，稳定触发；
            // 检测到新版本就静默下载暂存，就绪后发 update:available 事件，前端据此显示可更新角标。
            let update_app = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                use tokio::time::{sleep, Duration};
                sleep(Duration::from_secs(3)).await; // 稍等，避开启动高峰（拉会话/连中转站）
                loop {
                    if let Ok(info) = updater::check(&update_app).await {
                        if info["hasUpdate"].as_bool().unwrap_or(false) {
                            if info["staged"].as_bool().unwrap_or(false) {
                                // 已暂存好（上次会话或本次刚下完）：直接通知前端显示角标
                                let _ = update_app.emit(updater::EV_AVAILABLE, info);
                            } else if let Ok(res) =
                                updater::download_and_stage(update_app.clone()).await
                            {
                                if res["ready"].as_bool().unwrap_or(false) {
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

            // 空闲提示更新（取代原「强制静默自动升级」）：新版本已下载好、且当前空闲
            //（没有任何会话在运行、没有员工任务在执行）时，主动弹窗让用户选择是否现在更新，
            // 而不是自动替换重启。每个版本每次运行只弹一次；用户「稍后」则不再打扰。
            let prompt_app = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                use tokio::time::{sleep, Duration};
                let mut prompted: Option<String> = None;
                sleep(Duration::from_secs(60)).await;
                loop {
                    if let Some(ver) = updater::staged_upgrade_version(&prompt_app) {
                        let idle = {
                            let state = prompt_app.state::<AppState>();
                            !any_session_running(&state) && !any_employee_working(&state)
                        };
                        if idle && prompted.as_deref() != Some(ver.as_str()) {
                            if let Ok(info) = updater::check(&prompt_app).await {
                                let _ = prompt_app.emit(updater::EV_PROMPT, info);
                                prompted = Some(ver);
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
            get_thread,
            list_projects,
            remove_project,
            prewarm,
            scratch_dir,
            get_quota,
            get_model_costs,
            check_update,
            get_pxpipe_service_status,
            restart_pxpipe_service,
            download_staged_update,
            apply_staged_update,
            report_activity,
            take_restore_thread,
            create_thread,
            delete_thread,
            delete_threads,
            open_in_editor,
            revert_file_changes,
            open_in_explorer,
            open_in_terminal,
            open_url,
            rename_thread,
            set_thread_model,
            set_thread_mode,
            set_thread_reasoning_effort,
            set_thread_agent,
            get_model_options,
            get_slash_commands,
            send_prompt,
            truncate_thread,
            cancel_turn,
            compact_thread,
            respond_permission,
            get_settings,
            set_settings,
            get_backend_availability,
            get_cli_statuses,
            upgrade_cli,
            restart_devin,
            get_status,
            get_logs,
            get_relay_status,
            verify_relay,
            get_relay_peers,
            refresh_relay_peers,
            get_relay_inbox,
            share_thread,
            advanced_share,
            accept_share,
            decline_share,
            list_roaming_folders,
            is_folder_roaming,
            set_folder_roaming,
            create_roaming_thread,
            recall_roaming_thread,
            request_peer_models,
            respond_roam_request,
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
            list_employees,
            create_employee,
            update_employee,
            delete_employee,
            set_employee_enabled,
            get_employee_mind,
            set_employee_mind_enabled,
            resume_employee_mind,
            run_employee_now,
            list_employee_tasks,
            assign_task,
            delete_task,
            register_ledger_item,
            list_decisions,
            list_notices,
            resolve_decision,
            reject_decision,
            read_report,
            review_report,
            dismiss_decision,
            delete_decision,
            get_employee_memory,
            add_employee_memory,
            update_employee_memory,
            delete_employee_memory,
            set_employee_memory_pinned,
            set_employee_memory_feedback,
            list_marks,
            release_mark,
            reset_mark,
            set_mark,
            list_shared_marks,
            release_shared_mark,
            reset_shared_mark,
            set_shared_mark,
            semantic_status,
            semantic_pull,
            semantic_rebuild
        ])
        .build(tauri::generate_context!())
        .expect("Nova 启动失败")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                let state = app.state::<AppState>();
                // 临时会话随程序关闭一并删除，并清理其临时工作目录。
                // save 已改为后台节流落盘，退出时必须无条件同步 save_now，
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
