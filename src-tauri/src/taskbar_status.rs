//! Opt-in Windows taskbar child, not a desktop overlay. Its pixels remain in screenshots;
//! Jianlai only exempts text repaint inside an unchanged, verified child rectangle.

#[cfg(windows)]
mod bubble;
#[cfg(windows)]
mod windows;

/// Bubble data: the live card and pending confirmations (null when the Butterfly is off).
#[tauri::command]
pub(crate) fn taskbar_bubble(app: tauri::AppHandle) -> serde_json::Value {
    #[cfg(windows)]
    return bubble::payload(&app);
    #[cfg(not(windows))]
    { let _ = app; serde_json::Value::Null }
}

/// The bubble page reports its rendered CSS height; the first call after a click also shows it.
/// Returns whether the bubble is meant to be visible.
#[tauri::command]
pub(crate) fn taskbar_bubble_place(app: tauri::AppHandle, height: f64) -> bool {
    #[cfg(windows)]
    return bubble::place(&app, height);
    #[cfg(not(windows))]
    { let _ = (app, height); false }
}

#[tauri::command]
pub(crate) fn taskbar_bubble_hide(app: tauri::AppHandle) {
    #[cfg(windows)]
    bubble::hide(&app);
    #[cfg(not(windows))]
    let _ = app;
}

/// Close the bubble, marking its result as viewed.
#[tauri::command]
pub(crate) fn taskbar_bubble_dismiss(app: tauri::AppHandle, thread_id: Option<String>) {
    #[cfg(windows)]
    bubble::dismiss(&app, thread_id);
    #[cfg(not(windows))]
    let _ = (app, thread_id);
}

/// Quick reply, or a new scratch session when the idle card has no thread.
#[tauri::command]
pub(crate) fn taskbar_bubble_reply(app: tauri::AppHandle, thread_id: Option<String>, text: String) -> Result<(), String> {
    #[cfg(windows)]
    return bubble::reply(&app, thread_id.as_deref(), &text);
    #[cfg(not(windows))]
    { let _ = (app, thread_id, text); Err("仅支持 Windows".into()) }
}

#[tauri::command]
pub(crate) fn taskbar_bubble_open(app: tauri::AppHandle, thread_id: Option<String>) {
    #[cfg(windows)]
    bubble::open_thread(&app, thread_id);
    #[cfg(not(windows))]
    let _ = (app, thread_id);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

pub(crate) fn start(app: &tauri::AppHandle) {
    #[cfg(windows)]
    if !crate::server::is_headless() {
        windows::start(app.clone());
    }
    #[cfg(not(windows))]
    let _ = app;
}

pub(crate) fn bounds() -> Option<Rect> {
    #[cfg(windows)]
    return windows::bounds();
    #[cfg(not(windows))]
    None
}

#[cfg(windows)]
static ACTIVITY: std::sync::Mutex<Option<&'static str>> = std::sync::Mutex::new(None);
#[cfg(windows)]
static SEEN: std::sync::Mutex<Seen> = std::sync::Mutex::new(Seen { at: std::collections::BTreeMap::new(), unread: std::collections::BTreeSet::new() });

/// A thread is viewed once clicked on the island, or opened in Nova after finishing unread,
/// at or after its last update. In memory only: after a restart the latest result shows once more.
#[cfg(any(windows, test))]
#[derive(Default)]
struct Seen { at: std::collections::BTreeMap<String, i64>, unread: std::collections::BTreeSet<String> }

#[cfg(any(windows, test))]
impl Seen {
    fn observe(&mut self, id: &str, unread: u32, updated_at: i64) {
        if unread > 0 {
            if !self.unread.contains(id) { self.unread.insert(id.into()); }
        } else if self.unread.remove(id) {
            self.at.insert(id.into(), updated_at);
        }
    }
    fn viewed(&self, id: &str, updated_at: i64) -> bool { self.at.get(id).is_some_and(|&at| at >= updated_at) }
}

/// Newest unread result first, else the newest thread; anything viewed since its last update is skipped.
#[cfg(any(windows, test))]
fn pick<'a>(threads: &[(&'a str, i64, u32)], seen: &Seen) -> Option<&'a str> {
    let fresh = |t: &&(&str, i64, u32)| !seen.viewed(t.0, t.1);
    threads.iter().filter(|t| t.2 > 0).filter(fresh).max_by_key(|t| t.1)
        .or_else(|| threads.iter().max_by_key(|t| t.1).filter(fresh))
        .map(|t| t.0)
}

