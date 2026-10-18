//! A real Explorer child, never a floating window or a taskbar layout modification.
//! UIA lives on a separate MTA: querying our own HWND on the message thread deadlocks.

use super::{drop_target, free_slot, snapshot, Kind, Rect, Status};
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
        System::{LibraryLoader::GetModuleHandleW, SystemInformation::{GetVersionExW, OSVERSIONINFOW}},
        UI::{
            Controls::{InitCommonControlsEx, ICC_WIN95_CLASSES, INITCOMMONCONTROLSEX},
            HiDpi::*, Input::KeyboardAndMouse::{TrackMouseEvent, TRACKMOUSEEVENT, TME_LEAVE}, WindowsAndMessaging::*,
        },
    },
};

const CLASS: *const u16 = w!("Nova.TaskbarStatus");
const MAX_AGE: Duration = Duration::from_millis(1500);
/// Logical width of the icon-only chip: the leading butterfly slot plus a symmetric cap.
const ICON_ONLY: i32 = 34;
static APP: OnceLock<tauri::AppHandle> = OnceLock::new();
static WINDOW: AtomicUsize = AtomicUsize::new(0);
static THREAD: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
const REFRESH: u32 = WM_APP + 1;
const WM_MOUSELEAVE: u32 = 0x02A3; // Win32 Controls message paired with TME_LEAVE.
static LAYOUT: Mutex<Option<Arc<Layout>>> = Mutex::new(None);
const ANIMATION: usize = 1;
const GLOW: usize = 2;

// Everything below is touched only by the status message thread (refresh + window_proc).
thread_local! {
    static CURRENT: RefCell<Option<Status>> = const { RefCell::new(None) };
    static EPOCH: Instant = Instant::now();
    static ANIMATING: Cell<bool> = const { Cell::new(false) };
    /// This press found the bubble open, so its click only closes it.
    /// The button-up that ends a double click must not start a bubble.
    static DOUBLE: Cell<bool> = const { Cell::new(false) };
    /// The first click can mark its result read and advance CURRENT before the double click.
    static CLICKED: RefCell<Option<String>> = const { RefCell::new(None) };
    /// Taskbar theme the chip was last painted for.
    static LIGHT: Cell<bool> = const { Cell::new(false) };
    /// A droppable drag is over the chip: paint the flowing rainbow border.
    static GLOWING: Cell<bool> = const { Cell::new(false) };
}

/// Drop-target Enter/Leave/Drop run on this same STA thread (OLE was initialized here).
pub(super) unsafe fn glow(hwnd: HWND, on: bool) {
    GLOWING.set(on);
    let mut motion = 1i32;
    SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, &mut motion as *mut _ as _, 0);
    // Reduced motion keeps a still rainbow border.
    if on && motion != 0 { SetTimer(hwnd, GLOW, 16, None); } else { KillTimer(hwnd, GLOW); }
    InvalidateRect(hwnd, null(), 0);
}

/// Saturated-but-soft HSV hue (s=0.8, v=1), `h` in turns.
fn rainbow(h: f64) -> u32 {
    let channel = |n: f64| {
        let k = (n + h.rem_euclid(1.0) * 6.0) % 6.0;
        ((1.0 - 0.8 * k.min(4.0 - k).clamp(0.0, 1.0)) * 255.0).round() as u32
    };
    rgb(channel(5.0), channel(3.0), channel(1.0))
}

/// Point at `s` (0..1) along a stadium's centerline inset by `inset`, clockwise from the top-left.
fn stadium_point(width: i32, height: i32, inset: f64, s: f64) -> POINT {
    use std::f64::consts::PI;
    let r = height as f64 / 2.0;
    let radius = (r - inset).max(0.0);
    let straight = (width as f64 - 2.0 * r).max(0.0);
    let arc = PI * radius;
    let d = s.rem_euclid(1.0) * (2.0 * straight + 2.0 * arc);
    let angle = |d: f64, start: f64| start + d / radius.max(1e-6);
    let (x, y) = if d < straight {
        (r + d, inset)
    } else if d < straight + arc {
        let a = angle(d - straight, -PI / 2.0);
        (r + straight + radius * a.cos(), r + radius * a.sin())
    } else if d < 2.0 * straight + arc {
        (r + straight - (d - straight - arc), height as f64 - inset)
    } else {
        let a = angle(d - 2.0 * straight - arc, PI / 2.0);
        (r + radius * a.cos(), r + radius * a.sin())
    };
    POINT { x: x.round() as i32, y: y.round() as i32 }
}

/// Flowing rainbow border: hues travel clockwise around the pill, with a soft inner halo.
unsafe fn glow_border(dc: HDC, rect: RECT, px: impl Fn(i32) -> i32, background: u32, high_contrast: bool) {
    const SEGMENTS: usize = 120;
    let t = EPOCH.with(|e| e.elapsed().as_secs_f64()) * 0.6;
    for (width, halo) in [(px(3).max(2), 0.6), (px(1).max(1), 0.0)] {
        let inset = width as f64 / 2.0;
        for i in 0..SEGMENTS {
            let s = i as f64 / SEGMENTS as f64;
            let color = if high_contrast { GetSysColor(COLOR_HIGHLIGHT) } else { mix(rainbow(s - t), background, halo) };
            let ends = [s, s + 1.0 / SEGMENTS as f64].map(|s| stadium_point(rect.right, rect.bottom, inset, s));
            stroke(dc, &ends, width, color);
        }
    }
}

