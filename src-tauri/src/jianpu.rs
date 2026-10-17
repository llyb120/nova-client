//! 剑谱：录下用户亲手演示的桌面操作（点击截图、滚动、按键、打字原文），交给 AI 整理成剑来知识图谱路径。
//! 录制只监听不拦截：低级钩子线程只转发事件，截图与归并在 worker 线程；剑来注入的输入（INJECTED 标记）
//! 和落在 Nova 自己窗口上的操作不录。录制文件保留在 `jianpu/<id>/`，供查看、重新学习或手动删除。
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    sync::{mpsc, Mutex, OnceLock},
    time::{Duration, Instant},
};
use tauri::{AppHandle, Emitter, Manager};

mod steps;
use steps::{drop_trailing_shortcut, fold, Ctx, Ev, Frame, Step};

type Result<T> = std::result::Result<T, String>;

const MAX_STEPS: usize = 200;
const MAX_TIME: Duration = Duration::from_secs(600);
pub(crate) const EV_CHANGED: &str = "jianpu:changed";

static APP: OnceLock<AppHandle> = OnceLock::new();
static ACTIVE: Mutex<Option<Active>> = Mutex::new(None);

struct Active {
    id: String,
    started: Instant,
    tx: mpsc::Sender<Msg>,
    worker: std::thread::JoinHandle<Vec<Ev>>,
}

/// 钩子线程 → worker。
#[derive(Debug)]
pub(crate) enum Raw {
    Down { button: u8, x: i32, y: i32 },
    Up { button: u8, x: i32, y: i32 },
    Wheel { delta: i32, horizontal: bool },
    Key { name: String, text: Option<String>, ctrl: bool, alt: bool, shift: bool, win: bool },
}
pub(crate) enum Msg { Raw(Raw, i64), Stop }

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Recording {
    pub id: String,
    pub started_at: i64,
    pub ended_at: i64,
    pub steps: Vec<Step>,
    #[serde(default)] pub learn_thread: Option<String>,
    /// 由这段剑谱保存的图谱路径 id。
    #[serde(default)] pub routes: Vec<String>,
}

fn root() -> PathBuf { crate::lyra::config::nova_root().join("jianpu") }

/// id 只允许本模块生成的字符，防止经由命令参数访问目录外文件。
fn dir_of(id: &str) -> Result<PathBuf> {
    if id.is_empty() || id.len() > 64 || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') { return Err("剑谱 id 无效".into()); }
    Ok(root().join(id))
}

fn load(id: &str) -> Result<Recording> {
    let raw = std::fs::read(dir_of(id)?.join("recording.json")).map_err(|e| format!("读取剑谱失败：{e}"))?;
    serde_json::from_slice(&raw).map_err(|e| format!("剑谱文件损坏：{e}"))
}

fn save(rec: &Recording) -> Result<()> {
    let dir = dir_of(&rec.id)?;
    let temp = dir.join("recording.json.tmp");
    std::fs::write(&temp, serde_json::to_vec_pretty(rec).map_err(|e| e.to_string())?).map_err(|e| format!("保存剑谱失败：{e}"))?;
    std::fs::rename(&temp, dir.join("recording.json")).map_err(|e| format!("保存剑谱失败：{e}"))
}

fn changed() {
    if let Some(app) = APP.get() { let _ = app.emit(EV_CHANGED, status()); }
}

pub(crate) fn init(app: &AppHandle) { let _ = APP.set(app.clone()); }

pub(crate) fn recording() -> bool { ACTIVE.lock().unwrap().is_some() }

pub(crate) fn status() -> Value {
    match ACTIVE.lock().unwrap().as_ref() {
        Some(a) => json!({"recording":true,"id":a.id,"elapsedMs":a.started.elapsed().as_millis() as u64}),
        None => json!({"recording":false}),
    }
}

pub(crate) fn toggle() -> Result<Value> {
    if recording() { stop().map(|rec| json!({"recording":false,"id":rec.id,"steps":rec.steps.len()})) } else { start() }
}

