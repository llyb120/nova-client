//! 自定义呈现布局：会话的过程和结果除流式文本外，还呈现在用户选择的布局里。
//!
//! - 布局 = 一份沙箱 HTML（容器）；数据 = 一个 JSON 对象，页面里 `novaLayout.onData` 接收。
//! - 布局只在「预设布局」页由设计会话设计（`layout_submit` 提交）；会话里只能选择预设，不能现场设计。
//! - 数据不靠主会话推送（模型常忘记或给出坏 JSON）：系统在会话运行中每 10 秒、每轮结束、选择布局时，
//!   收集本轮回复/思考/工具 + 各布局上次数据，用轻量模型一次请求生成所有布局的新数据（见 refresh）。

use crate::lyra::tools::Tool;
use crate::AppState;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use tauri::{AppHandle, Emitter, Listener, Manager};

pub const EV_LAYOUT: &str = "layout:changed";
const MAX_HTML: usize = 400_000;
const MAX_DATA: usize = 1_000_000;

static APP: OnceLock<AppHandle> = OnceLock::new();
static LOCK: Mutex<()> = Mutex::new(());

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase", default)]
struct Layout {
    id: String,
    name: String,
    html: String,
    /// 设计会话写的数据结构说明，refresh 据此生成数据。
    schema: String,
    /// 来源预设 id：生成数据时用该预设当前的附加提示词。
    preset: String,
    data: Map<String, Value>,
    designing: bool,
}

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase", default)]
struct Preset {
    id: String,
    name: String,
    html: String,
    schema: String,
    /// 生成数据时追加给轻量模型的提示词。
    prompt: String,
    /// 设计会话给的示例数据，用于预设预览和缩略图。
    mock: Map<String, Value>,
    designing: bool,
}

#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
struct ThreadLayouts {
    active: String,
    layouts: Vec<Layout>,
    /// 上次成功生成数据时的输入指纹（会话内容 + 看板结构/提示词，不含上次数据）：没变就不再生成，重启后也保持。
    input: u64,
}

/// 一个设计会话负责的预设：`main` 为空表示预设（旧版会话内设计留下的非空记录已不再使用），`target` 是预设 id。
#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(default)]
struct Design {
    thread: String,
    main: String,
    target: String,
}

#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
struct Store {
    presets: Vec<Preset>,
    threads: HashMap<String, ThreadLayouts>,
    designs: Vec<Design>,
}

/// 每个看板的最终页面（模板 + 数据）持久化在磁盘上，下次直接加载，不用重新生成。
fn doc_file(thread: &str, id: &str) -> PathBuf {
    crate::lyra::config::nova_root().join("layouts").join("threads").join(thread).join(format!("{id}.html"))
}

/// 把数据烘焙进模板：页面运行时启动时读 `window.__novaData`，无需等宿主推送。
fn bake(html: &str, data: &Map<String, Value>) -> String {
    let script = format!("<script>window.__novaData={};</script>", Value::Object(data.clone()).to_string().replace("</", "<\\/"));
    let lower = html.to_ascii_lowercase();
    let after = |tag: &str| lower.match_indices(tag)
        .find(|(i, _)| matches!(lower.as_bytes().get(i + tag.len()), Some(b'>' | b' ' | b'\n' | b'\r' | b'\t')))
        .and_then(|(i, _)| lower[i..].find('>').map(|j| i + j + 1));
    match after("<head").or_else(|| after("<html")) {
        Some(at) => format!("{}{script}{}", &html[..at], &html[at..]),
        None => format!("{script}{html}"),
    }
}

fn save_doc(thread: &str, l: &Layout) {
    let file = doc_file(thread, &l.id);
    if let Some(dir) = file.parent() { let _ = std::fs::create_dir_all(dir); }
    let _ = std::fs::write(file, bake(&l.html, &l.data));
}

/// 读磁盘上的最终页面；缺失（旧数据）时补写一份。
fn load_doc(thread: &str, l: &Layout) -> String {
    std::fs::read_to_string(doc_file(thread, &l.id)).unwrap_or_else(|_| {
        save_doc(thread, l);
        bake(&l.html, &l.data)
    })
}

