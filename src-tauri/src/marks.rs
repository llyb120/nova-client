//! 共享标记账本（Marks）：数字员工协作的「门锁」。
//!
//! 它只回答一个问题：这张单现在会不会撞车。
//!
//! 工作过程、判断、讨论、上奏和接力都留在会话里。账本只保存 scope/key、
//! 当前占用人、对应会话和租约。`claim` 是后端 Mutex 保护下的原子 CAS：
//! 已完成→跳过；他人持有且租约未过期→跳过；租约过期 / open / failed→接管。

use crate::threads::{now_ms, PromptImage};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

pub const EV_MARKS: &str = "marks:changed";

/// 一条标记账本条目：一个外部实体的占用锁。
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Mark {
    /// 命名空间 / 看板（如 "requirements"），隔离不同来源，避免 key 跨域冲突
    pub scope: String,
    /// 该来源内的唯一标识（如需求单号 "REQ-123"）
    pub key: String,
    #[serde(default)]
    pub title: String,
    /// open（待处理）| claimed（处理中）| done（完成）| failed（失败）
    pub status: String,
    /// 认领的员工 id
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub owner_name: Option<String>,
    /// 旧字段：历史版本曾把交接备注写进账本。新模型下只作兼容读取，不再承担过程记录。
    #[serde(default)]
    pub note: String,
    /// 当前处理这张单的会话。过程在会话里，账本只保存指针。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    /// 主管交办时随单子带上的图片/文件附件。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<PromptImage>,
    /// 租约到期时间（ms）。claimed 状态下超过此刻即可被别人接管；0 = 无租约
    #[serde(default)]
    pub lease_until: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

/// 认领结果。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ClaimOutcome {
    /// 认领成功（新建 / 接管 / 续租）
    Acquired,
    /// 已被他人认领且租约未过期（互斥，跳过）
    Taken,
    /// 已完成（去重，跳过）
    Done,
    /// 已失败（保留记录，需用户手动重新激活）
    Failed,
}

#[derive(Serialize, Deserialize, Default)]
struct MarksFile {
    marks: Vec<Mark>,
}

pub struct MarkStore {
    path: PathBuf,
    pub marks: Vec<Mark>,
}

