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
    let config_dir = state.config_dir.clone();
    let runtime = tauri::async_runtime::spawn_blocking(move || {
        crate::browser::ensure_playwright_runtime(&config_dir)
    })
    .await
    .map_err(|e| e.to_string())??;
    let runtime_node_modules = runtime.join("node_modules");
    let storage_state = runtime.join("storage-state.json");
    let headless = plan.get("headless").and_then(Value::as_bool).unwrap_or(true);
    let run_id = uuid::Uuid::new_v4();
    let record_output_dir = state.config_dir.join("browser-records").join(run_id.to_string());
    std::fs::create_dir_all(&record_output_dir).map_err(|e| format!("创建记录截图目录失败: {e}"))?;
    // 不再落盘 Plan 文件：把 Plan 直接拼进提示词，agent 不读文件。
    let plan_json = serde_json::to_string_pretty(&plan).map_err(|e| e.to_string())?;
    let prompt = format!(
        "请严格按顺序执行以下 Plan 中的全部步骤：\n\
         ```json\n{plan_json}\n```\n\
         Plan 是唯一事实来源：不得添加、删除、折叠、重排或跳过任何步骤；Plan 可以不包含 goto，直接从当前挂载页面执行其余步骤。\n\
         执行环境：\n\
         - 使用 Nova 共享 playwright-core：{node_modules}，禁止安装、升级或下载任何依赖或浏览器；\n\
         - 每次新建 browser/context/page，不复用历史标签页或历史 URL；\n\
         - 可加载登录态文件 {storage_state} 保留 Cookie/localStorage；Plan 中遇到 goto 时按所在顺序执行，没有 goto 时不补充导航。goto 携带 sessionStorage 时，必须通过 context.addInitScript 在目标文档创建时按目标 origin 注入，再发起首次 page.goto，确保鉴权脚本和请求读取前已经存在。\n\
         - Plan 顶层 headless={headless}；false 时必须使用系统 Chrome/Edge 显示真实窗口，true 时使用无头模式。\n\
         定位规则：\n\
         - 步骤只有三种：goto（按 url 跳转）、record（截取页面模块）、operate（操作）；operate 步骤的 prompt 就是自然语言描述的操作内容，看懂后在当前页面自行定位元素并完成操作，可结合 targetImagePaths 参考图辅助定位；\n\
         - record 步骤必须根据该步骤上已有的信息自行定位，包括 selector、targetImagePaths 参考图和 recordContent 补充要求；不得让用户在运行时补充信息。selector 存在时先用它快速验证候选模块；selector 为空或失效时，根据参考图和提示词在 DOM 中生成并验证唯一、稳定的 locator。确定元素后必须调用 locator.screenshot，而不是按参考图坐标裁切页面；locator 的唯一性不能只凭首个匹配判断，必须先在整页统计匹配数量，若大于 1 则结合参考图和 recordContent 再筛选，禁止直接取 first()；\n\
         - record 步骤必须遵循“等待页面就绪、识别步骤信息、匹配当前页面、生成并验证 locator、截图 DOM、检查结果”的顺序。所有 record 结果截图必须保存到目录 {record_output_dir}（不存在时先创建），禁止保存到其它位置。第一步不是立即定位：先等待 domcontentloaded，再对 networkidle 做有限等待（超时不能直接视为就绪）；持续检查页面中的 loading/spinner/skeleton/progress、全屏或模块级 mask/overlay、空数据占位和禁用态，等待它们消失。随后对候选模块每隔 800ms 读取一次关键特征（可见行数、文本长度、容器尺寸、表头及首末行），至少连续 3 次一致且存在实质数据后才算稳定；仅有表头、骨架、空白或“加载中”时禁止截图。若有 Cookie 提示、引导层等可关闭遮挡，应先正常关闭；不可关闭的遮挡必须等待或报告阻断，不得透过遮挡强行截图；\n\
         - 页面稳定后，实际读取每张 targetImagePaths 图片，提取模块类型、标题/表头、布局、边界、相邻元素、行列数量和大致尺寸等视觉特征；再枚举当前页面全部候选 DOM 区块（不要只查第一个），将候选的文字、结构、位置、尺寸及必要的候选截图与参考图逐项比对，只有确认是同类模块后才能确定 locator。禁止把参考图本身复制为结果，禁止看到相似文字后直接截取最小文字节点，也禁止未读取参考图就猜 selector；\n\
         - 所有 record 步骤都只能产出截图，不采集或返回文本、HTML、JSON 等其它形式的数据。outputName 是记录结果名称，必须保留并用于 analysisRecordRefs 的结果引用；recordContent 是该条记录的补充要求，可能同时包含定位线索、截图范围和其它约束，必须完整遵守；targetImagePaths 是视觉参考。确定模块后，应从命中的表头或子元素逐级向上检查父级，选择同时覆盖表头和真实数据区的最小合理容器；必须用数据行数量、容器 boundingBox 高度和截图预览验证，不能仅凭 class 名或 locator 首次命中。要求“完整表格数据”时，至少要看到表头和一行真实数据；若表格使用固定表头、内部滚动或虚拟列表，单次 locator.screenshot 只包含表头或当前视口就不算完成，必须滚动表格数据区并按顺序保存多张无遗漏、可衔接的截图，直到末行，所有截图归入同一 outputName；禁止截单元格、标题、局部行或整页代替；禁止把只含表头、没有数据行的截图当作完成，也不得在 record 残缺时仍进入后续分析步骤；\n\
         - 每张结果截图保存后必须立即重新读取刚保存的图片文件进行自检：确认不存在 spinner/skeleton/mask，并逐项对照 recordContent 核对截图包含的表头、数据行数量和内容范围。若只看到表头、数据行缺失、遮挡、裁剪或仍在加载，应删除/覆盖该无效结果，重新等待并改选容器或采用分段滚动截图，最多修正 3 次；不得把已知不完整的截图当作成功继续执行。截图文件名应包含步骤序号、安全化后的 outputName 和分段序号；\n\
         自适应执行规则（像人操作，不要先生成一份固定脚本后一次性运行）：\n\
         - 逐步处理 Plan：每次只处理当前步骤，先观察当前 URL、页面结构和可见状态，再决定具体 Playwright 调用；当前步骤成功后才进入下一步；\n\
         - 允许等待页面加载、动画结束、异步数据出现，处理遮罩、Cookie 提示、普通弹窗、新标签页和必要的滚动；\n\
         - 单步失败时先截图和检查页面状态，最多进行 2 次有依据的修正重试；不得机械重复同一个失败调用；\n\
         - 可以修正定位方式和等待策略，但不得改变步骤业务意图、跳过步骤或打乱顺序；goto 的目标 URL 不允许替换；\n\
         - 如果被登录、验证码、权限或不可恢复错误阻断，应停止后续步骤并在结论中明确说明阻断位置和原因。\n\
         执行与验证：\n\
         - goto 步骤出现时必须访问 step.url；若该 goto 带 sessionStorage，必须先注册 context.addInitScript，并在脚本中仅当 location.origin 等于 step.url 的 origin 时写入对应 key/value，然后再首次 page.goto(step.url)。禁止先访问目标站点、临时页面或业务地址再写入，必须确保目标站点首个鉴权脚本和请求读取到该值；不得用当前地址或历史页面代替；Plan 没有 goto 时直接执行其它步骤，不得自行补充 goto；\n\
         - 每步失败时保留错误和当前页面截图，经过允许的修正重试后仍失败才停止；\n\
         - 所有步骤完成后再按照 analysisPrompt 分析 analysisRecordRefs 指定名称对应的 record 页面块截图；analysisRecordRefs 为空时可使用全部 record 结果。不得直接使用 DOM 文本、HTML、参考文字或参考图片作为分析数据。\n\
         最终只返回自然语言结论，不要 JSON、代码块或逐步原始日志。结论应简洁说明：是否完成、关键结果、发生过的必要定位修正，以及失败时的阻断步骤和原因。",
        plan_json = plan_json,
        node_modules = runtime_node_modules.to_string_lossy(),
        storage_state = storage_state.to_string_lossy(),
        record_output_dir = record_output_dir.to_string_lossy(),
        headless = headless,
    );

    run_ephemeral_prompt(app, &state, cwd, prompt, PLAN_RUN_TIMEOUT_MS, agent_kind, model, true).await
}
