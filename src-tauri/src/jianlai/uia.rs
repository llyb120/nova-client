//! Fast batch-cached Windows UIAutomation observer (`IUIAutomationCacheRequest`).
//! Uses a persistent PMv2-DPI STA/MTA worker reusing COM objects, top-level popup HWND discovery,
//! and iterative BFS with repetitive-list throttling so deep controls stay fast and reachable.
#![allow(non_upper_case_globals)]
use std::{
    collections::VecDeque,
    ptr::null_mut,
    sync::{mpsc, Mutex},
    time::{Duration, Instant},
};
use windows::{
    core::{Interface, BSTR},
    Win32::{
        Foundation::HWND as UiaHwnd,
        System::{
            Com::{
                CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
                COINIT_MULTITHREADED,
            },
            Variant::VARIANT,
        },
        UI::Accessibility::*,
    },
};

const DEFAULT_DEADLINE: Duration = Duration::from_millis(750);
const FAST_DEADLINE: Duration = Duration::from_millis(150);
const VISIT_LIMIT: usize = 900;
const FAST_VISIT_LIMIT: usize = 480;
const FAST_MAX_DEPTH: usize = 9;
const FAST_REPETITIVE_SIBLING_CAP: usize = 24;

#[derive(Clone, Debug)]
pub(super) struct Item {
    pub kind: UIA_CONTROLTYPE_ID,
    pub role: String,
    pub name: String,
    pub automation_id: String,
    /// Physical screen pixels: `(left, top, width, height)`.
    pub rect: (i32, i32, i32, i32),
    pub depth: usize,
    pub disabled: bool,
    pub editable: bool,
    pub checked: Option<bool>,
    pub value: Option<String>,
    /// `(vertical_scroll_percent 0..=100, vertical_view_size_percent 0..=100)` when vertically scrollable.
    pub scroll: Option<(f64, f64)>,
    pub in_overlay: bool,
}

#[derive(Clone, Debug)]
pub(super) struct OverlayInfo {
    pub kind: &'static str,
    /// Physical screen pixels: `(left, top, width, height)`.
    pub rect: (i32, i32, i32, i32),
    pub has_confirm: bool,
    /// Safe blank point inside the parent window outside `rect`, in physical screen pixels.
    pub outside_point: Option<(i32, i32)>,
}

#[derive(Clone, Debug)]
pub(super) struct ScrollInfo {
    /// Physical screen pixels: `(left, top, width, height)`.
    pub rect: (i32, i32, i32, i32),
    pub percent: f64,
    pub view_size: f64,
}

pub(super) struct Report {
    pub items: Vec<Item>,
    pub overlay: Option<OverlayInfo>,
    pub scroll: Option<ScrollInfo>,
    pub truncated: bool,
    /// First truncation reason: `deadline` / `visitLimit` / `itemLimit` / `queryFailed`.
    pub truncated_by: Option<&'static str>,
    pub visited: usize,
    pub elapsed_ms: u128,
}

impl Report {
    fn mark_truncated(&mut self, reason: &'static str) {
        self.truncated = true;
        self.truncated_by.get_or_insert(reason);
    }
}

/// Full explicit control-tree query (`jianlai_view(screenshot: false)` or custom depth/filter).
pub(super) fn collect(
    hwnd: u32,
    depth: usize,
    max_items: usize,
    filter: Option<&str>,
) -> Result<Report, String> {
    start_query(hwnd, depth, max_items, filter, DEFAULT_DEADLINE, VISIT_LIMIT, false)?.wait()
}

/// Fast bounded control-tree query attached automatically to screenshots for `[1][2]` SoM badges.
/// Hard-capped at 150ms so slow/self-drawn apps never stall screenshot delivery.
#[cfg(test)]
pub(super) fn collect_fast(hwnd: u32, max_items: usize, filter: Option<&str>) -> Result<Report, String> {
    start_fast(hwnd, max_items, filter)?.wait()
}

