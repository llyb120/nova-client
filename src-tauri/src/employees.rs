//! 数字员工：常驻员工 + 会话主战场 + 账本门锁 + 记忆资料库 + Mind 守门。
//!
//! 心跳醒来后像一个尽责的人那样按序做事（run_cycle）：
//! ① 先消费 Notice 批复注入（主管已批、发送方预声明的 ActionPlan 落地）；
//! ② 续做在手单子（候旨中的只续租保活）；
//! ③ 确定性领取账本「待处理」的交办单子；
//! ④ 最后按常驻职责巡查找新活（侦察 → PLAN → 原子认领 → 开发）。
//! marks 账本保存待办与占用，只由 open 决定下一张单；过程、判断、讨论、上奏和接力都留在会话。
//! 协作统一走 Notice 广播：发送时声明处理后 ActionPlan，不再靠候旨→领旨写死流程。
//! 每轮出口统一成下一步动作，系统只按动作路由到自己、同事、御书房、完成或释放。
//!
//! 像人一样学习的进化闭环（经历事件 → Mind → 结论记忆 → 验证 → 内化）：
//! 办成/受阻/停滞/被叫停/主管批示只作为 Mind 事件进入后台守门；
//! 只有 Mind 或员工明确沉淀出的 memo/learn/lesson 才进入记忆库。
//! 守则被实践印证就 lesson-verify（满 LESSON_PROMOTE_AT 次自动内化），被证伪就 forget。
//!
//! 执行统一复用 AcpManager / CodexManager 的 run_prompt。

use crate::marks::{render_digest, ClaimOutcome, Mark, EV_MARKS};
use crate::threads::{
    embed_attachment_data, now_ms, AgentKind, Item, PromptImage, Thread, Worktree,
};
use crate::AppState;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Emitter, Manager};

pub const EV_EMPLOYEES: &str = "employees:changed";
pub const EV_TASKS: &str = "tasks:changed";
pub const EV_DECISIONS: &str = "decisions:changed";
/// 系统通知点击「员工上奏」时发给前端：直达御书房。
pub const EV_DECISIONS_OPEN: &str = "decisions:open";
/// 讨论/协作账本认领的租约（同 directive）：够长覆盖多轮开发 + 等待用户决策。
const DISCUSS_LEASE_MS: i64 = 2 * 60 * 60 * 1000;

/// 自动工作日志的保留上限。
pub(crate) const JOURNAL_KEEP: usize = 500;
/// Mind/agent 维护的长期知识与内化守则上限。用户保护的知识不参与自动淘汰。
pub(crate) const MANAGED_KNOWLEDGE_KEEP: usize = 120;
/// Dream 多次确认一条记忆低价值后才遗忘，避免一次误判直接删除。
const DREAM_FORGET_AFTER_DOWNGRADES: u32 = 3;
/// 每次注入的相关记忆总量。上下文宁缺毋滥，避免近期但无关的经历挤占当前任务。
const MEMORY_INJECT: usize = 8;
const KNOWLEDGE_INJECT: usize = 4;
const PROVEN_LESSON_INJECT: usize = 3;
const SEMANTIC_MIN_SIMILARITY: f32 = 0.35;
/// 守则「实践验证」达到几次自动内化为长期守则（像人：一次教训 + 一次印证才算学会）
const LESSON_PROMOTE_AT: u32 = 2;
/// 每轮开发注入的「试行守则」条数上限（太多会稀释注意力）
const TRIAL_LESSON_INJECT: usize = 4;

fn task_memory_usage() -> &'static std::sync::Mutex<HashMap<String, HashSet<i64>>> {
    static U: std::sync::OnceLock<std::sync::Mutex<HashMap<String, HashSet<i64>>>> =
        std::sync::OnceLock::new();
    U.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

fn task_memory_usage_key(employee_id: &str, task_id: &str) -> String {
    format!("{employee_id}\u{1}{task_id}")
}

fn remember_task_memory_usage(employee_id: &str, task_id: Option<&str>, used_ts: &[i64]) {
    let Some(task_id) = task_id.map(str::trim).filter(|s| !s.is_empty()) else {
        return;
    };
    if used_ts.is_empty() {
        return;
    }
    let key = task_memory_usage_key(employee_id, task_id);
    let mut usage = task_memory_usage().lock().unwrap();
    let set = usage.entry(key).or_default();
    set.extend(used_ts.iter().copied());
}

fn take_task_memory_usage(employee_id: &str, task_id: &str) -> Vec<i64> {
    let key = task_memory_usage_key(employee_id, task_id);
    task_memory_usage()
        .lock()
        .unwrap()
        .remove(&key)
        .map(|set| set.into_iter().collect())
        .unwrap_or_default()
}

fn apply_task_memory_evidence(
    app: &AppHandle,
    employee_id: &str,
    task_id: &str,
    outcome_kind: &str,
) {
    let used = take_task_memory_usage(employee_id, task_id);
    if used.is_empty() {
        return;
    }
    let state = app.state::<AppState>();
    state.memory.lock().unwrap().apply_usage_evidence(
        employee_id,
        &used,
        task_id,
        outcome_kind == "outcome:done",
    );
}

fn default_heartbeat() -> u64 {
    300
}

fn default_true() -> bool {
    true
}

fn default_origin() -> String {
    "user".to_string()
}

/// 一名常驻数字员工的配置。
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Employee {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub agent_kind: AgentKind,
    #[serde(default)]
    pub model: Option<String>,
    /// 巡查/心跳会话使用的后端（可与工作后端不同；留空 = 沿用工作后端）。
    #[serde(default)]
    pub heartbeat_agent_kind: Option<AgentKind>,
    /// 巡查/心跳专用模型（配合 heartbeat_agent_kind；留空 = 沿用工作模型）。
    /// 让「找活巡查」用便宜模型、真正开发用强模型，各占一个会话。
    #[serde(default)]
    pub heartbeat_model: Option<String>,
    /// Mind 专用后端（可与工作/巡查不同；留空 = 沿用巡查后端，再沿用工作后端）。
    #[serde(default, alias = "learningAgentKind", alias = "learning_agent_kind")]
    pub mind_agent_kind: Option<AgentKind>,
    /// Mind 专用模型（配合 mind_agent_kind；留空 = 沿用巡查模型，再沿用工作模型）。
    #[serde(default, alias = "learningModel", alias = "learning_model")]
    pub mind_model: Option<String>,
    /// 会话模式（统一为 build / plan）。build = 放开全部权限免确认；旧值 bypass 视同 build。
    #[serde(default)]
    pub mode: Option<String>,
    /// 岗位说明书（启动提示词）
    #[serde(default)]
    pub charter: String,
    /// 工作目录
    pub cwd: String,
    /// 自动心跳开关：false = 完全不做周期性自主行动（不巡查、不自动领活），
    /// 只在用户交办、御书房朱批、手动「立即执行一轮」时行动。旧数据缺省为开。
    #[serde(default = "default_true")]
    pub heartbeat_enabled: bool,
    /// 心跳周期（秒）
    #[serde(default = "default_heartbeat")]
    pub heartbeat_secs: u64,
    /// 上班时间（可选）：设置后，非上班时段自动休眠（不巡查、不开发）；None = 7×24 在岗。
    #[serde(default)]
    pub work_hours: Option<WorkHours>,
    /// 是否在岗
    #[serde(default)]
    pub enabled: bool,
    /// 是否自主找活：收件箱空时按 directive 去巡查
    #[serde(default)]
    pub self_directed: bool,
    /// 是否允许员工按任务需要使用独立 git worktree。
    #[serde(default, alias = "useWorktree", alias = "use_worktree")]
    pub allow_worktree: bool,
    /// 常驻职责：去哪里、找什么样的单子、怎么处理
    #[serde(default)]
    pub directive: String,
    /// 协作账本命名空间（同一 scope 的员工共享去重/互斥）；空 = 用私有 scope
    #[serde(default)]
    pub mark_scope: String,
    /// 账本是否共享到中转站（同组队友跨机器去重/互斥/接力）；false = 仅本机
    #[serde(default)]
    pub shared_ledger: bool,
    /// 讨论伙伴：开发中需要协议约定时自动联动的其他数字员工（本机或跨机队友）。
    /// 与 mark_scope 解耦——即使不自主找活也可以预先指定协作关系。
    #[serde(default)]
    pub partners: Vec<Partner>,
    /// 上次心跳执行时间（ms）
    #[serde(default)]
    pub last_heartbeat_ms: i64,
    /// 旧记忆补种为 Mind 事件的进度时间（ms）。旧 lastReflectMs 自动迁移到这里。
    #[serde(default, alias = "lastReflectMs", alias = "last_reflect_ms")]
    pub mind_event_seeded_at: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

/// 上班时间：员工只在此时段自动干活，时段外自动休眠。
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct WorkHours {
    /// 上班时刻 "HH:MM"（24 小时制）
    pub start: String,
    /// 下班时刻 "HH:MM"；start==end 视为全天；end<start 视为跨夜班（如 22:00→06:00）
    pub end: String,
    /// 上班的星期（1=周一 … 7=周日）；留空 = 每天都上班
    #[serde(default)]
    pub days: Vec<u8>,
}

/// 解析 "HH:MM" 为「当天分钟数」(0..1440)；非法返回 None。
fn parse_hm(s: &str) -> Option<u32> {
    let s = s.trim();
    let (h, m) = s.split_once(':')?;
    let h: u32 = h.trim().parse().ok()?;
    let m: u32 = m.trim().parse().ok()?;
    if h > 23 || m > 59 {
        return None;
    }
    Some(h * 60 + m)
}

/// 当前本地时间是否落在员工的上班时段内。未配置上班时间 → 恒为 true（7×24 在岗）。
fn within_work_hours(emp: &Employee) -> bool {
    use chrono::{Datelike, Local, Timelike};
    let Some(wh) = &emp.work_hours else {
        return true;
    };
    let now = Local::now();
    // 星期过滤（1=周一 … 7=周日）；留空表示每天。
    if !wh.days.is_empty() {
        let dow = now.weekday().number_from_monday() as u8;
        if !wh.days.contains(&dow) {
            return false;
        }
    }
    let (Some(start), Some(end)) = (parse_hm(&wh.start), parse_hm(&wh.end)) else {
        return true; // 时刻非法 → 不做限制，避免误休眠
    };
    let cur = now.hour() * 60 + now.minute();
    if start == end {
        true // 全天
    } else if start < end {
        cur >= start && cur < end // 常规白班
    } else {
        cur >= start || cur < end // 跨夜班
    }
}

/// 一个讨论伙伴：员工在开发中需要与之约定接口/协议时自动联动的对象。
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Partner {
    /// local（本机的另一名数字员工）| remote（同组队友机器上的数字员工）
    pub kind: String,
    /// 伙伴数字员工的名字（local=本机员工名；remote=队友那名员工的名字）
    pub name: String,
    /// remote 专用：队友在团队里的展示名（用于经中转站定向投递）
    #[serde(default)]
    pub peer: Option<String>,
}

/// 一道奏折：员工推进单子时拿不准的大事，具折上奏「御书房」，
/// 主管朱批准奏后员工领旨继续。候旨期间该单子保持认领（别人抢不到）。
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Decision {
    pub id: String,
    pub employee_id: String,
    pub employee_name: String,
    /// 关联的账本 scope / 单号（据此在下一轮把答复注入对应单子的开发）
    pub scope: String,
    pub mark_key: String,
    pub task_title: String,
    #[serde(default)]
    pub thread_id: Option<String>,
    /// 背景/上下文：员工说明「这是什么、为什么要你拍板、你的选择会带来什么」，让主管一眼看懂。
    #[serde(default)]
    pub brief: String,
    /// 决策类型：approve（是否推进）| choose（方案选型）| input（补充信息/内容）| priority（排优先级）| other
    #[serde(default)]
    pub category: String,
    pub question: String,
    #[serde(default)]
    pub options: Vec<String>,
    /// 阻塞签名：同一员工、同一单、同一类错误只保留一道候旨，避免重复弹窗。
    #[serde(default)]
    pub blocker_signature: String,
    /// 结构化建议动作：准奏后系统/员工优先按这个动作执行，而不是只读自然语言。
    #[serde(default)]
    pub proposed_action: String,
    /// 系统已自动处理过的诊断备注，用于必要时学习，不把流水写进长期记忆。
    #[serde(default)]
    pub auto_note: String,
    /// employee（普通员工奏折）| mind（Mind 自愈/纠错无法收敛时上奏）
    #[serde(default)]
    pub source: String,
    /// pending（候旨）| resolved（已准奏，待员工领旨）| consumed（已领旨）
    /// | shelved（留中不发，员工自行斟酌）| rejected（驳回）
    /// | withdrawn（单子完结自动撤回）
    /// | report（完工汇报待处理）| read（汇报已阅）| reviewed（汇报已批阅）
    pub status: String,
    #[serde(default)]
    pub answer: Option<String>,
    pub created_at: i64,
    #[serde(default)]
    pub resolved_at: i64,
}

/// 派给员工的一项任务（收件箱条目）。
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    pub id: String,
    pub employee_id: String,
    pub title: String,
    pub brief: String,
    /// queued | working | done | blocked
    pub status: String,
    /// 来源：user（用户派）| self（自主找活）| handoff:<同事名>（同事派）
    #[serde(default = "default_origin")]
    pub origin: String,
    #[serde(default)]
    pub thread_id: Option<String>,
    #[serde(default)]
    pub result: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// 数字员工的一条持续工作流。多个连续单子可以共用同一条分支/会话。
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Workflow {
    pub id: String,
    pub employee_id: String,
    pub scope: String,
    pub key: String,
    pub title: String,
    pub repo: String,
    pub cwd: String,
    pub branch: String,
    #[serde(default)]
    pub base_branch: String,
    pub use_worktree: bool,
    #[serde(default)]
    pub use_current_branch: bool,
    #[serde(default)]
    pub thread_id: Option<String>,
    /// active | waiting | merge_ready | closed
    pub status: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct JournalEntry {
    pub ts: i64,
    pub task_id: String,
    pub task_title: String,
    pub summary: String,
    /// 置顶（长期注入、不参与自动截断）：用户维护的长期知识，或已内化的守则
    #[serde(default)]
    pub pinned: bool,
    /// 条目类型（进化机制的原料标签）：
    /// - ""                普通记忆（memo/讨论/系统留痕）
    /// - "memory"          员工主动沉淀的可复用记忆
    /// - "knowledge"       员工主动沉淀的长期知识
    /// - "knowledge:user"  用户手动添加的长期知识
    /// - "lesson"          守则：员工从挫折中给自己立的行为规则（pinned=false 试行中，true 已内化）
    /// - "lesson:challenged" 守则：已被反证挑战，暂停注入，待 Mind 或【用户】修正
    /// - "lesson:retired"  守则：已淘汰，短期保留证伪证据
    /// - "outcome:done"    经历：单子办成（自动留痕）
    /// - "outcome:blocked" 经历：受阻放下（自动留痕，复盘重点）
    /// - "outcome:stalled" 经历：多轮无进展被自动释放（复盘重点）
    /// - "outcome:stopped" 经历：被主管手动叫停（强负反馈，复盘重点）
    /// - "supervision"     经历：主管批示/留中（主管的判断是最好的教材）
    #[serde(default)]
    pub kind: String,
    /// 守则的「实践验证」次数：lesson-verify 累积；达到阈值自动内化（pinned=true）
    #[serde(default)]
    pub evidence: u32,
    /// user | agent | system。用户来源可受保护，Mind 不得自动淘汰。
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub protected: bool,
    #[serde(default)]
    pub confidence: f32,
    #[serde(default)]
    pub last_used_at: i64,
    #[serde(default)]
    pub hit_count: u32,
    #[serde(default)]
    pub expires_at: i64,
    #[serde(default)]
    pub superseded_by: Option<i64>,
    #[serde(default)]
    pub positive_evidence: u32,
    #[serde(default)]
    pub negative_evidence: u32,
    /// -1 = 用户点踩，0 = 未反馈，1 = 用户点赞。
    #[serde(default)]
    pub user_feedback: i8,
    #[serde(default)]
    pub evidence_tasks: Vec<String>,
}

// ===== 命令收件箱（agent 工具 → 应用）=====
//
// agent 用自带 shell 调 `nova <工具>` 时，写操作（接力/讨论/记忆/收尾等）不能直接改运行中
// 应用的内存状态（CLI 是独立进程）。改为把一条命令追加进「收件箱」文件，应用后台循环读取并执行。
// 这样所有协作动作都由工具触发、实时（秒级）生效，且全后端可靠。

/// 一条来自 agent 工具的协作命令。
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct InboxCommand {
    /// relay | memo | learn | forget | done | blocked
    pub kind: String,
    /// 发起动作的员工 id（actor）
    #[serde(default)]
    pub from: String,
    #[serde(default)]
    pub scope: String,
    #[serde(default)]
    pub key: String,
    #[serde(default)]
    pub title: String,
    /// relay：接力对象 —— 员工名 | "self"（自己）| "user"（交用户决策）
    #[serde(default)]
    pub to: String,
    /// relay：接力类型 —— work（工作）| discuss（讨论）| decision（决策，等价 to=user）
    #[serde(default)]
    pub relay_kind: String,
    #[serde(default)]
    pub brief: String,
    #[serde(default)]
    pub question: String,
    #[serde(default)]
    pub options: Vec<String>,
    /// relay decision：决策类型 approve|choose|input|priority|other
    #[serde(default)]
    pub category: String,
    /// memo/learn：记忆正文
    #[serde(default)]
    pub text: String,
    /// forget：要删除的记忆时间戳
    #[serde(default)]
    pub ts: i64,
    /// done：收尾总结
    #[serde(default)]
    pub summary: String,
    /// blocked：受阻原因
    #[serde(default)]
    pub reason: String,
    /// 来源会话 id：应用侧从 NEXT_ACTION 解析时补上，方便御书房回链和批复后更新原会话。
    #[serde(default)]
    pub origin_thread_id: String,
    /// 决策来源：employee 普通工作 | wake 开工预检。CLI 默认留空即 employee。
    #[serde(default)]
    pub decision_source: String,
    /// 提交时间（ms）
    #[serde(default)]
    pub at: i64,
}

/// 命令收件箱文件路径（与各存储同在应用数据目录）。
pub(crate) fn commands_path(data_dir: &std::path::Path) -> PathBuf {
    data_dir.join("employee_commands.jsonl")
}

/// 追加一条命令到收件箱（供 CLI 工具进程调用）。
pub(crate) fn append_command(data_dir: &str, cmd: &InboxCommand) -> Result<(), String> {
    use std::io::Write;
    let path = commands_path(std::path::Path::new(data_dir));
    if let Some(p) = path.parent() {
        let _ = fs::create_dir_all(p);
    }
    let line = serde_json::to_string(cmd).map_err(|e| e.to_string())?;
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| e.to_string())?;
    writeln!(f, "{line}").map_err(|e| e.to_string())?;
    Ok(())
}

// ===== 持久化 =====

#[derive(Serialize, Deserialize, Default)]
struct EmployeesFile {
    employees: Vec<Employee>,
}

pub struct EmployeeStore {
    path: PathBuf,
    pub employees: Vec<Employee>,
}

impl EmployeeStore {
    pub fn load(dir: &PathBuf) -> Self {
        let path = dir.join("employees.json");
        let employees = fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<EmployeesFile>(&s).ok())
            .map(|f| f.employees)
            .unwrap_or_default();
        EmployeeStore { path, employees }
    }

    pub fn save(&self) {
        if let Some(parent) = self.path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let file = EmployeesFile {
            employees: self.employees.clone(),
        };
        if let Ok(json) = serde_json::to_string_pretty(&file) {
            let _ = fs::write(&self.path, json);
        }
    }

    pub fn get(&self, id: &str) -> Option<&Employee> {
        self.employees.iter().find(|e| e.id == id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut Employee> {
        self.employees.iter_mut().find(|e| e.id == id)
    }
}

#[derive(Serialize, Deserialize, Default)]
struct TasksFile {
    tasks: Vec<Task>,
}

pub struct TaskStore {
    path: PathBuf,
    pub tasks: Vec<Task>,
}

impl TaskStore {
    pub fn load(dir: &PathBuf) -> Self {
        let path = dir.join("employee_tasks.json");
        let mut tasks = fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<TasksFile>(&s).ok())
            .map(|f| f.tasks)
            .unwrap_or_default();
        // 上次进程异常退出残留的 working 任务：回退为 queued 重新排队
        for t in tasks.iter_mut() {
            if t.status == "working" {
                t.status = "queued".into();
            }
        }
        TaskStore { path, tasks }
    }

    pub fn save(&self) {
        if let Some(parent) = self.path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let file = TasksFile {
            tasks: self.tasks.clone(),
        };
        if let Ok(json) = serde_json::to_string_pretty(&file) {
            let _ = fs::write(&self.path, json);
        }
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut Task> {
        self.tasks.iter_mut().find(|t| t.id == id)
    }
}

#[derive(Serialize, Deserialize, Default)]
struct WorkflowsFile {
    workflows: Vec<Workflow>,
}

pub struct WorkflowStore {
    path: PathBuf,
    pub workflows: Vec<Workflow>,
}

impl WorkflowStore {
    pub fn load(dir: &PathBuf) -> Self {
        let path = dir.join("employee_workflows.json");
        let workflows = fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<WorkflowsFile>(&s).ok())
            .map(|f| f.workflows)
            .unwrap_or_default();
        WorkflowStore { path, workflows }
    }

    pub fn save(&self) {
        if let Some(parent) = self.path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let file = WorkflowsFile {
            workflows: self.workflows.clone(),
        };
        if let Ok(json) = serde_json::to_string_pretty(&file) {
            let _ = fs::write(&self.path, json);
        }
    }

    pub fn get_mut_by_task(
        &mut self,
        employee_id: &str,
        scope: &str,
        key: &str,
    ) -> Option<&mut Workflow> {
        self.workflows.iter_mut().find(|w| {
            w.employee_id == employee_id && w.scope == scope && w.key == key && w.status != "closed"
        })
    }

    pub fn active_for_employee(&self, employee_id: &str, limit: usize) -> Vec<Workflow> {
        let mut v: Vec<Workflow> = self
            .workflows
            .iter()
            .filter(|w| w.employee_id == employee_id && w.status != "closed")
            .cloned()
            .collect();
        v.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        v.truncate(limit);
        v
    }

    pub fn close_by_thread_id(&mut self, thread_id: &str) -> bool {
        let mut changed = false;
        for w in &mut self.workflows {
            if w.thread_id.as_deref() == Some(thread_id) && w.status != "closed" {
                w.status = "closed".into();
                w.updated_at = now_ms();
                changed = true;
            }
        }
        if changed {
            self.save();
        }
        changed
    }

    pub fn close_by_worktree(&mut self, repo: &str, path: &str, branch: &str) -> bool {
        let mut changed = false;
        for w in &mut self.workflows {
            if w.status != "closed" && w.repo == repo && w.cwd == path && w.branch == branch {
                w.status = "closed".into();
                w.updated_at = now_ms();
                changed = true;
            }
        }
        if changed {
            self.save();
        }
        changed
    }

    pub fn detach_threads(&mut self, thread_ids: &[String]) -> bool {
        let mut changed = false;
        for w in &mut self.workflows {
            if w.thread_id
                .as_ref()
                .is_some_and(|id| thread_ids.contains(id))
            {
                w.thread_id = None;
                w.updated_at = now_ms();
                changed = true;
            }
        }
        if changed {
            self.save();
        }
        changed
    }
}

#[derive(Serialize, Deserialize, Default)]
struct MemoryFile {
    journals: HashMap<String, Vec<JournalEntry>>,
}

pub struct MemoryStore {
    path: PathBuf,
    pub journals: HashMap<String, Vec<JournalEntry>>,
}

impl MemoryStore {
    pub fn load(dir: &PathBuf) -> Self {
        let path = dir.join("employee_memory.json");
        let journals = fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<MemoryFile>(&s).ok())
            .map(|f| f.journals)
            .unwrap_or_default();
        let mut store = MemoryStore { path, journals };
        if store.prune_duplicate_managed_knowledge() {
            store.save();
        }
        store
    }

    pub fn save(&self) {
        if let Some(parent) = self.path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let file = MemoryFile {
            journals: self.journals.clone(),
        };
        if let Ok(json) = serde_json::to_string_pretty(&file) {
            let _ = fs::write(&self.path, json);
        }
    }

    pub fn append(&mut self, employee_id: &str, entry: JournalEntry) {
        let list = self.journals.entry(employee_id.to_string()).or_default();
        list.push(entry);
        let now = now_ms();
        list.retain(|e| e.expires_at <= 0 || e.expires_at > now || e.protected);
        // 普通经历有界保留。
        let auto = list.iter().filter(|e| !e.pinned).count();
        if auto > JOURNAL_KEEP {
            let mut drop_n = auto - JOURNAL_KEEP;
            list.retain(|e| {
                if drop_n > 0 && !e.pinned && !e.protected {
                    drop_n -= 1;
                    false
                } else {
                    true
                }
            });
        }
        // Agent/Mind 学到的长期知识同样有界，人工保护知识不自动删除。
        let managed_pinned = list.iter().filter(|e| e.pinned && !e.protected).count();
        if managed_pinned > MANAGED_KNOWLEDGE_KEEP {
            let mut drop_n = managed_pinned - MANAGED_KNOWLEDGE_KEEP;
            list.retain(|e| {
                if drop_n > 0 && e.pinned && !e.protected && e.superseded_by.is_some() {
                    drop_n -= 1;
                    false
                } else {
                    true
                }
            });
            if drop_n > 0 {
                list.retain(|e| {
                    if drop_n > 0 && e.pinned && !e.protected {
                        drop_n -= 1;
                        false
                    } else {
                        true
                    }
                });
            }
        }
        self.save();
    }

    pub fn append_unique_managed(&mut self, employee_id: &str, entry: JournalEntry) -> bool {
        let new_key = normalized_memory_key(&entry.summary);
        let duplicate = self.journals.get(employee_id).is_some_and(|entries| {
            entries.iter().any(|existing| {
                let existing_key = normalized_memory_key(&existing.summary);
                memory_dedup_class(existing) == memory_dedup_class(&entry)
                    && (existing_key == new_key
                        || (new_key.len() >= 16
                            && (existing_key.contains(&new_key)
                                || new_key.contains(&existing_key)))
                        || memory_keys_similar(&existing_key, &new_key))
                    && existing.superseded_by.is_none()
            })
        });
        if duplicate {
            return false;
        }
        self.append(employee_id, entry);
        true
    }

    fn prune_duplicate_managed_knowledge(&mut self) -> bool {
        let mut changed = false;
        for list in self.journals.values_mut() {
            let protected_keys: Vec<String> = list
                .iter()
                .filter(|entry| {
                    entry.pinned
                        && entry.superseded_by.is_none()
                        && (entry.protected
                            || entry.source == "user"
                            || entry.kind.ends_with(":user"))
                        && memory_dedup_class(entry) == "knowledge"
                })
                .map(|entry| normalized_memory_key(&entry.summary))
                .collect();
            if protected_keys.is_empty() {
                continue;
            }
            let before = list.len();
            list.retain(|entry| {
                if !(entry.pinned
                    && !entry.protected
                    && entry.source.starts_with("mind:")
                    && entry.kind == "knowledge"
                    && entry.superseded_by.is_none())
                {
                    return true;
                }
                let key = normalized_memory_key(&entry.summary);
                !protected_keys.iter().any(|protected_key| {
                    key == *protected_key
                        || (key.len() >= 16
                            && (protected_key.contains(&key) || key.contains(protected_key)))
                        || memory_keys_similar(protected_key, &key)
                })
            });
            changed |= list.len() != before;
        }
        changed
    }

    pub fn all(&self, employee_id: &str) -> Vec<JournalEntry> {
        self.journals.get(employee_id).cloned().unwrap_or_default()
    }

    pub fn mark_used(&mut self, employee_id: &str, ts_list: &[i64]) {
        if ts_list.is_empty() {
            return;
        }
        let Some(list) = self.journals.get_mut(employee_id) else {
            return;
        };
        let now = now_ms();
        let ts_set: HashSet<i64> = ts_list.iter().copied().collect();
        let mut changed = false;
        for entry in list.iter_mut().filter(|entry| ts_set.contains(&entry.ts)) {
            entry.last_used_at = now;
            entry.hit_count = entry.hit_count.saturating_add(1);
            changed = true;
        }
        if changed {
            self.save();
        }
    }

    pub fn apply_usage_evidence(
        &mut self,
        employee_id: &str,
        ts_list: &[i64],
        task_id: &str,
        positive: bool,
    ) {
        if ts_list.is_empty() || task_id.trim().is_empty() {
            return;
        }
        let Some(list) = self.journals.get_mut(employee_id) else {
            return;
        };
        let ts_set: HashSet<i64> = ts_list.iter().copied().collect();
        let evidence_key = format!(
            "usage:{}:{}",
            if positive { "positive" } else { "negative" },
            task_id.trim()
        );
        let mut changed = false;
        for entry in list.iter_mut().filter(|entry| ts_set.contains(&entry.ts)) {
            if entry.source == "user" || entry.kind.ends_with(":user") {
                continue;
            }
            let reusable = entry.kind == "lesson"
                || entry.kind.starts_with("knowledge")
                || entry.kind.starts_with("memory")
                || (entry.pinned && !entry.kind.starts_with("outcome:"));
            if !reusable || entry.evidence_tasks.iter().any(|key| key == &evidence_key) {
                continue;
            }
            entry.evidence_tasks.push(evidence_key.clone());
            if positive {
                entry.positive_evidence = entry.positive_evidence.saturating_add(1);
                entry.confidence = (entry.confidence + 0.08).min(1.0);
            } else {
                entry.negative_evidence = entry.negative_evidence.saturating_add(1);
                entry.confidence = (entry.confidence - 0.10).max(0.0);
            }
            changed = true;
        }
        if changed {
            self.save();
        }
    }

    pub fn update_entry(&mut self, employee_id: &str, ts: i64, summary: &str) {
        if let Some(list) = self.journals.get_mut(employee_id) {
            if let Some(e) = list.iter_mut().find(|e| e.ts == ts) {
                e.summary = summary.to_string();
            }
        }
        self.save();
    }

    pub fn delete_entry(&mut self, employee_id: &str, ts: i64) {
        if let Some(list) = self.journals.get_mut(employee_id) {
            list.retain(|e| e.ts != ts);
        }
        self.save();
    }

    pub fn forget_managed(&mut self, employee_id: &str, ts: i64) -> bool {
        let Some(list) = self.journals.get_mut(employee_id) else {
            return false;
        };
        let Some(index) = list.iter().position(|e| e.ts == ts) else {
            return false;
        };
        if list[index].protected
            || list[index].source == "user"
            || list[index].kind.ends_with(":user")
        {
            return false;
        }
        if list[index].kind == "lesson" {
            let entry = &mut list[index];
            entry.kind = "lesson:retired".to_string();
            entry.pinned = false;
            entry.negative_evidence = entry.negative_evidence.saturating_add(1);
            entry.confidence = (entry.confidence - 0.35).max(0.0);
            entry.expires_at = now_ms() + 30 * 24 * 60 * 60 * 1000;
        } else {
            list.remove(index);
        }
        self.save();
        true
    }

    pub fn challenge_lesson(
        &mut self,
        employee_id: &str,
        ts: i64,
        evidence_key: &str,
        reason: &str,
    ) -> bool {
        let Some(list) = self.journals.get_mut(employee_id) else {
            return false;
        };
        let Some(entry) = list
            .iter_mut()
            .find(|e| e.ts == ts && e.kind == "lesson" && !e.protected)
        else {
            return false;
        };
        let evidence_key = evidence_key.trim();
        if !evidence_key.is_empty() && !entry.evidence_tasks.iter().any(|x| x == evidence_key) {
            entry.evidence_tasks.push(evidence_key.to_string());
        }
        entry.kind = "lesson:challenged".to_string();
        entry.pinned = false;
        entry.negative_evidence = entry.negative_evidence.saturating_add(1);
        entry.confidence = (entry.confidence - 0.25).max(0.0);
        let reason = reason.trim();
        if !reason.is_empty() && !entry.summary.contains(reason) {
            entry.summary = format!("{}\n\n【受挑战】{}", entry.summary.trim(), reason);
        }
        self.save();
        true
    }

    pub fn set_pinned(&mut self, employee_id: &str, ts: i64, pinned: bool) {
        if let Some(list) = self.journals.get_mut(employee_id) {
            if let Some(e) = list.iter_mut().find(|e| e.ts == ts) {
                e.pinned = pinned;
                e.protected = pinned;
                e.source = "user".to_string();
            }
        }
        self.save();
    }

    pub fn set_feedback(&mut self, employee_id: &str, ts: i64, feedback: i8) -> bool {
        if !(-1..=1).contains(&feedback) {
            return false;
        }
        let Some(entry) = self
            .journals
            .get_mut(employee_id)
            .and_then(|list| list.iter_mut().find(|entry| entry.ts == ts))
        else {
            return false;
        };
        if entry.source == "user" || entry.kind.ends_with(":user") {
            return false;
        }
        if entry.user_feedback == feedback {
            return true;
        }
        match entry.user_feedback {
            1 => {
                entry.positive_evidence = entry.positive_evidence.saturating_sub(1);
                entry.confidence = (entry.confidence - 0.15).max(0.0);
            }
            -1 => {
                entry.negative_evidence = entry.negative_evidence.saturating_sub(1);
                entry.confidence = (entry.confidence + 0.2).min(1.0);
            }
            _ => {}
        }
        match feedback {
            1 => {
                entry.positive_evidence = entry.positive_evidence.saturating_add(1);
                entry.confidence = (entry.confidence + 0.15).min(1.0);
            }
            -1 => {
                entry.negative_evidence = entry.negative_evidence.saturating_add(1);
                entry.confidence = (entry.confidence - 0.2).max(0.0);
            }
            _ => {}
        }
        entry.user_feedback = feedback;
        self.save();
        true
    }

    /// 守则被实践证实一次：验证数 +1；累计达到阈值自动「内化」（置顶长期注入）。
    /// 返回 (最新验证数, 本次是否发生内化)；ts 不是守则条目时返回 None。
    pub fn verify_lesson(
        &mut self,
        employee_id: &str,
        ts: i64,
        evidence_key: &str,
    ) -> Option<(u32, bool)> {
        let list = self.journals.get_mut(employee_id)?;
        let e = list.iter_mut().find(|e| e.ts == ts && e.kind == "lesson")?;
        let evidence_key = evidence_key.trim();
        if !evidence_key.is_empty() && e.evidence_tasks.iter().any(|x| x == evidence_key) {
            return Some((e.evidence, false));
        }
        if !evidence_key.is_empty() {
            e.evidence_tasks.push(evidence_key.to_string());
        }
        e.evidence = e.evidence.saturating_add(1);
        e.positive_evidence = e.positive_evidence.saturating_add(1);
        e.confidence = (e.confidence + 0.2).min(1.0);
        let promoted = !e.pinned
            && (e.evidence >= LESSON_PROMOTE_AT
                || (e.positive_evidence >= LESSON_PROMOTE_AT && e.negative_evidence == 0));
        if promoted {
            e.pinned = true;
            e.task_title = "守则".to_string();
        }
        let n = e.evidence;
        self.save();
        Some((n, promoted))
    }

    pub fn mark_superseded(&mut self, employee_id: &str, ts_list: &[i64], superseded_by: i64) {
        if ts_list.is_empty() {
            return;
        }
        if let Some(list) = self.journals.get_mut(employee_id) {
            for e in list.iter_mut() {
                if ts_list.contains(&e.ts) && !e.protected {
                    e.superseded_by = Some(superseded_by);
                    e.pinned = false;
                    e.confidence = (e.confidence - 0.2).max(0.0);
                }
            }
        }
        self.save();
    }

    pub fn downgrade_memory(
        &mut self,
        employee_id: &str,
        ts: i64,
        reason: &str,
        expires_at: i64,
    ) -> bool {
        let Some(list) = self.journals.get_mut(employee_id) else {
            return false;
        };
        let Some(entry) = list.iter_mut().find(|e| {
            e.ts == ts && !e.protected && e.source != "user" && !e.kind.ends_with(":user")
        }) else {
            return false;
        };
        entry.pinned = false;
        entry.negative_evidence = entry.negative_evidence.saturating_add(1);
        entry.confidence = (entry.confidence - 0.25).max(0.0);
        if entry.negative_evidence >= DREAM_FORGET_AFTER_DOWNGRADES {
            let index = list
                .iter()
                .position(|e| {
                    e.ts == ts && !e.protected && e.source != "user" && !e.kind.ends_with(":user")
                })
                .expect("Dream 降级项刚刚已找到");
            if list[index].kind == "lesson" {
                let entry = &mut list[index];
                entry.kind = "lesson:retired".to_string();
                entry.pinned = false;
                entry.expires_at = now_ms() + 30 * 24 * 60 * 60 * 1000;
            } else {
                list.remove(index);
            }
            self.save();
            return true;
        }
        if expires_at > 0 {
            entry.expires_at = expires_at;
        } else if entry.expires_at <= 0 {
            entry.expires_at = now_ms() + 90 * 24 * 60 * 60 * 1000;
        }
        let reason = reason.trim();
        if !reason.is_empty() && !entry.summary.contains(reason) {
            entry.summary = format!("{}\n\n【Dream 降级】{}", entry.summary.trim(), reason);
        }
        self.save();
        true
    }
}

