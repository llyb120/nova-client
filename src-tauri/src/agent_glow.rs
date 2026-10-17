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
/// 回合还在跑但这么久没再碰桌面，说明已转去做别的事（或卡住），也收起。
const IDLE_CAP: Duration = Duration::from_secs(60);
/// 外部 MCP 调用方不是 Nova 会话，看不到回合状态，只能按时间保持。
const EXTERNAL_LINGER: Duration = Duration::from_secs(20);
const TICK: Duration = Duration::from_millis(500);
const LABEL: &str = "agent-glow";
static APP: OnceLock<AppHandle> = OnceLock::new();
struct State { holds: u32, generation: u64, last: Option<Instant>, watching: bool }
static STATE: Mutex<State> = Mutex::new(State { holds: 0, generation: 0, last: None, watching: false });
static CURSOR: AtomicBool = AtomicBool::new(false);
/// 剑谱录制中：朱红慢呼吸 +「剑谱录制中」标签 + 毛笔指针，与剑来的天蓝 + 剑互斥（录制中剑来不接手桌面）。
static RECORDING: AtomicBool = AtomicBool::new(false);
/// 开始录制时的指针位置（屏幕物理像素）：按钮/快捷键触发处，涟漪从这里铺满屏幕。
static ORIGIN: Mutex<Option<(i32, i32)>> = Mutex::new(None);
/// 光效页当前加载的查询串；锁住整个 show，剑来与剑谱的显示互斥，不会各自以为页面已是自己的颜色。
static LOADED: Mutex<String> = Mutex::new(String::new());
/// 系统不支持把光效排除出截图时只报一次，之后不再建窗口；剑形指针不受影响（指针从不进截图）。
static OVERLAY_OFF: AtomicBool = AtomicBool::new(false);
/// 永久关闭的原因前缀（结构性问题）；其余失败当做一次性的，下次调用再试。
const FATAL: &str = "不再重试：";

pub(crate) fn init(app: &AppHandle) {
    let _ = APP.set(app.clone());
    // ponytail: 进程被强杀时品牌指针会留到下次启动/注销；这里无条件恢复用户的指针方案。
    restore_cursor();
}

/// 只看窗口列表/截图不算接手：只给已点亮的光效续期，从不点亮。
pub(crate) fn touch() {
    let mut s = STATE.lock().unwrap();
    if s.watching { s.last = Some(Instant::now()); }
}

/// 剑来拿到输入闸门（真正要动鼠标键盘/切窗）时才点亮；整批输入期间都保持，外部调用方也不会中途熄灭。
/// 显示在后台线程完成，从不阻塞工具。
pub(crate) fn hold() {
    let mut s = STATE.lock().unwrap();
    s.holds += 1;
    s.generation += 1;
    if !s.watching { s.watching = true; std::thread::spawn(watch); }
}

pub(crate) fn release() {
    let mut s = STATE.lock().unwrap();
    s.holds = s.holds.saturating_sub(1);
    s.last = Some(Instant::now());
}

