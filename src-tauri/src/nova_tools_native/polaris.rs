// Semantic facade over the preserved exact-symbol engine. All native callers use this boundary.
use super::*;
mod query {include!("polaris_query.rs");}
mod index {include!("polaris_index.rs");}
mod vectors {include!("polaris_vectors.rs");}
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
    if !a.terms.contains_key(&b.name.to_lowercase())&&!b.terms.contains_key(&a.name.to_lowercase()){return None;}
    let word=Regex::new(&format!(r"\b{}\s*\(",regex::escape(&b.name))).ok()?;
    if word.is_match(&a.passage) && (a.file==b.file||a.imports.iter().any(|i|i.name==b.name&&resolve_specifier(&i.from,&a.file,files).as_deref()==Some(&b.file))){return Some("callee-reference");}
    let backwards=Regex::new(&format!(r"\b{}\s*\(",regex::escape(&a.name))).ok()?;
    if backwards.is_match(&b.passage)&&(a.file==b.file||b.imports.iter().any(|i|i.name==a.name&&resolve_specifier(&i.from,&b.file,files).as_deref()==Some(&a.file))){return Some("caller-reference");}None
}
fn retrieve(root:&Path,q:&Query,started:Instant)->Result<String,String>{
    let root=root.canonicalize().map_err(|e|e.to_string())?;
    let deadline=started+Duration::from_millis(3500);
    let corpus=index::corpus(&root,started+Duration::from_millis(2200))?;
    let units=corpus.units.iter().filter(|u|u.role=="implementation"||(q.test_intent&&u.role=="test")||(q.doc_intent&&u.role=="documentation")||q.files.contains(&u.file)).cloned().collect::<Vec<_>>();
    let lexical=lexical_rank(&corpus,&units,q);
    let dense=vectors::search(&root,&units,&q.task,deadline);
    let mut scores=HashMap::<usize,f64>::new();
    // Reciprocal-rank fusion: raw BM25 and cosine scores are not commensurable.
    for (rank,(id,_)) in lexical.iter().enumerate(){*scores.entry(*id).or_default()+=1.0/(40.0+rank as f64);}
    for (rank,(id,_)) in dense.scores.iter().enumerate(){*scores.entry(*id).or_default()+=1.0/(40.0+rank as f64);}
    for (i,u) in units.iter().enumerate(){if q.files.contains(&u.file)||q.anchors.iter().any(|a|a.eq_ignore_ascii_case(&u.name)){*scores.entry(i).or_default()+=1.0;}}
    let mut ranked=scores.into_iter().collect::<Vec<_>>();ranked.sort_by(|a,b|b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    // De-duplicate snippets of the same function before the expensive second stage.
    let mut seen=HashSet::new();ranked.retain(|(i,_)|seen.insert(identity(&units[*i])));ranked.truncate(24);
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
    let files=corpus.units.iter().map(|u|u.file.clone()).collect::<HashSet<_>>();
    let parents=chosen.iter().take(2).map(|(i,_)|*i).collect::<Vec<_>>();
    for parent in parents {
        let mut neighbours=Vec::new();
        for (i,u) in units.iter().enumerate(){if Instant::now()>deadline{break;}if names.contains(&identity(u)){continue;}
            if let Some(role)=related(&units[parent],u,&files){let value=lexical.iter().find(|(j,_)|*j==i).map(|(_,v)|*v).unwrap_or(0.0);neighbours.push((i,role,value));}}
        neighbours.sort_by(|a,b|b.2.total_cmp(&a.2).then(a.0.cmp(&b.0)));
        for (i,role,_) in neighbours.into_iter().take(2){if chosen.len()>=7{break;}if names.insert(identity(&units[i]))&&index::verified(&root,&units[i]){chosen.push((i,role));}}
    }
    let mut body=String::new();let mut gaps=Vec::new();let mut evidence=Vec::new();let mut lines_left=q.lines;
    for (id,relation) in chosen{
        let u=&units[id];let source=&u.source;
        let full_start=if u.start<=u.owner_start{u.start}else{u.owner_start};
        let full=source[full_start-1..u.owner_end].join("\n");
        let full_fit=full.len()+body.len()+2048<q.hard&&u.owner_end-full_start+1<=lines_left;
        let (start,end)=if full_fit{(full_start,u.owner_end)}else{(u.start,u.end)};
        let snippet=source[start-1..end].iter().enumerate().map(|(n,s)|format!("{}: {}\n",start+n,s)).collect::<String>();
        let section=format!("\n### {}:{}-{} [{} {}]\ncoverage: {} {}:{}-{}\n```\n{snippet}```\n",u.file,start,end,relation,u.name,if full_fit{"BODY"}else{"PARTIAL"},u.file,start,end);
        if body.len()+section.len()+2048>q.hard||end-start+1>lines_left{gaps.push(serde_json::json!({"file":u.file,"start":u.owner_start,"end":u.owner_end,"reason":"budget"}));continue;}
        lines_left-=end-start+1;body.push_str(&section);
        evidence.push(serde_json::json!({"file":u.file,"symbol":u.name,"start":start,"end":end,"role":u.role,"relation":relation,"sourceHash":u.file_hash,"complete":full_fit}));
        if !full_fit{gaps.push(serde_json::json!({"file":u.file,"start":u.owner_start,"end":u.owner_end,"reason":"large-unit"}));}
    }
    let status=if evidence.is_empty(){"MISS"}else if corpus.partial||!gaps.is_empty()||dense.mode.contains("partial"){"PARTIAL"}else{"CANDIDATES"};
    let meta=serde_json::json!({"backend":dense.mode,"reranked":reranked,"semanticReady":dense.ready,"semanticTotal":dense.total,"note":dense.note,"rerankNote":rerank_note,
        "files":corpus.files,"units":units.len(),"changedFiles":corpus.changed,"indexPartial":corpus.partial,"unresolvedAnchors":missing,
        "queryMs":started.elapsed().as_secs_f64()*1000.0,"evidence":evidence,"next_reads":gaps});
    let header=format!("# CTX {status}\n# retrieval: {meta}\n# Evidence is verified current source; relevance/one-hop references are candidates, not a proof of root cause.\n");
    let mut out=format!("{header}{body}");
    // Metadata is bounded too. Do not split a code body or lie about its coverage.
    if out.len()>q.hard {out=format!("# CTX PARTIAL\n检索结果超出预算，请限定 files；未返回的代码不算已读。\n");}
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
    #[test] fn bad_paths_are_rejected(){for f in ["../secret.rs","/tmp/secret.rs","C:\\secret.rs"]{assert!(Query::parse(json!({"files":[f]})).is_err());}}
}
