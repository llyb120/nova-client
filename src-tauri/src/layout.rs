//! 自定义呈现布局：Agent 的回复除流式文本外，还可以呈现在用户设计的布局里。
//!
//! - 布局 = 一份沙箱 HTML（容器）；数据 = 一个 JSON 对象，由 `layout_update` 增量推送，页面里 `novaLayout.onData` 接收。
//! - `layout_create` 不由主会话自己写 HTML，而是交给并行的「设计会话」（Stage 会话，能看到主会话时间线）；
//!   设计会话用 `layout_submit` 提交成品，随后：主会话仍在运行则通知它去填数据；已结束则设计会话自己据主会话结果填数据。
//! - 会话布局按会话保存，预设布局全局保存，会话里可直接套用。

use crate::lyra::tools::Tool;
use crate::AppState;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use tauri::{AppHandle, Emitter, Manager};

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
    /// 设计会话写的数据结构说明，主会话据此调用 layout_update。
    schema: String,
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
    designing: bool,
}

#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
struct ThreadLayouts {
    active: String,
    layouts: Vec<Layout>,
}

/// 一个设计会话负责的目标：`main` 为空表示预设，`target` 是布局或预设 id。
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
    let _ = APP.set(app);
    // 重启后没有设计会话在跑：清掉遗留的「设计中」标记。
    let _ = update(|s| {
        s.presets.iter_mut().for_each(|p| p.designing = false);
        s.threads.values_mut().flat_map(|t| t.layouts.iter_mut()).for_each(|l| l.designing = false);
        Ok(())
    });
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

// ---------- 工具定义 ----------

const MAIN_NOTE: &str = "\n\n呈现布局：用户可以让你的回复呈现在自定义布局里，而不只是流式文本。layout_create 创建布局（由并行的设计会话完成，布局就绪后会通知你，不必等待）；就绪后用 layout_update 只发送数据来更新内容，数据里也可以放控制界面的变量（如主题、高亮项）。会话里已有布局（含用户预设）时直接 layout_update 即可；需要多个布局时可创建多个并用 activate 切换。";
const DESIGN_NOTE: &str = "\n\n你是布局设计会话：只负责设计呈现布局，完成后必须调用 layout_submit 提交。";

pub fn tools_for(thread_id: &str) -> (Vec<Tool>, &'static str) {
    let design = load().ok().and_then(|s| s.designs.iter().find(|d| d.thread == thread_id).cloned());
    let update_tool = || Tool {
        name: "layout_update",
        description: "更新布局的数据：只发送数据，不改布局结构。data 是对象，按键浅合并进布局当前数据（值为 null 删除该键）；键既可以是要展示的内容，也可以是控制界面的变量（如 theme、highlight）。layout 缺省为当前布局，可传布局 id 或名称；activate=true 同时切换到该布局；replace=true 用 data 整体替换。".into(),
        parameters: json!({"type": "object", "properties": {
            "data": {"type": "object", "description": "要合并的数据键值"},
            "layout": {"type": "string", "description": "布局 id 或名称，缺省为当前布局"},
            "activate": {"type": "boolean", "description": "是否切换到该布局"},
            "replace": {"type": "boolean", "description": "整体替换数据而不是合并"}
        }}),
    };
    match design {
        Some(d) => {
            let mut tools = vec![Tool {
                name: "layout_submit",
                description: "提交设计好的布局。html 必须是完整 HTML 文档，用 novaLayout.onData(fn) 或 data-bind 接收数据；schema 简明说明数据键、类型与含义（主会话据此发送数据）。".into(),
                parameters: json!({"type": "object", "properties": {
                    "html": {"type": "string", "description": "完整 HTML 文档（内联 CSS/JS，无外部资源）"},
                    "schema": {"type": "string", "description": "数据结构说明：有哪些键、类型、含义、示例"},
                    "name": {"type": "string", "description": "布局名称（可选）"}
                }, "required": ["html", "schema"]}),
            }];
            if !d.main.is_empty() {
                tools.push(update_tool());
            }
            (tools, DESIGN_NOTE)
        }
        None => (vec![
            Tool {
                name: "layout_create",
                description: "创建一个呈现布局（容器）。由并行的设计会话按 requirements 设计，立即返回；就绪后会通知你。给出 preset（预设布局名称或 id）则直接套用预设，不再设计。创建后用 layout_update 发送数据。".into(),
                parameters: json!({"type": "object", "properties": {
                    "name": {"type": "string", "description": "布局名称"},
                    "requirements": {"type": "string", "description": "布局用途、要呈现的内容类型、风格偏好；使用 preset 时可省略"},
                    "preset": {"type": "string", "description": "预设布局名称或 id"},
                    "activate": {"type": "boolean", "description": "是否切换到新布局，默认 true"}
                }, "required": ["name"]}),
            },
            update_tool(),
        ], MAIN_NOTE),
    }
}

