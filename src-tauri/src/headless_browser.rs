//! 无头浏览器：带 Nova Chrome 扩展与本机 Chrome 登录态的无头 Chrome 实例。
//!
//! 新会话页的开关打开后，chrome 工具改发无头实例（chrome_browser::prefer_headless），
//! 侧栏面板通过 `headless_browser_view` 每秒取一帧画面；关闭开关即杀掉进程。
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    process::{Child, Command},
    sync::Mutex,
    time::{Duration, Instant},
};
use tauri::{AppHandle, Manager};

/// 启动后等扩展连上的上限。
const START_TIMEOUT: Duration = Duration::from_secs(20);
/// 启动失败后至少等这么久再重试，避免面板每秒轮询都重开一次。
const RETRY_AFTER: Duration = Duration::from_secs(20);
/// UA 识别失败时，从这个时刻起按“启动窗口内新出现的实例”兜底识别无头实例。
const UA_GRACE: Duration = Duration::from_secs(10);
/// 每个会话最多保留的标签数：超出就按开启顺序关掉最旧的。
const TABS_PER_SESSION: usize = 6;
/// 这段时间内既没工具调用也没面板取帧，就回收无头浏览器进程；profile 落盘，登录态不丢。
const IDLE_RECLAIM: Duration = Duration::from_secs(15 * 60);

struct Headless {
    child: Mutex<Option<Child>>,
    start: tokio::sync::Mutex<()>,
    profile: PathBuf,
    /// 上次启动失败的原因与时间，用于退避。
    failure: Mutex<Option<(String, Instant)>>,
    /// 磁盘上复制到 cookie（真实 Chrome 运行时数据库被占用，复制不到）。
    cookies_from_disk: Mutex<bool>,
    /// 登录态同步的告警，侧栏面板直接显示。
    notice: Mutex<Option<String>>,
    /// threadId -> 这个会话用过的工具 owner 目录（模型 session_id 变化也会累积）。
    threads: Mutex<HashMap<String, Vec<String>>>,
    /// 标签归属，按开启顺序：（tabTag, owner）。会话删掉时据此关标签。
    tabs: Mutex<Vec<(String, String)>>,
    /// 最近一次真正用到无头浏览器的时间（工具调用或面板取帧）。
    last_used: Mutex<Instant>,
}

pub(crate) fn init(app: &AppHandle) {
    let profile = app.state::<crate::AppState>().config_dir.join("headless-browser");
    app.manage(Headless {
        child: Mutex::new(None),
        start: tokio::sync::Mutex::new(()),
        profile,
        failure: Mutex::new(None),
        cookies_from_disk: Mutex::new(false),
        notice: Mutex::new(None),
        threads: Mutex::new(HashMap::new()),
        tabs: Mutex::new(Vec::new()),
        last_used: Mutex::new(Instant::now()),
    });
    let enabled = app.state::<crate::AppState>().settings.lock().unwrap().headless_browser_enabled;
    crate::chrome_browser::set_prefer_headless(app, enabled);
    let reclaimer = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            reclaim_idle(&reclaimer).await;
        }
    });
}

/// 闲置回收：长时间没用就退出进程。会话数再多也只占一个进程，标签另按会话回收。
async fn reclaim_idle(app: &AppHandle) {
    if !crate::chrome_browser::prefers_headless(app) || crate::chrome_browser::headless_client(app).is_none() {
        return;
    }
    let idle = match app.try_state::<Headless>() {
        Some(state) => state.last_used.lock().unwrap().elapsed(),
        None => return,
    };
    if idle < IDLE_RECLAIM {
        return;
    }
    eprintln!("[headless] 闲置 {}s，回收无头浏览器", idle.as_secs());
    stop(app);
}

/// 开关变化：切换 chrome 工具路由；关闭时立刻停掉无头浏览器。
/// shortcut: 关掉就杀进程，记着无头的老会话随之回落真实浏览器；要让老会话继续用无头，升级成按会话引用计数。
pub(crate) fn apply(app: &AppHandle, enabled: bool) {
    crate::chrome_browser::set_prefer_headless(app, enabled);
    if !enabled {
        stop(app);
    }
}

/// 杀掉无头浏览器（渲染进程随浏览器进程退出，不需要逐个结束）。
pub(crate) fn stop(app: &AppHandle) {
    if let Some(state) = app.try_state::<Headless>() {
        kill(&state);
        *state.failure.lock().unwrap() = None;
        // 进程没了，标签归属也就没了；会话绑定（threads）保留，下次拉起继续用。
        state.tabs.lock().unwrap().clear();
    }
}