fn path() -> PathBuf {
    crate::lyra::config::nova_root().join("layouts.json")
}

/// 文件损坏时报错而不是回落默认值，避免下一次保存覆盖用户的预设。
fn load() -> Result<Store, String> {
    match std::fs::read_to_string(path()) {
        Ok(raw) => serde_json::from_str(&raw).map_err(|e| format!("layouts.json 解析失败：{e}")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Store::default()),
        Err(e) => Err(format!("读取 layouts.json 失败：{e}")),
    }
}

fn update<T>(f: impl FnOnce(&mut Store) -> Result<T, String>) -> Result<T, String> {
    let _guard = LOCK.lock().map_err(|_| "布局数据锁不可用")?;
    let mut store = load()?;
    let out = f(&mut store)?;
    let path = path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec(&store).map_err(|e| e.to_string())?).map_err(|e| format!("保存布局失败：{e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("保存布局失败：{e}"))?;
    Ok(out)
}

pub fn start(app: AppHandle) {
    let _ = APP.set(app.clone());
    // 重启后没有设计会话在跑：清掉遗留的「设计中」标记。
    let _ = update(|s| {
        s.presets.iter_mut().for_each(|p| p.designing = false);
        s.threads.values_mut().flat_map(|t| t.layouts.iter_mut()).for_each(|l| l.designing = false);
        Ok(())
    });
    start_refresher(&app);
}

fn emit(payload: Value) {
    if let Some(app) = APP.get() {
        let _ = app.emit(EV_LAYOUT, payload);
    }
}

fn meta_changed(thread: &str) {
    emit(json!({"kind": "meta", "threadId": thread}));
}

fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()[..8].to_string()
}

fn text<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args[key].as_str().map(str::trim).filter(|v| !v.is_empty())
}

fn light_model() -> String {
    // 场景模型不是 Lyra 的 provider/model 形式时用 Lyra 默认模型。
    APP.get().and_then(|app| app.state::<AppState>().scene_model(crate::settings::Scene::Lightweight).ok())
        .map(|(_, m)| m).filter(|m| m.contains('/')).unwrap_or_default()
}

// ---------- 工具定义 ----------

const DESIGN_NOTE: &str = "\n\n你是布局设计会话：只负责设计呈现布局，完成后必须调用 layout_submit 提交。";

/// 只有设计会话有布局工具；主会话不再拿到任何布局工具。
pub fn tools_for(thread_id: &str) -> (Vec<Tool>, String) {
    let is_design = load().ok().is_some_and(|s| s.designs.iter().any(|d| d.thread == thread_id));
    if !is_design {
        return (Vec::new(), String::new());
    }
    (vec![Tool {
        name: "layout_submit",
        description: "提交设计好的布局。html 必须是完整 HTML 文档，用 novaLayout.onData(fn) 或 data-bind 接收数据；schema 简明说明数据键、类型与含义（系统据此从会话内容生成数据）。".into(),
        parameters: json!({"type": "object", "properties": {
            "html": {"type": "string", "description": "完整 HTML 文档（内联 CSS/JS，无外部资源）"},
            "schema": {"type": "string", "description": "数据结构说明：有哪些键、类型、含义、示例"},
            "mock": {"type": "object", "description": "符合 schema 的示例数据（内容逼真、各字段都有值，含 session），用于预览效果"},
            "name": {"type": "string", "description": "布局名称（可选）"}
        }, "required": ["html", "schema", "mock"]}),
    }], DESIGN_NOTE.into())
}

// ---------- 布局数据自动刷新 ----------

const REFRESH_SECS: u64 = 10;
const MAX_CONVERSATION: usize = 16_000;
/// 每个会话的刷新状态：同一会话只有一个请求在跑（结果按顺序落地，不会后发先至）；
/// 请求期间又有新触发就记 pending，跑完再来一轮。
#[derive(Default)]
struct Refresh {
    busy: bool,
    pending: bool,
}
static REFRESHING: Mutex<Option<HashMap<String, Refresh>>> = Mutex::new(None);
/// 用户正在看的看板所属会话（前端上报）；不在看的会话不请求，回来时再补。
static VIEWING: Mutex<String> = Mutex::new(String::new());

