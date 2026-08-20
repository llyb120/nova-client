//! 截图分析通过后台临时会话完成；计划运行会话持久保留在双子座独立历史。
//! agent/skill 自己调用 Playwright（MCP 或 CLI），Nova 不内置 Playwright 运行器。
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager, State};

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

    dispatch_prompt(&app, thread_id.clone(), prompt, Vec::new())?;

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

    run_ephemeral_prompt(app, &state, cwd, prompt, ANALYSIS_TIMEOUT_MS, AgentKind::Lyra, None, false).await
}

/// 运行计划：把 Playwright 计划落盘，交给后台临时会话，由 agent/skill
/// 自己调用 Playwright（CLI / MCP / 脚本）执行并验证，返回执行结论文本。
#[tauri::command]
pub async fn run_plan_with_agent(
    app: AppHandle,
    state: State<'_, AppState>,
    cwd: String,
    plan: Value,
    agent_kind: AgentKind,
    model: Option<String>,
) -> Result<String, String> {
    // Plan 原样落盘：只包含当前片段编辑器中的步骤和运行配置，不补 startUrl、不改顺序、不预执行。
    let dir = state.config_dir.join("browser-plans");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let run_id = uuid::Uuid::new_v4();
    let path = dir.join(format!("plan-{run_id}.json"));
    let record_output_dir = state.config_dir.join("browser-records").join(run_id.to_string());
    std::fs::create_dir_all(&record_output_dir).map_err(|e| format!("创建记录截图目录失败: {e}"))?;
    let body = serde_json::to_string_pretty(&plan).map_err(|e| e.to_string())?;
    std::fs::write(&path, &body).map_err(|e| e.to_string())?;

    let config_dir = state.config_dir.clone();
    let runtime = tauri::async_runtime::spawn_blocking(move || {
        crate::browser::ensure_playwright_runtime(&config_dir)
    })
    .await
    .map_err(|e| e.to_string())??;
    let runtime_node_modules = runtime.join("node_modules");
    let storage_state = runtime.join("storage-state.json");
    let headless = plan.get("headless").and_then(Value::as_bool).unwrap_or(true);
    let prompt = format!(
        "请严格按顺序执行这个 Plan 文件中的全部步骤：{path}\n\
         Plan 是唯一事实来源：不得添加、删除、折叠、重排或跳过任何步骤；Plan 可以不包含 goto，直接从当前挂载页面执行其余步骤。\n\
         执行环境：\n\
         - 使用 Nova 共享 playwright-core：{node_modules}，禁止安装、升级或下载任何依赖或浏览器；\n\
         - 每次新建 browser/context/page，不复用历史标签页或历史 URL；\n\
         - 可加载登录态文件 {storage_state} 保留 Cookie/localStorage；Plan 中遇到 goto 时按所在顺序执行，没有 goto 时不补充导航。\n\
         - Plan 顶层 headless={headless}；false 时必须使用系统 Chrome/Edge 显示真实窗口，true 时使用无头模式。\n\
         定位规则：\n\
         - 普通操作步骤优先使用 selector；若 selector 为空、失效或不唯一，必须参考 targetImagePaths 中的截图识别目标元素后再执行；\n\
         - targetImagePaths 只用于视觉定位，不是待分析数据；不得因为图片存在而跳过对应操作；\n\
         - 所有 record 步骤都只能产出截图，不采集或返回文本、HTML、JSON 等其它形式的数据。record 步骤没有名称或引用语义；忽略历史 Plan 中可能残留的 outputName。使用 recordContent 的文字说明和 targetImagePaths 的视觉参考定位当前网页上的目标块：必须同时观察截图视觉特征与当前 DOM（文字、结构、role、尺寸和相邻内容）判断真实位置，但 DOM 只用于辅助定位，不能作为记录结果；定位后使用目标元素 locator.screenshot 截取该块并保存到 {record_output_dir}，不要截整页代替。每个 record 步骤至少产出一张按步骤序号命名的 PNG；若目标跨多个必要元素，可保存多张并在最终结论中列出路径；\n\
         - setSessionStorage 步骤必须在当前页面执行 page.evaluate，将 step.key/step.value 原样写入 window.sessionStorage；禁止调用 alert、confirm、prompt 或注入任何可见弹窗；它作用于当前页面 origin，不得改写为 localStorage、Cookie 或启动参数；写入后应在页面上下文读取同一 key 静默校验，若当前页没有可用的 http(s) origin，应停止并说明需要先执行 goto；\n\
         - 选择器失效时允许根据当前页面、文字参考和截图推断等价 selector，但必须执行原步骤 action。\n\
         自适应执行规则（像人操作，不要先生成一份固定脚本后一次性运行）：\n\
         - 逐步读取 Plan：每次只处理当前步骤，先观察当前 URL、页面结构和可见状态，再决定具体 Playwright 调用；当前步骤成功后才进入下一步；\n\
         - selector 只是首选提示而不是唯一答案。找不到、不可见、不唯一或页面结构变化时，可结合元素文字、role、label、placeholder、附近内容以及 targetImagePaths 重新定位；\n\
         - 允许等待页面加载、动画结束、异步数据出现，处理遮罩、Cookie 提示、普通弹窗、新标签页和必要的滚动；\n\
         - 单步失败时先截图和检查页面状态，最多进行 2 次有依据的修正重试；不得机械重复同一个失败调用；\n\
         - 可以修正定位方式和等待策略，但不得改变步骤业务意图、跳过步骤或打乱顺序；goto 的目标 URL 不允许替换；\n\
         - 如果被登录、验证码、权限或不可恢复错误阻断，应停止后续步骤并在结论中明确说明阻断位置和原因。\n\
         执行与验证：\n\
         - goto 步骤出现时必须访问 step.url；若该 goto 带 sessionStorage，则必须在首次请求发出前完成注入，禁止先 page.goto 再写入。推荐先为目标 origin 打开一个不会触发业务鉴权的空白响应，写入 sessionStorage 后再 page.goto(step.url)，或使用 context.addInitScript 按目标 origin 在文档最早期写入；必须确保目标站点首个鉴权脚本和请求读取到该值。不得用当前地址或历史页面代替；Plan 没有 goto 时直接执行其它步骤，不得自行补充 goto；\n\
         - 每步失败时保留错误和当前页面截图，经过允许的修正重试后仍失败才停止；\n\
         - 所有步骤完成后再按照 analysisPrompt 分析 record 步骤实际保存的页面块截图；不得直接使用 DOM 文本、HTML、参考文字或参考图片作为分析数据。\n\
         最终只返回自然语言结论，不要 JSON、代码块或逐步原始日志。结论应简洁说明：是否完成、关键结果、发生过的必要定位修正，以及失败时的阻断步骤和原因。",
        path = path.to_string_lossy(),
        node_modules = runtime_node_modules.to_string_lossy(),
        storage_state = storage_state.to_string_lossy(),
        record_output_dir = record_output_dir.to_string_lossy(),
        headless = headless,
    );

    run_ephemeral_prompt(app, &state, cwd, prompt, PLAN_RUN_TIMEOUT_MS, agent_kind, model, true).await
}