fn normalized_memory_key(text: &str) -> String {
    text.chars()
        .filter(|c| !is_memory_key_separator(*c))
        .flat_map(|c| c.to_lowercase())
        .take(240)
        .collect()
}

fn is_memory_key_separator(c: char) -> bool {
    c.is_whitespace()
        || c.is_ascii_punctuation()
        || matches!(
            c,
            '，' | '。'
                | '；'
                | '、'
                | '：'
                | '！'
                | '？'
                | '（'
                | '）'
                | '【'
                | '】'
                | '「'
                | '」'
                | '“'
                | '”'
                | '‘'
                | '’'
                | '《'
                | '》'
                | '…'
                | '—'
                | '－'
                | '～'
                | '·'
        )
}

fn memory_dedup_class(entry: &JournalEntry) -> &'static str {
    if entry.kind == "lesson" || entry.kind.starts_with("lesson:") {
        "lesson"
    } else if entry.kind.starts_with("knowledge")
        || (entry.pinned
            && !entry.kind.starts_with("outcome:")
            && (entry.protected || entry.source == "user" || entry.kind.is_empty()))
    {
        "knowledge"
    } else if entry.kind.starts_with("memory") || entry.kind.is_empty() {
        "memory"
    } else {
        "other"
    }
}

fn memory_keys_similar(a: &str, b: &str) -> bool {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let min_len = a_chars.len().min(b_chars.len());
    if min_len < 48 {
        return false;
    }
    let a_grams = char_bigrams(&a_chars);
    let b_grams = char_bigrams(&b_chars);
    if a_grams.is_empty() || b_grams.is_empty() {
        return false;
    }
    let overlap = a_grams.intersection(&b_grams).count();
    let smaller = a_grams.len().min(b_grams.len());
    overlap * 100 >= smaller * 88
}

fn char_bigrams(chars: &[char]) -> HashSet<String> {
    chars
        .windows(2)
        .map(|w| w.iter().collect::<String>())
        .collect()
}

#[derive(Serialize, Deserialize, Default)]
struct DecisionsFile {
    decisions: Vec<Decision>,
}

/// 奏折的持久化存储（御书房的数据源）。
pub struct DecisionStore {
    path: PathBuf,
    pub decisions: Vec<Decision>,
}

impl DecisionStore {
    pub fn load(dir: &PathBuf) -> Self {
        let path = dir.join("employee_decisions.json");
        let decisions = fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<DecisionsFile>(&s).ok())
            .map(|f| f.decisions)
            .unwrap_or_default();
        DecisionStore { path, decisions }
    }

    pub fn save(&self) {
        if let Some(parent) = self.path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let file = DecisionsFile {
            decisions: self.decisions.clone(),
        };
        if let Ok(json) = serde_json::to_string_pretty(&file) {
            let _ = fs::write(&self.path, json);
        }
    }

    pub fn list(&self) -> Vec<Decision> {
        let mut list = self.decisions.clone();
        // 候旨（pending）在前，其余按时间倒序
        list.sort_by(|a, b| {
            let pa = if a.status == "pending" { 0 } else { 1 };
            let pb = if b.status == "pending" { 0 } else { 1 };
            pa.cmp(&pb).then(b.created_at.cmp(&a.created_at))
        });
        list
    }

    /// 已归档（已领旨 consumed / 留中 shelved / 驳回 rejected / 已撤回 withdrawn / 已阅 read / 已批阅 reviewed）的奏折只保留最近若干条；
    /// 候旨（pending）、已准奏待领旨（resolved）与未读汇报（report）的永不清理。
    fn prune(&mut self) {
        const KEEP_ARCHIVED: usize = 120;
        let mut archived: Vec<(i64, String)> = self
            .decisions
            .iter()
            .filter(|d| d.status != "pending" && d.status != "resolved" && d.status != "report")
            .map(|d| (d.resolved_at.max(d.created_at), d.id.clone()))
            .collect();
        if archived.len() <= KEEP_ARCHIVED {
            return;
        }
        archived.sort_by(|a, b| b.0.cmp(&a.0));
        let drop: HashSet<String> = archived[KEEP_ARCHIVED..]
            .iter()
            .map(|(_, id)| id.clone())
            .collect();
        self.decisions.retain(|d| !drop.contains(&d.id));
    }

    pub fn add(&mut self, d: Decision) {
        self.decisions.push(d);
        self.prune();
        self.save();
    }

    pub fn add_or_merge_pending(&mut self, d: Decision) -> bool {
        if !d.blocker_signature.trim().is_empty() {
            if let Some(existing) = self.decisions.iter_mut().find(|x| {
                x.status == "pending"
                    && x.employee_id == d.employee_id
                    && x.scope == d.scope
                    && x.mark_key == d.mark_key
                    && x.blocker_signature == d.blocker_signature
            }) {
                existing.brief = if existing.brief.trim().is_empty() {
                    d.brief
                } else if d.brief.trim().is_empty() || existing.brief.contains(d.brief.trim()) {
                    existing.brief.clone()
                } else {
                    format!("{}\n\n【新证据】{}", existing.brief.trim(), d.brief.trim())
                };
                if !d.proposed_action.trim().is_empty() {
                    existing.proposed_action = d.proposed_action;
                }
                if !d.auto_note.trim().is_empty() {
                    existing.auto_note = d.auto_note;
                }
                existing.created_at = now_ms();
                self.save();
                return false;
            }
        }
        self.add(d);
        true
    }

    /// 某员工在某单子上是否有「候旨中」的奏折（有则该单子保持在手，等主管批复）。
    pub fn pending_for(&self, employee_id: &str, scope: &str, key: &str) -> bool {
        self.decisions.iter().any(|d| {
            d.status == "pending"
                && d.employee_id == employee_id
                && d.scope == scope
                && d.mark_key == key
        })
    }

    /// 某单子上是否有任何人的候旨奏折（不限员工）。用于确定性领活时避开
    /// 「已上奏御书房、等主管批示」的单子——批示未下，别的员工不要抢着重做。
    pub fn pending_on(&self, scope: &str, key: &str) -> bool {
        self.decisions
            .iter()
            .any(|d| d.status == "pending" && d.scope == scope && d.mark_key == key)
    }

    /// 某员工全部「已批阅、待领旨」的奏折（准奏 resolved / 留中 shelved / 驳回 rejected），按批复先后排序：先批的先办。
    pub fn actionable_for_employee(&self, employee_id: &str) -> Vec<Decision> {
        let mut v: Vec<Decision> = self
            .decisions
            .iter()
            .filter(|d| {
                (d.status == "resolved" || d.status == "shelved" || d.status == "rejected")
                    && d.employee_id == employee_id
            })
            .cloned()
            .collect();
        v.sort_by_key(|d| d.resolved_at);
        v
    }

    /// 领旨：把该单子上全部已批阅（准奏/留中/驳回）的奏折转为 consumed（留档御书房「已批阅」，
    /// 不物理删除，让主管能看到员工确实领旨了）。返回领旨前的快照（保留原状态），按批复先后。
    pub fn take_actionable(&mut self, employee_id: &str, scope: &str, key: &str) -> Vec<Decision> {
        let mut out: Vec<Decision> = Vec::new();
        for d in self.decisions.iter_mut() {
            if (d.status == "resolved" || d.status == "shelved" || d.status == "rejected")
                && d.employee_id == employee_id
                && d.scope == scope
                && d.mark_key == key
            {
                out.push(d.clone());
                d.status = "consumed".into();
            }
        }
        if !out.is_empty() {
            out.sort_by_key(|d| d.resolved_at);
            self.prune();
            self.save();
        }
        out
    }

    /// 单子完结时撤回其上全部候旨奏折（问题已随单子失效），避免御书房堆积废折。返回撤回条数。
    pub fn withdraw_for(&mut self, scope: &str, key: &str) -> usize {
        let now = now_ms();
        let mut n = 0;
        for d in self.decisions.iter_mut() {
            if d.status == "pending" && d.scope == scope && d.mark_key == key {
                d.status = "withdrawn".into();
                d.resolved_at = now;
                n += 1;
            }
        }
        if n > 0 {
            self.prune();
            self.save();
        }
        n
    }

    /// 朱批准奏：仅候旨中（pending）的奏折可批，避免重复批复污染状态。
    pub fn resolve(&mut self, id: &str, answer: &str) -> bool {
        if let Some(d) = self
            .decisions
            .iter_mut()
            .find(|d| d.id == id && d.status == "pending")
        {
            d.status = "resolved".into();
            d.answer = Some(answer.to_string());
            d.resolved_at = now_ms();
            self.save();
            true
        } else {
            false
        }
    }

    /// 汇报已读：主管确认收到员工的完工汇报后归档（不唤醒员工，无需批示）。
    pub fn mark_read(&mut self, id: &str) -> bool {
        if let Some(d) = self
            .decisions
            .iter_mut()
            .find(|d| d.id == id && d.status == "report")
        {
            d.status = "read".into();
            d.resolved_at = now_ms();
            self.prune();
            self.save();
            true
        } else {
            false
        }
    }

    /// 汇报批阅：保留主管批示并归档，不进入员工领旨队列。
    pub fn review_report(&mut self, id: &str, answer: &str) -> bool {
        if let Some(d) = self
            .decisions
            .iter_mut()
            .find(|d| d.id == id && d.status == "report")
        {
            d.status = "reviewed".into();
            d.answer = Some(answer.to_string());
            d.resolved_at = now_ms();
            self.prune();
            self.save();
            true
        } else {
            false
        }
    }

    /// 留中不发：不作批示、归档留存。员工下一轮不再等待，自行斟酌推进该单子。
    pub fn shelve(&mut self, id: &str) -> bool {
        if let Some(d) = self
            .decisions
            .iter_mut()
            .find(|d| d.id == id && d.status == "pending")
        {
            d.status = "shelved".into();
            d.resolved_at = now_ms();
            self.prune();
            self.save();
            true
        } else {
            false
        }
    }

    /// 驳回：主管明确不批准该请求。员工领旨后停止这张单，避免继续围着同一问题上奏。
    pub fn reject(&mut self, id: &str, answer: &str) -> bool {
        if let Some(d) = self
            .decisions
            .iter_mut()
            .find(|d| d.id == id && d.status == "pending")
        {
            d.status = "rejected".into();
            d.answer = Some(answer.to_string());
            d.resolved_at = now_ms();
            self.save();
            true
        } else {
            false
        }
    }

    pub fn remove(&mut self, id: &str) {
        self.decisions.retain(|d| d.id != id);
        self.save();
    }
}

// ===== 命令逻辑 =====

pub fn list_employees(app: &AppHandle) -> Vec<Employee> {
    let state = app.state::<AppState>();
    let mut list = state.employees.lock().unwrap().employees.clone();
    list.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    list
}

#[allow(clippy::too_many_arguments)]
pub fn create_employee(
    app: &AppHandle,
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
    work_hours: Option<WorkHours>,
    enabled: Option<bool>,
    self_directed: Option<bool>,
    allow_worktree: Option<bool>,
    directive: Option<String>,
    mark_scope: Option<String>,
    shared_ledger: Option<bool>,
    partners: Option<Vec<Partner>>,
) -> Result<Employee, String> {
    let name = name.trim().to_string();
    let cwd = cwd.trim().to_string();
    if name.is_empty() {
        return Err("请填写员工名字".into());
    }
    if cwd.is_empty() {
        return Err("请填写工作目录".into());
    }
    let now = now_ms();
    let emp = Employee {
        id: uuid::Uuid::new_v4().to_string(),
        name,
        agent_kind: agent_kind.unwrap_or(AgentKind::Devin),
        model: model.filter(|s| !s.is_empty()),
        heartbeat_agent_kind,
        heartbeat_model: heartbeat_model.filter(|s| !s.is_empty()),
        mind_agent_kind,
        mind_model: mind_model.filter(|s| !s.is_empty()),
        mode: mode.filter(|s| !s.is_empty()),
        charter: charter.trim().to_string(),
        cwd,
        heartbeat_enabled: heartbeat_enabled.unwrap_or(true),
        heartbeat_secs: heartbeat_secs.unwrap_or_else(default_heartbeat).max(10),
        work_hours: work_hours.filter(|w| !w.start.trim().is_empty() && !w.end.trim().is_empty()),
        enabled: enabled.unwrap_or(true),
        self_directed: self_directed.unwrap_or(false),
        allow_worktree: allow_worktree.unwrap_or(false),
        directive: directive.unwrap_or_default().trim().to_string(),
        mark_scope: mark_scope.unwrap_or_default().trim().to_string(),
        shared_ledger: shared_ledger.unwrap_or(false),
        partners: partners.unwrap_or_default(),
        last_heartbeat_ms: 0,
        mind_event_seeded_at: 0,
        created_at: now,
        updated_at: now,
    };
    {
        let state = app.state::<AppState>();
        let mut store = state.employees.lock().unwrap();
        store.employees.push(emp.clone());
        store.save();
    }
    let _ = app.emit(EV_EMPLOYEES, json!({}));
    Ok(emp)
}

pub fn update_employee(app: &AppHandle, mut emp: Employee) -> Result<(), String> {
    let employee_id = emp.id.clone();
    emp.name = emp.name.trim().to_string();
    emp.cwd = emp.cwd.trim().to_string();
    emp.mind_model = emp.mind_model.filter(|s| !s.trim().is_empty());
    emp.heartbeat_model = emp.heartbeat_model.filter(|s| !s.trim().is_empty());
    emp.model = emp.model.filter(|s| !s.trim().is_empty());
    if emp.name.is_empty() {
        return Err("请填写员工名字".into());
    }
    if emp.cwd.is_empty() {
        return Err("请填写工作目录".into());
    }
    emp.heartbeat_secs = emp.heartbeat_secs.max(10);
    emp.directive = emp.directive.trim().to_string();
    emp.mark_scope = emp.mark_scope.trim().to_string();
    // 上班时间：start/end 任一为空视为「未设置」，回落 7×24。
    emp.work_hours = emp
        .work_hours
        .filter(|w| !w.start.trim().is_empty() && !w.end.trim().is_empty());
    {
        let state = app.state::<AppState>();
        let mut store = state.employees.lock().unwrap();
        let slot = store.get_mut(&emp.id).ok_or("员工不存在")?;
        emp.last_heartbeat_ms = slot.last_heartbeat_ms;
        emp.created_at = slot.created_at;
        emp.updated_at = now_ms();
        *slot = emp;
        store.save();
    }
    crate::mind::invalidate_brief(app, &employee_id, "员工配置已更新，Dream 将重新整理资料库");
    let _ = app.emit(EV_EMPLOYEES, json!({}));
    Ok(())
}

pub fn delete_employee(app: &AppHandle, id: &str) {
    {
        let state = app.state::<AppState>();
        {
            let mut store = state.employees.lock().unwrap();
            store.employees.retain(|e| e.id != id);
            store.save();
        }
        {
            let mut tasks = state.tasks.lock().unwrap();
            tasks.tasks.retain(|t| t.employee_id != id);
            tasks.save();
        }
        {
            let mut mem = state.memory.lock().unwrap();
            mem.journals.remove(id);
            mem.save();
        }
        {
            let mut dec = state.decisions.lock().unwrap();
            dec.decisions.retain(|d| d.employee_id != id);
            dec.save();
        }
        {
            state.notices.lock().unwrap().retain_employee(id);
        }
    }
    crate::mind::remove_employee(app, id);
    let _ = app.emit(EV_EMPLOYEES, json!({}));
    let _ = app.emit(EV_TASKS, json!({}));
    let _ = app.emit(EV_DECISIONS, json!({}));
}

pub fn set_employee_enabled(app: &AppHandle, id: &str, enabled: bool) {
    {
        let state = app.state::<AppState>();
        let mut store = state.employees.lock().unwrap();
        if let Some(e) = store.get_mut(id) {
            e.enabled = enabled;
            e.updated_at = now_ms();
        }
        store.save();
    }
    let _ = app.emit(EV_EMPLOYEES, json!({}));
}

pub fn list_tasks(app: &AppHandle) -> Vec<Task> {
    let state = app.state::<AppState>();
    let mut list = state.tasks.lock().unwrap().tasks.clone();
    list.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    list
}

pub fn assign_task(
    app: &AppHandle,
    employee_id: String,
    title: String,
    brief: String,
) -> Result<Task, String> {
    let title = title.trim().to_string();
    if title.is_empty() {
        return Err("请填写任务标题".into());
    }
    {
        let state = app.state::<AppState>();
        if state.employees.lock().unwrap().get(&employee_id).is_none() {
            return Err("员工不存在".into());
        }
    }
    let task = create_task(
        app,
        &employee_id,
        &title,
        brief.trim(),
        "user",
        None,
        "queued",
    )
    .ok_or("创建任务失败")?;
    crate::mind::preempt_for_work(app, &employee_id);
    Ok(task)
}

fn thread_running(app: &AppHandle, thread_id: &str) -> bool {
    let state = app.state::<AppState>();
    let kind = {
        let store = state.store.lock().unwrap();
        store.get(thread_id).map(|t| t.agent_kind.clone())
    };
    match kind.and_then(|kind| state.acp_for(&kind)) {
        Some(mgr) => mgr.is_running(thread_id),
        None => state.codex.is_running(thread_id),
    }
}

pub fn delete_task(app: &AppHandle, id: &str) -> Result<(), String> {
    let thread_id = {
        let state = app.state::<AppState>();
        let mut store = state.tasks.lock().unwrap();
        let thread_id = store
            .tasks
            .iter()
            .find(|t| t.id == id)
            .and_then(|t| t.thread_id.clone());
        if thread_id
            .as_deref()
            .is_some_and(|thread_id| thread_running(app, thread_id))
        {
            return Err("对应会话正在运行，请先停止".into());
        }
        store.tasks.retain(|t| t.id != id);
        store.save();
        thread_id
    };
    let _ = app.emit(EV_TASKS, json!({}));
    if let Some(thread_id) = thread_id {
        discard_thread(app, &thread_id);
    }
    Ok(())
}

pub fn delete_tasks_for_threads(app: &AppHandle, thread_ids: &[String]) -> usize {
    if thread_ids.is_empty() {
        return 0;
    }
    let removed = {
        let state = app.state::<AppState>();
        let mut store = state.tasks.lock().unwrap();
        let before = store.tasks.len();
        store.tasks.retain(|t| {
            !t.thread_id
                .as_ref()
                .is_some_and(|thread_id| thread_ids.contains(thread_id))
        });
        let removed = before - store.tasks.len();
        if removed > 0 {
            store.save();
        }
        removed
    };
    if removed > 0 {
        let _ = app.emit(EV_TASKS, json!({}));
    }
    removed
}

pub fn employee_memory(app: &AppHandle, id: &str) -> Vec<JournalEntry> {
    let state = app.state::<AppState>();
    let mem = state.memory.lock().unwrap();
    mem.journals.get(id).cloned().unwrap_or_default()
}

/// 用户手动新增一条记忆/知识（pinned=true 即长期知识，不会被自动截断）。
pub fn add_memory(
    app: &AppHandle,
    id: &str,
    title: String,
    summary: String,
    pinned: bool,
) -> Result<JournalEntry, String> {
    let summary = summary.trim().to_string();
    if summary.is_empty() {
        return Err("内容不能为空".into());
    }
    {
        let state = app.state::<AppState>();
        if state.employees.lock().unwrap().get(id).is_none() {
            return Err("员工不存在".into());
        }
    }
    let entry = JournalEntry {
        ts: now_ms(),
        task_id: String::new(),
        task_title: {
            let t = title.trim();
            if t.is_empty() {
                "知识".to_string()
            } else {
                t.to_string()
            }
        },
        summary,
        pinned,
        kind: if pinned {
            "knowledge:user".to_string()
        } else {
            "memory:user".to_string()
        },
        evidence: 0,
        source: "user".to_string(),
        protected: pinned,
        confidence: 1.0,
        last_used_at: 0,
        hit_count: 0,
        expires_at: 0,
        superseded_by: None,
        positive_evidence: 0,
        negative_evidence: 0,
        user_feedback: 0,
        evidence_tasks: Vec::new(),
    };
    let state = app.state::<AppState>();
    state.memory.lock().unwrap().append(id, entry.clone());
    crate::mind::invalidate_brief(app, id, "新增人工知识，Dream 将在下次空闲时整理资料库");
    crate::mind::check_memory_pressure(app, id);
    Ok(entry)
}

pub fn update_memory_entry(app: &AppHandle, id: &str, ts: i64, summary: String) {
    let state = app.state::<AppState>();
    state
        .memory
        .lock()
        .unwrap()
        .update_entry(id, ts, summary.trim());
    crate::mind::invalidate_brief(app, id, "记忆内容已修改，Dream 将在下次空闲时整理资料库");
}

pub fn delete_memory_entry(app: &AppHandle, id: &str, ts: i64) {
    let state = app.state::<AppState>();
    state.memory.lock().unwrap().delete_entry(id, ts);
    crate::mind::invalidate_brief(app, id, "记忆已删除，Dream 将在下次空闲时整理资料库");
}

pub fn set_memory_pinned(app: &AppHandle, id: &str, ts: i64, pinned: bool) {
    let state = app.state::<AppState>();
    state.memory.lock().unwrap().set_pinned(id, ts, pinned);
    crate::mind::invalidate_brief(
        app,
        id,
        "长期知识状态已变化，Dream 将在下次空闲时整理资料库",
    );
    crate::mind::check_memory_pressure(app, id);
}

pub fn set_memory_feedback(app: &AppHandle, id: &str, ts: i64, feedback: i8) -> Result<(), String> {
    let changed = {
        let state = app.state::<AppState>();
        let changed = state.memory.lock().unwrap().set_feedback(id, ts, feedback);
        changed
    };
    if !changed {
        return Err("记忆不存在、属于用户手动知识，或反馈值无效".into());
    }
    crate::mind::invalidate_brief(app, id, "用户评价了记忆，Dream 将结合反馈重新判断");
    Ok(())
}

/// 立即唤起某员工干一轮（忽略心跳节奏）。像叫醒一个人：他自己去找活、或续做手上的单子。
pub fn run_now(app: &AppHandle, id: &str) -> Result<(), String> {
    let state = app.state::<AppState>();
    let emp = state
        .employees
        .lock()
        .unwrap()
        .get(id)
        .cloned()
        .ok_or("员工不存在")?;
    if crate::mind::snapshot(app, id).active_thread_id.is_some() {
        crate::mind::preempt_for_work(app, id);
        let mut employees = state.employees.lock().unwrap();
        if let Some(e) = employees.get_mut(id) {
            e.last_heartbeat_ms = 0;
        }
        employees.save();
        return Ok(());
    }
    if employee_has_running_thread(app, id) {
        return Err("该员工有会话正在运行，请稍候（可在「数字员工」会话列表里查看或停止）".into());
    }
    let busy = {
        let tasks = state.tasks.lock().unwrap();
        tasks
            .tasks
            .iter()
            .any(|t| t.employee_id == id && t.status == "working")
    };
    if busy {
        return Err("该员工正在工作，请稍候".into());
    }
    run_cycle(app.clone(), emp, true);
    Ok(())
}

/// 心跳：遍历在岗、到点且空闲的员工，各自跑一轮自主工作循环。
pub fn heartbeat_tick(app: &AppHandle) {
    let state = app.state::<AppState>();
    let now = now_ms();
    let mut to_run: Vec<Employee> = Vec::new();
    {
        let employees = state.employees.lock().unwrap().employees.clone();
        // 有待消费批复注入的员工：批示到了就得起来办，不受上班时段限制。
        let with_imperial: HashSet<String> = {
            let n = state.notices.lock().unwrap();
            n.injections
                .keys()
                .filter_map(|k| k.split('\u{1}').next().map(|s| s.to_string()))
                .collect()
        };
        let tasks = state.tasks.lock().unwrap();
        for emp in employees.iter().filter(|e| e.enabled) {
            // 关闭自动心跳的员工不做周期性自主行动，完全由用户指定工作
            // （交办会立即唤起一轮、手动「立即执行」不走这里）。
            // 例外：御书房已有朱批 = 主管明确发话，照常起来领旨。
            if !emp.heartbeat_enabled && !with_imperial.contains(&emp.id) {
                continue;
            }
            // 非上班时段自动休眠：不巡查、不开发（手动「立即执行一轮」不受此限制）。
            // 例外：主管已在御书房朱批准奏 → 像人一样立刻起来领旨办事。
            if !within_work_hours(emp) && !with_imperial.contains(&emp.id) {
                continue;
            }
            let busy = tasks
                .tasks
                .iter()
                .any(|t| t.employee_id == emp.id && t.status == "working");
            if busy {
                continue;
            }
            if now - emp.last_heartbeat_ms < (emp.heartbeat_secs as i64) * 1000 {
                continue;
            }
            to_run.push(emp.clone());
        }
    }
    for emp in to_run {
        // 同一员工同一时间只允许一个运行中会话（增加人数才能提升并发上限）。
        if employee_has_running_thread(app, &emp.id) {
            continue;
        }
        run_cycle(app.clone(), emp, false);
    }
}

