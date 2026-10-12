//! A real Explorer child, never a floating window or a taskbar layout modification.
//! UIA lives on a separate MTA: querying our own HWND on the message thread deadlocks.

use super::{free_slot, snapshot, Kind, Rect, Status};
use std::{
    cell::{Cell, RefCell},
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
        Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM},
        Graphics::{Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED}, Gdi::*},
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Controls::{
                InitCommonControlsEx, ICC_WIN95_CLASSES, INITCOMMONCONTROLSEX, NMHDR, NMTTDISPINFOW,
                TOOLTIPS_CLASSW, TTDT_AUTOPOP, TTF_IDISHWND, TTF_SUBCLASS, TTM_ADDTOOLW, TTM_SETDELAYTIME,
                TTM_SETMAXTIPWIDTH, TTN_GETDISPINFOW, TTS_ALWAYSTIP, TTS_NOPREFIX, TTTOOLINFOW,
            },
            HiDpi::*, Input::KeyboardAndMouse::{TrackMouseEvent, TRACKMOUSEEVENT, TME_LEAVE}, WindowsAndMessaging::*,
        },
    },
};

const CLASS: *const u16 = w!("Nova.TaskbarStatus");
const MAX_AGE: Duration = Duration::from_millis(1500);
static APP: OnceLock<tauri::AppHandle> = OnceLock::new();
static WINDOW: AtomicUsize = AtomicUsize::new(0);
static THREAD: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
const REFRESH: u32 = WM_APP + 1;
const WM_MOUSELEAVE: u32 = 0x02A3; // Win32 Controls message paired with TME_LEAVE.
static LAYOUT: Mutex<Option<Arc<Layout>>> = Mutex::new(None);
const ANIMATION: usize = 1;

