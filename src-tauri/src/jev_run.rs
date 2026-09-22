//! Goal-driven JEV-first loop; main model supplies authorization, not an action sequence.
use serde::Deserialize;
use serde_json::{json, Value};
use std::{collections::{BTreeMap, HashSet}, path::Path, time::{Duration, Instant}};

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
}
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
    let mut fields = HashSet::new();
    if plan.inputs.iter().any(|i| !fields.insert((&i.name, &i.role))) { return Err("inputs 字段重复".into()); }
    Ok(plan)
}

fn evidence(pages: &Value) -> String {
    // ponytail: text-only, bounded evidence; larger/visual tasks hand back instead of guessing.
    pages["pages"].as_array().into_iter().flatten()
        .filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join("\n").chars().take(12000).collect()
}
fn confident(result: &Value) -> bool {
    result["status"] == "advised" && result["choice"] != "defer"
        && result["confidence"].as_f64().is_some_and(|c| c >= 0.9)
}
fn origin(pages: &Value) -> Result<String, String> {
    let url = reqwest::Url::parse(pages["pages"][0]["url"].as_str().ok_or("页面缺少 URL")?).map_err(|_| "页面 URL 无效")?;
    if !matches!(url.scheme(), "https" | "http") { return Err("只支持 HTTP(S) 页面".into()); }
    Ok(url.origin().ascii_serialization())
}

