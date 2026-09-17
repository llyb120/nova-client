//! 剑来: native desktop input and screenshot observations. No browser automation.
use enigo::{Axis, Button, Coordinate, Direction, Enigo, Key, Keyboard, Mouse, Settings};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    path::Path,
    sync::Mutex,
    time::{Duration, Instant},
};
use xcap::{Monitor, Window};

type Result<T> = std::result::Result<T, String>;
fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}
// ponytail: one desktop, one outstanding observation and a global try-lock; use a desktop broker if independent seats are needed.
static DESKTOP: Mutex<Option<Snapshot>> = Mutex::new(None);
static OBSERVATION_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

pub(crate) fn tool_definition() -> Value {
    serde_json::from_str(include_str!("../../scripts/jianlai-tool.json")).unwrap()
}

#[derive(Clone, Debug, PartialEq)]
struct Surface {
    id: u32,
    pid: Option<u32>,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}
#[derive(Clone)]
struct Shot {
    image_id: String,
    surface: Surface,
    pixels: (u32, u32),
    source_pixels: (u32, u32),
    region: Option<Region>,
}
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Region { x: u32, y: u32, width: u32, height: u32 }
impl Region {
    fn validate(self, pixels: (u32, u32)) -> Result<()> {
        if self.width == 0 || self.height == 0
            || self.x.checked_add(self.width).is_none_or(|v| v > pixels.0)
            || self.y.checked_add(self.height).is_none_or(|v| v > pixels.1) {
            return Err("region超出原始窗口截图范围，必须使用originalWidth/originalHeight像素坐标".into());
        }
        Ok(())
    }
}
struct Snapshot {
    id: String,
    owner: String,
    taken: Instant,
    window: Option<u32>,
    max_edge: u32,
    foreground: Option<(u32, u32)>,
    shots: Vec<Shot>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Request {
    operation: String,
    window_id: Option<u32>,
    monitor_id: Option<u32>,
    region: Option<Region>,
    snapshot_id: Option<String>,
    image_id: Option<String>,
    feedback: Option<String>,
    actions: Option<Vec<Action>>,
    max_edge: Option<u32>,
    image_path: Option<String>,
    notes: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Action {
    action: String,
    x: Option<i32>,
    y: Option<i32>,
    to_x: Option<i32>,
    to_y: Option<i32>,
    button: Option<String>,
    text: Option<String>,
    key: Option<String>,
    delta: Option<i32>,
    axis: Option<String>,
    ms: Option<u64>,
}

fn window_surface(w: &Window) -> Result<Surface> {
    Ok(Surface {
        id: w.id().map_err(err)?,
        pid: Some(w.pid().map_err(err)?),
        x: w.x().map_err(err)?,
        y: w.y().map_err(err)?,
        width: w.width().map_err(err)?,
        height: w.height().map_err(err)?,
    })
}
fn monitor_surface(m: &Monitor) -> Result<Surface> {
    // XCap exposes Linux monitor geometry in logical coordinates; X11 input uses physical pixels.
    let scale = if cfg!(target_os = "linux") {
        m.scale_factor().map_err(err)? as f64
    } else {
        1.0
    };
    if !scale.is_finite() || scale <= 0.0 {
        return Err("无效屏幕缩放比例".into());
    }
    Ok(Surface {
        id: m.id().map_err(err)?,
        pid: None,
        x: (m.x().map_err(err)? as f64 * scale).round() as i32,
        y: (m.y().map_err(err)? as f64 * scale).round() as i32,
        width: (m.width().map_err(err)? as f64 * scale).round() as u32,
        height: (m.height().map_err(err)? as f64 * scale).round() as u32,
    })
}
fn window(id: u32) -> Result<Window> {
    Window::all()
        .map_err(err)?
        .into_iter()
        .find(|w| w.id().ok() == Some(id))
        .ok_or_else(|| "程序窗口已关闭，请重新 windows".into())
}
#[cfg(windows)]
fn foreground() -> Result<Option<(u32, u32)>> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};
    // Read only: switching windows remains a mouse/keyboard operation.
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.is_null() { return Ok(None); }
    let mut pid = 0;
    if unsafe { GetWindowThreadProcessId(hwnd, &mut pid) } == 0 {
        return Err("无法读取前台窗口身份".into());
    }
    Ok(Some((hwnd as usize as u32, pid)))
}
#[cfg(not(windows))]
fn foreground() -> Result<Option<(u32, u32)>> {
    for w in Window::all().map_err(err)? {
        if w.is_focused().map_err(err)? {
            return Ok(Some((w.id().map_err(err)?, w.pid().map_err(err)?)));
        }
    }
    Ok(None)
}
fn windows() -> Result<Value> {
    let mut rows = Vec::new();
    for w in Window::all().map_err(err)? {
        rows.push(
            json!({"windowId":w.id().map_err(err)?,"pid":w.pid().map_err(err)?,
            "monitorId":w.current_monitor().and_then(|m| m.id()).ok(),
            "width":w.width().ok(),"height":w.height().ok(),
            "app":w.app_name().map_err(err)?,"title":w.title().map_err(err)?,
            "focused":w.is_focused().map_err(err)?,"minimized":w.is_minimized().map_err(err)?}),
        );
    }
    Ok(json!({"windows":rows}))
}
fn shot_folder(owner: &str) -> std::path::PathBuf {
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    owner.hash(&mut hash);
    crate::lyra::config::nova_root().join("desktop-shots").join(format!("{:016x}", hash.finish()))
}

fn cursor_position() -> Option<(i32, i32)> {
    #[cfg(windows)]
    {
        let mut p = windows_sys::Win32::Foundation::POINT { x:0, y:0 };
        if unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut p) } != 0 {
            return Some((p.x, p.y));
        }
    }
    None
}

fn mark_cursor(image: &mut xcap::image::RgbaImage, surface: &Surface, source: (u32, u32), region: Option<Region>, position: (i32, i32)) -> Option<(u32, u32)> {
    if surface.width == 0 || surface.height == 0 || source.0 == 0 || source.1 == 0 { return None; }
    let r = region.unwrap_or(Region { x:0, y:0, width:source.0, height:source.1 });
    if r.validate(source).is_err() { return None; }
    let x = (position.0 as i128 - surface.x as i128) * source.0 as i128 - r.x as i128 * surface.width as i128;
    let y = (position.1 as i128 - surface.y as i128) * source.1 as i128 - r.y as i128 * surface.height as i128;
    let width = r.width as i128 * surface.width as i128;
    let height = r.height as i128 * surface.height as i128;
    if x < 0 || y < 0 || x >= width || y >= height { return None; }
    let (x, y) = ((x * image.width() as i128 / width) as i32, (y * image.height() as i128 / height) as i32);
    // Hollow high-contrast ring: preserve the target pixels at the hotspot.
    for dy in -12i32..=12 {
        for dx in -12i32..=12 {
            let d = dx * dx + dy * dy;
            if !(64..=144).contains(&d) { continue; }
            let (px, py) = (x + dx, y + dy);
            if px >= 0 && py >= 0 && px < image.width() as i32 && py < image.height() as i32 {
                image.put_pixel(px as u32, py as u32, xcap::image::Rgba(if (81..=121).contains(&d) { [255,0,180,255] } else { [0,0,0,255] }));
            }
        }
    }
    Some((x as u32, y as u32))
}