pub(super) fn start_fast(hwnd: u32, max_items: usize, filter: Option<&str>) -> Result<PendingQuery, String> {
    start_query(hwnd, FAST_MAX_DEPTH, max_items, filter, FAST_DEADLINE, FAST_VISIT_LIMIT, true)
}

type Tools = (IUIAutomation2, IUIAutomationCacheRequest, IUIAutomationTreeWalker);
type Job = Box<dyn FnOnce(&Tools) + Send>;
static WORKER: Mutex<Option<mpsc::SyncSender<Job>>> = Mutex::new(None);

pub(super) struct PendingQuery(mpsc::Receiver<Result<Report, String>>, Instant);
impl PendingQuery {
    pub(super) fn wait(self) -> Result<Report, String> {
        let timeout = self.1.saturating_duration_since(Instant::now());
        self.0
            .recv_timeout(timeout)
            .map_err(|e| format!("UIA 查询未完成：{e}"))?
    }
}

fn start_query(
    hwnd: u32,
    depth: usize,
    max_items: usize,
    filter: Option<&str>,
    deadline: Duration,
    visit_limit: usize,
    interactive_only: bool,
) -> Result<PendingQuery, String> {
    let filter = filter.map(str::to_owned);
    let until = Instant::now() + deadline + Duration::from_millis(120);
    let (tx, rx) = mpsc::channel();
    let job: Job = Box::new(move |tools| {
        let remaining = until.saturating_duration_since(Instant::now());
        if !remaining.is_zero() {
            let _ = tx.send(query(
                tools,
                hwnd,
                depth,
                max_items,
                filter.as_deref(),
                deadline.min(remaining),
                visit_limit,
                interactive_only,
            ));
        }
    });
    let mut slot = WORKER.lock().map_err(|_| "UIA 线程锁不可用")?;
    if slot.is_none() {
        // Zero-capacity rendezvous channel: if a previous COM call is still busy/hung,
        // `try_send` immediately reports busy instead of queuing behind a stalled app.
        let (send, jobs) = mpsc::sync_channel::<Job>(0);
        std::thread::Builder::new()
            .name("nova-uia".into())
            .spawn(move || unsafe {
                use windows_sys::Win32::UI::HiDpi::{
                    SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
                };
                let _ = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
                if CoInitializeEx(None, COINIT_MULTITHREADED).is_err() {
                    return;
                }
                if let Ok(tools) = make_tools() {
                    for job in jobs {
                        job(&tools);
                    }
                }
                CoUninitialize();
            })
            .map_err(|e| format!("启动 UIA 线程失败：{e}"))?;
        *slot = Some(send);
    }
    match slot.as_ref().unwrap().try_send(job) {
        Ok(()) => Ok(PendingQuery(rx, until)),
        Err(mpsc::TrySendError::Full(_)) => Err("UIA 查询线程正忙".into()),
        Err(mpsc::TrySendError::Disconnected(_)) => {
            *slot = None;
            Err("UIA 线程已退出".into())
        }
    }
}

fn is_interactive_kind(kind: UIA_CONTROLTYPE_ID) -> bool {
    matches!(
        kind,
        UIA_ButtonControlTypeId
            | UIA_EditControlTypeId
            | UIA_ComboBoxControlTypeId
            | UIA_CheckBoxControlTypeId
            | UIA_RadioButtonControlTypeId
            | UIA_HyperlinkControlTypeId
            | UIA_MenuItemControlTypeId
            | UIA_TabItemControlTypeId
            | UIA_ListItemControlTypeId
            | UIA_TreeItemControlTypeId
            | UIA_DataItemControlTypeId
            | UIA_SplitButtonControlTypeId
            | UIA_SliderControlTypeId
            | UIA_SpinnerControlTypeId
            | UIA_CalendarControlTypeId
    )
}

fn is_repetitive_kind(kind: UIA_CONTROLTYPE_ID) -> bool {
    matches!(
        kind,
        UIA_ListItemControlTypeId | UIA_TreeItemControlTypeId | UIA_DataItemControlTypeId
    )
}