// ---------- 工具执行 ----------

/// 工具调用方 → 会话，沿用员工工具的 owner 约定。
pub fn execute(name: &str, args: &Value, owner: &str) -> Result<Value, String> {
    let app = APP.get().ok_or("布局功能尚未就绪")?;
    let thread = crate::employee::owner_thread(&app.state::<AppState>(), owner).ok_or("找不到调用方会话")?;
    match name {
        "layout_create" => create(app, &thread, args),
        "layout_update" => update_data(&thread, args),
        "layout_submit" => submit(app, &thread, args),
        _ => Err(format!("未知布局工具：{name}")),
    }
}

fn find<'a>(t: &'a mut ThreadLayouts, key: Option<&str>) -> Option<&'a mut Layout> {
    let key = key.unwrap_or(&t.active).to_string();
    let index = t.layouts.iter().position(|l| l.id == key).or_else(|| t.layouts.iter().position(|l| l.name == key))?;
    t.layouts.get_mut(index)
}

fn listing(t: &ThreadLayouts) -> Value {
    json!(t.layouts.iter().map(|l| json!({"id": l.id, "name": l.name, "designing": l.designing, "active": l.id == t.active})).collect::<Vec<_>>())
}

fn create(app: &AppHandle, thread: &str, args: &Value) -> Result<Value, String> {
    let name = text(args, "name").unwrap_or("布局").to_string();
    let activate = args["activate"].as_bool().unwrap_or(true);
    if let Some(key) = text(args, "preset") {
        let result = update(|s| {
            let preset = s.presets.iter().find(|p| p.id == key || p.name == key).filter(|p| !p.html.is_empty()).cloned()
                .ok_or_else(|| format!("没有可用的预设「{key}」，可用：{}", s.presets.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join("、")))?;
            let t = s.threads.entry(thread.into()).or_default();
            let layout = Layout { id: new_id(), name: preset.name, html: preset.html, schema: preset.schema, ..Default::default() };
            let out = json!({"ok": true, "layoutId": layout.id, "status": "ready", "schema": layout.schema,
                "next": "布局已就绪，用 layout_update 发送数据。"});
            if activate || t.active.is_empty() { t.active = layout.id.clone(); }
            t.layouts.push(layout);
            Ok(out)
        })?;
        meta_changed(thread);
        return Ok(result);
    }
    let requirements = text(args, "requirements").ok_or("缺少 requirements（或给出 preset）")?.to_string();
    let id = update(|s| {
        let t = s.threads.entry(thread.into()).or_default();
        let id = new_id();
        t.layouts.push(Layout { id: id.clone(), name: name.clone(), designing: true, ..Default::default() });
        if activate || t.active.is_empty() { t.active = id.clone(); }
        Ok(id)
    })?;
    meta_changed(thread);
    start_design(app, thread, &id, &requirements)?;
    Ok(json!({"ok": true, "layoutId": id, "status": "designing",
        "next": "设计会话正在并行设计，就绪后会通知你并给出数据结构；请继续当前任务，不要等待，也不要重复创建。"}))
}

