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
    // Score the complete owner once. A parser's name may match strongly while
    // the execution/cleanup branch is in a longer caller and a different slice.
    let facets=q.facets();
    let coverage=unique.iter().map(|&id| {
        let u=&units[id];
        let mut words=query::tokens(&u.source[u.owner_start-1..u.owner_end].join("\n")).into_iter().collect::<HashSet<_>>();
        words.extend(u.name_terms.iter().cloned());
        let mask=facets.iter().enumerate().filter_map(|(n,g)|g.split('|').any(|w|words.contains(w)).then_some(n)).collect::<HashSet<_>>();
        (identity(u),mask)
    }).collect::<HashMap<_,_>>();
    let gains=|from:usize,to:usize|coverage[&identity(&units[to])].difference(&coverage[&identity(&units[from])]).count();
    let mut execution_callers=HashSet::new();
    let mut verified=HashMap::<String,bool>::new();let mut emitted=HashSet::new();
    let mut ordered=Vec::<(usize,&str)>::new();let mut planned=HashSet::new();
    for (position,&seed) in roots.iter().enumerate(){
        if planned.insert(identity(&units[seed])){ordered.push((seed,"primary"));}
        if position>=2{continue;}
        let mut frontier=std::collections::VecDeque::from([(seed,0usize,false)]);let mut seed_links=0;
        while let Some((parent,depth,type_only))=frontier.pop_front(){
            if depth>=3||seed_links>=8||Instant::now()>=deadline{continue;}
            let mut neighbours=Vec::new();let data_names=dependency_names(&units[parent]);
            for &i in &unique{
                let u=&units[i];if planned.contains(&identity(u)){continue;}
                let rel=(if type_only{None}else{related(&units[parent],u,&files)}).or_else(||data_link(&units[parent],u,&files,&data_names).then_some("type-reference"));
                if let Some(mut role)=rel{
                    // A Result<()> signature is not an invocation of its type alias.
                    if role=="callee-reference"&&data_definition(u){role="type-reference";}
                    // Wide callers are impact information; don't let them evict actual bodies.
                    let gain=gains(parent,i);
                    if role=="caller-reference"&&bytes(u)>q.hard/4&&gain==0{continue;}
                    let priority=match role{"callee-reference"|"command-reference"=>3,"type-reference"=>2,_=>1};
                    let relevance=*scores.get(&i).unwrap_or(&0.0)+if role=="caller-reference"{gain as f64*2.0}else{0.0};
                    neighbours.push((i,role,priority,relevance,bytes(u)));
                }
            }
            neighbours.sort_by(|a,b|b.2.cmp(&a.2).then(b.3.total_cmp(&a.3)).then(a.4.cmp(&b.4)).then(a.0.cmp(&b.0)));
            // At most one extra execution caller for each leading seed. It must
            // add a requested facet, not just mention a commonly named helper.
            if depth==0 {
                if let Some(&(caller,_,_,_,_))=neighbours.iter().filter(|(i,role,_,_,_)|*role=="caller-reference"&&gains(parent,*i)>0).max_by(|a,b|a.3.total_cmp(&b.3).then(b.4.cmp(&a.4))) {
                    if planned.insert(identity(&units[caller])) {
                        ordered.push((caller,"caller-reference"));execution_callers.insert(identity(&units[caller]));seed_links+=1;
                        result.links.push(serde_json::json!({"from":format!("{}:{}",units[parent].file,units[parent].name),"to":format!("{}:{}",units[caller].file,units[caller].name),"kind":"caller-reference"}));
                        frontier.push_back((caller,depth+1,true));
                    }
                }
            }
            for (i,role,_,_,_) in neighbours.into_iter().filter(|(_,role,_,_,_)|*role!="caller-reference").take(if depth==0{4}else{2}){
                if seed_links>=8{break;}
                if planned.insert(identity(&units[i])){
                    ordered.push((i,role));seed_links+=1;
                    result.links.push(serde_json::json!({"from":format!("{}:{}",units[parent].file,units[parent].name),"to":format!("{}:{}",units[i].file,units[i].name),"kind":role}));
                    if matches!(role,"callee-reference"|"command-reference"|"caller-reference"){frontier.push_back((i,depth+1,role=="caller-reference"));}
                }
            }
        }
    }
    let root_rank=roots.iter().enumerate().map(|(n,i)|(identity(&units[*i]),n)).collect::<HashMap<_,_>>();
    for (id,role) in &mut ordered{if root_rank.contains_key(&identity(&units[*id])){*role="primary";}}
    ordered.sort_by_key(|(id,role)|{
        let primary=root_rank.get(&identity(&units[*id])).copied();
        if primary==Some(0){return 0;}
        let execution=execution_callers.contains(&identity(&units[*id]));
        if execution&&bytes(&units[*id])<=q.hard/2{return 1;}
        if primary==Some(1){return 2;}
        if execution{return 5;}
        if let Some(n)=primary{return 6+n;}
        if matches!(*role,"callee-reference"|"command-reference"){3}
        else if *role=="type-reference"{4}else{20}
    });
    let mut lines_left=q.lines;let mut ranges=HashMap::<String,Vec<(usize,usize)>>::new();
    let code_budget=q.hard;
    for (position,(id,relation)) in ordered.iter().copied().enumerate(){
        let u=&units[id];if !emitted.insert(identity(u)){continue;}
        if !*verified.entry(u.file.clone()).or_insert_with(||index::verified(root,u)){
            result.gaps.push(serde_json::json!({"file":u.file,"start":u.owner_start,"end":u.owner_end,"reason":"source-changed"}));continue;
        }
        if let Some(&(a,b))=ranges.get(&u.file).into_iter().flatten().find(|&&(a,b)|a<=u.owner_start&&b>=u.owner_end){
            result.evidence.push(serde_json::json!({"file":u.file,"symbol":u.name,"start":u.owner_start,"end":u.owner_end,"role":u.role,"relation":relation,"sourceHash":u.file_hash,"complete":true,"includedIn":[a,b]}));continue;
        }
        let pending=ordered[position+1..].iter().map(|(i,_)|{let v=&units[*i];serde_json::json!({"file":v.file,"start":v.owner_start,"end":v.owner_end,"reason":"working-set-budget"})}).collect::<Vec<_>>();
        let meta_bytes=serde_json::to_string(&result.evidence).unwrap().len()+serde_json::to_string(&result.gaps).unwrap().len()+serde_json::to_string(&pending).unwrap().len()+1400;
        let available=code_budget.saturating_sub(result.body.len()+meta_bytes);
        let future=ordered[position+1..].iter().take(3).map(|(i,_)|bytes(&units[*i]).min(1800)).sum::<usize>();
        let cap=available.saturating_sub(future.min(2000).min(available/2)).max(available.min(1000));
        let source=&u.source;let full_start=u.start.min(u.owner_start);
        let full_cost=source[full_start-1..u.owner_end].iter().enumerate().map(|(n,s)|s.len()+format!("{}: ",full_start+n).len()+1).sum::<usize>()+180;
        // An execution body fitting half the total budget is more useful
        // intact than reserving its last branch for unrelated alternatives.
        let cap=if execution_callers.contains(&identity(u))&&full_cost<=q.hard/2&&full_cost<=available {available}else{cap};
        let (start,mut end)=if full_cost<=cap&&u.owner_end-full_start+1<=lines_left{(full_start,u.owner_end)}else{(u.start,u.end)};
        while end>=start&&(source[start-1..end].iter().enumerate().map(|(n,s)|s.len()+format!("{}: ",start+n).len()+1).sum::<usize>()+180>cap||end-start+1>lines_left){end-=1;}
        if end<start{result.gaps.push(serde_json::json!({"file":u.file,"start":u.owner_start,"end":u.owner_end,"reason":"working-set-budget"}));continue;}
        let complete=start<=u.owner_start&&end>=u.owner_end&&(u.role!="source-text"||(start==1&&end==u.source.len()));
        let snippet=source[start-1..end].iter().enumerate().map(|(n,s)|format!("{}: {}\n",start+n,s)).collect::<String>();
        result.body.push_str(&format!("\n### {}:{}-{} [{} {}]\ncoverage: {} {}:{}-{}\n```\n{snippet}```\n",u.file,start,end,relation,u.name,if u.role=="source-text"{"SOURCE_RANGE"}else if complete{"BODY"}else{"PARTIAL"},u.file,start,end));
        lines_left-=end-start+1;ranges.entry(u.file.clone()).or_default().push((start,end));
        result.evidence.push(serde_json::json!({"file":u.file,"symbol":u.name,"start":start,"end":end,"role":u.role,"relation":relation,"sourceHash":u.file_hash,"complete":complete}));
        if !complete{result.gaps.push(serde_json::json!({"file":u.file,"start":u.owner_start,"end":u.owner_end,"reason":if u.role=="source-text"{"source-range-only"}else{"large-unit"}}));}
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

#[cfg(test)] mod envelope_tests {
    use super::*;
    #[test] fn many_dependencies_never_turn_a_small_budget_into_an_error() {
        let d=tempfile::tempdir().unwrap();let mut text=String::from("// 取消任务\nfn cancel_job() {\n");
        for i in 0..12 {text.push_str(&format!(" helper_{i}();\n"));}text.push_str("}\n");
        for i in 0..12 {text.push_str(&format!("fn helper_{i}() {{ let payload = \"{}\"; }}\n","x".repeat(600)));}
        fs::write(d.path().join("jobs.rs"),text).unwrap();
        let out=polaris(d.path(),serde_json::json!({"task":"取消任务","maxBytes":8192})).unwrap();
        assert!(out.len()<=8192);assert!(out.contains("fn cancel_job"));
    }
}