// ===== 命令收件箱处理（agent 工具 → 应用状态）=====

/// 应用侧：定时（心跳前）读取并执行收件箱里的协作命令，使 agent 工具的写操作实时生效。
/// 读取后立即清空文件；处理期间新写入的命令留待下个周期，避免丢失/重复。
pub async fn process_command_inbox(app: &AppHandle) {
    let dir = crate::nova_data_dir(app);
    let path = commands_path(&dir);
    let content = match fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return, // 文件不存在 = 无命令
    };
    if content.trim().is_empty() {
        let _ = fs::write(&path, "");
        return;
    }
    // 先清空，再处理：处理期间（可能较慢）新追加的命令进入空文件，下一周期再取。
    let _ = fs::write(&path, "");
    let cmds: Vec<InboxCommand> = content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<InboxCommand>(l).ok())
        .collect();
    if cmds.is_empty() {
        return;
    }
    for cmd in cmds {
        let _ = exec_inbox_command(app, cmd).await;
    }
    // 刷新各面板（账本 / 决策台 / 员工 / 任务）。
    let _ = app.emit(EV_MARKS, json!({}));
    let _ = app.emit(EV_DECISIONS, json!({}));
    let _ = app.emit(EV_EMPLOYEES, json!({}));
    let _ = app.emit(EV_TASKS, json!({}));
}

async fn exec_inbox_command(app: &AppHandle, cmd: InboxCommand) -> Result<(), String> {
    let Some(actor) = find_employee(app, &cmd.from) else {
        return Err(format!("发起方员工不存在：{}", cmd.from));
    };
    match cmd.kind.as_str() {
        "relay" => exec_relay(app, &actor, cmd).await,
        "memo" => {
            let title = memo_title(&cmd);
            append_journal_full(app, &actor.id, &cmd.key, &title, &cmd.text, false, "memory");
            Ok(())
        }
        "learn" => {
            let title = memo_title(&cmd);
            append_journal_full(
                app,
                &actor.id,
                &cmd.key,
                &title,
                &cmd.text,
                true,
                "knowledge",
            );
            Ok(())
        }
        // 守则（试行）：员工从挫折中给自己立的行为规则，先试行、经实践验证后内化。
        "lesson" => {
            let text = cmd.text.trim();
            if !text.is_empty() {
                append_necessary_lesson(app, &actor, &cmd.key, "守则（试行）", text);
            }
            Ok(())
        }
        // 守则验证：实践证实一次 +1，达到阈值自动内化（置顶长期注入）。
        "lesson-verify" => {
            if cmd.ts != 0 {
                let evidence_key = if cmd.key.trim().is_empty() {
                    format!("event:{}", cmd.at)
                } else {
                    cmd.key.trim().to_string()
                };
                {
                    let state = app.state::<AppState>();
                    let mut mem = state.memory.lock().unwrap();
                    let _ = mem.verify_lesson(&actor.id, cmd.ts, &evidence_key);
                }
            }
            Ok(())
        }
        "forget" => {
            forget_memory(app, &actor.id, cmd.ts);
            Ok(())
        }
        "done" => {
            exec_done(app, &actor, &cmd).await;
            Ok(())
        }
        "blocked" => {
            exec_blocked(app, &actor, &cmd).await;
            Ok(())
        }
        other => Err(format!("未知命令 kind：{other}")),
    }
}

fn memo_title(cmd: &InboxCommand) -> String {
    if !cmd.title.trim().is_empty() {
        cmd.title.trim().to_string()
    } else if !cmd.key.trim().is_empty() {
        cmd.key.trim().to_string()
    } else {
        "沉淀".to_string()
    }
}

/// 按 id 优先、其次按名字解析员工（工具里烘焙的是 id，name 作兜底）。

/// 宣员工起床：清零心跳计时，下一 tick 即唤起。
pub fn summon_employee(app: &AppHandle, emp_id: &str) {
    let state = app.state::<AppState>();
    let mut emps = state.employees.lock().unwrap();
    if let Some(e) = emps.get_mut(emp_id) {
        e.last_heartbeat_ms = 0;
        e.updated_at = now_ms();
    }
    emps.save();
}

/// Notice 系统通知（Windows toast）；供 notice 模块调用。
pub fn notify_decision_toast(app: &AppHandle, emp_name: &str, question: &str) {
    notify_decision(app, emp_name, question);
}

pub fn notice_append_event(
    app: &AppHandle,
    employee_id: &str,
    task_id: &str,
    task_title: &str,
    summary: &str,
    kind: &str,
) {
    append_event(app, employee_id, task_id, task_title, summary, kind);
}

/// Notice Action：认领/夺回单子（resume_claim / claim_for）。
pub async fn notice_claim_mark(
    app: &AppHandle,
    employee_id: &str,
    scope: &str,
    key: &str,
    title: &str,
    brief: &str,
) {
    let Some(emp) = find_employee(app, employee_id) else {
        return;
    };
    let use_shared = emp.shared_ledger && relay_configured(app);
    let (outcome, _) = ledger_claim_one(app, use_shared, scope, key, title, &emp).await;
    if outcome == ClaimOutcome::Acquired {
        if !brief.trim().is_empty() {
            ledger_set_status(
                app,
                use_shared,
                scope,
                key,
                "claimed",
                Some(brief.trim().to_string()),
                false,
            )
            .await;
        }
        cooloff_clear(scope, key);
        reset_dev_round_counter(scope, key);
        let _ = app.emit(EV_MARKS, json!({}));
    }
}

/// Notice Action：驳回停单。
pub async fn notice_fail_mark(
    app: &AppHandle,
    employee_id: &str,
    scope: &str,
    key: &str,
    reason: &str,
) {
    let emp = find_employee(app, employee_id);
    let use_shared = emp
        .as_ref()
        .is_some_and(|e| e.shared_ledger && relay_configured(app));
    ledger_set_status(
        app,
        use_shared,
        scope,
        key,
        "failed",
        Some(reason.to_string()),
        true,
    )
    .await;
    if !employee_id.is_empty() {
        cooloff_add(scope, key, employee_id);
    }
    reset_dev_rounds(scope, key);
    if let Some(emp) = emp.as_ref() {
        mark_workflow_blocked(app, &emp.id, scope, key);
    }
    let _ = app.emit(EV_MARKS, json!({}));
}

/// Notice Action：释放回待处理。
pub async fn notice_release_mark(
    app: &AppHandle,
    scope: &str,
    key: &str,
    reason: Option<&str>,
    cooloff_employee_id: Option<&str>,
) {
    let emp = cooloff_employee_id.and_then(|id| find_employee(app, id));
    let use_shared = emp
        .as_ref()
        .is_some_and(|e| e.shared_ledger && relay_configured(app));
    let use_shared = if use_shared {
        true
    } else {
        let st = app.state::<AppState>();
        let emps = st.employees.lock().unwrap();
        emps.employees
            .iter()
            .any(|e| e.shared_ledger && relay_configured(app) && emp_scope(e) == scope)
    };
    ledger_set_status(
        app,
        use_shared,
        scope,
        key,
        "open",
        reason.map(|s| s.to_string()),
        true,
    )
    .await;
    if let Some(eid) = cooloff_employee_id {
        cooloff_add(scope, key, eid);
        reset_dev_rounds(scope, key);
    }
    let _ = app.emit(EV_MARKS, json!({}));
}

pub fn find_employee(app: &AppHandle, id_or_name: &str) -> Option<Employee> {
    let id_or_name = id_or_name.trim();
    if id_or_name.is_empty() {
        return None;
    }
    let state = app.state::<AppState>();
    let store = state.employees.lock().unwrap();
    store
        .employees
        .iter()
        .find(|e| e.id == id_or_name)
        .or_else(|| store.employees.iter().find(|e| e.name == id_or_name))
        .cloned()
}

fn relay_configured(app: &AppHandle) -> bool {
    app.state::<AppState>().relay.is_configured()
}

/// 命令里带了 scope 就用，否则回退到该员工的私有/共享 scope。
fn cmd_scope_or(emp: &Employee, cmd: &InboxCommand) -> String {
    if cmd.scope.trim().is_empty() {
        emp_scope(emp)
    } else {
        cmd.scope.trim().to_string()
    }
}

/// 把一个单子「派」给指定 owner（建 claimed 标记）。账本只保存占用关系；
/// 交接说明会写进目标会话，避免账本变成第二套工作史。
#[allow(dead_code)]
async fn ledger_assign(
    app: &AppHandle,
    target: &Employee,
    use_shared: bool,
    scope: &str,
    key: &str,
    title: &str,
    brief: &str,
) -> ClaimOutcome {
    let (outcome, _snap) = ledger_claim_one(app, use_shared, scope, key, title, target).await;
    if outcome == ClaimOutcome::Acquired && !brief.trim().is_empty() {
        // 开发会话不再按单子复用；交接说明必须落到账本备注里，目标员工新开会话时才能看到。
        ledger_set_status(
            app,
            use_shared,
            scope,
            key,
            "claimed",
            Some(brief.trim().to_string()),
            false,
        )
        .await;
    }
    outcome
}

/// relay：接力（work）/ 讨论（discuss）/ 交用户决策（decision 或 to=user）。
async fn exec_relay(app: &AppHandle, actor: &Employee, cmd: InboxCommand) -> Result<(), String> {
    let to = cmd.to.trim();
    let kind = cmd.relay_kind.trim();

    // 上奏御书房：建一道候旨奏折（并把单子挂在 actor 名下，主管批复后由其领旨继续）。
    if kind.eq_ignore_ascii_case("decision") || to.eq_ignore_ascii_case("user") {
        exec_relay_decision(app, actor, &cmd).await;
        return Ok(());
    }

    // 解析接力目标：self=自己、否则必须是本机已启用同事（to 必须真实存在）。
    let target = if to.eq_ignore_ascii_case("self") {
        actor.clone()
    } else {
        match resolve_relay_target(app, actor, to) {
            Ok(e) => e,
            Err(msg) => {
                append_journal(app, &actor.id, &cmd.key, "交棒目标非法", &msg);
                return Err(msg);
            }
        }
    };

    // 数字员工像人一样：随时可以决定自己接着做（to=self）。不再需要任何「允许接力给自己」开关。

    if kind.eq_ignore_ascii_case("discuss") {
        let topic = if cmd.title.trim().is_empty() {
            cmd.key.clone()
        } else {
            cmd.title.clone()
        };
        let body = if !cmd.brief.trim().is_empty() {
            cmd.brief.clone()
        } else {
            cmd.question.clone()
        };
        let question_brief = if body.trim().is_empty() {
            topic.clone()
        } else {
            format!("{topic}\n{body}")
        };
        let from_scope = cmd_scope_or(actor, &cmd);
        let to_scope = if target.id == actor.id {
            from_scope.clone()
        } else {
            emp_scope(&target)
        };
        let key = cmd.key.clone();
        let title = if cmd.title.trim().is_empty() {
            key.clone()
        } else {
            cmd.title.clone()
        };
        // 串行 wake-do：去程交棒给 B，不再旁路同步开会。
        let expect = crate::notice::template_discuss(
            actor,
            &target,
            &from_scope,
            &to_scope,
            &key,
            &title,
            &question_brief,
        );
        crate::notice::emit_notice(
            app,
            crate::notice::EmitParams {
                from: crate::notice::ActorRef::employee(&actor.id, &actor.name),
                to: crate::notice::ActorRef::employee(&target.id, &target.name),
                label: "discuss".into(),
                topic: crate::notice::NoticeTopic {
                    scope: to_scope,
                    mark_key: key.clone(),
                    title: title.clone(),
                    thread_id: chain_anchor_thread(app, &cmd.origin_thread_id),
                },
                body: crate::notice::NoticeBody {
                    brief: body,
                    question: Some(topic),
                    options: vec![],
                },
                hold: None,
                expect,
                dedupe_key: None,
                meta: crate::notice::NoticeMeta::default(),
            },
        );
        append_journal(
            app,
            &actor.id,
            &key,
            "发起讨论",
            &format!(
                "已交棒给 {} 讨论：[{}] {}（等待对方答复后再继续）",
                target.name, key, title
            ),
        );
        return Ok(());
    }

    // 默认 work：emit Notice（onDelivered = release + claim + wake）
    let source_scope = cmd_scope_or(actor, &cmd);
    let target_scope = if target.id == actor.id {
        source_scope.clone()
    } else {
        emp_scope(&target)
    };
    let same = target.id == actor.id;
    let title = if cmd.title.trim().is_empty() {
        cmd.key.clone()
    } else {
        cmd.title.clone()
    };
    let expect = crate::notice::template_work(
        actor,
        &target,
        &source_scope,
        &target_scope,
        &cmd.key,
        &title,
        &cmd.brief,
        same,
    );
    crate::notice::emit_notice(
        app,
        crate::notice::EmitParams {
            from: crate::notice::ActorRef::employee(&actor.id, &actor.name),
            to: crate::notice::ActorRef::employee(&target.id, &target.name),
            label: "work".into(),
            topic: crate::notice::NoticeTopic {
                scope: target_scope.clone(),
                mark_key: cmd.key.clone(),
                title: title.clone(),
                thread_id: chain_anchor_thread(app, &cmd.origin_thread_id),
            },
            body: crate::notice::NoticeBody {
                brief: cmd.brief.clone(),
                question: None,
                options: vec![],
            },
            hold: None,
            expect,
            dedupe_key: None,
            meta: crate::notice::NoticeMeta::default(),
        },
    );
    let who = if same {
        "自己".to_string()
    } else {
        target.name.clone()
    };
    append_journal(
        app,
        &actor.id,
        &cmd.key,
        "会话接力",
        &format!("已接力给 {who}：[{}] {}", cmd.key, title),
    );
    Ok(())
}

fn list_employee_names(app: &AppHandle) -> String {
    let names = list_relay_peer_names(app, false);
    if names.is_empty() {
        "（无）".into()
    } else {
        names.join("、")
    }
}

/// 可交棒同事名（默认只含已启用；`include_disabled=true` 时标注未启用）。
fn list_relay_peer_names(app: &AppHandle, include_disabled: bool) -> Vec<String> {
    let state = app.state::<AppState>();
    let store = state.employees.lock().unwrap();
    store
        .employees
        .iter()
        .filter(|e| e.enabled || include_disabled)
        .map(|e| {
            if e.enabled {
                e.name.clone()
            } else {
                format!("{}(未启用)", e.name)
            }
        })
        .collect()
}

/// 解析接力/讨论目标：只允许 self / user / 本机已启用员工。不存在则 Err。
fn resolve_relay_target(app: &AppHandle, actor: &Employee, to: &str) -> Result<Employee, String> {
    let to = to.trim();
    if to.is_empty() {
        return Err("交棒目标 to 不能为空（必须是本机已启用同事名、self 或 user）".into());
    }
    if to.eq_ignore_ascii_case("self") {
        return Ok(actor.clone());
    }
    if to.eq_ignore_ascii_case("user") {
        return Err("to=user 请走 decision/escalate，不要走 discuss/handoff".into());
    }
    match find_employee(app, to) {
        Some(e) if e.enabled || e.id == actor.id => Ok(e),
        Some(e) => Err(format!(
            "同事「{}」未启用，不能作为 to。可交棒：{}",
            e.name,
            list_employee_names(app)
        )),
        None => Err(format!(
            "to「{to}」不存在。to 必须是本机已启用员工名。可交棒：{}",
            list_employee_names(app)
        )),
    }
}

/// relay decision / to=user：把单子挂到 actor 名下（claimed）并 emit 上奏 Notice（含 PendingIntent）。
async fn exec_relay_decision(app: &AppHandle, actor: &Employee, cmd: &InboxCommand) {
    let scope = cmd_scope_or(actor, cmd);
    let use_shared = actor.shared_ledger && relay_configured(app);
    let key = cmd.key.trim();
    if key.is_empty() {
        return;
    }
    let task_title = task_title_of(key, &cmd.title);
    let (outcome, _) = ledger_claim_one(app, use_shared, &scope, key, &task_title, actor).await;
    if outcome != ClaimOutcome::Acquired {
        append_journal(
            app,
            &actor.id,
            key,
            "交用户决策未生效",
            &format!("[{key}] 已被他人认领或已完成。"),
        );
        return;
    }
    let question = if !cmd.question.trim().is_empty() {
        cmd.question.trim().to_string()
    } else if !cmd.brief.trim().is_empty() {
        cmd.brief.trim().to_string()
    } else {
        format!("是否推进：{task_title}？")
    };
    let brief = cmd.brief.trim().to_string();
    let category = normalize_decision_category(&cmd.category);
    let note = if brief.is_empty() {
        question.clone()
    } else {
        brief.clone()
    };
    ledger_set_status(app, use_shared, &scope, key, "claimed", Some(note), false).await;
    let thread_id = if cmd.origin_thread_id.trim().is_empty() {
        dev_thread_get(&scope, key).and_then(|t| chain_anchor_thread(app, &t))
    } else {
        chain_anchor_thread(app, &cmd.origin_thread_id)
            .or_else(|| Some(cmd.origin_thread_id.trim().to_string()))
    };
    let source = if cmd.decision_source.trim() == "wake" {
        "wake"
    } else {
        "employee"
    };
    let dedupe = Some(decision_signature(&scope, key, &category, &question));
    crate::notice::emit_decision(
        app,
        actor,
        &scope,
        key,
        &task_title,
        &brief,
        &question,
        &category,
        cmd.options.clone(),
        thread_id,
        dedupe,
        source,
        "",
        "",
    );
    append_journal(
        app,
        &actor.id,
        key,
        "上奏御书房",
        &format!("已具折上奏御书房候旨：{task_title}"),
    );
}

/// 员工上奏后的系统通知（窗口在前台时不打扰）：主管点击直达御书房批阅。
fn notify_decision(app: &AppHandle, emp_name: &str, question: &str) {
    crate::sys_notify::notify_decision(app, emp_name, &one_line(question, 100), EV_DECISIONS_OPEN);
}

/// 把 agent 传入的决策类型收敛到已知取值；未知/空 → other。
fn normalize_decision_category(raw: &str) -> String {
    match raw.trim().to_lowercase().as_str() {
        "approve" | "审批" | "是否" => "approve".to_string(),
        "choose" | "选型" | "选择" => "choose".to_string(),
        "input" | "补充" | "内容" => "input".to_string(),
        "priority" | "优先级" | "排期" => "priority".to_string(),
        "" => "other".to_string(),
        other => other.to_string(),
    }
}

fn stable_error_kind(text: &str) -> &'static str {
    let lower = text.to_lowercase();
    if lower.contains("分支已存在") || lower.contains("already exists") || lower.contains("branch")
    {
        "git-branch-conflict"
    } else if lower.contains("工作流目录不存在") || lower.contains("目录不存在") {
        "workflow-dir-missing"
    } else if lower.contains("未提交改动") || lower.contains("working tree") {
        "dirty-worktree"
    } else if lower.contains("仓库正在被另一名员工使用") {
        "repo-locked"
    } else if lower.contains("不是 git 仓库") {
        "not-git-repo"
    } else {
        "workflow-error"
    }
}

fn decision_signature(scope: &str, key: &str, category: &str, text: &str) -> String {
    format!(
        "{}\u{1}{}\u{1}{}\u{1}{}",
        scope.trim(),
        key.trim(),
        category.trim(),
        stable_error_kind(text)
    )
}

fn workflow_decision_options(question: &str) -> Vec<String> {
    match stable_error_kind(question) {
        "dirty-worktree" => vec![
            "先停止，等我处理工作区改动".into(),
            "改用独立 worktree 继续".into(),
            "放回待处理，稍后再试".into(),
        ],
        "workflow-dir-missing" => {
            vec!["重建工作流目录后继续".into(), "放回待处理，稍后再试".into()]
        }
        "repo-locked" => vec!["稍后自动重试".into(), "放回待处理，等另一个员工结束".into()],
        "not-git-repo" => vec!["驳回，工作目录配置错误".into()],
        _ => vec![
            "按系统建议自动修复后继续".into(),
            "放回待处理，稍后再试".into(),
            "驳回，停止这张单".into(),
        ],
    }
}

fn proposed_action_for_workflow_error(question: &str) -> String {
    match stable_error_kind(question) {
        "dirty-worktree" => "wait-for-clean-worktree".into(),
        "workflow-dir-missing" => "rebuild-workflow".into(),
        "repo-locked" => "retry-later".into(),
        "not-git-repo" => "fix-employee-cwd".into(),
        _ => "diagnose-workflow-error".into(),
    }
}

/// done 工具：把单子标记为完成（收尾），并撤回其上残余的候旨奏折（问题已随单子失效）。
async fn exec_done(app: &AppHandle, actor: &Employee, cmd: &InboxCommand) {
    let scope = cmd_scope_or(actor, cmd);
    let use_shared = actor.shared_ledger && relay_configured(app);
    let task_title = task_title_of(&cmd.key, &cmd.title);
    let thread_id = dev_thread_get(&scope, &cmd.key);
    // 已是完成态（文本 DONE 兜底先落了账）→ 幂等跳过，避免重复留痕。
    let already = ledger_status(app, use_shared, &scope, &cmd.key).await == "done";
    let note = if cmd.summary.trim().is_empty() {
        None
    } else {
        Some(cmd.summary.trim().to_string())
    };
    commit_workflow_if_needed(
        app,
        actor,
        &scope,
        &cmd.key,
        &task_title,
        note.as_deref().unwrap_or("任务完成"),
    );
    ledger_set_status(app, use_shared, &scope, &cmd.key, "done", note, false).await;
    withdraw_decisions(app, &scope, &cmd.key);
    reset_dev_rounds(&scope, &cmd.key);
    if !already {
        let what = if cmd.summary.trim().is_empty() {
            format!("单子 [{}] 已办结。", cmd.key)
        } else {
            cmd.summary.trim().to_string()
        };
        append_event(
            app,
            &actor.id,
            &cmd.key,
            &format!("办成 {}", cmd.key),
            &what,
            "outcome:done",
        );
        // 完工汇报呈进御书房（主管点已读归档即可，不需要批示）
        file_report(app, actor, &scope, &cmd.key, &task_title, &what, thread_id);
    }
}

/// blocked 工具：释放认领回到「待处理」（供他人接力）。本人进入冷却，防止下一跳又自己捡回来。
/// 候旨保护：该单子上有自己候旨中的奏折时**不释放**——上奏候旨不是受阻，单子必须留在名下，
/// 否则主管批复后没人领旨（agent 常把两者混淆，一边上奏一边 blocked）。
async fn exec_blocked(app: &AppHandle, actor: &Employee, cmd: &InboxCommand) {
    let scope = cmd_scope_or(actor, cmd);
    let waiting = crate::notice::pending_hold_for(app, &actor.id, &scope, &cmd.key);
    if waiting {
        append_journal(
            app,
            &actor.id,
            &cmd.key,
            "候旨中，未释放",
            &format!(
                "[{}] 已上奏御书房候旨，blocked 不生效；主管批复后继续。",
                cmd.key
            ),
        );
        return;
    }
    let use_shared = actor.shared_ledger && relay_configured(app);
    // 已失败的单子只保留记录，不能由 blocked 兜底自动放回待处理。
    let st = ledger_status(app, use_shared, &scope, &cmd.key).await;
    if st == "failed" {
        cooloff_add(&scope, &cmd.key, &actor.id);
        reset_dev_rounds(&scope, &cmd.key);
        return;
    }
    let already = st == "open";
    let note = if cmd.reason.trim().is_empty() {
        None
    } else {
        Some(cmd.reason.trim().to_string())
    };
    ledger_set_status(app, use_shared, &scope, &cmd.key, "open", note, true).await;
    cooloff_add(&scope, &cmd.key, &actor.id);
    reset_dev_rounds(&scope, &cmd.key);
    if !already {
        let why = if cmd.reason.trim().is_empty() {
            "（未说明原因）".to_string()
        } else {
            cmd.reason.trim().to_string()
        };
        append_event(
            app,
            &actor.id,
            &cmd.key,
            &format!("受阻放下 {}", cmd.key),
            &format!("我受阻放下了这张单子，原因：{why}"),
            "outcome:blocked",
        );
    }
}

/// forget 工具：按时间戳删除一条记忆（清冗）。
fn forget_memory(app: &AppHandle, employee_id: &str, ts: i64) {
    if ts == 0 {
        return;
    }
    let state = app.state::<AppState>();
    let _ = state.memory.lock().unwrap().forget_managed(employee_id, ts);
}

// ===== 开发「无进展」兜底计数（内存态，重启即清；配合租约防死循环）=====

const MAX_DEV_ROUNDS: u32 = 8;

fn dev_rounds() -> &'static std::sync::Mutex<HashMap<String, u32>> {
    static R: std::sync::OnceLock<std::sync::Mutex<HashMap<String, u32>>> =
        std::sync::OnceLock::new();
    R.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

fn rounds_key(scope: &str, key: &str) -> String {
    format!("{scope}\u{1}{key}")
}

/// 「受阻冷却」名单（内存态，重启即清）：某员工在某单子上受阻/被自动释放/被手动停止后，
/// 心跳的**确定性自动领活**跳过该单子，避免「受阻放下 → 下一跳又自己捡回来」的死循环。
/// 主管重新交办、御书房批示、巡查（LLM 判断）不受此限制。
fn blocked_cooloff() -> &'static std::sync::Mutex<HashSet<String>> {
    static R: std::sync::OnceLock<std::sync::Mutex<HashSet<String>>> = std::sync::OnceLock::new();
    R.get_or_init(|| std::sync::Mutex::new(HashSet::new()))
}

fn cooloff_key(scope: &str, key: &str, emp_id: &str) -> String {
    format!("{scope}\u{1}{key}\u{1}{emp_id}")
}

fn cooloff_add(scope: &str, key: &str, emp_id: &str) {
    blocked_cooloff()
        .lock()
        .unwrap()
        .insert(cooloff_key(scope, key, emp_id));
}

fn cooloff_hit(scope: &str, key: &str, emp_id: &str) -> bool {
    blocked_cooloff()
        .lock()
        .unwrap()
        .contains(&cooloff_key(scope, key, emp_id))
}

/// 主管重新交办/手动复位某单子时，清掉所有员工在其上的冷却（明确要求再试）。
pub fn cooloff_clear(scope: &str, key: &str) {
    let prefix = format!("{scope}\u{1}{key}\u{1}");
    blocked_cooloff()
        .lock()
        .unwrap()
        .retain(|k| !k.starts_with(&prefix));
}

fn dev_thread_get(scope: &str, key: &str) -> Option<String> {
    // 工作流续接已移除：不再按 scope+key 找旧开发会话。
    let _ = (scope, key);
    None
}

/// 本单子「无终态结论」的连续轮数 +1，返回累计值。
fn bump_dev_rounds(scope: &str, key: &str) -> u32 {
    let mut m = dev_rounds().lock().unwrap();
    let e = m.entry(rounds_key(scope, key)).or_insert(0);
    *e += 1;
    *e
}

/// 单子收尾（完成/受阻/释放）：清掉轮数计数与会话绑定——下次若再认领会开全新会话。
fn reset_dev_rounds(scope: &str, key: &str) {
    let k = rounds_key(scope, key);
    dev_rounds().lock().unwrap().remove(&k);
}

/// 只清「无进展轮数」计数、保留会话绑定：候旨/领旨后继续同一会话，但计数从头再来。
fn reset_dev_round_counter(scope: &str, key: &str) {
    dev_rounds().lock().unwrap().remove(&rounds_key(scope, key));
}

fn repo_locks() -> &'static std::sync::Mutex<HashSet<String>> {
    static R: std::sync::OnceLock<std::sync::Mutex<HashSet<String>>> = std::sync::OnceLock::new();
    R.get_or_init(|| std::sync::Mutex::new(HashSet::new()))
}

struct RepoLockGuard {
    repo: Option<String>,
}

impl Drop for RepoLockGuard {
    fn drop(&mut self) {
        if let Some(repo) = self.repo.take() {
            repo_locks().lock().unwrap().remove(&repo);
        }
    }
}

fn try_lock_repo(repo: &str) -> Option<RepoLockGuard> {
    let mut locks = repo_locks().lock().unwrap();
    if locks.contains(repo) {
        None
    } else {
        locks.insert(repo.to_string());
        Some(RepoLockGuard {
            repo: Some(repo.to_string()),
        })
    }
}

/// 一次 Do 前的临时执行位置（不持久化；分支策略由 Wake 当场决定）。
struct WorkspaceReady {
    workspace: Workflow,
    _repo_lock: Option<RepoLockGuard>,
}

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
struct WorkPreflight {
    /// current | existingBranch | newBranch | worktree —— current = 留在源/当前分支
    #[serde(default)]
    mode: String,
    #[serde(default)]
    branch: String,
    #[serde(default)]
    base_branch: String,
    #[serde(default)]
    branch_candidates: Vec<String>,
    #[serde(default)]
    support: Vec<String>,
    #[serde(default)]
    focus_files: Vec<String>,
    #[serde(default)]
    commands: Vec<String>,
    #[serde(default)]
    risks: Vec<String>,
    #[serde(default)]
    reason: String,
    #[serde(default)]
    summary: String,
}

/// Wake 小模型结构化决策：路由下一步 +（若 do）工作区策略。
#[derive(Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
struct WakeDecision {
    /// do | discuss | handoff | escalate | done | release
    #[serde(default)]
    intent: String,
    #[serde(default)]
    reason: String,
    #[serde(default)]
    brief: String,
    #[serde(default)]
    to: String,
    #[serde(default)]
    question: String,
    #[serde(default)]
    options: Vec<String>,
    #[serde(default)]
    category: String,
    #[serde(default)]
    workspace: Option<WorkPreflight>,
    // 兼容旧 PREFLIGHT：字段直接挂在根上
    #[serde(default)]
    mode: String,
    #[serde(default)]
    branch: String,
    #[serde(default)]
    base_branch: String,
    #[serde(default)]
    branch_candidates: Vec<String>,
    #[serde(default)]
    support: Vec<String>,
    #[serde(default)]
    focus_files: Vec<String>,
    #[serde(default)]
    commands: Vec<String>,
    #[serde(default)]
    risks: Vec<String>,
    #[serde(default)]
    summary: String,
}

impl WakeDecision {
    fn intent_key(&self) -> &str {
        let i = self.intent.trim();
        if i.is_empty() {
            "do"
        } else {
            i
        }
    }

    fn workspace_plan(&self) -> WorkPreflight {
        if let Some(w) = &self.workspace {
            return w.clone();
        }
        WorkPreflight {
            mode: self.mode.clone(),
            branch: self.branch.clone(),
            base_branch: self.base_branch.clone(),
            branch_candidates: self.branch_candidates.clone(),
            support: self.support.clone(),
            focus_files: self.focus_files.clone(),
            commands: self.commands.clone(),
            risks: self.risks.clone(),
            reason: self.reason.clone(),
            summary: self.summary.clone(),
        }
    }
}

enum WakeRun {
    /// 进入 Do；plan 为工作区策略（含 mode=current）
    Do {
        thread_id: String,
        plan: WorkPreflight,
    },
    /// 不进入 Do：已发 Notice / 已路由
    Routed {
        thread_id: String,
    },
    Escalated {
        thread_id: String,
    },
    Cancelled {
        thread_id: String,
    },
    /// 非 git 仓等：跳过 Wake 模型，按 current 直接 Do
    Skipped,
}