fn update_data(thread: &str, args: &Value) -> Result<Value, String> {
    // 设计会话（会话布局的）代主会话更新数据。
    let main = load()?.designs.iter().find(|d| d.thread == thread && !d.main.is_empty()).map_or(thread.to_string(), |d| d.main.clone());
    let data = match &args["data"] {
        Value::Object(map) => Some(map.clone()),
        Value::Null => None,
        _ => return Err("data 必须是对象".into()),
    };
    if data.is_none() && args["activate"].as_bool() != Some(true) {
        return Err("缺少 data".into());
    }
    let replace = args["replace"].as_bool().unwrap_or(false);
    let (layout_id, merged, active, keys) = update(|s| {
        let t = s.threads.get_mut(&main).filter(|t| !t.layouts.is_empty()).ok_or("当前会话还没有布局，先用 layout_create 创建")?;
        let key = text(args, "layout");
        let available = listing(t);
        let layout = find(t, key).ok_or_else(|| format!("没有这个布局，现有：{available}"))?;
        let mut keys = Vec::new();
        if let Some(data) = data {
            if replace { layout.data.clear(); }
            for (k, v) in data {
                keys.push(k.clone());
                if v.is_null() { layout.data.remove(&k); } else { layout.data.insert(k, v); }
            }
            if serde_json::to_string(&layout.data).map_or(0, |s| s.len()) > MAX_DATA {
                return Err("布局数据超过 1MB 上限，请精简".into());
            }
        }
        let (id, merged) = (layout.id.clone(), layout.data.clone());
        if args["activate"].as_bool() == Some(true) { t.active = id.clone(); }
        Ok((id, merged, t.active.clone(), keys))
    })?;
    emit(json!({"kind": "data", "threadId": main, "layoutId": layout_id, "data": merged, "active": active}));
    Ok(json!({"ok": true, "layoutId": layout_id, "updatedKeys": keys}))
}

fn submit(app: &AppHandle, thread: &str, args: &Value) -> Result<Value, String> {
    let design = load()?.designs.into_iter().find(|d| d.thread == thread).ok_or("只有布局设计会话能提交布局")?;
    let html = text(args, "html").ok_or("缺少 html")?;
    if html.len() > MAX_HTML { return Err("html 过大（上限 400KB），请精简".into()); }
    let schema = text(args, "schema").unwrap_or_default().to_string();
    let name = text(args, "name").map(str::to_string);
    let html = html.to_string();
    if design.main.is_empty() {
        update(|s| {
            let p = s.presets.iter_mut().find(|p| p.id == design.target).ok_or("预设已被删除")?;
            (p.html, p.schema, p.designing) = (html, schema, false);
            if let Some(name) = name { p.name = name; }
            Ok(())
        })?;
        emit(json!({"kind": "presets"}));
        return Ok(json!({"ok": true, "next": "预设布局已保存，结束即可。"}));
    }
    let (layout_name, layout_schema, data) = update(|s| {
        let t = s.threads.get_mut(&design.main).ok_or("会话的布局已被删除")?;
        let l = find(t, Some(&design.target)).ok_or("该布局已被删除")?;
        (l.html, l.schema, l.designing) = (html, schema, false);
        if let Some(name) = name { l.name = name; }
        Ok((l.name.clone(), l.schema.clone(), l.data.clone()))
    })?;
    meta_changed(&design.main);
    let state = app.state::<AppState>();
    if crate::running_by_id(&state, &design.main) {
        let note = format!("[布局系统通知] 布局「{layout_name}」（id={}）已设计完成/更新。数据结构：\n{layout_schema}\n请用 layout_update（layout=\"{}\"）把相关结果作为数据发送到该布局；布局只是容器，不要在回复里重复其内容。", design.target, design.target);
        crate::dispatch_prompt(app, design.main.clone(), note, Vec::new())?;
        return Ok(json!({"ok": true, "next": "已通知仍在运行的主会话去填数据，你的工作完成，结束即可。"}));
    }
    let filled = if data.is_empty() { "目前还没有数据" } else { "已有数据（可能需要按新结构更新）" };
    Ok(json!({"ok": true, "next": format!("主会话已结束，由你负责呈现：阅读引用的主会话时间线，用 layout_update（layout=\"{}\"）把主会话的结果按你的数据结构填进布局，然后结束。该布局{filled}。", design.target)}))
}

