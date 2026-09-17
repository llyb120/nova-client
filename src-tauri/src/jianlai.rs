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

fn capture(owner: &str, window_id: Option<u32>, max_edge: u32, state: &mut Option<Snapshot>) -> Result<Value> {
    let started = Instant::now();
    let mut capture_ms = 0;
    let mut resize_ms = 0;
    let mut encode_ms = 0;
    *state = None;
    let id = uuid::Uuid::new_v4().to_string();
    let folder = shot_folder(owner);
    std::fs::create_dir_all(&folder).map_err(err)?;
    let before = foreground()?;
    let mut shots = Vec::new();
    let mut images = Vec::new();
    let mut save = |surface: Surface, image: xcap::image::RgbaImage| -> Result<()> {
        if surface.width == 0 || surface.height == 0 || image.width() == 0 || image.height() == 0 {
            return Err("截图范围为空".into());
        }
        let image_id = format!(
            "{}-{}",
            if window_id.is_some() {
                "window"
            } else {
                "monitor"
            },
            surface.id
        );
        let original_pixels = image.dimensions();
        let resize_started = Instant::now();
        let image = if max_edge > 0 && image.width().max(image.height()) > max_edge {
            let scale = max_edge as f64 / image.width().max(image.height()) as f64;
            xcap::image::imageops::resize(&image,
                (image.width() as f64 * scale).round().max(1.0) as u32,
                (image.height() as f64 * scale).round().max(1.0) as u32,
                xcap::image::imageops::FilterType::Triangle)
        } else { image };
        resize_ms += resize_started.elapsed().as_millis();
        let path = folder.join(format!("{id}-{image_id}.png"));
        let encode_started = Instant::now();
        image.save(&path).map_err(err)?;
        encode_ms += encode_started.elapsed().as_millis();
        images.push(json!({"imageId":image_id,"path":path,"width":image.width(),"height":image.height(),
            "originalWidth":original_pixels.0,"originalHeight":original_pixels.1,
            "desktopBounds":{"x":surface.x,"y":surface.y,"width":surface.width,"height":surface.height}}));
        shots.push(Shot {
            image_id,
            surface,
            pixels: image.dimensions(),
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
        let image = w.capture_image().map_err(err)?;
        capture_ms += capture_started.elapsed().as_millis();
        if window_surface(&w)? != surface {
            return Err("截图期间窗口移动，请重新截图".into());
        }
        save(surface, image)?;
    } else {
        let monitors = Monitor::all().map_err(err)?;
        if monitors.is_empty() || monitors.len() > 16 {
            return Err("需要1至16个可截图显示器".into());
        }
        for m in monitors {
            let surface = monitor_surface(&m)?;
            let capture_started = Instant::now();
            let image = m.capture_image().map_err(err)?;
            capture_ms += capture_started.elapsed().as_millis();
            save(surface, image)?;
        }
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
        json!({"snapshotId":id,"windowId":window_id,"images":images,"coordinateSpace":"image-pixels","expiresInMs":180000,
            "foreground":before,"maxEdge":max_edge,
            "timingsMs":{"capture":capture_ms,"resize":resize_ms,"encodeAndSave":encode_ms,
                "other":started.elapsed().as_millis().saturating_sub(capture_ms + resize_ms + encode_ms),
                "total":started.elapsed().as_millis()}}),
    )
}

fn point(shot: &Shot, x: Option<i32>, y: Option<i32>) -> Result<(i32, i32)> {
    let (x, y) = (x.ok_or("缺少x坐标")?, y.ok_or("缺少y坐标")?);
    let (pw, ph) = shot.pixels;
    if x < 0 || y < 0 || x as u32 >= pw || y as u32 >= ph || pw == 0 || ph == 0 {
        return Err("坐标超出原始截图范围".into());
    }
    let s = &shot.surface;
    let px = s.x as i64 + x as i64 * s.width as i64 / pw as i64;
    let py = s.y as i64 + y as i64 * s.height as i64 / ph as i64;
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
        let w = window(id)?;
        if !w.is_focused().map_err(err)?
            || w.is_minimized().map_err(err)?
            || window_surface(&w)? != shot.surface
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
            for other in Window::all().map_err(err)? {
                if other.id().map_err(err)? == id {
                    break;
                }
                if other.is_minimized().map_err(err)? {
                    continue;
                }
                let r = window_surface(&other)?;
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
        if current.len() != snap.shots.len()
            || snap.shots.iter().any(|s| !current.contains(&s.surface))
        {
            return Err("显示器布局已改变，请重新截图".into());
        }
    }
    Ok(())
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
    let notes = args.get("notes").cloned();
    let result = run_inner(owner, args);
    let mut result = match result {
        Err(e) if is_act => json!({"status":"not_executed","completedActions":0,
            "error":format!("{e}；本批次尚未执行")}),
        other => other?,
    };
    result["source"] = json!("jianlai");
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
    if max_edge != 0 && !(640..=3840).contains(&max_edge) {
        return Err("maxEdge允许0（原分辨率）或640–3840；本批次尚未执行".into());
    }
    if request.notes.as_ref().is_some_and(|s| s.chars().count() > 12000) {
        return Err("notes最多12000字符；本批次尚未执行".into());
    }
    if request
        .feedback
        .as_deref()
        .is_some_and(|s| !matches!(s, "screenshot" | "none"))
    {
        return Err("无效feedback".into());
    }
    let mut state = DESKTOP
        .try_lock()
        .map_err(|_| "剑来正在操作桌面，请勿并行调用")?;
    match request.operation.as_str() {
        "windows" => windows(),
        "screenshot" => {
            let mut result = capture(&owner, request.window_id, max_edge, &mut state)?;
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
            let snap = state.as_ref().ok_or("请先截图")?;
            if snap.owner != owner
                || Some(&snap.id) != request.snapshot_id.as_ref()
            {
                return Err("快照无效或已过期，请重新截图；不能重放动作".into());
            }
            if request.window_id.is_some() && request.window_id != snap.window {
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
            if let Err(e) = check_target(snap, &shot, &actions[0]) {
                let window_id = snap.window;
                let mut result = json!({"status":"not_executed","completedActions":0,"error":e});
                observe(&owner, window_id, max_edge, &mut state, &mut result);
                return Ok(result);
            }
            let mut enigo = Enigo::new(&Settings::default()).map_err(err)?;
            // Consume before the first OS event; all later failures are explicitly non-retryable.
            let mut snap = state.take().unwrap();
            let mut completed = 0;
            let mut failure = None;
            let mut attempted = false;
            for a in &actions {
                if let Err(e) = check_target(&snap, &shot, a) {
                    failure = Some(e);
                    break;
                }
                attempted = true;
                if let Err(e) = input(&mut enigo, &shot, a) {
                    failure = Some(e);
                    break;
                }
                completed += 1;
                std::thread::sleep(Duration::from_millis(80));
                match foreground() {
                    Ok(f) => snap.foreground = f,
                    Err(e) => {
                        failure = Some(e);
                        break;
                    }
                }
            }
            let mut result = json!({"status":if failure.is_some(){if attempted {"needs_review"} else {"not_executed"}}else{"executed"},"completedActions":completed,"error":failure,"notes":request.notes});
            if failure.is_some() || request.feedback.as_deref() != Some("none") {
                observe(&owner, snap.window, max_edge, &mut state, &mut result);
            }
            Ok(result)
        }
        _ => Err("未知剑来操作".into()),
    }
}
// Observation never retries input. An unfocused/closed/minimized window falls back to the desktop.
fn observe(owner: &str, window_id: Option<u32>, max_edge: u32, state: &mut Option<Snapshot>, result: &mut Value) {
    // A window capture can show an occluded app. After a switch, observe the actual
    // desktop instead of repeatedly returning a background image that cannot receive input.
    let window_id = match window_id {
        Some(id) if !foreground().ok().flatten().is_some_and(|(focused, _)| focused == id) => {
            result["windowObservationError"] = json!("目标窗口不在前台，改用桌面截图");
            None
        }
        id => id,
    };
    let observation = capture(owner, window_id, max_edge, state).or_else(|e| {
        if window_id.is_none() { return Err(e); }
        result["windowObservationError"] = json!(e);
        capture(owner, None, max_edge, state)
    });
    match observation {
        Ok(value) => result.as_object_mut().unwrap().extend(value.as_object().unwrap().clone()),
        Err(e) => result["observationError"] = json!(e),
    }
}

pub(crate) async fn execute(root: &Path, args: &Value) -> Result<Value> {
    let (_, owner) = crate::native_browser::current_context(root)?;
    let args = args.clone();
    tokio::task::spawn_blocking(move || run(owner, args))
        .await
        .map_err(err)?
}

#[cfg(test)]
mod tests {
    use super::*;
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
        };
        assert_eq!(point(&shot, Some(1920), Some(1080)).unwrap(), (-960, 540));
        assert!(point(&shot, Some(3840), Some(0)).is_err());
        assert!(point(&shot, Some(-1), Some(0)).is_err());
        assert!(point(&shot, None, Some(0)).is_err());
        let scaled = Shot { pixels: (960, 540), ..shot.clone() };
        assert_eq!(point(&scaled, Some(480), Some(270)).unwrap(), (-960, 540));
        assert_eq!(point(&scaled, Some(959), Some(539)).unwrap(), (-2, 1078));
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
            surface:Surface {id:1,pid:None,x:0,y:0,width:100,height:100}, pixels:(100,100) };
        *DESKTOP.lock().unwrap() = Some(Snapshot {id:"validation".into(),owner:"validation".into(),
            taken:Instant::now(),window:None,max_edge:1600,foreground:None,shots:vec![shot]});
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
            let expected = Window::all().unwrap().into_iter()
                .find(|w| w.is_focused().unwrap_or(false))
                .map(|w| (w.id().unwrap(), w.pid().unwrap()));
            assert_eq!(foreground().unwrap(), expected);
        }
        // A departed target must yield an actionable desktop, not its old window image.
        let mut observation = json!({});
        let mut snapshot = None;
        observe("test", Some(u32::MAX), 1600, &mut snapshot, &mut observation);
        assert!(observation["windowId"].is_null());
        assert!(snapshot.is_some(), "{observation}");
        assert!(snapshot.unwrap().window.is_none());
        let shot = run("test".into(), json!({"operation":"screenshot"})).unwrap();
        assert!(!shot["images"].as_array().unwrap().is_empty());
        let args = json!({"operation":"act","snapshotId":shot["snapshotId"],"imageId":shot["images"][0]["imageId"],"actions":[{"action":"move","x":10,"y":10}]});
        let result = run("test".into(), args.clone()).unwrap();
        assert_eq!(result["status"], "executed");
        assert!(!result["images"].as_array().unwrap().is_empty());
        assert_eq!(run("test".into(), args).unwrap()["status"], "not_executed");
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
        for result in [observation, shot, result, recovered] {
            for img in result["images"].as_array().unwrap() {
                let _ = std::fs::remove_file(img["path"].as_str().unwrap());
            }
        }
    }
}
