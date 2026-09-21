"""Measured corrections: bounded metadata, fresh query cache and parallel declaration work."""
from pathlib import Path
R=Path('src-tauri/src/nova_tools_native')
def edit(name,old,new):
 p=R/name;s=p.read_text();assert s.count(old)==1,(name,old[:80]);p.write_text(s.replace(old,new))
edit('polaris_packet.rs','    let reserve=3000usize.min(q.hard/3);let code_budget=q.hard.saturating_sub(reserve);','    let code_budget=q.hard;')
edit('polaris_packet.rs','        let available=code_budget.saturating_sub(result.body.len());','''        let pending=ordered[position+1..].iter().map(|(i,_)|{let v=&units[*i];serde_json::json!({"file":v.file,"start":v.owner_start,"end":v.owner_end,"reason":"working-set-budget"})}).collect::<Vec<_>>();
        let meta_bytes=serde_json::to_string(&result.evidence).unwrap().len()+serde_json::to_string(&result.gaps).unwrap().len()+serde_json::to_string(&pending).unwrap().len()+1400;
        let available=code_budget.saturating_sub(result.body.len()+meta_bytes);''')
edit('polaris_packet.rs','future.min(available/2)','future.min(2000).min(available/2)')
# A complete content fingerprint is checked before consulting the query cache.
edit('polaris_demand_v2.rs','struct DemandCache { entries:BTreeMap<String,LightFile>, bytes:usize }','struct DemandCache { entries:BTreeMap<String,LightFile>, bytes:usize, results:BTreeMap<String,(String,Corpus,DemandStats)> }')
edit('polaris_demand_v2.rs','#[derive(Default,Serialize)]','#[derive(Clone,Default,Serialize)]')
edit('polaris_demand_v2.rs','    pub scanned_files:usize,','    pub query_cache_hit:bool,\n    pub scanned_files:usize,')
edit('polaris_demand_v2.rs','    if cache.bytes>MAX_PARSE_BYTES*2{cache.entries.clear();cache.bytes=0;}','''    if cache.bytes>MAX_PARSE_BYTES*2{cache.entries.clear();cache.results.clear();cache.bytes=0;}
    let query_key=q.params.to_string();
    let fingerprint=digest(rows.iter().map(|r|format!("{}\\0{}\\n",r.file,r.hash)).collect::<String>().as_bytes());
    if !partial {if let Some((old,c,previous))=cache.results.get(&query_key){if old==&fingerprint {
        let mut d=previous.clone();d.query_cache_hit=true;d.discovery_ms=stats.discovery_ms;d.parse_ms=0.0;d.reparsed_files=0;d.parsed_files=0;d.parsed_bytes=0;
        let mut c=c.clone();c.changed=0;return Ok((c,d));
    }}}''')
edit('polaris_demand_v2.rs','    let c=subset(units,parsed.len(),stats.reparsed_files,partial);Ok((c,stats))','''    let c=subset(units,parsed.len(),stats.reparsed_files,partial);
    if !partial {if cache.results.len()>=64 {cache.results.clear();}cache.results.insert(query_key,(fingerprint,c.clone(),stats.clone()));}
    Ok((c,stats))''')
edit('polaris_demand_v2.rs','        for line in lines {if selected.insert','        let mut sorted=lines.into_iter().collect::<Vec<_>>();sorted.sort_unstable();\n        for line in sorted {if selected.insert')
p=R/'polaris_demand_v2.rs';s=p.read_text();start=s.index('fn declarations(');end=s.index('fn role_allowed(',start)
s=s[:start]+'''fn build_light(row:&Candidate)->LightFile {
    let entry=scan_source(&row.text,&row.file);let source=Arc::new(row.text.lines().map(str::to_owned).collect::<Vec<_>>());
    let names=entry.syms.iter().filter(|s|is_retrieval_unit(s,&source)).map(|s|{
        let mut terms=query::tokens(&s.name).into_iter().collect::<HashSet<_>>();terms.extend(identifier_aliases(&s.name));(s.ln,terms)
    }).collect();
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
''' + s[end:];p.write_text(s)
# Physical key input is not prompt submission; no project-specific symbol mapping.
edit('polaris_query.rs','        let test_intent=', '''        if ["键盘","按键","组合键","keyboard","keystroke"].iter().any(|word|task.contains(word)) {
            for (term,weight) in &mut terms {
                if ["键盘","按键","组合键","keyboard","keypress","keystroke","chord","key"].contains(&term.as_str()){*weight*=5.0;}
                if ["发送","提交","send","submit","prompt","dispatch","deliver"].contains(&term.as_str()){*weight*=0.5;}
            }
        }
        let test_intent=''')
p=R/'polaris_packet.rs';s=p.read_text();s+='''
#[cfg(test)] mod envelope_tests {
    use super::*;
    #[test] fn many_dependencies_never_turn_a_small_budget_into_an_error() {
        let d=tempfile::tempdir().unwrap();let mut text=String::from("// 取消任务\\nfn cancel_job() {\\n");
        for i in 0..12 {text.push_str(&format!(" helper_{i}();\\n"));}text.push_str("}\\n");
        for i in 0..12 {text.push_str(&format!("fn helper_{i}() {{ let payload = \\\"{}\\\"; }}\\n","x".repeat(600)));}
        fs::write(d.path().join("jobs.rs"),text).unwrap();
        let out=polaris(d.path(),serde_json::json!({"task":"取消任务","maxBytes":8192})).unwrap();
        assert!(out.len()<=8192);assert!(out.contains("fn cancel_job"));
    }
}
''';p.write_text(s)
# sections() strips line numbers for scoring. The editor needs original lines.
p=Path('scripts/polaris-one-shot-edit.py');s=p.read_text();start=s.index('        for section in ab.sections(packet):');end=s.index("        reconstructed=",start)
s=s[:start]+'''        in_file=False
        for ln in packet.splitlines():
            if ln.startswith('### '):in_file=bool(re.match(r'^### task\\.rs:\\d+-\\d+ ',ln))
            if not in_file:continue
            m=re.match(r'^(\\d+): (.*)$',ln)
            if m:
                n=int(m[1]);assert n not in source_lines or source_lines[n]==m[2];source_lines[n]=m[2]
''' + s[end:];p.write_text(s)
print('Applied bounded metadata, content-verified query cache, parallel declarations and edit-fixture parser correction.')