// ===== 执行 =====

async fn run_prompt_for(kind: &AgentKind, app: &AppHandle, thread_id: String, prompt: String) {
    run_prompt_for_images(kind, app, thread_id, prompt, Vec::new()).await;
}

async fn run_prompt_for_images(
    kind: &AgentKind,
    app: &AppHandle,
    thread_id: String,
    prompt: String,
    images: Vec<PromptImage>,
) {
    // 先取出 manager 再 await，避免持着 State 引用跨 await 点
    let acp_mgr = app.state::<AppState>().acp_for(kind);
    match acp_mgr {
        Some(mgr) => mgr.run_prompt(thread_id, prompt, images).await,
        None => {
            let mgr = app.state::<AppState>().codex.clone();
            mgr.run_prompt(thread_id, prompt, images).await
        }
    }
}

pub(crate) async fn run_employee_prompt(
    kind: &AgentKind,
    app: &AppHandle,
    thread_id: String,
    prompt: String,
) {
    run_prompt_for(kind, app, thread_id, prompt).await;
}

/// 员工的协作账本 scope：填了就用，否则用私有 scope。
fn emp_scope(emp: &Employee) -> String {
    if emp.mark_scope.trim().is_empty() {
        format!("emp:{}", emp.id)
    } else {
        emp.mark_scope.trim().to_string()
    }
}

pub(crate) fn employee_scope(emp: &Employee) -> String {
    emp_scope(emp)
}

pub(crate) fn mind_agent_kind(emp: &Employee) -> AgentKind {
    emp.mind_agent_kind
        .clone()
        .or_else(|| emp.heartbeat_agent_kind.clone())
        .unwrap_or_else(|| emp.agent_kind.clone())
}

pub(crate) fn new_mind_thread(app: &AppHandle, emp: &Employee, title: &str) -> String {
    new_mind_thread_with_parent(app, emp, title, true, None)
}

fn new_mind_thread_with_parent(
    app: &AppHandle,
    emp: &Employee,
    title: &str,
    mind_thread: bool,
    parent_thread_id: Option<&str>,
) -> String {
    let model = emp
        .mind_model
        .clone()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| emp.heartbeat_model.clone().filter(|s| !s.trim().is_empty()))
        .or_else(|| emp.model.clone());
    let id = new_thread_full(
        app,
        emp,
        title,
        emp.cwd.clone(),
        mind_agent_kind(emp),
        model,
        None,
        mind_thread,
    );
    set_thread_parent(app, &id, parent_thread_id);
    id
}

fn set_thread_parent(app: &AppHandle, thread_id: &str, parent_thread_id: Option<&str>) {
    let Some(parent_id) = parent_thread_id.map(str::trim).filter(|s| !s.is_empty()) else {
        return;
    };
    if parent_id == thread_id {
        return;
    }
    let state = app.state::<AppState>();
    let mut store = state.store.lock().unwrap();
    if let Some(thread) = store.get_mut(thread_id) {
        thread.parent_thread_id = Some(parent_id.to_string());
    }
    store.save();
    let _ = app.emit(crate::acp::EV_THREADS, json!({}));
}

/// 沿 parent_thread_id 走到链根（同一任务链共用一个父会话）。
fn thread_chain_root(app: &AppHandle, thread_id: &str) -> String {
    let state = app.state::<AppState>();
    let store = state.store.lock().unwrap();
    let mut cur = thread_id.to_string();
    let mut guard = 0;
    while guard < 32 {
        guard += 1;
        let Some(t) = store.get(&cur) else {
            break;
        };
        match t
            .parent_thread_id
            .as_deref()
            .filter(|s| !s.trim().is_empty())
        {
            Some(p) if p != cur => cur = p.to_string(),
            _ => break,
        }
    }
    cur
}

/// 本单链上的父会话：优先 Notice 上记录的 origin（上奏/交棒锚点），其次已有会话的链根。
fn resolve_chain_parent(
    app: &AppHandle,
    employee_id: &str,
    scope: &str,
    key: &str,
    hint_thread: Option<&str>,
) -> Option<String> {
    let from_notice = {
        let st = app.state::<AppState>();
        let store = st.notices.lock().unwrap();
        store
            .notices
            .iter()
            .rev()
            .find(|n| {
                n.topic.scope == scope
                    && n.topic.mark_key == key
                    && n.topic
                        .thread_id
                        .as_ref()
                        .is_some_and(|t| !t.trim().is_empty())
                    && (n.from.employee_id() == Some(employee_id)
                        || n.to.employee_id() == Some(employee_id))
            })
            .and_then(|n| n.topic.thread_id.clone())
    };
    if let Some(t) = from_notice.filter(|t| thread_alive(app, t)) {
        let root = thread_chain_root(app, &t);
        if thread_alive(app, &root) {
            return Some(root);
        }
    }
    if let Some(h) = hint_thread.filter(|s| !s.trim().is_empty() && thread_alive(app, s)) {
        let root = thread_chain_root(app, h);
        if thread_alive(app, &root) {
            return Some(root);
        }
    }
    None
}

/// 把 origin 归一成链根，便于 Notice.topic.thread_id 始终指向同一父会话。
fn chain_anchor_thread(app: &AppHandle, origin_thread_id: &str) -> Option<String> {
    let id = origin_thread_id.trim();
    if id.is_empty() || !thread_alive(app, id) {
        return None;
    }
    Some(thread_chain_root(app, id))
}

pub(crate) fn mark_mind_completed(app: &AppHandle, employee_id: &str) {
    let state = app.state::<AppState>();
    let mut employees = state.employees.lock().unwrap();
    if let Some(emp) = employees.get_mut(employee_id) {
        emp.mind_event_seeded_at = now_ms();
        emp.updated_at = now_ms();
    }
    employees.save();
}

pub(crate) fn mind_employee_idle(app: &AppHandle, emp: &Employee) -> bool {
    if employee_has_running_thread(app, &emp.id) {
        return false;
    }
    let state = app.state::<AppState>();
    if state
        .tasks
        .lock()
        .unwrap()
        .tasks
        .iter()
        .any(|t| t.employee_id == emp.id && (t.status == "queued" || t.status == "working"))
    {
        return false;
    }
    if crate::notice::has_pending_work_signal(app, &emp.id) {
        return false;
    }
    if !emp.shared_ledger {
        let scope = emp_scope(emp);
        if state.marks.lock().unwrap().marks.iter().any(|m| {
            if m.scope != scope || m.status == "done" {
                return false;
            }
            if m.status == "open" {
                return true;
            }
            m.status == "claimed"
                && m.owner.as_deref() == Some(emp.id.as_str())
                && !crate::notice::pending_hold_for(app, &emp.id, &scope, &m.key)
        }) {
            return false;
        }
    } else if emp.heartbeat_enabled
        && now_ms() - emp.last_heartbeat_ms >= (emp.heartbeat_secs as i64) * 1000
    {
        return false;
    }
    true
}

pub(crate) async fn cancel_employee_thread(app: &AppHandle, thread_id: &str) {
    let kind = {
        let state = app.state::<AppState>();
        let store = state.store.lock().unwrap();
        store.get(thread_id).map(|t| t.agent_kind.clone())
    };
    let Some(kind) = kind else {
        return;
    };
    let state = app.state::<AppState>();
    state
        .cancelled_employee_threads
        .lock()
        .unwrap()
        .insert(thread_id.to_string());
    match state.acp_for(&kind) {
        Some(mgr) => {
            let _ = mgr.cancel(thread_id).await;
        }
        None => {
            let _ = state.codex.cancel(thread_id).await;
        }
    }
}

const STOP_REASON_LEDGER_DELETED: &str = "__nova_ledger_deleted__";

pub(crate) async fn cancel_deleted_ledger_thread(app: &AppHandle, thread_id: &str) {
    let exists = {
        let state = app.state::<AppState>();
        let exists = state.store.lock().unwrap().get(thread_id).is_some();
        exists
    };
    if !exists {
        return;
    }
    app.state::<AppState>()
        .employee_stop_reasons
        .lock()
        .unwrap()
        .insert(
            thread_id.to_string(),
            STOP_REASON_LEDGER_DELETED.to_string(),
        );
    cancel_employee_thread(app, thread_id).await;
}

fn task_title_of(key: &str, title: &str) -> String {
    if title.trim().is_empty() {
        key.to_string()
    } else {
        format!("{key} {title}")
    }
}

fn attachment_text(images: &[PromptImage]) -> String {
    if images.is_empty() {
        return String::new();
    }
    let mut out = String::from("【随单附件】");
    for (i, img) in images.iter().enumerate() {
        let name = if img.name.trim().is_empty() {
            "未命名附件"
        } else {
            img.name.trim()
        };
        out.push_str(&format!("\n{}. {} ({})", i + 1, name, img.mime_type));
    }
    out
}

const LEDGER_ATTACH_START: &str = "\n\n<!--nova-ledger-attachments:";
const LEDGER_ATTACH_END: &str = "-->";

fn shared_note_with_attachments(note: &str, images: &[PromptImage]) -> String {
    if images.is_empty() {
        return note.to_string();
    }
    let json = serde_json::to_string(images).unwrap_or_default();
    format!(
        "{}\n\n{}{}{}{}",
        note.trim(),
        attachment_text(images),
        LEDGER_ATTACH_START,
        json,
        LEDGER_ATTACH_END,
    )
    .trim()
    .to_string()
}

fn restore_mark_attachments(mut mark: Mark) -> Mark {
    let Some(start) = mark.note.find(LEDGER_ATTACH_START) else {
        return mark;
    };
    let payload_start = start + LEDGER_ATTACH_START.len();
    let Some(end_rel) = mark.note[payload_start..].find(LEDGER_ATTACH_END) else {
        return mark;
    };
    let end = payload_start + end_rel;
    if let Ok(images) = serde_json::from_str::<Vec<PromptImage>>(&mark.note[payload_start..end]) {
        mark.images = images;
        mark.note = format!(
            "{}{}",
            &mark.note[..start],
            &mark.note[end + LEDGER_ATTACH_END.len()..]
        )
        .trim()
        .to_string();
    }
    mark
}

// ---- 账本读写：本机 MarkStore 与共享账本（中转站）统一封装 ----

async fn ledger_marks(app: &AppHandle, use_shared: bool, scope: &str) -> Vec<Mark> {
    if use_shared {
        let relay = app.state::<AppState>().relay.clone();
        match relay.ledger_list(scope).await {
            Ok(vals) => marks_from_values(vals),
            Err(_) => Vec::new(),
        }
    } else {
        let state = app.state::<AppState>();
        let marks = state.marks.lock().unwrap();
        marks.list(Some(scope))
    }
}

async fn ledger_digest(app: &AppHandle, use_shared: bool, scope: &str) -> Result<String, String> {
    if use_shared {
        let relay = app.state::<AppState>().relay.clone();
        let vals = relay.ledger_list(scope).await?;
        Ok(render_digest(&marks_from_values(vals)))
    } else {
        let state = app.state::<AppState>();
        let marks = state.marks.lock().unwrap();
        Ok(marks.digest(scope))
    }
}

async fn ledger_claim_one(
    app: &AppHandle,
    use_shared: bool,
    scope: &str,
    key: &str,
    title: &str,
    emp: &Employee,
) -> (ClaimOutcome, Option<Mark>) {
    if use_shared {
        let relay = app.state::<AppState>().relay.clone();
        match relay
            .ledger_claim(scope, key, title, &emp.id, &emp.name, DISCUSS_LEASE_MS)
            .await
        {
            Ok((oc, mv)) => (parse_outcome(&oc), serde_json::from_value::<Mark>(mv).ok()),
            Err(_) => (ClaimOutcome::Taken, None),
        }
    } else {
        let state = app.state::<AppState>();
        let mut marks = state.marks.lock().unwrap();
        marks.claim(scope, key, title, &emp.id, &emp.name, DISCUSS_LEASE_MS)
    }
}

async fn ledger_set_status(
    app: &AppHandle,
    use_shared: bool,
    scope: &str,
    key: &str,
    status: &str,
    note: Option<String>,
    release: bool,
) {
    if use_shared {
        let relay = app.state::<AppState>().relay.clone();
        let _ = relay
            .ledger_set(scope, key, status, note.as_deref(), release)
            .await;
    } else {
        let state = app.state::<AppState>();
        let mut marks = state.marks.lock().unwrap();
        marks.set_status(scope, key, status, note, release);
    }
}

async fn delete_work_mark(app: &AppHandle, use_shared: bool, scope: &str, key: &str) {
    ledger_set_status(
        app,
        use_shared,
        scope,
        key,
        "failed",
        Some("任务已停止，保留失败记录；如需重做，请手动重新激活。".to_string()),
        true,
    )
    .await;
    cooloff_clear(scope, key);
    reset_dev_rounds(scope, key);
    withdraw_decisions(app, scope, key);
    let _ = app.emit(EV_MARKS, json!({}));
}

async fn reject_work_mark(
    app: &AppHandle,
    use_shared: bool,
    scope: &str,
    key: &str,
    employee_id: &str,
) {
    ledger_set_status(
        app,
        use_shared,
        scope,
        key,
        "failed",
        Some("开工预检被主管驳回，已停止推进；如需重做，请手动重新激活。".to_string()),
        true,
    )
    .await;
    cooloff_add(scope, key, employee_id);
    reset_dev_rounds(scope, key);
    let _ = app.emit(EV_MARKS, json!({}));
}

/// 更新账本里的会话指针。共享账本暂不强依赖服务端支持；本机账本足够保证会话接力可回看。
async fn ledger_set_thread(
    app: &AppHandle,
    use_shared: bool,
    scope: &str,
    key: &str,
    thread_id: &str,
) {
    if use_shared {
        return;
    }
    let state = app.state::<AppState>();
    let mut marks = state.marks.lock().unwrap();
    marks.set_thread(scope, key, thread_id);
}

/// 当前员工会话是否被用户手动停止。
fn thread_cancelled(app: &AppHandle, thread_id: &str) -> bool {
    let state = app.state::<AppState>();
    let cancelled = state
        .cancelled_employee_threads
        .lock()
        .unwrap()
        .contains(thread_id);
    cancelled
}

pub(crate) fn employee_thread_cancelled(app: &AppHandle, thread_id: &str) -> bool {
    thread_cancelled(app, thread_id)
}

fn clear_thread_cancelled(app: &AppHandle, thread_id: &str) {
    let state = app.state::<AppState>();
    state
        .cancelled_employee_threads
        .lock()
        .unwrap()
        .remove(thread_id);
    state
        .employee_stop_reasons
        .lock()
        .unwrap()
        .remove(thread_id);
}

pub(crate) fn clear_employee_thread_cancelled(app: &AppHandle, thread_id: &str) {
    clear_thread_cancelled(app, thread_id);
}

/// 该员工当前是否已有会话在运行（用 agent 的真实运行态判断）。
/// 用于「同一员工同一时间只允许一个运行中会话」：通过增加员工人数来提升并发上限。
fn employee_has_running_thread(app: &AppHandle, employee_id: &str) -> bool {
    let state = app.state::<AppState>();
    let ids: Vec<(String, AgentKind)> = {
        let store = state.store.lock().unwrap();
        store
            .threads
            .iter()
            .filter(|t| t.employee_id.as_deref() == Some(employee_id))
            .map(|t| (t.id.clone(), t.agent_kind.clone()))
            .collect()
    };
    ids.iter().any(|(id, kind)| match state.acp_for(kind) {
        Some(mgr) => mgr.is_running(id),
        None => state.codex.is_running(id),
    })
}

/// 用户手动停止时的业务收尾：保留失败记录，任务记为 blocked。
/// 停止原因已由 cancel_turn 直接写成 Dream 事件，避免不同类型的员工会话遗漏反馈。
/// failed 不会被自动领取；用户需要重做时可手动重新激活。
async fn abort_on_stop(
    app: &AppHandle,
    emp: &Employee,
    scope: &str,
    use_shared: bool,
    key: &str,
    task_id: Option<&str>,
    thread_id: &str,
) {
    let stop_reason = app
        .state::<AppState>()
        .employee_stop_reasons
        .lock()
        .unwrap()
        .remove(thread_id)
        .unwrap_or_default();
    if stop_reason == STOP_REASON_LEDGER_DELETED {
        reset_dev_rounds(scope, key);
        delete_tasks_for_threads(app, &[thread_id.to_string()]);
        return;
    }
    apply_task_memory_evidence(app, &emp.id, key, "outcome:stopped");
    reset_dev_rounds(scope, key);
    let note = if stop_reason.is_empty() {
        "用户手动停止，本单保留为失败记录；如需重做，请手动重新激活。".to_string()
    } else {
        format!("用户手动停止：{stop_reason}。本单保留为失败记录；如需重做，请手动重新激活。")
    };
    ledger_set_status(app, use_shared, scope, key, "failed", Some(note), true).await;
    cooloff_add(scope, key, &emp.id);
    let _ = app.emit(EV_MARKS, json!({}));
    if let Some(tid) = task_id {
        finish_task(
            app,
            tid,
            "blocked",
            Some(if stop_reason.is_empty() {
                "用户手动停止".into()
            } else {
                format!("用户手动停止：{stop_reason}")
            }),
        );
    }
}

pub async fn delete_work_by_thread(app: &AppHandle, thread_id: &str) {
    let thread_meta = {
        let state = app.state::<AppState>();
        let store = state.store.lock().unwrap();
        store
            .get(thread_id)
            .map(|t| (t.employee_id.clone(), t.title.clone(), t.mind_thread))
    };
    let (employee_id, thread_title, mind_thread) =
        thread_meta.unwrap_or((None, String::new(), false));
    let emp = employee_id.as_deref().and_then(|id| find_employee(app, id));
    let scope = emp.as_ref().map(emp_scope);
    let use_shared = emp
        .as_ref()
        .is_some_and(|emp| emp.shared_ledger && app.state::<AppState>().relay.is_configured());
    let title_matches = |m: &Mark| {
        if thread_title.trim().is_empty() {
            return false;
        }
        let task_title = task_title_of(&m.key, &m.title);
        thread_title.contains(&task_title)
            || thread_title.contains(&m.key)
            || (!m.title.trim().is_empty() && thread_title.contains(m.title.trim()))
    };
    if mind_thread {
        let decision_mark = {
            let state = app.state::<AppState>();
            let from_notice = {
                let n = state.notices.lock().unwrap();
                n.notices
                    .iter()
                    .find(|d| d.topic.thread_id.as_deref() == Some(thread_id))
                    .map(|d| (d.topic.scope.clone(), d.topic.mark_key.clone()))
            };
            from_notice.or_else(|| {
                let d = state.decisions.lock().unwrap();
                d.decisions
                    .iter()
                    .find(|d| d.thread_id.as_deref() == Some(thread_id))
                    .map(|d| (d.scope.clone(), d.mark_key.clone()))
            })
        };
        if let Some((scope, key)) = decision_mark {
            delete_work_mark(app, use_shared, &scope, &key).await;
        } else if thread_title.contains("Wake") || thread_title.contains("开工预检") {
            if let (Some(emp), Some(scope)) = (emp.as_ref(), scope.as_deref()) {
                if use_shared {
                    let relay = app.state::<AppState>().relay.clone();
                    if let Ok(vals) = relay.ledger_list(scope).await {
                        if let Some(mark) = marks_from_values(vals).into_iter().find(|m| {
                            m.status == "claimed" && m.owner.as_deref() == Some(emp.id.as_str())
                        }) {
                            delete_work_mark(app, true, scope, &mark.key).await;
                        }
                    }
                } else {
                    let mark = {
                        let state = app.state::<AppState>();
                        let marks = state.marks.lock().unwrap();
                        marks
                            .marks
                            .iter()
                            .find(|m| {
                                m.scope == scope
                                    && m.status == "claimed"
                                    && m.owner.as_deref() == Some(emp.id.as_str())
                            })
                            .cloned()
                    };
                    if let Some(mark) = mark {
                        delete_work_mark(app, false, &mark.scope, &mark.key).await;
                    }
                }
            }
        }
        delete_tasks_for_threads(app, &[thread_id.to_string()]);
        discard_thread(app, thread_id);
        return;
    }

    let local_mark = {
        let state = app.state::<AppState>();
        let marks = state.marks.lock().unwrap();
        marks
            .marks
            .iter()
            .find(|m| {
                m.thread_id.as_deref() == Some(thread_id)
                    || (scope.as_deref() == Some(m.scope.as_str())
                        && employee_id
                            .as_deref()
                            .is_some_and(|id| m.owner.as_deref() == Some(id))
                        && m.status == "claimed"
                        && title_matches(m))
            })
            .cloned()
    };
    if let Some(mark) = local_mark {
        delete_work_mark(app, false, &mark.scope, &mark.key).await;
    } else if let (Some(emp), Some(scope)) = (emp.as_ref(), scope.as_deref()) {
        if emp.shared_ledger && app.state::<AppState>().relay.is_configured() {
            let relay = app.state::<AppState>().relay.clone();
            if let Ok(vals) = relay.ledger_list(scope).await {
                if let Some(mark) = marks_from_values(vals).into_iter().find(|m| {
                    m.status == "claimed"
                        && m.owner.as_deref() == Some(emp.id.as_str())
                        && title_matches(m)
                }) {
                    delete_work_mark(app, true, scope, &mark.key).await;
                }
            }
        }
    }
    delete_tasks_for_threads(app, &[thread_id.to_string()]);
    discard_thread(app, thread_id);
}

/// 统一的自主工作循环，像一个尽责的人醒来后的行为顺序：
/// ① 先看主管有没有批示（御书房已准奏/留中的折子）——有就放下别的先办；
/// ② 再续做手上认领中的单子（候旨中的只续租保活、不打扰主管）；
/// ③ 然后领取账本里「待处理」的交办单子（确定性认领，不靠巡查 LLM 自觉）；
/// ④ 手头没活才反思沉淀；
/// ⑤ 最后才按常驻职责巡查找新活。
///
/// `manual`：用户手动「立即执行一轮」时为 true——不受上班时段限制；
/// 心跳自动触发为 false——非上班时段只为领旨破例，办完旨意即回去休眠。
fn run_cycle(app: AppHandle, emp: Employee, manual: bool) {
    // 同一员工同一时间只允许一个运行中会话：已有会话在跑就跳过（不重置心跳，下次再试）。
    if employee_has_running_thread(&app, &emp.id) {
        return;
    }
    let now = now_ms();
    {
        let state = app.state::<AppState>();
        let mut employees = state.employees.lock().unwrap();
        if let Some(e) = employees.get_mut(&emp.id) {
            e.last_heartbeat_ms = now;
            e.updated_at = now;
        }
        employees.save();
    }
    let _ = app.emit(EV_EMPLOYEES, json!({}));

    if !std::path::Path::new(&emp.cwd).is_dir() {
        return;
    }

    let scope = emp_scope(&emp);

    tauri::async_runtime::spawn(async move {
        let relay = app.state::<AppState>().relay.clone();
        let use_shared = emp.shared_ledger && relay.is_configured();

        // ① 优先消费批复注入（respond Notice 时写入的 PendingIntent 落地）。
        if run_notice_injections(&app, &emp, &scope, use_shared).await {
            return;
        }
        // 非上班时段只为「消费批复」破例起床；没有注入就回去休眠。
        // 手动「立即执行一轮」不受此限制。
        if !manual && !within_work_hours(&emp) {
            return;
        }

        // ② 在手单子（owner=我 且 处理中）：候旨中的续租保活；其余挑最久未推进的一张续做。
        let my_marks: Vec<Mark> = ledger_marks(&app, use_shared, &scope)
            .await
            .into_iter()
            .filter(|m| m.status == "claimed" && m.owner.as_deref() == Some(emp.id.as_str()))
            .collect();
        let mut inhand: Option<Mark> = None;
        for m in &my_marks {
            // 旧会话刚被停止但账本释放尚未完成时，不得抢先恢复它。
            if dev_thread_get(&scope, &m.key)
                .is_some_and(|thread_id| thread_cancelled(&app, &thread_id))
            {
                continue;
            }
            let waiting = crate::notice::pending_hold_for(&app, &emp.id, &scope, &m.key);
            if waiting {
                // 候旨中：续租保住认领（别人抢不走），继续看下一张——不因一张折子冻结全部工作。
                let _ = ledger_claim_one(&app, use_shared, &scope, &m.key, &m.title, &emp).await;
                continue;
            }
            let better = match &inhand {
                None => true,
                Some(cur) => m.updated_at < cur.updated_at, // 最久没动的优先，避免旧单被饿死
            };
            if better {
                inhand = Some(m.clone());
            }
        }

        if let Some(m) = inhand {
            let prior_thread = dev_thread_get(&scope, &m.key).filter(|t| thread_alive(&app, t));
            let extra =
                crate::notice::take_injection(&app, &emp.id, &scope, &m.key).unwrap_or_default();
            develop_and_conclude(
                &app,
                &emp,
                &scope,
                use_shared,
                &m.key,
                &m.title,
                &m.note,
                m.images.clone(),
                &extra,
                prior_thread,
            )
            .await;
            return;
        }

        // ③ 账本里「待处理」的单子（主管交办/同事转交）：像人一样直接领来干，
        //    不再依赖巡查 LLM「自觉认领」。后登记/后更新的先做；跳过自己刚受阻放下的（冷却），留给他人接力。
        if run_pickup_open(&app, &emp, &scope, use_shared).await {
            return;
        }

        // 关闭自动心跳表示不做自主巡查。手动交办/立即执行仍会处理明确的在手单和待办，
        // 但没有明确工作时到此结束，不再创建巡查会话。
        if !emp.heartbeat_enabled {
            return;
        }

        // ④ 巡查：判断当前是否有值得推进的事，用 relay 工具指定接力对象（同事/自己/用户）。
        // Mind 已从工作循环抽离，统一由 mind_tick 在确认真空闲后处理。
        //    巡查本身不改代码、不登记待认领；一切行动都通过 relay 工具表达。
        let digest = match ledger_digest(&app, use_shared, &scope).await {
            Ok(d) => d,
            Err(_) => return,
        };
        // 巡查（心跳）单独一个会话，可用与工作不同的后端 + 便宜模型（配了就用，否则回退工作后端/模型）。
        let scout_kind = emp
            .heartbeat_agent_kind
            .clone()
            .unwrap_or_else(|| emp.agent_kind.clone());
        let scout_model = emp
            .heartbeat_model
            .clone()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| emp.model.clone());
        // 巡查会话与工作后端相同才沿用工作模式，跨后端时用后端默认模式（避免非法模式报错）。
        let scout_mode = if scout_kind == emp.agent_kind {
            emp.mode.clone()
        } else {
            None
        };
        let thread_id = new_thread_full(
            &app,
            &emp,
            &format!("[{}] 巡查", emp.name),
            emp.cwd.clone(),
            scout_kind.clone(),
            scout_model,
            scout_mode,
            false,
        );
        let mem_query = format!("{} {}", emp.directive, digest);
        let memory = retrieve_memory(&app, &emp.id, &mem_query, None).await;
        let mem_dir = export_memory_files(&app, &emp).unwrap_or_default();
        let scout_prompt = build_scout_prompt(&app, &emp, &scope, &digest, &memory, &mem_dir);
        run_prompt_for(&scout_kind, &app, thread_id.clone(), scout_prompt).await;
        // 用户手动停止：丢弃巡查会话，本轮到此为止（下次心跳再来）。
        if thread_cancelled(&app, &thread_id) {
            discard_thread(&app, &thread_id);
            clear_thread_cancelled(&app, &thread_id);
            return;
        }
        // 巡查不再依赖 agent 亲自跑 relay CLI（弱模型常只写一段总结、并不真的执行工具，导致
        // 「心跳后就到此为止、没有任何接力/登记」）。改为解析它在末尾输出的 PLAN 行动块，由应用侧
        // 确定性地登记/接力/交决策（复用收件箱命令链路，含跨机接力），随后立刻就地开发一个
        // 「接力给自己」的在手单子，让一次心跳真正推进下去。
        let scout_out = extract_last_assistant(&app, &thread_id).unwrap_or_default();
        if thread_has_error(&app, &thread_id) {
            rename_thread(&app, &thread_id, &format!("[{}] 巡查 · 出错", emp.name));
            return;
        }
        let actions = parse_scout_actions(&scout_out, &scope, &emp.id);
        if actions.is_empty() {
            // 无行动：干净的「无事可做」删除不留噪音；有判断但没给出规范 PLAN 的保留可查。
            if is_idle_scout(&scout_out) {
                discard_thread(&app, &thread_id);
            } else {
                rename_thread(&app, &thread_id, &format!("[{}] 巡查", emp.name));
            }
            return;
        }
        // 确定性执行巡查规划：建 claimed 标记 / 发起讨论 / 建决策（与 CLI relay 走同一落地逻辑）。
        for cmd in actions {
            let _ = exec_inbox_command(&app, cmd).await;
        }
        let _ = app.emit(EV_MARKS, json!({}));
        let _ = app.emit(EV_DECISIONS, json!({}));
        let _ = app.emit(EV_EMPLOYEES, json!({}));
        let _ = app.emit(EV_TASKS, json!({}));
        rename_thread(&app, &thread_id, &format!("[{}] 巡查", emp.name));

        // 立刻就地开发一个「接力给自己、且无待决策」的在手单子（数字员工像人一样，自己能做的就自己做）。
        // 其余单子（派给同事 / 交用户 / 本轮没轮到的自派单）留待后续心跳或对应对象处理。
        if thread_cancelled(&app, &thread_id) {
            discard_thread(&app, &thread_id);
            clear_thread_cancelled(&app, &thread_id);
            return;
        }
        let marks = ledger_marks(&app, use_shared, &scope).await;
        let inhand = marks.into_iter().find(|m| {
            if m.status != "claimed" || m.owner.as_deref() != Some(emp.id.as_str()) {
                return false;
            }
            !crate::notice::pending_hold_for(&app, &emp.id, &scope, &m.key)
        });
        if let Some(m) = inhand {
            let prior_thread = dev_thread_get(&scope, &m.key).filter(|t| thread_alive(&app, t));
            let extra =
                crate::notice::take_injection(&app, &emp.id, &scope, &m.key).unwrap_or_default();
            develop_and_conclude(
                &app,
                &emp,
                &scope,
                use_shared,
                &m.key,
                &m.title,
                &m.note,
                m.images.clone(),
                &extra,
                prior_thread,
            )
            .await;
        }
    });
}

