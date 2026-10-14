//! 剑来接手桌面时的可见提示：屏幕边缘呼吸光效 + 剑形鼠标指针（仅 Windows）。
//! 不影响剑来自己的判断：光效层 WDA_EXCLUDEFROMCAPTURE 排除在所有截图之外（不支持时直接不显示），
//! 鼠标穿透、不可激活，并以无标题 WS_EX_TOOLWINDOW 避开 xcap 窗口枚举（前台/遮挡校验看不到它）；
//! 系统截图本就不含鼠标指针，换指针样式不改变任何截图像素。
use std::{
    ptr::null_mut,
    sync::{atomic::{AtomicBool, Ordering}, mpsc, Mutex, OnceLock},
    time::{Duration, Instant},
};
use tauri::{AppHandle, Manager, WebviewWindow};
use windows_sys::Win32::{
    Foundation::HWND,
    Graphics::Gdi::{CreateBitmap, CreateDIBSection, DeleteObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS},
    UI::WindowsAndMessaging::*,
};

/// 会话回合仍在运行时保持（模型看图思考、两次剑来调用之间不断开）；回合结束即收起。
/// 回合还在跑但这么久没再碰桌面，说明已转去做别的事，也收起。
const IDLE_CAP: Duration = Duration::from_secs(180);
/// 外部 MCP 调用方不是 Nova 会话，看不到回合状态，只能按时间保持。
const EXTERNAL_LINGER: Duration = Duration::from_secs(20);
const TICK: Duration = Duration::from_millis(500);
const LABEL: &str = "agent-glow";
static APP: OnceLock<AppHandle> = OnceLock::new();
struct State { holds: u32, generation: u64, last: Option<Instant>, watching: bool, layout: String }
static STATE: Mutex<State> = Mutex::new(State { holds: 0, generation: 0, last: None, watching: false, layout: String::new() });
static CURSOR: AtomicBool = AtomicBool::new(false);
/// 系统不支持把光效排除出截图时只报一次，之后不再建窗口；剑形指针不受影响（指针从不进截图）。
static OVERLAY_OFF: AtomicBool = AtomicBool::new(false);
/// 永久关闭的原因前缀（结构性问题）；其余失败当做一次性的，下次调用再试。
const FATAL: &str = "不再重试：";

pub(crate) fn init(app: &AppHandle) {
    let _ = APP.set(app.clone());
    // ponytail: 进程被强杀时品牌指针会留到下次启动/注销；这里无条件恢复用户的指针方案。
    restore_cursor();
}

fn wake(hold: bool) {
    let mut s = STATE.lock().unwrap();
    if hold { s.holds += 1; } else { s.last = Some(Instant::now()); }
    s.generation += 1;
    if !s.watching { s.watching = true; std::thread::spawn(watch); }
}

/// 剑来的任意一次调用（第一次观察就算）都点亮：模型还在看图时用户已经知道剑来接手了。
/// 后台模式不碰用户的指针与前台，也不点亮。显示在后台线程完成，从不阻塞工具。
pub(crate) fn touch() { wake(false); }

/// 剑来拿到输入闸门时调用；整批输入期间都保持，外部调用方也不会中途熄灭。
pub(crate) fn hold() { wake(true); }

pub(crate) fn release() {
    let mut s = STATE.lock().unwrap();
    s.holds = s.holds.saturating_sub(1);
    s.last = Some(Instant::now());
}

/// 退出时只恢复指针：不等待 UI 线程，窗口随进程销毁。
pub(crate) fn exit() {
    if CURSOR.load(Ordering::Acquire) { restore_cursor(); }
}

fn any_running(app: &AppHandle) -> bool {
    let Some(state) = app.try_state::<crate::AppState>() else { return false };
    let Ok(store) = state.store.lock() else { return false };
    store.threads.iter().any(|t| crate::is_running(&state, t))
}

fn wanted(holds: u32, idle: Duration, running: bool, saw_running: bool) -> bool {
    holds > 0 || idle < if running { IDLE_CAP } else if saw_running { Duration::ZERO } else { EXTERNAL_LINGER }
}

fn watch() {
    let Some(app) = APP.get() else { STATE.lock().unwrap().watching = false; return };
    let (mut shown, mut saw_running) = (0u64, false);
    loop {
        let running = any_running(app);
        saw_running |= running;
        let (generation, mut layout) = {
            let mut s = STATE.lock().unwrap();
            if !wanted(s.holds, s.last.map_or(Duration::ZERO, |t| t.elapsed()), running, saw_running) {
                // 持锁收起：新的 hold 只能排在这次隐藏之后再显示。
                s.watching = false;
                hide(app);
                return;
            }
            (s.generation, std::mem::take(&mut s.layout))
        };
        // 每次新的剑来调用都换好指针、重新置顶并按当前显示器布局铺满。
        if generation != shown {
            shown = generation;
            if !CURSOR.load(Ordering::Acquire) { brand_cursor(); }
            if !OVERLAY_OFF.load(Ordering::Relaxed) {
                if let Err(e) = show(app, &mut layout) {
                    eprintln!("[agent-glow] {e}");
                    if e.starts_with(FATAL) { OVERLAY_OFF.store(true, Ordering::Relaxed); }
                }
            }
        }
        STATE.lock().unwrap().layout = layout;
        std::thread::sleep(TICK);
    }
}

