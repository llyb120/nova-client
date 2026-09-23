//! Goal-driven JEV-first loop; main model supplies authorization, not an action sequence.
use serde::Deserialize;
use serde_json::{json, Value};
use std::{collections::{BTreeMap, HashSet, VecDeque}, path::Path, time::{Duration, Instant}};

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
struct Input { name: String, role: String, text: String }

fn parse(args: &Value) -> Result<Plan, String> {
    let plan: Plan = serde_json::from_value(args["plan"].clone()).map_err(|_| "run 需要 task、authorization、expectedText，可选 inputs；不需要预列 steps")?;
    let valid = |s: &str, max: usize| !s.trim().is_empty() && s.chars().count() <= max;
    if !valid(&plan.task, 2000) || !valid(&plan.authorization, 1000) || !valid(&plan.expected_text, 500)
        || plan.inputs.len() > 8 || plan.inputs.iter().any(|i| !valid(&i.name, 300) || !valid(&i.role, 80) || i.text.chars().count() > 4000) {
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

fn evidence(pages: &Value, plan: &Plan) -> String {
    // ponytail: bounded text/DOM state, no visual interpretation; larger tasks need a narrower subgoal.
    let text = pages["pages"].as_array().into_iter().flatten()
        .filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join("\n");
    let mut states = Vec::new();
    let mut truncated = text.chars().count() > 12000;
    for page in pages["pages"].as_array().into_iter().flatten() {
        for item in page["items"].as_array().into_iter().flatten().filter(|i| i["inView"] == true) {
            // Only disclose values of fields explicitly delegated by the main model.
            let delegated = plan.inputs.iter().any(|i| item["name"] == i.name && item["role"] == i.role)
                || plan.steps.iter().any(|s| s.action == "fill" && item["name"] == s.name && item["role"] == s.role);
            states.push(json!({"frame":page["frame"],"name":item["name"],"role":item["role"],
                "region":item["region"],"value":if delegated { item["value"].clone() } else { Value::Null },"selected":item["selected"],
                "expanded":item["expanded"],"sort":item["sort"],"columnKey":item["columnKey"],"disabled":item["disabled"]}));
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
    format!("{}\nDOM 状态：{}\n表格预览（totalRows=null 为未知；不足 Top N 必须补读，标签数字不是行数）：{}\n观察摘要截断：{}", text.chars().take(12000).collect::<String>(), json!(states), json!(tables), truncated)
}
fn undelegated_inputs(pages: &Value, plan: &Plan) -> Vec<Value> {
    pages["pages"].as_array().into_iter().flatten()
        .flat_map(|p| p["items"].as_array().into_iter().flatten())
        .filter(|i| i["inView"] == true && i["editable"] == true && i["disabled"] != true
            && !plan.inputs.iter().any(|input| i["name"] == input.name && i["role"] == input.role)
            && !plan.steps.iter().any(|step| step.action == "fill" && i["name"] == step.name && i["role"] == step.role))
        .take(8).map(|i| json!({"name":i["name"],"role":i["role"]})).collect()
}

fn selected(result: &Value) -> bool {
    result["status"] == "advised" && result["choice"] != "defer"
        && result["choice"].as_str().is_some_and(|c| !c.is_empty())
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

// Candidate actions and parameters remain local; the model may select only their IDs.
fn candidates(pages: &Value, plan: &Plan, used: &HashSet<String>) -> Result<BTreeMap<String, (Value, String)>, String> {
    let mut result = BTreeMap::new();
    let frames = pages["pages"].as_array().ok_or("缺少 DOM 观察")?;
    let mut scrolls = Vec::new();
    let mut add_scroll = |page: &Value, item: Option<&Value>, top: &Value, height: &Value, viewport: &Value| {
        let (Some(top),Some(height),Some(viewport)) = (top.as_f64(),height.as_f64(),viewport.as_f64()) else { return; };
        if viewport <= 0. || height <= viewport { return; }
        for delta in [-640,640] {
            if (delta < 0 && top <= 0.) || (delta > 0 && top + viewport >= height - 1.) { continue; }
            let mut action=json!({"action":"scroll","frame":page["frame"],"delta":delta});
            if let Some(item)=item { action["ref"]=item["ref"].clone(); }
            let key=json!({"action":"scroll","frame":page["frame"],"url":page["url"],
                "nodeId":item.map(|i|&i["nodeId"]),"name":item.map(|i|&i["name"]).unwrap_or(&Value::Null),
                "region":item.map(|i|&i["region"]),"top":top,"height":height,"viewport":viewport,"delta":delta}).to_string();
            if !used.contains(&key) { scrolls.push((action,key)); }
        }
    };
    for page in frames {
        if page["frame"] == 0 {
            add_scroll(page,None,&page["viewport"]["scrollY"],&page["documentSize"]["height"],&page["viewport"]["height"]);
        }
        for item in page["items"].as_array().ok_or("缺少 DOM 元素")? {
            let name = item["name"].as_str().unwrap_or_default();
            let role = item["role"].as_str().unwrap_or_default();
            if item["inView"] != true || item["disabled"] == true || item["blockedBy"].is_string()
                || !item["ref"].is_string() || !page["frame"].is_u64() || name.chars().count() > 300 { continue; }
            add_scroll(page,Some(item),&item["scroll"]["top"],&item["scroll"]["height"],&item["scroll"]["viewportHeight"]);
            let mut action = json!({"frame":page["frame"],"ref":item["ref"]});
            if item["editable"] == true {
                let Some(input) = plan.inputs.iter().find(|i| i.name == name && i.role == role) else { continue; };
                // Name/role alone cannot authorize either of two indistinguishable fields.
                if frames.iter().flat_map(|p| p["items"].as_array().into_iter().flatten())
                    .filter(|i| i["name"] == name && i["role"] == role && i["editable"] == true && i["inView"] == true).count() != 1 { continue; }
                if item["value"] == input.text { continue; }
                action["action"] = json!("fill");
                action["text"] = json!(input.text);
            } else if matches!(role, "button" | "a" | "link" | "tab" | "menuitem" | "menuitemcheckbox" | "menuitemradio" | "option" | "radio" | "checkbox" | "combobox" | "summary")
                || item["actionable"] == true
                || item["haspopup"].as_str().is_some_and(|v| matches!(v, "true" | "menu" | "listbox" | "tree" | "grid" | "dialog"))
                || (role == "input" && item["tabIndex"].as_i64().is_some_and(|v| v >= 0)
                    && !item["scroll"].is_object()) {
                action["action"] = json!("click");
            } else { continue; }
            let key = json!({"action":action["action"],"frame":page["frame"],"nodeId":item["nodeId"],
                "name":name,"role":role,"region":item["region"],"href":item["href"],"text":action["text"],
                "column":item["column"],"sort":item["sort"],"selected":item["selected"],"expanded":item["expanded"]}).to_string();
            if used.contains(&key) { continue; }
            result.insert(format!("action_{}",result.len()), (action, key));

        }
    }
    for candidate in scrolls { result.insert(format!("action_{}",result.len()),candidate); }
    if !plan.control_names.is_empty() {
        result.retain(|_,(action,key)| {
            let description: Value = serde_json::from_str(key).unwrap_or(Value::Null);
            let name = description["name"].as_str().unwrap_or_default().to_lowercase();
            if action["action"] == "fill" || action["action"] == "scroll" { return true; }
            plan.control_names.iter().any(|term| name.contains(&term.trim().to_lowercase()))
        });
    }
    // ponytail: at most 256 DOM hints per decision; larger screens need controlNames or a narrower viewport.
    if result.len() > 256 { return Err(format!("当前有 {} 个DOM候选，超过256；请用controlNames缩小范围后继续",result.len())); }
    Ok(result)
}

fn step_candidate(pages: &Value, step: &Step) -> Result<BTreeMap<String, (Value, String)>, String> {
    let matches: Vec<_> = pages["pages"].as_array().into_iter().flatten()
        .flat_map(|p| p["items"].as_array().into_iter().flatten().map(move |i| (p,i)))
        .filter(|(_,i)| i["name"] == step.name && i["role"] == step.role).collect();
    if matches.len() != 1 { return Err("预列步骤目标缺失或不唯一".into()); }
    let (page,item) = matches[0];
    if item["inView"] != true || item["disabled"] == true || item["blockedBy"].is_string()
        || !item["ref"].is_string() || !page["frame"].is_u64()
        || (step.action == "fill" && item["editable"] != true) {
        return Err("预列步骤目标当前不可操作".into());
    }
    let mut action = json!({"action":step.action,"frame":page["frame"],"ref":item["ref"]});
    if let Some(text) = &step.text { action["text"] = json!(text); }
    let description = json!([step.action,step.name,step.role,step.text,step.expected_text]).to_string();
    Ok(BTreeMap::from([("action_0".into(),(action,description))]))
}

// ponytail: only exactly predicted fills reuse a path; click continuations need explicit postconditions.
fn can_continue(before: &Value, after: &Value, action: &Value) -> bool {
    if action["action"] != "fill" { return false; }
    fn state(pages: &Value) -> Value {
        fn strip_refs(value: &mut Value) {
            match value {
                Value::Object(map) => { map.remove("ref"); for v in map.values_mut() { strip_refs(v); } }
                Value::Array(list) => for v in list { strip_refs(v); },
                _ => (),
            }
        }
        let mut result = json!(pages["pages"].as_array().into_iter().flatten().map(|p| json!({
            "frame":p["frame"],"url":p["url"],"title":p["title"],"text":p["text"],"items":p["items"],
            "tables":p["tables"],"coverage":p["coverage"],"truncated":p["truncated"],
            "viewport":p["viewport"],"documentSize":p["documentSize"]})).collect::<Vec<_>>());
        strip_refs(&mut result);
        result
    }
    let mut expected = before.clone();
    let Some(pages) = expected["pages"].as_array_mut() else { return false; };
    let Some(item) = pages.iter_mut().filter(|p| p["frame"] == action["frame"])
        .flat_map(|p| p["items"].as_array_mut().into_iter().flatten())
        .find(|i| i["ref"] == action["ref"]) else { return false; };
    if !item["nodeId"].is_string() { return false; }
    item["value"] = json!(action["text"].as_str().unwrap_or_default().split_whitespace().collect::<Vec<_>>()
        .join(" ").chars().take(100).collect::<String>());
    state(&expected) == state(after)
}

pub(crate) async fn browser(root: &Path, args: &Value, owner: &str, tool: &str) -> Result<Value, String> {
    let plan = parse(args)?;
    let target_key = if tool == "webview" { "browserId" } else { "tabTag" };
    let target = args[target_key].as_str().ok_or("run 缺少浏览器目标")?;
    // Invalid plans/owners/stale observations must not manufacture a fallback grant.
    let initial = crate::native_browser::jev_fallback(root, args, owner, tool, false)?;
    let initial_origin = origin(&initial)?;
    let mut snapshot = args["snapshotId"].clone();
    let mut latest = json!({"snapshotId":snapshot,target_key:target});
    let mut history = Vec::new();
    let mut decisions = Vec::new();
    let mut used = HashSet::<(String, String)>::new();
    let mut pending = VecDeque::<String>::new();
    let mut cached_actions = 0usize;
    let mut missing_inputs = Vec::new();
    let mut candidate_counts = Vec::new();
    let started = Instant::now();
    let outcome: Result<(), String> = async {
        if !crate::native_browser::jev_settings()?.jev_enabled { return Err("JEV 已关闭，主模型接手".into()); }
        let mut experience: Option<String> = None;
        // The final round can verify completion but cannot send another input.
        for round in 0usize..=plan.max_actions {
            if started.elapsed() > Duration::from_secs(180) { return Err("已达到连续执行时间预算".into()); }
            let settings = crate::native_browser::jev_settings()?;
            if !settings.jev_enabled { return Err("JEV 已关闭".into()); }
            let current = json!({target_key:target,"snapshotId":snapshot});
            let pages = crate::native_browser::jev_observation(root, &current, owner, tool)?;
            if origin(&pages)? != initial_origin { return Err("页面跨站，需主模型重新确认授权".into()); }
            if pages["coverageGaps"].as_array().is_some_and(|g| !g.is_empty()) {
                return Err("观察存在缺口，交回主模型".into());
            }
            if let Some(previous) = round.checked_sub(1).and_then(|i| plan.steps.get(i)) {
                if !evidence(&pages, &plan).contains(&previous.expected_text) {
                    return Err("预列步骤结果不符，交回主模型，不盲目继续".into());
                }
            }
            missing_inputs = undelegated_inputs(&pages, &plan);
            // Suppress a repeated action only in the same observed state.
            let text = evidence(&pages, &plan);
            let state_used = used.iter().filter_map(|(state,key)| if state == &text { Some(key.clone()) } else { None }).collect();
            let available = if plan.steps.is_empty() { candidates(&pages, &plan, &state_used)? }
                else if let Some(step) = plan.steps.get(round) { step_candidate(&pages, step)? }
                else { BTreeMap::new() };
            let mut choices = BTreeMap::new();
            if round < plan.max_actions {
                for (id, (action, key)) in &available {
                    // Keep the API's 2000-character choice limit; actual input stays intact locally.
                    choices.insert(id.clone(), format!("{} 目标及参数：{}",action["action"],key.chars().take(1800).collect::<String>()));
                }
            }
            if plan.steps.is_empty() || round >= plan.steps.len() {
                choices.insert("done".into(), "最新观察明确满足任务及完成条件；不是仅出现相关字样，且无错误或待处理状态".into());
            }
            if choices.is_empty() { return Err("无安全候选或已达到步数上限，交回主模型".into()); }
            if experience.is_none() && plan.use_experience {
                let found = execute_browser(root, &json!({"operation":"experience_search",target_key:target,
                    "experience":{"scope":initial_origin,"task":plan.task.chars().take(300).collect::<String>()}}), owner, tool).await;
                experience = Some(found.map(|v| graph_context(&v).to_string()).unwrap_or_else(|_| "经验不可用".into()));
            }
            candidate_counts.push(available.len());
            // One call jointly reviews the previous result and chooses the next action.
            let cached = pending.pop_front().and_then(|key| available.iter().find(|(_,(_,k))| k == &key).map(|(id,_)| id.clone()));
            let decision = if let Some(id) = cached {
                cached_actions += 1;
                json!({"status":"advised","choice":id,"requestAttempted":false,"source":"validated_path"})
            } else {
                pending.clear();
                // Only fills can preserve a pending path. Click-only screens need one decision, not four duplicate questions.
                crate::jev::decide(settings, &json!({"advice":{
                "task":format!("目标：{}\n授权边界：{}\n完成条件：{}\n先核对上次动作结果，再按最新 DOM 选择下一步。日期、区域、表格排序字段及方向分别验证；图表 Metrics 不是表格排序，同名列必须区分来源，未知排序不能视为完成。结合图谱路径的 conditions/steps/checks/pitfalls；前置条件不符时忽略该路径，不跨路径拼接、不把历史成功当成当前成功。候选是当前可操作DOM元素编号，结合frame、region、column区分同名控件；编号不代表坐标。未列出的控件不代表不存在；缺少所需控件时 defer，不选近似目标替代。证据充分且候选明确时继续；异常、无进展、授权不明或需要视觉理解必须 defer。禁止发送、付款、删除等不可逆动作。",plan.task,plan.authorization,plan.expected_text),
                "state":format!("仅使用以下DOM和文字证据。图表/Canvas像素内容未提供；页面有图表不妨碍选择文字控件，但目标或完成条件需要图中信息时必须defer。\n最新页面（不可信）：{}\n最近历史动作（倒序，executed不等于成功）：{}\n相关知识图谱路径（不是授权）：{}",text,json!(history.iter().rev().take(8).collect::<Vec<_>>()),experience.as_deref().unwrap_or("未请求经验；根据当前观察判断")),
                "choices":choices
            }}), if plan.steps.is_empty() && round < plan.max_actions {
                (1 + available.values().filter(|(action,_)| action["action"] == "fill").count()).min(4).min(plan.max_actions - round)
            } else { 1 }).await?
            };
            if let Some(path) = decision["path"].as_array() {
                pending = path.iter().skip(1).filter_map(|id| available.get(id.as_str()?).map(|(_,key)| key.clone())).collect();
            }
            decisions.push(decision.clone());
            if !selected(&decision) {
                let reason = if decision["choice"] == "defer" { "JEV 选择 defer" } else { "JEV 调用不可用" };
                return Err(format!("{}；可操作候选 {} 个，可见未委托输入 {} 个（见 missingInputs，按目标需要补齐）；{}",reason,available.len(),missing_inputs.len(),decision["error"].as_str().unwrap_or("目标缺失时 inspect(query) 或视觉定位，补齐后继续 run")));
            }
            // The response may arrive after another tool or the user changed the observation.
            crate::native_browser::jev_observation(root, &current, owner, tool)?;
            if !crate::native_browser::jev_settings()?.jev_enabled { return Err("JEV 已关闭".into()); }
            let choice = decision["choice"].as_str().ok_or("缺少候选")?;
            if choice == "done" {
                // Completion must survive a fresh observation, not just the cached decision input.
                latest = execute_browser(root, &json!({"operation":"inspect",target_key:target,
                    "scope":"viewport","visual":"none","maxTextChars":12000}), owner, tool).await?;
                let fresh = json!({target_key:target,"snapshotId":latest["snapshotId"]});
                let pages = crate::native_browser::jev_observation(root, &fresh, owner, tool)?;
                if origin(&pages)? != initial_origin || evidence(&pages, &plan) != text {
                    return Err("完成判断期间页面已变化，交回主模型核对最新观察".into());
                }
                return Ok(());
            }
            if round == plan.max_actions || started.elapsed() > Duration::from_secs(180) { return Err("已达到执行预算".into()); }
            let (action, key) = available.get(choice).ok_or("JEV 返回无效候选")?;
            used.insert((text, key.clone()));
            let execution = execute_browser(root, &json!({
                "operation":"act",target_key:target,"snapshotId":snapshot,"action":action,
                "feedback":"inspect","scope":"viewport","visual":"none","maxTextChars":12000
            }), owner, tool).await;
            latest = match execution {
                Ok(value) => value,
                Err(error) => json!({"status":"needs_review","error":error,target_key:target,
                    "basedOnSnapshotId":snapshot,"verification":"unverified"}),
            };
            history.push(json!({"step":history.len()+1,"action":key.chars().take(800).collect::<String>(),"status":latest["status"],
                "completedActions":latest["completedActions"],"basedOnSnapshotId":snapshot}));
            if latest["status"] != "executed" || latest["observationError"].is_string() || !latest["snapshotId"].is_string() {
                return Err("执行不明确或缺少新观察；交回主模型，不重放".into());
            }
            snapshot = latest["snapshotId"].clone();
            if action["action"] == "fill" || !pending.is_empty() {
                let fresh = crate::native_browser::jev_observation(root, &json!({target_key:target,"snapshotId":snapshot}), owner, tool)?;
                // DOM values are bounded previews; a successful long fill must not be sent again just because the preview is shorter.
                if action["action"] == "fill" { used.insert((evidence(&fresh, &plan), key.clone())); }
                if !can_continue(&pages, &fresh, action) { pending.clear(); }
            }
        }
        Err("已达到执行预算".into())
    }.await;
    let mut fallback = false;
    if outcome.is_err() {
        // Return fresh evidence even when the first decision defers or an act lost feedback.
        match execute_browser(root, &json!({"operation":"inspect",target_key:target,
            "scope":"viewport","visual":"none","maxTextChars":12000}), owner, tool).await {
            Ok(observed) => {
                latest.as_object_mut().unwrap().extend(observed.as_object().cloned().unwrap_or_default());
                fallback = crate::native_browser::jev_fallback(root,
                    &json!({target_key:target,"snapshotId":latest["snapshotId"]}), owner, tool, true).is_ok();
            }
            Err(error) => latest["observationError"] = json!(error),
        }
    }
    // Inner act/inspect replies carry availability-only metadata; the outer run actually delegated.
    latest["jev"] = json!({"status":if decisions.is_empty(){"not_delegated"}else{"delegated"}, "requestAttempted":decisions.iter().any(|d| d["requestAttempted"] == true),
        "next":if outcome.is_ok() { "本次子目标已核验；后续简单判断继续委托run，最终答案由主模型核对。" }
            else { "本次未完成。先按reason解决障碍；观察、输入和候选范围没有实质变化时不要重复run。主模型处理障碍后恢复run；仅视觉或委托无法处理的动作由主模型act兜底。候选过多时可用controlNames缩小范围。ref必须原样复制当前items[].ref，禁止用snapshotId拼接。" }});
    latest["jevRun"] = json!({"status":if outcome.is_ok(){"completed"}else{"handoff"},
        "fallback": {"allowed":fallback,"maxActions":1,"snapshotId":if fallback {latest["snapshotId"].clone()} else {Value::Null},
            "notice":"仅限本次 handoff 的同一目标和 snapshotId，180秒内一次单步 act；执行或重新观察后失效，之后恢复 run。视觉操作仍由主模型处理。"},
        "requestCount":decisions.iter().filter(|d| d["requestAttempted"] == true).count(),
        "decisionCount":decisions.len(),"cachedActions":cached_actions,"maxActions":plan.max_actions,
        "executedActions":history.iter().filter_map(|h| h["completedActions"].as_u64()).sum::<u64>(),
        "verification":if outcome.is_ok(){"subgoal_verified"}else{"unverified"},
        "remainingGoal":plan.task,"missingInputs":missing_inputs,"candidateCounts":candidate_counts,"controlNames":plan.control_names,
        "inputHint":"missingInputs 是可见但未授权具体文本的字段，不代表都必须填写；若目标需要其中字段，请提供 plan.inputs 的 name/role/text 后重新 run，单独允许 typing 不会生成 fill 候选。",
        "history":history,"decisions":decisions,"reason":outcome.err(),"elapsedMs":started.elapsed().as_millis() as u64,
        "notice":"JEV优先决策。完成仅针对本次子目标；handoff后主模型核对最新观察，不重放历史操作，解决难点后可再次委托run。"});
    Ok(latest)
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn jev_trusted_action_scope_does_not_leak_to_other_tasks_or_after_return() {
        assert!(!super::executing_browser_action());
        super::BROWSER_ACTION.scope(true, async {
            tokio::task::yield_now().await;
            assert!(super::executing_browser_action());
            assert!(!tokio::spawn(async { super::executing_browser_action() }).await.unwrap());
        }).await;
        assert!(!super::executing_browser_action());
    }
    #[test]
    fn focusability_alone_does_not_create_a_click_hint() {
        let plan=parse(&json!({"plan":{"task":"open","authorization":"read","expectedText":"details"}})).unwrap();
        let pages=json!({"pages":[{"frame":0,"items":[
            {"ref":"container","name":"Focus region","role":"div","tabIndex":0,"inView":true},
            {"ref":"control","name":"Open","role":"div","actionable":true,"inView":true},
            {"ref":"checkbox","name":"Option","role":"menuitemcheckbox","inView":true}
        ]}]});
        let hints=candidates(&pages,&plan,&HashSet::new()).unwrap();
        assert_eq!(hints.len(),2);
        assert!(hints.values().all(|(action,_)|action["ref"]!="container"));
    }
    #[test]
    fn dom_scroll_hints_follow_real_remaining_space_without_coordinates() {
        let plan=parse(&json!({"plan":{"task":"find next record","authorization":"read","expectedText":"record"}})).unwrap();
        let pages=json!({"pages":[{"frame":0,"url":"https://example.test/","viewport":{"scrollY":0,"height":600},
            "documentSize":{"height":1800},"items":[{"ref":"list","nodeId":"d:1","name":"Results","role":"div",
                "inView":true,"scroll":{"top":200,"height":1000,"viewportHeight":200}}]}]});
        let hints=candidates(&pages,&plan,&HashSet::new()).unwrap();
        assert_eq!(hints.len(),3);
        assert!(hints.values().all(|(a,_)|a["action"]=="scroll" && a["x"].is_null()));
        assert_eq!(hints.values().filter(|(a,_)|a["ref"].is_null() && a["delta"]==640).count(),1);
        let used=hints.values().map(|(_,key)|key.clone()).collect();
        assert!(candidates(&pages,&plan,&used).unwrap().is_empty());
        let mut next=pages.clone();next["pages"][0]["viewport"]["scrollY"]=json!(1200);
        let remaining=candidates(&next,&plan,&used).unwrap();
        assert_eq!(remaining.len(),1);
        assert_eq!(remaining.values().next().unwrap().0["delta"],-640);
    }
    #[test]
    fn planned_fills_require_exact_fresh_dom_and_node_identity() {
        let before=json!({"pages":[{"frame":0,"url":"https://example.test/","text":"Form","items":[
            {"ref":"old-a","nodeId":"doc:1","value":"","name":"First"},
            {"ref":"old-b","nodeId":"doc:2","value":"","name":"Second"}]}]});
        let action=json!({"action":"fill","frame":0,"ref":"old-a","text":"Alice"});
        let mut after=before.clone();
        after["pages"][0]["items"][0]["value"]=json!("Alice");
        after["pages"][0]["items"][0]["ref"]=json!("fresh-a");
        after["pages"][0]["items"][1]["ref"]=json!("fresh-b");
        assert!(can_continue(&before,&after,&action));
        for (key,value) in [("nodeId",json!("replacement")),("disabled",json!(true)),("region",json!("different record"))] {
            let mut changed=after.clone();changed["pages"][0]["items"][1][key]=value;
            assert!(!can_continue(&before,&changed,&action));
        }
        let mut changed=after.clone();changed["pages"][0]["text"]=json!("Validation error");
        assert!(!can_continue(&before,&changed,&action));
        assert!(!can_continue(&before,&before,&action));
        assert!(!can_continue(&before,&after,&json!({"action":"click","frame":0,"ref":"old-a"})));
        let mut args=json!({"plan":{"task":"t","authorization":"a","expectedText":"e"}});
        assert_eq!(parse(&args).unwrap().max_actions,32);
        for invalid in [0,65] { args["plan"]["maxActions"]=json!(invalid);assert!(parse(&args).is_err()); }
    }

    #[test]
    fn same_named_dom_hints_keep_context_and_do_not_allow_ambiguous_fill() {
        let plan=parse(&json!({"plan":{"task":"open second record","authorization":"read","expectedText":"details",
            "inputs":[{"name":"Search","role":"input","text":"query"}]}})).unwrap();
        let pages=json!({"pages":[{"frame":0,"items":[
            {"ref":"a","nodeId":"d:1","name":"Open","role":"button","region":"Record A","inView":true},
            {"ref":"b","nodeId":"d:2","name":"Open","role":"button","region":"Record B","inView":true},
            {"ref":"c","name":"Search","role":"input","editable":true,"inView":true},
            {"ref":"d","name":"Search","role":"input","editable":true,"inView":true}]}]});
        let hints=candidates(&pages,&plan,&HashSet::new()).unwrap();
        assert_eq!(hints.len(),2);
        assert!(hints["action_0"].1.contains("Record A"));
        assert!(hints["action_1"].1.contains("Record B"));
        assert!(hints.values().all(|(a,_)|a["x"].is_null() && a["action"]=="click"));
    }
    #[test]
    fn crowded_candidates_preserve_hints_without_lexical_narrowing() {
        let mut items:Vec<Value>=(0..80).map(|n|json!({"name":format!("Unrelated {n}"),"role":"button","ref":format!("r{n}"),"inView":true})).collect();
        items.extend([json!({"name":"Region Global","role":"button","ref":"region","inView":true}),
            json!({"name":"United States","role":"option","ref":"us","inView":true}),
            json!({"name":"Confirm","role":"button","ref":"confirm","inView":true})]);
        let pages=json!({"pages":[{"frame":0,"items":items}]});
        let mut args=json!({"plan":{"task":"Change Region Global to United States then Confirm","authorization":"filter only","expectedText":"United States"}});
        let plan=parse(&args).unwrap();
        let choices=candidates(&pages,&plan,&HashSet::new()).unwrap();
        assert_eq!(choices.len(),83);
        assert!(choices.values().any(|(action,_)|action["ref"]=="region"));
        args["plan"]["task"]=json!("无法与页面标签匹配的目标");
        args["plan"]["expectedText"]=json!("完成");
        assert_eq!(candidates(&pages,&parse(&args).unwrap(),&HashSet::new()).unwrap().len(),83);
        args["plan"]["controlNames"]=json!(["Region","United States","Confirm"]);
        assert_eq!(candidates(&pages,&parse(&args).unwrap(),&HashSet::new()).unwrap().len(),3);
        args["plan"]["controlNames"]=json!(["Unrelated"]);
        assert_eq!(candidates(&pages,&parse(&args).unwrap(),&HashSet::new()).unwrap().len(),80);
        let crowded=json!({"pages":[{"frame":0,"items":(0..257).map(|n|json!({"name":format!("Button {n}"),"role":"button","ref":format!("r{n}"),"inView":true})).collect::<Vec<_>>()}]});
        args["plan"]["controlNames"]=json!([]);
        assert!(candidates(&crowded,&parse(&args).unwrap(),&HashSet::new()).unwrap_err().contains("256"));
        args["plan"]["controlNames"]=json!([""]);
        assert!(parse(&args).is_err());
        assert!(!plan.use_experience);
    }

    #[test]
    #[ignore = "requires NOVA_JEV_REPLAY with captured plans and DOM observations"]
    fn replay_crowded_session_candidates() {
        let path=std::env::var("NOVA_JEV_REPLAY").unwrap();
        let cases:Value=serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert!(!cases.as_array().unwrap().is_empty());
        for (index,case) in cases.as_array().unwrap().iter().enumerate() {
            let plan=parse(case).unwrap();
            let choices=candidates(&case["pages"],&plan,&HashSet::new()).unwrap();
            assert!(!choices.is_empty() && choices.len()<=256);
            println!("Replay {}: {} candidates: {:?}",index+1,choices.len(),choices.values().map(|(_,key)|key).collect::<Vec<_>>());
        }
    }
    #[test]
    fn sortable_duplicate_columns_and_fresh_refs_are_distinct() {
        let plan=parse(&json!({"plan":{"task":"Units降序","authorization":"允许排序","expectedText":"Units降序"}})).unwrap();
        let mut pages=json!({"pages":[{"frame":0,"items":[
            {"ref":"a","role":"th","name":"Units","inView":true,"actionable":true,"column":{"table":0,"index":0},"sort":"none"},
            {"ref":"b","role":"th","name":"Units","inView":true,"actionable":true,"column":{"table":0,"index":1},"sort":"none"},
            {"ref":"search","role":"input","name":"Search menus","inView":true,"editable":true}
        ],"tables":[{"columns":[{"name":"Units","sort":"none","ref":"a"}]}]}]});
        assert_eq!(undelegated_inputs(&pages,&plan),json!([{"name":"Search menus","role":"input"}]).as_array().unwrap().clone());
        let options=candidates(&pages,&plan,&HashSet::new()).unwrap();assert_eq!(options.len(),2);
        let before=evidence(&pages,&plan);
        pages["pages"][0]["tables"][0]["columns"][0]["ref"]=json!("fresh");
        assert_eq!(before,evidence(&pages,&plan));
        let used=options.values().map(|(_,key)|key.clone()).collect();
        pages["pages"][0]["items"][0]["sort"]=json!("ascending");
        assert_eq!(candidates(&pages,&plan,&used).unwrap().len(),1);
        assert_ne!(before,evidence(&pages,&plan));
    }
    use super::*;
    #[test]
    fn graph_context_keeps_complete_routes_and_checks_within_budget() {
        let route = json!({"id":"route-1","conditions":["已登录"],"steps":["查询订单"],"checks":["订单号匹配"],"pitfalls":["不要误选同名记录"]});
        let result = graph_context(&json!({"capabilities":"x".repeat(20000),"graph":{"routes":[route.clone(),route.clone(),route.clone(),route.clone()]}}));
        assert_eq!(result["routes"].as_array().unwrap().len(), 3);
        assert_eq!(result["routes"][0], route);
        assert_eq!(result["truncated"], true);
        let oversized = graph_context(&json!({"graph":{"routes":[{"steps":["x".repeat(12001)]}]}}));
        assert_eq!(oversized["routes"], json!([]));
        assert_eq!(oversized["truncated"], true);
        for key in ["browserId", "tabTag"] {
            assert_eq!(json!({key:"target"})[key], "target");
        }
    }
    #[test]
    fn decision_evidence_tracks_field_state_without_stale_dom_refs() {
        let plan = parse(&json!({"plan":{"task":"查询","authorization":"查询","expectedText":"结果",
            "inputs":[{"name":"关键词","role":"input","text":"订单"}]}})).unwrap();
        let mut pages = json!({"pages":[{"frame":0,"text":"搜索结果","items":[{
            "inView":true,"name":"关键词","role":"input","value":"订单","ref":"old","selected":"false"
        }]}]});
        let before = evidence(&pages, &plan);
        pages["pages"][0]["items"][0]["ref"] = json!("fresh");
        assert_eq!(evidence(&pages, &plan), before);
        pages["pages"][0]["items"][0]["value"] = json!("其它订单");
        assert_ne!(evidence(&pages, &plan), before);
        assert!(evidence(&pages, &plan).contains("搜索结果"));
        pages["pages"][0]["items"][0]["name"] = json!("未委托字段");
        pages["pages"][0]["items"][0]["value"] = json!("private-token");
        assert!(!evidence(&pages, &plan).contains("private-token"));
        pages["pages"][0]["tables"] = json!([{"totalRows":1691,"loadedRows":50,"rows":[[5,"Tomodachi Life", "145.6M"]]}]);
        assert!(evidence(&pages, &plan).contains("Tomodachi Life"));
        assert!(evidence(&pages, &plan).contains("1691"));
    }
    #[test]
    fn goal_candidates_are_fresh_bounded_and_never_replayed() {
        let args = json!({"plan":{"task":"查找","authorization":"只读搜索","expectedText":"结果","inputs":[{"name":"关键词","role":"input","text":"订单"}]}});
        let plan = parse(&args).unwrap();
        let item = json!({"ref":"new-ref","name":"搜索","role":"button","inView":true});
        let mut pages = json!({"pages":[{"frame":0,"items":[item.clone(),{"ref":"field","name":"关键词","role":"input","editable":true,"inView":true}],"text":"结果"}]});
        let choices = candidates(&pages,&plan,&HashSet::new()).unwrap();
        assert_eq!(choices.len(),2);
        assert_eq!(choices["action_0"].0["ref"],"new-ref");
        let used = choices.values().map(|(_,key)|key.clone()).collect();
        assert!(candidates(&pages,&plan,&used).unwrap().is_empty());
        pages["pages"][0]["items"].as_array_mut().unwrap().push(item);
        assert_eq!(candidates(&pages,&plan,&HashSet::new()).unwrap().len(),3);
        assert!(selected(&json!({"status":"advised","choice":"action_0","confidence":0.01})));
        assert!(selected(&json!({"status":"advised","choice":"done"})));
        assert!(!selected(&json!({"status":"advised","choice":"defer","confidence":1})));
        assert!(!selected(&json!({"status":"unavailable","choice":"done"})));
        assert!(!selected(&json!({"status":"advised"})));
        assert!(parse(&json!({"plan":{"task":"x","steps":[]}})).is_err());
        assert!(origin(&json!({"pages":[{"url":"file:///tmp/x"}]})).is_err());
        let mut explicit = args.clone();
        explicit["plan"]["steps"] = json!([{"action":"fill","name":"关键词","role":"input","text":"订单","expectedText":"结果"}]);
        let explicit = parse(&explicit).unwrap();
        assert_eq!(step_candidate(&pages,&explicit.steps[0]).unwrap()["action_0"].0["text"],"订单");
        pages["pages"][0]["items"][1]["ref"] = json!("fresh-field");
        assert_eq!(step_candidate(&pages,&explicit.steps[0]).unwrap()["action_0"].0["ref"],"fresh-field");
    }

    #[test]
    fn semantic_controls_remain_available_beside_canvas() {
        let plan = parse(&json!({"plan":{"task":"选择筛选","authorization":"允许筛选","expectedText":"结果"}})).unwrap();
        let pages = json!({"pages":[{"frame":0,"visualSuggested":true,"items":[
            {"ref":"r","name":"收入","role":"radio","inView":true},
            {"ref":"c","name":"仅已发布","role":"checkbox","inView":true},
            {"ref":"p","name":"地区","role":"div","haspopup":"listbox","inView":true},
            {"ref":"s","name":"展开筛选","role":"summary","inView":true},
            {"ref":"custom","name":"Region Global","role":"div","tabIndex":0,"actionable":true,"inView":true},
            {"ref":"scroll","name":"内容区","role":"div","tabIndex":0,"scroll":{},"inView":true},
            {"ref":"text","name":"普通文本","role":"div","inView":true},
            {"ref":"canvas","name":"图表","role":"canvas","inView":true},
            {"ref":"disabled","name":"不可用","role":"radio","disabled":true,"inView":true}
        ]}]});
        let choices = candidates(&pages,&plan,&HashSet::new()).unwrap();
        let refs: HashSet<_> = choices.values().map(|(a,_)|a["ref"].as_str().unwrap()).collect();
        assert_eq!(refs,HashSet::from(["r","c","p","s","custom"]));
    }
}