pub(crate) fn finalize_wake_thread_decision(app: &AppHandle, dec: &Decision) {
    if dec.source != "wake" {
        return;
    }
    let Some(thread_id) = dec.thread_id.as_deref() else {
        return;
    };
    match dec.status.as_str() {
        "rejected" => {
            append_thread_system(
                app,
                thread_id,
                &format!(
                    "【预检批复】主管已驳回这次 Wake 上奏，本单已停止推进。批复：{}",
                    dec.answer.clone().unwrap_or_else(|| "驳回".to_string())
                ),
                "warn",
            );
            rename_thread(
                app,
                thread_id,
                &format!("[{}] Wake · 开工预检已驳回", dec.employee_name),
            );
            let emp = find_employee(app, &dec.employee_id);
            let use_shared = emp.as_ref().is_some_and(|emp| {
                emp.shared_ledger && app.state::<AppState>().relay.is_configured()
            });
            let app2 = app.clone();
            let scope = dec.scope.clone();
            let key = dec.mark_key.clone();
            let employee_id = dec.employee_id.clone();
            tauri::async_runtime::spawn(async move {
                reject_work_mark(&app2, use_shared, &scope, &key, &employee_id).await;
            });
        }
        "shelved" => {
            append_thread_system(
                app,
                thread_id,
                "【预检批复】主管留中不发；员工会按专业判断继续，不再停在这次 Wake 上奏。",
                "info",
            );
            rename_thread(
                app,
                thread_id,
                &format!("[{}] Wake · 开工预检留中", dec.employee_name),
            );
        }
        "resolved" => {
            append_thread_system(
                app,
                thread_id,
                &format!(
                    "【预检批复】主管已准奏，员工会领旨进入开发。批复：{}",
                    dec.answer.clone().unwrap_or_default()
                ),
                "info",
            );
            rename_thread(
                app,
                thread_id,
                &format!("[{}] Wake · 开工预检已准奏", dec.employee_name),
            );
        }
        _ => {}
    }
}

/// ① 优先消费 Notice 批复注入（respond 时写入的 PendingIntent）。
/// 夺回单子并带着批示开发；处理了一张返回 true。
async fn run_notice_injections(
    app: &AppHandle,
    emp: &Employee,
    default_scope: &str,
    use_shared: bool,
) -> bool {
    let pending: Vec<(String, String)> = {
        let st = app.state::<AppState>();
        let store = st.notices.lock().unwrap();
        let prefix = format!("{}\u{1}", emp.id);
        store
            .injections
            .keys()
            .filter(|k| k.starts_with(&prefix))
            .filter_map(|k| {
                let mut parts = k.split('\u{1}');
                let _eid = parts.next()?;
                let scope = parts.next()?.to_string();
                let key = parts.next()?.to_string();
                Some((scope, key))
            })
            .collect()
    };
    if pending.is_empty() {
        return false;
    }
    let (scope, key) = &pending[0];
    let extra = crate::notice::take_injection(app, &emp.id, scope, key).unwrap_or_default();
    if extra.trim().is_empty() {
        return false;
    }
    let title = ledger_marks(app, use_shared, scope)
        .await
        .into_iter()
        .find(|m| m.key == *key)
        .map(|m| m.title)
        .unwrap_or_else(|| key.clone());
    let (outcome, snap) = ledger_claim_one(app, use_shared, scope, key, &title, emp).await;
    if outcome == ClaimOutcome::Taken {
        append_event(
            app,
            &emp.id,
            key,
            "批示已到，但单子已被同事接走",
            &format!("主管批示已到，但该单已由同事接手，我未再执行。\n{extra}"),
            "supervision",
        );
        return false;
    }
    if matches!(outcome, ClaimOutcome::Done | ClaimOutcome::Failed) {
        return false;
    }
    cooloff_clear(scope, key);
    let _ = app.emit(EV_MARKS, json!({}));
    let note = snap.as_ref().map(|m| m.note.clone()).unwrap_or_default();
    let images = snap.as_ref().map(|m| m.images.clone()).unwrap_or_default();
    let title = snap.as_ref().map(|m| m.title.clone()).unwrap_or(title);
    let prior_thread = dev_thread_get(scope, key).filter(|t| thread_alive(app, t));
    reset_dev_round_counter(scope, key);
    let _ = default_scope;
    develop_and_conclude(
        app,
        emp,
        scope,
        use_shared,
        key,
        &title,
        &note,
        images,
        &extra,
        prior_thread,
    )
    .await;
    true
}

/// ③ 确定性领活：账本里「待处理」（open 或租约过期）的单子，直接认领一张开工。
/// 像一个勤快的人：主管交办的、同事转交的活，醒来先看一眼就动手，不等巡查 LLM「自觉发现」。
/// open 是账本里的任务来源，按后进先出领取；账本同时负责占用和冲突。
/// 认领到并开工返回 true。
async fn run_pickup_open(app: &AppHandle, emp: &Employee, scope: &str, use_shared: bool) -> bool {
    let now = now_ms();
    let mut candidates: Vec<Mark> = ledger_marks(app, use_shared, scope)
        .await
        .into_iter()
        .filter(|m| {
            let claimable = m.status == "open" || (m.status == "claimed" && m.lease_until <= now); // 认领者挂了，可接力
            if !claimable {
                return false;
            }
            // 自己刚放下的（受阻/自动释放/手动停止）冷却中：留给同事接力，别自己捡回来死循环。
            if cooloff_hit(scope, &m.key, &emp.id) {
                return false;
            }
            // 有人正就这张单子候旨（含别的员工）：等主管批示，不抢着做。
            !crate::notice::pending_hold_on(app, scope, &m.key)
        })
        .collect();
    if candidates.is_empty() {
        return false;
    }
    candidates.sort_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then_with(|| b.created_at.cmp(&a.created_at))
    }); // 后进先出，新交办先办
    for m in candidates {
        let (outcome, snap) = ledger_claim_one(app, use_shared, scope, &m.key, &m.title, emp).await;
        if outcome != ClaimOutcome::Acquired {
            continue; // 被同事抢先，看下一张
        }
        let _ = app.emit(EV_MARKS, json!({}));
        let note = snap
            .as_ref()
            .map(|s| s.note.clone())
            .unwrap_or_else(|| m.note.clone());
        let images = snap
            .as_ref()
            .map(|s| s.images.clone())
            .unwrap_or_else(|| m.images.clone());
        let prior_thread = dev_thread_get(scope, &m.key).filter(|t| thread_alive(app, t));
        develop_and_conclude(
            app,
            emp,
            scope,
            use_shared,
            &m.key,
            &m.title,
            &note,
            images,
            "",
            prior_thread,
        )
        .await;
        return true;
    }
    false
}

/// 从巡查输出里解析结构化「行动块」（PLAN）。巡查不再依赖 agent 亲自跑 relay CLI（弱模型常只写
/// 总结不执行工具），改为让它在末尾输出机器可读的 PLAN，由应用侧确定性地登记/接力/交决策。
///
/// 只解析 `===PLAN===` 与 `===END===` 之间的行，避免误吃正文里的 markdown 表格（同样含 `|`）。
/// 每行格式：`<work|discuss|decision> | <to> | <key> | <title> | <detail>`；块内 `IDLE` 表示无事。
/// 返回一组 kind="relay" 的收件箱命令，交由 exec_inbox_command 落地（与 CLI relay 完全同链路）。
fn find_ascii_case_insensitive(haystack: &str, needle: &str) -> Option<usize> {
    let h = haystack.as_bytes();
    let n = needle.as_bytes();
    if n.is_empty() {
        return Some(0);
    }
    if n.len() > h.len() {
        return None;
    }
    h.windows(n.len()).position(|w| {
        w.iter()
            .zip(n.iter())
            .all(|(a, b)| a.to_ascii_lowercase() == b.to_ascii_lowercase())
    })
}

fn parse_scout_actions(text: &str, scope: &str, actor_id: &str) -> Vec<InboxCommand> {
    let plan_marker = "===plan===";
    let end_marker = "===end===";
    let Some(start) = find_ascii_case_insensitive(text, plan_marker) else {
        return Vec::new();
    };
    let after = &text[start + plan_marker.len()..];
    let body = match find_ascii_case_insensitive(after, end_marker) {
        Some(j) => &after[..j],
        None => after,
    };
    let mut out: Vec<InboxCommand> = Vec::new();
    for raw in body.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with("```") {
            continue;
        }
        let low = line.to_lowercase();
        if low.contains("===plan===") || low.contains("===end===") {
            continue;
        }
        // 去掉列表/表格前后缀（- * · • # 及首尾竖线）。
        let cleaned = line
            .trim_start_matches(['-', '*', '·', '•', '#', ' '])
            .trim_matches('|')
            .trim();
        if cleaned.is_empty() {
            continue;
        }
        let fields: Vec<String> = cleaned
            .splitn(5, '|')
            .map(|s| s.trim().to_string())
            .collect();
        let kind0 = fields[0].to_lowercase();
        if kind0 == "idle" {
            continue;
        }
        let relay_kind = match kind0.as_str() {
            "work" | "接力" | "开发" => "work",
            "discuss" | "讨论" | "对齐" => "discuss",
            "decision" | "决策" => "decision",
            _ => continue,
        };
        let get = |i: usize| fields.get(i).cloned().unwrap_or_default();
        let mut to = get(1);
        let key = get(2);
        let title = get(3);
        let detail = get(4);
        if key.trim().is_empty() {
            continue;
        }
        if to.trim().is_empty() {
            to = if relay_kind == "decision" {
                "user".into()
            } else {
                "self".into()
            };
        }
        out.push(InboxCommand {
            kind: "relay".into(),
            from: actor_id.to_string(),
            scope: scope.to_string(),
            key,
            title,
            to,
            relay_kind: relay_kind.to_string(),
            brief: detail.clone(),
            question: if relay_kind == "decision" {
                detail
            } else {
                String::new()
            },
            at: now_ms(),
            ..Default::default()
        });
    }
    out
}

/// 巡查是否「无事可做」：最后一条非空输出恰为 IDLE（或整体为空）。
fn is_idle_scout(text: &str) -> bool {
    text.lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .map(|l| l.trim().eq_ignore_ascii_case("IDLE"))
        .unwrap_or(true)
}

/// 开发一轮并根据结论落账：完成 / 受阻释放 / 上报决策 / 与伙伴约定后续做。
/// 每次 Do 前先跑 Wake（小模型）：决定下一步 + 是否留在源分支 / 换分支 / worktree。
#[allow(clippy::too_many_arguments)]
async fn develop_and_conclude(
    app: &AppHandle,
    emp: &Employee,
    scope: &str,
    use_shared: bool,
    key: &str,
    title: &str,
    prior_note: &str,
    images: Vec<PromptImage>,
    extra: &str,
    thread_id: Option<String>,
) {
    let task_title = task_title_of(key, title);
    // 同一任务链共用一个父会话：Notice 锚点 / 既有会话链根。
    let chain_parent = resolve_chain_parent(app, &emp.id, scope, key, thread_id.as_deref());
    // 续做同一会话时仍跑 Wake：分支策略由 Wake 当场决定，不依赖 Workflow 缓存。
    // 批注后也必须先 Wake（提示词带批示），再由 Wake 决定是否进入 Do——禁止跳过 Wake 无脑开工。
    let wake_run = run_wake(
        app,
        emp,
        scope,
        key,
        title,
        prior_note,
        extra,
        chain_parent.as_deref(),
    )
    .await;
    if let WakeRun::Cancelled { thread_id } = &wake_run {
        ledger_set_status(
            app,
            use_shared,
            scope,
            key,
            "failed",
            Some(
                "Wake 被用户终止，本次任务已停止，保留失败记录；如需重做，请手动重新激活。"
                    .to_string(),
            ),
            true,
        )
        .await;
        cooloff_add(scope, key, &emp.id);
        let _ = app.emit(EV_MARKS, json!({}));
        append_event(
            app,
            &emp.id,
            key,
            &format!("Wake 被停止 {key}"),
            "用户终止了 Wake；系统已停止整个任务，没有继续开启开发会话。",
            "outcome:stopped",
        );
        record_preflight_prepare_failure(app, Some(thread_id), "用户终止了 Wake");
        reset_dev_rounds(scope, key);
        return;
    }
    if let WakeRun::Escalated { thread_id } = &wake_run {
        let _ = app.emit(EV_MARKS, json!({}));
        let _ = app.emit(EV_DECISIONS, json!({}));
        append_thread_system(
            app,
            thread_id,
            "【Wake 上奏】已把这张单呈进御书房，等待主管批示后再继续。",
            "info",
        );
        reset_dev_round_counter(scope, key);
        return;
    }
    if let WakeRun::Routed { thread_id } = &wake_run {
        let _ = app.emit(EV_MARKS, json!({}));
        let _ = app.emit(EV_DECISIONS, json!({}));
        let _ = app.emit(EV_EMPLOYEES, json!({}));
        // 详细成败已写在 Wake 会话系统消息里，这里不再重复谎称「已发出」
        let _ = thread_id;
        reset_dev_round_counter(scope, key);
        return;
    }
    let wake_thread_id = match &wake_run {
        WakeRun::Do { thread_id, .. } => Some(thread_id.clone()),
        _ => None,
    };
    let preflight = match &wake_run {
        WakeRun::Do { plan, .. } => Some(plan.clone()),
        WakeRun::Skipped => Some(WorkPreflight {
            mode: "current".into(),
            summary: "非 git 仓或 Wake 跳过，沿用当前目录".into(),
            ..Default::default()
        }),
        _ => None,
    };
    let (workspace_ready, preflight_fallback_note) = match apply_workspace(
        app,
        emp,
        scope,
        key,
        title,
        preflight.as_ref(),
    ) {
        Ok(run) => (run, String::new()),
        Err(e) if preflight.as_ref().is_some_and(|p| p.mode != "current") => {
            match apply_workspace(app, emp, scope, key, title, None) {
                Ok(run) => (
                    run,
                    format!(
                        "\n\n【Wake 交还说明】\nWake 已完成决策，但执行位置未能由应用自动准备：{e}\n系统已降级为当前分支/当前目录并把 Wake 结果交还给本开发会话。请先核对当前仓库状态；如确需换分支或 worktree，在会话内自行说明风险并处理，除非确实需要【用户】拍板，否则不要因为 Wake 建议失败就上奏。"
                    ),
                ),
                Err(e2) => {
                    ledger_set_status(
                        app,
                        use_shared,
                        scope,
                        key,
                        "open",
                        Some(format!(
                            "Wake 后准备开发环境失败，已退回待处理：{e}；降级当前分支也失败：{e2}"
                        )),
                        true,
                    )
                    .await;
                    record_preflight_prepare_failure(
                        app,
                        wake_thread_id.as_deref(),
                        &format!("{e}；降级当前分支也失败：{e2}"),
                    );
                    reset_dev_round_counter(scope, key);
                    return;
                }
            }
        }
        Err(e) => {
            file_workflow_decision(
                app,
                emp,
                scope,
                key,
                &task_title,
                &format!("我准备执行环境失败：{e}。请先处理仓库状态。"),
            );
            reset_dev_round_counter(scope, key);
            return;
        }
    };
    let workflow = workspace_ready.workspace.clone();
    record_preflight_prepare_result(
        app,
        wake_thread_id.as_deref(),
        &workflow,
        &preflight_fallback_note,
    );
    // Do 会话：必须用工作模型，且挂在本轮 Wake 之下；绝不复用 mind/Wake 会话。
    let (thread_id, is_new) = if let Some(wid) = wake_thread_id.as_deref() {
        let t = new_dev_thread(
            app,
            emp,
            &format!("[{}] Do · {}", emp.name, task_title),
            &workflow,
            Some(wid),
        );
        (t, true)
    } else {
        match thread_id.filter(|t| thread_alive(app, t) && !thread_is_mind(app, t)) {
            Some(t) => {
                let has_started = thread_has_assistant(app, &t);
                (t, !has_started)
            }
            None => {
                let t = new_dev_thread(
                    app,
                    emp,
                    &format!("[{}] Do · {}", emp.name, task_title),
                    &workflow,
                    None,
                );
                (t, true)
            }
        }
    };
    attach_workflow_thread(app, &workflow, &thread_id);
    ledger_set_thread(app, use_shared, scope, key, &thread_id).await;
    // 记一条工作记录（origin self, working），兼作忙碌闸与活动历史
    let task_id = create_task(
        app,
        &emp.id,
        &task_title,
        &emp.directive,
        "self",
        Some(thread_id.clone()),
        "working",
    )
    .map(|t| t.id);

    let dev_prompt = if is_new {
        // 长期知识 + 与本单相关的历史工作记忆（语义/向量检索，不可用时回退 BM25）主动注入；
        // 更全的记忆再导出成文件，让 agent 用 grep/读取工具按需深挖（被动）。
        // 检索只使用本单稳定目标。此前记录和额外上下文本身可能含无关近期工作，
        // 不得反过来污染检索，把旧分支越搜越强。
        let memory = match &preflight {
            Some(plan) => preflight_memory_block(plan),
            None => String::new(),
        };
        let mem_dir = export_memory_files(app, emp).unwrap_or_default();
        let extra = format!(
            "{}{}{}{}",
            workflow_context(&workflow, emp, true),
            preflight_extra_block(preflight.as_ref()),
            preflight_fallback_note,
            extra
        );
        build_dev_prompt(
            app, emp, scope, key, title, prior_note, &memory, &mem_dir, &extra,
        )
    } else {
        let extra = format!("{}{}", workflow_context(&workflow, emp, false), extra);
        let extra = followup_extra_with_note(prior_note, &extra);
        build_dev_followup(app, emp, &task_title, &extra)
    };
    let prompt_images = if is_new { images } else { Vec::new() };
    run_prompt_for_images(
        &emp.agent_kind,
        app,
        thread_id.clone(),
        dev_prompt,
        prompt_images,
    )
    .await;
    // 用户手动停止：这一轮作废。绝不落账为「完成」，释放认领（回到「待处理」），且不写记忆。
    if thread_cancelled(app, &thread_id) {
        abort_on_stop(
            app,
            emp,
            scope,
            use_shared,
            key,
            task_id.as_deref(),
            &thread_id,
        )
        .await;
        clear_thread_cancelled(app, &thread_id);
        return;
    }
    let dev_out = extract_last_assistant(app, &thread_id)
        .unwrap_or_else(|| "（本轮没有产生文字总结）".to_string());

    // 若本棒是「答讨论」：用本轮产出作为意见回程，交还发起方（串行 wake-do）。
    if let Some(notice_id) = crate::notice::pending_discuss_id(app, &emp.id, scope, key) {
        let _ = crate::notice::respond_notice(
            app,
            &notice_id,
            crate::notice::ActorRef::employee(&emp.id, &emp.name),
            crate::notice::RespondParams {
                choice_id: None,
                text: Some(dev_out.clone()),
                reject: false,
            },
        );
        if let Some(tid) = &task_id {
            finish_task(
                app,
                tid,
                "done",
                Some(format!("已答复讨论并交还发起方。\n{dev_out}")),
            );
        }
        append_journal(
            app,
            &emp.id,
            key,
            "答复讨论",
            &format!("已答复并交还：{}", one_line(&dev_out, 200)),
        );
        let _ = app.emit(EV_MARKS, json!({}));
        let _ = app.emit(EV_DECISIONS, json!({}));
        let _ = app.emit(EV_EMPLOYEES, json!({}));
        reset_dev_rounds(scope, key);
        return;
    }

    let parsed_next = parse_next_action(&dev_out, &emp.id, scope, key, &task_title);
    if let Some(ParsedNextAction::Command(mut cmd)) = parsed_next {
        cmd.origin_thread_id = thread_id.clone();
        let _ = exec_inbox_command(app, cmd).await;
        let _ = app.emit(EV_MARKS, json!({}));
        let _ = app.emit(EV_DECISIONS, json!({}));
        let _ = app.emit(EV_EMPLOYEES, json!({}));
        let _ = app.emit(EV_TASKS, json!({}));
    }

    // 收尾/协作统一由 NEXT_ACTION 表达，应用侧解析后路由。
    // 这里保留旧工具/旧文本兜底：
    //   1) 先看账本是否已被路由改成终态（done/释放/转交）；
    //   2) 再兼容 DONE/WAITING/BLOCKED 文本；
    //   3) 否则续租留待下一轮，并用「连续 N 轮无终态自动释放」防死循环。

    // 本轮是否已上奏御书房候旨（relay decision 可能在会话中途就被收件箱落地）。
    // 候旨 ≠ 受阻：**绝不能释放认领**；单子保持在员工名下，批复后由注入+唤醒继续。
    let waiting = crate::notice::pending_hold_for(app, &emp.id, scope, key);

    // 1) 工具可能已把单子改成终态。
    let current_mark = ledger_marks(app, use_shared, scope)
        .await
        .into_iter()
        .find(|m| m.key == key);
    let status = current_mark
        .as_ref()
        .map(|m| m.status.clone())
        .unwrap_or_default();
    if status == "done" {
        commit_workflow_if_needed(app, emp, scope, key, &task_title, &dev_out);
        if let Some(tid) = &task_id {
            finish_task(app, tid, "done", Some(dev_out.clone()));
        }
        withdraw_decisions(app, scope, key);
        reset_dev_rounds(scope, key);
        return;
    }
    if status == "claimed"
        && current_mark
            .as_ref()
            .and_then(|m| m.owner.as_deref())
            .is_some_and(|owner| owner != emp.id)
    {
        if let Some(tid) = &task_id {
            finish_task(
                app,
                tid,
                "done",
                Some("已接力给其他员工，会话在对方名下继续。".into()),
            );
        }
        reset_dev_rounds(scope, key);
        return;
    }
    if status == "open" || status == "failed" {
        // blocked 工具已释放认领（回到「待处理」供他人接力）。冷却一下，别下一跳又自己捡回来。
        cooloff_add(scope, key, &emp.id);
        if let Some(tid) = &task_id {
            finish_task(app, tid, "blocked", Some(dev_out.clone()));
        }
        reset_dev_rounds(scope, key);
        return;
    }

    // 2) 已上奏候旨 → 保持认领挂起，等主管在御书房批复；清零无进展计数（候旨不算没进展，
    //    不能因为等主管等太久就被「防死循环」误释放）。以真实存在的候旨奏折为准；
    //    agent 只写 WAITING 却没真上奏的，走下面的正常多轮/防死循环链路（有界）。
    if waiting {
        mark_workflow_waiting(app, &emp.id, scope, key);
        reset_dev_round_counter(scope, key);
        let _ = ledger_claim_one(app, use_shared, scope, key, &task_title, emp).await;
        let _ = app.emit(EV_MARKS, json!({}));
        if let Some(tid) = &task_id {
            finish_task(app, tid, "done", Some(dev_out.clone()));
        }
        return;
    }

    // 3) 仍 claimed → 极薄文本兜底。
    if let Some(reason) = parse_blocked(&dev_out) {
        ledger_set_status(
            app,
            use_shared,
            scope,
            key,
            "open",
            Some(dev_out.clone()),
            true,
        )
        .await;
        cooloff_add(scope, key, &emp.id);
        let _ = app.emit(EV_MARKS, json!({}));
        if let Some(tid) = &task_id {
            finish_task(app, tid, "blocked", Some(reason.clone()));
        }
        append_event(
            app,
            &emp.id,
            key,
            &format!("受阻放下 {key}"),
            &format!("我受阻放下了这张单子，原因：{reason}"),
            "outcome:blocked",
        );
        reset_dev_rounds(scope, key);
        return;
    }
    if has_done_marker(&dev_out) {
        commit_workflow_if_needed(app, emp, scope, key, &task_title, &dev_out);
        ledger_set_status(
            app,
            use_shared,
            scope,
            key,
            "done",
            Some(dev_out.clone()),
            false,
        )
        .await;
        let _ = app.emit(EV_MARKS, json!({}));
        if let Some(tid) = &task_id {
            finish_task(app, tid, "done", Some(dev_out.clone()));
        }
        append_event(
            app,
            &emp.id,
            key,
            &format!("办成 {key}"),
            &one_line(&dev_out, 400),
            "outcome:done",
        );
        // 旧文本 DONE 兜底完成的，同样呈上完工汇报。
        // NEXT_ACTION done 或旧 done 工具会先改账本为 done，走上面的分支，不会重复汇报。
        file_report(
            app,
            emp,
            scope,
            key,
            &task_title,
            &dev_out,
            Some(thread_id.clone()),
        );
        withdraw_decisions(app, scope, key);
        reset_dev_rounds(scope, key);
        return;
    }

    // 4) 无明确终态：可能本轮还在多轮推进，或旧工具命令还没被后台处理。
    //    续租保持认领、下一轮继续；用无进展计数兜底防死循环。
    let rounds = bump_dev_rounds(scope, key);
    if rounds >= MAX_DEV_ROUNDS {
        ledger_set_status(
            app,
            use_shared,
            scope,
            key,
            "open",
            Some(format!(
                "连续 {rounds} 轮无明确进展，自动释放接力以防死循环。"
            )),
            true,
        )
        .await;
        cooloff_add(scope, key, &emp.id);
        let _ = app.emit(EV_MARKS, json!({}));
        if let Some(tid) = &task_id {
            finish_task(app, tid, "blocked", Some("连续多轮无进展，自动释放".into()));
        }
        append_event(
            app,
            &emp.id,
            key,
            &format!("停滞被释放 {key}"),
            &format!(
                "我在这张单子上连续 {rounds} 轮没有给出明确结论，被系统自动释放。值得复盘：是任务太大没拆解、方向不对，还是我忘了收尾（done/blocked/relay 三选一）。最后一轮输出摘要：{}",
                one_line(&dev_out, 200)
            ),
            "outcome:stalled",
        );
        reset_dev_rounds(scope, key);
        return;
    }
    // 续租，下一轮继续（保持认领）。
    let _ = ledger_claim_one(app, use_shared, scope, key, &task_title, emp).await;
    let _ = app.emit(EV_MARKS, json!({}));
    if let Some(tid) = &task_id {
        finish_task(app, tid, "done", Some(dev_out.clone()));
    }
}

/// 员工办结一张单子后向御书房递一份「完工汇报」Notice。
fn file_report(
    app: &AppHandle,
    emp: &Employee,
    scope: &str,
    key: &str,
    task_title: &str,
    summary: &str,
    thread_id: Option<String>,
) {
    crate::notice::emit_report(
        app,
        emp,
        scope,
        key,
        task_title,
        &one_line(summary, 400),
        thread_id,
    );
}

/// 单子完结时撤回其上全部候旨 Notice。
fn withdraw_decisions(app: &AppHandle, scope: &str, key: &str) {
    crate::notice::withdraw_notices(app, scope, key);
}

/// 读取某单子在账本里的当前状态（done/open/claimed/failed）；不存在返回空串。
async fn ledger_status(app: &AppHandle, use_shared: bool, scope: &str, key: &str) -> String {
    ledger_marks(app, use_shared, scope)
        .await
        .into_iter()
        .find(|m| m.key == key)
        .map(|m| m.status)
        .unwrap_or_default()
}

/// 极薄兜底：优先走 NEXT_ACTION / done 工具；兼容旧员工末尾写 DONE。
fn has_done_marker(text: &str) -> bool {
    text.lines().any(|line| {
        let line = line.trim();
        if line.eq_ignore_ascii_case("DONE") {
            return true;
        }
        line.get(..5)
            .is_some_and(|head| head.eq_ignore_ascii_case("DONE:"))
            && !line[5..].trim().is_empty()
    })
}

// ---- 讨论伙伴联动（同机直接跑 / 跨机经中转站）----

/// 执行开发输出里的 DISCUSS 指令：本机伙伴直接生成答复并记入双方记忆；跨机伙伴经中转站投递。
/// 旧本地同步讨论（已由 Notice 串行交棒取代）；跨机路径仍可能参考。
#[allow(dead_code)]
async fn exec_discuss(
    app: &AppHandle,
    from: &Employee,
    scope: &str,
    key: &str,
    items: Vec<(String, String, String)>,
) {
    for (name, topic, body) in items {
        // 本机同名员工 → 本地伙伴联动
        let local = {
            let st = app.state::<AppState>();
            let emps = st.employees.lock().unwrap();
            emps.employees
                .iter()
                .find(|e| e.name == name && e.id != from.id)
                .cloned()
        };
        if let Some(peer_emp) = local {
            let reply = run_partner_reply(app, &from.name, &peer_emp, &topic, &body).await;
            append_journal(
                app,
                &peer_emp.id,
                key,
                &format!("与 {} 的协议约定", from.name),
                &format!("主题：{topic}\n对方诉求：{body}\n我的答复：{reply}"),
            );
            append_journal(
                app,
                &from.id,
                key,
                &format!("与 {} 的协议约定", name),
                &format!("主题：{topic}\n我的诉求：{body}\n{name} 答复：{reply}"),
            );
            continue;
        }
        // 跨机伙伴 → 按 partners 里 remote 配置定向投递
        let partner = from
            .partners
            .iter()
            .find(|p| p.kind == "remote" && p.name == name)
            .cloned();
        if let Some(p) = partner {
            let peer_name = p.peer.clone().unwrap_or_default();
            match resolve_peer_token(app, &peer_name) {
                Some(tok) => {
                    let relay = app.state::<AppState>().relay.clone();
                    relay.spawn_send(
                        tok,
                        "employee.discuss",
                        json!({
                            "fromEmp": from.name,
                            "fromScope": scope,
                            "fromKey": key,
                            "toEmp": name,
                            "topic": topic,
                            "body": body,
                            "corrId": uuid::Uuid::new_v4().to_string(),
                        }),
                    );
                    append_journal(
                        app,
                        &from.id,
                        key,
                        &format!("向队友 {peer_name} 的 {name} 发起约定"),
                        &format!("主题：{topic}\n内容：{body}\n（已投递，回复后下一轮继续）"),
                    );
                }
                None => append_journal(
                    app,
                    &from.id,
                    key,
                    "跨机讨论未送达",
                    &format!("找不到在线队友「{peer_name}」，无法联系其员工 {name}。"),
                ),
            }
        } else {
            append_journal(
                app,
                &from.id,
                key,
                "讨论对象未配置",
                &format!("想找「{name}」讨论，但未在讨论伙伴里配置该对象。"),
            );
        }
    }
    let _ = app.emit(EV_EMPLOYEES, json!({}));
}

/// 以被咨询员工的身份跑一轮「回复」，返回其答复文本。
async fn run_partner_reply(
    app: &AppHandle,
    from_name: &str,
    peer: &Employee,
    topic: &str,
    body: &str,
) -> String {
    if !std::path::Path::new(&peer.cwd).is_dir() {
        return format!("（{} 的工作目录不可用，未能给出答复）", peer.name);
    }
    let thread_id = new_thread(
        app,
        peer,
        &format!("[{}] 回复 {} 的约定", peer.name, from_name),
    );
    let memory = retrieve_memory(app, &peer.id, &format!("{topic} {body}"), None).await;
    let prompt = build_reply_prompt(peer, from_name, topic, body, &memory);
    run_prompt_for(&peer.agent_kind, app, thread_id.clone(), prompt).await;
    extract_last_assistant(app, &thread_id)
        .unwrap_or_else(|| "（对方没有给出明确答复）".to_string())
}

/// 按队友展示名解析其中转站 token（用于定向投递）。
#[allow(dead_code)]
fn resolve_peer_token(app: &AppHandle, peer_name: &str) -> Option<String> {
    if peer_name.trim().is_empty() {
        return None;
    }
    let relay = app.state::<AppState>().relay.clone();
    let peers = relay.peers();
    let arr = peers.as_array()?;
    for p in arr {
        let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("");
        if name == peer_name {
            return p
                .get("token")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
        }
    }
    None
}

