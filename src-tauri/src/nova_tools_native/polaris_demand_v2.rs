// Query-first discovery. Detailed passages are built only for selected declarations.
const INITIAL_FILES: usize = 16;
const MAX_PARSED_FILES: usize = 40;
const MAX_DISCOVERY_BYTES: u64 = 64 * 1024 * 1024;
const MAX_PARSE_BYTES: usize = 8 * 1024 * 1024;
#[derive(Clone)]
struct LightFile {
    hash:String, entry:FileEntry, source:Arc<Vec<String>>,
    names:HashMap<usize,HashSet<String>>, built:BTreeMap<usize,Vec<Arc<CodeUnit>>>,
}
#[derive(Default)]
struct DemandCache { entries:BTreeMap<String,LightFile>, bytes:usize, results:BTreeMap<String,(String,Corpus,DemandStats)> }
static DEMAND:OnceLock<Mutex<BTreeMap<String,Arc<Mutex<DemandCache>>>>>=OnceLock::new();
struct Candidate { file:String,text:String,hash:String,hits:Vec<usize>,score:f64 }
#[derive(Clone,Default,Serialize)]
#[serde(rename_all="camelCase")]
pub(super) struct DemandStats {
    pub query_cache_hit:bool,
    pub scanned_files:usize,pub scanned_bytes:u64,pub parsed_files:usize,
    pub parsed_bytes:usize,pub reparsed_files:usize,pub candidate_files_omitted:usize,
    pub discovery_ms:f64,pub parse_ms:f64,pub dependency_rounds:usize,
    pub candidate_declarations:usize,pub materialized_declarations:usize,
}
fn subset(units:Vec<Arc<CodeUnit>>,files:usize,changed:usize,partial:bool)->Corpus {
    let mut df=HashMap::new();
    for u in &units {for term in u.terms.keys(){*df.entry(term.clone()).or_default()+=1;}}
    let average=(units.iter().map(|u|u.length).sum::<f64>()/units.len().max(1) as f64).max(1.0);
    Corpus{units:Arc::new(units),df:Arc::new(df),average,partial,files,changed}
}
fn demand_slot(root:&Path)->Result<Arc<Mutex<DemandCache>>,String>{
    let key=normalize_root(root);let mut roots=DEMAND.get_or_init(Default::default).lock().map_err(|_|"candidate cache lock poisoned")?;
    if !roots.contains_key(&key)&&roots.len()>=2 {if let Some(old)=roots.keys().next().cloned(){roots.remove(&old);}}
    Ok(roots.entry(key).or_default().clone())
}
fn build_light(row:&Candidate)->LightFile {
    let started=Instant::now();let entry=scan_source(&row.text,&row.file);let parsed_ms=started.elapsed().as_secs_f64()*1000.0;let source=Arc::new(row.text.lines().map(str::to_owned).collect::<Vec<_>>());
    let names=entry.syms.iter().filter(|s|is_retrieval_unit(s,&source)).map(|s|{
        let mut terms=query::tokens(&s.name).into_iter().collect::<HashSet<_>>();terms.extend(identifier_aliases(&s.name));(s.ln,terms)
    }).collect();
    if std::env::var_os("NOVA_POLARIS_TRACE_PARSE").is_some(){eprintln!("[polaris-parse] {}",serde_json::json!({"file":row.file,"bytes":row.text.len(),"parseMs":parsed_ms,"totalMs":started.elapsed().as_secs_f64()*1000.0,"declarations":entry.syms.len()}));}
    LightFile{hash:row.hash.clone(),entry,source,names,built:BTreeMap::new()}
}
fn declarations(rows:&[Candidate],wanted:&[usize],cache:&mut DemandCache,parsed:&mut HashSet<usize>,stats:&mut DemandStats,deadline:Instant)->bool {
    let started=Instant::now();let mut partial=false;let mut work=Vec::new();
    for &id in wanted {
        if parsed.contains(&id){continue;}
        if Instant::now()>=deadline||parsed.len()>=MAX_PARSED_FILES{partial=true;break;}
        let row=&rows[id];if stats.parsed_bytes+row.text.len()>MAX_PARSE_BYTES{partial=true;continue;}
        if cache.entries.get(&row.file).is_none_or(|e|e.hash!=row.hash){cache.entries.remove(&row.file);work.push(id);}
        parsed.insert(id);stats.parsed_files+=1;stats.parsed_bytes+=row.text.len();
    }
    for batch in work.chunks(4){
        if Instant::now()>=deadline{for &id in batch{parsed.remove(&id);}partial=true;continue;}
        let built=thread::scope(|scope|{
            let jobs=batch.iter().map(|&id|scope.spawn(move ||(id,build_light(&rows[id])))).collect::<Vec<_>>();
            jobs.into_iter().filter_map(|job|job.join().ok()).collect::<Vec<_>>()
        });
        if built.len()!=batch.len(){partial=true;}
        for (id,file) in built{cache.entries.insert(rows[id].file.clone(),file);stats.reparsed_files+=1;}
    }
    parsed.retain(|id|cache.entries.get(&rows[*id].file).is_some_and(|e|e.hash==rows[*id].hash));
    stats.parse_ms+=started.elapsed().as_secs_f64()*1000.0;partial
}
fn role_allowed(file:&str,s:&Symbol,q:&query::Query)->bool{
    let test=s.kind.starts_with("test:")||file_role(file)=="test";
    (!test||q.test_intent||q.anchors.iter().any(|a|a==&s.name)||q.files.iter().any(|f|f==file&&file_role(f)=="test"))&&
        (file_role(file)!="documentation"||q.doc_intent||q.files.iter().any(|f|f==file))
}
fn select_spans(rows:&[Candidate],ids:&[usize],cache:&DemandCache,q:&query::Query,
    matcher:&regex::RegexSet,terms:&[(String,f64)],idf:&[f64],names:&HashSet<String>,caller_names:&HashSet<(String,String)>,cap:usize)->Vec<(usize,usize)>{
    let mut ranked=Vec::new();
    for &id in ids {
        let row=&rows[id];let Some(file)=cache.entries.get(&row.file)else{continue;};
        for s in &file.entry.syms {
            if !file.names.contains_key(&s.ln)||!role_allowed(&row.file,s,q){continue;}
            let first=s.ln.saturating_sub(1);let end=s.end.min(file.source.len());if first>=end{continue;}
            let body=file.source[first..end.min(first+320)].join("\n");
            let header=file.source[first.saturating_sub(4)..(first+4).min(end)].join("\n");
            let matched=matcher.matches(&body);let head=matcher.matches(&header);
            let mut score=0.0;
            for (i,(term,weight)) in terms.iter().enumerate(){
                if file.names[&s.ln].contains(term){score+=weight*idf[i]*6.0;}
                if head.matched(i){score+=weight*idf[i]*2.0;}
                if matched.matched(i){score+=weight*idf[i];}
            }
            score*=q.focus.score(std::iter::once(row.file.as_str()).chain(file.source.iter().take(5).map(String::as_str)).chain(std::iter::once(body.as_str())));
            if names.contains(&s.name){score+=10000.0;}
            if q.anchors.iter().any(|a|a.eq_ignore_ascii_case(&s.name)){score+=20000.0;}
            if caller_names.iter().any(|(file,name)|row.file==*file&&body.contains(name)){score+=3000.0*q.focus.score(std::iter::once(row.file.as_str()).chain(file.source.iter().take(5).map(String::as_str)).chain(std::iter::once(body.as_str())));}
            if score>0.0 {ranked.push((id,s.ln,score/(1.0+0.04*((end-first) as f64/80.0).ln_1p())));}
        }
        if file.names.is_empty()&&!file.source.is_empty(){ranked.push((id,1,row.score));}
    }
    ranked.sort_by(|a,b|b.2.total_cmp(&a.2).then(rows[a.0].file.cmp(&rows[b.0].file)).then(a.1.cmp(&b.1)));
    let mut selected=Vec::new();let mut seen=HashSet::new();
    // Keep source-connected execution callers before file diversity consumes
    // the bounded declaration budget. No additional global index is built.
    let facets=q.facets();
    let mut seeds=caller_names.iter().collect::<Vec<_>>();seeds.sort();
    let mut wanted_callers=Vec::<(usize,usize,usize,f64)>::new();
    for (seed_file,seed_name) in seeds.into_iter().take(64) {
        let Some(&id)=ids.iter().find(|&&id|rows[id].file==*seed_file)else{continue;};
        let Some(file)=cache.entries.get(seed_file)else{continue;};
        let Some(seed)=file.entry.syms.iter().find(|s|s.name==*seed_name)else{continue;};
        let words=query::tokens(&file.source[seed.ln-1..seed.end.min(file.source.len())].join("\n")).into_iter().collect::<HashSet<_>>();
        let missing=facets.iter().filter(|g|!g.split('|').any(|w|words.contains(w))).collect::<Vec<_>>();
        if missing.is_empty(){continue;}
        let mut best=None;
        for symbol in &file.entry.syms {
            if symbol.name==*seed_name||!file.names.contains_key(&symbol.ln)||!role_allowed(seed_file,symbol,q){continue;}
            let body=file.source[symbol.ln-1..symbol.end.min(file.source.len())].join("\n");
            if !body.contains(seed_name.as_str())||!references(&body).0.contains(seed_name.as_str()){continue;}
            let body_words=query::tokens(&body).into_iter().collect::<HashSet<_>>();
            let gain=missing.iter().filter(|g|g.split('|').any(|w|body_words.contains(w))).count();
            if gain==0{continue;}
            let relevance=ranked.iter().find(|r|r.0==id&&r.1==symbol.ln).map(|r|r.2).unwrap_or(0.0);
            let candidate=(id,symbol.ln,gain,relevance);
            if best.as_ref().is_none_or(|old:&(usize,usize,usize,f64)|gain>old.2||(gain==old.2&&relevance>old.3)){best=Some(candidate);}
        }
        if let Some(candidate)=best{wanted_callers.push(candidate);}
    }
    wanted_callers.sort_by(|a,b|b.2.cmp(&a.2).then(b.3.total_cmp(&a.3)).then(rows[a.0].file.cmp(&rows[b.0].file)).then(a.1.cmp(&b.1)));
    for (id,line,_,_) in wanted_callers {
        if !selected.contains(&(id,line)){selected.push((id,line));seen.insert(id);}
        if selected.len()>=8.min(cap/4){break;}
    }
    for &(id,line,_) in &ranked {if seen.insert(id){selected.push((id,line));if selected.len()>=cap{break;}}}
    for &(id,line,_) in &ranked {if selected.len()>=cap{break;}if !selected.contains(&(id,line)){selected.push((id,line));}}
    selected
}
fn materialize(rows:&[Candidate],wanted:&[(usize,usize)],cache:&mut DemandCache,
    selected:&mut HashSet<(usize,usize)>,units:&mut Vec<Arc<CodeUnit>>,stats:&mut DemandStats,deadline:Instant)->bool {
    let started=Instant::now();let mut grouped=BTreeMap::<usize,HashSet<usize>>::new();let mut partial=false;
    for &(id,line) in wanted {if !selected.contains(&(id,line)){grouped.entry(id).or_default().insert(line);}}
    for (id,lines) in grouped {
        if Instant::now()>=deadline{partial=true;break;}
        let row=&rows[id];let file=cache.entries.get_mut(&row.file).unwrap();
        let missing=lines.iter().filter(|line|!file.built.contains_key(line)).copied().collect::<HashSet<_>>();
        if !missing.is_empty(){
            for u in make_units_selected(&row.file,&row.text,&file.entry,file.source.clone(),Some(&missing)){
                file.built.entry(u.owner_start).or_default().push(u);
            }
        }
        let mut sorted=lines.into_iter().collect::<Vec<_>>();sorted.sort_unstable();
        for line in sorted {if selected.insert((id,line)){if let Some(built)=file.built.get(&line){units.extend(built.iter().cloned());stats.materialized_declarations+=1;}}}
    }
    stats.parse_ms+=started.elapsed().as_secs_f64()*1000.0;partial
}
pub(super) fn demand_corpus(root:&Path,q:&query::Query,deadline:Instant)->Result<(Corpus,DemandStats),String>{
    let started=Instant::now();let root=root.canonicalize().map_err(|e|e.to_string())?;
    let mut walker=ignore::WalkBuilder::new(&root);walker.hidden(false).require_git(false).follow_links(false);
    walker.filter_entry(|e|!e.file_type().is_some_and(|f|f.is_dir())||!matches!(e.file_name().to_str(),Some(".git"|"node_modules"|"target"|"dist"|"vendor"|".venv"|"coverage"|".codegraph")));
    let terms=q.terms.iter().filter(|(t,_)|!t.is_ascii()||t.len()>=3).cloned().collect::<Vec<_>>();
    let matcher=regex::RegexSetBuilder::new(terms.iter().map(|(t,_)|regex::escape(t))).case_insensitive(true).size_limit(4*1024*1024).build().map_err(|e|e.to_string())?;
    let mut paths=Vec::new();let mut partial=false;let mut stats=DemandStats::default();
    for item in walker.build(){
        if Instant::now()>=deadline{partial=true;break;}
        let item=match item{Ok(e)=>e,Err(_)=>{partial=true;continue;}};
        if !item.file_type().is_some_and(|f|f.is_file()){continue;}
        let Some(file)=item.path().strip_prefix(&root).ok().and_then(|p|p.to_str()).map(|p|p.replace('\\',"/"))else{continue;};
        let role=file_role(&file);
        if is_searchable_implementation_file(&file)&&(role=="implementation"||(q.test_intent&&role=="test")||(q.doc_intent&&role=="documentation")){
            paths.push(file);if paths.len()>=8000{partial=true;break;}
        }
    }
    paths.extend(q.files.iter().cloned());paths.sort();paths.dedup();let mut rows=Vec::new();let mut df=vec![0usize;terms.len()];
    for file in paths{
        if Instant::now()>=deadline{partial=true;break;}
        if !safe_file(&root,&file){partial=true;continue;}
        let path=root.join(&file);let Some(stamp)=metadata_stamp(&path)else{partial=true;continue;};
        if stamp.0>2*1024*1024||stats.scanned_bytes+stamp.0>MAX_DISCOVERY_BYTES{partial=true;continue;}
        let text=match fs::read_to_string(&path){Ok(t)=>t,Err(_)=>{partial=true;continue;}};
        if metadata_stamp(&path)!=Some(stamp){partial=true;continue;}
        stats.scanned_files+=1;stats.scanned_bytes+=text.len() as u64;
        let mut hits=matcher.matches(&text).into_iter().collect::<Vec<_>>();hits.extend(matcher.matches(&file));hits.sort_unstable();hits.dedup();for &hit in &hits{df[hit]+=1;}
        rows.push(Candidate{hash:digest(text.as_bytes()),file,text,hits,score:0.0});
    }
    let n=rows.len().max(1) as f64;let idf=df.iter().map(|d|(1.0+n/(*d).max(1) as f64).ln()).collect::<Vec<_>>();
    for row in &mut rows{
        let path_hits=matcher.matches(&row.file);
        for &i in &row.hits{row.score+=terms[i].1*idf[i]*if path_hits.matched(i){3.0}else{1.0};}
        row.score*=q.focus.score([row.file.as_str(),row.text.as_str()]);
        row.score/=1.0+0.8*(1.0+row.text.len() as f64/8000.0).ln();
        if q.files.contains(&row.file){row.score+=10000.0;}
        if q.anchors.iter().any(|a|row.text.contains(a)){row.score+=1000.0;}
    }
    let mut order=(0..rows.len()).filter(|&i|rows[i].score>0.0).collect::<Vec<_>>();
    order.sort_by(|&a,&b|rows[b].score.total_cmp(&rows[a].score).then(rows[a].file.cmp(&rows[b].file)));
    stats.discovery_ms=started.elapsed().as_secs_f64()*1000.0;
    let slot=demand_slot(&root)?;let mut cache=slot.try_lock().map_err(|_|"same repository candidate parsing is busy")?;
    let valid=rows.iter().map(|r|r.file.as_str()).collect::<HashSet<_>>();cache.entries.retain(|file,_|valid.contains(file.as_str()));
    if cache.bytes>MAX_PARSE_BYTES*2{cache.entries.clear();cache.results.clear();cache.bytes=0;}
    let query_key=q.params.to_string();
    let fingerprint=digest(rows.iter().map(|r|format!("{}\0{}\n",r.file,r.hash)).collect::<String>().as_bytes());
    cache.results.retain(|_,(old,_,_)|old==&fingerprint);
    if !partial {if let Some((old,c,previous))=cache.results.get(&query_key){if old==&fingerprint {
        let mut d=previous.clone();d.query_cache_hit=true;d.discovery_ms=stats.discovery_ms;d.parse_ms=0.0;d.reparsed_files=0;d.parsed_files=0;d.parsed_bytes=0;
        let mut c=c.clone();c.changed=0;return Ok((c,d));
    }}}
    let mut parsed=HashSet::new();let initial=order.iter().copied().take(INITIAL_FILES).collect::<Vec<_>>();
    partial|=declarations(&rows,&initial,&mut cache,&mut parsed,&mut stats,deadline);
    let mut ids=parsed.iter().copied().collect::<Vec<_>>();ids.sort_unstable();
    let wanted=select_spans(&rows,&ids,&cache,q,&matcher,&terms,&idf,&HashSet::new(),&HashSet::new(),64);
    let mut selected=HashSet::new();let mut units=Vec::new();
    partial|=materialize(&rows,&wanted,&mut cache,&mut selected,&mut units,&mut stats,deadline);
    let files=rows.iter().map(|r|r.file.clone()).collect::<HashSet<_>>();
    let mut required_names=HashSet::<String>::new();
    for expansion in 0..3{
        if units.is_empty()||Instant::now()>=deadline{break;}
        let c=subset(units.clone(),parsed.len(),stats.reparsed_files,partial);
        let lexical=super::lexical_rank(&c,&units,q);
        let seeds=super::rank::fuse(&lexical,&[],&units,q);
        let mut direct=HashSet::new();let mut names=HashSet::new();let callers=seeds.iter().take(2).map(|(i,_)|(units[*i].file.clone(),units[*i].name.clone())).collect::<HashSet<_>>();
        let mut expand=seeds.iter().take(6).map(|(id,_)|*id).collect::<Vec<_>>();
        let mut identities=expand.iter().map(|i|super::identity(&units[*i])).collect::<HashSet<_>>();
        for (i,u) in units.iter().enumerate(){if required_names.contains(&u.name)&&identities.insert(super::identity(u)){expand.push(i);if expand.len()>=64{break;}}}
        for id in expand{
            let u=&units[id];direct.insert(u.file.clone());
            names.extend(u.calls.iter().filter(|s|s.len()>=3).cloned());
            names.extend(super::packet::dependency_names(u));names.extend(u.commands.iter().cloned());
            for import in u.imports.iter(){if u.calls.contains(&import.name)||u.members.iter().any(|(o,_)|o==&import.name){if let Some(f)=resolve_specifier(&import.from,&u.file,&files){direct.insert(f);names.insert(import.orig.clone().unwrap_or_else(||import.name.clone()));}}}
            for (object,member) in &u.members{if let Some(suffix)=object.strip_prefix("crate::"){names.insert(member.clone());if let Some(pos)=u.file.rfind("src/"){let prefix=&u.file[..pos+4];let module=suffix.replace("::","/");for f in [format!("{prefix}{module}.rs"),format!("{prefix}{module}/mod.rs")]{if files.contains(&f){direct.insert(f);}}}}}
        }
        required_names.extend(names.iter().cloned());
        let mut neighbours=rows.iter().enumerate().filter(|(i,_)|!parsed.contains(i)).filter_map(|(i,r)|{
            let explicit=direct.contains(&r.file);let caller=callers.iter().any(|(_,name)|r.text.contains(name));
            (explicit||caller).then_some((i,if explicit{10000.0+r.score}else{r.score}))
        }).collect::<Vec<_>>();
        neighbours.sort_by(|a,b|b.1.total_cmp(&a.1).then(rows[a.0].file.cmp(&rows[b.0].file)));
        let extra=neighbours.iter().take(if expansion<2{8}else{4}).map(|x|x.0).collect::<Vec<_>>();
        partial|=declarations(&rows,&extra,&mut cache,&mut parsed,&mut stats,deadline);
        let mut ids=parsed.iter().copied().collect::<Vec<_>>();ids.sort_unstable();
        let linked=select_spans(&rows,&ids,&cache,q,&matcher,&terms,&idf,&names,&callers,48);
        let before=selected.len();partial|=materialize(&rows,&linked,&mut cache,&mut selected,&mut units,&mut stats,deadline);
        stats.dependency_rounds+=1;if selected.len()==before&&extra.is_empty(){break;}
    }
    stats.candidate_files_omitted=order.iter().filter(|i|!parsed.contains(i)).count();
    stats.candidate_declarations=parsed.iter().filter_map(|i|cache.entries.get(&rows[*i].file)).map(|f|f.names.len()).sum();
    cache.bytes=cache.entries.values().map(|e|e.source.iter().map(|s|s.len()+1).sum::<usize>()).sum();
    let c=subset(units,parsed.len(),stats.reparsed_files,partial);
    if !partial {if cache.results.len()>=64 {cache.results.clear();}cache.results.insert(query_key,(fingerprint,c.clone(),stats.clone()));}
    Ok((c,stats))
}

