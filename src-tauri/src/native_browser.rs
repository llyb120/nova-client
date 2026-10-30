//! Chrome automation engine, using the authenticated extension transport.
use crate::AppState;
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::Duration,
};
use tauri::{AppHandle, Manager};

static APP: OnceLock<AppHandle> = OnceLock::new();
tokio::task_local! { static CONTROL_TAB: String; static SHOT_OWNER: String; }
const PAGE_SCRIPT: &str = include_str!("native_browser_page.js");

struct Session {
    thread_id: String,
    active_tab: String,
    cancel: Arc<AtomicBool>,
}

#[derive(Default)]
struct BrowserState {
    gate: tokio::sync::Mutex<()>,
    frame_sessions: Mutex<std::collections::HashMap<String, String>>,
    observations: Mutex<std::collections::HashMap<String, Observation>>,
}

fn active_label(_app: &AppHandle) -> Result<String, String> {
    CONTROL_TAB.try_with(Clone::clone).map_err(|_| "缺少 Chrome tabTag".into())
}

// Keep both the renderer surface and Chrome's WebP encoder within their limits.
const MAX_FULL_PAGE_DEVICE_PX: f64 = 16383.;

pub(crate) fn init(app: &AppHandle) {
    app.manage(BrowserState::default());
    crate::chrome_browser::init(app);
    let _ = APP.set(app.clone());
}

fn check(_app: &AppHandle, current: &Session) -> Result<(), String> {
    if current.cancel.load(Ordering::SeqCst) {
        return Err("已停止浏览器控制".into());
    }
    Ok(())
}

fn normalized_url(raw: &str) -> Result<tauri::Url, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("请输入网址".into());
    }
    let value = if raw.contains("://") {
        raw.to_string()
    } else if raw.starts_with("localhost")
        || raw.starts_with("127.0.0.1")
        || raw.starts_with("[::1]")
    {
        format!("http://{raw}")
    } else {
        format!("https://{raw}")
    };
    let url = tauri::Url::parse(&value).map_err(|_| "网址无效")?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("仅支持不含凭据的 HTTP(S) 网址".into());
    }
    Ok(url)
}

async fn cdp(
    app: &AppHandle,
    method: &str,
    params: Value,
    session_id: Option<&str>,
    cancel: Option<Arc<AtomicBool>>,
) -> Result<Value, String> {
    if cancel
        .as_ref()
        .is_some_and(|flag| flag.load(Ordering::SeqCst))
    {
        return Err("已停止".into());
    }
    let label = active_label(app)?;
    let tag = label.rsplit(':').next().ok_or("缺少 Chrome tabTag")?;
    crate::chrome_browser::request(
        app,
        "cdp",
        json!({"tabTag":tag,"method":method,"params":params,"sessionId":session_id}),
    ).await
}

#[derive(Clone)]
struct Frame {
    id: String,
    parent: Option<String>,
    session: Option<String>,
    context: i64,
}

async fn evaluate(app: &AppHandle, frame: &Frame, expression: String) -> Result<Value, String> {
    let result = cdp(app, "Runtime.evaluate", json!({"expression":expression,"contextId":frame.context,"returnByValue":true,"awaitPromise":true}), frame.session.as_deref(), None).await?;
    if result.get("exceptionDetails").is_some() {
        return Err(result["exceptionDetails"]["exception"]["description"]
            .as_str()
            .unwrap_or("页面脚本执行失败")
            .into());
    }
    Ok(result["result"]["value"].clone())
}

#[derive(Clone)]
struct Observation {
    frames: Vec<Frame>,
    pages: Value,
    id: String,
    captured: std::time::Instant,
    screenshot: bool,
    full_page: bool,
    images: Vec<ScreenshotImage>,
}

#[derive(Clone)]
struct ScreenshotImage {
    id: String,
    path: std::path::PathBuf,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    pixels: (u32, u32),
    delivered: Option<crate::visual_guard::Delivered>,
}
impl ScreenshotImage {
    fn point(&self, x: f64, y: f64) -> Result<(f64, f64), String> {
        if !x.is_finite() || !y.is_finite() || x < 0. || y < 0.
            || x >= self.pixels.0 as f64 || y >= self.pixels.1 as f64 {
            return Err("图片坐标越界，请使用该 imageId 的 pixelWidth/pixelHeight".into());
        }
        Ok((self.x + x * self.width / self.pixels.0 as f64,
            self.y + y * self.height / self.pixels.1 as f64))
    }
}
fn webp_dimensions(bytes: &[u8]) -> Result<(u32, u32), String> {
    let (w, h) = xcap::image::ImageReader::with_format(std::io::Cursor::new(bytes), xcap::image::ImageFormat::WebP)
        .into_dimensions().map_err(|e| format!("截图不是有效的 WebP：{e}"))?;
    if w == 0 || h == 0 || w as u64 * h as u64 > 64_000_000 { return Err("截图像素超出安全预算，请裁剪或缩小截图".into()); }
    Ok((w, h))
}

fn screenshot_pixels(data:&str, edge:u32) -> Result<xcap::image::RgbaImage,String> {
    use base64::Engine;
    let bytes=base64::engine::general_purpose::STANDARD.decode(data).map_err(|e|e.to_string())?;
    webp_dimensions(&bytes)?;
    // CDP implementations do not all produce the same pixel dimensions for
    // clip.scale=1. Enforce the budget against the actual WebP, not guessed DPR.
    Ok(resize_screenshot(xcap::image::load_from_memory(&bytes).map_err(|e|e.to_string())?,edge))
}

fn resize_screenshot(image: xcap::image::DynamicImage, edge: u32) -> xcap::image::RgbaImage {
    if edge > 0 && image.width().max(image.height()) > edge {
        image.resize(edge, edge, xcap::image::imageops::FilterType::Triangle)
    } else { image }.into_rgba8()
}

fn deliver_screenshot(observation: &mut Observation, previous: Option<&Observation>, stream: Option<String>,
    image: xcap::image::RgbaImage, path: std::path::PathBuf, (x, y, width, height): (f64, f64, f64, f64)) -> Result<Vec<Value>, String> {
    let page = &observation.pages["pages"][0];
    // Images the model addresses by pixel must fit its vision cap; otherwise it sees a second downscale.
    let scale = crate::lyra::bridge::vision_scale(image.width() as f64, image.height() as f64);
    let image = if scale < 1.0 {
        xcap::image::imageops::resize(&image, ((image.width() as f64 * scale).round() as u32).max(1),
            ((image.height() as f64 * scale).round() as u32).max(1), xcap::image::imageops::FilterType::Triangle)
    } else { image };
    // Viewports and normal full-page tiles share Jianlai's unchanged-frame reuse. Crops/exports stay standalone.
    // Match each document tile independently; identical pixel sizes alone do not prove identical coordinates.
    let prior = previous.filter(|p| p.captured.elapsed() <= Duration::from_secs(180) && p.full_page == observation.full_page
        && page["stamp"].is_string() && p.pages["pages"][0]["stamp"] == page["stamp"]
        && p.pages["pages"][0]["document"] == page["document"])
        .and_then(|p| p.images.iter().filter(|i| (i.x,i.y,i.width,i.height) == (x,y,width,height))
            .filter_map(|i| i.delivered.as_ref()).find(|d| Some(&d.stream) == stream.as_ref()));
    let id = format!("{}-{}", observation.id, observation.images.len());
    let pixels = image.dimensions();
    let mut entry = json!({"imageId":id,"snapshotId":observation.id,"x":x,"y":y,"width":width,"height":height,
        "pixelWidth":pixels.0,"pixelHeight":pixels.1});
    let delivered = crate::visual_guard::deliver_frame(&mut entry, prior, stream, image, None, path.clone())?;
    let path = delivered.as_ref().map_or(path, |p| p.path.clone());
    observation.images.push(ScreenshotImage { id, path, x, y, width, height, pixels, delivered });
    Ok(vec![entry])
}
async fn capture_viewport(app:&AppHandle, viewport:&Value, x:f64, y:f64, w:f64, h:f64) -> Result<xcap::image::DynamicImage,String> {
    use base64::Engine;
    // No CDP clip/scale: capture the existing surface without a temporary render
    // size/scale override. Crop and downsample locally, also for pixel preflight.
    // Chromium uses lossless WebP at quality 100; keep compression noise out of pixel guards.
    let shot=cdp(app,"Page.captureScreenshot",json!({"format":"webp","quality":100,"fromSurface":true,"captureBeyondViewport":false}),None,None).await?;
    let bytes=base64::engine::general_purpose::STANDARD.decode(shot["data"].as_str().ok_or("截图为空")?).map_err(|e|e.to_string())?;
    let pixels=webp_dimensions(&bytes)?;
    let vw=viewport["width"].as_f64().filter(|v|*v>0.).ok_or("无效视口")?;
    let vh=viewport["height"].as_f64().filter(|v|*v>0.).ok_or("无效视口")?;
    let (sx,sy)=(pixels.0 as f64/vw,pixels.1 as f64/vh);
    let left=(x*sx).round().clamp(0.,pixels.0 as f64) as u32;
    let top=(y*sy).round().clamp(0.,pixels.1 as f64) as u32;
    let right=((x+w)*sx).round().clamp(0.,pixels.0 as f64) as u32;
    let bottom=((y+h)*sy).round().clamp(0.,pixels.1 as f64) as u32;
    if right<=left || bottom<=top {return Err("截图范围为空".into());}
    Ok(xcap::image::load_from_memory(&bytes).map_err(|e|e.to_string())?.crop_imm(left,top,right-left,bottom-top))
}

// Inner scrollers (TAPD/Jira tables) clip rows that captureBeyondViewport never reaches: grow the
// emulated viewport until they stop overflowing. Only viewport-sized scrollers grow, so a round
// that gains nothing is reverted instead of padding blank space.
// ponytail: 3 rounds and the GPU texture limit; taller content is reported as incomplete.
async fn expand_inner_scroll(app:&AppHandle, frame:&Frame, dpr:f64, target:Option<&str>) -> Result<Option<(f64,bool)>,String> {
    let script=format!("__novaWebview.innerScrollOverflow({})",json!(target));
    let overflow=|| async { Ok::<f64,String>(evaluate(app,frame,script.clone()).await?.as_f64().unwrap_or(0.)) };
    let mut over=overflow().await?;
    if over<=1. {return Ok(None);}
    let size=evaluate(app,frame,"[innerWidth,innerHeight]".into()).await?;
    let width=size[0].as_f64().filter(|v|*v>0.).ok_or("无效视口")?;
    let base=size[1].as_f64().filter(|v|*v>0.).ok_or("无效视口")?;
    let limit=(MAX_FULL_PAGE_DEVICE_PX/dpr).floor();
    let resize=|height:f64| cdp(app,"Emulation.setDeviceMetricsOverride",json!({"width":width,"height":height,"deviceScaleFactor":0,"mobile":false}),None,None);
    let settle=|| evaluate(app,frame,"__novaWebview.settle(300,1500)".into());
    let mut height=base;
    for _ in 0..3 {
        let next=(height+over).ceil().min(limit);
        if next<=height {break;}
        resize(next).await?;
        settle().await?;
        let after=overflow().await?;
        if after>=over-1. {
            if height==base {cdp(app,"Emulation.clearDeviceMetricsOverride",json!({}),None,None).await?;} else {resize(height).await?;}
            settle().await?;
            break;
        }
        (height,over)=(next,after);
        if over<=1. {break;}
    }
    Ok((height>base).then_some((height,over<=1.)))
}

async fn observe(app: &AppHandle, scope: &str) -> Result<Observation, String> {
    fn collect(tree: &Value, session: Option<String>, frames: &mut Vec<Frame>) {
        if let Some(id) = tree["frame"]["id"].as_str() {
            frames.push(Frame {
                id: id.into(),
                parent: tree["frame"]["parentId"].as_str().map(str::to_owned),
                session: session.clone(),
                context: 0,
            });
        }
        if let Some(children) = tree["childFrames"].as_array() {
            for child in children {
                collect(child, session.clone(), frames);
            }
        }
    }
    let tree = cdp(app, "Page.getFrameTree", json!({}), None, None).await?;
    let mut frames = Vec::new();
    collect(&tree["frameTree"], None, &mut frames);
    let targets = cdp(app, "Target.getTargets", json!({}), None, None).await?;
    let mut gaps = Vec::new();
    if let Some(targets) = targets["targetInfos"].as_array() {
        app.state::<BrowserState>()
            .frame_sessions
            .lock()
            .unwrap()
            .retain(|key, _| {
                targets
                    .iter()
                    .any(|t| t["targetId"].as_str() == key.rsplit(':').next())
            });
        for target in targets.iter().filter(|t| t["type"] == "iframe").take(12) {
            let id = target["targetId"].as_str().ok_or("框架没有 ID")?;
            let cache_key = format!("{}:{id}", active_label(app)?);
            let cached = app
                .state::<BrowserState>()
                .frame_sessions
                .lock()
                .unwrap()
                .get(&cache_key)
                .cloned();
            let sid = if let Some(sid) = cached {
                sid
            } else {
                let attached = cdp(
                    app,
                    "Target.attachToTarget",
                    json!({"targetId":id,"flatten":true}),
                    None,
                    None,
                )
                .await?;
                let sid = attached["sessionId"]
                    .as_str()
                    .ok_or("无法连接框架")?
                    .to_string();
                app.state::<BrowserState>()
                    .frame_sessions
                    .lock()
                    .unwrap()
                    .insert(cache_key.clone(), sid.clone());
                sid
            };
            match cdp(app, "Page.getFrameTree", json!({}), Some(&sid), None).await {
                Ok(tree) => {
                    let mut candidates = Vec::new();
                    collect(&tree["frameTree"], Some(sid), &mut candidates);
                    // getTargets includes background tabs. Only this page's descendants belong in its observation.
                    if candidates.first().is_some_and(|child| {
                        child
                            .parent
                            .as_ref()
                            .is_some_and(|parent| frames.iter().any(|f| &f.id == parent))
                            || frames.iter().any(|f| f.id == child.id)
                    }) {
                        for child in candidates {
                            frames.retain(|f| f.id != child.id);
                            frames.push(child);
                        }
                    }
                }
                Err(e) => {
                    app.state::<BrowserState>()
                        .frame_sessions
                        .lock()
                        .unwrap()
                        .remove(&cache_key);
                    gaps.push(e);
                }
            }
        }
    }
    let mut pages = Vec::new();
    let mut ready = Vec::new();
    if frames.len() > 12 {
        gaps.push("框架超过12个，本次只读取前12个，不能视为完整页面".into());
    }
    for mut frame in frames.into_iter().take(12) {
        let result = async {
            let world = cdp(
                app,
                "Page.createIsolatedWorld",
                json!({"frameId":frame.id,"worldName":"nova-browser-control"}),
                frame.session.as_deref(),
                None,
            )
            .await?;
            frame.context = world["executionContextId"]
                .as_i64()
                .ok_or("无效页面上下文")?;
            let nonce = uuid::Uuid::new_v4().simple().to_string();
            evaluate(
                app,
                &frame,
                format!("{PAGE_SCRIPT}; __novaWebview.observe({},20000,{})", json!(nonce), json!(scope)),
            )
            .await
        }
        .await;
        match result {
            Ok(mut page) => {
                page["frame"] = json!(ready.len());
                pages.push(page);
                ready.push(frame);
            }
            Err(error) if frame.parent.is_none() && frame.session.is_none() => return Err(format!("主页面尚未就绪：{error}")),
            Err(error) => gaps.push(error),
        }
    }
    if ready.is_empty() {
        return Err("页面尚未就绪，请稍后重新观察".into());
    }
    Ok(Observation {
        frames: ready,
        pages: json!({"pages":pages,"coverageGaps":gaps,"domCoordinates":"item.point is frame-local viewport coordinates; item.documentRect is frame-local document coordinates. Prefer frame/ref for iframe targets."}),
        id: uuid::Uuid::new_v4().to_string(),
        captured: std::time::Instant::now(),
        screenshot: false,
        full_page: false,
        images: Vec::new(),
    })
}

#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    ClickAt {
        x: f64,
        y: f64,
        #[serde(default)] button: Option<String>,
        #[serde(default)] click_count: Option<u8>,
    },
    Move {
        x: f64,
        y: f64,
    },
    Drag {
        x: f64,
        y: f64,
        to_x: f64,
        to_y: f64,
        #[serde(default)] duration_ms: Option<u64>,
    },
    Type {
        text: String,
    },
    ScrollAt {
        x: f64,
        y: f64,
        delta: i32,
        #[serde(default)] delta_x: Option<i32>,
    },
    Click {
        frame: usize,
        r#ref: String,
        #[serde(default)] button: Option<String>,
        #[serde(default)] click_count: Option<u8>,
    },
    Fill {
        frame: usize,
        r#ref: String,
        text: String,
    },
    Upload {
        frame: usize,
        selector: String,
        files: Vec<String>,
    },
    Press {
        key: String,
    },
    Scroll {
        frame: usize,
        r#ref: Option<String>,
        delta: i32,
        #[serde(default)] delta_x: Option<i32>,
    },
    Wait {
        ms: u64,
    },
}

fn parse_action(text: &str) -> Result<Action, String> {
    let text = text.trim();
    let text = text
        .strip_prefix("```json")
        .or_else(|| text.strip_prefix("```"))
        .unwrap_or(text)
        .trim();
    let text = text.strip_suffix("```").unwrap_or(text).trim();
    let value = serde_json::from_str::<Value>(text).map(normalize_action);
    value.and_then(serde_json::from_value).map_err(|e| format!("动作 JSON 无效：{e}。DOM 点击用 action={{\"action\":\"click\",\"frame\":0,\"ref\":\"最新引用\"}}；图片坐标点击用 action={{\"action\":\"click_at\",\"x\":100,\"y\":100}}，imageId 放在工具顶层且与 snapshotId 来自同次截图；按键仅用 action={{\"action\":\"press\",\"key\":\"Enter\"}}，不传 frame/ref。"))
}

/// Repairs the common harmless slips of weaker models instead of bouncing the call: a missing
/// `frame` on DOM actions means the main frame, and keys/waits/typing ignore frame/ref.
fn normalize_action(mut value: Value) -> Value {
    let Some(map) = value.as_object_mut() else { return value };
    if !map.contains_key("action") { if let Some(kind) = map.remove("type").filter(Value::is_string) { map.insert("action".into(), kind); } }
    match map.get("action").and_then(Value::as_str).unwrap_or_default() {
        "click" | "fill" | "scroll" | "upload" => { map.entry("frame").or_insert(json!(0)); }
        "press" | "wait" | "type" | "move" | "click_at" | "drag" | "scroll_at" => { map.remove("frame"); map.remove("ref"); }
        _ => {}
    }
    value
}