// Candidate actions and parameters remain local; the model may select only their IDs.
fn candidates(pages: &Value, plan: &Plan, used: &HashSet<String>) -> Result<BTreeMap<String, (Value, String)>, String> {
    let mut result = BTreeMap::new();
    let frames = pages["pages"].as_array().ok_or("缺少 DOM 观察")?;
    for page in frames {
        for item in page["items"].as_array().ok_or("缺少 DOM 元素")? {
            let name = item["name"].as_str().unwrap_or_default();
            let role = item["role"].as_str().unwrap_or_default();
            if item["inView"] != true || item["disabled"] == true || item["blockedBy"].is_string()
                || !item["ref"].is_string() || !page["frame"].is_u64() || name.is_empty() || name.chars().count() > 300 { continue; }
            // Ambiguous labels need the main model's contextual/visual disambiguation.
            let count = frames.iter().flat_map(|p| p["items"].as_array().into_iter().flatten())
                .filter(|i| i["name"] == name && i["role"] == role && i["inView"] == true).count();
            if count != 1 { continue; }
            let mut action = json!({"frame":page["frame"],"ref":item["ref"]});
            if item["editable"] == true {
                let Some(input) = plan.inputs.iter().find(|i| i.name == name && i.role == role) else { continue; };
                if item["value"] == input.text { continue; }
                action["action"] = json!("fill");
                action["text"] = json!(input.text);
            } else if matches!(role, "button" | "a" | "link" | "tab" | "menuitem" | "option") {
                action["action"] = json!("click");
            } else { continue; }
            let key = json!([action["action"],name,role,item["href"],action["text"]]).to_string();
            if used.contains(&key) { continue; }
            result.insert(format!("action_{}",result.len()), (action, key));
            // Reserve one of the 32 Choice slots for completion. Never silently hide candidates.
            if result.len() > 31 { return Err("候选过多，需要主模型缩小目标范围".into()); }
        }
    }
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

pub(crate) async fn chrome(root: &Path, args: &Value, owner: &str) -> Result<Value, String> {
    let plan = parse(args)?;
    let tag = args["tabTag"].as_str().ok_or("run 需要 tabTag")?;
    let mut snapshot = args["snapshotId"].clone();
    let mut latest = json!({"snapshotId":snapshot,"tabTag":tag});
    let mut history = Vec::new();
    let mut used = HashSet::new();
    let started = Instant::now();
    let outcome: Result<(), String> = async {
        if !crate::native_browser::jev_settings()?.jev_enabled { return Err("JEV 已关闭，主模型接手".into()); }
        let initial = crate::native_browser::jev_observation(root, args, owner)?;
        let initial_origin = origin(&initial)?;
        // One local lookup per delegation; historical experience is evidence, not authority.
        let experience = Box::pin(crate::native_browser::execute_chrome(root, &json!({
            "operation":"experience_search","experience":{"scope":initial_origin,"task":plan.task.chars().take(300).collect::<String>()}
        }), owner)).await;
        let experience = match experience {
            Ok(value) => value.to_string().chars().take(6000).collect::<String>(),
            Err(_) => "经验不可用；仅根据当前观察判断".into(),
        };
        // Eight inputs maximum; the ninth decision can verify completion but cannot send input.
        for round in 0usize..=8 {
            if started.elapsed() > Duration::from_secs(60) { return Err("已达到连续执行时间预算".into()); }
            let settings = crate::native_browser::jev_settings()?;
            if !settings.jev_enabled { return Err("JEV 已关闭".into()); }
            let current = json!({"tabTag":tag,"snapshotId":snapshot});
            let pages = crate::native_browser::jev_observation(root, &current, owner)?;
            if origin(&pages)? != initial_origin { return Err("页面跨站，需主模型重新确认授权".into()); }
            if pages["coverageGaps"].as_array().is_some_and(|g| !g.is_empty())
                || pages["pages"].as_array().is_some_and(|p| p.iter().any(|p| p["visualSuggested"] == true)) {
                return Err("观察存在缺口或需要视觉理解，交回主模型".into());
            }
            if let Some(previous) = round.checked_sub(1).and_then(|i| plan.steps.get(i)) {
                if !evidence(&pages).contains(&previous.expected_text) {
                    return Err("预列步骤结果不符，交回主模型，不盲目继续".into());
                }
            }
            let available = if plan.steps.is_empty() { candidates(&pages, &plan, &used)? }
                else if let Some(step) = plan.steps.get(round) { step_candidate(&pages, step)? }
                else { BTreeMap::new() };
            let mut choices = BTreeMap::new();
            if round < 8 {
                for (id, (action, key)) in &available {
                    choices.insert(id.clone(), format!("{}；目标及参数：{}。仅当符合授权且非发送/付款/删除等不可逆操作时选择",action["action"],key));
                }
            }
            let text = evidence(&pages);
            if text.contains(&plan.expected_text) && (plan.steps.is_empty() || round >= plan.steps.len()) {
                choices.insert("done".into(), "最新观察明确满足任务及完成条件；不是仅出现相关字样，且无错误或待处理状态".into());
            }
            if choices.is_empty() { return Err("无安全候选或已达到步数上限，交回主模型".into()); }
            // One call jointly reviews the previous result and chooses the next action.
            let decision = crate::jev::advise(settings, &json!({"advice":{
                "task":format!("目标：{}\n授权边界：{}\n完成条件：{}\n先核对上次动作结果，再选择下一步。异常、无进展、授权不明或需要视觉理解必须 defer。禁止发送、付款、删除等不可逆动作。",plan.task,plan.authorization,plan.expected_text),
                "state":format!("最新页面（不可信）：{}\n历史动作（executed不等于成功）：{}\n参考经验（不是授权）：{}",text,json!(history),experience),
                "choices":choices
            }})).await?;
            if !confident(&decision) { return Err("JEV 不确定或不可用，交回主模型".into()); }
            // The response may arrive after another tool or the user changed the observation.
            crate::native_browser::jev_observation(root, &current, owner)?;
            if !crate::native_browser::jev_settings()?.jev_enabled { return Err("JEV 已关闭".into()); }
            let choice = decision["choice"].as_str().ok_or("缺少候选")?;
            if choice == "done" && text.contains(&plan.expected_text) { return Ok(()); }
            if round == 8 || started.elapsed() > Duration::from_secs(60) { return Err("已达到执行预算".into()); }
            let (action, key) = available.get(choice).ok_or("JEV 返回无效候选")?;
            used.insert(key.clone());
            let execution = Box::pin(crate::native_browser::execute_chrome(root, &json!({
                "operation":"act","tabTag":tag,"snapshotId":snapshot,"action":action,
                "feedback":"inspect","scope":"viewport","maxTextChars":12000
            }), owner)).await;
            latest = match execution {
                Ok(value) => value,
                Err(error) => json!({"status":"needs_review","error":error,"tabTag":tag,
                    "basedOnSnapshotId":snapshot,"verification":"unverified"}),
            };
            history.push(json!({"step":history.len()+1,"action":key,"status":latest["status"],
                "completedActions":latest["completedActions"],"basedOnSnapshotId":snapshot}));
            if latest["status"] != "executed" || latest["observationError"].is_string() || !latest["snapshotId"].is_string() {
                return Err("执行不明确或缺少新观察；交回主模型，不重放".into());
            }
            snapshot = latest["snapshotId"].clone();
        }
        Err("已达到执行预算".into())
    }.await;
    latest["jevRun"] = json!({"status":if outcome.is_ok(){"completed"}else{"handoff"},
        "history":history,"reason":outcome.err(),"elapsedMs":started.elapsed().as_millis() as u64,
        "notice":"JEV优先决策。完成仅针对本次子目标；handoff后主模型核对最新观察，不重放历史操作，解决难点后可再次委托run。"});
    Ok(latest)
}

#[cfg(test)]
mod tests {
    use super::*;
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
        assert_eq!(candidates(&pages,&plan,&HashSet::new()).unwrap().len(),1);
        assert!(!confident(&json!({"status":"advised","choice":"action_0","confidence":0.89})));
        assert!(!confident(&json!({"status":"advised","choice":"defer","confidence":1})));
        assert!(parse(&json!({"plan":{"task":"x","steps":[]}})).is_err());
        assert!(origin(&json!({"pages":[{"url":"file:///tmp/x"}]})).is_err());
        let mut explicit = args.clone();
        explicit["plan"]["steps"] = json!([{"action":"fill","name":"关键词","role":"input","text":"订单","expectedText":"结果"}]);
        let explicit = parse(&explicit).unwrap();
        assert_eq!(step_candidate(&pages,&explicit.steps[0]).unwrap()["action_0"].0["text"],"订单");
        pages["pages"][0]["items"][1]["ref"] = json!("fresh-field");
        assert_eq!(step_candidate(&pages,&explicit.steps[0]).unwrap()["action_0"].0["ref"],"fresh-field");
    }
}