// ---------- 设计会话 ----------

fn start_design(app: &AppHandle, main: &str, layout_id: &str, request: &str) -> Result<(), String> {
    let state = app.state::<AppState>();
    let existing = load()?.designs.into_iter().find(|d| d.main == main && d.target == layout_id)
        .filter(|d| state.store.lock().unwrap().get(&d.thread).is_some());
    let thread = match existing {
        Some(d) => d.thread,
        None => {
            let id = crate::create_stage_thread(app.clone(), app.state::<AppState>(), main.into(), true)?.id;
            update(|s| {
                s.designs.retain(|d| !(d.main == main && d.target == layout_id));
                s.designs.push(Design { thread: id.clone(), main: main.into(), target: layout_id.into() });
                Ok(())
            })?;
            set_title(app, &id, "[Design] 布局设计");
            id
        }
    };
    let current = update(|s| Ok(s.threads.get(main).and_then(|t| t.layouts.iter().find(|l| l.id == layout_id)).cloned()))?;
    let prompt = design_prompt(request, current.as_ref().map(|l| (l.html.as_str(), l.schema.as_str())), false);
    crate::dispatch_prompt(app, thread, prompt, Vec::new())
}

fn start_preset_design(app: &AppHandle, preset_id: &str, request: &str) -> Result<(), String> {
    let state = app.state::<AppState>();
    let existing = load()?.designs.into_iter().find(|d| d.main.is_empty() && d.target == preset_id)
        .filter(|d| state.store.lock().unwrap().get(&d.thread).is_some());
    let thread = match existing {
        Some(d) => d.thread,
        None => {
            let (kind, model) = state.scene_model(crate::settings::Scene::Coding).or_else(|_| state.scene_model(crate::settings::Scene::Lightweight))?;
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
    let prompt = design_prompt(request, current.as_ref().map(|p| (p.html.as_str(), p.schema.as_str())), true);
    crate::dispatch_prompt(app, thread, prompt, Vec::new())
}

fn set_title(app: &AppHandle, id: &str, title: &str) {
    let state = app.state::<AppState>();
    let mut store = state.store.lock().unwrap();
    if let Some(t) = store.get_mut(id) {
        t.title = title.into();
        store.save();
    }
    drop(store);
    let _ = app.emit(crate::acp::EV_THREADS, json!({}));
}

const DESIGN_RULES: &str = "[布局设计任务]\n你是布局设计师，设计一个「呈现布局」：在会话旁的沙箱 iframe 里渲染的完整 HTML 页面（无网络，不能引用任何外部资源，CSS/JS 全部内联）。\n约定：\n- 数据由页面运行时注入：`novaLayout.onData((data, changedKeys) => {...})` 在加载时和每次数据更新时调用，data 是完整的当前数据对象；`novaLayout.data` 随时可读。运行时已自动提供，不要自己定义。\n- 另有零代码绑定：元素加 `data-bind=\"键.子键\"` 自动填文本，`data-bind-html=\"键\"` 填 HTML。\n- 布局只是容器：不要写死具体内容，由 data 驱动；没有数据时显示友好的空态。data 里也可能有控制界面的变量（如 theme、highlight、title），按需读取并调整界面。\n- 自适应宽高，配色用 `prefers-color-scheme` 或 data.theme 兼顾明暗；交互（折叠、切换标签等）可在页面内用 JS 完成。\n- 完成后必须调用 layout_submit 提交：html（完整文档）、schema（数据键、类型、含义与示例，主会话据此发送数据）、name。\n";

fn design_prompt(request: &str, current: Option<(&str, &str)>, preset: bool) -> String {
    let mut out = String::from(DESIGN_RULES);
    if preset {
        out.push_str("这是一个预设布局：通用、可复用，不依赖具体会话。\n");
    }
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
            Ok(json!({"active": t.map(|t| t.active.clone()).unwrap_or_default(), "layouts": t.map(|t| &t.layouts).cloned().unwrap_or_default(), "presets": s.presets}))
        }
        "design" => {
            let thread = need_thread()?;
            let request = text(&args, "text").ok_or("缺少设计要求")?.to_string();
            let make_new = args["new"].as_bool().unwrap_or(false);
            let id = update(|s| {
                let t = s.threads.entry(thread.clone()).or_default();
                let target = if make_new { None } else { find(t, key.as_deref()).map(|l| l.id.clone()) };
                let id = match target {
                    Some(id) => id,
                    None => {
                        let id = new_id();
                        let name = format!("布局 {}", t.layouts.len() + 1);
                        t.layouts.push(Layout { id: id.clone(), name, ..Default::default() });
                        id
                    }
                };
                if let Some(l) = find(t, Some(&id)) { l.designing = true; }
                t.active = id.clone();
                Ok(id)
            })?;
            meta_changed(&thread);
            start_design(&app, &thread, &id, &request).inspect_err(|_| {
                let _ = update(|s| { if let Some(l) = s.threads.get_mut(&thread).and_then(|t| find(t, Some(&id))) { l.designing = false; } Ok(()) });
                meta_changed(&thread);
            })?;
            Ok(json!({"layoutId": id}))
        }
        "switch" | "delete" | "rename" | "use_preset" | "save_preset" | "clear_data" => {
            let thread = need_thread()?;
            let out = update(|s| {
                let t = s.threads.entry(thread.clone()).or_default();
                match op.as_str() {
                    "switch" => { let id = find(t, key.as_deref()).ok_or("没有这个布局")?.id.clone(); t.active = id; }
                    "delete" => {
                        let id = find(t, key.as_deref()).ok_or("没有这个布局")?.id.clone();
                        t.layouts.retain(|l| l.id != id);
                        if t.active == id { t.active = t.layouts.last().map(|l| l.id.clone()).unwrap_or_default(); }
                        s.designs.retain(|d| !(d.main == thread && d.target == id));
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
                        t.layouts.push(Layout { id: id.clone(), name: p.name, html: p.html, schema: p.schema, ..Default::default() });
                        t.active = id;
                    }
                    "save_preset" => {
                        let l = find(t, key.as_deref()).filter(|l| !l.html.is_empty()).ok_or("布局还没有设计完成")?.clone();
                        s.presets.push(Preset { id: new_id(), name: l.name, html: l.html, schema: l.schema, designing: false });
                    }
                    _ => {
                        // clear_data：清空布局数据。
                        if let Some(l) = find(t, key.as_deref()) { l.data.clear(); }
                    }
                }
                Ok(json!({}))
            })?;
            meta_changed(&thread);
            if op == "save_preset" { emit(json!({"kind": "presets"})); }
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
        };
        assert_eq!(find(&mut t, None).unwrap().id, "a");
        assert_eq!(find(&mut t, Some("乙")).unwrap().id, "b");
        assert!(find(&mut t, Some("无")).is_none());
    }

    #[test]
    fn tool_sets_differ_for_main_and_unknown_threads() {
        // 未登记为设计会话的会话得到创建/更新工具。
        let names: Vec<_> = tools_for("no-such-thread").0.iter().map(|t| t.name).collect();
        assert_eq!(names, ["layout_create", "layout_update"]);
    }
}