fn with_refresh<T>(thread: &str, f: impl FnOnce(&mut Refresh) -> T) -> T {
    f(REFRESHING.lock().unwrap().get_or_insert_with(Default::default).entry(thread.to_string()).or_default())
}

fn viewing(thread: &str) -> bool {
    *VIEWING.lock().unwrap() == thread
}
fn chars_tail(s: &str, max: usize) -> &str {
    let n = s.chars().count();
    if n <= max { s } else { &s[s.char_indices().nth(n - max).map_or(0, |(i, _)| i)..] }
}

/// 本轮会话内容（最后一条用户消息起：提示、思考、工具、回复），超长时保留结尾。
/// 页面里的 `data.session`：本轮任务、步骤、最新回复，随每次生成一起落盘（不再实时推送，避免无谓重绘）。
fn session_value(app: &AppHandle, thread: &str) -> Value {
    use crate::threads::Item;
    let state = app.state::<AppState>();
    let running = crate::running_by_id(&state, thread);
    let Ok(store) = state.store.lock() else { return Value::Null };
    let Some(t) = store.get(thread) else { return Value::Null };
    let users: Vec<usize> = t.items.iter().enumerate().filter(|(_, i)| matches!(i, Item::User { .. })).map(|(n, _)| n).collect();
    let start = users.last().copied();
    let turn = &t.items[start.map_or(0, |s| s + 1)..];
    let prompt = start.and_then(|s| match &t.items[s] { Item::User { text, .. } => Some(chars_tail(text, 4000).to_string()), _ => None }).unwrap_or_default();
    let steps: Vec<Value> = turn.iter().filter_map(|i| match i {
        Item::Tool { call, .. } => Some(json!({"title": call.title, "status": call.status, "kind": call.kind})),
        _ => None,
    }).collect();
    let steps = steps[steps.len().saturating_sub(60)..].to_vec();
    let answer = turn.iter().rev().find_map(|i| match i { Item::Assistant { text, .. } if !text.trim().is_empty() => Some(chars_tail(text, 30_000).to_string()), _ => None }).unwrap_or_default();
    json!({"running": running, "prompt": prompt, "answer": answer, "steps": steps, "turns": users.len()})
}

fn conversation(app: &AppHandle, thread: &str) -> Option<(String, bool)> {
    use crate::threads::Item;
    let state = app.state::<AppState>();
    let running = crate::running_by_id(&state, thread);
    let store = state.store.lock().ok()?;
    let items = &store.get(thread)?.items;
    let start = items.iter().rposition(|i| matches!(i, Item::User { .. })).unwrap_or(0);
    let mut out = String::new();
    for item in &items[start..] {
        match item {
            Item::User { text, .. } => out.push_str(&format!("【用户】{}\n", chars_tail(text, 3000))),
            Item::Thought { text, .. } => out.push_str(&format!("【思考】{}\n", chars_tail(text, 1500))),
            Item::Assistant { text, .. } if !text.trim().is_empty() => out.push_str(&format!("【回复】{text}\n")),
            Item::Tool { call, .. } => out.push_str(&format!("【工具】{} [{}]\n", call.title, call.status)),
            _ => {}
        }
    }
    (!out.is_empty()).then(|| (chars_tail(&out, MAX_CONVERSATION).to_string(), running))
}

fn refresh_prompt(layouts: &[Layout], presets: &[Preset], conversation: &str, running: bool) -> String {
    let mut out = String::from("你负责把一个 AI 助手会话的过程和结果呈现到若干「布局」里。根据下方会话内容，为每个布局生成最新数据。\n规则：严格遵循各布局的数据结构说明；在上次数据基础上更新，仍然正确的内容保留；会话里还没有的信息不要编造（可沿用上次的值、留空或标记为进行中）；不要输出名为 session 的键。\n只输出一个 JSON 对象：键是布局 id，值是该布局完整的新数据对象。不要解释，不要代码块。\n\n# 布局\n");
    for l in layouts {
        out.push_str(&format!("\n## id={} 「{}」\n数据结构：\n{}\n", l.id, l.name, l.schema));
        if let Some(extra) = presets.iter().find(|p| p.id == l.preset).map(|p| p.prompt.trim()).filter(|p| !p.is_empty()) {
            out.push_str(&format!("附加要求：\n{extra}\n"));
        }
        out.push_str(&format!("上次数据：{}\n", Value::Object(l.data.clone())));
    }
    out.push_str(&format!("\n# 会话（{}）\n{conversation}", if running { "仍在运行，以下为目前进展" } else { "本轮已结束" }));
    out
}

