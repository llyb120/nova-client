//! 后台操作：不碰用户的真实光标、键盘焦点与前台窗口，用户可以同时使用电脑。
//!
//! - macOS：事件用 CGEventPostToPid 直接投给窗口所属进程，不需要前台、不被遮挡；截图取窗口自身像素。
//! - Linux：在 Nova 私有的 Xvfb「座席」里操作，用户的 X 会话完全不受影响。
//! - 其它平台没有无副作用的输入通道，直接拒绝；任何平台都绝不回退到真实输入。
//!
//! 后台标志放在线程局部：一次 `run` 在同一个线程里完成，`foreground()` 据此返回 None，
//! 于是所有「前台未变」校验在后台模式下自然成立，不必逐处传参。

use super::{Action, Result};
use serde_json::{json, Value};
use std::cell::Cell;

thread_local! { static ACTIVE: Cell<bool> = const { Cell::new(false) }; }

/// 当前线程正在执行后台请求。
pub(super) fn active() -> bool {
    ACTIVE.with(Cell::get)
}

pub(super) struct Scope(bool);
impl Drop for Scope {
    fn drop(&mut self) {
        if self.0 {
            ACTIVE.with(|a| a.set(false));
        }
    }
}

/// 进入后台模式；平台不支持时直接报错，调用方不得改走真实输入。
pub(super) fn enter(on: bool) -> Result<Scope> {
    if !on {
        return Ok(Scope(false));
    }
    platform_ready()?;
    ACTIVE.with(|a| a.set(true));
    Ok(Scope(true))
}

/// 后台截图/操作的范围：macOS 必须是窗口，Linux 座席只有桌面视图。
pub(super) fn check_scope(window: bool) -> Result<()> {
    match (cfg!(target_os = "macos"), window) {
        (true, false) => Err("macOS 后台操作需要 windowId（用 windows 查，窗口不必在前台）".into()),
        (false, true) => Err("Linux 后台座席只有桌面视图，不要传 windowId".into()),
        _ => Ok(()),
    }
}

/// 写进 windows 结果的后台说明，告诉模型这个模式下该怎么用。
pub(super) fn info() -> Result<Value> {
    #[cfg(target_os = "linux")]
    {
        let (display, (width, height), _) = linux::seat()?;
        Ok(json!({"platform":"linux","display":display,"size":[width,height],
            "notice":format!("这是 Nova 私有的虚拟桌面，用户看不到。先在你的 shell 里用 DISPLAY={display} 启动要操作的应用（如 DISPLAY={display} firefox &），再 screenshot/act 并带 background:true。座席没有窗口管理器，只有桌面视图，windows 列表为空。")}))
    }
    #[cfg(target_os = "macos")]
    {
        Ok(json!({"platform":"macos","mode":"post-to-pid",
            "notice":"事件直接投递给窗口所属进程，不动真实光标与前台。键盘最稳；点击/滚动个别应用可能不响应，或被应用自行激活（结果会带 foregroundChanged）。"}))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        Err(unsupported())
    }
}

