//! JEV × DOM decision tree. Every observation walks the same tree and the first matching node acts:
//! guard → wait → reflex → path → jev → fallback. Local nodes act without a model round-trip; JEV
//! plans the current step plus a short same-screen path only at information boundaries, and each
//! continuation step is re-bound and validated against fresh DOM. The main model is the fallback.
use serde::Deserialize;
use serde_json::{json, Value};
use std::{collections::{BTreeMap, HashMap, HashSet, VecDeque}, path::Path, time::{Duration, Instant}};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Plan {
    task: String,
    authorization: String,
    expected_text: String,
    #[serde(default)]
    inputs: Vec<Input>,
    #[serde(default)]
    steps: Vec<Step>,
    #[serde(default)]
    control_names: Vec<String>,
    #[serde(default)]
    use_experience: bool,
    #[serde(default = "default_max_actions")]
    max_actions: usize,
    /// Concrete values the main model wrote into the goal (e.g. "United States"); local only.
    #[serde(skip)]
    phrases: Vec<String>,
}
fn default_max_actions() -> usize { 32 }
/// One step of the main model's plan. The main model decides *what* to do; the runner finds the
/// control (locally, JEV only for ties), executes consecutive steps without round-trips, and
/// checks `expect` before moving on.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Step {
    action: String,
    /// Natural description of the control, e.g. "Top Charts 菜单下的 PC & Console Games".
    #[serde(default)]
    target: String,
    /// Optional exact visible name / role / container text (region, field, column or group).
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    within: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    key: Option<String>,
    #[serde(default)]
    direction: Option<String>,
    /// Observable result that must hold before the next step (literal text first, then JEV yes/no).
    #[serde(default)]
    expect: Option<String>,
    #[serde(default)]
    expected_text: Option<String>,
    /// Extra attempts of this step while `expect` is not yet met (e.g. sort toggles asc → desc).
    #[serde(default)]
    repeat: usize,
    /// Skip instead of handing off when the target is absent (e.g. a cookie banner).
    #[serde(default)]
    optional: bool,
}

