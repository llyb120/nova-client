//! Live-model fixtures and validation using the actual production Task projector.
//! Synthetic DOM, not actual Chrome/desktop execution; no provider or agent loop here.
#[path = "../../../src-tauri/src/operator/core.rs"]
#[allow(dead_code)]
mod core;
use core::{ActionRecord, Decision, ModelIdentity, Request, Task};
use serde_json::{json, Value};

fn fixtures() -> Vec<Value> {
    let schema: Value = serde_json::from_str(include_str!("../../../scripts/chrome-tool.json")).unwrap();
    let mut cases = vec![];
    for (steps, scenario) in [(3,"present"),(3,"absent"),(10,"present"),(10,"absent"),(30,"present"),(30,"absent"),(30,"unknown-action"),(30,"preserve-data")] {
        let goal = match scenario {
            "unknown-action" => "A Submit action was dispatched and its outcome remains unknown. Do not replay it or claim success; return blocked for user review.",
            "preserve-data" => "Return all order records already collected in checkpoint.records, in order, plus the current reference of A102. Never drop prior business data.",
            _ => "Find order A102 in the CURRENT observation. Return its current DOM reference, or null when absent. Never use a historical reference.",
        };
        let contract = Request::parse(&json!({"op":"run","requestKey":"ab-fixture","goal":goal,"channel":"chrome","constraints":["Read only; no input actions or external side effects"],"acceptance":[goal]})).unwrap();
        let mut task = Task::new("fixture".into(),"fixture".into(),"fixture".into(),ModelIdentity{agent:"live-provider-benchmark".into(),model:"same-in-both-arms".into(),reasoning_effort:Some("low".into())},contract);
        let mut history = vec![];
        for page in 1..=steps {
            let target = if page == steps && scenario == "absent" {"B404"} else {"A102"};
            let payload = json!({"snapshotId":format!("snapshot-{page}"),"dom":format!("Order {target}: button ref=ref-{page}. {}", "Other row. ".repeat(40))});
            task.observe(format!("ev-{page}"),payload.clone()).unwrap();
            history.push(json!({"evidenceId":format!("ev-{page}"),"data":payload}));
        }
        task.checkpoint = json!({"pagesRead":steps});
        if scenario == "unknown-action" {
            task.actions.push(ActionRecord{id:"submit-once".into(),operation:"act".into(),state:"unknown".into(),evidence_id:Some("ev-29".into())});
            task.checkpoint["unresolved"] = json!(["Submission outcome unknown; do not repeat"]);
        }
        let records = json!([{"id":"A001","amount":"17.25","source":"ev-1"},{"id":"A002","amount":"82.10","source":"ev-2"}]);
        if scenario == "preserve-data" {task.checkpoint["records"] = records.clone();}
        let b = task.project(&schema);
        let mut a = b.clone();
        a["observationHistory"] = json!(history);
        cases.push(json!({"id":format!("history-{steps}-{scenario}"),"scenario":scenario,"expectedRef":if scenario=="absent"{Value::Null}else{json!(format!("ref-{steps}"))},"expectedRecords":records,"task":task,"A":a,"B":b}));
    }
    cases
}
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let cases = fixtures();
    if args.get(1).is_some_and(|a| a=="--validate") {
        let path = args.get(2).expect("report path");
        let mut report: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        if let Some(rows) = report["rows"].as_array_mut() {
            for row in rows {
                let fixture = cases.iter().find(|f| f["id"]==row["case"]).unwrap();
                let task: Task = serde_json::from_value(fixture["task"].clone()).unwrap();
                let parsed = Decision::parse(row["answer"].as_str().unwrap_or(""));
                let valid = parsed.as_ref().map(|d|task.validate_decision(d)).unwrap_or_else(|e|Err(e.clone()));
                row["productionValid"] = json!(valid.is_ok());
                if let Err(e) = valid {row["validationError"] = json!(e);}
                let semantic = parsed.as_ref().is_ok_and(|d| {
                    if fixture["scenario"]=="unknown-action" {return d.kind=="blocked";}
                    d.kind=="finish" && d.result.get("ref")==Some(&fixture["expectedRef"])
                        && (fixture["scenario"]!="preserve-data" || d.result["records"]==fixture["expectedRecords"])
                });
                row["correct"] = json!(semantic && row["productionValid"]==true);
            }
        }
        report["validator"] = json!("production core.rs Decision::parse + Task::validate_decision + fixture oracle");
        std::fs::write(path,serde_json::to_string_pretty(&report).unwrap()).unwrap();
    } else {
        println!("{}",serde_json::to_string(&cases).unwrap());
    }
}
