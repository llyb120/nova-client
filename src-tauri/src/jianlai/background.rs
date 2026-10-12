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
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Mutex;
    use std::time::Duration;
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{ConnectionExt, ImageFormat};

    const SIZE: (u32, u32) = (1440, 900);
    /// 座席在剑来里的虚拟显示器 id 高位；低位是第几次拉起，座席重启后旧快照因 Surface 不同而失效。
    const SEAT_MONITOR: u32 = 0x5ea7_0000;
    static GENERATION: AtomicU32 = AtomicU32::new(0);
    // ponytail: 全进程一个座席，所有会话共用；要按会话隔离时改成 owner → Seat 的表。
    static SEAT: Mutex<Option<(Child, String, u32)>> = Mutex::new(None);

    /// 座席的 (DISPLAY, 尺寸, 第几代)；没有或已退出就重新拉起。
    pub(super) fn seat() -> Result<(String, (u32, u32), u32)> {
        let mut seat = SEAT.lock().map_err(|_| "座席锁不可用")?;
        if let Some((child, display, generation)) = seat.as_mut() {
            if child.try_wait().map_err(err)?.is_none() {
                return Ok((display.clone(), SIZE, *generation));
            }
        }
        let n = (200..300)
            .find(|n| !std::path::Path::new(&format!("/tmp/.X11-unix/X{n}")).exists()
                && !std::path::Path::new(&format!("/tmp/.X{n}-lock")).exists())
            .ok_or("没有空闲的 X 显示号")?;
        let display = format!(":{n}");
        let mut command = Command::new("Xvfb");
        command
            .args([&display, "-screen", "0", &format!("{}x{}x24", SIZE.0, SIZE.1), "-nolisten", "tcp"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // Nova 退出（含崩溃）时由内核带走座席，不留孤儿 Xvfb。
        unsafe {
            command.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                Ok(())
            });
        }
        let mut child = command.spawn().map_err(|e| format!("无法启动 Xvfb（请先安装 xvfb）：{e}"))?;
        // 套接字文件出现不等于已经在监听，以真正连上为准。
        for _ in 0..80 {
            if x11rb::rust_connection::RustConnection::connect(Some(display.as_str())).is_ok() {
                let generation = GENERATION.fetch_add(1, Ordering::Relaxed);
                *seat = Some((child, display.clone(), generation));
                return Ok((display, SIZE, generation));
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        let _ = child.kill();
        let _ = child.wait();
        Err(format!("Xvfb 在 2 秒内没有接受连接（{display}）"))
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
        let rgba = reply.data.chunks_exact(4).flat_map(|p| [p[2], p[1], p[0], 255]).collect();
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
    use x11rb::protocol::xproto::{ConnectionExt, CreateWindowAux, EventMask, WindowClass};
    use x11rb::protocol::Event;

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
        let display = match linux::seat() {
            Ok((display, ..)) => display,
            Err(e) => return eprintln!("跳过：无法启动座席：{e}"),
        };
        // 客户端窗口：纯红 200x200，位于座席 (100,100)。
        let (conn, n) = x11rb::rust_connection::RustConnection::connect(Some(display.as_str())).unwrap();
        let screen = &conn.setup().roots[n];
        let window = conn.generate_id().unwrap();
        conn.create_window(screen.root_depth, window, screen.root, 100, 100, 200, 200, 0, WindowClass::INPUT_OUTPUT, 0,
            &CreateWindowAux::new().background_pixel(0x00ff_0000).event_mask(EventMask::BUTTON_PRESS | EventMask::KEY_PRESS)).unwrap();
        conn.map_window(window).unwrap();
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
        assert_eq!(clicks, vec![(50, 50)], "点击应以窗口内相对坐标落在座席窗口上");
        assert!(keys >= 3, "h、i、Enter 都应送达座席窗口，实际 {keys}");
        assert_eq!(input_activity().0, injected_before, "后台输入不得记为真实注入，否则会掩盖用户自己的操作");
        let _ = std::fs::remove_file(path);
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
