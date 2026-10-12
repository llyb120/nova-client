//! A real Explorer child, never a floating window or a taskbar layout modification.
//! UIA lives on a separate MTA: querying our own HWND on the message thread deadlocks.

use super::{free_slot, snapshot, Rect};
use std::{
    ptr::{null, null_mut},
    sync::{atomic::{AtomicUsize, Ordering}, Arc, Mutex, OnceLock},
    thread,
    time::{Duration, Instant},
};
use windows::{
    core::Interface,
    Win32::{
        Foundation::HWND as UiaHwnd,
        System::Com::{CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED},
        UI::Accessibility::*,
    },
};
use windows_sys::{
    core::w,
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
        Graphics::{Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED}, Gdi::*},
        System::LibraryLoader::GetModuleHandleW,
        UI::{HiDpi::*, WindowsAndMessaging::*},
    },
};

const CLASS: *const u16 = w!("Nova.TaskbarStatus");
const MAX_AGE: Duration = Duration::from_millis(1500);
static APP: OnceLock<tauri::AppHandle> = OnceLock::new();
static WINDOW: AtomicUsize = AtomicUsize::new(0);
static THREAD: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
const REFRESH: u32 = WM_APP + 1;
static LAYOUT: Mutex<Option<Arc<Layout>>> = Mutex::new(None);

struct Layout {
    parent: usize,
    pid: u32,
    taskbar: Rect,
    slot: Rect,
    measured: Instant,
}

pub(super) fn start(app: tauri::AppHandle) {
    if APP.set(app).is_err() {
        return;
    }
    if let Err(error) = thread::Builder::new().name("nova-taskbar-status".into()).spawn(|| unsafe { message_loop() }) {
        eprintln!("[taskbar-status] cannot start native thread: {error}");
    }
}

pub(super) fn wake() {
    let thread = THREAD.load(Ordering::Acquire);
    if thread != 0 { unsafe { PostThreadMessageW(thread, REFRESH, 0, 0); } }
}

/// Read the *actual* visible physical rectangle, including Explorer-driven movement.
/// The caller must compare before/after rectangles before masking a text refresh.
pub(super) fn bounds() -> Option<Rect> {
    unsafe {
        let hwnd = WINDOW.load(Ordering::Acquire) as HWND;
        if hwnd.is_null() || IsWindowVisible(hwnd) == 0 {
            return None;
        }
        let previous = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        if previous.is_null() {
            return None;
        }
        let result = (|| {
            let parent = GetParent(hwnd);
            if parent != FindWindowW(w!("Shell_TrayWnd"), null()) { return None; }
            let mut pid = 0;
            GetWindowThreadProcessId(hwnd, &mut pid);
            if pid != std::process::id() { return None; }
            taskbar_rect(parent)?;
            let rect = window_rect(hwnd)?;
            // Never exempt an app/menu/tooltip painted over this rectangle. Walk only
            // top-level windows ahead of the taskbar in Z order, failing closed on errors.
            let mut above = GetTopWindow(null_mut());
            let mut remaining = 256;
            while above != parent {
                if above.is_null() || remaining == 0 { return None; }
                remaining -= 1;
                if IsWindowVisible(above) != 0 && IsIconic(above) == 0 {
                    let r = window_rect(above)?;
                    if r.left < rect.right && r.right > rect.left && r.top < rect.bottom && r.bottom > rect.top {
                        let mut cloaked = 0u32;
                        if DwmGetWindowAttribute(above, DWMWA_CLOAKED as u32, &mut cloaked as *mut _ as _, 4) < 0 || cloaked == 0 { return None; }
                    }
                }
                above = GetWindow(above, GW_HWNDNEXT);
            }
            let middle = POINT { x: rect.left + (rect.right - rect.left) / 2, y: rect.top + (rect.bottom - rect.top) / 2 };
            if WindowFromPoint(middle) != hwnd { return None; }
            Some(rect)
        })();
        SetThreadDpiAwarenessContext(previous);
        result
    }
}

unsafe fn window_rect(hwnd: HWND) -> Option<Rect> {
    let mut r = RECT::default();
    (GetWindowRect(hwnd, &mut r) != 0).then_some(Rect { left: r.left, top: r.top, right: r.right, bottom: r.bottom })
}

fn same_rect(a: &Rect, b: &Rect) -> bool {
    (a.left, a.top, a.right, a.bottom) == (b.left, b.top, b.right, b.bottom)
}