fn is_confirm_name(name: &str) -> bool {
    let low = name.to_lowercase();
    ["confirm", "apply", "ok", "okay", "save", "done", "submit", "确定", "确认", "应用", "保存", "完成", "提交"]
        .iter()
        .any(|k| low == *k || low.contains(k))
}

fn var_bool(v: &VARIANT) -> Option<bool> {
    bool::try_from(v).ok()
}

fn var_i32(v: &VARIANT) -> Option<i32> {
    i32::try_from(v).ok()
}

fn var_f64(v: &VARIANT) -> Option<f64> {
    f64::try_from(v).ok()
}

fn var_str(v: &VARIANT) -> Option<String> {
    let s = BSTR::try_from(v).ok()?.to_string();
    let t = s.trim();
    (!t.is_empty()).then(|| t.chars().take(80).collect())
}

fn rect_contains(outer: (i32, i32, i32, i32), inner: (i32, i32, i32, i32)) -> bool {
    let cx = inner.0 + inner.2 / 2;
    let cy = inner.1 + inner.3 / 2;
    cx >= outer.0 && cx <= outer.0 + outer.2 && cy >= outer.1 && cy <= outer.1 + outer.3
}

fn near_duplicate_item(existing: &Item, candidate: &Item) -> bool {
    let (al, at, aw, ah) = existing.rect;
    let (bl, bt, bw, bh) = candidate.rect;
    let (ar, ab) = (al + aw, at + ah);
    let (br, bb) = (bl + bw, bt + bh);
    let close = (al - bl).abs() <= 3
        && (at - bt).abs() <= 3
        && (ar - br).abs() <= 3
        && (ab - bb).abs() <= 3;
    close && (existing.name == candidate.name || candidate.name.is_empty())
}

fn compute_outside_point(
    win_rect: (i32, i32, i32, i32),
    ov_rect: (i32, i32, i32, i32),
    items: &[Item],
) -> Option<(i32, i32)> {
    let (wl, wt, ww, wh) = win_rect;
    let (wr, wb) = (wl + ww, wt + wh);
    let (ol, ot, ow, oh) = ov_rect;
    let (or, ob) = (ol + ow, ot + oh);
    let candidates = [
        (ol - 28, ot + (oh / 4).min(40)),
        (ol - 28, ot + oh / 2),
        (or + 28, ot + (oh / 4).min(40)),
        (ol + ow / 2, ot - 28),
        (ol + ow / 2, ob + 28),
        (wl + 24, wt + 44),
        (wr - 24, wt + 44),
    ];
    for (x, y) in candidates {
        if x < wl + 8 || x > wr - 8 || y < wt + 36 || y > wb - 8 {
            continue;
        }
        if x >= ol - 6 && x <= or + 6 && y >= ot - 6 && y <= ob + 6 {
            continue;
        }
        let hits_control = items.iter().any(|it| {
            let (l, t, w, h) = it.rect;
            x >= l - 4 && x <= l + w + 4 && y >= t - 4 && y <= t + h + 4
        });
        if !hits_control {
            return Some((x, y));
        }
    }
    None
}