#[cfg(test)]
mod demand_tests {
    use super::*;
    #[test] fn cold_query_parses_candidates_not_the_whole_repository() {
        let d=tempfile::tempdir().unwrap();
        for i in 0..100 {fs::write(d.path().join(format!("noise{i}.rs")),format!("fn unrelated_{i}() {{ unrelated(); }}\n")).unwrap();}
        fs::write(d.path().join("real.rs"),"// 取消任务\nfn cancel_job() { stop(); }\n").unwrap();
        let q=query::Query::parse(serde_json::json!({"task":"取消任务"})).unwrap();
        let (c,s)=demand_corpus(d.path(),&q,Instant::now()+Duration::from_secs(5)).unwrap();
        assert_eq!(s.scanned_files,101);assert!(s.parsed_files<10);assert!(c.units.iter().any(|u|u.name=="cancel_job"));
    }
    #[test] fn same_metadata_edit_changes_recall_before_source_verification() {
        let d=tempfile::tempdir().unwrap();let p=d.path().join("job.rs");
        fs::write(&p,"fn old_cancel() {}\n").unwrap();let stamp=fs::metadata(&p).unwrap().modified().unwrap();
        let q=query::Query::parse(serde_json::json!({"task":"cancel"})).unwrap();
        let (_,first)=demand_corpus(d.path(),&q,Instant::now()+Duration::from_secs(5)).unwrap();assert_eq!(first.reparsed_files,1);
        fs::write(&p,"fn new_cancel() {}\n").unwrap();fs::File::options().write(true).open(&p).unwrap().set_times(fs::FileTimes::new().set_modified(stamp)).unwrap();
        let (c,s)=demand_corpus(d.path(),&q,Instant::now()+Duration::from_secs(5)).unwrap();
        assert_eq!(s.reparsed_files,1);assert!(c.units.iter().any(|u|u.name=="new_cancel"));assert!(!c.units.iter().any(|u|u.name=="old_cancel"));
    }
    #[test] fn ignored_and_deleted_files_do_not_survive_candidate_cache() {
        let d=tempfile::tempdir().unwrap();fs::write(d.path().join(".gitignore"),"hidden.rs\n").unwrap();
        fs::write(d.path().join("hidden.rs"),"fn cancel_hidden() {}\n").unwrap();fs::write(d.path().join("visible.rs"),"fn cancel_visible() {}\n").unwrap();
        let q=query::Query::parse(serde_json::json!({"task":"cancel"})).unwrap();
        let (c,_)=demand_corpus(d.path(),&q,Instant::now()+Duration::from_secs(5)).unwrap();assert!(c.units.iter().all(|u|u.file=="visible.rs"));
        fs::remove_file(d.path().join("visible.rs")).unwrap();let(c,_)=demand_corpus(d.path(),&q,Instant::now()+Duration::from_secs(5)).unwrap();assert!(c.units.is_empty());
    }
    #[test] fn expired_discovery_is_explicitly_partial() {
        let d=tempfile::tempdir().unwrap();fs::write(d.path().join("job.rs"),"fn cancel_job() {}\n").unwrap();
        let q=query::Query::parse(serde_json::json!({"task":"cancel"})).unwrap();
        let (c,_)=demand_corpus(d.path(),&q,Instant::now()-Duration::from_secs(1)).unwrap();assert!(c.partial);
    }
}

#[cfg(test)] mod caller_discovery_tests {
    use super::*;
    #[test] fn bounded_discovery_keeps_execution_caller_among_many_key_named_helpers() {
        let d=tempfile::tempdir().unwrap();
        let mut text=String::from("// 桌面键盘\nfn decode_chord() -> bool { true }\nfn perform_input() -> bool { let parsed=decode_chord(); release_pressed(); parsed }\nfn release_pressed() {}\n");
        for n in 0..120 {text.push_str(&format!("fn keyboard_key_parse_{n}() -> bool {{ decode_chord() }}\n"));}
        fs::write(d.path().join("device.rs"),text).unwrap();
        let q=query::Query::parse(serde_json::json!({"task":"键盘组合按键解析后释放","keywords":["decode_chord"],"maxBytes":12000})).unwrap();
        let (c,_)=demand_corpus(d.path(),&q,Instant::now()+Duration::from_secs(3)).unwrap();
        assert!(c.units.iter().any(|u|u.name=="perform_input"));
        assert!(c.units.iter().any(|u|u.name=="decode_chord"));
    }
}
