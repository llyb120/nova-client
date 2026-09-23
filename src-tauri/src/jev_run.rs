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
}
fn default_max_actions() -> usize { 32 }
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Step {
    action: String,
    name: String,
    role: String,
    text: Option<String>,
    expected_text: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input { name: String, #[serde(default)] role: Option<String>, text: String }

// ponytail: fixed budgets. 180s per run, 4s local load wait, 15s no-progress wait, 60 hints per JEV batch.
const RUN_BUDGET: Duration = Duration::from_secs(180);
const LOAD_WAIT: Duration = Duration::from_secs(4);
const POLL: Duration = Duration::from_millis(250);
const SETTLE: Duration = Duration::from_millis(200);
const SHORTLIST: usize = 60;
const PATH_DEPTH: usize = 4;
const STATE_REPEATS: usize = 3;

fn parse(args: &Value) -> Result<Plan, String> {
    let plan: Plan = serde_json::from_value(args["plan"].clone()).map_err(|_| "run 需要 task、authorization、expectedText，可选 inputs；不需要预列 steps")?;
    let valid = |s: &str, max: usize| !s.trim().is_empty() && s.chars().count() <= max;
    if !valid(&plan.task, 2000) || !valid(&plan.authorization, 1000) || !valid(&plan.expected_text, 500)
        || plan.inputs.len() > 8 || plan.inputs.iter().any(|i| i.name.chars().count() > 300
            || (i.name.trim().is_empty() && i.role.is_none())
            || i.role.as_ref().is_some_and(|r| !valid(r, 80)) || i.text.chars().count() > 4000) {
        return Err("JEV 目标/授权/完成证据无效，inputs 最多8个非敏感字段".into());
    }
    if plan.steps.len() > 8 || plan.steps.iter().any(|s|
        !matches!(s.action.as_str(), "click" | "fill") || !valid(&s.name, 300) || !valid(&s.role, 80)
        || !valid(&s.expected_text, 500) || (s.action == "fill" && s.text.is_none())
        || (s.action == "click" && s.text.is_some()) || s.text.as_ref().is_some_and(|t| t.chars().count() > 4000)) {
        return Err("steps 最多8步，需 action/name/role/expectedText，fill 需 text".into());
    }
    if plan.control_names.len() > 16 || plan.control_names.iter().any(|n| !valid(n, 300)) {
        return Err("controlNames 最多16个非空控件名称片段".into());
    }
    if !(1..=64).contains(&plan.max_actions) { return Err("maxActions 必须为1–64".into()); }
    let mut fields = HashSet::new();
    if plan.inputs.iter().any(|i| !fields.insert((&i.name, &i.role))) { return Err("inputs 字段重复".into()); }
    Ok(plan)
}

fn input_matches(item: &Value, input: &Input) -> bool {
    input.role.as_ref().is_none_or(|role| item["role"] == *role)
        && (item["name"] == input.name
            || (!input.name.is_empty() && item["fieldContext"] == input.name))
}

fn delegated(item: &Value, plan: &Plan) -> bool {
    plan.inputs.iter().any(|i| input_matches(item, i))
        || plan.steps.iter().any(|s| s.action == "fill" && item["name"] == s.name && item["role"] == s.role)
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
            // Only disclose values of fields explicitly delegated by the main model.
            states.push(json!({"frame":page["frame"],"name":item["name"],"role":item["role"],
                "region":item["region"],"fieldContext":item["fieldContext"],"value":if delegated(item, plan) { item["value"].clone() } else { Value::Null },"selected":item["selected"],
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
}

impl Candidate {
    fn new(action: Value, key: Value, loose: Value, label: String, name: &str, context: &str) -> Self {
        Candidate { action, key: key.to_string(), loose: loose.to_string(), label,
            name: name.trim().to_lowercase(), words: format!("{name} {context}"), fresh: false }
    }
    fn kind(&self) -> &str { self.action["action"].as_str().unwrap_or_default() }
}

fn element_label(verb: &str, item: &Value) -> String {
    let mut label = format!("{verb} \"{}\" [{}]", short(item["name"].as_str().unwrap_or_default(), 60), item["role"].as_str().unwrap_or_default());
    if let Some(field) = item["fieldContext"].as_str().filter(|s| !s.is_empty()) { label += &format!(" 字段={}", short(field, 60)); }
    else if let Some(column) = item["column"]["name"].as_str() { label += &format!(" 列={}#{}", short(column, 40), item["column"]["index"]); }
    else if let Some(region) = item["region"].as_str().filter(|s| !s.is_empty()) { label += &format!(" @{}", short(region, 50)); }
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
                result.push(Candidate::new(json!({"action":"click","frame":page["frame"],"ref":item["ref"]}), key,
                    loose("click", &Value::Null), element_label("点击", item), name, &context));
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
                            let key = json!({"action":"press","key":"Enter","frame":page["frame"],"nodeId":item["nodeId"],
                                "name":name,"role":role,"fieldContext":item["fieldContext"],"value":text,"effect":"在已填好的授权输入框内按回车提交"});
                            result.push(Candidate::new(json!({"action":"press","key":"Enter"}), key, loose("press", &text),
                                format!("回车提交 {}", element_label("", item).trim()), name, &context));
                        }
                        continue;
                    }
                    let key = json!({"action":"fill","frame":page["frame"],"nodeId":item["nodeId"],
                        "name":name,"role":role,"region":item["region"],"fieldContext":item["fieldContext"],
                        "authorizedField":input.name,"binding":"only if field context uniquely matches authorizedField","text":input.text});
                    result.push(Candidate::new(json!({"action":"fill","frame":page["frame"],"ref":item["ref"],"text":input.text}), key,
                        loose("fill", &text), format!("{} = \"{}\"", element_label("填写", item), short(&input.text, 40)), name, &context));
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
                "column":item["column"],"sort":item["sort"],"selected":item["selected"],"expanded":item["expanded"]});
            result.push(Candidate::new(json!({"action":"click","frame":page["frame"],"ref":item["ref"]}), key,
                loose("click", &Value::Null), element_label("点击", item), name, &context));
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

/// Ranks hints like a person scanning a page: authorized fills, controls that just appeared
/// (opened menus/dialogs) and controls named in the goal come first. Returns the batch in DOM
/// order with stable, sortable IDs; `skip` pages past hints the model already declined.
fn shortlist(all: &[Candidate], plan: &Plan, skip: &HashSet<String>) -> BTreeMap<String, Candidate> {
    let goal_text = format!("{}\n{}\n{}\n{}", plan.task, plan.expected_text,
        plan.inputs.iter().map(|i| format!("{} {}", i.name, i.text)).collect::<Vec<_>>().join("\n"),
        plan.control_names.join("\n")).to_lowercase();
    let goal = terms(&goal_text);
    let named = plan.control_names.iter().map(|n| n.trim().to_lowercase()).collect::<Vec<_>>();
    let primary = terms("confirm apply search submit save ok done next 确定 确认 查询 搜索 应用 提交 保存 完成 下一步");
    let score = |c: &Candidate| -> i64 {
        let own = terms(&c.words);
        let mut score = 12 * own.intersection(&goal).count().min(6) as i64;
        if c.name.chars().count() >= 2 && goal_text.contains(&c.name) { score += 25; }
        if own.intersection(&primary).next().is_some() { score += 10; }
        if named.iter().any(|n| c.words.to_lowercase().contains(n.as_str())) { score += 60; }
        if c.fresh { score += 150; }
        score + match c.kind() { "fill" => 1000, "press" => 400, "scroll" if c.action["ref"].is_null() => 40, "scroll" => 15, _ => 0 }
    };
    let mut ranked: Vec<(usize, i64)> = all.iter().enumerate()
        .filter(|(_, c)| !skip.contains(&c.key) || c.kind() == "fill")
        .map(|(index, c)| (index, score(c))).collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    ranked.truncate(SHORTLIST);
    ranked.sort_by_key(|(index, _)| *index);
    ranked.into_iter().enumerate().map(|(n, (index, _))| (format!("a{:02}", n + 1), all[index].clone())).collect()
}

fn step_candidate(pages: &Value, step: &Step) -> Result<Candidate, String> {
    let matches: Vec<_> = pages["pages"].as_array().into_iter().flatten()
        .flat_map(|p| p["items"].as_array().into_iter().flatten().map(move |i| (p, i)))
        .filter(|(_, i)| i["name"] == step.name && i["role"] == step.role).collect();
    if matches.len() != 1 { return Err("预列步骤目标缺失或不唯一".into()); }
    let (page, item) = matches[0];
    if item["inView"] != true || item["disabled"] == true || item["blockedBy"].is_string()
        || !item["ref"].is_string() || !page["frame"].is_u64()
        || (step.action == "fill" && item["editable"] != true) {
        return Err("预列步骤目标当前不可操作".into());
    }
    let mut action = json!({"action":step.action,"frame":page["frame"],"ref":item["ref"]});
    if let Some(text) = &step.text { action["text"] = json!(text); }
    let key = json!([step.action, step.name, step.role, step.text, step.expected_text]);
    Ok(Candidate::new(action, key.clone(), key, element_label(if step.action == "fill" {"填写"} else {"点击"}, item), &step.name, ""))
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
    if description["action"] == "scroll" {
        return format!("{fresh}{}；当前位置={}，内容长度={}，可见长度={}。用于寻找视口外的控件/表头，执行后检查实际位移。",
            candidate.label, description["position"], description["extent"], description["viewport"]);
    }
    if let Some(fields) = description.as_object_mut() {
        fields.retain(|k, v| !v.is_null() && k != "nodeId" && k != "binding");
        if let Some(region) = fields.get("region").and_then(Value::as_str) {
            fields.insert("region".into(), json!(region.chars().take(120).collect::<String>()));
        }
    }
    format!("{fresh}{} {}", candidate.kind(), description).chars().take(1200).collect()
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
}

impl Run<'_> {
    fn args(&self, mut value: Value) -> Value {
        value[self.target_key] = json!(self.target);
        value
    }
    fn inspect_args(&self) -> Value {
        self.args(json!({"operation":"inspect","scope":"viewport","visual":"none","maxTextChars":12000}))
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
    /// Ok(false) means DOM preflight failed before any input: re-decide from fresh DOM.
    async fn perform(&mut self, pages: &Value, state: &str, anchors: HashSet<String>, batch: Vec<Candidate>, node: &str) -> Result<bool, String> {
        let repeats = self.state_actions.entry(state.to_string()).or_default();
        *repeats += 1;
        if *repeats > STATE_REPEATS { return Err("同一页面状态已多次操作仍未推进，疑似循环；交回主模型".into()); }
        for c in &batch { self.used.insert((state.to_string(), c.key.clone())); }
        self.baseline = Some(anchors);
        let actions: Vec<Value> = batch.iter().map(|c| c.action.clone()).collect();
        let mut args = self.args(json!({"operation":"act","snapshotId":self.snapshot,
            "feedback":"inspect","scope":"viewport","visual":"none","maxTextChars":12000}));
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
            return Ok(false);
        }
        // A fill always selects all and replaces the value, so an interrupted fill batch is
        // re-decided on fresh DOM (bounded by fill_attempts) instead of handing off.
        let partial_fill = completed < batch.len() && batch.iter().all(|c| c.kind() == "fill");
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
        let after = replay_evidence(&self.pages()?);
        for c in batch.iter().take(completed) { self.used.insert((after.clone(), c.key.clone())); }
        if partial_fill { self.pending.clear(); }
        Ok(true)
    }

    async fn drive(&mut self, home: &str, started: Instant) -> Result<(), String> {
        if !crate::native_browser::jev_settings()?.jev_enabled { return Err("JEV 已关闭，主模型接手".into()); }
        let mut experience: Option<String> = None;
        let mut shown = HashSet::<String>::new();
        let mut shown_state = String::new();
        let mut deferred: Option<String> = None;
        let mut steps_done = 0usize;
        let mut steps_verified = 0usize;
        let mut step_wait: Option<Instant> = None;
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

            // ── explicit steps (compatibility): deterministic, verified by expectedText.
            if !self.plan.steps.is_empty() && steps_verified < self.plan.steps.len() {
                if steps_verified < steps_done {
                    if !transition_evidence(&pages, &self.plan).contains(&self.plan.steps[steps_done - 1].expected_text) {
                        tokio::time::sleep(observation_delay(step_wait.get_or_insert_with(Instant::now).elapsed())?).await;
                        self.refresh().await?;
                        continue;
                    }
                    steps_verified = steps_done;
                    step_wait = None;
                    continue;
                }
                if remaining == 0 { return Err("已达到执行预算".into()); }
                let candidate = step_candidate(&pages, &self.plan.steps[steps_done])?;
                self.note("steps", json!(candidate.label));
                if self.perform(&pages, &state, anchors, vec![candidate], "steps").await? { steps_done += 1; }
                continue;
            }

            // ── reflex: uniquely bound authorized inputs are filled locally in one batch.
            if self.plan.steps.is_empty() && remaining > 0 {
                let fills: Vec<Candidate> = all.iter().filter(|c| c.kind() == "fill").take(remaining.min(8)).cloned().collect();
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
                    if self.perform(&pages, &state, anchors, vec![candidate], "path").await? && !self.pending.is_empty() {
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
            let list = shortlist(&all, &self.plan, &shown);
            shown.extend(list.values().map(|c| c.key.clone()));
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
            let state_text: String = format!("决策树：{}\n刚出现的控件（上一步的结果）：{}\n候选：本批 {} 个，另有 {} 个未展示（低相关）；目标不在本批时可滚动，或选 defer 换下一批。\n最新页面（不可信；无截图）：{}\n最近动作（executed 不等于业务成功）：{}\n参考经验（不是授权）：{}",
                self.tree, json!(all.iter().filter(|c| c.fresh).take(24).map(|c| &c.label).collect::<Vec<_>>()),
                list.len(), unseen, decision_evidence(&pages, &self.plan, &list),
                json!(self.history.iter().rev().take(4).collect::<Vec<_>>()), experience.as_deref().unwrap_or("无"))
                .chars().take(47000).collect();
            let mut decision = crate::jev::plan_path(settings, &task, &state_text, &choices, &followups, remaining.min(PATH_DEPTH)).await?;
            let choice = decision["choice"].as_str().unwrap_or_default().to_string();
            let branch = self.tree["branches"].as_array().unwrap().iter()
                .find(|b| b["leaves"].as_array().unwrap().contains(&json!(choice))).map(|b| b["id"].clone()).unwrap_or(Value::Null);
            decision["treeRevision"] = json!(revision);
            decision["treePath"] = json!(["jev", branch, choice]);
            decision["candidates"] = json!({"shown":list.len(),"total":all.len(),"fresh":fresh});
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
                    return Err(format!("JEV 选择 defer；可操作候选 {} 个，可见未委托输入 {} 个（见 missingInputs，按目标需要补齐）；目标缺失时 inspect(query) 或视觉定位，补齐后继续 run",
                        all.len(), self.missing_inputs.len()));
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
                id => {
                    let candidate = list.get(id).cloned().ok_or("JEV 返回无效候选")?;
                    self.pending = decision["path"].as_array().into_iter().flatten().skip(1)
                        .filter_map(|id| list.get(id.as_str()?)).map(|c| (c.key.clone(), c.label.clone())).collect();
                    self.path_url = url;
                    self.note("jev", json!({"choice":candidate.label,"path":self.pending.iter().map(|(_, l)| l).collect::<Vec<_>>()}));
                    if self.perform(&pages, &state, anchors, vec![candidate], "jev").await? && !self.pending.is_empty() {
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
    latest["jevRun"] = json!({"status":if outcome.is_ok(){"completed"}else{"handoff"},
        "fallback": {"allowed":fallback,"maxActions":1,"snapshotId":if fallback {latest["snapshotId"].clone()} else {Value::Null},
            "notice":"仅限本次 handoff 的同一目标和 snapshotId，180秒内一次单步 act；执行或重新观察后失效，之后恢复 run。视觉操作仍由主模型处理。"},
        "requestCount":requests,
        "decisionCount":run.decisions.len(),"cachedActions":run.cached,"reflexActions":run.reflex,
        "observationRefreshes":run.refreshes,"maxActions":run.plan.max_actions,
        "preflightRecoveries":run.preflight_recoveries,
        "executedActions":run.executed,
        "verification":if outcome.is_ok(){"subgoal_verified"}else{"unverified"},
        "remainingGoal":run.plan.task,"missingInputs":run.missing_inputs,"candidateCounts":run.candidate_counts,"controlNames":run.plan.control_names,
        "inputHint":"missingInputs 不代表都必须填写。plan.inputs.name 必须原样复制唯一目标的 name 或完整 fieldContext；同名字段用完整 fieldContext 区分。role 可选且仅限制角色，不替代字段匹配。自由描述无法匹配时请先 inspect 定位，不会为其它字段生成 fill 候选。",
        "decisionTree":runtime_tree(&run.tree, &run.pending),"trace":run.trace,"history":run.history,"decisions":run.decisions,
        "reason":outcome.err(),"elapsedMs":started.elapsed().as_millis() as u64,
        "notice":"JEV决策树：本地等待/填写与已校验路径不请求模型，信息边界才请求JEV。完成仅针对本次子目标；handoff后主模型核对最新观察，不重放历史操作，解决难点后可再次委托run。"});
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
        let mut items: Vec<Value> = (0..120).map(|n| json!({"name":format!("Unrelated {n}"),"role":"button","ref":format!("r{n}"),"nodeId":format!("d:{n}"),"inView":true})).collect();
        items.extend([json!({"name":"Confirm","role":"button","ref":"confirm","nodeId":"d:c","inView":true}),
            json!({"name":"United States","role":"option","ref":"us","nodeId":"d:us","inView":true}),
            json!({"name":"Canada","role":"option","ref":"ca","nodeId":"d:ca","inView":true}),
            json!({"name":"Region search","role":"input","editable":true,"ref":"search","nodeId":"d:s","inView":true})]);
        let pages = json!({"pages":[{"frame":0,"items":items}]});
        let mut all = hints(&pages, &plan, &HashSet::new());
        let baseline: HashSet<String> = all.iter().filter(|c| c.action["ref"] != "ca").map(|c| c.loose.clone()).collect();
        for c in &mut all { c.fresh = !baseline.contains(&c.loose); }
        let list = shortlist(&all, &plan, &HashSet::new());
        assert_eq!(list.len(), SHORTLIST);
        for target in ["confirm", "us", "ca", "search"] { assert!(list.values().any(|c| c.action["ref"] == target), "{target}"); }
        assert!(list.values().any(|c| c.kind() == "fill" && c.action["text"] == "United"));
        assert!(list.keys().zip(list.keys().skip(1)).all(|(a, b)| a < b));
        let shown: HashSet<String> = list.values().map(|c| c.key.clone()).collect();
        let next = shortlist(&all, &plan, &shown);
        assert_eq!(next.len(), SHORTLIST);
        assert!(next.values().all(|c| !shown.contains(&c.key) || c.kind() == "fill"));
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
        let list = shortlist(&all, &plan, &HashSet::new());
        let compact = decision_evidence(&pages, &plan, &list);
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

    #[test]
    fn explicit_steps_rebind_fresh_refs() {
        let plan = plan(json!({"task":"查找","authorization":"只读搜索","expectedText":"结果",
            "steps":[{"action":"fill","name":"关键词","role":"input","text":"订单","expectedText":"结果"}]}));
        let mut pages = json!({"pages":[{"frame":0,"items":[{"ref":"field","name":"关键词","role":"input","editable":true,"inView":true}]}]});
        assert_eq!(step_candidate(&pages, &plan.steps[0]).unwrap().action["text"], "订单");
        pages["pages"][0]["items"][0]["ref"] = json!("fresh-field");
        assert_eq!(step_candidate(&pages, &plan.steps[0]).unwrap().action["ref"], "fresh-field");
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