fn start() -> Result<Value> {
    let mut active = ACTIVE.lock().unwrap();
    if active.is_some() { return Err("剑谱正在录制".into()); }
    if crate::jianlai::input_activity().1 { return Err("剑来正在操作桌面，等它结束后再录制剑谱".into()); }
    let id = format!("{}-{}", chrono::Local::now().format("%Y%m%d-%H%M%S"), &uuid::Uuid::new_v4().simple().to_string()[..6]);
    let dir = dir_of(&id)?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建剑谱目录失败：{e}"))?;
    let (tx, rx) = mpsc::channel();
    let started = Instant::now();
    platform::start(tx.clone())?;
    let worker = std::thread::Builder::new().name("nova-jianpu".into())
        .spawn(move || worker(rx, dir, started)).map_err(|e| { platform::stop(); e.to_string() })?;
    *active = Some(Active { id: id.clone(), started, tx, worker });
    drop(active);
    #[cfg(windows)]
    crate::agent_glow::recording(true);
    changed();
    Ok(json!({"recording":true,"id":id}))
}

pub(crate) fn stop() -> Result<Recording> {
    let Some(active) = ACTIVE.lock().unwrap().take() else { return Err("没有正在录制的剑谱".into()) };
    platform::stop();
    #[cfg(windows)]
    crate::agent_glow::recording(false);
    let _ = active.tx.send(Msg::Stop);
    let events = active.worker.join().map_err(|_| "剑谱录制线程异常退出".to_string())?;
    let mut steps = fold(&events);
    drop_trailing_shortcut(&mut steps, &shortcut());
    let rec = Recording { id: active.id, started_at: chrono::Utc::now().timestamp_millis() - active.started.elapsed().as_millis() as i64,
        ended_at: chrono::Utc::now().timestamp_millis(), steps, learn_thread: None, routes: Vec::new() };
    let saved = save(&rec);
    changed();
    saved.map(|_| rec)
}

/// 快捷键/自动停止入口：停止时只收起光效、静默保存（不打开剑谱页）；只有失败才用系统通知告知，
/// 因为此时 Nova 窗口通常不在前台。
pub(crate) fn toggle_notify() {
    let Some(app) = APP.get() else { return };
    if let Err(e) = toggle() { crate::sys_notify::show(app, "剑谱", &e, false, None); }
}

/// 退出 Nova 时保存正在进行的录制，并卸载钩子、恢复指针。
pub(crate) fn exit() { if recording() { let _ = stop(); } }

fn shortcut() -> String {
    APP.get().and_then(|app| app.try_state::<crate::AppState>().map(|s| s.settings.lock().unwrap().jianpu_shortcut.clone())).unwrap_or_default()
}

fn worker(rx: mpsc::Receiver<Msg>, dir: PathBuf, started: Instant) -> Vec<Ev> {
    let mut events = Vec::new();
    let mut shots = 0usize;
    let mut typing = false;
    let mut sensor = platform::Sensor::new();
    let stop_soon = || { std::thread::spawn(|| if recording() { toggle_notify() }); };
    loop {
        // 停顿 400ms 时读一次焦点输入框：回车发送会清空输入框，只在段落结束时读可能已读不到。
        let msg = if typing { rx.recv_timeout(Duration::from_millis(400)) } else { rx.recv().map_err(|_| mpsc::RecvTimeoutError::Disconnected) };
        let (raw, at) = match msg {
            Ok(Msg::Raw(raw, at)) => (raw, at),
            Ok(Msg::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                if typing { if let Some(text) = sensor.field_value() { events.push(Ev::Value { text }); } }
                return events;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if let Some(text) = sensor.field_value() { events.push(Ev::Value { text }); }
                typing = false;
                continue;
            }
        };
        let types = matches!(&raw, Raw::Key { text: Some(_), ctrl: false, alt: false, win: false, .. }) || matches!(&raw, Raw::Key { name, .. } if name == "Backspace");
        if typing && !types { if let Some(text) = sensor.field_value() { events.push(Ev::Value { text }); } }
        typing = types;
        events.push(match raw {
            Raw::Down { button, x, y } => {
                let (ctx, frame) = sensor.shot((x, y), &dir, shots + 1);
                shots += frame.is_some() as usize;
                Ev::Down { at, button, pt: (x, y), ctx, frame }
            }
            Raw::Up { button, x, y } => Ev::Up { button, pt: (x, y) },
            Raw::Wheel { delta, horizontal } => Ev::Wheel { at, delta, horizontal, ctx: sensor.foreground() },
            Raw::Key { name, text, ctrl, alt, shift, win } => Ev::Key { at, name, text, mods: [ctrl, alt, shift, win], ctx: sensor.foreground() },
        });
        if events.len() >= MAX_STEPS * 3 || shots >= MAX_STEPS || started.elapsed() >= MAX_TIME { stop_soon(); }
    }
}