impl Step {
    fn expect(&self) -> Option<&str> { self.expect.as_deref().or(self.expected_text.as_deref()).filter(|s| !s.trim().is_empty()) }
    fn describe(&self) -> String {
        let mut text = format!("{} {}", self.action, if self.target.is_empty() { self.name.as_deref().unwrap_or("页面") } else { &self.target });
        if let Some(name) = self.name.as_deref().filter(|_| !self.target.is_empty()) { text += &format!(" (名称={name})"); }
        if let Some(within) = &self.within { text += &format!(" 位于「{within}」"); }
        if let Some(value) = &self.text { text += &format!(" 输入「{}」", short(value, 60)); }
        if let Some(key) = &self.key { text += &format!(" 按键 {key}"); }
        if let Some(expect) = self.expect() { text += &format!(" → 期望：{expect}"); }
        text
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Input {
    #[serde(default)]
    name: String,
    // Main models naturally copy the observed fieldContext; accept it instead of rejecting the run.
    #[serde(default)]
    field_context: Option<String>,
    #[serde(default)]
    role: Option<String>,
    text: String,
}

// ponytail: fixed budgets. 180s per run, 4s local load wait, 15s no-progress wait, 90 entries per JEV batch
// (with groups collapsed most screens fit entirely; ranking only trims crowded pages).
const RUN_BUDGET: Duration = Duration::from_secs(180);
const LOAD_WAIT: Duration = Duration::from_secs(4);
const POLL: Duration = Duration::from_millis(250);
const SETTLE: Duration = Duration::from_millis(200);
const SHORTLIST: usize = 90;
const GROUP_MIN: usize = 8;
const PATH_DEPTH: usize = 4;
const STATE_REPEATS: usize = 3;

fn parse(args: &Value) -> Result<Plan, String> {
    let plan: Plan = serde_json::from_value(args["plan"].clone())
        .map_err(|e| format!("plan 格式错误（{e}）；需要 task、authorization、expectedText，推荐 steps"))?;
    let valid = |s: &str, max: usize| !s.trim().is_empty() && s.chars().count() <= max;
    if !valid(&plan.task, 2000) || !valid(&plan.authorization, 1000) || !valid(&plan.expected_text, 500)
        || plan.inputs.len() > 8 || plan.inputs.iter().any(|i| i.name.chars().count() > 300
            || (i.name.trim().is_empty() && i.role.is_none() && i.field_context.is_none())
            || i.field_context.as_ref().is_some_and(|f| !valid(f, 600))
            || i.role.as_ref().is_some_and(|r| !valid(r, 80)) || i.text.chars().count() > 4000) {
        return Err("JEV 目标/授权/完成证据无效，inputs 最多8个非敏感字段".into());
    }
    let optional = |v: &Option<String>, max: usize| v.as_ref().is_none_or(|s| valid(s, max));
    if let Some((index, _)) = plan.steps.iter().enumerate().find(|(_, s)|
        !matches!(s.action.as_str(), "click" | "fill" | "press" | "scroll")
        || (matches!(s.action.as_str(), "click" | "fill") && !valid(&s.target, 300) && !optional(&s.name, 300))
        || (matches!(s.action.as_str(), "click" | "fill") && s.target.trim().is_empty() && s.name.is_none())
        || s.target.chars().count() > 300 || !optional(&s.name, 300) || !optional(&s.role, 80) || !optional(&s.within, 300)
        || !optional(&s.expect, 500) || !optional(&s.expected_text, 500) || s.repeat > 3
        || (s.action == "fill") != s.text.is_some() || s.text.as_ref().is_some_and(|t| t.chars().count() > 4000)
        || (s.action == "press") != s.key.is_some()
        || s.key.as_deref().is_some_and(|k| !matches!(k, "Enter" | "Tab" | "Escape" | "ArrowDown" | "ArrowUp" | "Space" | "Backspace"))
        || s.direction.as_deref().is_some_and(|d| s.action != "scroll" || !matches!(d, "up" | "down" | "left" | "right"))) {
        return Err(format!("steps[{index}] 无效：action 为 click/fill/press/scroll；click/fill 需 target（或 name）；fill 需 text；press 需 key（Enter/Tab/Escape/ArrowDown/ArrowUp/Space/Backspace）；scroll 可选 direction；repeat≤3"));
    }
    if plan.steps.len() > 24 { return Err("steps 最多24步；更长的流程分段 run".into()); }
    if plan.control_names.len() > 16 || plan.control_names.iter().any(|n| !valid(n, 300)) {
        return Err("controlNames 最多16个非空控件名称片段".into());
    }
    if !(1..=64).contains(&plan.max_actions) { return Err("maxActions 必须为1–64".into()); }
    let mut fields = HashSet::new();
    if plan.inputs.iter().any(|i| !fields.insert((&i.name, &i.role, &i.field_context))) { return Err("inputs 字段重复".into()); }
    let mut plan = plan;
    plan.phrases = goal_phrases(&format!("{}\n{}\n{}", plan.task, plan.authorization, plan.expected_text));
    Ok(plan)
}

/// Concrete values a person would type into a search box: quoted text and Title-Case Latin
/// phrases ("United States", "M Science"). Only the main model's own words are ever typed.
fn goal_phrases(text: &str) -> Vec<String> {
    static QUOTED: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static TITLED: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let quoted = QUOTED.get_or_init(|| regex::Regex::new(r#"[“"「『‘]([^“”"「」『』‘’\n]{2,40})[”"」』’]"#).unwrap());
    // ASCII boundary on purpose: CJK characters are Unicode word characters, so `\b` would miss
    // "改为United States".
    let titled = TITLED.get_or_init(|| regex::Regex::new(r"(?:^|[^A-Za-z0-9.'\-])([A-Z][A-Za-z0-9.'\-]*(?:\s+(?:&\s+)?[A-Z0-9][A-Za-z0-9.'\-]*)*)").unwrap());
    let mut out: Vec<String> = Vec::new();
    for phrase in quoted.captures_iter(text).map(|c| c[1].trim().to_string())
        .chain(titled.captures_iter(text).map(|c| c[1].trim().to_string())) {
        if phrase.chars().count() >= 2 && phrase.chars().count() <= 40
            && !out.iter().any(|p| p.eq_ignore_ascii_case(&phrase)) { out.push(phrase); }
        // ponytail: 24 phrases per plan; longer goals should split into subgoals.
        if out.len() >= 24 { break; }
    }
    out
}

fn search_like(item: &Value) -> bool {
    matches!(item["role"].as_str(), Some("searchbox" | "combobox"))
        || [&item["name"], &item["fieldContext"]].iter().any(|v| v.as_str().is_some_and(|s| {
            let s = s.chars().take(80).collect::<String>().to_lowercase();
            ["search", "filter", "find", "query", "keyword", "搜索", "查找", "筛选", "过滤", "检索"].iter().any(|k| s.contains(k))
        }))
}

/// Goal phrases worth typing into this search box: stated in a clause that also mentions the
/// field's context (e.g. "Region … United States"), and not already visible on the page.
fn search_phrases(plan: &Plan, item: &Value, visible: &str) -> Vec<String> {
    let context = format!("{} {}", item["name"].as_str().unwrap_or_default(),
        item["fieldContext"].as_str().unwrap_or_default().chars().take(120).collect::<String>());
    let context_lower = context.to_lowercase();
    let generic = terms("search select all filter find query keyword 搜索 查找 筛选");
    let anchors: HashSet<String> = terms(&context).difference(&generic).cloned().collect();
    let goal = format!("{}\n{}", plan.task, plan.authorization);
    let clauses: Vec<String> = goal.split(|c: char| "。；;\n，,、()（）".contains(c)).map(str::to_lowercase).collect();
    let mut scored: Vec<(usize, &String)> = plan.phrases.iter().filter_map(|phrase| {
        let lower = phrase.to_lowercase();
        if visible.contains(&lower) || context_lower.contains(&lower) || item["value"].as_str().is_some_and(|v| v.eq_ignore_ascii_case(phrase)) { return None; }
        let score = clauses.iter().filter(|c| c.contains(&lower) && terms(c).intersection(&anchors).next().is_some()).count();
        (score > 0).then_some((score, phrase))
    }).collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0));
    scored.into_iter().take(3).map(|(_, p)| p.clone()).collect()
}

fn input_matches(item: &Value, input: &Input) -> bool {
    let context = item["fieldContext"].as_str().unwrap_or_default();
    // ponytail: a copied fieldContext may be cut by the observation summary; ≥24 chars prefix still binds.
    input.role.as_ref().is_none_or(|role| item["role"] == *role)
        && input.field_context.as_ref().is_none_or(|f| context == f || (f.chars().count() >= 24 && context.starts_with(f.as_str())))
        && (item["name"] == input.name
            || (!input.name.is_empty() && context == input.name)
            || (input.name.is_empty() && input.field_context.is_some()))
}

/// Date pickers normalise typed values and open calendars; JEV decides those (presets are often
/// better). Plain text fields are filled locally.
fn date_like(candidate: &Candidate) -> bool {
    let key: Value = serde_json::from_str(&candidate.key).unwrap_or(Value::Null);
    let text = candidate.action["text"].as_str().unwrap_or_default();
    let name = key["name"].as_str().unwrap_or_default().to_lowercase();
    key["fieldContext"].as_str().is_some_and(|f| f.ends_with("/2]")) || name.contains("date") || name.contains("日期")
        || (text.len() >= 8 && text.chars().all(|c| c.is_ascii_digit() || "-/.: ".contains(c)))
}

fn delegated(item: &Value, plan: &Plan) -> bool {
    plan.inputs.iter().any(|i| input_matches(item, i))
        || plan.steps.iter().any(|s| s.action == "fill" && item["value"].as_str().is_some_and(|v| Some(v) == s.text.as_deref()))
}

fn short(text: &str, max: usize) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() <= max { text } else { format!("{}…", text.chars().take(max).collect::<String>()) }
}

fn evidence(pages: &Value, plan: &Plan) -> String {
    // ponytail: bounded text/DOM state, no visual interpretation; larger tasks need a narrower subgoal.
    let text = pages["pages"].as_array().into_iter().flatten()
        .filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join("\n");
    let mut states = Vec::new();
    let mut truncated = text.chars().count() > 12000;
    for page in pages["pages"].as_array().into_iter().flatten() {
        for item in page["items"].as_array().into_iter().flatten().filter(|i| i["inView"] == true) {
            // Only disclose values of fields explicitly delegated by the main model, or values that
            // are the main model's own goal wording (typed into a search box).
            let disclosed = delegated(item, plan)
                || item["value"].as_str().is_some_and(|v| plan.phrases.iter().any(|p| v.eq_ignore_ascii_case(p)));
            states.push(json!({"frame":page["frame"],"name":item["name"],"role":item["role"],
                "region":item["region"],"fieldContext":item["fieldContext"],"value":if disclosed { item["value"].clone() } else { Value::Null },"selected":item["selected"],
                "expanded":item["expanded"],"sort":item["sort"],"columnKey":item["columnKey"],"disabled":item["disabled"],
                "blockedBy":item["blockedBy"],"editable":item["editable"],"nodeId":item["nodeId"]}));
            if json!(states).to_string().chars().count() > 6000 { states.pop(); truncated = true; break; }
        }
    }
    let mut tables = Vec::new();
    for table in pages["pages"].as_array().into_iter().flatten()
        .flat_map(|p| p["tables"].as_array().into_iter().flatten()) {
        let mut table = table.clone();
        // References change on every observation; semantic completion evidence must not.
        if let Some(columns) = table["columns"].as_array_mut() {
            for column in columns { if let Some(object) = column.as_object_mut() { object.remove("ref"); } }
        }
        tables.push(table);
        if json!(tables).to_string().chars().count() > 6000 {
            tables.pop(); truncated = true; break;
        }
    }
    let locations: Vec<_> = pages["pages"].as_array().into_iter().flatten()
        .map(|p| json!({"url":p["url"],"readyState":p["readyState"],"loading":p["loading"],
            // Selection is completion evidence even when its click was suppressed or DOM text was truncated.
            // ponytail: 64 visible selection controls; larger lists explicitly report incomplete evidence.
            "selectionFields":p["items"].as_array().into_iter().flatten().filter(|i|i["inView"]==true && !i["selected"].is_null())
                .take(64).map(|i|json!({"name":i["name"],"role":i["role"],"region":i["region"].as_str().map(|s|s.chars().take(120).collect::<String>()),"selected":i["selected"]})).collect::<Vec<_>>(),
            "selectionFieldsTruncated":p["items"].as_array().into_iter().flatten().filter(|i|i["inView"]==true && !i["selected"].is_null()).count()>64,
            "dateFields":p["items"].as_array().into_iter().flatten().filter(|i|i["inView"]==true && i["dateValue"].is_string())
                .map(|i|json!({"field":i["fieldContext"],"value":i["dateValue"]})).collect::<Vec<_>>(),
            "tables":p["tables"].as_array().into_iter().flatten().map(|t|
                json!({"index":t["index"],"headers":t["headers"],"loadedRows":t["loadedRows"],"totalRows":t["totalRows"]})).collect::<Vec<_>>()})).collect();
    format!("当前页面及加载/结果状态：{}\n{}\nDOM 状态：{}\n表格预览（独立表头表的loadedRows=0不代表同页数据表为空；totalRows=null为未知；不足Top N必须补读）：{}\n观察摘要截断：{}", json!(locations), text.chars().take(12000).collect::<String>(), json!(states), json!(tables), truncated)
}

// Transition identity must cover all controls, including those beyond the prompt's text budget.
// Tooltip/body copy and observation refs are not progress.
fn replay_evidence(pages: &Value) -> String {
    let state: Vec<_> = pages["pages"].as_array().into_iter().flatten().map(|p| {
        let items: Vec<_> = p["items"].as_array().into_iter().flatten().map(|i|
            json!([i["nodeId"],i["name"],i["inView"],i["disabled"],i["editable"],i["value"],
                i["selected"],i["expanded"],i["sort"],i["scroll"]])).collect();
        let tables: Vec<_> = p["tables"].as_array().into_iter().flatten().map(|t|
            json!([t["headers"],t["rows"],t["totalRows"]])).collect();
        json!([p["frame"],p["url"],p["viewport"],items,tables])
    }).collect();
    json!(state).to_string()
}

fn transition_evidence(pages: &Value, plan: &Plan) -> String {
    let controls: Vec<_> = pages["pages"].as_array().into_iter().flatten().map(|p| {
        let items: Vec<_> = p["items"].as_array().into_iter().flatten().map(|i|
            json!([i["nodeId"],i["name"],i["inView"],i["actionable"],i["disabled"],i["blockedBy"],
                i["editable"],i["haspopup"],i["selected"],i["expanded"],i["sort"],i["scroll"]])).collect();
        json!([p["frame"],p["loading"],p["viewport"],items])
    }).collect();
    format!("{}\n{}", evidence(pages, plan), json!(controls))
}

fn undelegated_inputs(pages: &Value, plan: &Plan) -> Vec<Value> {
    pages["pages"].as_array().into_iter().flatten()
        .flat_map(|p| p["items"].as_array().into_iter().flatten())
        .filter(|i| i["inView"] == true && i["editable"] == true && i["disabled"] != true && !delegated(i, plan))
        .take(8).map(|i| json!({"name":i["name"],"role":i["role"],"fieldContext":i["fieldContext"]})).collect()
}

fn origin(pages: &Value) -> Result<String, String> {
    let url = reqwest::Url::parse(pages["pages"][0]["url"].as_str().ok_or("页面缺少 URL")?).map_err(|_| "页面 URL 无效")?;
    if !matches!(url.scheme(), "https" | "http") { return Err("只支持 HTTP(S) 页面".into()); }
    Ok(url.origin().ascii_serialization())
}

// Only this module can open the trusted scope; tool JSON cannot opt out of JEV.
tokio::task_local! { static BROWSER_ACTION: bool; }

pub(crate) fn executing_browser_action() -> bool {
    BROWSER_ACTION.try_with(|active| *active).unwrap_or(false)
}

async fn execute_browser(root: &Path, args: &Value, owner: &str, tool: &str) -> Result<Value, String> {
    BROWSER_ACTION.scope(args["operation"] == "act", async { match tool {
        "webview" => Box::pin(crate::native_browser::execute(root, args)).await,
        "chrome" => Box::pin(crate::native_browser::execute_chrome(root, args, owner)).await,
        _ => Err("不支持的 JEV 浏览器".into()),
    }}).await
}

fn graph_context(search: &Value) -> Value {
    // ponytail: at most three complete graph routes / 12k chars; larger graphs need a narrower subgoal.
    // Never cut serialized JSON: that used to drop route checks or the graph altogether.
    let mut routes = Vec::new();
    for route in search["graph"]["routes"].as_array().into_iter().flatten().take(3) {
        routes.push(route.clone());
        if json!(routes).to_string().chars().count() > 12000 { routes.pop(); break; }
    }
    json!({"routes":routes,"truncated":search["graphTruncated"] == true
        || search["graph"]["routes"].as_array().is_some_and(|all| all.len() > routes.len()),
        "notice":"每条路径保留独立的条件、步骤及检查点；只在当前观察满足条件时参考，不拼接不同路径。"})
}

/// One executable DOM hint. `action` (with the live ref) never leaves this process; the model
/// only sees an ID plus `key`/`label`. `key` is node + state identity (replay protection and
/// path re-binding); `loose` is target identity without node/state (detects newly surfaced UI).
#[derive(Clone, Debug)]
struct Candidate {
    action: Value,
    key: String,
    loose: String,
    label: String,
    name: String,
    words: String,
    fresh: bool,
    group: Option<String>,
    /// Fill whose text comes from the goal wording rather than plan.inputs; never auto-filled.
    derived: bool,
    /// Click hint on a non-password editable control (guided fill steps bind here).
    editable: bool,
}

impl Candidate {
    fn new(action: Value, key: Value, loose: Value, label: String, name: &str, context: &str) -> Self {
        Candidate { action, key: key.to_string(), loose: loose.to_string(), label,
            name: name.trim().to_lowercase(), words: format!("{name} {context}"), fresh: false, group: None, derived: false, editable: false }
    }
    fn kind(&self) -> &str { self.action["action"].as_str().unwrap_or_default() }
}

fn element_label(verb: &str, item: &Value) -> String {
    let name = item["name"].as_str().unwrap_or_default();
    let mut label = if name.is_empty() {
        format!("{verb} 无名{}[{}]", if item["column"].is_object() { "表头图标" } else { "图标" }, item["role"].as_str().unwrap_or_default())
    } else { format!("{verb} \"{}\" [{}]", short(name, 60), item["role"].as_str().unwrap_or_default()) };
    if let Some(icon) = item["icon"].as_str() { label += &format!(" icon={icon}"); }
    if let Some(field) = item["fieldContext"].as_str().filter(|s| !s.is_empty()) { label += &format!(" 字段={}", short(field, 60)); }
    else if let Some(column) = item["column"]["name"].as_str() {
        label += &format!(" 列={}#{}", short(column, 40), item["column"]["index"]);
        if let Some(group) = item["column"]["group"].as_str().filter(|s| !s.is_empty()) { label += &format!(" 分组={}", short(group, 30)); }
    }
    else if let Some(region) = item["region"].as_str().filter(|s| !s.is_empty()) { label += &format!(" @{}", short(region, 50)); }
    // The URL path is the most reliable navigation evidence (…/top-charts/pc vs …/benchmark).
    if let Some(path) = item["href"].as_str().and_then(|h| reqwest::Url::parse(h).ok()).map(|u| u.path().to_string()).filter(|p| p.len() > 1) {
        label += &format!(" →{}", short(&path, 60));
    }
    match item["selected"].as_bool().or_else(|| item["selected"].as_str().and_then(|s| s.parse().ok())) {
        Some(true) => label += " 已选中", Some(false) => label += " 未选中", None => (),
    }
    if let Some(sort) = item["sort"].as_str() { label += &format!(" sort={sort}"); }
    label
}

fn scroll_hints(page: &Value, item: Option<&Value>, horizontal: bool) -> Vec<Candidate> {
    let (top, height, viewport, min) = match item {
        None if horizontal => (&page["viewport"]["scrollX"], &page["documentSize"]["width"], &page["viewport"]["width"], page["viewport"]["minScrollX"].as_f64().unwrap_or(0.)),
        None => (&page["viewport"]["scrollY"], &page["documentSize"]["height"], &page["viewport"]["height"], 0.),
        Some(i) if horizontal => (&i["scroll"]["left"], &i["scroll"]["width"], &i["scroll"]["viewportWidth"], i["scroll"]["minLeft"].as_f64().unwrap_or(0.)),
        Some(i) => (&i["scroll"]["top"], &i["scroll"]["height"], &i["scroll"]["viewportHeight"], 0.),
    };
    let (Some(top), Some(height), Some(viewport)) = (top.as_f64(), height.as_f64(), viewport.as_f64()) else { return Vec::new(); };
    if viewport <= 0. || height <= viewport { return Vec::new(); }
    if item.is_some_and(|i| i["blockedBy"].is_string() && !i["scroll"][if horizontal {"pointX"} else {"pointY"}].is_object()) { return Vec::new(); }
    let step = (viewport * 0.8).clamp(80., 640.).round() as i32;
    let null = Value::Null;
    let name = item.map_or(&null, |i| &i["name"]);
    let area = name.as_str().filter(|s| !s.is_empty()).map(|s| short(s, 40)).unwrap_or_else(|| "页面".into());
    let mut out = Vec::new();
    for delta in [-step, step] {
        if (delta < 0 && top <= min) || (delta > 0 && top >= min + height - viewport - 1.) { continue; }
        let direction = match (horizontal, delta > 0) { (true, true) => "right", (true, false) => "left", (false, true) => "down", _ => "up" };
        let mut action = json!({"action":"scroll","frame":page["frame"],"delta":if horizontal {0} else {delta}});
        if horizontal { action["delta_x"] = json!(delta); }
        if let Some(i) = item { action["ref"] = i["ref"].clone(); }
        let key = json!({"action":"scroll","frame":page["frame"],"url":page["url"],
            "nodeId":item.map(|i|&i["nodeId"]),"name":name,"role":item.map(|i|&i["role"]),"region":item.map(|i|&i["region"]),
            "position":top,"extent":height,"viewport":viewport,"axis":if horizontal {"horizontal"} else {"vertical"},"direction":direction,"delta":delta});
        let loose = json!(["scroll", page["frame"], name, item.map(|i| &i["role"]), direction]);
        let zh = match direction { "up" => "向上", "down" => "向下", "left" => "向左", _ => "向右" };
        out.push(Candidate::new(action, key, loose, format!("滚动{zh} {area}"), &area, ""));
    }
    out
}

// Candidate actions and parameters remain local; the model may select only their IDs.
fn candidates(pages: &Value, plan: &Plan, used: &HashSet<String>) -> Result<Vec<Candidate>, String> {
    let frames = pages["pages"].as_array().ok_or("缺少 DOM 观察")?;
    let mut result = Vec::new();
    let mut scrolls = Vec::new();
    for page in frames {
        if page["frame"] == 0 {
            scrolls.extend(scroll_hints(page, None, false));
            scrolls.extend(scroll_hints(page, None, true));
        }
        let focused = page["focus"]["identity"].as_u64().map(|id| format!(":{id}"));
        let visible = page["visibleText"].as_str().or(page["text"].as_str()).unwrap_or_default().to_lowercase();
        for item in page["items"].as_array().ok_or("缺少 DOM 元素")? {
            let name = item["name"].as_str().unwrap_or_default();
            let role = item["role"].as_str().unwrap_or_default();
            if item["inView"] != true || item["disabled"] == true
                || !item["ref"].is_string() || !page["frame"].is_u64() || name.chars().count() > 300 { continue; }
            scrolls.extend(scroll_hints(page, Some(item), false));
            scrolls.extend(scroll_hints(page, Some(item), true));
            if item["blockedBy"].is_string() { continue; }
            let context = format!("{} {}", item["fieldContext"].as_str().unwrap_or_default(), item["column"]["name"].as_str().unwrap_or_default());
            let loose = |kind: &str, text: &Value| json!([kind, page["frame"], name, role, item["region"].as_str().map(|r| short(r, 120)),
                item["fieldContext"], item["column"]["name"], item["href"], text]);
            if item["editable"] == true {
                if item["password"] == true { continue; }
                // Clicking an editable control can open a date picker or combobox without typing.
                let key = json!({"action":"click","frame":page["frame"],"nodeId":item["nodeId"],
                    "name":name,"role":role,"region":item["region"],"fieldContext":item["fieldContext"],"expanded":item["expanded"]});
                let mut click = Candidate::new(json!({"action":"click","frame":page["frame"],"ref":item["ref"]}), key,
                    loose("click", &Value::Null), element_label("点击", item), name, &context);
                click.editable = true;
                result.push(click);
                for input in &plan.inputs {
                    // Bind authorized text before presenting choices. A model must never
                    // be offered that text for unrelated fields, even when they have labels.
                    if !input_matches(item, input) || frames.iter().flat_map(|p| p["items"].as_array().into_iter().flatten())
                        .filter(|i| input_matches(i, input) && i["editable"] == true && i["inView"] == true
                            && i["disabled"] != true && i["password"] != true).count() != 1 { continue; }
                    let text = json!(input.text);
                    if item["value"] == text {
                        // Human shortcut: a filled, focused authorized field can be submitted with Enter.
                        if focused.as_ref().is_some_and(|f| item["nodeId"].as_str().is_some_and(|id| id.ends_with(f.as_str()))) {
                            for (key_name, effect, verb) in [("Enter", "在已填好的授权输入框内按回车提交", "回车提交"),
                                ("Tab", "确认当前输入并把焦点移到下一个字段（不提交），常用于日期区间的开始→结束", "Tab 确认并移到下一字段")] {
                                let key = json!({"action":"press","key":key_name,"frame":page["frame"],"nodeId":item["nodeId"],
                                    "name":name,"role":role,"fieldContext":item["fieldContext"],"value":text,"effect":effect});
                                result.push(Candidate::new(json!({"action":"press","key":key_name}), key, loose(key_name, &text),
                                    format!("{verb} {}", element_label("", item).trim()), name, &context));
                            }
                        }
                        continue;
                    }
                    let key = json!({"action":"fill","frame":page["frame"],"nodeId":item["nodeId"],
                        "name":name,"role":role,"region":item["region"],"fieldContext":item["fieldContext"],
                        "authorizedField":input.field_context.as_ref().unwrap_or(&input.name),"binding":"only if field context uniquely matches authorizedField","text":input.text});
                    result.push(Candidate::new(json!({"action":"fill","frame":page["frame"],"ref":item["ref"],"text":input.text}), key,
                        loose("fill", &text), format!("{} = \"{}\"", element_label("填写", item), short(&input.text, 40)), name, &context));
                }
                // Like a person: the goal names a value that the list does not show → type it into the
                // panel's search box. Only search/filter boxes, only the main model's own words.
                if search_like(item) && !plan.inputs.iter().any(|input| input_matches(item, input)) {
                    for phrase in search_phrases(plan, item, &visible) {
                        let text = json!(phrase);
                        let key = json!({"action":"fill","frame":page["frame"],"nodeId":item["nodeId"],"name":name,"role":role,
                            "fieldContext":item["fieldContext"],"text":phrase,"derivedFrom":"任务文字中与该字段同句出现、页面上尚不可见的值；仅用于搜索/筛选"});
                        let mut candidate = Candidate::new(json!({"action":"fill","frame":page["frame"],"ref":item["ref"],"text":phrase}), key,
                            loose("fill", &text), format!("{} = \"{}\"（来自任务文字）", element_label("搜索框输入", item), short(&phrase, 40)), name, &context);
                        candidate.derived = true;
                        result.push(candidate);
                    }
                }
                continue;
            }
            if !(matches!(role, "button" | "a" | "link" | "tab" | "menuitem" | "menuitemcheckbox" | "menuitemradio" | "option" | "radio" | "checkbox" | "combobox" | "summary")
                || item["actionable"] == true
                || item["haspopup"].as_str().is_some_and(|v| matches!(v, "true" | "menu" | "listbox" | "tree" | "grid" | "dialog"))
                || ((role == "input" || item["column"].is_object()) && item["tabIndex"].as_i64().is_some_and(|v| v >= 0)
                    && !item["scroll"].is_object())) { continue; }
            let key = json!({"action":"click","frame":page["frame"],"nodeId":item["nodeId"],
                "name":name,"role":role,"region":item["region"],"fieldContext":item["fieldContext"],"href":item["href"],"text":Value::Null,
                "column":item["column"],"sort":item["sort"],"icon":item["icon"],"selected":item["selected"],"expanded":item["expanded"]});
            result.push(Candidate::new(json!({"action":"click","frame":page["frame"],"ref":item["ref"]}), key,
                loose("click", &Value::Null), element_label("点击", item), name, &context));
        }
    }
    result.extend(scrolls);
    result.retain(|c| !used.contains(&c.key));
    // Same role + same name shape (digits folded) = one visual collection: calendar day cells,
    // per-row "Open" buttons, unnamed column icons. Large collections are offered as one group.
    for c in result.iter_mut().filter(|c| c.kind() == "click") {
        let key: Value = serde_json::from_str(&c.key).unwrap_or(Value::Null);
        let mut shape = String::new();
        for ch in c.name.chars() {
            if ch.is_ascii_digit() { if !shape.ends_with('#') { shape.push('#'); } } else { shape.push(ch); }
        }
        c.group = Some(json!([key["frame"], key["role"], shape.split_whitespace().collect::<Vec<_>>().join(" ")]).to_string());
    }
    Ok(result)
}

fn is_cjk(c: char) -> bool { matches!(c as u32, 0x3040..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xAC00..=0xD7AF) }

/// Lowercased latin words (≥2 chars) and CJK bigrams; enough for relevance ranking, not authorization.
fn terms(text: &str) -> HashSet<String> {
    const STOP: &[&str] = &["the","and","or","to","of","in","on","for","an","by","with","then","is","be","it","as","at","all","only"];
    let chars: Vec<char> = text.to_lowercase().chars().collect();
    let mut out = HashSet::new();
    let mut i = 0;
    while i < chars.len() {
        let start = i;
        if is_cjk(chars[i]) {
            while i < chars.len() && is_cjk(chars[i]) { i += 1; }
            out.extend(chars[start..i].windows(2).map(|pair| pair.iter().collect::<String>()));
        } else if chars[i].is_alphanumeric() {
            while i < chars.len() && chars[i].is_alphanumeric() && !is_cjk(chars[i]) { i += 1; }
            let word: String = chars[start..i].iter().collect();
            if word.chars().count() >= 2 && !STOP.contains(&word.as_str()) { out.insert(word); }
        } else { i += 1; }
    }
    out
}

/// One JEV batch: individual hints (`aNN`) plus collapsed collections (`gNN`) that expand into a
/// second, member-only decision. `covered` lists every candidate key represented in the batch.
struct Batch {
    list: BTreeMap<String, Candidate>,
    groups: BTreeMap<String, (String, Vec<Candidate>)>,
    covered: Vec<String>,
}

/// Ranks hints like a person scanning a page: authorized fills, controls that just appeared
/// (opened menus/dialogs) and controls named in the goal come first; big repetitive collections
/// collapse into one entry so they cannot crowd out unique controls such as preset shortcuts.
/// Returns the batch in DOM order with stable, sortable IDs; `skip` pages past declined hints.
fn shortlist(all: &[Candidate], plan: &Plan, skip: &HashSet<String>) -> Batch {
    let goal_text = format!("{}\n{}\n{}\n{}", plan.task, plan.expected_text,
        plan.inputs.iter().map(|i| format!("{} {}", i.name, i.text)).collect::<Vec<_>>().join("\n"),
        plan.control_names.join("\n")).to_lowercase();
    let goal = terms(&goal_text);
    let named = plan.control_names.iter().map(|n| n.trim().to_lowercase()).collect::<Vec<_>>();
    let primary = terms("confirm apply search submit save ok done next 确定 确认 查询 搜索 应用 提交 保存 完成 下一步");
    // A goal only "names" a control when the name stands alone. Occurrences inside a larger token
    // don't count: the "19" and "03" of the authorized date 2026-03-19 are not the calendar's
    // 19th and 3rd cells, and letting them score leaves stray cells outside their collapsed group.
    let called_out = |name: &str| -> bool {
        let name: Vec<char> = name.chars().collect();
        if name.len() < 2 { return false; }
        if !name.iter().all(char::is_ascii_digit) { return goal_text.contains(&name.iter().collect::<String>()); }
        let text: Vec<char> = goal_text.chars().collect();
        let boundary = |c: char| !c.is_ascii_alphanumeric() && !"-/.:".contains(c);
        text.windows(name.len()).enumerate().any(|(i, window)| window == name.as_slice()
            && i.checked_sub(1).is_none_or(|j| boundary(text[j]))
            && text.get(i + name.len()).is_none_or(|c| boundary(*c)))
    };
    let relevance = |c: &Candidate| -> i64 {
        let own = terms(&c.words);
        let mut score = 12 * own.intersection(&goal).count().min(6) as i64;
        if called_out(&c.name) { score += 25; }
        if named.iter().any(|n| c.words.to_lowercase().contains(n.as_str())) { score += 60; }
        score
    };
    let score = |c: &Candidate| -> i64 {
        let mut score = relevance(c);
        if terms(&c.words).intersection(&primary).next().is_some() { score += 10; }
        if c.fresh { score += 150; }
        score + match c.kind() { "fill" if c.derived => 300, "fill" => 1000, "press" => 400, "scroll" if c.action["ref"].is_null() => 40, "scroll" => 15, _ => 0 }
    };
    let open: Vec<usize> = (0..all.len()).filter(|&i| !skip.contains(&all[i].key) || all[i].kind() == "fill").collect();
    let mut sizes = HashMap::<&str, usize>::new();
    for &i in &open { if let Some(g) = &all[i].group { *sizes.entry(g).or_default() += 1; } }
    // (order, score, members): one member = individual hint, several = collapsed group.
    let mut entries: Vec<(usize, i64, Vec<usize>)> = Vec::new();
    let mut grouped = BTreeMap::<usize, Vec<usize>>::new();
    let mut group_slot = HashMap::<&str, usize>::new();
    for &i in &open {
        let c = &all[i];
        match c.group.as_deref() {
            // Members explicitly named by the goal stay individually selectable.
            Some(g) if sizes[g] >= GROUP_MIN && relevance(c) < 25 => {
                let first = *group_slot.entry(g).or_insert(i);
                grouped.entry(first).or_default().push(i);
            }
            _ => entries.push((i, score(c), vec![i])),
        }
    }
    for (first, members) in grouped {
        let best = members.iter().map(|&i| score(&all[i])).max().unwrap_or(0);
        entries.push((first, best, members));
    }
    entries.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    entries.truncate(SHORTLIST);
    entries.sort_by_key(|(order, _, _)| *order);
    let mut batch = Batch { list: BTreeMap::new(), groups: BTreeMap::new(), covered: Vec::new() };
    for (_, _, members) in entries {
        batch.covered.extend(members.iter().map(|&i| all[i].key.clone()));
        if members.len() == 1 {
            batch.list.insert(format!("a{:02}", batch.list.len() + 1), all[members[0]].clone());
            continue;
        }
        let items: Vec<Candidate> = members.iter().map(|&i| all[i].clone()).collect();
        let sample = |c: &Candidate| short(&c.label, 60);
        let label = format!("{}折叠分组：{} 个同类控件（{}、{} … {}）。选择后展开，再从中选一个",
            if items.iter().any(|c| c.fresh) { "[新] " } else { "" }, items.len(),
            sample(&items[0]), sample(&items[1]), sample(items.last().unwrap()));
        batch.groups.insert(format!("g{:02}", batch.groups.len() + 1), (label, items));
    }
    batch
}

/// A control name mentioned only to be excluded ("…不是顶部的 Search…", "不要点…") must repel
/// instead of attract. Short generic names ("search", "确认") are descriptive, not identifiers,
/// so only specific names (long or multi-word) are treated as exclusions.
fn negated_mention(target: &str, name: &str) -> bool {
    if name.chars().count() < 8 && !name.contains(' ') { return false; }
    const CUES: &[&str] = &["不是", "不要", "禁止", "别点", "别选", "勿", "避免", "而非", "而不是", "不用", "不需要", "无需", "不点", "排除"];
    let mut found = false;
    let mut idx = 0;
    while let Some(pos) = target[idx..].find(name) {
        found = true;
        let before: String = target[..idx + pos].chars().rev().take(12).collect::<Vec<_>>().into_iter().rev().collect();
        if !CUES.iter().any(|w| before.contains(w)) { return false; }
        idx += pos + name.len();
    }
    found
}

/// How well a hint matches a guided step. Exact name, quoted text and container are decisive;
/// description words, role words ("复选框", "排序图标"…) and freshness break ties.
fn step_score(step: &Step, c: &Candidate) -> i64 {
    let key: Value = serde_json::from_str(&c.key).unwrap_or(Value::Null);
    match step.action.as_str() {
        "fill" if !c.editable => return i64::MIN,
        "click" if c.kind() != "click" => return i64::MIN,
        "scroll" => {
            if c.kind() != "scroll" || key["direction"] != step.direction.as_deref().unwrap_or("down") { return i64::MIN; }
            if step.target.trim().is_empty() { return if c.action["ref"].is_null() { 60 } else { 1 }; }
        }
        _ => (),
    }
    let target = step.target.to_lowercase();
    let label = c.label.to_lowercase();
    let context = [&key["region"], &key["fieldContext"], &key["column"]["name"], &key["column"]["group"]].iter()
        .filter_map(|v| v.as_str()).collect::<Vec<_>>().join(" ").to_lowercase();
    let role = key["role"].as_str().unwrap_or_default();
    let has = |words: &[&str]| words.iter().any(|w| target.contains(w));
    let mut score = 0i64;
    if let Some(name) = &step.name {
        let name = name.trim().to_lowercase();
        // `name` may identify the column/group rather than the control's own visible text:
        // name "Digital Units" + target "…排序图标" means that column's icon, not the header text.
        // Without this, the -60 nameless penalty buries the right icon under the header text.
        let col = key["column"]["name"].as_str().unwrap_or_default().trim().to_lowercase();
        let grp = key["column"]["group"].as_str().unwrap_or_default().trim().to_lowercase();
        let col_hit = !col.is_empty() && col.chars().count() >= 2 && (col == name || col.contains(&name) || name.contains(&col));
        let grp_hit = !grp.is_empty() && grp.chars().count() >= 2 && (grp == name || grp.contains(&name) || name.contains(&grp));
        if col_hit {
            score += if c.name.is_empty() { 60 } else if c.name == name { 100 } else { 25 };
        } else if grp_hit {
            score += 25;
        } else {
            score += if c.name == name { 100 } else if !c.name.is_empty() && (c.name.contains(&name) || name.contains(&c.name)) { 35 } else { -60 };
        }
    }
    static QUOTED: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let quoted = QUOTED.get_or_init(|| regex::Regex::new(r#"[“"「『‘]([^“”"「」『』‘’\n]{1,60})[”"」』’]"#).unwrap());
    for q in quoted.captures_iter(&step.target) {
        let q = q[1].trim().to_lowercase();
        if negated_mention(&target, &q) { continue; }
        if c.name == q { score += 80 } else if label.contains(&q) || context.contains(&q) { score += 25 }
    }
    // A control whose whole name appears in the description; longer names are more specific.
    // A name mentioned only to be excluded ("不是…", "不要点…") repels instead of attracting.
    if c.name.chars().count() >= 2 && target.contains(&c.name) {
        score += if negated_mention(&target, &c.name) { -60 } else { 30 + c.name.chars().count().min(20) as i64 };
    }
    let generic = terms("点击 click 按钮 button 复选框 勾选 checkbox 输入框 文本框 input 图标 icon 排序 链接 link 菜单 menu 选项 option 下的 里的 中的 面板 panel 的 列 column");
    let wanted: HashSet<String> = terms(&target).difference(&generic).cloned().collect();
    let hit = wanted.intersection(&terms(&format!("{label} {context}"))).count();
    score += 12 * hit as i64;
    if !wanted.is_empty() && hit == wanted.len() { score += 15; }
    let icon = key["icon"].as_str().unwrap_or_default();
    if has(&["复选框", "勾选", "checkbox"]) && (matches!(role, "checkbox" | "menuitemcheckbox") || !key["selected"].is_null()) { score += 25; }
    if has(&["搜索框", "输入框", "文本框", "input", "search box"]) && c.editable { score += 25; }
    if has(&["图标", "icon"]) { score += if c.name.is_empty() { 25 } else { -30 }; }
    if has(&["排序", "sort"]) && (["sort", "caret", "order", "asc", "desc"].iter().any(|k| icon.contains(k)) || !key["sort"].is_null()) { score += 25; }
    if has(&["按钮", "button"]) && role == "button" { score += 10; }
    if has(&["链接", "link", "菜单", "menu"]) && matches!(role, "a" | "link" | "menuitem" | "li") { score += 10; }
    if has(&["选项", "option"]) && matches!(role, "option" | "menuitemcheckbox" | "menuitemradio" | "checkbox" | "radio") { score += 10; }
    // Table disambiguation: a column/group named in the description is decisive, so the
    // wrong column's icon cannot tie with the right one (sort icon vs header text vs sibling column).
    if let Some(col) = key["column"]["name"].as_str() {
        let col = col.trim().to_lowercase();
        if col.chars().count() >= 2 && target.contains(&col) { score += 20; }
    }
    if let Some(group) = key["column"]["group"].as_str() {
        let group = group.trim().to_lowercase();
        if group.chars().count() >= 2 && target.contains(&group) { score += 10; }
    }
    if let Some(within) = &step.within {
        let within = within.to_lowercase();
        score += if context.contains(&within) || label.contains(&within) { 40 } else { -40 };
    }
    if let Some(wanted) = &step.role { score += if role.eq_ignore_ascii_case(wanted) { 30 } else { -30 }; }
    if c.fresh { score += 15; }
    score
}

/// Local certainty: a strong match clearly ahead of the runner-up. Anything closer goes to JEV.
fn step_confident(best: i64, second: Option<i64>) -> bool {
    best >= 40 && second.is_none_or(|s| best - s >= 25)
}

fn step_ranking<'a>(step: &Step, all: &'a [Candidate]) -> Vec<(i64, &'a Candidate)> {
    let mut ranked: Vec<(i64, &Candidate)> = all.iter().map(|c| (step_score(step, c), c)).filter(|(s, _)| *s > 0).collect();
    ranked.sort_by(|a, b| b.0.cmp(&a.0));
    ranked
}

/// A fill step carrying an input's exact text may only bind to that input's field (or a derived
/// search candidate with the same text) — never to an unrelated box that happens to match the
/// description. Steps with novel text keep legacy freedom.
fn rank_for_step<'a>(step: &Step, plan: &Plan, all: &'a [Candidate]) -> Vec<(i64, &'a Candidate)> {
    let mut ranked = step_ranking(step, all);
    if step.action == "fill" {
        if let Some(text) = step.text.as_deref() {
            if plan.inputs.iter().any(|i| i.text == text) {
                ranked.retain(|(_, c)| {
                    let key: Value = serde_json::from_str(&c.key).unwrap_or(Value::Null);
                    if key["text"].as_str() == Some(text) { return true; }
                    // The clickable face of the authorized field (the fill action is built on bind).
                    c.editable && plan.inputs.iter().any(|i| i.text == text && input_matches(
                        &json!({"name":key["name"],"role":key["role"],"fieldContext":key["fieldContext"]}), &i))
                });
            }
        }
    }
    ranked
}

/// The executable action for a step bound to a hint.
fn bind_step(step: &Step, index: usize, c: &Candidate) -> Candidate {
    let mut bound = c.clone();
    if step.action == "fill" {
        bound.action = json!({"action":"fill","frame":c.action["frame"],"ref":c.action["ref"],"text":step.text});
        bound.label = format!("{} = \"{}\"", c.label.replacen("点击", "填写", 1), short(step.text.as_deref().unwrap_or_default(), 40));
        bound.key = format!("{}|fill|{}", c.key, step.text.as_deref().unwrap_or_default());
    }
    bound.label = format!("第{}步 {}", index + 1, bound.label);
    bound
}

/// Only steps that cannot change what the following targets mean may run back-to-back in one act:
/// typing, Tab, and toggling a selection. Menus, links, submits and Enter end the batch.
fn chains(step: &Step, bound: &Candidate) -> bool {
    if step.expect().is_some() || step.repeat > 0 { return false; }
    let key: Value = serde_json::from_str(&bound.key.split("|fill|").next().unwrap_or_default()).unwrap_or(Value::Null);
    match step.action.as_str() {
        "fill" => true,
        "press" => step.key.as_deref() == Some("Tab"),
        "click" => matches!(key["role"].as_str(), Some("checkbox" | "radio" | "option" | "menuitemcheckbox" | "menuitemradio"))
            && key["haspopup"].is_null(),
        _ => false,
    }
}

fn literal_met(pages: &Value, expect: &str) -> bool {
    let haystack = pages["pages"].as_array().into_iter().flatten()
        .map(|p| format!("{} {} {}", p["url"].as_str().unwrap_or_default(),
            p["visibleText"].as_str().or(p["text"].as_str()).unwrap_or_default(), p["title"].as_str().unwrap_or_default()))
        .collect::<Vec<_>>().join("\n").to_lowercase();
    expect.split(['|', '｜']).map(str::trim).filter(|s| !s.is_empty()).any(|s| haystack.contains(&s.to_lowercase()))
}

/// Panel selections usually need a commit step. Runs that changed a selection or filled a
/// value but never clicked the panel's Confirm-like button report a precise failure: the literal
/// completion text often matches already ("Germany" appears as selected), yet the filter is not
/// applied. Triggers only when commit buttons were visibly offered while the panel was open — not
/// merely present somewhere on the page — so pure read runs never see this error.
fn needs_commit(plan: &Plan, history: &[Value], pages: &Value) -> Option<String> {
    const COMMIT_WORDS: &[&str] = &["确认", "Apply", "apply", "Confirm", "confirm", "应用", "提交"];
    if !COMMIT_WORDS.iter().any(|w| plan.authorization.contains(w) || plan.task.contains(w)) { return None; }
    // The panel must actually have been interacted with: a fill, or a selection that flipped on.
    let changed_selection = json!(history).to_string().contains("已选中");
    let filled = history.iter().flat_map(|h| h["actions"].as_array().into_iter().flatten()).filter_map(Value::as_str)
        .any(|a| a.contains("填写"));
    if !changed_selection && !filled { return None; }
    // "填写" alone only counts when it typed a filter value (option text / search phrase),
    // not a date or other unrelated field.
    if !changed_selection && filled {
        let typed_filter_value = history.iter().flat_map(|h| h["actions"].as_array().into_iter().flatten())
            .filter_map(Value::as_str).any(|a| a.contains("填写") && {
                let lower = a.to_lowercase();
                plan.inputs.iter().any(|i| !i.text.trim().is_empty() && lower.contains(&i.text.trim().to_lowercase()))
                    || plan.phrases.iter().any(|p| !p.is_empty() && lower.contains(&p.to_lowercase()))
            });
        if !typed_filter_value { return None; }
    }
    let actions: Vec<&str> = history.iter().flat_map(|h| h["actions"].as_array().into_iter().flatten())
        .filter_map(Value::as_str).collect();
    const NAMES: &[&str] = &["confirm", "apply", "submit", "save", "确认", "确定", "应用", "提交", "保存"];
    let commit = pages["pages"].as_array()?.iter()
        .flat_map(|p| p["items"].as_array().into_iter().flatten())
        .find(|i| i["inView"] == true && i["disabled"] != true && i["ref"].is_string()
            && NAMES.iter().any(|n| i["name"].as_str().unwrap_or_default().trim().eq_ignore_ascii_case(n)))?;
    let name = commit["name"].as_str().unwrap_or_default();
    if actions.iter().any(|a| a.contains(name)) { return None; }
    // Already applied? The value shows outside the option itself: a filter summary pill
    // ("Country/Market Germany") or the URL. Selecting often applies immediately; only
    // insist on the commit click when nothing shows the filter took effect.
    let want = plan.expected_text.trim().to_lowercase();
    if !want.is_empty() {
        let applied = pages["pages"].as_array().into_iter().flatten().any(|p| {
            p["url"].as_str().is_some_and(|u| u.to_lowercase().contains(&want))
                || p["items"].as_array().into_iter().flatten().any(|i| i["inView"] == true
                    && i["name"].as_str().is_some_and(|n| n.to_lowercase().contains(&want))
                    && !matches!(i["role"].as_str(),
                        Some("checkbox" | "radio" | "option" | "menuitem" | "menuitemcheckbox" | "menuitemradio" | "li")))
        });
        if applied { return None; }
    }
    // The commit button was a choice on an earlier screen only if its state survived a
    // refresh (selection flips, panel search results) or it sits next to one.
    let beside_panel = pages["pages"].as_array().into_iter().flatten()
        .flat_map(|p| p["items"].as_array().into_iter().flatten())
        .any(|i| i["selected"].is_boolean() || i["expanded"] == "true");
    let panel_interaction = history.iter().flat_map(|h| h["actions"].as_array().into_iter().flatten())
        .filter_map(Value::as_str).any(|a| a.contains("面板") || a.contains("复选") || a.contains("panel") || a.contains("checkbox"));
    if !changed_selection && !panel_interaction && !beside_panel { return None; }
    Some(format!("本轮已勾选/填写，但「{name}」仍可点击且本轮未点，筛选可能尚未生效；请新起一次 run，只提交一步点击「{name}」(target 写明所在面板/筛选区)，然后核验「{}", plan.expected_text))
}

// ponytail: allow up to 15s without progress; long-running jobs need a separate monitoring goal.
fn observation_delay(elapsed: Duration) -> Result<Duration, String> {
    Duration::from_secs(15).checked_sub(elapsed).filter(|left| !left.is_zero())
        .map(|left| left.min(POLL))
        .ok_or_else(|| "等待页面变化超过15秒，交回主模型；不重放已执行动作".into())
}

// The JEV branch of the tree only contains current DOM leaves; rebuilding after feedback
// invalidates every previous reference.
fn decision_tree(pages: &Value, list: &BTreeMap<String, Candidate>, choices: &BTreeMap<String, String>, revision: usize) -> Value {
    let mut branches = json!([
        {"id":"interact","when":"目标可见可操作，或是通往目标的入口/关闭遮挡的控件：点击、填写或回车。[新]出现的菜单选项优先于同名表头；表头文字可能只打开释义，排序入口可能是该列无名图标","leaves":[]},
        {"id":"reveal","when":"目标在视口外：按目标所在区域和方向滚动；scrollFeedback.changed=false表示没动，换区域或方向","leaves":[]},
        {"id":"wait","when":"确有加载或刚提交的结果尚未出现；已可操作的菜单不属于等待","leaves":[]},
        {"id":"verify","when":"URL参数、选中集合selectionFields、日期dateFields、排序和表格结果已满足全部完成条件；选项出现不等于已选中","leaves":[]},
        {"id":"blocked","when":"缺授权/输入、真实歧义或需要视觉","leaves":["defer"]}
    ]);
    for id in choices.keys() {
        let branch = match id.as_str() {
            "observe" => 2, "done" => 3,
            _ if list.get(id).is_some_and(|c| c.kind() == "scroll") => 1,
            _ => 0,
        };
        branches[branch]["leaves"].as_array_mut().unwrap().push(json!(id));
    }
    let obstacles: Vec<_> = pages["pages"].as_array().into_iter().flatten()
        .flat_map(|p| p["items"].as_array().into_iter().flatten()
            .filter(|i| i["inView"] == true && i["blockedBy"].is_string())
            .map(|i| json!({"target":i["name"],"blockedBy":i["blockedBy"]})))
        .take(16).collect();
    json!({"revision":revision,"obstacles":obstacles,"branches":branches})
}

fn runtime_tree(jev: &Value, pending: &VecDeque<(String, String)>) -> Value {
    json!({"root":"每次观察后按顺序判断，首个成立的节点执行；动作后旧叶子全部失效","nodes":[
        {"id":"guard","when":"跨站、观察缺口、预算耗尽或同一状态反复操作","then":"交回主模型"},
        {"id":"wait","when":"页面 loading","then":"本地每250ms刷新，最多4秒，不请求JEV"},
        {"id":"reflex","when":"已授权 inputs 唯一绑定且当前值不同","then":"本地批量 fill，不请求JEV"},
        {"id":"path","when":"JEV 路径下一步在新DOM中同节点同状态、URL不变、没有新控件出现且页面稳定","then":"直接执行，不请求JEV",
            "pending":pending.iter().map(|(_, label)| label).collect::<Vec<_>>()},
        {"id":"jev","when":"信息边界（新菜单、新页面、路径失效）","then":"JEV 一次请求选择当前一步并预测同屏后续路径","tree":jev},
        {"id":"fallback","when":"defer、JEV 不可用或执行不明确","then":"返回最新观察，主模型兜底"}]})
}

fn decision_evidence(pages: &Value, plan: &Plan, list: &BTreeMap<String, Candidate>) -> String {
    let mut view = pages.clone();
    for page in view["pages"].as_array_mut().into_iter().flatten() {
        let text = page["visibleText"].as_str().unwrap_or_else(|| page["text"].as_str().unwrap_or_default());
        // ponytail: old captures lack visibleText; preserve both toolbar and appended popup, within 4k chars.
        let chars: Vec<_> = text.chars().collect();
        page["text"] = json!(if chars.len() <= 4000 { text.to_owned() } else {
            format!("{}\n[正文省略]\n{}", chars[..2400].iter().collect::<String>(), chars[chars.len()-1600..].iter().collect::<String>())
        });
        if let Some(items) = page["items"].as_array_mut() {
            items.retain(|i| list.values().any(|c| c.action["ref"].is_string() && c.action["ref"] == i["ref"])
                || !i["selected"].is_null() || i["expanded"] == "true" || i["sort"].is_string() || i["dateValue"].is_string());
            for item in items { if let Some(region) = item["region"].as_str() { item["region"] = json!(region.chars().take(120).collect::<String>()); } }
        }
    }
    evidence(&view, plan)
}

/// Observable consequence of one action: navigation, newly surfaced controls, newly visible text
/// (tooltips, validation, result counts) and changed control states. Empty = nothing happened.
fn action_effect(before: &Value, after: &Value, plan: &Plan) -> Value {
    let url = |pages: &Value| pages["pages"][0]["url"].as_str().unwrap_or_default().to_string();
    let lines = |pages: &Value| -> Vec<String> {
        pages["pages"].as_array().into_iter().flatten()
            .flat_map(|p| p["visibleText"].as_str().or(p["text"].as_str()).unwrap_or_default().lines().map(str::trim).map(String::from).collect::<Vec<_>>())
            .filter(|l| !l.is_empty()).collect()
    };
    let old_lines: HashSet<String> = lines(before).into_iter().collect();
    let new_text: Vec<String> = lines(after).into_iter().filter(|l| !old_lines.contains(l)).take(4).map(|l| short(&l, 80)).collect();
    let none = HashSet::new();
    let old_controls: HashSet<String> = candidates(before, plan, &none).unwrap_or_default().into_iter().map(|c| c.loose).collect();
    let new_controls: Vec<String> = candidates(after, plan, &none).unwrap_or_default().into_iter()
        .filter(|c| c.kind() != "scroll" && !old_controls.contains(&c.loose)).map(|c| c.label).collect();
    let states = |pages: &Value| -> HashMap<String, Value> {
        pages["pages"].as_array().into_iter().flatten().flat_map(|p| p["items"].as_array().into_iter().flatten())
            .filter_map(|i| Some((i["nodeId"].as_str()?.to_string(), json!([i["selected"], i["expanded"], i["sort"], i["icon"], i["name"]]))))
            .collect()
    };
    let old_states = states(before);
    let changed: Vec<String> = after["pages"].as_array().into_iter().flatten().flat_map(|p| p["items"].as_array().into_iter().flatten())
        .filter(|i| i["nodeId"].as_str().and_then(|id| old_states.get(id))
            .is_some_and(|old| old != &json!([i["selected"], i["expanded"], i["sort"], i["icon"], i["name"]])))
        .take(6).map(|i| element_label("", i).trim().to_string()).collect();
    let (from, to) = (url(before), url(after));
    let nothing = from == to && new_text.is_empty() && new_controls.is_empty() && changed.is_empty();
    json!({"url":if from != to { json!(to) } else { Value::Null },
        "newControls":new_controls.iter().take(6).collect::<Vec<_>>(),"newControlCount":new_controls.len(),
        "newText":new_text,"changed":changed,
        "summary":if nothing { "页面没有可见变化：该动作可能无效，换目标或方法" } else { "" }})
}

fn fill_identity(pages: &Value, action: &Value) -> Option<String> {
    if action["action"] != "fill" { return None; }
    let page = pages["pages"].as_array()?.iter().find(|p| p["frame"] == action["frame"])?;
    let item = page["items"].as_array()?.iter().find(|i| i["ref"] == action["ref"])?;
    Some(json!([page["frame"], item["nodeId"].as_str()?, item["fieldContext"], action["text"]]).to_string())
}

fn choice_description(candidate: &Candidate) -> String {
    let mut description: Value = serde_json::from_str(&candidate.key).unwrap_or(Value::Null);
    let fresh = if candidate.fresh { "[新] " } else { "" };
    if description["action"] == "click" && matches!(description["role"].as_str(), Some("checkbox" | "menuitemcheckbox")) {
        description["effect"] = json!(match description["selected"].as_bool().or_else(|| description["selected"].as_str().and_then(|s| s.parse().ok())) {
            Some(true) => "取消选中（不是展开）；若目标是保留选中，不要点击",
            Some(false) => "选中（不是展开）；分组复选框可能同时选中全部子项",
            None => "切换选择状态（不是展开）；当前选中状态不明确",
        });
    }
    if description["action"] == "click" && ["confirm", "apply", "submit", "save", "确认", "确定", "应用", "提交", "保存"]
        .iter().any(|n| description["name"].as_str().unwrap_or_default().trim().eq_ignore_ascii_case(n)) {
        description["effect"] = json!("确认/应用按钮：面板内的勾选或填写通常要点它才真正生效；若本轮刚勾选/填写过，下一步通常是点它");
    }
    if description["action"] == "scroll" {
        return format!("{fresh}{}；当前位置={}，内容长度={}，可见长度={}。用于寻找视口外的控件/表头，执行后检查实际位移。",
            candidate.label, description["position"], description["extent"], description["viewport"]);
    }
    if description["action"] == "click" && description["column"].is_object() {
        let icon = description["icon"].as_str().unwrap_or_default();
        description["effect"] = json!(if description["name"].as_str().is_some_and(|n| !n.is_empty()) {
            "表头文字：可能只弹出释义浮层而不排序；若上一步点它只出现释义文字，改点同列的无名排序图标"
        } else if ["sort", "caret", "order", "asc", "desc"].iter().any(|k| icon.contains(k)) {
            "表头排序图标：切换该列排序（常见顺序 无→升序→降序）；先核对列名/分组，同名列看分组与序号"
        } else { "表头内无名图标：通常是排序或筛选入口，看 icon 提示" });
    }
    // The label already carries name/role/context/icon/href/state; only keep fields it lacks.
    let mut extra = serde_json::Map::new();
    for key in ["effect", "text", "authorizedField", "derivedFrom", "expanded", "haspopup"] {
        if let Some(value) = description.get(key).filter(|v| !v.is_null()) { extra.insert(key.into(), value.clone()); }
    }
    if let Some(region) = description["region"].as_str().filter(|r| !r.is_empty() && !candidate.label.contains(&short(r, 50))) {
        extra.insert("region".into(), json!(short(region, 100)));
    }
    let extra = if extra.is_empty() { String::new() } else { format!(" {}", Value::Object(extra)) };
    format!("{fresh}{}{extra}", candidate.label).chars().take(700).collect()
}

struct Run<'a> {
    root: &'a Path,
    owner: &'a str,
    tool: &'a str,
    target_key: &'static str,
    target: String,
    plan: Plan,
    snapshot: Value,
    latest: Value,
    history: Vec<Value>,
    decisions: Vec<Value>,
    trace: Vec<Value>,
    tree: Value,
    missing_inputs: Vec<Value>,
    candidate_counts: Vec<usize>,
    refreshes: usize,
    executed: usize,
    cached: usize,
    reflex: usize,
    preflight_recoveries: usize,
    preflight: usize,
    used: HashSet<(String, String)>,
    filled: HashSet<String>,
    fill_attempts: HashMap<String, usize>,
    state_actions: HashMap<String, usize>,
    baseline: Option<HashSet<String>>,
    pending: VecDeque<(String, String)>,
    path_url: Value,
    loading_since: Option<Instant>,
    // Guided mode (plan.steps): progress, how targets were bound, and expectation checks.
    raw_steps: Value,
    step_index: usize,
    resolved_locally: usize,
    resolved_by_jev: usize,
    checks: Vec<Value>,
    step_candidates: Vec<String>,
}