fn validate_action(action: &Action) -> Result<(), String> {
    match action {
        Action::Click { button, click_count, .. } | Action::ClickAt { button, click_count, .. } => {
            if button.as_deref().is_some_and(|b| !matches!(b, "left"|"right"|"middle")) || click_count.is_some_and(|n| !(1..=2).contains(&n)) { return Err("button 或 click_count 无效".into()); }
        }
        Action::Upload { selector, files, .. } if selector.trim().is_empty() || selector.len() > 2000 || files.is_empty() || files.len() > 16 => return Err("upload需要明确selector及1–16个文件".into()),
        Action::Type { text } | Action::Fill { text, .. } if text.len() > 16000 => return Err("输入过长".into()),
        Action::Wait { ms } if *ms > 2000 => return Err("wait.ms 必须为 0–2000".into()),
        Action::Scroll { delta, .. } | Action::ScrollAt { delta, .. } if delta.unsigned_abs() > 1200 => return Err("单次滚动不能超过1200像素".into()),
        Action::Press { key } if key_spec(key).is_none() => return Err("不支持的按键".into()),
        _ => (),
    }
    match action {
        Action::ClickAt { x,y,.. } | Action::Move { x,y } | Action::ScrollAt { x,y,.. } | Action::Drag { x,y,.. }
            if !x.is_finite() || !y.is_finite() || *x<0. || *y<0. => return Err("坐标无效".into()),
        _ => (),
    }
    if let Action::Drag { to_x,to_y,duration_ms,.. } = action {
        if !to_x.is_finite() || !to_y.is_finite() || *to_x<0. || *to_y<0. || duration_ms.is_some_and(|t| !(80..=1500).contains(&t)) { return Err("拖动终点或 duration_ms 无效".into()); }
    }
    if let Action::ScrollAt { delta_x:Some(x), .. } | Action::Scroll { delta_x:Some(x), .. } = action { if x.unsigned_abs()>1200 { return Err("delta_x 超过1200像素".into()); } }
    Ok(())
}

fn upload_files(root: &Path, files: &[String]) -> Result<Vec<String>, String> {
    let root = dunce::canonicalize(root).map_err(|e| format!("上传目录不可用：{e}"))?;
    files.iter().map(|file| {
        if file.trim().is_empty() { return Err("上传路径不能为空".into()); }
        let path = dunce::canonicalize(root.join(file)).map_err(|e| format!("上传文件不可用：{e}"))?;
        if !path.starts_with(&root) || !path.is_file() {
            return Err("只能上传当前工作目录内的普通文件；目录外文件请先由用户明确授权复制进来".into());
        }
        std::fs::File::open(&path).map_err(|e| format!("上传文件不可读：{e}"))?;
        path.to_str().map(str::to_owned).ok_or_else(|| "上传路径必须为有效Unicode".into())
    }).collect()
}

#[cfg(test)]
#[test]
fn upload_rejects_paths_outside_working_directory() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("project");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("input.txt"), "upload").unwrap();
    std::fs::write(dir.path().join("outside.txt"), "private").unwrap();
    assert_eq!(upload_files(&root, &["input.txt".into()]).unwrap().len(), 1);
    for path in ["", ".", "missing.txt", "../outside.txt"] {
        assert!(upload_files(&root, &[path.into()]).is_err());
    }
    assert!(upload_files(&root, &[dir.path().join("outside.txt").to_str().unwrap().into()]).is_err());
    let action = parse_action(r#"{"action":"upload","selector":"input[type=file]","files":["input.txt"]}"#).unwrap();
    assert!(matches!(action, Action::Upload { frame: 0, .. }));
    assert!(parse_actions(&json!({"action":{"action":"upload","selector":"input[type=file]","files":[]}}), 16).is_err());
}

fn parse_actions(args: &Value, max_actions: usize) -> Result<Vec<Action>, String> {
    if !args["action"].is_null() && !args["actions"].is_null() { return Err("action 和 actions 不能同时提供：单步仅传 action，多步仅传 actions；删除另一个字段。本批次未执行。".into()); }
    let values = if let Some(values)=args["actions"].as_array() { values.clone() } else { vec![args["action"].clone()] };
    if values.is_empty() || values.len()>max_actions { return Err(format!("每批需要1–{max_actions}个确定动作")); }
    // Models sometimes pass the action as a JSON string; parse it as-is instead of double-quoting it.
    values.into_iter().map(|value| { let action=parse_action(&value.as_str().map(str::to_string).unwrap_or_else(|| value.to_string()))?; validate_action(&action)?; Ok(action) }).collect()
}