unsafe fn taskbar_rect(hwnd: HWND) -> Option<Rect> {
    if hwnd.is_null() || IsWindowVisible(hwnd) == 0 {
        return None;
    }
    let mut cloaked = 0u32;
    if DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED as u32, &mut cloaked as *mut _ as _, 4) < 0 || cloaked != 0 {
        return None;
    }
    let r = window_rect(hwnd)?;
    let mut monitor = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
    if GetMonitorInfoW(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONULL), &mut monitor) == 0 {
        return None;
    }
    let m = monitor.rcMonitor;
    // Only a fully revealed, single-row horizontal main taskbar. Auto-hide motion
    // and unsupported shell replacements fail closed instead of guessing a slot.
    let dpi = GetDpiForWindow(hwnd);
    (dpi > 0 && r.left == m.left && r.right == m.right
        && (r.top == m.top || r.bottom == m.bottom)
        && r.top >= m.top && r.bottom <= m.bottom
        && r.bottom - r.top >= (24 * dpi / 96) as i32
        && r.bottom - r.top <= (96 * dpi / 96) as i32
        && r.right - r.left > r.bottom - r.top)
        .then_some(r)
}

unsafe fn layout_worker() {
    if CoInitializeEx(None, COINIT_MULTITHREADED).is_err() {
        eprintln!("[taskbar-status] cannot initialize layout COM apartment");
        return;
    }
    SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    // No COM objects, UIA calls or foreign-window messages while holding LAYOUT.
    loop {
        let layout = APP.get().and_then(snapshot).and_then(|_| query_layout()).map(Arc::new);
        *LAYOUT.lock().unwrap_or_else(|e| e.into_inner()) = layout;
        thread::sleep(Duration::from_millis(500));
        if APP.get().is_none() {
            break;
        }
    }
    CoUninitialize();
}

unsafe fn query_layout() -> Option<Layout> {
    let measured = Instant::now();
    let parent = FindWindowW(w!("Shell_TrayWnd"), null());
    let taskbar = taskbar_rect(parent)?;
    let mut pid = 0;
    GetWindowThreadProcessId(parent, &mut pid);
    if pid == 0 { return None; }
    let automation: IUIAutomation2 = CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER).ok()?;
    automation.SetConnectionTimeout(300).ok()?;
    automation.SetTransactionTimeout(300).ok()?;
    let root = automation.ElementFromHandle(UiaHwnd(parent)).ok()?;
    let root_rect = root.CurrentBoundingRectangle().ok()?;
    if !same_rect(&taskbar, &Rect { left: root_rect.left, top: root_rect.top, right: root_rect.right, bottom: root_rect.bottom }) {
        return None;
    }
    let walker = automation.ControlViewWalker().ok()?;
    let mut occupied = Vec::new();
    let mut remaining = 256;
    collect(&walker, &root, 0, &mut remaining, measured, &mut occupied)?;
    // An empty/partial accessibility tree is not evidence that the taskbar is empty.
    if occupied.len() < 2 || measured.elapsed() > Duration::from_millis(900) {
        return None;
    }
    let dpi = GetDpiForWindow(parent) as i32;
    let slot = free_slot(Rect { left: taskbar.left, top: taskbar.top, right: taskbar.right, bottom: taskbar.bottom }, &occupied, 240 * dpi / 96, 160 * dpi / 96, 8 * dpi / 96)?;
    if !same_rect(&taskbar, &taskbar_rect(parent)?) || FindWindowW(w!("Shell_TrayWnd"), null()) != parent {
        return None;
    }
    Some(Layout { parent: parent as usize, pid, taskbar, slot, measured })
}

/// Preserve S_OK + null (end of traversal) separately from a failed UIA query.
/// The generated non-optional wrappers turn both into Err, losing this distinction.
unsafe fn relative(walker: &IUIAutomationTreeWalker, node: &IUIAutomationElement, first: bool) -> Option<Option<IUIAutomationElement>> {
    let mut raw = null_mut();
    let call = if first { walker.vtable().GetFirstChildElement } else { walker.vtable().GetNextSiblingElement };
    call(walker.as_raw(), node.as_raw(), &mut raw).ok().ok()?;
    Some((!raw.is_null()).then(|| IUIAutomationElement::from_raw(raw)))
}

#[allow(non_upper_case_globals)] // Windows SDK names.
fn occupies(kind: UIA_CONTROLTYPE_ID, leaf: bool, focusable: bool, actionable: bool) -> bool {
    let container = matches!(kind, UIA_PaneControlTypeId | UIA_GroupControlTypeId | UIA_ToolBarControlTypeId | UIA_ListControlTypeId | UIA_TabControlTypeId | UIA_WindowControlTypeId | UIA_CustomControlTypeId);
    // Opaque leaves occupy their entire bounds; never infer blank space inside them.
    leaf || actionable || (!container && (focusable || matches!(kind, UIA_ButtonControlTypeId | UIA_SplitButtonControlTypeId | UIA_ListItemControlTypeId | UIA_MenuItemControlTypeId | UIA_TabItemControlTypeId)))
}

