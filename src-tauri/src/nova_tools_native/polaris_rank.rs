// Source-backed refinement, independent of query examples or expected answers.
use super::*;

/// Resolve only an explicit crate/self/super Rust function path to an existing
/// source module. Never join unrelated functions just because their names match.
pub(super) fn qualified_call(from:&CodeUnit,to:&CodeUnit,files:&HashSet<String>)->bool {
    if !from.file.ends_with(".rs")||!to.file.ends_with(".rs"){return false;}
    from.members.iter().any(|(object,member)| {
        if member!=&to.name||!object.contains("::"){return false;}
        let mut parts=object.split("::");let first=parts.next().unwrap_or("");
        let base=if first=="crate" {
            let Some(pos)=from.file.rfind("src/")else{return false;};from.file[..pos+4].trim_end_matches('/').to_string()
        }else if matches!(first,"self"|"super") {
            let Some((dir,file))=from.file.rsplit_once('/')else{return false;};
            let module=if matches!(file,"lib.rs"|"main.rs"|"mod.rs"){dir.to_string()}else{format!("{dir}/{}",file.trim_end_matches(".rs"))};
            if first=="super" {module.rsplit_once('/').map(|(d,_)|d.to_string()).unwrap_or_default()}else{module}
        }else{return false;};
        let suffix=parts.collect::<Vec<_>>().join("/");
        if suffix.is_empty(){return false;}
        let path=format!("{base}/{suffix}");
        [format!("{path}.rs"),format!("{path}/mod.rs")].iter().any(|p|p==&to.file&&files.contains(p))
    })
}
fn forwarding(unit:&CodeUnit)->bool {
    if unit.owner_end.saturating_sub(unit.owner_start)>18{return false;}
    let text=unit.source[unit.owner_start-1..unit.owner_end].join("\n");
    let Some((_,body))=text.split_once('{')else{return false;};
    // Small predicates and stateful handlers are real behavior, not forwarding.
    if ["let ","const ","if ","match ","for ","while ","&&","||","==","!=",".push(",".insert("].iter().any(|s|body.contains(s)){return false;}
    !unit.calls.is_empty()
}
/// Replace a pure forwarding entry with its sole verified in-repository callee.
/// The wrapper remains available as caller evidence. This is a bounded graph
/// refinement, not a guessed symbol or an expansion to arbitrary similarly named code.
pub(super) fn refine_forwarders(ranked:&mut Vec<(usize,f64)>,units:&[Arc<CodeUnit>],q:&Query){
    let files=units.iter().map(|u|u.file.clone()).collect::<HashSet<_>>();
    let mut by_name=HashMap::<&str,Vec<usize>>::new();
    for (i,u) in units.iter().enumerate(){by_name.entry(&u.name).or_default().push(i);}
    let mut handled=HashSet::new();
    for _ in 0..2 {
        let batch=ranked.iter().take(12).copied().collect::<Vec<_>>();
        for (id,score) in batch {
            let u=&units[id];
            if !forwarding(u)||!handled.insert(identity(u))||q.files.contains(&u.file)||q.anchors.iter().any(|a|a.eq_ignore_ascii_case(&u.name)){continue;}
            let mut seen=HashSet::new();let mut targets=Vec::new();
            for call in &u.calls {
                for &other in by_name.get(call.as_str()).into_iter().flatten(){
                    let v=&units[other];
                    if identity(v)==identity(u)||!seen.insert(identity(v)){continue;}
                    if matches!(related(u,v,&files),Some("callee-reference"|"command-reference")){targets.push(other);}
                }
            }
            if targets.len()!=1{continue;}
            let target=targets[0];
            if let Some((_,s))=ranked.iter_mut().find(|(i,_)|*i==id){*s*=0.45;}
            if let Some((_,s))=ranked.iter_mut().find(|(i,_)|identity(&units[*i])==identity(&units[target])){*s=(*s).max(score*1.05);}
            else{ranked.push((target,score*1.05));}
        }
        ranked.sort_by(|a,b|b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn qualified_rust_wrappers_lead_to_their_actual_module_only(){
        let dir=tempfile::tempdir().unwrap();fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/lib.rs"),"// 读取剪贴板文件路径\npub fn paths() -> Vec<String> { crate::clipboard::read_paths() }\n").unwrap();
        fs::write(dir.path().join("src/clipboard.rs"),"pub fn read_paths() -> Vec<String> { let result = read_system(); result }\n").unwrap();
        fs::write(dir.path().join("src/unrelated.rs"),"pub fn read_paths() -> Vec<String> { unreachable!() }\n").unwrap();
        let c=index::corpus(dir.path(),Instant::now()+Duration::from_secs(5)).unwrap();let files=c.units.iter().map(|u|u.file.clone()).collect();
        let wrapper=c.units.iter().find(|u|u.name=="paths").unwrap();
        let actual=c.units.iter().find(|u|u.file=="src/clipboard.rs").unwrap();let noise=c.units.iter().find(|u|u.file=="src/unrelated.rs").unwrap();
        assert!(qualified_call(wrapper,actual,&files));assert!(!qualified_call(wrapper,noise,&files));
        let mut ranked=vec![(c.units.iter().position(|u|u.name=="paths").unwrap(),1.0)];
        let q=Query::parse(serde_json::json!({"task":"读取剪贴板文件路径"})).unwrap();
        refine_forwarders(&mut ranked,&c.units,&q);
        assert_eq!(c.units[ranked[0].0].file,"src/clipboard.rs");
    }
    #[test] fn short_predicates_and_state_mutations_are_not_forwarders(){
        let dir=tempfile::tempdir().unwrap();
        fs::write(dir.path().join("jobs.rs"),"pub fn retry(code:u32) -> bool { code == 403 && enabled() }\npub fn stop() { let state = load(); cancel(state); }\n").unwrap();
        let c=index::corpus(dir.path(),Instant::now()+Duration::from_secs(5)).unwrap();
        assert!(c.units.iter().all(|u|!forwarding(u)));
    }
}