fn summary(rec: &Recording) -> Value {
    let mut apps: Vec<&str> = Vec::new();
    for s in &rec.steps { if !s.app.is_empty() && !apps.contains(&s.app.as_str()) { apps.push(&s.app); } }
    let state = if !rec.routes.is_empty() { "learned" } else if rec.learn_thread.is_some() { "learning" } else { "recorded" };
    json!({"id":rec.id,"startedAt":rec.started_at,"endedAt":rec.ended_at,"steps":rec.steps.len(),"apps":apps,
        "state":state,"learnThread":rec.learn_thread,"routes":rec.routes})
}

pub(crate) fn list() -> Vec<Value> {
    let mut all: Vec<Recording> = std::fs::read_dir(root()).into_iter().flatten().flatten()
        .filter_map(|e| load(&e.file_name().to_string_lossy()).ok()).collect();
    all.sort_by(|a, b| b.started_at.cmp(&a.started_at));
    all.iter().map(summary).collect()
}

/// experience_save 带 recordingId 时代替「180 秒内的观察」做来源校验：剑来的 scope 必须是演示里出现过的应用，
/// chrome 的 scope 必须是演示里 Chrome 地址栏出现过的网站 origin。
pub(crate) fn scope_of(id: &str, tool: &str, raw_scope: &str) -> Result<String> {
    let rec = load(id)?;
    let want = crate::tool_experience::scope(tool, raw_scope)?;
    let seen = |s: &Step| if tool == "chrome" {
        s.url.as_deref().and_then(|u| crate::tool_experience::scope("chrome", u).ok()).as_deref() == Some(want.as_str())
    } else { s.app.to_lowercase() == want };
    if rec.steps.iter().any(seen) { Ok(want) } else if tool == "chrome" {
        Err("scope 不是这段剑谱里 Chrome 打开过的网站 origin".into())
    } else { Err("scope 不是这段剑谱里出现过的应用名".into()) }
}

pub(crate) fn link_route(id: &str, route: &str) {
    if let Ok(mut rec) = load(id) {
        if !rec.routes.iter().any(|r| r == route) { rec.routes.push(route.into()); let _ = save(&rec); changed(); }
    }
}

const LEARN_RULES: &str = "你在整理一段用户亲手演示的桌面操作（剑谱），目标是把它存进剑来知识图谱，供以后复用。只整理，不要操作桌面，也不要重放。\n\
下方 JSON 是按时间排列的步骤：click/double_click/right_click/drag 的 x/y 是对应截图（image 字段，文件在下方截图目录里）里的像素，红圈标出了点击位置；scroll 的 key 是方向、amount 是格数；key 是组合键；type 的 text 是按键得到的文字，value 是这段输入结束时读到的输入框内容（更可信；输入法输入只有 value）。截图不随提示词附上：需要确认点的是什么时，用 read 按 image 文件名读取截图目录下的图片，只读需要的几张。\n\
1. 按 app 拆开（图谱按应用/网站隔离，不能跨应用拼接）；带 url 的步骤是在 Chrome 网页里的操作，按网站 origin 拆开，归为 chrome 经验；每个应用/网站按目标切成一条或多条路径，task 写成通用目标，如“给联系人发送文件”。\n\
2. 对照截图把坐标写成语义步骤（如 点击「发送」按钮），去掉试错、绕路和撤销的操作；超过 12 步时合并相邻步骤。\n\
3. 输入内容：必须输入指定内容的（命令、网址、路径、配置值、固定选项文字）在步骤里原样写出，如 输入「git status」并回车；每次会变的（搜索词、人名、日期、正文）只写方法，如 在搜索框输入要找的联系人名称。拿不准按会变处理。\n\
4. 桌面应用的路径调用 jianlai operation=experience_save，scope 填该应用 app 原值；Chrome 网页里的路径调用 chrome operation=experience_save，scope 填网站 origin（如 https://github.com，取自 url），conditions 写起始页面路径，步骤按网页控件写（如 点击 button「提交」），不写坐标。experience 都带 recordingId、task、conditions（入口条件）、steps、checks（演示最后可见的结果）、pitfalls（演示中暴露的坑，可空）、evidence（简述演示终态）、outcome=success、redacted=true；不需要 snapshotId/tabTag。\n\
5. 保存完用一两句话汇报存了哪几条路径。演示提炼不出可复用路径时直接说明原因，不保存。";

