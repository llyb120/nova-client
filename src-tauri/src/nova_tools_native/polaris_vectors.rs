// Optional learned retrieval. No provider is contacted without explicit user configuration.
use super::*;
use std::collections::BTreeMap;
use std::io::Write;
use reqwest::blocking::Client;

#[derive(Clone)]
struct Config {url:String,model:String,token:String,rerank:bool,key:String}
impl Config {
    fn load()->Result<Option<Self>,String>{
        let Ok(url)=std::env::var("NOVA_POLARIS_SEMANTIC_URL") else{return Ok(None)};
        let url=url.trim_end_matches('/').to_string();let parsed=reqwest::Url::parse(&url).map_err(|_|"invalid semantic URL")?;
        let local=matches!(parsed.host_str(),Some("127.0.0.1"|"[::1]"|"::1"));
        if !parsed.username().is_empty()||parsed.password().is_some()||parsed.query().is_some()||parsed.fragment().is_some(){return Err("semantic URL must not contain credentials/query/fragment".into());}
        if !(local&&parsed.scheme()=="http") && !(parsed.scheme()=="https"&&std::env::var("NOVA_POLARIS_ALLOW_REMOTE").as_deref()==Ok("1")){return Err("semantic service must use literal loopback HTTP; remote HTTPS requires explicit NOVA_POLARIS_ALLOW_REMOTE=1 consent".into());}
        let model=std::env::var("NOVA_POLARIS_SEMANTIC_MODEL").map_err(|_|"semantic model identity/revision must be configured")?;
        let token=std::env::var("NOVA_POLARIS_SEMANTIC_TOKEN").map_err(|_|"semantic service token must be configured")?;
        if model.is_empty()||model.len()>256||token.is_empty(){return Err("empty semantic model/token".into());}
        let key=index::digest(format!("v1|{url}|{model}").as_bytes());
        Ok(Some(Self{url,model,token,key,rerank:std::env::var("NOVA_POLARIS_RERANK").as_deref()==Ok("1")}))
    }
    fn client(&self,timeout:Duration)->Result<Client,String>{Client::builder().no_proxy().redirect(reqwest::redirect::Policy::none()).timeout(timeout).build().map_err(|_|"semantic client initialization failed".into())}
    fn post(&self,path:&str,body:Value,timeout:Duration)->Result<Value,String>{
        let response=self.client(timeout)?.post(format!("{}{path}",self.url)).bearer_auth(&self.token).json(&body).send().map_err(|_|"semantic service timeout/unavailable")?;
        if !response.status().is_success(){return Err(format!("semantic service HTTP {}",response.status().as_u16()));}
        let mut bytes=Vec::new();response.take(4*1024*1024+1).read_to_end(&mut bytes).map_err(|_|"semantic response read failed")?;
        if bytes.len()>4*1024*1024{return Err("semantic response too large".into());}
        serde_json::from_slice(&bytes).map_err(|_|"invalid semantic JSON".into())
    }
    fn embeddings(&self,texts:&[String],kind:&str,timeout:Duration)->Result<Vec<Vec<f32>>,String>{
        let v=self.post("/embed",serde_json::json!({"model":self.model,"kind":kind,"texts":texts}),timeout)?;
        if v["model"].as_str()!=Some(&self.model){return Err("semantic model identity changed".into());}
        let rows=v["vectors"].as_array().ok_or("missing vectors")?;
        if rows.len()!=texts.len(){return Err("embedding count mismatch".into());}
        let mut dimension=None;rows.iter().map(|row|{
            let mut out=row.as_array().ok_or("invalid vector")?.iter().map(|n|n.as_f64().filter(|n|n.is_finite()).map(|n|n as f32).ok_or("nonfinite embedding")).collect::<Result<Vec<_>,_>>()?;
            if !(16..=4096).contains(&out.len())||dimension.is_some_and(|d|d!=out.len()){return Err("invalid embedding dimension".into());}dimension=Some(out.len());
            let norm=out.iter().map(|n|(*n as f64).powi(2)).sum::<f64>().sqrt();if !norm.is_finite()||norm<1e-12{return Err("zero/nonfinite embedding".into());}
            for n in &mut out{*n=(*n as f64/norm)as f32;}Ok(out)
        }).collect()
    }
}
#[derive(Default)]
struct State {vectors:HashMap<String,Vec<f32>>,loading:bool,last_error:Option<String>,retry_at:Option<Instant>,loaded:bool}
static STATES:OnceLock<Mutex<BTreeMap<String,Arc<Mutex<State>>>>>=OnceLock::new();
#[derive(Serialize,Deserialize)]
struct Disk {version:u32,key:String,vectors:HashMap<String,Vec<f32>>}
fn state(root:&Path,c:&Config)->Arc<Mutex<State>>{
    let k=format!("{}:{}",normalize_root(root),c.key);let mut all=STATES.get_or_init(||Mutex::new(BTreeMap::new())).lock().unwrap_or_else(|e|e.into_inner());
    if !all.contains_key(&k)&&all.len()>=2{if let Some(old)=all.keys().next().cloned(){all.remove(&old);}}
    all.entry(k).or_default().clone()
}
fn load_disk(root:&Path,c:&Config,s:&mut State){
    if s.loaded{return;}s.loaded=true;let p=index::cache_file(root);
    if fs::metadata(&p).ok().is_none_or(|m|m.len()>256*1024*1024){return;}
    if let Ok(disk)=fs::read(&p).map_err(|e|e.to_string()).and_then(|b|serde_json::from_slice::<Disk>(&b).map_err(|e|e.to_string())){
        if disk.version==1&&disk.key==c.key&&disk.vectors.len()<=50000 {
            s.vectors=disk.vectors.into_iter().filter(|(id,v)|id.len()==64&&(16..=4096).contains(&v.len())&&v.iter().all(|n|n.is_finite())&&(v.iter().map(|n|(*n as f64).powi(2)).sum::<f64>()-1.0).abs()<0.01).collect();
        }
    }
}
fn save_disk(root:&Path,c:&Config,vectors:HashMap<String,Vec<f32>>){
    let path=index::cache_file(root);if let Some(p)=path.parent(){let _=fs::create_dir_all(p);}
    let tmp=path.with_extension(format!("{}.tmp",std::process::id()));
    let result=(||->Result<(),String>{let mut f=fs::File::create(&tmp).map_err(|e|e.to_string())?;serde_json::to_writer(&mut f,&Disk{version:1,key:c.key.clone(),vectors}).map_err(|e|e.to_string())?;f.flush().map_err(|e|e.to_string())?;drop(f);
        // A cache is disposable. Windows cannot rename onto an existing file.
        if path.exists(){fs::remove_file(&path).map_err(|e|e.to_string())?;}fs::rename(&tmp,&path).map_err(|e|e.to_string())})();if result.is_err(){let _=fs::remove_file(tmp);}
}
fn warm(root:PathBuf,c:Config,slot:Arc<Mutex<State>>,units:Vec<Arc<index::CodeUnit>>){
    let mut missing=Vec::new();{
        let mut s=slot.lock().unwrap_or_else(|e|e.into_inner());load_disk(&root,&c,&mut s);
        let valid=units.iter().map(|u|u.hash.as_str()).collect::<HashSet<_>>();s.vectors.retain(|k,_|valid.contains(k.as_str()));
        let mut seen=HashSet::new();for u in &units{if !s.vectors.contains_key(&u.hash)&&seen.insert(u.hash.clone()){missing.push((u.hash.clone(),u.passage.clone()));}}
    }
    let mut error=None;
    for batch in missing.chunks(8){
        // Index building is off the query path; one bounded batch at a time.
        match c.embeddings(&batch.iter().map(|(_,t)|t.clone()).collect::<Vec<_>>(),"passage",Duration::from_secs(30)){
            Ok(rows)=>{let mut s=slot.lock().unwrap_or_else(|e|e.into_inner());for ((hash,_),v) in batch.iter().zip(rows){s.vectors.insert(hash.clone(),v);}},
            Err(e)=>{error=Some(e);break;}
        }
    }
    let values={let mut s=slot.lock().unwrap_or_else(|e|e.into_inner());s.last_error=error;s.retry_at=s.last_error.as_ref().map(|_|Instant::now()+Duration::from_secs(30));s.loading=false;s.vectors.clone()};save_disk(&root,&c,values);
}
#[derive(Default)]
pub(super) struct Dense {pub scores:Vec<(usize,f64)>,pub mode:String,pub ready:usize,pub total:usize,pub note:Option<String>}
/// Explicit setup helper used by the local benchmark, not exposed as an agent tool argument.
pub(super) fn prepare(root:&Path,units:&[Arc<index::CodeUnit>])->Result<(),String>{
    let c=Config::load()?.ok_or("semantic service is not configured")?;let slot=state(root,&c);
    {let mut s=slot.lock().map_err(|_|"semantic index lock")?;if s.loading{return Err("semantic index is already building".into());}s.loading=true;}
    warm(root.to_path_buf(),c,slot.clone(),units.to_vec());let s=slot.lock().map_err(|_|"semantic index lock")?;
    if let Some(e)=&s.last_error{return Err(e.clone());}Ok(())
}
pub(super) fn search(root:&Path,units:&[Arc<index::CodeUnit>],query:&str,deadline:Instant)->Dense{
    let mut result=Dense{mode:"lexical".into(),total:units.len(),..Default::default()};
    let c=match Config::load(){Ok(Some(c))=>c,Ok(None)=>{result.note=Some("semantic model not configured; lexical/concept expansion only".into());return result},Err(e)=>{result.note=Some(e);return result}};
    let slot=state(root,&c);let mut start=false;
    let values={let mut s=slot.lock().unwrap_or_else(|e|e.into_inner());
        result.ready=units.iter().filter(|u|s.vectors.contains_key(&u.hash)).count();result.note=s.last_error.clone();
        if result.ready<units.len()&&!s.loading&&s.retry_at.is_none_or(|t|Instant::now()>t){s.loading=true;start=true;}
        units.iter().enumerate().filter_map(|(i,u)|s.vectors.get(&u.hash).map(|v|(i,v.clone()))).collect::<Vec<_>>()};
    if start{let root=root.to_path_buf();let cfg=c.clone();let units=units.to_vec();thread::spawn(move||warm(root,cfg,slot,units));}
    if values.is_empty(){result.mode="lexical+semantic-warming".into();return result;}
    let Some(remaining)=deadline.checked_duration_since(Instant::now()) else{result.note=Some("semantic query budget exhausted".into());return result};
    let rows=match c.embeddings(&[query.into()],"query",remaining.min(Duration::from_millis(1500))){Ok(v)=>v,Err(e)=>{result.note=Some(e);return result}};
    let q=&rows[0];result.scores=values.into_iter().filter(|(_,v)|v.len()==q.len()).map(|(i,v)|(i,v.iter().zip(q).map(|(a,b)|*a as f64 * *b as f64).sum())).collect();
    result.scores.sort_by(|a,b|b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));result.scores.truncate(64);
    result.mode=if result.ready==result.total{"hybrid"}else{"hybrid-partial"}.into();result
}
pub(super) fn rerank(query:&str,docs:&[String],deadline:Instant)->Result<Option<Vec<f64>>,String>{
    let Some(c)=Config::load()? else{return Ok(None)};if !c.rerank{return Ok(None);}
    let Some(remaining)=deadline.checked_duration_since(Instant::now())else{return Err("rerank budget exhausted".into())};
    let response=c.post("/rerank",serde_json::json!({"model":c.model,"query":query,"texts":docs}),remaining.min(Duration::from_millis(1500)))?;
    let scores=response["scores"].as_array().ok_or("missing rerank scores")?;if scores.len()!=docs.len(){return Err("rerank count mismatch".into());}
    Ok(Some(scores.iter().map(|v|v.as_f64().filter(|n|n.is_finite()).ok_or_else(||"nonfinite rerank score".into())).collect::<Result<Vec<_>,String>>()?))
}