/// 输入指纹：只含会话内容和看板定义，不含上次生成的数据——否则每次生成后指纹都变，切回来又会重新生成。
fn input_key(layouts: &[Layout], presets: &[Preset], conv: &str, running: bool) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (conv, running).hash(&mut h);
    for l in layouts {
        (&l.id, &l.schema, presets.iter().find(|p| p.id == l.preset).map(|p| p.prompt.as_str())).hash(&mut h);
    }
    h.finish().max(1)
}

/// 本次刷新的提示词与输入指纹；没有看板、没有会话内容或输入没变时为 None。
fn build_refresh(app: &AppHandle, thread: &str) -> Option<(String, u64)> {
    let store = load().ok()?;
    let t = store.threads.get(thread)?;
    let layouts: Vec<_> = t.layouts.iter().filter(|l| !l.html.is_empty()).cloned().collect();
    if layouts.is_empty() { return None; }
    let (conv, running) = conversation(app, thread)?;
    let key = input_key(&layouts, &store.presets, &conv, running);
    if key == t.input { return None; }
    Some((refresh_prompt(&layouts, &store.presets, &conv, running), key))
}

/// 一次请求刷新会话里所有看板的数据；只刷新用户正在看的会话。
pub fn refresh(app: &AppHandle, thread: &str) {
    if !viewing(thread) { return; }
    let start = with_refresh(thread, |s| if s.busy { s.pending = true; false } else { s.busy = true; true });
    if !start { return; }
    let (app, thread) = (app.clone(), thread.to_string());
    tauri::async_runtime::spawn(async move {
        loop {
            if let Some((prompt, key)) = build_refresh(&app, &thread) {
                emit(json!({"kind": "refreshing", "threadId": thread, "on": true}));
                let result = crate::lyra::run_oneshot(&reqwest::Client::new(), &json!({"action": "generate", "model": light_model(), "prompt": prompt}), None).await;
                if let Some(Value::Object(all)) = result.as_ref().ok().and_then(Value::as_str).and_then(lenient_object) {
                    // 失败不记指纹，下次触发时重试。
                    let _ = apply_refresh(&thread, all, key, session_value(&app, &thread));
                }
            }
            let again = with_refresh(&thread, |s| {
                let again = s.pending && viewing(&thread);
                s.pending = false;
                if !again { s.busy = false; }
                again
            });
            if !again { break; }
        }
        emit(json!({"kind": "refreshing", "threadId": thread, "on": false}));
    });
}

/// 前端上报正在看哪个会话的看板（空 = 没在看）；回到某会话时若期间有新内容就补一次。
fn set_viewing(app: &AppHandle, thread: &str) {
    *VIEWING.lock().unwrap() = thread.to_string();
    if !thread.is_empty() { refresh(app, thread); }
}
fn apply_refresh(thread: &str, all: Map<String, Value>, key: u64, session: Value) -> Result<(), String> {
    let changed = update(|s| {
        let t = s.threads.get_mut(thread).ok_or("布局已删除")?;
        t.input = key;
        let mut changed = Vec::new();
        for l in t.layouts.iter_mut() {
            let Some(Value::Object(mut data)) = all.get(&l.id).cloned() else { continue };
            data.insert("session".into(), session.clone());
            if data == l.data || serde_json::to_string(&data).map_or(0, |s| s.len()) > MAX_DATA { continue; }
            l.data = data;
            save_doc(thread, l);
            changed.push((l.id.clone(), l.data.clone()));
        }
        Ok(changed)
    })?;
    for (id, data) in changed {
        emit(json!({"kind": "data", "threadId": thread, "layoutId": id, "data": data}));
    }
    Ok(())
}