/// Island click: the result it showed is now viewed.
#[cfg(windows)]
fn mark_seen(app: &tauri::AppHandle, thread: &str) {
    use tauri::Manager;
    let state = app.state::<crate::AppState>();
    let stamp = state.store.lock().ok().and_then(|store| store.threads.iter().find(|t| t.id == thread).map(|t| t.updated_at));
    if let (Some(stamp), Ok(mut seen)) = (stamp, SEEN.lock()) { seen.at.insert(thread.into(), stamp); }
}

pub(crate) struct Activity {
    #[cfg(windows)]
    previous: Option<&'static str>,
}

/// Called only inside Jianlai's desktop lease; nested capture/input scopes restore their parent.
pub(crate) fn activity(label: &'static str) -> Activity {
    #[cfg(windows)]
    {
        let previous = ACTIVITY.lock().unwrap().replace(label);
        windows::wake();
        Activity { previous }
    }
    #[cfg(not(windows))]
    { let _ = label; Activity {} }
}

impl Drop for Activity {
    fn drop(&mut self) {
        #[cfg(windows)]
        {
            *ACTIVITY.lock().unwrap() = self.previous;
            windows::wake();
        }
    }
}

/// Right-align in the rightmost verified gap. Never move or resize Explorer controls.
#[cfg(any(windows, test))]
fn free_slot(taskbar: Rect, occupied: &[Rect], desired_width: i32, min_width: i32, gap: i32) -> Option<Rect> {
    if gap < 0 || min_width <= 0 || desired_width < min_width || taskbar.bottom <= taskbar.top {
        return None;
    }
    let start = taskbar.left.checked_add(gap)?;
    let end = taskbar.right.checked_sub(gap)?;
    let top = taskbar.top.checked_add(gap)?;
    let bottom = taskbar.bottom.checked_sub(gap)?;
    if start >= end || top >= bottom { return None; }
    let mut spans: Vec<_> = occupied.iter()
        .filter(|r| r.top < taskbar.bottom && r.bottom > taskbar.top && r.left < r.right)
        .map(|r| (r.left.saturating_sub(gap).max(start), r.right.saturating_add(gap).min(end)))
        .filter(|(left, right)| left < right)
        .collect();
    spans.sort_unstable();
    spans.push((end, end));
    let mut cursor = start;
    let mut best = None;
    for (left, right) in spans {
        let width = left.saturating_sub(cursor).min(desired_width);
        if width >= min_width {
            best = Some(Rect { left: left - width, right: left, top, bottom });
        }
        cursor = cursor.max(right);
    }
    best
}

