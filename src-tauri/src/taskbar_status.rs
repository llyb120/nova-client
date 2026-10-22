//! Always-on Windows taskbar child, not a desktop overlay. Its pixels remain in screenshots;
//! Jianlai only exempts text repaint inside an unchanged, verified child rectangle.

pub(crate) mod assistant;
pub(crate) mod capsules;
#[cfg(windows)]
mod drop_target;
#[cfg(windows)]
mod bubble;
#[cfg(windows)]
mod ask;
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
pub(crate) fn taskbar_bubble_activate(app: tauri::AppHandle) {
    #[cfg(windows)]
    bubble::activate(&app);
    #[cfg(not(windows))]
    let _ = app;
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

/// 待确认小气泡的数据。
#[tauri::command]
pub(crate) fn taskbar_ask(app: tauri::AppHandle) -> serde_json::Value {
    #[cfg(windows)]
    return ask::payload(&app);
    #[cfg(not(windows))]
    { let _ = app; serde_json::Value::Null }
}

/// 待确认小气泡报告渲染高度，按蝴蝶位置显示。
#[tauri::command]
pub(crate) fn taskbar_ask_place(app: tauri::AppHandle, height: f64) -> bool {
    #[cfg(windows)]
    return ask::place(&app, height);
    #[cfg(not(windows))]
    { let _ = (app, height); false }
}

/// Butterfly 面板「星海」标签的操作（剑谱录制、知识图谱）。
#[tauri::command]
pub(crate) async fn taskbar_quick(app: tauri::AppHandle, action: String) -> Result<serde_json::Value, String> {
    #[cfg(windows)]
    return tokio::task::spawn_blocking(move || bubble::quick(&app, &action)).await.map_err(|e| e.to_string())?;
    #[cfg(not(windows))]
    { let _ = (app, action); Err("仅支持 Windows".into()) }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

pub(crate) fn start(app: &tauri::AppHandle) {
    // Resume accepted assistant tasks even before the next panel is opened.
    if let Err(error) = assistant::start(app) { eprintln!("[taskbar-assistant] {error}"); }
    #[cfg(windows)]
    if !crate::server::is_headless() {
        windows::start(app.clone());
        // 待确认直接在蝴蝶上方弹出，不用用户点过去看。
        use tauri::Listener;
        app.listen(crate::acp::EV_PERMISSION, |event| {
            // 数字员工的接管询问只有气泡这一个入口（员工会话不在主窗口列表里），总是弹。
            let always = event.payload().contains("\"employee-");
            windows::on_app(move |app| ask::alert(app, always));
        });
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

/// Keep an open card readable after marking it seen; the chip still skips viewed results.
#[cfg(any(windows, test))]
fn pick<'a>(threads: &[(&'a str, i64, u32, Option<i64>)], shown: Option<&str>) -> Option<&'a str> {
    if let Some(thread) = threads.iter().find(|t| Some(t.0) == shown) { return Some(thread.0); }
    threads.iter().filter(|t| t.2 > 0 && !t.3.is_some_and(|at| at >= t.1)).max_by_key(|t| t.1).map(|t| t.0)
}

