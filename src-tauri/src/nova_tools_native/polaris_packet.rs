// Assemble source-backed working sets, not four unrelated search snippets.
// Code and line numbers are preserved; omitted bodies remain explicit.
use super::*;
pub(super) fn dependency_names(unit:&CodeUnit)->HashSet<String>{
    static IDENT:OnceLock<Regex>=OnceLock::new();
    let body=unit.source[unit.owner_start-1..unit.owner_end].join("\n");
    IDENT.get_or_init(||Regex::new(r"\b[A-Z][A-Za-z0-9_]{2,}\b").unwrap())
        .find_iter(&body).take(256).map(|m|m.as_str().to_string()).collect()
}
fn data_definition(unit:&CodeUnit)->bool{
    matches!(unit.passage.split(':').next(),Some("Defined type"|"Defined class"|"Defined const"))
}
fn data_link(from:&CodeUnit,to:&CodeUnit,files:&HashSet<String>,names:&HashSet<String>)->bool{
    if !data_definition(to)||!names.contains(&to.name){return false;}
    from.file==to.file||from.imports.iter().any(|im|{
        im.orig.as_deref().unwrap_or(&im.name)==to.name&&resolve_specifier(&im.from,&from.file,files).as_deref()==Some(to.file.as_str())
    })
}
pub(super) struct Packet{pub body:String,pub evidence:Vec<Value>,pub gaps:Vec<Value>,pub links:Vec<Value>}
fn bytes(unit:&CodeUnit)->usize{
    unit.source[unit.owner_start-1..unit.owner_end].iter().map(|s|s.len()+10).sum::<usize>()
}
pub(super) fn pack(root:&Path,units:&[Arc<CodeUnit>],ranked:&[(usize,f64)],q:&Query,deadline:Instant)->Packet{
    let mut result=Packet{body:String::new(),evidence:vec![],gaps:vec![],links:vec![]};
    let files=units.iter().map(|u|u.file.clone()).collect::<HashSet<_>>();
    let scores=ranked.iter().copied().collect::<HashMap<_,_>>();
    let mut roots=Vec::new();let mut owners=HashSet::new();let mut per_file=HashMap::<String,usize>::new();
    for &(id,_) in ranked{
        let u=&units[id];if *per_file.get(&u.file).unwrap_or(&0)>=2||!owners.insert(identity(u)){continue;}
        roots.push(id);*per_file.entry(u.file.clone()).or_default()+=1;if roots.len()>=4{break;}
    }
    // One representative per declaration, not every overlapping slice.
    let mut unique=Vec::new();let mut seen=HashSet::new();
    for &(i,_) in ranked{if seen.insert(identity(&units[i])){unique.push(i);}}
    for (i,u) in units.iter().enumerate(){if seen.insert(identity(u)){unique.push(i);}}
    let mut verified=HashMap::<String,bool>::new();let mut emitted=HashSet::new();
    let mut ordered=Vec::<(usize,&str)>::new();let mut planned=HashSet::new();
    for (position,&seed) in roots.iter().enumerate(){
        if planned.insert(identity(&units[seed])){ordered.push((seed,"primary"));}
        if position>=2{continue;}
        let mut frontier=std::collections::VecDeque::from([(seed,0usize)]);let mut seed_links=0;
        while let Some((parent,depth))=frontier.pop_front(){
            if depth>=2||seed_links>=6||Instant::now()>=deadline{continue;}
            let mut neighbours=Vec::new();let data_names=dependency_names(&units[parent]);
            for &i in &unique{
                let u=&units[i];if planned.contains(&identity(u)){continue;}
                let rel=related(&units[parent],u,&files).or_else(||data_link(&units[parent],u,&files,&data_names).then_some("type-reference"));
                if let Some(role)=rel{
                    // Wide callers are impact information; don't let them evict actual bodies.
                    if role=="caller-reference"&&bytes(u)>q.hard/4{continue;}
                    let priority=match role{"callee-reference"|"command-reference"=>3,"type-reference"=>2,_=>1};
                    neighbours.push((i,role,priority,*scores.get(&i).unwrap_or(&0.0),bytes(u)));
                }
            }
            neighbours.sort_by(|a,b|b.2.cmp(&a.2).then(b.3.total_cmp(&a.3)).then(a.4.cmp(&b.4)).then(a.0.cmp(&b.0)));
            for (i,role,_,_,_) in neighbours.into_iter().take(if depth==0{4}else{2}){
                if seed_links>=6{break;}
                if planned.insert(identity(&units[i])){
                    ordered.push((i,role));seed_links+=1;
                    result.links.push(serde_json::json!({"from":format!("{}:{}",units[parent].file,units[parent].name),"to":format!("{}:{}",units[i].file,units[i].name),"kind":role}));
                    if matches!(role,"callee-reference"|"command-reference"){frontier.push_back((i,depth+1));}
                }
            }
        }
    }
    let mut lines_left=q.lines;let mut ranges=HashMap::<String,Vec<(usize,usize)>>::new();
    let reserve=3000usize.min(q.hard/3);let code_budget=q.hard.saturating_sub(reserve);
    for (position,(id,relation)) in ordered.iter().copied().enumerate(){
        let u=&units[id];if !emitted.insert(identity(u)){continue;}
        if !*verified.entry(u.file.clone()).or_insert_with(||index::verified(root,u)){
            result.gaps.push(serde_json::json!({"file":u.file,"start":u.owner_start,"end":u.owner_end,"reason":"source-changed"}));continue;
        }
        if let Some(&(a,b))=ranges.get(&u.file).into_iter().flatten().find(|&&(a,b)|a<=u.owner_start&&b>=u.owner_end){
            result.evidence.push(serde_json::json!({"file":u.file,"symbol":u.name,"start":u.owner_start,"end":u.owner_end,"role":u.role,"relation":relation,"sourceHash":u.file_hash,"complete":true,"includedIn":[a,b]}));continue;
        }
        let available=code_budget.saturating_sub(result.body.len());
        let future=ordered[position+1..].iter().take(3).map(|(i,_)|bytes(&units[*i]).min(1800)).sum::<usize>();
        let cap=available.saturating_sub(future.min(available/2)).max(available.min(1000));
        let source=&u.source;let full_start=u.start.min(u.owner_start);
        let full_cost=source[full_start-1..u.owner_end].iter().enumerate().map(|(n,s)|s.len()+format!("{}: ",full_start+n).len()+1).sum::<usize>()+180;
        let (start,mut end)=if full_cost<=cap&&u.owner_end-full_start+1<=lines_left{(full_start,u.owner_end)}else{(u.start,u.end)};
        while end>=start&&(source[start-1..end].iter().enumerate().map(|(n,s)|s.len()+format!("{}: ",start+n).len()+1).sum::<usize>()+180>cap||end-start+1>lines_left){end-=1;}
        if end<start{result.gaps.push(serde_json::json!({"file":u.file,"start":u.owner_start,"end":u.owner_end,"reason":"working-set-budget"}));continue;}
        let complete=start<=u.owner_start&&end>=u.owner_end;
        let snippet=source[start-1..end].iter().enumerate().map(|(n,s)|format!("{}: {}\n",start+n,s)).collect::<String>();
        result.body.push_str(&format!("\n### {}:{}-{} [{} {}]\ncoverage: {} {}:{}-{}\n```\n{snippet}```\n",u.file,start,end,relation,u.name,if complete{"BODY"}else{"PARTIAL"},u.file,start,end));
        lines_left-=end-start+1;ranges.entry(u.file.clone()).or_default().push((start,end));
        result.evidence.push(serde_json::json!({"file":u.file,"symbol":u.name,"start":start,"end":end,"role":u.role,"relation":relation,"sourceHash":u.file_hash,"complete":complete}));
        if !complete{result.gaps.push(serde_json::json!({"file":u.file,"start":u.owner_start,"end":u.owner_end,"reason":"large-unit"}));}
    }
    result
}
#[cfg(test)]
mod packet_tests{
    use super::*;
    #[test]fn one_call_contains_implementation_helper_and_local_type(){
        let d=tempfile::tempdir().unwrap();
        fs::write(d.path().join("jobs.rs"),"pub struct CancelPolicy { pub enabled: bool }\n// 取消后台任务\npub fn cancel_job(p: CancelPolicy) -> bool { should_cancel(p) }\nfn should_cancel(p: CancelPolicy) -> bool { p.enabled }\n").unwrap();
        let out=polaris(d.path(),serde_json::json!({"task":"取消后台任务","maxBytes":12000})).unwrap();
        for s in ["fn cancel_job", "fn should_cancel", "struct CancelPolicy"]{assert!(out.contains(s),"{s}: {out}");}
        assert!(out.len()<=12000);
    }
    #[test]fn closure_keeps_both_error_branch_and_request_execution(){
        let d=tempfile::tempdir().unwrap();
        fs::write(d.path().join("client.rs"),"// 更新请求重试\nfn request_update() -> bool { if should_retry() { resend() } else { false } }\nfn should_retry() -> bool { true }\nfn resend() -> bool { false }\n").unwrap();
        let out=polaris(d.path(),serde_json::json!({"task":"更新请求重试","maxBytes":12000})).unwrap();
        for s in ["fn request_update", "fn should_retry", "fn resend"]{assert!(out.contains(s),"{s}: {out}");}
    }
}