/// Windows' own taskbar/Start theme (not the app theme). Missing on old Windows 10: dark.
pub(super) fn light_theme() -> bool {
    winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER)
        .open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize")
        .and_then(|key| key.get_value::<u32, _>("SystemUsesLightTheme"))
        .is_ok_and(|light| light != 0)
}

/// Tauri window calls round-trip through the main thread; never block Explorer's child on them.
pub(super) fn on_app(work: impl FnOnce(&tauri::AppHandle) + Send + 'static) -> bool {
    let Some(app) = APP.get() else { return false; };
    thread::Builder::new().name("nova-taskbar-app".into()).spawn(move || work(app))
        .map_err(|error| eprintln!("[taskbar-status] cannot dispatch app work: {error}"))
        .is_ok()
}

/// Chip rectangle and its monitor's work area, in physical pixels.
pub(super) unsafe fn anchor(hwnd: HWND) -> Option<super::bubble::Anchor> {
    let previous = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    if previous.is_null() { return None; }
    let result = (|| {
        let chip = window_rect(hwnd)?;
        let mut monitor = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        if GetMonitorInfoW(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST), &mut monitor) == 0 { return None; }
        let w = monitor.rcWork;
        let scale = GetDpiForWindow(hwnd).max(96) as f64 / 96.0;
        Some(super::bubble::Anchor { chip, work: Rect { left: w.left, top: w.top, right: w.right, bottom: w.bottom }, scale })
    })();
    SetThreadDpiAwarenessContext(previous);
    result
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
        let layout = APP.get().and_then(|app| snapshot(app, None)).and_then(|_| query_layout()).map(Arc::new);
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
    let (desired, gap) = (300 * dpi / 96, 6 * dpi / 96);
    // Centered taskbars shrink the gaps as apps open; degrade to a shorter label, then the
    // icon-only chip (butterfly + padding), instead of hiding Butterfly.
    let slot = [160, 96, ICON_ONLY].into_iter().find_map(|min| free_slot(taskbar, &occupied, desired, min * dpi / 96, gap));
    let slot = pill_slot(slot?, dpi);
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
    // OLE needs an STA with a pumping message loop, not the layout worker's MTA.
    // The guard revokes any remaining target before balancing OleInitialize on this thread.
    let _ole = match drop_target::Ole::initialize() {
        Ok(ole) => ole,
        Err(error) => { eprintln!("[taskbar-status] cannot initialize OLE: {error}"); return; }
    };
    if SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2).is_null() { return; }
    let instance = GetModuleHandleW(null());
    let class = WNDCLASSW { lpfnWndProc: Some(window_proc), hInstance: instance, lpszClassName: CLASS, style: CS_DBLCLKS | CS_HREDRAW | CS_VREDRAW, hCursor: LoadCursorW(null_mut(), IDC_HAND), ..Default::default() };
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
    let summary = APP.get().and_then(|app| snapshot(app, None));
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
        if !hwnd.is_null() { KillTimer(hwnd, ANIMATION); ShowWindow(hwnd, SW_HIDE); }
        ANIMATING.set(false);
        return;
    };
    let parent = layout.parent as HWND;
    if !hwnd.is_null() && (IsWindow(hwnd) == 0 || GetParent(hwnd) != parent) {
        drop_target::revoke();
        WINDOW.store(0, Ordering::Release);
        if IsWindow(hwnd) != 0 { DestroyWindow(hwnd); }
        hwnd = null_mut();
    }
    if hwnd.is_null() {
        // Match the parent while creating the cross-process child. Never SetParent:
        // that can reset process DPI awareness. Restore physical coordinates after.
        let previous = SetThreadDpiAwarenessContext(GetWindowDpiAwarenessContext(parent));
        if previous.is_null() { return; }
        // Windows 11's XAML taskbar is composed without a redirection surface, so a plain GDI
        // child never reaches the screen. A layered child (Windows 8+, declared in the app
        // manifest) gets its own surface; fall back to a plain child if the host refuses it.
        // Windows 10's GDI taskbar never shows a layered child: keep it on the plain child.
        let create = |ex| CreateWindowExW(ex | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW, CLASS, w!("Nova Butterfly"), WS_CHILD | WS_CLIPSIBLINGS, 0, 0, 0, 0, parent, null_mut(), GetModuleHandleW(null()), null());
        let mut os = OSVERSIONINFOW { dwOSVersionInfoSize: size_of::<OSVERSIONINFOW>() as u32, ..Default::default() };
        // The manifest's Windows 10 declaration makes this report the real build (11 = 22000+).
        if GetVersionExW(&mut os) != 0 && os.dwBuildNumber >= 22000 { hwnd = create(WS_EX_LAYERED); }
        if !hwnd.is_null() && SetLayeredWindowAttributes(hwnd, 0, 255, LWA_ALPHA) == 0 {
            DestroyWindow(hwnd);
            hwnd = null_mut();
        }
        if hwnd.is_null() { hwnd = create(0); }
        SetThreadDpiAwarenessContext(previous);
        if hwnd.is_null() { return; }
        WINDOW.store(hwnd as usize, Ordering::Release);
        if let Err(error) = drop_target::register(UiaHwnd(hwnd)) {
            eprintln!("[taskbar-status] cannot register drop target: {error}");
        }
        ANIMATING.set(false);
        GLOWING.set(false);
        *shown = None;
        // Warm the bubble webview now so clicks open at once.
        on_app(|app| { super::bubble::window(app); });
    }
    // Honour Windows' animation preference, including changes while a task is running.
    let mut motion = 1i32;
    SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, &mut motion as *mut _ as _, 0);
    let animate = motion != 0 && matches!(summary.kind, Kind::Running | Kind::Waiting);
    if ANIMATING.replace(animate) != animate {
        if animate { SetTimer(hwnd, ANIMATION, 16, None); } else { KillTimer(hwnd, ANIMATION); }
        InvalidateRect(hwnd, null(), 0);
    }
    if shown.as_ref() != Some(&summary) {
        let wide: Vec<u16> = summary.title().chars().take(500).collect::<String>().encode_utf16().chain(Some(0)).collect();
        if SetWindowTextW(hwnd, wide.as_ptr()) == 0 { ShowWindow(hwnd, SW_HIDE); return; }
        CURRENT.with(|current| *current.borrow_mut() = Some(summary.clone()));
        *shown = Some(summary);
        InvalidateRect(hwnd, null(), 0);
    }
    // A child of the taskbar never sees the top-level WM_SETTINGCHANGE broadcast, so follow a
    // light/dark switch on the refresh tick (one small registry read).
    let light = light_theme();
    if LIGHT.with(|l| l.replace(light)) != light { InvalidateRect(hwnd, null(), 0); }
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
        let placed = mapped && SetWindowPos(hwnd, HWND_TOP, a.x, a.y, b.x - a.x, b.y - a.y, SWP_NOACTIVATE | SWP_SHOWWINDOW | SWP_NOCOPYBITS) != 0;
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

