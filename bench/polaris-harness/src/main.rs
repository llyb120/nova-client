// Exact production modules, no ported/mock retrieval implementation.
#[path="../../../src-tauri/src/nova_tools_native/mod.rs"]
mod engine;
use std::{io::{self,BufRead,Write},path::Path,time::Instant};
use serde_json::{json,Value};
fn main(){
    for line in io::stdin().lock().lines(){let Ok(line)=line else{break};let started=Instant::now();
        let result=(||->Result<Value,String>{let v:Value=serde_json::from_str(&line).map_err(|e|e.to_string())?;
            let root=Path::new(v["root"].as_str().ok_or("root is required")?);let mode=v["mode"].as_str().unwrap_or("candidate");
            let p=v.get("params").cloned().unwrap_or_else(||json!({}));
            if mode=="prepare"{return engine::lexical::semantic::prepare_semantic_index(root);}
            let text=if mode=="baseline"{engine::lexical::polaris(root,p)}else{engine::context::polaris(root,p)}?;
            Ok(json!({"text":text}))})();
        let response=match result{Ok(v)=>json!({"ok":true,"result":v,"ms":started.elapsed().as_secs_f64()*1000.0}),Err(e)=>json!({"ok":false,"error":e,"ms":started.elapsed().as_secs_f64()*1000.0})};
        println!("{response}");let _=io::stdout().flush();
    }
}