fn visually_similar(a: &xcap::image::RgbaImage, b: &xcap::image::RgbaImage) -> bool {
    if a.dimensions() != b.dimensions() || a.is_empty() { return false; }
    // ponytail: tolerate 0.05% changed pixels for caret/noise; this can miss tiny updates.
    // Visual quiet is not application readiness; use semantic result checks, not a looser threshold.
    let allowance = a.as_raw().len() / 4 / 2000;
    a.pixels().zip(b.pixels()).filter(|(x, y)| {
        (0..3).any(|c| x[c].abs_diff(y[c]) > 16)
    }).take(allowance + 1).count() <= allowance
}

fn settled_image(mut grab: impl FnMut() -> Result<xcap::image::RgbaImage>, settle: bool)
    -> Result<(xcap::image::RgbaImage, Value)> {
    let started = Instant::now();
    let mut image = grab()?;
    let mut samples = 1;
    let mut status = "not_checked";
    if settle {
        let mut anchor = image.clone();
        let mut quiet_since = Instant::now();
        loop {
            let elapsed = started.elapsed();
            if elapsed >= Duration::from_millis(2000) { status = "timeout"; break; }
            std::thread::sleep(Duration::from_millis(150).min(Duration::from_millis(2000) - elapsed));
            image = grab()?;
            samples += 1;
            if !visually_similar(&anchor, &image) {
                anchor = image.clone();
                quiet_since = Instant::now();
            }
            // Compare against the quiet-period anchor so gradual changes cannot accumulate unnoticed.
            if started.elapsed() >= Duration::from_millis(2000) { status = "timeout"; break; }
            if started.elapsed() >= Duration::from_millis(600)
                && quiet_since.elapsed() >= Duration::from_millis(450) {
                status = "stable";
                break;
            }
        }
    }
    Ok((image, json!({"status":status,"samples":samples,"elapsedMs":started.elapsed().as_millis()})))
}

fn capture(owner: &str, window_id: Option<u32>, monitor_id: Option<u32>, region: Option<Region>, max_edge: u32, settle: bool, state: &mut Option<Snapshot>) -> Result<Value> {
    let started = Instant::now();
    let mut capture_ms = 0;
    let mut resize_ms = 0;
    let mut encode_ms = 0;
    *state = None;
    let id = uuid::Uuid::new_v4().to_string();
    let sequence = OBSERVATION_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let folder = shot_folder(owner);
    std::fs::create_dir_all(&folder).map_err(err)?;
    let before = foreground()?;
    let cursor_before = cursor_position();
    let mut shots = Vec::new();
    let mut images = Vec::new();
    let mut save = |surface: Surface, image: xcap::image::RgbaImage, stability: Value| -> Result<()> {
        let captured_at = chrono::Utc::now().to_rfc3339();
        if surface.width == 0 || surface.height == 0 || image.width() == 0 || image.height() == 0 {
            return Err("截图范围为空".into());
        }
        let mut image_id = format!(
            "{}-{}",
            if window_id.is_some() {
                "window"
            } else {
                "monitor"
            },
            surface.id
        );
        if let Some(r) = region {
            image_id.push_str(&format!("-region-{}-{}-{}-{}", r.x, r.y, r.width, r.height));
        }
        let original_pixels = image.dimensions();
        let image = if let Some(r) = region {
            r.validate(original_pixels)?;
            xcap::image::imageops::crop_imm(&image, r.x, r.y, r.width, r.height).to_image()
        } else { image };
        let resize_started = Instant::now();
        let mut image = if max_edge > 0 && image.width().max(image.height()) > max_edge {
            let scale = max_edge as f64 / image.width().max(image.height()) as f64;
            let (width, height) = image.dimensions();
            xcap::image::DynamicImage::ImageRgba8(image).resize_exact(
                (width as f64 * scale).round().max(1.0) as u32,
                (height as f64 * scale).round().max(1.0) as u32,
                xcap::image::imageops::FilterType::Triangle).into_rgba8()
        } else { image };
        resize_ms += resize_started.elapsed().as_millis();
        let cursor = cursor_before.filter(|p| Some(*p) == cursor_position())
            .filter(|_| window_id.is_none() || before.map(|f| f.0) == window_id)
            .and_then(|p| mark_cursor(&mut image, &surface, original_pixels, region, p));
        let path = folder.join(format!("{id}-{image_id}.png"));
        let encode_started = Instant::now();
        image.save(&path).map_err(err)?;
        encode_ms += encode_started.elapsed().as_millis();
        images.push(json!({"imageId":image_id,"path":path,"width":image.width(),"height":image.height(),
            "snapshotId":id,"capturedAt":captured_at,"stability":stability,
            "originalWidth":original_pixels.0,"originalHeight":original_pixels.1,
            "cursor":cursor.map(|(x,y)| json!({"x":x,"y":y,"marker":"magenta-ring","source":"system-pointer"})),
            "region":region.map(|r| json!({"x":r.x,"y":r.y,"width":r.width,"height":r.height})),
            "desktopBounds":{"x":surface.x,"y":surface.y,"width":surface.width,"height":surface.height}}));
        shots.push(Shot {
            image_id,
            surface,
            pixels: image.dimensions(),
            source_pixels: original_pixels,
            region,
        });
        Ok(())
    };
    if let Some(wid) = window_id {
        let w = window(wid)?;
        if w.is_minimized().map_err(err)? {
            return Err("窗口已最小化，请先通过桌面恢复窗口".into());
        }
        let surface = window_surface(&w)?;
        let capture_started = Instant::now();
        // PrintWindow omits separate popup windows (e.g. recipient suggestions).
        // Foreground input must observe the pixels that will actually receive it.
        let (image, stability) = settled_image(|| {
            if foreground()? != before || window_surface(&w)? != surface {
                return Err("取样期间窗口或焦点改变，请重新截图".into());
            }
            #[cfg(windows)]
            let image = if before == Some((surface.id, surface.pid.unwrap())) {
                let monitor = w.current_monitor().map_err(err)?;
                let bounds = monitor_surface(&monitor)?;
                let image = monitor.capture_image().map_err(err)?;
                visible_window_crop(&surface, &bounds, &image)?
            } else { w.capture_image().map_err(err)? };
            #[cfg(not(windows))]
            let image = w.capture_image().map_err(err)?;
            Ok(image)
        }, settle)?;
        capture_ms += capture_started.elapsed().as_millis();
        if window_surface(&w)? != surface {
            return Err("截图期间窗口移动，请重新截图".into());
        }
        save(surface, image, stability)?;
    } else {
        let monitors = Monitor::all().map_err(err)?;
        if monitors.is_empty() || monitors.len() > 16 {
            return Err("需要1至16个可截图显示器".into());
        }
        for m in monitors {
            let surface = monitor_surface(&m)?;
            if monitor_id.is_some_and(|id| id != surface.id) { continue; }
            let capture_started = Instant::now();
            let (image, stability) = settled_image(|| {
                if foreground()? != before || monitor_surface(&m)? != surface {
                    return Err("取样期间屏幕或焦点改变，请重新截图".into());
                }
                m.capture_image().map_err(err)
            }, settle)?;
            capture_ms += capture_started.elapsed().as_millis();
            save(surface, image, stability)?;
        }
        if shots.is_empty() { return Err("显示器不存在，请重新截图".into()); }
    }
    if foreground()? != before {
        return Err("截图期间焦点改变，请重新截图".into());
    }
    *state = Some(Snapshot {
        id: id.clone(),
        owner: owner.into(),
        taken: Instant::now(),
        window: window_id,
        max_edge,
        foreground: before,
        shots,
    });
    Ok(
        json!({"snapshotId":id,"observationSequence":sequence,"historical":false,"windowId":window_id,"images":images,"coordinateSpace":"image-pixels","expiresInMs":180000,
            "foreground":before,"maxEdge":max_edge,
            "timingsMs":{"capture":capture_ms,"resize":resize_ms,"encodeAndSave":encode_ms,
                "other":started.elapsed().as_millis().saturating_sub(capture_ms + resize_ms + encode_ms),
                "total":started.elapsed().as_millis()}}),
    )
}