impl MarkStore {
    pub fn load(dir: &PathBuf) -> Self {
        let path = dir.join("marks.json");
        let marks = fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<MarksFile>(&s).ok())
            .map(|f| f.marks)
            .unwrap_or_default();
        MarkStore { path, marks }
    }

    pub fn save(&self) {
        if let Some(parent) = self.path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let file = MarksFile {
            marks: self.marks.clone(),
        };
        if let Ok(json) = serde_json::to_string_pretty(&file) {
            let _ = fs::write(&self.path, json);
        }
    }

    fn idx(&self, scope: &str, key: &str) -> Option<usize> {
        self.marks
            .iter()
            .position(|m| m.scope == scope && m.key == key)
    }

    pub fn get(&self, scope: &str, key: &str) -> Option<&Mark> {
        self.idx(scope, key).map(|i| &self.marks[i])
    }

    /// 原子认领（CAS）。返回结果 + 认领后该条 Mark 的快照（含已有 note，供接力注入）。
    pub fn claim(
        &mut self,
        scope: &str,
        key: &str,
        title: &str,
        owner: &str,
        owner_name: &str,
        ttl_ms: i64,
    ) -> (ClaimOutcome, Option<Mark>) {
        let now = now_ms();
        if let Some(i) = self.idx(scope, key) {
            let (status, is_owner, lease) = {
                let m = &self.marks[i];
                (
                    m.status.clone(),
                    m.owner.as_deref() == Some(owner),
                    m.lease_until,
                )
            };
            if status == "done" {
                return (ClaimOutcome::Done, Some(self.marks[i].clone()));
            }
            if status == "failed" {
                return (ClaimOutcome::Failed, Some(self.marks[i].clone()));
            }
            if status == "claimed" && !is_owner && lease > now {
                return (ClaimOutcome::Taken, Some(self.marks[i].clone()));
            }
            // 接管 / 续租：open / 租约过期 / 本就是自己
            let m = &mut self.marks[i];
            m.status = "claimed".into();
            m.owner = Some(owner.to_string());
            m.owner_name = Some(owner_name.to_string());
            m.lease_until = now + ttl_ms;
            if !title.is_empty() && m.title.is_empty() {
                m.title = title.to_string();
            }
            m.updated_at = now;
            let snap = m.clone();
            self.save();
            return (ClaimOutcome::Acquired, Some(snap));
        }
        // 首次登记：直接认领
        let mark = Mark {
            scope: scope.to_string(),
            key: key.to_string(),
            title: title.to_string(),
            status: "claimed".into(),
            owner: Some(owner.to_string()),
            owner_name: Some(owner_name.to_string()),
            note: String::new(),
            thread_id: None,
            images: Vec::new(),
            lease_until: now + ttl_ms,
            created_at: now,
            updated_at: now,
        };
        self.marks.push(mark.clone());
        self.save();
        (ClaimOutcome::Acquired, Some(mark))
    }

    /// 设置状态；note 为 Some 时覆盖备注；release=true 时清空认领与租约（回到可被接管）。
    pub fn set_status(
        &mut self,
        scope: &str,
        key: &str,
        status: &str,
        note: Option<String>,
        release: bool,
    ) {
        if let Some(i) = self.idx(scope, key) {
            let m = &mut self.marks[i];
            m.status = status.to_string();
            if let Some(n) = note {
                m.note = n;
            }
            if release || status == "done" {
                m.owner = None;
                m.owner_name = None;
                m.lease_until = 0;
                if release {
                    m.thread_id = None;
                }
            }
            m.updated_at = now_ms();
            self.save();
        }
    }

    /// 只更新当前会话指针。账本仍然不保存过程。
    pub fn set_thread(&mut self, scope: &str, key: &str, thread_id: &str) {
        if let Some(i) = self.idx(scope, key) {
            let m = &mut self.marks[i];
            if thread_id.trim().is_empty() {
                m.thread_id = None;
            } else {
                m.thread_id = Some(thread_id.to_string());
            }
            m.updated_at = now_ms();
            self.save();
        }
    }

    /// 释放认领（回到 open，供他人接手）
    pub fn release(&mut self, scope: &str, key: &str) {
        self.set_status(scope, key, "open", None, true);
    }

    /// 登记一个「待处理(open)」单子（用户交办 / 同事转交）。
    /// 已存在则补全标题/备注，不覆盖 claimed/done/failed；失败项只能由用户手动重新激活。
    pub fn register_open(
        &mut self,
        scope: &str,
        key: &str,
        title: &str,
        note: &str,
        images: Vec<PromptImage>,
    ) {
        let now = now_ms();
        if let Some(i) = self.idx(scope, key) {
            let m = &mut self.marks[i];
            if m.title.is_empty() && !title.is_empty() {
                m.title = title.to_string();
            }
            if !note.is_empty() {
                m.note = note.to_string();
            }
            if !images.is_empty() {
                m.images = images;
            }
            m.updated_at = now;
            self.save();
            return;
        }
        self.marks.push(Mark {
            scope: scope.to_string(),
            key: key.to_string(),
            title: title.to_string(),
            status: "open".into(),
            owner: None,
            owner_name: None,
            note: note.to_string(),
            thread_id: None,
            images,
            lease_until: 0,
            created_at: now,
            updated_at: now,
        });
        self.save();
    }

    /// 删除标记（可重新被发现处理）
    pub fn remove(&mut self, scope: &str, key: &str) {
        self.marks.retain(|m| !(m.scope == scope && m.key == key));
        self.save();
    }

    pub fn list(&self, scope: Option<&str>) -> Vec<Mark> {
        let mut list: Vec<Mark> = match scope {
            Some(s) => self
                .marks
                .iter()
                .filter(|m| m.scope == s)
                .cloned()
                .collect(),
            None => self.marks.clone(),
        };
        list.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        list
    }

    /// 渲染某 scope 的账本摘要，注入侦察 prompt，让 agent 避开已处理 / 处理中的单子。
    pub fn digest(&self, scope: &str) -> String {
        let filtered: Vec<Mark> = self
            .marks
            .iter()
            .filter(|m| m.scope == scope)
            .cloned()
            .collect();
        render_digest(&filtered)
    }
}

/// 把一组 Mark 渲染成账本摘要（本地账本与共享账本共用）。
pub fn render_digest(marks: &[Mark]) -> String {
    let now = now_ms();
    let items: Vec<String> = marks
        .iter()
        .map(|m| {
            let st = match m.status.as_str() {
                "done" => "已完成",
                "failed" => "失败",
                "claimed" if m.lease_until > now => "处理中",
                _ => "待处理",
            };
            let who = m
                .owner_name
                .as_deref()
                .filter(|n| !n.is_empty() && st == "处理中")
                .map(|n| format!("（{n} 在做）"))
                .unwrap_or_default();
            let title = if m.title.is_empty() {
                String::new()
            } else {
                format!(" {}", m.title)
            };
            let thread = m
                .thread_id
                .as_deref()
                .filter(|id| !id.is_empty())
                .map(|id| format!(" · 会话 {}", id.chars().take(8).collect::<String>()))
                .unwrap_or_default();
            format!("- [{st}] {}{title}{who}{thread}", m.key)
        })
        .collect();
    if items.is_empty() {
        "（账本为空，还没有登记过任何单子）".to_string()
    } else {
        items.join("\n")
    }
}
