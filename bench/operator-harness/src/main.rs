//! Deterministic footprint A/B. Uses the production projector, not a copy.
//! No LLM, screenshots or real desktop are involved; not a success-rate benchmark.
#[path = "../../../src-tauri/src/operator/core.rs"]
mod core;
#[path = "../../../src-tauri/src/operator/runtime.rs"]
#[allow(dead_code)]
mod runtime;
use core::{ModelIdentity,Request,Task};
use serde_json::{json,Value};

fn main() {
    let schema:Value=serde_json::from_str(include_str!("../../../scripts/chrome-tool.json")).unwrap();
    let mut reports=vec![];
    for steps in [10,30,60,120] {
        let contract=Request::parse(&json!({"op":"run","requestKey":"ab","goal":"Read every fixture page, preserving coverage","channel":"chrome","constraints":["Never submit or edit"],"acceptance":["Read all fixture pages"]})).unwrap();
        let mut t=Task::new("fixture".into(),"fixture".into(),"fixture".into(),ModelIdentity{agent:"fixture-no-model".into(),model:"none".into(),reasoning_effort:None},contract);
        let mut history=vec![];let mut a_bytes=0usize;let mut b_bytes=0usize;let mut a_images=0;let mut b_images=0;
        for page in 1..=steps {
            let observation=json!({"snapshotId":format!("s{page}"),"dom":format!("Page {page}: {}", "fixture order row; ".repeat(450)),"images":[{"imageId":format!("image-{page}"),"path":format!("fixture-{page}.png"),"pixelWidth":1280,"pixelHeight":720}]});
            history.push(core::without_blobs(&observation));
            t.observe(format!("ev{page}"),observation).unwrap();t.checkpoint=json!({"pagesRead":page,"recordsRead":page*25,"unresolved":steps-page});
            let b=t.project(&schema);
            let mut a=b.clone();a["currentObservation"]=Value::Null;a["observationHistory"]=json!(history);
            a_bytes+=serde_json::to_vec(&a).unwrap().len();b_bytes+=serde_json::to_vec(&b).unwrap().len();a_images+=page;b_images+=1;
        }
        reports.push(json!({"steps":steps,"A":{"inputTextBytes":a_bytes,"referencedImageCount":a_images},"B":{"inputTextBytes":b_bytes,"referencedImageCount":b_images},"textByteReductionPercent":100.0*(1.0-b_bytes as f64/a_bytes as f64),"imageReferenceReductionPercent":100.0*(1.0-b_images as f64/a_images as f64)}));
    }
    println!("{}",serde_json::to_string_pretty(&json!({"benchmark":"deterministic-production-projector-ab","baseline":"append-only fixture observations, not an unmodified CodeBuddy runtime","modelCalls":0,"realGuiActions":0,"tokenEstimates":false,"measures":"cumulative text bytes and image references only; not model cost, latency or success rate","cases":reports})).unwrap());
}