// Everything below is touched only by the status message thread (refresh + window_proc).
thread_local! {
    static CURRENT: RefCell<Option<Status>> = const { RefCell::new(None) };
    static TIP: RefCell<Vec<u16>> = const { RefCell::new(Vec::new()) };
    static TOOLTIP: Cell<usize> = const { Cell::new(0) };
    static EPOCH: Instant = Instant::now();
}

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
    let slot = pill_slot(free_slot(taskbar, &occupied, 300 * dpi / 96, 160 * dpi / 96, 6 * dpi / 96)?, dpi);
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
    let controls = INITCOMMONCONTROLSEX { dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32, dwICC: ICC_WIN95_CLASSES };
    InitCommonControlsEx(&controls);
    let timer = SetTimer(null_mut(), 0, 250, None);
    if timer == 0 { UnregisterClassW(CLASS, instance); return; }
    if let Err(error) = thread::Builder::new().name("nova-taskbar-layout".into()).spawn(|| unsafe { layout_worker() }) {
        eprintln!("[taskbar-status] cannot start layout worker: {error}");
        KillTimer(null_mut(), timer);
        UnregisterClassW(CLASS, instance);
        return;
    }
    THREAD.store(windows_sys::Win32::System::Threading::GetCurrentThreadId(), Ordering::Release);
    let mut shown = None;
    let mut message = MSG::default();
    while GetMessageW(&mut message, null_mut(), 0, 0) > 0 {
        if message.hwnd.is_null() && (message.message == REFRESH || (message.message == WM_TIMER && message.wParam == timer)) {
            refresh(&mut shown);
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

unsafe fn refresh(shown: &mut Option<Status>) {
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
        *shown = None;
        attach_tooltip(hwnd);
    }
    if shown.as_ref() != Some(&summary) {
        let wide: Vec<u16> = summary.title().chars().take(500).collect::<String>().encode_utf16().chain(Some(0)).collect();
        if SetWindowTextW(hwnd, wide.as_ptr()) == 0 { ShowWindow(hwnd, SW_HIDE); return; }
        // Only live states breathe; settled states cost no repaints.
        if matches!(summary.kind, Kind::Running | Kind::Waiting) { SetTimer(hwnd, ANIMATION, 40, None); } else { KillTimer(hwnd, ANIMATION); }
        CURRENT.with(|current| *current.borrow_mut() = Some(summary.clone()));
        *shown = Some(summary);
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

/// Keep the chip centered and compact even on a tall taskbar; never enlarge the verified gap.
fn pill_slot(mut slot: Rect, dpi: i32) -> Rect {
    let height = (28 * dpi / 96).min(slot.bottom - slot.top);
    slot.top += (slot.bottom - slot.top - height) / 2;
    slot.bottom = slot.top + height;
    slot
}

#[derive(Clone, Copy)]
struct Palette {
    background: u32,
    border: u32,
    text: u32,
    muted: u32,
}

const fn rgb(r: u32, g: u32, b: u32) -> u32 { r | (g << 8) | (b << 16) }

/// Dynamic-Island look: the same near-black pill on light and dark taskbars.
fn palette(hover: bool) -> Palette {
    Palette {
        background: if hover { rgb(40, 40, 44) } else { rgb(16, 16, 18) },
        border: rgb(58, 58, 64), text: rgb(245, 245, 247), muted: rgb(152, 152, 160),
    }
}

/// State colour of the leading indicator (Apple system palette).
fn signal(kind: Kind) -> u32 {
    match kind {
        Kind::Running => rgb(10, 132, 255),
        Kind::Waiting => rgb(255, 159, 10),
        Kind::Done => rgb(48, 209, 88),
        Kind::Error => rgb(255, 69, 58),
        Kind::Idle => rgb(142, 142, 147),
    }
}

/// GDI has no alpha: blend by hand.
fn mix(a: u32, b: u32, t: f64) -> u32 {
    let channel = |shift: u32| {
        let (x, y) = (((a >> shift) & 255) as f64, ((b >> shift) & 255) as f64);
        ((x + (y - x) * t).round().clamp(0.0, 255.0) as u32) << shift
    };
    channel(0) | channel(8) | channel(16)
}

unsafe fn dot(dc: HDC, x: i32, y: i32, r: i32, color: u32) {
    let brush = CreateSolidBrush(color);
    let old_brush = SelectObject(dc, brush);
    let old_pen = SelectObject(dc, GetStockObject(NULL_PEN));
    Ellipse(dc, x - r, y - r, x + r + 1, y + r + 1);
    SelectObject(dc, old_pen);
    SelectObject(dc, old_brush);
    DeleteObject(brush);
}

unsafe fn stroke(dc: HDC, points: &[POINT], width: i32, color: u32) {
    let pen = CreatePen(PS_SOLID, width, color);
    let old_pen = SelectObject(dc, pen);
    Polyline(dc, points.as_ptr(), points.len() as i32);
    SelectObject(dc, old_pen);
    DeleteObject(pen);
}

unsafe fn paint_chip(hwnd: HWND, dc: HDC, rect: RECT) {
    let dpi = GetDpiForWindow(hwnd).max(96) as i32;
    let px = |n: i32| n * dpi / 96;
    let mut cursor = POINT::default();
    let hover = GetCursorPos(&mut cursor) != 0 && WindowFromPoint(cursor) == hwnd;
    let status = CURRENT.with(|current| current.borrow().clone());
    let kind = status.as_ref().map_or(Kind::Idle, |s| s.kind);
    let mut colors = palette(hover);
    let mut accent = signal(kind);
    let mut contrast = HIGHCONTRASTW { cbSize: size_of::<HIGHCONTRASTW>() as u32, ..Default::default() };
    if SystemParametersInfoW(SPI_GETHIGHCONTRAST, contrast.cbSize, &mut contrast as *mut _ as _, 0) != 0
        && contrast.dwFlags.0 & HCF_HIGHCONTRASTON.0 != 0
    {
        colors = Palette {
            background: GetSysColor(if hover { COLOR_HIGHLIGHT } else { COLOR_BTNFACE }),
            border: GetSysColor(COLOR_BTNTEXT),
            text: GetSysColor(if hover { COLOR_HIGHLIGHTTEXT } else { COLOR_BTNTEXT }),
            muted: GetSysColor(if hover { COLOR_HIGHLIGHTTEXT } else { COLOR_BTNTEXT }),
        };
        accent = colors.text;
    }
    // Buffer the small native child so status changes/animation never flash a grey rectangle.
    let buffer = CreateCompatibleDC(dc);
    let bitmap = CreateCompatibleBitmap(dc, rect.right, rect.bottom);
    let buffered = !buffer.is_null() && !bitmap.is_null();
    let target = if buffered { buffer } else { dc };
    let old_bitmap = if buffered { SelectObject(buffer, bitmap) } else { null_mut() };
    let brush = CreateSolidBrush(colors.background);
    let pen = CreatePen(PS_SOLID, px(1).max(1), colors.border);
    FillRect(target, &rect, brush);
    let old_brush = SelectObject(target, brush);
    let old_pen = SelectObject(target, pen);
    RoundRect(target, 0, 0, rect.right, rect.bottom, rect.bottom, rect.bottom);
    SelectObject(target, old_pen);
    SelectObject(target, old_brush);
    DeleteObject(pen);
    DeleteObject(brush);

    // Leading indicator: breathing while live, a glyph once settled.
    let (cx, cy) = (px(17), rect.bottom / 2);
    match kind {
        Kind::Running | Kind::Waiting => {
            let phase = (EPOCH.with(|e| e.elapsed().as_secs_f64()) * std::f64::consts::TAU / 1.4).sin() * 0.5 + 0.5;
            dot(target, cx, cy, px(4) + (phase * px(3) as f64) as i32, mix(colors.background, accent, 0.5 - 0.35 * phase));
            dot(target, cx, cy, px(3), accent);
        }
        Kind::Done | Kind::Error => {
            dot(target, cx, cy, px(7), accent);
            let width = px(2).max(1);
            if kind == Kind::Done {
                let check = [POINT { x: cx - px(3), y: cy }, POINT { x: cx - px(1), y: cy + px(2) }, POINT { x: cx + px(3), y: cy - px(2) }];
                stroke(target, &check, width, colors.background);
            } else {
                stroke(target, &[POINT { x: cx, y: cy - px(4) }, POINT { x: cx, y: cy + px(1) }], width, colors.background);
                dot(target, cx, cy + px(3), px(1), colors.background);
            }
        }
        Kind::Idle => dot(target, cx, cy, px(3), accent),
    }

    SetBkMode(target, TRANSPARENT as i32);
    let font = CreateFontW(-px(12), 0, 0, 0, FW_NORMAL as i32, 0, 0, 0, DEFAULT_CHARSET as u32, 0, 0, CLEARTYPE_QUALITY as u32, 0, w!("Segoe UI"));
    let small = CreateFontW(-px(11), 0, 0, 0, FW_NORMAL as i32, 0, 0, 0, DEFAULT_CHARSET as u32, 0, 0, CLEARTYPE_QUALITY as u32, 0, w!("Segoe UI"));
    let old_font = SelectObject(target, GetStockObject(DEFAULT_GUI_FONT));
    let (text, trailing) = status.map_or_else(Default::default, |s| (s.text, s.trailing));
    let left = px(32);
    let mut right = rect.right - px(12);
    if !trailing.is_empty() {
        // Trailing info (elapsed / finished-ago) is right-aligned and never truncated.
        if !small.is_null() { SelectObject(target, small); }
        SetTextColor(target, colors.muted);
        let wide: Vec<u16> = trailing.encode_utf16().collect();
        let mut size = SIZE::default();
        GetTextExtentPoint32W(target, wide.as_ptr(), wide.len() as i32, &mut size);
        let mut area = RECT { left, right, ..rect };
        DrawTextW(target, wide.as_ptr(), wide.len() as i32, &mut area, DT_SINGLELINE | DT_VCENTER | DT_RIGHT | DT_NOPREFIX);
        right -= size.cx + px(8);
    }
    SelectObject(target, if font.is_null() { GetStockObject(DEFAULT_GUI_FONT) } else { font });
    SetTextColor(target, colors.text);
    let wide: Vec<u16> = text.encode_utf16().collect();
    let mut area = RECT { left, right: right.max(left), ..rect };
    DrawTextW(target, wide.as_ptr(), wide.len() as i32, &mut area, DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX);
    SelectObject(target, old_font);
    if !font.is_null() { DeleteObject(font); }
    if !small.is_null() { DeleteObject(small); }
    if buffered {
        let mut client = RECT::default();
        GetClientRect(hwnd, &mut client);
        BitBlt(dc, 0, 0, client.right, client.bottom, buffer, 0, 0, SRCCOPY);
        SelectObject(buffer, old_bitmap);
    }
    if !bitmap.is_null() { DeleteObject(bitmap); }
    if !buffer.is_null() { DeleteDC(buffer); }
}

/// Hover detail (thread title + conclusion excerpt), filled lazily from CURRENT.
unsafe fn attach_tooltip(hwnd: HWND) {
    let tip = CreateWindowExW(WS_EX_TOPMOST | WS_EX_NOACTIVATE, TOOLTIPS_CLASSW, null(), WS_POPUP | TTS_NOPREFIX | TTS_ALWAYSTIP,
        CW_USEDEFAULT, CW_USEDEFAULT, CW_USEDEFAULT, CW_USEDEFAULT, null_mut(), null_mut(), GetModuleHandleW(null()), null());
    if tip.is_null() { return; }
    let info = TTTOOLINFOW {
        cbSize: size_of::<TTTOOLINFOW>() as u32, uFlags: TTF_IDISHWND | TTF_SUBCLASS,
        hwnd, uId: hwnd as usize, lpszText: usize::MAX as *mut u16, // LPSTR_TEXTCALLBACKW
        ..Default::default()
    };
    SendMessageW(tip, TTM_ADDTOOLW, 0, &info as *const _ as LPARAM);
    SendMessageW(tip, TTM_SETMAXTIPWIDTH, 0, (360 * GetDpiForWindow(hwnd).max(96) / 96) as LPARAM);
    SendMessageW(tip, TTM_SETDELAYTIME, TTDT_AUTOPOP as WPARAM, 30_000);
    TOOLTIP.with(|t| t.set(tip as usize));
}

unsafe extern "system" fn window_proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match message {
        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
        WM_LBUTTONUP => {
            // Like a notification click: bring Nova forward on the thread the island is about.
            if let Some(app) = APP.get() {
                crate::sys_notify::focus_main_window(app);
                if let Some(thread) = CURRENT.with(|c| c.borrow().as_ref().and_then(|s| s.thread.clone())) {
                    use tauri::Emitter;
                    let _ = app.emit(crate::acp::EV_NOTIFY_OPEN, serde_json::json!({ "threadId": thread }));
                }
            }
            0
        }
        WM_TIMER if wparam == ANIMATION => {
            InvalidateRect(hwnd, null(), 0);
            0
        }
        WM_NOTIFY if (*(lparam as *const NMHDR)).code == TTN_GETDISPINFOW => {
            let info = &mut *(lparam as *mut NMTTDISPINFOW);
            let detail = CURRENT.with(|c| c.borrow().as_ref().map(|s| s.detail.clone()).unwrap_or_default());
            TIP.with(|tip| {
                let mut tip = tip.borrow_mut();
                *tip = detail.encode_utf16().chain(Some(0)).collect();
                info.lpszText = tip.as_mut_ptr();
            });
            0
        }
        WM_SIZE => {
            let mut rect = RECT::default();
            GetClientRect(hwnd, &mut rect);
            let region = CreateRoundRectRgn(0, 0, rect.right + 1, rect.bottom + 1, rect.bottom, rect.bottom);
            // Windows owns the region only after a successful SetWindowRgn.
            if !region.is_null() && SetWindowRgn(hwnd, region, 1) == 0 { DeleteObject(region); }
            0
        }
        WM_MOUSEMOVE => {
            let mut tracking = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
            TrackMouseEvent(&mut tracking);
            InvalidateRect(hwnd, null(), 0);
            0
        }
        WM_MOUSELEAVE | WM_THEMECHANGED | WM_SYSCOLORCHANGE | WM_SETTINGCHANGE => {
            InvalidateRect(hwnd, null(), 0);
            0
        }
        WM_ERASEBKGND => 1,
        WM_PAINT => {
            let mut paint = PAINTSTRUCT::default();
            let dc = BeginPaint(hwnd, &mut paint);
            if !dc.is_null() {
                let mut rect = RECT::default();
                GetClientRect(hwnd, &mut rect);
                paint_chip(hwnd, dc, rect);
            }
            EndPaint(hwnd, &paint);
            0
        }
        WM_DESTROY => {
            // The tooltip is an unowned popup; it does not die with us.
            let tip = TOOLTIP.with(|t| t.replace(0)) as HWND;
            if !tip.is_null() { DestroyWindow(tip); }
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
    fn chip_stays_inside_the_verified_gap_at_every_scale() {
        for dpi in [96, 120, 144, 192] {
            for height in [20, 40, 80] {
                let slot = Rect { left: -1200, top: -100, right: -800, bottom: -100 + height * dpi / 96 };
                let chip = pill_slot(slot, dpi);
                assert_eq!((chip.left, chip.right), (slot.left, slot.right));
                assert!(chip.top >= slot.top && chip.bottom <= slot.bottom);
                assert!(chip.bottom > chip.top && chip.bottom - chip.top <= 28 * dpi / 96);
                assert!(((chip.top - slot.top) - (slot.bottom - chip.bottom)).abs() <= 1);
            }
        }
    }

    #[test]
    fn chip_text_meets_normal_text_contrast_with_and_without_hover() {
        fn luminance(color: u32) -> f64 {
            let channel = |shift| {
                let value = ((color >> shift) & 255u32) as f64 / 255.0;
                if value <= 0.04045 { value / 12.92 } else { ((value + 0.055) / 1.055).powf(2.4) }
            };
            0.2126 * channel(0) + 0.7152 * channel(8) + 0.0722 * channel(16)
        }
        for hover in [false, true] {
            let colors = palette(hover);
            for text in [colors.text, colors.muted] {
                let a = luminance(text);
                let b = luminance(colors.background);
                assert!((a.max(b) + 0.05) / (a.min(b) + 0.05) >= 4.5);
            }
        }
        assert_eq!(mix(rgb(0, 0, 0), rgb(200, 100, 50), 0.5), rgb(100, 50, 25));
    }

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