#[cfg(windows)]
fn visible_window_crop(window: &Surface, monitor: &Surface, image: &xcap::image::RgbaImage) -> Result<xcap::image::RgbaImage> {
    let x = window.x as i64 - monitor.x as i64;
    let y = window.y as i64 - monitor.y as i64;
    if image.dimensions() != (monitor.width, monitor.height)
        || x < 0 || y < 0 || window.width == 0 || window.height == 0
        || x + window.width as i64 > monitor.width as i64
        || y + window.height as i64 > monitor.height as i64 {
        return Err("窗口跨屏、部分离屏或屏幕像素比例不一致，请使用monitorId截图定位".into());
    }
    Ok(xcap::image::imageops::crop_imm(image, x as u32, y as u32, window.width, window.height).to_image())
}

fn point(shot: &Shot, x: Option<i32>, y: Option<i32>) -> Result<(i32, i32)> {
    let (x, y) = (x.ok_or("缺少x坐标")?, y.ok_or("缺少y坐标")?);
    let (pw, ph) = shot.pixels;
    if x < 0 || y < 0 || x as u32 >= pw || y as u32 >= ph || pw == 0 || ph == 0
        || shot.source_pixels.0 == 0 || shot.source_pixels.1 == 0 {
        return Err("坐标超出原始截图范围".into());
    }
    let s = &shot.surface;
    let r = shot.region.unwrap_or(Region { x:0, y:0, width:shot.source_pixels.0, height:shot.source_pixels.1 });
    let px = s.x as i128 + (r.x as i128 * pw as i128 + x as i128 * r.width as i128)
        * s.width as i128 / (pw as i128 * shot.source_pixels.0 as i128);
    let py = s.y as i128 + (r.y as i128 * ph as i128 + y as i128 * r.height as i128)
        * s.height as i128 / (ph as i128 * shot.source_pixels.1 as i128);
    Ok((
        i32::try_from(px).map_err(err)?,
        i32::try_from(py).map_err(err)?,
    ))
}
fn keys(raw: &str) -> Result<Vec<Key>> {
    let keys = raw
        .split('+')
        .map(|s| match s.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => Ok(Key::Control),
            "alt" | "option" => Ok(Key::Alt),
            "shift" => Ok(Key::Shift),
            "meta" | "super" | "cmd" | "win" => Ok(Key::Meta),
            "enter" | "return" => Ok(Key::Return),
            "tab" => Ok(Key::Tab),
            "escape" | "esc" => Ok(Key::Escape),
            "space" => Ok(Key::Space),
            "backspace" => Ok(Key::Backspace),
            "delete" => Ok(Key::Delete),
            "left" => Ok(Key::LeftArrow),
            "right" => Ok(Key::RightArrow),
            "up" => Ok(Key::UpArrow),
            "down" => Ok(Key::DownArrow),
            "home" => Ok(Key::Home),
            "end" => Ok(Key::End),
            "pageup" => Ok(Key::PageUp),
            "pagedown" => Ok(Key::PageDown),
            "f1" => Ok(Key::F1),
            "f2" => Ok(Key::F2),
            "f3" => Ok(Key::F3),
            "f4" => Ok(Key::F4),
            "f5" => Ok(Key::F5),
            "f6" => Ok(Key::F6),
            "f7" => Ok(Key::F7),
            "f8" => Ok(Key::F8),
            "f9" => Ok(Key::F9),
            "f10" => Ok(Key::F10),
            "f11" => Ok(Key::F11),
            "f12" => Ok(Key::F12),
            // Enigo 0.6 passes VkKeyScanExW's shift-state high byte through as VK.
            // Shortcuts use physical letter/digit VKs; text still uses enigo.text.
            #[cfg(windows)]
            _ if s.len() == 1 && s.as_bytes()[0].is_ascii_alphanumeric() =>
                Ok(Key::Other(s.as_bytes()[0].to_ascii_uppercase() as u32)),
            _ if s.chars().count() == 1 => Ok(Key::Unicode(s.chars().next().unwrap())),
            _ => Err(format!("不支持的按键：{s}")),
        })
        .collect::<Result<Vec<_>>>()?;
    if keys.len() > 4 {
        return Err("组合键最多4键".into());
    }
    Ok(keys)
}
fn validate(a: &Action, shot: &Shot) -> Result<()> {
    if !matches!(
        a.action.as_str(),
        "click" | "double_click" | "move" | "drag" | "type" | "press" | "scroll" | "wait"
    ) {
        return Err("未知剑来动作".into());
    }
    if a.button
        .as_deref()
        .is_some_and(|b| !matches!(b, "left" | "right" | "middle"))
        || a.axis
            .as_deref()
            .is_some_and(|v| !matches!(v, "vertical" | "horizontal"))
    {
        return Err("button仅允许left/right/middle；axis仅允许vertical/horizontal".into());
    }
    if a.ms.unwrap_or(0) > 2000 {
        return Err(format!("ms={}，允许范围为 0–2000", a.ms.unwrap()));
    }
    if matches!(
        a.action.as_str(),
        "click" | "double_click" | "move" | "drag" | "scroll"
    ) {
        point(shot, a.x, a.y)?;
    }
    if a.action == "drag" {
        point(shot, a.to_x, a.to_y)?;
    }
    if a.action == "press" {
        keys(a.key.as_deref().ok_or("press 缺少key")?)?;
    }
    if a.action == "type" {
        let text = a.text.as_deref().ok_or("type 缺少text")?;
        if text.chars().count() > 10000 || text.contains('\0') {
            return Err("文本过长或含NUL".into());
        }
    }
    if a.action == "scroll" && !(-100..=100).contains(&a.delta.ok_or("scroll 缺少delta")?) {
        return Err("滚动刻度必须在-100至100之间".into());
    }
    Ok(())
}
fn check_target(snap: &Snapshot, shot: &Shot, a: &Action) -> Result<()> {
    if snap.taken.elapsed() > Duration::from_secs(180) {
        return Err("截图已过期".into());
    }
    if foreground()? != snap.foreground {
        return Err("前台程序已改变，请重新截图".into());
    }
    if let Some(id) = snap.window {
        // Reuse one Z-order enumeration for geometry and occlusion checks.
        let windows = Window::all().map_err(err)?;
        let w = windows.iter().find(|w| w.id().ok() == Some(id))
            .ok_or("程序窗口已关闭，请重新 windows")?;
        if !w.is_focused().map_err(err)?
            || w.is_minimized().map_err(err)?
            || window_surface(w)? != shot.surface
        {
            return Err("目标窗口失焦、移动或尺寸改变，请重新截图".into());
        }
        // Reject an overlapping higher window before coordinate input; window screenshots may include occluded content.
        if a.x.is_some() {
            let p = point(shot, a.x, a.y)?;
            let end = if a.action == "drag" {
                point(shot, a.to_x, a.to_y)?
            } else {
                p
            };
            for other in &windows {
                if other.id().map_err(err)? == id {
                    break;
                }
                if other.is_minimized().map_err(err)? {
                    continue;
                }
                let r = window_surface(other)?;
                if [p, end].iter().any(|&(x, y)| {
                    x as i64 >= r.x as i64
                        && y as i64 >= r.y as i64
                        && (x as i64) < r.x as i64 + r.width as i64
                        && (y as i64) < r.y as i64 + r.height as i64
                }) {
                    return Err("目标坐标被其它窗口遮挡，请使用桌面截图".into());
                }
            }
        }
    } else {
        let monitors = Monitor::all().map_err(err)?;
        let current = monitors
            .iter()
            .map(monitor_surface)
            .collect::<Result<Vec<_>>>()?;
        if snap.shots.iter().any(|s| !current.contains(&s.surface))
        {
            return Err("显示器布局已改变，请重新截图".into());
        }
    }
    Ok(())
}

