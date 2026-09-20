"""Keep source-linked execution callers inside the bounded declaration selection.
The candidate list must not evict a needed caller before packet assembly sees it.
Run after polaris-connected-packet.py; no repository-specific labels or names.
"""
from pathlib import Path
p=Path('src-tauri/src/nova_tools_native/polaris_demand_v2.rs');s=p.read_text()
old='''    let mut selected=Vec::new();let mut seen=HashSet::new();
    for &(id,line,_) in &ranked {if seen.insert(id){selected.push((id,line));if selected.len()>=cap{break;}}}'''
new='''    let mut selected=Vec::new();let mut seen=HashSet::new();
    // Keep source-connected execution callers before file diversity consumes
    // the bounded declaration budget. No additional global index is built.
    let facets=q.facets();
    let mut seeds=caller_names.iter().collect::<Vec<_>>();seeds.sort();
    let mut wanted_callers=Vec::<(usize,usize,usize,f64)>::new();
    for (seed_file,seed_name) in seeds.into_iter().take(64) {
        let Some(&id)=ids.iter().find(|&&id|rows[id].file==*seed_file)else{continue;};
        let Some(file)=cache.entries.get(seed_file)else{continue;};
        let Some(seed)=file.entry.syms.iter().find(|s|s.name==*seed_name)else{continue;};
        let words=query::tokens(&file.source[seed.ln-1..seed.end.min(file.source.len())].join("\\n")).into_iter().collect::<HashSet<_>>();
        let missing=facets.iter().filter(|g|!g.split('|').any(|w|words.contains(w))).collect::<Vec<_>>();
        if missing.is_empty(){continue;}
        let mut best=None;
        for symbol in &file.entry.syms {
            if symbol.name==*seed_name||!file.names.contains_key(&symbol.ln)||!role_allowed(seed_file,symbol,q){continue;}
            let body=file.source[symbol.ln-1..symbol.end.min(file.source.len())].join("\\n");
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
    for &(id,line,_) in &ranked {if seen.insert(id){selected.push((id,line));if selected.len()>=cap{break;}}}'''
assert s.count(old)==1;s=s.replace(old,new)
# Explore callers of the two leading implementations, not callers of every
# transitive helper. Downstream helper/type discovery retains its own budget.
old='let mut direct=HashSet::new();let mut names=HashSet::new();let mut callers=HashSet::new();'
new='let mut direct=HashSet::new();let mut names=HashSet::new();let callers=seeds.iter().take(2).map(|(i,_)|(units[*i].file.clone(),units[*i].name.clone())).collect::<HashSet<_>>();'
assert s.count(old)==1;s=s.replace(old,new)
old='direct.insert(u.file.clone());callers.insert((u.file.clone(),u.name.clone()));'
assert s.count(old)==1;s=s.replace(old,'direct.insert(u.file.clone());')
s+='''
#[cfg(test)] mod caller_discovery_tests {
    use super::*;
    #[test] fn bounded_discovery_keeps_execution_caller_among_many_key_named_helpers() {
        let d=tempfile::tempdir().unwrap();
        let mut text=String::from("// 桌面键盘\\nfn decode_chord() -> bool { true }\\nfn perform_input() -> bool { let parsed=decode_chord(); release_pressed(); parsed }\\nfn release_pressed() {}\\n");
        for n in 0..120 {text.push_str(&format!("fn keyboard_key_parse_{n}() -> bool {{ decode_chord() }}\\n"));}
        fs::write(d.path().join("device.rs"),text).unwrap();
        let q=query::Query::parse(serde_json::json!({"task":"键盘组合按键解析后释放","keywords":["decode_chord"],"maxBytes":12000})).unwrap();
        let (c,_)=demand_corpus(d.path(),&q,Instant::now()+Duration::from_secs(3)).unwrap();
        assert!(c.units.iter().any(|u|u.name=="perform_input"));
        assert!(c.units.iter().any(|u|u.name=="decode_chord"));
    }
}
''';p.write_text(s)
p=Path('src-tauri/src/nova_tools_native/polaris_packet.rs');s=p.read_text()
old='''        if let Some(&n)=root_rank.get(&identity(&units[*id])){return if n<2{n}else{6+n};}
        if execution_callers.contains(&identity(&units[*id])){2}
        else if matches!(*role,"callee-reference"|"command-reference"){3}
        else if *role=="type-reference"{4}else{20}'''
new='''        let primary=root_rank.get(&identity(&units[*id])).copied();
        if primary==Some(0){return 0;}
        let execution=execution_callers.contains(&identity(&units[*id]));
        if execution&&bytes(&units[*id])<=q.hard/2{return 1;}
        if primary==Some(1){return 2;}
        if execution{return 5;}
        if let Some(n)=primary{return 6+n;}
        if matches!(*role,"callee-reference"|"command-reference"){3}
        else if *role=="type-reference"{4}else{20}'''
assert s.count(old)==1;s=s.replace(old,new)
old='''        let (start,mut end)=if full_cost<=cap&&u.owner_end-full_start+1<=lines_left{(full_start,u.owner_end)}else{(u.start,u.end)};'''
new='''        // An execution body fitting half the total budget is more useful
        // intact than reserving its last branch for unrelated alternatives.
        let cap=if execution_callers.contains(&identity(u))&&full_cost<=q.hard/2&&full_cost<=available {available}else{cap};
        let (start,mut end)=if full_cost<=cap&&u.owner_end-full_start+1<=lines_left{(full_start,u.owner_end)}else{(u.start,u.end)};'''
assert s.count(old)==1;s=s.replace(old,new)
p.write_text(s)
print('Reserved caller discovery and complete execution bodies; no budget increase, label changes or Reasonix modifications.')