enum Resolution { Found(Candidate), Missing, Ambiguous(String) }

fn press_candidate(step: &Step, index: usize) -> Candidate {
    let key = step.key.as_deref().unwrap_or_default();
    Candidate::new(json!({"action":"press","key":key}), json!({"action":"press","key":key,"step":index}),
        json!(["press", key, index]), format!("第{}步 按键 {key}", index + 1), "", "")
}

const PICK_INSTRUCTIONS: &str = "主模型已经决定了这一步要做什么，你只负责从候选中找出这一步所指的那个控件。\
依次核对：名称或含义（界面可能是英文，按语义匹配）、所在区域/字段/列与分组、图标类型、链接路径；\
[新] 表示上一步刚出现（例如刚展开的菜单或面板），通常就是这一步要找的。\
上一步点击后页面没有变化的控件不要再选；刚展开的菜单或面板中新出现的选项优先于背景里的同名控件。\
只选完全对应的一个；都不对，或有两个同样符合时选 defer。";

const CHECK_INSTRUCTIONS: &str = "只根据最新页面证据判断期望是否已经成立。排序看 sort/aria-sort、icon 状态、URL 参数和表格数据的实际顺序；\
筛选看选中集合（selectionFields）与筛选区文字；日期看 dateFields；导航看 URL 和标题。\
只出现释义/提示文字不算完成。证据不足选 defer。";