fn action_delay(actions: &[Action], index: usize) -> Duration {
    // Explicit waits already provide settling time; pointer motion needs no extra delay.
    let redundant = matches!(actions[index].action.as_str(), "wait" | "move")
        || actions.get(index + 1).is_some_and(|a| a.action == "wait");
    Duration::from_millis(if redundant { 0 } else { 80 })
}

fn needs_stable_feedback(actions: &[Action], needs_review: bool) -> bool {
    // Pure positioning gets an immediate frame; hover-dependent UI must use move + wait.
    needs_review || actions.iter().any(|a| a.action != "move")
}
fn input(enigo: &mut Enigo, shot: &Shot, a: &Action) -> Result<()> {
    let button = match a.button.as_deref() {
        Some("right") => Button::Right,
        Some("middle") => Button::Middle,
        _ => Button::Left,
    };
    if matches!(
        a.action.as_str(),
        "click" | "double_click" | "move" | "drag" | "scroll"
    ) {
        let (x, y) = point(shot, a.x, a.y)?;
        enigo.move_mouse(x, y, Coordinate::Abs).map_err(err)?;
    }
    match a.action.as_str() {
        "click" => enigo.button(button, Direction::Click).map_err(err)?,
        "double_click" => {
            enigo.button(button, Direction::Click).map_err(err)?;
            std::thread::sleep(Duration::from_millis(70));
            enigo.button(button, Direction::Click).map_err(err)?;
        }
        "drag" => {
            let (x, y) = point(shot, a.x, a.y)?;
            let (tx, ty) = point(shot, a.to_x, a.to_y)?;
            enigo.button(button, Direction::Press).map_err(err)?;
            let moved = (1..=12).try_for_each(|i| {
                enigo
                    .move_mouse(
                        (x as i64 + (tx as i64 - x as i64) * i / 12) as i32,
                        (y as i64 + (ty as i64 - y as i64) * i / 12) as i32,
                        Coordinate::Abs,
                    )
                    .map_err(err)?;
                std::thread::sleep(Duration::from_millis(16));
                Ok::<_, String>(())
            });
            let released = enigo.button(button, Direction::Release).map_err(err);
            moved?;
            released?;
        }
        "type" => enigo.text(a.text.as_deref().unwrap()).map_err(err)?,
        "press" => {
            let keys = keys(a.key.as_deref().unwrap())?;
            let pressed = keys
                .iter()
                .try_for_each(|k| enigo.key(*k, Direction::Press).map_err(err));
            let mut release_error = None;
            for k in keys.iter().rev() {
                if let Err(e) = enigo.key(*k, Direction::Release) {
                    release_error = Some(err(e));
                }
            }
            pressed?;
            if let Some(e) = release_error {
                return Err(e);
            }
        }
        "scroll" => enigo
            .scroll(
                a.delta.unwrap(),
                if a.axis.as_deref() == Some("horizontal") {
                    Axis::Horizontal
                } else {
                    Axis::Vertical
                },
            )
            .map_err(err)?,
        "wait" => std::thread::sleep(Duration::from_millis(a.ms.unwrap_or(250))),
        "move" => (),
        _ => unreachable!(),
    }
    Ok(())
}

fn run(owner: String, args: Value) -> Result<Value> {
    let is_act = args["operation"] == "act";
    let operation = args["operation"].clone();
    let based_on = args["snapshotId"].clone();
    let notes = args.get("notes").cloned();
    let result = run_inner(owner, args);
    let mut result = match result {
        Err(e) if is_act => json!({"status":"not_executed","completedActions":0,
            "error":format!("{e}；本批次尚未执行")}),
        other => other?,
    };
    result["source"] = json!("jianlai");
    result["operation"] = operation;
    if is_act {
        result["basedOnSnapshotId"] = based_on;
        result["verification"] = json!("unverified");
    }
    if is_act || result.get("images").is_some() {
        result["verificationNotice"] = json!("executed仅表示输入已发送；stable仅表示取样期间画面近似稳定，不证明加载完成或任务成功。核对最新图中的目标、选中项/筛选条件与正文是否一致；不符或timeout时重新观察，不重放输入。最终结论绑定snapshotId及可见证据；未覆盖完整范围不能断言全部或不存在。notes为模型记录，未经工具验证。");
    }
    if let Some(notes) = notes.filter(|v| v.as_str().is_some_and(|s| s.chars().count() <= 12000)) {
        result["notes"] = notes;
    }
    Ok(result)
}

