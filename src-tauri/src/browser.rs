//! 浏览器录制：拉起一个长驻 Node 进程（browser_recorder_node.js），用 Playwright
//! 驱动真实 Chrome/Edge 窗口。Node 侧负责注入采集脚本、截图；Rust 通过 stdin/stdout
//! JSON 行协议收发。录制事件缓冲在 Rust，前端轮询/事件推送获取。
use std::collections::HashMap;
use std::io::Write;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager, State};

use crate::AppState;

const RECORDER_JS: &str = include_str!("browser_recorder_node.js");
const COLLECTOR_JS: &str = include_str!("browser_collector.js");
const PLAYWRIGHT_VERSION: &str = "1.61.1";

pub(crate) fn playwright_runtime_dir(config_dir: &std::path::Path) -> std::path::PathBuf {
    config_dir.join("browser-runtime")
}

/// Nova 共享 Playwright 运行时。首次缺失时安装一次，后续所有项目和临时会话复用；
/// 只安装 playwright-core，不下载 Chromium，浏览器使用系统 Chrome/Edge。
pub(crate) fn ensure_playwright_runtime(config_dir: &std::path::Path) -> Result<std::path::PathBuf, String> {
    let runtime = playwright_runtime_dir(config_dir);
    let module = runtime.join("node_modules").join("playwright-core");
    let marker = runtime.join(".nova-playwright-version");
    let current = std::fs::read_to_string(&marker).unwrap_or_default();
    if module.is_dir() && current.trim() == PLAYWRIGHT_VERSION {
        return Ok(runtime);
    }

    std::fs::create_dir_all(&runtime).map_err(|e| format!("创建 Playwright 共享目录失败: {e}"))?;
    let package = json!({
        "name": "nova-browser-runtime",
        "private": true,
        "version": "1.0.0"
    });
    std::fs::write(
        runtime.join("package.json"),
        serde_json::to_vec_pretty(&package).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("写入 Playwright 共享配置失败: {e}"))?;

    #[cfg(windows)]
    let npm = "npm.cmd";
    #[cfg(not(windows))]
    let npm = "npm";
    let package_spec = format!("playwright-core@{PLAYWRIGHT_VERSION}");
    let output = std::process::Command::new(npm)
        .current_dir(&runtime)
        .args([
            "install",
            "--no-audit",
            "--no-fund",
            "--save-exact",
            package_spec.as_str(),
        ])
        // 明确禁止 Playwright 安装浏览器二进制；运行时只调用系统 Chrome/Edge。
        .env("PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD", "1")
        .output()
        .map_err(|e| format!("启动 npm 安装 Playwright 共享运行时失败: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("首次安装 Playwright 共享运行时失败: {}", stderr.trim()));
    }
    std::fs::write(&marker, PLAYWRIGHT_VERSION)
        .map_err(|e| format!("写入 Playwright 版本标记失败: {e}"))?;
    Ok(runtime)
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct BrowserInfo {
    pub url: String,
    pub recording: bool,
    pub paused: bool,
    pub running: bool,
    pub event_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum NodeMsg {
    #[serde(rename = "hello")]
    Hello,
    #[serde(rename = "event")]
    Event {
        ts: f64,
        url: String,
        kind: String,
        #[serde(default)]
        target: Option<Value>,
        #[serde(default)]
        data: Option<Value>,
    },
    #[serde(rename = "nav")]
    Nav {
        url: String,
        #[serde(default)]
        error: Option<String>,
    },
    #[serde(rename = "recording")]
    Recording { on: bool },
    #[serde(rename = "shot")]
    Shot {
        req_id: String,
        #[serde(default)]
        data: Option<String>,
        #[serde(default)]
        cancelled: bool,
        #[serde(default)]
        clip: Option<Value>,
    },
    #[serde(rename = "closed")]
    Closed,
    #[serde(rename = "error")]
    Error { error: String },
}

struct RecorderProc {
    stdin: std::process::ChildStdin,
}

pub struct BrowserManager {
    pub recording: AtomicBool,
    pub paused: AtomicBool,
    pub url: Mutex<String>,
    pub events: Mutex<Vec<Value>>,
    pub pending_shots: Mutex<HashMap<String, tokio::sync::oneshot::Sender<Result<Option<String>, String>>>>,
    pub last_event_id: Mutex<u64>,
    proc: Mutex<Option<RecorderProc>>,
}

impl BrowserManager {
    pub fn new() -> Self {
        Self {
            recording: AtomicBool::new(false),
            paused: AtomicBool::new(false),
            url: Mutex::new(String::new()),
            events: Mutex::new(Vec::new()),
            pending_shots: Mutex::new(HashMap::new()),
            last_event_id: Mutex::new(0),
            proc: Mutex::new(None),
        }
    }
}

fn node_modules_root(state: &AppState) -> Result<String, String> {
    let runtime = ensure_playwright_runtime(&state.config_dir)?;
    Ok(runtime.join("node_modules").to_string_lossy().to_string())
}

/// 确保录制进程已启动；返回是否为新启动。
fn ensure_proc(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
    if state.browser.proc.lock().unwrap().is_some() {
        return Ok(());
    }
    // 把脚本写到临时文件
    let dir = std::env::temp_dir().join("nova-browser-recorder");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let script = dir.join("recorder.js");
    std::fs::write(&script, RECORDER_JS).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("browser_collector.js"), COLLECTOR_JS).map_err(|e| e.to_string())?;

    let node_path = node_modules_root(&state)?;
    let storage_state = state.config_dir.join("browser-runtime").join("storage-state.json");
    let mut child = std::process::Command::new("node")
        .arg(&script)
        .env("NODE_PATH", &node_path)
        .env("NOVA_BROWSER_STORAGE_STATE", &storage_state)
        .current_dir(&dir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .map_err(|e| format!("启动录制进程失败（需要 node 在 PATH）: {e}"))?;

    let stdin = child.stdin.take().ok_or("无法获取录制进程 stdin")?;
    let stdout = child.stdout.take().ok_or("无法获取录制进程 stdout")?;
    state.browser.proc.lock().unwrap().replace(RecorderProc { stdin });

    // stdout 读取循环
    let app2 = app.clone();
    std::thread::spawn(move || {
        use std::io::{BufRead, BufReader};
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            let Ok(line) = line else { break };
            let Some(rest) = line.strip_prefix("__NOVA__") else { continue };
            let Ok(msg) = serde_json::from_str::<NodeMsg>(rest) else { continue };
            handle_node_msg(&app2, msg);
        }
        // 进程退出：清理
        let st = app2.state::<AppState>();
        st.browser.proc.lock().unwrap().take();
        st.browser.recording.store(false, Ordering::SeqCst);
        let _ = app2.emit("browser://info", info_of(&st));
        let _ = app2.emit("browser://closed", ());
    });

    Ok(())
}

fn handle_node_msg(app: &AppHandle, msg: NodeMsg) {
    let state = app.state::<AppState>();
    match msg {
        NodeMsg::Hello => {
            eprintln!("[browser] recorder process ready");
        }
        NodeMsg::Nav { url, error } => {
            *state.browser.url.lock().unwrap() = url.clone();
            if let Some(e) = error {
                eprintln!("[browser] nav error: {e}");
            }
            let _ = app.emit("browser://navigate", json!({ "url": url }));
            let _ = app.emit("browser://info", info_of(&state));
        }
        NodeMsg::Recording { on } => {
            state.browser.recording.store(on, Ordering::SeqCst);
            let _ = app.emit("browser://info", info_of(&state));
        }
        NodeMsg::Event { ts, url, kind, target, data } => {
            if state.browser.recording.load(Ordering::SeqCst) && !state.browser.paused.load(Ordering::SeqCst) {
                let mut events = state.browser.events.lock().unwrap();
                // input 每次按键都会上报；同一输入框在没有其它步骤插入时原地更新，
                // 最终只保留用户完成输入后的最后值。
                let selector = target
                    .as_ref()
                    .and_then(|value| value.get("selector"))
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let merged_input = kind == "input"
                    && events.last_mut().is_some_and(|last| {
                        let same_selector = last
                            .get("target")
                            .and_then(|value| value.get("selector"))
                            .and_then(Value::as_str)
                            == selector.as_deref();
                        if last.get("kind").and_then(Value::as_str) == Some("input") && same_selector {
                            if let Some(object) = last.as_object_mut() {
                                object.insert("ts".into(), json!(ts));
                                object.insert("url".into(), json!(url));
                                object.insert("target".into(), json!(target));
                                object.insert("data".into(), json!(data));
                            }
                            true
                        } else {
                            false
                        }
                    });
                if !merged_input {
                    let mut id_guard = state.browser.last_event_id.lock().unwrap();
                    *id_guard += 1;
                    let id = *id_guard;
                    drop(id_guard);
                    events.push(json!({ "id": id, "ts": ts, "url": url, "kind": kind, "target": target, "data": data }));
                }
                let count = events.len();
                drop(events);
                let _ = app.emit("browser://event", json!({ "count": count }));
                let _ = app.emit("browser://info", info_of(&state));
            }
        }
        NodeMsg::Shot { req_id, data, cancelled, .. } => {
            let tx = state.browser.pending_shots.lock().unwrap().remove(&req_id);
            if let Some(tx) = tx {
                let _ = tx.send(if cancelled { Ok(None) } else { Ok(data) });
            }
        }
        NodeMsg::Closed => {
            state.browser.proc.lock().unwrap().take();
            state.browser.recording.store(false, Ordering::SeqCst);
            let _ = app.emit("browser://closed", ());
            let _ = app.emit("browser://info", info_of(&state));
        }
        NodeMsg::Error { error } => {
            eprintln!("[browser] recorder error: {error}");
            let _ = app.emit("browser://error", json!({ "error": error }));
        }
    }
}

fn info_of(state: &AppState) -> BrowserInfo {
    let b = &state.browser;
    BrowserInfo {
        url: b.url.lock().unwrap().clone(),
        recording: b.recording.load(Ordering::SeqCst),
        paused: b.paused.load(Ordering::SeqCst),
        running: b.proc.lock().unwrap().is_some(),
        event_count: b.events.lock().unwrap().len(),
    }
}

fn send_cmd(app: &AppHandle, cmd: Value) -> Result<(), String> {
    ensure_proc(app)?;
    let state = app.state::<AppState>();
    let mut guard = state.browser.proc.lock().unwrap();
    let Some(proc) = guard.as_mut() else {
        return Err("录制进程未启动".into());
    };
    let line = serde_json::to_string(&cmd).map_err(|e| e.to_string())? + "\n";
    proc.stdin.write_all(line.as_bytes()).map_err(|e| e.to_string())?;
    proc.stdin.flush().map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn browser_info(app: AppHandle) -> BrowserInfo {
    let state = app.state::<AppState>();
    info_of(&state)
}

#[tauri::command]
pub async fn browser_open(app: AppHandle, url: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || send_cmd(&app, json!({ "cmd": "navigate", "url": url })))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn browser_navigate(app: AppHandle, url: String) -> Result<(), String> {
    send_cmd(&app, json!({ "cmd": "navigate", "url": url }))
}

#[tauri::command]
pub fn browser_record_start(app: AppHandle) -> Result<(), String> {
    {
        let state = app.state::<AppState>();
        state.browser.events.lock().unwrap().clear();
        state.browser.recording.store(true, Ordering::SeqCst);
        state.browser.paused.store(false, Ordering::SeqCst);
    }
    send_cmd(&app, json!({ "cmd": "startRecord" }))
}

#[tauri::command]
pub fn browser_record_stop(app: AppHandle) -> Result<Vec<Value>, String> {
    {
        let state = app.state::<AppState>();
        state.browser.recording.store(false, Ordering::SeqCst);
        state.browser.paused.store(false, Ordering::SeqCst);
    }
    send_cmd(&app, json!({ "cmd": "stopRecord" }))?;
    let state = app.state::<AppState>();
    let events = std::mem::take(&mut *state.browser.events.lock().unwrap());
    Ok(events)
}

#[tauri::command]
pub fn browser_record_pause(app: AppHandle) -> Result<(), String> {
    app.state::<AppState>().browser.paused.store(true, Ordering::SeqCst);
    send_cmd(&app, json!({ "cmd": "pause" }))
}

#[tauri::command]
pub fn browser_record_resume(app: AppHandle) -> Result<(), String> {
    app.state::<AppState>().browser.paused.store(false, Ordering::SeqCst);
    send_cmd(&app, json!({ "cmd": "resume" }))
}

#[tauri::command]
pub fn browser_events(app: AppHandle) -> Vec<Value> {
    app.state::<AppState>().browser.events.lock().unwrap().clone()
}

/// 整页截图（视口）
#[tauri::command]
pub async fn browser_capture_screenshot(app: AppHandle) -> Result<String, String> {
    let req_id = uuid::Uuid::new_v4().to_string();
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.state::<AppState>().browser.pending_shots.lock().unwrap().insert(req_id.clone(), tx);
    send_cmd(&app, json!({ "cmd": "screenshot", "reqId": req_id }))?;
    match tokio::time::timeout(std::time::Duration::from_secs(15), rx).await {
        Ok(Ok(Ok(Some(data)))) => Ok(data),
        Ok(Ok(Ok(None))) => Err("截图被取消".into()),
        Ok(Ok(Err(e))) => Err(e),
        Ok(Err(_)) => Err("截图通道关闭".into()),
        Err(_) => Err("截图超时".into()),
    }
}

/// 框选截图：在页面内拖框选择区域
#[tauri::command]
pub async fn browser_capture_region(app: AppHandle) -> Result<Value, String> {
    let req_id = uuid::Uuid::new_v4().to_string();
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.state::<AppState>().browser.pending_shots.lock().unwrap().insert(req_id.clone(), tx);
    send_cmd(&app, json!({ "cmd": "regionScreenshot", "reqId": req_id }))?;
    match tokio::time::timeout(std::time::Duration::from_secs(60), rx).await {
        Ok(Ok(Ok(Some(data)))) => Ok(json!({ "image": data })),
        Ok(Ok(Ok(None))) => Err("已取消框选".into()),
        Ok(Ok(Err(e))) => Err(e),
        Ok(Err(_)) => Err("截图通道关闭".into()),
        Err(_) => Err("框选超时".into()),
    }
}

#[tauri::command]
pub fn browser_close(app: AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
    if state.browser.proc.lock().unwrap().is_some() {
        send_cmd(&app, json!({ "cmd": "close" }))?;
    }
    Ok(())
}

pub fn save_shot_to_disk(config_dir: &std::path::Path, data_url: &str, name: Option<&str>) -> Result<String, String> {
    let Some(b64) = data_url.strip_prefix("data:image/png;base64,") else {
        return Err("只支持 PNG data URL".into());
    };
    let bytes = base64::engine::general_purpose::STANDARD.decode(b64).map_err(|e| e.to_string())?;
    let dir = config_dir.join("browser-shots");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let file = name
        .filter(|n| !n.trim().is_empty())
        .map(sanitize_filename)
        .unwrap_or_else(|| format!("shot-{}", uuid::Uuid::new_v4()));
    let path = dir.join(format!("{file}.png"));
    std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
    Ok(path.to_string_lossy().to_string())
}

#[tauri::command]
pub fn browser_save_shot(state: State<'_, AppState>, data_url: String, name: Option<String>) -> Result<String, String> {
    save_shot_to_disk(&state.config_dir, &data_url, name.as_deref())
}

fn sanitize_filename(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .chars()
        .take(48)
        .collect()
}

/// 生成 Playwright 计划：把录制事件流整理为可执行步骤。
#[tauri::command]
pub fn browser_compile_plan(_state: State<'_, AppState>, events: Vec<Value>) -> Value {
    let mut steps: Vec<Value> = Vec::new();
    let mut pending_input: Option<(String, String, String, Value)> = None;

    let flush_input = |steps: &mut Vec<Value>, pending: &mut Option<(String, String, String, Value)>| {
        if let Some((sel, val, tag, images)) = pending.take() {
            let method = if tag == "select" { "selectOption" } else { "fill" };
            steps.push(json!({ "action": method, "selector": sel, "value": val, "targetImagePaths": images }));
        }
    };

    // 严格保留片段编辑器中的步骤顺序，不做导航折叠或额外补步。
    for ev in &events {
        let kind = ev.get("kind").and_then(Value::as_str).unwrap_or("");
        let target = ev.get("target").cloned().unwrap_or(Value::Null);
        let sel = target.get("selector").and_then(Value::as_str).unwrap_or("").to_string();
        match kind {
            "record" => {
                flush_input(&mut steps, &mut pending_input);
                let data = ev.get("data").cloned().unwrap_or(Value::Null);
                steps.push(json!({
                    "action": "record",
                    "selector": sel,
                    "outputName": data.get("outputName").and_then(Value::as_str).unwrap_or(""),
                    "recordContent": data.get("recordContent").and_then(Value::as_str).unwrap_or(""),
                    "targetImagePaths": data.get("targetImagePaths").or_else(|| data.get("imagePaths")).cloned().unwrap_or_else(|| json!([])),
                }));
            }
            "click" | "submit" => {
                flush_input(&mut steps, &mut pending_input);
                if !sel.is_empty() || !target.get("imagePaths").map_or(true, |value| value.as_array().is_none_or(Vec::is_empty)) {
                    steps.push(json!({ "action": "click", "selector": sel, "targetImagePaths": target.get("imagePaths").cloned().unwrap_or_else(|| json!([])) }));
                }
            }
            "input" | "change" => {
                let val = ev.get("data").and_then(|d| d.get("value")).and_then(Value::as_str).unwrap_or("").to_string();
                let tag = target.get("tag").and_then(Value::as_str).unwrap_or("").to_string();
                if !sel.is_empty() || !target.get("imagePaths").map_or(true, |value| value.as_array().is_none_or(Vec::is_empty)) {
                    pending_input = Some((sel, val, tag, target.get("imagePaths").cloned().unwrap_or_else(|| json!([]))));
                }
            }
            "key" => {
                let key = ev.get("data").and_then(|d| d.get("key")).and_then(Value::as_str).unwrap_or("");
                if key == "Enter" || key == "Tab" {
                    flush_input(&mut steps, &mut pending_input);
                    steps.push(json!({ "action": "press", "key": key }));
                }
            }
            "navigate" => {
                flush_input(&mut steps, &mut pending_input);
                // 当前片段中的每一条 navigate 都原样编排为 goto，不再根据来源过滤。
                let url = ev.get("url").and_then(Value::as_str).unwrap_or("").to_string();
                if !url.is_empty() {
                    let data = ev.get("data").cloned().unwrap_or(Value::Null);
                    let storage_enabled = data.get("navigateStorageEnabled").and_then(Value::as_bool).unwrap_or(false);
                    let storage_key = data.get("storageKey").and_then(Value::as_str).unwrap_or("");
                    if storage_enabled && !storage_key.is_empty() {
                        steps.push(json!({
                            "action": "goto",
                            "url": url,
                            "sessionStorage": {
                                "key": storage_key,
                                "value": data.get("storageValue").and_then(Value::as_str).unwrap_or(""),
                            },
                        }));
                    } else {
                        steps.push(json!({ "action": "goto", "url": url }));
                    }
                }
            }
            _ => {}
        }
    }
    flush_input(&mut steps, &mut pending_input);

    json!({ "version": 1, "steps": steps })
}