/// 收到队友员工的讨论请求：本机按 toEmp 找员工生成答复，回发 employee.discuss.reply。
pub fn on_remote_discuss(
    app: &AppHandle,
    from_token: &str,
    from_peer_name: &str,
    data: serde_json::Value,
) {
    let app = app.clone();
    let from_token = from_token.to_string();
    let from_peer_name = from_peer_name.to_string();
    tauri::async_runtime::spawn(async move {
        let to_emp = data
            .get("toEmp")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let from_emp = data
            .get("fromEmp")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let topic = data
            .get("topic")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let body = data
            .get("body")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let corr = data
            .get("corrId")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let from_key = data
            .get("fromKey")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let from_scope = data
            .get("fromScope")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let peer = {
            let st = app.state::<AppState>();
            let emps = st.employees.lock().unwrap();
            emps.employees.iter().find(|e| e.name == to_emp).cloned()
        };
        let Some(peer) = peer else { return };
        let reply = run_partner_reply(&app, &from_emp, &peer, &topic, &body).await;
        append_journal(
            &app,
            &peer.id,
            &from_key,
            &format!("回复队友 {from_peer_name} 的 {from_emp}"),
            &format!("主题：{topic}\n对方诉求：{body}\n我的答复：{reply}"),
        );
        let relay = app.state::<AppState>().relay.clone();
        relay.spawn_send(
            from_token,
            "employee.discuss.reply",
            json!({
                "fromEmp": to_emp,
                "toEmp": from_emp,
                "toScope": from_scope,
                "toKey": from_key,
                "topic": topic,
                "reply": reply,
                "corrId": corr,
            }),
        );
        let _ = app.emit(EV_EMPLOYEES, json!({}));
    });
}

/// 收到队友对我方发起讨论的回复：记入发起员工记忆，下一轮开发自动带上。
pub fn on_remote_discuss_reply(app: &AppHandle, data: serde_json::Value) {
    let to_emp = data
        .get("toEmp")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let from_emp = data
        .get("fromEmp")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let topic = data
        .get("topic")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let reply = data
        .get("reply")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let key = data
        .get("toKey")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let emp = {
        let st = app.state::<AppState>();
        let emps = st.employees.lock().unwrap();
        emps.employees.iter().find(|e| e.name == to_emp).cloned()
    };
    let Some(emp) = emp else { return };
    append_journal(
        app,
        &emp.id,
        &key,
        &format!("队友 {from_emp} 的约定回复"),
        &format!("主题：{topic}\n{from_emp} 答复：{reply}"),
    );
    let _ = app.emit(EV_EMPLOYEES, json!({}));
}

/// 把中转站返回的账本条目 JSON 反序列化为 Mark 列表。
fn marks_from_values(vals: Vec<serde_json::Value>) -> Vec<Mark> {
    vals.into_iter()
        .filter_map(|v| serde_json::from_value(v).ok())
        .map(restore_mark_attachments)
        .collect()
}

/// 把中转站返回的 outcome 字符串映射为 ClaimOutcome。
fn parse_outcome(s: &str) -> ClaimOutcome {
    match s {
        "taken" => ClaimOutcome::Taken,
        "done" => ClaimOutcome::Done,
        "failed" => ClaimOutcome::Failed,
        _ => ClaimOutcome::Acquired,
    }
}

/// 新建一个正式会话（前端会话列表可见，可点开查看员工干活）。
fn new_thread(app: &AppHandle, emp: &Employee, title: &str) -> String {
    new_thread_with_model(app, emp, title, emp.model.clone())
}

/// 与 new_thread 相同，但可指定本会话使用的模型（用于巡查/开发各配不同模型）。
fn new_thread_with_model(
    app: &AppHandle,
    emp: &Employee,
    title: &str,
    model: Option<String>,
) -> String {
    new_thread_full(
        app,
        emp,
        title,
        emp.cwd.clone(),
        emp.agent_kind.clone(),
        model,
        emp.mode.clone(),
        false,
    )
}

/// 开发会话：在工作流指定目录里开一个会话（用工作模型/模式）。
fn new_dev_thread(
    app: &AppHandle,
    emp: &Employee,
    title: &str,
    workflow: &Workflow,
    parent_thread_id: Option<&str>,
) -> String {
    let id = new_thread_full(
        app,
        emp,
        title,
        workflow.cwd.clone(),
        emp.agent_kind.clone(),
        emp.model.clone(),
        emp.mode.clone(),
        false,
    );
    if let Some(parent_id) = parent_thread_id.filter(|s| !s.trim().is_empty()) {
        let state = app.state::<AppState>();
        let mut store = state.store.lock().unwrap();
        if let Some(thread) = store.get_mut(&id) {
            thread.parent_thread_id = Some(parent_id.to_string());
        }
        store.save();
        let _ = app.emit(crate::acp::EV_THREADS, json!({}));
    }
    if workflow.use_worktree {
        let item = {
            let state = app.state::<AppState>();
            let mut store = state.store.lock().unwrap();
            let it = store.get_mut(&id).map(|t| {
                t.worktree = Some(Worktree {
                    repo: workflow.repo.clone(),
                    path: workflow.cwd.clone(),
                    branch: workflow.branch.clone(),
                });
                t.push_system(
                    format!(
                        "工作流分支：{}（基于 {}）",
                        workflow.branch,
                        if workflow.base_branch.is_empty() {
                            "当前分支"
                        } else {
                            &workflow.base_branch
                        }
                    ),
                    "info",
                )
            });
            store.save();
            it
        };
        if let Some(it) = item {
            let _ = app.emit(
                crate::acp::EV_UPDATE,
                json!({ "threadId": id, "op": { "t": "upsert", "item": it } }),
            );
        }
        let _ = app.emit(crate::acp::EV_THREADS, json!({}));
    }
    attach_workflow_thread(app, workflow, &id);
    id
}

/// 完全指定 目录 + 后端 + 模型 + 模式地新建员工会话（巡查可用与工作不同的后端；开发可用 worktree 目录）。
fn new_thread_full(
    app: &AppHandle,
    emp: &Employee,
    title: &str,
    cwd: String,
    agent_kind: AgentKind,
    model: Option<String>,
    mode: Option<String>,
    mind_thread: bool,
) -> String {
    let mut thread = Thread::new(cwd, agent_kind, model, mode, None, false);
    thread.title = title.to_string();
    thread.employee_id = Some(emp.id.clone());
    thread.mind_thread = mind_thread;
    let id = thread.id.clone();
    {
        let state = app.state::<AppState>();
        let mut store = state.store.lock().unwrap();
        store.threads.push(thread);
        store.save();
    }
    let _ = app.emit(crate::acp::EV_THREADS, json!({}));
    id
}

fn append_necessary_lesson(app: &AppHandle, emp: &Employee, key: &str, title: &str, lesson: &str) {
    let exists = {
        let state = app.state::<AppState>();
        let mem = state.memory.lock().unwrap();
        mem.all(&emp.id)
            .iter()
            .any(|e| e.kind == "lesson" && e.summary == lesson)
    };
    if exists {
        return;
    }
    append_journal_kind(app, &emp.id, key, title, lesson, false, "lesson", 1);
}

