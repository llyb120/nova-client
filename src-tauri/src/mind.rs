use crate::employees::{self, Decision, Employee, JournalEntry};
use crate::threads::now_ms;
use crate::AppState;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{hash_map::DefaultHasher, HashMap, HashSet};
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use tauri::{AppHandle, Emitter, Manager};

pub const EV_MIND: &str = "mind:changed";

const NORMAL_EVENT_BATCH: usize = 5;
const MAX_EVENTS_PER_EMPLOYEE: usize = 400;
const MAX_EVENTS_PER_RUN: usize = 14;
const RUN_LEASE_MS: i64 = 20 * 60 * 1000;
const MANUAL_SNOOZE_MS: i64 = 6 * 60 * 60 * 1000;
const PREEMPT_RETRY_MS: i64 = 30 * 1000;
const BASE_RETRY_MS: i64 = 60 * 1000;
const MAX_RETRY_MS: i64 = 6 * 60 * 60 * 1000;
const REPEAT_INPUT_LIMIT: u32 = 3;

fn default_true() -> bool {
    true
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct MindEvent {
    pub seq: u64,
    pub id: String,
    pub employee_id: String,
    #[serde(default)]
    pub scope: String,
    #[serde(default)]
    pub task_id: String,
    #[serde(default)]
    pub task_title: String,
    pub kind: String,
    pub severity: u8,
    pub summary: String,
    pub created_at: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct MindHandoff {
    pub to: String,
    #[serde(default)]
    pub scope: String,
    #[serde(default)]
    pub key: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub brief: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct AttentionPlan {
    pub version: u64,
    #[serde(default)]
    pub focus: String,
    #[serde(default)]
    pub reasons: Vec<String>,
    #[serde(default)]
    pub risks: Vec<String>,
    #[serde(default)]
    pub rules: Vec<String>,
    #[serde(default)]
    pub deferred: Vec<String>,
    #[serde(default)]
    pub stop_conditions: Vec<String>,
    #[serde(default)]
    pub handoff: Option<MindHandoff>,
    #[serde(default)]
    pub summary: String,
    pub source_event_seq: u64,
    pub updated_at: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct MindState {
    pub employee_id: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub active_run_id: Option<String>,
    #[serde(default)]
    pub active_thread_id: Option<String>,
    #[serde(default)]
    pub stop_reason: String,
    #[serde(default)]
    pub lease_until: i64,
    #[serde(default)]
    pub cursor_seq: u64,
    #[serde(default)]
    pub dismissed_through_seq: u64,
    #[serde(default)]
    pub snoozed_until: i64,
    #[serde(default)]
    pub next_retry_at: i64,
    #[serde(default)]
    pub consecutive_failures: u32,
    #[serde(default)]
    pub last_input_fingerprint: String,
    #[serde(default)]
    pub repeated_input_count: u32,
    #[serde(default)]
    pub last_run_at: i64,
    #[serde(default)]
    pub last_summary: String,
    #[serde(default)]
    pub last_error: String,
    #[serde(default)]
    pub attention_plan: Option<AttentionPlan>,
}

impl MindState {
    fn new(employee_id: &str) -> Self {
        Self {
            employee_id: employee_id.to_string(),
            enabled: true,
            status: "idle".to_string(),
            active_run_id: None,
            active_thread_id: None,
            stop_reason: String::new(),
            lease_until: 0,
            cursor_seq: 0,
            dismissed_through_seq: 0,
            snoozed_until: 0,
            next_retry_at: 0,
            consecutive_failures: 0,
            last_input_fingerprint: String::new(),
            repeated_input_count: 0,
            last_run_at: 0,
            last_summary: String::new(),
            last_error: String::new(),
            attention_plan: None,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct MindSnapshot {
    pub employee_id: String,
    pub enabled: bool,
    pub status: String,
    pub active_thread_id: Option<String>,
    pub pending_events: usize,
    pub snoozed_until: i64,
    pub next_retry_at: i64,
    pub consecutive_failures: u32,
    pub last_run_at: i64,
    pub last_summary: String,
    pub last_error: String,
    pub attention_plan: Option<AttentionPlan>,
    pub memory_entries: usize,
    pub protected_memory_entries: usize,
    pub journal_limit: usize,
    pub managed_knowledge_limit: usize,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
struct MindAudit {
    run_id: String,
    employee_id: String,
    event_ids: Vec<String>,
    model: String,
    summary: String,
    actions: Vec<String>,
    status: String,
    created_at: i64,
}

#[derive(Serialize, Deserialize, Default)]
struct MindFile {
    #[serde(default)]
    next_seq: u64,
    #[serde(default)]
    states: HashMap<String, MindState>,
    #[serde(default)]
    events: HashMap<String, Vec<MindEvent>>,
    #[serde(default)]
    handoff_chains: HashMap<String, Vec<String>>,
    #[serde(default)]
    audits: Vec<MindAudit>,
}

pub struct MindStore {
    path: PathBuf,
    next_seq: u64,
    pub states: HashMap<String, MindState>,
    events: HashMap<String, Vec<MindEvent>>,
    handoff_chains: HashMap<String, Vec<String>>,
    audits: Vec<MindAudit>,
}

impl MindStore {
    pub fn load(dir: &PathBuf) -> Self {
        let path = dir.join("employee_mind.json");
        let mut file = fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<MindFile>(&s).ok())
            .unwrap_or_default();
        for state in file.states.values_mut() {
            if state.status == "running" || state.status == "preempting" {
                state.status = "cooldown".to_string();
                state.active_run_id = None;
                state.active_thread_id = None;
                state.stop_reason.clear();
                state.lease_until = 0;
                state.next_retry_at = now_ms() + BASE_RETRY_MS;
                state.last_error =
                    "应用上次退出时 Dream 仍在运行，已自动恢复为冷却状态".to_string();
            }
        }
        Self {
            path,
            next_seq: file.next_seq,
            states: file.states,
            events: file.events,
            handoff_chains: file.handoff_chains,
            audits: file.audits,
        }
    }

    pub fn save(&self) {
        if let Some(parent) = self.path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let file = MindFile {
            next_seq: self.next_seq,
            states: self.states.clone(),
            events: self.events.clone(),
            handoff_chains: self.handoff_chains.clone(),
            audits: self.audits.clone(),
        };
        let Ok(json) = serde_json::to_string_pretty(&file) else {
            return;
        };
        let tmp = self.path.with_extension("json.tmp");
        if fs::write(&tmp, json).is_ok() {
            let _ = fs::remove_file(&self.path);
            let _ = fs::rename(tmp, &self.path);
        }
    }

    fn state_mut(&mut self, employee_id: &str) -> &mut MindState {
        self.states
            .entry(employee_id.to_string())
            .or_insert_with(|| MindState::new(employee_id))
    }

    fn add_event(&mut self, mut event: MindEvent) -> bool {
        let list = self.events.entry(event.employee_id.clone()).or_default();
        if list.iter().any(|e| e.id == event.id) {
            return false;
        }
        if let Some(last) = list.last() {
            if last.kind == event.kind
                && last.task_id == event.task_id
                && last.summary == event.summary
                && event.created_at - last.created_at < 60_000
            {
                return false;
            }
        }
        self.next_seq = self.next_seq.saturating_add(1);
        event.seq = self.next_seq;
        list.push(event);
        if list.len() > MAX_EVENTS_PER_EMPLOYEE {
            let drop_n = list.len() - MAX_EVENTS_PER_EMPLOYEE;
            list.drain(0..drop_n);
        }
        true
    }

    fn pending_events(&self, employee_id: &str) -> Vec<MindEvent> {
        let Some(state) = self.states.get(employee_id) else {
            return self.events.get(employee_id).cloned().unwrap_or_default();
        };
        let after = state.cursor_seq.max(state.dismissed_through_seq);
        self.events
            .get(employee_id)
            .map(|events| {
                events
                    .iter()
                    .filter(|e| e.seq > after)
                    .take(MAX_EVENTS_PER_RUN)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    fn pending_count(&self, employee_id: &str) -> usize {
        let after = self
            .states
            .get(employee_id)
            .map(|state| state.cursor_seq.max(state.dismissed_through_seq))
            .unwrap_or(0);
        self.events
            .get(employee_id)
            .map(|events| events.iter().filter(|event| event.seq > after).count())
            .unwrap_or(0)
    }

    fn latest_seq(&self, employee_id: &str) -> u64 {
        self.events
            .get(employee_id)
            .and_then(|events| events.last())
            .map(|e| e.seq)
            .unwrap_or(0)
    }

    fn add_audit(&mut self, audit: MindAudit) {
        const KEEP_AUDITS: usize = 240;
        self.audits.push(audit);
        if self.audits.len() > KEEP_AUDITS {
            let drop_n = self.audits.len() - KEEP_AUDITS;
            self.audits.drain(0..drop_n);
        }
    }

    fn snapshot(&mut self, employee_id: &str) -> MindSnapshot {
        let pending = self.pending_count(employee_id);
        let state = self.state_mut(employee_id);
        MindSnapshot {
            employee_id: employee_id.to_string(),
            enabled: state.enabled,
            status: state.status.clone(),
            active_thread_id: state.active_thread_id.clone(),
            pending_events: pending,
            snoozed_until: state.snoozed_until,
            next_retry_at: state.next_retry_at,
            consecutive_failures: state.consecutive_failures,
            last_run_at: state.last_run_at,
            last_summary: state.last_summary.clone(),
            last_error: state.last_error.clone(),
            attention_plan: state.attention_plan.clone(),
            memory_entries: 0,
            protected_memory_entries: 0,
            journal_limit: employees::JOURNAL_KEEP,
            managed_knowledge_limit: employees::MANAGED_KNOWLEDGE_KEEP,
        }
    }
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct MindOutput {
    #[serde(default)]
    summary: String,
    #[serde(default)]
    knowledge: Vec<MindKnowledgeAction>,
    #[serde(default)]
    merge_memory: Vec<DreamMergeAction>,
    #[serde(default)]
    downgrade_memory: Vec<DreamDowngradeAction>,
    #[serde(default)]
    lessons: Vec<MindLessonAction>,
    #[serde(default)]
    verify_lessons: Vec<MindVerifyAction>,
    #[serde(default)]
    challenge_lessons: Vec<MindChallengeAction>,
    #[serde(default)]
    retire_memory: Vec<i64>,
    #[serde(default)]
    pause: bool,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct MindKnowledgeAction {
    #[serde(default)]
    title: String,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    expires_at: i64,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct MindLessonAction {
    #[serde(default)]
    summary: String,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct DreamMergeAction {
    #[serde(default)]
    title: String,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    replace_memory: Vec<i64>,
    #[serde(default)]
    expires_at: i64,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct DreamDowngradeAction {
    ts: i64,
    #[serde(default)]
    reason: String,
    #[serde(default)]
    expires_at: i64,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct MindVerifyAction {
    ts: i64,
    #[serde(default)]
    event_id: String,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct MindChallengeAction {
    ts: i64,
    #[serde(default)]
    event_id: String,
    #[serde(default)]
    reason: String,
}

impl MindOutput {
    fn has_progress(&self) -> bool {
        !self.summary.trim().is_empty()
            || !self.knowledge.is_empty()
            || !self.merge_memory.is_empty()
            || !self.downgrade_memory.is_empty()
            || !self.lessons.is_empty()
            || !self.verify_lessons.is_empty()
            || !self.challenge_lessons.is_empty()
            || !self.retire_memory.is_empty()
            || self.pause
    }
}

#[derive(Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
struct MindEscalation {
    #[serde(default)]
    question: String,
    #[serde(default)]
    brief: String,
    #[serde(default)]
    options: Vec<String>,
    #[serde(default)]
    proposed_action: String,
}

pub fn record_journal_event(
    app: &AppHandle,
    employee_id: &str,
    scope: &str,
    task_id: &str,
    task_title: &str,
    summary: &str,
    kind: &str,
    created_at: i64,
) {
    if summary.trim().is_empty() {
        return;
    }
    let severity = event_severity(kind);
    let event = MindEvent {
        seq: 0,
        id: format!("journal:{employee_id}:{created_at}:{kind}"),
        employee_id: employee_id.to_string(),
        scope: scope.to_string(),
        task_id: task_id.to_string(),
        task_title: task_title.to_string(),
        kind: kind.to_string(),
        severity,
        summary: cap(summary, 600),
        created_at,
    };
    let changed = {
        let state = app.state::<AppState>();
        let mut mind = state.mind.lock().unwrap();
        let changed = mind.add_event(event);
        if changed {
            mind.save();
        }
        changed
    };
    if changed {
        let _ = app.emit(EV_MIND, json!({ "employeeId": employee_id }));
    }
}

pub fn invalidate_brief(app: &AppHandle, employee_id: &str, reason: &str) {
    let scope = {
        let state = app.state::<AppState>();
        let scope = state
            .employees
            .lock()
            .unwrap()
            .get(employee_id)
            .map(employees::employee_scope)
            .unwrap_or_else(|| format!("emp:{employee_id}"));
        scope
    };
    let created_at = now_ms();
    let state = app.state::<AppState>();
    let mut mind = state.mind.lock().unwrap();
    let st = mind.state_mut(employee_id);
    st.attention_plan = None;
    st.last_summary = reason.to_string();
    st.last_error.clear();
    mind.add_event(MindEvent {
        seq: 0,
        id: format!(
            "knowledge:{employee_id}:{created_at}:{}",
            stable_hash(reason)
        ),
        employee_id: employee_id.to_string(),
        scope,
        task_id: String::new(),
        task_title: "知识与配置变化".to_string(),
        kind: "knowledge_changed".to_string(),
        severity: 8,
        summary: cap(reason, 600),
        created_at,
    });
    mind.save();
    let _ = app.emit(EV_MIND, json!({ "employeeId": employee_id }));
}

pub fn remove_employee(app: &AppHandle, employee_id: &str) {
    let state = app.state::<AppState>();
    let mut mind = state.mind.lock().unwrap();
    mind.states.remove(employee_id);
    mind.events.remove(employee_id);
    mind.handoff_chains
        .retain(|_, chain| !chain.iter().any(|x| x == employee_id));
    mind.save();
}

pub fn snapshot(app: &AppHandle, employee_id: &str) -> MindSnapshot {
    let mut snapshot = {
        let state = app.state::<AppState>();
        let mut mind = state.mind.lock().unwrap();
        let snapshot = mind.snapshot(employee_id);
        mind.save();
        snapshot
    };
    let memory = {
        let state = app.state::<AppState>();
        let memory = state.memory.lock().unwrap().all(employee_id);
        memory
    };
    snapshot.memory_entries = memory.len();
    snapshot.protected_memory_entries = memory.iter().filter(|entry| entry.protected).count();
    snapshot
}

pub fn check_memory_pressure(app: &AppHandle, employee_id: &str) {
    let (employee, protected_count) = {
        let state = app.state::<AppState>();
        let employee = state.employees.lock().unwrap().get(employee_id).cloned();
        let protected_count = state
            .memory
            .lock()
            .unwrap()
            .all(employee_id)
            .into_iter()
            .filter(|entry| entry.protected)
            .count();
        (employee, protected_count)
    };
    let Some(employee) = employee else {
        return;
    };
    if protected_count <= employees::MANAGED_KNOWLEDGE_KEEP {
        return;
    }
    create_system_escalation(
        app,
        &employee,
        "受保护知识已经超过单员工记忆容量",
        &format!(
            "当前有 {protected_count} 条受保护知识，自动清理不会删除【用户】维护的内容。请整理或调整后续容量策略。"
        ),
        vec![
            "由【用户】整理受保护知识".into(),
            "暂时保留并继续工作".into(),
            "暂停新增 Dream 记忆".into(),
        ],
    );
}

pub fn set_enabled(app: &AppHandle, employee_id: &str, enabled: bool) {
    let active_thread = {
        let state = app.state::<AppState>();
        let mut mind = state.mind.lock().unwrap();
        let st = mind.state_mut(employee_id);
        st.enabled = enabled;
        let active_thread = if enabled {
            if st.status == "paused" {
                st.status = "idle".to_string();
                st.snoozed_until = 0;
                st.next_retry_at = 0;
                st.last_error.clear();
            }
            None
        } else {
            let thread = st.active_thread_id.take();
            st.active_run_id = None;
            st.lease_until = 0;
            st.stop_reason = "manual".to_string();
            st.status = "paused".to_string();
            st.last_error = "Dream 已由【用户】暂停".to_string();
            thread
        };
        mind.save();
        active_thread
    };
    if let Some(thread_id) = active_thread {
        let app2 = app.clone();
        tauri::async_runtime::spawn(async move {
            employees::cancel_employee_thread(&app2, &thread_id).await;
        });
    }
    let _ = app.emit(EV_MIND, json!({ "employeeId": employee_id }));
}

pub fn resume(app: &AppHandle, employee_id: &str) {
    let state = app.state::<AppState>();
    let mut mind = state.mind.lock().unwrap();
    let st = mind.state_mut(employee_id);
    st.enabled = true;
    st.status = "idle".to_string();
    st.snoozed_until = 0;
    st.next_retry_at = 0;
    st.consecutive_failures = 0;
    st.repeated_input_count = 0;
    st.last_error.clear();
    mind.save();
    let _ = app.emit(EV_MIND, json!({ "employeeId": employee_id }));
}

pub fn is_active_thread(app: &AppHandle, thread_id: &str) -> bool {
    let state = app.state::<AppState>();
    let mind = state.mind.lock().unwrap();
    mind.states
        .values()
        .any(|s| s.active_thread_id.as_deref() == Some(thread_id))
}

pub fn manual_stop(app: &AppHandle, employee_id: &str, thread_id: &str) {
    let state = app.state::<AppState>();
    let mut mind = state.mind.lock().unwrap();
    let latest = mind.latest_seq(employee_id);
    let st = mind.state_mut(employee_id);
    if st.active_thread_id.as_deref() != Some(thread_id) {
        return;
    }
    st.status = "paused".to_string();
    st.stop_reason = "manual".to_string();
    st.dismissed_through_seq = latest;
    st.snoozed_until = now_ms() + MANUAL_SNOOZE_MS;
    st.last_error = "本轮 Dream 已由【用户】停止，相同事件批次不会自动重开".to_string();
    mind.save();
    let _ = app.emit(EV_MIND, json!({ "employeeId": employee_id }));
}

pub fn preempt_for_work(app: &AppHandle, employee_id: &str) {
    let thread = {
        let state = app.state::<AppState>();
        let mut mind = state.mind.lock().unwrap();
        let st = mind.state_mut(employee_id);
        if st.status != "running" {
            return;
        }
        st.status = "preempting".to_string();
        st.stop_reason = "work".to_string();
        st.last_error = "出现新工作，Dream 已让出执行权".to_string();
        let thread = st.active_thread_id.clone();
        mind.save();
        thread
    };
    let Some(thread_id) = thread else {
        return;
    };
    let app2 = app.clone();
    tauri::async_runtime::spawn(async move {
        employees::cancel_employee_thread(&app2, &thread_id).await;
    });
}

pub fn tick(app: &AppHandle) {
    sync_legacy_events(app);
    recover_expired_runs(app);
    let employees = {
        let state = app.state::<AppState>();
        let employees = state.employees.lock().unwrap().employees.clone();
        employees
    };
    for emp in employees.into_iter().filter(|e| e.enabled) {
        if !employees::mind_employee_idle(app, &emp) {
            continue;
        }
        let run = {
            let state = app.state::<AppState>();
            let mut mind = state.mind.lock().unwrap();
            let pending = mind.pending_events(&emp.id);
            if pending.is_empty() {
                continue;
            }
            let now = now_ms();
            let st = mind.state_mut(&emp.id);
            if !st.enabled
                || st.status == "running"
                || st.status == "preempting"
                || st.status == "paused"
                || now < st.snoozed_until
                || now < st.next_retry_at
            {
                continue;
            }
            let strong = pending.iter().any(|e| e.severity >= 8);
            if !strong && pending.len() < NORMAL_EVENT_BATCH {
                continue;
            }
            let fingerprint = event_fingerprint(&pending);
            if st.last_input_fingerprint == fingerprint {
                st.repeated_input_count = st.repeated_input_count.saturating_add(1);
            } else {
                st.last_input_fingerprint = fingerprint.clone();
                st.repeated_input_count = 1;
            }
            if st.repeated_input_count >= REPEAT_INPUT_LIMIT {
                st.status = "paused".to_string();
                st.last_error =
                    "相同事件连续处理仍无进展，Dream 已熔断并等待【用户】处理".to_string();
                mind.save();
                drop(mind);
                create_system_escalation(
                    app,
                    &emp,
                    "Dream 连续处理相同事件但没有状态推进",
                    "Dream 已自动熔断。请决定恢复、忽略该批事件，或调整 Dream 模型。",
                    vec![
                        "恢复 Dream".into(),
                        "忽略这批事件".into(),
                        "保持暂停".into(),
                    ],
                );
                continue;
            }
            let run_id = uuid::Uuid::new_v4().to_string();
            st.status = "running".to_string();
            st.active_run_id = Some(run_id.clone());
            st.active_thread_id = None;
            st.stop_reason.clear();
            st.lease_until = now + RUN_LEASE_MS;
            st.last_run_at = now;
            st.last_error.clear();
            mind.save();
            Some((run_id, pending, fingerprint))
        };
        let Some((run_id, pending, fingerprint)) = run else {
            continue;
        };
        let app2 = app.clone();
        tauri::async_runtime::spawn(async move {
            run_once(&app2, emp, run_id, pending, fingerprint).await;
        });
    }
}

async fn run_once(
    app: &AppHandle,
    emp: Employee,
    run_id: String,
    events: Vec<MindEvent>,
    fingerprint: String,
) {
    let title = format!("[{}] Dream", emp.name);
    let thread_id = employees::new_mind_thread(app, &emp, &title);
    {
        let state = app.state::<AppState>();
        let mut mind = state.mind.lock().unwrap();
        let st = mind.state_mut(&emp.id);
        if st.active_run_id.as_deref() != Some(run_id.as_str()) {
            employees::discard_employee_thread(app, &thread_id);
            return;
        }
        st.active_thread_id = Some(thread_id.clone());
        mind.save();
    }
    let context = build_context(app, &emp, &events);
    let prompt = build_prompt(&emp, &context);
    let kind = employees::mind_agent_kind(&emp);
    employees::run_employee_prompt(&kind, app, thread_id.clone(), prompt.clone()).await;

    if employees::employee_thread_cancelled(app, &thread_id) {
        employees::clear_employee_thread_cancelled(app, &thread_id);
        employees::discard_employee_thread(app, &thread_id);
        finish_cancelled(app, &emp.id, &run_id);
        return;
    }
    if employees::employee_thread_has_error(app, &thread_id) {
        employees::rename_employee_thread(app, &thread_id, &format!("[{}] Dream · 出错", emp.name));
        finish_failure(app, &emp, &run_id, "Dream 模型会话返回错误");
        return;
    }
    let output_text = employees::last_employee_assistant(app, &thread_id).unwrap_or_default();
    let Some(output) = parse_output(&output_text) else {
        employees::rename_employee_thread(
            app,
            &thread_id,
            &format!("[{}] Dream · 格式错误", emp.name),
        );
        finish_failure(app, &emp, &run_id, "Dream 没有返回有效的结构化结果");
        return;
    };
    if !output.has_progress() {
        employees::rename_employee_thread(
            app,
            &thread_id,
            &format!("[{}] Dream · 无进展", emp.name),
        );
        finish_failure(
            app,
            &emp,
            &run_id,
            "Dream 返回了结构化结果，但没有产生任何状态推进",
        );
        return;
    }
    apply_output(
        app,
        &emp,
        &run_id,
        &thread_id,
        &events,
        &fingerprint,
        output,
    )
    .await;
}

async fn apply_output(
    app: &AppHandle,
    emp: &Employee,
    run_id: &str,
    thread_id: &str,
    events: &[MindEvent],
    fingerprint: &str,
    output: MindOutput,
) {
    let last_seq = events.last().map(|e| e.seq).unwrap_or(0);
    let still_active = {
        let state = app.state::<AppState>();
        let mut mind = state.mind.lock().unwrap();
        let st = mind.state_mut(&emp.id);
        st.active_run_id.as_deref() == Some(run_id)
    };
    if !still_active {
        return;
    }
    let actions = match apply_memory_actions(app, emp, run_id, events, &output) {
        Ok(actions) => actions,
        Err(error) => {
            employees::rename_employee_thread(
                app,
                thread_id,
                &format!("[{}] Dream · 动作无效", emp.name),
            );
            finish_failure(app, emp, run_id, &error);
            return;
        }
    };
    let summary = cap(&output.summary, 600);
    {
        let state = app.state::<AppState>();
        let mut mind = state.mind.lock().unwrap();
        let st = mind.state_mut(&emp.id);
        st.status = if output.pause {
            "paused".to_string()
        } else {
            "idle".to_string()
        };
        st.active_run_id = None;
        st.active_thread_id = None;
        st.stop_reason.clear();
        st.lease_until = 0;
        st.cursor_seq = st.cursor_seq.max(last_seq);
        st.consecutive_failures = 0;
        st.next_retry_at = 0;
        st.last_input_fingerprint = fingerprint.to_string();
        st.last_summary = summary.clone();
        st.last_error.clear();
        st.attention_plan = None;
        let final_status = st.status.clone();
        mind.add_audit(MindAudit {
            run_id: run_id.to_string(),
            employee_id: emp.id.clone(),
            event_ids: events.iter().map(|event| event.id.clone()).collect(),
            model: format!(
                "{:?}:{}",
                employees::mind_agent_kind(emp),
                emp.mind_model.as_deref().unwrap_or_default()
            ),
            summary,
            actions,
            status: final_status,
            created_at: now_ms(),
        });
        mind.save();
    }
    employees::mark_mind_completed(app, &emp.id);
    employees::rename_employee_thread(app, thread_id, &format!("[{}] Dream · 已沉淀", emp.name));
    let _ = app.emit(EV_MIND, json!({ "employeeId": emp.id }));
    let _ = app.emit(employees::EV_EMPLOYEES, json!({}));
}

fn finish_cancelled(app: &AppHandle, employee_id: &str, run_id: &str) {
    let state = app.state::<AppState>();
    let mut mind = state.mind.lock().unwrap();
    let st = mind.state_mut(employee_id);
    if st.active_run_id.as_deref() != Some(run_id) {
        return;
    }
    let preempted = st.stop_reason == "work" || st.status == "preempting";
    st.active_run_id = None;
    st.active_thread_id = None;
    st.lease_until = 0;
    st.stop_reason.clear();
    if preempted {
        st.status = "cooldown".to_string();
        st.next_retry_at = now_ms() + PREEMPT_RETRY_MS;
        st.last_error = "Dream 已被工作抢占，事件保留待空闲后继续".to_string();
    } else if st.status != "paused" {
        st.status = "paused".to_string();
        st.snoozed_until = now_ms() + MANUAL_SNOOZE_MS;
        st.last_error = "本轮 Dream 已停止，相同事件批次不会自动重开".to_string();
    }
    mind.save();
    let _ = app.emit(EV_MIND, json!({ "employeeId": employee_id }));
}

fn finish_failure(app: &AppHandle, emp: &Employee, run_id: &str, error: &str) {
    let should_escalate = {
        let state = app.state::<AppState>();
        let mut mind = state.mind.lock().unwrap();
        let st = mind.state_mut(&emp.id);
        if st.active_run_id.as_deref() != Some(run_id) {
            return;
        }
        st.active_run_id = None;
        st.active_thread_id = None;
        st.lease_until = 0;
        st.stop_reason.clear();
        st.consecutive_failures = st.consecutive_failures.saturating_add(1);
        st.last_error = error.to_string();
        let exp = st.consecutive_failures.saturating_sub(1).min(8);
        let delay = (BASE_RETRY_MS.saturating_mul(1_i64 << exp)).min(MAX_RETRY_MS);
        st.next_retry_at = now_ms() + delay;
        if st.consecutive_failures >= 3 {
            st.status = "paused".to_string();
            true
        } else {
            st.status = "cooldown".to_string();
            false
        }
    };
    {
        let state = app.state::<AppState>();
        state.mind.lock().unwrap().save();
    }
    if should_escalate {
        create_system_escalation(
            app,
            emp,
            "Dream 连续运行失败",
            error,
            vec![
                "恢复 Dream".into(),
                "更换 Dream 模型".into(),
                "保持暂停".into(),
            ],
        );
    }
    let _ = app.emit(EV_MIND, json!({ "employeeId": emp.id }));
}

fn recover_expired_runs(app: &AppHandle) {
    let now = now_ms();
    let state = app.state::<AppState>();
    let mut mind = state.mind.lock().unwrap();
    let mut changed = false;
    for st in mind.states.values_mut() {
        if (st.status == "running" || st.status == "preempting")
            && st.lease_until > 0
            && st.lease_until < now
        {
            st.status = "cooldown".to_string();
            st.active_run_id = None;
            st.active_thread_id = None;
            st.stop_reason.clear();
            st.lease_until = 0;
            st.consecutive_failures = st.consecutive_failures.saturating_add(1);
            st.next_retry_at = now + BASE_RETRY_MS;
            st.last_error = "Dream 运行租约超时，已自动释放".to_string();
            changed = true;
        }
    }
    if changed {
        mind.save();
        let _ = app.emit(EV_MIND, json!({}));
    }
}

fn sync_legacy_events(app: &AppHandle) {
    let (employees, memory) = {
        let state = app.state::<AppState>();
        let employees = state.employees.lock().unwrap().employees.clone();
        let memory = state.memory.lock().unwrap().journals.clone();
        (employees, memory)
    };
    let state = app.state::<AppState>();
    let mut mind = state.mind.lock().unwrap();
    let mut changed = false;
    for emp in employees {
        let entries = memory.get(&emp.id).cloned().unwrap_or_default();
        for e in entries.into_iter().filter(|e| {
            e.ts > emp.mind_event_seeded_at
                && matches!(
                    e.kind.as_str(),
                    "outcome:done"
                        | "outcome:blocked"
                        | "outcome:stalled"
                        | "outcome:stopped"
                        | "supervision"
                )
        }) {
            let event = MindEvent {
                seq: 0,
                id: format!("journal:{}:{}:{}", emp.id, e.ts, e.kind),
                employee_id: emp.id.clone(),
                scope: employees::employee_scope(&emp),
                task_id: e.task_id,
                task_title: e.task_title,
                kind: e.kind.clone(),
                severity: event_severity(&e.kind),
                summary: cap(&e.summary, 600),
                created_at: e.ts,
            };
            changed |= mind.add_event(event);
        }
    }
    if changed {
        mind.save();
    }
}

fn build_context(app: &AppHandle, emp: &Employee, events: &[MindEvent]) -> String {
    let memory = {
        let state = app.state::<AppState>();
        let memory = state.memory.lock().unwrap().all(&emp.id);
        memory
    };
    let mut lessons: Vec<&JournalEntry> = memory
        .iter()
        .filter(|e| e.kind == "lesson" || e.kind == "lesson:challenged")
        .collect();
    lessons.sort_by(|a, b| {
        b.negative_evidence
            .cmp(&a.negative_evidence)
            .then(b.positive_evidence.cmp(&a.positive_evidence))
            .then(b.ts.cmp(&a.ts))
    });
    let mut knowledge: Vec<&JournalEntry> = memory
        .iter()
        .filter(|e| e.pinned && e.kind != "lesson")
        .collect();
    knowledge.sort_by(|a, b| b.ts.cmp(&a.ts));
    let mut merge_candidates: Vec<&JournalEntry> = memory
        .iter()
        .filter(|e| {
            !e.protected
                && e.superseded_by.is_none()
                && !e.pinned
                && (e.kind.starts_with("memory") || e.kind.starts_with("knowledge"))
        })
        .collect();
    merge_candidates.sort_by(|a, b| {
        b.negative_evidence
            .cmp(&a.negative_evidence)
            .then(a.positive_evidence.cmp(&b.positive_evidence))
            .then(b.ts.cmp(&a.ts))
    });
    let mut out = String::new();
    out.push_str("【新增经历事件】\n");
    for event in events {
        out.push_str(&format!(
            "- eventId={} seq={} kind={} task={} severity={} {}\n",
            event.id,
            event.seq,
            event.kind,
            if event.task_title.trim().is_empty() {
                event.task_id.as_str()
            } else {
                event.task_title.as_str()
            },
            event.severity,
            cap(&event.summary, 420)
        ));
    }
    out.push_str("\n【现有试行、内化与受挑战守则】\n");
    if lessons.is_empty() {
        out.push_str("（无）\n");
    } else {
        for lesson in lessons.into_iter().take(10) {
            out.push_str(&format!(
                "- ts={} state={} evidence={} userFeedback={} positive={} negative={} {}\n",
                lesson.ts,
                if lesson.kind == "lesson:challenged" {
                    "受挑战"
                } else if lesson.pinned {
                    "内化"
                } else {
                    "试行"
                },
                lesson.evidence,
                lesson.user_feedback,
                lesson.positive_evidence,
                lesson.negative_evidence,
                cap(&lesson.summary, 260)
            ));
        }
    }
    out.push_str("\n【长期知识】\n");
    if knowledge.is_empty() {
        out.push_str("（无）\n");
    } else {
        for item in knowledge.into_iter().take(8) {
            out.push_str(&format!(
                "- ts={} confidence={:.2} userFeedback={} positive={} negative={} {}\n",
                item.ts,
                item.confidence,
                item.user_feedback,
                item.positive_evidence,
                item.negative_evidence,
                cap(&item.summary, 260)
            ));
        }
    }
    out.push_str("\n【近期可合并或降级的记忆】\n");
    if merge_candidates.is_empty() {
        out.push_str("（无）\n");
    } else {
        for item in merge_candidates.into_iter().take(12) {
            out.push_str(&format!(
                "- ts={} kind={} confidence={:.2} userFeedback={} positive={} negative={} {}\n",
                item.ts,
                item.kind,
                item.confidence,
                item.user_feedback,
                item.positive_evidence,
                item.negative_evidence,
                cap(&item.summary, 260)
            ));
        }
    }
    cap(&out, 9_000)
}

fn build_prompt(emp: &Employee, context: &str) -> String {
    format!(
        "你是数字员工「{name}」的 Dream。员工空闲时才会做梦。\
做梦不是工作流，不处理业务，不判断下一步路由，不上奏业务问题，不接力给同事，不修改项目代码。\
做梦只负责反思学习：把白天的经历压缩成更精炼的知识，合并相似经验，降级不常用或价值变低的记忆。\
你不做业务任务，不判断下一步路由，不上奏业务问题，不接力给同事，不修改项目代码。所有持久化动作必须放进最终 JSON，由应用验证后执行。\n\n\
【岗位说明】\n{charter}\n\n\
{context}\n\n\
处理要求：\n\
- 只整理可复用结论。事实、接口、长期规则和经验总结写入 knowledge；行为规则和踩坑原因写入 lessons。不要补没有证据的新结论。普通进度、尝试过程、临时想法不要写入记忆。\n\
- 现有长期知识里已经覆盖的结论不要换一种说法再写入 knowledge；尤其不要把【用户】维护的知识改写成 Dream 自己的新知识。\n\
- userFeedback=1 是【用户】点赞，userFeedback=-1 是【用户】点踩，优先级高于自动统计。点赞内容优先保留和强化，点踩内容优先复核、降级或修正，但仍需结合具体证据，不能机械删除。\n\
- 多条相似经验能抽象成一条更精炼结论时，写入 mergeMemory，并用 replaceMemory 标出被替代的旧 ts。replaceMemory 只能填写非用户、非受保护的旧记忆；【用户】维护的知识只能作为判断依据，不能被替换、合并或降级。\n\
- 不常用、重复、过窄、时效性变低但还不该删除的内容，写入 downgradeMemory；系统会在多次降级后才遗忘。用户添加的知识永远不能降级或遗忘。\n\
- 【用户】纠正、受阻、停滞和被叫停优先处理；negative>=2 的守则/记忆优先 challengeLessons、downgradeMemory 或 retireMemory。内化守则遇到反证但尚需修正时写入 challengeLessons，确认过时或有害时再写入 retireMemory。\n\
- 只有现有试行守则被真实工作结果反复印证（positive>=2 且 negative=0），并且本批含 outcome:done 事件时，才写入 verifyLessons 并引用该 eventId。不要为了凑数强化。\n\
- 无法判断是否该沉淀时宁可不写。pause=true 只表示 Dream 暂停，不影响员工工作。\n\
- 最终必须输出一个 JSON 块，除此之外的总结保持简短。格式如下：\n\
===MIND_JSON===\n\
{{\"knowledge\":[{{\"title\":\"知识\",\"summary\":\"事实\",\"expiresAt\":0}}],\
\"mergeMemory\":[{{\"title\":\"合并后的知识\",\"summary\":\"更精炼的结论\",\"replaceMemory\":[123,456],\"expiresAt\":0}}],\
\"downgradeMemory\":[{{\"ts\":123,\"reason\":\"重复/过窄/不常用/时效降低\",\"expiresAt\":0}}],\
\"lessons\":[{{\"summary\":\"当情境发生时，就执行动作\"}}],\"verifyLessons\":[{{\"ts\":123,\"eventId\":\"事件ID\"}}],\
\"challengeLessons\":[{{\"ts\":123,\"eventId\":\"事件ID\",\"reason\":\"反证原因\"}}],\"retireMemory\":[456],\
\"summary\":\"本轮 Dream 摘要\",\"pause\":false}}\n\
===END===",
        name = emp.name,
        charter = if emp.charter.trim().is_empty() {
            "（未设置岗位说明）"
        } else {
            emp.charter.trim()
        },
        context = context,
    )
}

fn apply_memory_actions(
    app: &AppHandle,
    emp: &Employee,
    run_id: &str,
    events: &[MindEvent],
    output: &MindOutput,
) -> Result<Vec<String>, String> {
    if output.knowledge.len() > 5
        || output.merge_memory.len() > 4
        || output.downgrade_memory.len() > 6
        || output.lessons.len() > 3
        || output.verify_lessons.len() > 5
        || output.challenge_lessons.len() > 5
        || output.retire_memory.len() > 5
    {
        return Err("Dream 单次记忆动作超过允许上限".to_string());
    }
    let existing = {
        let state = app.state::<AppState>();
        let existing = state.memory.lock().unwrap().all(&emp.id);
        existing
    };
    for action in &output.verify_lessons {
        let Some(event) = events.iter().find(|event| event.id == action.event_id) else {
            continue;
        };
        if event.kind != "outcome:done" {
            continue;
        }
        if !existing
            .iter()
            .any(|entry| entry.ts == action.ts && entry.kind == "lesson")
        {
            continue;
        }
    }
    for action in &output.challenge_lessons {
        let Some(event) = events.iter().find(|event| event.id == action.event_id) else {
            continue;
        };
        if event.kind == "outcome:done" {
            continue;
        }
        let Some(entry) = existing.iter().find(|entry| entry.ts == action.ts) else {
            continue;
        };
        if entry.kind != "lesson" {
            continue;
        }
        if entry.protected {
            continue;
        }
    }
    for ts in &output.retire_memory {
        let Some(entry) = existing.iter().find(|entry| entry.ts == *ts) else {
            continue;
        };
        if entry.protected || entry.source == "user" || entry.kind.ends_with(":user") {
            continue;
        }
    }
    for action in &output.merge_memory {
        if action.summary.trim().is_empty() {
            continue;
        }
        if action.replace_memory.len() > 8 {
            return Err("Dream 单次合并引用了过多旧记忆".to_string());
        }
        for ts in &action.replace_memory {
            let Some(entry) = existing.iter().find(|entry| entry.ts == *ts) else {
                continue;
            };
            if entry.protected || entry.source == "user" || entry.kind.ends_with(":user") {
                continue;
            }
        }
    }
    for action in &output.downgrade_memory {
        let Some(entry) = existing.iter().find(|entry| entry.ts == action.ts) else {
            continue;
        };
        if entry.protected || entry.source == "user" || entry.kind.ends_with(":user") {
            continue;
        }
    }

    let source = format!("mind:{run_id}");
    let mut actions = Vec::new();
    let state = app.state::<AppState>();
    let mut memory = state.memory.lock().unwrap();
    for (index, action) in output.knowledge.iter().enumerate() {
        let summary = cap(action.summary.trim(), 600);
        if summary.is_empty() {
            continue;
        }
        let created_at = now_ms() + index as i64;
        let added = memory.append_unique_managed(
            &emp.id,
            JournalEntry {
                ts: created_at,
                task_id: run_id.to_string(),
                task_title: if action.title.trim().is_empty() {
                    "Dream 知识".to_string()
                } else {
                    cap(action.title.trim(), 120)
                },
                summary,
                pinned: true,
                kind: "knowledge".to_string(),
                evidence: 0,
                source: source.clone(),
                protected: false,
                confidence: 0.65,
                last_used_at: 0,
                hit_count: 0,
                expires_at: action.expires_at.max(0),
                superseded_by: None,
                positive_evidence: 0,
                negative_evidence: 0,
                user_feedback: 0,
                evidence_tasks: Vec::new(),
            },
        );
        if added {
            actions.push("新增长期知识".to_string());
        }
    }
    for (index, action) in output.lessons.iter().enumerate() {
        let summary = cap(action.summary.trim(), 500);
        if summary.is_empty() {
            continue;
        }
        let added = memory.append_unique_managed(
            &emp.id,
            JournalEntry {
                ts: now_ms() + output.knowledge.len() as i64 + index as i64,
                task_id: run_id.to_string(),
                task_title: "守则（试行）".to_string(),
                summary,
                pinned: false,
                kind: "lesson".to_string(),
                evidence: 0,
                source: source.clone(),
                protected: false,
                confidence: 0.45,
                last_used_at: 0,
                hit_count: 0,
                expires_at: 0,
                superseded_by: None,
                positive_evidence: 0,
                negative_evidence: 0,
                user_feedback: 0,
                evidence_tasks: Vec::new(),
            },
        );
        if added {
            actions.push("新增试行守则".to_string());
        }
    }
    for (index, action) in output.merge_memory.iter().enumerate() {
        let summary = cap(action.summary.trim(), 700);
        if summary.is_empty() {
            continue;
        }
        let created_at =
            now_ms() + output.knowledge.len() as i64 + output.lessons.len() as i64 + index as i64;
        let title = if action.title.trim().is_empty() {
            "Dream 合并".to_string()
        } else {
            cap(action.title.trim(), 120)
        };
        let added = memory.append_unique_managed(
            &emp.id,
            JournalEntry {
                ts: created_at,
                task_id: run_id.to_string(),
                task_title: title,
                summary,
                pinned: true,
                kind: "knowledge".to_string(),
                evidence: 0,
                source: source.clone(),
                protected: false,
                confidence: 0.72,
                last_used_at: 0,
                hit_count: 0,
                expires_at: action.expires_at.max(0),
                superseded_by: None,
                positive_evidence: 0,
                negative_evidence: 0,
                user_feedback: 0,
                evidence_tasks: Vec::new(),
            },
        );
        if added {
            memory.mark_superseded(&emp.id, &action.replace_memory, created_at);
            actions.push("Dream 合并相似经验".to_string());
        }
    }
    for action in &output.downgrade_memory {
        if memory.downgrade_memory(&emp.id, action.ts, &action.reason, action.expires_at) {
            actions.push("Dream 降级低价值记忆".to_string());
        }
    }
    for action in &output.verify_lessons {
        let can_verify = existing
            .iter()
            .find(|entry| entry.ts == action.ts && entry.kind == "lesson")
            .map(|entry| entry.positive_evidence >= 2 && entry.negative_evidence == 0)
            .unwrap_or(false);
        if !can_verify {
            actions.push("试行守则证据不足，暂缓内化".to_string());
            continue;
        }
        let Some(event) = events.iter().find(|event| event.id == action.event_id) else {
            continue;
        };
        let evidence_key = if event.task_id.trim().is_empty() {
            event.id.as_str()
        } else {
            event.task_id.as_str()
        };
        let Some((_, promoted)) = memory.verify_lesson(&emp.id, action.ts, evidence_key) else {
            continue;
        };
        actions.push(if promoted {
            "试行守则已内化".to_string()
        } else {
            "试行守则增加独立证据".to_string()
        });
    }
    for action in &output.challenge_lessons {
        let Some(event) = events.iter().find(|event| event.id == action.event_id) else {
            continue;
        };
        let evidence_key = if event.task_id.trim().is_empty() {
            event.id.as_str()
        } else {
            event.task_id.as_str()
        };
        if !memory.challenge_lesson(&emp.id, action.ts, evidence_key, &action.reason) {
            continue;
        }
        actions.push("内化守则进入受挑战状态".to_string());
    }
    for ts in &output.retire_memory {
        if !memory.forget_managed(&emp.id, *ts) {
            continue;
        }
        actions.push("记忆或守则已退役".to_string());
    }
    Ok(actions)
}

fn parse_output(text: &str) -> Option<MindOutput> {
    let start = text.rfind("===MIND_JSON===")?;
    let rest = &text[start + "===MIND_JSON===".len()..];
    let end = rest.find("===END===")?;
    let raw = rest[..end]
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    serde_json::from_str(raw).ok()
}

fn create_mind_decision(
    app: &AppHandle,
    emp: &Employee,
    run_id: &str,
    thread_id: Option<&str>,
    escalation: MindEscalation,
) {
    let added = crate::notice::emit_decision(
        app,
        emp,
        &format!("mind:{}", emp.id),
        run_id,
        &format!("{} 的 Dream 需要裁决", emp.name),
        &cap(&escalation.brief, 1_500),
        &cap(&escalation.question, 500),
        "other",
        cap_vec(escalation.options, 5, 180),
        thread_id.map(str::to_string),
        Some(format!("mind:{}:{}", emp.id, stable_hash(&escalation.question))),
        "mind",
        &cap(&escalation.proposed_action, 500),
        "Dream 已暂停相关学习，数字员工仍可继续工作。",
    )
    .is_some();
    {
        let state = app.state::<AppState>();
        let mut mind = state.mind.lock().unwrap();
        let st = mind.state_mut(&emp.id);
        st.status = "paused".to_string();
        st.last_error = "Dream 已上奏御书房，等待【用户】裁决".to_string();
        mind.save();
    }
    if added {
        let _ = app.emit(employees::EV_DECISIONS, json!({}));
        let _ = app.emit(employees::EV_DECISIONS_OPEN, json!({}));
    }
    let _ = app.emit(EV_MIND, json!({ "employeeId": emp.id }));
}

fn create_system_escalation(
    app: &AppHandle,
    emp: &Employee,
    question: &str,
    brief: &str,
    options: Vec<String>,
) {
    create_mind_decision(
        app,
        emp,
        &uuid::Uuid::new_v4().to_string(),
        None,
        MindEscalation {
            question: question.to_string(),
            brief: brief.to_string(),
            options,
            proposed_action: "检查 Dream 状态与模型配置后再恢复".to_string(),
        },
    );
}

pub fn on_decision(app: &AppHandle, decision: &Decision) {
    if decision.source != "mind" {
        return;
    }
    let answer = decision.answer.clone().unwrap_or_default();
    record_journal_event(
        app,
        &decision.employee_id,
        &decision.scope,
        &decision.mark_key,
        &decision.task_title,
        &format!("【用户】对 Dream 的裁决：{answer}"),
        "supervision",
        now_ms(),
    );
    let state = app.state::<AppState>();
    let mut mind = state.mind.lock().unwrap();
    let latest = mind.latest_seq(&decision.employee_id);
    let st = mind.state_mut(&decision.employee_id);
    if decision.status == "rejected"
        || decision.status == "shelved"
        || answer.contains("忽略")
        || answer.contains("保持暂停")
    {
        st.dismissed_through_seq = latest;
        st.status = "paused".to_string();
    } else {
        st.status = "idle".to_string();
        st.enabled = true;
        st.snoozed_until = 0;
        st.next_retry_at = 0;
        st.consecutive_failures = 0;
        st.repeated_input_count = 0;
    }
    st.last_error.clear();
    mind.save();
    let _ = app.emit(EV_MIND, json!({ "employeeId": decision.employee_id }));
}

fn event_severity(kind: &str) -> u8 {
    match kind {
        "supervision" | "outcome:stopped" => 10,
        "outcome:stalled" => 9,
        "outcome:blocked" => 8,
        "outcome:done" => 3,
        _ => 2,
    }
}

fn event_fingerprint(events: &[MindEvent]) -> String {
    let mut hasher = DefaultHasher::new();
    for event in events {
        event.kind.hash(&mut hasher);
        event.scope.hash(&mut hasher);
        event.task_id.hash(&mut hasher);
        event.summary.hash(&mut hasher);
    }
    format!("{:016x}", hasher.finish())
}

fn stable_hash(text: &str) -> String {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn cap(text: &str, max_chars: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max_chars {
        flat
    } else {
        flat.chars().take(max_chars).collect::<String>() + "…"
    }
}

fn cap_vec(items: Vec<String>, max_items: usize, max_chars: usize) -> Vec<String> {
    let mut seen = HashSet::new();
    items
        .into_iter()
        .map(|x| cap(&x, max_chars))
        .filter(|x| !x.is_empty() && seen.insert(x.clone()))
        .take(max_items)
        .collect()
}