unsafe fn collect(walker: &IUIAutomationTreeWalker, node: &IUIAutomationElement, depth: usize, remaining: &mut usize, started: Instant, occupied: &mut Vec<Rect>) -> Option<()> {
    // ponytail: trust a complete ControlView tree, at most 12 levels / 256 nodes /
    // 900 ms. Opaque controls reserve their entire rectangle; incomplete queries
    // hide the strip. Upgrade to verified shell adapters/UIA events, never guess gaps.
    if depth > 12 || *remaining == 0 || started.elapsed() > Duration::from_millis(900) { return None; }
    *remaining -= 1;
    if node.CurrentProcessId().ok()? == std::process::id() as i32 || node.CurrentIsOffscreen().ok()?.as_bool() {
        return Some(());
    }
    let mut child = relative(walker, node, true)?;
    if depth != 0 {
        let r = node.CurrentBoundingRectangle().ok()?;
        if r.right > r.left && r.bottom > r.top {
            let kind = node.CurrentControlType().ok()?;
            let mut actionable = false;
            for property in [UIA_IsInvokePatternAvailablePropertyId, UIA_IsTogglePatternAvailablePropertyId, UIA_IsSelectionItemPatternAvailablePropertyId, UIA_IsExpandCollapsePatternAvailablePropertyId] {
                actionable |= bool::try_from(&node.GetCurrentPropertyValue(property).ok()?).ok()?;
            }
            if occupies(kind, child.is_none(), node.CurrentIsKeyboardFocusable().ok()?.as_bool(), actionable) {
                occupied.push(Rect { left: r.left, top: r.top, right: r.right, bottom: r.bottom });
            }
        }
    }
    while let Some(element) = child {
        collect(walker, &element, depth + 1, remaining, started, occupied)?;
        child = relative(walker, &element, false)?;
    }
    Some(())
}

