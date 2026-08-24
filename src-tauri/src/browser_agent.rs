//! 截图分析通过后台临时会话完成；计划运行会话持久保留在双子座独立历史。
//! agent/skill 自己调用 Playwright（MCP 或 CLI），Nova 不内置 Playwright 运行器。
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use base64::Engine;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager, State};

use crate::threads::PromptImage;
use crate::{dispatch_prompt, is_running, AppState, AgentKind, Item};

const ANALYSIS_TIMEOUT_MS: u64 = 120_000;
const PLAN_RUN_TIMEOUT_MS: u64 = 600_000;

pub struct BrowserAgentState {
    /// 后台会话：threadId -> oneshot 等待者
    pending: Mutex<HashMap<String, tokio::sync::oneshot::Sender<Result<String, String>>>>,
    /// 双子座计划执行会话需持久保留；截图分析会话结束后删除。
    retained: Mutex<HashSet<String>>,
}

impl BrowserAgentState {
    pub fn new() -> Self {
        Self {
            pending: Mutex::new(HashMap::new()),
            retained: Mutex::new(HashSet::new()),
        }
    }
}

/// 从线程里提取最后一条 Assistant 文本
fn last_assistant_text(thread: &crate::Thread) -> Option<String> {
    thread.items.iter().rev().find_map(|it| match it {
        Item::Assistant { text, .. } => Some(text.clone()),
        _ => None,
    })
}

/// 建一个后台临时会话并等待 agent 返回最终文本；用完即删。
async fn run_ephemeral_prompt(
    app: AppHandle,
    state: &State<'_, AppState>,
    cwd: String,
    prompt: String,
    timeout_ms: u64,
    agent_kind: AgentKind,
    model: Option<String>,
    retain_browser_thread: bool,
    images: Vec<PromptImage>,
) -> Result<String, String> {
    let thread = crate::create_thread(
        app.clone(),
        state.clone(),
        cwd,
        Some(agent_kind),
        model.filter(|value| !value.trim().is_empty()),
        None,
        None,
        Some(!retain_browser_thread), // 截图分析临时清理；计划执行保留到双子座历史
        None,
        None,
        None,
        None,
        None,
    )
    .await?;

    let thread_id = thread.id.clone();
    if retain_browser_thread {
        let mut store = state.store.lock().unwrap();
        if let Some(saved) = store.get_mut(&thread_id) {
            saved.browser_thread = true;
            saved.title = format!("双子座执行 · {}", chrono::Local::now().format("%m-%d %H:%M"));
            saved.ephemeral = false;
            store.save();
        }
        app.state::<AppState>().browser_agent.retained.lock().unwrap().insert(thread_id.clone());
        let _ = app.emit(crate::acp::EV_THREADS, json!({}));
    }
    let (tx, rx) = tokio::sync::oneshot::channel::<Result<String, String>>();
    {
        let st = app.state::<AppState>();
        st.browser_agent
            .pending
            .lock()
            .unwrap()
            .insert(thread_id.clone(), tx);
    }

    dispatch_prompt(&app, thread_id.clone(), prompt, images)?;

    // 轮询线程运行状态；结束后取最后一条 assistant 文本
    let app_clone = app.clone();
    let tid = thread_id.clone();
    tokio::spawn(async move {
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_millis(timeout_ms);
        loop {
            if tokio::time::Instant::now() > deadline {
                finish_ephemeral(&app_clone, &tid, Err("执行超时".into()));
                return;
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(600)).await;
            let (running, last_text) = {
                let st = app_clone.state::<AppState>();
                let store = st.store.lock().unwrap();
                match store.get(&tid) {
                    Some(t) => (is_running(&st, t), last_assistant_text(t)),
                    None => (false, None),
                }
            };
            if !running {
                let result = last_text.ok_or_else(|| "agent 未返回内容".to_string());
                finish_ephemeral(&app_clone, &tid, result);
                return;
            }
        }
    });

    match rx.await {
        Ok(res) => res,
        Err(_) => Err("执行通道被关闭".into()),
    }
}

fn finish_ephemeral(app: &AppHandle, thread_id: &str, result: Result<String, String>) {
    let tx = {
        let st = app.state::<AppState>();
        let mut guard = st.browser_agent.pending.lock().unwrap();
        guard.remove(thread_id)
    };
    if let Some(tx) = tx {
        let _ = tx.send(result);
    }
    // 双子座执行会话保留在独立历史；截图分析临时会话结束后删除。
    let retained = {
        let st = app.state::<AppState>();
        let retained = st.browser_agent.retained.lock().unwrap().remove(thread_id);
        retained
    };
    if retained {
        return;
    }
    let app2 = app.clone();
    let tid = thread_id.to_string();
    tauri::async_runtime::spawn(async move {
        let st = app2.state::<AppState>();
        let removed = {
            let mut store = st.store.lock().unwrap();
            if let Some(pos) = store.threads.iter().position(|t| t.id == tid) {
                let t = store.threads.remove(pos);
                store.save();
                Some(t)
            } else {
                None
            }
        };
        if removed.is_some() {
            let _ = app2.emit(crate::acp::EV_THREADS, json!({}));
        }
    });
}