/// Butterfly pill matching the taskbar theme: a near-black chip on dark, a white card on light.
fn palette(light: bool, hover: bool) -> Palette {
    if light {
        Palette {
            background: if hover { rgb(240, 240, 242) } else { rgb(252, 252, 252) },
            border: rgb(222, 222, 226), text: rgb(26, 26, 26), muted: rgb(96, 96, 96),
        }
    } else {
        Palette {
            background: if hover { rgb(40, 40, 44) } else { rgb(16, 16, 18) },
            border: rgb(58, 58, 64), text: rgb(245, 245, 247), muted: rgb(152, 152, 160),
        }
    }
}

/// State colour of the leading indicator: Apple's dark palette, Windows 11's light semantic colours.
fn signal(light: bool, kind: Kind) -> u32 {
    match (kind, light) {
        (Kind::Running, false) => rgb(10, 132, 255),
        (Kind::Waiting, false) => rgb(255, 159, 10),
        (Kind::Done, false) => rgb(48, 209, 88),
        (Kind::Error, false) => rgb(255, 69, 58),
        (Kind::Idle, false) => rgb(142, 142, 147),
        (Kind::Running, true) => rgb(0, 95, 184),
        (Kind::Waiting, true) => rgb(157, 93, 0),
        (Kind::Done, true) => rgb(15, 123, 15),
        (Kind::Error, true) => rgb(196, 43, 28),
        (Kind::Idle, true) => rgb(112, 112, 112),
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
    ellipse(dc, x - r, y - r, x + r, y + r, color);
}

unsafe fn ellipse(dc: HDC, left: i32, top: i32, right: i32, bottom: i32, color: u32) {
    let brush = CreateSolidBrush(color);
    let old_brush = SelectObject(dc, brush);
    let old_pen = SelectObject(dc, GetStockObject(NULL_PEN));
    Ellipse(dc, left, top, right + 1, bottom + 1);
    SelectObject(dc, old_pen);
    SelectObject(dc, old_brush);
    DeleteObject(brush);
}

// Cubic outlines: a swept forewing and a smaller, scalloped hindwing, not mirrored icon lobes.
const FOREWING: [(f64, f64); 13] = [
    (0.3, 1.0), (1.1, 3.2), (4.7, 6.4), (6.9, 5.8),
    (8.0, 5.5), (6.7, 2.2), (6.2, 0.9),
    (5.0, -0.4), (2.1, -0.5), (0.35, 0.1),
    (0.15, 0.35), (0.15, 0.75), (0.3, 1.0),
];
const HINDWING: [(f64, f64); 16] = [
    (0.25, 0.65), (2.7, 1.1), (5.8, 1.0), (5.7, -1.6),
    (5.6, -2.1), (5.2, -2.5), (4.9, -2.5),
    (4.8, -3.1), (4.3, -3.6), (3.8, -3.5),
    (3.3, -4.4), (2.2, -4.4), (1.7, -3.7),
    (0.9, -2.9), (0.35, -1.0), (0.25, 0.65),
];

#[derive(Clone, Copy)]
struct ButterflyPose {
    phase: f64,
    effort: f64,
    yaw: f64,
    bank: f64,
    drift: f64,
    lift: f64,
    scale: f64,
    flying: bool,
}

impl ButterflyPose {
    fn at(t: f64, flying: bool, waiting: bool) -> Self {
        use std::f64::consts::{FRAC_PI_4, TAU};
        if !flying {
            return Self { phase: 0.8, effort: 0.65, yaw: FRAC_PI_4, bank: -0.22, drift: 0.0, lift: 0.0, scale: 1.15, flying: false };
        }
        // A brief glide between bouts; continuous phase modulation avoids a mechanical loop.
        let glide = (((t * 1.43).sin() - 0.72) / 0.28).clamp(0.0, 1.0);
        let effort = 1.0 - 0.9 * glide * glide * (3.0 - 2.0 * glide);
        // Keep one continuous beat clock across Running/Waiting; waiting only tightens the flight path.
        // Readable, slow-motion wing strokes in a 28-DIP viewport, rather than an insect-like buzz.
        let phase = t * TAU * 1.65 + 0.22 * (t * 1.7).sin();
        let wander = if waiting { 0.9 } else { 1.0 };
        let turn = t * 1.45;
        Self {
            phase, effort, flying: true,
            yaw: FRAC_PI_4 + 0.13 * turn.sin(),
            bank: -0.22 + 0.16 * turn.cos() + 0.055 * effort * (phase - 0.8).sin(),
            drift: wander * (1.8 * turn.sin() + 0.30 * (t * 0.62).sin()),
            // Let the subject travel several pixels before the camera catches up; bank into turns.
            lift: wander * 1.2 * (t * 2.1).sin() + 0.42 * effort * (phase - 1.1).sin(),
            scale: 1.10 + 0.045 * (t * 0.83).sin(),
        }
    }

    fn project(self, x: f64, y: f64, z: f64) -> (f64, f64, f64) {
        let (sy, cy) = self.yaw.sin_cos();
        // A higher three-quarter camera reveals both wing surfaces instead of an edge-on sliver.
        let (se, ce) = 0.82f64.sin_cos();
        let depth = (x * sy - y * cy) * ce + z * se;
        let perspective = self.scale * 42.0 / (42.0 - depth);
        let (u, v) = (x * cy + y * sy, (x * sy - y * cy) * se - z * ce);
        let (sb, cb) = self.bank.sin_cos();
        ((u * cb - v * sb) * perspective + self.drift,
         (u * sb + v * cb) * perspective + self.lift, depth)
    }

    fn wing(self, span: f64, y: f64, side: f64) -> (f64, f64, f64) {
        // The tip/trailing edge lag the root; near and far wings are not perfectly synchronous.
        let phase = self.phase - span * 0.028 - side * 0.055;
        let angle = 0.18 + self.effort * (0.94 * phase.sin() + 0.16 * (2.0 * phase).sin()
            + (1.5 - y).max(0.0) * 0.022 * phase.cos());
        self.project(side * span * angle.cos(), y, span * angle.sin())
    }
}

unsafe fn butterfly_path(dc: HDC, points: &[POINT], color: u32) {
    let brush = CreateSolidBrush(color);
    let old = SelectObject(dc, brush);
    BeginPath(dc);
    MoveToEx(dc, points[0].x, points[0].y, null_mut());
    PolyBezierTo(dc, points[1..].as_ptr(), (points.len() - 1) as u32);
    CloseFigure(dc);
    EndPath(dc);
    FillPath(dc);
    SelectObject(dc, old);
    DeleteObject(brush);
}

unsafe fn butterfly_frame(dc: HDC, cx: i32, cy: i32, unit: f64, pose: ButterflyPose, colors: [u32; 3], high_contrast: bool) {
    let [accent, background, ink] = colors;
    let at = |p: (f64, f64, f64)| POINT {
        x: cx + (p.0 * unit).round() as i32, y: cy + (p.1 * unit).round() as i32,
    };
    let rim = if high_contrast { accent } else { mix(accent, rgb(6, 15, 26), 0.78) };
    if pose.flying && !high_contrast {
        // A faint, short shutter smear on moving wings; keep the body and counters tack-sharp.
        let shutter = ButterflyPose { phase: pose.phase - 0.28, ..pose };
        let fade = 1.0 - 0.13 * pose.effort * pose.phase.cos().abs();
        for side in [-1.0, 1.0] {
            for shape in [&HINDWING[..], &FOREWING[..]] {
                let points: Vec<_> = shape.iter().map(|&(x, y)| at(shutter.wing(x, y, side))).collect();
                butterfly_path(dc, &points, mix(accent, background, fade));
            }
        }
    }
    // ponytail: whole-wing depth ordering is enough at this fixed 45° view; an orbiting camera needs a depth-tested mesh.
    let mut sides = [-1.0, 1.0];
    sides.sort_by(|a, b| pose.wing(4.0, 0.5, *a).2.total_cmp(&pose.wing(4.0, 0.5, *b).2));
    let draw_body = || {
        // Tapered abdomen, thorax, head and two fine clubbed antennae, aligned with the flight axis.
        let body = if high_contrast { background } else { rim };
        let line = |points: &[(f64, f64, f64)], width: f64, color| {
            let points: Vec<POINT> = points.iter().map(|&(x, y, z)| at(pose.project(x, y, z))).collect();
            stroke(dc, &points, (unit * width).round().max(1.0) as i32, color);
        };
        line(&[(0.0, -4.1, 0.2), (0.0, -2.3, 0.35), (0.0, 1.8, 0.35)], 0.8, body);
        for (y, radius) in [(1.4, 0.63), (2.6, 0.48)] {
            let p = at(pose.project(0.0, y, 0.4));
            dot(dc, p.x, p.y, (unit * radius).round().max(1.0) as i32, body);
        }
        line(&[(-0.15, -2.8, 0.65), (-0.15, 0.3, 0.7), (-0.15, 1.8, 0.8)], 0.22, mix(accent, ink, 0.48));
        for side in [-1.0, 1.0] {
            line(&[(side * 0.15, 2.9, 0.4), (side * 0.50, 3.8, 0.65), (side * 0.85, 4.25, 0.6)], 0.18, if high_contrast { accent } else { mix(accent, ink, 0.32) });
            let tip = at(pose.project(side * 0.85, 4.25, 0.6));
            dot(dc, tip.x, tip.y, (unit * 0.14).round().max(1.0) as i32, accent);
        }
    };
    for (layer, side) in sides.into_iter().enumerate() {
        if layer == 1 { draw_body(); } // The near wing occludes the thorax, not the other way around.
        for (shape, veins) in [
            (&HINDWING[..], [(5.2, -1.6), (4.4, -2.6), (2.8, -3.8)]),
            (&FOREWING[..], [(6.8, 5.45), (6.6, 4.0), (6.0, 2.1)]),
        ] {
            let light = 0.22 + 0.22 * (pose.phase - 0.4).cos().abs() * pose.effort;
            for (inset, shine) in [(1.0, -1.0), (0.90, 0.0), (0.76, light * 0.65), (0.56, light)] {
                let color = if high_contrast { accent } else if shine < 0.0 { rim } else {
                    let blue = mix(accent, rgb(208, 242, 250), shine);
                    mix(blue, rim, if layer == 0 { 0.28 } else { 0.0 })
                };
                let points: Vec<POINT> = shape.iter().map(|&(x, y)|
                    at(pose.wing(x * inset, 0.7 + (y - 0.7) * (0.5 + inset * 0.5), side))).collect();
                butterfly_path(dc, &points, color);
                if high_contrast { break; }
            }
            if !high_contrast {
                for (x, y) in veins {
                    let vein = [(0.4, 0.7), (x * 0.52, y * 0.48 + 0.36), (x * 0.94, y * 0.94)];
                    let points = vein.map(|(x, y)| at(pose.wing(x, y, side)));
                    stroke(dc, &points, (unit * 0.16).round().max(1.0) as i32, mix(accent, rim, 0.58));
                    let spot = at(pose.wing(x * 0.99, y, side));
                    dot(dc, spot.x, spot.y, (unit * 0.16).round().max(1.0) as i32, mix(accent, ink, 0.55));
                }
            }
        }
    }
}

/// A tiny tracked-camera viewport. Supersample only the butterfly, never the ClearType text.
unsafe fn butterfly(dc: HDC, cx: i32, cy: i32, unit: f64, pose: ButterflyPose, colors: [u32; 3], high_contrast: bool) {
    let width = (28.0 * unit).round().max(1.0) as i32;
    let height = (26.0 * unit).round().max(1.0) as i32;
    let buffer = CreateCompatibleDC(dc);
    let bitmap = CreateCompatibleBitmap(dc, width * 4, height * 4);
    if buffer.is_null() || bitmap.is_null() {
        if !bitmap.is_null() { DeleteObject(bitmap); }
        if !buffer.is_null() { DeleteDC(buffer); }
        butterfly_frame(dc, cx, cy, unit, pose, colors, high_contrast);
        return;
    }
    let old_bitmap = SelectObject(buffer, bitmap);
    // The lens overlaps the rounded corner slightly: retain the chip border underneath it.
    StretchBlt(buffer, 0, 0, width * 4, height * 4, dc, cx - width / 2, cy - height / 2, width, height, SRCCOPY);
    butterfly_frame(buffer, width * 2, height * 2, unit * 4.0, pose, colors, high_contrast);
    let old_mode = SetStretchBltMode(dc, HALFTONE);
    let mut origin = POINT::default();
    SetBrushOrgEx(dc, 0, 0, &mut origin);
    StretchBlt(dc, cx - width / 2, cy - height / 2, width, height, buffer, 0, 0, width * 4, height * 4, SRCCOPY);
    SetBrushOrgEx(dc, origin.x, origin.y, null_mut());
    SetStretchBltMode(dc, old_mode);
    SelectObject(buffer, old_bitmap);
    DeleteObject(bitmap);
    DeleteDC(buffer);
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
    let light = LIGHT.with(Cell::get);
    let mut colors = palette(light, hover);
    let mut accent = signal(light, kind);
    let mut contrast = HIGHCONTRASTW { cbSize: size_of::<HIGHCONTRASTW>() as u32, ..Default::default() };
    let high_contrast = SystemParametersInfoW(SPI_GETHIGHCONTRAST, contrast.cbSize, &mut contrast as *mut _ as _, 0) != 0
        && contrast.dwFlags.0 & HCF_HIGHCONTRASTON.0 != 0;
    if high_contrast {
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

    // Keep the camera inside the existing leading slot; counters and hit targets do not move.
    let (cx, cy) = (px(17), rect.bottom / 2);
    match kind {
        Kind::Running | Kind::Waiting | Kind::Idle => {
            let pose = ButterflyPose::at(EPOCH.with(|e| e.elapsed().as_secs_f64()), ANIMATING.get(), kind == Kind::Waiting);
            let unit = (dpi as f64 / 96.0).min((rect.bottom - px(2)).max(1) as f64 / 26.0);
            butterfly(target, cx, cy, unit, pose, [accent, colors.background, colors.text], high_contrast);
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
    }

    SetBkMode(target, TRANSPARENT as i32);
    let font = CreateFontW(-px(12), 0, 0, 0, FW_NORMAL as i32, 0, 0, 0, DEFAULT_CHARSET as u32, 0, 0, CLEARTYPE_QUALITY as u32, 0, w!("Segoe UI"));
    let small = CreateFontW(-px(11), 0, 0, 0, FW_NORMAL as i32, 0, 0, 0, DEFAULT_CHARSET as u32, 0, 0, CLEARTYPE_QUALITY as u32, 0, w!("Segoe UI"));
    let old_font = SelectObject(target, GetStockObject(DEFAULT_GUI_FONT));
    // Icon-only chip: no room for a readable label, so skip text instead of drawing a lone ellipsis.
    let (text, trailing) = status.filter(|_| rect.right >= px(ICON_ONLY + 30)).map_or_else(Default::default, |s| (s.text, s.trailing));
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
    // DrawTextW still dereferences its string for count=0; an empty Vec has no valid buffer.
    if !wide.is_empty() {
        DrawTextW(target, wide.as_ptr(), wide.len() as i32, &mut area, DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX);
    }
    SelectObject(target, old_font);
    if !font.is_null() { DeleteObject(font); }
    if !small.is_null() { DeleteObject(small); }
    if GLOWING.get() { glow_border(target, rect, px, colors.background, high_contrast); }
    if buffered {
        let mut client = RECT::default();
        GetClientRect(hwnd, &mut client);
        BitBlt(dc, 0, 0, client.right, client.bottom, buffer, 0, 0, SRCCOPY);
        SelectObject(buffer, old_bitmap);
    }
    if !bitmap.is_null() { DeleteObject(bitmap); }
    if !buffer.is_null() { DeleteDC(buffer); }
}

unsafe extern "system" fn window_proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match message {
        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
        WM_LBUTTONDOWN => {
            CLICKED.with(|clicked| *clicked.borrow_mut() = CURRENT.with(|c| c.borrow().as_ref().and_then(|s| s.thread.clone())));
            on_app(|app| { super::bubble::window(app); });
            0
        }
        WM_LBUTTONUP => {
            if DOUBLE.replace(false) { return 0; }
            // Only a click opens the panel (no hover preview). A double click still hands over to Nova and closes the panel it just opened.
            if let Some(anchor) = anchor(hwnd) { on_app(move |app| super::bubble::open(app, anchor)); }
            0
        }
        WM_LBUTTONDBLCLK => {
            // Double click: the full Nova window, on the thread the Butterfly is about.
            DOUBLE.set(true);
            let thread = CLICKED.with(|clicked| clicked.borrow_mut().take());
            on_app(move |app| super::bubble::open_thread(app, thread));
            0
        }
        WM_TIMER if wparam == ANIMATION => {
            if IsWindowVisible(hwnd) != 0 {
                let mut lens = RECT::default();
                GetClientRect(hwnd, &mut lens);
                lens.right = (31 * GetDpiForWindow(hwnd).max(96) as i32 / 96).min(lens.right);
                // Commit each frame now; Win11's layered child gets a whole-surface repaint.
                // Plain Win10 children keep the small icon-only update region.
                let layered = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) & WS_EX_LAYERED as isize != 0;
                RedrawWindow(hwnd, if layered { null() } else { &lens }, null_mut(), RDW_INVALIDATE | RDW_UPDATENOW);
            }
            0
        }
        WM_TIMER if wparam == GLOW => {
            RedrawWindow(hwnd, null(), null_mut(), RDW_INVALIDATE | RDW_UPDATENOW);
            0
        }
        WM_SIZE => {
            let mut rect = RECT::default();
            GetClientRect(hwnd, &mut rect);
            let region = CreateRoundRectRgn(0, 0, rect.right + 1, rect.bottom + 1, rect.bottom, rect.bottom);
            // Windows owns the region only after a successful SetWindowRgn.
            if !region.is_null() && SetWindowRgn(hwnd, region, 1) == 0 { DeleteObject(region); }
            // SetWindowRgn can finish its redraw inside this resize. Leave the whole new border
            // dirty so an idle chip cannot retain pixels clipped from the previous width.
            InvalidateRect(hwnd, null(), 0);
            0
        }
        WM_MOUSEMOVE => {
            let mut tracking = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
            TrackMouseEvent(&mut tracking);
            InvalidateRect(hwnd, null(), 0);
            0
        }
        WM_MOUSELEAVE => {
            InvalidateRect(hwnd, null(), 0);
            0
        }
        WM_THEMECHANGED | WM_SYSCOLORCHANGE | WM_SETTINGCHANGE => {
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
            // Also runs when Explorer destroys its child, before the HWND becomes invalid.
            drop_target::revoke();
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
    fn resizing_chip_repaints_the_entire_rounded_border() {
        unsafe {
            let instance = GetModuleHandleW(null());
            let class = w!("Nova.TaskbarStatus.ResizeTest");
            let definition = WNDCLASSW { lpfnWndProc: Some(window_proc), hInstance: instance,
                lpszClassName: class, style: CS_HREDRAW | CS_VREDRAW, ..Default::default() };
            assert_ne!(RegisterClassW(&definition), 0);
            // Keep the native paint path active, off-screen and without taking focus or a taskbar slot.
            let hwnd = CreateWindowExW(WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW, class, w!(""), WS_POPUP | WS_VISIBLE, -32000, -32000, 300, 28,
                null_mut(), null_mut(), instance, null());
            assert!(!hwnd.is_null());
            let region = CreateRectRgn(0, 0, 0, 0);
            assert!(!region.is_null());
            for (width, height) in [(250, 28), (300, 28), (200, 35), (450, 42), (160, 28)] {
                ValidateRect(hwnd, null());
                assert_ne!(SetWindowPos(hwnd, null_mut(), 0, 0, width, height, SWP_NOMOVE | SWP_NOACTIVATE | SWP_NOZORDER), 0);
                assert_ne!(GetWindowRgn(hwnd, region), 0);
                assert_eq!(PtInRegion(region, width - 2, 1), 0, "right corner must follow the new width");
                assert_ne!(PtInRegion(region, width - 2, height / 2), 0, "right cap must remain visible");
                let mut dirty = RECT::default();
                assert_ne!(GetUpdateRect(hwnd, &mut dirty, 0), 0, "resize must repaint even when idle");
                assert_eq!((dirty.left, dirty.top, dirty.right, dirty.bottom), (0, 0, width, height),
                    "resize must repaint both ends, not just exposed pixels or the animated icon");
            }
            DeleteObject(region);
            DestroyWindow(hwnd);
            UnregisterClassW(class, instance);
        }
    }

    #[test]
    fn double_click_keeps_the_result_target_after_the_first_click_marks_it_read() {
        CURRENT.with(|c| *c.borrow_mut() = Some(Status::new(Kind::Done, "Done".into(), String::new(), &[], Some("first".into()))));
        unsafe { window_proc(null_mut(), WM_LBUTTONDOWN, 0, 0); }
        CURRENT.with(|c| *c.borrow_mut() = None); // The chip has advanced after acknowledging the open card.
        assert_eq!(CLICKED.with(|clicked| clicked.borrow().clone()).as_deref(), Some("first"));
        unsafe { window_proc(null_mut(), WM_LBUTTONDBLCLK, 0, 0); }
        assert!(CLICKED.with(|clicked| clicked.borrow().is_none()));
        DOUBLE.set(false);
    }

    #[test]
    fn animation_tick_paints_immediately_and_refreshes_the_whole_layered_surface() {
        thread_local! { static PAINTS: RefCell<Vec<Rect>> = const { RefCell::new(Vec::new()) }; }
        unsafe extern "system" fn observe(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
            if message == WM_PAINT {
                let mut dirty = RECT::default();
                GetUpdateRect(hwnd, &mut dirty, 0);
                PAINTS.with(|paints| paints.borrow_mut().push(Rect {
                    left: dirty.left, top: dirty.top, right: dirty.right, bottom: dirty.bottom,
                }));
            }
            window_proc(hwnd, message, wparam, lparam)
        }
        unsafe {
            let instance = GetModuleHandleW(null());
            let class = w!("Nova.TaskbarStatus.AnimationTest");
            let definition = WNDCLASSW { lpfnWndProc: Some(observe), hInstance: instance,
                lpszClassName: class, ..Default::default() };
            assert_ne!(RegisterClassW(&definition), 0);
            for layered in [false, true] {
                // Exercise both paint paths off-screen, without Explorer or input/focus changes.
                let ex = WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | if layered { WS_EX_LAYERED } else { 0 };
                let hwnd = CreateWindowExW(ex, class, w!(""), WS_POPUP | WS_VISIBLE,
                    -32000, -32000, 300, 28, null_mut(), null_mut(), instance, null());
                assert!(!hwnd.is_null());
                if layered { assert_ne!(SetLayeredWindowAttributes(hwnd, 0, 255, LWA_ALPHA), 0); }
                UpdateWindow(hwnd);
                PAINTS.with(|paints| paints.borrow_mut().clear());
                SendMessageW(hwnd, WM_TIMER, ANIMATION, 0);
                let mut client = RECT::default();
                GetClientRect(hwnd, &mut client);
                let right = if layered { client.right } else { (31 * GetDpiForWindow(hwnd).max(96) as i32 / 96).min(client.right) };
                PAINTS.with(|paints| assert_eq!(paints.borrow().as_slice(), &[Rect {
                    left: 0, top: 0, right, bottom: client.bottom,
                }], "timer must paint before returning (layered={layered})"));
                DestroyWindow(hwnd);
            }
            UnregisterClassW(class, instance);
        }
    }

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
    fn glow_border_is_a_closed_stadium_inside_the_chip_with_rainbow_hues() {
        let (w, h, inset) = (160, 28, 1.0);
        let points: Vec<POINT> = (0..=480).map(|i| stadium_point(w, h, inset, i as f64 / 480.0)).collect();
        assert_eq!((points[0].x, points[0].y), (14, 1));
        assert_eq!((points[480].x, points[480].y), (points[0].x, points[0].y));
        for pair in points.windows(2) {
            assert!((pair[0].x - pair[1].x).abs() <= 2 && (pair[0].y - pair[1].y).abs() <= 2, "gap at {:?}", (pair[0].x, pair[0].y));
        }
        assert!(points.iter().all(|p| (0..=w).contains(&p.x) && (0..=h).contains(&p.y)));
        assert_eq!(rainbow(0.0), rgb(255, 51, 51));
        assert_eq!(rainbow(1.0 / 3.0), rgb(51, 255, 51));
        assert_eq!(rainbow(-1.0 / 3.0), rgb(51, 51, 255));
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
        let contrast = |a: u32, b: u32| {
            let (a, b) = (luminance(a), luminance(b));
            (a.max(b) + 0.05) / (a.min(b) + 0.05)
        };
        for light in [false, true] {
            for hover in [false, true] {
                let colors = palette(light, hover);
                for text in [colors.text, colors.muted] {
                    assert!(contrast(text, colors.background) >= 4.5, "text, light={light} hover={hover}");
                }
                // The butterfly / state glyph is a graphic: 3:1 against its own theme's chip.
                for kind in [Kind::Idle, Kind::Running, Kind::Waiting, Kind::Done, Kind::Error] {
                    assert!(contrast(signal(light, kind), colors.background) >= 3.0, "{kind:?}, light={light} hover={hover}");
                }
            }
        }
        assert_eq!(mix(rgb(0, 0, 0), rgb(200, 100, 50), 0.5), rgb(100, 50, 25));
    }

    #[test]
    fn butterfly_flight_is_continuous_and_stays_inside_the_lens() {
        let mut effort_range = (1.0f64, 0.0f64);
        let mut extent = (0.0f64, 0.0f64);
        let mut drift_range = (f64::INFINITY, f64::NEG_INFINITY);
        let mut lift_range = (f64::INFINITY, f64::NEG_INFINITY);
        for waiting in [false, true] {
            for frame in 0..7200 {
                let t = frame as f64 / 120.0;
                let pose = ButterflyPose::at(t, true, waiting);
                let next = ButterflyPose::at(t + 1.0 / 240.0, true, waiting);
                let switched = ButterflyPose::at(t, true, !waiting);
                assert_eq!(pose.phase, switched.phase, "status switch restarted the beat clock");
                assert!((pose.drift - switched.drift).hypot(pose.lift - switched.lift) < 0.5);
                effort_range = (effort_range.0.min(pose.effort), effort_range.1.max(pose.effort));
                drift_range = (drift_range.0.min(pose.drift), drift_range.1.max(pose.drift));
                lift_range = (lift_range.0.min(pose.lift), lift_range.1.max(pose.lift));
                let beats_per_second = (next.phase - pose.phase) * 240.0 / std::f64::consts::TAU;
                assert!((1.5..1.8).contains(&beats_per_second), "wing strokes became too fast");
                assert!((pose.yaw.to_degrees() - 45.0).abs() < 8.0);
                assert!(pose.wing(4.0, 0.5, 1.0).2 > pose.wing(4.0, 0.5, -1.0).2);
                for side in [-1.0, 1.0] {
                    for shape in [&FOREWING[..], &HINDWING[..]] {
                        for segment in shape.windows(4).step_by(3) {
                            // GDI draws the projected cubic, not its off-curve control points.
                            let points: Vec<_> = segment.iter().map(|&(x, y)| pose.wing(x, y, side)).collect();
                            let after: Vec<_> = segment.iter().map(|&(x, y)| next.wing(x, y, side)).collect();
                            for sample in 0..=24 {
                                let u = sample as f64 / 24.0;
                                let v = 1.0 - u;
                                let weights = [v * v * v, 3.0 * v * v * u, 3.0 * v * u * u, u * u * u];
                                let curve = |points: &[(f64, f64, f64)]| weights.iter().zip(points)
                                    .fold((0.0, 0.0), |(x, y), (w, p)| (x + w * p.0, y + w * p.1));
                                let (x, y) = curve(&points);
                                let (nx, ny) = curve(&after);
                                assert!(x.is_finite() && y.is_finite(), "non-finite wing at {t}");
                                extent = (extent.0.max(x.abs()), extent.1.max(y.abs()));
                                assert!((nx - x).hypot(ny - y) < 1.6, "discontinuous wing at {t}");
                            }
                        }
                    }
                }
            }
        }
        assert!(extent.0 < 13.5 && extent.1 < 12.5, "wing left the 28×26-DIP lens: maximum extent {extent:?}");
        assert!(drift_range.1 - drift_range.0 > 3.8 && lift_range.1 - lift_range.0 > 2.8, "flight became imperceptible");
        // Even in a glide, show substantial wing area rather than a thin side-on line.
        let spread = ButterflyPose { phase: 0.0, effort: 0.0, ..ButterflyPose::at(0.0, false, false) };
        let top = spread.wing(6.9, 5.8, -1.0).1;
        let bottom = spread.wing(3.8, -3.5, 1.0).1;
        assert!((bottom - top).abs() > 8.0, "camera flattened the wings");
        assert!(effort_range.0 < 0.15 && effort_range.1 > 0.95, "missing glide or power stroke");
        let rest = ButterflyPose::at(0.0, false, false).wing(6.0, 3.0, 1.0);
        assert_eq!(rest, ButterflyPose::at(100.0, false, true).wing(6.0, 3.0, 1.0));
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