/// 用户在看的会话：运行中每 10 秒刷新一次；一轮结束时再刷新一次拿最终结果。
fn start_refresher(app: &AppHandle) {
    let handle = app.clone();
    app.listen(crate::acp::EV_TURN, move |e| {
        let Ok(v) = serde_json::from_str::<Value>(e.payload()) else { return };
        let Some(tid) = v["threadId"].as_str() else { return };
        if !v["running"].as_bool().unwrap_or(false) {
            refresh(&handle, tid);
        }
    });
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(REFRESH_SECS)).await;
            let tid = VIEWING.lock().unwrap().clone();
            if !tid.is_empty() && crate::running_by_id(&handle.state::<AppState>(), &tid) {
                refresh(&handle, &tid);
            }
        }
    });
}

// ---------- 工具执行 ----------

/// 宽松解析：去掉代码块围栏、取首个 { 到最后一个 } 之间的内容。
fn lenient_object(raw: &str) -> Option<Value> {
    let s = raw.trim().trim_start_matches("```json").trim_start_matches("```").trim_end_matches("```").trim();
    let s = &s[s.find('{')?..=s.rfind('}')?];
    serde_json::from_str::<Value>(s).ok().filter(Value::is_object)
}

/// 工具调用方 → 会话，沿用员工工具的 owner 约定。
pub fn execute(name: &str, args: &Value, owner: &str) -> Result<Value, String> {
    let app = APP.get().ok_or("布局功能尚未就绪")?;
    let thread = crate::employee::owner_thread(&app.state::<AppState>(), owner).ok_or("找不到调用方会话")?;
    match name {
        "layout_submit" => submit(&thread, args),
        _ => Err(format!("未知布局工具：{name}")),
    }
}

fn find<'a>(t: &'a mut ThreadLayouts, key: Option<&str>) -> Option<&'a mut Layout> {
    let key = key.unwrap_or(&t.active).to_string();
    let index = t.layouts.iter().position(|l| l.id == key).or_else(|| t.layouts.iter().position(|l| l.name == key))?;
    t.layouts.get_mut(index)
}

fn submit(thread: &str, args: &Value) -> Result<Value, String> {
    let design = load()?.designs.into_iter().find(|d| d.thread == thread && d.main.is_empty()).ok_or("只有预设布局的设计会话能提交布局")?;
    let html = text(args, "html").ok_or("缺少 html")?;
    if html.len() > MAX_HTML { return Err("html 过大（上限 400KB），请精简".into()); }
    let schema = text(args, "schema").unwrap_or_default().to_string();
    let name = text(args, "name").map(str::to_string);
    // mock 也接受 JSON 字符串（模型常把对象序列化成字符串）。
    let mock = match &args["mock"] {
        Value::Object(m) => m.clone(),
        Value::String(s) => lenient_object(s).and_then(|v| v.as_object().cloned()).unwrap_or_default(),
        _ => Map::new(),
    };
    let html = html.to_string();
    update(|s| {
        let p = s.presets.iter_mut().find(|p| p.id == design.target).ok_or("预设已被删除")?;
        (p.html, p.schema, p.mock, p.designing) = (html, schema, mock, false);
        if let Some(name) = name { p.name = name; }
        Ok(())
    })?;
    emit(json!({"kind": "presets"}));
    Ok(json!({"ok": true, "next": "预设布局已保存，结束即可。"}))
}

// ---------- 设计会话（仅预设） ----------