/// 分析截图：后台临时会话让 agent 根据截图 + 目标描述推断稳定选择器。
/// 返回 { selector, confidence, reasoning } 的原始文本（前端再做 JSON 提取）。
#[tauri::command]
pub async fn analyze_screenshot(
    app: AppHandle,
    state: State<'_, AppState>,
    cwd: String,
    image_data_url: String,
    hint: String,
) -> Result<String, String> {
    // 先把截图落到磁盘，agent 通过路径读文件，避免 base64 阻塞通道
    let image_path = crate::browser::save_shot_to_disk(
        &state.config_dir,
        &image_data_url,
        Some(&format!("analysis-{}", uuid::Uuid::new_v4())),
    )?;

    let prompt = format!(
        "请分析这张网页截图：{path}\n\
         用户标记的目的：{hint}\n\
         任务：推断该标记区域对应的稳定、可复用的 CSS/Playwright 选择器。\n\
         要求：\n\
         1. 只输出一个 JSON 对象，不要任何前后缀说明；\n\
         2. 字段：selector（推荐选择器）、confidence（0-1 数字）、reasoning（一句话说明）；\n\
         3. 选择器优先级：data-testid > aria-label > role/text > id > name > 结构路径；\n\
         4. 如果无法确定，selector 置空，confidence 给 0。\n\
         现在直接输出 JSON。",
        path = image_path,
        hint = if hint.trim().is_empty() { "（未填写，请根据截图内容判断）" } else { hint.trim() },
    );

    run_ephemeral_prompt(app, &state, cwd, prompt, ANALYSIS_TIMEOUT_MS, AgentKind::Lyra, None, false, Vec::new()).await
}