#[cfg(any(windows, test))]
fn tool_label(call: &crate::threads::ToolCall) -> String {
    // Describe the public operation, never copy typed text, commands or model reasoning.
    let title = call.title.to_lowercase();
    if title.contains("jianlai") || title.contains("剑来") {
        let input = call.raw_input.as_ref().unwrap_or(&serde_json::Value::Null);
        let input = input.get("arguments").or_else(|| input.get("args")).unwrap_or(input);
        let action = input.get("actions").and_then(|a| a.as_array()).and_then(|a| a.first())
            .or_else(|| input.get("action"));
        let label = match input["operation"].as_str().unwrap_or("") {
            "windows" => "观察窗口",
            "screenshot" => "截取画面",
            "activate" => "切换窗口",
            "act" | "run" => match action.and_then(|a| a["action"].as_str()) {
                Some("click" | "double_click") => "点击控件",
                Some("type") => "输入内容",
                Some("press") => "发送按键",
                Some("scroll") => "滚动页面",
                Some("drag") => "拖动控件",
                Some("move") => "移动鼠标",
                Some("wait") => "等待界面",
                _ => "操作桌面",
            },
            _ => "观察／操作桌面",
        };
        return format!("剑来 · {label}");
    }
    match call.kind.as_str() {
        "read" => "读取文件",
        "edit" => "修改文件",
        "execute" => "执行命令",
        "search" => "检索资料",
        "fetch" => "获取资料",
        _ if title.contains("chrome") || title.contains("webview") => "操作浏览器",
        _ => "执行工具",
    }.to_string()
}

#[cfg(any(windows, test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind { Idle, Running, Waiting, Done, Error }

/// One frame of the island: a glanceable line, hover detail and the thread a click opens.
#[cfg(any(windows, test))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Status {
    pub kind: Kind,
    pub text: String,
    pub trailing: String,
    pub detail: String,
    pub thread: Option<String>,
}

#[cfg(any(windows, test))]
impl Status {
    fn new(kind: Kind, text: String, trailing: String, detail: &[&str], thread: Option<String>) -> Self {
        let detail = detail.iter().filter(|s| !s.is_empty()).copied().collect::<Vec<_>>().join("\n");
        Self { kind, text, trailing, detail, thread }
    }

    /// Accessible name / window text.
    pub fn title(&self) -> String {
        if self.trailing.is_empty() { format!("Butterfly · {}", self.text) } else { format!("Butterfly · {} · {}", self.text, self.trailing) }
    }
}

#[cfg(windows)]
fn snapshot(app: &tauri::AppHandle) -> Option<Status> {
    use crate::threads::Item;
    use tauri::Manager;
    let state = app.state::<crate::AppState>();
    // Enabled = always visible, foreground or not.
    // Hidden while a dev instance runs; it reappears when dev exits (refresh polls every 250ms).
    if !state.settings.lock().ok()?.taskbar_status_enabled || crate::meteor::release_yields_to_dev() {
        return None;
    }
    let now = crate::threads::now_ms();
    let desktop = *ACTIVITY.lock().ok()?;
    let waiting: Vec<Option<String>> = state.remote_permissions.lock().ok()?.values()
        .map(|v| v["threadId"].as_str().map(str::to_string)).collect();
    if desktop.is_none() && !waiting.is_empty() {
        let text = format!("{} 项待你确认", waiting.len());
        return Some(Status::new(Kind::Waiting, text, String::new(), &["有操作等待你确认", "单击在气泡中处理 · 双击打开 Nova"], waiting.into_iter().flatten().next()));
    }
    // ponytail: scan thread headers once per refresh, not transcripts; if histories grow large,
    // replace this with a running-id index maintained by the shared runtime.
    let store = state.store.lock().ok()?;
    let running: Vec<_> = store.threads.iter().filter(|t| crate::is_running(&state, t)).collect();
    if let Some(thread) = running.iter().max_by_key(|t| t.updated_at) {
        let turn = || thread.items.iter().rev().take_while(|item| !matches!(item, Item::User { .. } | Item::Turn { .. }));
        let activity = match desktop {
            Some(activity) => format!("剑来 · {activity}"),
            None => turn().find_map(|item| match item {
                Item::Tool { call, .. } if matches!(call.status.as_str(), "pending" | "in_progress") => Some(tool_label(call)),
                _ => None,
            }).unwrap_or_else(|| match turn().next() {
                Some(Item::Thought { .. }) => "正在思考",
                Some(Item::Assistant { .. }) => "正在回复",
                _ => "正在处理",
            }.into()),
        };
        let started = thread.items.iter().rev().find_map(|item| match item { Item::User { ts, .. } => Some(*ts), _ => None });
        let text = if running.len() > 1 { format!("{} 项 · {activity}", running.len()) } else { activity.clone() };
        let trailing = started.map(|t| elapsed(now - t)).unwrap_or_default();
        return Some(Status::new(Kind::Running, text, trailing, &[&thread.title, &activity, "单击查看进度 · 双击打开 Nova"], Some(thread.id.clone())));
    }
    if let Some(activity) = desktop {
        return Some(Status::new(Kind::Running, format!("剑来 · {activity}"), String::new(), &["Nova 正在操作桌面"], None));
    }
    let mut seen = SEEN.lock().ok()?;
    let candidates: Vec<_> = store.threads.iter().filter(|t| !t.items.is_empty()).map(|t| {
        seen.observe(&t.id, t.unread_turns, t.updated_at);
        (t.id.as_str(), t.updated_at, t.unread_turns)
    }).collect();
    let Some(thread) = pick(&candidates, &seen).and_then(|id| store.threads.iter().find(|t| t.id == id)) else {
        return Some(Status::new(Kind::Idle, "空闲".into(), String::new(), &["Butterfly", "单击发起新会话 · 双击打开 Nova"], None));
    };
    // The last turn's final reply (or error) is the conclusion.
    let (kind, body) = thread.items.iter().rev()
        .take_while(|item| !matches!(item, Item::User { .. }))
        .find_map(|item| match item {
            Item::Assistant { text, .. } if conclusion(text).is_some() => Some((Kind::Done, text.as_str())),
            Item::System { text, level, .. } if level == "error" => Some((Kind::Error, text.as_str())),
            _ => None,
        }).unwrap_or((Kind::Done, ""));
    let text = conclusion(body).unwrap_or_else(|| if kind == Kind::Error { "任务出错" } else { "已完成" }.into());
    Some(Status::new(kind, text, ago(now - thread.updated_at), &[&thread.title, &excerpt(body, 240), "单击查看完整结论 · 双击打开会话"], Some(thread.id.clone())))
}

