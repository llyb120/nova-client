//! Data-only CodeBuddy ACP decision adapter. Not a second general coding agent.
//! Explicit CLI whitelist, strict empty MCP config, no transcripts and no native
//! permission approvals. Unsupported CLI flags fail startup, never fall back.
use super::*;
use crate::operator::{Factory, Model, ModelFuture, Reply, Registration};

#[derive(Default)]
pub(super) struct Capture {
    pub sid: String,
    text: String,
    usage: CodeBuddyTurnUsage,
    pub violation: bool,
}
impl Capture {
    pub fn update(&mut self, params: &Value) {
        if params["sessionId"].as_str()!=Some(self.sid.as_str()) {return;}
        let u=&params["update"];
        match u["sessionUpdate"].as_str() {
            Some("agent_message_chunk")=>{
                let text=extract_text(&u["content"]);
                if self.text.len().saturating_add(text.len())>128*1024 {self.violation=true;} else {self.text.push_str(&text);}
            }
            Some("usage_update")=>self.usage.update(u),
            Some("tool_call"|"tool_call_update")=>self.violation=true,
            _=>{}
        }
    }
}

pub(super) fn current_option(config: &Value, name: &str) -> Option<String> {
    config["configOptions"].as_array()?.iter().find(|c|c["id"]==name)?["currentValue"]
        .as_str().filter(|s|!s.is_empty()).map(str::to_string)
}

pub(super) fn bind(parent:&Arc<AcpManager>, thread:&str, sid:&str) -> Option<Registration> {
    if parent.kind!=AgentKind::CodeBuddy || !crate::operator::enabled() {return None;}
    let (root,read_only)={
        let state=parent.app.state::<AppState>();let store=state.store.lock().unwrap();
        let t=store.get(thread)?;
        (PathBuf::from(&t.cwd),t.mode.as_deref().map(unify_mode_id).as_deref()==Some("plan"))
    };
    if read_only {return None;}
    // Read the applied, session-local configuration, NOT the visible picker or a
    // global model cache. Unknown defaults are blocked instead of silently guessed.
    let config=parent.operator_configs.lock().unwrap().get(sid).cloned().unwrap_or(Value::Null);
    let (model,effort)={let routes=parent.routes.lock().unwrap();let route=routes.get(sid)?;
        (route.applied_model.clone().or_else(||config.pointer("/models/currentModelId").and_then(Value::as_str).map(str::to_string)).or_else(||current_option(&config,"model")),
         route.applied_effort.clone().or_else(||current_option(&config,"thought_level")))
    };
    let parent=parent.clone();let scope=parent.cwd_change_scope(thread);
    let thread=thread.to_owned();
    let factory:Factory=Arc::new(move||Box::new(CodeBuddyModel::new(&parent,thread.clone(),model.clone(),effort.clone())));
    Some(crate::operator::register(scope,root,Arc::new(AtomicBool::new(false)),factory))
}

