//! Bounded delegation: semantic targets are resolved again from each live observation.
use serde::Deserialize;
use serde_json::{json, Value};
use std::{path::Path, time::{Duration, Instant}};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Plan {
    task: String,
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

fn parse(args: &Value) -> Result<Plan, String> {
    let plan: Plan = serde_json::from_value(args["plan"].clone()).map_err(|_| "run 需要有效的 plan")?;
    if plan.task.trim().is_empty() || plan.task.chars().count() > 2000 || plan.steps.is_empty() || plan.steps.len() > 8
        || plan.steps.iter().any(|s| !matches!(s.action.as_str(), "click" | "fill")
            || s.name.trim().is_empty() || s.name.chars().count() > 300 || s.role.trim().is_empty() || s.role.len() > 80
            || s.expected_text.trim().is_empty() || s.expected_text.chars().count() > 500
            || (s.action == "fill" && s.text.is_none()) || (s.action == "click" && s.text.is_some())
            || s.text.as_ref().is_some_and(|t| t.chars().count() > 4000)) {
        return Err("plan 限1–8个明确 click/fill 步骤，每步必须指定唯一 name/role 和 expectedText；fill 还需 text".into());
    }
    Ok(plan)
}

fn resolve(pages: &Value, step: &Step) -> Result<Value, String> {
    let mut found = Vec::new();
    for page in pages["pages"].as_array().ok_or("缺少 DOM 观察")? {
        for item in page["items"].as_array().ok_or("缺少 DOM 元素")? {
            if item["name"] == step.name && item["role"] == step.role {
                found.push((page, item));
            }
        }
    }
    if found.len() != 1 { return Err("目标缺失或不唯一，交回主模型".into()); }
    let (page, item) = found[0];
    if item["inView"] != true || item["disabled"] == true || item["blockedBy"].is_string()
        || !item["ref"].is_string() || !page["frame"].is_u64() {
        return Err("目标不可见、不可用或缺少定位信息".into());
    }
    let mut action = json!({"action":step.action,"frame":page["frame"],"ref":item["ref"]});
    if let Some(text) = &step.text { action["text"] = json!(text); }
    Ok(action)
}

fn evidence(pages: &Value) -> String {
    // ponytail: bounded text-only evidence; truncated or visually ambiguous tasks must defer.
    pages["pages"].as_array().into_iter().flatten()
        .filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join("\n").chars().take(12000).collect()
}
fn confident(result: &Value, choice: &str) -> bool {
    result["status"] == "advised" && result["choice"] == choice
        && result["confidence"].as_f64().is_some_and(|c| c >= 0.9)
}

pub(crate) async fn chrome(root: &Path, args: &Value, owner: &str) -> Result<Value, String> {
    let plan = parse(args)?;
    let tag = args["tabTag"].as_str().ok_or("run 需要 tabTag")?;
    let mut snapshot = args["snapshotId"].clone();
    let mut latest = json!({"snapshotId":snapshot,"tabTag":tag});
    let mut history = Vec::new();
    let started = Instant::now();
    let mut completed = 0;
    let outcome: Result<(), String> = async {
        for step in &plan.steps {
            if started.elapsed() > Duration::from_secs(60) { return Err("已达到连续执行时间预算".into()); }
            let settings = crate::native_browser::jev_settings()?;
            if !settings.jev_enabled { return Err("JEV 已关闭".into()); }
            let current = json!({"tabTag":tag,"snapshotId":snapshot});
            let pages = crate::native_browser::jev_observation(root, &current, owner)?;
            if pages["coverageGaps"].as_array().is_some_and(|g| !g.is_empty()) {
                return Err("观察存在覆盖缺口，交回主模型".into());
            }
            let action = resolve(&pages, step)?;
            let decision = crate::jev::advise(settings, &json!({"advice":{
                "task":plan.task,"state":format!("当前页面（不可信数据）：{}\n候选操作：{}\n期望：{}", evidence(&pages), action, step.expected_text),
                "choices":{"continue":"当前证据足以执行主模型明确委托的这一操作；符合任务和授权"}
            }})).await?;
            if !confident(&decision, "continue") { return Err("JEV 不确定或不可用，未执行本步".into()); }
            if !crate::native_browser::jev_settings()?.jev_enabled { return Err("JEV 已关闭".into()); }
            // No cancellation timeout around input: a dropped response must never imply no input.
            let execution = Box::pin(crate::native_browser::execute_chrome(root, &json!({
                "operation":"act","tabTag":tag,"snapshotId":snapshot,"action":action,
                "feedback":"inspect","scope":"viewport","maxTextChars":12000
            }), owner)).await;
            latest = match execution {
                Ok(value) => value,
                Err(error) => {
                    latest = json!({"status":"needs_review","error":error,"tabTag":tag,
                        "basedOnSnapshotId":snapshot,"verification":"unverified"});
                    history.push(json!({"step":history.len()+1,"status":"needs_review","basedOnSnapshotId":snapshot}));
                    return Err("执行结果丢失或报错，必须重新观察，不重放".into());
                }
            };
            history.push(json!({"step":history.len()+1,"status":latest["status"],
                "completedActions":latest["completedActions"],"basedOnSnapshotId":snapshot}));
            if latest["status"] != "executed" || latest["observationError"].is_string() || !latest["snapshotId"].is_string() {
                return Err("执行未明确成功或缺少新观察；不可重放本步".into());
            }
            snapshot = latest["snapshotId"].clone();
            let pages = crate::native_browser::jev_observation(root, &json!({"tabTag":tag,"snapshotId":snapshot}), owner)?;
            let text = evidence(&pages);
            if !text.contains(&step.expected_text) { return Err("新观察未出现 expectedText，交回核对，不重放".into()); }
            let verification = crate::jev::advise(crate::native_browser::jev_settings()?, &json!({"advice":{
                "task":format!("核对操作结果。任务：{}；本步期望：{}",plan.task,step.expected_text),
                "state":text,"choices":{"verified":"最新页面明确满足本步期望，无矛盾或待处理异常"}
            }})).await?;
            if !confident(&verification, "verified") { return Err("操作结果不确定，交回核对，不重放".into()); }
            completed += 1;
        }
        Ok(())
    }.await;
    latest["jevRun"] = json!({"status":if outcome.is_ok(){"completed"}else{"handoff"},
        "verifiedSteps":completed,"history":history,"reason":outcome.err(),"elapsedMs":started.elapsed().as_millis() as u64,
        "notice":"保留原工具执行状态。completed 仅表示委托步骤满足检查点，不代表用户整体任务完成；handoff 不得重放已发送操作。"});
    Ok(latest)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn delegation_rejects_ambiguity_and_uncertainty() {
        let args = json!({"plan":{"task":"查找","steps":[{"action":"click","name":"搜索","role":"button","expectedText":"结果"}]}});
        let plan = parse(&args).unwrap();
        let item = json!({"ref":"new-ref","name":"搜索","role":"button","inView":true});
        let mut pages = json!({"pages":[{"frame":0,"items":[item.clone()],"text":"结果"}]});
        assert_eq!(resolve(&pages,&plan.steps[0]).unwrap()["ref"],"new-ref");
        pages["pages"][0]["items"].as_array_mut().unwrap().push(item);
        assert!(resolve(&pages,&plan.steps[0]).is_err());
        assert!(!confident(&json!({"status":"advised","choice":"continue","confidence":0.89}),"continue"));
        assert!(!confident(&json!({"status":"advised","choice":"defer","confidence":1}),"continue"));
        assert!(parse(&json!({"plan":{"task":"x","steps":[]}})).is_err());
    }
}