/// Live refs are revalidated before input. Keyboard steps may follow an explicitly targeted field.
fn refs_in(observation:&Observation, actions:&[Action]) -> bool {
    let (mut has_ref, mut field_focus) = (false, false);
    actions.iter().all(|action| match action {
        Action::Click{r#ref,..}|Action::Fill{r#ref,..}|Action::Scroll{r#ref:Some(r#ref),..} => {
            let item=observation.pages["pages"].as_array().and_then(|pages| pages.iter().filter_map(|p|p["items"].as_array()).flatten().find(|i|i["ref"].as_str()==Some(r#ref.as_str())));
            has_ref=true;
            if matches!(action,Action::Click{..}|Action::Fill{..}) {field_focus=item.is_some_and(|i|i["editable"]==true);}
            item.is_some()
        },
        Action::Type{..}|Action::Press{..} => field_focus,
        Action::Wait{..} => true,
        _ => false,
    }) && has_ref
}

fn preflight(observation:&Observation, action:&mut Action, image_id:Option<&str>) -> Result<(),String> {
    match action {
        Action::Click{frame,r#ref,..}|Action::Fill{frame,r#ref,..}|Action::Scroll{frame,r#ref:Some(r#ref),..}=>{
            let has=|f:usize| observation.pages["pages"][f]["items"].as_array().is_some_and(|items|items.iter().any(|item|item["ref"].as_str()==Some(r#ref.as_str())));
            if !has(*frame) {
                // ref 带文档前缀、跨 frame 唯一：模型常照示例填 frame 0，按 ref 实际所在 frame 纠正。
                *frame=(0..observation.frames.len()).find(|f|has(*f)).ok_or("ref 不在当前快照的任何 frame 中；ref 必须原样复制 elements 行首的 ref，不能拼造。这不等于快照过期；先核对已有观察中的真实 ref，不要直接重试或无故重新截图")?;
            }
        }
        Action::Upload{frame,..} if observation.frames.get(*frame).is_none()=>return Err("upload.frame 不在当前观察中".into()),
        Action::Scroll{frame,..} if *frame!=0=>return Err("子框架滚动需要明确 ref".into()),
        Action::ClickAt{x,y,..}|Action::Move{x,y}|Action::Drag{x,y,..}|Action::ScrollAt{x,y,..}=>{
            if !observation.screenshot {return Err(format!("坐标操作需要截图：当前 snapshotId={} 没有图片。调用 screenshot 后同时使用它返回的新 snapshotId 和 imageId；不要将旧 imageId 与新 DOM 快照混用。", observation.id));}
            let (cx,cy)=image_point(observation,*x,*y,image_id)?;
            if !observation.images.iter().any(|image|cx>=image.x&&cy>=image.y&&cx<image.x+image.width&&cy<image.y+image.height) {return Err("坐标不在返回的图片中".into());}
            if let Action::Drag{to_x,to_y,..}=action {
                if observation.full_page {return Err("拖动需要 fullPage=false 的视口截图".into());}
                let (tx,ty)=image_point(observation,*to_x,*to_y,image_id)?;
                if !observation.images.iter().any(|image|tx>=image.x&&ty>=image.y&&tx<image.x+image.width&&ty<image.y+image.height) {return Err("拖动终点不在图片中".into());}
            }
        }
        _=>(),
    }
    Ok(())
}

async fn point(
    app: &AppHandle,
    observation: &Observation,
    index: usize,
    reference: &str,
    mode: &str,
) -> Result<Value, String> {
    let frame = observation
        .frames
        .get(index)
        .ok_or("模型选择了不存在的 frame")?;
    // Scroll ancestor frames before measuring the final point.
    let mut chain = Vec::new();
    let mut child = frame;
    while let Some(parent_id) = &child.parent {
        let parent = observation
            .frames
            .iter()
            .find(|f| &f.id == parent_id)
            .ok_or("父框架不可访问")?;
        let owner = cdp(
            app,
            "DOM.getFrameOwner",
            json!({"frameId":child.id}),
            parent.session.as_deref(),
            None,
        )
        .await?;
        let node = cdp(
            app,
            "DOM.resolveNode",
            json!({"backendNodeId":owner["backendNodeId"],"executionContextId":parent.context}),
            parent.session.as_deref(),
            None,
        )
        .await?;
        let object = node["object"]["objectId"]
            .as_str()
            .ok_or("框架元素不可访问")?
            .to_owned();
        chain.push((parent, object));
        child = parent;
        if chain.len() > 12 {
            return Err("框架嵌套过深".into());
        }
    }
    for (parent, object) in chain.iter().rev() {
        cdp(app, "Runtime.callFunctionOn", json!({"objectId":object,"functionDeclaration":"async function(){const r=this.getBoundingClientRect();if(r.bottom<=0||r.top>=innerHeight||r.right<=0||r.left>=innerWidth){this.scrollIntoView({block:'nearest',inline:'nearest',behavior:'instant'});await new Promise(r=>setTimeout(r,50));}}","awaitPromise":true}), parent.session.as_deref(), None).await?;
    }
    let mut value = evaluate(
        app,
        frame,
        format!("__novaWebview.prepare({},{})", json!(reference), json!(mode)),
    )
    .await?;
    value["localRect"] = value["rect"].clone();
    for (parent, object) in &chain {
        let result = cdp(app, "Runtime.callFunctionOn", json!({"objectId":object,"returnByValue":true,"arguments":[{"value":value}],"functionDeclaration":"function(p){const r=this.getBoundingClientRect();for(let e=this;e;e=e.parentElement||e.getRootNode()?.host){const t=getComputedStyle(e).transform;if(t!=='none'){const m=new DOMMatrixReadOnly(t);if(!m.is2D||m.a<=0||m.d<=0||Math.abs(m.b)>.00001||Math.abs(m.c)>.00001)throw Error('框架旋转/透视不能安全映射');}}const sx=r.width/this.offsetWidth,sy=r.height/this.offsetHeight,x=r.x+(this.clientLeft+p.x)*sx,y=r.y+(this.clientTop+p.y)*sy;let hit=this.ownerDocument.elementFromPoint(x,y);while(hit?.shadowRoot){const next=hit.shadowRoot.elementFromPoint(x,y);if(!next||next===hit)break;hit=next;}if(hit!==this)throw Error('框架被遮挡');return {x,y,rect:{x:r.x+(this.clientLeft+p.rect.x)*sx,y:r.y+(this.clientTop+p.rect.y)*sy,width:p.rect.width*sx,height:p.rect.height*sy}}}"}), parent.session.as_deref(), None).await?;
        if result.get("exceptionDetails").is_some() {
            return Err("框架坐标无法安全映射".into());
        }
        value["rect"] = result["result"]["value"]["rect"].clone();
        value["x"] = json!(result["result"]["value"]["x"]
            .as_f64()
            .ok_or("无效框架坐标")?);
        value["y"] = json!(result["result"]["value"]["y"]
            .as_f64()
            .ok_or("无效框架坐标")?);
    }
    for (parent, object) in &chain {
        let _ = cdp(app, "Runtime.releaseObject", json!({"objectId":object}), parent.session.as_deref(), None).await;
    }
    Ok(value)
}

// Retained outside the cancellable future. Timeouts must release held inputs,
// report uncertainty, and must never make a possibly executed click retryable.
#[derive(Default)]
struct InputProgress {
    attempted: bool,
    focus: Option<(usize, Value)>,
    dom_preflight: bool,
    scroll_feedback: Option<Value>,
    completed: usize,
    held_mouse: Option<Value>,
    held_key: Option<Value>,
}

async fn mouse(app: &AppHandle, s: &Session, p: &Value, button: &str, count: u8, progress: &mut InputProgress) -> Result<(), String> {
    check(app, s)?;
    for n in 1..=count {
        check(app,s)?;
        progress.attempted = true;
        progress.held_mouse=Some(json!({"type":"mouseReleased","x":p["x"],"y":p["y"],"button":button,"clickCount":n}));
        let pressed = cdp(app,"Input.dispatchMouseEvent",json!({"type":"mousePressed","x":p["x"],"y":p["y"],"button":button,"clickCount":n}),None,Some(s.cancel.clone())).await;
        // Release even when a pressed reply failed; never retry the press.
        let released = cdp(app,"Input.dispatchMouseEvent",json!({"type":"mouseReleased","x":p["x"],"y":p["y"],"button":button,"clickCount":n}),None,None).await;
        if released.is_ok() {progress.held_mouse=None;}
        pressed?; released?;
    }
    Ok(())
}
/// "Ctrl+Shift+ArrowDown" → (key, DOM code, virtual key, CDP modifiers, editing command).
/// Canvas spreadsheets are keyboard-driven: range selection, edge jumps, F2 and clipboard shortcuts.
fn key_spec(name: &str) -> Option<(String,String,i32,i32,Option<&'static str>)> {
    let mut parts: Vec<&str> = name.split('+').collect();
    let base = parts.pop()?;
    let mut modifiers = 0;
    for m in parts { modifiers |= match m { "Alt"=>1, "Control"|"Ctrl"=>2, "Meta"|"Cmd"=>4, "Shift"=>8, _=>return None }; }
    let named = |k: &str, vk| Some((k.to_string(), k.to_string(), vk));
    let (key, code, vk) = match base {
        "Enter"=>named("Enter",13), "Tab"=>named("Tab",9), "Escape"=>named("Escape",27), "Backspace"=>named("Backspace",8),
        "Delete"=>named("Delete",46), "Home"=>named("Home",36), "End"=>named("End",35), "PageUp"=>named("PageUp",33), "PageDown"=>named("PageDown",34),
        "ArrowLeft"=>named("ArrowLeft",37), "ArrowUp"=>named("ArrowUp",38), "ArrowRight"=>named("ArrowRight",39), "ArrowDown"=>named("ArrowDown",40),
        "Space"=>Some((" ".into(), "Space".into(), 32)),
        f if f.len() <= 3 && f.starts_with('F') => { let n: i32 = f[1..].parse().ok().filter(|n| (1..=12).contains(n))?; named(f, 111 + n) }
        // Bare printable characters belong to `type`: a keyDown without text inserts nothing.
        c if c.len() == 1 && c.as_bytes()[0].is_ascii_alphanumeric() && modifiers & 7 != 0 => {
            let upper = c.to_ascii_uppercase();
            let key = if modifiers & 8 != 0 { upper.clone() } else { c.to_ascii_lowercase() };
            let code = if upper.as_bytes()[0].is_ascii_digit() { format!("Digit{upper}") } else { format!("Key{upper}") };
            Some((key, code, upper.as_bytes()[0] as i32))
        }
        _ => None,
    }?;
    // CDP key events do not trigger browser clipboard actions by themselves; the editing command does.
    let command = if modifiers == 2 { match vk { 67=>Some("copy"), 88=>Some("cut"), 86=>Some("paste"), _=>None } } else { None };
    Some((key, code, vk, modifiers, command))
}
async fn key(app: &AppHandle, s: &Session, name: &str, progress: &mut InputProgress) -> Result<(), String> {
    let (key,code,vk,modifiers,command)=key_spec(name).ok_or("不支持的按键")?;
    check(app,s)?; progress.attempted=true;
    let up=json!({"type":"keyUp","key":key,"code":code,"windowsVirtualKeyCode":vk,"modifiers":modifiers});
    let mut down=up.clone(); down["type"]=json!("keyDown");
    if let Some(command)=command {down["commands"]=json!([command]);}
    progress.held_key=Some(up.clone());
    let down=cdp(app,"Input.dispatchKeyEvent",down,None,Some(s.cancel.clone())).await;
    let up=cdp(app,"Input.dispatchKeyEvent",up,None,None).await;
    if up.is_ok() {progress.held_key=None;}
    down?;up?;Ok(())
}
async fn frame_has_focus(app:&AppHandle, observation:&Observation, index:usize) -> Result<bool,String> {
    let mut child=observation.frames.get(index).ok_or("frame 不存在")?;
    let mut depth=0;
    while let Some(parent_id)=&child.parent {
        depth+=1;if depth>12 {return Err("焦点框架嵌套过深".into());}
        let parent=observation.frames.iter().find(|f|&f.id==parent_id).ok_or("焦点父框架不可访问")?;
        let owner=cdp(app,"DOM.getFrameOwner",json!({"frameId":child.id}),parent.session.as_deref(),None).await?;
        let resolved=cdp(app,"DOM.resolveNode",json!({"backendNodeId":owner["backendNodeId"],"executionContextId":parent.context}),parent.session.as_deref(),None).await?;
        let object=resolved["object"]["objectId"].as_str().ok_or("焦点框架不可访问")?;
        let result=cdp(app,"Runtime.callFunctionOn",json!({"objectId":object,"returnByValue":true,"functionDeclaration":"function(){let e=this.ownerDocument.activeElement;while(e?.shadowRoot?.activeElement)e=e.shadowRoot.activeElement;return e===this;}"}),parent.session.as_deref(),None).await;
        let _=cdp(app,"Runtime.releaseObject",json!({"objectId":object}),parent.session.as_deref(),None).await;
        if result?["result"]["value"]!=true {return Ok(false);}
        child=parent;
    }
    Ok(true)
}
async fn require_focus(app:&AppHandle, observation:&Observation, index:usize, reference:&str) -> Result<(),String> {
    let frame=observation.frames.get(index).ok_or("frame 不存在")?;
    if !frame_has_focus(app,observation,index).await? {return Err("焦点已离开目标框架，停止填写".into());}
    let state=evaluate(app,frame,format!("__novaWebview.inputState({})",json!(reference))).await?;
    if state["matches"]!=true || state["editable"]!=true { return Err("目标焦点已改变或不可编辑，停止输入；请根据新观察继续".into()); }
    Ok(())
}

async fn apply(
    app: &AppHandle, s: &Session, observation: &Observation, action: &Action,
    image_id: Option<&str>, progress: &mut InputProgress, root: &Path,
) -> Result<(), String> {
    check(app,s)?;
    match action {
        Action::ClickAt {x,y,..} | Action::Move {x,y} | Action::Drag {x,y,..} | Action::ScrollAt {x,y,..} => {
            if observation.full_page && matches!(action,Action::Drag{..}) { return Err("拖动请使用 fullPage=false 的视口截图".into()); }
            let p=coordinate(app,observation,*x,*y,image_id).await?;
            if matches!(action,Action::ClickAt{..}|Action::ScrollAt{..}|Action::Drag{..}) {
                progress.attempted=true;
                cdp(app,"Input.dispatchMouseEvent",json!({"type":"mouseMoved","x":p["x"],"y":p["y"]}),None,Some(s.cancel.clone())).await?;
                if p["hoverTarget"]["ref"].is_string() {
                    evaluate(app,&observation.frames[0],format!("__novaWebview.verifyPoint({},{},{},{})",p["hoverTarget"]["ref"],p["x"],p["y"],p["hoverTarget"]["rect"])).await?;
                    if evaluate(app,&observation.frames[0],"__novaWebview.stamp()".into()).await?!=p["stamp"] {return Err("悬停期间视口已变化，停止点击".into());}
                }
            }
            match action {
                Action::ClickAt {button,click_count,..} => {
                    mouse(app,s,&p,button.as_deref().unwrap_or("left"),click_count.unwrap_or(1),progress).await?;
                    progress.focus=(p["editable"]==true).then(||(0,p["identity"].clone()));
                },
                Action::Move{..} => { progress.attempted=true; cdp(app,"Input.dispatchMouseEvent",json!({"type":"mouseMoved","x":p["x"],"y":p["y"]}),None,Some(s.cancel.clone())).await?; },
                Action::ScrollAt{delta,delta_x,..} => { progress.attempted=true; cdp(app,"Input.dispatchMouseEvent",json!({"type":"mouseWheel","x":p["x"],"y":p["y"],"deltaX":delta_x.unwrap_or(0),"deltaY":delta}),None,Some(s.cancel.clone())).await?; },
                Action::Drag{to_x,to_y,duration_ms,..} => {
                    let end=coordinate(app,observation,*to_x,*to_y,image_id).await?;
                    let (x,y)=(p["x"].as_f64().ok_or("无效坐标")?,p["y"].as_f64().ok_or("无效坐标")?);
                    let (tx,ty)=(end["x"].as_f64().ok_or("无效终点")?,end["y"].as_f64().ok_or("无效终点")?);
                    let duration=duration_ms.unwrap_or(160); let steps=(duration/16).clamp(5,24);
                    let mut last=(x,y); progress.attempted=true;
                    let moved=async {
                        cdp(app,"Input.dispatchMouseEvent",json!({"type":"mouseMoved","x":x,"y":y}),None,Some(s.cancel.clone())).await?;
                        progress.held_mouse=Some(json!({"type":"mouseReleased","x":x,"y":y,"button":"left","clickCount":1}));
                        cdp(app,"Input.dispatchMouseEvent",json!({"type":"mousePressed","x":x,"y":y,"button":"left","buttons":1,"clickCount":1}),None,Some(s.cancel.clone())).await?;
                        for i in 1..=steps {
                            last=(x+(tx-x)*i as f64/steps as f64,y+(ty-y)*i as f64/steps as f64);
                            progress.held_mouse=Some(json!({"type":"mouseReleased","x":last.0,"y":last.1,"button":"left","clickCount":1}));
                            cdp(app,"Input.dispatchMouseEvent",json!({"type":"mouseMoved","x":last.0,"y":last.1,"button":"left","buttons":1}),None,Some(s.cancel.clone())).await?;
                            tokio::time::sleep(Duration::from_millis(duration/steps)).await;
                        }
                        Ok::<_,String>(())
                    }.await;
                    let released=cdp(app,"Input.dispatchMouseEvent",json!({"type":"mouseReleased","x":last.0,"y":last.1,"button":"left","clickCount":1}),None,None).await;
                    if released.is_ok() {progress.held_mouse=None;}
                    moved?; released?;
                }
                _=>unreachable!(),
            }
        }
        Action::Type{text} => {
            let mut focused=false;
            for (index,frame) in observation.frames.iter().enumerate() {
                let now=evaluate(app,frame,"__novaWebview.inputState()".into()).await?;
                if !frame_has_focus(app,observation,index).await? {continue;}
                if now["editable"]==true {
                    let expected=progress.focus.as_ref().map(|(frame,identity)| if *frame==index {identity} else {&Value::Null})
                        .unwrap_or(&observation.pages["pages"][index]["focus"]["identity"]);
                    if now["identity"].is_null() || &now["identity"]!=expected { return Err("输入焦点已变化，请重新观察后输入".into()); }
                    focused=true;
                }
            }
            if !focused { return Err("没有已确认的可编辑焦点；请先 click 或使用 fill".into()); }
            check(app,s)?; progress.attempted=true;
            cdp(app,"Input.insertText",json!({"text":text}),None,Some(s.cancel.clone())).await?;
        }
        Action::Click{frame,r#ref,..} | Action::Fill{frame,r#ref,..} => {
            progress.dom_preflight=true;
            let mode=if matches!(action,Action::Fill{..}) {"fill"} else {"click"};
            let p=point(app,observation,*frame,r#ref,mode).await?;
            // Hover is real input, then validate the same target again before pressing.
            progress.attempted=true;
            cdp(app,"Input.dispatchMouseEvent",json!({"type":"mouseMoved","x":p["x"],"y":p["y"]}),None,Some(s.cancel.clone())).await?;
            evaluate(app,&observation.frames[*frame],format!("__novaWebview.verifyPoint({},{},{},{},{})",json!(r#ref),p["localX"],p["localY"],p["localRect"],json!(mode))).await?;
            if observation.frames[*frame].parent.is_some() {
                let after=point(app,observation,*frame,r#ref,mode).await?;
                if ["x","y"].iter().any(|axis| (p[*axis].as_f64().unwrap_or(f64::NAN)-after[*axis].as_f64().unwrap_or(f64::NAN)).abs()>=0.5) {
                    return Err("框架在悬停后移动，已停止点击，请重新观察".into());
                }
            }
            progress.dom_preflight=false;
            if let Action::Fill{text,..}=action {
                mouse(app,s,&p,"left",1,progress).await?;
                require_focus(app,observation,*frame,r#ref).await?;
                key(app,s,"Control+A",progress).await?;
                require_focus(app,observation,*frame,r#ref).await?;
                check(app,s)?; progress.attempted=true;
                cdp(app,"Input.insertText",json!({"text":text}),None,Some(s.cancel.clone())).await?;
                let verified=evaluate(app,&observation.frames[*frame],format!("__novaWebview.verifyValue({},{})",json!(r#ref),json!(text))).await?;
                if verified["matches"]!=true {return Err("字段实际值与请求不一致（可能被页面校验或长度限制修改）；已停止，请核对新观察".into());}
            } else if let Action::Click{button,click_count,..}=action {
                mouse(app,s,&p,button.as_deref().unwrap_or("left"),click_count.unwrap_or(1),progress).await?;
            }
            // Remember the intended node, never whichever field a page handler redirected focus to.
            progress.focus=(p["editable"]==true).then(||(*frame,p["identity"].clone()));
        }
        Action::Upload{frame,selector,files} => {
            let files = upload_files(root, files)?;
            let frame = &observation.frames[*frame];
            // Resolve a unique file input in the observed isolated world, including hidden inputs.
            let expression = format!("(() => {{ const nodes=document.querySelectorAll({}); if(nodes.length!==1) throw Error('upload selector必须唯一匹配'); const el=nodes[0]; if(!(el instanceof HTMLInputElement)||el.type!=='file'||el.disabled||el.webkitdirectory||(!el.multiple&&{}>1)) throw Error('目标不是可用的文件input或不支持所选文件数量'); return el; }})()", json!(selector), files.len());
            let reply = cdp(app,"Runtime.evaluate",json!({"expression":expression,"contextId":frame.context,"returnByValue":false}),frame.session.as_deref(),Some(s.cancel.clone())).await?;
            if reply.get("exceptionDetails").is_some() {
                return Err(reply["exceptionDetails"]["exception"]["description"].as_str().unwrap_or("上传目标校验失败").into());
            }
            let object = reply["result"]["objectId"].as_str().ok_or("未取得文件input，未上传")?;
            check(app,s)?;
            progress.attempted=true;
            let uploaded = cdp(app,"DOM.setFileInputFiles",json!({"objectId":object,"files":files}),frame.session.as_deref(),Some(s.cancel.clone())).await;
            let _ = cdp(app,"Runtime.releaseObject",json!({"objectId":object}),frame.session.as_deref(),None).await;
            uploaded?;
        }
        Action::Press{key:name}=>{
            if let Some((index,identity))=&progress.focus {
                let now=evaluate(app,&observation.frames[*index],"__novaWebview.inputState()".into()).await?;
                if !frame_has_focus(app,observation,*index).await? || identity.is_null() || &now["identity"]!=identity {
                    return Err("输入焦点已变化，停止后续按键；请根据新观察继续".into());
                }
            }
            key(app,s,name,progress).await?;
        },
        Action::Scroll{frame,r#ref,delta,delta_x}=>{
            progress.dom_preflight=true;
            let mode=if delta_x.unwrap_or(0)!=0 {"scroll_x"} else {"scroll_y"};
            let p=if let Some(reference)=r#ref {point(app,observation,*frame,reference,mode).await?} else {
                if *frame!=0 {return Err("子框架滚动需要明确 ref".into());}
                evaluate(app,&observation.frames[0],format!("__novaWebview.prepare(null,{})",json!(mode))).await?
            };
            progress.dom_preflight=false;
            check(app,s)?; progress.attempted=true;
            cdp(app,"Input.dispatchMouseEvent",json!({"type":"mouseWheel","x":p["x"],"y":p["y"],"deltaX":delta_x.unwrap_or(0),"deltaY":delta}),None,Some(s.cancel.clone())).await?;
            progress.scroll_feedback=Some(evaluate(app,&observation.frames[*frame],
                format!("__novaWebview.scrollFeedback({},{})",json!(r#ref),p["scroll"])).await?);
        }
        Action::Wait{ms}=>tokio::time::sleep(Duration::from_millis(*ms)).await,
    }
    check(app,s)
}

fn image_point(observation: &Observation, x:f64, y:f64, image_id:Option<&str>) -> Result<(f64,f64),String> {
    match image_id {
        Some(id)=>observation.images.iter().find(|image| image.id==id).ok_or("imageId 不属于当前快照")?.point(x,y),
        None=>Ok((x,y)), // Backwards-compatible CSS coordinates; new clients should supply imageId.
    }
}
async fn coordinate(
    app: &AppHandle,
    observation: &Observation,
    x: f64,
    y: f64,
    image_id: Option<&str>,
) -> Result<Value, String> {
    if !observation.screenshot {
        return Err(
            "当前观察没有可用截图，不能授权坐标操作：请 screenshot 或 inspect（省略 visual=none），也可按 DOM frame/ref 操作"
                .into(),
        );
    }
    if observation.captured.elapsed() > Duration::from_secs(180) {
        return Err(format!(
            "截图已过期：已过去 {} 秒，有效期180秒；请 screenshot，不是 DOM 动画错误",
            observation.captured.elapsed().as_secs()
        ));
    }
    let (x,y)=image_point(observation,x,y,image_id)?;
    evaluate(
        app,
        &observation.frames[0],
        format!(
            "__novaWebview.coordinate({},{},{},{})",
            json!(x),
            json!(y),
            observation.pages["pages"][0]["stamp"],
            observation.full_page
        ),
    )
    .await
}

fn snapshot_options(args: &Value, with_image: bool) -> Result<(Option<&Value>, Option<&str>), String> {
    // Empty optional crop placeholders carry no target; never discard a non-empty crop.
    let region = args.get("region").filter(|v| !v.is_null() && !v.as_object().is_some_and(|o| o.is_empty()));
    let reference = match args.get("ref").filter(|v| !v.is_null()) {
        Some(value) => Some(value.as_str().ok_or("ref 必须是最新观察中的引用字符串；不裁剪时省略")?).filter(|s| !s.trim().is_empty()),
        None => None,
    };
    if !with_image && (region.is_some() || reference.is_some()) {
        return Err("inspect 不接受 region/ref 裁剪；读页面只传 operation、tabTag 和可选 query；裁剪请用 screenshot".into());
    }
    if region.is_some() && reference.is_some() {
        return Err("region 与 ref 裁剪不能同时提供；按坐标裁剪时省略 ref，按元素裁剪时省略 region".into());
    }
    if let Some(region) = region {
        if !["x", "y", "width", "height"].iter().all(|key| region.get(*key).and_then(Value::as_f64)
            .is_some_and(|v| v.is_finite() && if matches!(*key, "width" | "height") { v > 0. } else { v >= 0. })) {
            return Err("region 需完整的有限数值 x/y/width/height：x/y≥0，width/height>0；不裁剪时省略 region".into());
        }
    }
    if args.get("maxEdge").filter(|v| !v.is_null()).is_some_and(|v|
        !v.as_u64().is_some_and(|edge| edge == 0 || (320..=3840).contains(&edge))) {
        return Err("maxEdge 必须为整数0或320–3840；使用默认截图尺寸时省略 maxEdge".into());
    }
    Ok((region, reference))
}

async fn snapshot(
    app: &AppHandle,
    with_image: bool,
    args: &Value,
    previous: Option<&Observation>,
) -> Result<(Observation, Value), String> {
    let (region, reference) = snapshot_options(args, with_image)?;
    let mut region = region.cloned();
    // ref+expandScroll: capture that element's whole inner scroll content (main frame only).
    let mut target=None;
    if let Some(reference)=reference {
        let previous=app.state::<BrowserState>().observations.lock().unwrap().get(&active_label(app)?).cloned().ok_or("目标裁剪需要最新 inspect/screenshot")?;
        if args["snapshotId"].as_str()!=Some(&previous.id) { return Err("目标裁剪 snapshotId 已失效".into()); }
        if args["expandScroll"]==true {
            if args["frame"].as_u64().unwrap_or(0)!=0 {return Err("expandScroll 按 ref 截取仅支持主框架 frame=0".into());}
            target=Some(reference);
        } else {
            let p=point(app,&previous,args["frame"].as_u64().unwrap_or(0) as usize,reference,"capture").await?;
            region=Some(p["rect"].clone());
        }
    }
    let scope=args["scope"].as_str().unwrap_or("all");
    if !matches!(scope,"all"|"viewport") {return Err("scope 必须为 all 或 viewport".into());}
    let mut observation = observe(app,scope).await?;
    let auto_visual=!with_image && args["visual"]!="none";
    let with_image=with_image || auto_visual;
    let dir = SHOT_OWNER.try_with(|owner| crate::jianlai::shot_folder(owner, "browser-shots"))
        .unwrap_or_else(|_| state_dir(app).join("browser-shots"));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let document_path = dir.join(format!("{}.json", observation.id));
    std::fs::write(
        &document_path,
        serde_json::to_vec(&observation.pages).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    // The private __delta flag is set only by act feedback: diff against the pre-action observation.
    let base = previous.filter(|_| args["__delta"] == true).map(|p| &p.pages);
    let mut result = inline_summary(&observation.pages, args, base);
    result["snapshotId"] = json!(observation.id);
    result["visualReason"] = if auto_visual {json!("默认附当前视口截图，辅助 DOM 判断布局、遮挡和绘制内容；visual=none 可关闭")} else {Value::Null};
    result["documentPath"] = json!(document_path);
    if with_image {
        let capture = async {
        // expandScroll output is meant to be read/shared: keep original pixels unless asked otherwise.
        let keep_original=args["expandScroll"]==true;
        let edge=args["maxEdge"].as_u64().unwrap_or(if keep_original {0} else {1600});
        observation.full_page = !auto_visual && region.is_none() && (args["fullPage"] == true || keep_original);
        let mut images = Vec::new();
        let mut document = None;
        let mut expanded = None;
        if observation.full_page {
            // Retain the root world for cleanup even if capture is cancelled or times out.
            app.state::<BrowserState>()
                .observations
                .lock()
                .unwrap()
                .insert(active_label(app)?, observation.clone());
            evaluate(
                app,
                &observation.frames[0],
                "__novaWebview.captureLayout(true)".into(),
            )
            .await?;
            let dpr = evaluate(app, &observation.frames[0], "devicePixelRatio".into()).await?
                .as_f64().filter(|v| v.is_finite() && *v > 0.).unwrap_or(1.);
            if args["expandScroll"] == true {
                expanded = expand_inner_scroll(app, &observation.frames[0], dpr, target).await?;
            }
            let element = match target {
                Some(reference) => Some(evaluate(app, &observation.frames[0], format!("__novaWebview.documentRect({})", json!(reference))).await?),
                None => None,
            };
            let metrics = cdp(app, "Page.getLayoutMetrics", json!({}), None, None).await?;
            let size = metrics
                .get("cssContentSize")
                .unwrap_or(&metrics["contentSize"]);
            let width = size["width"]
                .as_f64()
                .filter(|v| v.is_finite() && *v > 0.)
                .ok_or("无效文档宽度")?
                .ceil();
            let height = size["height"]
                .as_f64()
                .filter(|v| v.is_finite() && *v > 0.)
                .ok_or("无效文档高度")?
                .ceil();
            // captureBeyondViewport resizes the renderer to the document; past the GPU texture limit
            // (16384 device px) Chromium crashes the page/GPU process and weak GPUs can hang the system.
            let rect = |key: &str| element.as_ref().map_or(Ok(0.), |e| e[key].as_f64().filter(|v| v.is_finite()).ok_or("目标元素尺寸无效"));
            let (left, top) = (rect("x")?.max(0.).floor(), rect("y")?.max(0.).floor());
            let (width, height) = if element.is_some() {
                ((rect("x")? + rect("width")?).min(width).ceil() - left, (rect("y")? + rect("height")?).min(height).ceil() - top)
            } else { (width, height) };
            if width < 1. || height < 1. { return Err("目标元素不可见".into()); }
            if width.max(height) * dpr > MAX_FULL_PAGE_DEVICE_PX {
                if expanded.take().is_some() {cdp(app,"Emulation.clearDeviceMetricsOverride",json!({}),None,None).await?;}
                if target.is_some() {
                    evaluate(app, &observation.frames[0], "__novaWebview.captureLayout(false)".into()).await?;
                    return Err("目标区域超出截图安全上限，请改用分页或读 DOM".into());
                }
                evaluate(app, &observation.frames[0], "__novaWebview.captureLayout(false)".into()).await?;
                observation.full_page = false;
                result["fullPageFallback"] = json!(format!("文档 {width}x{height} CSS像素超出整页截图安全上限，已改为视口截图；屏外内容请滚动后再截或读 DOM"));
            } else {
                document = Some((left, top, width, height, dpr));
            }
        }
        if let Some((left, top, width, height, dpr)) = document {
            // Bound native-resolution tiles before local downsampling, including high-DPI screens.
            let tile_width = (2048. / dpr.max(1.)).floor().max(1.);
            let tile_height = (4096. / dpr.max(1.)).floor().max(1.);
            let columns = (width / tile_width).ceil() as u64;
            let rows = (height / tile_height).ceil() as u64;
            let total = columns.checked_mul(rows).ok_or("文档尺寸过大")?;
            let offset = args["tileOffset"].as_u64().unwrap_or(0);
            if offset >= total {
                return Err("tileOffset超出整页截图范围".into());
            }
            // ponytail: at most 4 tiles / 32M CSS pixels per call; nextTile makes huge pages resumable, never silently cropped.
            let end = total.min(offset + 4);
            let label = active_label(app)?;
            let tag = label.rsplit(':').next().unwrap_or(&label);
            for tile in offset..end {
                let (dx, dy) = ((tile % columns) as f64 * tile_width, (tile / columns) as f64 * tile_height);
                let (x, y, w, h) = (left + dx, top + dy, (width - dx).min(tile_width), (height - dy).min(tile_height));
                // clip.scale changes Chromium's render scale; resize the captured pixels locally instead.
                let shot=cdp(app,"Page.captureScreenshot",json!({"format":"webp","quality":100,"fromSurface":true,"captureBeyondViewport":true,"clip":{"x":x,"y":y,"width":w,"height":h,"scale":1}}),None,None).await?;
                let data=shot["data"].as_str().ok_or("截图为空")?;
                if !keep_original {
                    let path = dir.join(format!("{}-{tile}.webp", observation.id));
                    let pixels = screenshot_pixels(data, edge as u32)?;
                    let mut parts = deliver_screenshot(&mut observation, previous, Some(format!("{tag}:document:{tile}")),
                        pixels, path, (x,y,w,h))?;
                    for part in &mut parts { part["tile"] = json!(tile); }
                    images.extend(parts);
                    continue;
                }
                // Expanded-scroll exports keep a complete standalone image; never reuse their temporary layout.
                let (bytes,pixels)=if edge==0 {
                    use base64::Engine;
                    let raw=base64::engine::general_purpose::STANDARD.decode(data).map_err(|e|e.to_string())?;
                    let pixels=webp_dimensions(&raw)?;
                    (raw,pixels)
                } else {
                    let image = screenshot_pixels(data, edge as u32)?;
                    (crate::visual_guard::encode_webp(&image)?, image.dimensions())
                };
                let path = dir.join(format!("{}-{tile}.webp", observation.id));
                std::fs::write(&path,bytes).map_err(|e|e.to_string())?;
                let image_id=format!("{}-{tile}",observation.id);
                // The expanded layout differs from the restored page: such tiles are read-only.
                if expanded.is_none() {observation.images.push(ScreenshotImage{id:image_id.clone(),path:path.clone(),x,y,width:w,height:h,pixels,delivered:None});}
                images.push(json!({"imageId":image_id,"path":path,"tile":tile,"x":x,"y":y,"width":w,"height":h,"pixelWidth":pixels.0,"pixelHeight":pixels.1}));
            }
            if let Some((viewport_height,complete))=expanded {
                cdp(app,"Emulation.clearDeviceMetricsOverride",json!({}),None,None).await?;
                result["scrollExpanded"]=json!({"viewportHeight":viewport_height,"complete":complete,
                    "note":if complete {"已临时加高视口展开内部滚动区并恢复；图片仅供阅读，坐标操作请用 fullPage=false 截图"} else {"内部滚动区超出安全高度或无法随视口展开，长图仍不完整；图片仅供阅读"}});
            }
            // Pagination counts document tiles, not the variable number of changed patches in images.
            let next = end;
            result["documentSize"] = json!({"width":width,"height":height});
            result["totalTiles"] = json!(total);
            result["screenshotComplete"] = json!(offset == 0 && next == total);
            result["nextTile"] = if next < total {
                json!(next)
            } else {
                Value::Null
            };
        } else {
            let viewport=&observation.pages["pages"][0]["viewport"];
            let vw=viewport["width"].as_f64().ok_or("无效视口")?;let vh=viewport["height"].as_f64().ok_or("无效视口")?;
            let stream = region.is_none().then(|| active_label(app)).transpose()?
                .map(|label| format!("{}:viewport", label.rsplit(':').next().unwrap_or(&label)));
            let (x,y,w,h)=if let Some(region)=region {
                let get=|key:&str|region[key].as_f64().filter(|v|v.is_finite()).ok_or("region 必须包含有限的 x/y/width/height");
                let (x,y,w,h)=(get("x")?,get("y")?,get("width")?,get("height")?);
                if w<=0. || h<=0. || x>=vw || y>=vh || x+w<=0. || y+h<=0. {return Err("region 不在当前视口中".into());}
                (x.max(0.),y.max(0.),(x+w).min(vw)-x.max(0.),(y+h).min(vh)-y.max(0.))
            } else {(0.,0.,vw,vh)};
            let path=dir.join(format!("{}.webp",observation.id));
            let image = resize_screenshot(capture_viewport(app,viewport,x,y,w,h).await?,edge as u32);
            images = deliver_screenshot(&mut observation, previous, stream, image, path, (x,y,w,h))?;
        }
        result["path"] = images[0]["path"].clone();
        result["images"] = json!(images);
        result["fullPage"] = json!(observation.full_page);
        observation.screenshot = true;
        Ok::<(), String>(())
        }.await;
        if let Err(error) = capture {
            if !auto_visual { return Err(error); }
            // Optional visual feedback must not discard a usable DOM observation.
            result["screenshotError"] = json!(error);
        }
    }
    observation.captured = std::time::Instant::now();
    if let Some(ids) = carry_screenshot(&mut observation, previous.filter(|_| result["screenshotError"].is_null())) {
        result["reusableImageIds"] = json!(ids);
    }
    app.state::<BrowserState>()
        .observations
        .lock()
        .unwrap()
        .insert(active_label(app)?, observation.clone());
    Ok((observation, result))
}

// Only the tool reply is compacted: stored observations (Altair, preflight, coordinates) and
// documentPath keep the full DOM. Do not use Lyra's output fallback here: it also cuts names/rows.
// `base` (act feedback only) is the pre-action observation: pages of the same document then reply
// with what changed only (+new/~changed/-gone elements, new text lines); refs are stable per document.
fn inline_summary(pages: &Value, args: &Value, base: Option<&Value>) -> Value {
    let mut result = pages.clone();
    if let Some(object) = result.as_object_mut() { object.remove("domCoordinates"); }
    let mut remaining_items = args["maxItems"].as_u64().unwrap_or(60).clamp(1, 2000) as usize;
    let mut remaining_text = args["maxTextChars"].as_u64().unwrap_or(3000).clamp(100, 100000) as usize;
    // Separate, global budgets: viewport evidence must not disappear behind a long document prefix.
    let mut remaining_visible = remaining_text;
    let mut remaining_headings = 20;
    let query = args["query"].as_str().unwrap_or("").trim().to_lowercase();
    // 有 query 时正文只从命中前 120 字开始，未命中则为空。
    let around_query = |original: &str| -> String {
        if query.is_empty() { return original.to_string(); }
        let lower = original.to_lowercase();
        lower.find(&query).map(|at| {
            let start = lower[..at].char_indices().rev().nth(120).map_or(0, |(index, _)| index);
            lower[start..].to_string()
        }).unwrap_or_default()
    };
    let lines = |text: &Value| -> HashSet<String> { text.as_str().unwrap_or_default().lines().map(str::to_owned).collect() };
    if let Some(pages) = result["pages"].as_array_mut() {
        for page in pages {
            let before = base.and_then(|b| b["pages"].as_array()?.iter()
                .find(|p| p["document"].is_string() && p["document"] == page["document"]));
            if let Some(object) = page.as_object_mut() {
                for key in ["document", "version", "stamp", "timingsMs", "scannedRoots"] { object.remove(key); }
            }
            if let Some(coverage) = page["coverage"].as_object_mut() { coverage.remove("note"); coverage.remove("scope"); }
            let mut trimmed = false;
            let items = page["items"].as_array().cloned().unwrap_or_default();
            let mut gone = Vec::new();
            // (mark, item): "" full listing, "+" new, "~" changed since `before`.
            let mut listed: Vec<(&str, Value)> = match before {
                None => items.into_iter().map(|item| ("", item)).collect(),
                Some(before) => {
                    let old: HashMap<&str, &Value> = before["items"].as_array().into_iter().flatten()
                        .filter_map(|i| Some((i["ref"].as_str()?, i))).collect();
                    let current: HashSet<String> = items.iter().filter_map(|i| i["ref"].as_str().map(str::to_owned)).collect();
                    gone = old.iter().filter(|(r, _)| !current.contains(**r)).map(|(_, i)| (*i).clone()).collect();
                    let unchanged = items.len();
                    let changed: Vec<(&str, Value)> = items.into_iter().filter_map(|item| match item["ref"].as_str().and_then(|r| old.get(r)) {
                        None => Some(("+", item)),
                        Some(o) if item_state(o) != item_state(&item) || (item["inView"] == true && o["inView"] != true) => Some(("~", item)),
                        Some(_) => None,
                    }).collect();
                    page["delta"] = json!(true);
                    page["unchangedItems"] = json!(unchanged - changed.len());
                    let old_text = lines(&before["text"]);
                    let fresh: String = page["text"].as_str().unwrap_or_default().lines()
                        .filter(|l| !l.trim().is_empty() && !old_text.contains(*l)).collect::<Vec<_>>().join("\n");
                    page["text"] = json!(fresh);
                    if let Some(object) = page.as_object_mut() { object.remove("visibleText"); }
                    if let Some(tables) = page["tables"].as_array_mut() {
                        let old = before["tables"].as_array().cloned().unwrap_or_default();
                        tables.retain(|t| !old.contains(t));
                    }
                    if let Some(headings) = page["headings"].as_array_mut() {
                        let old = before["headings"].as_array().cloned().unwrap_or_default();
                        headings.retain(|h| !old.contains(h));
                    }
                    if page["canvases"] == before["canvases"] { if let Some(o) = page.as_object_mut() { o.remove("canvases"); } }
                    changed
                }
            };
            let original_text = page["text"].as_str().unwrap_or_default().to_string();
            let mut text: String = around_query(&original_text).chars().take(remaining_text).collect();
            remaining_text -= text.chars().count();
            trimmed |= text.len() < original_text.len();
            if let Some(visible) = page.get_mut("visibleText") {
                if let Some(original) = visible.as_str().filter(|_| !query.is_empty()) {
                    let matched = around_query(original);
                    trimmed |= matched.len() < original.len();
                    *visible = json!(matched);
                }
                trimmed |= truncate_inline_text(visible, remaining_visible);
                remaining_visible -= visible.as_str().unwrap_or_default().chars().count();
                // Short pages: the document text adds nothing the visible text does not already show.
                let shown = lines(visible);
                if text.lines().all(|l| l.trim().is_empty() || shown.contains(l)) { text.clear(); }
            }
            page["text"] = json!(text);
            listed.retain(|(_, item)| query.is_empty() || format!("{} {} {}", item["name"], item["region"], item["href"])
                .to_lowercase().contains(&query));
            // Preserve observed order: an obscured first result must not become the last result.
            let total = listed.len();
            listed.truncate(remaining_items);
            remaining_items -= listed.len();
            trimmed |= total > listed.len();
            let mut out = Vec::new();
            let mut last_region = String::new();
            for (mark, item) in &listed {
                // ponytail: region previews are 120 chars, printed once per run of same-region elements; query/documentPath recover the rest.
                let mut region = item["region"].clone();
                trimmed |= truncate_inline_text(&mut region, 120);
                let region = region.as_str().unwrap_or_default().to_string();
                if region != last_region { out.push(format!("@ {}", if region.is_empty() { "-" } else { &region })); last_region = region; }
                out.push(item_line(item, mark));
            }
            for item in gone.iter().take(10) { out.push(format!("- {} {} {}", str_of(&item["ref"]), str_of(&item["role"]), json!(str_of(&item["name"])))); }
            if gone.len() > 10 { out.push(format!("- …另有{}个元素消失", gone.len() - 10)); }
            // 有 query 时表格只留命中行；表头与行都未命中的表整张省略（documentPath 仍是全量）。
            if let Some(tables) = page.get_mut("tables").and_then(Value::as_array_mut).filter(|_| !query.is_empty()) {
                let before = tables.to_vec();
                tables.retain_mut(|table| {
                    let header_hit = table["headers"].to_string().to_lowercase().contains(&query);
                    let rows = table.get_mut("rows").and_then(Value::as_array_mut).map_or(0, |rows| {
                        rows.retain(|row| row.to_string().to_lowercase().contains(&query));
                        rows.len()
                    });
                    if table.get("returnedRows").is_some() { table["returnedRows"] = json!(rows); }
                    header_hit || rows > 0
                });
                trimmed |= *tables != before;
            }
            if let Some(headings) = page["headings"].as_array_mut() {
                trimmed |= headings.len() > remaining_headings;
                headings.truncate(remaining_headings);
                remaining_headings -= headings.len();
            }
            if let Some(object) = page.as_object_mut() { object.remove("items"); }
            page["inlineTruncated"] = json!(trimmed);
            page["matchingItems"] = json!(total);
            page["elements"] = json!(out.join("\n"));
        }
    }
    result
}

fn str_of(value: &Value) -> &str { value.as_str().unwrap_or_default() }

/// What an element looks like to the model, without geometry: equal states mean "unchanged".
fn item_state(item: &Value) -> Value {
    let mut state = item.clone();
    if let Some(object) = state.as_object_mut() {
        for key in ["ref", "nodeId", "point", "viewportRect", "documentRect", "inView", "tabIndex"] { object.remove(key); }
    }
    state
}

/// One element per line: `[mark]ref role "name" key=value… flags`. Geometry is kept only where
/// coordinates are the interface (Canvas, scroll containers); DOM actions bind by frame/ref.
fn item_line(item: &Value, mark: &str) -> String {
    let token = |value: &Value| match value.as_str() {
        Some(s) if !s.is_empty() && !s.chars().any(|c| c.is_whitespace() || matches!(c, '"' | '=' | '\\')) => s.to_string(),
        Some(s) => json!(s).to_string(),
        None => value.to_string(),
    };
    let rect = |r: &Value| format!("{},{},{},{}", r["x"].as_f64().unwrap_or(0.).round(), r["y"].as_f64().unwrap_or(0.).round(),
        r["width"].as_f64().unwrap_or(0.).round(), r["height"].as_f64().unwrap_or(0.).round());
    let mut line = format!("{mark}{} {} {}", str_of(&item["ref"]), str_of(&item["role"]), json!(str_of(&item["name"])));
    for (key, label) in [("value", "value"), ("dateValue", "date"), ("selected", "selected"), ("expanded", "expanded"), ("haspopup", "popup"),
        ("sort", "sort"), ("href", "href"), ("icon", "icon"), ("columnKey", "key"), ("fieldContext", "field"), ("blockedBy", "blockedBy")] {
        let value = &item[key];
        if !value.is_null() && value != "" { line += &format!(" {label}={}", token(value)); }
    }
    let column = &item["column"];
    if column.is_object() {
        line += &format!(" col={}.{}:{}", column["table"], column["index"], json!(str_of(&column["name"])));
        if !str_of(&column["group"]).is_empty() { line += &format!("/{}", json!(str_of(&column["group"]))); }
    }
    for flag in ["disabled", "editable", "password", "actionable"] { if item[flag] == true { line += &format!(" {flag}"); } }
    if item["inView"] != true { line += " offscreen"; }
    if item["canvas"].is_object() { line += &format!(" canvas={}x{}", item["canvas"]["width"], item["canvas"]["height"]); }
    if let Some(scroll) = item["scroll"].as_object() {
        let scroll: serde_json::Map<String, Value> = scroll.iter().filter(|(_, v)| !v.is_null()).map(|(k, v)| (k.clone(), v.clone())).collect();
        line += &format!(" scroll={}", Value::Object(scroll));
    }
    if (item["canvas"].is_object() || item["scroll"].is_object()) && item["viewportRect"].is_object() { line += &format!(" rect={}", rect(&item["viewportRect"])); }
    line
}

fn truncate_inline_text(value: &mut Value, max_chars: usize) -> bool {
    if let Value::String(text) = value {
        if let Some((end, _)) = text.char_indices().nth(max_chars) {
            text.truncate(end);
            return true;
        }
    }
    false
}

/// A DOM-only refresh with the same URL/viewport/scroll/zoom stamp keeps the last screenshot usable:
/// its coordinates still map 1:1 and every coordinate input re-checks the live pixels at the target.
/// Without this, inspect or an act with DOM feedback forced another screenshot round-trip.
fn carry_screenshot(observation: &mut Observation, previous: Option<&Observation>) -> Option<Vec<String>> {
    let stamp = &observation.pages["pages"][0]["stamp"];
    let previous = previous.filter(|p| !observation.screenshot && p.screenshot && stamp.is_string()
        && p.pages["pages"][0]["stamp"] == *stamp && p.captured.elapsed() <= Duration::from_secs(180))?;
    observation.screenshot = true;
    observation.full_page = previous.full_page;
    observation.images = previous.images.clone();
    // Age stays the image's, so the 180s coordinate expiry is not extended by DOM refreshes.
    observation.captured = previous.captured;
    Some(observation.images.iter().map(|i| i.id.clone()).collect())
}

fn evaluation_args(args: &Value) -> Result<(&str, usize), String> {
    let expression = args["expression"].as_str().filter(|s| !s.trim().is_empty() && s.chars().count() <= 32000)
        .ok_or("evaluate 需非空 expression，最多32000字符；多条语句请用 IIFE 返回JSON结果")?;
    let frame = match args.get("frame") {
        None => 0,
        Some(value) => value.as_u64().filter(|n| *n < 12).ok_or("frame 必须为观察返回的0–11整数")? as usize,
    };
    Ok((expression, frame))
}

async fn control_session(
    app: &AppHandle,
    root: &Path,
    mut s: Session,
    operation: &str,
    args: &Value,
) -> Result<Value, String> {
    let state = app.state::<BrowserState>();
    let _guard = state.gate.try_lock().map_err(|_| "浏览器正在执行任务")?;
    let started = std::time::Instant::now();
    if args["feedback"].as_str().is_some_and(|v|!matches!(v,"none"|"inspect"|"screenshot")) {return Err("feedback 无效".into());}
    if args["scope"].as_str().is_some_and(|v|!matches!(v,"all"|"viewport")) {return Err("scope 无效".into());}
    let settle_ms = match args.get("settleMs").filter(|v|!v.is_null()) {
        None => 1500,
        Some(v) => v.as_u64().filter(|ms| *ms <= 4000).ok_or("settleMs 必须为 0–4000")?,
    };
    s.cancel = crate::chrome_browser::begin(app, s.active_tab.rsplit(':').next().unwrap());
    check(app, &s)?;
    let mut completed_action = None;
    let mut progress=InputProgress::default();
    let task=CONTROL_TAB.scope(s.active_tab.clone(),async {
        let current=state.observations.lock().unwrap().get(&s.active_tab).cloned();
        if !matches!(operation,"act"|"evaluate") {return Ok(snapshot(app,operation=="screenshot",args,current.as_ref()).await?.1);}
        let script = if operation == "evaluate" { Some(evaluation_args(args)?) } else { None };
        let mut actions=if script.is_some() { Vec::new() } else { parse_actions(args, 16)? };
        let live_refs=script.is_none() && current.as_ref().is_some_and(|o|refs_in(o,&actions));
        let with_image=current.as_ref().is_some_and(|o|o.screenshot) || actions.iter().any(|a|matches!(a,Action::ClickAt{..}|Action::Move{..}|Action::Drag{..}|Action::ScrollAt{..}));
        let Some(observation)=current.clone().filter(|o|live_refs || (args["snapshotId"].as_str()==Some(&o.id) && o.captured.elapsed()<=Duration::from_secs(180))) else {
            // Nothing was sent; attach a fresh observation so the model re-decides in this round-trip.
            let mut observe_args=args.clone();
            if let Some(a)=observe_args.as_object_mut() {for key in ["ref","frame","region"] {a.remove(key);}}
            observe_args["fullPage"]=json!(false);
            if with_image {observe_args["scope"]=json!("viewport");}
            let mut fresh=snapshot(app,with_image,&observe_args,current.as_ref()).await?.1;
            fresh["status"]=json!("not_executed");fresh["completedActions"]=json!(0);fresh["inputAttempted"]=json!(false);
            fresh["reason"]=json!("snapshotId 不是该标签最新观察或观察已过期，本批次未执行；已附当前观察，用新的 snapshotId/ref/imageId 重新决策，不要重放");
            return Ok(fresh);
        };
        // Static validation for the WHOLE batch; revalidate the live node/focus
        // before every individual input. Never retarget by matching its label.
        for action in &mut actions {
            preflight(&observation,action,args["imageId"].as_str())?;
            if let Action::Upload{files,..} = action { *files = upload_files(root, files)?; }
        }
        if let Some((_, frame)) = script {
            if observation.frames.get(frame).is_none() { return Err("frame 不在该快照中，请按观察返回的frame执行".into()); }
        }
        // Compare inventories, not foreground focus: unrelated/user-created tabs must never become our target.
        let opens_tab = |action: &Action| matches!(action, Action::Click{..} | Action::ClickAt{..} | Action::Press{..} | Action::Upload{..});
        let source_tag = s.active_tab.rsplit(':').next().ok_or("缺少 Chrome tabTag")?;
        let before_tabs = if script.is_some() || actions.iter().any(opens_tab) {
            Some(crate::chrome_browser::request(app, "tabs", json!({})).await?)
        } else { None };
        let mut opened = Vec::new();
        // Background tabs throttle animation/timer sampling, making stable DOM targets time out as "moving".
        // Activate the explicitly bound tab before live checks; stale geometry/identity is still checked by apply.
        crate::chrome_browser::request(app,"select_tab",json!({"tabTag":s.active_tab.rsplit(':').next().ok_or("缺少 Chrome tabTag")?})).await?;
        state.observations.lock().unwrap().remove(&s.active_tab);
        let mut failure=None;
        let mut action_timings=Vec::new();
        let clipboard=crate::clipboard::sequence();
        let mut script_value = None;
        if let Some((expression, index)) = script {
            // A script can change any DOM state; retire every owner's observation of this tab before dispatch.
            let suffix = format!(":{}", s.active_tab.rsplit(':').next().unwrap());
            state.observations.lock().unwrap().retain(|key, _| !key.ends_with(&suffix));
            let frame = &observation.frames[index];
            let page = &observation.pages["pages"][index];
            let expression = format!("__novaWebview.evaluate({},{},{})", json!(expression), page["document"], page["url"]);
            progress.attempted = true;
            // CDP timeout stops synchronous JS, not an awaited Promise. Bound our wait too; async effects may continue.
            let reply = tokio::time::timeout(Duration::from_secs(5), cdp(app, "Runtime.evaluate", json!({"expression":expression,"contextId":frame.context,
                "returnByValue":true,"awaitPromise":true,"timeout":5000}), frame.session.as_deref(), Some(s.cancel.clone())))
                .await.map_err(|_| "脚本等待5秒超时，可能已部分执行且异步脚本可能仍在运行；先观察确认，不要重放")??;
            if reply.get("exceptionDetails").is_some() {
                failure = Some(reply["exceptionDetails"]["exception"]["description"].as_str()
                    .or(reply["exceptionDetails"]["text"].as_str()).unwrap_or("脚本执行失败，请观察确认，不要重放").to_string());
            } else {
                let outcome = &reply["result"]["value"];
                if outcome["status"] == "executed" {
                    progress.completed = 1;
                    script_value = Some(outcome["value"].clone());
                } else {
                    progress.attempted = outcome["status"] != "not_executed";
                    failure = Some(outcome["reason"].as_str().unwrap_or("脚本结果未知，请观察确认，不要重放").to_string());
                }
            }
        }
        for (index, action) in actions.iter().enumerate() {
            let began=std::time::Instant::now();
            let applied=apply(app,&s,&observation,action,args["imageId"].as_str(),&mut progress,root).await;
            action_timings.push(began.elapsed().as_millis());
            match applied {
                Ok(())=>progress.completed+=1,
                Err(error)=>{failure=Some(error);break;},
            }
            if index + 1 < actions.len() {
                if let Some(before) = &before_tabs {
                    match crate::chrome_browser::request(app, "tabs", json!({})).await {
                        Ok(after) => opened = opened_tabs(before, &after, source_tag),
                        Err(error) => { failure = Some(format!("动作后标签查询失败，余下动作未执行：{error}")); break; }
                    }
                    if !opened.is_empty() {
                        failure = Some("动作已打开新标签页或窗口，余下动作未执行".into());
                        break;
                    }
                }
            }
        }
        let mut result=json!({"status":if failure.is_none(){"executed"}else if progress.attempted{"needs_review"}else{"not_executed"},
            "scrollFeedback":progress.scroll_feedback,
            "canReobserve":failure.is_some() && progress.dom_preflight && progress.completed==0,
            // The failing action itself sent no input (DOM preflight); earlier batch actions did complete.
            "failedAtPreflight":failure.is_some() && progress.dom_preflight,
            "reason":failure,"inputAttempted":progress.attempted,"completedActions":progress.completed,"actionTimingsMs":action_timings,"basedOnSnapshotId":observation.id,"verification":"unverified"});
        if let Some(value) = script_value { result["value"] = value; }
        result["actionMs"] = json!(started.elapsed().as_millis());
        if args["snapshotId"].as_str()!=Some(&observation.id) {
            result["snapshotIdNote"]=json!(format!("传入的 snapshotId 不是最新观察（最新为 {}）；本批 ref 均属最新观察，已按 ref 执行。snapshotId 取工具返回的顶层 snapshotId，不是 ref", observation.id));
        }
        result.as_object_mut().unwrap().retain(|key, v| key == "value" || !v.is_null());
        completed_action = Some(result.clone());
        // A click/Enter often starts a navigation; feeding back the old DOM costs the model another
        // inspect round-trip. Wait (bounded) for the tab to finish loading before observing.
        if failure.is_none() && settle_ms > 0 && args["feedback"] != "none" && !s.cancel.load(Ordering::SeqCst)
            && actions.iter().any(|a| matches!(a, Action::Click{..} | Action::ClickAt{..} | Action::Press{..})) {
            let tag = s.active_tab.rsplit(':').next().unwrap_or_default();
            result["settle"] = wait_for_tab_load(app, tag, Duration::from_millis(settle_ms)).await;
            // Loaded is not rendered: SPAs patch the DOM after their XHR. Wait for the DOM to go quiet
            // so the feedback shows the result instead of costing the model another observe round-trip.
            let budget = settle_ms.saturating_sub(result["settle"]["waitedMs"].as_u64().unwrap_or(0)).min(1000);
            if result["settle"]["status"] == "complete" && budget >= 100 {
                // A navigation replaced the document (and our world): nothing to wait for there.
                if let Ok(quiet) = evaluate(app, &observation.frames[0], format!("__novaWebview.settle(250,{budget})")).await { result["settle"]["dom"] = quiet; }
            }
            completed_action = Some(result.clone());
        }
        if progress.attempted && !s.cancel.load(Ordering::SeqCst) {
            if let Some(before) = &before_tabs {
                match crate::chrome_browser::request(app, "tabs", json!({})).await {
                    Ok(after) => {
                        let latest = opened_tabs(before, &after, source_tag);
                        if !latest.is_empty() { opened = latest; }
                    }
                    Err(error) => result["tabDiscoveryError"] = json!(error),
                }
            }
        }
        if !opened.is_empty() {
            result["openedTabs"] = json!(opened);
            result["sourceTabTag"] = json!(source_tag);
            completed_action = Some(result.clone());
        }
        // Capturing the source would reactivate it over the popup. Observe a proven new target
        // after releasing this session's gate; ambiguous targets return only the new-tab inventory.
        if opened.is_empty() && (args["feedback"] != "none" || failure.is_some()) && !s.cancel.load(Ordering::SeqCst) {
            let mut feedback_args = args.clone();
            feedback_args["fullPage"] = json!(false);
            feedback_args["__delta"] = json!(failure.is_none() && script.is_none());
            if let Some(args)=feedback_args.as_object_mut() {for key in ["ref","frame","region"] {args.remove(key);} }
            if args["feedback"]=="inspect" {feedback_args["visual"]=json!("none");}
            // Occlusion cannot be explained by an unchanged DOM delta; show the blocker.
            let visual=args["feedback"]=="screenshot"
                || failure.as_deref().is_some_and(|reason| reason.contains("目标被遮挡"));
            // Feedback failure must never turn a completed mutation into a retryable action failure.
            // A script retires input snapshots, not the comparison baseline. Fresh document/stamp/geometry
            // checks in deliver_screenshot still refuse reuse after navigation or viewport changes.
            match tokio::time::timeout(Duration::from_secs(10), snapshot(app,visual,&feedback_args,
                if script.is_none() || visual || feedback_args["visual"] != "none" { Some(&observation) } else { None })).await {
                Ok(Ok((_, feedback))) => result.as_object_mut().unwrap().extend(feedback.as_object().unwrap().clone()),
                Ok(Err(error)) => result["observationError"] = json!(error),
                Err(_) => result["observationError"] = json!("动作后观察10秒超时；动作状态如上，请观察确认，不要重放"),
            }
        }
        // Read after feedback so pages that write the clipboard asynchronously have finished.
        if let Some(copied)=crate::clipboard::copied_since(clipboard) {result["clipboard"]=copied;}
        // Altair runs save their own verified route; everything else feeds the turn's auto trail.
        if script.is_none() && !crate::altair_run::executing_browser_action() {
            let before = &observation.pages["pages"][0];
            let after = if result["pages"][0]["url"].is_string() { &result["pages"][0] } else { before };
            crate::tool_experience::record("chrome", &s.thread_id,
                before["url"].as_str().unwrap_or_default(), after["url"].as_str().unwrap_or_default(),
                after["title"].as_str().unwrap_or_default(),
                actions[..progress.completed].iter().filter_map(|a| trail_step(&observation.pages, a)).collect());
        }
        Ok(result)
    });
    let mut result = tokio::select! {
        r=tokio::time::timeout(Duration::from_secs(15),task)=>r.map_err(|_|"浏览器操作15秒超时；已发送的脚本/动作可能仍在执行，请重新观察确认结果".to_string()).and_then(|r|r),
        _=async {while !s.cancel.load(Ordering::SeqCst) {tokio::time::sleep(Duration::from_millis(30)).await;}}=>Err("已取消等待浏览器结果；已发送的脚本/动作可能仍在执行，请检查页面确认结果".into()),
    };
    if let (Err(error), Some(mut completed)) = (&result, completed_action) {
        completed["observationError"] = json!(error);
        result = Ok(completed);
    }
    s.cancel.store(true, Ordering::SeqCst);
    if matches!(operation,"act"|"evaluate") {
        if let Err(error)=&result {
            result=Ok(json!({"status":if progress.attempted {"needs_review"} else {"not_executed"},
                "reason":error,"completedActions":progress.completed,"inputAttempted":progress.attempted,
                "basedOnSnapshotId":args["snapshotId"],
                "verification":"unverified","next":"先重新观察确认状态；不要重放可能已经执行的动作"}));
        }
        let mut cleanup_errors=Vec::new();
        // Best-effort cleanup independently of cancellation. Never replay the
        // original down/press; uncertain releases remain needs_review.
        for (method,params) in [("Input.dispatchMouseEvent",progress.held_mouse.take()),("Input.dispatchKeyEvent",progress.held_key.take())] {
            if let Some(params)=params {
                if let Err(error)=CONTROL_TAB.scope(s.active_tab.clone(),cdp(app,method,params,None,None)).await {cleanup_errors.push(error);}
            }
        }
        if !cleanup_errors.is_empty() {
            if let Ok(value)=&mut result {value["status"]=json!("needs_review");value["inputReleaseErrors"]=json!(cleanup_errors);}
        }
    }
    if operation == "screenshot" && (args["fullPage"] == true || args["expandScroll"] == true) {
        let captured = state
            .observations
            .lock()
            .unwrap()
            .get(&s.active_tab)
            .cloned();
        if let Some(captured) = captured.filter(|o| o.full_page) {
            let restored = CONTROL_TAB
                .scope(s.active_tab.clone(), async {
                    // Also undo expandScroll's viewport override if capture failed or was cancelled.
                    let cleared = if args["expandScroll"] == true {
                        cdp(app, "Emulation.clearDeviceMetricsOverride", json!({}), None, None).await.map(|_| ())
                    } else { Ok(()) };
                    // A viewport capture resets the full-page scrollbar/layout override.
                    let reset = cdp(
                        app,
                        "Page.captureScreenshot",
                        json!({"format":"webp","quality":100,"captureBeyondViewport":false}),
                        None,
                        None,
                    )
                    .await;
                    let style = evaluate(
                        app,
                        &captured.frames[0],
                        "__novaWebview.captureLayout(false)".into(),
                    )
                    .await;
                    cleared?;
                    reset?;
                    style?;
                    Ok::<_, String>(())
                })
                .await;
            if let Err(error) = restored {
                if result.is_ok() {
                    result = Err(format!("整页截图后恢复页面布局失败，请重新观察：{error}"));
                }
            }
        }
    }
    if let Ok(value) = &mut result {
        value["durationMs"] = json!(started.elapsed().as_millis());
    }
    result
}

/// Coordinate actions carrying a `target` instead of x/y are rejected up front (no target locating).
async fn resolve_targets(_root: &Path, args: &Value, _owner: &str, _tool: &str) -> Result<Value, String> {
    let named = |a: &Value| a["target"].is_string();
    if args["operation"] != "act" || !(named(&args["action"]) || args["actions"].as_array().is_some_and(|a| a.iter().any(named))) {
        return Ok(args.clone());
    }
    Err("target 定位暂不支持，请使用 x/y 或 DOM ref".into())
}

/// A missing `operation` is inferred from the other fields instead of failing the call:
/// plan → run, action(s) → act, expression → evaluate, url without target → open, target only → inspect, nothing → tabs.
/// Drop default-valued fields and cap long URLs (login redirects) so multi-tab results stay small.
fn compact_tab(mut tab: Value) -> Value {
    if let Some(o) = tab.as_object_mut() {
        for (k, v) in [("allowed", json!(true)), ("controllable", json!(true)), ("loading", json!(false)), ("status", json!("complete")), ("incognito", json!(false))] {
            if o.get(k) == Some(&v) { o.remove(k); }
        }
        o.remove("windowId");
        if let Some(url) = o.get("url").and_then(Value::as_str).filter(|u| u.chars().count() > 120) {
            let short: String = url.chars().take(120).collect();
            o.insert("url".into(), json!(format!("{short}…")));
        }
    }
    tab
}

fn infer_operation(args: &Value, target_key: &str) -> (Value, Option<&'static str>) {
    if args["operation"].as_str().is_some_and(|op| !op.is_empty()) || !args.is_object() { return (args.clone(), None); }
    let op = if args["plan"].is_object() { "run" }
        else if !args["action"].is_null() || !args["actions"].is_null() { "act" }
        else if args["expression"].is_string() { "evaluate" }
        else if args["url"].is_string() { if target_key == "tabTag" && args[target_key].is_string() { "goto" } else { "open" } }
        else if args[target_key].is_string() { "inspect" }
        else { "tabs" };
    let mut args = args.clone();
    args["operation"] = json!(op);
    (args, Some(op))
}

/// Semantic, value-free description of one completed act for the auto trail; coordinates and
/// scrolls carry no reusable meaning and are skipped.
fn trail_step(pages: &Value, action: &Action) -> Option<String> {
    let target = |frame: &usize, r: &str| pages["pages"][*frame]["items"].as_array()?.iter().find(|i| i["ref"] == r).map(|i| {
        let name = crate::tool_experience::step_name(i["name"].as_str().unwrap_or_default());
        let role = i["role"].as_str().or(i["tag"].as_str()).unwrap_or("元素");
        if name.is_empty() { role.to_string() } else { format!("{role}「{name}」") }
    });
    match action {
        Action::Click { frame, r#ref, .. } => Some(format!("点击 {}", target(frame, r#ref)?)),
        Action::Fill { frame, r#ref, .. } => Some(format!("填写 {}", target(frame, r#ref)?)),
        Action::Press { key } => Some(format!("按 {key}")),
        _ => None,
    }
}

/// Attach verified routes the first time this session observes a site; see tool_experience::hint.
fn with_experience_hint(tool: &str, owner: &str, mut result: Value) -> Value {
    let origin = result["pages"][0]["url"].as_str().and_then(|u| tauri::Url::parse(u).ok()).map(|u| u.origin().ascii_serialization());
    if let Some(hint) = origin.and_then(|o| crate::tool_experience::hint(&crate::tool_experience::dir(), tool, owner, &o)) {
        result["experienceHint"] = hint;
    }
    result
}

pub(crate) fn tool_owner(root: &Path, owner: &str) -> Result<String, String> {
    if owner.trim().is_empty() || owner.len() > 4096 {
        return Err("缺少有效工具客户端标识，请重启工具客户端".into());
    }
    let root = dunce::canonicalize(root).map_err(|e| e.to_string())?;
    if !root.is_dir() {
        return Err("工具工作目录不存在".into());
    }
    Ok(format!("{owner}:{}", root.display()))
}

fn altair_observation_key(root: &Path, args: &Value, owner: &str, tool: &str) -> Result<String, String> {
    if tool != "chrome" { return Err("不支持的浏览器".into()); }
    let owner = tool_owner(root, owner)?;
    let tag = args["tabTag"].as_str().ok_or("缺少 tabTag")?;
    Ok(format!("chrome:{owner}:{tag}"))
}

pub(crate) fn altair_observation(root: &Path, args: &Value, owner: &str, tool: &str) -> Result<Value, String> {
    let app = APP.get().ok_or("仅 Nova 内可用")?;
    let key = altair_observation_key(root, args, owner, tool)?;
    let state = app.state::<BrowserState>();
    let observations = state.observations.lock().unwrap();
    let observation = observations.get(&key)
        .filter(|o| args["snapshotId"].as_str() == Some(&o.id) && o.captured.elapsed() <= Duration::from_secs(180))
        .ok_or("观察已失效，交回主模型重新观察")?;
    Ok(observation.pages.clone())
}

fn chrome_args(args: &Value, owner: &str, observations: &std::collections::HashMap<String, Observation>) -> Result<Value, String> {
    let mut args = args.as_object().cloned().ok_or("chrome 参数必须是对象")?;
    let operation = args.get("operation").and_then(Value::as_str).unwrap_or_default().to_owned();
    match operation.as_str() {
        "connect" | "status" | "tabs" | "open" | "new_tab" | "downloads" | "experience_search" => return Ok(Value::Object(args)),
        // Models may fill unrelated fields from the shared schema. Inspect only consumes read
        // options; even non-empty screenshot/action placeholders must not prevent a DOM read.
        "inspect" => args.retain(|key, _| matches!(key.as_str(),
            "operation" | "tabTag" | "tabId" | "browserId" | "snapshotId" |
            "query" | "scope" | "visual" | "maxItems" | "maxTextChars" | "maxEdge")),
        "screenshot" | "devtools" | "act" | "evaluate" | "run" | "select_tab" | "close_tab" | "goto" | "back" | "forward" | "reload" | "stop" | "experience_save" | "experience_feedback" => {},
        _ => return Err(format!("operation {operation:?} 无效；可用：tabs/inspect/act/screenshot/evaluate/run/open/goto/select_tab/close_tab/back/forward/reload/devtools/status 等。click/fill 是 act 的 action，不是 operation；不知道目标时先 {{\"operation\":\"tabs\",\"query\":\"关键词\"}}")),
    }
    let valid_tag = |tag: &str| tag.len() <= 80 && tag.starts_with('C')
        && tag.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
    if let Some(tag) = args.get("tabTag") {
        if !tag.as_str().is_some_and(valid_tag) {
            return Err("tabTag 无效；原样复制 tabs[].tag 或 open 返回的 tabTag（不是数字 tabId/browserId）".into());
        }
        return Ok(Value::Object(args));
    }
    // 弱模型经常把正确的 tabTag 填进 tabId/browserId；值本身就是合法 tag 时直接用，不打回去重试。
    let adopted = ["tabId", "browserId"].iter()
        .find_map(|key| args.get(*key)?.as_str().filter(|tag| valid_tag(tag)).map(str::to_string));
    if let Some(tag) = adopted {
        args.remove("tabId");
        args.remove("browserId");
        args.insert("tabTag".into(), json!(tag));
        return Ok(Value::Object(args));
    }
    if args.contains_key("tabId") || args.contains_key("browserId") {
        return Err("Chrome 目标参数名是 tabTag；请复制 tabs[].tag 或 open 返回的 tabTag，不能使用 tabId/browserId".into());
    }
    // Reuse only this owner's observations, never Chrome's active tab or another client's target.
    // ponytail: linear scan of live observations; index by owner if tab counts make this measurable.
    let prefix = format!("chrome:{owner}:");
    let mut matches = observations.iter().filter_map(|(key, observation)| {
        let tag = key.strip_prefix(&prefix).filter(|tag| valid_tag(tag))?;
        (observation.captured.elapsed() <= Duration::from_secs(180)
            && args.get("snapshotId").is_none_or(|id| id.as_str() == Some(&observation.id)))
            .then_some(tag)
    });
    let tag = matches.next().filter(|_| matches.next().is_none()).ok_or(
        "缺少 tabTag，且无法从本会话 snapshotId 或唯一有效观察确定目标。请复制上次返回的 tabTag；没有则调用 {\"operation\":\"tabs\",\"query\":\"目标标题或URL片段\"}，再携带 tabTag 继续；act 还需最新 snapshotId 和 action/actions",
    )?;
    args.insert("tabTag".into(), json!(tag));
    Ok(Value::Object(args))
}

pub(crate) async fn execute_chrome(root: &Path, args: &Value, owner: &str) -> Result<Value, String> {
    let thread_id = tool_owner(root, owner)?;
    let app = APP.get().ok_or("网页工具仅在 Nova 桌面应用内可用")?;
    // 浏览器与桌面共用一块屏幕：数字员工任务同样排队、询问。
    if !crate::tool_experience::is_operation(args) {
        // 只读的查看标签/页面放行；会改变浏览器画面的操作前才征得用户同意。
        let input = !matches!(args["operation"].as_str(), Some("tabs" | "inspect" | "screenshot" | "devtools" | "status" | "connect" | "downloads"));
        crate::employee::claim_desktop(owner, input).await?;
    }
    let (args, inferred) = infer_operation(args, "tabTag");
    let args = &args;
    let normalized = chrome_args(args, &thread_id, &app.state::<BrowserState>().observations.lock().unwrap())?;
    if matches!(normalized["operation"].as_str(), Some("inspect" | "screenshot")) {
        if let Err(reason) = snapshot_options(&normalized, normalized["operation"] == "screenshot") {
            // Only argument failures before any browser command are safe to mark not_executed.
            return Ok(json!({"status":"not_executed","error":"invalid_arguments","reason":reason,
                "completedActions":0,"inputAttempted":false,"tabTag":normalized["tabTag"],
                "next":"先用 nextCall 获取页面，再按目标选择 DOM 操作或一种截图裁剪；不要原样重试错误参数",
                "nextCall":{"operation":"inspect","tabTag":normalized["tabTag"]}}));
        }
    }
    let normalized = resolve_targets(root, &normalized, owner, "chrome").await?;
    let mut result = SHOT_OWNER.scope(owner.to_owned(), execute_chrome_inner(root, &normalized, owner)).await?;
    if let Some(op) = inferred { result["operationInferred"] = json!(op); }
    if args.get("tabTag").is_none() && normalized["tabTag"].is_string() {
        if !result["tabTag"].is_string() { result["tabTag"] = normalized["tabTag"].clone(); }
        result["targetResolvedFrom"] = json!(if args.get("snapshotId").is_some() { "snapshotId" } else { "sessionObservation" });
    }
    Ok(with_experience_hint("chrome", &thread_id, result))
}

async fn execute_chrome_inner(root: &Path, args: &Value, owner: &str) -> Result<Value, String> {
    // An act consumes its input snapshot; always return a real observation, never mint a stale ID.
    let mut args = args.clone();
    if args["operation"] == "act" && args["feedback"] == "none" { args["feedback"] = json!("inspect"); }
    let args = &args;
    let app = APP.get().ok_or("网页工具仅在 Nova 桌面应用内可用")?;
    let thread_id = tool_owner(root, owner)?;
    let operation = args["operation"].as_str().unwrap_or_default();
    if operation == "run" {
        return Box::pin(crate::altair_run::browser(root, args, owner, "chrome")).await;
    }
    if crate::tool_experience::is_operation(args) {
        // 剑谱整理：用户在 Chrome 里的演示录制代替本会话标签观察做来源校验。
        if let Some(id) = args["experience"]["recordingId"].as_str().filter(|s| !s.is_empty()).map(str::to_string) {
            if operation != "experience_save" { return Err("recordingId仅用于experience_save".into()); }
            let mut args = args.clone();
            return tokio::task::spawn_blocking(move || {
                let observed = crate::jianpu::scope_of(&id, "chrome", args["experience"]["scope"].as_str().unwrap_or_default())?;
                args["snapshotId"] = json!(format!("{}{id}", crate::tool_experience::DEMO));
                let result = crate::tool_experience::execute(&crate::tool_experience::dir(), "chrome", &thread_id, &args, Some(&observed))?;
                if let Some(route) = result["experience"]["id"].as_str() { crate::jianpu::link_route(&id, route); }
                Ok(result)
            }).await.map_err(|e| e.to_string())?;
        }
        let observed = if operation == "experience_search" { None } else {
            let tag = args["tabTag"].as_str().ok_or("经验保存/反馈需tabTag")?;
            let state = app.state::<BrowserState>();
            let observations = state.observations.lock().unwrap();
            let observation = observations.get(&format!("chrome:{thread_id}:{tag}"))
                .filter(|o| args["snapshotId"].as_str() == Some(&o.id) && o.captured.elapsed() <= Duration::from_secs(180))
                .ok_or("保存/反馈经验前需本会话该标签最新inspect/screenshot（180秒内）")?;
            let url = tauri::Url::parse(observation.pages["pages"][0]["url"].as_str().ok_or("观察缺少网站URL")?).map_err(|e| e.to_string())?;
            Some(url.origin().ascii_serialization())
        };
        let args = args.clone();
        return tokio::task::spawn_blocking(move || crate::tool_experience::execute(
            &crate::tool_experience::dir(), "chrome", &thread_id, &args, observed.as_deref()))
            .await.map_err(|e| e.to_string())?;
    }
    let connection = crate::chrome_browser::connect(app).await?;
    if operation == "downloads" {
        return crate::chrome_browser::request(app, operation, args.clone()).await
            .map_err(|e| format!("{e}；下载查询需要 Nova Chrome 0.1.6，请确认扩展已更新并重新加载"));
    }
    if matches!(operation, "connect" | "status") {
        let mut connection = connection;
        if connection["connected"] == true {
            match crate::chrome_browser::request(app, "status", json!({})).await {
                Ok(capabilities) => {
                    connection["incognitoAllowed"] = capabilities["incognitoAllowed"].clone();
                    connection["version"] = capabilities["version"].clone();
                }
                Err(error) => connection["capabilityError"] = json!(format!("{error}；请重新加载Nova Chrome扩展")),
            }
            if connection["browsers"].as_array().is_some_and(|b| b.len() > 1) {
                connection["extensions"] = json!(crate::chrome_browser::extension_statuses(app).await);
            }
        }
        return Ok(connection);
    }
    let observe = match args["observe"].as_str() {
        None => "inspect",
        Some(mode @ ("inspect" | "screenshot" | "none")) => mode,
        Some(_) => return Err("observe 必须为 inspect/screenshot/none".into()),
    };
    // Anything that changes the real screen or focus shares one lease with jianlai; the other
    // tool's stale desktop snapshot is retired explicitly instead of failing mid-batch later.
    let mutating = matches!(operation, "open" | "new_tab" | "select_tab" | "close_tab" | "goto" | "back" | "forward" | "reload" | "act" | "evaluate");
    let _lease = if mutating {
        match crate::jianlai::lease_input("chrome") {
            Ok(lease) => Some(lease),
            Err(error) if matches!(operation, "act" | "evaluate") => return Ok(json!({"status":"not_executed","completedActions":0,"inputAttempted":false,
                "reason":error,"basedOnSnapshotId":args["snapshotId"],"verification":"unverified","browser":"chrome","tabTag":args["tabTag"]})),
            Err(error) => return Err(error),
        }
    } else { None };
    if matches!(operation, "tabs" | "open" | "new_tab") {
        let mut params = args.clone();
        if let Some(url) = args["url"].as_str() {
            params["url"] = json!(normalized_url(url)?.to_string());
        }
        strip_observe_keys(&mut params);
        let mut value = crate::chrome_browser::request(app, operation, params).await?;
        let mut observe_args = args.clone();
        let mut target = None;
        if operation == "tabs" {
            // `query` binds deterministically when exactly one tab matches; no guessing the active tab.
            if let Some(q) = args["query"].as_str().map(str::trim).filter(|q| !q.is_empty()) {
                let q = q.to_lowercase();
                let all = value["tabs"].as_array().cloned().unwrap_or_default();
                let matched: Vec<Value> = all.iter().filter(|t| ["title", "url"].iter()
                    .any(|k| t[*k].as_str().is_some_and(|s| s.to_lowercase().contains(&q)))).cloned().collect();
                let controllable: Vec<&Value> = matched.iter().filter(|t| t["controllable"] != false).collect();
                value["totalTabs"] = json!(all.len());
                value["query"] = json!(q);
                match controllable.as_slice() {
                    [one] => { value["tabTag"] = one["tag"].clone(); target = one["tag"].as_str().map(str::to_owned); }
                    [] => value["next"] = json!("没有可操作标签匹配 query；换关键词，或 open(url) 新开标签"),
                    _ => value["next"] = json!("query 匹配到多个标签：按 title/url 选一个 tabTag 再 inspect；重复标签优先选 active:true 的，或换更具体的 query"),
                }
                value["tabs"] = json!(matched.into_iter().map(compact_tab).collect::<Vec<_>>());
                if let Some(a) = observe_args.as_object_mut() { a.remove("query"); }
            }
        } else {
            if args["url"].is_string() { target = value["tabTag"].as_str().map(str::to_owned); }
        }
        if let (Some(tag), false) = (target, observe == "none") {
            let observed = observe_tab(app, root, &thread_id, &tag, &observe_args, observe, Duration::from_secs(8)).await;
            value.as_object_mut().unwrap().extend(observed.as_object().cloned().unwrap_or_default());
            value["tabTag"] = json!(tag);
            value["browser"] = json!("chrome");
        }
        return Ok(value);
    }
    let tag = args["tabTag"].as_str().ok_or("缺少 tabTag")?;
    if operation == "devtools" {
        // Read-only: console/network recorded by the extension's debugger session, no screen change.
        let mut params = args.clone();
        strip_observe_keys(&mut params);
        return crate::chrome_browser::request(app, operation, params).await
            .map_err(|e| if e.contains("未知 Chrome 操作") { format!("{e}；devtools 需要 Nova Chrome 0.1.9，请更新并在 chrome://extensions 重新加载扩展") } else { e });
    }
    if operation == "stop" {
        crate::chrome_browser::stop(app, tag);
        app.state::<BrowserState>().observations.lock().unwrap().remove(&format!("chrome:{thread_id}:{tag}"));
    }
    if matches!(
        operation,
        "select_tab" | "close_tab" | "goto" | "back" | "forward" | "reload" | "stop"
    ) {
        let mut params = args.clone();
        if operation == "goto" {
            params["url"] =
                json!(normalized_url(args["url"].as_str().unwrap_or_default())?.to_string());
        }
        strip_observe_keys(&mut params);
        let mut value = crate::chrome_browser::request(app, operation, params).await?;
        if matches!(
            operation,
            "close_tab" | "goto" | "back" | "forward" | "reload" | "stop"
        ) {
            app.state::<BrowserState>()
                .observations
                .lock()
                .unwrap()
                .retain(|key, _| !key.ends_with(&format!(":{tag}")));
        }
        // Navigation returns the loaded page directly: no separate inspect round-trip.
        if matches!(operation, "select_tab" | "goto" | "back" | "forward" | "reload") && observe != "none" {
            let observed = observe_tab(app, root, &thread_id, tag, args, observe, Duration::from_secs(8)).await;
            value.as_object_mut().unwrap().extend(observed.as_object().cloned().unwrap_or_default());
            value["tabTag"] = json!(tag);
            value["browser"] = json!("chrome");
        }
        return Ok(value);
    }
    if !matches!(operation, "inspect" | "screenshot" | "act" | "evaluate") {
        return Err("未知 chrome 操作".into());
    }
    let mut value = control_session(app, root, chrome_session(&thread_id, tag), operation, args).await?;
    value["tabTag"] = json!(tag);
    value["browser"] = json!("chrome");
    if matches!(operation, "act" | "evaluate") && args["feedback"] != "none" {
        if let Some(target) = opened_tab_target(&value).map(str::to_owned) {
            let mode = if args["feedback"] == "screenshot" { "screenshot" } else { "inspect" };
            let mut observe_args = args.clone();
            if args["feedback"] == "inspect" { observe_args["visual"] = json!("none"); }
            let observed = observe_tab(app, root, &thread_id, &target, &observe_args, mode, Duration::from_secs(8)).await;
            value.as_object_mut().unwrap().extend(observed.as_object().cloned().unwrap_or_default());
            value["tabTag"] = json!(target);
        }
    }
    if matches!(operation, "act" | "evaluate") && value["status"] != "executed" { value = errors_first(value); }
    Ok(value)
}

/// 仅调整键顺序：失败时把状态、原因和重发所需 ID 放在长观察之前，输出被截断也不丢。
fn errors_first(value: Value) -> Value {
    let Value::Object(mut rest) = value else { return value };
    let mut out = serde_json::Map::new();
    for key in ["status", "reason", "completedActions", "inputAttempted", "snapshotId", "tabTag", "observationError", "next"] {
        if let Some(v) = rest.remove(key) { out.insert(key.into(), v); }
    }
    out.extend(rest);
    Value::Object(out)
}

// Observation-only parameters ride along with navigation calls; the extension must not see them.
fn strip_observe_keys(params: &mut Value) {
    if let Some(object) = params.as_object_mut() {
        for key in ["observe", "query", "scope", "maxItems", "maxTextChars", "maxEdge", "visual", "settleMs"] {
            object.remove(key);
        }
    }
}

fn chrome_session(thread_id: &str, tag: &str) -> Session {
    Session {
        thread_id: thread_id.into(),
        active_tab: format!("chrome:{thread_id}:{tag}"),
        cancel: Arc::new(AtomicBool::new(false)),
    }
}

fn opened_tabs(before: &Value, after: &Value, source: &str) -> Vec<Value> {
    let Some(before) = before["tabs"].as_array() else { return Vec::new() };
    let known: std::collections::HashSet<_> = before.iter().filter_map(|tab| tab["tag"].as_str()).collect();
    after["tabs"].as_array().into_iter().flatten().filter(|tab|
        tab["openerTag"] == source && tab["tag"].as_str().is_some_and(|tag| !known.contains(tag)))
        .cloned().collect()
}

fn opened_tab_target(result: &Value) -> Option<&str> {
    match result["openedTabs"].as_array()?.as_slice() {
        [tab] if tab["active"] == true && tab["controllable"] == true && tab["allowed"] != false => tab["tag"].as_str(),
        _ => None,
    }
}

/// Poll the extension's tab inventory until the tab finished loading (status complete, no
/// pendingUrl). Bounded: a page that never settles still returns so the caller observes what is
/// there; older extensions without `loading` report complete immediately.
async fn wait_for_tab_load(app: &AppHandle, tag: &str, budget: Duration) -> Value {
    let started = std::time::Instant::now();
    // Let the navigation commit before trusting the previous page's "complete".
    tokio::time::sleep(Duration::from_millis(120)).await;
    let mut state = json!({"status":"unknown"});
    loop {
        match crate::chrome_browser::request(app, "tabs", json!({})).await {
            Ok(value) => {
                let Some(tab) = value["tabs"].as_array().and_then(|tabs| tabs.iter().find(|t| t["tag"] == tag)).cloned() else {
                    state = json!({"status":"closed"});
                    break;
                };
                let loading = tab["loading"] == true;
                state = json!({"status": if loading {"loading"} else {"complete"}, "url": tab["url"], "title": tab["title"]});
                if !loading { break; }
            }
            Err(error) => { state = json!({"status":"unknown","error":error}); break; }
        }
        if started.elapsed() >= budget { break; }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    state["waitedMs"] = json!(started.elapsed().as_millis());
    state
}

/// Observe a tab right after a navigation-style operation so the caller gets snapshotId/DOM in
/// the same reply. Load waits and "not ready" retries share one bounded budget; observation
/// failure is reported alongside the completed operation, never as an operation failure.
async fn observe_tab(app: &AppHandle, root: &Path, thread_id: &str, tag: &str, args: &Value, mode: &str, budget: Duration) -> Value {
    let started = std::time::Instant::now();
    let mut result = json!({"load": wait_for_tab_load(app, tag, budget).await});
    if result["load"]["status"] == "closed" {
        result["observationError"] = json!("标签已关闭，无法观察");
        return result;
    }
    let mut observe_args = json!({"operation":mode,"tabTag":tag,"fullPage":false});
    observe_args["scope"] = args.get("scope").filter(|v| !v.is_null()).cloned().unwrap_or(json!("viewport"));
    for key in ["query", "maxItems", "maxTextChars", "maxEdge", "visual"] {
        if let Some(value) = args.get(key).filter(|v| !v.is_null()) { observe_args[key] = value.clone(); }
    }
    loop {
        match control_session(app, root, chrome_session(thread_id, tag), mode, &observe_args).await {
            Ok(observed) => {
                result.as_object_mut().unwrap().extend(observed.as_object().cloned().unwrap_or_default());
                result["observed"] = json!(mode);
                break;
            }
            Err(error) if error.contains("尚未就绪") && started.elapsed() < budget => tokio::time::sleep(Duration::from_millis(250)).await,
            Err(error) => {
                result["observationError"] = json!(format!("{error}；操作本身已完成，页面可能仍在加载，稍后 inspect"));
                break;
            }
        }
    }
    result
}

fn state_dir(app: &AppHandle) -> std::path::PathBuf {
    app.state::<AppState>().config_dir.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn popup_feedback_follows_only_one_new_active_child() {
        let before = json!({"tabs":[{"tag":"source"},{"tag":"old-child","openerTag":"source"}]});
        for window in [1, 2] {
            let child = json!({"tag":"new-child","openerTag":"source","active":true,"controllable":true,"windowId":window});
            let after = json!({"tabs":[{"tag":"source"},{"tag":"old-child","openerTag":"source"},
                {"tag":"unrelated","openerTag":"other","active":true},child]});
            let opened = opened_tabs(&before, &after, "source");
            assert_eq!(opened, vec![child.clone()], "tabs and separate popup windows share opener-based discovery");
            let mut result = json!({"openedTabs":opened});
            assert_eq!(opened_tab_target(&result), Some("new-child"));
            for (key, value) in [("active",false),("controllable",false),("allowed",false)] {
                let mut blocked = result.clone();
                blocked["openedTabs"][0][key] = json!(value);
                assert_eq!(opened_tab_target(&blocked),None,"{key}: never guess or focus a background/restricted tab");
            }
            result["openedTabs"].as_array_mut().unwrap().push(json!({"tag":"second-child","active":true,"controllable":true}));
            assert_eq!(opened_tab_target(&result),None,"multiple children require a model decision");
            assert!(opened_tabs(&after,&after,"source").is_empty(),"old children are not a new action effect");
            assert!(opened_tabs(&json!({}),&after,"source").is_empty(),"missing baseline cannot prove a new tab");
        }
    }

    #[test]
    fn inline_summary_lists_compact_elements_and_act_deltas() {
        let item = |n: usize, name: &str| json!({"ref":format!("d.{n}"),"nodeId":format!("doc:{n}"),"role":"input","tag":"input",
            "name":name,"fieldContext":"日期 [field 2/2]","region":"Filters panel","inView":n < 50,"editable":true,"disabled":false,
            "selected":false,"sort":"descending","column":{"table":0,"index":2,"name":"Units","group":"M Science"},
            "value":"2026-09-23","dateValue":"2026-09-23","point":{"x":42.5,"y":36.25},
            "viewportRect":{"x":40,"y":30,"width":100,"height":20},"documentRect":{"x":40,"y":1030,"width":100,"height":20},"tabIndex":0});
        let table = json!({"headers":["Name","Units"],"rows":[["Game","100"],["needle row","9"]],"returnedRows":2});
        let page = |items: Vec<Value>, text: &str| json!({"frame":0,"document":"doc","url":"https://a.test/","stamp":"[]","version":"v",
            "text":text,"visibleText":"Visible line","items":items,"tables":[table.clone()],"headings":[{"level":"H2","text":"Results"}],
            "coverage":{"scope":"all","note":"boilerplate","lazyImages":0}});
        let full = json!({"coverageGaps":[],"domCoordinates":"boilerplate","pages":[page((0..60).map(|n| item(n, &format!("Search {n}"))).collect(), "Visible line")]});
        let original = full.to_string();
        let summary = inline_summary(&full, &json!({"maxItems":61}), None);
        assert_eq!(full.to_string(), original, "Altair and documentPath must retain the full observation");
        let p = &summary["pages"][0];
        let elements = p["elements"].as_str().unwrap();
        assert_eq!(elements.lines().next(), Some("@ Filters panel"), "region printed once per run");
        assert_eq!(elements.lines().filter(|l| l.starts_with('@')).count(), 1);
        assert_eq!(elements.lines().nth(1), Some(r#"d.0 input "Search 0" value=2026-09-23 date=2026-09-23 selected=false sort=descending field="日期 [field 2/2]" col=0.2:"Units"/"M Science" editable"#));
        assert!(elements.lines().last().unwrap().ends_with(" offscreen"));
        for key in ["items", "stamp", "version", "document", "domCoordinates"] { assert!(p.get(key).is_none() && summary.get(key).is_none(), "{key}"); }
        assert!(p["coverage"].get("note").is_none() && p["coverage"]["lazyImages"] == 0);
        assert_eq!(p["text"], "", "document text already shown as visible text");
        assert_eq!(p["tables"], json!([table]));
        assert!(summary.to_string().len() < original.len() / 2, "compact lines must shrink the reply: {} vs {}", summary.to_string().len(), original.len());

        let searched = inline_summary(&full, &json!({"query":"needle","maxItems":1}), None);
        assert_eq!(searched["pages"][0]["matchingItems"], 0);
        assert_eq!(searched["pages"][0]["tables"][0]["rows"], json!([["needle row","9"]]));
        let limited = inline_summary(&full, &json!({"maxItems":5}), None);
        assert_eq!(limited["pages"][0]["inlineTruncated"], true);
        assert_eq!(limited["pages"][0]["matchingItems"], 60);

        let mut first = item(51, "Intelligence");
        first["blockedBy"] = json!("Search suggestions");
        let ordered = json!({"pages":[page(vec![first, item(1, "Intelligence")], "Results")]});
        let first_only = inline_summary(&ordered, &json!({"query":"Intelligence","maxItems":1}), None);
        let first_line = first_only["pages"][0]["elements"].as_str().unwrap();
        assert!(first_line.contains("d.51 ") && !first_line.contains("d.1 "), "truncation must retain the first result even when it is blocked or offscreen");

        // After an act: same document → only changed elements, new text and changed tables.
        let mut after_items: Vec<Value> = (1..60).map(|n| item(n, &format!("Search {n}"))).collect();
        after_items[0]["value"] = json!("2027-01-01");
        after_items[1]["point"] = json!({"x":1,"y":1}); // geometry only: unchanged
        after_items[55]["inView"] = json!(true); // scrolled into view
        after_items.push(item(99, "Apply"));
        let after = json!({"pages":[page(after_items, "Visible line\nSaved 3 rows")]});
        let delta = inline_summary(&after, &json!({}), Some(&full));
        let p = &delta["pages"][0];
        let lines: Vec<&str> = p["elements"].as_str().unwrap().lines().filter(|l| !l.starts_with('@')).collect();
        assert_eq!(lines.len(), 4, "{lines:?}");
        assert!(lines[0].starts_with("~d.1 ") && lines[0].contains("value=2027-01-01"));
        assert!(lines.iter().any(|l| l.starts_with("~d.56 ")) && lines.iter().any(|l| l.starts_with("+d.99 input \"Apply\"")));
        assert_eq!(lines[3], r#"- d.0 input "Search 0""#);
        assert_eq!(p["delta"], true);
        assert_eq!(p["unchangedItems"], 57);
        assert_eq!(p["text"], "Saved 3 rows");
        assert!(p.get("visibleText").is_none());
        assert_eq!(p["tables"], json!([]));
        assert_eq!(p["headings"], json!([]));
        // Another document (navigation) is never diffed.
        let mut navigated = after.clone();
        navigated["pages"][0]["document"] = json!("other");
        assert!(inline_summary(&navigated, &json!({}), Some(&full))["pages"][0].get("delta").is_none());
    }

    #[test]
    fn item_line_keeps_coordinate_geometry_for_canvas_and_scroll_only() {
        let canvas = json!({"ref":"f.1","role":"canvas","name":"","inView":true,"canvas":{"width":1000,"height":800},
            "point":{"x":5,"y":5},"viewportRect":{"x":0.4,"y":10,"width":500,"height":400}});
        assert_eq!(item_line(&canvas, ""), r#"f.1 canvas "" canvas=1000x800 rect=0,10,500,400"#);
        let pane = json!({"ref":"f.2","role":"div","name":"list","inView":true,"scroll":{"top":10,"height":900,"pointX":null},
            "viewportRect":{"x":0,"y":0,"width":200,"height":300},"blockedBy":"Cookie banner"});
        assert_eq!(item_line(&pane, "~"), r#"~f.2 div "list" blockedBy="Cookie banner" scroll={"top":10,"height":900} rect=0,0,200,300"#);
    }

    #[test]
    fn inline_summary_marks_each_preview_cut_and_handles_unicode() {
        let mut text = json!("é中😀x");
        assert!(truncate_inline_text(&mut text, 3));
        assert_eq!(text, "é中😀");
        assert!(!truncate_inline_text(&mut text, 3));
        assert!(truncate_inline_text(&mut text, 0));
        assert_eq!(text, "");
        let small = json!({"pages":[{"text":"ok","visibleText":"ok","items":[],"headings":[],"truncated":false}]});
        let summarize = |v: &Value| inline_summary(v, &json!({}), None);
        assert_eq!(summarize(&small)["pages"][0]["inlineTruncated"], false);
        for (key, value) in [("visibleText", json!("界".repeat(3001))),
            ("headings", json!(vec![json!({"text":"heading"});21])),
            ("items", json!([{"ref":"r","name":"","fieldContext":"exact","region":"界".repeat(121)}]))] {
            let mut full = small.clone();
            full["pages"][0][key] = value;
            let summary = summarize(&full);
            assert_eq!(summary["pages"][0]["inlineTruncated"], true, "{key}");
            assert_eq!(summary["pages"][0]["truncated"], false, "preview cuts are not collection gaps");
        }
    }

    #[test]
    fn spreadsheet_shortcuts_parse_and_clipboard_keys_carry_commands() {
        let k = |n| key_spec(n).map(|(key, code, vk, m, c)| (key, code, vk, m, c));
        assert_eq!(k("Ctrl+A"), Some(("a".into(), "KeyA".into(), 65, 2, None)));
        assert_eq!(k("Control+Shift+Z"), Some(("Z".into(), "KeyZ".into(), 90, 10, None)));
        assert_eq!(k("Ctrl+C").unwrap().4, Some("copy"));
        assert_eq!(k("Ctrl+V").unwrap().4, Some("paste"));
        assert_eq!(k("Ctrl+Shift+V").unwrap().4, None);
        assert_eq!(k("Shift+ArrowDown"), Some(("ArrowDown".into(), "ArrowDown".into(), 40, 8, None)));
        assert_eq!(k("Ctrl+Home").unwrap().3, 2);
        assert_eq!(k("F2").unwrap().2, 113);
        assert_eq!(k("Ctrl+1").unwrap().1, "Digit1");
        assert_eq!(k("Space").unwrap().0, " ");
        for bad in ["a", "Shift+a", "F13", "Hyper+A", "Ctrl+", "Ctrl+Foo"] { assert!(key_spec(bad).is_none(), "{bad}"); }
    }

    #[test]
    fn dom_refresh_keeps_screenshot_only_for_the_same_viewport() {
        let observed = |id: &str, stamp: Value, shot: bool| Observation { frames: Vec::new(), id: id.into(),
            pages: json!({"pages":[{"stamp":stamp}]}), captured: std::time::Instant::now(), screenshot: shot, full_page: false,
            images: if shot { vec![ScreenshotImage { id: format!("{id}-0"), path: "a.png".into(), x: 0., y: 0., width: 10., height: 10., pixels: (10, 10), delivered: None }] } else { Vec::new() } };
        let shot = observed("old", json!("[\"u\",0]"), true);
        let mut same = observed("new", json!("[\"u\",0]"), false);
        assert_eq!(carry_screenshot(&mut same, Some(&shot)), Some(vec!["old-0".to_string()]));
        assert!(same.screenshot && same.captured == shot.captured && same.images[0].id == "old-0");
        // Scrolled/navigated, unknown stamp, no prior image, or already a fresh screenshot: nothing carried.
        for (mut next, previous) in [(observed("new", json!("[\"u\",9]"), false), Some(&shot)), (observed("new", Value::Null, false), Some(&observed("old", Value::Null, true))),
            (observed("new", json!("[\"u\",0]"), false), Some(&observed("old", json!("[\"u\",0]"), false))), (observed("new", json!("[\"u\",0]"), true), Some(&shot)),
            (observed("new", json!("[\"u\",0]"), false), None)] {
            let had = next.screenshot;
            assert_eq!(carry_screenshot(&mut next, previous), None);
            assert_eq!(next.screenshot, had);
        }
    }

    #[test]
    fn viewport_frames_share_delivery_without_losing_coordinates_or_scope() {
        use xcap::image::{Rgba, RgbaImage};
        let dir = tempfile::tempdir().unwrap();
        let page = RgbaImage::from_pixel(320, 320, Rgba([255, 255, 255, 255]));
        let observed = |id: &str| Observation { frames: Vec::new(), id: id.into(),
            pages: json!({"pages":[{"stamp":"same-url-scroll-zoom","document":"doc"}]}),
            captured: std::time::Instant::now(), screenshot: true, full_page: false, images: Vec::new() };
        let deliver = |o: &mut Observation, p: Option<&Observation>, stream: Option<&str>, pixels: RgbaImage| {
            let path = dir.path().join(format!("{}.webp", o.id));
            deliver_screenshot(o, p, stream.map(str::to_owned), pixels, path, (0., 0., 640., 640.)).unwrap()
        };
        let mut first = observed("first");
        let key = deliver(&mut first, None, Some("C1:viewport"), page.clone());
        assert_eq!(key.len(), 1);
        assert_eq!(key[0]["frameStream"], "C1:viewport");
        assert!(key[0]["path"].is_string() && key[0]["unchanged"].is_null());
        let mut typed = page.clone();
        typed.put_pixel(150, 150, Rgba([0, 0, 0, 255]));
        let mut next = observed("next");
        let changed = deliver(&mut next, Some(&first), Some("C1:viewport"), typed.clone());
        assert!(changed[0]["unchanged"].is_null() && changed[0]["path"] != key[0]["path"], "any visible change sends a complete new image");
        assert_eq!(next.images[0].point(150., 150.).unwrap(), (300., 300.));
        assert_eq!(next.images[0].pixels, (320, 320));
        assert_eq!(next.images[0].delivered.as_ref().unwrap().seen.get_pixel(150, 150), typed.get_pixel(150, 150), "preflight sees the complete current image");
        let mut unchanged = observed("unchanged");
        let same = deliver(&mut unchanged, Some(&next), Some("C1:viewport"), typed.clone());
        assert_eq!(same.len(), 1);
        assert_eq!(same[0]["unchanged"], true);
        assert_eq!(same[0]["path"], changed[0]["path"], "unchanged frames reuse the delivered file");
        assert_eq!(unchanged.images[0].path.to_str(), changed[0]["path"].as_str());
        for case in ["scroll", "document", "expired", "tab", "size"] {
            let mut prior = next.clone();
            let mut fresh = observed(case);
            if case == "scroll" { fresh.pages["pages"][0]["stamp"] = json!("new-scroll"); }
            if case == "document" { fresh.pages["pages"][0]["document"] = json!("new-doc"); }
            if case == "expired" { prior.captured -= Duration::from_secs(181); }
            let stream = if case == "tab" { "C2:viewport" } else { "C1:viewport" };
            let pixels = if case == "size" { RgbaImage::new(321, 320) } else { typed.clone() };
            let images = deliver(&mut fresh, Some(&prior), Some(stream), pixels);
            assert!(images[0]["unchanged"].is_null(), "{case}");
        }
        let mut crop = observed("crop");
        let images = deliver(&mut crop, Some(&first), None, page);
        assert!(images[0]["path"].is_string() && images[0]["frameStream"].is_null());
        assert!(crop.images[0].delivered.is_none(), "explicit crops always remain standalone");
    }
    #[test]
    fn preflight_corrects_frame_from_unique_ref() {
        let frame = |id: &str| Frame { id: id.into(), parent: None, session: None, context: 0 };
        let observation = Observation { frames: vec![frame("top"), frame("child")], id: "s".into(),
            pages: json!({"pages":[{"items":[{"ref":"a:1"}]},{"items":[{"ref":"b:51","editable":true}]}]}),
            captured: std::time::Instant::now(), screenshot: false, full_page: false, images: Vec::new() };
        let mut click = parse_action(r#"{"action":"click","frame":0,"ref":"b:51"}"#).unwrap();
        preflight(&observation, &mut click, None).unwrap();
        assert!(matches!(click, Action::Click { frame: 1, .. }));
        let mut fake = parse_action(r#"{"action":"click","frame":0,"ref":"c:1"}"#).unwrap();
        assert!(preflight(&observation, &mut fake, None).is_err());
        // Keyboard actions need a preceding explicit field target to reuse an old snapshot.
        let act = |json: &str| vec![parse_action(json).unwrap()];
        assert!(refs_in(&observation, &act(r#"{"action":"click","frame":0,"ref":"b:51"}"#)));
        assert!(!refs_in(&observation, &act(r#"{"action":"click","frame":0,"ref":"c:1"}"#)));
        assert!(!refs_in(&observation, &act(r#"{"action":"press","key":"Enter"}"#)));
        assert!(!refs_in(&observation, &[]));
        let batch=|text:&str| parse_actions(&serde_json::from_str::<Value>(text).unwrap(),16).unwrap();
        for good in [
            r#"{"actions":[{"action":"click","ref":"b:51"},{"action":"type","text":"query"},{"action":"press","key":"Enter"}]}"#,
            r#"{"actions":[{"action":"fill","ref":"b:51","text":"query"},{"action":"wait","ms":10},{"action":"press","key":"Enter"}]}"#,
        ] {assert!(refs_in(&observation,&batch(good)),"{good}");}
        for bad in [
            r#"{"actions":[{"action":"type","text":"query"},{"action":"click","ref":"b:51"}]}"#,
            r#"{"actions":[{"action":"click","ref":"a:1"},{"action":"press","key":"Enter"}]}"#,
            r#"{"actions":[{"action":"click","ref":"b:51"},{"action":"click_at","x":10,"y":10}]}"#,
            r#"{"actions":[{"action":"wait","ms":10}]}"#,
        ] {assert!(!refs_in(&observation,&batch(bad)),"{bad}");}
    }

    #[test]
    fn errors_first_only_reorders() {
        let value = json!({"pages":[1],"snapshotId":"s","status":"not_executed","reason":"r","x":1});
        let ordered = errors_first(value.clone());
        assert_eq!(ordered, value);
        assert_eq!(ordered.as_object().unwrap().keys().take(3).collect::<Vec<_>>(), ["status", "reason", "snapshotId"]);
    }

    #[test]
    fn full_page_tiles_reuse_frames_without_crossing_coordinate_bases() {
        use xcap::image::{Rgba, RgbaImage};
        let dir = tempfile::tempdir().unwrap();
        let page = RgbaImage::from_pixel(128, 96, Rgba([255,255,255,255]));
        let observed = |id: &str, full_page| Observation { frames: Vec::new(), id: id.into(),
            pages: json!({"pages":[{"stamp":"same-url-scroll-zoom","document":"doc"}]}),
            captured: std::time::Instant::now(), screenshot: true, full_page, images: Vec::new() };
        let deliver = |o: &mut Observation, p: Option<&Observation>, stream: &str, xywh, pixels: RgbaImage| {
            let path = dir.path().join(format!("{}-{}.webp",o.id,o.images.len()));
            deliver_screenshot(o,p,Some(stream.into()),pixels,path,xywh).unwrap()
        };
        let mut first = observed("first", true);
        let top = deliver(&mut first,None,"C1:document:0",(0.,0.,256.,192.),page.clone());
        let bottom = deliver(&mut first,None,"C1:document:1",(0.,192.,256.,192.),page.clone());
        assert_ne!(top[0]["imageId"],bottom[0]["imageId"]);
        let mut changed = page.clone();
        changed.put_pixel(112,80,Rgba([0,0,0,255]));
        let mut second = observed("second", true);
        let unchanged = deliver(&mut second,Some(&first),"C1:document:0",(0.,0.,256.,192.),page.clone());
        let updated = deliver(&mut second,Some(&first),"C1:document:1",(0.,192.,256.,192.),changed.clone());
        assert_eq!(unchanged[0]["unchanged"],true);
        assert_eq!(unchanged[0]["path"],top[0]["path"]);
        assert!(updated[0]["unchanged"].is_null() && updated[0]["path"] != bottom[0]["path"]);
        assert_eq!(updated[0]["frameStream"],"C1:document:1");
        assert_eq!(second.images[1].point(112.,80.).unwrap(),(224.,352.),"image pixels map to document CSS coordinates");
        assert_eq!(second.images[1].delivered.as_ref().unwrap().seen.as_ref(),&changed);
        // Geometry changes must refuse reuse even when the scaled image dimensions happen to match.
        for (full_page, xywh) in [(true,(0.,128.,256.,192.)),(true,(0.,192.,128.,96.)),(false,(0.,192.,256.,192.))] {
            let mut resized=observed("reset",full_page);
            let reset=deliver(&mut resized,Some(&second),"C1:document:1",xywh,changed.clone());
            assert!(reset[0]["unchanged"].is_null());
        }
        let mut viewport=observed("viewport",false);
        let key=deliver(&mut viewport,Some(&second),"C1:viewport",(0.,0.,256.,192.),page);
        assert!(key[0]["unchanged"].is_null(),"action feedback cannot reuse a document tile as its viewport frame");
    }
    #[test]
    fn snapshot_options_ignore_only_empty_placeholders_and_reject_invalid_crops() {
        for args in [json!({}), json!({"region":null,"ref":null}), json!({"region":{},"ref":""}),
            json!({"region":{},"ref":" \t","maxEdge":null})] {
            for with_image in [false, true] {
                assert_eq!(snapshot_options(&args, with_image).unwrap(), (None, None), "{args}");
            }
        }
        let region = json!({"x":0,"y":20,"width":100,"height":80});
        let args = json!({"region":region,"ref":""});
        assert_eq!(snapshot_options(&args, true).unwrap(), (Some(&region), None));
        assert!(snapshot_options(&args, false).unwrap_err().contains("inspect 不接受"));
        let args = json!({"region":{},"ref":"observed:1"});
        assert_eq!(snapshot_options(&args, true).unwrap(), (None, Some("observed:1")));
        assert!(snapshot_options(&args, false).is_err());
        let conflict = json!({"region":region,"ref":"observed:1"});
        assert!(snapshot_options(&conflict, true).unwrap_err().contains("不能同时提供"));
        assert!(snapshot_options(&json!({"ref":123}), true).is_err());
        for region in [json!({"x":0}), json!({"x":0,"y":0,"width":0,"height":1}),
            json!({"x":-1,"y":0,"width":1,"height":1}), json!({"x":0,"y":0,"width":1,"height":null}),
            json!([]), json!(""), json!(false)] {
            assert!(snapshot_options(&json!({"region":region}), true).is_err(), "{region}");
        }
        for edge in [json!(-1), json!(1), json!(319), json!(3841), json!(320.5), json!("1600")] {
            assert!(snapshot_options(&json!({"maxEdge":edge}), true).is_err(), "{edge}");
        }
        for edge in [0, 320, 1600, 3840] {
            assert!(snapshot_options(&json!({"maxEdge":edge}), true).is_ok());
        }
    }

    #[test]
    fn chrome_inspect_ignores_other_operations_fields_before_validation() {
        let expected = json!({"operation":"inspect","tabTag":"C1-test","snapshotId":"old",
            "query":"订单号","scope":"viewport","visual":"none","maxItems":10,"maxTextChars":500,"maxEdge":320});
        let unrelated = json!({"region":{"x":0,"y":0,"width":0,"height":0},"ref":"placeholder","frame":99,
            "fullPage":true,"expandScroll":true,"tileOffset":999,"imageId":"old-image",
            "feedback":"unused","observe":"unused","settleMs":99999,
            "action":{"action":"click","ref":"placeholder"},"expression":"throw Error('must not run')"});
        let mut args = expected.clone();
        args.as_object_mut().unwrap().extend(unrelated.as_object().unwrap().clone());
        let normalized = chrome_args(&args, "owner", &HashMap::new()).unwrap();
        assert_eq!(normalized, expected);
        assert_eq!(snapshot_options(&normalized, false).unwrap(), (None, None));
        // The same fields retain their meaning for operations that actually use them.
        for operation in ["screenshot", "evaluate", "act"] {
            args["operation"] = json!(operation);
            assert_eq!(chrome_args(&args, "owner", &HashMap::new()).unwrap(), args);
        }
        args["operation"] = json!("screenshot");
        let normalized = chrome_args(&args, "owner", &HashMap::new()).unwrap();
        assert!(snapshot_options(&normalized, true).unwrap_err().contains("不能同时提供"));
    }

    #[test]
    fn chrome_recovers_missing_target_only_from_unambiguous_owned_observations() {
        let observation = Observation { frames: Vec::new(), pages: json!({}), id: "current".into(),
            captured: std::time::Instant::now(), screenshot: true, full_page: false,
            images: Vec::new() };
        let mut observations = std::collections::HashMap::new();
        let inspect = json!({"operation":"inspect"});
        assert!(chrome_args(&inspect, "owner", &observations).is_err());
        observations.insert("chrome:other:C1-test".into(), observation.clone());
        observations.insert("chrome:owner:another-root:C1-test".into(), observation.clone());
        assert!(chrome_args(&inspect, "owner", &observations).is_err());
        observations.insert("chrome:owner:C1-test".into(), observation.clone());
        assert_eq!(chrome_args(&inspect, "owner", &observations).unwrap()["tabTag"], "C1-test");
        let act = json!({"operation":"act","snapshotId":"current","action":{"action":"click","frame":0,"ref":"observed-ref"}});
        let normalized = chrome_args(&act, "owner", &observations).unwrap();
        assert_eq!(normalized["tabTag"], "C1-test");
        assert_eq!(normalized["action"], act["action"]);
        assert_eq!(normalized["snapshotId"], "current");
        for key in ["tabId", "browserId"] {
            let mut wrong_target = act.clone();
            wrong_target[key] = json!("another-target");
            assert!(chrome_args(&wrong_target, "owner", &observations).is_err());
            // 弱模型把合法 tag 填进了错字段：直接当 tabTag 用，不打回去重试。
            wrong_target[key] = json!("C1-test");
            let adopted = chrome_args(&wrong_target, "owner", &observations).unwrap();
            assert_eq!(adopted["tabTag"], "C1-test");
            assert!(adopted.get(key).is_none(), "{key} 不进入归一后的参数");
        }
        let mut second = observation.clone();
        second.id = "second".into();
        observations.insert("chrome:owner:C2-test".into(), second);
        assert!(chrome_args(&inspect, "owner", &observations).is_err());
        assert_eq!(chrome_args(&act, "owner", &observations).unwrap()["tabTag"], "C1-test");
        for operation in ["run", "evaluate", "experience_save", "experience_feedback", "goto", "close_tab"] {
            let args = json!({"operation":operation,"snapshotId":"second"});
            assert_eq!(chrome_args(&args, "owner", &observations).unwrap()["tabTag"], "C2-test");
        }
        for snapshot in [json!("stale"), json!(123), Value::Null] {
            assert!(chrome_args(&json!({"operation":"act","snapshotId":snapshot}), "owner", &observations).is_err());
        }
        for tag in [json!(""), json!(123), json!("C1:other"), Value::Null] {
            assert!(chrome_args(&json!({"operation":"run","tabTag":tag,"snapshotId":"current"}), "owner", &observations).is_err());
        }
        let explicit = json!({"operation":"inspect","tabTag":"C3-explicit"});
        assert_eq!(chrome_args(&explicit, "owner", &observations).unwrap(), explicit);
        observations.get_mut("chrome:owner:C1-test").unwrap().captured -= Duration::from_secs(181);
        assert!(chrome_args(&act, "owner", &observations).is_err());
        observations.remove("chrome:owner:C2-test");
        assert!(chrome_args(&inspect, "owner", &observations).is_err());
        for operation in ["connect", "status", "tabs", "open", "new_tab", "downloads", "experience_search"] {
            let args = json!({"operation":operation});
            assert_eq!(chrome_args(&args, "owner", &observations).unwrap(), args);
        }
        assert!(chrome_args(&json!({}), "owner", &observations).is_err());
    }

    #[test]
    fn background_tool_owners_are_stable_and_isolated() {
        let root = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let owner = tool_owner(root.path(), "client-a").unwrap();
        assert_eq!(owner, tool_owner(&root.path().join("."), "client-a").unwrap());
        assert_ne!(owner, tool_owner(root.path(), "client-b").unwrap());
        assert_ne!(owner, tool_owner(other.path(), "client-a").unwrap());
        assert!(tool_owner(root.path(), "").is_err());
        assert!(tool_owner(&root.path().join("missing"), "client-a").is_err());
    }

    #[test]
    fn pixel_crop_mapping_and_action_batches_are_bounded() {
        let image=ScreenshotImage{id:"image".into(),path:"unused.png".into(),x:100.,y:300.,width:600.,height:400.,pixels:(1200,800),delivered:None};
        assert_eq!(image.point(400.,200.).unwrap(),(300.,400.));
        assert!(image.point(1200.,0.).is_err());assert!(image.point(-1.,0.).is_err());assert!(image.point(f64::NAN,0.).is_err());
        let small=ScreenshotImage{pixels:(300,200),..image};assert_eq!(small.point(100.,50.).unwrap(),(300.,400.));
        assert!(parse_actions(&json!({"actions":[{"action":"fill","frame":0,"ref":"a","text":"one"},{"action":"click","frame":0,"ref":"b","button":"right","click_count":2}]}), 8).is_ok());
        for value in [json!({"actions":[]}),json!({"actions":[{"action":"click_at","x":1,"y":2,"button":"bad"}]}),
            json!({"action":{"action":"drag","x":1,"y":2,"to_x":3,"to_y":4,"duration_ms":99999}}),
            json!({"action":{"action":"wait","ms":3000}}),json!({"actions":[{"action":"press","key":"Enter"},{"action":"press","key":"bad"}]}),
            json!({"action":{"action":"wait","ms":1},"actions":[{"action":"wait","ms":1}]})] {assert!(parse_actions(&value, 8).is_err(),"{value}");}
        assert!(parse_actions(&json!({"actions":vec![json!({"action":"wait","ms":0});16]}), 16).is_ok());
        assert!(parse_actions(&json!({"actions":vec![json!({"action":"wait","ms":0});17]}), 16).is_err());
        assert!(parse_actions(&json!({"actions":vec![json!({"action":"wait","ms":0});9]}), 8).is_err());
        let image = xcap::image::RgbaImage::from_pixel(32, 24, xcap::image::Rgba([30, 90, 150, 255]));
        let mut webp = crate::visual_guard::encode_webp(&image).unwrap();
        assert_eq!(webp_dimensions(&webp).unwrap(), (32, 24));
        assert!(webp_dimensions(&webp[..23]).is_err());
        // VP8L stores width-1 and height-1 in 14 bits each, before pixel data.
        let header = webp.windows(4).position(|b| b == b"VP8L").unwrap() + 9;
        webp[header..header+4].copy_from_slice(&(7999u32 | (7999 << 14)).to_le_bytes());
        assert_eq!(webp_dimensions(&webp).unwrap(), (8000, 8000));
        webp[header..header+4].copy_from_slice(&(8000u32 | (7999 << 14)).to_le_bytes());
        assert!(webp_dimensions(&webp).unwrap_err().contains("安全预算"));
    }
    #[test]
    fn screenshot_budget_uses_actual_image_dimensions() {
        use base64::Engine;
        let image = xcap::image::RgbaImage::from_pixel(2400, 1200, xcap::image::Rgba([30, 90, 150, 255]));
        let webp = crate::visual_guard::encode_webp(&image).unwrap();
        let encoded = base64::engine::general_purpose::STANDARD.encode(&webp);
        let resized = screenshot_pixels(&encoded, 1600).unwrap();
        assert_eq!(resized.dimensions(), (1600, 800));
        let unchanged = screenshot_pixels(&encoded, 0).unwrap();
        assert_eq!(unchanged.dimensions(), (2400, 1200));
        assert_eq!(unchanged, image);
        assert_eq!(xcap::image::guess_format(&webp).unwrap(), xcap::image::ImageFormat::WebP);
        assert!(screenshot_pixels("not-base64", 1600).is_err());
    }
    #[test]
    fn native_browser_rejects_unsafe_urls_and_unstructured_actions() {
        assert_eq!(normalized_url("localhost:5173").unwrap().scheme(), "http");
        for url in [
            "file:///C:/secret",
            "javascript:alert(1)",
            "https://user:pass@example.com",
        ] {
            assert!(normalized_url(url).is_err(), "{url}");
        }
        assert!(parse_action(r#"{"action":"click","frame":0,"ref":"v1:0"}"#).is_ok());
        assert!(
            parse_action(r#"{"action":"click","frame":0,"ref":"v1:0","selector":"body"}"#).is_err()
        );
        assert!(parse_action(r#"{"action":"eval","code":"alert(1)"}"#).is_err());
        let error = parse_action(r#"{"action":"click","x":100,"y":100}"#).unwrap_err();
        assert!(error.contains("click_at") && error.contains("imageId 放在工具顶层"));
        // Harmless slips are repaired instead of bouncing the call.
        assert!(matches!(parse_action(r#"{"action":"press","key":"Enter","frame":0}"#), Ok(Action::Press { .. })));
        assert!(matches!(parse_action(r#"{"action":"click","ref":"v1:0"}"#), Ok(Action::Click { frame: 0, .. })));
        assert!(matches!(parse_action(r#"{"type":"fill","ref":"v1:0","text":"x"}"#), Ok(Action::Fill { frame: 0, .. })));
        assert_eq!(parse_actions(&json!({"action":"{\"action\":\"wait\",\"ms\":10}"}), 8).unwrap().len(), 1);
    }

    #[test]
    fn evaluate_validates_script_frame_and_tool_contract() {
        assert_eq!(evaluation_args(&json!({"expression":"document.title"})).unwrap(), ("document.title", 0));
        assert_eq!(evaluation_args(&json!({"expression":"1", "frame":11})).unwrap().1, 11);
        for args in [json!({}), json!({"expression":" "}), json!({"expression":32}),
            json!({"expression":"x".repeat(32001)}), json!({"expression":"1","frame":-1}),
            json!({"expression":"1","frame":12}), json!({"expression":"1","frame":0.5})] {
            assert!(evaluation_args(&args).is_err(), "{args}");
        }
        let schema = crate::chrome_browser::tool_definition();
        assert!(schema["inputSchema"]["properties"]["operation"]["enum"].as_array().unwrap().contains(&json!("evaluate")));
        assert_eq!(schema["inputSchema"]["properties"]["expression"]["maxLength"], 32000);
    }

    #[test]
    fn missing_operation_is_inferred() {
        let op = |v: Value, key| infer_operation(&v, key).1;
        assert_eq!(op(json!({"tabTag":"C1-a","action":{"action":"press","key":"Enter"}}), "tabTag"), Some("act"));
        assert_eq!(op(json!({"tabTag":"C1-a","plan":{"task":"t"}}), "tabTag"), Some("run"));
        assert_eq!(op(json!({"url":"https://a.com"}), "tabTag"), Some("open"));
        assert_eq!(op(json!({"url":"https://a.com","tabTag":"C1-a"}), "tabTag"), Some("goto"));
        assert_eq!(op(json!({"url":"https://a.com","browserId":"b"}), "browserId"), Some("open"));
        assert_eq!(op(json!({"tabTag":"C1-a"}), "tabTag"), Some("inspect"));
        assert_eq!(op(json!({"tabTag":"C1-a","expression":"document.title"}), "tabTag"), Some("evaluate"));
        assert_eq!(op(json!({}), "tabTag"), Some("tabs"));
        assert_eq!(op(json!({"operation":"tabs"}), "tabTag"), None);
    }
}
