// Query-first retrieval: no repository-wide AST/unit/vector build on the online path.
// Included inside index so the existing parser, identity and source verification remain shared.
const INITIAL_FILES: usize = 16;
const MAX_PARSED_FILES: usize = 32;
const MAX_DISCOVERY_BYTES: u64 = 64 * 1024 * 1024;
const MAX_PARSE_BYTES: usize = 8 * 1024 * 1024;
#[derive(Default)]
struct DemandCache { entries: BTreeMap<String, (String, Vec<Arc<CodeUnit>>)>, bytes: usize }
static DEMAND: OnceLock<Mutex<BTreeMap<String, Arc<Mutex<DemandCache>>>>> = OnceLock::new();
struct Candidate { file: String, text: String, hash: String, hits: Vec<usize>, score: f64 }
#[derive(Default, Serialize)]
#[serde(rename_all="camelCase")]
pub(super) struct DemandStats {
    pub scanned_files: usize, pub scanned_bytes: u64, pub parsed_files: usize,
    pub parsed_bytes: usize, pub reparsed_files: usize, pub candidate_files_omitted: usize,
    pub discovery_ms: f64, pub parse_ms: f64, pub dependency_rounds: usize,
}
fn subset(units: Vec<Arc<CodeUnit>>, files: usize, changed: usize, partial: bool) -> Corpus {
    let mut df = HashMap::new();
    for u in &units { for term in u.terms.keys() { *df.entry(term.clone()).or_default() += 1; } }
    let average = (units.iter().map(|u|u.length).sum::<f64>() / units.len().max(1) as f64).max(1.0);
    Corpus { units: Arc::new(units), df: Arc::new(df), average, partial, files, changed }
}
fn demand_slot(root: &Path) -> Result<Arc<Mutex<DemandCache>>,String> {
    let key=normalize_root(root);
    let mut roots=DEMAND.get_or_init(Default::default).lock().map_err(|_|"candidate cache lock poisoned")?;
    if !roots.contains_key(&key) && roots.len()>=2 { if let Some(old)=roots.keys().next().cloned(){roots.remove(&old);} }
    Ok(roots.entry(key).or_default().clone())
}
fn parse_candidates(rows: &[Candidate], wanted: &[usize], cache: &mut DemandCache,
    parsed: &mut HashSet<usize>, units: &mut Vec<Arc<CodeUnit>>, stats: &mut DemandStats,
    q: &query::Query, deadline: Instant) -> bool {
    let started=Instant::now(); let mut partial=false;
    for &id in wanted {
        if parsed.contains(&id){continue;}
        if Instant::now()>=deadline || parsed.len()>=MAX_PARSED_FILES {partial=true;break;}
        let row=&rows[id];
        if stats.parsed_bytes+row.text.len()>MAX_PARSE_BYTES {partial=true;continue;}
        if cache.entries.get(&row.file).is_none_or(|(hash,_)|hash!=&row.hash) {
            let built=make_units(&row.file,&row.text);
            cache.entries.insert(row.file.clone(),(row.hash.clone(),built));
            stats.reparsed_files+=1;
        }
        if let Some((_,built))=cache.entries.get(&row.file) {
            units.extend(built.iter().filter(|u|u.role=="implementation" ||
                (q.test_intent&&u.role=="test") || (q.doc_intent&&u.role=="documentation") ||
                q.files.contains(&u.file)).cloned());
        }
        parsed.insert(id);stats.parsed_files+=1;stats.parsed_bytes+=row.text.len();
    }
    stats.parse_ms+=started.elapsed().as_secs_f64()*1000.0;
    partial
}
/// A fresh bounded text scan finds new files and same-mtime edits without a startup index.
/// Only the highest-scoring files and source-linked neighbours receive expensive parsing.
/// File recall is deliberately bounded; its omissions are reported separately from body coverage.
pub(super) fn demand_corpus(root:&Path,q:&query::Query,deadline:Instant)->Result<(Corpus,DemandStats),String>{
    let started=Instant::now();let root=root.canonicalize().map_err(|e|e.to_string())?;
    let mut walker=ignore::WalkBuilder::new(&root);
    walker.hidden(false).require_git(false).follow_links(false);
    walker.filter_entry(|e| !e.file_type().is_some_and(|f|f.is_dir()) || !matches!(e.file_name().to_str(),Some(".git"|"node_modules"|"target"|"dist"|"vendor"|".venv"|"coverage"|".codegraph")));
    let terms=q.terms.iter().filter(|(term,_)|!term.is_ascii()||term.len()>=3).cloned().collect::<Vec<_>>();
    let matcher=regex::RegexSetBuilder::new(terms.iter().map(|(term,_)|regex::escape(term)))
        .case_insensitive(true).size_limit(4*1024*1024).build().map_err(|e|e.to_string())?;
    let mut paths=Vec::new();let mut partial=false;let mut stats=DemandStats::default();
    for item in walker.build() {
        if Instant::now()>=deadline {partial=true;break;}
        let item=match item{Ok(item)=>item,Err(_)=>{partial=true;continue;}};
        if !item.file_type().is_some_and(|t|t.is_file()){continue;}
        let Some(file)=item.path().strip_prefix(&root).ok().and_then(|p|p.to_str()).map(|p|p.replace('\\',"/"))else{continue;};
        let role=file_role(&file);
        if is_searchable_implementation_file(&file) && (role=="implementation"||(q.test_intent&&role=="test")||(q.doc_intent&&role=="documentation")) {
            paths.push(file);if paths.len()>=8000 {partial=true;break;}
        }
    }
    paths.extend(q.files.iter().cloned());paths.sort();paths.dedup();
    let mut rows=Vec::new();let mut df=vec![0usize;terms.len()];
    for file in paths {
        if Instant::now()>=deadline {partial=true;break;}
        if !safe_file(&root,&file){partial=true;continue;}
        let path=root.join(&file);let Some(stamp)=metadata_stamp(&path)else{partial=true;continue;};
        if stamp.0>2*1024*1024 || stats.scanned_bytes+stamp.0>MAX_DISCOVERY_BYTES {partial=true;continue;}
        let text=match fs::read_to_string(&path){Ok(text)=>text,Err(_)=>{partial=true;continue;}};
        if metadata_stamp(&path)!=Some(stamp){partial=true;continue;}
        stats.scanned_files+=1;stats.scanned_bytes+=text.len() as u64;
        let mut hits=matcher.matches(&text).into_iter().collect::<Vec<_>>();
        hits.extend(matcher.matches(&file));hits.sort_unstable();hits.dedup();
        for &hit in &hits {df[hit]+=1;}
        rows.push(Candidate{hash:digest(text.as_bytes()),file,text,hits,score:0.0});
    }
    let n=rows.len().max(1) as f64;
    for row in &mut rows {
        let path_hits=matcher.matches(&row.file);
        for &id in &row.hits {row.score+=terms[id].1*(1.0+n/(df[id].max(1) as f64)).ln()*(if path_hits.matched(id){3.0}else{1.0});}
        // Avoid making a monolithic file win solely because it contains every generic word.
        row.score/=1.0+0.15*(1.0+row.text.len() as f64/16000.0).ln();
        if q.files.contains(&row.file){row.score+=10000.0;}
        if q.anchors.iter().any(|a|row.text.contains(a)){row.score+=1000.0;}
    }
    let mut order=(0..rows.len()).filter(|&i|rows[i].score>0.0).collect::<Vec<_>>();
    order.sort_by(|&a,&b|rows[b].score.total_cmp(&rows[a].score).then(rows[a].file.cmp(&rows[b].file)));
    stats.discovery_ms=started.elapsed().as_secs_f64()*1000.0;
    let slot=demand_slot(&root)?;let mut cache=slot.try_lock().map_err(|_|"same repository candidate parsing is busy")?;
    let valid=rows.iter().map(|r|r.file.as_str()).collect::<HashSet<_>>();
    cache.entries.retain(|file,_|valid.contains(file.as_str()));
    // A cache improves repeat queries only. It never determines recall or retains stale text.
    if cache.bytes>MAX_PARSE_BYTES*2 {cache.entries.clear();cache.bytes=0;}
    let mut parsed=HashSet::new();let mut units=Vec::new();
    let initial=order.iter().copied().take(INITIAL_FILES).collect::<Vec<_>>();
    partial|=parse_candidates(&rows,&initial,&mut cache,&mut parsed,&mut units,&mut stats,q,deadline);
    let files=rows.iter().map(|r|r.file.clone()).collect::<HashSet<_>>();
    // Rank before expansion: do not recursively parse every import in an entire application.
    for _ in 0..2 {
        if units.is_empty()||Instant::now()>=deadline||parsed.len()>=MAX_PARSED_FILES {break;}
        let c=subset(units.clone(),parsed.len(),stats.reparsed_files,partial);
        let lexical=super::lexical_rank(&c,&units,q);
        let seeds=rank::fuse(&lexical,&[],&units,q);
        let mut direct=HashSet::new();let mut names=HashSet::new();let mut events=HashSet::new();
        for &(id,_) in seeds.iter().take(4) {
            let u=&units[id];
            if u.name.len()>=4 {names.insert(u.name.clone());}
            names.extend(u.commands.iter().filter(|s|s.len()>=4).cloned());
            for (_,event) in &u.events {events.insert(event.clone());}
            for import in u.imports.iter() {
                if u.calls.contains(&import.name)||u.members.iter().any(|(object,_)|object==&import.name) {
                    if let Some(file)=resolve_specifier(&import.from,&u.file,&files){direct.insert(file);}
                }
            }
            for (object,member) in &u.members {
                if !object.contains("::"){continue;}
                names.insert(member.clone());
                if let Some(suffix)=object.strip_prefix("crate::") {
                    if let Some(pos)=u.file.rfind("src/") {
                        let prefix=&u.file[..pos+4];let module=suffix.replace("::","/");
                        for path in [format!("{prefix}{module}.rs"),format!("{prefix}{module}/mod.rs")] {if files.contains(&path){direct.insert(path);}}
                    }
                }
            }
        }
        let mut neighbours=Vec::new();
        for (i,row) in rows.iter().enumerate().filter(|(i,_)|!parsed.contains(i)) {
            let direct_hit=direct.contains(&row.file);
            let event_hit=events.iter().any(|s|row.text.contains(s));
            let name_hits=names.iter().filter(|name|row.text.contains(name.as_str())).count();
            if direct_hit||event_hit||name_hits>0 {neighbours.push((i,(if direct_hit{10000.0}else{0.0})+(if event_hit{1000.0}else{0.0})+name_hits.min(8) as f64*10.0+row.score));}
        }
        neighbours.sort_by(|a,b|b.1.total_cmp(&a.1).then(rows[a.0].file.cmp(&rows[b.0].file)));
        let wanted=neighbours.iter().take(8).map(|&(i,_)|i).collect::<Vec<_>>();
        if wanted.is_empty(){break;}
        stats.dependency_rounds+=1;
        partial|=parse_candidates(&rows,&wanted,&mut cache,&mut parsed,&mut units,&mut stats,q,deadline);
    }
    stats.candidate_files_omitted=order.iter().filter(|i|!parsed.contains(i)).count();
    cache.bytes=cache.entries.values().map(|(_,units)|units.first().map(|u|u.source.iter().map(|s|s.len()+1).sum::<usize>()).unwrap_or(0)).sum();
    let result=subset(units,parsed.len(),stats.reparsed_files,partial);
    Ok((result,stats))
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