fn show(app: &AppHandle, loaded: &mut String) -> Result<(), String> {
    let mut rects = Vec::new();
    for m in xcap::Monitor::all().map_err(|e| e.to_string())? {
        rects.push((m.x().map_err(|e| e.to_string())?, m.y().map_err(|e| e.to_string())?,
            m.width().map_err(|e| e.to_string())? as i32, m.height().map_err(|e| e.to_string())? as i32));
    }
    let (Some(left), Some(top)) = (rects.iter().map(|r| r.0).min(), rects.iter().map(|r| r.1).min()) else { return Err("没有显示器".into()) };
    let right = rects.iter().map(|r| r.0 + r.2).max().unwrap();
    let bottom = rects.iter().map(|r| r.1 + r.3).max().unwrap();
    // 各屏相对窗口左上角的物理像素，页面按 devicePixelRatio 换算。
    let layout = rects.iter().map(|r| format!("{},{},{},{}", r.0 - left, r.1 - top, r.2, r.3)).collect::<Vec<_>>().join(",");
    let window = window(app, &layout)?;
    if *loaded != layout {
        if !loaded.is_empty() {
            let mut url = window.url().map_err(|e| e.to_string())?;
            url.set_query(Some(&format!("s={layout}")));
            window.navigate(url).map_err(|e| e.to_string())?;
        }
        *loaded = layout;
    }
    let hwnd = window.hwnd().map_err(|e| e.to_string())?.0 as usize;
    let (tx, rx) = mpsc::channel();
    window.run_on_main_thread(move || {
        let result = unsafe {
            let hwnd = hwnd as HWND;
            // tao 重算样式时会丢掉自加的 TOOLWINDOW，所以每次显示前补上并回读。
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, GetWindowLongPtrW(hwnd, GWL_EXSTYLE) | WS_EX_TOOLWINDOW as isize);
            let mut affinity = 0;
            if GetWindowDisplayAffinity(hwnd, &mut affinity) == 0 || affinity != WDA_EXCLUDEFROMCAPTURE {
                Err(format!("{FATAL}系统不支持把光效排除出截图（需 Windows 10 2004+），不显示"))
            } else if GetWindowLongPtrW(hwnd, GWL_EXSTYLE) & (WS_EX_TOOLWINDOW | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE) as isize
                != (WS_EX_TOOLWINDOW | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE) as isize {
                Err(format!("{FATAL}光效层样式未就绪，不显示"))
            } else {
                // 跨 DPI 移动会触发 WM_DPICHANGED 把窗口改成建议尺寸；第二次落在同一 DPI，尺寸准确。
                for _ in 0..2 {
                    SetWindowPos(hwnd, HWND_TOPMOST, left, top, right - left, bottom - top, SWP_NOACTIVATE | SWP_SHOWWINDOW);
                }
                Ok(())
            }
        };
        let _ = tx.send(result);
    }).map_err(|e| e.to_string())?;
    rx.recv_timeout(Duration::from_secs(2)).map_err(|_| "光效层显示超时".to_string())?
}

fn window(app: &AppHandle, layout: &str) -> Result<WebviewWindow, String> {
    if let Some(window) = app.get_webview_window(LABEL) { return Ok(window); }
    let url = tauri::WebviewUrl::App(format!("agent-glow.html?s={layout}").into());
    // 空标题：xcap 只跳过无标题的 TOOLWINDOW。
    let window = tauri::WebviewWindowBuilder::new(app, LABEL, url)
        .title("").decorations(false).resizable(false).shadow(false)
        .transparent(true).background_color(tauri::window::Color(0, 0, 0, 0))
        .always_on_top(true).skip_taskbar(true).visible(false).focused(false).focusable(false)
        .content_protected(true)
        .build().map_err(|e| e.to_string())?;
    window.set_ignore_cursor_events(true).map_err(|e| e.to_string())?;
    Ok(window)
}

fn hide(app: &AppHandle) {
    restore_cursor();
    let Some(window) = app.get_webview_window(LABEL) else { return };
    let Ok(hwnd) = window.hwnd() else { return };
    let hwnd = hwnd.0 as usize;
    let _ = window.run_on_main_thread(move || unsafe { ShowWindow(hwnd as HWND, SW_HIDE); });
}

