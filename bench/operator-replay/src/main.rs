//! Contract/context replay only. No API usage counters or GUI timings are invented.
#[path="../../../src-tauri/src/operator/core.rs"]
mod core;
use core::{Mode,State,Task,Tool};
use serde_json::{json,Value};
use std::time::Instant;

fn main(){
    let chrome:Value=serde_json::from_str(include_str!("../../../scripts/chrome-tool.json")).unwrap();
    let jianlai:Value=serde_json::from_str(include_str!("../../../scripts/jianlai-tool.json")).unwrap();
    let full_contracts=json!({"chrome":chrome,"jianlai":jianlai});
    let mut compact_contracts=full_contracts.clone();
    // Keep exactly the production native schemas; capability-description difference
    // is separated from history-only savings in the reported native-schema bytes.
    for tool in ["chrome","jianlai"]{compact_contracts[tool]["description"]=json!("Native input contract; choose tool freely, validate current target, do not replay uncertain inputs.");}
    let cases=[("single-step",2,0),("form",6,0),("cross-app-otp",10,32*1024),("long-history",24,128*1024),("long-table",30,32*1024),("canvas",12,16*1024),("tool-switches",20,64*1024),("navigation",16,16*1024),("same-name-targets",8,32*1024),("stale-observation",18,64*1024)];
    let mut rows=Vec::new();
    for (name,rounds,noise) in cases{
        for (label,mode) in [("A-reference-envelope",Mode::Original),("B-isolated",Mode::Isolated),("C-adaptive",Mode::Adaptive)]{
            let mut latencies=Vec::new();let mut total_bytes=0;let mut peak_bytes=0;let mut count=0;
            for _ in 0..25{
                let task:Task=serde_json::from_value(json!({"goal":name,"constraints":["do not submit twice"],"successCriteria":["fixture result visible"]})).unwrap();
                let mut s=State::new(task,mode).unwrap();let mut original_messages=Vec::new();let started=Instant::now();let mut bytes=0;let mut peak=0;
                for n in 0..rounds{
                    let tool=if n%3==0{Tool::Jianlai}else{Tool::Chrome};
                    let args=json!({"operation":if tool==Tool::Chrome{"inspect"}else{"screenshot"}});
                    // Synthetic fixture receipts, not recordings of the user's desktop.
                    let result=json!({"snapshotId":format!("{name}-{n}"),"text":format!("fixture-visible-region-{n}: {}","bounded page content ".repeat(110)),"items":[{"ref":format!("{n}:0"),"name":"test field","editable":true}]});
                    original_messages.push(json!({"role":"tool","name":tool.name(),"result":result,"arguments":args}));
                    s.record(format!("step-{n}"),tool,args,result,false,"coverage: checked fixture rows, no real user data".into());
                    let request=if mode==Mode::Original{json!({"parentHistory":"unrelated history ".repeat(noise/18),"nativeTools":full_contracts,"messages":original_messages,"task":name})}
                        else{let mut f=s.frame(None).unwrap();f["nativeTools"]=if mode==Mode::Adaptive{compact_contracts.clone()}else{full_contracts.clone()};f};
                    let len=serde_json::to_vec(&request).unwrap().len();bytes+=len;peak=peak.max(len);
                }
                latencies.push(started.elapsed().as_secs_f64()*1000.);total_bytes=bytes;peak_bytes=peak;count=rounds;
            }
            latencies.sort_by(f64::total_cmp);
            rows.push(json!({"case":name,"profile":label,"fixtureRounds":count,"repeats":25,"serializedInputBytesTotal":total_bytes,"peakSerializedInputBytes":peak_bytes,"assemblyMedianMs":latencies[12],"assemblyP95Ms":latencies[23],"actualModelCalls":null,"inputTokens":null,"outputTokens":null,"businessSuccess":null,"guiElapsedMs":null}));
        }
    }
    let report=json!({"kind":"synthetic-contract-context-replay","baselineCommit":"3da28d30f1adfd0da3b993813a47aa3fff20fdeb","sourceCommit":option_env!("GITHUB_SHA"),"nativeBatchLimitAllProfiles":8,"scope":"Same synthetic receipts in all profiles. A reconstructs a reference raw-tool envelope; it is NOT execution of the original Agent. B/C exercise the production State. No model or browser/desktop is invoked. Byte counts are NOT token counts. Assembly CPU time is NOT task latency. C replay uses a short capability placeholder, not the full production system prompt; isolated history-only byte counts should be used for stronger attribution.","rows":rows});
    let path=std::env::args().nth(1).unwrap_or("bench/operator-context.report.json".into());std::fs::write(&path,serde_json::to_vec_pretty(&report).unwrap()).unwrap();println!("wrote {path}: {} case/profile rows, 25 repetitions each; live metrics not measured",rows.len());
}
