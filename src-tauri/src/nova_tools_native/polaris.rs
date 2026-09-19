// Semantic facade over the preserved exact-symbol engine. All native callers use this boundary.
use super::*;
mod query {include!("polaris_query.rs");}
mod index {include!("polaris_index.rs");}
mod vectors {include!("polaris_vectors.rs");}
mod rank {include!("polaris_rank.rs");}
use query::Query;
use index::CodeUnit;

pub fn fast_context(root:&Path,params:Value)->Result<String,String>{polaris(root,params)}
pub fn polaris(root:&Path,params:Value)->Result<String,String>{
    let started=Instant::now();let q=Query::parse(params)?;
    // Keep the established graph packer and latency for already-known symbols/files.
    if q.task.is_empty(){return super::polaris(root,q.params);}
    let output=retrieve(root,&q,started)?;
    eprintln!("[nova-tools-profile] polaris.hybrid: {:.2}ms",started.elapsed().as_secs_f64()*1000.0);Ok(output)
}
fn lexical_rank(corpus:&index::Corpus,units:&[Arc<CodeUnit>],q:&Query)->Vec<(usize,f64)>{
    let n=corpus.units.len().max(1) as f64;
    let mut rows=Vec::new();
    for (i,u) in units.iter().enumerate(){
        let mut score=0.0;let mut covered=0;
        for (term,weight) in &q.terms {if let Some(tf)=u.terms.get(term){let df=*corpus.df.get(term).unwrap_or(&1) as f64;let idf=(1.0+(n-df+0.5)/(df+0.5)).ln();score+=weight*idf*tf*2.2/(tf+1.2*(0.25+0.75*u.length/corpus.average));if *weight==1.0 {covered+=1;}}}
        // A named action/object is an independent field, not drowned out by
        // repeated prose in a large component or by tiny incidental call sites.
        for (term,weight) in &q.terms {if u.name_terms.contains(term){let df=*corpus.df.get(term).unwrap_or(&1) as f64;score+=4.0*weight*(1.0+(n-df+0.5)/(df+0.5)).ln();}}
        // Identity is independent from frequency and the number of noisy files.
        let exact=q.anchors.iter().any(|a|a.eq_ignore_ascii_case(&u.name));
        let explicit=q.files.iter().any(|f|f==&u.file);
        if exact{score+=40.0;}if explicit{score+=30.0;}
        if score>0.0 {rows.push((i,score*(1.0+(covered.min(12) as f64)*0.025)));}
    }rows.sort_by(|a,b|b.1.total_cmp(&a.1).then_with(||units[a.0].file.cmp(&units[b.0].file)).then(units[a.0].start.cmp(&units[b.0].start)));rows.truncate(96);rows
}
fn identity(u:&CodeUnit)->(String,String,usize){(u.file.clone(),u.name.clone(),u.owner_start)}
fn related(a:&CodeUnit,b:&CodeUnit,files:&HashSet<String>)->Option<&'static str>{
    if identity(a)==identity(b)||b.name=="<module>"||b.name.len()<3{return None;}
    let calls=|from:&CodeUnit,to:&CodeUnit|{
        rank::qualified_call(from,to,files)||
        (from.file==to.file&&from.calls.contains(&to.name))||from.imports.iter().any(|i|{
            (i.orig.as_deref().unwrap_or(&i.name)==to.name&&from.calls.contains(&i.name)||from.members.iter().any(|(object,member)|object==&i.name&&member==&to.name))
                &&resolve_specifier(&i.from,&from.file,files).as_deref()==Some(&to.file)
        })
    };
    if calls(a,b){return Some("callee-reference");}
    if calls(b,a){return Some("caller-reference");}
    if (a.commands.contains(&b.name)&&b.passage.contains("#[tauri::command"))||(b.commands.contains(&a.name)&&a.passage.contains("#[tauri::command")){return Some("command-reference");}
    if a.events.iter().any(|(kind,event)|b.events.iter().any(|(other,value)|event==value&&((kind=="emit")!=(other=="emit")))){return Some("event-reference");}
    None
}
fn retrieve(root:&Path,q:&Query,started:Instant)->Result<String,String>{retrieve_attempt(root,q,started,false)}
fn retrieve_attempt(root:&Path,q:&Query,started:Instant,retried:bool)->Result<String,String>{
    let root=root.canonicalize().map_err(|e|e.to_string())?;
    let deadline=started+Duration::from_millis(3500);
    let corpus=index::corpus(&root,started+Duration::from_millis(2200))?;
    let mut units=corpus.units.iter().filter(|u|u.role=="implementation"||(q.test_intent&&u.role=="test")||(q.doc_intent&&u.role=="documentation")||q.files.contains(&u.file)).cloned().collect::<Vec<_>>();
    let known=units.iter().map(|u|u.file.clone()).collect::<HashSet<_>>();
    units.extend(index::explicit_units(&root,&q.files,&known,deadline));
    let semantic_query=q.semantic_query();
    let (lexical,dense)=thread::scope(|scope| {
        if std::env::var_os("NOVA_POLARIS_SEMANTIC_URL").is_some() {
            let pending=scope.spawn(||vectors::search(&root,&units,&semantic_query,deadline));
            let lexical=lexical_rank(&corpus,&units,q);
            let dense=pending.join().unwrap_or_else(|_|vectors::Dense{mode:"lexical".into(),note:Some("semantic worker failed; lexical evidence retained".into()),..Default::default()});
            (lexical,dense)
        } else {(lexical_rank(&corpus,&units,q),vectors::search(&root,&units,&semantic_query,deadline))}
    });
    // Refine each recall channel independently before fusion. Otherwise a
    // semantically popular wrapper can erase the lexical channel's concrete body.
    let mut ranked=rank::fuse(&lexical,&dense.scores,&units,q);
    ranked.truncate(32);
    // An in-flight edit invalidates old semantic results as well as lexical evidence.
    let stale=ranked.iter().filter(|(i,_)|!index::verified(&root,&units[*i])).map(|(i,_)|units[*i].file.clone()).collect::<HashSet<_>>();
    if !stale.is_empty(){index::invalidate(&root,&stale);if !retried&&Instant::now()<deadline{return retrieve_attempt(&root,q,started,true);}}
    ranked.retain(|(i,_)|!stale.contains(&units[*i].file));
    let mut rerank_note=None;let mut reranked=false;
    let limit=ranked.len().min(16);
    let docs=ranked.iter().take(limit).map(|(i,_)|units[*i].passage.clone()).collect::<Vec<_>>();
    if !docs.is_empty(){match vectors::rerank(&q.task,&docs,deadline){Ok(Some(values))=>{
        // Exact user identities keep priority. The optional cross-encoder orders the rest.
        for ((i,score),new) in ranked.iter_mut().take(limit).zip(values){*score=if q.files.contains(&units[*i].file)||q.anchors.iter().any(|a|a.eq_ignore_ascii_case(&units[*i].name)){1e6}else{new};}
        ranked[..limit].sort_by(|a,b|b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));reranked=true;
    },Ok(None)=>{},Err(e)=>rerank_note=Some(e)}}
    let missing=q.anchors.iter().filter(|a|!units.iter().any(|u|u.name.eq_ignore_ascii_case(a))).cloned().collect::<Vec<_>>();
    let mut chosen=Vec::<(usize,&str)>::new();let mut names=HashSet::new();let mut counts=HashMap::<String,usize>::new();
    // Primary implementations always get body budget before helpers and file limits.
    for (i,_) in &ranked {
        let u=&units[*i];if *counts.get(&u.file).unwrap_or(&0)>=2{continue;}
        if !index::verified(&root,u){continue;}
        chosen.push((*i,"primary"));names.insert(identity(u));*counts.entry(u.file.clone()).or_default()+=1;
        if chosen.len()==4{break;}
    }
    let files=units.iter().map(|u|u.file.clone()).collect::<HashSet<_>>();
    let mut frontier=chosen.iter().take(2).map(|(i,_)|(*i,0)).collect::<std::collections::VecDeque<_>>();
    let mut links=Vec::new();
    while let Some((parent,depth))=frontier.pop_front() {
        if depth>=2||chosen.len()>=9||Instant::now()>deadline{continue;}
        let mut neighbours=Vec::new();
        for (i,u) in units.iter().enumerate(){if Instant::now()>deadline{break;}if names.contains(&identity(u)){continue;}
            if let Some(role)=related(&units[parent],u,&files){let value=lexical.iter().find(|(j,_)|*j==i).map(|(_,v)|*v).unwrap_or(0.0);neighbours.push((i,role,value));}}
        neighbours.sort_by(|a,b|b.2.total_cmp(&a.2).then(a.0.cmp(&b.0)));
        for (i,role,_) in neighbours.into_iter().take(2){if chosen.len()>=9{break;}if names.insert(identity(&units[i]))&&index::verified(&root,&units[i]){
            chosen.push((i,role));frontier.push_back((i,depth+1));
            links.push(serde_json::json!({"from":format!("{}:{}",units[parent].file,units[parent].name),"to":format!("{}:{}",units[i].file,units[i].name),"kind":role}));
        }}
    }
    let mut body=String::new();let mut gaps=Vec::new();let mut evidence=Vec::new();let mut lines_left=q.lines;
    for (id,relation) in chosen{
        let u=&units[id];let source=&u.source;
        let full_start=if u.start<=u.owner_start{u.start}else{u.owner_start};
        let full_fit=u.owner_end-full_start+1<=lines_left && source[full_start-1..u.owner_end].iter().map(|s|s.len()+1).sum::<usize>()+body.len()+2048<q.hard;
        let (start,mut end)=if full_fit{(full_start,u.owner_end)}else{(u.start,u.end)};
        // Include whole lines only; a pathological one-line literal is a deferred read.
        while end>=start && (source[start-1..end].iter().enumerate().map(|(n,s)|s.len()+format!("{}: ",start+n).len()+1).sum::<usize>()+body.len()+4096>q.hard || end-start+1>lines_left) {
            if end==start {end=start-1;break;} end-=1;
        }
        if end<start{gaps.push(serde_json::json!({"file":u.file,"start":u.owner_start,"end":u.owner_end,"reason":"line-exceeds-budget"}));continue;}
        let full_fit=full_fit&&end==u.owner_end;
        let snippet=source[start-1..end].iter().enumerate().map(|(n,s)|format!("{}: {}\n",start+n,s)).collect::<String>();
        let section=format!("\n### {}:{}-{} [{} {}]\ncoverage: {} {}:{}-{}\n```\n{snippet}```\n",u.file,start,end,relation,u.name,if full_fit{"BODY"}else{"PARTIAL"},u.file,start,end);
        if body.len()+section.len()+2048>q.hard||end-start+1>lines_left{gaps.push(serde_json::json!({"file":u.file,"start":u.owner_start,"end":u.owner_end,"reason":"budget"}));continue;}
        lines_left-=end-start+1;body.push_str(&section);
        evidence.push(serde_json::json!({"file":u.file,"symbol":u.name,"start":start,"end":end,"role":u.role,"relation":relation,"sourceHash":u.file_hash,"complete":full_fit}));
        if !full_fit{gaps.push(serde_json::json!({"file":u.file,"start":u.owner_start,"end":u.owner_end,"reason":"large-unit"}));}
    }
    let time_budget_reached=Instant::now()>deadline;
    let partial=corpus.partial||!gaps.is_empty()||dense.mode.contains("partial")||dense.mode.contains("warming")||time_budget_reached;
    let status=if partial{"PARTIAL"}else if evidence.is_empty(){"MISS"}else{"CANDIDATES"};
    let unresolved_files=q.files.iter().filter(|f|!units.iter().any(|u|&u.file==*f)).cloned().collect::<Vec<_>>();
    let meta=serde_json::json!({"backend":dense.mode,"reranked":reranked,"semanticReady":dense.ready,"semanticTotal":dense.total,"note":dense.note,"rerankNote":rerank_note,
        "files":corpus.files,"units":units.len(),"changedFiles":corpus.changed,"indexPartial":corpus.partial,"timeBudgetReached":time_budget_reached,"refreshedAfterEdit":retried,"unresolvedAnchors":missing,"unresolvedFiles":unresolved_files,"links":links,
        "couplingNote":if q.params["coupling"].as_bool()==Some(true){Some("natural-language results use source references; git co-change hints require an exact-symbol query")}else{None},"queryMs":started.elapsed().as_secs_f64()*1000.0,"evidence":evidence,"next_reads":gaps});
    let header=format!("# CTX {status}\n# retrieval: {meta}\n# Evidence is verified current source; relevance/one-hop references are candidates, not a proof of root cause.\n");
    let mut out=format!("{header}{body}");
    // Metadata is bounded too. Do not split a code body or lie about its coverage.
    if out.len()>q.hard {
        // Keep the verified source; trim diagnostic verbosity, never silently crop code.
        let compact=serde_json::json!({"backend":meta["backend"],"indexPartial":meta["indexPartial"],"evidence":meta["evidence"],"next_reads":meta["next_reads"]});
        out=format!("# CTX PARTIAL\n# retrieval: {compact}\n{body}");
        if out.len()>q.hard {return Err("结果元数据超过预算，请限定 files".into());}
    }
    Ok(out)
}
/// Index construction is separated from online latency in A/B. Not an agent action.
pub(crate) fn prepare_semantic_index(root:&Path)->Result<Value,String>{
    let started=Instant::now();let corpus=index::corpus(root,started+Duration::from_secs(120))?;
    if corpus.partial{return Err("cannot benchmark an incomplete semantic index".into());}
    let units=corpus.units.iter().filter(|u|u.role=="implementation").cloned().collect::<Vec<_>>();
    vectors::prepare(root,&units)?;Ok(serde_json::json!({"files":corpus.files,"units":units.len(),"ms":started.elapsed().as_secs_f64()*1000.0}))
}