impl Run<'_> {
    fn args(&self, mut value: Value) -> Value {
        value[self.target_key] = json!(self.target);
        value
    }
    // JEV reads the full stored observation; the inline summary only goes back to the main model,
    // so keep it small enough to never be truncated into a file the model has to read with a shell.
    fn inspect_args(&self) -> Value {
        self.args(json!({"operation":"inspect","scope":"viewport","visual":"none","maxTextChars":2500,"maxItems":30}))
    }
    fn pages(&self) -> Result<Value, String> {
        crate::native_browser::jev_observation(self.root, &self.args(json!({"snapshotId":self.snapshot})), self.owner, self.tool)
    }
    async fn refresh(&mut self) -> Result<Value, String> {
        let observed = execute_browser(self.root, &self.inspect_args(), self.owner, self.tool).await?;
        self.refreshes += 1;
        self.snapshot = observed["snapshotId"].clone();
        self.latest = observed;
        self.pages()
    }
    fn note(&mut self, node: &str, detail: Value) {
        // ponytail: keep the last 64 tree decisions; history keeps every executed action.
        if self.trace.len() >= 64 { self.trace.remove(0); }
        self.trace.push(json!({"node":node,"detail":detail,"executed":self.executed}));
    }

    /// Executes one batch (a single hint, or several reflex fills) from the observed state.
    /// Ok(None): DOM preflight failed before any input — re-decide from fresh DOM.
    /// Ok(Some(n)): the first n actions of the batch completed (the rest sent no input).
    async fn perform(&mut self, pages: &Value, state: &str, anchors: HashSet<String>, batch: Vec<Candidate>, node: &str) -> Result<Option<usize>, String> {
        let repeats = self.state_actions.entry(state.to_string()).or_default();
        *repeats += 1;
        if *repeats > STATE_REPEATS { return Err("同一页面状态已多次操作仍未推进，疑似循环；交回主模型".into()); }
        for c in &batch { self.used.insert((state.to_string(), c.key.clone())); }
        self.baseline = Some(anchors);
        let actions: Vec<Value> = batch.iter().map(|c| c.action.clone()).collect();
        let mut args = self.args(json!({"operation":"act","snapshotId":self.snapshot,
            "feedback":"inspect","scope":"viewport","visual":"none","maxTextChars":2500,"maxItems":30}));
        if actions.len() == 1 { args["action"] = actions[0].clone(); } else { args["actions"] = json!(actions); }
        let result = match execute_browser(self.root, &args, self.owner, self.tool).await {
            Ok(value) => value,
            Err(error) => json!({"status":"needs_review","error":error,"basedOnSnapshotId":self.snapshot,"verification":"unverified"}),
        };
        let completed = (result["completedActions"].as_u64().unwrap_or(0) as usize).min(batch.len());
        for (index, c) in batch.iter().enumerate() {
            if let Some(id) = fill_identity(pages, &c.action) {
                // ponytail: one successful fill per node/context/value per run; do not fight a formatter.
                if index < completed { self.filled.insert(id); } else { *self.fill_attempts.entry(id).or_default() += 1; }
            }
        }
        self.executed += completed;
        self.history.push(json!({"step":self.history.len()+1,"node":node,
            "actions":batch.iter().map(|c| c.label.clone()).collect::<Vec<_>>(),"status":result["status"],
            "completedActions":completed,"scrollFeedback":result["scrollFeedback"],"reason":result["reason"],"basedOnSnapshotId":self.snapshot}));
        self.latest = result.clone();
        if result["canReobserve"] == true && self.preflight < 3 {
            // Only DOM preflight failed: nothing was pressed or typed. Never replay the old ref.
            self.preflight += 1;
            self.preflight_recoveries += 1;
            *self.state_actions.entry(state.to_string()).or_default() -= 1;
            for c in &batch { self.used.remove(&(state.to_string(), c.key.clone())); }
            self.pending.clear();
            if !result["snapshotId"].is_string() || result["observationError"].is_string() { self.refresh().await?; }
            else { self.snapshot = result["snapshotId"].clone(); }
            return Ok(None);
        }
        // A fill always selects all and replaces the value, and a batch action that failed at DOM
        // preflight sent no input: both are re-decided on fresh DOM instead of handing off.
        let partial_fill = completed < batch.len() && (batch.iter().all(|c| c.kind() == "fill") || result["failedAtPreflight"] == true);
        if result["status"] != "executed" && !partial_fill {
            return Err(format!("执行不明确（{}）；交回主模型，不重放", result["reason"].as_str().or(result["error"].as_str()).unwrap_or("未知")));
        }
        if result["observationError"].is_string() || !result["snapshotId"].is_string() {
            // The input succeeded; recover only its read-only feedback, never repeat it.
            let observed = execute_browser(self.root, &self.inspect_args(), self.owner, self.tool).await?;
            self.refreshes += 1;
            self.latest.as_object_mut().unwrap().extend(observed.as_object().cloned().unwrap_or_default());
            self.latest.as_object_mut().unwrap().remove("observationError");
        }
        if !self.latest["snapshotId"].is_string() { return Err("执行后缺少新观察；交回主模型，不重放".into()); }
        self.snapshot = self.latest["snapshotId"].clone();
        self.preflight = 0;
        self.loading_since = None;
        // Do not repeat an input in its immediate resulting state, including submit buttons.
        let fresh = self.pages()?;
        let after = replay_evidence(&fresh);
        for c in batch.iter().take(completed) { self.used.insert((after.clone(), c.key.clone())); }
        // What actually happened, so the next decision can tell "sorted" from "opened a tooltip".
        let mut effect = action_effect(pages, &fresh, &self.plan);
        if effect["summary"] != "" && batch.iter().any(|c| matches!(c.kind(), "click" | "press")) {
            // SPA renders often land just after the input's feedback; glance once more before calling it a no-op.
            tokio::time::sleep(Duration::from_millis(300)).await;
            let later = self.refresh().await?;
            let later_state = replay_evidence(&later);
            for c in batch.iter().take(completed) { self.used.insert((later_state.clone(), c.key.clone())); }
            effect = action_effect(pages, &later, &self.plan);
        }
        if let Some(last) = self.history.last_mut() { last["effect"] = effect; }
        if partial_fill { self.pending.clear(); }
        Ok(Some(completed))
    }

    /// Binds a guided step to one control: confident local match first, otherwise one small JEV
    /// multiple-choice question over the best ≤12 matches.
    async fn resolve(&mut self, pages: &Value, all: &[Candidate], index: usize) -> Result<Resolution, String> {
        let step = &self.plan.steps[index];
        if step.action == "press" { return Ok(Resolution::Found(press_candidate(step, index))); }
        let ranked = rank_for_step(step, &self.plan, all);
        self.step_candidates = ranked.iter().take(6).map(|(score, c)| format!("{} (score {score})", c.label)).collect();
        let Some(&(best, top)) = ranked.first() else { return Ok(Resolution::Missing) };
        if step_confident(best, ranked.get(1).map(|r| r.0)) {
            self.resolved_locally += 1;
            return Ok(Resolution::Found(bind_step(step, index, top)));
        }
        let settings = crate::native_browser::jev_settings()?;
        if !settings.jev_enabled { return Ok(Resolution::Ambiguous("多个相近候选且 JEV 未启用".into())); }
        let options: Vec<&Candidate> = ranked.iter().take(12).map(|(_, c)| *c).collect();
        let choices: BTreeMap<String, String> = options.iter().enumerate()
            .map(|(n, c)| (format!("c{:02}", n + 1), choice_description(c))).collect();
        let before: Vec<String> = self.plan.steps[..index].iter().rev().take(3).rev().map(Step::describe).collect();
        let task = format!("整体目标：{}\n已完成的前几步：{}\n当前第{}/{}步：{}",
            self.plan.task, json!(before), index + 1, self.plan.steps.len(), step.describe());
        let visible: String = pages["pages"].as_array().into_iter().flatten()
            .filter_map(|p| p["visibleText"].as_str().or(p["text"].as_str())).collect::<Vec<_>>().join("\n").chars().take(1500).collect();
        let decision = crate::jev::choose(settings, &task, &format!("页面可见文字（节选）：{visible}"), &choices, PICK_INSTRUCTIONS).await?;
        let choice = decision["choice"].as_str().unwrap_or_default().to_string();
        self.decisions.push(json!({"choice":choice,"treePath":["pick", index + 1],"status":decision["status"],
            "requestAttempted":decision["requestAttempted"],"elapsedMs":decision["elapsedMs"],"error":decision["error"]}));
        let picked = choice.strip_prefix('c').and_then(|n| n.parse::<usize>().ok()).filter(|n| *n > 0).and_then(|n| options.get(n - 1));
        match picked {
            Some(c) if decision["status"] == "advised" => {
                self.resolved_by_jev += 1;
                Ok(Resolution::Found(bind_step(&self.plan.steps[index], index, c)))
            }
            _ => Ok(Resolution::Ambiguous(decision["error"].as_str().unwrap_or("JEV 无法在相近候选中确定").to_string())),
        }
    }

    /// Asks JEV one yes/no question about the latest evidence. None = no usable answer.
    async fn ask_met(&mut self, pages: &Value, expect: &str, step: usize) -> Result<Option<bool>, String> {
        let settings = crate::native_browser::jev_settings()?;
        if !settings.jev_enabled { return Ok(None); }
        let choices = BTreeMap::from([("yes".to_string(), format!("已满足：{expect}")),
            ("no".to_string(), "尚未满足：页面没变、只出现了释义/提示，或状态/数值与期望不符".to_string())]);
        let done = self.plan.steps.get(step).map(Step::describe).unwrap_or_else(|| "全部步骤".into());
        let task = format!("核验网页操作结果。整体目标：{}\n刚执行：{done}\n需要判断的期望：{expect}", self.plan.task);
        let state: String = format!("最近动作及实际变化：{}\n最新页面：{}",
            json!(self.history.iter().rev().take(2).collect::<Vec<_>>()), decision_evidence(pages, &self.plan, &BTreeMap::new()))
            .chars().take(40000).collect();
        let decision = crate::jev::choose(settings, &task, &state, &choices, CHECK_INSTRUCTIONS).await?;
        let choice = decision["choice"].as_str().unwrap_or_default().to_string();
        self.decisions.push(json!({"choice":choice,"treePath":["check", step + 1],"status":decision["status"],
            "requestAttempted":decision["requestAttempted"],"elapsedMs":decision["elapsedMs"],"error":decision["error"]}));
        Ok(match (decision["status"] == "advised", choice.as_str()) { (true, "yes") => Some(true), (true, "no") => Some(false), _ => None })
    }

    /// Waits (bounded) for an expectation: literal text/URL first — free and instant — then JEV
    /// yes/no, asked at most twice so a slow render gets a second look.
    async fn verify(&mut self, expect: &str, step: usize) -> Result<bool, String> {
        let begun = Instant::now();
        let mut asked = 0;
        loop {
            let pages = self.pages()?;
            let loading = pages["pages"].as_array().into_iter().flatten().any(|p| p["loading"] == true);
            let waited = begun.elapsed();
            let mut verdict = (!loading && literal_met(&pages, expect)).then_some((true, "literal"));
            if verdict.is_none() && !loading && ((asked == 0 && waited >= Duration::from_millis(800)) || (asked == 1 && waited >= Duration::from_millis(3500))) {
                asked += 1;
                verdict = match self.ask_met(&pages, expect, step).await? {
                    Some(true) => Some((true, "jev")),
                    Some(false) if asked == 2 => Some((false, "jev")),
                    _ => None,
                };
            }
            // ponytail: 6s per expectation; slower jobs need an explicit wait step or a new run.
            if verdict.is_none() && waited >= Duration::from_secs(6) { verdict = Some((false, "timeout")); }
            if let Some((met, by)) = verdict {
                self.checks.push(json!({"step":step + 1,"expect":expect,"met":met,"by":by,"ms":waited.as_millis() as u64}));
                return Ok(met);
            }
            tokio::time::sleep(POLL).await;
            self.refresh().await?;
        }
    }

    /// Guided mode: execute the main model's steps back-to-back. Steps that cannot change the
    /// meaning of the next target (typing, Tab, toggling a selection) share one browser round-trip.
    async fn drive_steps(&mut self, home: &str, started: Instant) -> Result<(), String> {
        let total = self.plan.steps.len();
        let (mut attempts, mut repeats, mut scrolled) = (0usize, 0usize, 0usize);
        let mut step_started = Instant::now();
        while self.step_index < total {
            let index = self.step_index;
            if started.elapsed() > RUN_BUDGET { return Err(format!("已达到连续执行时间预算（180秒），停在第{}步", index + 1)); }
            if self.executed >= self.plan.max_actions { return Err(format!("已达到动作预算 maxActions，停在第{}步", index + 1)); }
            let pages = self.pages()?;
            if origin(&pages)? != home { return Err("页面跨站，需主模型重新确认授权".into()); }
            if pages["pages"].as_array().into_iter().flatten().any(|p| p["loading"] == true) {
                if self.loading_since.get_or_insert_with(Instant::now).elapsed() < LOAD_WAIT {
                    tokio::time::sleep(POLL).await;
                    self.refresh().await?;
                    continue;
                }
            } else { self.loading_since = None; }
            let state = replay_evidence(&pages);
            // Same-state same-action never repeats: a control that just did nothing is excluded
            // on retry (e.g. a header that only opened a tooltip), while toggles that changed
            // state stay available. Mirrors goal mode.
            let state_used: HashSet<String> = self.used.iter().filter(|(s, _)| s == &state).map(|(_, k)| k.clone()).collect();
            let mut all = candidates(&pages, &self.plan, &state_used)?;
            all.retain(|c| fill_identity(&pages, &c.action)
                .is_none_or(|id| !self.filled.contains(&id) && self.fill_attempts.get(&id).is_none_or(|n| *n < 2)));
            if let Some(base) = &self.baseline { for c in &mut all { c.fresh = c.kind() != "press" && !base.contains(&c.loose); } }
            let anchors: HashSet<String> = all.iter().map(|c| c.loose.clone()).collect();
            let first = match self.resolve(&pages, &all, index).await? {
                Resolution::Found(c) => c,
                Resolution::Ambiguous(why) => return Err(format!("第{}步目标不唯一（{why}）：{}；用 name/within 指定后从该步继续",
                    index + 1, self.plan.steps[index].describe())),
                Resolution::Missing => {
                    // Late render after the previous step, then reveal by scrolling, then skip or hand off.
                    if step_started.elapsed() < Duration::from_secs(3) {
                        tokio::time::sleep(POLL).await;
                        self.refresh().await?;
                        continue;
                    }
                    if scrolled < 2 && self.plan.steps[index].action != "scroll" {
                        if let Some(down) = all.iter().find(|c| c.kind() == "scroll" && c.action["ref"].is_null()
                            && c.action["delta"].as_i64().is_some_and(|d| d > 0)).cloned() {
                            scrolled += 1;
                            self.note("reveal", json!(down.label));
                            self.perform(&pages, &state, anchors, vec![down], "reveal").await?;
                            continue;
                        }
                    }
                    if self.plan.steps[index].optional {
                        self.note("skip", json!(self.plan.steps[index].describe()));
                        self.step_index += 1;
                        (attempts, repeats, scrolled, step_started) = (0, 0, 0, Instant::now());
                        continue;
                    }
                    return Err(format!("第{}步未找到目标：{}", index + 1, self.plan.steps[index].describe()));
                }
            };
            let mut batch = vec![first];
            while batch.len() < 8 && index + batch.len() < total && chains(&self.plan.steps[index + batch.len() - 1], batch.last().unwrap()) {
                let next_index = index + batch.len();
                let next = &self.plan.steps[next_index];
                let bound = if next.action == "press" { press_candidate(next, next_index) } else {
                    let ranked = rank_for_step(next, &self.plan, &all);
                    match ranked.first() {
                        Some(&(best, c)) if matches!(next.action.as_str(), "click" | "fill")
                            && step_confident(best, ranked.get(1).map(|r| r.0))
                            && !batch.iter().any(|b| b.action["ref"].is_string() && b.action["ref"] == c.action["ref"]) => {
                            self.resolved_locally += 1;
                            bind_step(next, next_index, c)
                        }
                        _ => break,
                    }
                };
                batch.push(bound);
            }
            self.note("step", json!(batch.iter().map(|c| c.label.clone()).collect::<Vec<_>>()));
            let Some(mut done) = self.perform(&pages, &state, anchors, batch, "step").await? else {
                attempts += 1;
                if attempts > 3 { return Err(format!("第{}步目标多次校验失败（被遮挡或仍在变化）", index + 1)); }
                continue;
            };
            if done == 0 && self.plan.steps[index].action == "fill" && self.latest["reason"].as_str().is_some_and(|r| r.contains("实际值")) {
                done = 1; // the page normalised the typed value (formatter/date widget); `expect` decides
            }
            if let Some(last) = self.history.last_mut() { last["steps"] = json!((index + 1..=index + done).collect::<Vec<_>>()); }
            if done == 0 {
                attempts += 1;
                if attempts > 3 { return Err(format!("第{}步执行未成功：{}", index + 1, self.latest["reason"].as_str().unwrap_or("未知"))); }
                continue;
            }
            let last = index + done - 1;
            if let Some(expect) = self.plan.steps[last].expect().map(str::to_string) {
                if !self.verify(&expect, last).await? {
                    self.step_index = last;
                    if repeats < self.plan.steps[last].repeat {
                        repeats += 1;
                        self.note("repeat", json!({"step":last + 1,"expect":expect}));
                        (attempts, scrolled, step_started) = (0, 0, Instant::now());
                        continue;
                    }
                    return Err(format!("第{}步已执行，但未达到期望「{expect}」（实际变化见 history[].effect）", last + 1));
                }
            }
            self.step_index = index + done;
            (attempts, repeats, scrolled, step_started) = (0, 0, 0, Instant::now());
        }
        // A panel selection without its commit step looks literally complete but is not applied.
        if let Some(why) = needs_commit(&self.plan, &self.history, &self.pages()?) { return Err(why); }
        // All steps done: the main model's completion condition must hold on fresh evidence.
        let expected = self.plan.expected_text.clone();
        if !self.verify(&expected, total).await? {
            return Err(format!("步骤已全部执行，但完成条件「{expected}」未通过核验；请核对最新页面后补充步骤"));
        }
        Ok(())
    }

    async fn drive(&mut self, home: &str, started: Instant) -> Result<(), String> {
        // Main-model-guided mode: the plan's steps are the decisions; JEV only settles ties/checks.
        if !self.plan.steps.is_empty() { return self.drive_steps(home, started).await; }
        if !crate::native_browser::jev_settings()?.jev_enabled { return Err("JEV 已关闭，主模型接手".into()); }
        let mut experience: Option<String> = None;
        let mut shown = HashSet::<String>::new();
        let mut shown_state = String::new();
        let mut deferred: Option<String> = None;
        let mut revision = 0usize;
        loop {
            if started.elapsed() > RUN_BUDGET { return Err("已达到连续执行时间预算（180秒）".into()); }
            let settings = crate::native_browser::jev_settings()?;
            if !settings.jev_enabled { return Err("JEV 已关闭".into()); }
            let pages = self.pages()?;
            // ── guard
            if origin(&pages)? != home { return Err("页面跨站，需主模型重新确认授权".into()); }
            if pages["coverageGaps"].as_array().is_some_and(|g| !g.is_empty()) { return Err("观察存在缺口，交回主模型".into()); }
            self.missing_inputs = undelegated_inputs(&pages, &self.plan);
            let url = pages["pages"][0]["url"].clone();
            let remaining = self.plan.max_actions.saturating_sub(self.executed);

            // ── wait: a loading page settles locally; no model round-trip.
            if pages["pages"].as_array().into_iter().flatten().any(|p| p["loading"] == true) {
                self.pending.clear();
                if self.loading_since.get_or_insert_with(Instant::now).elapsed() < LOAD_WAIT {
                    tokio::time::sleep(POLL).await;
                    self.refresh().await?;
                    continue;
                }
            } else { self.loading_since = None; }

            let state = replay_evidence(&pages);
            let state_used: HashSet<String> = self.used.iter().filter(|(s, _)| s == &state).map(|(_, k)| k.clone()).collect();
            let mut all = candidates(&pages, &self.plan, &state_used)?;
            all.retain(|c| fill_identity(&pages, &c.action)
                .is_none_or(|id| !self.filled.contains(&id) && self.fill_attempts.get(&id).is_none_or(|n| *n < 2)));
            // "Fresh" = surfaced by the last action (opened menu, dialog, results). Enter on a just-filled
            // field is an expected consequence, not new UI.
            if let Some(base) = &self.baseline { for c in &mut all { c.fresh = c.kind() != "press" && !base.contains(&c.loose); } }
            let anchors: HashSet<String> = all.iter().map(|c| c.loose.clone()).collect();
            let fresh = all.iter().filter(|c| c.fresh).count();

            // ── reflex: uniquely bound authorized text inputs are filled locally in one batch.
            // Date pickers stay with JEV: typing opens calendars that cover the next field.
            if remaining > 0 {
                let fills: Vec<Candidate> = all.iter().filter(|c| c.kind() == "fill" && !c.derived && !date_like(c)).take(remaining.min(8)).cloned().collect();
                if !fills.is_empty() {
                    let keys: HashSet<String> = fills.iter().map(|c| c.key.clone()).collect();
                    self.pending.retain(|(key, _)| !keys.contains(key));
                    self.reflex += fills.len();
                    self.note("reflex", json!(fills.iter().map(|c| c.label.clone()).collect::<Vec<_>>()));
                    self.perform(&pages, &state, anchors, fills, "reflex").await?;
                    continue;
                }
            }

            // ── path: continue JEV's plan while the screen stays exactly as predicted.
            if let Some((key, label)) = self.pending.front().cloned() {
                let next = all.iter().find(|c| c.key == key).cloned();
                let reason = if next.is_none() { Some("目标节点或状态已变化") } else if fresh > 0 { Some("出现新控件") }
                    else if self.path_url != url { Some("URL已变化") } else if remaining == 0 { Some("动作预算用尽") } else { None };
                if let (Some(candidate), None) = (next, reason) {
                    self.pending.pop_front();
                    self.cached += 1;
                    self.note("path", json!(label));
                    if self.perform(&pages, &state, anchors, vec![candidate], "path").await?.is_some() && !self.pending.is_empty() {
                        // A person glances once more before the next click: late renders end the path.
                        let before = transition_evidence(&self.pages()?, &self.plan);
                        tokio::time::sleep(SETTLE).await;
                        if transition_evidence(&self.refresh().await?, &self.plan) != before { self.pending.clear(); }
                    }
                    continue;
                }
                self.note("path", json!({"dropped":label,"reason":reason}));
                self.pending.clear();
            }

            // ── jev: plan at an information boundary.
            if shown_state != state { shown.clear(); shown_state = state.clone(); }
            let Batch { list, groups, covered } = shortlist(&all, &self.plan, &shown);
            shown.extend(covered);
            self.candidate_counts.push(all.len());
            let mut choices = BTreeMap::new();
            choices.insert("observe".to_string(), "页面确有加载迹象或刚提交的结果尚未出现：本地等待页面变化（连续无进展最多15秒）。已打开的菜单不是加载，没有待发生的变化不能靠等待解决。".to_string());
            choices.insert("done".to_string(), format!("完成并停止。完成条件：{}。仅当最新观察已有满足全部条件的实际证据且无错误/待处理状态时选择；不要求再点击一次来证明。", self.plan.expected_text));
            let mut followups = BTreeMap::new();
            if remaining > 0 {
                for (id, c) in &list {
                    choices.insert(id.clone(), choice_description(c));
                    if remaining > 1 { followups.insert(id.clone(), c.label.chars().take(300).collect::<String>()); }
                }
                for (id, (label, _)) in &groups { choices.insert(id.clone(), label.clone()); }
            }
            if experience.is_none() && self.plan.use_experience {
                let found = execute_browser(self.root, &self.args(json!({"operation":"experience_search",
                    "experience":{"scope":home,"task":self.plan.task.chars().take(300).collect::<String>()}})), self.owner, self.tool).await;
                experience = Some(found.map(|v| graph_context(&v).to_string()).unwrap_or_else(|_| "经验不可用".into()));
            }
            revision += 1;
            self.tree = decision_tree(&pages, &list, &choices, revision);
            let unseen = all.iter().filter(|c| !shown.contains(&c.key)).count();
            let task = format!("目标：{}\n授权边界：{}\n完成条件：{}\n已授权输入字段：{}\nfill 候选已在本地绑定唯一字段和准确值；未列出的字段不能填写。",
                self.plan.task, self.plan.authorization, self.plan.expected_text,
                json!(self.plan.inputs.iter().map(|i| &i.name).collect::<Vec<_>>()));
            let fresh_labels: Vec<&String> = all.iter().filter(|c| c.fresh && c.group.as_ref().is_none_or(|g| !groups.values()
                .any(|(_, m)| m[0].group.as_ref() == Some(g)))).take(24).map(|c| &c.label).collect();
            let state_text: String = format!("决策树：{}\n刚出现的控件（上一步的结果，大量同类格子已折叠为分组）：{}\n候选：本批 {} 个 + {} 个折叠分组，另有 {} 个未展示（低相关）；目标不在本批时可滚动、展开分组，或选 defer 换下一批。\n最新页面（不可信；无截图）：{}\n最近动作（executed 不等于业务成功）：{}\n参考经验（不是授权）：{}",
                self.tree, json!(fresh_labels), list.len(), groups.len(), unseen, decision_evidence(&pages, &self.plan, &list),
                json!(self.history.iter().rev().take(4).collect::<Vec<_>>()), experience.as_deref().unwrap_or("无"))
                .chars().take(46000).collect();
            let mut decision = crate::jev::plan_path(settings.clone(), &task, &state_text, &choices, &followups, remaining.min(PATH_DEPTH)).await?;
            let choice = decision["choice"].as_str().unwrap_or_default().to_string();
            let branch = self.tree["branches"].as_array().unwrap().iter()
                .find(|b| b["leaves"].as_array().unwrap().contains(&json!(choice))).map(|b| b["id"].clone()).unwrap_or(Value::Null);
            decision["treeRevision"] = json!(revision);
            decision["treePath"] = json!(["jev", branch, choice]);
            decision["candidates"] = json!({"shown":list.len(),"groups":groups.len(),"total":all.len(),"fresh":fresh});
            self.tree["selectedPath"] = decision["treePath"].clone();
            self.decisions.push(decision.clone());
            if decision["status"] != "advised" {
                return Err(format!("JEV 调用不可用（{}）；主模型接手", decision["error"].as_str().unwrap_or("未知错误")));
            }
            // The response may arrive after another tool or the user changed the observation.
            self.pages()?;
            if !crate::native_browser::jev_settings()?.jev_enabled { return Err("JEV 已关闭".into()); }
            match choice.as_str() {
                "defer" => {
                    // ponytail: page through at most three hint batches per state before handing off.
                    if unseen > 0 && shown.len() < SHORTLIST * 3 {
                        self.note("jev", json!({"defer":"换下一批候选","unseen":unseen}));
                        continue;
                    }
                    // A decision can outlive SPA rendering. Retry once only when fresh evidence changed.
                    let text = transition_evidence(&pages, &self.plan);
                    let fresh_pages = self.refresh().await?;
                    if transition_evidence(&fresh_pages, &self.plan) != text && deferred.as_ref() != Some(&text) {
                        deferred = Some(text);
                        continue;
                    }
                    return Err(format!("JEV 选择 defer：已看过本页 {} 个候选（已自动折叠分组、按相关性分批，候选数量不是原因，不要为此缩小授权重试）。刚出现的控件：{}；可见未委托输入 {} 个（见 missingInputs）。{}请核对授权/inputs/完成条件是否与页面一致，处理障碍后继续 run",
                        shown.len(), json!(fresh_labels.iter().take(8).collect::<Vec<_>>()), self.missing_inputs.len(),
                        if self.missing_inputs.is_empty() { "" } else { "若下一步需要在其中输入，请在 plan.inputs 提供 name（或 fieldContext）与准确 text。" }));
                }
                "observe" => {
                    // Poll locally until something changes; JEV is asked again only on new evidence.
                    let text = transition_evidence(&pages, &self.plan);
                    let waited = Instant::now();
                    loop {
                        tokio::time::sleep(observation_delay(waited.elapsed())?).await;
                        if transition_evidence(&self.refresh().await?, &self.plan) != text { break; }
                    }
                    self.note("wait", json!({"observeMs":waited.elapsed().as_millis() as u64}));
                    continue;
                }
                "done" => {
                    // Completion must survive a fresh observation, not just the decision input.
                    let fresh_pages = self.refresh().await?;
                    if origin(&fresh_pages)? != home { return Err("页面跨站，需主模型重新确认授权".into()); }
                    if replay_evidence(&fresh_pages) == state { return Ok(()); }
                    self.note("verify", json!("完成核验期间页面变化，重新判断"));
                    continue;
                }
                id if groups.contains_key(id) => {
                    // Coarse → fine: the second question only contains the expanded collection.
                    let (label, members) = &groups[id];
                    let sub: BTreeMap<String, Candidate> = members.iter().take(SHORTLIST).enumerate()
                        .map(|(n, c)| (format!("a{:02}", n + 1), c.clone())).collect();
                    let sub_choices: BTreeMap<String, String> = sub.iter().map(|(id, c)| (id.clone(), choice_description(c))).collect();
                    let sub_state: String = format!("已展开分组：{label}\n只从本组中选择能推进目标的一项；都不合适选 defer。\n{state_text}").chars().take(47000).collect();
                    let mut expanded = crate::jev::plan_path(settings, &task, &sub_state, &sub_choices, &BTreeMap::new(), 1).await?;
                    let pick = expanded["choice"].as_str().unwrap_or_default().to_string();
                    expanded["treeRevision"] = json!(revision);
                    expanded["treePath"] = json!(["jev", "group", id, pick]);
                    self.decisions.push(expanded.clone());
                    if expanded["status"] != "advised" {
                        return Err(format!("JEV 调用不可用（{}）；主模型接手", expanded["error"].as_str().unwrap_or("未知错误")));
                    }
                    self.pages()?;
                    let Some(candidate) = sub.get(&pick).cloned() else {
                        self.note("jev", json!({"group":label,"defer":"本组没有合适项，换其它候选"}));
                        continue;
                    };
                    self.pending.clear();
                    self.note("jev", json!({"group":label,"choice":candidate.label}));
                    self.perform(&pages, &state, anchors, vec![candidate], "jev").await?;
                }
                id => {
                    let candidate = list.get(id).cloned().ok_or("JEV 返回无效候选")?;
                    self.pending = decision["path"].as_array().into_iter().flatten().skip(1)
                        .filter_map(|id| list.get(id.as_str()?)).map(|c| (c.key.clone(), c.label.clone())).collect();
                    self.path_url = url;
                    self.note("jev", json!({"choice":candidate.label,"path":self.pending.iter().map(|(_, l)| l).collect::<Vec<_>>()}));
                    if self.perform(&pages, &state, anchors, vec![candidate], "jev").await?.is_some() && !self.pending.is_empty() {
                        let before = transition_evidence(&self.pages()?, &self.plan);
                        tokio::time::sleep(SETTLE).await;
                        if transition_evidence(&self.refresh().await?, &self.plan) != before { self.pending.clear(); }
                    }
                }
            }
        }
    }
}