/// 与剑来截图里的落点标记同款的剑，只是光晕更浓：真实指针不会进入任何截图，浓光晕只帮用户找指针。
fn brand_cursor() {
    use crate::jianlai::{pointer_pixels, CURSOR_GLOW, POINTER_TIP};
    unsafe {
        let size = GetSystemMetrics(SM_CXCURSOR).clamp(32, 256);
        let mut info: BITMAPINFO = std::mem::zeroed();
        info.bmiHeader = BITMAPINFOHEADER { biSize: size_of::<BITMAPINFOHEADER>() as u32, biWidth: size, biHeight: -size,
            biPlanes: 1, biBitCount: 32, biCompression: BI_RGB, ..std::mem::zeroed() };
        let mut bits = null_mut();
        let color = CreateDIBSection(null_mut(), &info, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
        if color.is_null() || bits.is_null() { return; }
        let pixels = pointer_pixels(size, CURSOR_GLOW);
        std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits as *mut u32, pixels.len());
        // 全 0 的 AND 掩码：像素完全由带 alpha 的彩色位图决定。
        let mask_bits = vec![0u8; (((size + 15) / 16) * 2 * size) as usize];
        let mask = CreateBitmap(size, size, 1, 1, mask_bits.as_ptr() as _);
        let hotspot = (POINTER_TIP * size as f32 / 32.0) as u32;
        let cursor = CreateIconIndirect(&ICONINFO { fIcon: 0, xHotspot: hotspot, yHotspot: hotspot, hbmMask: mask, hbmColor: color });
        DeleteObject(color);
        DeleteObject(mask);
        if cursor.is_null() { return; }
        // SetSystemCursor 接管并销毁传入句柄，每个系统指针各给一份副本。
        for id in [OCR_NORMAL, OCR_IBEAM, OCR_HAND] {
            if SetSystemCursor(CopyIcon(cursor), id) != 0 { CURSOR.store(true, Ordering::Release); }
        }
        DestroyIcon(cursor);
    }
}

fn restore_cursor() {
    // 从注册表重新加载用户自己的指针方案，而不是猜测默认值。
    unsafe { SystemParametersInfoW(SPI_SETCURSORS, 0, null_mut(), 0); }
    CURSOR.store(false, Ordering::Release);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sword_renders_blade_guard_and_an_ink_dot_glow_at_the_tip() {
        for size in [32, 48, 64] {
            let p = crate::jianlai::pointer_pixels(size, crate::jianlai::CURSOR_GLOW);
            let at = |x: f32, y: f32| p[((y * size as f32 / 32.0) as i32 * size + (x * size as f32 / 32.0) as i32) as usize];
            assert_eq!(at(31.0, 0.5), 0, "{size}px 右上角透明");
            assert_eq!(at(0.5, 31.0), 0, "{size}px 左下角透明");
            let blade = at(15.5, 15.5);
            assert_eq!(blade >> 24, 255, "{size}px 对角线上是剑身");
            assert!((blade & 255) > 200, "{size}px 剑身是浅蓝白: {blade:08x}");
            let guard = at(22.06, 18.59);
            assert_eq!(guard >> 24, 255, "{size}px 护手横跨剑身: {guard:08x}");
            // 剑尖周围是天蓝墨点：中心浓、向外淡出，始终半透明以免糊住落点。
            let near = at(7.0, 9.0) >> 24;
            let far = at(4.0, 10.0) >> 24;
            assert!(near > far && far > 0 && near < 255, "{size}px 光晕由浓到淡: {near} -> {far}");
            assert!((at(4.0, 10.0) & 255) > 200, "{size}px 光晕是天蓝色");
        }
    }

    #[test]
    fn the_screenshot_marker_keeps_the_tip_readable() {
        let (cursor, marker) = (crate::jianlai::pointer_pixels(32, crate::jianlai::CURSOR_GLOW),
            crate::jianlai::pointer_pixels(32, crate::jianlai::MARKER_GLOW));
        let alpha = |p: &Vec<u32>, x: usize, y: usize| p[y * 32 + x] >> 24;
        // 剑尖左侧几像素：只有光晕，没有剑身也没有描边。
        assert!(alpha(&marker, 6, 10) * 2 < alpha(&cursor, 6, 10) && alpha(&marker, 6, 10) < 64,
            "反馈图里的光晕明显更淡，不挡内容：{} vs {}", alpha(&marker, 6, 10), alpha(&cursor, 6, 10));
        assert_eq!(alpha(&marker, 16, 16), 255, "剑身本身仍然实心可辨");
    }

    #[test]
    fn glow_follows_the_turn_not_each_call() {
        let s = Duration::from_secs;
        assert!(wanted(1, s(999), false, true), "正在输入");
        assert!(wanted(0, s(60), true, true), "回合仍在跑：调用之间不断开");
        assert!(!wanted(0, s(200), true, true), "回合在跑但久未操作桌面");
        assert!(!wanted(0, s(0), false, true), "回合结束立即收起");
        assert!(wanted(0, s(10), false, false) && !wanted(0, s(30), false, false), "外部调用方按时间");
    }
}