/// Non-empty lines with common markdown decoration removed.
#[cfg(any(windows, test))]
fn plain_lines(text: &str) -> impl Iterator<Item = String> + '_ {
    text.lines()
        .map(|l| l.trim().trim_start_matches(['#', '>', '-', '*', '|', ' ']).replace("**", "").replace('`', "").trim().to_string())
        .filter(|l| !l.is_empty())
}

#[cfg(any(windows, test))]
fn clip(text: &str, max: usize) -> String {
    if text.chars().count() > max { text.chars().take(max).chain(Some('…')).collect() } else { text.to_string() }
}

/// First meaningful line of the final reply, capped for the small chip.
#[cfg(any(windows, test))]
fn conclusion(text: &str) -> Option<String> {
    plain_lines(text).next().map(|line| clip(&line, 60))
}

/// A few leading lines for the hover tooltip.
#[cfg(any(windows, test))]
fn excerpt(text: &str, max: usize) -> String {
    clip(&plain_lines(text).take(6).collect::<Vec<_>>().join("\n"), max)
}

#[cfg(any(windows, test))]
fn elapsed(ms: i64) -> String {
    let s = ms.max(0) / 1000;
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
}

#[cfg(any(windows, test))]
fn ago(ms: i64) -> String {
    match ms.max(0) / 1000 {
        s if s < 60 => "刚刚".into(),
        s if s < 3600 => format!("{} 分钟前", s / 60),
        s if s < 86400 => format!("{} 小时前", s / 3600),
        s => format!("{} 天前", s / 86400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conclusion_takes_first_plain_line_and_truncates() {
        assert_eq!(conclusion("\n## **完成** 修复了 `foo`\n细节").as_deref(), Some("完成 修复了 foo"));
        assert_eq!(conclusion("  \n```\n"), None);
        assert_eq!(conclusion(&"字".repeat(80)).unwrap().chars().count(), 61);
        assert_eq!(excerpt("# 结论\n\n- 一\n- 二", 240), "结论\n一\n二");
        assert_eq!(elapsed(83_000), "1:23");
        assert_eq!(elapsed(3_723_000), "1:02:03");
        assert_eq!((ago(5_000), ago(180_000), ago(7_200_000)), ("刚刚".into(), "3 分钟前".into(), "2 小时前".into()));
        let status = Status::new(Kind::Done, "好".into(), "刚刚".into(), &["标题", "", "点击"], None);
        assert_eq!((status.title().as_str(), status.detail.as_str()), ("Butterfly · 好 · 刚刚", "标题\n点击"));
    }

    #[test]
    fn viewed_results_give_way_to_other_unread_results() {
        let mut seen = Seen::default();
        let threads = [("old", 10, 0), ("a", 30, 1), ("b", 20, 2), ("latest", 40, 0)];
        assert_eq!(pick(&threads, &seen), Some("a"), "unread beats a newer already-read thread");
        seen.at.insert("a".into(), 30); // Clicked on the island.
        assert_eq!(pick(&threads, &seen), Some("b"));
        seen.observe("b", 2, 20);
        seen.observe("b", 0, 20); // Opened in Nova: unread cleared.
        let threads = [("old", 10, 0), ("a", 30, 0), ("b", 20, 0), ("latest", 40, 0)];
        assert_eq!(pick(&threads, &seen), Some("latest"), "no unread left: the newest result if not viewed");
        seen.at.insert("latest".into(), 40);
        assert_eq!(pick(&threads, &seen), None, "never falls back to old history");
        let threads = [("latest", 41, 0)];
        assert_eq!(pick(&threads, &seen), Some("latest"), "a newer update is unviewed again");
    }

    #[test]
    fn taskbar_slot_preserves_controls_and_handles_negative_monitor_coordinates() {
        let bar = Rect { left: -1920, top: 1040, right: 0, bottom: 1080 };
        let icons = Rect { left: -1200, top: 1040, right: -500, bottom: 1080 };
        let tray = Rect { left: -300, top: 1040, right: 0, bottom: 1080 };
        let slot = free_slot(bar, &[icons, tray], 240, 160, 4).unwrap();
        // Prefer the narrower right-hand gap over the larger one on the left.
        assert_eq!(slot, Rect { left: -496, top: 1044, right: -304, bottom: 1076 });
        assert_eq!(free_slot(bar, &[tray, icons, icons], 240, 160, 4), Some(slot));
        assert_eq!(free_slot(bar, &[], 240, 160, 4), Some(Rect { left: -244, right: -4, ..slot }));
        // If the right gap is too narrow, right-align inside the preceding safe gap.
        let wider_icons = Rect { right: -450, ..icons };
        assert_eq!(free_slot(bar, &[wider_icons, tray], 240, 160, 4), Some(Rect { left: -1444, right: -1204, ..slot }));
        assert!(free_slot(bar, &[bar], 240, 160, 4).is_none());
        assert!(free_slot(bar, &[], 100, 160, 4).is_none());
        // Two individually valid but overlapping occupied spans are a single obstruction.
        let almost_full = Rect { right: -100, ..bar };
        assert!(free_slot(bar, &[almost_full, tray], 240, 160, 4).is_none());
    }

    #[test]
    fn taskbar_reports_public_operation_without_leaking_typed_content() {
        let call = crate::threads::ToolCall {
            tool_call_id: "test".into(), title: "functions.jianlai".into(), kind: "other".into(),
            status: "in_progress".into(), content: vec![], locations: vec![], raw_output: None,
            raw_input: Some(serde_json::json!({"operation":"act","actions":[{"action":"type","text":"private value"}]})),
        };
        assert_eq!(tool_label(&call), "剑来 · 输入内容");
    }
}