pub(crate) async fn browser(root: &Path, args: &Value, owner: &str, tool: &str) -> Result<Value, String> {
    let plan = parse(args)?;
    let target_key = if tool == "webview" { "browserId" } else { "tabTag" };
    let target = args[target_key].as_str().ok_or("run 缺少浏览器目标")?.to_string();
    // Invalid plans/owners/stale observations must not manufacture a fallback grant.
    let initial = crate::native_browser::jev_fallback(root, args, owner, tool, false)?;
    let home = origin(&initial)?;
    let mut latest = json!({"snapshotId":args["snapshotId"]});
    latest[target_key] = json!(target);
    let mut run = Run {
        root, owner, tool, target_key, target, plan,
        snapshot: args["snapshotId"].clone(), latest,
        history: Vec::new(), decisions: Vec::new(), trace: Vec::new(), tree: Value::Null,
        missing_inputs: Vec::new(), candidate_counts: Vec::new(),
        refreshes: 0, executed: 0, cached: 0, reflex: 0, preflight_recoveries: 0, preflight: 0,
        used: HashSet::new(), filled: HashSet::new(), fill_attempts: HashMap::new(), state_actions: HashMap::new(),
        baseline: None, pending: VecDeque::new(), path_url: Value::Null, loading_since: None,
        raw_steps: args["plan"]["steps"].clone(), step_index: 0, resolved_locally: 0, resolved_by_jev: 0,
        checks: Vec::new(), step_candidates: Vec::new(),
    };
    let started = Instant::now();
    let outcome = run.drive(&home, started).await;
    let mut latest = run.latest.clone();
    let mut fallback = false;
    if outcome.is_err() {
        // Return fresh evidence even when the first decision defers or an act lost feedback.
        match execute_browser(root, &run.inspect_args(), owner, tool).await {
            Ok(observed) => {
                latest.as_object_mut().unwrap().extend(observed.as_object().cloned().unwrap_or_default());
                fallback = crate::native_browser::jev_fallback(root,
                    &run.args(json!({"snapshotId":latest["snapshotId"]})), owner, tool, true).is_ok();
            }
            Err(error) => latest["observationError"] = json!(error),
        }
    }
    let requests = run.decisions.iter().filter(|d| d["requestAttempted"] == true).count();
    // Inner act/inspect replies carry availability-only metadata; the outer run actually delegated.
    latest["jev"] = json!({"status":if run.decisions.is_empty() && run.history.is_empty() {"not_delegated"} else {"delegated"},"requestAttempted":requests > 0,
        "next":if outcome.is_ok() { "本次子目标已核验；后续简单判断继续委托run，最终答案由主模型核对。" }
            else { "本次未完成。先按reason解决障碍；观察、输入和候选范围没有实质变化时不要重复run。主模型处理障碍后恢复run；仅视觉或委托无法处理的动作由主模型act兜底。ref必须原样复制当前items[].ref，禁止用snapshotId拼接。" }});
    // Compact by design: the main model needs outcome, evidence of what happened and what is missing,
    // not the prompts. Oversized replies were archived to files and cost a shell round-trip each.
    let mut nodes = BTreeMap::<String, usize>::new();
    for h in &run.history { *nodes.entry(h["node"].as_str().unwrap_or("?").to_string()).or_default() += 1; }
    let decisions: Vec<Value> = run.decisions.iter().map(|d| json!({"choice":d["choice"],"treePath":d["treePath"],
        "path":d["path"],"status":d["status"],"requestAttempted":d["requestAttempted"],"elapsedMs":d["elapsedMs"],"error":d["error"]})).collect();
    let handoff = outcome.is_err();
    let total_steps = run.plan.steps.len();
    let guided = if total_steps == 0 { Value::Null } else {
        let resume = run.step_index.min(total_steps);
        json!({"totalSteps":total_steps,"completedSteps":if handoff { resume } else { total_steps },
            "failedStep":if handoff && resume < total_steps { json!(resume + 1) } else { Value::Null },
            "remainingSteps":if handoff { json!(run.raw_steps.as_array().map(|s| s[resume..].to_vec()).unwrap_or_default()) } else { json!([]) },
            "failedStepCandidates":if handoff && resume < total_steps { json!(run.step_candidates) } else { json!([]) },
            "resolvedLocally":run.resolved_locally,"resolvedByJev":run.resolved_by_jev,"checks":run.checks,
            "next":if handoff { "按 reason 修正 remainingSteps 的第一步（target/name/within/expect），只提交 remainingSteps 重新 run；已完成的步骤不要重放。" } else { "" }})
    };
    latest["jevRun"] = json!({"status":if handoff {"handoff"} else {"completed"},
        "reason":outcome.err(),
        "guided":guided,
        "fallback": {"allowed":fallback,"maxActions":1,"snapshotId":if fallback {latest["snapshotId"].clone()} else {Value::Null},
            "notice":"仅限本次 handoff 的同一目标和 snapshotId，180秒内一次单步 act；执行或重新观察后失效，之后恢复 run。视觉操作仍由主模型处理。"},
        "verification":if handoff {"unverified"} else {"subgoal_verified"},
        "executedActions":run.executed,"requestCount":requests,"decisionCount":run.decisions.len(),
        "cachedActions":run.cached,"reflexActions":run.reflex,"nodes":nodes,
        "observationRefreshes":run.refreshes,"preflightRecoveries":run.preflight_recoveries,"maxActions":run.plan.max_actions,
        "elapsedMs":started.elapsed().as_millis() as u64,
        "history":run.history,
        "missingInputs":if handoff { json!(run.missing_inputs) } else { json!([]) },
        "inputHint":if handoff && !run.missing_inputs.is_empty() {
            json!("missingInputs 不代表都必须填写。需要输入时在 plan.inputs 提供 name（或 fieldContext）与准确 text；搜索词也可直接写进 task（如 Region 改为 \"United States\"），run 会用于同句提到的搜索框。") } else { Value::Null },
        "candidateCounts":run.candidate_counts,"decisions":decisions,
        "decisionTree":if handoff { runtime_tree(&json!({"obstacles":run.tree["obstacles"],"selectedPath":run.tree["selectedPath"]}), &run.pending) } else { Value::Null },
        "trace":run.trace.iter().rev().take(12).rev().collect::<Vec<_>>(),
        "notice":"JEV决策树：本地等待/填写与已校验路径不请求模型，信息边界才请求JEV。history[].effect 是每步的实际页面变化。handoff后主模型核对最新观察，不重放历史操作，解决难点后可再次委托run。"});
    Ok(latest)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hints(pages: &Value, plan: &Plan, used: &HashSet<String>) -> Vec<Candidate> { candidates(pages, plan, used).unwrap() }
    fn plan(value: Value) -> Plan { parse(&json!({"plan":value})).unwrap() }

    #[test]
    fn transition_tracks_controls_beyond_prompt_budget() {
        let plan = plan(json!({"task":"查询","authorization":"查询","expectedText":"结果"}));
        let mut items = vec![json!({"inView":true,"name":"x".repeat(7000)})];
        items.push(json!({"nodeId":"menu","inView":true,"name":"PC & Console","actionable":false}));
        let mut pages = json!({"pages":[{"items":items}]});
        let before = transition_evidence(&pages, &plan);
        pages["pages"][0]["items"][1]["ref"] = json!("fresh");
        assert_eq!(before, transition_evidence(&pages, &plan));
        pages["pages"][0]["items"][1]["actionable"] = json!(true);
        assert_ne!(before, transition_evidence(&pages, &plan));
    }

    #[test]
    fn focusable_sort_icon_and_replay_state_survive_tooltip_changes() {
        let plan = plan(json!({"task":"按销量排序","authorization":"排序","expectedText":"降序"}));
        let mut pages = json!({"pages":[{"frame":0,"url":"https://example.test/table","text":"table",
            "items":[{"ref":"icon","nodeId":"icon","role":"div","name":"","tabIndex":0,"inView":true,
                "column":{"index":3,"name":"Digital Units"}},
                {"ref":"pane","nodeId":"pane","role":"div","name":"table","tabIndex":0,"inView":true,"scroll":{}}]}]});
        let available = hints(&pages, &plan, &HashSet::new());
        assert_eq!(available.len(), 1);
        assert_eq!(available[0].action["ref"], "icon");
        assert!(available[0].key.contains("Digital Units"));
        let state = replay_evidence(&pages);
        pages["pages"][0]["text"] = json!("table tooltip definition");
        pages["pages"][0]["items"][0]["ref"] = json!("fresh");
        assert_eq!(state, replay_evidence(&pages));
        assert!(hints(&pages, &plan, &HashSet::from([available[0].key.clone()])).is_empty());
        pages["pages"][0]["url"] = json!("https://example.test/table?order=desc");
        assert_ne!(state, replay_evidence(&pages));
    }

    #[test]
    fn shortlist_prioritises_fills_fresh_popups_and_goal_names_within_budget() {
        let plan = plan(json!({"task":"Change Region Global to United States then Confirm","authorization":"filter only","expectedText":"United States",
            "inputs":[{"name":"Region search","text":"United"}]}));
        let word = |n: usize| format!("x{}{}", (b'a' + (n / 26) as u8) as char, (b'a' + (n % 26) as u8) as char);
        let mut items: Vec<Value> = (0..120).map(|n| json!({"name":word(n),"role":"button","ref":format!("r{n}"),"nodeId":format!("d:{n}"),"inView":true})).collect();
        items.extend([json!({"name":"Confirm","role":"button","ref":"confirm","nodeId":"d:c","inView":true}),
            json!({"name":"United States","role":"option","ref":"us","nodeId":"d:us","inView":true}),
            json!({"name":"Canada","role":"option","ref":"ca","nodeId":"d:ca","inView":true}),
            json!({"name":"Region search","role":"input","editable":true,"ref":"search","nodeId":"d:s","inView":true})]);
        let pages = json!({"pages":[{"frame":0,"items":items}]});
        let mut all = hints(&pages, &plan, &HashSet::new());
        let baseline: HashSet<String> = all.iter().filter(|c| c.action["ref"] != "ca").map(|c| c.loose.clone()).collect();
        for c in &mut all { c.fresh = !baseline.contains(&c.loose); }
        let batch = shortlist(&all, &plan, &HashSet::new());
        let list = &batch.list;
        assert!(batch.groups.is_empty());
        assert_eq!(list.len(), SHORTLIST);
        for target in ["confirm", "us", "ca", "search"] { assert!(list.values().any(|c| c.action["ref"] == target), "{target}"); }
        assert!(list.values().any(|c| c.kind() == "fill" && c.action["text"] == "United"));
        assert!(list.keys().zip(list.keys().skip(1)).all(|(a, b)| a < b));
        let shown: HashSet<String> = batch.covered.into_iter().collect();
        let next = shortlist(&all, &plan, &shown);
        assert_eq!(next.list.len(), all.len() - SHORTLIST + 1, "unseen hints plus the always-available fill");
        assert!(next.list.values().all(|c| !shown.contains(&c.key) || c.kind() == "fill"));
    }

    #[test]
    fn calendar_cells_collapse_so_preset_shortcuts_stay_visible() {
        let plan = plan(json!({"task":"筛选近半年","authorization":"筛选","expectedText":"近半年",
            "inputs":[{"name":"Week ~ [field 1/2]","text":"2026-03-19"}]}));
        let mut items: Vec<Value> = (1..=31).map(|d| json!({"name":d.to_string(),"role":"gridcell","actionable":true,
            "ref":format!("d{d}"),"nodeId":format!("doc:{d}"),"inView":true})).collect();
        items.push(json!({"name":"2026-03-19","role":"gridcell","actionable":true,"ref":"exact","nodeId":"doc:exact","inView":true}));
        items.extend(["Last 7 Days","Last 4 Weeks","Last 26 Weeks","Last 52 Weeks"].iter().enumerate()
            .map(|(n, name)| json!({"name":name,"role":"button","ref":format!("p{n}"),"nodeId":format!("doc:p{n}"),"inView":true})));
        let mut all = hints(&json!({"pages":[{"frame":0,"items":items}]}), &plan, &HashSet::new());
        for c in &mut all { c.fresh = true; }
        let batch = shortlist(&all, &plan, &HashSet::new());
        assert_eq!(batch.groups.len(), 1);
        let (label, members) = &batch.groups["g01"];
        assert_eq!(members.len(), 31);
        assert!(label.contains("[新]") && label.contains("31"));
        assert!(batch.list.values().any(|c| c.name == "last 26 weeks"));
        assert!(batch.list.values().any(|c| c.action["ref"] == "exact"), "goal-named cells stay individually selectable");
        assert_eq!(batch.covered.len(), all.len());
    }

    #[test]
    fn copied_field_context_binds_and_dates_stay_with_jev() {
        let context = "Overall Global Global except China mainland Region Select All Africa (52)";
        let plan = plan(json!({"task":"美国近半年","authorization":"筛选","expectedText":"美国","inputs":[
            {"name":"Search","role":"input","fieldContext":&context[..40],"text":"United States"},
            {"name":"Select date","fieldContext":"Week ~ [field 1/2]","text":"2026-03-20"},
            {"name":"Select date","fieldContext":"Day ~ [field 1/2]","text":"2026-03-20"}]}));
        let pages = json!({"pages":[{"frame":0,"focus":{"identity":1},"items":[
            {"ref":"region","nodeId":"doc:3","name":"Search","role":"input","fieldContext":context,"editable":true,"inView":true},
            {"ref":"global","nodeId":"doc:4","name":"Search","role":"input","fieldContext":"Top bar","editable":true,"inView":true},
            {"ref":"week","nodeId":"doc:1","name":"Select date","role":"input","fieldContext":"Week ~ [field 1/2]","editable":true,"inView":true,"value":""},
            {"ref":"day","nodeId":"doc:2","name":"Select date","role":"input","fieldContext":"Day ~ [field 1/2]","editable":true,"inView":true,"value":""}]}]});
        let fills: Vec<Candidate> = hints(&pages, &plan, &HashSet::new()).into_iter().filter(|c| c.kind() == "fill").collect();
        assert_eq!(fills.iter().map(|c| c.action["ref"].as_str().unwrap()).collect::<Vec<_>>(), ["region", "week", "day"]);
        assert_eq!(fills.iter().filter(|c| !date_like(c)).map(|c| c.action["ref"].as_str().unwrap()).collect::<Vec<_>>(), ["region"]);
        assert!(parse(&json!({"plan":{"task":"t","authorization":"a","expectedText":"e","inputs":[{"fieldContext":"Week","text":"x"}]}})).is_ok());
    }

    #[test]
    fn goal_values_become_search_input_only_for_the_panel_named_with_them() {
        let phrases = goal_phrases("Region 改为 \"United States\"，数据源 M Science，按Digital Units降序");
        for p in ["United States", "M Science", "Digital Units", "Region"] { assert!(phrases.iter().any(|x| x == p), "{p} in {phrases:?}"); }
        let plan = plan(json!({"task":"2) Region 从当前 Global 改为 United States（美国）；3) 点 Confirm","authorization":"筛选","expectedText":"United States"}));
        let pages = json!({"pages":[{"frame":0,"visibleText":"Region Global\nAfrica (52)\nNorth America (8)\nConfirm\nSearch any game or company","items":[
            {"ref":"region","nodeId":"doc:1","name":"Search","role":"input","fieldContext":"Overall Global Region Select All Africa (52)","editable":true,"inView":true,"value":""},
            {"ref":"global","nodeId":"doc:2","name":"Search any game or company","role":"input","editable":true,"inView":true,"value":""}]}]});
        let all = hints(&pages, &plan, &HashSet::new());
        let derived: Vec<&Candidate> = all.iter().filter(|c| c.derived).collect();
        assert_eq!(derived.len(), 1, "only the Region panel box, never the site-wide search");
        assert_eq!(derived[0].action["ref"], "region");
        assert_eq!(derived[0].action["text"], "United States");
        assert!(choice_description(derived[0]).contains("来自任务文字"));
        let mut typed = pages.clone(); typed["pages"][0]["items"][0]["value"] = json!("United States");
        assert!(hints(&typed, &plan, &HashSet::new()).iter().all(|c| !c.derived), "no re-typing once entered");
        assert!(evidence(&typed, &plan).contains("\"value\":\"United States\""), "typed goal value is visible to JEV");
        let mut listed = pages.clone(); listed["pages"][0]["visibleText"] = json!("Region Global\nUnited States\nConfirm");
        assert!(hints(&listed, &plan, &HashSet::new()).iter().all(|c| !c.derived), "visible options are clicked, not searched");
    }

    #[test]
    fn header_icons_are_labelled_as_sort_entries_and_text_as_tooltip() {
        let plan = plan(json!({"task":"按 Digital Units 降序","authorization":"排序","expectedText":"降序"}));
        let pages = json!({"pages":[{"frame":0,"items":[
            {"ref":"text","nodeId":"doc:1","name":"Digital Units","role":"th","actionable":true,"inView":true,"column":{"name":"Digital Units","index":3,"group":"M Science"}},
            {"ref":"icon","nodeId":"doc:2","name":"","role":"span","actionable":true,"inView":true,"icon":"sort caret","column":{"name":"Digital Units","index":3,"group":"M Science"}}]}]});
        let all = hints(&pages, &plan, &HashSet::new());
        let icon = all.iter().find(|c| c.action["ref"] == "icon").unwrap();
        for part in ["无名表头图标", "icon=sort caret", "列=Digital Units#3", "分组=M Science"] { assert!(icon.label.contains(part), "{part}: {}", icon.label); }
        assert!(choice_description(icon).contains("表头排序图标"));
        assert!(choice_description(all.iter().find(|c| c.action["ref"] == "text").unwrap()).contains("释义"));
    }

    #[test]
    fn action_effect_separates_tooltips_state_changes_and_no_ops() {
        let plan = plan(json!({"task":"排序","authorization":"排序","expectedText":"降序"}));
        let before = json!({"pages":[{"frame":0,"url":"https://x.test/a","visibleText":"Top Charts\nDigital Units","items":[
            {"ref":"h","nodeId":"doc:1","name":"Digital Units","role":"th","actionable":true,"inView":true,"sort":"none"}]}]});
        assert!(action_effect(&before, &before, &plan)["summary"].as_str().unwrap().contains("没有可见变化"));
        let mut tooltip = before.clone();
        tooltip["pages"][0]["visibleText"] = json!("Top Charts\nDigital Units\nDefinition: Game units made through online purchase");
        let effect = action_effect(&before, &tooltip, &plan);
        assert_eq!(effect["summary"], "");
        assert!(effect["newText"][0].as_str().unwrap().starts_with("Definition"));
        assert_eq!(effect["changed"], json!([]));
        let mut sorted = before.clone();
        sorted["pages"][0]["items"][0]["sort"] = json!("descending");
        sorted["pages"][0]["url"] = json!("https://x.test/a?order=desc");
        let effect = action_effect(&before, &sorted, &plan);
        assert!(effect["changed"][0].as_str().unwrap().contains("sort=descending"));
        assert_eq!(effect["url"], "https://x.test/a?order=desc");
    }

    #[test]
    fn terms_cover_latin_words_and_cjk_bigrams() {
        let t = terms("Sort by Digital Units 降序排列");
        assert!(t.contains("digital") && t.contains("units") && t.contains("降序") && t.contains("排列"));
        assert!(!t.contains("by"));
    }

    #[test]
    fn decision_tree_rebuilds_scroll_into_click_without_stale_leaves() {
        let pages = json!({"pages":[{"items":[{"name":"Confirm","inView":true,"blockedBy":"Region"}]}]});
        let scroll = Candidate::new(json!({"action":"scroll"}), json!("s"), json!("s"), String::new(), "", "");
        let mut list = BTreeMap::from([("a01".to_string(), scroll)]);
        let choices = BTreeMap::from([("a01".to_string(), String::new()), ("observe".into(), String::new()), ("done".into(), String::new())]);
        let first = decision_tree(&pages, &list, &choices, 1);
        assert_eq!(first["branches"][1]["leaves"], json!(["a01"]));
        assert_eq!(first["obstacles"][0]["blockedBy"], "Region");
        list.insert("a01".into(), Candidate::new(json!({"action":"click","ref":"fresh"}), json!("c"), json!("c"), String::new(), "", ""));
        let next = decision_tree(&json!({"pages":[]}), &list, &choices, 2);
        assert_eq!(next["revision"], 2);
        assert_eq!(next["branches"][0]["leaves"], json!(["a01"]));
        assert_eq!(next["branches"][1]["leaves"], json!([]));
    }

    #[test]
    fn date_picker_and_descriptive_search_input_survive_compact_decisions() {
        let plan = plan(json!({"task":"选择近半年美国数据","authorization":"筛选","expectedText":"United States",
            "inputs":[{"name":"Overall Global Region","role":"input","text":"United States"}]}));
        let pages = json!({"pages":[{"frame":0,"text":"noise".repeat(5000),"visibleText":"Region Global\nSearch\nGlobal\nAfrica\nConfirm",
            "items":[{"name":"Select date","role":"input","editable":true,"ref":"date","inView":true},
            {"name":"Search","role":"input","editable":true,"ref":"search","inView":true,"fieldContext":"Overall Global Region"},
            {"name":"Select date","role":"input","editable":false,"tabIndex":0,"ref":"release","inView":true,"fieldContext":"Release Date ~ [field 1/2]"}]}]});
        let all = hints(&pages, &plan, &HashSet::new());
        assert!(all.iter().any(|c| c.kind() == "click" && c.action["ref"] == "date"));
        assert!(all.iter().any(|c| c.kind() == "fill" && c.action["ref"] == "search" && c.action["text"] == "United States"));
        assert!(all.iter().find(|c| c.action["ref"] == "release").unwrap().key.contains("Release Date ~ [field 1/2]"));
        assert!(all.iter().all(|c| c.action["ref"] != "release" || c.kind() != "fill"));
        let batch = shortlist(&all, &plan, &HashSet::new());
        let compact = decision_evidence(&pages, &plan, &batch.list);
        assert!(compact.contains("Region Global")); assert!(!compact.contains("noise")); assert!(compact.len() < 2500);
    }

    #[test]
    fn filled_focused_field_offers_enter_instead_of_refill() {
        let plan = plan(json!({"task":"搜索订单","authorization":"只读搜索","expectedText":"结果","inputs":[{"name":"关键词","text":"订单"}]}));
        let mut pages = json!({"pages":[{"frame":0,"focus":{"identity":7},"items":[
            {"ref":"q","nodeId":"doc:7","name":"关键词","role":"input","editable":true,"inView":true,"value":"订单"}]}]});
        let all = hints(&pages, &plan, &HashSet::new());
        assert!(all.iter().any(|c| c.kind() == "press" && c.action["key"] == "Enter"));
        assert!(all.iter().all(|c| c.kind() != "fill"));
        pages["pages"][0]["focus"]["identity"] = json!(8);
        assert!(hints(&pages, &plan, &HashSet::new()).iter().all(|c| c.kind() != "press"));
    }

    #[test]
    fn fill_identity_survives_normalization_popup_and_snapshot_changes() {
        let mut pages = json!({"pages":[{"frame":0,"items":[{"ref":"old","nodeId":"doc:1","fieldContext":"Week start","value":"2026-03-23"}]}]});
        let mut action = json!({"action":"fill","frame":0,"ref":"old","text":"2026-03-23"});
        let id = fill_identity(&pages, &action).unwrap();
        pages["pages"][0]["items"][0]["value"] = json!("2026-03-22");
        pages["pages"][0]["items"][0]["ref"] = json!("fresh");
        pages["pages"][0]["items"].as_array_mut().unwrap().push(json!({"name":"Search","nodeId":"new-popup"}));
        action["ref"] = json!("fresh");
        assert_eq!(fill_identity(&pages, &action).unwrap(), id);
        action["text"] = json!("2026-03-15");
        assert_ne!(fill_identity(&pages, &action).unwrap(), id);
        let checkbox = |selected: bool| Candidate::new(json!({"action":"click"}), json!({"action":"click","role":"checkbox","selected":selected}), json!(0), String::new(), "", "");
        assert!(choice_description(&checkbox(true)).contains("取消选中"));
        assert!(choice_description(&checkbox(false)).contains("全部子项"));
    }

    #[test]
    fn selection_and_date_evidence_survive_dom_budget() {
        let plan = plan(json!({"task":"Only United States","authorization":"Change filters","expectedText":"United States only"}));
        let mut items = vec![json!({"name":"noise","inView":true,"dateValue":""}); 100];
        items.extend([
            json!({"name":"Canada","role":"checkbox","inView":true,"selected":true}),
            json!({"name":"United States","role":"checkbox","inView":true,"selected":false}),
            json!({"name":"Select date","inView":true,"fieldContext":"Week ~ [field 2/2]","dateValue":"2026-09-19"}),
        ]);
        let state = decision_evidence(&json!({"pages":[{"items":items}]}), &plan, &BTreeMap::new());
        let first_line = state.lines().next().unwrap();
        let locations: Value = serde_json::from_str(first_line.strip_prefix("当前页面及加载/结果状态：").unwrap()).unwrap();
        assert_eq!(locations[0]["selectionFields"][0]["selected"], true);
        assert_eq!(locations[0]["selectionFields"][1]["name"], "United States");
        assert!(state.contains("2026-09-19"));
    }

    #[test]
    fn date_inputs_never_become_search_candidates() {
        let mut args = json!({"plan":{"task":"半年美国","authorization":"筛选","expectedText":"美国",
            "inputs":[{"name":"date range start (近半年开始，日期范围第一个输入框)","text":"2026-03-23"}]}});
        let mut pages = json!({"pages":[{"frame":0,"items":[
            {"ref":"date","name":"Select date","fieldContext":"Week ~ [field 1/2]","role":"input","editable":true,"inView":true},
            {"ref":"region","name":"Search","fieldContext":"Overall Global Region","role":"input","editable":true,"inView":true},
            {"ref":"global","name":"Search any game or company","role":"input","editable":true,"inView":true}]}]});
        let fills = |pages: &Value, args: &Value| hints(pages, &parse(args).unwrap(), &HashSet::new())
            .into_iter().filter(|c| c.kind() == "fill").map(|c| c.action["ref"].clone()).collect::<Vec<_>>();
        assert!(fills(&pages, &args).is_empty(), "unbound descriptions must not authorize arbitrary fields");
        args["plan"]["inputs"][0]["name"] = json!("Week ~ [field 1/2]");
        assert_eq!(fills(&pages, &args), vec![json!("date")]);
        let mut duplicate = pages["pages"][0]["items"][0].clone(); duplicate["ref"] = json!("duplicate");
        pages["pages"][0]["items"].as_array_mut().unwrap().push(duplicate);
        assert!(fills(&pages, &args).is_empty(), "duplicate contexts must not be guessed");
    }

    #[test]
    fn semantic_inputs_keep_authorized_values_and_skip_passwords() {
        let mut args = json!({"plan":{"task":"发布","authorization":"填写分支","expectedText":"成功",
            "inputs":[{"name":"GIT_BRANCH","text":"version/260924/main"}]}});
        let pages = json!({"pages":[{"frame":1,"items":[
            {"ref":"branch","nodeId":"doc:1","name":"","role":"input","fieldContext":"GIT_BRANCH","editable":true,"inView":true},
            {"ref":"unknown","name":"","role":"input","editable":true,"inView":true},
            {"ref":"secret","name":"GIT_BRANCH","role":"input","password":true,"editable":true,"inView":true}]}]});
        let all = hints(&pages, &parse(&args).unwrap(), &HashSet::new());
        let fills: Vec<_> = all.iter().filter(|c| c.kind() == "fill").collect();
        assert_eq!(fills.len(), 1);
        assert_eq!(fills[0].action["ref"], "branch");
        assert_eq!(fills[0].action["text"], "version/260924/main");
        args["plan"]["inputs"][0]["role"] = json!("textarea");
        assert!(hints(&pages, &parse(&args).unwrap(), &HashSet::new()).iter().all(|c| c.kind() != "fill"));
        args["plan"]["inputs"][0]["name"] = json!("");
        args["plan"]["inputs"][0].as_object_mut().unwrap().remove("role");
        assert!(parse(&args).is_err());
    }

    #[test]
    fn waits_are_bounded_and_used_actions_are_not_replayed() {
        assert_eq!(observation_delay(Duration::ZERO).unwrap(), POLL);
        assert_eq!(observation_delay(Duration::from_millis(14900)).unwrap(), Duration::from_millis(100));
        assert!(observation_delay(Duration::from_secs(15)).is_err());
        let plan = plan(json!({"task":"提交","authorization":"提交一次","expectedText":"新结果"}));
        let pages = json!({"pages":[{"frame":0,"url":"https://example.test/","items":[
            {"name":"提交","role":"button","ref":"r","nodeId":"doc:1","inView":true}]}]});
        let used = hints(&pages, &plan, &HashSet::new()).into_iter().map(|c| c.key).collect();
        let mut fresh = pages.clone(); fresh["pages"][0]["items"][0]["ref"] = json!("fresh");
        assert!(hints(&fresh, &plan, &used).is_empty());
        assert_eq!(evidence(&pages, &plan), evidence(&fresh, &plan));
        let mut args = json!({"plan":{"task":"t","authorization":"a","expectedText":"e"}});
        assert_eq!(parse(&args).unwrap().max_actions, 32);
        for invalid in [0, 65] { args["plan"]["maxActions"] = json!(invalid); assert!(parse(&args).is_err()); }
        assert!(origin(&json!({"pages":[{"url":"file:///tmp/x"}]})).is_err());
    }

    #[tokio::test]
    async fn jev_trusted_action_scope_does_not_leak_to_other_tasks_or_after_return() {
        assert!(!executing_browser_action());
        BROWSER_ACTION.scope(true, async {
            tokio::task::yield_now().await;
            assert!(executing_browser_action());
            assert!(!tokio::spawn(async { executing_browser_action() }).await.unwrap());
        }).await;
        assert!(!executing_browser_action());
    }

    #[test]
    fn dom_scroll_hints_follow_real_remaining_space_without_coordinates() {
        let plan = plan(json!({"task":"find next record","authorization":"read","expectedText":"record"}));
        let pages = json!({"pages":[{"frame":0,"url":"https://example.test/","viewport":{"scrollY":0,"height":600},
            "documentSize":{"height":1800},"items":[{"ref":"list","nodeId":"d:1","name":"Results","role":"div",
                "inView":true,"scroll":{"top":200,"height":1000,"viewportHeight":200}}]}]});
        let all = hints(&pages, &plan, &HashSet::new());
        assert_eq!(all.len(), 3);
        assert!(all.iter().all(|c| c.kind() == "scroll" && c.action["x"].is_null()));
        assert_eq!(all.iter().filter(|c| c.action["ref"].is_null() && c.action["delta"] == 480).count(), 1);
        let used = all.iter().map(|c| c.key.clone()).collect();
        assert!(hints(&pages, &plan, &used).is_empty());
        let mut next = pages.clone(); next["pages"][0]["viewport"]["scrollY"] = json!(1200);
        let remaining = hints(&next, &plan, &used);
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].action["delta"], -480);
    }

    fn bound(plan: &Plan, index: usize, pages: &Value) -> Option<Candidate> {
        let all = hints(pages, plan, &HashSet::new());
        let ranked = step_ranking(&plan.steps[index], &all);
        let (best, top) = *ranked.first()?;
        step_confident(best, ranked.get(1).map(|r| r.0)).then(|| bind_step(&plan.steps[index], index, top))
    }

    #[test]
    fn legacy_exact_steps_still_bind_to_fresh_refs() {
        let plan = plan(json!({"task":"查找","authorization":"只读搜索","expectedText":"结果",
            "steps":[{"action":"fill","name":"关键词","role":"input","text":"订单","expectedText":"结果"}]}));
        assert_eq!(plan.steps[0].expect(), Some("结果"));
        let mut pages = json!({"pages":[{"frame":0,"items":[{"ref":"field","nodeId":"d:1","name":"关键词","role":"input","editable":true,"inView":true}]}]});
        assert_eq!(bound(&plan, 0, &pages).unwrap().action, json!({"action":"fill","frame":0,"ref":"field","text":"订单"}));
        pages["pages"][0]["items"][0]["ref"] = json!("fresh-field");
        assert_eq!(bound(&plan, 0, &pages).unwrap().action["ref"], "fresh-field");
    }

    #[test]
    fn guided_steps_bind_natural_descriptions_to_the_right_control() {
        let goal = plan(json!({"task":"榜单","authorization":"只读筛选","expectedText":"降序","steps":[
            {"action":"click","target":"Top Charts 下的 PC & Console Games"},
            {"action":"click","target":"M Science 分组下 Digital Units 列的排序图标","expect":"降序","repeat":2},
            {"action":"click","target":"United States 复选框"}]}));
        let nav = json!({"pages":[{"frame":0,"items":[
            {"ref":"bench","nodeId":"d:1","name":"PC&Console: Explore Benchmark","role":"div","actionable":true,"inView":true,"region":"Home Dashboard Intelligence Opinions"},
            {"ref":"chart","nodeId":"d:2","name":"PC & Console Games","role":"li","actionable":true,"inView":true,"region":"Top Charts Mobile Games NEW PC Games Console Games"}]}]});
        assert_eq!(bound(&goal, 0, &nav).unwrap().action["ref"], "chart");
        let header = json!({"pages":[{"frame":0,"items":[
            {"ref":"text","nodeId":"d:3","name":"Digital Units","role":"th","actionable":true,"inView":true,"column":{"name":"Digital Units","index":3,"group":"M Science"}},
            {"ref":"icon","nodeId":"d:4","name":"","role":"span","actionable":true,"inView":true,"icon":"sort caret","column":{"name":"Digital Units","index":3,"group":"M Science"}},
            {"ref":"steam","nodeId":"d:5","name":"","role":"span","actionable":true,"inView":true,"icon":"sort caret","column":{"name":"Steam Units","index":6,"group":"Steam"}}]}]});
        assert_eq!(bound(&goal, 1, &header).unwrap().action["ref"], "icon");
        // name="…列" identifies the column for an icon step: the text header loses to the icon.
        let made = plan(json!({"task":"榜单","authorization":"只读筛选","expectedText":"降序","steps":[
            {"action":"click","target":"M Science 分组下那个列的排序图标","name":"Digital Units","expect":"降序","repeat":2}]}));
        assert_eq!(bound(&made, 0, &header).unwrap().action["ref"], "icon");
        let region = json!({"pages":[{"frame":0,"items":[
            {"ref":"us","nodeId":"d:6","name":"United States","role":"checkbox","selected":false,"inView":true},
            {"ref":"ca","nodeId":"d:7","name":"Canada","role":"checkbox","selected":false,"inView":true}]}]});
        assert_eq!(bound(&goal, 2, &region).unwrap().action["ref"], "us");
        // Ambiguity is never guessed locally: two equally named controls go to JEV / the main model.
        let twins = json!({"pages":[{"frame":0,"items":[
            {"ref":"a","nodeId":"d:8","name":"United States","role":"checkbox","inView":true,"region":"Record A"},
            {"ref":"b","nodeId":"d:9","name":"United States","role":"checkbox","inView":true,"region":"Record B"}]}]});
        assert!(bound(&goal, 2, &twins).is_none());
    }

    #[test]
    fn excluded_names_repel_and_authorized_text_stays_on_its_field() {
        let plan = plan(json!({"task":"选地区 Germany","authorization":"只读筛选","expectedText":"Germany",
            "inputs":[{"name":"Search","role":"input","text":"Germany"}],
            "steps":[{"action":"fill","target":"地区下拉面板内的搜索框（不是页面顶部的 Search any game or company）","text":"Germany"}]}));
        assert!(negated_mention(&plan.steps[0].target.to_lowercase(), "search any game or company"));
        assert!(!negated_mention(&plan.steps[0].target.to_lowercase(), "search"));
        let pages = json!({"pages":[{"frame":0,"items":[
            {"ref":"global","nodeId":"g:1","name":"Search any game or company","role":"input","editable":true,"inView":true},
            {"ref":"panel","nodeId":"p:1","name":"Search","role":"input","editable":true,"inView":true,
                "fieldContext":"Overall Global Global except China mainland Region Select All Africa (52)"}]}]});
        let all = hints(&pages, &plan, &HashSet::new());
        // The authorized text exists only on its own field: the global box is not even offered.
        let ranked = rank_for_step(&plan.steps[0], &plan, &all);
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].1.action["ref"], "panel");
        // …and the excluded full name scores below, not above.
        let global = all.iter().find(|c| c.action["ref"] == "global").unwrap();
        let panel = all.iter().find(|c| c.action["ref"] == "panel").unwrap();
        assert!(step_score(&plan.steps[0], global) < step_score(&plan.steps[0], panel));
    }

    #[test]
    fn commit_step_is_flagged_before_literal_success_lies() {
        let interagency = json!({"task":"选地区","authorization":"允许勾选 Germany；如有确认/Apply 按钮则点击它应用","expectedText":"Germany",
            "steps":[{"action":"click","target":"Germany 选项"}]});
        let history = vec![json!({"actions":["第1步 点击 \"Germany\" [checkbox] 未选中"],"status":"executed","completedActions":1,
            "effect":{"changed":["\"Germany\" [checkbox] 已选中"]}})];
        let pages = json!({"pages":[{"frame":0,"items":[
            {"ref":"c","nodeId":"c:1","name":"Confirm","role":"button","inView":true},
            {"ref":"g","nodeId":"g:1","name":"Germany","role":"checkbox","inView":true,"selected":true}]}]});
        assert!(needs_commit(&plan(interagency), &history, &pages).is_some());
        // Summary pill shows the filter applied → quiet even with the box still ticked.
        let mut applied_pages = pages.clone();
        applied_pages["pages"][0]["items"].as_array_mut().unwrap().push(
            json!({"ref":"s","nodeId":"s:1","name":"Country/Market Germany","role":"div","inView":true}));
        assert!(needs_commit(&plan(json!({"task":"选地区","authorization":"允许勾选 Germany；如有确认/Apply 按钮则点击它应用","expectedText":"Germany",
            "steps":[{"action":"click","target":"Germany 选项"}]})), &history, &applied_pages).is_none());
        // Clicked already → quiet.
        let mut done = history.clone();
        done.push(json!({"actions":["第2步 点击 \"Confirm\" [button]"],"status":"executed","completedActions":1,"effect":{}}));
        assert!(needs_commit(&plan(json!({"task":"选地区","authorization":"允许勾选 Germany；如有确认/Apply 按钮则点击它应用","expectedText":"Germany",
            "steps":[{"action":"click","target":"Germany 选项"}]})), &done, &pages).is_none());
        // A date-only run with Confirm merely somewhere on the page stays quiet.
        let date_run = json!({"task":"设时间","authorization":"允许填写日期；如有确认按钮则点它","expectedText":"2026-03-24",
            "steps":[{"action":"fill","target":"Week 起始日期框","text":"2026-03-24"}]});
        let date_history = vec![json!({"actions":["第1步 填写 \"Select date\" [input] = \"2026-03-24\""],"status":"executed",
            "completedActions":1,"effect":{"newText":["2026-03-22 ~ 2026-09-26"]}})];
        let date_pages = json!({"pages":[{"frame":0,"items":[
            {"ref":"c","nodeId":"c:1","name":"Confirm","role":"button","inView":true},
            {"ref":"d","nodeId":"d:1","name":"Select date","role":"input","inView":true,"editable":true}]}]});
        assert!(needs_commit(&plan(date_run), &date_history, &date_pages).is_none());
        // No mention in authorization → quiet.
        let plain_value = json!({"task":"看一眼","authorization":"只读浏览","expectedText":"德国",
            "steps":[{"action":"click","target":"Germany 选项"}]});
        assert!(needs_commit(&plan(plain_value), &history, &pages).is_none());
    }

    #[test]
    fn only_non_navigating_steps_chain_into_one_browser_round_trip() {
        let plan = plan(json!({"task":"t","authorization":"a","expectedText":"e","steps":[
            {"action":"fill","target":"搜索框","text":"United States"},
            {"action":"click","target":"United States 复选框"},
            {"action":"click","target":"Confirm 按钮"},
            {"action":"press","key":"Enter"},
            {"action":"click","target":"排序图标","expect":"降序"}]}));
        let checkbox = Candidate::new(json!({"action":"click"}), json!({"role":"checkbox"}), json!(0), String::new(), "", "");
        let button = Candidate::new(json!({"action":"click"}), json!({"role":"button"}), json!(0), String::new(), "", "");
        assert!(chains(&plan.steps[0], &checkbox));
        assert!(chains(&plan.steps[1], &checkbox));
        assert!(!chains(&plan.steps[2], &button));
        assert!(!chains(&plan.steps[3], &press_candidate(&plan.steps[3], 3)));
        assert!(!chains(&plan.steps[4], &checkbox), "a step with an expectation is always checked before continuing");
        for invalid in [json!({"action":"press"}), json!({"action":"fill","target":"x"}), json!({"action":"hover","target":"x"}),
            json!({"action":"click"}), json!({"action":"click","target":"x","repeat":4}), json!({"action":"press","key":"F5"})] {
            assert!(parse(&json!({"plan":{"task":"t","authorization":"a","expectedText":"e","steps":[invalid]}})).is_err());
        }
        let pages = json!({"pages":[{"url":"https://x.test/?order=desc","visibleText":"Region United States","title":"Top"}]});
        assert!(literal_met(&pages, "美国|United States") && literal_met(&pages, "order=desc") && !literal_met(&pages, "Canada"));
    }

    #[test]
    fn semantic_controls_remain_available_beside_canvas() {
        let plan = plan(json!({"task":"选择筛选","authorization":"允许筛选","expectedText":"结果"}));
        let pages = json!({"pages":[{"frame":0,"visualSuggested":true,"items":[
            {"ref":"r","name":"收入","role":"radio","inView":true},
            {"ref":"c","name":"仅已发布","role":"checkbox","inView":true},
            {"ref":"p","name":"地区","role":"div","haspopup":"listbox","inView":true},
            {"ref":"s","name":"展开筛选","role":"summary","inView":true},
            {"ref":"custom","name":"Region Global","role":"div","tabIndex":0,"actionable":true,"inView":true},
            {"ref":"scroll","name":"内容区","role":"div","tabIndex":0,"scroll":{},"inView":true},
            {"ref":"container","name":"Focus region","role":"div","tabIndex":0,"inView":true},
            {"ref":"text","name":"普通文本","role":"div","inView":true},
            {"ref":"disabled","name":"不可用","role":"radio","disabled":true,"inView":true}
        ]}]});
        let refs: HashSet<String> = hints(&pages, &plan, &HashSet::new()).iter().map(|c| c.action["ref"].as_str().unwrap().to_string()).collect();
        assert_eq!(refs, HashSet::from(["r", "c", "p", "s", "custom"].map(String::from)));
    }

    #[test]
    fn graph_context_keeps_complete_routes_and_checks_within_budget() {
        let route = json!({"id":"route-1","conditions":["已登录"],"steps":["查询订单"],"checks":["订单号匹配"]});
        let result = graph_context(&json!({"graph":{"routes":[route.clone(),route.clone(),route.clone(),route.clone()]}}));
        assert_eq!(result["routes"].as_array().unwrap().len(), 3);
        assert_eq!(result["truncated"], true);
        let oversized = graph_context(&json!({"graph":{"routes":[{"steps":["x".repeat(12001)]}]}}));
        assert_eq!(oversized["routes"], json!([]));
    }
}
