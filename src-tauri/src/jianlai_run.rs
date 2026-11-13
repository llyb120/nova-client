//! 剑来执行器：主模型只给出 task 与 steps（action + target），本模块在最新截图和控件树上绑定并执行。
//! 与 Chrome 的 chrome_run 同一原理：树唯一命中直接取点，否则轻量 oneshot；不确定或 expect 不成立就交回。

use serde::Deserialize;
use serde_json::{json, Value};
use std::path::Path;
use std::time::{Duration, Instant};

const RUN_BUDGET: Duration = Duration::from_secs(180);

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Plan {
    task: String,
    #[serde(default)]
    authorization: String,
    #[serde(default)]
    expected_text: String,
    #[serde(default)]
    steps: Vec<Step>,
    #[serde(default = "default_max_actions")]
    max_actions: usize,
}

fn default_max_actions() -> usize { 32 }

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Step {
    action: String,
    #[serde(default)]
    target: String,
    #[serde(default)]
    to_target: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    key: Option<String>,
    #[serde(default)]
    delta: Option<i32>,
    #[serde(default)]
    axis: Option<String>,
    #[serde(default)]
    when: Option<String>,
    #[serde(default)]
    expect: Option<String>,
    #[serde(default)]
    expected_text: Option<String>,
    #[serde(default)]
    optional: Option<bool>,
}

impl Step {
    fn expect(&self) -> Option<&str> {
        self.expect.as_deref().or(self.expected_text.as_deref()).filter(|s| !s.trim().is_empty())
    }
    fn query(&self) -> String {
        if !self.target.trim().is_empty() {
            self.target.clone()
        } else {
            self.name.clone().unwrap_or_default()
        }
    }
    fn describe(&self) -> String {
        let target = if self.query().is_empty() { self.action.clone() } else { self.query() };
        format!("{} {}", self.action, target)
    }
}

fn parse(args: &Value) -> Result<Plan, String> {
    let plan = if args["plan"].is_object() { &args["plan"] } else { args };
    let plan: Plan = serde_json::from_value(plan.clone())
        .map_err(|e| format!("plan 格式错误（{e}）；需要 task 与 steps，如 [{{\"action\":\"click\",\"target\":\"保存按钮\"}}]"))?;
    if plan.task.trim().is_empty() || plan.steps.is_empty() || plan.steps.len() > 24 {
        return Err("run 需要非空 task，以及 1–24 个 steps".into());
    }
    for (index, step) in plan.steps.iter().enumerate() {
        if !matches!(step.action.as_str(), "click" | "double_click" | "move" | "drag" | "type" | "press" | "scroll" | "wait") {
            return Err(format!("steps[{index}] 未知动作 {}", step.action));
        }
        if matches!(step.action.as_str(), "click" | "double_click" | "move" | "drag" | "scroll") && step.query().trim().is_empty() {
            return Err(format!("steps[{index}] 的 {} 需要 target 或 name", step.action));
        }
        if step.action == "drag" && step.to_target.as_deref().unwrap_or("").trim().is_empty() {
            return Err(format!("steps[{index}] 的 drag 需要 toTarget"));
        }
        if step.action == "press" && step.key.as_deref().unwrap_or("").trim().is_empty() {
            return Err(format!("steps[{index}] 的 press 需要 key"));
        }
        if step.action == "type" && step.text.as_deref().unwrap_or("").is_empty() {
            return Err(format!("steps[{index}] 的 type 需要 text"));
        }
    }
    Ok(plan)
}

fn evidence(result: &Value) -> String {
    let mut parts = Vec::new();
    if let Some(title) = result.pointer("/elements/title").and_then(Value::as_str) {
        parts.push(title.to_string());
    }
    if let Some(app) = result.pointer("/elements/app").and_then(Value::as_str) {
        parts.push(app.to_string());
    }
    for item in result["elements"]["items"].as_array().into_iter().flatten() {
        if let Some(name) = item["name"].as_str().filter(|s| !s.is_empty()) {
            parts.push(name.to_string());
        }
    }
    parts.join("\n")
}

fn literal_met(result: &Value, expect: &str) -> bool {
    let text = evidence(result).to_lowercase();
    expect.split('|').map(str::trim).filter(|s| !s.is_empty()).any(|part| text.contains(&part.to_lowercase()))
}

async fn observe(root: &Path, owner: &str, window_id: Option<&Value>) -> Result<Value, String> {
    let mut args = json!({"operation":"windows","observe":true});
    if let Some(id) = window_id.filter(|v| !v.is_null()) {
        args["windowId"] = id.clone();
    }
    Box::pin(crate::jianlai::execute(root, &args, owner)).await
}

