//! Opt-in Windows taskbar child, not a desktop overlay. Its pixels remain in screenshots;
//! Jianlai only exempts text repaint inside an unchanged, verified child rectangle.

#[cfg(windows)]
mod windows;

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

/// Never move or resize Explorer controls. Use only a gap in the observed layout.
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
        if width >= min_width && best.is_none_or(|r: Rect| width > r.right - r.left) {
            best = Some(Rect { left: cursor, right: cursor + width, top, bottom });
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

#[cfg(windows)]
fn snapshot(app: &tauri::AppHandle) -> Option<String> {
    use crate::threads::Item;
    use tauri::Manager;
    let state = app.state::<crate::AppState>();
    if !state.settings.lock().ok()?.taskbar_status_enabled {
        return None;
    }
    if app.get_window("main").is_some_and(|w| w.is_focused().unwrap_or(false)) {
        return None;
    }
    if let Some(activity) = *ACTIVITY.lock().ok()? {
        return Some(format!("Nova · 剑来 · {activity}"));
    }
    let permissions = state.remote_permissions.lock().ok()?.len();
    if permissions > 0 {
        return Some(format!("Nova · {permissions} 项待确认"));
    }
    // ponytail: scan thread headers once per refresh, not transcripts; if histories grow large,
    // replace this with a running-id index maintained by the shared runtime.
    let store = state.store.lock().ok()?;
    let mut running = store.threads.iter().filter(|t| crate::is_running(&state, t));
    let mut newest = running.next()?;
    let mut count = 1;
    for thread in running {
        count += 1;
        if thread.updated_at > newest.updated_at { newest = thread; }
    }
    let activity = newest.items.iter().rev()
        .take_while(|item| !matches!(item, Item::User { .. } | Item::Turn { .. }))
        .find_map(|item| match item {
            Item::Tool { call, .. } if matches!(call.status.as_str(), "pending" | "in_progress") => Some(tool_label(call)),
            _ => None,
        }).unwrap_or_else(|| "正在处理".into());
    Some(if count > 1 { format!("Nova · {count} 项 · {activity}") } else { format!("Nova · {activity}") })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn taskbar_slot_preserves_controls_and_handles_negative_monitor_coordinates() {
        let bar = Rect { left: -1920, top: 1040, right: 0, bottom: 1080 };
        let icons = Rect { left: -1200, top: 1040, right: -500, bottom: 1080 };
        let tray = Rect { left: -300, top: 1040, right: 0, bottom: 1080 };
        let slot = free_slot(bar, &[icons, tray], 240, 160, 4).unwrap();
        assert_eq!(slot, Rect { left: -1916, top: 1044, right: -1676, bottom: 1076 });
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