/// 运行计划：agent 通过本地 HTTP 端口闭环驱动已打开的浏览器逐步执行。
/// 每次运行绑定一个独立 tab（runId），可并行多个；agent 发一条命令、看结果、再决策。
#[tauri::command]
pub async fn run_plan_with_agent(
    app: AppHandle,
    state: State<'_, AppState>,
    cwd: String,
    plan: Value,
    agent_kind: AgentKind,
    model: Option<String>,
) -> Result<String, String> {
    // 确保浏览器已打开并拿到执行端口；未打开则启动录制进程。
    let exec_port = crate::browser::ensure_exec_port(&app).await?;
    let run_id = uuid::Uuid::new_v4().to_string();
    let record_output_dir = state.config_dir.join("browser-records").join(&run_id);
    std::fs::create_dir_all(&record_output_dir).map_err(|e| format!("创建记录截图目录失败: {e}"))?;
    let plan_json = serde_json::to_string_pretty(&plan).map_err(|e| e.to_string())?;
    let record_dir = record_output_dir.to_string_lossy().replace('\\', "/");

    // 收集 plan 里所有步骤的参考图，读成 base64 作为消息图片随提示词一起发给 agent，
    // 这样多模态模型能直接“看到”参考图，不依赖 agent 本地读文件权限或 execReadImage。
    let mut ref_images: Vec<PromptImage> = Vec::new();
    if let Some(steps) = plan.get("steps").and_then(|s| s.as_array()) {
        for step in steps {
            if let Some(paths) = step.get("targetImagePaths").and_then(|p| p.as_array()) {
                for p in paths {
                    let Some(path) = p.as_str() else { continue };
                    let Ok(bytes) = std::fs::read(path) else { continue };
                    let mime = if path.to_ascii_lowercase().ends_with(".jpg") || path.to_ascii_lowercase().ends_with(".jpeg") {
                        "image/jpeg"
                    } else if path.to_ascii_lowercase().ends_with(".webp") {
                        "image/webp"
                    } else {
                        "image/png"
                    };
                    ref_images.push(PromptImage {
                        name: std::path::Path::new(path)
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_else(|| "ref.png".into()),
                        mime_type: mime.into(),
                        data: Some(base64::engine::general_purpose::STANDARD.encode(&bytes)),
                        uri: None,
                        size: None,
                    });
                }
            }
        }
    }

    let prompt = format!(
        "你要通过本地 HTTP 接口驱动一个已打开的浏览器，逐步完成以下 Plan。这是闭环：每发一条命令拿到结果后再决定下一步，不要预先写死整个流程。\n\
         Plan：\n```json\n{plan_json}\n```\n\
         本次运行的 runId = `{run_id}`，所有命令都必须带上它（多任务并行时用于区分各自独立的 tab，禁止混用其它 runId）。\n\
         执行接口：POST http://127.0.0.1:{exec_port}/ ，请求体是 JSON 命令，返回 `{{ ok, data }}` 或 `{{ ok:false, error }}`。用 curl / Invoke-RestMethod / fetch 均可，逐条发送。\n\
         可用命令（cmd 字段 + runId 字段）：\n\
         - execOpen：新建本次运行的专属 tab。必须先发一次。例 `{{\"cmd\":\"execOpen\",\"runId\":\"{run_id}\"}}`\n\
         - execGoto：跳转。`{{...,\"url\":\"https://...\"}}`；如需先写 sessionStorage 再加 `\"sessionStorage\":{{\"key\":\"k\",\"value\":\"v\"}}`。\n\
         - execEval：在页面执行 JS 并返回结果。`{{...,\"expression\":\"...\"}}`。expression 会在页面里 eval，返回可 JSON 序列化的值。\n\
         - execClick / execFill：按 selector 点击 / 填值。`{{...,\"selector\":\"...\"}}`（execFill 加 `\"value\":\"...\"`）。\n\
         - execViewport：返回当前视口宽高，据此知道截图坐标范围。\n\
         - execMouseClick / execMouseMove / execScroll / execType / execKey：纯视觉坐标操作。`{{...,\"x\":123,\"y\":456}}` 在视口坐标点击（execMouseClick 可加 `\"double\":true` 双击）；execScroll 用 `\"deltaY\":600` 滚动；execType 用 `\"text\":\"...\"` 逐键输入；execKey 用 `\"key\":\"Enter\"`。\n\
         - execQuery：探测某个 selector 匹配到的元素（最多 20 个），只返回 tag/前 60 字文本/是否可见。`{{...,\"selector\":\"...\"}}`。**仅作纯视觉失败时的后备**，平时别用。\n\
         - execShot：截图保存到文件。整页 `{{...,\"path\":\"<绝对路径.png>\"}}`；截某元素加 `\"selector\":\"...\"`；元素是内部滚动容器（表格/列表有滚动条）时再加 `\"full\":true`，会展开截出完整内容而不是只截可见一屏。\n\
         - execReadImage：读取本地参考图返回 dataURL。`{{...,\"path\":\"<绝对路径>\"}}`，用于查看 Plan 里的 targetImagePaths。\n\
         - execClose：关闭本次运行的 tab。全部完成后发。\n\
         操作方式（纯视觉优先，像人一样不看 DOM）：\n\
         - **默认用纯视觉操作**：先用 execShot 截当前页面，用你自己的看图工具看截图，认出目标元素在图里的像素位置，再用 execMouseClick(x,y) 在该坐标真实点击、execType 输入文字、execScroll 滚动把视野外目标滚进来再截再点。截图坐标与视口坐标 1:1（可先用 execViewport 拿宽高确认范围）。\n\
         - 所有操作都产生真实鼠标/键盘事件，页面自身的校验、联动、监听会正常触发。\n\
         - **禁止**用 execEval 改 DOM、设 value、dispatchEvent、调页面内部函数来绕过真实操作。execEval/execQuery/execClick/execFill 只在纯视觉反复失败时作后备，且 execEval 只读不改。\n\
         - 高效：不要拉整个 DOM/outerHTML 分析；靠看截图定位即可。\n\
         - **一次只执行一个鼠标动作**：每条命令只点一下/滚一次/输一段，发完拿到结果、重新截图确认后再发下一个。禁止一次连点多个位置或把多步操作攒在一起发，否则会因页面未及时响应而点错。\n\
         - 下拉/筛选类操作：先 execClick 展开，再用 execEval 列出候选项文本（像上面那样只取 value+文本），选中匹配项后 execClick 该项，最后确认。\n\
         联动筛选器（选了第一个才出现/更新第二个）：\n\
         - 不要指望一开始就在 DOM 里看到所有筛选项。按依赖顺序逐个处理：选完第一个筛选项后，用 execWait 等第二个筛选项出现/刷新（等它的 selector 可见，或等 loading 消失、选项数量变化），再用 execQuery/execEval 读取它的最新候选项并选择。\n\
         - 选中某一级后，后续级可能被重置或重载，每次都要重新读候选项，不要用选上一级之前拿到的旧选项。\n\
         - 某一级选完后页面可能异步刷新数据，进下一级或截图前先 execWait 等加载结束。\n\
         关于选项匹配（重要）：\n\
         - Plan 要求选某个值（如国家=美国）但页面没有完全相同选项时，**不要放弃也不要直接退出**。选最接近的合理项（如 North America / USA / United States），照常完成后续步骤，并在最终结论里明确说明\"页面无 X，已选 Y\"。只有完全没有任何相关可选项才算阻断。\n\
         - 同理，目标元素找不到时先换 selector/文字/role 再试，而不是立即报错停止。\n\
         文件路径：所有 path 用正斜杠 `/` 写绝对路径（如 `{record_dir}/xxx.png`），不要用反斜杠，避免转义或乱码导致保存失败。\n\
         步骤执行要点：\n\
         - goto 步骤：execGoto。\n\
         - operate 步骤：prompt 是自然语言操作。按上面拟人方式完成（execClick/execFill）。**每步操作后必须做视觉自检**（见下“视觉反馈”），确认生效再进下一步；不要只看接口返回 ok 就当成功。\n\
         参考图（必看）：\n\
         - 各步骤配置时附加的参考截图已作为**图片附件**随本提示一起发给你（targetImagePaths 对应的图），请先查看再执行对应步骤，不得忽略。顺序与 plan 中各步骤出现的顺序一致。\n\
         - operate：据参考图确认要操作的目标（哪个按钮/筛选项/填什么值），再用 execQuery 定位、execClick/execFill 拟人执行；图与页面不一致时以页面实际为准。\n\
         - record：参考图展示了要截的模块样子，截完用它对照自检（结构/表头/数据区是否一致）。\n\
         - record 步骤：截取一个完整页面模块截图。流程：等页面稳定（用 execEval 轮询直到无 loading/spinner 且关键内容连续几次一致）→ 定位到同时覆盖表头和数据区的最小容器 → execShot 带 selector 截图，path 存到 {record_dir}/ 下（文件名含 outputName 和分段序号）→ 保存后自检（只有表头/数据缺失/遮挡/加载中都算失败，最多修正 3 次）。表格/列表要看到表头和至少一行数据；目标内容一屏装不下时（无论滚动条在容器还是 body 上），用 execShot 加 `\"full\":true`，它会展开目标及其祖先让完整内容渲染出来再截；若仍截不全或是虚拟列表，再分段滚动截到末行，归同一 outputName。record 只产出截图，不采文本。\n\
         - 遮挡处理：操作或截图前若目标被侧栏/弹窗/悬浮窗遮挡，先找到并点击其关闭按钮；关不掉的调大窗口或滚动让目标露出；仍不行的仅对遮挡元素设 display:none 后截图，截完恢复。禁止改目标模块本身。\n\
         视觉反馈（强制，每个 operate 后必做）：\n\
         - 操作后用 execShot 截一张当前页面的图（整页即可，存到 {record_dir}/ 下，如 verify-步骤名.png）。\n\
         - 然后**用你自己内置的看图/读图工具打开这张截图**，亲眼确认页面状态：上一步是否生效、目标是否出现/选中、有没有报错或遮挡。\n\
         - 看清楚再决定下一步：生效就继续；没生效就换 selector/等待后重试（最多 2 次）。禁止不看图就盲目连发操作。\n\
         - 参考图（配置时的 targetImagePaths）已作为图片附件发给你，对照它和当前截图判断操作是否到位。\n\
         - 内容一屏看不全时（长列表/长表格/需滚动区域）：用 execScroll 分段滚动，每滚一段 execShot 截一张并看图，滚动+截图交替直到看全目标内容，再继续操作或拼接判断；不要只凭第一屏就下结论。\n\
         - 单步失败：先看 error，用最小化 execEval/execShot 检查页面状态，换定位方式或等待重试，最多 2 次；不得跳过步骤、打乱顺序、替换 goto 的 url。被登录/验证码/权限阻断则停止并说明。\n\
         全部步骤完成后，按 analysisPrompt 分析 analysisRecordRefs 指定的 record 截图（为空则用全部 record 结果），用 execShot 或直接读已保存的截图文件分析，不得用 DOM 文本代替。最后发 execClose。\n\
         最终只返回自然语言结论，不要 JSON/代码块/原始日志：是否完成、关键结果、必要的定位修正与选项替代说明、失败时的阻断步骤和原因。",
        plan_json = plan_json,
        run_id = run_id,
        exec_port = exec_port,
        record_dir = record_dir,
    );

    run_ephemeral_prompt(app, &state, cwd, prompt, PLAN_RUN_TIMEOUT_MS, agent_kind, model, true, ref_images).await
}
