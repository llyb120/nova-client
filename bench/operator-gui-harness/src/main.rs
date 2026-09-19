//! Native control integration harness. The UI shell/profile and model transport are
//! test adapters; Operator runtime, Chrome transport/engine, Jianlai and guards are
//! imported unmodified from production. No mock screenshot or input callbacks.
#[path = "../../../src-tauri/src/operator/core.rs"]
mod core;
#[path = "../../../src-tauri/src/operator/runtime.rs"]
#[allow(dead_code)]
mod runtime;
#[path = "../../../src-tauri/src/native_browser.rs"]
#[allow(dead_code)]
mod native_browser;
#[path = "../../../src-tauri/src/chrome_browser.rs"]
#[allow(dead_code)]
mod chrome_browser;
#[path = "../../../src-tauri/src/jianlai.rs"]
#[allow(dead_code)]
mod jianlai;
#[path = "../../../src-tauri/src/visual_guard.rs"]
#[allow(dead_code)]
mod visual_guard;
#[path = "../../../src-tauri/src/tool_experience.rs"]
#[allow(dead_code)]
mod tool_experience;
mod operator { pub use crate::runtime::check_access; }
// Isolated configuration adapter, never reads the user's actual Nova profile.
mod lyra { pub mod config { pub fn nova_root() -> std::path::PathBuf {
    std::env::var_os("OPERATOR_TEST_DATA").expect("isolated data required").into()
} } }
use serde_json::{json, Value};
use std::{collections::HashMap, io::{self, BufRead, Write}, path::PathBuf, sync::{Arc,Mutex}, time::Instant};
use tauri::Manager;
struct Thread { cwd: String }
// Only the shell metadata fields used by the production browser modules.
struct AppState { config_dir: PathBuf, active_thread: Mutex<Option<String>>, store: Mutex<HashMap<String,Thread>> }
fn emit(value: Value) { let mut out=io::stdout().lock(); writeln!(out,"{value}").unwrap(); out.flush().unwrap(); }
async fn native(channel: &str, root: &std::path::Path, args:&Value, owner:&str)->Result<Value,String>{
    if channel=="chrome" {native_browser::execute_chrome(root,args,owner).await}
    else if channel=="jianlai" {jianlai::execute(root,args,owner).await}
    else {Err("unsupported native channel".into())}
}
fn main(){
    let root=PathBuf::from(std::env::var_os("OPERATOR_TEST_WORKSPACE").expect("isolated workspace required"));
    let data=lyra::config::nova_root(); std::fs::create_dir_all(&root).unwrap();std::fs::create_dir_all(&data).unwrap();
    let model=std::env::var("OPERATOR_TEST_MODEL").expect("resolved model required");
    let endpoint=std::env::var("OPERATOR_TEST_CALLBACK").expect("local model adapter required");
    assert!(endpoint.starts_with("http://127.0.0.1:"));
    let cb_token=std::env::var("OPERATOR_TEST_CALLBACK_TOKEN").expect("callback token required");
    let state=AppState{config_dir:data.clone(),active_thread:Mutex::new(None),store:Mutex::new(HashMap::new())};
    tauri::Builder::default().manage(state).setup(move|app|{
        // Hidden shell: all visible test UI belongs to the real Chromium process.
        let _window=tauri::WindowBuilder::new(app,"main").title("Operator test host").visible(false).build()?;
        native_browser::init(app.handle());
        let handle=app.handle().clone();
        std::thread::spawn(move||{
            let client=reqwest::Client::builder().no_proxy().timeout(std::time::Duration::from_secs(80)).build().unwrap();
            let mut registrations:HashMap<String,runtime::Registration>=HashMap::new();
            emit(json!({"ready":true,"model":model,"scope":"production Operator + native Chrome/Jianlai; test shell and HTTP model adapter"}));
            for line in io::stdin().lock().lines(){
                let Ok(line)=line else{break};
                let command:Value=match serde_json::from_str(&line){Ok(v)=>v,Err(e)=>{emit(json!({"error":e.to_string()}));continue}};
                let id=command["id"].clone();
                let method=command["method"].as_str().unwrap_or("");
                if method=="close"{emit(json!({"id":id,"result":{"closed":true}}));break;}
                let result:Result<Value,String>=tauri::async_runtime::block_on(async{
                    match method {
                        "native"=>native(command["channel"].as_str().unwrap_or(""),&root,&command["args"],"harness-setup").await,
                        "bind"=>{
                            let run=command["run"].as_str().ok_or("run required")?.to_string();
                            let callback=endpoint.clone();let token=cb_token.clone();let http=client.clone();let label=run.clone();
                            let decide:runtime::Decide=Arc::new(move|input|{
                                let http=http.clone();let callback=callback.clone();let token=token.clone();let label=label.clone();
                                Box::pin(async move{
                                    let body=json!({"run":label,"context":input.context,"images":input.images,"system":runtime::SYSTEM});
                                    let response=http.post(&callback).header("X-Test-Token",token).json(&body).send().await.map_err(|e|e.to_string())?;
                                    let code=response.status();let answer:Value=response.json().await.map_err(|e|e.to_string())?;
                                    if !code.is_success(){return Err(answer["error"].as_str().unwrap_or("model adapter failed").into());}
                                    answer["text"].as_str().map(String::from).ok_or("missing decision".into())
                                })
                            });
                            let label=run.clone();
                            let r=runtime::register(&run,root.clone(),data.clone(),core::ModelIdentity{agent:"commandcode-api-test-parent".into(),model:model.clone(),reasoning_effort:Some("low".into())},decide,runtime::Native{
                                chrome:chrome_browser::tool_definition(),jianlai:jianlai::tool_definition(),
                                execute:Arc::new(move|channel,root,args,owner|{
                                    let label=label.clone();Box::pin(async move{
                                        let start=Instant::now();let result=native(&channel,&root,&args,&owner).await;
                                        emit(json!({"event":"native","run":label,"channel":channel,"args":args,"result":result.as_ref().ok(),"error":result.as_ref().err(),"elapsedMs":start.elapsed().as_secs_f64()*1000.}));
                                        result
                                    })
                                })
                            });
                            let scope=r.scope.clone();registrations.insert(run,r);Ok(json!({"scope":scope}))
                        },
                        "operate"=>runtime::execute(command["scope"].as_str().ok_or("scope required")?,&root,&command["args"]).await,
                        _=>Err("unsupported harness command".into())
                    }
                });
                match result {Ok(value)=>emit(json!({"id":id,"result":value})),Err(error)=>emit(json!({"id":id,"error":error}))}
            }
            handle.exit(0);
        });
        Ok(())
    }).run(tauri::generate_context!()).expect("native harness failed");
}