fn platform_ready() -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        linux::seat().map(|_| ())
    }
    #[cfg(target_os = "macos")]
    {
        if !mac::trusted() {
            return Err("后台输入需要「系统设置 → 隐私与安全性 → 辅助功能」授权 Nova，授权后重试".into());
        }
        Ok(())
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        Err(unsupported())
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn unsupported() -> String {
    "当前系统没有不影响前台的输入通道（只有一个输入队列），后台操作仅支持 macOS 和 Linux；本次未执行，不会改用真实输入".into()
}

// ───────────────────────── Linux：私有 Xvfb 座席 ─────────────────────────

#[cfg(target_os = "linux")]
pub(super) use linux::{seat_grab, seat_settings, seat_surface};

#[cfg(target_os = "linux")]
pub(super) fn seat_display() -> Result<String> {
    Ok(linux::seat()?.0)
}

/// Display of a seat that is already running. Does not start Xvfb.
#[cfg(target_os = "linux")]
pub(super) fn seated_display() -> Option<String> {
    linux::seated_display()
}

/// 非 Linux 没有座席；这些入口只为让共用代码在所有平台编译，check_scope 已在到达前拒绝。
#[cfg(not(target_os = "linux"))]
pub(super) fn seat_surface() -> Result<super::Surface> {
    Err("该平台没有后台座席".into())
}
#[cfg(not(target_os = "linux"))]
pub(super) fn seat_grab() -> Result<xcap::image::RgbaImage> {
    Err("该平台没有后台座席".into())
}
#[cfg(not(target_os = "linux"))]
pub(super) fn seat_settings() -> Result<enigo::Settings> {
    Err("该平台没有后台座席".into())
}

#[cfg(target_os = "linux")]
mod linux {
    use super::super::{err, Surface};
    use super::*;
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Command, Stdio};
    use std::sync::atomic::Ordering;
    use std::sync::Mutex;
    use std::time::Duration;
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{ConnectionExt, ImageFormat};

    const SIZE: (u32, u32) = (1440, 900);
    /// 座席在剑来里的虚拟显示器 id 高位；低位是第几次拉起，座席重启后旧快照因 Surface 不同而失效。
    const SEAT_MONITOR: u32 = 0x5ea7_0000;
    // ponytail: 全进程一个座席，所有会话共用；要按会话隔离时改成 owner → Seat 的表。
    // Xvfb 的父线程必须活过调用方的线程池线程：PR_SET_PDEATHSIG 跟的是创建它的线程，不是进程。
    static HOST: std::sync::OnceLock<Mutex<std::sync::mpsc::Sender<std::sync::mpsc::Sender<Result<(String, (u32, u32), u32)>>>>> = std::sync::OnceLock::new();
    static REAP_PID: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);
    static REAP_DISPLAY: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);
    static SEATED: Mutex<Option<String>> = Mutex::new(None);

    /// Name of the live seat, if this process has one. Reading it must not start Xvfb.
    pub(super) fn seated_display() -> Option<String> {
        SEATED.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn set_seated(name: Option<String>) {
        *SEATED.lock().unwrap_or_else(|e| e.into_inner()) = name;
    }

    /// 座席的 (DISPLAY, 尺寸, 第几代)；没有或已退出就重新拉起。
    pub(super) fn seat() -> Result<(String, (u32, u32), u32)> {
        let mut tx = host().lock().unwrap_or_else(|e| e.into_inner()).clone();
        for _ in 0..2 {
            let (reply_tx, reply_rx) = std::sync::mpsc::channel();
            if tx.send(reply_tx).is_err() {
                let mut slot = host().lock().unwrap_or_else(|e| e.into_inner());
                *slot = spawn_host();
                tx = slot.clone();
                continue;
            }
            return reply_rx.recv().unwrap_or_else(|_| Err("座席线程已退出".into()));
        }
        Err("座席线程不可用".into())
    }

    fn host() -> &'static Mutex<std::sync::mpsc::Sender<std::sync::mpsc::Sender<Result<(String, (u32, u32), u32)>>>> {
        HOST.get_or_init(|| Mutex::new(spawn_host()))
    }

    fn spawn_host() -> std::sync::mpsc::Sender<std::sync::mpsc::Sender<Result<(String, (u32, u32), u32)>>> {
        install_reaper();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::Builder::new().name("jianlai-seat".into()).spawn(move || host_loop(rx)).expect("jianlai seat thread");
        tx
    }

    fn host_loop(rx: std::sync::mpsc::Receiver<std::sync::mpsc::Sender<Result<(String, (u32, u32), u32)>>>) {
        let mut live: Option<Live> = None;
        let mut generation = 0u32;
        while let Ok(reply) = rx.recv() {
            if let Some(seat) = live.as_mut() {
                match seat.child.try_wait() {
                    Ok(None) => {
                        let _ = reply.send(Ok((seat.display.clone(), SIZE, seat.generation)));
                        continue;
                    }
                    _ => {
                        set_seated(None);
                        release_display(seat.number);
                        live = None;
                    }
                }
            }
            match start_xvfb() {
                Ok(mut seat) => {
                    generation = generation.wrapping_add(1);
                    seat.generation = generation;
                    let info = (seat.display.clone(), SIZE, generation);
                    set_seated(Some(seat.display.clone()));
                    live = Some(seat);
                    let _ = reply.send(Ok(info));
                }
                Err(error) => {
                    let _ = reply.send(Err(error));
                }
            }
        }
    }

    struct Live {
        child: Child,
        display: String,
        number: u32,
        generation: u32,
    }

    fn start_xvfb() -> Result<Live> {
        let n = (200..300).find(|n| display_available(*n)).ok_or("没有空闲的 X 显示号")?;
        let display = format!(":{n}");
        let mut command = Command::new("Xvfb");
        command
            // -noreset: a screenshot or settle connection must not reset the server and kill apps on this display.
            .args([&display, "-screen", "0", &format!("{}x{}x24", SIZE.0, SIZE.1), "-nolisten", "tcp", "-noreset"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // Parent is the long-lived seat thread, so a finished worker thread does not SIGKILL Xvfb.
        // Process exit still does: that thread dies with the process, and atexit unlinks the lock.
        unsafe {
            command.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                if libc::getppid() == 1 {
                    libc::raise(libc::SIGKILL);
                }
                Ok(())
            });
        }
        let mut child = command.spawn().map_err(|e| format!("无法启动 Xvfb（请先安装 xvfb）：{e}"))?;
        REAP_PID.store(child.id() as i32, Ordering::Release);
        REAP_DISPLAY.store(n as i32, Ordering::Release);
        for _ in 0..80 {
            if x11rb::rust_connection::RustConnection::connect(Some(display.as_str())).is_ok() {
                return Ok(Live { child, display, number: n, generation: 0 });
            }
            if child.try_wait().ok().flatten().is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        let _ = child.kill();
        let _ = child.wait();
        release_display(n);
        Err(format!("Xvfb 在 2 秒内没有接受连接（{display}）"))
    }

    /// True when this display number can be used. A lock whose pid is gone is removed, with its socket.
    pub(super) fn display_available(n: u32) -> bool {
        let lock_path = format!("/tmp/.X{n}-lock");
        let sock_path = format!("/tmp/.X11-unix/X{n}");
        if std::path::Path::new(&lock_path).exists() {
            if lock_pid_alive(&lock_path) {
                return false;
            }
            let _ = std::fs::remove_file(&lock_path);
            let _ = std::fs::remove_file(&sock_path);
            return true;
        }
        if std::path::Path::new(&sock_path).exists() {
            if x11rb::rust_connection::RustConnection::connect(Some(&format!(":{n}"))).is_ok() {
                return false;
            }
            let _ = std::fs::remove_file(&sock_path);
        }
        true
    }

    fn lock_pid_alive(path: &str) -> bool {
        let Ok(text) = std::fs::read_to_string(path) else {
            return true;
        };
        match text.split_whitespace().next().and_then(|s| s.parse::<i32>().ok()) {
            Some(pid) => pid_alive(pid),
            None => false,
        }
    }

    fn pid_alive(pid: i32) -> bool {
        if pid <= 0 {
            return false;
        }
        let alive = unsafe { libc::kill(pid, 0) } == 0;
        alive || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
    }

    fn release_display(n: u32) {
        if REAP_DISPLAY.load(Ordering::Acquire) == n as i32 {
            REAP_PID.store(0, Ordering::Release);
            REAP_DISPLAY.store(-1, Ordering::Release);
        }
        let _ = std::fs::remove_file(format!("/tmp/.X{n}-lock"));
        let _ = std::fs::remove_file(format!("/tmp/.X11-unix/X{n}"));
    }

    fn install_reaper() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| unsafe {
            libc::atexit(reap_seat);
        });
    }

    extern "C" fn reap_seat() {
        unsafe { reap_seat_inner() }
    }

    unsafe fn reap_seat_inner() {
        let pid = REAP_PID.load(Ordering::Acquire);
        let n = REAP_DISPLAY.load(Ordering::Acquire);
        if pid > 0 {
            libc::kill(pid, libc::SIGTERM);
            let mut status = 0;
            for _ in 0..30 {
                if libc::waitpid(pid, &mut status, libc::WNOHANG) != 0 {
                    break;
                }
                libc::usleep(10_000);
            }
            libc::kill(pid, libc::SIGKILL);
            libc::waitpid(pid, &mut status, libc::WNOHANG);
        }
        if n >= 0 {
            let lock = format!("/tmp/.X{n}-lock\0");
            let sock = format!("/tmp/.X11-unix/X{n}\0");
            libc::unlink(lock.as_ptr().cast());
            libc::unlink(sock.as_ptr().cast());
        }
    }

    pub(crate) fn seat_surface() -> Result<Surface> {
        let (_, (width, height), generation) = seat()?;
        Ok(Surface { id: SEAT_MONITOR | generation, pid: None, x: 0, y: 0, width, height })
    }

    /// 输入走座席：Enigo 按实例指定 X 显示，不读也不改进程环境变量。
    pub(crate) fn seat_settings() -> Result<enigo::Settings> {
        Ok(enigo::Settings { x11_display: Some(seat()?.0), ..Default::default() })
    }

    pub(crate) fn seat_grab() -> Result<xcap::image::RgbaImage> {
        let (display, ..) = seat()?;
        let (conn, screen) = x11rb::rust_connection::RustConnection::connect(Some(display.as_str())).map_err(err)?;
        let screen = &conn.setup().roots[screen];
        let (width, height) = (screen.width_in_pixels, screen.height_in_pixels);
        let reply = conn
            .get_image(ImageFormat::Z_PIXMAP, screen.root, 0, 0, width, height, !0).map_err(err)?
            .reply().map_err(err)?;
        let bits = conn.setup().pixmap_formats.iter().find(|f| f.depth == reply.depth).map(|f| f.bits_per_pixel);
        if bits != Some(32) {
            return Err(format!("座席像素格式不受支持：depth={} bpp={bits:?}", reply.depth));
        }
        let mut rgba = reply.data;
        fastpix::bgra_to_rgba(&mut rgba);
        xcap::image::RgbaImage::from_raw(width as u32, height as u32, rgba).ok_or_else(|| "座席截图尺寸异常".into())
    }
}