/// 剑谱开始/停止录制。剑来的光效若还在延时保持，watch 看到录制后自行退出、不再收起。
pub(crate) fn recording(on: bool) {
    RECORDING.store(on, Ordering::Release);
    if on {
        // 点按钮开始时指针就停在按钮上；快捷键开始则从指针所在处起涟漪。
        let mut p = windows_sys::Win32::Foundation::POINT { x: 0, y: 0 };
        *ORIGIN.lock().unwrap() = (unsafe { GetCursorPos(&mut p) } != 0).then_some((p.x, p.y));
    }
    let Some(app) = APP.get() else { return };
    let app = app.clone();
    std::thread::spawn(move || {
        // 持 STATE 锁：剑来 watch 的「收起」要么排在前面，要么看到录制后退出，不会把刚亮起的朱红光效收掉。
        let _state = STATE.lock().unwrap();
        if !on { hide(&app); }
        else {
            brand_cursor(true);
            if !OVERLAY_OFF.load(Ordering::Relaxed) {
                if let Err(e) = show(&app) { eprintln!("[agent-glow] {e}"); }
            }
        }
    });
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
        let generation = {
            let mut s = STATE.lock().unwrap();
            if RECORDING.load(Ordering::Acquire) { s.watching = false; return; }
            if !wanted(s.holds, s.last.map_or(Duration::ZERO, |t| t.elapsed()), running, saw_running) {
                // 持锁收起：新的 hold 只能排在这次隐藏之后再显示。
                s.watching = false;
                hide(app);
                return;
            }
            s.generation
        };
        // 每次新的剑来调用都换好指针、重新置顶并按当前显示器布局铺满。
        if generation != shown {
            shown = generation;
            if !CURSOR.load(Ordering::Acquire) { brand_cursor(false); }
            if !OVERLAY_OFF.load(Ordering::Relaxed) {
                if let Err(e) = show(app) {
                    eprintln!("[agent-glow] {e}");
                    if e.starts_with(FATAL) { OVERLAY_OFF.store(true, Ordering::Relaxed); }
                }
            }
        }
        std::thread::sleep(TICK);
    }
}

fn show(app: &AppHandle) -> Result<(), String> {
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
    let mut query = format!("s={layout}{}", if RECORDING.load(Ordering::Acquire) { "&m=rec" } else { "" });
    // 起点每次不同，带上它会让页面重新加载，涟漪也就每次开始录制都播一遍。
    if let Some((x, y)) = ORIGIN.lock().unwrap().take().filter(|_| RECORDING.load(Ordering::Acquire)) {
        query += &format!("&o={},{}", x - left, y - top);
    }
    let mut loaded = LOADED.lock().unwrap();
    let fresh = app.get_webview_window(LABEL).is_none();
    let window = window(app, &query)?;
    // 新建的窗口已带着 query 加载；已有窗口在天蓝 ↔ 朱红、布局变化或新起点时重新加载。
    if fresh { *loaded = query; } else if *loaded != query {
        let mut url = window.url().map_err(|e| e.to_string())?;
        url.set_query(Some(&query));
        window.navigate(url).map_err(|e| e.to_string())?;
        *loaded = query;
    }
    let hwnd = window.hwnd().map_err(|e| e.to_string())?.0 as usize;
    let (tx, rx) = mpsc::channel();
    let shown = window.clone();
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
                // 再经 tao 显示一次：只用 Win32 显示时 WebView2 仍当自己不可见，动画停在首帧（涟漪不播、呼吸不动）。
                // 窗口不可聚焦（NOACTIVATE），不会抢前台。
                let _ = shown.show();
                Ok(())
            }
        };
        let _ = tx.send(result);
    }).map_err(|e| e.to_string())?;
    rx.recv_timeout(Duration::from_secs(2)).map_err(|_| "光效层显示超时".to_string())?
}

fn window(app: &AppHandle, query: &str) -> Result<WebviewWindow, String> {
    if let Some(window) = app.get_webview_window(LABEL) { return Ok(window); }
    let url = tauri::WebviewUrl::App(format!("agent-glow.html?{query}").into());
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
    let hidden = window.clone();
    // tao 与 Win32 都收起：tao 记得自己「已显示」，下一次 show() 才不会被它当成无事可做。
    let _ = window.run_on_main_thread(move || { let _ = hidden.hide(); unsafe { ShowWindow(hwnd as HWND, SW_HIDE); } });
}