/// 每次 Do 前的 Wake：小模型快速路由 + 决定工作区（含是否留在源分支）。
/// `chain_parent`：本任务链的父会话；新 Wake 挂在其下，便于同一链归入一个父会话。
async fn run_wake(
    app: &AppHandle,
    emp: &Employee,
    scope: &str,
    key: &str,
    title: &str,
    prior_note: &str,
    extra: &str,
    chain_parent: Option<&str>,
) -> WakeRun {
    if !crate::gitwt::is_repo(&emp.cwd) {
        return WakeRun::Skipped;
    }
    if crate::gitwt::repo_root(&emp.cwd).is_err() {
        return WakeRun::Skipped;
    };
    let has_edict = supervision_forces_do(prior_note, extra);
    let prompt = build_wake_prompt(app, emp, scope, key, title, prior_note, extra);
    let wake_title = if has_edict {
        format!("[{}] Wake · 领旨", emp.name)
    } else {
        format!("[{}] Wake", emp.name)
    };
    // 父会话：已有链根则挂其下；本 Wake 若成为上奏/交棒锚点，后续轮次会继续挂在同一根下。
    let parent = chain_parent.filter(|p| !p.trim().is_empty() && thread_alive(app, p));
    let thread_id = new_mind_thread_with_parent(app, emp, &wake_title, false, parent);
    let kind = mind_agent_kind(emp);
    run_prompt_for(&kind, app, thread_id.clone(), prompt).await;
    if thread_cancelled(app, &thread_id) {
        clear_thread_cancelled(app, &thread_id);
        let _ = app
            .state::<AppState>()
            .employee_stop_reasons
            .lock()
            .unwrap()
            .remove(&thread_id);
        append_thread_system(
            app,
            &thread_id,
            "【Wake 终止】用户已终止 Wake；系统不会继续开启开发会话。",
            "warn",
        );
        return WakeRun::Cancelled { thread_id };
    }
    if thread_has_error(app, &thread_id) {
        rename_thread(app, &thread_id, &format!("[{}] Wake · 出错", emp.name));
        return WakeRun::Skipped;
    }
    let output = extract_last_assistant(app, &thread_id).unwrap_or_default();

    // 兼容旧 NEXT_ACTION escalate
    if let Some(ParsedNextAction::Command(mut cmd)) =
        parse_next_action(&output, &emp.id, scope, key, title)
    {
        if cmd.kind == "relay" && cmd.relay_kind == "decision" {
            cmd.origin_thread_id = thread_id.clone();
            cmd.decision_source = "wake".to_string();
            let _ = exec_inbox_command(app, cmd).await;
            rename_thread(app, &thread_id, &format!("[{}] Wake · 候旨", emp.name));
            return WakeRun::Escalated { thread_id };
        }
    }

    let Some(decision) = parse_wake_decision(&output) else {
        append_thread_system(
            app,
            &thread_id,
            "【Wake 交还】未返回有效结构化结果，系统将按当前分支/当前目录继续 Do。",
            "warn",
        );
        return WakeRun::Do {
            thread_id,
            plan: WorkPreflight {
                mode: "current".into(),
                summary: "Wake 解析失败，降级 current".into(),
                ..Default::default()
            },
        };
    };

    let intent = decision.intent_key().to_lowercase();
    match intent.as_str() {
        "escalate" | "decision" | "ask_user" | "上奏" | "请示" => {
            let mut cmd = InboxCommand {
                kind: "relay".into(),
                from: emp.id.clone(),
                scope: scope.to_string(),
                key: key.to_string(),
                title: title.to_string(),
                to: "user".into(),
                relay_kind: "decision".into(),
                brief: if decision.brief.trim().is_empty() {
                    decision.reason.clone()
                } else {
                    decision.brief.clone()
                },
                question: if decision.question.trim().is_empty() {
                    decision.reason.clone()
                } else {
                    decision.question.clone()
                },
                options: decision.options.clone(),
                category: if decision.category.trim().is_empty() {
                    "other".into()
                } else {
                    decision.category.clone()
                },
                origin_thread_id: thread_id.clone(),
                decision_source: "wake".into(),
                at: now_ms(),
                ..Default::default()
            };
            if cmd.question.trim().is_empty() {
                cmd.question = format!("是否继续推进：{}？", task_title_of(key, title));
            }
            let _ = exec_inbox_command(app, cmd).await;
            rename_thread(app, &thread_id, &format!("[{}] Wake · 候旨", emp.name));
            WakeRun::Escalated { thread_id }
        }
        "discuss" | "discussion" | "讨论" | "handoff" | "relay" | "work" | "transfer" | "接力"
        | "转交" | "done" | "complete" | "完成" | "release" | "blocked" | "block" | "释放"
        | "受阻" => {
            let relay_kind = match intent.as_str() {
                "discuss" | "discussion" | "讨论" => "discuss",
                "done" | "complete" | "完成" => {
                    // 有主管批示要求继续时，禁止在 Wake 阶段办结（否则无 Do、且像用 Wake 模型干活）
                    if supervision_forces_do(prior_note, extra) {
                        append_thread_system(
                            app,
                            &thread_id,
                            "【Wake】已有主管批示要求继续推进，不能在 Wake 直接办结；进入 Do（工作模型）。",
                            "warn",
                        );
                        rename_thread(app, &thread_id, &format!("[{}] Wake", emp.name));
                        return WakeRun::Do {
                            thread_id,
                            plan: decision.workspace_plan(),
                        };
                    }
                    let cmd = InboxCommand {
                        kind: "done".into(),
                        from: emp.id.clone(),
                        scope: scope.to_string(),
                        key: key.to_string(),
                        title: title.to_string(),
                        summary: if decision.brief.trim().is_empty() {
                            decision.reason.clone()
                        } else {
                            decision.brief.clone()
                        },
                        origin_thread_id: thread_id.clone(),
                        at: now_ms(),
                        ..Default::default()
                    };
                    let _ = exec_inbox_command(app, cmd).await;
                    rename_thread(app, &thread_id, &format!("[{}] Wake · 办结", emp.name));
                    return WakeRun::Routed { thread_id };
                }
                "release" | "blocked" | "block" | "释放" | "受阻" => {
                    let cmd = InboxCommand {
                        kind: "blocked".into(),
                        from: emp.id.clone(),
                        scope: scope.to_string(),
                        key: key.to_string(),
                        title: title.to_string(),
                        reason: if decision.brief.trim().is_empty() {
                            decision.reason.clone()
                        } else {
                            decision.brief.clone()
                        },
                        origin_thread_id: thread_id.clone(),
                        at: now_ms(),
                        ..Default::default()
                    };
                    let _ = exec_inbox_command(app, cmd).await;
                    rename_thread(app, &thread_id, &format!("[{}] Wake · 释放", emp.name));
                    return WakeRun::Routed { thread_id };
                }
                _ => "work",
            };
            let to = decision.to.trim();
            if to.is_empty() {
                append_thread_system(
                    app,
                    &thread_id,
                    "【Wake】协作意图缺少 to，已降级为进入 Do。",
                    "warn",
                );
                rename_thread(app, &thread_id, &format!("[{}] Wake", emp.name));
                return WakeRun::Do {
                    thread_id,
                    plan: decision.workspace_plan(),
                };
            }
            // to 必须真实存在：校验失败则拒绝 discuss/handoff，改上奏用户（带着原问题）
            if !to.eq_ignore_ascii_case("self") && !to.eq_ignore_ascii_case("user") {
                if let Err(e) = resolve_relay_target(app, emp, to) {
                    append_thread_system(
                        app,
                        &thread_id,
                        &format!(
                            "【Wake】to 必须是本机已启用员工。{e}\n已改为上奏【用户】，请主管直接答复。"
                        ),
                        "warn",
                    );
                    let mut cmd = InboxCommand {
                        kind: "relay".into(),
                        from: emp.id.clone(),
                        scope: scope.to_string(),
                        key: key.to_string(),
                        title: title.to_string(),
                        to: "user".into(),
                        relay_kind: "decision".into(),
                        brief: format!(
                            "原 to「{to}」非法，无法交棒。\n{}",
                            if decision.brief.trim().is_empty() {
                                decision.reason.clone()
                            } else {
                                decision.brief.clone()
                            }
                        ),
                        question: if decision.question.trim().is_empty() {
                            decision.reason.clone()
                        } else {
                            decision.question.clone()
                        },
                        options: decision.options.clone(),
                        category: "input".into(),
                        origin_thread_id: thread_id.clone(),
                        decision_source: "wake".into(),
                        at: now_ms(),
                        ..Default::default()
                    };
                    if cmd.question.trim().is_empty() {
                        cmd.question = format!("（原 to「{to}」不存在）请协助判断如何推进。");
                    }
                    let _ = exec_inbox_command(app, cmd).await;
                    rename_thread(app, &thread_id, &format!("[{}] Wake · 候旨", emp.name));
                    return WakeRun::Escalated { thread_id };
                }
            }
            let cmd = InboxCommand {
                kind: "relay".into(),
                from: emp.id.clone(),
                scope: scope.to_string(),
                key: key.to_string(),
                title: title.to_string(),
                to: to.to_string(),
                relay_kind: relay_kind.into(),
                brief: if decision.brief.trim().is_empty() {
                    decision.reason.clone()
                } else {
                    decision.brief.clone()
                },
                question: decision.question.clone(),
                origin_thread_id: thread_id.clone(),
                at: now_ms(),
                ..Default::default()
            };
            match exec_inbox_command(app, cmd).await {
                Ok(()) => {
                    rename_thread(
                        app,
                        &thread_id,
                        &format!("[{}] Wake · {}", emp.name, relay_kind),
                    );
                    append_thread_system(
                        app,
                        &thread_id,
                        &format!(
                            "【Wake 交棒】已向「{to}」发出 {relay_kind} Notice，等待对方 wake-do 后再继续。"
                        ),
                        "info",
                    );
                    WakeRun::Routed { thread_id }
                }
                Err(e) => {
                    append_thread_system(
                        app,
                        &thread_id,
                        &format!("【Wake 交棒失败】{e}。本轮改为进入 Do，请自行处理或上奏。"),
                        "warn",
                    );
                    rename_thread(app, &thread_id, &format!("[{}] Wake", emp.name));
                    WakeRun::Do {
                        thread_id,
                        plan: decision.workspace_plan(),
                    }
                }
            }
        }
        _ => {
            // do / continue / 空 → 进入 Do，workspace 由 Wake 决定（含 current=源分支）
            let plan = decision.workspace_plan();
            rename_thread(app, &thread_id, &format!("[{}] Wake", emp.name));
            WakeRun::Do { thread_id, plan }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn build_wake_prompt(
    app: &AppHandle,
    emp: &Employee,
    scope: &str,
    key: &str,
    title: &str,
    prior_note: &str,
    extra: &str,
) -> String {
    let tools = tools_manual(app, emp, scope, key, &["kb-search", "ledger-list"]);
    let peers = {
        let names = list_relay_peer_names(app, false);
        if names.is_empty() {
            "（无已启用同事；discuss/handoff 只能 to=self，或改用 escalate）".into()
        } else {
            names.join("、")
        }
    };
    let worktree_rule = if emp.allow_worktree {
        "允许选择 worktree；只有任务明显需要隔离长期改动或避免污染当前分支时才选。"
    } else {
        "配置不允许 worktree；mode 不要输出 worktree。"
    };
    let (supervision_block, prior_disp, extra_disp) = wake_context_blocks(prior_note, extra);
    format!(
        "你是数字员工「{name}」的 Wake。你只做开干前快速思考与路由。\n\
**严禁改代码、写文件、跑写入命令、提交、安装依赖或任何会改仓库的操作。** 只允许只读检索。\n\n\
【任务】[{key}] {title}\nscope：{scope}\n\
{supervision}\
此前进展：{prior}\n补充上下文：{extra}\n\n\
【岗位说明】\n{charter}\n\n\
【本机可交棒同事】{peers}\n\
（to 只能选自上表或 self；禁止虚构人名。找不到人时系统会改上奏御书房。）\n\n\
{tools}\
请先用上面的只读工具与仓库只读核查当前状态、近期工作、记忆知识和账本占用。\n\
若下一步是 do：必须主动检索本地分支并给出 workspace；真正干活由后续 **Do 会话（工作模型）** 完成，不在本 Wake 会话里干。\n\
{worktree_rule}\n\n\
只输出一个 JSON 块：\n\
===WAKE_JSON===\n\
{{\n\
  \"intent\":\"do|discuss|handoff|escalate|done|release\",\n\
  \"reason\":\"一句话理由\",\n\
  \"brief\":\"交给对方或办结的说明\",\n\
  \"to\":\"同事名|self|（仅 discuss/handoff 需要；必须真实存在）\",\n\
  \"question\":\"上奏或讨论的问题\",\n\
  \"options\":[],\n\
  \"workspace\":{{\n\
    \"mode\":\"current|existingBranch|newBranch|worktree\",\n\
    \"branch\":\"\",\n\
    \"baseBranch\":\"\",\n\
    \"branchCandidates\":[],\n\
    \"support\":[],\n\
    \"focusFiles\":[],\n\
    \"commands\":[],\n\
    \"risks\":[],\n\
    \"reason\":\"为什么这样选分支\",\n\
    \"summary\":\"一句话工作区结论\"\n\
  }}\n\
}}\n\
===END===\n\n\
规则（按优先级）：\n\
0. **若有【主管批注】**：必须先读懂批注意图再路由；默认 intent=do，把执行留给 Do；禁止无视批注；禁止在 Wake 里动手；禁止用 done 敷衍。\n\
1. **默认 intent=do**：自己能查、能改、能验证的，输出 do + workspace，把活留给 Do 会话；不要在 Wake 里动手。\n\
2. **intent=escalate**：需要【用户】拍板（外部服务/权限/配置归属/重大取舍）。不要虚构同事 discuss。\n\
3. **intent=discuss 极少用**：仅当必须另一名已启用数字员工专长，且 to 在列表中。\n\
4. intent=handoff：整单转交同事。\n\
5. **intent=done 极严**：仅当单子已真正完成或确认无需再做；若上下文含主管批示/留中要求继续，必须输出 do，禁止 done。\n\
6. intent=release：确实受阻才释放。\n\
7. workspace.mode=current 表示留在源/当前分支。\n\
也兼容旧格式 ===PREFLIGHT_JSON===（视为 intent=do）。",
        name = emp.name,
        key = key,
        title = title.trim(),
        scope = scope,
        supervision = supervision_block,
        prior = prior_disp,
        extra = extra_disp,
        charter = charter_or_default(&emp.charter),
        peers = peers,
        tools = tools,
        worktree_rule = worktree_rule,
    )
}

/// 把主管批注从上下文里抽成醒目块，供 Wake 阅读后再决定，而不是无脑 do。
fn wake_context_blocks(prior_note: &str, extra: &str) -> (String, String, String) {
    let prior = prior_note.trim();
    let extra = extra.trim();
    if !supervision_forces_do(prior, extra) {
        return (
            String::new(),
            if prior.is_empty() {
                "（无）".into()
            } else {
                prior.to_string()
            },
            if extra.is_empty() {
                "（无）".into()
            } else {
                extra.to_string()
            },
        );
    }
    let mut edict = String::new();
    if extra.contains("主管已在御书房")
        || extra.contains("主管批示")
        || extra.contains("留中不发")
        || extra.contains("遵照批示")
    {
        edict.push_str(extra);
    } else if prior.contains("主管批示") || prior.contains("留中不发") || prior.contains("遵照批示")
    {
        edict.push_str(prior);
    } else {
        if !prior.is_empty() {
            edict.push_str(prior);
        }
        if !extra.is_empty() {
            if !edict.is_empty() {
                edict.push('\n');
            }
            edict.push_str(extra);
        }
    }
    let block = format!(
        "【主管批注——本轮 Wake 必须据此路由】\n{edict}\n\
说明：这是御书房批示后的再唤醒。先理解批注要你做什么/不做什么，再输出 intent（通常为 do）与 workspace；真正执行在后续 Do 会话。禁止跳过批注、禁止在本会话改代码。\n\n"
    );
    let prior_disp = if prior.is_empty() || edict.contains(prior) {
        "（见上方主管批注 / 无额外进展）".into()
    } else {
        prior.to_string()
    };
    let extra_disp = if extra.is_empty() || edict.contains(extra) {
        "（已并入上方主管批注）".into()
    } else {
        extra.to_string()
    };
    (block, prior_disp, extra_disp)
}

fn parse_wake_decision(text: &str) -> Option<WakeDecision> {
    if let Some(start) = text.rfind("===WAKE_JSON===") {
        let rest = &text[start + "===WAKE_JSON===".len()..];
        if let Some(end) = rest.find("===END===") {
            if let Some(parsed) = parse_wake_decision_json(&rest[..end]) {
                return Some(parsed);
            }
        }
    }
    if let Some(start) = text.rfind("===PREFLIGHT_JSON===") {
        let rest = &text[start + "===PREFLIGHT_JSON===".len()..];
        if let Some(end) = rest.find("===END===") {
            if let Some(plan) = parse_work_preflight_json(&rest[..end]) {
                return Some(WakeDecision {
                    intent: "do".into(),
                    workspace: Some(plan.clone()),
                    mode: plan.mode,
                    branch: plan.branch,
                    base_branch: plan.base_branch,
                    branch_candidates: plan.branch_candidates,
                    support: plan.support,
                    focus_files: plan.focus_files,
                    commands: plan.commands,
                    risks: plan.risks,
                    reason: plan.reason,
                    summary: plan.summary,
                    ..Default::default()
                });
            }
        }
    }
    parse_wake_decision_json(text).or_else(|| {
        let plan = parse_work_preflight_json(text)?;
        Some(WakeDecision {
            intent: "do".into(),
            workspace: Some(plan.clone()),
            mode: plan.mode,
            branch: plan.branch,
            base_branch: plan.base_branch,
            branch_candidates: plan.branch_candidates,
            support: plan.support,
            focus_files: plan.focus_files,
            commands: plan.commands,
            risks: plan.risks,
            reason: plan.reason,
            summary: plan.summary,
            ..Default::default()
        })
    })
}

fn parse_wake_decision_json(text: &str) -> Option<WakeDecision> {
    let raw = text
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    serde_json::from_str(raw).ok().or_else(|| {
        let start = raw.find('{')?;
        let end = raw.rfind('}')?;
        if end <= start {
            return None;
        }
        serde_json::from_str(raw[start..=end].trim()).ok()
    })
}

#[cfg(test)]
fn parse_work_preflight(text: &str) -> Option<WorkPreflight> {
    parse_wake_decision(text).map(|d| d.workspace_plan())
}

fn parse_work_preflight_json(text: &str) -> Option<WorkPreflight> {
    let raw = text
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    serde_json::from_str(raw).ok().or_else(|| {
        let start = raw.find('{')?;
        let end = raw.rfind('}')?;
        if end <= start {
            return None;
        }
        serde_json::from_str(raw[start..=end].trim()).ok()
    })
}

fn preflight_memory_block(preflight: &WorkPreflight) -> String {
    let mut out = String::new();
    if !preflight.summary.trim().is_empty() {
        out.push_str("【Wake 开工预检摘要】\n");
        out.push_str("- ");
        out.push_str(preflight.summary.trim());
        out.push('\n');
    }
    out.push_str("\n【Wake 预检执行位置建议】\n");
    out.push_str(&format!(
        "- mode={} branch={} baseBranch={}\n",
        if preflight.mode.trim().is_empty() {
            "current"
        } else {
            preflight.mode.trim()
        },
        if preflight.branch.trim().is_empty() {
            "（空）"
        } else {
            preflight.branch.trim()
        },
        if preflight.base_branch.trim().is_empty() {
            "（当前 HEAD/当前分支）"
        } else {
            preflight.base_branch.trim()
        },
    ));
    if !preflight.branch_candidates.is_empty() {
        out.push_str("\n【相关分支候选】\n");
        for candidate in preflight.branch_candidates.iter().take(5) {
            out.push_str(&format!("- {}\n", one_line(candidate, 220)));
        }
    }
    if !preflight.support.is_empty() {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str("【本轮真正需要的支持信息】\n");
        for s in preflight.support.iter().take(5) {
            out.push_str(&format!("- {}\n", one_line(s, 260)));
        }
    }
    if out.trim().is_empty() {
        "（Wake 开工预检未找到必须注入的历史支持；以当前仓库事实为准）".to_string()
    } else {
        out.trim_end().to_string()
    }
}

fn preflight_extra_block(preflight: Option<&WorkPreflight>) -> String {
    let Some(p) = preflight else {
        return String::new();
    };
    let mut out = String::new();
    if !p.reason.trim().is_empty() {
        out.push_str("\n\n【Wake 预检选择理由】\n");
        out.push_str(p.reason.trim());
    }
    out.push_str("\n\n【Wake 预检原始结构化结果】\n");
    out.push_str(&format!(
        "mode: {}\nbranch: {}\nbaseBranch: {}\n",
        if p.mode.trim().is_empty() {
            "current"
        } else {
            p.mode.trim()
        },
        if p.branch.trim().is_empty() {
            "（空）"
        } else {
            p.branch.trim()
        },
        if p.base_branch.trim().is_empty() {
            "（当前 HEAD/当前分支）"
        } else {
            p.base_branch.trim()
        },
    ));
    if !p.branch_candidates.is_empty() {
        out.push_str("\n【检索到的分支候选】\n");
        for candidate in p.branch_candidates.iter().take(5) {
            out.push_str(&format!("- {}\n", one_line(candidate, 220)));
        }
    }
    if !p.focus_files.is_empty() {
        out.push_str("\n\n【建议优先查看】\n");
        for f in p.focus_files.iter().take(6) {
            out.push_str(&format!("- {}\n", one_line(f, 180)));
        }
    }
    if !p.commands.is_empty() {
        out.push_str("\n【建议先做的只读检查】\n");
        for c in p.commands.iter().take(4) {
            out.push_str(&format!("- {}\n", one_line(c, 220)));
        }
    }
    if !p.risks.is_empty() {
        out.push_str("\n【预检风险/不确定点】\n");
        for r in p.risks.iter().take(4) {
            out.push_str(&format!("- {}\n", one_line(r, 220)));
        }
    }
    out
}

fn record_preflight_prepare_result(
    app: &AppHandle,
    preflight_thread_id: Option<&str>,
    workflow: &Workflow,
    fallback_note: &str,
) {
    let Some(thread_id) = preflight_thread_id else {
        return;
    };
    let mode = if workflow.use_worktree {
        "独立 worktree"
    } else if workflow.use_current_branch {
        "当前分支"
    } else {
        "原地分支"
    };
    let base = if workflow.base_branch.trim().is_empty() {
        "当前 HEAD/当前分支"
    } else {
        workflow.base_branch.trim()
    };
    let mut text = format!(
        "【Wake 交还】分支/工作目录已按 Wake 决策准备好，接下来会开启员工开发子会话。\n模式：{mode}\n仓库：{}\n工作目录：{}\n分支：{}\n基准：{}",
        workflow.repo, workflow.cwd, workflow.branch, base
    );
    if !fallback_note.trim().is_empty() {
        text.push_str("\n\n注意：Wake 的原始执行位置建议未能自动应用，已降级到上面的执行位置；详细原因会交给开发子会话。");
    }
    append_thread_system(app, thread_id, &text, "info");
}

fn record_preflight_prepare_failure(
    app: &AppHandle,
    preflight_thread_id: Option<&str>,
    error: &str,
) {
    let Some(thread_id) = preflight_thread_id else {
        return;
    };
    append_thread_system(
        app,
        thread_id,
        &format!(
            "【Wake 交还】分支/工作目录准备失败，未进入御书房；这张单已退回待处理，等待下一轮员工自行重试或处理仓库状态。\n原因：{}",
            error
        ),
        "warn",
    );
}

fn append_thread_system(app: &AppHandle, thread_id: &str, text: &str, level: &str) {
    let item = {
        let state = app.state::<AppState>();
        let mut store = state.store.lock().unwrap();
        let item = store
            .get_mut(thread_id)
            .map(|thread| thread.push_system(text.to_string(), level));
        store.save();
        item
    };
    if let Some(item) = item {
        let _ = app.emit(
            crate::acp::EV_UPDATE,
            json!({ "threadId": thread_id, "op": { "t": "upsert", "item": item } }),
        );
    }
    let _ = app.emit(crate::acp::EV_THREADS, json!({}));
}

/// 按 Wake 给出的工作区策略准备执行位置（不持久化 Workflow）。
/// mode=current：留在源/当前分支；已满足则等价 no-op。
fn apply_workspace(
    app: &AppHandle,
    emp: &Employee,
    scope: &str,
    key: &str,
    title: &str,
    preflight: Option<&WorkPreflight>,
) -> Result<WorkspaceReady, String> {
    if !crate::gitwt::is_repo(&emp.cwd) {
        return Err(format!("工作目录不是 git 仓库：{}", emp.cwd));
    }
    let repo = crate::gitwt::repo_root(&emp.cwd)?;
    let plan = normalize_preflight_plan(preflight, emp, key);
    if plan.mode == "worktree" && emp.allow_worktree {
        let state = app.state::<AppState>();
        let wt = crate::create_worktree_for(
            state.inner(),
            &repo,
            Some(plan.branch.as_str()),
            Some(plan.base_branch.as_str()),
            None,
            false,
        )?;
        let workspace = Workflow {
            id: uuid::Uuid::new_v4().to_string(),
            employee_id: emp.id.clone(),
            scope: scope.to_string(),
            key: key.to_string(),
            title: task_title_of(key, title),
            repo: wt.repo,
            cwd: wt.path,
            branch: wt.branch,
            base_branch: plan.base_branch,
            use_worktree: true,
            use_current_branch: false,
            thread_id: None,
            status: "active".into(),
            created_at: now_ms(),
            updated_at: now_ms(),
        };
        return Ok(WorkspaceReady {
            workspace,
            _repo_lock: None,
        });
    }
    let lock = try_lock_repo(&repo).ok_or_else(|| format!("仓库正在被另一名员工使用：{repo}"))?;
    let current = current_branch_label(&repo);
    let mut branch = current.clone();
    let mut base_branch = String::new();
    let mut use_current_branch = true;
    match plan.mode.as_str() {
        "existingBranch" if !plan.branch.trim().is_empty() && plan.branch != current => {
            if !crate::gitwt::is_clean(&repo)? {
                return Err(format!(
                    "Wake 建议切到已有分支 {}，但当前工作区不干净，不能自动切换。",
                    plan.branch
                ));
            }
            let branches = crate::gitwt::list_branches(&repo).unwrap_or_default();
            if !branches.iter().any(|b| b == &plan.branch) {
                return Err(format!("Wake 建议的已有分支不存在：{}", plan.branch));
            }
            crate::gitwt::checkout(&repo, &plan.branch)?;
            branch = plan.branch;
            use_current_branch = false;
        }
        "newBranch" if !plan.branch.trim().is_empty() && plan.branch != current => {
            if !crate::gitwt::is_clean(&repo)? {
                return Err(format!(
                    "Wake 建议新建分支 {}，但当前工作区不干净，不能自动切换。",
                    plan.branch
                ));
            }
            if !crate::gitwt::valid_branch(&plan.branch) {
                return Err(format!("Wake 建议的分支名不合法：{}", plan.branch));
            }
            if let Some(msg) = crate::gitwt::branch_conflict(&repo, &plan.branch) {
                return Err(msg);
            }
            crate::gitwt::checkout_new_branch(&repo, &plan.branch, &plan.base_branch)?;
            branch = plan.branch;
            base_branch = plan.base_branch;
            use_current_branch = false;
        }
        _ => {}
    }
    let workspace = Workflow {
        id: uuid::Uuid::new_v4().to_string(),
        employee_id: emp.id.clone(),
        scope: scope.to_string(),
        key: key.to_string(),
        title: task_title_of(key, title),
        repo: repo.clone(),
        cwd: repo,
        branch,
        base_branch,
        use_worktree: false,
        use_current_branch,
        thread_id: None,
        status: "active".into(),
        created_at: now_ms(),
        updated_at: now_ms(),
    };
    Ok(WorkspaceReady {
        workspace,
        _repo_lock: Some(lock),
    })
}

fn normalize_preflight_plan(
    preflight: Option<&WorkPreflight>,
    emp: &Employee,
    key: &str,
) -> WorkPreflight {
    let mut p = preflight.cloned().unwrap_or_default();
    p.mode = match p.mode.trim() {
        "existingBranch" | "newBranch" | "worktree" | "current" => p.mode.trim().to_string(),
        "existing_branch" | "existing" => "existingBranch".to_string(),
        "new_branch" | "branch" => "newBranch".to_string(),
        "work_tree" => "worktree".to_string(),
        _ => "current".to_string(),
    };
    p.branch = p.branch.trim().to_string();
    p.base_branch = p.base_branch.trim().to_string();
    if p.mode == "worktree" && !emp.allow_worktree {
        p.mode = "current".to_string();
    }
    if (p.mode == "worktree" || p.mode == "newBranch") && p.branch.is_empty() {
        p.branch = default_employee_branch(emp, key);
    }
    p
}

fn default_employee_branch(emp: &Employee, key: &str) -> String {
    format!(
        "nova/{}/{}",
        branch_component(&emp.name, "employee"),
        branch_component(key, "task")
    )
}

fn branch_component(s: &str, fallback: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in s.trim().chars() {
        let mapped = if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            Some(ch.to_ascii_lowercase())
        } else if ch.is_alphanumeric() {
            Some(ch)
        } else {
            None
        };
        if let Some(c) = mapped {
            out.push(c);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
        if out.chars().count() >= 48 {
            break;
        }
    }
    let out = out.trim_matches('-').trim_matches('_').to_string();
    if out.is_empty() {
        fallback.to_string()
    } else {
        out
    }
}

fn unique_manual_key(content: &str) -> String {
    let base = one_line(content, 48);
    let suffix = uuid::Uuid::new_v4().to_string();
    let suffix = suffix.split('-').next().unwrap_or("new");
    format!("{base}#{suffix}")
}

fn current_branch_label(repo: &str) -> String {
    let branch = crate::gitwt::current_branch(repo);
    if branch.is_empty() {
        "HEAD".to_string()
    } else {
        branch
    }
}

fn attach_workflow_thread(app: &AppHandle, workflow: &Workflow, thread_id: &str) {
    let state = app.state::<AppState>();
    {
        let mut flows = state.workflows.lock().unwrap();
        if let Some(w) = flows.workflows.iter_mut().find(|w| w.id == workflow.id) {
            w.thread_id = Some(thread_id.to_string());
            w.updated_at = now_ms();
        }
        flows.save();
    }
    if workflow.use_worktree {
        let mut wts = state.worktrees.lock().unwrap();
        if let Some(wt) = wts.worktrees.iter_mut().find(|w| {
            w.repo == workflow.repo && w.path == workflow.cwd && w.branch == workflow.branch
        }) {
            wt.thread_id = Some(thread_id.to_string());
        }
        wts.save();
    }
}

fn mark_workflow_waiting(app: &AppHandle, emp_id: &str, scope: &str, key: &str) {
    let state = app.state::<AppState>();
    let mut flows = state.workflows.lock().unwrap();
    if let Some(w) = flows.get_mut_by_task(emp_id, scope, key) {
        w.status = "waiting".into();
        w.updated_at = now_ms();
        flows.save();
    }
}

fn mark_workflow_blocked(app: &AppHandle, emp_id: &str, scope: &str, key: &str) {
    let state = app.state::<AppState>();
    let mut flows = state.workflows.lock().unwrap();
    if let Some(w) = flows.get_mut_by_task(emp_id, scope, key) {
        w.status = "blocked".into();
        w.updated_at = now_ms();
        flows.save();
    }
}

#[allow(dead_code)]
fn route_key_from_decision(dec: &Decision) -> Option<String> {
    let answer = dec.answer.as_deref().unwrap_or_default();
    for opt in &dec.options {
        let Some(start) = opt.find('[') else { continue };
        let Some(end_rel) = opt[start + 1..].find(']') else {
            continue;
        };
        let key = opt[start + 1..start + 1 + end_rel].trim();
        if !key.is_empty() && (answer.contains(key) || answer.contains(opt)) {
            return Some(key.to_string());
        }
    }
    None
}

fn workflow_context(workflow: &Workflow, _emp: &Employee, startup: bool) -> String {
    if workflow.use_current_branch && startup {
        return format!(
            "\n\n【Git 工作方式】\n仓库：{}\n初始工作目录：{}\n{instruction}",
            workflow.repo,
            workflow.cwd,
            instruction = "Wake 已完成，本轮先沿用当前分支/目录执行（Wake 明确选择 mode=current）。工作模型不要再花大量注意力判断是否切分支；如实际检查发现不适用，再在 NEXT_ACTION 中说明风险或上奏。可创建和提交，但不要自行把工作分支合并回当前分支；是否合并由用户或界面操作决定。"
        );
    }
    let mode = if workflow.use_worktree {
        "独立 worktree"
    } else if workflow.use_current_branch {
        "当前分支"
    } else {
        "原地分支"
    };
    let base = if workflow.base_branch.trim().is_empty() {
        "当前分支"
    } else {
        workflow.base_branch.trim()
    };
    let branch_instruction = if workflow.use_current_branch {
        "继续沿用本会话已经选择的 Git 工作方式，不要无故重新切换。"
    } else {
        "继续在这个分支推进，不要自行切到无关分支，不要自行合并回当前分支。"
    };
    format!(
        "\n\n【Git 执行位置】\n模式：{mode}\n仓库：{}\n工作目录：{}\n分支：{}\n基准分支：{}\n{branch_instruction}",
        workflow.repo, workflow.cwd, workflow.branch, base
    )
}

fn commit_workflow_if_needed(
    app: &AppHandle,
    emp: &Employee,
    scope: &str,
    key: &str,
    task_title: &str,
    summary: &str,
) {
    let workflow = {
        let state = app.state::<AppState>();
        let mut flows = state.workflows.lock().unwrap();
        let found = flows.get_mut_by_task(&emp.id, scope, key).map(|w| {
            w.status = "waiting".into();
            w.updated_at = now_ms();
            w.clone()
        });
        if found.is_some() {
            flows.save();
        }
        found
    };
    let Some(workflow) = workflow else { return };
    if workflow.use_worktree {
        return;
    }
    let msg = format!("{}: {}", task_title.trim(), one_line(summary, 120));
    match crate::gitwt::commit_all(&workflow.cwd, &msg) {
        Ok(true) => append_journal(
            app,
            &emp.id,
            key,
            "自动提交",
            &format!("已在分支 {} 提交本轮改动。", workflow.branch),
        ),
        Ok(false) => {}
        Err(e) => {
            append_journal(app, &emp.id, key, "自动提交失败", &e);
            file_workflow_decision(
                app,
                emp,
                scope,
                key,
                task_title,
                &format!(
                    "我完成了任务，但在分支 {} 自动提交失败：{e}。请决定如何处理。",
                    workflow.branch
                ),
            );
        }
    }
}

fn file_workflow_decision(
    app: &AppHandle,
    emp: &Employee,
    scope: &str,
    key: &str,
    task_title: &str,
    question: &str,
) {
    let signature = decision_signature(scope, key, "workflow", question);
    let _ = crate::notice::emit_decision(
        app,
        emp,
        scope,
        key,
        task_title,
        "",
        question,
        "choose",
        workflow_decision_options(question),
        dev_thread_get(scope, key),
        Some(signature),
        "employee",
        &proposed_action_for_workflow_error(question),
        "",
    );
}

fn discard_thread(app: &AppHandle, thread_id: &str) {
    {
        let state = app.state::<AppState>();
        let mut store = state.store.lock().unwrap();
        store.threads.retain(|t| t.id != thread_id);
        store.save();
    }
    let state = app.state::<AppState>();
    state.acp.forget_session_of_thread(thread_id);
    state.codebuddy.forget_session_of_thread(thread_id);
    state.claudecode.forget_session_of_thread(thread_id);
    state.opencode.forget_session_of_thread(thread_id);
    state.codex.forget_session_of_thread(thread_id);
    let _ = app.emit(crate::acp::EV_THREADS, json!({}));
}

pub(crate) fn discard_employee_thread(app: &AppHandle, thread_id: &str) {
    discard_thread(app, thread_id);
}

/// 重命名会话标题（用于把巡查会话标注上它选中的单子，便于历史查看）。
fn rename_thread(app: &AppHandle, thread_id: &str, title: &str) {
    {
        let state = app.state::<AppState>();
        let mut store = state.store.lock().unwrap();
        if let Some(t) = store.get_mut(thread_id) {
            t.title = title.to_string();
            t.updated_at = now_ms();
        }
        store.save();
    }
    let _ = app.emit(crate::acp::EV_THREADS, json!({}));
}

pub(crate) fn rename_employee_thread(app: &AppHandle, thread_id: &str, title: &str) {
    rename_thread(app, thread_id, title);
}

fn create_task(
    app: &AppHandle,
    employee_id: &str,
    title: &str,
    brief: &str,
    origin: &str,
    thread_id: Option<String>,
    status: &str,
) -> Option<Task> {
    let title = title.trim();
    if title.is_empty() {
        return None;
    }
    let now = now_ms();
    let task = Task {
        id: uuid::Uuid::new_v4().to_string(),
        employee_id: employee_id.to_string(),
        title: title.to_string(),
        brief: brief.trim().to_string(),
        status: status.to_string(),
        origin: origin.to_string(),
        thread_id,
        result: None,
        created_at: now,
        updated_at: now,
    };
    {
        let state = app.state::<AppState>();
        let mut store = state.tasks.lock().unwrap();
        store.tasks.push(task.clone());
        store.save();
    }
    let _ = app.emit(EV_TASKS, json!({}));
    Some(task)
}

fn finish_task(app: &AppHandle, task_id: &str, status: &str, result: Option<String>) {
    {
        let state = app.state::<AppState>();
        let mut tasks = state.tasks.lock().unwrap();
        if let Some(t) = tasks.get_mut(task_id) {
            t.status = status.into();
            if let Some(r) = result {
                t.result = Some(r);
            }
            t.updated_at = now_ms();
        }
        tasks.save();
    }
    let _ = app.emit(EV_TASKS, json!({}));
}

/// 登记一个「待处理」单子（本机或共享账本）。用于转交同事、用户交办。
async fn ledger_register_open(
    app: &AppHandle,
    use_shared: bool,
    scope: &str,
    key: &str,
    title: &str,
    note: &str,
    images: Vec<PromptImage>,
) {
    if use_shared {
        if ledger_status(app, true, scope, key).await == "failed" {
            return;
        }
        let relay = app.state::<AppState>().relay.clone();
        // 服务端 set 不会创建条目：先 claim 建条目，再释放为 open。
        if let Ok((outcome, _)) = relay.ledger_claim(scope, key, title, "", "", 1).await {
            if outcome == "failed" {
                return;
            }
        }
        let note = shared_note_with_attachments(note, &images);
        let _ = relay
            .ledger_set(scope, key, "open", Some(&note), true)
            .await;
    } else {
        let state = app.state::<AppState>();
        let mut marks = state.marks.lock().unwrap();
        marks.register_open(scope, key, title, note, images);
    }
}

/// 迁移旧收件箱：把历史 tasks.json 里 queued/working 的任务转成对应员工账本的「待处理」单子，
/// 让新模型下员工能自主认领（旧的收件箱驱动已下线）。迁移后删除这些队列任务，避免重复迁移。
pub fn migrate_tasks_to_ledger(app: &AppHandle) {
    let state = app.state::<AppState>();
    let employees = state.employees.lock().unwrap().employees.clone();
    let old: Vec<Task> = {
        let tasks = state.tasks.lock().unwrap();
        tasks
            .tasks
            .iter()
            .filter(|t| t.status == "queued" || t.status == "working")
            .cloned()
            .collect()
    };
    if old.is_empty() {
        return;
    }
    {
        let mut marks = state.marks.lock().unwrap();
        for t in &old {
            if let Some(emp) = employees.iter().find(|e| e.id == t.employee_id) {
                let scope = emp_scope(emp);
                let key = one_line(&t.title, 60);
                marks.register_open(&scope, &key, &t.title, t.brief.trim(), Vec::new());
            }
        }
    }
    {
        let ids: std::collections::HashSet<String> = old.iter().map(|t| t.id.clone()).collect();
        let mut tasks = state.tasks.lock().unwrap();
        tasks.tasks.retain(|t| !ids.contains(&t.id));
        tasks.save();
    }
    let _ = app.emit(EV_MARKS, json!({}));
    let _ = app.emit(EV_TASKS, json!({}));
}

/// 用户交办：把一个具体单子登记到该员工账本的「待处理」，员工唤起后自行侦察认领。
/// 无需单独填标题：用「标题」或「说明」中任一非空内容作为单子内容，据此生成账本 key。
pub async fn register_ledger_item(
    app: &AppHandle,
    employee_id: String,
    title: String,
    brief: String,
    mut images: Vec<PromptImage>,
) -> Result<(), String> {
    embed_attachment_data(&mut images);
    let title = title.trim().to_string();
    let brief = brief.trim().to_string();
    // 内容来源：优先标题，其次说明。二者皆空才报错（不再强制填标题）。
    let content = if !title.is_empty() {
        title.clone()
    } else if !brief.is_empty() {
        brief.clone()
    } else if !images.is_empty() {
        let names = images
            .iter()
            .map(|img| img.name.trim())
            .filter(|name| !name.is_empty())
            .take(3)
            .collect::<Vec<_>>()
            .join("、");
        if names.is_empty() {
            format!("附件交办（{} 个）", images.len())
        } else {
            format!("附件交办：{names}")
        }
    } else {
        String::new()
    };
    if content.is_empty() {
        return Err("请填写单子内容".into());
    }
    let emp = {
        let state = app.state::<AppState>();
        let store = state.employees.lock().unwrap();
        store.get(&employee_id).cloned()
    }
    .ok_or("员工不存在")?;
    crate::mind::preempt_for_work(app, &employee_id);
    let scope = emp_scope(&emp);
    let use_shared = emp.shared_ledger && app.state::<AppState>().relay.is_configured();
    // 用户每次亲自交办都必须是一张新单，不能因为内容相同/相似撞到旧 key 后续进旧会话。
    let proposed_key = unique_manual_key(&content);
    // 无独立标题时：把内容当作账本标题展示，不再单独存 note（避免同一句话重复显示）。
    let (mark_title, note) = if title.is_empty() {
        (content.clone(), String::new())
    } else {
        (title.clone(), brief.clone())
    };
    let key = proposed_key;
    ledger_register_open(app, use_shared, &scope, &key, &mark_title, &note, images).await;
    // 用户亲自交办就是一次新的执行要求。即使同 key 的旧单已经完成，也重新开放再做一遍；
    // 不能让历史 done 状态吞掉本次交办，随后误落入巡查。
    if ledger_status(app, use_shared, &scope, &key).await == "done" {
        ledger_set_status(
            app,
            use_shared,
            &scope,
            &key,
            "open",
            Some(note.clone()),
            true,
        )
        .await;
    }
    // 主管亲自交办：清掉冷却（明确要求做），并清零心跳让员工尽快（≤5s）醒来领活。
    cooloff_clear(&scope, &key);
    {
        let state = app.state::<AppState>();
        let mut emps = state.employees.lock().unwrap();
        if let Some(e) = emps.get_mut(&employee_id) {
            e.last_heartbeat_ms = 0;
        }
        emps.save();
    }
    let _ = app.emit(EV_MARKS, json!({}));
    // 关闭自动心跳的员工不会自己醒来领活：交办即用户点名，立即唤起一轮。
    // 正忙则忽略（单子已登记在账本，忙完后用户可再「立即执行」）。
    if !emp.heartbeat_enabled {
        let _ = run_now(app, &employee_id);
    }
    Ok(())
}

/// 该会话是否仍存在（用户可能已删除员工会话）。复用前校验，避免往已删除的会话发消息。
fn thread_alive(app: &AppHandle, thread_id: &str) -> bool {
    let state = app.state::<AppState>();
    let store = state.store.lock().unwrap();
    store.get(thread_id).is_some()
}

fn thread_is_mind(app: &AppHandle, thread_id: &str) -> bool {
    let state = app.state::<AppState>();
    let store = state.store.lock().unwrap();
    store
        .get(thread_id)
        .is_some_and(|t| t.mind_thread || t.title.contains("Wake"))
}

/// 主管批示/留中要求继续推进时，Wake 不得直接 done。
fn supervision_forces_do(prior_note: &str, extra: &str) -> bool {
    let s = format!("{prior_note}\n{extra}");
    s.contains("主管批示")
        || s.contains("主管已在御书房")
        || s.contains("留中不发")
        || s.contains("遵照批示")
        || s.contains("第一时间遵照")
        || s.contains("请**放下其他事")
}

fn thread_has_assistant(app: &AppHandle, thread_id: &str) -> bool {
    let state = app.state::<AppState>();
    let store = state.store.lock().unwrap();
    store.get(thread_id).is_some_and(|thread| {
        thread
            .items
            .iter()
            .any(|it| matches!(it, Item::Assistant { text, .. } if !text.trim().is_empty()))
    })
}

fn extract_last_assistant(app: &AppHandle, thread_id: &str) -> Option<String> {
    let state = app.state::<AppState>();
    let store = state.store.lock().unwrap();
    let thread = store.get(thread_id)?;
    thread.items.iter().rev().find_map(|it| match it {
        Item::Assistant { text, .. } if !text.trim().is_empty() => Some(text.trim().to_string()),
        _ => None,
    })
}

pub(crate) fn last_employee_assistant(app: &AppHandle, thread_id: &str) -> Option<String> {
    extract_last_assistant(app, thread_id)
}

/// 该会话里是否有错误条目（agent 报错以 System{level:"error"} 记录）。
/// 用于巡查会话的取舍：出错的巡查保留下来便于排查，干净无收获的才自动删除。
fn thread_has_error(app: &AppHandle, thread_id: &str) -> bool {
    let state = app.state::<AppState>();
    let store = state.store.lock().unwrap();
    let Some(thread) = store.get(thread_id) else {
        return false;
    };
    thread
        .items
        .iter()
        .any(|it| matches!(it, Item::System { level, .. } if level == "error"))
}

pub(crate) fn employee_thread_has_error(app: &AppHandle, thread_id: &str) -> bool {
    thread_has_error(app, thread_id)
}

fn append_journal(
    app: &AppHandle,
    employee_id: &str,
    task_id: &str,
    task_title: &str,
    summary: &str,
) {
    append_journal_kind(app, employee_id, task_id, task_title, summary, false, "", 0);
}

/// 与 append_journal 相同，但可指定是否置顶为「长期知识」（learn 工具沉淀时置顶）。
fn append_journal_full(
    app: &AppHandle,
    employee_id: &str,
    task_id: &str,
    task_title: &str,
    summary: &str,
    pinned: bool,
    kind: &str,
) {
    append_journal_kind(
        app,
        employee_id,
        task_id,
        task_title,
        summary,
        pinned,
        kind,
        0,
    );
}

/// 经历事件：办成/受阻/停滞/被叫停/主管批示等只送给 Mind 做收尾判断，
/// 不直接进入记忆库。记忆库只吃 memo/learn/lesson 这类可复用结论。
fn append_event(
    app: &AppHandle,
    employee_id: &str,
    task_id: &str,
    task_title: &str,
    summary: &str,
    kind: &str,
) {
    if summary.trim().is_empty() {
        return;
    };
    if matches!(
        kind,
        "outcome:done" | "outcome:blocked" | "outcome:stalled" | "outcome:stopped"
    ) {
        apply_task_memory_evidence(app, employee_id, task_id, kind);
    }
    let created_at = now_ms();
    let scope = find_employee(app, employee_id)
        .map(|emp| emp_scope(&emp))
        .unwrap_or_else(|| format!("emp:{employee_id}"));
    crate::mind::record_journal_event(
        app,
        employee_id,
        &scope,
        task_id,
        task_title,
        summary,
        kind,
        created_at,
    );
}

/// 底层：写一条带类型标签的记忆。
#[allow(clippy::too_many_arguments)]
fn append_journal_kind(
    app: &AppHandle,
    employee_id: &str,
    task_id: &str,
    task_title: &str,
    summary: &str,
    pinned: bool,
    kind: &str,
    evidence: u32,
) -> Option<i64> {
    if summary.trim().is_empty() {
        return None;
    }
    let created_at = now_ms();
    let state = app.state::<AppState>();
    let mut mem = state.memory.lock().unwrap();
    mem.append(
        employee_id,
        JournalEntry {
            ts: created_at,
            task_id: task_id.to_string(),
            task_title: task_title.to_string(),
            summary: summary.to_string(),
            pinned,
            kind: kind.to_string(),
            evidence,
            source: if kind.starts_with("outcome:") || kind == "supervision" {
                "system".to_string()
            } else {
                "agent".to_string()
            },
            protected: false,
            confidence: if pinned { 0.8 } else { 0.5 },
            last_used_at: 0,
            hit_count: 0,
            expires_at: 0,
            superseded_by: None,
            positive_evidence: evidence,
            negative_evidence: 0,
            user_feedback: 0,
            evidence_tasks: Vec::new(),
        },
    );
    Some(created_at)
}

/// 毫秒时间戳格式化为本地可读日期。
fn fmt_ts(ts: i64) -> String {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_millis_opt(ts)
        .single()
        .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

/// 把名字收敛成安全的目录名（保留中英文与数字，其余替换为 _）。
fn sanitize_name(name: &str) -> String {
    let s: String = name
        .trim()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || ('\u{4e00}'..='\u{9fff}').contains(&c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    let s = s.trim_matches('_').to_string();
    if s.is_empty() {
        "employee".to_string()
    } else {
        s
    }
}

/// 把员工的长期知识与工作记忆导出到用户数据目录，供 agent 用自带读取/grep
/// 工具「按需、被动」检索（而不是每轮把工作日志硬塞进提示词）。
/// 返回绝对目录（如 `~/.nova/memory/<name>`）；失败返回 None。
fn export_memory_files(app: &AppHandle, emp: &Employee) -> Option<String> {
    let entries = {
        let state = app.state::<AppState>();
        let mem = state.memory.lock().unwrap();
        mem.all(&emp.id)
    };
    let safe = sanitize_name(&emp.name);
    let abs_dir = app
        .state::<AppState>()
        .config_dir
        .join("memory")
        .join(&safe);
    if fs::create_dir_all(&abs_dir).is_err() {
        return None;
    }

    // 长期知识（用户置顶，务必遵守）+ 守则（从教训进化来的行为规则）
    let mut pinned: Vec<&JournalEntry> = entries
        .iter()
        .filter(|e| e.pinned && e.kind != "lesson")
        .collect();
    pinned.sort_by(|a, b| b.ts.cmp(&a.ts));
    let mut k = String::from("# 长期知识（务必遵守）\n\n");
    if pinned.is_empty() {
        k.push_str("（暂无）\n");
    } else {
        for e in &pinned {
            k.push_str(&format!("- {}\n", e.summary.trim()));
        }
    }
    let mut lessons: Vec<&JournalEntry> = entries.iter().filter(|e| e.kind == "lesson").collect();
    lessons.sort_by(|a, b| b.pinned.cmp(&a.pinned).then(b.ts.cmp(&a.ts)));
    if !lessons.is_empty() {
        k.push_str("\n# 工作守则（从自己的经验教训中进化而来）\n\n");
        for e in &lessons {
            let tag = if e.pinned {
                "已内化".to_string()
            } else {
                format!("试行·已验证{}次", e.evidence)
            };
            k.push_str(&format!("- [{}] {}\n", tag, e.summary.trim()));
        }
    }
    let mut challenged: Vec<&JournalEntry> = entries
        .iter()
        .filter(|e| e.kind == "lesson:challenged")
        .collect();
    challenged.sort_by(|a, b| b.ts.cmp(&a.ts));
    if !challenged.is_empty() {
        k.push_str("\n# 受挑战守则（暂停自动遵守，等待修正或淘汰）\n\n");
        for e in challenged.iter().take(12) {
            k.push_str(&format!("- {}\n", e.summary.trim()));
        }
    }
    if fs::write(abs_dir.join("knowledge.md"), k).is_err() {
        return None;
    }

    // 工作记忆（历史工作日志，新→旧；守则已在 knowledge.md，单独排除）
    let mut logs: Vec<&JournalEntry> = entries
        .iter()
        .filter(|e| !e.pinned && !e.kind.starts_with("lesson"))
        .collect();
    logs.sort_by(|a, b| b.ts.cmp(&a.ts));
    let mut m = String::from("# 工作记忆（历史工作日志，新→旧）\n\n");
    if logs.is_empty() {
        m.push_str("（暂无历史记忆，这可能是你的第一项任务）\n");
    } else {
        for e in &logs {
            let title = if e.task_title.trim().is_empty() {
                "(无标题)"
            } else {
                e.task_title.trim()
            };
            m.push_str(&format!(
                "## [{}] {}\n{}\n\n",
                fmt_ts(e.ts),
                title,
                e.summary.trim()
            ));
        }
    }
    if fs::write(abs_dir.join("memory.md"), m).is_err() {
        return None;
    }
    cleanup_legacy_memory_files(emp, &safe);
    Some(abs_dir.to_string_lossy().to_string())
}

fn cleanup_legacy_memory_files(emp: &Employee, safe_name: &str) {
    let project_nova = Path::new(&emp.cwd).join(".nova");
    let memory_root = project_nova.join("memory");
    let employee_dir = memory_root.join(safe_name);
    let _ = fs::remove_file(employee_dir.join("knowledge.md"));
    let _ = fs::remove_file(employee_dir.join("memory.md"));
    let _ = fs::remove_dir(&employee_dir);
    let _ = fs::remove_dir(&memory_root);
    let _ = fs::remove_dir(&project_nova);
}

/// 记忆/知识只通过工具按需检索，不再把导出文件路径塞进模型提示词。
fn memory_hint(_mem_dir: &str) -> String {
    String::new()
}

/// CLI：可执行文件路径 + 数据目录。
fn tool_ctx(app: &AppHandle) -> Option<(String, String)> {
    let exe = std::env::current_exe()
        .ok()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    let dir = crate::nova_data_dir(app).to_string_lossy().to_string();
    if exe.trim().is_empty() || dir.trim().is_empty() {
        None
    } else {
        Some((exe, dir))
    }
}

/// 完整工具手册：全后端统一走 nova CLI（经 shell），不再挂 MCP。
fn tools_manual(
    app: &AppHandle,
    emp: &Employee,
    scope: &str,
    key: &str,
    include: &[&str],
) -> String {
    tools_manual_cli(app, emp, scope, key, include)
}

fn tools_manual_cli(
    app: &AppHandle,
    emp: &Employee,
    scope: &str,
    key: &str,
    include: &[&str],
) -> String {
    let Some((exe, dir)) = tool_ctx(app) else {
        return String::new();
    };
    let invoke = if cfg!(windows) {
        format!("& \"{exe}\"")
    } else {
        format!("\"{exe}\"")
    };
    let base = format!("--data-dir \"{dir}\" --employee {id}", id = emp.id);
    let keyarg = if key.trim().is_empty() {
        "--key \"<单号/唯一标识>\"".to_string()
    } else {
        format!("--key \"{key}\"")
    };
    let has_write = include.iter().any(|t| {
        matches!(
            *t,
            "relay" | "memo" | "learn" | "lesson" | "lesson-verify" | "forget" | "done" | "blocked"
        )
    });
    let mut s = if has_write {
        String::from(
            "【工具】请用下面的 nova CLI（经 shell 执行）。读工具查资料；写工具提前落地 NEXT_ACTION。\n不要调用 MCP。\n",
        )
    } else {
        String::from("【工具】请用下面的 nova CLI（经 shell 执行）做只读检索。\n不要调用 MCP。\n")
    };
    if cfg!(windows) {
        s.push_str("PowerShell 注意：必须保留 `& \"...exe\" 参数...`。\n");
    }
    for t in include {
        let line = match *t {
            "kb-search" => format!(
                "- 检索记忆/知识：\n  `{invoke} kb-search {base} --query \"<关键词或问题>\"`\n"
            ),
            "ledger-list" => format!(
                "- 看门锁账本：\n  `{invoke} ledger-list --data-dir \"{dir}\" --scope \"{scope}\"`\n"
            ),
            "relay" => format!(
                "- 接力/讨论/上奏：\n  `{invoke} relay {base} --to <同事名|self|user> --kind <work|discuss|decision> --key \"<单号>\" --title \"<标题>\" --brief \"<交代>\"`\n"
            ),
            "memo" => format!(
                "- 沉淀记忆：\n  `{invoke} memo {base} {keyarg} --text \"<结论>\"`\n"
            ),
            "learn" => format!(
                "- 长期知识：\n  `{invoke} learn {base} --text \"<知识>\"`\n"
            ),
            "lesson" => format!(
                "- 试行守则：\n  `{invoke} lesson {base} {keyarg} --text \"当…时，就…\"`\n"
            ),
            "lesson-verify" => format!(
                "- 守则验证：\n  `{invoke} lesson-verify {base} {keyarg} --ts <守则时间戳>`\n",
            ),
            "forget" => format!(
                "- 清理记忆：\n  `{invoke} forget {base} --ts <记忆时间戳>`\n"
            ),
            "done" => format!(
                "- 办结：\n  `{invoke} done {base} --scope \"{scope}\" {keyarg} --summary \"<结果>\"`\n"
            ),
            "blocked" => format!(
                "- 受阻释放：\n  `{invoke} blocked {base} --scope \"{scope}\" {keyarg} --reason \"<原因>\"`\n"
            ),
            _ => String::new(),
        };
        s.push_str(&line);
    }
    s.push('\n');
    s
}

/// 相关性检索记忆：知识、守则和工作经历都先经过当前任务相关性过滤。
/// 历史仅作为候选线索，不能成为当前完成状态的证据。
async fn retrieve_memory(
    app: &AppHandle,
    employee_id: &str,
    query: &str,
    used_by_task: Option<&str>,
) -> String {
    let now = now_ms();
    let entries: Vec<JournalEntry> = {
        let state = app.state::<AppState>();
        let mem = state.memory.lock().unwrap();
        mem.all(employee_id)
    }
    .into_iter()
    .filter(|e| {
        e.superseded_by.is_none()
            && e.kind != "lesson:retired"
            && (e.expires_at <= 0 || e.expires_at > now)
    })
    .collect();
    if entries.is_empty() {
        return "（暂无历史记忆，这是你的第一项任务）".to_string();
    }

    let candidates: Vec<&JournalEntry> = entries
        .iter()
        .filter(|e| {
            e.kind != "lesson:challenged"
                && (e.kind.starts_with("lesson")
                    || e.kind.starts_with("knowledge")
                    || e.kind.starts_with("memory")
                    || e.source == "user"
                    || e.pinned)
        })
        .collect();
    let ranked = match semantic_rank(app, employee_id, query, &candidates, MEMORY_INJECT).await {
        Some(v) => v,
        None => bm25_rank(&candidates, query, MEMORY_INJECT),
    };
    let user_knowledge: Vec<&JournalEntry> = ranked
        .iter()
        .copied()
        .filter(|e| e.pinned && e.kind != "lesson" && e.source == "user")
        .take(KNOWLEDGE_INJECT)
        .collect();
    let knowledge: Vec<&JournalEntry> = ranked
        .iter()
        .copied()
        .filter(|e| e.pinned && e.kind != "lesson" && e.source != "user")
        .take(KNOWLEDGE_INJECT)
        .collect();
    let proven: Vec<&JournalEntry> = ranked
        .iter()
        .copied()
        .filter(|e| e.pinned && e.kind == "lesson")
        .take(PROVEN_LESSON_INJECT)
        .collect();
    let trial: Vec<&JournalEntry> = ranked
        .iter()
        .copied()
        .filter(|e| !e.pinned && e.kind == "lesson")
        .take(TRIAL_LESSON_INJECT)
        .collect();
    let memories: Vec<&JournalEntry> = ranked
        .iter()
        .copied()
        .filter(|e| !e.pinned && (e.kind.starts_with("memory") || e.source == "user"))
        .collect();

    let mut out = String::new();
    if !user_knowledge.is_empty() {
        out.push_str("【相关人工知识（用户维护；只在当前条件匹配时采用）】\n");
        for e in &user_knowledge {
            out.push_str(&format!("- {}\n", one_line(&e.summary, 300)));
        }
    }
    if !knowledge.is_empty() {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str("【相关事实与长期知识（只在当前条件匹配时采用；冲突时以当前状态为准）】\n");
        for e in &knowledge {
            out.push_str(&format!("- {}\n", one_line(&e.summary, 300)));
        }
    }
    if !proven.is_empty() {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str("【相关已验证经验（用于选择做法，不代表当前任务事实）】\n");
        for e in &proven {
            out.push_str(&format!("- {}\n", one_line(&e.summary, 300)));
        }
    }
    if !trial.is_empty() {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str("【相关试行经验（仅作候选；本轮真实有效才强化，被证伪就修正）】\n");
        for e in &trial {
            out.push_str(&format!(
                "- (ts={}, 已验证{}次) {}\n",
                e.ts,
                e.evidence,
                one_line(&e.summary, 300)
            ));
        }
    }
    let challenged_refs: Vec<&JournalEntry> = entries
        .iter()
        .filter(|e| e.kind == "lesson:challenged")
        .collect();
    let challenged = bm25_rank(&challenged_refs, query, 2);
    if !challenged.is_empty() {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str("【受挑战守则（不要直接遵守，用作避坑和修正线索）】\n");
        for e in &challenged {
            out.push_str(&format!("- {}\n", one_line(&e.summary, 260)));
        }
    }
    if !memories.is_empty() {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str("【相关可复用记忆（只影响做法，不代表任务状态）】\n");
        for e in &memories {
            out.push_str(&format!(
                "- 〔{}〕{}\n",
                e.task_title,
                one_line(&e.summary, 240)
            ));
        }
    }
    let mut used_ts: HashSet<i64> = HashSet::new();
    used_ts.extend(user_knowledge.iter().map(|entry| entry.ts));
    used_ts.extend(knowledge.iter().map(|entry| entry.ts));
    used_ts.extend(proven.iter().map(|entry| entry.ts));
    used_ts.extend(trial.iter().map(|entry| entry.ts));
    used_ts.extend(challenged.iter().map(|entry| entry.ts));
    used_ts.extend(memories.iter().map(|entry| entry.ts));
    if !used_ts.is_empty() {
        let used: Vec<i64> = used_ts.into_iter().collect();
        {
            let state = app.state::<AppState>();
            state.memory.lock().unwrap().mark_used(employee_id, &used);
        }
        remember_task_memory_usage(employee_id, used_by_task, &used);
    }
    if out.is_empty() {
        "（暂无历史记忆）".to_string()
    } else {
        out.trim_end().to_string()
    }
}

/// 语义检索：启用且 embedding 服务可用时按向量相似度排序取 top-k；
/// 未启用 / 服务不可达 / 出错 → 返回 None，由调用方回退 BM25。
/// 缺失向量惰性补算并落地；换模型自动整体失效重建。
async fn semantic_rank<'a>(
    app: &AppHandle,
    employee_id: &str,
    query: &str,
    docs: &[&'a JournalEntry],
    top_k: usize,
) -> Option<Vec<&'a JournalEntry>> {
    if docs.is_empty() {
        return None;
    }
    let (enabled, endpoint, model, key) = {
        let state = app.state::<AppState>();
        let s = state.settings.lock().unwrap();
        (
            s.semantic_enabled,
            s.embed_endpoint.clone(),
            s.embed_model.clone(),
            s.embed_api_key.clone(),
        )
    };
    if !enabled || endpoint.trim().is_empty() || model.trim().is_empty() {
        return None;
    }
    let client = reqwest::Client::new();

    // query 向量（E5/bge 类模型建议加 query/passage 前缀，对其他模型无害）
    let qv = crate::semantic::embed(
        &client,
        &endpoint,
        &model,
        &key,
        &[format!("query: {query}")],
    )
    .await
    .ok()?;
    let qv = qv.into_iter().next()?;
    if qv.is_empty() {
        return None;
    }

    // 找出缺（有效）向量的记忆，惰性补算
    let mut missing_idx: Vec<usize> = Vec::new();
    let mut missing_txt: Vec<String> = Vec::new();
    {
        let state = app.state::<AppState>();
        let vs = state.vectors.lock().unwrap();
        let model_ok = vs.model == model;
        for (i, d) in docs.iter().enumerate() {
            if !(model_ok && vs.get(employee_id, d.ts).is_some()) {
                missing_idx.push(i);
                missing_txt.push(format!(
                    "passage: {}",
                    one_line(&format!("{} {}", d.task_title, d.summary), 512)
                ));
            }
        }
    }
    if !missing_txt.is_empty() {
        let embs = crate::semantic::embed(&client, &endpoint, &model, &key, &missing_txt)
            .await
            .ok()?;
        if embs.len() != missing_txt.len() {
            return None;
        }
        let state = app.state::<AppState>();
        let mut vs = state.vectors.lock().unwrap();
        if vs.model != model {
            vs.set_model(&model);
        }
        for (k, i) in missing_idx.iter().enumerate() {
            vs.put(employee_id, docs[*i].ts, embs[k].clone());
        }
        vs.save();
    }

    let state = app.state::<AppState>();
    let vs = state.vectors.lock().unwrap();
    let mut scored: Vec<(f32, f32, &JournalEntry)> = docs
        .iter()
        .filter_map(|d| {
            vs.get(employee_id, d.ts).map(|v| {
                let base = crate::semantic::cosine(&qv, v);
                (base + evidence_boost_f32(d), base, *d)
            })
        })
        .collect();
    drop(vs);
    scored.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.2.ts.cmp(&a.2.ts))
    });
    Some(
        scored
            .into_iter()
            .filter(|(_, base, _)| *base >= SEMANTIC_MIN_SIMILARITY)
            .take(top_k)
            .map(|(_, _, d)| d)
            .collect(),
    )
}

fn evidence_delta(e: &JournalEntry) -> i32 {
    e.positive_evidence as i32 - e.negative_evidence as i32
}

fn evidence_boost_f32(e: &JournalEntry) -> f32 {
    evidence_delta(e) as f32 * 0.05
}

fn evidence_boost_f64(e: &JournalEntry) -> f64 {
    evidence_delta(e) as f64 * 0.05
}

fn is_cjk(ch: char) -> bool {
    matches!(ch as u32, 0x3040..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF)
}

fn push_ascii(buf: &mut String, out: &mut Vec<String>) {
    if buf.chars().count() >= 2 {
        out.push(std::mem::take(buf));
    } else {
        buf.clear();
    }
}

fn push_cjk(buf: &mut Vec<char>, out: &mut Vec<String>) {
    if buf.len() == 1 {
        out.push(buf[0].to_string());
    } else {
        for w in buf.windows(2) {
            out.push(w.iter().collect());
        }
    }
    buf.clear();
}

/// 中文友好的轻量分词：英文/数字按整词、中文按相邻 bigram。保留重复（供词频统计）。
fn tokenize_terms(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut ascii = String::new();
    let mut cjk: Vec<char> = Vec::new();
    for ch in text.chars().flat_map(|c| c.to_lowercase()) {
        if ch.is_ascii_alphanumeric() {
            push_cjk(&mut cjk, &mut out);
            ascii.push(ch);
        } else if is_cjk(ch) {
            push_ascii(&mut ascii, &mut out);
            cjk.push(ch);
        } else {
            push_ascii(&mut ascii, &mut out);
            push_cjk(&mut cjk, &mut out);
        }
    }
    push_ascii(&mut ascii, &mut out);
    push_cjk(&mut cjk, &mut out);
    out
}

/// BM25 检索：对一批工作日志按与 query 的相关度排序，取 top-k。
/// - 标题命中加权（TITLE_BOOST）；
/// - IDF 抑制高频泛词，长度归一避免长文档占便宜；
/// - 叠加温和的时间衰减（新近记忆略优，相关度仍主导）。
/// query 为空时不注入，避免“近期”被误当成“相关”。无外部依赖。
pub(crate) fn bm25_rank<'a>(
    docs: &[&'a JournalEntry],
    query: &str,
    top_k: usize,
) -> Vec<&'a JournalEntry> {
    if docs.is_empty() {
        return Vec::new();
    }
    let q_terms: HashSet<String> = tokenize_terms(query).into_iter().collect();
    if q_terms.is_empty() {
        return Vec::new();
    }

    const TITLE_BOOST: u32 = 2;
    let doc_tfs: Vec<HashMap<String, u32>> = docs
        .iter()
        .map(|e| {
            let mut tf: HashMap<String, u32> = HashMap::new();
            for t in tokenize_terms(&e.summary) {
                *tf.entry(t).or_insert(0) += 1;
            }
            for t in tokenize_terms(&e.task_title) {
                *tf.entry(t).or_insert(0) += TITLE_BOOST;
            }
            tf
        })
        .collect();
    let doc_len: Vec<f64> = doc_tfs
        .iter()
        .map(|tf| tf.values().map(|&c| c as f64).sum())
        .collect();
    let n = docs.len() as f64;
    let avgdl = (doc_len.iter().sum::<f64>() / n).max(1.0);

    let mut df: HashMap<&str, f64> = HashMap::new();
    for qt in &q_terms {
        let c = doc_tfs.iter().filter(|tf| tf.contains_key(qt)).count() as f64;
        df.insert(qt.as_str(), c);
    }

    let k1 = 1.5f64;
    let b = 0.75f64;
    let now = now_ms();
    let mut scored: Vec<(f64, f64, &JournalEntry)> = docs
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let tf = &doc_tfs[i];
            let dl = doc_len[i];
            let mut s = 0.0f64;
            for qt in &q_terms {
                let f = *tf.get(qt).unwrap_or(&0) as f64;
                if f == 0.0 {
                    continue;
                }
                let n_qi = *df.get(qt.as_str()).unwrap_or(&0.0);
                let idf = (((n - n_qi + 0.5) / (n_qi + 0.5)) + 1.0).ln();
                let denom = f + k1 * (1.0 - b + b * (dl / avgdl));
                s += idf * (f * (k1 + 1.0)) / denom;
            }
            // 30 天半衰期的温和时间加成
            let age_days = ((now - e.ts).max(0) as f64) / (1000.0 * 60.0 * 60.0 * 24.0);
            let recency = 0.5f64.powf(age_days / 30.0);
            let base = s * (1.0 + 0.2 * recency);
            (base + evidence_boost_f64(e), base, *e)
        })
        .collect();
    scored.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.2.ts.cmp(&a.2.ts))
    });
    scored
        .into_iter()
        .filter(|(_, base, _)| *base > 0.0)
        .take(top_k)
        .map(|(_, _, e)| e)
        .collect()
}