struct CodeBuddyModel {
    parent: std::sync::Weak<AcpManager>,
    parent_thread: String,
    manager: Arc<AcpManager>,
    settings: Settings,
    model: Option<String>,
    effort: Option<String>,
    conn: Option<Arc<AcpConn>>,
    sid: String,
    epoch: usize,
    directory: PathBuf,
}
impl CodeBuddyModel {
    fn new(parent:&Arc<AcpManager>,parent_thread:String,model:Option<String>,effort:Option<String>)->Self {
        let manager=AcpManager::new_with_env(parent.app.clone(),parent.kind.clone(),parent.launch_env.clone(),format!("operator-{}",uuid::Uuid::new_v4()));
        manager.operator_only.store(true,Ordering::SeqCst);
        let settings=parent.app.state::<AppState>().settings.lock().unwrap().clone();
        Self{parent:Arc::downgrade(parent),parent_thread,manager,settings,model,effort,conn:None,sid:String::new(),epoch:0,directory:std::env::temp_dir().join(format!("nova-operator-{}",uuid::Uuid::new_v4()))}
    }
    async fn open(&mut self)->Result<(),String>{
        let model=self.model.as_deref().filter(|s|!s.is_empty()).ok_or("Operator 无法核实主会话实际模型；请明确选择模型后重试")?;
        let effort=self.effort.as_deref().filter(|s|!s.is_empty()).ok_or("Operator 无法核实主会话实际思考档位；请明确选择档位后重试")?;
        tokio::fs::create_dir_all(&self.directory).await.map_err(|e|e.to_string())?;
        let cwd=self.directory.to_string_lossy().into_owned();
        let conn=self.manager.spawn_codebuddy_stdio_conn(&self.settings,"operator",Some(&cwd)).await?;
        // Store the child BEFORE fallible initialization so Drop kills it on error.
        self.conn=Some(conn.clone());
        let session=conn.request("session/new",json!({"cwd":cwd,"mcpServers":[]}),Some(Duration::from_secs(60))).await?;
        self.sid=session["sessionId"].as_str().ok_or("operator ACP missing sessionId")?.into();
        conn.request("session/set_config_option",json!({"sessionId":self.sid,"configId":"model","value":model}),Some(Duration::from_secs(30))).await.map_err(|e|format!("Operator 不会回退模型：{e}"))?;
        conn.request("session/set_config_option",json!({"sessionId":self.sid,"configId":"thought_level","value":effort}),Some(Duration::from_secs(30))).await.map_err(|e|format!("Operator 不会回退思考档位：{e}"))?;
        Ok(())
    }
}
impl Drop for CodeBuddyModel {fn drop(&mut self){if let Some(conn)=self.conn.take(){conn.kill();}let _=std::fs::remove_dir_all(&self.directory);}}
impl Model for CodeBuddyModel {
    fn account_usage(&self,run_id:&str,metrics:&Value){
        let Some(parent)=self.parent.upgrade() else{return};
        let mut turns=parent.codebuddy_turn_usage.lock().unwrap();
        let Some(turn)=turns.get_mut(&self.parent_thread) else{return};
        turn.add_operator(run_id, metrics);
    }
    fn identity(&self)->Value{json!({"agent":"codebuddy","transport":"isolated-acp","model":self.model,"reasoningEffort":self.effort,"inherited":self.model.is_some()&&self.effort.is_some()})}
    fn decide<'a>(&'a mut self,frame:&'a Value,schema:&'a Value,epoch:usize,cancel:&'a Arc<AtomicBool>)->ModelFuture<'a>{Box::pin(async move{
        let reset=self.conn.is_none()||epoch!=self.epoch;
        if reset {if let Some(conn)=self.conn.take(){conn.kill();}self.open().await?;self.epoch=epoch;}
        if cancel.load(Ordering::SeqCst){return Err("Operator cancelled".into());}
        // ACP retains history. Append only the newest data while this session lives;
        // on a bounded rebase start with a full working frame, never parent's history.
        let mut data=frame.clone();
        if !reset {
            data.as_object_mut().unwrap().remove("nativeTools");
            if let Some(recent)=data["recent"].as_array_mut(){if recent.len()>1 {let last=recent.pop().unwrap();*recent=vec![last];}}
        }
        let mut prompt=crate::operator::native::image_parts(frame).await?;
        // CodeBuddy treats the final text block as the current prompt.
        prompt.push(json!({"type":"text","text":if reset {format!("{}\nDecision schema: {}\nTask frame: {}",crate::operator::core::SYSTEM,schema,data)} else {format!("Latest task state (replaces stale observations; preserve user constraints): {data}")}}));
        *self.manager.operator_capture.lock().unwrap()=Capture{sid:self.sid.clone(),..Default::default()};
        let conn=self.conn.as_ref().unwrap().clone();
        let request=conn.request("session/prompt",json!({"sessionId":self.sid,"prompt":prompt}),None);
        let cancelled=async{while !cancel.load(Ordering::SeqCst){sleep(Duration::from_millis(25)).await;}};
        let result=tokio::select!{r=request=>r,_=cancelled=>{conn.notify("session/cancel",json!({"sessionId":self.sid}));conn.kill();return Err("Operator cancelled".into());}}?;
        let capture=std::mem::take(&mut *self.manager.operator_capture.lock().unwrap());
        if capture.violation {return Err("CodeBuddy attempted native tools or oversized output; Operator stopped without dispatching that decision".into());}
        let usage=capture.usage.finish().or_else(||{
            let u=result.get("usage")?;
            let i=u.get("inputTokens")?.as_u64()?;let o=u.get("outputTokens")?.as_u64()?;
            Some(json!({"inputTokens":i,"outputTokens":o}))
        });
        Ok(Reply{text:capture.text,usage})
    })}
}

#[cfg(test)]mod tests{
    #[test]fn operator_usage_is_counted_once_with_parent(){
        let mut u=super::CodeBuddyTurnUsage::default();
        u.messages.insert("parent".into(),(40,5));
        let child=serde_json::json!({"usageComplete":true,"usage":{"inputTokens":100,"outputTokens":20}});
        u.add_operator("child-a",&child);u.add_operator("child-a",&child);
        assert_eq!(u.finish().unwrap()["totalTokens"],165);
    }
    #[test]fn operator_unknown_usage_preserves_known_counts_without_fabricating_total(){
        let mut u=super::CodeBuddyTurnUsage::default();
        u.messages.insert("parent".into(),(40,5));
        u.add_operator("child-a",&serde_json::json!({"usageComplete":false,"usage":null,"knownUsage":{"inputTokens":100,"outputTokens":20}}));
        let v=u.finish().unwrap();assert_eq!(v["usageComplete"],false);assert!(v.get("totalTokens").is_none());assert_eq!(v["knownUsage"]["inputTokens"],140);
    }
    #[test]fn operator_unknown_usage_does_not_fall_back_to_parent_only_total(){
        let mut u=super::CodeBuddyTurnUsage::default();u.add_operator("child-a",&serde_json::json!({}));
        let v=u.finish().unwrap();assert_eq!(v["usageComplete"],false);assert!(v.get("totalTokens").is_none());
    }
    use super::*;
    #[test]fn no_global_default_selection(){assert!(current_option(&json!({}),"model").is_none());assert_eq!(current_option(&json!({"configOptions":[{"id":"model","currentValue":"chosen"}]}),"model"),Some("chosen".into()));}
    #[test]fn capture_is_session_bound(){let mut c=Capture{sid:"mine".into(),..Default::default()};c.update(&json!({"sessionId":"other","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"secret"}}}));assert!(c.text.is_empty());c.update(&json!({"sessionId":"mine","update":{"sessionUpdate":"tool_call"}}));assert!(c.violation);}
}
