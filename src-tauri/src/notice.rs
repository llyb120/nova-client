//! 统一协作 Notice 广播：发送方声明处理后 ActionPlan（类 Android PendingIntent）。
//!
//! label 只作展示标签，不驱动流程；真正行为由 expect 里的 Action 决定。

use crate::employees::{self, Decision, Employee};
use crate::threads::now_ms;
use crate::AppState;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use tauri::{AppHandle, Emitter, Manager};

pub const EV_NOTICES: &str = "notices:changed";
/// 兼容旧前端事件名
pub const EV_DECISIONS: &str = "decisions:changed";

// ===== 数据模型 =====

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ActorRef {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl ActorRef {
    pub fn user() -> Self {
        Self {
            kind: "user".into(),
            id: None,
            name: None,
        }
    }
    pub fn system() -> Self {
        Self {
            kind: "system".into(),
            id: None,
            name: None,
        }
    }
    pub fn employee(id: &str, name: &str) -> Self {
        Self {
            kind: "employee".into(),
            id: Some(id.to_string()),
            name: Some(name.to_string()),
        }
    }
    pub fn peer(name: &str) -> Self {
        Self {
            kind: "peer".into(),
            id: None,
            name: Some(name.to_string()),
        }
    }
    pub fn is_user(&self) -> bool {
        self.kind.eq_ignore_ascii_case("user")
    }
    pub fn employee_id(&self) -> Option<&str> {
        if self.kind.eq_ignore_ascii_case("employee") {
            self.id.as_deref()
        } else {
            None
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct NoticeTopic {
    pub scope: String,
    pub mark_key: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct NoticeOption {
    pub id: String,
    pub label: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct NoticeBody {
    #[serde(default)]
    pub brief: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question: Option<String>,
    #[serde(default)]
    pub options: Vec<NoticeOption>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct NoticeHold {
    pub scope: String,
    pub key: String,
    pub owner_employee_id: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Action {
    ResumeClaim {
        employee_id: String,
        scope: String,
        key: String,
        #[serde(default)]
        title: String,
    },
    FailMark {
        scope: String,
        key: String,
        reason: String,
    },
    ReleaseMark {
        scope: String,
        key: String,
        #[serde(default)]
        reason: Option<String>,
        #[serde(default)]
        cooloff_employee_id: Option<String>,
    },
    ClaimFor {
        employee_id: String,
        scope: String,
        key: String,
        #[serde(default)]
        title: String,
        #[serde(default)]
        brief: String,
    },
    InjectContext {
        employee_id: String,
        text: String,
        #[serde(default)]
        scope: String,
        #[serde(default)]
        key: String,
        #[serde(default)]
        thread_id: Option<String>,
    },
    AppendMemory {
        employee_id: String,
        text: String,
        #[serde(default)]
        kind: String,
        #[serde(default)]
        task_id: String,
        #[serde(default)]
        task_title: String,
    },
    Wake {
        employee_id: String,
    },
    Noop,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct NoticeExpect {
    /// none | ack | choice | input
    pub mode: String,
    #[serde(default)]
    pub on_delivered: Vec<Action>,
    #[serde(default)]
    pub on_handled: Vec<Action>,
    #[serde(default)]
    pub on_choice: HashMap<String, Vec<Action>>,
    #[serde(default)]
    pub on_reject: Vec<Action>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct NoticeResponse {
    pub by: ActorRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub choice_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    pub at: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct NoticeMeta {
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub proposed_action: String,
    #[serde(default)]
    pub auto_note: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Notice {
    pub id: String,
    pub from: ActorRef,
    pub to: ActorRef,
    /// decision | work | discuss | report | info —— 仅标签
    pub label: String,
    pub topic: NoticeTopic,
    pub body: NoticeBody,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hold: Option<NoticeHold>,
    pub expect: NoticeExpect,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dedupe_key: Option<String>,
    /// pending | delivered | handled | rejected | withdrawn | expired
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<NoticeResponse>,
    #[serde(default)]
    pub meta: NoticeMeta,
    pub created_at: i64,
    #[serde(default)]
    pub handled_at: i64,
}

#[derive(Serialize, Deserialize, Default)]
struct NoticesFile {
    notices: Vec<Notice>,
    #[serde(default)]
    injections: HashMap<String, String>,
}

/// 持久化 + 待注入上下文（inject_context 的 PendingIntent 落地）
pub struct NoticeStore {
    path: PathBuf,
    pub notices: Vec<Notice>,
    /// emp_id\u{1}scope\u{1}key → 下轮开发注入文本
    pub injections: HashMap<String, String>,
}

impl NoticeStore {
    pub fn load(dir: &PathBuf) -> Self {
        let path = dir.join("employee_notices.json");
        let (notices, injections) = fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<NoticesFile>(&s).ok())
            .map(|f| (f.notices, f.injections))
            .unwrap_or_default();
        NoticeStore {
            path,
            notices,
            injections,
        }
    }

    pub fn save(&self) {
        if let Some(parent) = self.path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let file = NoticesFile {
            notices: self.notices.clone(),
            injections: self.injections.clone(),
        };
        if let Ok(json) = serde_json::to_string_pretty(&file) {
            let _ = fs::write(&self.path, json);
        }
    }

    fn prune(&mut self) {
        const KEEP_ARCHIVED: usize = 120;
        let mut archived: Vec<(i64, String)> = self
            .notices
            .iter()
            .filter(|n| {
                !matches!(
                    n.status.as_str(),
                    "pending" | "delivered"
                )
            })
            .map(|n| (n.handled_at.max(n.created_at), n.id.clone()))
            .collect();
        if archived.len() <= KEEP_ARCHIVED {
            return;
        }
        archived.sort_by(|a, b| b.0.cmp(&a.0));
        let drop: HashSet<String> = archived[KEEP_ARCHIVED..]
            .iter()
            .map(|(_, id)| id.clone())
            .collect();
        self.notices.retain(|n| !drop.contains(&n.id));
    }

    pub fn list(&self) -> Vec<Notice> {
        let mut list = self.notices.clone();
        list.sort_by(|a, b| {
            let pa = if a.status == "pending" || a.status == "delivered" {
                0
            } else {
                1
            };
            let pb = if b.status == "pending" || b.status == "delivered" {
                0
            } else {
                1
            };
            pa.cmp(&pb).then(b.created_at.cmp(&a.created_at))
        });
        list
    }

    pub fn get(&self, id: &str) -> Option<&Notice> {
        self.notices.iter().find(|n| n.id == id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut Notice> {
        self.notices.iter_mut().find(|n| n.id == id)
    }

    pub fn add(&mut self, n: Notice) {
        self.notices.push(n);
        self.prune();
        self.save();
    }

    /// 同 dedupe_key 的 pending 合并；返回是否新建。
    pub fn add_or_merge_pending(&mut self, n: Notice) -> bool {
        if let Some(key) = n.dedupe_key.as_deref().filter(|s| !s.is_empty()) {
            if let Some(existing) = self.notices.iter_mut().find(|x| {
                x.status == "pending"
                    && x.dedupe_key.as_deref() == Some(key)
                    && x.from.employee_id() == n.from.employee_id()
                    && x.topic.scope == n.topic.scope
                    && x.topic.mark_key == n.topic.mark_key
            }) {
                if existing.body.brief.trim().is_empty() {
                    existing.body.brief = n.body.brief;
                } else if !n.body.brief.trim().is_empty()
                    && !existing.body.brief.contains(n.body.brief.trim())
                {
                    existing.body.brief = format!(
                        "{}\n\n【新证据】{}",
                        existing.body.brief.trim(),
                        n.body.brief.trim()
                    );
                }
                if n.body.question.as_ref().is_some_and(|q| !q.trim().is_empty()) {
                    existing.body.question = n.body.question;
                }
                if !n.body.options.is_empty() {
                    existing.body.options = n.body.options;
                }
                if !n.meta.proposed_action.trim().is_empty() {
                    existing.meta.proposed_action = n.meta.proposed_action;
                }
                if !n.meta.auto_note.trim().is_empty() {
                    existing.meta.auto_note = n.meta.auto_note;
                }
                existing.expect = n.expect;
                existing.hold = n.hold.or(existing.hold.clone());
                existing.created_at = now_ms();
                self.save();
                return false;
            }
        }
        self.add(n);
        true
    }

    pub fn pending_hold_for(&self, employee_id: &str, scope: &str, key: &str) -> bool {
        self.notices.iter().any(|n| {
            n.status == "pending"
                && n.hold.as_ref().is_some_and(|h| {
                    h.owner_employee_id == employee_id && h.scope == scope && h.key == key
                })
        })
    }

    pub fn pending_hold_on(&self, scope: &str, key: &str) -> bool {
        self.notices.iter().any(|n| {
            n.status == "pending"
                && n.hold
                    .as_ref()
                    .is_some_and(|h| h.scope == scope && h.key == key)
        })
    }

    pub fn withdraw_for(&mut self, scope: &str, key: &str) -> usize {
        let now = now_ms();
        let mut n = 0;
        for notice in self.notices.iter_mut() {
            if notice.status == "pending"
                && notice.topic.scope == scope
                && notice.topic.mark_key == key
            {
                notice.status = "withdrawn".into();
                notice.handled_at = now;
                n += 1;
            }
        }
        if n > 0 {
            self.prune();
            self.save();
        }
        n
    }

    pub fn retain_employee(&mut self, employee_id: &str) {
        self.notices.retain(|n| n.from.employee_id() != Some(employee_id));
        self.injections
            .retain(|k, _| !k.starts_with(&format!("{employee_id}\u{1}")));
        self.save();
    }

    /// 物理删除一条 Notice（御书房手动清理已批阅等归档记录）。
    pub fn remove(&mut self, id: &str) -> bool {
        let before = self.notices.len();
        self.notices.retain(|n| n.id != id);
        if self.notices.len() < before {
            self.save();
            true
        } else {
            false
        }
    }

    fn injection_key(employee_id: &str, scope: &str, key: &str) -> String {
        format!("{employee_id}\u{1}{scope}\u{1}{key}")
    }

    pub fn put_injection(&mut self, employee_id: &str, scope: &str, key: &str, text: &str) {
        if text.trim().is_empty() {
            return;
        }
        let k = Self::injection_key(employee_id, scope, key);
        let entry = self.injections.entry(k).or_default();
        if entry.is_empty() {
            *entry = text.to_string();
        } else {
            entry.push_str(text);
        }
        self.save();
    }

    pub fn take_injection(&mut self, employee_id: &str, scope: &str, key: &str) -> Option<String> {
        let k = Self::injection_key(employee_id, scope, key);
        let v = self.injections.remove(&k);
        if v.is_some() {
            self.save();
        }
        v.filter(|s| !s.trim().is_empty())
    }

    /// 是否有待消费的注入（用于心跳破例起床）
    pub fn has_injection_for(&self, employee_id: &str) -> bool {
        let prefix = format!("{employee_id}\u{1}");
        self.injections.keys().any(|k| k.starts_with(&prefix))
    }
}

// ===== 默认 ActionPlan 模板 =====

fn opt_id_label(label: &str) -> NoticeOption {
    let id = label
        .chars()
        .take(24)
        .collect::<String>()
        .replace([' ', '\n', '\t'], "_");
    NoticeOption {
        id: if id.is_empty() {
            uuid::Uuid::new_v4().to_string()
        } else {
            id
        },
        label: label.to_string(),
    }
}

pub fn options_from_labels(labels: &[String]) -> Vec<NoticeOption> {
    labels
        .iter()
        .filter(|s| !s.trim().is_empty())
        .map(|s| opt_id_label(s.trim()))
        .collect()
}

/// 上奏用户：choice + hold；准奏/留中注入并唤醒，驳回 fail。
pub fn template_decision(
    sender: &Employee,
    scope: &str,
    key: &str,
    title: &str,
) -> (Option<NoticeHold>, NoticeExpect) {
    let emp_id = sender.id.clone();
    let hold = Some(NoticeHold {
        scope: scope.to_string(),
        key: key.to_string(),
        owner_employee_id: emp_id.clone(),
    });
    let resume = Action::ResumeClaim {
        employee_id: emp_id.clone(),
        scope: scope.to_string(),
        key: key.to_string(),
        title: title.to_string(),
    };
    let wake = Action::Wake {
        employee_id: emp_id.clone(),
    };
    // inject 文本在 respond 时按批复动态填入；模板里放占位，respond 会改写
    let inject_approve = Action::InjectContext {
        employee_id: emp_id.clone(),
        text: String::new(),
        scope: scope.to_string(),
        key: key.to_string(),
        thread_id: None,
    };
    let expect = NoticeExpect {
        mode: "choice".into(),
        on_delivered: vec![],
        on_handled: vec![inject_approve.clone(), resume.clone(), wake.clone()],
        on_choice: HashMap::from([
            (
                "approve".into(),
                vec![inject_approve.clone(), resume.clone(), wake.clone()],
            ),
            (
                "shelve".into(),
                vec![inject_approve, resume, wake.clone()],
            ),
        ]),
        on_reject: vec![
            Action::FailMark {
                scope: scope.to_string(),
                key: key.to_string(),
                reason: String::new(),
            },
            Action::AppendMemory {
                employee_id: emp_id,
                text: String::new(),
                kind: "supervision".into(),
                task_id: key.to_string(),
                task_title: title.to_string(),
            },
            wake,
        ],
    };
    (hold, expect)
}

/// 接力：投递即 release 发送方 + claim 目标 + wake。
pub fn template_work(
    from: &Employee,
    to: &Employee,
    from_scope: &str,
    to_scope: &str,
    key: &str,
    title: &str,
    brief: &str,
    same_person: bool,
) -> NoticeExpect {
    let mut on_delivered = Vec::new();
    if !same_person {
        on_delivered.push(Action::ReleaseMark {
            scope: from_scope.to_string(),
            key: key.to_string(),
            reason: None,
            cooloff_employee_id: Some(from.id.clone()),
        });
    }
    on_delivered.push(Action::ClaimFor {
        employee_id: to.id.clone(),
        scope: to_scope.to_string(),
        key: key.to_string(),
        title: title.to_string(),
        brief: brief.to_string(),
    });
    on_delivered.push(Action::Wake {
        employee_id: to.id.clone(),
    });
    NoticeExpect {
        mode: "none".into(),
        on_delivered,
        on_handled: vec![],
        on_choice: HashMap::new(),
        on_reject: vec![],
    }
}

/// 讨论：串行交棒。去程把棒交给 B；B 答复后回程把意见注入 A 并交还。
pub fn template_discuss(
    from: &Employee,
    to: &Employee,
    from_scope: &str,
    to_scope: &str,
    key: &str,
    title: &str,
    question_brief: &str,
) -> NoticeExpect {
    let q = if question_brief.trim().is_empty() {
        title.to_string()
    } else {
        question_brief.trim().to_string()
    };
    let inject_b = Action::InjectContext {
        employee_id: to.id.clone(),
        text: format!(
            "\n\n【同事 {} 发起讨论——请只回答这一问题，答完本轮即可；系统会把你的答复交还给对方】\n问题：{q}\n",
            from.name
        ),
        scope: to_scope.to_string(),
        key: key.to_string(),
        thread_id: None,
    };
    let on_delivered = vec![
        Action::ReleaseMark {
            scope: from_scope.to_string(),
            key: key.to_string(),
            reason: Some("讨论交棒，等待对方答复".into()),
            cooloff_employee_id: Some(from.id.clone()),
        },
        Action::ClaimFor {
            employee_id: to.id.clone(),
            scope: to_scope.to_string(),
            key: key.to_string(),
            title: title.to_string(),
            brief: q.clone(),
        },
        inject_b,
        Action::Wake {
            employee_id: to.id.clone(),
        },
    ];
    let on_handled = vec![
        Action::InjectContext {
            employee_id: from.id.clone(),
            text: String::new(), // respond 时填入 B 的答复
            scope: from_scope.to_string(),
            key: key.to_string(),
            thread_id: None,
        },
        Action::ReleaseMark {
            scope: to_scope.to_string(),
            key: key.to_string(),
            reason: Some("讨论完毕，交还发起方".into()),
            cooloff_employee_id: Some(to.id.clone()),
        },
        Action::ResumeClaim {
            employee_id: from.id.clone(),
            scope: from_scope.to_string(),
            key: key.to_string(),
            title: title.to_string(),
        },
        Action::Wake {
            employee_id: from.id.clone(),
        },
    ];
    NoticeExpect {
        mode: "input".into(),
        on_delivered,
        on_handled,
        on_choice: HashMap::new(),
        on_reject: vec![],
    }
}

/// 汇报：ack/input，不 resume_claim。
pub fn template_report(sender: &Employee, key: &str, title: &str) -> NoticeExpect {
    NoticeExpect {
        mode: "ack".into(),
        on_delivered: vec![],
        on_handled: vec![Action::AppendMemory {
            employee_id: sender.id.clone(),
            text: String::new(),
            kind: "supervision".into(),
            task_id: key.to_string(),
            task_title: title.to_string(),
        }],
        on_choice: HashMap::new(),
        on_reject: vec![],
    }
}

// ===== 构建 / 投递 / 响应 =====

pub struct EmitParams {
    pub from: ActorRef,
    pub to: ActorRef,
    pub label: String,
    pub topic: NoticeTopic,
    pub body: NoticeBody,
    pub hold: Option<NoticeHold>,
    pub expect: NoticeExpect,
    pub dedupe_key: Option<String>,
    pub meta: NoticeMeta,
}

pub fn emit_notice(app: &AppHandle, params: EmitParams) -> Option<Notice> {
    let id = uuid::Uuid::new_v4().to_string();
    let mut notice = Notice {
        id: id.clone(),
        from: params.from,
        to: params.to,
        label: params.label,
        topic: params.topic,
        body: params.body,
        hold: params.hold,
        expect: params.expect,
        dedupe_key: params.dedupe_key,
        status: "pending".into(),
        response: None,
        meta: params.meta,
        created_at: now_ms(),
        handled_at: 0,
    };

    let added = {
        let st = app.state::<AppState>();
        let mut store = st.notices.lock().unwrap();
        store.add_or_merge_pending(notice.clone())
    };
    if !added {
        // 合并到已有 pending：取回最新那条
        let st = app.state::<AppState>();
        let store = st.notices.lock().unwrap();
        return store
            .notices
            .iter()
            .rev()
            .find(|n| {
                n.status == "pending"
                    && n.dedupe_key == notice.dedupe_key
                    && n.topic.mark_key == notice.topic.mark_key
            })
            .cloned();
    }

    // mode=none：投递即执行 onDelivered 并标 delivered/handled
    if notice.expect.mode == "none" {
        let actions = notice.expect.on_delivered.clone();
        {
            let st = app.state::<AppState>();
            let mut store = st.notices.lock().unwrap();
            if let Some(n) = store.get_mut(&id) {
                n.status = "handled".into();
                n.handled_at = now_ms();
                n.response = Some(NoticeResponse {
                    by: ActorRef::system(),
                    choice_id: None,
                    text: Some("delivered".into()),
                    at: now_ms(),
                });
                notice = n.clone();
            }
            store.save();
        }
        execute_actions(app, &notice, &actions, None);
    } else if !notice.expect.on_delivered.is_empty() {
        let actions = notice.expect.on_delivered.clone();
        {
            let st = app.state::<AppState>();
            let mut store = st.notices.lock().unwrap();
            if let Some(n) = store.get_mut(&id) {
                n.status = "delivered".into();
                notice = n.clone();
            }
            store.save();
        }
        execute_actions(app, &notice, &actions, None);
    }

    emit_changed(app);
    if notice.to.is_user() && matches!(notice.label.as_str(), "decision" | "report") {
        notify_user_notice(app, &notice);
    }
    Some(notice)
}

pub struct RespondParams {
    pub choice_id: Option<String>,
    pub text: Option<String>,
    pub reject: bool,
}

pub fn respond_notice(app: &AppHandle, id: &str, by: ActorRef, params: RespondParams) -> Result<Notice, String> {
    let notice = {
        let st = app.state::<AppState>();
        let store = st.notices.lock().unwrap();
        store
            .get(id)
            .cloned()
            .ok_or_else(|| "通知不存在".to_string())?
    };
    if !matches!(notice.status.as_str(), "pending" | "delivered") {
        return Err("通知已处理".into());
    }

    let text = params.text.clone().unwrap_or_default();
    let choice = params.choice_id.clone().unwrap_or_default();
    let now = now_ms();

    let mut actions = if params.reject {
        notice.expect.on_reject.clone()
    } else if !choice.is_empty() {
        notice
            .expect
            .on_choice
            .get(&choice)
            .cloned()
            .unwrap_or_else(|| notice.expect.on_handled.clone())
    } else {
        notice.expect.on_handled.clone()
    };

    // 把批复文本填进占位 Action
    fill_action_placeholders(&mut actions, &notice, &text, params.reject, &choice);

    let new_status = if params.reject { "rejected" } else { "handled" };
    let updated = {
        let st = app.state::<AppState>();
        let mut store = st.notices.lock().unwrap();
        let n = store.get_mut(id).ok_or_else(|| "通知不存在".to_string())?;
        n.status = new_status.into();
        n.handled_at = now;
        n.response = Some(NoticeResponse {
            by: by.clone(),
            choice_id: params.choice_id.clone(),
            text: params.text.clone(),
            at: now,
        });
        // 清 hold（逻辑上已处理完）
        n.hold = None;
        let snap = n.clone();
        store.prune();
        store.save();
        snap
    };

    execute_actions(app, &updated, &actions, Some(&text));
    emit_changed(app);

    // Mind / Wake 副作用
    if updated.meta.source == "mind" {
        let mut dec = notice_to_decision(&updated);
        // mind::on_decision 认 rejected/shelved；handled+shelve 已映射
        if params.reject {
            dec.status = "rejected".into();
        } else if choice == "shelve" {
            dec.status = "shelved".into();
        }
        crate::mind::on_decision(app, &dec);
    } else if updated.meta.source == "wake" {
        let mut dec = notice_to_decision(&updated);
        if params.reject {
            dec.status = "rejected".into();
        } else if choice == "shelve" {
            dec.status = "shelved".into();
        } else {
            dec.status = "resolved".into();
        }
        employees::finalize_wake_thread_decision(app, &dec);
        if !params.reject {
            crate::mind::preempt_for_work(app, updated.from.employee_id().unwrap_or(""));
        }
    } else if updated.label == "decision" && !params.reject {
        if let Some(eid) = updated.from.employee_id() {
            crate::mind::preempt_for_work(app, eid);
        }
    }

    Ok(updated)
}

fn fill_action_placeholders(
    actions: &mut [Action],
    notice: &Notice,
    text: &str,
    reject: bool,
    choice: &str,
) {
    let question = notice
        .body
        .question
        .clone()
        .unwrap_or_else(|| notice.body.brief.clone());

    // 讨论回程：意见 = 对方答复原文
    if notice.label == "discuss" && !reject {
        let reply = if text.trim().is_empty() {
            "（对方未给出明确答复）".to_string()
        } else {
            text.trim().to_string()
        };
        let peer = notice
            .response
            .as_ref()
            .and_then(|r| r.by.name.clone())
            .or_else(|| notice.to.name.clone())
            .unwrap_or_else(|| "同事".into());
        let inject_text = format!(
            "\n\n【同事 {peer} 已答复你的讨论】\n- 你的问题：{question}\n  答复：{reply}\n请参考该意见继续推进；不要再就同一问题重复发起讨论，除非出现新的不确定点。"
        );
        let mem_text = format!("我与 {peer} 讨论：{question}\n对方答复：{reply}");
        for a in actions.iter_mut() {
            match a {
                Action::InjectContext {
                    text: t,
                    thread_id,
                    ..
                } if t.is_empty() => {
                    *t = inject_text.clone();
                    if thread_id.is_none() {
                        *thread_id = notice.topic.thread_id.clone();
                    }
                }
                Action::AppendMemory { text: t, .. } if t.is_empty() => {
                    *t = mem_text.clone();
                }
                _ => {}
            }
        }
        return;
    }

    let inject_text = if reject {
        format!(
            "\n\n【主管已在御书房批阅你上奏的折子】\n- 你奏问：{question}\n  主管驳回：{}\n请停止推进这张单，不要再就同一问题上奏。",
            if text.trim().is_empty() {
                "驳回"
            } else {
                text.trim()
            }
        )
    } else if choice == "shelve" || text.trim().is_empty() && choice == "shelve" {
        format!(
            "\n\n【主管已在御书房批阅你上奏的折子】\n- 你奏问：{question}\n  批示：留中不发（主管未作决定）。请按你的专业判断选择最稳妥的方案推进，把理由写进总结；不要再就同一问题上奏。\n请**放下其他事，第一时间遵照批示把这件事办完**；只有真正遇到全新的、你无法自行判断的问题时，才再上奏。\n系统会先以 Wake 阅读本批注再决定如何进入 Do，禁止跳过批注无脑开工。"
        )
    } else {
        let answer = if text.trim().is_empty() {
            "准奏".to_string()
        } else {
            text.trim().to_string()
        };
        let mut s = format!(
            "\n\n【主管已在御书房批阅你上奏的折子】\n- 你奏问：{question}\n  主管批示：{answer}"
        );
        if !notice.meta.proposed_action.trim().is_empty() {
            s.push_str(&format!(
                "\n  建议动作：{}",
                notice.meta.proposed_action.trim()
            ));
        }
        s.push_str(
            "\n请**放下其他事，第一时间遵照批示把这件事办完**（结合此前的背景与进展），不要再就同一问题重复上奏；只有真正遇到全新的、你无法自行判断的问题时，才再上奏。\n系统会先以 Wake 阅读本批注再决定如何进入 Do，禁止跳过批注无脑开工。",
        );
        s
    };

    let mem_text = if reject {
        format!(
            "我曾奏问：{question}\n主管驳回：{}\n这张单已停止推进。",
            if text.trim().is_empty() {
                "驳回"
            } else {
                text.trim()
            }
        )
    } else if choice == "shelve" {
        format!(
            "我奏问：{question}\n主管留中不发（让我自行斟酌）。复盘提示：这类问题主管认为我可以自己定，下次同类情况直接按专业判断办，不必上奏。"
        )
    } else if notice.label == "report" && !text.trim().is_empty() {
        format!(
            "完工汇报：{}\n【用户】批阅：{}",
            question,
            text.trim()
        )
    } else {
        format!(
            "我奏问：{question}\n主管批示：{}\n复盘提示：记住主管在这类问题上的口径，下次同类情况按此办理，不必再问。",
            if text.trim().is_empty() {
                "准奏"
            } else {
                text.trim()
            }
        )
    };

    for a in actions.iter_mut() {
        match a {
            Action::InjectContext {
                text: t,
                thread_id,
                ..
            } => {
                if t.is_empty() {
                    *t = inject_text.clone();
                }
                if thread_id.is_none() {
                    *thread_id = notice.topic.thread_id.clone();
                }
            }
            Action::FailMark { reason, .. } => {
                if reason.is_empty() {
                    *reason = if text.trim().is_empty() {
                        "主管驳回，该单停止推进。".into()
                    } else {
                        text.trim().to_string()
                    };
                }
            }
            Action::AppendMemory { text: t, .. } => {
                if t.is_empty() {
                    if notice.label == "report"
                        && (text.trim().is_empty() || text.trim().eq_ignore_ascii_case("ack"))
                    {
                        // 已读不写记忆
                    } else {
                        *t = mem_text.clone();
                    }
                }
            }
            _ => {}
        }
    }
}

pub fn withdraw_notices(app: &AppHandle, scope: &str, key: &str) -> usize {
    let n = {
        let st = app.state::<AppState>();
        let mut store = st.notices.lock().unwrap();
        store.withdraw_for(scope, key)
    };
    if n > 0 {
        emit_changed(app);
    }
    n
}

pub fn pending_hold_for(app: &AppHandle, employee_id: &str, scope: &str, key: &str) -> bool {
    let st = app.state::<AppState>();
    let store = st.notices.lock().unwrap();
    store.pending_hold_for(employee_id, scope, key)
}

pub fn pending_hold_on(app: &AppHandle, scope: &str, key: &str) -> bool {
    let st = app.state::<AppState>();
    let store = st.notices.lock().unwrap();
    store.pending_hold_on(scope, key)
}

/// 发给该员工、待答复的讨论 Notice（串行交棒回程用）。
pub fn pending_discuss_id(app: &AppHandle, employee_id: &str, scope: &str, key: &str) -> Option<String> {
    let st = app.state::<AppState>();
    let store = st.notices.lock().unwrap();
    store
        .notices
        .iter()
        .rev()
        .find(|n| {
            matches!(n.status.as_str(), "pending" | "delivered")
                && n.label == "discuss"
                && n.expect.mode == "input"
                && n.to.employee_id() == Some(employee_id)
                && n.topic.scope == scope
                && n.topic.mark_key == key
        })
        .map(|n| n.id.clone())
}

pub fn take_injection(app: &AppHandle, employee_id: &str, scope: &str, key: &str) -> Option<String> {
    let st = app.state::<AppState>();
    let mut store = st.notices.lock().unwrap();
    store.take_injection(employee_id, scope, key)
}

pub fn has_pending_work_signal(app: &AppHandle, employee_id: &str) -> bool {
    let st = app.state::<AppState>();
    let store = st.notices.lock().unwrap();
    store.has_injection_for(employee_id)
}

fn emit_changed(app: &AppHandle) {
    let _ = app.emit(EV_NOTICES, json!({}));
    let _ = app.emit(EV_DECISIONS, json!({}));
    let _ = app.emit(employees::EV_EMPLOYEES, json!({}));
}

fn notify_user_notice(app: &AppHandle, notice: &Notice) {
    if notice.label != "decision" {
        return;
    }
    let emp_name = notice
        .from
        .name
        .clone()
        .unwrap_or_else(|| "员工".into());
    let question = notice
        .body
        .question
        .clone()
        .unwrap_or_else(|| notice.body.brief.clone());
    employees::notify_decision_toast(app, &emp_name, &question);
}

// ===== Action 执行 =====

fn execute_actions(app: &AppHandle, notice: &Notice, actions: &[Action], _response_text: Option<&str>) {
    for action in actions {
        match action {
            Action::Noop => {}
            Action::Wake { employee_id } => {
                employees::summon_employee(app, employee_id);
            }
            Action::InjectContext {
                employee_id,
                text,
                scope,
                key,
                ..
            } => {
                let scope = if scope.is_empty() {
                    notice.topic.scope.clone()
                } else {
                    scope.clone()
                };
                let key = if key.is_empty() {
                    notice.topic.mark_key.clone()
                } else {
                    key.clone()
                };
                let st = app.state::<AppState>();
                st.notices
                    .lock()
                    .unwrap()
                    .put_injection(employee_id, &scope, &key, text);
            }
            Action::AppendMemory {
                employee_id,
                text,
                kind,
                task_id,
                task_title,
            } => {
                if text.trim().is_empty() {
                    continue;
                }
                let tid = if task_id.is_empty() {
                    notice.topic.mark_key.as_str()
                } else {
                    task_id.as_str()
                };
                let title = if task_title.is_empty() {
                    notice.topic.title.as_str()
                } else {
                    task_title.as_str()
                };
                let k = if kind.is_empty() { "supervision" } else { kind };
                employees::notice_append_event(app, employee_id, tid, title, text, k);
            }
            Action::ResumeClaim {
                employee_id,
                scope,
                key,
                title,
            } => {
                let app2 = app.clone();
                let employee_id = employee_id.clone();
                let scope = scope.clone();
                let key = key.clone();
                let title = if title.is_empty() {
                    notice.topic.title.clone()
                } else {
                    title.clone()
                };
                tauri::async_runtime::spawn(async move {
                    employees::notice_claim_mark(&app2, &employee_id, &scope, &key, &title, "").await;
                });
            }
            Action::ClaimFor {
                employee_id,
                scope,
                key,
                title,
                brief,
            } => {
                let app2 = app.clone();
                let employee_id = employee_id.clone();
                let scope = scope.clone();
                let key = key.clone();
                let title = if title.is_empty() {
                    notice.topic.title.clone()
                } else {
                    title.clone()
                };
                let brief = brief.clone();
                tauri::async_runtime::spawn(async move {
                    employees::notice_claim_mark(
                        &app2, &employee_id, &scope, &key, &title, &brief,
                    )
                    .await;
                });
            }
            Action::FailMark {
                scope,
                key,
                reason,
            } => {
                let app2 = app.clone();
                let scope = scope.clone();
                let key = key.clone();
                let reason = reason.clone();
                let emp_id = notice.from.employee_id().unwrap_or("").to_string();
                tauri::async_runtime::spawn(async move {
                    employees::notice_fail_mark(&app2, &emp_id, &scope, &key, &reason).await;
                });
            }
            Action::ReleaseMark {
                scope,
                key,
                reason,
                cooloff_employee_id,
            } => {
                let app2 = app.clone();
                let scope = scope.clone();
                let key = key.clone();
                let reason = reason.clone();
                let cooloff = cooloff_employee_id.clone();
                tauri::async_runtime::spawn(async move {
                    employees::notice_release_mark(
                        &app2,
                        &scope,
                        &key,
                        reason.as_deref(),
                        cooloff.as_deref(),
                    )
                    .await;
                });
            }
        }
    }
}

// ===== Decision 投影（兼容旧 UI / mind）=====

pub fn notice_to_decision(n: &Notice) -> Decision {
    let (status, answer, resolved_at) = match n.status.as_str() {
        "pending" | "delivered" => {
            if n.label == "report" {
                ("report".into(), None, 0)
            } else {
                ("pending".into(), None, 0)
            }
        }
        "handled" => {
            let text = n.response.as_ref().and_then(|r| r.text.clone());
            let choice = n
                .response
                .as_ref()
                .and_then(|r| r.choice_id.clone())
                .unwrap_or_default();
            if n.label == "report" {
                if text.as_ref().is_some_and(|t| !t.trim().is_empty())
                    && text.as_deref() != Some("ack")
                {
                    ("reviewed".into(), text, n.handled_at)
                } else {
                    ("read".into(), text, n.handled_at)
                }
            } else if choice == "shelve" {
                ("shelved".into(), text, n.handled_at)
            } else {
                // 已执行 ActionPlan，等价于旧「已领旨」
                ("consumed".into(), text, n.handled_at)
            }
        }
        "rejected" => (
            "rejected".into(),
            n.response.as_ref().and_then(|r| r.text.clone()),
            n.handled_at,
        ),
        "withdrawn" => ("withdrawn".into(), None, n.handled_at),
        other => (other.to_string(), None, n.handled_at),
    };

    let options: Vec<String> = n.body.options.iter().map(|o| o.label.clone()).collect();
    Decision {
        id: n.id.clone(),
        employee_id: n.from.id.clone().unwrap_or_default(),
        employee_name: n.from.name.clone().unwrap_or_default(),
        scope: n.topic.scope.clone(),
        mark_key: n.topic.mark_key.clone(),
        task_title: n.topic.title.clone(),
        thread_id: n.topic.thread_id.clone(),
        brief: n.body.brief.clone(),
        category: if n.label == "report" {
            "report".into()
        } else if n.meta.category.is_empty() {
            "other".into()
        } else {
            n.meta.category.clone()
        },
        question: n
            .body
            .question
            .clone()
            .unwrap_or_else(|| n.body.brief.clone()),
        options,
        blocker_signature: n.dedupe_key.clone().unwrap_or_default(),
        proposed_action: n.meta.proposed_action.clone(),
        auto_note: n.meta.auto_note.clone(),
        source: if n.meta.source.is_empty() {
            "employee".into()
        } else {
            n.meta.source.clone()
        },
        status,
        answer,
        created_at: n.created_at,
        resolved_at,
    }
}

pub fn list_as_decisions(app: &AppHandle) -> Vec<Decision> {
    let st = app.state::<AppState>();
    let mut list: Vec<Decision> = st
        .notices
        .lock()
        .unwrap()
        .list()
        .iter()
        .map(notice_to_decision)
        .collect();
    // 合并尚未迁移的旧 Decision 存档
    let legacy = st.decisions.lock().unwrap().list();
    for d in legacy {
        if !list.iter().any(|x| x.id == d.id) {
            list.push(d);
        }
    }
    list.sort_by(|a, b| {
        let pa = if a.status == "pending" || a.status == "report" {
            0
        } else {
            1
        };
        let pb = if b.status == "pending" || b.status == "report" {
            0
        } else {
            1
        };
        pa.cmp(&pb).then(b.created_at.cmp(&a.created_at))
    });
    list
}

/// 从旧 DecisionStore 迁移 pending/report 到 Notice（启动时调用一次）。
pub fn migrate_from_decisions(app: &AppHandle) {
    let legacy: Vec<Decision> = {
        let st = app.state::<AppState>();
        let d = st.decisions.lock().unwrap();
        d.decisions.clone()
    };
    if legacy.is_empty() {
        return;
    }
    let mut migrated = 0;
    for d in legacy {
        if !matches!(d.status.as_str(), "pending" | "report" | "resolved" | "shelved" | "rejected")
        {
            continue;
        }
        let exists = {
            let st = app.state::<AppState>();
            let store = st.notices.lock().unwrap();
            store.notices.iter().any(|n| n.id == d.id)
        };
        if exists {
            continue;
        }
        let emp = employees::find_employee(app, &d.employee_id);
        let Some(emp) = emp else { continue };
        let (hold, mut expect) = if d.status == "report" || d.category == "report" {
            (None, template_report(&emp, &d.mark_key, &d.task_title))
        } else {
            template_decision(&emp, &d.scope, &d.mark_key, &d.task_title)
        };
        if d.status == "report" || d.category == "report" {
            expect.mode = "ack".into();
        }
        let status = match d.status.as_str() {
            "report" => "pending",
            "resolved" | "shelved" => "handled", // 旧待领旨：注入后由心跳消费；这里标 handled 并补注入
            "rejected" => "rejected",
            _ => "pending",
        };
        let notice = Notice {
            id: d.id.clone(),
            from: ActorRef::employee(&d.employee_id, &d.employee_name),
            to: ActorRef::user(),
            label: if d.status == "report" || d.category == "report" {
                "report".into()
            } else {
                "decision".into()
            },
            topic: NoticeTopic {
                scope: d.scope.clone(),
                mark_key: d.mark_key.clone(),
                title: d.task_title.clone(),
                thread_id: d.thread_id.clone(),
            },
            body: NoticeBody {
                brief: d.brief.clone(),
                question: Some(d.question.clone()),
                options: options_from_labels(&d.options),
            },
            hold: if status == "pending" { hold } else { None },
            expect,
            dedupe_key: if d.blocker_signature.is_empty() {
                None
            } else {
                Some(d.blocker_signature.clone())
            },
            status: status.into(),
            response: d.answer.as_ref().map(|a| NoticeResponse {
                by: ActorRef::user(),
                choice_id: match d.status.as_str() {
                    "shelved" => Some("shelve".into()),
                    "rejected" => Some("reject".into()),
                    _ => Some("approve".into()),
                },
                text: Some(a.clone()),
                at: d.resolved_at,
            }),
            meta: NoticeMeta {
                category: d.category.clone(),
                source: d.source.clone(),
                proposed_action: d.proposed_action.clone(),
                auto_note: d.auto_note.clone(),
            },
            created_at: d.created_at,
            handled_at: d.resolved_at,
        };
        // 旧 resolved/shelved：补注入，让员工下一跳继续（替代领旨队列）
        if matches!(d.status.as_str(), "resolved" | "shelved") {
            let mut actions = if d.status == "shelved" {
                notice
                    .expect
                    .on_choice
                    .get("shelve")
                    .cloned()
                    .unwrap_or_else(|| notice.expect.on_handled.clone())
            } else {
                notice.expect.on_handled.clone()
            };
            let choice = if d.status == "shelved" { "shelve" } else { "approve" };
            fill_action_placeholders(
                &mut actions,
                &notice,
                d.answer.as_deref().unwrap_or(""),
                false,
                choice,
            );
            {
                let st = app.state::<AppState>();
                st.notices.lock().unwrap().add(notice.clone());
            }
            execute_actions(app, &notice, &actions, d.answer.as_deref());
        } else if d.status == "rejected" {
            let mut actions = notice.expect.on_reject.clone();
            fill_action_placeholders(
                &mut actions,
                &notice,
                d.answer.as_deref().unwrap_or(""),
                true,
                "reject",
            );
            {
                let st = app.state::<AppState>();
                st.notices.lock().unwrap().add(notice.clone());
            }
            execute_actions(app, &notice, &actions, d.answer.as_deref());
        } else {
            let st = app.state::<AppState>();
            st.notices.lock().unwrap().add(notice);
        }
        migrated += 1;
    }
    if migrated > 0 {
        // 清掉已迁移的活跃旧折，避免双源
        {
            let st = app.state::<AppState>();
            let mut dec = st.decisions.lock().unwrap();
            dec.decisions.retain(|d| {
                !matches!(
                    d.status.as_str(),
                    "pending" | "report" | "resolved" | "shelved" | "rejected"
                )
            });
            dec.save();
        }
        emit_changed(app);
    }
}

/// 便捷：发上奏 Notice
pub fn emit_decision(
    app: &AppHandle,
    emp: &Employee,
    scope: &str,
    key: &str,
    title: &str,
    brief: &str,
    question: &str,
    category: &str,
    options: Vec<String>,
    thread_id: Option<String>,
    dedupe_key: Option<String>,
    source: &str,
    proposed_action: &str,
    auto_note: &str,
) -> Option<Notice> {
    let (hold, expect) = template_decision(emp, scope, key, title);
    emit_notice(
        app,
        EmitParams {
            from: ActorRef::employee(&emp.id, &emp.name),
            to: ActorRef::user(),
            label: "decision".into(),
            topic: NoticeTopic {
                scope: scope.to_string(),
                mark_key: key.to_string(),
                title: title.to_string(),
                thread_id,
            },
            body: NoticeBody {
                brief: brief.to_string(),
                question: Some(question.to_string()),
                options: options_from_labels(&options),
            },
            hold,
            expect,
            dedupe_key,
            meta: NoticeMeta {
                category: category.to_string(),
                source: source.to_string(),
                proposed_action: proposed_action.to_string(),
                auto_note: auto_note.to_string(),
            },
        },
    )
}

pub fn emit_report(
    app: &AppHandle,
    emp: &Employee,
    scope: &str,
    key: &str,
    title: &str,
    summary: &str,
    thread_id: Option<String>,
) -> Option<Notice> {
    let expect = template_report(emp, key, title);
    emit_notice(
        app,
        EmitParams {
            from: ActorRef::employee(&emp.id, &emp.name),
            to: ActorRef::user(),
            label: "report".into(),
            topic: NoticeTopic {
                scope: scope.to_string(),
                mark_key: key.to_string(),
                title: title.to_string(),
                thread_id,
            },
            body: NoticeBody {
                brief: String::new(),
                question: Some(summary.to_string()),
                options: vec![],
            },
            hold: None,
            expect,
            dedupe_key: None,
            meta: NoticeMeta {
                category: "report".into(),
                source: "employee".into(),
                proposed_action: String::new(),
                auto_note: String::new(),
            },
        },
    )
}