fn run_inner(owner: String, args: Value) -> Result<Value> {
    let is_act = args["operation"] == "act";
    let request: Request = match serde_json::from_value(args) {
        Ok(request) => request,
        Err(e) if is_act => return Ok(json!({"status":"not_executed","completedActions":0,
            "error":format!("参数无效：{e}；本批次尚未执行")})),
        Err(e) => return Err(err(e)),
    };
    let max_edge = request.max_edge.unwrap_or(1600);
    if request.region.is_some() && (request.operation != "screenshot" || request.window_id.is_none()) {
        return Err("region仅用于screenshot且必须指定windowId".into());
    }
    if request.monitor_id.is_some() && request.operation != "screenshot" {
        return Err("monitorId仅用于screenshot；act由imageId选择屏幕".into());
    }
    if max_edge != 0 && !(640..=3840).contains(&max_edge) {
        return Err("maxEdge允许0（原分辨率）或640–3840；本批次尚未执行".into());
    }
    if request.notes.as_ref().is_some_and(|s| s.chars().count() > 12000) {
        return Err("notes最多12000字符；本批次尚未执行".into());
    }
    if request
        .feedback
        .as_deref()
            .is_some_and(|s| !matches!(s, "screenshot" | "desktop" | "none"))
    {
        return Err("无效feedback".into());
    }
    let mut state = DESKTOP
        .try_lock()
        .map_err(|_| "剑来正在操作桌面，请勿并行调用")?;
    match request.operation.as_str() {
        "windows" => windows(),
        "screenshot" => {
            if request.window_id.is_some() && request.monitor_id.is_some() {
                return Err("windowId和monitorId不能同时用于截图".into());
            }
            let mut result = capture(&owner, request.window_id, request.monitor_id, request.region, max_edge, false, &mut state)?;
            result["notes"] = json!(request.notes);
            Ok(result)
        }
        "recall" => {
            let path = std::fs::canonicalize(request.image_path.ok_or("recall缺少imagePath")?).map_err(err)?;
            let folder = std::fs::canonicalize(shot_folder(&owner)).map_err(err)?;
            if !path.starts_with(folder) || path.extension().and_then(|s| s.to_str()) != Some("png") {
                return Err("只能回看本会话的桌面截图".into());
            }
            Ok(json!({"images":[{"path":path}],"historical":true,
                "notice":"历史截图仅供阅读，不产生可操作快照；操作前请使用当前截图"}))
        }
        "act" => {
            if cfg!(target_os = "linux") && std::env::var_os("WAYLAND_DISPLAY").is_some() {
                return Err(
                    "剑来 Linux 输入当前仅支持 X11；Wayland 不会静默使用 XWayland 操作".into(),
                );
            }
            if state.is_none() {
                let mut result = json!({"status":"not_executed","completedActions":0,"error":"没有可用快照，附当前观察；基于新图重新决策"});
                observe(&owner, request.window_id, None, request.feedback.as_deref() == Some("desktop"), max_edge, false, &mut state, &mut result);
                return Ok(result);
            }
            let snap = state.as_ref().unwrap();
            if snap.owner != owner {
                return Err("快照无效或已过期，请重新截图；不能重放动作".into());
            }
            if Some(&snap.id) != request.snapshot_id.as_ref() {
                let window_id = snap.window;
                let max_edge = snap.max_edge;
                let mut result = json!({"status":"not_executed","completedActions":0,
                    "error":"旧snapshotId已失效，附当前观察；请基于新图重新决策，不自动重放"});
                observe(&owner, window_id, None, request.feedback.as_deref() == Some("desktop"), max_edge, false, &mut state, &mut result);
                return Ok(result);
            }
            if snap.window.is_some() && request.window_id.is_some() && request.window_id != snap.window {
                return Err("windowId与截图不符".into());
            }
            let shot = snap
                .shots
                .iter()
                .find(|s| Some(&s.image_id) == request.image_id.as_ref())
                .cloned()
                .ok_or("imageId不属于此截图")?;
            let actions = request.actions.ok_or("缺少actions")?;
            if actions.is_empty() || actions.len() > 8 {
                return Err("每次需要1至8个动作".into());
            }
            for (index, a) in actions.iter().enumerate() {
                if let Err(e) = validate(a, &shot) {
                    return Ok(json!({"status":"not_executed","completedActions":0,
                        "error":format!("actions[{index}].{e}；本批次尚未执行")}));
                }
            }
            let max_edge = request.max_edge.unwrap_or(snap.max_edge);
            let feedback_window = request.window_id.or(snap.window);
            let monitor_id = snap.window.is_none().then_some(shot.surface.id);
            if let Err(e) = check_target(snap, &shot, &actions[0]) {
                let mut result = json!({"status":"not_executed","completedActions":0,"error":e});
                observe(&owner, feedback_window, monitor_id, request.feedback.as_deref() == Some("desktop"), max_edge, false, &mut state, &mut result);
                return Ok(result);
            }
            let mut enigo = Enigo::new(&Settings::default()).map_err(err)?;
            // Consume before the first OS event; all later failures are explicitly non-retryable.
            let mut snap = state.take().unwrap();
            let mut completed = 0;
            let mut failure = None;
            let mut attempted = false;
            for (index, a) in actions.iter().enumerate() {
                if a.action != "wait" {
                    if let Err(e) = check_target(&snap, &shot, a) {
                        failure = Some(e);
                        break;
                    }
                }
                attempted = true;
                if let Err(e) = input(&mut enigo, &shot, a) {
                    failure = Some(e);
                    break;
                }
                completed += 1;
                std::thread::sleep(action_delay(&actions, index));
                // Let an explicit wait finish before observing a click/key's focus transition.
                if actions.get(index + 1).is_some_and(|a| a.action == "wait") { continue; }
                match foreground() {
                    Ok(f) => {
                        if f != snap.foreground && actions[index + 1..].iter().any(|a| a.action != "wait") {
                            failure = Some("动作后前台窗口改变，已停止后续输入；请根据新截图继续，不要重放整批".into());
                            break;
                        }
                        snap.foreground = f;
                    }
                    Err(e) => {
                        failure = Some(e);
                        break;
                    }
                }
            }
            let mut result = json!({"status":if failure.is_some(){if attempted {"needs_review"} else {"not_executed"}}else{"executed"},"completedActions":completed,"error":failure,"notes":request.notes});
            if shot.region.is_some() {
                result["coordinateNotice"] = json!("局部操作后的反馈恢复完整窗口；使用新的imageId和完整图片坐标，不沿用局部坐标");
            }
            if failure.is_some() || request.feedback.as_deref() != Some("none") {
                let settle = needs_stable_feedback(&actions[..completed], result["status"] == "needs_review");
                observe(&owner, feedback_window, monitor_id, request.feedback.as_deref() == Some("desktop"), max_edge, settle, &mut state, &mut result);
            }
            Ok(result)
        }
        _ => Err("未知剑来操作".into()),
    }
}
// Observation never retries input; follow the foreground and fall back to desktop if needed.
fn observe(owner: &str, previous_window: Option<u32>, monitor_id: Option<u32>, desktop: bool, max_edge: u32, settle: bool, state: &mut Option<Snapshot>, result: &mut Value) {
    // Follow the actual foreground after opening a compose window/dialog, never activate it.
    // Shell task view and hidden helper windows must stay in desktop scope.
    let focused = foreground().ok().flatten().and_then(|(id, _)| window(id).ok())
        .filter(|w| w.width().unwrap_or(0) > 32 && w.height().unwrap_or(0) > 32
            && !w.is_minimized().unwrap_or(true));
    let monitor_id = monitor_id.or_else(|| focused.as_ref()?.current_monitor().ok()?.id().ok());
    let window_id = if desktop { None } else { focused.as_ref().and_then(|w| w.id().ok()) };
    if window_id != previous_window {
        result["observationScopeChanged"] = json!(true);
        result["coordinateNotice"] = json!("截图范围已变化，必须使用本次imageId和图片内坐标，不沿用上一张图坐标");
    }
    let observation = capture(owner, window_id, if window_id.is_some() { None } else { monitor_id }, None, max_edge, settle, state).or_else(|e| {
        if window_id.is_none() && monitor_id.is_none() { return Err(e); }
        result["windowObservationError"] = json!(e);
        capture(owner, None, monitor_id, None, max_edge, settle, state)
    });
    match observation {
        Ok(value) => result.as_object_mut().unwrap().extend(value.as_object().unwrap().clone()),
        Err(e) => result["observationError"] = json!(e),
    }
}