pub(crate) async fn desktop(root: &Path, args: &Value, owner: &str) -> Result<Value, String> {
    let plan = parse(args)?;
    let started = Instant::now();
    let mut latest = if args["snapshotId"].as_str().is_some_and(|s| !s.is_empty()) {
        json!({"snapshotId": args["snapshotId"].clone(), "elements": Value::Null})
    } else {
        observe(root, owner, args.get("windowId")).await?
    };
    let mut history = Vec::new();
    let mut executed = 0usize;
    let mut step_index = 0usize;
    let total = plan.steps.len();
    let mut reason = String::new();
    while step_index < total {
        if started.elapsed() > RUN_BUDGET {
            reason = format!("已达到连续执行时间预算（180秒），停在第{}步", step_index + 1);
            break;
        }
        if executed >= plan.max_actions {
            reason = format!("已达到动作预算 maxActions，停在第{}步", step_index + 1);
            break;
        }
        let step = plan.steps[step_index].clone();
        if let Some(when) = step.when.as_deref() {
            if !literal_met(&latest, when) {
                reason = format!("第{}步预测条件未成立「{when}」；本步未执行，按最新观察重新规划", step_index + 1);
                break;
            }
        }
        let action = match step.action.as_str() {
            "wait" => json!({"action":"wait","ms":250}),
            "press" => json!({"action":"press","key":step.key}),
            "type" => json!({"action":"type","text":step.text}),
            "scroll" => {
                let mut value = json!({"action":"scroll","target":step.query(),"delta":step.delta.unwrap_or(-3)});
                if let Some(axis) = &step.axis { value["axis"] = json!(axis); }
                value
            }
            "drag" => json!({"action":"drag","target":step.query(),"toTarget":step.to_target}),
            "double_click" => json!({"action":"double_click","target":step.query()}),
            "move" => json!({"action":"move","target":step.query()}),
            _ => json!({"action":"click","target":step.query()}),
        };
        let mut act = json!({
            "operation":"act",
            "snapshotId": latest.get("snapshotId").cloned().unwrap_or(Value::Null),
            "actions":[action.clone()],
            "feedback":"screenshot",
        });
        if let Some(id) = args.get("windowId") { act["windowId"] = id.clone(); }
        match Box::pin(crate::jianlai::execute(root, &act, owner)).await {
            Ok(result) => {
                let completed = result["completedActions"].as_u64().unwrap_or(0) as usize;
                executed += completed;
                history.push(json!({
                    "step": step_index + 1,
                    "describe": step.describe(),
                    "status": result["status"],
                    "located": result.get("located").cloned(),
                    "error": result.get("error").cloned(),
                }));
                latest = result;
                if latest["status"] == "not_executed" || completed == 0 {
                    if step.optional == Some(true) {
                        step_index += 1;
                        continue;
                    }
                    reason = format!("第{}步未执行：{}", step_index + 1, latest["error"].as_str().unwrap_or("未知"));
                    break;
                }
                if let Some(expect) = step.expect() {
                    if !literal_met(&latest, expect) {
                        reason = format!("第{}步执行后未看到「{expect}」，交回主模型", step_index + 1);
                        step_index += 1;
                        break;
                    }
                }
                step_index += 1;
            }
            Err(error) => {
                if step.optional == Some(true) {
                    history.push(json!({"step":step_index+1,"describe":step.describe(),"status":"skipped","error":error}));
                    step_index += 1;
                    continue;
                }
                reason = format!("第{}步失败：{error}", step_index + 1);
                break;
            }
        }
    }
    let remaining: Vec<Value> = plan.steps[step_index.min(total)..].iter().map(|s| json!({
        "action": s.action, "target": s.target, "toTarget": s.to_target, "name": s.name,
        "text": s.text, "key": s.key, "delta": s.delta, "axis": s.axis,
        "when": s.when, "expect": s.expect(),
    })).collect();
    let handoff = !reason.is_empty() || step_index < total;
    if !plan.expected_text.trim().is_empty() && !handoff && !literal_met(&latest, &plan.expected_text) {
        reason = format!("计划已跑完，但未看到「{}」", plan.expected_text);
    }
    let mut result = latest;
    result["task"] = json!(plan.task);
    result["authorization"] = json!(plan.authorization);
    result["runHistory"] = json!(history);
    result["completedSteps"] = json!(step_index);
    result["remainingSteps"] = json!(remaining);
    result["reason"] = json!(if reason.is_empty() { Value::Null } else { json!(reason) });
    result["next"] = json!(if handoff || result["reason"].is_string() {
        "按 reason 修正 remainingSteps 的第一步（target/name/expect），只提交 remainingSteps 重新 run；已完成的步骤不要重放。"
    } else { "" });
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_requires_target_on_pointer_steps() {
        let err = parse(&json!({"plan":{"task":"保存","steps":[{"action":"click"}]}})).unwrap_err();
        assert!(err.contains("target"), "{err}");
        let plan = parse(&json!({"task":"保存","steps":[{"action":"click","target":"保存按钮"},{"action":"press","key":"Enter"}]})).unwrap();
        assert_eq!(plan.steps.len(), 2);
        assert_eq!(plan.steps[0].query(), "保存按钮");
    }
}