#[cfg(test)]
mod semantic_tests {
    use super::*;
    use serde_json::json;
    #[test] fn chinese_query_and_native_task_have_the_same_meaning(){
        let a=Query::parse(json!({"query":"停止按钮为什么不能立即中断生成"})).unwrap();
        let b=Query::parse(json!({"task":"停止按钮为什么不能立即中断生成"})).unwrap();
        assert_eq!(a.task,b.task);assert_eq!(a.terms,b.terms);assert!(a.anchors.is_empty());
        for t in ["停止","cancel","abort","生成"]{assert!(a.terms.iter().any(|(s,_)|s==t),"{t}");}
    }
    #[test] fn two_character_terms_and_camel_case_are_indexed(){
        for t in ["取消","登录"]{assert!(query::tokens(t).contains(&t.into()));}
        let v=query::tokens("HTTPServer cancelCurrentTurn foo_bar");for t in ["http","server","cancel","current","turn","foo","bar"]{assert!(v.contains(&t.into()),"{t}");}
    }
    #[test] fn guessed_symbols_do_not_block_a_natural_language_task(){
        let d=tempfile::tempdir().unwrap();fs::create_dir(d.path().join("src")).unwrap();fs::write(d.path().join("src/jobs.rs"),"// 停止正在运行的会话，取消任务。\npub fn cancel_job() { cancel_handle(); }\n").unwrap();
        let out=polaris(d.path(),json!({"task":"停止会话任务","keywords":["inventedStopGeneration"]})).unwrap();
        assert!(out.contains("pub fn cancel_job()"),"{out}");assert!(out.contains("inventedStopGeneration"));
    }
    #[test] fn evidence_tracks_edits_deletions_and_gitignore(){
        let d=tempfile::tempdir().unwrap();fs::create_dir(d.path().join("src")).unwrap();fs::write(d.path().join(".gitignore"),"secret.rs\n").unwrap();
        fs::write(d.path().join("secret.rs"),"// 停止任务\nfn secret() {}\n").unwrap();
        let p=d.path().join("src/main.rs");fs::write(&p,"// 取消任务\npub fn old_cancel() {}\n").unwrap();
        let a=polaris(d.path(),json!({"task":"取消任务"})).unwrap();assert!(a.contains("old_cancel"));assert!(!a.contains("secret.rs"));
        fs::write(&p,"// 取消任务，已更新\npub fn fresh_cancel() {}\n").unwrap();let b=polaris(d.path(),json!({"task":"取消任务"})).unwrap();assert!(b.contains("fresh_cancel"));assert!(!b.contains("old_cancel"));
        fs::remove_file(p).unwrap();let c=polaris(d.path(),json!({"task":"取消任务"})).unwrap();assert!(!c.contains("fresh_cancel"));
    }
    #[test] fn primary_body_survives_many_similar_symbols(){
        let d=tempfile::tempdir().unwrap();fs::create_dir(d.path().join("src")).unwrap();
        for i in 0..15{fs::write(d.path().join(format!("src/noise{i}.rs")),format!("// 取消任务\npub fn cancel_job_option_{i}() {{}}\n")).unwrap();}
        fs::write(d.path().join("src/z.rs"),"// 执行真正的取消任务\npub fn cancel_job() { stop(); }\n").unwrap();
        let out=polaris(d.path(),json!({"task":"取消任务","keywords":["cancel_job"]})).unwrap();assert!(out.contains("pub fn cancel_job() { stop(); }"),"{out}");assert!(out.len()<=32768);
    }
    #[test] fn same_metadata_edit_is_recalled_once_from_current_source(){
        let d=tempfile::tempdir().unwrap();fs::create_dir(d.path().join("src")).unwrap();let p=d.path().join("src/job.rs");
        fs::write(&p,"// 取消任务\npub fn old_cancel() { stop(); }\n").unwrap();
        let stamp=fs::metadata(&p).unwrap().modified().unwrap();
        let old=polaris(d.path(),json!({"task":"取消任务"})).unwrap();assert!(old.contains("old_cancel"));
        fs::write(&p,"// 取消任务\npub fn new_cancel() { stop(); }\n").unwrap();
        fs::File::options().write(true).open(&p).unwrap().set_times(fs::FileTimes::new().set_modified(stamp)).unwrap();
        let new=polaris(d.path(),json!({"task":"取消任务"})).unwrap();assert!(new.contains("new_cancel"),"{new}");assert!(!new.contains("old_cancel"));
    }
    #[test] fn pathological_source_lines_do_not_escape_the_output_budget(){
        let d=tempfile::tempdir().unwrap();fs::create_dir(d.path().join("src")).unwrap();
        fs::write(d.path().join("src/job.rs"),format!("// 取消任务\npub fn cancel() {{\n  let text = \"{}\";\n}}\n","x".repeat(100000))).unwrap();
        let out=polaris(d.path(),json!({"task":"取消任务","maxBytes":8192})).unwrap();assert!(out.len()<=8192);assert!(out.contains("PARTIAL"));
    }
    #[test] fn shared_events_and_import_aliases_are_evidence_not_name_guesses(){
        let d=tempfile::tempdir().unwrap();fs::create_dir(d.path().join("src")).unwrap();
        fs::write(d.path().join("src/a.ts"),"export function cancelJob() { emit('jobs:cancel', {}); }\n").unwrap();
        fs::write(d.path().join("src/b.ts"),"import {cancelJob as abortJob} from './a';\n// 用户要求停止任务\nexport function stopButton() { abortJob(); }\n").unwrap();
        fs::write(d.path().join("src/c.ts"),"export function observeJobs() { listen('jobs:cancel', onCancel); }\n").unwrap();
        let corpus=index::corpus(d.path(),Instant::now()+Duration::from_secs(5)).unwrap();let files=HashSet::from(["src/a.ts".into(),"src/b.ts".into(),"src/c.ts".into()]);
        let a=corpus.units.iter().find(|u|u.name=="cancelJob").unwrap();let b=corpus.units.iter().find(|u|u.name=="stopButton").unwrap();let c=corpus.units.iter().find(|u|u.name=="observeJobs").unwrap();
        assert_eq!(related(b,a,&files),Some("callee-reference"));assert_eq!(related(a,c,&files),Some("event-reference"));
    }
    #[test] fn call_sites_and_scalar_locals_are_not_definitions(){
        let d=tempfile::tempdir().unwrap();
        fs::write(d.path().join("actions.ts"),"export function runJob() {\n  const state = 1;\n  sendRequest({state});\n  const done = () => state;\n}\nclass Queue {\n  cancel() { stop(); }\n}\n").unwrap();
        let c=index::corpus(d.path(),Instant::now()+Duration::from_secs(5)).unwrap();let names=c.units.iter().map(|u|u.name.as_str()).collect::<Vec<_>>();
        assert!(names.contains(&"runJob"));assert!(names.contains(&"cancel"));assert!(names.contains(&"done"));
        assert!(!names.contains(&"sendRequest"));assert!(!names.contains(&"state"));
    }
    #[test] fn expired_index_never_claims_a_complete_miss(){
        let d=tempfile::tempdir().unwrap();let q=Query::parse(json!({"task":"取消任务"})).unwrap();
        let out=retrieve(d.path(),&q,Instant::now()-Duration::from_secs(5)).unwrap();assert!(out.starts_with("# CTX PARTIAL"));
    }
    #[test] fn explicit_configuration_and_case_sensitive_paths_are_preserved(){
        let d=tempfile::tempdir().unwrap();fs::write(d.path().join("settings.json"),"{\"timeout\": 42}\n").unwrap();
        let out=polaris(d.path(),json!({"task":"默认超时配置","files":["settings.json"]})).unwrap();assert!(out.contains("\"timeout\": 42"),"{out}");
        let q=Query::parse(json!({"files":["Foo.ts","foo.ts"]})).unwrap();assert_eq!(q.files.len(),2);
    }
    #[test] fn namespace_api_to_native_command_links_are_bounded_and_verified(){
        let d=tempfile::tempdir().unwrap();fs::create_dir(d.path().join("src")).unwrap();
        fs::write(d.path().join("src/api.ts"),"export const api = {\n  stopWork: (id: string) => invoke('cancel_work', {id}),\n};\n").unwrap();
        fs::write(d.path().join("src/ui.ts"),"import {api} from './api';\nexport function stopButton() { api.stopWork('job'); }\n").unwrap();
        fs::write(d.path().join("src/core.rs"),"#[tauri::command]\nfn cancel_work() { notify_cancel(); }\n").unwrap();
        let c=index::corpus(d.path(),Instant::now()+Duration::from_secs(5)).unwrap();let files=c.units.iter().map(|u|u.file.clone()).collect::<HashSet<_>>();
        let ui=c.units.iter().find(|u|u.name=="stopButton").unwrap();let api=c.units.iter().find(|u|u.name=="stopWork").unwrap();let native=c.units.iter().find(|u|u.name=="cancel_work").unwrap();
        assert_eq!(related(ui,api,&files),Some("callee-reference"));assert_eq!(related(api,native,&files),Some("command-reference"));
    }
    #[test] fn bad_paths_are_rejected(){for f in ["../secret.rs","/tmp/secret.rs","C:\\secret.rs"]{assert!(Query::parse(json!({"files":[f]})).is_err());}}
}