/// 会话启动时绑定 threadId 与工具 owner（archive 目录）：侧栏据此只列本会话的标签。
pub(crate) fn bind_thread(app: &AppHandle, thread_id: &str, owner: &str) {
    if thread_id.is_empty() || owner.is_empty() {
        return;
    }
    let state = app.state::<Headless>();
    let mut threads = state.threads.lock().unwrap();
    let roots = threads.entry(thread_id.to_string()).or_default();
    if roots.iter().any(|root| root == owner) {
        return;
    }
    // 模型 session_id 偶尔会变：留最近几个 owner，旧轮的标签仍算这个会话的。
    if roots.len() >= 8 {
        roots.remove(0);
    }
    roots.push(owner.to_string());
}

/// 会话自己（含它的分身 agent）开过的标签；会话还没绑定时就是空集：面板只看自己的。
pub(crate) fn session_tags(app: &AppHandle, thread_id: &str) -> HashSet<String> {
    let state = app.state::<Headless>();
    let roots = state.threads.lock().unwrap().get(thread_id).cloned().unwrap_or_default();
    let tags: HashSet<String> = state.tabs.lock().unwrap().iter()
        .filter(|(_, owner)| owned_by(&roots, owner))
        .map(|(tag, _)| tag.clone())
        .collect();
    tags
}

/// 记下标签属于哪个会话：侧栏按会话过滤，会话删除或超限时据此回收。
pub(crate) fn claim_tab(app: &AppHandle, tag: &str) {
    let Some(owner) = crate::native_browser::session_owner() else { return };
    if owner.is_empty() || tag.is_empty() {
        return;
    }
    let state = app.state::<Headless>();
    let stale = {
        let mut tabs = state.tabs.lock().unwrap();
        if !takeable(&tabs, tag, &owner) {
            return;
        }
        tabs.retain(|(existing, _)| existing != tag);
        tabs.push((tag.to_string(), owner.clone()));
        overflow(&tabs, &owner)
    };
    close_tabs(app, &state, stale);
}

/// 无头实例只有一个，标签按会话归属：已经属于别的会话的标签不抢过来，
/// 否则那个会话的侧栏会突然空掉；无主标签（如启动时的 about:blank）才记到调用它的会话名下。
fn takeable(tabs: &[(String, String)], tag: &str, owner: &str) -> bool {
    !tabs.iter().any(|(existing, held)| existing == tag && held != owner)
}

/// 会话删除：关掉它自己的标签，清掉绑定。
pub(crate) fn release_thread(app: &AppHandle, thread_id: &str) {
    let state = app.state::<Headless>();
    let Some(roots) = state.threads.lock().unwrap().remove(thread_id) else { return };
    let tags: Vec<String> = {
        let mut tabs = state.tabs.lock().unwrap();
        let (mine, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut *tabs)
            .into_iter()
            .partition(|(_, owner)| owned_by(&roots, owner));
        *tabs = rest;
        mine.into_iter().map(|(tag, _)| tag).collect()
    };
    close_tabs(app, &state, tags);
}

/// 侧栏每次取帧、chrome 工具每次调用都刷新，闲置回收据此判断。
pub(crate) fn touch(app: &AppHandle) {
    if let Some(state) = app.try_state::<Headless>() {
        *state.last_used.lock().unwrap() = Instant::now();
    }
}

fn close_tabs(app: &AppHandle, state: &Headless, tags: Vec<String>) {
    if tags.is_empty() {
        return;
    }
    state.tabs.lock().unwrap().retain(|(tag, _)| !tags.contains(tag));
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let Some(client) = crate::chrome_browser::headless_client(&app) else { return };
        for tag in tags {
            let _ = crate::chrome_browser::request_client(&app, &client, "close_tab", json!({ "tabTag": tag })).await;
        }
    });
}

/// 一个会话的工具 owner 目录，以及它的分身 agent 目录，都算它自己。
fn owned_by(roots: &[String], owner: &str) -> bool {
    roots.iter().any(|root| owner == root || owner.starts_with(&child_root(root)))
}

fn child_root(root: &str) -> String {
    format!("{root}{sep}agents{sep}", sep = std::path::MAIN_SEPARATOR)
}

