//! 星轨：Butterfly 面板里的待办清单，`~/.nova/orbit.json`。
//! 用户的改动会作为事件（orbit.added / orbit.done / orbit.changed）触发订阅了它的数字员工职责；
//! 员工经 employee 工具读写星轨时不发事件，避免职责被自己的改动再次触发。

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::Emitter;

const MAX_ITEMS: usize = 500;
const MAX_TEXT_CHARS: usize = 500;
pub const EV_ORBIT: &str = "orbit:changed";

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Item {
    pub id: String,
    pub text: String,
    pub done: bool,
    /// 标星：排在待办最前。
    pub star: bool,
    pub created_at: i64,
    pub done_at: i64,
}

static LOCK: Mutex<()> = Mutex::new(());

fn path() -> PathBuf {
    crate::lyra::config::nova_root().join("orbit.json")
}

fn load() -> Result<Vec<Item>, String> {
    match std::fs::read_to_string(path()) {
        Ok(raw) => serde_json::from_str(&raw).map_err(|e| format!("orbit.json 解析失败：{e}")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(format!("读取 orbit.json 失败：{e}")),
    }
}

/// 读改写一次；`user` 为真且改动有事件时通知数字员工。f 返回（结果，事件名，事件描述）。
fn change<T>(user: bool, f: impl FnOnce(&mut Vec<Item>) -> Result<(T, &'static [&'static str], String), String>) -> Result<T, String> {
    let (out, events, detail) = {
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut items = load()?;
        let (out, events, detail) = f(&mut items)?;
        let tmp = path().with_extension("json.tmp");
        let json = serde_json::to_string_pretty(&items).map_err(|e| e.to_string())?;
        std::fs::create_dir_all(path().parent().unwrap()).and_then(|_| std::fs::write(&tmp, json)).and_then(|_| std::fs::rename(&tmp, path()))
            .map_err(|e| format!("保存 orbit.json 失败：{e}"))?;
        (out, events, detail)
    };
    if let Some(app) = crate::employee::app_handle() { let _ = app.emit(EV_ORBIT, json!({})); }
    if user && !events.is_empty() { crate::employee::event(events, &detail); }
    Ok(out)
}

fn clean(text: &str) -> Result<String, String> {
    let text = text.trim();
    if text.is_empty() { return Err("内容不能为空".into()); }
    if text.chars().count() > MAX_TEXT_CHARS { return Err(format!("每条最多 {MAX_TEXT_CHARS} 字")); }
    Ok(text.to_string())
}

fn quote(text: &str) -> String {
    text.chars().take(60).collect()
}

pub(crate) fn list() -> Result<Vec<Item>, String> {
    load()
}

/// 新增到最前面。`user`：用户在界面上加的（会触发事件）；员工经工具加的不触发。
pub(crate) fn add(text: &str, user: bool) -> Result<Value, String> {
    let text = clean(text)?;
    change(user, |items| {
        if items.len() >= MAX_ITEMS { return Err(format!("星轨最多 {MAX_ITEMS} 条，先清掉已完成的")); }
        let now = crate::threads::now_ms();
        let item = Item { id: uuid::Uuid::new_v4().simple().to_string()[..12].to_string(), text: text.clone(), created_at: now, ..Default::default() };
        let out = json!({"ok": true, "id": item.id});
        items.insert(0, item);
        Ok((out, &["orbit.added", "orbit.changed"], format!("新增事项「{}」", quote(&text))))
    })
}

pub(crate) fn set_done(id: &str, done: bool, user: bool) -> Result<Value, String> {
    change(user, |items| {
        let item = items.iter_mut().find(|i| i.id == id).ok_or("没有这条事项")?;
        let changed = item.done != done;
        item.done = done;
        item.done_at = if done { crate::threads::now_ms() } else { 0 };
        let (events, verb): (&'static [&'static str], _) = match (changed, done) {
            (false, _) => (&[], ""),
            (true, true) => (&["orbit.done", "orbit.changed"], "完成事项"),
            (true, false) => (&["orbit.changed"], "重新打开事项"),
        };
        Ok((json!({"ok": true}), events, format!("{verb}「{}」", quote(&item.text))))
    })
}

/// 界面编辑：patch 里的 text / star 有哪个改哪个。
pub(crate) fn edit(id: &str, patch: &Value) -> Result<(), String> {
    let text = patch["text"].as_str().map(clean).transpose()?;
    change(true, |items| {
        let item = items.iter_mut().find(|i| i.id == id).ok_or("没有这条事项")?;
        let mut what = Vec::new();
        if let Some(text) = text.filter(|t| *t != item.text) { item.text = text; what.push("修改了内容"); }
        if let Some(star) = patch["star"].as_bool().filter(|s| *s != item.star) { item.star = star; what.push(if star { "标星" } else { "取消标星" }); }
        let events: &'static [&'static str] = if what.is_empty() { &[] } else { &["orbit.changed"] };
        Ok(((), events, format!("{}事项「{}」", what.join("并"), quote(&item.text))))
    })
}

pub(crate) fn remove(id: &str) -> Result<(), String> {
    change(true, |items| {
        let at = items.iter().position(|i| i.id == id).ok_or("没有这条事项")?;
        let item = items.remove(at);
        Ok(((), &["orbit.changed"], format!("删除事项「{}」", quote(&item.text))))
    })
}

pub(crate) fn clear_done() -> Result<(), String> {
    change(true, |items| {
        let before = items.len();
        items.retain(|i| !i.done);
        let n = before - items.len();
        let events: &'static [&'static str] = if n == 0 { &[] } else { &["orbit.changed"] };
        Ok(((), events, format!("清除了 {n} 条已完成事项")))
    })
}

// ---------- 前端命令 ----------

#[tauri::command]
pub fn orbit_list() -> Result<Vec<Item>, String> {
    list()
}

#[tauri::command]
pub fn orbit_add(text: String) -> Result<Value, String> {
    add(&text, true)
}

#[tauri::command]
pub fn orbit_done(id: String, done: bool) -> Result<Value, String> {
    set_done(&id, done, true)
}

#[tauri::command]
pub fn orbit_edit(id: String, patch: Value) -> Result<(), String> {
    edit(&id, &patch)
}

#[tauri::command]
pub fn orbit_remove(id: String) -> Result<(), String> {
    remove(&id)
}

#[tauri::command]
pub fn orbit_clear_done() -> Result<(), String> {
    clear_done()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_trimmed_and_bounded() {
        assert_eq!(clean("  买牛奶 ").unwrap(), "买牛奶");
        assert!(clean("   ").is_err());
        assert!(clean(&"x".repeat(MAX_TEXT_CHARS + 1)).is_err());
    }
}