fn start_preset_design(app: &AppHandle, preset_id: &str, request: &str) -> Result<(), String> {
    let state = app.state::<AppState>();
    let existing = load()?.designs.into_iter().find(|d| d.main.is_empty() && d.target == preset_id)
        .filter(|d| state.store.lock().unwrap().get(&d.thread).is_some());
    let thread = match existing {
        Some(d) => d.thread,
        None => {
            let (kind, model) = state.scene_model(crate::settings::Scene::Lightweight)?;
            let cwd = crate::lyra::config::nova_root().join("layouts");
            std::fs::create_dir_all(&cwd).map_err(|e| format!("创建目录失败：{e}"))?;
            let mut t = crate::threads::Thread::new(cwd.to_string_lossy().into(), kind, Some(model).filter(|m| !m.is_empty()), Some("build".into()), None, false);
            t.title = "[Design] 预设布局".into();
            let id = t.id.clone();
            {
                let mut store = state.store.lock().unwrap();
                store.threads.push(t);
                store.save();
            }
            let _ = app.emit(crate::acp::EV_THREADS, json!({}));
            update(|s| {
                s.designs.retain(|d| !(d.main.is_empty() && d.target == preset_id));
                s.designs.push(Design { thread: id.clone(), main: String::new(), target: preset_id.into() });
                Ok(())
            })?;
            id
        }
    };
    let current = update(|s| Ok(s.presets.iter().find(|p| p.id == preset_id).cloned()))?;
    let prompt = design_prompt(request, current.as_ref().map(|p| (p.html.as_str(), p.schema.as_str())));
    crate::dispatch_prompt(app, thread, prompt, Vec::new())
}

const DESIGN_RULES: &str = "[布局设计任务]\n你是布局设计师，设计一个可复用的「呈现布局」预设：在会话侧栏的沙箱 iframe 里渲染的完整 HTML 页面（无网络，不能引用任何外部资源，CSS/JS 全部内联），用来呈现任意会话的过程和结果。\n约定：\n- 数据由页面运行时注入：`novaLayout.onData((data, changedKeys) => {...})` 在加载时和每次数据更新时调用，data 是完整的当前数据对象；`novaLayout.data` 随时可读。运行时已自动提供，不要自己定义。\n- 另有零代码绑定：元素加 `data-bind=\"键.子键\"` 自动填文本，`data-bind-html=\"键\"` 填 HTML。\n- 数据来源：会话运行中每 10 秒及每轮结束时，系统用一个轻量模型读会话内容（回复、思考、工具步骤）并按你的 schema 生成 data。所以 schema 要写清每个键的含义和填写规则，键宜少而明确，便于模型从对话中提取。\n- `data.session` 由系统在每次生成数据时附上（只读，不写进 schema）：`{running:boolean, prompt:string 本轮任务, answer:string 主会话最新回复(Markdown), steps:[{title,status:'pending'|'in_progress'|'completed'|'failed',kind}], turns:number}`。结构化数据可能滞后几秒或还是空的，页面必须在只有 session 时也有意义（如显示任务、进度、最新回复）。对话本身仍在旁边显示，页面不必重复罗列全部步骤。\n- 布局只是容器：不要写死具体内容，由 data 驱动；没有数据时显示友好的空态。\n- 页面显示在侧栏的一个栏位里（可能与其他栏位并排，宽度常在 260–700px）：自适应宽度，自行滚动，信息层级清晰。\n- 主题：运行时会把宿主主题注入页面——`<html data-theme=\"dark|light\">` 以及 CSS 变量 `--nova-bg`（页面底色）、`--nova-panel`（卡片底色）、`--nova-hover`、`--nova-text`、`--nova-text-dim`、`--nova-text-faint`、`--nova-border`、`--nova-accent`、`--nova-red`、`--nova-green`、`--nova-sans`、`--nova-mono`，切换主题时自动更新。所有颜色必须用这些变量（`var(--nova-xxx)`），不要写死颜色；要用到半透明色请用 `color-mix(in srgb, var(--nova-accent) 15%, transparent)`。交互（折叠、切换标签等）可在页面内用 JS 完成。\n- 完成后必须调用 layout_submit 提交：html（完整文档）、schema（数据键、类型、含义、填写规则与示例）、mock（一份逼真的示例数据，覆盖 schema 的所有键并带上 session，用户在预览里据此看效果）、name。\n";

fn design_prompt(request: &str, current: Option<(&str, &str)>) -> String {
    let mut out = String::from(DESIGN_RULES);
    if let Some((html, schema)) = current.filter(|(html, _)| !html.is_empty()) {
        out.push_str(&format!("\n当前布局的数据结构说明：\n{schema}\n\n当前布局 HTML（在此基础上修改，除非用户要求重做）：\n```html\n{html}\n```\n"));
    }
    out.push_str(&format!("\n用户的要求：\n{request}"));
    out
}