pub(crate) async fn execute(root: &Path, args: &Value, owner: &str) -> Result<Value> {
    let owner = crate::native_browser::tool_owner(root, owner)?;
    let args = args.clone();
    tokio::task::spawn_blocking(move || {
        if crate::tool_experience::is_operation(&args) {
            let observed = if args["operation"] == "experience_search" { None } else {
                let state = DESKTOP.try_lock().map_err(|_| "剑来正在操作桌面")?;
                let snap = state.as_ref().filter(|s| s.owner == owner && args["snapshotId"].as_str() == Some(&s.id)
                    && s.taken.elapsed() <= Duration::from_secs(180)).ok_or("保存/反馈经验前需本会话最新截图（180秒内）")?;
                let id = snap.window.or(snap.foreground.map(|(id, _)| id)).ok_or("请截目标应用窗口后记录经验")?;
                Some(crate::tool_experience::scope("jianlai", &window(id)?.app_name().map_err(err)?)?)
            };
            return crate::tool_experience::execute(&crate::lyra::config::nova_root().join("tool-experiences"), "jianlai", &owner, &args, observed.as_deref());
        }
        run(owner, args)
    })
        .await
        .map_err(err)?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn observation_waits_for_delayed_content_and_bounds_animation() {
        use xcap::image::{Rgba, RgbaImage};
        let old = RgbaImage::from_pixel(100, 100, Rgba([255, 255, 255, 255]));
        let mut caret = old.clone();
        for y in 0..5 { caret.put_pixel(0, y, Rgba([0, 0, 0, 255])); }
        assert!(visually_similar(&old, &caret));
        caret.put_pixel(0, 5, Rgba([0, 0, 0, 255]));
        assert!(!visually_similar(&old, &caret));
        assert!(!visually_similar(&old, &RgbaImage::new(101, 100)));

        // Header changes immediately; list remains old for the first two samples.
        let new = RgbaImage::from_pixel(100, 100, Rgba([0, 0, 0, 255]));
        let mut calls = 0;
        let (frame, report) = settled_image(|| {
            calls += 1;
            Ok(if calls < 3 { old.clone() } else { new.clone() })
        }, true).unwrap();
        assert_eq!(frame, new);
        assert_eq!(report["status"], "stable");
        assert!(calls >= 6);

        let mut last = old.clone();
        let (_, report) = settled_image(|| {
            last = if last == old { new.clone() } else { old.clone() };
            Ok(last.clone())
        }, true).unwrap();
        assert_eq!(report["status"], "timeout");
        assert!(report["elapsedMs"].as_u64().unwrap() >= 2000);
        let (frame, report) = settled_image(|| Ok(old.clone()), false).unwrap();
        assert_eq!(frame, old);
        assert_eq!(report["samples"], 1);
        assert_eq!(report["status"], "not_checked");
        assert!(settled_image(|| Err("capture failed".into()), true).is_err());
        let result = run("validation".into(), json!({"operation":"act", "snapshotId":"old", "unexpected":true})).unwrap();
        assert_eq!(result["operation"], "act");
        assert_eq!(result["basedOnSnapshotId"], "old");
        assert_eq!(result["verification"], "unverified");
    }
    #[test]
    fn cursor_marker_tracks_actual_position_in_scaled_regions() {
        let surface = Surface { id:1, pid:None, x:-1920, y:-100, width:1920, height:1080 };
        let region = Region { x:200, y:100, width:800, height:400 };
        let original = xcap::image::RgbaImage::from_pixel(400, 200, xcap::image::Rgba([255,255,255,255]));
        let mut image = original.clone();
        assert_eq!(mark_cursor(&mut image, &surface, (3840,2160), Some(region), (-1620,50)), Some((200,100)));
        assert_eq!(image.get_pixel(200,100), original.get_pixel(200,100));
        assert_eq!(image.get_pixel(210,100).0, [255,0,180,255]);
        assert_eq!(mark_cursor(&mut image, &surface, (3840,2160), Some(region), (-1820,-50)), Some((0,0)));
        let unchanged = image.clone();
        assert_eq!(mark_cursor(&mut image, &surface, (3840,2160), Some(region), (-1821,-50)), None);
        assert_eq!(mark_cursor(&mut image, &surface, (3840,2160), Some(region), (-1420,-50)), None);
        assert_eq!(image, unchanged);
    }
    // Deterministic screenshot pipeline benchmark; no desktop input or live capture.
    #[test]
    #[ignore]
    fn screenshot_pipeline_benchmark() {
        let source = xcap::image::RgbaImage::from_fn(1920, 1080, |x, y| {
            let v = if (x / 120 + y / 24) % 2 == 0 { 245 } else { 45 };
            xcap::image::Rgba([v, v, v, 255])
        });
        let expected = xcap::image::imageops::resize(&source, 1600, 900, xcap::image::imageops::FilterType::Triangle);
        for _ in 0..3 {
            let started = Instant::now();
            let resized = xcap::image::DynamicImage::ImageRgba8(source.clone())
                .resize_exact(1600, 900, xcap::image::imageops::FilterType::Triangle).into_rgba8();
            let resize_ms = started.elapsed().as_millis();
            assert_eq!(resized, expected);
            let mut encoded = std::io::Cursor::new(Vec::new());
            let started = Instant::now();
            resized.write_to(&mut encoded, xcap::image::ImageFormat::Png).unwrap();
            let encode_ms = started.elapsed().as_millis();
            assert_eq!(xcap::image::load_from_memory(encoded.get_ref()).unwrap().to_rgba8(), resized);
            eprintln!("screenshot pipeline: resize={resize_ms}ms encode={encode_ms}ms bytes={}", encoded.get_ref().len());
        }
    }
    #[cfg(windows)]
    #[test]
    fn windows_shortcuts_and_visible_crop() {
        for letter in b'a'..=b'z' {
            let lower = keys(&format!("Ctrl+{}", letter as char)).unwrap();
            let upper = keys(&format!("Ctrl+{}", letter.to_ascii_uppercase() as char)).unwrap();
            assert_eq!(lower, upper);
            assert_eq!(upper, vec![Key::Control, Key::Other(letter.to_ascii_uppercase() as u32)]);
        }
        assert_eq!(keys("Ctrl+Shift+S").unwrap(), vec![Key::Control, Key::Shift, Key::Other(0x53)]);
        assert_eq!(keys("Win+1").unwrap(), vec![Key::Meta, Key::Other(0x31)]);
        let monitor = Surface { id:1, pid:None, x:-100, y:20, width:100, height:80 };
        let window = Surface { x:-80, y:30, width:40, height:30, ..monitor.clone() };
        let image = xcap::image::RgbaImage::from_fn(100, 80, |x, y| xcap::image::Rgba([x as u8, y as u8, 0, 255]));
        let crop = visible_window_crop(&window, &monitor, &image).unwrap();
        assert_eq!(crop.dimensions(), (40,30));
        assert_eq!(crop.get_pixel(0,0), image.get_pixel(20,10));
        assert_eq!(crop.get_pixel(39,29), image.get_pixel(59,39));
        assert!(visible_window_crop(&Surface { x:-101, ..window.clone() }, &monitor, &image).is_err());
        assert!(visible_window_crop(&Surface { x:-10, ..window }, &monitor, &image).is_err());
        assert!(visible_window_crop(&monitor, &monitor, &crop).is_err());
    }
    #[test]
    fn waits_are_not_paid_twice() {
        let actions: Vec<Action> = serde_json::from_value(json!([
            {"action":"click","x":10,"y":10}, {"action":"wait","ms":400},
            {"action":"type","text":"test"}, {"action":"wait","ms":500},
            {"action":"move","x":20,"y":20}, {"action":"click","x":20,"y":20},
            {"action":"type","text":"next"}
        ])).unwrap();
        let delays: Vec<_> = (0..actions.len()).map(|i| action_delay(&actions, i).as_millis()).collect();
        assert_eq!(delays, [0, 0, 0, 0, 0, 80, 80]);
        assert!(!needs_stable_feedback(&[], false));
        assert!(!needs_stable_feedback(&actions[4..5], false));
        assert!(needs_stable_feedback(&actions[4..5], true));
        assert!(needs_stable_feedback(&actions[4..6], false));
        assert!(needs_stable_feedback(&actions[3..5], false));
        for action in ["click", "double_click", "drag", "type", "press", "scroll", "wait"] {
            let batch: Vec<Action> = serde_json::from_value(json!([{ "action": action }])).unwrap();
            assert!(needs_stable_feedback(&batch, false), "{action}");
        }
        assert!(run("validation".into(), json!({"operation":"screenshot","windowId":1,"monitorId":2})).is_err());
    }
    #[test]
    fn coordinates_keys_and_validation() {
        let shot = Shot {
            image_id: "test".into(),
            surface: Surface {
                id: 1,
                pid: None,
                x: -1920,
                y: 0,
                width: 1920,
                height: 1080,
            },
            pixels: (3840, 2160),
            source_pixels: (3840, 2160), region: None,
        };
        assert_eq!(point(&shot, Some(1920), Some(1080)).unwrap(), (-960, 540));
        assert!(point(&shot, Some(3840), Some(0)).is_err());
        assert!(point(&shot, Some(-1), Some(0)).is_err());
        assert!(point(&shot, None, Some(0)).is_err());
        let scaled = Shot { pixels: (960, 540), ..shot.clone() };
        assert_eq!(point(&scaled, Some(480), Some(270)).unwrap(), (-960, 540));
        assert_eq!(point(&scaled, Some(959), Some(539)).unwrap(), (-2, 1078));
        let desktop = Shot { surface: Surface { x:0, y:0, ..shot.surface.clone() }, pixels:(1600,900), ..shot.clone() };
        // The reported click was on the toolbar in the source image, not a DPI offset.
        assert_eq!(point(&desktop, Some(860), Some(520)).unwrap(), (1032, 624));
        let region = Region { x:200, y:100, width:800, height:400 };
        region.validate(shot.source_pixels).unwrap();
        let cropped = Shot { region:Some(region), pixels:(400,200), ..shot.clone() };
        assert_eq!(point(&cropped, Some(0), Some(0)).unwrap(), (-1820,50));
        assert_eq!(point(&cropped, Some(200), Some(100)).unwrap(), (-1620,150));
        assert!(point(&cropped, Some(400), Some(0)).is_err());
        assert_eq!(cropped.surface, shot.surface); // Geometry checks still cover the whole window.
        assert!(Region { x:u32::MAX, ..region }.validate(shot.source_pixels).is_err());
        assert!(Region { width:0, ..region }.validate(shot.source_pixels).is_err());
        assert!(Region { y:2100, ..region }.validate(shot.source_pixels).is_err());
        let wait: Action = serde_json::from_value(json!({"action":"wait","ms":8000})).unwrap();
        assert_eq!(validate(&wait, &shot).unwrap_err(), "ms=8000，允许范围为 0–2000");
        assert!(validate(&serde_json::from_value(json!({"action":"wait","ms":2000})).unwrap(), &shot).is_ok());
        assert_eq!(keys("Ctrl+Shift+S").unwrap().len(), 3);
        assert!(keys("Ctrl+unknown").is_err());
        assert!(keys("Ctrl+Shift+Alt+Meta+S").is_err());
        for value in [
            json!({"action":"scroll","x":1,"y":1,"delta":101}),
            json!({"action":"wait","ms":2001}),
            json!({"action":"drag","x":1,"y":1}),
        ] {
            assert!(validate(&serde_json::from_value(value).unwrap(), &shot).is_err());
        }
    }
    #[test]
    fn invalid_wait_rejects_whole_batch_before_input() {
        let shot = Shot { image_id:"monitor-1".into(),
            surface:Surface {id:1,pid:None,x:0,y:0,width:100,height:100}, pixels:(100,100), source_pixels:(100,100), region:None };
        *DESKTOP.lock().unwrap() = Some(Snapshot {id:"validation".into(),owner:"validation".into(),
            taken:Instant::now(),window:None,max_edge:1600,foreground:None,shots:vec![shot]});
        let foreign = run("other-owner".into(), json!({"operation":"act","snapshotId":"old"})).unwrap();
        assert_eq!(foreign["completedActions"], 0);
        assert!(foreign.get("images").is_none());
        assert_eq!(DESKTOP.lock().unwrap().as_ref().unwrap().id, "validation");
        let result = run("validation".into(), json!({"operation":"act","snapshotId":"validation",
            "imageId":"monitor-1","actions":[{"action":"click","x":1,"y":1},{"action":"wait","ms":8000}]})).unwrap();
        assert_eq!(result["status"], "not_executed");
        assert_eq!(result["completedActions"], 0);
        assert!(result["error"].as_str().unwrap().contains("actions[1].ms=8000"));
        assert!(DESKTOP.lock().unwrap().take().is_some());
        let invalid = run("validation".into(), json!({"operation":"act","actions":[{"action":"wait","ms":-1}]})).unwrap();
        assert_eq!(invalid["status"], "not_executed");
    }

    // Opt-in real desktop smoke test: captures and moves only the pointer; never clicks/types into user applications.
    #[test]
    #[ignore]
    fn desktop_smoke() {
        #[cfg(windows)]
        {
            let before = foreground().unwrap();
            let expected = Window::all().unwrap().into_iter()
                .find(|w| w.is_focused().unwrap_or(false))
                .map(|w| (w.id().unwrap(), w.pid().unwrap()));
            let after = foreground().unwrap();
            // Shell surfaces may be focused but excluded by XCap's application-window filter.
            if expected.is_some() && before == after { assert_eq!(after, expected); }
        }
        // A departed target must yield an actionable desktop, not its old window image.
        let mut observation = json!({});
        let mut snapshot = None;
        observe("test", Some(u32::MAX), None, true, 1600, false, &mut snapshot, &mut observation);
        assert!(observation["windowId"].is_null());
        assert!(snapshot.is_some(), "{observation}");
        assert!(snapshot.unwrap().window.is_none());
        let mut focused_result = json!({});
        let mut focused_snapshot = None;
        observe("test", None, None, false, 1600, false, &mut focused_snapshot, &mut focused_result);
        assert!(focused_snapshot.is_some(), "{focused_result}");
        if let Some(id) = focused_result["windowId"].as_u64() {
            assert_eq!(focused_result["foreground"][0], id);
            let cropped = run("test".into(), json!({"operation":"screenshot","windowId":id,
                "maxEdge":0,"region":{"x":0,"y":0,"width":32,"height":32}})).unwrap();
            assert_eq!(cropped["images"][0]["width"], 32);
            assert_eq!(cropped["images"][0]["height"], 32);
            let _ = std::fs::remove_file(cropped["images"][0]["path"].as_str().unwrap());
        }
        let shot = run("test".into(), json!({"operation":"screenshot"})).unwrap();
        assert!(!shot["images"].as_array().unwrap().is_empty());
        let args = json!({"operation":"act","snapshotId":shot["snapshotId"],"imageId":shot["images"][0]["imageId"],"feedback":"desktop","actions":[{"action":"move","x":10,"y":10}]});
        let result = run("test".into(), args.clone()).unwrap();
        assert_eq!(result["status"], "executed");
        assert_eq!(result["images"].as_array().unwrap().len(), 1);
        assert_eq!(result["images"][0]["imageId"], shot["images"][0]["imageId"]);
        #[cfg(windows)]
        {
            let cursor = &result["images"][0]["cursor"];
            assert!((cursor["x"].as_i64().expect("stationary system cursor") - 10).abs() <= 1);
            assert!((cursor["y"].as_i64().unwrap() - 10).abs() <= 1);
        }
        eprintln!("single-screen feedback timings: {}", result["timingsMs"]);
        let stale = run("test".into(), args).unwrap();
        assert_eq!(stale["status"], "not_executed");
        assert_eq!(stale["completedActions"], 0);
        assert!(stale["images"].as_array().is_some());
        assert_ne!(stale["snapshotId"], result["snapshotId"]);
        // Simulate a stale foreground identity without actually changing the user's focus.
        DESKTOP.lock().unwrap().as_mut().unwrap().foreground = Some((u32::MAX, u32::MAX));
        let recovered = run("test".into(), json!({"operation":"act","snapshotId":result["snapshotId"],
            "imageId":result["images"][0]["imageId"],"feedback":"none",
            "actions":[{"action":"move","x":10,"y":10}]})).unwrap();
        assert_eq!(recovered["status"], "not_executed");
        assert_eq!(recovered["completedActions"], 0);
        assert_ne!(recovered["snapshotId"], result["snapshotId"]);
        assert!(!recovered["images"].as_array().unwrap().is_empty());
        if let Ok(id) = std::env::var("NOVA_JIANLAI_TEST_WINDOW") {
            // Only target an explicitly supplied disposable test window; never type into an arbitrary desktop app.
            let id: u32 = id.parse().unwrap();
            let frame = run(
                "test".into(),
                json!({"operation":"screenshot","windowId":id}),
            )
            .unwrap();
            let input = run(
                "test".into(),
                json!({"operation":"act","snapshotId":frame["snapshotId"],
                "imageId":frame["images"][0]["imageId"],"actions":[{"action":"click","x":40,"y":40},
                    {"action":"type","text":"jianlai"},{"action":"press","key":"Enter"}]}),
            )
            .unwrap();
            assert_eq!(input["status"], "executed", "{input}");
            assert_eq!(input["completedActions"], 3);
            assert!(input["images"].as_array().is_some());
            for result in [frame, input] {
                for img in result["images"].as_array().unwrap() {
                    let _ = std::fs::remove_file(img["path"].as_str().unwrap());
                }
            }
        }
        for result in [observation, focused_result, shot, result, stale, recovered] {
            for img in result["images"].as_array().unwrap() {
                let _ = std::fs::remove_file(img["path"].as_str().unwrap());
            }
        }
    }
}