pub(crate) fn learn(app: &AppHandle, id: &str) -> Result<String> {
    let mut rec = load(id)?;
    if rec.steps.is_empty() { return Err("这段剑谱没有录到操作".into()); }
    let dir = dir_of(id)?;
    let state = app.state::<crate::AppState>();
    let (kind, model) = {
        let s = state.settings.lock().unwrap();
        // 暂时沿用 Meteor（流星）的执行后端与模型。
        (crate::threads::AgentKind::from_str(&s.meteor_agent_kind).unwrap_or(crate::threads::AgentKind::Lyra), s.meteor_model.clone())
    };
    if !state.agent_enabled(&kind) { return Err(format!("{} 后端已关闭，请在设置 → Meteor 里更换执行模型", kind.label())); }
    // 与 Meteor 一样在临时目录开会话：不给每段剑谱各建一个项目；截图按绝对路径读取。
    let scratch = std::env::temp_dir().join(crate::SCRATCH_MARK).join(format!("jianpu-{id}"));
    std::fs::create_dir_all(&scratch).map_err(|e| format!("创建临时目录失败：{e}"))?;
    let mut thread = crate::threads::Thread::new(scratch.to_string_lossy().to_string(), kind, Some(model).filter(|m| !m.is_empty()), Some("build".into()), None, false);
    let apps = summary(&rec)["apps"].as_array().map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join("、")).unwrap_or_default();
    thread.title = format!("剑谱 · {}", apps.chars().take(40).collect::<String>());
    let thread_id = thread.id.clone();
    {
        let mut store = state.store.lock().unwrap();
        store.threads.push(thread);
        store.save();
    }
    let prompt = format!("{LEARN_RULES}\n\nrecordingId: {id}\n截图目录：{}\n```json\n{}\n```", dir.display(), serde_json::to_string(&rec.steps).unwrap_or_default());
    if let Err(e) = crate::dispatch_prompt(app, thread_id.clone(), prompt, Vec::new()) {
        let mut store = state.store.lock().unwrap();
        store.threads.retain(|t| t.id != thread_id);
        store.save();
        return Err(e);
    }
    let _ = app.emit(crate::acp::EV_THREADS, json!({}));
    rec.learn_thread = Some(thread_id.clone());
    save(&rec)?;
    changed();
    Ok(thread_id)
}

#[tauri::command]
pub(crate) async fn jianpu_toggle() -> Result<Value> { tokio::task::spawn_blocking(toggle).await.map_err(|e| e.to_string())? }

#[tauri::command]
pub(crate) fn jianpu_status() -> Value { status() }

#[tauri::command]
pub(crate) async fn jianpu_list() -> Vec<Value> { tokio::task::spawn_blocking(list).await.unwrap_or_default() }

#[tauri::command]
pub(crate) fn jianpu_get(id: String) -> Result<Value> {
    let rec = load(&id)?;
    let mut value = summary(&rec);
    value["stepList"] = json!(rec.steps);
    value["dir"] = json!(dir_of(&id)?.to_string_lossy());
    Ok(value)
}