#[cfg(test)] mod transitive_context_tests {
    use super::*;
    #[test]fn constant_referenced_only_by_helper_is_included_without_a_second_read(){
        let d=tempfile::tempdir().unwrap();
        fs::write(d.path().join("work.rs"),"const RETRY_CAP: u8 = 3;\nstruct Attempt { used: u8 }\n// 请求重试\nfn retry_request(a: Attempt) -> bool { within_limit(a.used) }\nfn within_limit(used: u8) -> bool { used < RETRY_CAP }\n").unwrap();
        let out=polaris(d.path(),serde_json::json!({"task":"请求重试","maxBytes":12000})).unwrap();
        for code in ["fn retry_request", "fn within_limit", "struct Attempt", "const RETRY_CAP"]{assert!(out.contains(code),"{code}: {out}");}
    }
}

#[cfg(test)] mod source_edge_tests {
    use super::*;
    #[test]fn jsx_component_edges_reach_three_hop_implementation(){
        let d=tempfile::tempdir().unwrap();
        fs::write(d.path().join("panel.tsx"),"import { Bridge } from './bridge';\n// 收起命令行面板再打开时保留进程\nexport function Panel() { return <Bridge/>; }\n").unwrap();
        fs::write(d.path().join("bridge.tsx"),"import { Leaf } from './leaf';\nexport function Bridge() { return <Leaf/>; }\n").unwrap();
        fs::write(d.path().join("leaf.tsx"),"import { keepProcess } from './process';\nexport function Leaf() { keepProcess(); return <div/>; }\n").unwrap();
        fs::write(d.path().join("process.ts"),"let active: unknown;\nexport function keepProcess() { if (!active) active = launch(); return active; }\n").unwrap();
        let out=polaris(d.path(),serde_json::json!({"task":"收起命令行面板再打开时保留进程","maxBytes":12000})).unwrap();
        for code in ["function Panel", "function Bridge", "function Leaf", "function keepProcess"]{assert!(out.contains(code),"{code}: {out}");}
        assert!(out.len()<=12000);
    }
    #[test]fn strings_comments_and_native_tags_are_not_component_calls(){
        let d=tempfile::tempdir().unwrap();
        fs::write(d.path().join("panel.tsx"),"function Panel() { const text = '<Imaginary/>'; /* <Ghost/> */ return <Real/><div/>; }\nfunction Real() { return <span/>; }\n").unwrap();
        let corpus=index::corpus(d.path(),Instant::now()+Duration::from_secs(3)).unwrap();
        let u=corpus.units.iter().find(|u|u.name=="Panel").unwrap();
        assert!(u.calls.contains("Real"));
        for name in ["Imaginary","Ghost","div"]{assert!(!u.calls.contains(name),"{name}");}
    }
}

#[cfg(test)] mod connected_execution_tests {
    use super::*;
    #[test] fn a_long_execution_caller_is_not_replaced_by_an_unrelated_short_match() {
        let d=tempfile::tempdir().unwrap();
        let mut source=String::from("// 解析输入协议\nfn parse_message() -> bool { true }\n// 发送输入并释放资源\nfn execute_message() -> bool {\n let parsed = parse_message();\n");
        for n in 0..75 { source.push_str(&format!(" let value_{n} = {n};\n")); }
        source.push_str(" release_resource();\n parsed\n}\nfn release_resource() {}\n");
        fs::write(d.path().join("driver.rs"),source).unwrap();
        let out=polaris(d.path(),serde_json::json!({"task":"解析输入协议后发送，释放资源","keywords":["parse_message"],"maxBytes":12000})).unwrap();
        assert!(out.contains("fn parse_message"),"{out}");
        assert!(out.contains("fn execute_message"),"{out}");
        assert!(out.contains("release_resource();"),"{out}");
        assert!(out.len()<=12000);
    }
}
