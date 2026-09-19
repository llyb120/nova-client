// Source-backed refinement, independent of query examples or expected answers.
use super::*;

/// Refine source-backed retrieval channels independently, then combine ranks.
/// Precise lexical evidence is retained without turning semantic-only matches off.
pub(super) fn fuse(lexical:&[(usize,f64)],dense:&[(usize,f64)],units:&[Arc<CodeUnit>],q:&Query)->Vec<(usize,f64)> {
    let channel=|rows:&[(usize,f64)]| {
        let mut seen=HashSet::new();
        let mut ranked=rows.iter().enumerate().filter_map(|(r,(i,_))|seen.insert(identity(&units[*i])).then_some((*i,1.0/(20.0+r as f64)))).collect::<Vec<_>>();
        refine_forwarders(&mut ranked,units,q);ranked
    };
    let lexical=channel(lexical);let dense=channel(dense);
    // Channels may select different slices of the SAME function. Accumulate
    // at the owner identity, not at the slice index, or its votes get split and
    // the later duplicate-removal silently discards half of the evidence.
    let mut scores=HashMap::<(String,String,usize),(usize,f64)>::new();
    for (rows,weight) in [(&lexical,0.7),(&dense,0.3)] {
        // The source checks above produce meaningful score margins. Replacing
        // those margins by another reciprocal rank discards operation/constraint
        // evidence and lets a broad semantic match overrule a concrete body.
        let maximum=rows.first().map(|(_,score)|*score).unwrap_or(1.0).max(f64::EPSILON);
        for (id,score) in rows.iter() {
            let entry=scores.entry(identity(&units[*id])).or_insert((*id,0.0));
            entry.1+=weight*(score/maximum).clamp(0.0,1.0);
        }
    }
    for (i,u) in units.iter().enumerate() {
        if q.files.contains(&u.file)||q.anchors.iter().any(|a|a.eq_ignore_ascii_case(&u.name)) {
            scores.entry(identity(u)).or_insert((i,0.0)).1=2.0;
        }
    }
    let mut out=scores.into_values().collect::<Vec<_>>();
    out.sort_by(|a,b|b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    if std::env::var_os("NOVA_POLARIS_TRACE_RANK").is_some() {
        let rows=|channel:&[(usize,f64)]|channel.iter().take(32).map(|(i,s)|serde_json::json!({
            "file":units[*i].file,"symbol":units[*i].name,"start":units[*i].owner_start,"score":s
        })).collect::<Vec<_>>();
        eprintln!("[polaris-rank] {}",serde_json::json!({"query":q.task,"lexical":rows(&lexical),"semantic":rows(&dense),"fused":rows(&out)}));
    }
    out
}

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
    // Expression-bodied arrows are common IPC and functional wrappers; they
    // have no body brace, or only a later brace for an argument object.
    let start=match (text.find("=>"),text.find('{')) {
        (Some(a),Some(b)) if a<b=>a+2,
        (Some(a),None)=>a+2,
        (_,Some(b))=>b+1,
        _=>return false,
    };
    let body=&text[start..];
    // Small predicates and stateful handlers are real behavior, not forwarding.
    if ["let ","const ","if ","match ","for ","while ","&&","||","==","!=",".push(",".insert("].iter().any(|s|body.contains(s)){return false;}
    !unit.calls.is_empty()
}
/// Replace a pure forwarding entry with its sole verified in-repository callee.
/// The wrapper remains available as caller evidence. This is a bounded graph
/// refinement, not a guessed symbol or an expansion to arbitrary similarly named code.
pub(super) fn refine_forwarders(ranked:&mut Vec<(usize,f64)>,units:&[Arc<CodeUnit>],q:&Query){
    let facets=q.facets();
    if !facets.is_empty() {
        let predicates=facets.iter().filter(|group|group.split('|').any(|word|
            q.terms.iter().any(|(term,weight)|term==word&&*weight>1.0))).collect::<Vec<_>>();
        for (id,score) in ranked.iter_mut() {
            let u=&units[*id];
            if q.files.contains(&u.file)||q.anchors.iter().any(|a|a.eq_ignore_ascii_case(&u.name)){continue;}
            let covered=facets.iter().filter(|group|group.split('|').any(|word|u.terms.contains_key(word))).count();
            let fraction=covered as f64/facets.len() as f64;
            // Keep a nonzero semantic-only path while preferring evidence that
            // covers both the requested operation AND its object/constraints.
            *score*=0.30+0.70*fraction*fraction;
            // An encrypted *input type* is not an encryption operation. Require
            // support in a defined name, a called operation, or original CJK
            // source/comment text, rather than the signature's dictionary aliases.
            let operation=predicates.is_empty()||predicates.iter().any(|group|group.split('|').any(|word|
                u.name_terms.contains(word)
                ||u.calls.iter().any(|call|query::tokens(call).iter().any(|term|term==word))
                ||(!word.is_ascii()&&u.source[u.start-1..u.end].iter().any(|line|line.contains(word)))));
            if !operation{*score*=0.40;}
            let name_operation=predicates.iter().any(|group|group.split('|').any(|word|u.name_terms.contains(word)));
            if name_operation {*score*=1.3;}
            // The user-facing command is the behavior dispatcher, unless it
            // merely forwards (the next stage then resolves its concrete callee).
            if ["按钮","点击","按下"].iter().any(|word|q.task.contains(word)) && name_operation
                && u.source[u.owner_start.saturating_sub(4)..u.owner_start].iter().any(|line|line.contains("#[tauri::command")) {*score*=1.3;}

        }
        ranked.sort_by(|a,b|b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    }
    let files=units.iter().map(|u|u.file.clone()).collect::<HashSet<_>>();
    let mut by_name=HashMap::<&str,Vec<usize>>::new();
    for (i,u) in units.iter().enumerate(){by_name.entry(&u.name).or_default().push(i);}
    let mut handled=HashSet::new();
    for _ in 0..4 {
        let batch=ranked.iter().take(12).copied().collect::<Vec<_>>();
        for (id,score) in batch {
            let u=&units[id];
            if !forwarding(u)||!handled.insert(identity(u))||q.files.contains(&u.file)||q.anchors.iter().any(|a|a.eq_ignore_ascii_case(&u.name)){continue;}
            let mut seen=HashSet::new();let mut targets=Vec::new();
            for call in u.calls.iter().chain(u.commands.iter()) {
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
    #[test] fn declared_module_namespace_is_not_an_unrelated_same_name() {
        let dir=tempfile::tempdir().unwrap();fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/lib.rs"),"mod system;\npub fn paths() -> Vec<String> { system::read_paths() }\n").unwrap();
        fs::write(dir.path().join("src/system.rs"),"pub fn read_paths() -> Vec<String> { read_native() }\n").unwrap();
        let c=index::corpus(dir.path(),Instant::now()+Duration::from_secs(5)).unwrap();
        let files=c.units.iter().map(|u|u.file.clone()).collect::<HashSet<_>>();
        let wrapper=c.units.iter().find(|u|u.name=="paths").unwrap();
        let implementation=c.units.iter().find(|u|u.name=="read_paths").unwrap();
        assert_eq!(related(wrapper,implementation,&files),Some("callee-reference"));
    }

    #[test] fn expression_arrows_and_native_wrappers_reach_the_concrete_body() {
        let dir=tempfile::tempdir().unwrap();fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/api.ts"),"export const api = {\n list: () => invoke('native_list'),\n};\n").unwrap();
        fs::write(dir.path().join("src/lib.rs"),"mod disk;\n#[tauri::command]\npub fn native_list() -> Vec<String> { disk::paths() }\n").unwrap();
        fs::write(dir.path().join("src/disk.rs"),"pub fn paths() -> Vec<String> { enumerate() }\nfn enumerate() -> Vec<String> { let entries = read_directory(); entries }\n").unwrap();
        let c=index::corpus(dir.path(),Instant::now()+Duration::from_secs(5)).unwrap();
        let mut rows=vec![(c.units.iter().position(|u|u.name=="list").unwrap(),1.0)];
        let q=Query::parse(serde_json::json!({"task":"读取文件列表"})).unwrap();
        refine_forwarders(&mut rows,&c.units,&q);
        assert_eq!(c.units[rows[0].0].name,"enumerate");
    }
    #[test] fn function_values_passed_as_callbacks_link_only_to_real_source() {
        let dir=tempfile::tempdir().unwrap();
        fs::write(dir.path().join("pipeline.ts"),"function convert(row: string) { return row.toUpperCase(); }\nexport function run(rows: string[]) { return rows.map(convert); }\n").unwrap();
        let c=index::corpus(dir.path(),Instant::now()+Duration::from_secs(5)).unwrap();
        let files=c.units.iter().map(|u|u.file.clone()).collect();
        let run=c.units.iter().find(|u|u.name=="run").unwrap();
        let converter=c.units.iter().find(|u|u.name=="convert").unwrap();
        assert_eq!(related(run,converter,&files),Some("callee-reference"));
    }

    #[test] fn slices_of_one_function_share_fusion_votes() {
        let dir=tempfile::tempdir().unwrap();
        let mut text=String::new();
        for n in 0..8 {text.push_str(&format!("fn other{n}() {{ let x = 1; consume(x); }}\n"));}
        text.push_str("fn target() {\n");
        for _ in 0..150 {text.push_str("let x = 1;\n");}
        text.push_str("}\n");fs::write(dir.path().join("logic.rs"),text).unwrap();
        let c=index::corpus(dir.path(),Instant::now()+Duration::from_secs(5)).unwrap();
        let parts=c.units.iter().enumerate().filter(|(_,u)|u.name=="target").map(|(i,_)|i).collect::<Vec<_>>();
        assert!(parts.len()>1);
        let mut lexical=c.units.iter().enumerate().filter(|(_,u)|u.name!="target").map(|(i,_)|(i,1.0)).collect::<Vec<_>>();
        lexical.push((parts[0],1.0));
        let q=Query::parse(serde_json::json!({"task":"特定的约定"})).unwrap();
        let fused=fuse(&lexical,&[(parts[1],1.0)],&c.units,&q);
        assert_eq!(c.units[fused[0].0].name,"target");
        assert_eq!(fused.iter().filter(|(i,_)|c.units[*i].name=="target").count(),1);
    }

}