unsafe fn message_loop() {
    if SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2).is_null() { return; }
    let instance = GetModuleHandleW(null());
    let class = WNDCLASSW { lpfnWndProc: Some(window_proc), hInstance: instance, lpszClassName: CLASS, hCursor: LoadCursorW(null_mut(), IDC_HAND), ..Default::default() };
    if RegisterClassW(&class) == 0 { return; }
    let timer = SetTimer(null_mut(), 0, 250, None);
    if timer == 0 { UnregisterClassW(CLASS, instance); return; }
    if let Err(error) = thread::Builder::new().name("nova-taskbar-layout".into()).spawn(|| unsafe { layout_worker() }) {
        eprintln!("[taskbar-status] cannot start layout worker: {error}");
        KillTimer(null_mut(), timer);
        UnregisterClassW(CLASS, instance);
        return;
    }
    THREAD.store(windows_sys::Win32::System::Threading::GetCurrentThreadId(), Ordering::Release);
    let mut text = String::new();
    let mut message = MSG::default();
    while GetMessageW(&mut message, null_mut(), 0, 0) > 0 {
        if message.hwnd.is_null() && (message.message == REFRESH || (message.message == WM_TIMER && message.wParam == timer)) {
            refresh(&mut text);
        } else {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    THREAD.store(0, Ordering::Release);
    KillTimer(null_mut(), timer);
    let hwnd = WINDOW.swap(0, Ordering::AcqRel) as HWND;
    if !hwnd.is_null() { DestroyWindow(hwnd); }
    UnregisterClassW(CLASS, instance);
}

unsafe fn refresh(text: &mut String) {
    let mut hwnd = WINDOW.load(Ordering::Acquire) as HWND;
    let summary = APP.get().and_then(snapshot);
    let layout = LAYOUT.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let ready = layout.as_ref().filter(|layout| {
        let parent = layout.parent as HWND;
        let mut pid = 0;
        GetWindowThreadProcessId(parent, &mut pid);
        layout.measured.elapsed() < MAX_AGE && pid == layout.pid
            && FindWindowW(w!("Shell_TrayWnd"), null()) == parent
            && taskbar_rect(parent).is_some_and(|r| same_rect(&r, &layout.taskbar))
    });
    let (Some(summary), Some(layout)) = (summary, ready) else {
        if !hwnd.is_null() { ShowWindow(hwnd, SW_HIDE); }
        return;
    };
    let parent = layout.parent as HWND;
    if !hwnd.is_null() && (IsWindow(hwnd) == 0 || GetParent(hwnd) != parent) {
        WINDOW.store(0, Ordering::Release);
        if IsWindow(hwnd) != 0 { DestroyWindow(hwnd); }
        hwnd = null_mut();
    }
    if hwnd.is_null() {
        // Match the parent while creating the cross-process child. Never SetParent:
        // that can reset process DPI awareness. Restore physical coordinates after.
        let previous = SetThreadDpiAwarenessContext(GetWindowDpiAwarenessContext(parent));
        if previous.is_null() { return; }
        hwnd = CreateWindowExW(WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW, CLASS, w!("Nova taskbar status"), WS_CHILD | WS_CLIPSIBLINGS, 0, 0, 0, 0, parent, null_mut(), GetModuleHandleW(null()), null());
        SetThreadDpiAwarenessContext(previous);
        if hwnd.is_null() { return; }
        WINDOW.store(hwnd as usize, Ordering::Release);
        text.clear();
    }
    let title = summary;
    if *text != title {
        let wide: Vec<u16> = title.chars().take(500).collect::<String>().encode_utf16().chain(Some(0)).collect();
        if SetWindowTextW(hwnd, wide.as_ptr()) == 0 { ShowWindow(hwnd, SW_HIDE); return; }
        *text = title;
        InvalidateRect(hwnd, null(), 0);
    }
    let current = window_rect(hwnd);
    if IsWindowVisible(hwnd) == 0 || !current.as_ref().is_some_and(|r| same_rect(r, &layout.slot)) {
        // UIA rectangles are physical; child placement is in its parent's client units.
        let previous = SetThreadDpiAwarenessContext(GetWindowDpiAwarenessContext(parent));
        if previous.is_null() { ShowWindow(hwnd, SW_HIDE); return; }
        let mut a = POINT { x: layout.slot.left, y: layout.slot.top };
        let mut b = POINT { x: layout.slot.right, y: layout.slot.bottom };
        let mapped = PhysicalToLogicalPointForPerMonitorDPI(parent, &mut a) != 0
            && PhysicalToLogicalPointForPerMonitorDPI(parent, &mut b) != 0
            && ScreenToClient(parent, &mut a) != 0 && ScreenToClient(parent, &mut b) != 0;
        let placed = mapped && SetWindowPos(hwnd, HWND_TOP, a.x, a.y, b.x - a.x, b.y - a.y, SWP_NOACTIVATE | SWP_SHOWWINDOW) != 0;
        SetThreadDpiAwarenessContext(previous);
        if !placed { ShowWindow(hwnd, SW_HIDE); }
    }
}

unsafe extern "system" fn window_proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match message {
        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
        WM_LBUTTONUP => {
            ShowWindow(hwnd, SW_HIDE);
            if let Some(app) = APP.get() { crate::sys_notify::focus_main_window(app); }
            0
        }
        WM_ERASEBKGND => 1,
        WM_PAINT => {
            let mut paint = PAINTSTRUCT::default();
            let dc = BeginPaint(hwnd, &mut paint);
            if !dc.is_null() {
                let mut rect = RECT::default();
                GetClientRect(hwnd, &mut rect);
                FillRect(dc, &rect, GetSysColorBrush(COLOR_BTNFACE));
                SetBkMode(dc, TRANSPARENT as i32);
                SetTextColor(dc, GetSysColor(COLOR_BTNTEXT));
                let dpi = GetDpiForWindow(hwnd).max(96) as i32;
                let font = CreateFontW(-12 * dpi / 96, 0, 0, 0, FW_NORMAL as i32, 0, 0, 0, DEFAULT_CHARSET as u32, 0, 0, CLEARTYPE_QUALITY as u32, 0, w!("Segoe UI"));
                let old = if font.is_null() { null_mut() } else { SelectObject(dc, font) };
                rect.left += 8 * dpi / 96;
                rect.right -= 8 * dpi / 96;
                let mut text = [0u16; 1024];
                let length = GetWindowTextW(hwnd, text.as_mut_ptr(), text.len() as i32);
                DrawTextW(dc, text.as_ptr(), length, &mut rect, DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX);
                if !font.is_null() { SelectObject(dc, old); DeleteObject(font); }
            }
            EndPaint(hwnd, &paint);
            0
        }
        WM_NCDESTROY => {
            let _ = WINDOW.compare_exchange(hwnd as usize, 0, Ordering::AcqRel, Ordering::Acquire);
            DefWindowProcW(hwnd, message, wparam, lparam)
        }
        _ => DefWindowProcW(hwnd, message, wparam, lparam),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn containers_are_not_blank_space_or_giant_controls() {
        assert!(!occupies(UIA_PaneControlTypeId, false, true, false));
        assert!(!occupies(UIA_ToolBarControlTypeId, false, true, false));
        assert!(occupies(UIA_CustomControlTypeId, true, false, false));
        assert!(occupies(UIA_ButtonControlTypeId, false, false, true));
        assert!(occupies(UIA_CustomControlTypeId, false, false, true));
        assert!(occupies(UIA_TextControlTypeId, true, false, false));
    }
}