// ───────────────────── macOS：事件直接投递给目标进程 ─────────────────────

/// 组合键解析，与平台无关，便于直接测试：返回 (修饰键, ANSI 物理键码)。
#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub(super) struct Mods { pub shift: bool, pub ctrl: bool, pub alt: bool, pub cmd: bool }

/// ponytail: 键码按 ANSI/US 物理键位；非 US 布局下 Cmd+字母 可能对应到别的字符，需要时改查 TIS 布局表。
#[cfg(any(target_os = "macos", test))]
pub(super) fn parse_combo(raw: &str) -> Result<(Mods, u16)> {
    let (mut mods, mut main) = (Mods::default(), None);
    for part in raw.split('+') {
        match part.to_ascii_lowercase().as_str() {
            "shift" => mods.shift = true,
            "ctrl" | "control" => mods.ctrl = true,
            "alt" | "option" => mods.alt = true,
            "meta" | "super" | "cmd" | "win" => mods.cmd = true,
            key => {
                let code = key_code(key).ok_or_else(|| format!("后台按键不支持：{part}"))?;
                if main.replace(code).is_some() {
                    return Err("后台组合键只能有一个非修饰键".into());
                }
            }
        }
    }
    Ok((mods, main.ok_or("后台组合键需要一个非修饰键")?))
}

#[cfg(any(target_os = "macos", test))]
fn key_code(name: &str) -> Option<u16> {
    Some(match name {
        "a" => 0, "s" => 1, "d" => 2, "f" => 3, "h" => 4, "g" => 5, "z" => 6, "x" => 7, "c" => 8, "v" => 9,
        "b" => 11, "q" => 12, "w" => 13, "e" => 14, "r" => 15, "y" => 16, "t" => 17,
        "1" => 18, "2" => 19, "3" => 20, "4" => 21, "6" => 22, "5" => 23, "=" => 24, "9" => 25, "7" => 26,
        "-" => 27, "8" => 28, "0" => 29, "]" => 30, "o" => 31, "u" => 32, "[" => 33, "i" => 34, "p" => 35,
        "enter" | "return" => 36, "l" => 37, "j" => 38, "'" => 39, "k" => 40, ";" => 41, "\\" => 42,
        "," => 43, "/" => 44, "n" => 45, "m" => 46, "." => 47, "tab" => 48, "space" => 49, "`" => 50,
        "backspace" => 51, "escape" | "esc" => 53, "delete" => 117,
        "home" => 115, "end" => 119, "pageup" => 116, "pagedown" => 121,
        "left" | "arrowleft" => 123, "right" | "arrowright" => 124, "down" | "arrowdown" => 125, "up" | "arrowup" => 126,
        "f1" => 122, "f2" => 120, "f3" => 99, "f4" => 118, "f5" => 96, "f6" => 97,
        "f7" => 98, "f8" => 100, "f9" => 101, "f10" => 109, "f11" => 103, "f12" => 111,
        _ => return None,
    })
}

/// 后台键位表比 Enigo 窄：整批预校验，不能等前几个动作已执行后才发现按键不支持。
pub(super) fn validate(a: &Action) -> Result<()> {
    #[cfg(target_os = "macos")]
    if a.action == "press" {
        parse_combo(a.key.as_deref().ok_or("press 缺少key")?)?;
    }
    let _ = a;
    Ok(())
}

/// 把一个动作投给 `pid`。`at`/`to` 是屏幕全局坐标（点）；返回前绝不触碰真实输入设备。
#[cfg(target_os = "macos")]
pub(super) fn post(pid: i32, window: u32, at: (i32, i32), to: (i32, i32), a: &Action) -> Result<()> {
    mac::post(pid, window, at, to, a)
}
#[cfg(not(target_os = "macos"))]
pub(super) fn post(_: i32, _: u32, _: (i32, i32), _: (i32, i32), _: &Action) -> Result<()> {
    Err("该平台没有按进程投递事件的通道".into())
}

/// 真实前台窗口是否在后台批次中被应用自行改变（AppKit 可能在收到点击时激活应用）。
pub(super) fn foreground_notice(before: Option<(u32, u32)>, after: Option<(u32, u32)>) -> Option<Value> {
    (before != after).then(|| json!({"before":before,"after":after,
        "notice":"应用在后台收到输入后自行把窗口提到了前台，影响了用户；改用键盘或换不抢前台的方式，不要重放整批"}))
}

