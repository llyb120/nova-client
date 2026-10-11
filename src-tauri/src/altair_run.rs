//! Guided browser runs: executes the main model's `plan.steps` back-to-back against fresh DOM.
//! The main model makes every decision; this module makes none and sends no screenshots. Each step
//! is bound locally to one control of the latest observation (name/role/container/description
//! scoring), consecutive steps that cannot change what the next target means share one act, and
//! `expect`/`expectedText` are checked literally (page text, URL, title). Anything uncertain —
//! close or missing matches, unmet expectations, cross-site pages, exhausted budgets — hands back to
//! the main model with the latest observation and the remaining steps.
use serde::Deserialize;
use serde_json::{json, Value};
use std::{collections::{BTreeMap, HashMap, HashSet}, path::Path, time::{Duration, Instant}};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Plan {
    task: String,
    // The vision proxy has safe defaults; with Altair off, preserve the old run contract.
    #[serde(default = "default_authorization")]
    authorization: String,
    #[serde(default)]
    expected_text: String,
    #[serde(default)]
    inputs: Vec<Input>,
    #[serde(default)]
    steps: Vec<Step>,
    #[serde(default)]
    control_names: Vec<String>,
    #[serde(default, rename = "useExperience")]
    _use_experience: bool,
    #[serde(default = "default_max_actions")]
    max_actions: usize,
    /// expectedText was given: only then is the final page-text check a completion gate.
    #[serde(skip)]
    check_goal: bool,
    /// Concrete values the main model wrote into the goal (e.g. "United States"); local only.
    #[serde(skip)]
    phrases: Vec<String>,
}
fn default_max_actions() -> usize { 32 }
fn default_authorization() -> String { "仅限完成目标所需的页面内点击、填写、滚动；不做支付、删除、对外发送等不可逆操作".into() }
/// One step of the main model's plan. The main model decides *what* to do; the runner binds the
/// control locally on fresh DOM (close matches go back to the main model), executes consecutive
/// steps without round-trips, and checks `expect` before moving on.
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
    /// Observable result that must hold before the next step: literal text/URL/title fragments,
    /// alternatives separated by `|`.
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
    fn describe(&self) -> String { self.describe_with(true) }
    /// `values=false` keeps typed values out of saved routes (they are task data, not the path).
    fn describe_with(&self, values: bool) -> String {
        let mut text = format!("{} {}", self.action, if self.target.is_empty() { self.name.as_deref().unwrap_or("页面") } else { &self.target });
        if let Some(name) = self.name.as_deref().filter(|_| !self.target.is_empty()) { text += &format!(" (名称={name})"); }
        if let Some(within) = &self.within { text += &format!(" 位于「{within}」"); }
        if let Some(value) = self.text.as_ref().filter(|_| values) { text += &format!(" 输入「{}」", short(value, 60)); }
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

// ponytail: fixed budgets. 180s per run, 4s local load wait, 250ms polling.
const RUN_BUDGET: Duration = Duration::from_secs(180);
const LOAD_WAIT: Duration = Duration::from_secs(4);
const POLL: Duration = Duration::from_millis(250);
const STATE_REPEATS: usize = 3;

fn parse(args: &Value, altair_enabled: bool) -> Result<Plan, String> {
    if !altair_enabled && ["authorization", "expectedText"].iter().any(|key| !args["plan"][key].is_string()) {
        return Err("Altair 关闭时 run 需要 plan.task、authorization、expectedText；直接操作用 act".into());
    }
    let mut plan: Plan = serde_json::from_value(args["plan"].clone())
        .map_err(|e| format!("plan 格式错误（{e}）；需要 task 与 steps，steps 须为对象数组，如 [{{\"action\":\"click\",\"target\":\"顶部导航 Intelligence\",\"expect\":\"情报页\"}}]，不是字符串"))?;
    if altair_enabled && plan.steps.is_empty() { return Err("run 需要 plan.steps（主模型给出的步骤）；单步或同屏合批直接用 act".into()); }
    plan.check_goal = !altair_enabled || !plan.expected_text.trim().is_empty();
    if !plan.check_goal { plan.expected_text = plan.task.chars().take(500).collect(); }
    let valid = |s: &str, max: usize| !s.trim().is_empty() && s.chars().count() <= max;
    if !valid(&plan.task, 2000) || !valid(&plan.authorization, 1000) || !valid(&plan.expected_text, 500)
        || plan.inputs.len() > 8 || plan.inputs.iter().any(|i| i.name.chars().count() > 300
            || (i.name.trim().is_empty() && i.role.is_none() && i.field_context.is_none())
            || i.field_context.as_ref().is_some_and(|f| !valid(f, 600))
            || i.role.as_ref().is_some_and(|r| !valid(r, 80)) || i.text.chars().count() > 4000) {
        return Err("run 目标/授权/完成证据无效，inputs 最多8个非敏感字段".into());
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
    plan.phrases = goal_phrases(&format!("{}\n{}\n{}", plan.task, plan.authorization, plan.expected_text));
    Ok(plan)
}

/// Concrete values a person would type into a search box: quoted text and Title-Case Latin
/// phrases ("United States", "M Science"). Used to recognise filter values typed by fill steps.
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

fn input_matches(item: &Value, input: &Input) -> bool {
    let context = item["fieldContext"].as_str().unwrap_or_default();
    // ponytail: a copied fieldContext may be cut by the observation summary; ≥24 chars prefix still binds.
    input.role.as_ref().is_none_or(|role| item["role"] == *role)
        && input.field_context.as_ref().is_none_or(|f| context == f || (f.chars().count() >= 24 && context.starts_with(f.as_str())))
        && (item["name"] == input.name
            || (!input.name.is_empty() && context == input.name)
            || (input.name.is_empty() && input.field_context.is_some()))
}

fn short(text: &str, max: usize) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() <= max { text } else { format!("{}…", text.chars().take(max).collect::<String>()) }
}

// Transition identity must cover all controls, including those beyond the reply's text budget.
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

fn origin(pages: &Value) -> Result<String, String> {
    let url = reqwest::Url::parse(pages["pages"][0]["url"].as_str().ok_or("页面缺少 URL")?).map_err(|_| "页面 URL 无效")?;
    if !matches!(url.scheme(), "https" | "http") { return Err("只支持 HTTP(S) 页面".into()); }
    Ok(url.origin().ascii_serialization())
}

// Distinguish run-executed actions from the main model's own actions for experience recording.
tokio::task_local! { static BROWSER_ACTION: bool; }

pub(crate) fn executing_browser_action() -> bool {
    BROWSER_ACTION.try_with(|active| *active).unwrap_or(false)
}

async fn execute_browser(root: &Path, args: &Value, owner: &str, tool: &str) -> Result<Value, String> {
    BROWSER_ACTION.scope(args["operation"] == "act", async { match tool {
        "webview" => Box::pin(crate::native_browser::execute(root, args)).await,
        "chrome" => Box::pin(crate::native_browser::execute_chrome(root, args, owner)).await,
        _ => Err("不支持的浏览器".into()),
    }}).await
}

fn route_save(plan: &Plan, start: &Value, home: &str, snapshot: &str) -> Value {
    let path = crate::tool_experience::entry_path(start["pages"][0]["url"].as_str().unwrap_or_default());
    let steps = crate::tool_experience::fit_steps(&plan.steps.iter().map(|s| s.describe_with(false)).collect::<Vec<_>>());
    json!({"operation":"experience_save","snapshotId":snapshot,"experience":{"scope":home,"task":short(&plan.task, 290),
        "conditions":[format!("起始页面 {}", short(&path, 400))],"steps":steps,"checks":[short(&plan.expected_text, 480)],
        "evidence":format!("run 已核验完成条件：{}", short(&plan.expected_text, 400)),"outcome":"success","redacted":true}})
}

/// One executable DOM hint. `action` (with the live ref) never leaves this process. `key` is
/// node + state identity (replay protection); `loose` is target identity without node/state
/// (detects newly surfaced UI).
#[derive(Clone, Debug)]
struct Candidate {
    action: Value,
    key: String,
    loose: String,
    label: String,
    name: String,
    fresh: bool,
    /// Click hint on a non-password editable control (guided fill steps bind here).
    editable: bool,
}

impl Candidate {
    fn new(action: Value, key: Value, loose: Value, label: String, name: &str) -> Self {
        Candidate { action, key: key.to_string(), loose: loose.to_string(), label,
            name: name.trim().to_lowercase(), fresh: false, editable: false }
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
    // 80% keeps overlap for reading; cap only at the act delta limit so tall viewports still page in one step.
    let step = (viewport * 0.8).clamp(80., 1200.).round() as i32;
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
        out.push(Candidate::new(action, key, loose, format!("滚动{zh} {area}"), &area));
    }
    out
}

/// Every visible, enabled control a guided step could bind to: clicks (editable fields included,
/// fill steps bind to them) and page/region scrolls. Passwords are never offered.
fn candidates(pages: &Value, used: &HashSet<String>) -> Result<Vec<Candidate>, String> {
    let frames = pages["pages"].as_array().ok_or("缺少 DOM 观察")?;
    let mut result = Vec::new();
    let mut scrolls = Vec::new();
    for page in frames {
        if page["frame"] == 0 {
            scrolls.extend(scroll_hints(page, None, false));
            scrolls.extend(scroll_hints(page, None, true));
        }
        for item in page["items"].as_array().ok_or("缺少 DOM 元素")? {
            let name = item["name"].as_str().unwrap_or_default();
            let role = item["role"].as_str().unwrap_or_default();
            if item["inView"] != true || item["disabled"] == true
                || !item["ref"].is_string() || !page["frame"].is_u64() || name.chars().count() > 300 { continue; }
            scrolls.extend(scroll_hints(page, Some(item), false));
            scrolls.extend(scroll_hints(page, Some(item), true));
            if item["blockedBy"].is_string() { continue; }
            let loose = json!(["click", page["frame"], name, role, item["region"].as_str().map(|r| short(r, 120)),
                item["fieldContext"], item["column"]["name"], item["href"], Value::Null]);
            let action = json!({"action":"click","frame":page["frame"],"ref":item["ref"]});
            if item["editable"] == true {
                if item["password"] == true { continue; }
                // Clicking an editable control can open a date picker or combobox without typing.
                let key = json!({"action":"click","frame":page["frame"],"nodeId":item["nodeId"],
                    "name":name,"role":role,"region":item["region"],"fieldContext":item["fieldContext"],"expanded":item["expanded"]});
                let mut click = Candidate::new(action, key, loose, element_label("点击", item), name);
                click.editable = true;
                result.push(click);
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
            result.push(Candidate::new(action, key, loose, element_label("点击", item), name));
        }
    }
    result.extend(scrolls);
    result.retain(|c| !used.contains(&c.key));
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

/// Local certainty: a strong match clearly ahead of the runner-up. Anything closer goes back to
/// the main model.
fn step_confident(best: i64, second: Option<i64>) -> bool {
    best >= 40 && second.is_none_or(|s| best - s >= 25)
}

fn step_ranking<'a>(step: &Step, all: &'a [Candidate]) -> Vec<(i64, &'a Candidate)> {
    let mut ranked: Vec<(i64, &Candidate)> = all.iter().map(|c| (step_score(step, c), c)).filter(|(s, _)| *s > 0).collect();
    ranked.sort_by(|a, b| b.0.cmp(&a.0));
    ranked
}

/// A fill step carrying an input's exact text may only bind to that input's field — never to an
/// unrelated box that happens to match the description. Steps with novel text keep legacy freedom.
fn rank_for_step<'a>(step: &Step, plan: &Plan, all: &'a [Candidate]) -> Vec<(i64, &'a Candidate)> {
    let mut ranked = step_ranking(step, all);
    if let Some(text) = step.text.as_deref().filter(|_| step.action == "fill") {
        if plan.inputs.iter().any(|i| i.text == text) {
            ranked.retain(|(_, c)| {
                let key: Value = serde_json::from_str(&c.key).unwrap_or(Value::Null);
                c.editable && plan.inputs.iter().any(|i| i.text == text && input_matches(
                    &json!({"name":key["name"],"role":key["role"],"fieldContext":key["fieldContext"]}), i))
            });
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
    let key: Value = serde_json::from_str(bound.key.split("|fill|").next().unwrap_or_default()).unwrap_or(Value::Null);
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

fn result_signature(pages: &Value) -> String {
    json!(pages["pages"].as_array().into_iter().flatten().map(|p| json!([p["url"],
        p["items"].as_array().into_iter().flatten().filter(|i| i["inView"] == true
            && (!i["selected"].is_null() || i["dateValue"].is_string() || i["sort"].is_string()))
            .map(|i| json!([i["name"],i["role"],i["fieldContext"],i["selected"],i["dateValue"],i["sort"]])).collect::<Vec<_>>(),
        p["tables"].as_array().into_iter().flatten().map(|t| json!([t["headers"],t["rows"],t["totalRows"],
            t["columns"].as_array().into_iter().flatten().map(|c| json!([c["name"],c["index"],c["group"],c["sort"]])).collect::<Vec<_>>()])).collect::<Vec<_>>()
    ])).collect::<Vec<_>>()).to_string()
}

/// Observable consequence of one action: navigation, newly surfaced controls, newly visible text
/// (tooltips, validation, result counts) and changed control states. Empty = nothing happened.
fn action_effect(before: &Value, after: &Value) -> Value {
    let url = |pages: &Value| pages["pages"][0]["url"].as_str().unwrap_or_default().to_string();
    let lines = |pages: &Value| -> Vec<String> {
        pages["pages"].as_array().into_iter().flatten()
            .flat_map(|p| p["visibleText"].as_str().or(p["text"].as_str()).unwrap_or_default().lines().map(str::trim).map(String::from).collect::<Vec<_>>())
            .filter(|l| !l.is_empty()).collect()
    };
    let old_lines: HashSet<String> = lines(before).into_iter().collect();
    let new_text: Vec<String> = lines(after).into_iter().filter(|l| !old_lines.contains(l)).take(4).map(|l| short(&l, 80)).collect();
    let none = HashSet::new();
    let old_controls: HashSet<String> = candidates(before, &none).unwrap_or_default().into_iter().map(|c| c.loose).collect();
    let new_controls: Vec<String> = candidates(after, &none).unwrap_or_default().into_iter()
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
    let results_changed = result_signature(before) != result_signature(after);
    let nothing = from == to && !results_changed && new_text.is_empty() && new_controls.is_empty() && changed.is_empty();
    json!({"url":if from != to { json!(to) } else { Value::Null },
        "newControls":new_controls.iter().take(6).collect::<Vec<_>>(),"newControlCount":new_controls.len(),
        "newText":new_text,"changed":changed,"resultsChanged":results_changed,
        "summary":if nothing { "页面没有可见变化：该动作可能无效，换目标或方法" } else { "" }})
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
    trace: Vec<Value>,
    refreshes: usize,
    executed: usize,
    preflight_recoveries: usize,
    preflight: usize,
    used: HashSet<(String, String)>,
    state_actions: HashMap<String, usize>,
    baseline: Option<HashSet<String>>,
    loading_since: Option<Instant>,
    // Progress, how targets were bound, and expectation checks.
    raw_steps: Value,
    step_index: usize,
    resolved_locally: usize,
    checks: Vec<Value>,
    step_candidates: Vec<String>,
}

enum Resolution { Found(Candidate), Missing, Ambiguous(String) }

fn press_candidate(step: &Step, index: usize) -> Candidate {
    let key = step.key.as_deref().unwrap_or_default();
    Candidate::new(json!({"action":"press","key":key}), json!({"action":"press","key":key,"step":index}),
        json!(["press", key, index]), format!("第{}步 按键 {key}", index + 1), "")
}

impl Run<'_> {
    fn args(&self, mut value: Value) -> Value {
        value[self.target_key] = json!(self.target);
        value
    }
    // The run reads the full stored observation; the inline summary only goes back to the main
    // model, so keep it small enough to never be truncated into a file it has to read with a shell.
    fn inspect_args(&self) -> Value {
        self.args(json!({"operation":"inspect","scope":"viewport","visual":"none","maxTextChars":2500,"maxItems":30}))
    }
    fn pages(&self) -> Result<Value, String> {
        crate::native_browser::altair_observation(self.root, &self.args(json!({"snapshotId":self.snapshot})), self.owner, self.tool)
    }
    async fn refresh(&mut self) -> Result<Value, String> {
        let observed = execute_browser(self.root, &self.inspect_args(), self.owner, self.tool).await?;
        self.refreshes += 1;
        self.snapshot = observed["snapshotId"].clone();
        self.latest = observed;
        self.pages()
    }
    fn note(&mut self, node: &str, detail: Value) {
        // ponytail: keep the last 64 notes; history keeps every executed action.
        if self.trace.len() >= 64 { self.trace.remove(0); }
        self.trace.push(json!({"node":node,"detail":detail,"executed":self.executed}));
    }

    /// Executes one batch (a single step, or several chained steps) from the observed state.
    /// Ok(None): DOM preflight failed before any input — re-bind from fresh DOM.
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
            if !result["snapshotId"].is_string() || result["observationError"].is_string() { self.refresh().await?; }
            else { self.snapshot = result["snapshotId"].clone(); }
            return Ok(None);
        }
        // A fill always selects all and replaces the value, and a batch action that failed at DOM
        // preflight sent no input: both are re-bound on fresh DOM instead of handing off.
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
        // What actually happened, so the main model can tell "sorted" from "opened a tooltip".
        let mut effect = action_effect(pages, &fresh);
        if effect["summary"] != "" && batch.iter().any(|c| matches!(c.kind(), "click" | "press")) {
            // SPA renders often land just after the input's feedback; glance once more before calling it a no-op.
            tokio::time::sleep(Duration::from_millis(300)).await;
            let later = self.refresh().await?;
            let later_state = replay_evidence(&later);
            for c in batch.iter().take(completed) { self.used.insert((later_state.clone(), c.key.clone())); }
            effect = action_effect(pages, &later);
        }
        if let Some(last) = self.history.last_mut() { last["effect"] = effect; }
        Ok(Some(completed))
    }

    /// Binds a guided step to one control of the current observation. Only a confident local
    /// match binds; weak or close matches go back to the main model with their labels.
    fn resolve(&mut self, all: &[Candidate], index: usize) -> Resolution {
        let step = &self.plan.steps[index];
        if step.action == "press" { return Resolution::Found(press_candidate(step, index)); }
        let ranked = rank_for_step(step, &self.plan, all);
        self.step_candidates = ranked.iter().take(6).map(|(score, c)| format!("{} (score {score})", c.label)).collect();
        let Some(&(best, top)) = ranked.first() else { return Resolution::Missing };
        if step_confident(best, ranked.get(1).map(|r| r.0)) {
            self.resolved_locally += 1;
            return Resolution::Found(bind_step(step, index, top));
        }
        let why = if ranked.len() == 1 { "唯一候选匹配度不足" } else { "多个相近候选" };
        Resolution::Ambiguous(format!("{why}：{}", self.step_candidates.iter().take(3).cloned().collect::<Vec<_>>().join("；")))
    }

    /// Waits (bounded) for an expectation to appear literally in page text, URL or title.
    /// Semantic judgement belongs to the main model: an unmet literal check hands back.
    async fn verify(&mut self, expect: &str, step: usize) -> Result<bool, String> {
        let begun = Instant::now();
        loop {
            let pages = self.pages()?;
            let loading = pages["pages"].as_array().into_iter().flatten().any(|p| p["loading"] == true);
            // ponytail: 6s per expectation; slower jobs need an explicit wait step or a new run.
            let verdict = if !loading && literal_met(&pages, expect) { Some((true, "literal")) }
                else if begun.elapsed() >= Duration::from_secs(6) { Some((false, "timeout")) } else { None };
            if let Some((met, by)) = verdict {
                self.checks.push(json!({"step":step + 1,"expect":expect,"met":met,"by":by,"ms":begun.elapsed().as_millis() as u64}));
                return Ok(met);
            }
            tokio::time::sleep(POLL).await;
            self.refresh().await?;
        }
    }

    /// Executes the main model's steps back-to-back. Steps that cannot change the meaning of the
    /// next target (typing, Tab, toggling a selection) share one browser round-trip.
    async fn drive_steps(&mut self, home: &str, started: Instant) -> Result<(), String> {
        let total = self.plan.steps.len();
        // A legacy goal-only run must hand back without sending input, even if its goal is visible.
        if total == 0 { return Err("Altair 已关闭，主模型接手；请直接 act 或提供 plan.steps".into()); }
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
            // state stay available.
            let state_used: HashSet<String> = self.used.iter().filter(|(s, _)| s == &state).map(|(_, k)| k.clone()).collect();
            let mut all = candidates(&pages, &state_used)?;
            // "Fresh" = surfaced by the last action (opened menu, dialog, results).
            if let Some(base) = &self.baseline { for c in &mut all { c.fresh = !base.contains(&c.loose); } }
            let anchors: HashSet<String> = all.iter().map(|c| c.loose.clone()).collect();
            let first = match self.resolve(&all, index) {
                Resolution::Found(c) => c,
                Resolution::Ambiguous(why) => return Err(format!("第{}步目标不确定（{why}）：{}；用 name/within/role 指定后从该步继续",
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
        // Without an explicit expectedText the task is not literal page text: the main model judges.
        let expected = self.plan.expected_text.clone();
        if self.plan.check_goal && !self.verify(&expected, total).await? {
            return Err(format!("需要按最新页面重新规划，但完成条件「{expected}」未通过核验；请核对最新页面后补充步骤"));
        }
        Ok(())
    }
}

pub(crate) async fn browser(root: &Path, args: &Value, owner: &str, tool: &str) -> Result<Value, String> {
    let altair_enabled = crate::native_browser::altair_settings()?.altair_enabled;
    let plan = parse(args, altair_enabled)?;
    let target_key = if tool == "webview" { "browserId" } else { "tabTag" };
    let target = args[target_key].as_str().ok_or("run 缺少浏览器目标")?.to_string();
    let mut snapshot = args["snapshotId"].clone();
    let initial = match crate::native_browser::altair_observation(root, args, owner, tool) {
        Ok(pages) => pages,
        Err(error) if !altair_enabled => return Err(error),
        // run binds steps against the live DOM, so a missing/stale snapshotId just means observing
        // first instead of bouncing the model back for an inspect round-trip.
        Err(_) => {
            let mut inspect = json!({"operation":"inspect","scope":"viewport","visual":"none","maxTextChars":2500,"maxItems":30});
            inspect[target_key] = json!(target);
            snapshot = execute_browser(root, &inspect, owner, tool).await?["snapshotId"].clone();
            let mut fresh = args.clone();
            fresh["snapshotId"] = snapshot.clone();
            crate::native_browser::altair_observation(root, &fresh, owner, tool)?
        }
    };
    let home = origin(&initial)?;
    let mut latest = json!({"snapshotId":snapshot});
    latest[target_key] = json!(target);
    let mut run = Run {
        root, owner, tool, target_key, target, plan,
        snapshot, latest,
        history: Vec::new(), trace: Vec::new(),
        refreshes: 0, executed: 0, preflight_recoveries: 0, preflight: 0,
        used: HashSet::new(), state_actions: HashMap::new(),
        baseline: None, loading_since: None,
        raw_steps: args["plan"]["steps"].clone(), step_index: 0, resolved_locally: 0,
        checks: Vec::new(), step_candidates: Vec::new(),
    };
    let started = Instant::now();
    let outcome = run.drive_steps(&home, started).await;
    let mut latest = run.latest.clone();
    // A run that passed its completion check is a verified route: record it so the next session
    // gets it via experienceHint instead of relying on the model to call experience_save.
    // One-step plans are obvious from the page itself; storing them only buries real routes.
    let saved = if outcome.is_ok() && run.plan.check_goal && run.plan.steps.len() >= 2 {
        let save = route_save(&run.plan, &initial, &home, latest["snapshotId"].as_str().unwrap_or("run"));
        let owner = crate::native_browser::tool_owner(root, owner).unwrap_or_else(|_| owner.into());
        let (tool, scope) = (tool.to_string(), home.clone());
        tokio::task::spawn_blocking(move || crate::tool_experience::execute(&crate::tool_experience::dir(), &tool, &owner, &save, Some(&scope)))
            .await.map_err(|e| e.to_string()).and_then(|r| r)
            .map(|v| json!({"saved":true,"id":v["experience"]["id"]})).unwrap_or_else(|e| json!({"saved":false,"error":e}))
    } else { Value::Null };
    let handoff = outcome.is_err();
    if handoff {
        // Hand back with fresh evidence: the main model continues from the latest observation.
        match execute_browser(root, &run.inspect_args(), owner, tool).await {
            Ok(observed) => latest.as_object_mut().unwrap().extend(observed.as_object().cloned().unwrap_or_default()),
            Err(error) => latest["observationError"] = json!(error),
        }
    }
    // Compact by design: the main model needs outcome, evidence of what happened and what is missing.
    // Oversized replies were archived to files and cost a shell round-trip each.
    let mut nodes = BTreeMap::<String, usize>::new();
    for h in &run.history { *nodes.entry(h["node"].as_str().unwrap_or("?").to_string()).or_default() += 1; }
    let total_steps = run.plan.steps.len();
    let resume = run.step_index.min(total_steps);
    let guided = if total_steps == 0 { Value::Null } else { json!({"totalSteps":total_steps,"completedSteps":resume,
        "failedStep":if handoff && resume < total_steps { json!(resume + 1) } else { Value::Null },
        "remainingSteps":if handoff { json!(run.raw_steps.as_array().map(|s| s[resume..].to_vec()).unwrap_or_default()) } else { json!([]) },
        "failedStepCandidates":if handoff && resume < total_steps { json!(run.step_candidates) } else { json!([]) },
        "resolvedLocally":run.resolved_locally,"checks":run.checks,
        "next":if handoff { "按 reason 修正 remainingSteps 的第一步（target/name/within/expect），只提交 remainingSteps 重新 run；已完成的步骤不要重放。" } else { "" }}) };
    latest["altairRun"] = json!({"status":if handoff {"handoff"} else {"completed"},
        "reason":outcome.err(),
        "guided":guided,
        "verification":if handoff || !run.plan.check_goal {"unverified"} else {"subgoal_verified"},
        "executedActions":run.executed,"nodes":nodes,
        "requestCount":0,
        "observationRefreshes":run.refreshes,"preflightRecoveries":run.preflight_recoveries,"maxActions":run.plan.max_actions,
        "elapsedMs":started.elapsed().as_millis() as u64,
        "history":run.history,
        "experience":saved,
        // Diagnostics only matter on handoff; a completed run stays small for the main model.
        "trace":if handoff { json!(run.trace.iter().rev().take(12).rev().collect::<Vec<_>>()) } else { Value::Null },
        "next":if handoff { "按 reason 处理：用最新 snapshotId 直接 act，或修正 guided.remainingSteps 后重新 run；不重放已执行步骤。" }
            else if run.plan.check_goal { "步骤与完成条件已按页面文字核验；最终答案仍须核对真实数据、筛选与排序。" }
            else { "步骤已全部执行（未给 expectedText，未做完成核验）；按返回的观察/vision 自行判断结果。" },
        "notice":"run 只按主模型给出的 steps 在最新 DOM 上执行并做文字核验，不替主模型决策。history[].effect 是实际变化，不等于目标完成。"});
    Ok(latest)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hints(pages: &Value, used: &HashSet<String>) -> Vec<Candidate> { candidates(pages, used).unwrap() }
    fn plan(value: Value) -> Plan { parse(&json!({"plan":value}), true).unwrap() }

    #[test]
    fn run_requires_main_model_steps() {
        let error = parse(&json!({"plan":{"task":"筛选美国"}}), true).err().unwrap();
        assert!(error.contains("plan.steps") && error.contains("act"), "{error}");
        assert!(parse(&json!({"plan":{"task":"筛选美国","steps":[]}}), true).is_err());
        let p = plan(json!({"task":"筛选美国","steps":[{"action":"click","target":"United States 复选框"}]}));
        assert_eq!(p.expected_text, "筛选美国");
        assert!(p.authorization.contains("不可逆"));
        // Hints of the removed goal mode are accepted and ignored.
        assert!(parse(&json!({"plan":{"task":"t","useExperience":true,"controlNames":["x"],
            "steps":[{"action":"press","key":"Enter"}]}}), true).is_ok());
    }

    #[tokio::test]
    async fn disabled_run_preserves_legacy_plan_and_hands_back_without_input() {
        let args = json!({"plan":{"task":"筛选美国","authorization":"只读筛选","expectedText":"United States",
            "controlNames":["Region"],"useExperience":true}});
        let p = parse(&args, false).unwrap();
        assert!(p.steps.is_empty() && p.check_goal);
        assert!(parse(&args, true).is_err(), "enabled mode still requires explicit steps");
        for key in ["authorization", "expectedText"] {
            let mut invalid = args.clone();
            invalid["plan"].as_object_mut().unwrap().remove(key);
            assert!(parse(&invalid, false).is_err());
            invalid["plan"][key] = json!(" ");
            assert!(parse(&invalid, false).is_err());
        }
        let mut guided = args.clone();
        guided["plan"]["steps"] = json!([{"action":"click","target":"United States 复选框"}]);
        assert!(parse(&guided, false).unwrap().check_goal);
        guided["plan"]["controlNames"] = json!([""]);
        assert!(parse(&guided, false).is_err());
        let mut run = Run {
            root: Path::new("."), owner: "test", tool: "webview", target_key: "browserId", target: "test".into(), plan: p,
            snapshot: Value::Null, latest: Value::Null, history: vec![], trace: vec![], refreshes: 0, executed: 0,
            preflight_recoveries: 0, preflight: 0, used: HashSet::new(), state_actions: HashMap::new(),
            baseline: None, loading_since: None, raw_steps: Value::Null, step_index: 0, resolved_locally: 0,
            checks: vec![], step_candidates: vec![],
        };
        let error = run.drive_steps("https://example.test", Instant::now()).await.unwrap_err();
        assert!(error.contains("已关闭"), "{error}");
        assert_eq!(run.executed, 0);
        assert!(run.history.is_empty());
    }

    #[test]
    fn result_identity_ignores_popups_and_refs_but_not_results() {
        let pages = json!({"pages":[{"url":"https://x.test/?order=desc","visibleText":"Region United States",
            "items":[{"ref":"old","inView":true,"fieldContext":"Day start","dateValue":"2026-04-13"}],
            "tables":[{"headers":["Game","DAU"],"rows":[],"loadedRows":0},
                {"headers":[],"rows":[["A","5"],["B","4"],["C","3"]],"loadedRows":50}]}]});
        let mut changed = pages.clone(); changed["pages"][0]["items"][0]["ref"] = json!("new");
        changed["pages"][0]["visibleText"] = json!("Popup opened");
        assert_eq!(result_signature(&pages), result_signature(&changed), "popup/ref changes are not result changes");
        changed["pages"][0]["tables"][1]["rows"][0][1] = json!("0");
        assert_ne!(result_signature(&pages), result_signature(&changed));
        assert_eq!(action_effect(&pages, &changed)["resultsChanged"], true);
        changed = pages.clone(); changed["pages"][0]["items"][0]["dateValue"] = json!("2026-05-01");
        assert_ne!(result_signature(&pages), result_signature(&changed));
        changed = pages.clone(); changed["pages"][0]["tables"][0]["columns"] = json!([{"name":"DAU","sort":"ascending"}]);
        assert_ne!(result_signature(&pages), result_signature(&changed));
        changed = pages.clone(); changed["pages"][0]["items"].as_array_mut().unwrap()
            .push(json!({"inView":true,"name":"Canada","role":"checkbox","selected":true}));
        assert_ne!(result_signature(&pages), result_signature(&changed));
    }

    #[test]
    fn focusable_sort_icon_and_replay_state_survive_tooltip_changes() {
        let mut pages = json!({"pages":[{"frame":0,"url":"https://example.test/table","text":"table",
            "items":[{"ref":"icon","nodeId":"icon","role":"div","name":"","tabIndex":0,"inView":true,
                "column":{"index":3,"name":"Digital Units"}},
                {"ref":"pane","nodeId":"pane","role":"div","name":"table","tabIndex":0,"inView":true,"scroll":{}}]}]});
        let available = hints(&pages, &HashSet::new());
        assert_eq!(available.len(), 1);
        assert_eq!(available[0].action["ref"], "icon");
        assert!(available[0].key.contains("Digital Units"));
        let state = replay_evidence(&pages);
        pages["pages"][0]["text"] = json!("table tooltip definition");
        pages["pages"][0]["items"][0]["ref"] = json!("fresh");
        assert_eq!(state, replay_evidence(&pages));
        assert!(hints(&pages, &HashSet::from([available[0].key.clone()])).is_empty());
        pages["pages"][0]["url"] = json!("https://example.test/table?order=desc");
        assert_ne!(state, replay_evidence(&pages));
    }

    #[test]
    fn goal_phrases_pick_quoted_and_title_case_values() {
        let phrases = goal_phrases("Region 改为 \"United States\"，数据源 M Science，按Digital Units降序");
        for p in ["United States", "M Science", "Digital Units", "Region"] { assert!(phrases.iter().any(|x| x == p), "{p} in {phrases:?}"); }
    }

    #[test]
    fn header_icons_are_labelled_with_column_and_group() {
        let pages = json!({"pages":[{"frame":0,"items":[
            {"ref":"text","nodeId":"doc:1","name":"Digital Units","role":"th","actionable":true,"inView":true,"column":{"name":"Digital Units","index":3,"group":"M Science"}},
            {"ref":"icon","nodeId":"doc:2","name":"","role":"span","actionable":true,"inView":true,"icon":"sort caret","column":{"name":"Digital Units","index":3,"group":"M Science"}}]}]});
        let all = hints(&pages, &HashSet::new());
        let icon = all.iter().find(|c| c.action["ref"] == "icon").unwrap();
        for part in ["无名表头图标", "icon=sort caret", "列=Digital Units#3", "分组=M Science"] { assert!(icon.label.contains(part), "{part}: {}", icon.label); }
    }

    #[test]
    fn action_effect_separates_tooltips_state_changes_and_no_ops() {
        let before = json!({"pages":[{"frame":0,"url":"https://x.test/a","visibleText":"Top Charts\nDigital Units","items":[
            {"ref":"h","nodeId":"doc:1","name":"Digital Units","role":"th","actionable":true,"inView":true,"sort":"none"}]}]});
        assert!(action_effect(&before, &before)["summary"].as_str().unwrap().contains("没有可见变化"));
        let mut tooltip = before.clone();
        tooltip["pages"][0]["visibleText"] = json!("Top Charts\nDigital Units\nDefinition: Game units made through online purchase");
        let effect = action_effect(&before, &tooltip);
        assert_eq!(effect["summary"], "");
        assert!(effect["newText"][0].as_str().unwrap().starts_with("Definition"));
        assert_eq!(effect["changed"], json!([]));
        let mut sorted = before.clone();
        sorted["pages"][0]["items"][0]["sort"] = json!("descending");
        sorted["pages"][0]["url"] = json!("https://x.test/a?order=desc");
        let effect = action_effect(&before, &sorted);
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
    fn authorized_text_binds_to_its_field_and_passwords_are_never_offered() {
        let pages = json!({"pages":[{"frame":1,"items":[
            {"ref":"branch","nodeId":"doc:1","name":"","role":"input","fieldContext":"GIT_BRANCH","editable":true,"inView":true},
            {"ref":"secret","name":"GIT_BRANCH","role":"input","password":true,"editable":true,"inView":true}]}]});
        let all = hints(&pages, &HashSet::new());
        assert_eq!(all.len(), 1);
        assert!(all[0].editable && all[0].kind() == "click" && all[0].action["ref"] == "branch");
        let p = plan(json!({"task":"发布","inputs":[{"name":"GIT_BRANCH","text":"version/260924/main"}],
            "steps":[{"action":"fill","target":"GIT_BRANCH 输入框","text":"version/260924/main"}]}));
        assert_eq!(rank_for_step(&p.steps[0], &p, &all).len(), 1);
        assert_eq!(bound(&p, 0, &pages).unwrap().action, json!({"action":"fill","frame":1,"ref":"branch","text":"version/260924/main"}));
        let mut args = json!({"plan":{"task":"t","steps":[{"action":"press","key":"Enter"}],"inputs":[{"name":"","text":"x"}]}});
        assert!(parse(&args, true).is_err(), "nameless inputs need a role or fieldContext");
        args["plan"]["inputs"][0]["fieldContext"] = json!("Week ~ [field 1/2]");
        assert!(parse(&args, true).is_ok());
    }

    #[test]
    fn used_actions_are_not_replayed_and_plans_are_bounded() {
        let pages = json!({"pages":[{"frame":0,"url":"https://example.test/","items":[
            {"name":"提交","role":"button","ref":"r","nodeId":"doc:1","inView":true}]}]});
        let used = hints(&pages, &HashSet::new()).into_iter().map(|c| c.key).collect();
        let mut fresh = pages.clone(); fresh["pages"][0]["items"][0]["ref"] = json!("fresh");
        assert!(hints(&fresh, &used).is_empty());
        let step = json!({"action":"press","key":"Enter"});
        let mut args = json!({"plan":{"task":"t","authorization":"a","expectedText":"e","steps":[step]}});
        assert_eq!(parse(&args, true).unwrap().max_actions, 32);
        for invalid in [0, 65] { args["plan"]["maxActions"] = json!(invalid); assert!(parse(&args, true).is_err()); }
        args["plan"]["maxActions"] = json!(8);
        args["plan"]["steps"] = json!(vec![step; 25]);
        assert!(parse(&args, true).is_err());
        assert!(origin(&json!({"pages":[{"url":"file:///tmp/x"}]})).is_err());
    }

    #[tokio::test]
    async fn run_action_scope_does_not_leak_to_other_tasks_or_after_return() {
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
        let pages = json!({"pages":[{"frame":0,"url":"https://example.test/","viewport":{"scrollY":0,"height":600},
            "documentSize":{"height":1800},"items":[{"ref":"list","nodeId":"d:1","name":"Results","role":"div",
                "inView":true,"scroll":{"top":200,"height":1000,"viewportHeight":200}}]}]});
        let all = hints(&pages, &HashSet::new());
        assert_eq!(all.len(), 3);
        assert!(all.iter().all(|c| c.kind() == "scroll" && c.action["x"].is_null()));
        assert_eq!(all.iter().filter(|c| c.action["ref"].is_null() && c.action["delta"] == 480).count(), 1);
        let used = all.iter().map(|c| c.key.clone()).collect();
        assert!(hints(&pages, &used).is_empty());
        let mut next = pages.clone(); next["pages"][0]["viewport"]["scrollY"] = json!(1200);
        let remaining = hints(&next, &used);
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].action["delta"], -480);
    }

    fn bound(plan: &Plan, index: usize, pages: &Value) -> Option<Candidate> {
        let all = hints(pages, &HashSet::new());
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
        // Ambiguity is never guessed locally: two equally named controls go back to the main model.
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
        let all = hints(&pages, &HashSet::new());
        // The authorized text binds only to its own field: the global box is not even ranked.
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
        assert!(needs_commit(&plan(interagency.clone()), &history, &pages).is_some());
        // Summary pill shows the filter applied → quiet even with the box still ticked.
        let mut applied_pages = pages.clone();
        applied_pages["pages"][0]["items"].as_array_mut().unwrap().push(
            json!({"ref":"s","nodeId":"s:1","name":"Country/Market Germany","role":"div","inView":true}));
        assert!(needs_commit(&plan(interagency.clone()), &history, &applied_pages).is_none());
        // Clicked already → quiet.
        let mut done = history.clone();
        done.push(json!({"actions":["第2步 点击 \"Confirm\" [button]"],"status":"executed","completedActions":1,"effect":{}}));
        assert!(needs_commit(&plan(interagency), &done, &pages).is_none());
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
        let checkbox = Candidate::new(json!({"action":"click"}), json!({"role":"checkbox"}), json!(0), String::new(), "");
        let button = Candidate::new(json!({"action":"click"}), json!({"role":"button"}), json!(0), String::new(), "");
        assert!(chains(&plan.steps[0], &checkbox));
        assert!(chains(&plan.steps[1], &checkbox));
        assert!(!chains(&plan.steps[2], &button));
        assert!(!chains(&plan.steps[3], &press_candidate(&plan.steps[3], 3)));
        assert!(!chains(&plan.steps[4], &checkbox), "a step with an expectation is always checked before continuing");
        for invalid in [json!({"action":"press"}), json!({"action":"fill","target":"x"}), json!({"action":"hover","target":"x"}),
            json!({"action":"click"}), json!({"action":"click","target":"x","repeat":4}), json!({"action":"press","key":"F5"})] {
            assert!(parse(&json!({"plan":{"task":"t","authorization":"a","expectedText":"e","steps":[invalid]}}), true).is_err());
        }
        let pages = json!({"pages":[{"url":"https://x.test/?order=desc","visibleText":"Region United States","title":"Top"}]});
        assert!(literal_met(&pages, "美国|United States") && literal_met(&pages, "order=desc") && !literal_met(&pages, "Canada"));
    }

    #[test]
    fn semantic_controls_remain_available_beside_canvas() {
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
        let refs: HashSet<String> = hints(&pages, &HashSet::new()).iter().map(|c| c.action["ref"].as_str().unwrap().to_string()).collect();
        assert_eq!(refs, HashSet::from(["r", "c", "p", "s", "custom"].map(String::from)));
    }

    #[test]
    fn route_save_is_storable_without_typed_values() {
        let steps: Vec<Value> = (0..14).map(|i| json!({"action":"fill","target":format!("第{i}个框"),"text":"secret-value"})).collect();
        let p = plan(json!({"task":"查询","authorization":"只读","expectedText":"出现结果","steps":steps}));
        let save = route_save(&p, &json!({"pages":[{"url":"https://a.com/project/42/list?q=1"}]}), "https://a.com", "snap");
        let e = &save["experience"];
        assert_eq!((e["steps"].as_array().unwrap().len(), e["conditions"][0].clone()), (7, json!("起始页面 /project/*/list")));
        assert!(!save.to_string().contains("secret-value"));
    }
}