/// 超出每会话上限的最旧标签（按开启顺序，先开的先关）。
fn overflow(entries: &[(String, String)], owner: &str) -> Vec<String> {
    let mine: Vec<&str> = entries.iter().filter(|(_, kept)| kept == owner).map(|(tag, _)| tag.as_str()).collect();
    let extra = mine.len().saturating_sub(TABS_PER_SESSION);
    mine.iter().take(extra).map(|tag| tag.to_string()).collect()
}

fn kill(state: &Headless) {
    if let Some(mut child) = state.child.lock().unwrap().take() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

pub(crate) async fn ensure(app: &AppHandle) -> Result<(), String> {
    touch(app);
    if crate::chrome_browser::headless_client(app).is_some() {
        return Ok(());
    }
    let state = app.state::<Headless>();
    let _start = state.start.lock().await;
    if crate::chrome_browser::headless_client(app).is_some() {
        return Ok(());
    }
    // 进程还在（只是扩展短暂未连上）时不要再拉一个：同 profile 的第二个 Chrome 会直接退出。
    let alive = match state.child.lock().unwrap().as_mut() {
        Some(child) => matches!(child.try_wait(), Ok(None)),
        None => false,
    };
    if alive {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if crate::chrome_browser::headless_client(app).is_some() {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        return Err("无头浏览器在运行但扩展未连上；可在新会话页关掉「无头浏览器」开关再打开".into());
    }
    if let Some((error, at)) = state.failure.lock().unwrap().as_ref() {
        if at.elapsed() < RETRY_AFTER {
            return Err(error.clone());
        }
    }
    let before: HashSet<String> = crate::chrome_browser::client_ids(app).into_iter().collect();
    if let Err(error) = launch(app, &state, &before).await {
        kill(&state);
        let error = format!(
            "{error}；可在新会话页关闭「无头浏览器」开关改用真实浏览器"
        );
        *state.failure.lock().unwrap() = Some((error.clone(), Instant::now()));
        return Err(error);
    }
    *state.failure.lock().unwrap() = None;
    // profile 里的 cookie 在真实 Chrome 运行时复制不到（数据库被占用）：连上后用扩展补齐。
    let disk_cookies = *state.cookies_from_disk.lock().unwrap();
    let sync_app = app.clone();
    tauri::async_runtime::spawn(async move {
        let result = sync_cookies_from_real(&sync_app).await;
        // 磁盘上已经有 cookie（真实浏览器当时没开）时不拿同步失败去打扰用户。
        let notice = match result {
            Ok(_) => None,
            Err(_) if disk_cookies => None,
            Err(error) => Some(format!("未同步登录态：{error}；无头浏览器可能未登录")),
        };
        *sync_app.state::<Headless>().notice.lock().unwrap() = notice;
    });
    Ok(())
}

/// 用真实 Chrome 的 cookie 补全无头实例，返回写入数量。
async fn sync_cookies_from_real(app: &AppHandle) -> Result<usize, String> {
    let real = crate::chrome_browser::real_client(app).ok_or("真实 Chrome 未连接 Nova 扩展")?;
    let headless = crate::chrome_browser::headless_client(app).ok_or("无头浏览器已断开")?;
    let read = cdp_target(app, &real).await?;
    let cookies = export_cookies(app, &real, &read).await?;
    let write = cdp_target(app, &headless).await?;
    let count = import_cookies(app, &headless, &write, &cookies).await?;
    eprintln!("[headless] 已从真实 Chrome 同步 {count} 个 cookie");
    Ok(count)
}

async fn launch(app: &AppHandle, state: &Headless, before: &HashSet<String>) -> Result<(), String> {
    let chrome = crate::chrome_browser::find_chrome()
        .ok_or("未找到 Google Chrome：无头浏览器需要本机安装 Chrome")?;
    let extension = crate::chrome_browser::release_extension(app)?;
    // 上次异常退出可能留下指向旧端口的 DevToolsActivePort。
    let _ = std::fs::remove_file(state.profile.join("DevToolsActivePort"));
    let (copied, cookies_from_disk, warnings) = seed_profile(&state.profile);
    *state.cookies_from_disk.lock().unwrap() = cookies_from_disk;
    for warning in &warnings {
        eprintln!("[headless] {warning}");
    }
    let mut command = Command::new(&chrome);
    command
        .arg("--headless=new")
        .arg(format!("--user-data-dir={}", state.profile.display()))
        // 不要加 --load-extension / --disable-extensions-except：Chrome 137+ 上它们会让
        // CDP 加载的扩展“加载成功但完全不启动”（无 service worker，永远连不上 Nova）。
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        // 侧栏一帧就是整个视口：给到常见桌面分辨率，页面能一屏装下更多；再大截图每帧就太重了。
        .arg("--window-size=1920,1080")
        // 只留给本机的 CDP 加载扩展用；端口随机并只绑 127.0.0.1，退出即失效。
        .arg("--remote-debugging-port=0")
        .arg("--enable-unsafe-extension-debugging")
        .arg("about:blank");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let child = command.spawn().map_err(|e| format!("启动无头 Chrome 失败：{e}"))?;
    *state.child.lock().unwrap() = Some(child);
    let started = Instant::now();
    let mut load_error = String::new();
    match wait_debug_port(state, started).await {
        Ok((port, path)) => {
            if let Err(error) = load_extension(port, &path, &extension).await {
                load_error = error;
            }
        }
        Err(error) => load_error = error,
    }
    loop {
        if crate::chrome_browser::headless_client(app).is_some() {
            eprintln!("[headless] 已连接（复制 {copied} 项登录态文件）");
            return Ok(());
        }
        let exited = state
            .child
            .lock()
            .unwrap()
            .as_mut()
            .and_then(|child| child.try_wait().ok().flatten());
        if let Some(status) = exited {
            return Err(format!(
                "无头 Chrome 启动后立即退出（{status}）：可能是残留的同 profile 实例占用，请重试"
            ));
        }
        if started.elapsed() >= UA_GRACE {
            let appeared = crate::chrome_browser::client_ids(app)
                .into_iter()
                .find(|id| !before.contains(id));
            if let Some(client) = appeared {
                crate::chrome_browser::mark_headless(app, &client);
                eprintln!("[headless] 已连接（UA 未标记无头，按启动窗口识别）");
                return Ok(());
            }
        }
        if started.elapsed() >= START_TIMEOUT {
            let hint = if load_error.is_empty() {
                "扩展未能在启动后连上 Nova".to_string()
            } else {
                format!("加载扩展失败：{load_error}")
            };
            return Err(format!("无头 Chrome 已启动，但{hint}；请确认本机 Chrome 版本 ≥125"));
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// 等 Chrome 写出 DevToolsActivePort（第一行端口，第二行浏览器级 ws 路径）。
async fn wait_debug_port(state: &Headless, started: Instant) -> Result<(u16, String), String> {
    let file = state.profile.join("DevToolsActivePort");
    loop {
        if let Ok(text) = std::fs::read_to_string(&file) {
            let mut lines = text.lines();
            if let (Some(port), Some(path)) = (lines.next(), lines.next()) {
                if let Ok(port) = port.trim().parse::<u16>() {
                    return Ok((port, path.trim().to_string()));
                }
            }
        }
        if started.elapsed() >= Duration::from_secs(15) {
            return Err("读不到无头 Chrome 的调试端口".into());
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

/// Chrome 137+ 移除了 --load-extension，改用 CDP 的 Extensions.loadUnpacked。
async fn load_extension(port: u16, path: &str, extension: &Path) -> Result<(), String> {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};
    let url = format!("ws://127.0.0.1:{port}{path}");
    let request = url.as_str().into_client_request().map_err(|e| e.to_string())?;
    let stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .map_err(|e| format!("连接无头浏览器调试端口失败：{e}"))?;
    let (mut socket, _) = tokio_tungstenite::client_async(request, stream)
        .await
        .map_err(|e| format!("无头浏览器调试握手失败：{e}"))?;
    let command = json!({"id":1,"method":"Extensions.loadUnpacked","params":{"path":extension.to_string_lossy()}});
    socket.send(Message::Text(command.to_string().into())).await.map_err(|e| e.to_string())?;
    let reply = tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(message) = socket.next().await {
            let Ok(Message::Text(text)) = message else { continue };
            let Ok(value) = serde_json::from_str::<Value>(text.as_str()) else { continue };
            if value["id"] == 1 {
                return Ok(value);
            }
        }
        Err("无头浏览器调试连接已关闭".to_string())
    })
    .await
    .map_err(|_| "加载扩展超时".to_string())??;
    let _ = socket.close(None).await;
    if let Some(error) = reply["error"].as_object() {
        return Err(format!(
            "{}",
            error.get("message").and_then(Value::as_str).unwrap_or("Extensions.loadUnpacked 失败")
        ));
    }
    Ok(())
}

/// 从本机 Chrome 用户目录尽力复制登录态（cookie / localStorage / session / IndexedDB）。
/// 源浏览器正在运行时个别文件可能被占用：跳过并记录，不阻塞启动。
fn seed_profile(target: &Path) -> (usize, bool, Vec<String>) {
    let Some(source) = chrome_user_data_dir() else {
        return (0, false, vec!["未找到本机 Chrome 用户目录：无头浏览器将不带本机登录态".into()]);
    };
    let mut copied = 0;
    let mut cookies = false;
    let mut warnings = Vec::new();
    // ponytail: 只认 Default profile；多 profile 用户需要时再按 Local State 里最后使用的目录选择。
    let entries: [&[&str]; 11] = [
        &["Local State"],
        &["Default", "Cookies"],
        &["Default", "Cookies-journal"],
        &["Default", "Cookies-wal"],
        &["Default", "Network", "Cookies"],
        &["Default", "Network", "Cookies-journal"],
        &["Default", "Network", "Cookies-wal"],
        &["Default", "Local Storage"],
        &["Default", "Session Storage"],
        &["Default", "IndexedDB"],
        &["Default", "Web Data"],
    ];
    for parts in entries {
        let from = parts.iter().fold(source.clone(), |path, part| path.join(part));
        if !from.exists() {
            continue;
        }
        let to = parts.iter().fold(target.to_path_buf(), |path, part| path.join(part));
        match copy_path(&from, &to) {
            Ok(true) => {
                copied += 1;
                if parts.last() == Some(&"Cookies") {
                    cookies = true;
                }
            }
            Ok(false) => {}
            Err(error) => warnings.push(format!("复制 {} 失败：{error}", parts.join("/"))),
        }
    }
    (copied, cookies, warnings)
}

fn copy_path(from: &Path, to: &Path) -> Result<bool, std::io::Error> {
    if from.is_dir() {
        std::fs::create_dir_all(to)?;
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            // leveldb 的 LOCK 是进程锁文件，复制过去只会占住目标目录。
            if entry.file_name() == "LOCK" {
                continue;
            }
            copy_path(&entry.path(), &to.join(entry.file_name()))?;
        }
        Ok(true)
    } else if from.is_file() {
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(from, to)?;
        Ok(true)
    } else {
        Ok(false)
    }
}

fn chrome_user_data_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("LOCALAPPDATA")
            .map(|base| PathBuf::from(base).join("Google").join("Chrome").join("User Data"))
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME")
            .map(|home| PathBuf::from(home).join("Library/Application Support/Google/Chrome"))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config/google-chrome"))
    }
}

/// 侧栏面板的一帧：本会话自己的标签 + 当前标签截图；无头浏览器未启动时顺带启动。
#[tauri::command]
pub async fn headless_browser_view(
    app: AppHandle,
    webview: tauri::Webview,
    thread_id: Option<String>,
    tab_tag: Option<String>,
) -> Result<Value, String> {
    if webview.label() != "main" {
        return Err("仅 Nova 主界面可用".into());
    }
    if !crate::chrome_browser::prefers_headless(&app) {
        return Ok(json!({ "enabled": false }));
    }
    if let Err(error) = ensure(&app).await {
        return Ok(json!({ "enabled": true, "running": false, "error": error }));
    }
    let Some(client) = crate::chrome_browser::headless_client(&app) else {
        return Ok(json!({ "enabled": true, "running": false, "error": "无头浏览器连接已断开" }));
    };
    let tabs = match crate::chrome_browser::request_client(&app, &client, "tabs", json!({})).await {
        Ok(tabs) => tabs,
        Err(error) => return Ok(json!({ "enabled": true, "running": true, "error": format!("读取标签失败：{error}") })),
    };
    let all: Vec<Value> = tabs["tabs"].as_array().cloned().unwrap_or_default();
    // 已关掉的标签及时从归属表里清掉，别让归属表比浏览器活得久。
    {
        let live: HashSet<String> = all.iter().filter_map(|tab| tab["tag"].as_str().map(str::to_string)).collect();
        app.state::<Headless>().tabs.lock().unwrap().retain(|(tag, _)| live.contains(tag));
    }
    let owned = thread_id.as_deref().map(|id| session_tags(&app, id));
    let mut list = summarize(all);
    if let Some(owned) = &owned {
        // 会话只看得见自己开的标签：不暴露其它会话的页面，也不再替空面板开标签。
        list.retain(|tab| tab["tag"].as_str().is_some_and(|tag| owned.contains(tag)));
    }
    let mut chosen = tab_tag.filter(|tag| list.iter().any(|tab| tab["tag"] == *tag));
    if chosen.is_none() {
        chosen = list
            .iter()
            .find(|tab| tab["active"] == true)
            .and_then(|tab| tab["tag"].as_str().map(str::to_string))
            .or_else(|| list.first().and_then(|tab| tab["tag"].as_str().map(str::to_string)));
    }
    let Some(tag) = chosen else {
        return Ok(json!({
            "enabled": true, "running": true, "connected": true, "tabs": list, "tabTag": Value::Null,
            "notice": app.state::<Headless>().notice.lock().unwrap().clone(),
        }));
    };
    let current = list.iter().find(|tab| tab["tag"] == tag).cloned().unwrap_or(Value::Null);
    // 预览用有损 JPEG：每秒一帧，体积远小于无损截图。
    let (image, error) = match crate::chrome_browser::request_client(&app, &client, "cdp", json!({
        "tabTag": tag, "method": "Page.captureScreenshot",
        "params": { "format": "jpeg", "quality": 70, "fromSurface": true, "captureBeyondViewport": false },
    }))
    .await
    {
        Ok(value) => (
            value["data"].as_str().map(|data| format!("data:image/jpeg;base64,{data}")),
            Value::Null,
        ),
        Err(error) => (None, json!(error)),
    };
    Ok(json!({
        "enabled": true, "running": true, "connected": true,
        "tabs": list, "tabTag": tag,
        "title": current["title"], "url": current["url"], "loading": current["loading"],
        "image": image, "imageError": error,
        "notice": app.state::<Headless>().notice.lock().unwrap().clone(),
    }))
}

fn summarize(tabs: Vec<Value>) -> Vec<Value> {
    tabs.iter()
        .map(|tab| json!({
            "tag": tab["tag"], "title": tab["title"], "url": tab["url"],
            "active": tab["active"] == true, "loading": tab["loading"] == true,
        }))
        .collect()
}

/// 在真实浏览器打开当前页，并把无头浏览器里的登录态（cookie + localStorage）回写过去。
#[tauri::command]
pub async fn headless_browser_open(
    app: AppHandle,
    webview: tauri::Webview,
    tab_tag: String,
) -> Result<Value, String> {
    if webview.label() != "main" {
        return Err("仅 Nova 主界面可用".into());
    }
    let headless = crate::chrome_browser::headless_client(&app).ok_or("无头浏览器未启动")?;
    let tabs = crate::chrome_browser::request_client(&app, &headless, "tabs", json!({})).await?;
    let tab = tabs["tabs"]
        .as_array()
        .and_then(|list| list.iter().find(|tab| tab["tag"] == tab_tag.as_str()))
        .cloned()
        .ok_or("标签已关闭，请刷新面板")?;
    let url = tab["url"].as_str().unwrap_or_default().to_string();
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err("只能在真实浏览器打开 HTTP(S) 页面".into());
    }
    let mut warnings = Vec::new();
    let cookies = export_cookies(&app, &headless, &tab_tag).await.unwrap_or_else(|error| {
        warnings.push(format!("读取无头浏览器 cookie 失败：{error}"));
        Vec::new()
    });
    let storage = export_storage(&app, &headless, &tab_tag).await.unwrap_or_else(|error| {
        warnings.push(format!("读取无头浏览器 localStorage 失败：{error}"));
        Vec::new()
    });
    let Some(real) = crate::chrome_browser::real_client(&app) else {
        // 真实浏览器没连扩展：用本机 profile 直接打开（登录态本来就在真实浏览器里）。
        let chrome = crate::chrome_browser::find_chrome().ok_or("未找到 Google Chrome")?;
        Command::new(chrome)
            .arg(&url)
            .spawn()
            .map_err(|e| format!("打开真实 Chrome 失败：{e}"))?;
        warnings.push("真实 Chrome 未连接 Nova 扩展：只打开页面，未回写无头浏览器的登录态".into());
        return Ok(json!({ "opened": true, "browser": "system", "cookies": 0, "localStorage": 0, "warnings": warnings }));
    };
    if cookies.is_empty() && storage.is_empty() {
        warnings.push("无头浏览器这页本来就没有登录态可回写（没登录，或启动时没从真实 Chrome 同步过来）".into());
    }
    let target = cdp_target(&app, &real).await?;
    let imported = import_cookies(&app, &real, &target, &cookies).await.unwrap_or_else(|error| {
        warnings.push(format!("回写 cookie 到真实 Chrome 失败：{error}"));
        0
    });
    // focus：面板的“在真实浏览器打开”要把窗口提到前台，工具调用不抢焦点。
    let opened = crate::chrome_browser::request_client(&app, &real, "open", json!({ "url": url, "focus": true })).await?;
    let tag = opened["tabTag"].as_str().map(str::to_string);
    let mut restored = 0;
    if let Some(tag) = &tag {
        // localStorage 只能在该来源的页面加载后写入，写完刷新一次让页面重新读取。
        if !storage.is_empty() {
            if wait_loaded(&app, &real, tag).await {
                match apply_storage(&app, &real, tag, &storage).await {
                    Ok(count) => {
                        restored = count;
                        let _ = crate::chrome_browser::request_client(&app, &real, "reload", json!({ "tabTag": tag })).await;
                    }
                    Err(error) => warnings.push(format!("回写 localStorage 失败：{error}")),
                }
            } else {
                warnings.push("真实浏览器页面未在 8 秒内加载完成，未回写 localStorage".into());
            }
        }
    }
    Ok(json!({
        "opened": tag.is_some(), "browser": "chrome", "tabTag": tag,
        "cookies": imported, "localStorage": restored, "warnings": warnings,
    }))
}

/// 真实浏览器里挑一个可附加调试的标签；没有就开一张空白页。
async fn cdp_target(app: &AppHandle, client: &str) -> Result<String, String> {
    let tabs = crate::chrome_browser::request_client(app, client, "tabs", json!({})).await?;
    if let Some(tag) = tabs["tabs"]
        .as_array()
        .and_then(|list| {
            list.iter()
                .find(|tab| tab["controllable"] != false && tab["active"] == true)
                .or_else(|| list.iter().find(|tab| tab["controllable"] != false))
        })
        .and_then(|tab| tab["tag"].as_str())
    {
        return Ok(tag.to_string());
    }
    let opened = crate::chrome_browser::request_client(app, client, "open", json!({})).await?;
    opened["tabTag"].as_str().map(str::to_string).ok_or("真实浏览器没有可操作的标签".into())
}

async fn export_cookies(app: &AppHandle, client: &str, tag: &str) -> Result<Vec<Value>, String> {
    // Network.getAllCookies 需要先启用 Network 域；启用失败不影响后续读取。
    let _ = crate::chrome_browser::request_client(app, client, "cdp", json!({ "tabTag": tag, "method": "Network.enable", "params": {} })).await;
    let list = crate::chrome_browser::request_client(app, client, "cdp", json!({ "tabTag": tag, "method": "Network.getAllCookies", "params": {} })).await?;
    Ok(list["cookies"].as_array().cloned().unwrap_or_default())
}

async fn export_storage(app: &AppHandle, client: &str, tag: &str) -> Result<Vec<Vec<String>>, String> {
    let result = crate::chrome_browser::request_client(app, client, "cdp", json!({
        "tabTag": tag, "method": "Runtime.evaluate",
        "params": { "expression": "JSON.stringify(Object.entries(localStorage))", "returnByValue": true },
    }))
    .await?;
    let pairs: Vec<Vec<String>> = serde_json::from_str(result["result"]["value"].as_str().unwrap_or("[]")).unwrap_or_default();
    Ok(pairs.into_iter().filter(|pair| pair.len() == 2).collect())
}

fn cookie_key(cookie: &Value) -> String {
    format!(
        "{}\u{1}{}\u{1}{}",
        cookie["name"].as_str().unwrap_or_default(),
        cookie["domain"].as_str().unwrap_or_default(),
        cookie["path"].as_str().unwrap_or_default()
    )
}

/// 只保留 CDP 认识的字段；分区（partitioned）cookie 不跨实例复制。
fn cookie_param(cookie: &Value) -> Option<Value> {
    if cookie.get("partitionKey").is_some_and(|key| !key.is_null()) {
        return None;
    }
    let name = cookie["name"].as_str()?;
    let value = cookie["value"].as_str()?;
    let domain = cookie["domain"].as_str().filter(|domain| !domain.is_empty())?;
    let secure = cookie["secure"] == true;
    let path = cookie["path"].as_str().filter(|path| !path.is_empty()).unwrap_or("/");
    let mut param = json!({
        "name": name, "value": value, "path": path,
        "secure": secure, "httpOnly": cookie["httpOnly"] == true,
    });
    // 以点开头是域 cookie；否则用 url 落成 host-only，避免扩散到子域。
    if domain.starts_with('.') {
        param["domain"] = json!(domain);
    } else {
        let scheme = if secure { "https" } else { "http" };
        param["url"] = json!(format!("{scheme}://{domain}{path}"));
    }
    if let Some(same_site) = cookie["sameSite"].as_str().filter(|v| matches!(*v, "Strict" | "Lax" | "None")) {
        param["sameSite"] = json!(same_site);
    }
    if let Some(expires) = cookie["expires"].as_f64().filter(|expires| *expires > 0.0) {
        param["expires"] = json!(expires);
    }
    Some(param)
}

async fn import_cookies(app: &AppHandle, client: &str, tag: &str, cookies: &[Value]) -> Result<usize, String> {
    if cookies.is_empty() {
        return Ok(0);
    }
    // 值相同的不重设：真实浏览器可能刚刷新过同名 cookie。
    let existing = export_cookies(app, client, tag).await.unwrap_or_default();
    let same: HashMap<String, &str> = existing
        .iter()
        .map(|cookie| (cookie_key(cookie), cookie["value"].as_str().unwrap_or_default()))
        .collect();
    let params: Vec<Value> = cookies
        .iter()
        .filter(|cookie| same.get(&cookie_key(cookie)).copied() != Some(cookie["value"].as_str().unwrap_or_default()))
        .filter_map(cookie_param)
        .collect();
    if params.is_empty() {
        return Ok(0);
    }
    // 一次写入几百个，避免单个 CDP 命令体积/耗时超预算。
    for chunk in params.chunks(500) {
        crate::chrome_browser::request_client(app, client, "cdp", json!({
            "tabTag": tag, "method": "Network.setCookies", "params": { "cookies": chunk },
        }))
        .await?;
    }
    Ok(params.len())
}

async fn wait_loaded(app: &AppHandle, client: &str, tag: &str) -> bool {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        match crate::chrome_browser::request_client(app, client, "tabs", json!({})).await {
            Ok(tabs) => match tabs["tabs"].as_array().and_then(|list| list.iter().find(|tab| tab["tag"] == tag)) {
                Some(tab) if tab["loading"] != true => return true,
                None => return false,
                _ => {}
            },
            Err(_) => return false,
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
}

async fn apply_storage(app: &AppHandle, client: &str, tag: &str, pairs: &[Vec<String>]) -> Result<usize, String> {
    let items = serde_json::to_string(pairs).map_err(|e| e.to_string())?;
    let expression = format!(
        "(()=>{{const items={items};for(const [k,v] of items){{try{{localStorage.setItem(k,v)}}catch(e){{}}}}return items.length}})()"
    );
    let result = crate::chrome_browser::request_client(app, client, "cdp", json!({
        "tabTag": tag, "method": "Runtime.evaluate",
        "params": { "expression": expression, "returnByValue": true },
    }))
    .await?;
    if result.get("exceptionDetails").is_some() {
        return Err("页面拒绝写入 localStorage".into());
    }
    Ok(result["result"]["value"].as_u64().unwrap_or(0) as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(tag: &str, owner: &str) -> (String, String) {
        (tag.to_string(), owner.to_string())
    }

    #[test]
    fn a_session_owns_its_agents_tabs_but_not_other_sessions() {
        use std::path::MAIN_SEPARATOR as Sep;
        let root = std::path::Path::new("data").join("tool-results").join("s1").to_string_lossy().into_owned();
        let roots = vec![root.clone()];
        assert!(owned_by(&roots, &root), "its own owner");
        assert!(owned_by(&roots, &format!("{root}{Sep}agents{Sep}a1")), "child agent of the same session");
        assert!(!owned_by(&roots, &format!("{root}0")), "prefix must not leak into s10");
        assert!(!owned_by(&roots, &root.replace("s1", "s2")), "another session");
    }

    #[test]
    fn a_claimed_tab_is_never_taken_from_another_session() {
        let tabs = vec![entry("T1", "them")];
        assert!(!takeable(&tabs, "T1", "me"), "another session's tab stays theirs");
        assert!(takeable(&tabs, "T1", "them"), "the owner may refresh its own claim");
        assert!(takeable(&tabs, "T2", "me"), "an unowned tab is free");
    }

    #[test]
    fn a_session_only_keeps_its_newest_tabs() {
        let mut entries: Vec<(String, String)> = Vec::new();
        for index in 0..(TABS_PER_SESSION + 2) {
            entries.push(entry(&format!("T{index}"), "mine"));
        }
        entries.push(entry("other", "theirs"));
        assert_eq!(overflow(&entries, "mine"), vec!["T0".to_string(), "T1".to_string()]);
        assert!(overflow(&entries, "theirs").is_empty());
    }
}