/// Discovers top-level popup menus (`#32768`, `ComboLBox`), owned tool windows, and popup dialogs
/// strictly above `target_hwnd` in Win32 Z-order.
fn discover_popup_hwnds(target_hwnd: u32, win_rect: (i32, i32, i32, i32)) -> Vec<(u32, &'static str, (i32, i32, i32, i32))> {
    use windows_sys::Win32::{
        Foundation::{HWND, RECT},
        Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS},
        UI::WindowsAndMessaging::*,
    };
    let target = target_hwnd as usize as HWND;
    if target.is_null() {
        return Vec::new();
    }
    let mut target_pid = 0u32;
    let win_area = (win_rect.2.max(0) as i64) * (win_rect.3.max(0) as i64);
    let mut out = Vec::new();
    unsafe {
        if GetWindowThreadProcessId(target, &mut target_pid) == 0 || target_pid == 0 {
            return out;
        }
        let mut cur = GetTopWindow(null_mut());
        for _ in 0..64 {
            if cur.is_null() || cur == target {
                break;
            }
            if IsWindowVisible(cur) != 0 && IsIconic(cur) == 0 {
                let mut cloaked = 0u32;
                let _ = DwmGetWindowAttribute(cur, DWMWA_CLOAKED as u32, &mut cloaked as *mut _ as _, 4);
                let mut affinity = 0u32;
                let excluded = GetWindowDisplayAffinity(cur, &mut affinity) != 0 && affinity == WDA_EXCLUDEFROMCAPTURE;
                let ex = GetWindowLongW(cur, GWL_EXSTYLE) as u32;
                if cloaked == 0 && !excluded && (ex & WS_EX_TRANSPARENT) == 0 {
                    let mut class_buf = [0u16; 64];
                    let clen = GetClassNameW(cur, class_buf.as_mut_ptr(), class_buf.len() as i32);
                    let class = if clen > 0 {
                        String::from_utf16_lossy(&class_buf[..clen as usize])
                    } else {
                        String::new()
                    };
                    let mut pid = 0u32;
                    GetWindowThreadProcessId(cur, &mut pid);
                    let mut rect = RECT::default();
                    let has_rect = DwmGetWindowAttribute(
                        cur,
                        DWMWA_EXTENDED_FRAME_BOUNDS as u32,
                        &mut rect as *mut _ as _,
                        std::mem::size_of::<RECT>() as u32,
                    ) >= 0 || GetWindowRect(cur, &mut rect) != 0;
                    if has_rect {
                        let w = rect.right.saturating_sub(rect.left);
                        let h = rect.bottom.saturating_sub(rect.top);
                        if w >= 40 && h >= 24 {
                            let is_sys_menu = class == "#32768" || class == "ComboLBox";
                            let style = GetWindowLongW(cur, GWL_STYLE) as u32;
                            let owned = GetWindow(cur, GW_OWNER) == target;
                            let area = (w as i64) * (h as i64);
                            let same_proc_popup = pid == target_pid
                                && (owned || (style & WS_POPUP) != 0 || (win_area > 0 && area < win_area * 92 / 100));
                            if is_sys_menu || same_proc_popup {
                                let kind = if is_sys_menu || class.to_ascii_lowercase().contains("menu") {
                                    "menu"
                                } else {
                                    "dialog"
                                };
                                out.push((cur as usize as u32, kind, (rect.left, rect.top, w, h)));
                            }
                        }
                    }
                }
            }
            cur = GetWindow(cur, GW_HWNDNEXT);
        }
    }
    out
}

unsafe fn make_tools() -> Result<Tools, String> {
    let automation: IUIAutomation2 = CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER)
        .map_err(|e| format!("UIAutomation 不可用：{e}"))?;

    let cache = automation.CreateCacheRequest().map_err(|e| e.to_string())?;
    for prop in [
        UIA_NamePropertyId,
        UIA_AutomationIdPropertyId,
        UIA_ControlTypePropertyId,
        UIA_LocalizedControlTypePropertyId,
        UIA_BoundingRectanglePropertyId,
        UIA_IsOffscreenPropertyId,
        UIA_IsEnabledPropertyId,
        UIA_IsPasswordPropertyId,
        UIA_IsKeyboardFocusablePropertyId,
        UIA_ValueValuePropertyId,
        UIA_ValueIsReadOnlyPropertyId,
        UIA_ToggleToggleStatePropertyId,
        UIA_SelectionItemIsSelectedPropertyId,
        UIA_ScrollVerticallyScrollablePropertyId,
        UIA_ScrollVerticalScrollPercentPropertyId,
        UIA_ScrollVerticalViewSizePropertyId,
    ] {
        let _ = cache.AddProperty(prop);
    }

    let walker = automation.ControlViewWalker().map_err(|e| e.to_string())?;
    Ok((automation, cache, walker))
}