// ---------- 前端命令 ----------

#[tauri::command]
pub fn layout_cmd(app: AppHandle, op: String, args: Value) -> Result<Value, String> {
    let thread = text(&args, "threadId").unwrap_or_default().to_string();
    let need_thread = || if thread.is_empty() { Err("缺少 threadId".to_string()) } else { Ok(thread.clone()) };
    let key = text(&args, "layoutId").map(str::to_string);
    match op.as_str() {
        "get" => {
            let s = load()?;
            let t = s.threads.get(&thread);
            // 前端拿到的 html 是磁盘上的最终页面（已含数据）。
            let layouts: Vec<Layout> = t.map(|t| t.layouts.iter().map(|l| Layout { html: if l.html.is_empty() { String::new() } else { load_doc(&thread, l) }, ..l.clone() }).collect()).unwrap_or_default();
            Ok(json!({"active": t.map(|t| t.active.clone()).unwrap_or_default(), "layouts": layouts, "presets": s.presets}))
        }
        "viewing" => {
            set_viewing(&app, &thread);
            Ok(json!({}))
        }
        "switch" | "delete" | "rename" | "use_preset" | "clear_data" => {
            let thread = need_thread()?;
            let out = update(|s| {
                let t = s.threads.entry(thread.clone()).or_default();
                match op.as_str() {
                    "switch" => { let id = find(t, key.as_deref()).ok_or("没有这个布局")?.id.clone(); t.active = id; }
                    "delete" => {
                        let id = find(t, key.as_deref()).ok_or("没有这个布局")?.id.clone();
                        t.layouts.retain(|l| l.id != id);
                        let _ = std::fs::remove_file(doc_file(&thread, &id));
                        if t.active == id { t.active = t.layouts.last().map(|l| l.id.clone()).unwrap_or_default(); }
                    }
                    "rename" => {
                        let name = text(&args, "name").ok_or("缺少名称")?.to_string();
                        find(t, key.as_deref()).ok_or("没有这个布局")?.name = name;
                    }
                    "use_preset" => {
                        let pid = text(&args, "presetId").ok_or("缺少 presetId")?;
                        let p = s.presets.iter().find(|p| p.id == pid && !p.html.is_empty()).cloned().ok_or("预设不存在或尚未设计完成")?;
                        let t = s.threads.entry(thread.clone()).or_default();
                        let id = new_id();
                        let layout = Layout { id: id.clone(), preset: p.id, name: p.name, html: p.html, schema: p.schema, ..Default::default() };
                        save_doc(&thread, &layout);
                        t.layouts.push(layout);
                        t.active = id;
                    }
                    _ => {
                        // clear_data：清空布局数据。
                        if let Some(l) = find(t, key.as_deref()) { l.data.clear(); save_doc(&thread, l); }
                        t.input = 0;
                    }
                }
                Ok(json!({}))
            })?;
            meta_changed(&thread);
            // 选了布局（会话已停止也一样）就按已有会话内容填一次数据。
            if matches!(op.as_str(), "use_preset" | "clear_data") { refresh(&app, &thread); }
            Ok(out)
        }
        "preset_design" => {
            let request = text(&args, "text").ok_or("缺少设计要求")?.to_string();
            let existing = text(&args, "presetId").map(str::to_string);
            let name = text(&args, "name").unwrap_or("新预设").to_string();
            let id = update(|s| {
                let id = match existing.filter(|id| s.presets.iter().any(|p| &p.id == id)) {
                    Some(id) => id,
                    None => { let id = new_id(); s.presets.push(Preset { id: id.clone(), name, ..Default::default() }); id }
                };
                if let Some(p) = s.presets.iter_mut().find(|p| p.id == id) { p.designing = true; }
                Ok(id)
            })?;
            emit(json!({"kind": "presets"}));
            start_preset_design(&app, &id, &request).inspect_err(|_| {
                let _ = update(|s| { if let Some(p) = s.presets.iter_mut().find(|p| p.id == id) { p.designing = false; } Ok(()) });
                emit(json!({"kind": "presets"}));
            })?;
            Ok(json!({"presetId": id}))
        }
        "preset_prompt" => {
            let id = text(&args, "presetId").ok_or("缺少 presetId")?.to_string();
            let prompt = args["prompt"].as_str().unwrap_or_default().trim().to_string();
            update(|s| {
                let p = s.presets.iter_mut().find(|p| p.id == id).ok_or("预设不存在")?;
                p.prompt = prompt;
                Ok(())
            })?;
            emit(json!({"kind": "presets"}));
            let current = VIEWING.lock().unwrap().clone();
            refresh(&app, &current);
            Ok(json!({}))
        }
        "preset_delete" | "preset_rename" => {
            let id = text(&args, "presetId").ok_or("缺少 presetId")?.to_string();
            update(|s| {
                if op == "preset_delete" {
                    s.presets.retain(|p| p.id != id);
                    s.designs.retain(|d| !(d.main.is_empty() && d.target == id));
                } else {
                    let name = text(&args, "name").ok_or("缺少名称")?.to_string();
                    s.presets.iter_mut().find(|p| p.id == id).ok_or("预设不存在")?.name = name;
                }
                Ok(())
            })?;
            emit(json!({"kind": "presets"}));
            Ok(json!({}))
        }
        _ => Err(format!("未知布局操作：{op}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_by_id_name_or_active() {
        let mut t = ThreadLayouts {
            active: "a".into(),
            layouts: vec![Layout { id: "a".into(), name: "甲".into(), ..Default::default() }, Layout { id: "b".into(), name: "乙".into(), ..Default::default() }],
            ..Default::default()
        };
        assert_eq!(find(&mut t, None).unwrap().id, "a");
        assert_eq!(find(&mut t, Some("乙")).unwrap().id, "b");
        assert!(find(&mut t, Some("无")).is_none());
    }

    #[test]
    fn main_threads_get_no_layout_tools() {
        assert!(tools_for("no-such-thread").0.is_empty());
    }

    #[test]
    fn input_key_ignores_generated_data() {
        let mut ls = [Layout { id: "a".into(), schema: "s".into(), ..Default::default() }];
        let k = input_key(&ls, &[], "【回复】好", false);
        ls[0].data.insert("title".into(), json!("新"));
        assert_eq!(k, input_key(&ls, &[], "【回复】好", false));
        assert_ne!(k, input_key(&ls, &[], "【回复】好。", false));
    }

    #[test]
    fn bake_injects_data_after_head_not_header() {
        let mut d = Map::new();
        d.insert("t".into(), json!("</script>"));
        let out = bake("<html><HEAD lang=x><header></header></head></html>", &d);
        assert!(out.starts_with("<html><HEAD lang=x><script>window.__novaData="));
        assert!(!out.contains("\"</script>\""));
        assert!(bake("<div>x</div>", &d).starts_with("<script>"));
    }

    #[test]
    fn lenient_object_strips_fences_and_noise() {
        assert_eq!(lenient_object("```json\n{\"a\":1}\n```").unwrap()["a"], 1);
        assert_eq!(lenient_object("参数：{\"a\":{\"b\":2}} 完").unwrap()["a"]["b"], 2);
        assert!(lenient_object("{\"a\":\"x\"y\"}").is_none());
        assert!(lenient_object("[1]").is_none());
    }

    #[test]
    fn refresh_prompt_lists_every_layout_and_tail_keeps_end() {
        let ls = [Layout { id: "x1".into(), name: "甲".into(), schema: "title".into(), ..Default::default() },
            Layout { id: "y2".into(), name: "乙".into(), ..Default::default() }];
        let ps = [Preset { id: "p".into(), prompt: "突出风险".into(), ..Default::default() }];
        let mut ls = ls;
        ls[0].preset = "p".into();
        let p = refresh_prompt(&ls, &ps, "【回复】完成", false);
        assert!(p.contains("id=x1") && p.contains("id=y2") && p.contains("本轮已结束"));
        assert_eq!(p.matches("突出风险").count(), 1);
        assert_eq!(chars_tail("一二三四", 2), "三四");
    }
}
