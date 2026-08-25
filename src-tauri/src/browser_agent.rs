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
    // 双子座的无头开关属于浏览器进程启动配置；先校准模式，再获取执行端口。
    let mut headless = plan
        .get("headless")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    if crate::server::is_headless() {
        headless = true;
    }
    crate::browser::set_headless(app.clone(), headless).await?;
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
         - execEval：禁止使用。\n\
         - execClick / execFill / execQuery：禁止使用（依赖 selector/DOM）。\n\
         - execViewport：返回当前视口宽高，据此知道截图坐标范围。\n\
         - execMouseClick / execMouseMove / execScroll / execType / execKey：纯视觉坐标操作。`{{...,\"x\":123,\"y\":456}}` 在视口坐标点击（execMouseClick 可加 `\"double\":true` 双击）；execScroll 用 `\"deltaY\":600` 滚动；execType 用 `\"text\":\"...\"` 逐键输入；execKey 用 `\"key\":\"Enter\"`。\n\
         \n\
         - execShot：保存当前视口截图；record 优先用 `\"fullPage\":true` 一次保存浏览器完整页面，不依赖 selector/DOM。`{{...,\"path\":\"<绝对路径.png>\",\"fullPage\":true}}`。\n\
         - execReadImage：仅当图片没有作为附件且内置看图工具无法直接打开本地文件时使用；当前截图直接打开文件，禁止再转 dataURL。\n\
         - execClose：关闭本次运行的 tab。全部完成后发。\n\
         操作原则（只看画面、只用鼠标键盘）：\n\
         - **禁止使用 selector、DOM、JS、execEval、execQuery、execClick、execFill**。像真人一样，仅通过截图判断页面，用 execMouseClick/execMouseMove/execScroll/execType/execKey 操作。\n\
         - 默认按“最少观察点”执行：一张截图先规划当前稳定画面内所有确定操作；没有弹层、导航、刷新或布局变化时，可连续执行 2–4 个确定动作，再统一截图验收。HTTP 命令仍逐条取结果，但应在一次 shell 调用中顺序发送，减少模型往返。\n\
         - 页面布局变化后截图一次，根据文字、图标、颜色和相对位置识别目标并点击中心；截图坐标与视口坐标 1:1。布局变化后旧坐标作废，布局未变化时复用已确认坐标，不重复截图。\n\
         - 输入框作为一个微操作批次完成：点击中心 → Ctrl+A → execType → 必要时 Enter/Tab，最后只截图一次确认。\n\
         - 目标不在视口时分段滚动，每次滚动约视口高度的 70% 并重新截图；弹窗或遮挡出现时先按画面关闭。若目标在内部滚动区域，先把鼠标移到该区域中心再滚动。\n\
         - 下拉/筛选：根据当前截图点击展开 → 等待并截图一次 → 从新画面识别精确选项并点击。多个筛选控件同屏且选择后布局不变时，可依次设置，全部完成后统一截图；只有联动筛选才逐级截图。\n\
         联动筛选：严格逐级执行：\n\
         - 不要预判后续筛选项。每选一级，等待页面更新并重新截图，再从新画面定位下一级。\n\
         - 页面变化后旧坐标失效，必须重新截图定位。\n\
         - 确认画面稳定后再进入下一级。\n\
         关于选项匹配（重要）：\n\
         - Plan 要求选某个值（如国家=美国）但页面没有完全相同选项时，**不要放弃也不要直接退出**。选最接近的合理项（如 North America / USA / United States），照常完成后续步骤，并在最终结论里明确说明\"页面无 X，已选 Y\"。只有完全没有任何相关可选项才算阻断。\n\
         - 目标找不到时重新截图，检查是否需要滚动、关闭遮挡或等待加载；不要盲点。\n\
         文件路径：所有 path 用正斜杠 `/` 写绝对路径（如 `{record_dir}/xxx.png`），不要用反斜杠，避免转义或乱码导致保存失败。\n\
         步骤执行要点：\n\
         - goto 后等待页面稳定，只截取并读取一张 current.png；过程截图始终覆盖同一个 current.png，禁止同一画面连续 execShot，也禁止对刚保存的截图先 execReadImage 再用内置工具读取。\n\
         - operate 步骤：按“截图一次并规划 → 批量完成当前稳定画面的确定动作 → 布局变化时再截图 → 最终验收”执行；接口 ok 不代表页面操作成功。\n\
         参考图（必看）：\n\
         - 各步骤配置时附加的参考截图已作为**图片附件**随本提示一起发给你（targetImagePaths 对应的图），请先查看再执行对应步骤，不得忽略。顺序与 plan 中各步骤出现的顺序一致。\n\
         - operate：参考图只用于确认目标和预期状态；始终以当前页面截图为准，通过视觉坐标操作。\n\
         - record：参考图展示了要截的模块样子，截完用它对照自检（结构/表头/数据区是否一致）。\n\
         - record 步骤：等待页面视觉稳定后，**先调用一次 execShot(fullPage:true)** 保存完整页面到 {record_dir}/ 下，并打开结果检查目标的表头、数据区和末行/页尾是否都存在。普通 body 页面到此即完成，禁止再用滚轮逐屏截图。只有 fullPage 图仍缺失内容（内部滚动容器、虚拟列表或懒加载）时，才退回“截图 → 滚动 → 截图”循环：文件名包含 outputName 和递增分段序号，每次滚动约可见内容高度的 70% 并保留约 30% 重叠；内部滚动区域先将鼠标移到区域中心。持续到明确看到末行/页尾，或连续两次截图无新增内容；最多 30 段，达到上限仍未到底则报告截图不完整。record 只产出截图。\n\
         - 遮挡处理：按截图找到关闭按钮并点击；关不掉时滚动让目标露出。禁止用 JS 隐藏元素。\n\
         失败处理：\n\
         - 动作批次后只在布局变化或步骤结束时截图，确认是否生效、目标是否出现/选中，以及有无报错或遮挡。\n\
         - 成功立即继续，不重复操作。\n\
         - 未成功先确认动作是否其实已完成，避免重复提交。若未完成，处理加载或遮挡后获取新截图、重新计算坐标，只重试一次；禁止复用旧坐标。仍失败则停止并报告。\n\
         - 参考图（配置时的 targetImagePaths）已作为图片附件发给你，对照它和当前截图判断操作是否到位。\n\
         - record 的完成条件是完整图中已覆盖目标的表头、数据区和内容末尾。fullPage 图满足条件就立即完成；只有它无法覆盖内部滚动/虚拟内容时才分段滚动。分段时不能因单次滚动成功或接口 ok 提前结束；看到末尾或连续两次无新增内容才停止。record 结果本身就是验收，不另存同画面的 verify/final 图。\n\
         - 登录、验证码、权限或一次重定位重试后仍失败时停止；不得跳步、乱序或更改 goto URL。\n\
         全部步骤完成后，按 analysisPrompt 分析 analysisRecordRefs 指定的 record 截图（为空则用全部 record 结果），用 execShot 或直接读已保存的截图文件分析，不得用 DOM 文本代替。最后发 execClose。\n\
         最终只返回自然语言结论，不要 JSON/代码块/原始日志：是否完成、关键结果、必要的定位修正与选项替代说明、失败时的阻断步骤和原因。",
        plan_json = plan_json,
        run_id = run_id,
        exec_port = exec_port,
        record_dir = record_dir,
    );

    run_ephemeral_prompt(app, &state, cwd, prompt, PLAN_RUN_TIMEOUT_MS, agent_kind, model, true, ref_images).await
}