/// Acknowledge the rendered revision, or the current one when explicitly opening a session.
#[cfg(any(windows, test))]
fn mark_seen(store: &mut crate::threads::ThreadStore, thread: &str, stamp: Option<i64>) {
    let Some(result) = store.get_mut(thread) else { return };
    let stamp = stamp.unwrap_or(result.updated_at);
    if result.taskbar_seen_at.is_some_and(|at| at >= stamp) { return; }
    result.taskbar_seen_at = Some(stamp);
    store.save_thread(thread);
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
fn snapshot(app: &tauri::AppHandle, shown: Option<&str>) -> Option<Status> {
    use crate::threads::Item;
    use tauri::Manager;
    let state = app.state::<crate::AppState>();
    // Always visible, foreground or not.
    // Hidden while a dev instance runs; it reappears when dev exits (refresh polls every 250ms).
    if crate::meteor::release_yields_to_dev() {
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
        return Some(Status::new(Kind::Running, text, trailing, &[&thread.title, &activity, "单击打开助理 · 双击打开 Nova"], Some(thread.id.clone())));
    }
    if let Some(activity) = desktop {
        return Some(Status::new(Kind::Running, format!("剑来 · {activity}"), String::new(), &["Nova 正在操作桌面"], None));
    }
    let candidates: Vec<_> = store.threads.iter().filter(|t| !t.items.is_empty())
        .map(|t| (t.id.as_str(), t.updated_at, t.unread_turns, t.taskbar_seen_at)).collect();
    let Some(thread) = pick(&candidates, shown).and_then(|id| store.threads.iter().find(|t| t.id == id)) else {
        return Some(Status::new(Kind::Idle, "空闲".into(), String::new(), &["Butterfly", "单击向助理发消息 · 双击打开 Nova"], None));
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
    Some(Status::new(kind, text, ago(now - thread.updated_at), &[&thread.title, &excerpt(body, 240), "单击打开助理 · 双击打开会话"], Some(thread.id.clone())))
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
        let mut threads = [("old", 10, 0, None), ("a", 30, 1, None), ("b", 20, 2, None), ("latest", 40, 0, None)];
        assert_eq!(pick(&threads, None), Some("a"), "unread beats a newer already-read thread");
        threads[1].3 = Some(30); // Result rendered in the open bubble.
        assert_eq!(pick(&threads, None), Some("b"), "the chip advances immediately, without closing the bubble");
        assert_eq!(pick(&threads, Some("a")), Some("a"), "reading or replying must not switch cards after marking seen");
        assert_eq!(pick(&threads, Some("deleted")), Some("b"));
        for thread in &mut threads { thread.2 = 0; }
        assert_eq!(pick(&threads, None), None, "read in Nova (or never unread): the chip goes idle");
        assert_eq!(pick(&threads, Some("latest")), Some("latest"), "an open card stays readable while the chip is idle");
        let threads = [("a", 31, 1, Some(30))];
        assert_eq!(pick(&threads, None), Some("a"), "a newer unread update is shown again");
    }

    #[test]
    fn viewed_results_stay_seen_after_restart_without_hiding_new_updates() {
        use crate::threads::{AgentKind, Thread, ThreadStore};
        let dir = tempfile::tempdir().unwrap();
        let mut store = ThreadStore::load(dir.path().to_path_buf());
        let mut thread = Thread::new(String::new(), AgentKind::Lyra, None, None, None, false);
        thread.updated_at = 30;
        thread.unread_turns = 1;
        let id = thread.id.clone();
        store.threads.push(thread);
        mark_seen(&mut store, &id, Some(30));
        let mut snapshot = store.take_persist_snapshot().expect("acknowledgement must request persistence");
        ThreadStore::write_persist_snapshot(&mut snapshot).unwrap();
        drop(store);

        let mut store = ThreadStore::load(dir.path().to_path_buf());
        let thread = store.get(&id).unwrap();
        assert_eq!(thread.taskbar_seen_at, Some(30));
        assert_eq!(thread.unread_turns, 1, "Butterfly acknowledgement preserves Nova's unread count");
        assert_eq!(pick(&[(&id, thread.updated_at, thread.unread_turns, thread.taskbar_seen_at)], None), None);
        mark_seen(&mut store, &id, Some(20));
        assert!(store.take_persist_snapshot().is_none(), "stale acknowledgements neither roll back nor rewrite");

        store.get_mut(&id).unwrap().updated_at = 31;
        mark_seen(&mut store, &id, Some(30));
        let thread = store.get(&id).unwrap();
        assert_eq!(pick(&[(&id, thread.updated_at, thread.unread_turns, thread.taskbar_seen_at)], None), Some(id.as_str()), "an unrendered update stays visible");
        mark_seen(&mut store, &id, None);
        assert_eq!(store.get(&id).unwrap().taskbar_seen_at, Some(31), "opening a session acknowledges its current revision");
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
        // The icon-only fallback still fits the 92px gap that the full chip rejected.
        let crowded = Rect { right: -400, ..bar };
        assert!(free_slot(bar, &[crowded, tray], 240, 160, 4).is_none());
        assert_eq!(free_slot(bar, &[crowded, tray], 240, 34, 4), Some(Rect { left: -396, right: -304, ..slot }));
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