fn one_line(s: &str, max: usize) -> String {
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let chars: Vec<char> = flat.chars().collect();
    if chars.len() <= max {
        flat
    } else {
        let head: String = chars[..max].iter().collect();
        format!("{head}…")
    }
}

// ===== 指令解析（仅保留极薄的收尾文本兜底；其余协作动作全走工具）=====

enum ParsedNextAction {
    Continue,
    Command(InboxCommand),
}

fn extract_json_object(s: &str) -> Option<&str> {
    let start = s.find('{')?;
    let mut depth = 0i32;
    let mut in_str = false;
    let mut esc = false;
    for (idx, ch) in s[start..].char_indices() {
        if in_str {
            if esc {
                esc = false;
            } else if ch == '\\' {
                esc = true;
            } else if ch == '"' {
                in_str = false;
            }
            continue;
        }
        match ch {
            '"' => in_str = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&s[start..start + idx + ch.len_utf8()]);
                }
            }
            _ => {}
        }
    }
    None
}

fn json_str(v: &serde_json::Value, key: &str) -> String {
    v.get(key)
        .and_then(|x| x.as_str())
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn json_options(v: &serde_json::Value) -> Vec<String> {
    if let Some(arr) = v.get("options").and_then(|x| x.as_array()) {
        return arr
            .iter()
            .filter_map(|x| x.as_str())
            .map(|x| x.trim().to_string())
            .filter(|x| !x.is_empty())
            .collect();
    }
    json_str(v, "options")
        .split([';', '；', ',', '，'])
        .map(|x| x.trim().to_string())
        .filter(|x| !x.is_empty())
        .collect()
}

fn find_last_next_action_marker(text: &str) -> Option<usize> {
    let mut offset = 0usize;
    let mut found = None;
    for segment in text.split_inclusive('\n') {
        let line = segment.trim_end_matches(['\r', '\n']);
        let trimmed = line.trim_start();
        let leading = line.len().saturating_sub(trimmed.len());
        if trimmed.starts_with("NEXT_ACTION") && extract_json_object(trimmed).is_some() {
            found = Some(offset + leading);
        }
        offset += segment.len();
    }
    found
}

/// 统一出口：员工最后交出 NEXT_ACTION，系统只做路由。
fn parse_next_action(
    text: &str,
    actor_id: &str,
    scope: &str,
    key: &str,
    title: &str,
) -> Option<ParsedNextAction> {
    let marker_at = find_last_next_action_marker(text)?;
    let raw = &text[marker_at..];
    let json = extract_json_object(raw)?;
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let action = json_str(&v, "action").to_lowercase();
    if action.is_empty() {
        return None;
    }
    if matches!(action.as_str(), "continue" | "续做" | "继续" | "self") {
        return Some(ParsedNextAction::Continue);
    }

    let next_key = {
        let s = json_str(&v, "key");
        if s.is_empty() {
            key.to_string()
        } else {
            s
        }
    };
    let next_title = {
        let s = json_str(&v, "title");
        if s.is_empty() {
            title.to_string()
        } else {
            s
        }
    };
    let brief = {
        let b = json_str(&v, "brief");
        if b.is_empty() {
            json_str(&v, "summary")
        } else {
            b
        }
    };
    let mut cmd = InboxCommand {
        from: actor_id.to_string(),
        scope: scope.to_string(),
        key: next_key,
        title: next_title,
        brief: brief.clone(),
        at: now_ms(),
        ..Default::default()
    };

    match action.as_str() {
        "done" | "complete" | "完成" => {
            cmd.kind = "done".into();
            cmd.summary = {
                let s = json_str(&v, "summary");
                if s.is_empty() {
                    one_line(text, 400)
                } else {
                    s
                }
            };
            Some(ParsedNextAction::Command(cmd))
        }
        "release" | "blocked" | "block" | "释放" | "受阻" => {
            cmd.kind = "blocked".into();
            cmd.reason = {
                let s = json_str(&v, "reason");
                if s.is_empty() {
                    brief
                } else {
                    s
                }
            };
            Some(ParsedNextAction::Command(cmd))
        }
        "discuss" | "discussion" | "讨论" => {
            let to = json_str(&v, "to");
            if to.is_empty() {
                return None;
            }
            cmd.kind = "relay".into();
            cmd.to = to;
            cmd.relay_kind = "discuss".into();
            cmd.question = json_str(&v, "question");
            Some(ParsedNextAction::Command(cmd))
        }
        "handoff" | "relay" | "work" | "transfer" | "接力" | "转交" => {
            let to = json_str(&v, "to");
            if to.is_empty() {
                return None;
            }
            cmd.kind = "relay".into();
            cmd.to = to;
            cmd.relay_kind = "work".into();
            Some(ParsedNextAction::Command(cmd))
        }
        "escalate" | "decision" | "ask_user" | "上奏" | "请示" => {
            cmd.kind = "relay".into();
            cmd.to = "user".into();
            cmd.relay_kind = "decision".into();
            cmd.category = {
                let c = json_str(&v, "category");
                if c.is_empty() {
                    "other".into()
                } else {
                    c
                }
            };
            cmd.question = {
                let q = json_str(&v, "question");
                if q.is_empty() {
                    brief
                } else {
                    q
                }
            };
            cmd.options = json_options(&v);
            Some(ParsedNextAction::Command(cmd))
        }
        _ => None,
    }
}

/// 受阻结论：`BLOCKED: 原因`（自末尾向上找）。兜底用，防 agent 漏调 blocked 工具。
fn parse_blocked(text: &str) -> Option<String> {
    for line in text.lines().rev() {
        let l = line.trim();
        if let Some(r) = l.strip_prefix("BLOCKED:") {
            return Some(r.trim().to_string());
        }
    }
    None
}

// ===== prompt 组装 =====

fn charter_or_default(charter: &str) -> &str {
    if charter.trim().is_empty() {
        "（未设置岗位职责，请根据任务自行判断）"
    } else {
        charter.trim()
    }
}

fn build_scout_prompt(
    app: &AppHandle,
    emp: &Employee,
    scope: &str,
    digest: &str,
    knowledge: &str,
    mem_dir: &str,
) -> String {
    // 数字员工像人一样：能自己做的就自己做（to=self），也可以派合适的同事，或上奏主管——自行判断。
    let self_clause = "能自己做的就把单子接力给「自己」（to=self）亲自做；更适合同事的派同事；只有确需主管拿主意的才上奏（decision）。由你自行判断，怎么高效怎么来。";
    // 巡查阶段只给只读工具（核查用）；行动不再走 relay CLI（弱模型常不执行），改为末尾输出 PLAN 块。
    let tools = tools_manual(app, emp, scope, "", &["kb-search", "ledger-list"]);
    format!(
        "你是数字员工「{name}」。你手头的活都告一段落了，现在像一个尽责的员工一样进行一轮「巡查」，\
看看有没有该推进的新工作。这一轮**只判断、只调度，绝不动手改代码或执行任务**。\n\n\
【你的岗位职责】\n{charter}\n\n\
【常驻任务】\n{directive}\n\n\
【被动 Mind · 巡查检索】\n{memory}\n\n\
【协作账本 · 已登记的单子（避开「处理中/已完成」，勿重复）】\n{digest}\n\n\
{mem_hint}{tools}\
请**用上面的只读工具**核查需求来源与工作目录（列目录/读文件/grep、ledger-list 看账本、kb-search 查历史），\
判断按常驻任务**现在是否有值得推进的事**。账本里标为「待处理」的单子（主管登记或同事转交）通常就是要你推进的，应优先安排。\n\n\
判断完成后，**必须在回复的最末尾输出一个机器可读的「行动块」**——应用会据此自动登记/接力/上奏；\
**只写一段自然语言总结是没有任何效果的**（不会真的登记或接力）。格式如下：\n\
===PLAN===\n\
work | self | <单子唯一key> | <标题> | <一句话交代进展/要点>\n\
work | <同事名> | <单子唯一key> | <标题> | <交代>\n\
discuss | <同事名> | <单子唯一key> | <标题> | <要对齐的接口/协议问题>\n\
decision | user | <单子唯一key> | <标题> | <要主管拿主意的一句话问题>\n\
===END===\n\
规则：\n\
- 每行一件事，用 `|` 分隔 5 段：类型 | 对象 | key | 标题 | 说明。类型只能是 work / discuss / decision。\n\
- {self_clause}\n\
- **能自己判断、能自己做或能派同事做的，一律 work/discuss，别动不动上奏**；只有确实要主管拿主意的\
（方案取舍/优先级/对外承诺/需主管补充信息）才用 decision 呈进御书房，且一件事一行、说明写足，让主管不必再回头问。\n\
- `key` 用该单子的**稳定唯一标识**（同一单子多轮巡查复用同一 key，配合账本去重，禁止两人做同一件事）。\n\
- 可以有多行（多件事）；如果没有任何值得推进的事，行动块里**只写一行** `IDLE`。\n\
再次强调：本轮不要修改任何文件、不要开始开发；所有行动只通过末尾 ===PLAN===…===END=== 块表达。",
        name = emp.name,
        charter = charter_or_default(&emp.charter),
        directive = emp.directive.trim(),
        memory = knowledge,
        digest = digest,
        mem_hint = memory_hint(mem_dir),
        tools = tools,
        self_clause = self_clause,
    )
}

#[allow(clippy::too_many_arguments)]
fn build_dev_prompt(
    app: &AppHandle,
    emp: &Employee,
    scope: &str,
    key: &str,
    title: &str,
    prior_note: &str,
    knowledge: &str,
    mem_dir: &str,
    extra: &str,
) -> String {
    let prior = if prior_note.trim().is_empty() {
        String::new()
    } else {
        format!(
            "\n\n【此前的进展记录（可能是你上一轮或别的同事接力过来的，请在此基础上继续，不要从头重做）】\n{}",
            prior_note.trim()
        )
    };
    let tools = tools_manual(app, emp, scope, key, &["kb-search", "ledger-list"]);
    let preflight_block = if knowledge.trim().is_empty() {
        String::new()
    } else {
        format!("\n\n{}", knowledge.trim())
    };
    format!(
        "你是数字员工「{name}」。主战场是当前会话。你已拿到单子 [{key}] {title}（scope：{scope}），现在只推进这件事。{prior}{extra}\n\n\
【你的岗位职责】\n{charter}{preflight_block}\n\n\
{mem_hint}{tools}\
【出口规则】账本只管占用，会话记录过程。每轮最后必须交出一个明确下一步，系统只按它路由。不要在提示词里调用写入工具，最后一行写 `NEXT_ACTION {{...}}` 即可。\n\
可用 action：continue（继续自己做）、discuss（找同事讨论，填 to/brief）、handoff（交给同事，填 to/brief）、escalate（上奏【用户】，填 question/options/brief）、done（完成，填 summary）、release（释放，填 reason）。\n\
被动 Mind 会在收尾时整理事件，Dream 空闲时再反思学习并判断是否值得沉淀。普通进度不要写入记忆。\n\
最后一行示例：`NEXT_ACTION {{\"action\":\"continue\",\"brief\":\"下一轮继续验证\"}}`。",
        name = emp.name,
        key = key,
        title = title.trim(),
        scope = scope,
        prior = prior,
        extra = extra,
        charter = charter_or_default(&emp.charter),
        preflight_block = preflight_block,
        mem_hint = memory_hint(mem_dir),
        tools = tools,
    )
}

/// 复用历史会话时的「续做」提示：agent 已保留本单全部上下文，只需极简推进指令 + 本轮新增信息，
/// 避免每轮重发完整启动词、也避免它从零开始反复追问用户已给过的内容。
fn build_dev_followup(_app: &AppHandle, _emp: &Employee, task_title: &str, extra: &str) -> String {
    let extra = if extra.trim().is_empty() {
        String::new()
    } else {
        format!("\n{}", extra.trim())
    };
    format!(
        "继续推进当前会话里的这件事：{task_title}。沿用已有上下文，不要重头再来。{extra}\n\n\
过程留在会话里。需要协作、讨论、上奏、完成或释放时，只在最后一行写 `NEXT_ACTION {{...}}`，由系统路由。\n\
action 只能是 continue、discuss、handoff、escalate、done、release。收尾事件会交给被动 Mind，Dream 空闲时再反思学习。",
    )
}

fn followup_extra_with_note(prior_note: &str, extra: &str) -> String {
    let prior_note = prior_note.trim();
    let extra = extra.trim();
    match (prior_note.is_empty(), extra.is_empty()) {
        (true, true) => String::new(),
        (true, false) => extra.to_string(),
        (false, true) => format!("【本次交办】\n{prior_note}"),
        (false, false) => format!("{extra}\n\n【本次交办】\n{prior_note}"),
    }
}

/// 被咨询员工生成「约定/答复」的 prompt。
fn build_reply_prompt(
    peer: &Employee,
    from_name: &str,
    topic: &str,
    body: &str,
    memory: &str,
) -> String {
    format!(
        "你是数字员工「{me}」。同事「{from}」在推进工作时，需要与你就下面的事项达成协议/约定。\
请以本方视角给出明确、可执行的答复（接口约定、字段、协议、时间点等），简洁中文，避免空话。\n\n\
【你的岗位职责】\n{charter}\n\n\
【你的知识与相关记忆】\n{memory}\n\n\
【对方（{from}）想约定的主题】\n{topic}\n\n\
【对方的具体诉求】\n{body}\n\n\
请直接给出你的答复要点。",
        me = peer.name,
        from = from_name,
        charter = charter_or_default(&peer.charter),
        memory = memory,
        topic = topic,
        body = body,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_work_preflight_accepts_bare_json() {
        let parsed = parse_work_preflight(
            r#"{
                "mode": "current",
                "branch": "autofix/example",
                "baseBranch": "",
                "branchCandidates": ["autofix/example：提交信息命中任务关键词"],
                "support": ["从 online 拉修复分支"],
                "summary": "预检完成"
            }"#,
        )
        .expect("bare preflight json should parse");

        assert_eq!(parsed.mode, "current");
        assert_eq!(parsed.branch, "autofix/example");
        assert_eq!(
            parsed.branch_candidates,
            vec!["autofix/example：提交信息命中任务关键词"]
        );
        assert_eq!(parsed.support, vec!["从 online 拉修复分支"]);
    }

    #[test]
    fn parse_work_preflight_accepts_marked_json() {
        let parsed = parse_work_preflight(
            r#"text before
===PREFLIGHT_JSON===
```json
{"mode":"worktree","branch":"","baseBranch":"online","summary":"ready"}
```
===END===
text after"#,
        )
        .expect("marked preflight json should parse");

        assert_eq!(parsed.mode, "worktree");
        assert_eq!(parsed.base_branch, "online");
        assert_eq!(parsed.summary, "ready");
    }

    #[test]
    fn wake_context_blocks_surfaces_edict() {
        let (block, _, extra) = wake_context_blocks(
            "旧进展",
            "【主管已在御书房批阅你上奏的折子】\n主管批示：先别改线上",
        );
        assert!(block.contains("【主管批注"));
        assert!(block.contains("先别改线上"));
        assert!(extra.contains("已并入") || extra.contains("主管批示"));
    }

    #[test]
    fn supervision_forces_do_detects_批示() {
        assert!(supervision_forces_do(
            "",
            "【主管已在御书房批阅你上奏的折子】\n主管批示：先别改"
        ));
        assert!(supervision_forces_do("留中不发", ""));
        assert!(!supervision_forces_do("普通进展", "继续排查"));
    }

    #[test]
    fn parse_wake_decision_accepts_nested_workspace() {
        let d = parse_wake_decision(
            r#"===WAKE_JSON===
{"intent":"do","reason":"继续修","workspace":{"mode":"current","summary":"留在源分支"}}
===END==="#,
        )
        .expect("wake json");
        assert_eq!(d.intent_key(), "do");
        assert_eq!(d.workspace_plan().mode, "current");
        assert_eq!(d.workspace_plan().summary, "留在源分支");
    }

    #[test]
    fn parse_wake_decision_escalate() {
        let d = parse_wake_decision(
            r#"{"intent":"escalate","question":"选哪个分支？","options":["a","b"]}"#,
        )
        .expect("escalate");
        assert_eq!(d.intent_key(), "escalate");
        assert_eq!(d.question, "选哪个分支？");
    }
}