#[tauri::command]
pub(crate) fn jianpu_image(id: String, name: String) -> Result<String> {
    use base64::Engine;
    if name.contains(['/', '\\']) || !name.ends_with(".jpg") { return Err("图片名无效".into()); }
    let bytes = std::fs::read(dir_of(&id)?.join(name)).map_err(|e| format!("读取截图失败：{e}"))?;
    Ok(format!("data:image/jpeg;base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes)))
}

#[tauri::command]
pub(crate) fn jianpu_delete(id: String) -> Result<()> {
    if ACTIVE.lock().unwrap().as_ref().is_some_and(|a| a.id == id) { return Err("正在录制，先停止".into()); }
    std::fs::remove_dir_all(dir_of(&id)?).map_err(|e| format!("删除剑谱失败：{e}"))?;
    changed();
    Ok(())
}

#[tauri::command]
pub(crate) async fn jianpu_learn(app: AppHandle, id: String) -> Result<String> {
    tokio::task::spawn_blocking(move || learn(&app, &id)).await.map_err(|e| e.to_string())?
}

#[cfg(windows)]
mod platform {
    use super::{Ctx, Frame, Msg, Raw};
    use std::{path::Path, sync::{atomic::{AtomicU32, Ordering}, mpsc, Mutex}};
    use windows_sys::Win32::{
        Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM},
        System::{LibraryLoader::GetModuleHandleW, Threading::GetCurrentThreadId},
        UI::{Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
    };

    static TX: Mutex<Option<(mpsc::Sender<Msg>, std::time::Instant)>> = Mutex::new(None);
    static THREAD: AtomicU32 = AtomicU32::new(0);

    fn send(raw: Raw) {
        if let Some((tx, started)) = TX.lock().unwrap().as_ref() { let _ = tx.send(Msg::Raw(raw, started.elapsed().as_millis() as i64)); }
    }

    unsafe fn ours(hwnd: HWND) -> bool {
        let mut pid = 0;
        GetWindowThreadProcessId(hwnd, &mut pid);
        pid == std::process::id()
    }

    pub(super) fn start(tx: mpsc::Sender<Msg>) -> Result<(), String> {
        *TX.lock().unwrap() = Some((tx, std::time::Instant::now()));
        let (ready_tx, ready) = mpsc::channel();
        std::thread::Builder::new().name("nova-jianpu-hook".into()).spawn(move || unsafe {
            let module = GetModuleHandleW(std::ptr::null());
            let keyboard = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), module, 0);
            let mouse = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), module, 0);
            if keyboard.is_null() || mouse.is_null() {
                if !keyboard.is_null() { UnhookWindowsHookEx(keyboard); }
                if !mouse.is_null() { UnhookWindowsHookEx(mouse); }
                let _ = ready_tx.send(Err("无法安装全局输入钩子".to_string()));
                return;
            }
            THREAD.store(GetCurrentThreadId(), Ordering::Release);
            let _ = ready_tx.send(Ok(()));
            // 低级钩子由安装线程的消息循环驱动。
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {}
            UnhookWindowsHookEx(keyboard);
            UnhookWindowsHookEx(mouse);
        }).map_err(|e| e.to_string())?;
        let result = ready.recv().map_err(|e| e.to_string())?;
        if result.is_err() { TX.lock().unwrap().take(); }
        result
    }

    pub(super) fn stop() {
        TX.lock().unwrap().take();
        let thread = THREAD.swap(0, Ordering::AcqRel);
        if thread != 0 { unsafe { PostThreadMessageW(thread, WM_QUIT, 0, 0); } }
    }

    fn key_name(vk: u32) -> Option<String> {
        Some(match vk {
            0x10 | 0x11 | 0x12 | 0x14 | 0x5B | 0x5C | 0xA0..=0xA5 => return None,
            0x08 => "Backspace".into(), 0x09 => "Tab".into(), 0x0D => "Enter".into(), 0x1B => "Escape".into(), 0x20 => "Space".into(),
            0x21 => "PageUp".into(), 0x22 => "PageDown".into(), 0x23 => "End".into(), 0x24 => "Home".into(),
            0x25 => "ArrowLeft".into(), 0x26 => "ArrowUp".into(), 0x27 => "ArrowRight".into(), 0x28 => "ArrowDown".into(),
            0x2D => "Insert".into(), 0x2E => "Delete".into(),
            0x30..=0x39 | 0x41..=0x5A => char::from(vk as u8).to_string(),
            0x70..=0x87 => format!("F{}", vk - 0x6F),
            v if v == VK_PROCESSKEY as u32 => "IME".into(),
            v => format!("VK{v:02X}"),
        })
    }

    unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code >= 0 && matches!(wparam as u32, WM_KEYDOWN | WM_SYSKEYDOWN) {
            let k = &*(lparam as *const KBDLLHOOKSTRUCT);
            let foreground = GetForegroundWindow();
            if k.flags & LLKHF_INJECTED == 0 && !ours(foreground) {
                if let Some(name) = key_name(k.vkCode) {
                    let down = |vk: u16| GetAsyncKeyState(vk as i32) < 0;
                    let (ctrl, alt, shift, win) = (down(VK_CONTROL), down(VK_MENU), down(VK_SHIFT), down(VK_LWIN) || down(VK_RWIN));
                    let text = if name == "IME" { Some(String::new()) } else {
                        let mut state = [0u8; 256];
                        if shift { state[VK_SHIFT as usize] = 0x80; }
                        if GetKeyState(VK_CAPITAL as i32) & 1 != 0 { state[VK_CAPITAL as usize] = 1; }
                        // AltGr = Ctrl+Alt 出字符。
                        if ctrl && alt { state[VK_CONTROL as usize] = 0x80; state[VK_MENU as usize] = 0x80; }
                        let mut buf = [0u16; 8];
                        let layout = GetKeyboardLayout(GetWindowThreadProcessId(foreground, std::ptr::null_mut()));
                        // ponytail: wFlags=4 不改动内核键盘状态（Win10 1607+），旧系统上死键组合可能被打断。
                        let n = ToUnicodeEx(k.vkCode, k.scanCode, state.as_ptr(), buf.as_mut_ptr(), buf.len() as i32, 4, layout);
                        (n > 0).then(|| String::from_utf16_lossy(&buf[..n as usize])).filter(|t| !t.chars().any(char::is_control))
                    };
                    send(Raw::Key { name, text, ctrl, alt, shift, win });
                }
            }
        }
        CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam)
    }

    unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code >= 0 {
            let m = &*(lparam as *const MSLLHOOKSTRUCT);
            if m.flags & LLMHF_INJECTED == 0 && !ours(WindowFromPoint(m.pt)) {
                let (x, y) = (m.pt.x, m.pt.y);
                let raw = match wparam as u32 {
                    WM_LBUTTONDOWN => Some(Raw::Down { button: 0, x, y }),
                    WM_RBUTTONDOWN => Some(Raw::Down { button: 1, x, y }),
                    WM_MBUTTONDOWN => Some(Raw::Down { button: 2, x, y }),
                    WM_LBUTTONUP => Some(Raw::Up { button: 0, x, y }),
                    WM_RBUTTONUP => Some(Raw::Up { button: 1, x, y }),
                    WM_MBUTTONUP => Some(Raw::Up { button: 2, x, y }),
                    WM_MOUSEWHEEL => Some(Raw::Wheel { delta: (m.mouseData >> 16) as u16 as i16 as i32 / 120, horizontal: false }),
                    // 水平滚轮正值向右。
                    WM_MOUSEHWHEEL => Some(Raw::Wheel { delta: (m.mouseData >> 16) as u16 as i16 as i32 / 120, horizontal: true }),
                    _ => None,
                };
                if let Some(raw) = raw.filter(|r| !matches!(r, Raw::Wheel { delta: 0, .. })) { send(raw); }
            }
        }
        CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam)
    }

    /// worker 线程里的上下文、截图与 UIA 读值；COM 初始化在本线程。
    pub(super) struct Sensor { automation: Option<windows::Win32::UI::Accessibility::IUIAutomation>, cache: Option<(usize, Ctx)> }
    impl Sensor {
        pub(super) fn new() -> Self {
            use windows::Win32::{System::Com::*, UI::Accessibility::CUIAutomation};
            let automation = unsafe {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).ok()
            };
            Sensor { automation, cache: None }
        }

        fn ctx(&mut self, hwnd: HWND) -> (Ctx, Option<xcap::Window>) {
            let window = xcap::Window::all().ok().and_then(|all| all.into_iter().find(|w| w.id().ok() == Some(hwnd as usize as u32)));
            let ctx = window.as_ref().map(|w| Ctx { app: w.app_name().unwrap_or_default(), title: w.title().unwrap_or_default(), url: String::new() }).unwrap_or_default();
            self.cache = Some((hwnd as usize, ctx.clone()));
            (ctx, window)
        }

        pub(super) fn foreground(&mut self) -> Ctx {
            let hwnd = unsafe { GetForegroundWindow() };
            match &self.cache { Some((h, ctx)) if *h == hwnd as usize => ctx.clone(), _ => self.ctx(hwnd).0 }
        }

        pub(super) fn shot(&mut self, pt: (i32, i32), dir: &Path, n: usize) -> (Ctx, Option<Frame>) {
            let hwnd = unsafe { GetAncestor(WindowFromPoint(POINT { x: pt.0, y: pt.1 }), GA_ROOT) };
            let (mut ctx, window) = self.ctx(hwnd);
            // 按下时地址栏还是这次点击所在的页面。
            if ctx.app.to_lowercase().contains("chrome") { ctx.url = self.chrome_url(hwnd).unwrap_or_default(); }
            let file = format!("{n:03}.jpg");
            // 任务栏、桌面等不在窗口列表里的位置截所在显示器。
            let captured = window.and_then(|w| Some((w.capture_image().ok()?, (w.x().ok()?, w.y().ok()?), w.width().ok()?)))
                .or_else(|| { let m = xcap::Monitor::from_point(pt.0, pt.1).ok()?; Some((m.capture_image().ok()?, (m.x().ok()?, m.y().ok()?), m.width().ok()?)) });
            let frame = captured.and_then(|(image, origin, width)| {
                let scale = super::steps::save_frame(image, origin, width, pt, &dir.join(&file))?;
                Some(Frame { file, origin, scale })
            });
            (if ctx.app.is_empty() { Ctx { app: "桌面".into(), ..ctx } } else { ctx }, frame)
        }

        /// Chrome 地址栏内容（UIA 里窗口内第一个 Edit 控件）。显示时省掉的 https:// 补回来。
        /// ponytail: 依赖 Chrome 把地址栏放在第一个 Edit；界面改版时读不到就留空，该步按桌面应用整理。
        fn chrome_url(&self, hwnd: HWND) -> Option<String> {
            use windows::Win32::{Foundation::HWND as UiaHwnd, System::Variant::VARIANT, UI::Accessibility::*};
            let automation = self.automation.as_ref()?;
            let text = unsafe {
                let root = automation.ElementFromHandle(UiaHwnd(hwnd as _)).ok()?;
                let edit = automation.CreatePropertyCondition(UIA_ControlTypePropertyId, &VARIANT::from(UIA_EditControlTypeId.0)).ok()?;
                let bar = root.FindFirst(TreeScope_Descendants, &edit).ok()?;
                let pattern: IUIAutomationValuePattern = bar.GetCurrentPatternAs(UIA_ValuePatternId).ok()?;
                pattern.CurrentValue().ok()?.to_string()
            };
            let text = text.trim();
            if text.is_empty() || text.contains(' ') { return None; }
            let url = if text.contains("://") { text.to_string() } else { format!("https://{text}") };
            tauri::Url::parse(&url).ok().filter(|u| matches!(u.scheme(), "http" | "https")).map(|u| u.to_string())
        }

        /// 焦点输入框的当前内容；密码框等读不到时返回 None。
        pub(super) fn field_value(&mut self) -> Option<String> {
            use windows::Win32::UI::Accessibility::{IUIAutomationValuePattern, UIA_ValuePatternId};
            let automation = self.automation.as_ref()?;
            unsafe {
                let element = automation.GetFocusedElement().ok()?;
                let pattern: IUIAutomationValuePattern = element.GetCurrentPatternAs(UIA_ValuePatternId).ok()?;
                Some(pattern.CurrentValue().ok()?.to_string()).filter(|v| !v.is_empty())
            }
        }
    }
}

#[cfg(not(windows))]
mod platform {
    use super::{Ctx, Frame, Msg};
    use std::{path::Path, sync::mpsc};
    pub(super) fn start(_: mpsc::Sender<Msg>) -> Result<(), String> { Err("剑谱录制目前仅支持 Windows".into()) }
    pub(super) fn stop() {}
    pub(super) struct Sensor;
    impl Sensor {
        pub(super) fn new() -> Self { Sensor }
        pub(super) fn foreground(&mut self) -> Ctx { Ctx::default() }
        pub(super) fn shot(&mut self, _: (i32, i32), _: &Path, _: usize) -> (Ctx, Option<Frame>) { (Ctx::default(), None) }
        pub(super) fn field_value(&mut self) -> Option<String> { None }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn ids_cannot_escape_the_jianpu_folder() {
        assert!(super::dir_of("../x").is_err() && super::dir_of("a/b").is_err() && super::dir_of("20260101-000000-abc123").is_ok());
    }
}