#[allow(clippy::too_many_arguments)]
fn query(
    (automation, cache, walker): &Tools,
    hwnd: u32,
    depth: usize,
    max_items: usize,
    filter: Option<&str>,
    deadline: Duration,
    visit_limit: usize,
    interactive_only: bool,
) -> Result<Report, String> {
    let started = Instant::now();
    unsafe {
        let timeout_ms = if interactive_only {
            deadline.as_millis().clamp(80, 120) as u32
        } else {
            deadline.as_millis().clamp(100, 300) as u32
        };
        let _ = automation.SetConnectionTimeout(timeout_ms);
        let _ = automation.SetTransactionTimeout(timeout_ms);

        let root = automation
            .ElementFromHandleBuildCache(UiaHwnd(hwnd as _), cache)
            .map_err(|e| format!("读不到该窗口的控件树：{e}"))?;
        if root.CachedIsOffscreen().map(|v| v.as_bool()).unwrap_or(false) {
            return Err("窗口当前不可见（已最小化或被隐藏），先让它显示在桌面上再观察".into());
        }
        let win_rect = root
            .CachedBoundingRectangle()
            .ok()
            .map(|r| (r.left, r.top, r.right.saturating_sub(r.left), r.bottom.saturating_sub(r.top)))
            .unwrap_or((0, 0, 0, 0));

        let filter_low = filter.map(str::to_lowercase);
        let mut report = Report {
            items: Vec::new(),
            overlay: None,
            scroll: None,
            truncated: false,
            truncated_by: None,
            visited: 0,
            elapsed_ms: 0,
        };
        let mut best_overlay: Option<(&'static str, (i32, i32, i32, i32))> = None;
        let mut best_scroll: Option<(ScrollInfo, i64)> = None;
        let mut popup_rects: Vec<(i32, i32, i32, i32)> = Vec::new();

        // 1. Walk top-level popup menus / dialogs above `hwnd` first.
        for (pop_hwnd, pop_kind, pop_rect) in discover_popup_hwnds(hwnd, win_rect) {
            if let Ok(pop_root) = automation.ElementFromHandleBuildCache(UiaHwnd(pop_hwnd as _), cache) {
                let before_len = report.items.len();
                walk_bfs(
                    walker,
                    cache,
                    &pop_root,
                    depth.max(4),
                    max_items,
                    filter_low.as_deref(),
                    started,
                    deadline,
                    visit_limit,
                    interactive_only,
                    pop_rect,
                    &[],
                    &mut None,
                    &mut best_scroll,
                    &mut report,
                );
                if report.items.len() > before_len {
                    for item in &mut report.items[before_len..] {
                        item.in_overlay = true;
                    }
                    popup_rects.push(pop_rect);
                    if best_overlay.is_none() {
                        best_overlay = Some((pop_kind, pop_rect));
                    }
                }
            }
            if report.items.len() >= max_items || started.elapsed() > deadline {
                break;
            }
        }

        // 2. Walk main window control tree via iterative BFS.
        if report.items.len() < max_items && started.elapsed() <= deadline {
            walk_bfs(
                walker,
                cache,
                &root,
                depth,
                max_items,
                filter_low.as_deref(),
                started,
                deadline,
                visit_limit,
                interactive_only,
                win_rect,
                &popup_rects,
                &mut best_overlay,
                &mut best_scroll,
                &mut report,
            );
        }

        // 3. Prioritize overlay controls first and compute outsidePoint.
        if let Some((kind, ov_rect)) = best_overlay {
            let mut has_confirm = false;
            for item in &mut report.items {
                if item.in_overlay || rect_contains(ov_rect, item.rect) {
                    item.in_overlay = true;
                    if !item.disabled
                        && matches!(item.kind, UIA_ButtonControlTypeId | UIA_SplitButtonControlTypeId)
                        && is_confirm_name(&item.name)
                    {
                        has_confirm = true;
                    }
                }
            }
            if report.items.iter().any(|i| i.in_overlay) {
                report.items.sort_by_key(|i| !i.in_overlay);
                let outside_point = compute_outside_point(win_rect, ov_rect, &report.items);
                report.overlay = Some(OverlayInfo { kind, rect: ov_rect, has_confirm, outside_point });
            }
        }
        report.scroll = best_scroll.map(|(s, _)| s);
        report.elapsed_ms = started.elapsed().as_millis();
        Ok(report)
    }
}

#[allow(clippy::too_many_arguments)]
unsafe fn walk_bfs(
    walker: &IUIAutomationTreeWalker,
    cache: &IUIAutomationCacheRequest,
    root: &IUIAutomationElement,
    max_depth: usize,
    max_items: usize,
    filter: Option<&str>,
    started: Instant,
    deadline: Duration,
    visit_limit: usize,
    interactive_only: bool,
    win_rect: (i32, i32, i32, i32),
    occluding_popups: &[(i32, i32, i32, i32)],
    best_overlay: &mut Option<(&'static str, (i32, i32, i32, i32))>,
    best_scroll: &mut Option<(ScrollInfo, i64)>,
    report: &mut Report,
) {
    let mut queue = VecDeque::new();
    queue.push_back((root.clone(), 0usize));

    while let Some((node, level)) = queue.pop_front() {
        report.visited += 1;
        if report.visited > visit_limit {
            report.mark_truncated("visitLimit");
            return;
        }
        if started.elapsed() > deadline {
            report.mark_truncated("deadline");
            return;
        }
        if node.CachedIsOffscreen().map(|v| v.as_bool()).unwrap_or(true) {
            continue;
        }

        let kind = node.CachedControlType().unwrap_or(UIA_CONTROLTYPE_ID(0));
        let rect_opt = node.CachedBoundingRectangle().ok().and_then(|r| {
            let (w, h) = (r.right.saturating_sub(r.left), r.bottom.saturating_sub(r.top));
            (w > 0 && h > 0).then_some((r.left, r.top, w, h))
        });

        // Detect open child menus or modal dialogs inside the window.
        if level > 0 {
            if let Some(rect) = rect_opt {
                let win_area = (win_rect.2 as i64) * (win_rect.3 as i64);
                let area = (rect.2 as i64) * (rect.3 as i64);
                if rect.2 >= 80 && rect.3 >= 48 && (win_area == 0 || area < win_area * 90 / 100) {
                    if kind == UIA_MenuControlTypeId {
                        *best_overlay = Some(("menu", rect));
                    } else if kind == UIA_WindowControlTypeId && best_overlay.is_none() {
                        *best_overlay = Some(("dialog", rect));
                    }
                }
            }
        }

        // Detect primary vertically scrollable region via cached ScrollPattern properties.
        let scroll_state = read_scroll(&node);
        if let (Some((pct, view_size)), Some(rect)) = (scroll_state, rect_opt) {
            let area = (rect.2 as i64) * (rect.3 as i64);
            if rect.2 >= 180 && rect.3 >= 140 && view_size < 96.0 && pct < 98.5 {
                if best_scroll.as_ref().is_none_or(|(_, best_a)| area > *best_a) {
                    *best_scroll = Some((ScrollInfo { rect, percent: pct, view_size }, area));
                }
            }
        }

        if level > 0 {
            if let Some(item) = describe(&node, kind, rect_opt, scroll_state, level, filter, interactive_only) {
                let occluded_by_popup = occluding_popups.iter().any(|&pr| rect_contains(pr, item.rect));
                if !occluded_by_popup && !report.items.iter().any(|ex| near_duplicate_item(ex, &item)) {
                    report.items.push(item);
                    if report.items.len() >= max_items {
                        report.mark_truncated("itemLimit");
                        return;
                    }
                }
            }
        }

        if level >= max_depth {
            continue;
        }

        let mut child = match relative_cached(walker, cache, &node, true) {
            Some(child) => child,
            None => {
                report.mark_truncated("queryFailed");
                return;
            }
        };
        let mut repetitive_siblings = 0usize;
        while let Some(element) = child {
            if started.elapsed() > deadline {
                report.mark_truncated("deadline");
                return;
            }
            let next_sibling = relative_cached(walker, cache, &element, false);
            let visible_child = !element.CachedIsOffscreen().map(|v| v.as_bool()).unwrap_or(true);
            let child_kind = element.CachedControlType().unwrap_or(UIA_CONTROLTYPE_ID(0));
            let skip_repetitive = interactive_only
                && filter.is_none()
                && visible_child
                && is_repetitive_kind(child_kind)
                && {
                    repetitive_siblings += 1;
                    repetitive_siblings > FAST_REPETITIVE_SIBLING_CAP
                };
            if visible_child && !skip_repetitive {
                queue.push_back((element, level + 1));
            }
            child = match next_sibling {
                Some(next) => next,
                None => {
                    report.mark_truncated("queryFailed");
                    return;
                }
            };
        }
    }
}

unsafe fn read_scroll(node: &IUIAutomationElement) -> Option<(f64, f64)> {
    let scrollable = node
        .GetCachedPropertyValue(UIA_ScrollVerticallyScrollablePropertyId)
        .ok()
        .and_then(|v| var_bool(&v))
        .unwrap_or(false);
    if !scrollable {
        return None;
    }
    let pct = node
        .GetCachedPropertyValue(UIA_ScrollVerticalScrollPercentPropertyId)
        .ok()
        .and_then(|v| var_f64(&v))
        .filter(|v| v.is_finite() && *v >= 0.0)
        .unwrap_or(0.0);
    let view = node
        .GetCachedPropertyValue(UIA_ScrollVerticalViewSizePropertyId)
        .ok()
        .and_then(|v| var_f64(&v))
        .filter(|v| v.is_finite() && *v > 0.0)
        .unwrap_or(100.0);
    Some((pct.clamp(0.0, 100.0), view.clamp(1.0, 100.0)))
}

unsafe fn describe(
    node: &IUIAutomationElement,
    kind: UIA_CONTROLTYPE_ID,
    rect_opt: Option<(i32, i32, i32, i32)>,
    scroll: Option<(f64, f64)>,
    depth: usize,
    filter: Option<&str>,
    interactive_only: bool,
) -> Option<Item> {
    let rect = rect_opt?;
    if rect.2 < 4 || rect.3 < 4 {
        return None;
    }
    let name = node.CachedName().map(|v| v.to_string().trim().to_string()).unwrap_or_default();
    let automation_id = node.CachedAutomationId().map(|v| v.to_string().trim().to_string()).unwrap_or_default();
    let focusable = node.CachedIsKeyboardFocusable().map(|v| v.as_bool()).unwrap_or(false);
    let interactive = is_interactive_kind(kind);

    if interactive_only {
        if !interactive && scroll.is_none() && !(focusable && !name.is_empty()) {
            return None;
        }
        // Skip nameless unlabeled buttons/listitems unless they are editable inputs or scrollable containers
        if name.is_empty() && automation_id.is_empty()
            && !matches!(kind, UIA_EditControlTypeId | UIA_ComboBoxControlTypeId)
            && scroll.is_none()
        {
            return None;
        }
    } else if name.is_empty() && automation_id.is_empty() && scroll.is_none() {
        return None;
    }

    let role = node.CachedLocalizedControlType().map(|v| v.to_string()).unwrap_or_default();
    if let Some(filter) = filter {
        if ![&name, &automation_id, &role].iter().any(|v| v.to_lowercase().contains(filter)) {
            return None;
        }
    }

    let enabled = node.CachedIsEnabled().map(|v| v.as_bool()).unwrap_or(true);
    let disabled = !enabled;
    let is_pwd = node.CachedIsPassword().map(|v| v.as_bool()).unwrap_or(false);
    let read_only = node
        .GetCachedPropertyValue(UIA_ValueIsReadOnlyPropertyId)
        .ok()
        .and_then(|v| var_bool(&v))
        .unwrap_or(false);
    let editable = enabled
        && !read_only
        && matches!(kind, UIA_EditControlTypeId | UIA_ComboBoxControlTypeId | UIA_DocumentControlTypeId);

    let value = if matches!(kind, UIA_EditControlTypeId | UIA_ComboBoxControlTypeId) {
        node.GetCachedPropertyValue(UIA_ValueValuePropertyId)
            .ok()
            .and_then(|v| var_str(&v))
            .map(|s| if is_pwd { "***".to_string() } else { s })
    } else {
        None
    };

    let toggle = node
        .GetCachedPropertyValue(UIA_ToggleToggleStatePropertyId)
        .ok()
        .and_then(|v| var_i32(&v))
        .and_then(|s| match s {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        });
    let selected = node
        .GetCachedPropertyValue(UIA_SelectionItemIsSelectedPropertyId)
        .ok()
        .and_then(|v| var_bool(&v))
        .filter(|&b| b);
    let checked = toggle.or(selected);

    Some(Item {
        kind,
        role,
        name: name.chars().take(100).collect(),
        automation_id,
        rect,
        depth,
        disabled,
        editable,
        checked,
        value,
        scroll,
        in_overlay: false,
    })
}

unsafe fn relative_cached(
    walker: &IUIAutomationTreeWalker,
    cache: &IUIAutomationCacheRequest,
    node: &IUIAutomationElement,
    first: bool,
) -> Option<Option<IUIAutomationElement>> {
    let mut raw = null_mut();
    let call = if first {
        walker.vtable().GetFirstChildElementBuildCache
    } else {
        walker.vtable().GetNextSiblingElementBuildCache
    };
    call(walker.as_raw(), node.as_raw(), cache.as_raw(), &mut raw).ok().ok()?;
    Some((!raw.is_null()).then(|| IUIAutomationElement::from_raw(raw)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncation_keeps_the_first_reason() {
        let mut report = Report {
            items: Vec::new(),
            overlay: None,
            scroll: None,
            truncated: false,
            truncated_by: None,
            visited: 0,
            elapsed_ms: 0,
        };
        report.mark_truncated("itemLimit");
        report.mark_truncated("deadline");
        assert!(report.truncated);
        assert_eq!(report.truncated_by, Some("itemLimit"));
    }

    #[test]
    fn outside_point_avoids_overlay_and_existing_controls() {
        let win = (0, 0, 1200, 800);
        let ov = (400, 200, 300, 220);
        let pt = compute_outside_point(win, ov, &[]).expect("safe outside point");
        assert!(pt.0 < ov.0 || pt.0 > ov.0 + ov.2 || pt.1 < ov.1 || pt.1 > ov.1 + ov.3);
        assert!(pt.0 >= win.0 && pt.0 <= win.0 + win.2 && pt.1 >= win.1 && pt.1 <= win.1 + win.3);
    }

    #[test]
    #[ignore = "needs a real desktop: cargo test --lib uia_reads_a_real_window -- --ignored"]
    fn uia_reads_a_real_window() {
        let own = std::process::id();
        let target = xcap::Window::all().ok().and_then(|all| {
            all.into_iter().find(|w| {
                w.pid().ok() != Some(own)
                    && w.is_minimized().ok() == Some(false)
                    && w.width().ok().unwrap_or(0) > 200
                    && w.height().ok().unwrap_or(0) > 200
            })
        });
        let Some(window) = target else { return };
        let report = collect_fast(window.id().unwrap(), 60, None).expect("fast UIA query");
        assert!(report.visited > 1, "only the root was reachable");
        assert!(report.elapsed_ms < 1000, "query took {} ms", report.elapsed_ms);
    }
}