#[cfg(target_os = "macos")]
mod mac {
    use super::*;
    use core_graphics::event::{CGEvent, CGEventFlags, CGEventType, CGMouseButton, EventField, ScrollEventUnit};
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
    use core_graphics::geometry::CGPoint;
    use std::time::Duration;

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXIsProcessTrusted() -> bool;
    }
    /// 没有辅助功能授权时系统会静默丢弃事件，必须提前拦住，否则会得到「executed 但什么都没发生」。
    pub(super) fn trusted() -> bool {
        unsafe { AXIsProcessTrusted() }
    }

    fn pause(ms: u64) {
        std::thread::sleep(Duration::from_millis(ms));
    }
    fn failed<T>(_: T) -> String {
        "无法创建系统事件".into()
    }

    /// 按下并抬起一个物理键（带修饰键标志）。
    fn tap(source: &CGEventSource, pid: i32, code: u16, flags: CGEventFlags) -> Result<()> {
        for down in [true, false] {
            let event = CGEvent::new_keyboard_event(source.clone(), code, down).map_err(failed)?;
            event.set_flags(flags);
            event.post_to_pid(pid);
            pause(8);
        }
        Ok(())
    }

    /// CGEventKeyboardSetUnicodeString 一次最多可靠地带 20 个字符；输入文本忽略所有修饰键。
    fn type_text(source: &CGEventSource, pid: i32, text: &str) -> Result<()> {
        let chars: Vec<char> = text.chars().collect();
        for chunk in chars.chunks(20) {
            let chunk: String = chunk.iter().collect();
            for down in [true, false] {
                let event = CGEvent::new_keyboard_event(source.clone(), 0, down).map_err(failed)?;
                event.set_string(&chunk);
                event.set_flags(CGEventFlags::CGEventFlagNull);
                event.post_to_pid(pid);
            }
            pause(8);
        }
        Ok(())
    }

    pub(super) fn post(pid: i32, window: u32, at: (i32, i32), to: (i32, i32), a: &Action) -> Result<()> {
        // Private 状态源：用户此刻按住的真实修饰键不会混进我们的事件。
        let source = CGEventSource::new(CGEventSourceStateID::Private).map_err(failed)?;
        let point = |p: (i32, i32)| CGPoint::new(p.0 as f64, p.1 as f64);
        let button = match a.button.as_deref() {
            Some("right") => (CGMouseButton::Right, CGEventType::RightMouseDown, CGEventType::RightMouseUp, CGEventType::RightMouseDragged),
            Some("middle") => (CGMouseButton::Center, CGEventType::OtherMouseDown, CGEventType::OtherMouseUp, CGEventType::OtherMouseDragged),
            _ => (CGMouseButton::Left, CGEventType::LeftMouseDown, CGEventType::LeftMouseUp, CGEventType::LeftMouseDragged),
        };
        let mouse = |kind: CGEventType, p: (i32, i32), click: i64| -> Result<()> {
            let event = CGEvent::new_mouse_event(source.clone(), kind, point(p), button.0).map_err(failed)?;
            event.set_integer_value_field(EventField::MOUSE_EVENT_CLICK_STATE, click);
            // 告诉 AppKit 这个点属于哪个窗口：投递给进程时不会经过 WindowServer 的命中测试。
            event.set_integer_value_field(EventField::MOUSE_EVENT_WINDOW_UNDER_MOUSE_POINTER, window as i64);
            event.set_integer_value_field(EventField::MOUSE_EVENT_WINDOW_UNDER_MOUSE_POINTER_THAT_CAN_HANDLE_THIS_EVENT, window as i64);
            event.post_to_pid(pid);
            Ok(())
        };
        let hover = |p: (i32, i32)| mouse(CGEventType::MouseMoved, p, 0);
        let click = |p: (i32, i32), count: i64| -> Result<()> {
            mouse(button.1, p, count)?;
            pause(25);
            mouse(button.2, p, count)
        };
        match a.action.as_str() {
            "move" => hover(at)?,
            "click" => {
                hover(at)?;
                click(at, 1)?;
            }
            "double_click" => {
                hover(at)?;
                click(at, 1)?;
                pause(60);
                click(at, 2)?;
            }
            "drag" => {
                hover(at)?;
                mouse(button.1, at, 1)?;
                for i in 1..=12i64 {
                    let step = |from: i32, end: i32| (from as i64 + (end as i64 - from as i64) * i / 12) as i32;
                    mouse(button.3, (step(at.0, to.0), step(at.1, to.1)), 1)?;
                    pause(16);
                }
                mouse(button.2, to, 1)?;
            }
            "scroll" => {
                hover(at)?;
                let delta = a.delta.unwrap_or(0);
                let horizontal = a.axis.as_deref() == Some("horizontal");
                // 与 Enigo 同号：delta>0 向下/向右。
                let (w1, w2) = if horizontal { (0, -delta.signum()) } else { (-delta.signum(), 0) };
                for _ in 0..delta.unsigned_abs() {
                    let event = CGEvent::new_scroll_event(source.clone(), ScrollEventUnit::LINE, if horizontal { 2 } else { 1 }, w1, w2, 0).map_err(failed)?;
                    event.set_location(point(at));
                    event.post_to_pid(pid);
                    pause(8);
                }
            }
            "type" => {
                // 换行/制表符作为文本块首字符时 SetUnicodeString 会静默失败，拆成真实的 Return/Tab 键。
                let mut text = String::new();
                for c in a.text.as_deref().unwrap_or_default().replace("\r\n", "\n").chars() {
                    match c {
                        '\n' | '\r' | '\t' => {
                            type_text(&source, pid, &std::mem::take(&mut text))?;
                            tap(&source, pid, if c == '\t' { 48 } else { 36 }, CGEventFlags::CGEventFlagNull)?;
                        }
                        _ => text.push(c),
                    }
                }
                type_text(&source, pid, &text)?;
            }
            "press" => {
                let (mods, code) = parse_combo(a.key.as_deref().ok_or("press 缺少key")?)?;
                let mut flags = CGEventFlags::CGEventFlagNull;
                for (on, flag) in [(mods.shift, CGEventFlags::CGEventFlagShift), (mods.ctrl, CGEventFlags::CGEventFlagControl),
                    (mods.alt, CGEventFlags::CGEventFlagAlternate), (mods.cmd, CGEventFlags::CGEventFlagCommand)] {
                    if on { flags |= flag; }
                }
                tap(&source, pid, code, flags)?;
            }
            "wait" => pause(a.ms.unwrap_or(250)),
            _ => unreachable!(),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combos_split_modifiers_from_one_physical_key() {
        let (mods, code) = parse_combo("Cmd+Shift+4").unwrap();
        assert_eq!((mods, code), (Mods { cmd: true, shift: true, ..Default::default() }, 21));
        assert_eq!(parse_combo("Enter").unwrap().1, 36);
        assert_eq!(parse_combo("ctrl+left").unwrap(), (Mods { ctrl: true, ..Default::default() }, 123));
        // 只有修饰键、两个主键、未知键都必须在发送任何事件之前被拒绝。
        assert!(parse_combo("cmd+shift").is_err());
        assert!(parse_combo("a+b").is_err());
        assert!(parse_combo("你").is_err());
    }

    #[test]
    fn scope_matches_each_platforms_background_surface() {
        assert!(check_scope(cfg!(target_os = "macos")).is_ok());
        assert!(check_scope(!cfg!(target_os = "macos")).is_err());
    }

    #[test]
    fn scope_guard_resets_even_when_nested_work_fails() {
        assert!(!active());
        {
            let _off = enter(false).unwrap();
            assert!(!active());
        }
        ACTIVE.with(|a| a.set(true));
        drop(Scope(true));
        assert!(!active());
    }
}

/// 端到端：真实 Xvfb 座席 + 一个 X 客户端窗口，验证后台截图、点击、键入确实只落在座席里。
/// 没装 Xvfb 时跳过（与 desktop_smoke 一样依赖本机环境）。
#[cfg(all(test, target_os = "linux"))]
mod seat_e2e {
    use super::super::{input_activity, run};
    use super::*;
    use std::time::{Duration, Instant};
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{AtomEnum, ChangeGCAux, ChangeWindowAttributesAux, ConnectionExt, CreateGCAux, CreateWindowAux, EventMask, InputFocus, PropMode, Rectangle, WindowClass};
    use x11rb::protocol::Event;

    /// Seat tests share one Xvfb. Hold this across a whole test so a painter cannot change pixels
    /// under the pointer-marker check, and so display numbers stay stable.
    fn seat_tests() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 剑来同一时刻只允许一个调用；并行的其它测试占着桌面锁时重试。
    fn call(args: Value) -> Value {
        for _ in 0..150 {
            match run("seat-e2e".into(), args.clone()) {
                Ok(v) if v["error"].as_str().is_some_and(|e| e.contains("请勿并行")) => {}
                Ok(v) => return v,
                Err(e) if e.contains("请勿并行") => {}
                Err(e) => panic!("{args}: {e}"),
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("桌面锁一直被占用");
    }

    #[test]
    fn seat_screenshot_click_and_type_reach_only_the_private_display() {
        let _seat = seat_tests();
        let display = match linux::seat() {
            Ok((display, ..)) => display,
            Err(e) => return eprintln!("跳过：无法启动座席：{e}"),
        };
        // 客户端窗口：纯红 200x200，位于座席 (100,100)。指针先挪到右下角，剑尖不会盖住取样点。
        let (conn, n) = x11rb::rust_connection::RustConnection::connect(Some(display.as_str())).unwrap();
        let screen = &conn.setup().roots[n];
        let (root, depth) = (screen.root, screen.root_depth);
        let window = conn.generate_id().unwrap();
        conn.create_window(depth, window, root, 100, 100, 200, 200, 0, WindowClass::INPUT_OUTPUT, 0,
            &CreateWindowAux::new().background_pixel(0x00ff_0000).event_mask(EventMask::BUTTON_PRESS | EventMask::KEY_PRESS)).unwrap();
        conn.map_window(window).unwrap();
        conn.warp_pointer(x11rb::NONE, root, 0, 0, 0, 0, 1300, 800).unwrap();
        conn.set_input_focus(InputFocus::POINTER_ROOT, window, x11rb::CURRENT_TIME).unwrap();
        conn.flush().unwrap();
        std::thread::sleep(Duration::from_millis(200));
        let injected_before = input_activity().0;

        let shot = call(json!({"operation":"screenshot","background":true}));
        assert_eq!(shot["background"], true, "{shot}");
        let path = shot["images"][0]["path"].as_str().unwrap().to_string();
        let pixel = *xcap::image::open(&path).unwrap().to_rgba8().get_pixel(150, 150);
        assert_eq!(pixel.0, [255, 0, 0, 255], "后台截图应读到座席里的红色窗口");

        // 模式必须一致：后台快照上不带 background 的 act 不得执行，更不能落到真实输入。
        let act = |background: Option<bool>| {
            let mut args = json!({"operation":"act","snapshotId":shot["snapshotId"],"imageId":shot["images"][0]["imageId"],
                "feedback":"none","actions":[{"action":"click","x":150,"y":150},{"action":"type","text":"hi"},{"action":"press","key":"Enter"}]});
            if let Some(b) = background { args["background"] = json!(b); }
            call(args)
        };
        let refused = act(None);
        assert_eq!(refused["status"], "not_executed", "{refused}");
        assert_eq!(refused["completedActions"], 0);
        assert!(refused["error"].as_str().unwrap().contains("后台截图"), "{refused}");

        // 被拒绝的调用不消耗快照；同一快照上正确的后台 act 执行。
        let done = act(Some(true));
        assert_eq!(done["status"], "executed", "{done}");
        assert_eq!(done["completedActions"], 3);

        let (mut clicks, mut keys) = (vec![], 0);
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline && !(clicks.len() == 1 && keys >= 3) {
            match conn.poll_for_event().unwrap() {
                Some(Event::ButtonPress(e)) => clicks.push((e.event_x, e.event_y)),
                Some(Event::KeyPress(_)) => keys += 1,
                Some(_) => {}
                None => std::thread::sleep(Duration::from_millis(20)),
            }
        }
        // Image pixels are the model's coordinate space. A 1440×900 seat is under the long-edge
        // cap but over the pixel budget, so (150,150) is not screen (150,150).
        let image = &shot["images"][0];
        let map = |axis: &str, original: &str| {
            let at = 150u64;
            let pixels = image[axis].as_u64().unwrap();
            let source = image[original].as_u64().unwrap();
            ((2 * at + 1) * source / (2 * pixels)) as i16 - 100
        };
        let expected = (map("width", "originalWidth"), map("height", "originalHeight"));
        assert_eq!(clicks, vec![expected], "点击应以窗口内相对坐标落在座席窗口上");
        assert!(keys >= 3, "h、i、Enter 都应送达座席窗口，实际 {keys}");
        assert_eq!(input_activity().0, injected_before, "后台输入不得记为真实注入，否则会掩盖用户自己的操作");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn dead_lock_pid_frees_the_display_number() {
        let n = 480u32;
        let lock = format!("/tmp/.X{n}-lock");
        let sock = format!("/tmp/.X11-unix/X{n}");
        let _ = std::fs::remove_file(&lock);
        let _ = std::fs::remove_file(&sock);
        let mut sleeper = std::process::Command::new("sleep").arg("30").spawn().expect("sleep");
        std::fs::write(&lock, format!("{:10}\n", sleeper.id())).unwrap();
        std::fs::write(&sock, b"live").unwrap();
        assert!(!linux::display_available(n), "a live lock pid must keep the display number");
        assert!(std::path::Path::new(&lock).exists());
        sleeper.kill().unwrap();
        let _ = sleeper.wait();
        assert!(linux::display_available(n), "a dead lock pid must be reclaimed with its socket");
        assert!(!std::path::Path::new(&lock).exists());
        assert!(!std::path::Path::new(&sock).exists());

        std::fs::write(&lock, "not-a-pid\n").unwrap();
        std::fs::write(&sock, b"stale").unwrap();
        assert!(linux::display_available(n), "a lock without a pid is stale");
        assert!(!std::path::Path::new(&lock).exists() && !std::path::Path::new(&sock).exists());
    }

    #[test]
    fn seat_outlives_the_thread_that_asked_for_it() {
        let _seat = seat_tests();
        let started = std::thread::spawn(|| linux::seat().map(|(display, ..)| display)).join().unwrap();
        let display = match started {
            Ok(display) => display,
            Err(e) => return eprintln!("跳过：无法启动座席：{e}"),
        };
        std::thread::sleep(Duration::from_millis(400));
        x11rb::rust_connection::RustConnection::connect(Some(display.as_str()))
            .expect("座席不能随调用线程退出而消失");
        let again = linux::seat().expect("座席还在").0;
        assert_eq!(again, display, "线程退出后不应再占一个新的显示号");
    }

    #[test]
    fn moved_pointer_draws_a_new_sword_tip() {
        let _seat = seat_tests();
        let display = match linux::seat() {
            Ok((display, ..)) => display,
            Err(e) => return eprintln!("跳过：无法启动座席：{e}"),
        };
        let (conn, n) = x11rb::rust_connection::RustConnection::connect(Some(display.as_str())).unwrap();
        let root = conn.setup().roots[n].root;
        conn.warp_pointer(x11rb::NONE, root, 0, 0, 0, 0, 40, 40).unwrap();
        conn.flush().unwrap();
        drop(conn);
        let grabbed = Instant::now();
        linux::seat_grab().expect("座席截图");
        eprintln!("seat grab: {}ms", grabbed.elapsed().as_millis());
        let first = call(json!({"operation":"screenshot","background":true}));
        let image = &first["images"][0];
        let cursor = &image["cursor"];
        assert!(cursor.is_object(), "Linux 截图应带剑尖，实际 {first}");
        let (w, h) = (image["width"].as_u64().unwrap(), image["height"].as_u64().unwrap());
        let (x0, y0) = (cursor["x"].as_u64().unwrap(), cursor["y"].as_u64().unwrap());
        let (tx, ty) = if x0 + 220 < w && y0 + 140 < h { (x0 + 220, y0 + 140) } else { (12, 12) };
        assert!(tx != x0 || ty != y0);
        let moved = call(json!({"operation":"act","background":true,"snapshotId":first["snapshotId"],"imageId":image["imageId"],
            "feedback":"screenshot","actions":[{"action":"move","x":tx,"y":ty,"glide":false}]}));
        assert_eq!(moved["status"], "executed", "{moved}");
        let next = &moved["images"][0];
        assert_ne!(next["unchanged"].as_bool(), Some(true), "只移动指针也要换一张图：{next}");
        let (x1, y1) = (next["cursor"]["x"].as_i64().unwrap(), next["cursor"]["y"].as_i64().unwrap());
        assert!((x1 - tx as i64).abs() <= 2 && (y1 - ty as i64).abs() <= 2, "剑尖应落在移动目标上：{next}");
        eprintln!("pointer-only feedback timings: {}", moved["timingsMs"]);
        let encode = moved["timingsMs"]["encodeAndSave"].as_u64().unwrap_or(u64::MAX);
        assert!(encode < 80, "只移动指针不应整帧重编码，encodeAndSave={encode}ms");
        assert_eq!(moved["timingsMs"]["reusedFrames"].as_u64(), Some(0), "剑尖移动不是复用旧文件");
        let old_img = xcap::image::open(image["path"].as_str().unwrap()).unwrap().to_rgba8();
        let new_img = xcap::image::open(next["path"].as_str().unwrap()).unwrap().to_rgba8();
        assert_eq!(old_img.dimensions(), new_img.dimensions());
        let rows = |tip_y: i64| {
            let y0 = (tip_y - 10).max(0) as u32;
            let y1 = (tip_y - 10 + 32).clamp(0, old_img.height() as i64) as u32;
            (y0, y1)
        };
        let (a0, a1) = rows(y0 as i64);
        let (b0, b1) = rows(y1);
        let stride = old_img.width() as usize * 4;
        let (raw_old, raw_new) = (old_img.as_raw(), new_img.as_raw());
        let (mut outside, mut tip_changed) = (false, false);
        for y in 0..old_img.height() {
            let span = y as usize * stride..(y as usize + 1) * stride;
            let covered = (y >= a0 && y < a1) || (y >= b0 && y < b1);
            if covered {
                if y >= b0 && y < b1 && raw_old[span.clone()] != raw_new[span.clone()] {
                    tip_changed = true;
                }
            } else {
                assert_eq!(&raw_old[span.clone()], &raw_new[span], "剑尖以外的第 {y} 行不应变化");
                outside = true;
            }
        }
        assert!(outside && tip_changed, "新剑尖应只改到自己附近的行");
        let _ = std::fs::remove_file(image["path"].as_str().unwrap());
        let _ = std::fs::remove_file(next["path"].as_str().unwrap());
    }

    #[test]
    fn repaint_ends_the_wait_and_a_key_does_not() {
        let _seat = seat_tests();
        let display = match linux::seat() {
            Ok((display, ..)) => display,
            Err(e) => return eprintln!("跳过：无法启动座席：{e}"),
        };
        let (conn, n) = x11rb::rust_connection::RustConnection::connect(Some(display.as_str())).unwrap();
        let screen = &conn.setup().roots[n];
        let (root, depth) = (screen.root, screen.root_depth);
        let window = conn.generate_id().unwrap();
        conn.create_window(depth, window, root, 400, 400, 220, 220, 0, WindowClass::INPUT_OUTPUT, 0,
            &CreateWindowAux::new().background_pixel(0x0000_00ff)
                .event_mask(EventMask::BUTTON_PRESS | EventMask::KEY_PRESS | EventMask::EXPOSURE)).unwrap();
        conn.map_window(window).unwrap();
        conn.set_input_focus(InputFocus::POINTER_ROOT, window, x11rb::CURRENT_TIME).unwrap();
        conn.flush().unwrap();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let paint_stop = stop.clone();
        let painter = std::thread::spawn(move || {
            let mut color = 0x0000_00ffu32;
            while !paint_stop.load(std::sync::atomic::Ordering::Relaxed) {
                match conn.poll_for_event() {
                    Ok(Some(Event::ButtonPress(_))) | Ok(Some(Event::KeyPress(_))) | Ok(Some(Event::Expose(_))) => {
                        color ^= 0x00ff_0000;
                        let _ = conn.change_window_attributes(window, &ChangeWindowAttributesAux::new().background_pixel(color));
                        let _ = conn.clear_area(false, window, 0, 0, 0, 0);
                        let _ = conn.flush();
                    }
                    Ok(Some(_)) => {}
                    Ok(None) => std::thread::sleep(Duration::from_millis(2)),
                    Err(_) => break,
                }
            }
        });
        let shot = call(json!({"operation":"screenshot","background":true}));
        let image = &shot["images"][0];
        let at = |screen: u64, axis: &str, original: &str| {
            let pixels = image[axis].as_u64().unwrap();
            let source = image[original].as_u64().unwrap();
            screen * 2 * pixels / source / 2
        };
        let (x, y) = (at(500, "width", "originalWidth"), at(500, "height", "originalHeight"));
        let (warp, wn) = x11rb::rust_connection::RustConnection::connect(Some(display.as_str())).unwrap();
        warp.warp_pointer(x11rb::NONE, warp.setup().roots[wn].root, 0, 0, 0, 0, 470, 470).unwrap();
        warp.flush().unwrap();
        drop(warp);
        let started = Instant::now();
        let done = call(json!({"operation":"act","background":true,"snapshotId":shot["snapshotId"],"imageId":image["imageId"],
            "feedback":"none","actions":[
                {"action":"click","x":x,"y":y},
                {"action":"type","text":"hi"},
                {"action":"press","key":"Enter"}
            ]}));
        let elapsed = started.elapsed();
        eprintln!("seat click+type+enter after settle: {}ms", elapsed.as_millis());
        assert_eq!(done["status"], "executed", "{done}");
        // Full budgets are 300+300+180. A repaint must release the click and the type before that sum.
        assert!(elapsed < Duration::from_millis(700), "绘制后应提前结束，实际 {}ms", elapsed.as_millis());
        let fresh = call(json!({"operation":"screenshot","background":true}));
        let key_started = Instant::now();
        let key = call(json!({"operation":"act","background":true,"snapshotId":fresh["snapshotId"],"imageId":fresh["images"][0]["imageId"],
            "feedback":"none","actions":[{"action":"press","key":"Escape"}]}));
        let key_elapsed = key_started.elapsed();
        assert_eq!(key["status"], "executed", "{key}");
        assert!(key_elapsed >= Duration::from_millis(140), "按键仍睡满预算，实际 {}ms", key_elapsed.as_millis());
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let _ = painter.join();
        let _ = std::fs::remove_file(image["path"].as_str().unwrap());
    }

    #[test]
    fn small_paint_settles_and_clock_or_pointer_motion_does_not() {
        let _seat = seat_tests();
        let display = match linux::seat() {
            Ok((display, ..)) => display,
            Err(e) => return eprintln!("跳过：无法启动座席：{e}"),
        };
        let (conn, n) = x11rb::rust_connection::RustConnection::connect(Some(display.as_str())).unwrap();
        let screen = &conn.setup().roots[n];
        let (root, depth) = (screen.root, screen.root_depth);
        let [clock, nova, editor, quiet] = [0, 1, 2, 3].map(|_| conn.generate_id().unwrap());
        let make = |id, x, y, w, h, pixel, mask| {
            conn.create_window(depth, id, root, x, y, w, h, 0, WindowClass::INPUT_OUTPUT, 0,
                &CreateWindowAux::new().background_pixel(pixel).event_mask(mask)).unwrap();
        };
        make(clock, 8, 8, 64, 24, 0x0000_0080, EventMask::NO_EVENT);
        make(nova, 8, 40, 90, 24, 0x0000_8000, EventMask::NO_EVENT);
        make(editor, 720, 120, 280, 70, 0x0020_2020, EventMask::BUTTON_PRESS | EventMask::KEY_PRESS);
        make(quiet, 720, 260, 180, 70, 0x0040_4040, EventMask::BUTTON_PRESS);
        let wm_class = conn.intern_atom(false, b"WM_CLASS").unwrap().reply().unwrap().atom;
        let clock_class: &[u8] = b"clock\0Clock\0";
        let nova_class: &[u8] = b"nova\0Nova\0";
        conn.change_property(PropMode::REPLACE, clock, wm_class, AtomEnum::STRING, 8, clock_class.len() as u32, clock_class).unwrap();
        conn.change_property(PropMode::REPLACE, nova, wm_class, AtomEnum::STRING, 8, nova_class.len() as u32, nova_class).unwrap();
        let gc = conn.generate_id().unwrap();
        conn.create_gc(gc, editor, &CreateGCAux::new().foreground(0x00ff_ffff)).unwrap();
        for window in [clock, nova, editor, quiet] {
            conn.map_window(window).unwrap();
        }
        conn.flush().unwrap();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let tick = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let keys_seen = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let clicks_seen = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let (paint_stop, paint_tick, paint_keys, paint_clicks) = (stop.clone(), tick.clone(), keys_seen.clone(), clicks_seen.clone());
        let painter = std::thread::spawn(move || {
            let mut color = 0x00ff_ffffu32;
            let mut last = Instant::now();
            while !paint_stop.load(std::sync::atomic::Ordering::Relaxed) {
                if paint_tick.load(std::sync::atomic::Ordering::Relaxed) && last.elapsed() >= Duration::from_millis(15) {
                    color ^= 0x0000_00ff;
                    let _ = conn.change_gc(gc, &ChangeGCAux::new().foreground(color));
                    let mark = [Rectangle { x: 2, y: 2, width: 40, height: 16 }];
                    let _ = conn.poly_fill_rectangle(clock, gc, &mark);
                    let _ = conn.poly_fill_rectangle(nova, gc, &mark);
                    let _ = conn.flush();
                    last = Instant::now();
                }
                match conn.poll_for_event() {
                    Ok(Some(Event::ButtonPress(e))) if e.event == editor => {
                        paint_clicks.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let _ = conn.change_gc(gc, &ChangeGCAux::new().foreground(0x00ff_0000));
                        let _ = conn.poly_fill_rectangle(editor, gc, &[Rectangle { x: 20, y: 20, width: 12, height: 12 }]);
                        let _ = conn.flush();
                    }
                    Ok(Some(Event::ButtonPress(e))) if e.event == quiet => {
                        paint_clicks.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let _ = conn.change_gc(gc, &ChangeGCAux::new().foreground(color ^ 0x0000_ff00));
                        let _ = conn.poly_fill_rectangle(clock, gc, &[Rectangle { x: 2, y: 2, width: 40, height: 16 }]);
                        let _ = conn.flush();
                        // The settle wait is already running. Flying the pointer must not end it.
                        for x in [280i16, 460, 640, 820, 360] {
                            let _ = conn.warp_pointer(x11rb::NONE, root, 0, 0, 0, 0, x, 520);
                            let _ = conn.flush();
                        }
                    }
                    Ok(Some(Event::KeyPress(_))) => {
                        paint_keys.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        color ^= 0x00ff_0000;
                        let _ = conn.change_gc(gc, &ChangeGCAux::new().foreground(color));
                        let _ = conn.poly_fill_rectangle(editor, gc, &[Rectangle { x: 4, y: 4, width: 8, height: 14 }]);
                        let _ = conn.flush();
                    }
                    Ok(Some(_)) => {}
                    Ok(None) => std::thread::sleep(Duration::from_millis(2)),
                    Err(_) => break,
                }
            }
        });
        std::thread::sleep(Duration::from_millis(80));
        let (side, sn) = x11rb::rust_connection::RustConnection::connect(Some(display.as_str())).unwrap();
        let side_root = side.setup().roots[sn].root;
        let focus = |window| {
            side.set_input_focus(InputFocus::POINTER_ROOT, window, x11rb::CURRENT_TIME).unwrap();
            side.flush().unwrap();
        };
        let warp = |x, y| {
            side.warp_pointer(x11rb::NONE, side_root, 0, 0, 0, 0, x, y).unwrap();
            side.flush().unwrap();
        };
        let fresh = || call(json!({"operation":"screenshot","background":true}));
        let act_on = |shot: &Value, actions: Value| {
            let started = Instant::now();
            let done = call(json!({"operation":"act","background":true,"snapshotId":shot["snapshotId"],"imageId":shot["images"][0]["imageId"],
                "feedback":"none","actions":actions}));
            let elapsed = started.elapsed();
            assert_eq!(done["status"], "executed", "{done}");
            if let Some(path) = shot["images"][0]["path"].as_str() {
                let _ = std::fs::remove_file(path);
            }
            elapsed
        };
        let point = |image: &Value, x: u64, y: u64| {
            let at = |screen: u64, axis: &str, original: &str| {
                screen * 2 * image[axis].as_u64().unwrap() / image[original].as_u64().unwrap() / 2
            };
            (at(x, "width", "originalWidth"), at(y, "height", "originalHeight"))
        };

        let shot = fresh();
        focus(editor);
        let char_elapsed = act_on(&shot, json!([{"action":"type","text":"a"}]));
        eprintln!("仿 xterm 单字符等待: {}ms，编辑器收到 {} 个按键", char_elapsed.as_millis(), keys_seen.load(std::sync::atomic::Ordering::Relaxed));
        assert!(char_elapsed < Duration::from_millis(130), "一个字符绘出后应结束等待，实际 {}ms", char_elapsed.as_millis());

        tick.store(false, std::sync::atomic::Ordering::Relaxed);
        std::thread::sleep(Duration::from_millis(40));
        let shot = fresh();
        let (qx, qy) = point(&shot["images"][0], 780, 290);
        let quiet_elapsed = act_on(&shot, json!([{"action":"click","x":qx,"y":qy,"glide":false}]));
        eprintln!("时钟跳一下且指针飞过、点击本身不绘制: {}ms", quiet_elapsed.as_millis());
        assert!(quiet_elapsed >= Duration::from_millis(155), "时钟、Nova 窗口和指针飞过不能提前结束，实际 {}ms", quiet_elapsed.as_millis());

        focus(editor);
        let shot = fresh();
        let (bx, by) = point(&shot["images"][0], 780, 150);
        let button_elapsed = act_on(&shot, json!([{"action":"click","x":bx,"y":by,"glide":false}]));
        eprintln!("小按钮高亮: {}ms", button_elapsed.as_millis());
        assert!(button_elapsed < Duration::from_millis(130), "按钮高亮后应结束等待，实际 {}ms", button_elapsed.as_millis());
        assert!(clicks_seen.load(std::sync::atomic::Ordering::Relaxed) >= 2, "空白点击和按钮点击都应送到窗口");

        let shot = fresh();
        let key_elapsed = act_on(&shot, json!([{"action":"press","key":"Escape"}]));
        eprintln!("单独按键: {}ms", key_elapsed.as_millis());
        assert!(key_elapsed >= Duration::from_millis(150), "单独按键仍睡满预算，实际 {}ms", key_elapsed.as_millis());

        focus(editor);
        warp(760, 145);
        let shot = fresh();
        let (ex, ey) = point(&shot["images"][0], 780, 150);
        let typed = act_on(&shot, json!([
            {"action":"click","x":ex,"y":ey},
            {"action":"type","text":"echo hello"},
            {"action":"press","key":"Enter"}
        ]));
        eprintln!("点击 + type echo hello + Enter: {}ms", typed.as_millis());
        assert!(typed < Duration::from_millis(620), "点击、输入和回车应明显低于 810ms，实际 {}ms", typed.as_millis());

        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let _ = painter.join();
    }

    #[test]
    fn background_flag_is_rejected_where_it_cannot_apply() {
        for args in [json!({"operation":"activate","windowId":1,"background":true}),
                     json!({"operation":"screenshot","monitorId":1,"background":true})] {
            let error = run("seat-e2e".into(), args).unwrap_err();
            assert!(error.contains("background") || error.contains("后台"), "{error}");
        }
    }
}