/// 剑谱的毛笔：笔尖朝左上（热点），沿对角线依次是墨色笔头、金色笔箍、竹色笔杆、朱红笔尾。
fn brush(x: f32, y: f32) -> (f32, (f32, f32, f32)) {
    use crate::jianlai::{polygon_sd, POINTER_TIP};
    const SCALE: f32 = 0.82;
    let (dx, dy) = (x - POINTER_TIP, y - POINTER_TIP);
    let k = std::f32::consts::FRAC_1_SQRT_2 / SCALE;
    let (s, w) = ((dx + dy) * k, (dx - dy) * k);
    // 竹节：笔杆上每隔一段一道深色。
    let bamboo = if (s - 10.0).rem_euclid(5.0) < 0.6 { (120.0, 72.0, 24.0) } else { (217.0, 160.0, 82.0) };
    [
        (polygon_sd(&[(0.0, 0.0), (2.5, -1.3), (6.0, -2.1), (8.2, -2.1), (8.2, 2.1), (6.0, 2.1), (2.5, 1.3)], s, w), (28.0, 25.0, 23.0)),
        (polygon_sd(&[(8.0, -2.3), (10.2, -2.3), (10.2, 2.3), (8.0, 2.3)], s, w), (234.0, 179.0, 8.0)),
        (polygon_sd(&[(10.0, -1.5), (24.0, -1.5), (24.0, 1.5), (10.0, 1.5)], s, w), bamboo),
        (polygon_sd(&[(23.8, -1.7), (26.2, -1.7), (26.2, 1.7), (23.8, 1.7)], s, w), (220.0, 38.0, 38.0)),
    ].into_iter().min_by(|a, b| a.0.total_cmp(&b.0)).map(|(d, c)| (d * SCALE, c)).unwrap()
}

/// 剑来：与截图里的落点标记同款的剑，只是光晕更浓；剑谱：笔尖带朱红墨点的毛笔。
/// 真实指针不会进入任何截图，浓光晕只帮用户找指针。
fn brush_pixels(size: i32) -> Vec<u32> {
    crate::jianlai::paint_pointer(size, brush, (40.0, 20.0, 12.0), (239.0, 68.0, 68.0, crate::jianlai::CURSOR_GLOW))
}

fn brand_cursor(recording: bool) {
    use crate::jianlai::{pointer_pixels, CURSOR_GLOW, POINTER_TIP};
    unsafe {
        let size = GetSystemMetrics(SM_CXCURSOR).clamp(32, 256);
        let mut info: BITMAPINFO = std::mem::zeroed();
        info.bmiHeader = BITMAPINFOHEADER { biSize: size_of::<BITMAPINFOHEADER>() as u32, biWidth: size, biHeight: -size,
            biPlanes: 1, biBitCount: 32, biCompression: BI_RGB, ..std::mem::zeroed() };
        let mut bits = null_mut();
        let color = CreateDIBSection(null_mut(), &info, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
        if color.is_null() || bits.is_null() { return; }
        let pixels = if recording { brush_pixels(size) } else { pointer_pixels(size, CURSOR_GLOW) };
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
    fn brush_is_ink_tipped_with_a_red_glow_and_unlike_the_sword() {
        let (p, sword) = (brush_pixels(32), crate::jianlai::pointer_pixels(32, crate::jianlai::CURSOR_GLOW));
        let at = |p: &Vec<u32>, x: usize, y: usize| p[y * 32 + x];
        assert_eq!(at(&p, 13, 13) >> 24, 255, "笔头实心");
        assert!(at(&p, 13, 13) & 0xff < 60, "笔头是墨色: {:08x}", at(&p, 13, 13));
        let glow = at(&p, 6, 10);
        assert!(glow >> 24 > 0 && (glow >> 16) & 0xff > 200 && glow & 0xff < 120, "笔尖是朱红墨点: {glow:08x}");
        assert!(at(&sword, 6, 10) & 0xff > 200, "剑是天蓝，两者一眼可分");
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
        assert!(wanted(0, s(30), true, true), "回合仍在跑：调用之间不断开");
        assert!(!wanted(0, s(90), true, true), "回合在跑但久未操作桌面");
        touch();
        assert!(!STATE.lock().unwrap().watching, "只观察不点亮");
        assert!(!wanted(0, s(0), false, true), "回合结束立即收起");
        assert!(wanted(0, s(10), false, false) && !wanted(0, s(30), false, false), "外部调用方按时间");
    }
}
