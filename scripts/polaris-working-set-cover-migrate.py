"""Budgeted working-set coverage over the accepted query-first source.
No repository labels, file names, expected symbols, model calls, or Reasonix changes.
"""
from pathlib import Path
import hashlib
ROOT=Path("src-tauri/src/nova_tools_native")
def load(name,expected):
    p=ROOT/name;b=p.read_bytes()
    got=hashlib.sha1(b"blob "+str(len(b)).encode()+b"\0"+b).hexdigest()
    assert got==expected,(name,got)
    return p,b.decode("utf-8")
def sub(s,a,b):
    assert s.count(a)==1,a[:120]
    return s.replace(a,b)

# Stage 1: do not spend all parse slots on files that repeat the same query facet.
p,s=load("polaris_demand_v2.rs","82626c7fa9538fd54011cb08b5163a15d92f07fb")
needle='''    let mut parsed=HashSet::new();let initial=order.iter().copied().take(INITIAL_FILES).collect::<Vec<_>>();
    partial|=declarations(&rows,&initial,&mut cache,&mut parsed,&mut stats,deadline);'''
replacement='''    let mut parsed=HashSet::new();
    // Keep the strongest lexical guesses, then use the remaining cold-parse
    // budget to cover query facets not represented by those files. This is
    // weighted set cover over evidence already collected during discovery;
    // it performs no second walk/read and has no repository-specific labels.
    let facets=q.facets();
    let facet_weights=facets.iter().map(|group|group.split('|').filter_map(|word|
        q.terms.iter().find(|(term,_)|term==word).map(|(_,weight)|*weight)
    ).fold(1.0_f64,f64::max)).collect::<Vec<_>>();
    let facet_terms=facets.iter().map(|group|terms.iter().enumerate().filter_map(|(i,(term,_))|
        group.split('|').any(|word|word==term).then_some(i)
    ).collect::<HashSet<_>>()).collect::<Vec<_>>();
    let file_coverage=rows.iter().map(|row|facet_terms.iter().enumerate().filter_map(|(facet,indexes)|
        row.hits.iter().any(|hit|indexes.contains(hit)).then_some(facet)
    ).collect::<HashSet<_>>()).collect::<Vec<_>>();
    let mut initial=Vec::new();let mut chosen=HashSet::new();let mut covered=HashSet::new();
    for &id in order.iter().take(2) {
        if chosen.insert(id){initial.push(id);covered.extend(file_coverage[id].iter().copied());}
    }
    while initial.len()<INITIAL_FILES {
        let best=order.iter().take(192).copied().filter(|id|!chosen.contains(id)).max_by(|a,b|{
            let value=|id:usize| {
                let gain=file_coverage[id].iter().filter(|facet|!covered.contains(facet))
                    .map(|facet|facet_weights[*facet]).sum::<f64>();
                (gain,rows[id].score/(1.0+(rows[id].text.len() as f64/12000.0).ln_1p()))
            };
            let va=value(*a);let vb=value(*b);
            va.0.total_cmp(&vb.0).then(va.1.total_cmp(&vb.1)).then_with(||rows[*b].file.cmp(&rows[*a].file))
        });
        let Some(id)=best else{break;};
        let gain=file_coverage[id].iter().any(|facet|!covered.contains(facet));
        if !gain{break;}
        chosen.insert(id);initial.push(id);covered.extend(file_coverage[id].iter().copied());
    }
    for &id in &order {if initial.len()>=INITIAL_FILES{break;}if chosen.insert(id){initial.push(id);}}
    partial|=declarations(&rows,&initial,&mut cache,&mut parsed,&mut stats,deadline);'''
s=sub(s,needle,replacement)
p.write_text(s,encoding="utf-8")

# Stage 2: returned primary roots are also a working set, not four redundant top hits.
p,s=load("polaris_packet.rs","6bb83220c054df5ec332d09da524a4fe1dc2a628")
old='''    let scores=ranked.iter().copied().collect::<HashMap<_,_>>();
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
    let gains=|from:usize,to:usize|coverage[&identity(&units[to])].difference(&coverage[&identity(&units[from])]).count();'''
new='''    let scores=ranked.iter().copied().collect::<HashMap<_,_>>();
    // One representative per declaration, not every overlapping slice.
    let mut unique=Vec::new();let mut seen=HashSet::new();
    for &(i,_) in ranked{if seen.insert(identity(&units[i])){unique.push(i);}}
    for (i,u) in units.iter().enumerate(){if seen.insert(identity(u)){unique.push(i);}}
    // Score the complete owner once. A parser's name may match strongly while
    // the execution/cleanup branch is in a longer caller and a different slice.
    let facets=q.facets();
    let facet_weights=facets.iter().map(|group|group.split('|').filter_map(|word|
        q.terms.iter().find(|(term,_)|term==word).map(|(_,weight)|*weight)
    ).fold(1.0_f64,f64::max)).collect::<Vec<_>>();
    let coverage=unique.iter().map(|&id| {
        let u=&units[id];
        let mut words=query::tokens(&u.source[u.owner_start-1..u.owner_end].join("\n")).into_iter().collect::<HashSet<_>>();
        words.extend(u.name_terms.iter().cloned());
        let mask=facets.iter().enumerate().filter_map(|(n,g)|g.split('|').any(|w|words.contains(w)).then_some(n)).collect::<HashSet<_>>();
        (identity(u),mask)
    }).collect::<HashMap<_,_>>();
    let gains=|from:usize,to:usize|coverage[&identity(&units[to])].difference(&coverage[&identity(&units[from])]).count();

    // Preserve the strongest root, then spend the remaining three root slots
    // on marginal user-goal coverage before redundant high-rank alternatives.
    // This changes packet composition only; ranking and source evidence remain.
    let mut roots=Vec::new();let mut owners=HashSet::new();let mut per_file=HashMap::<String,usize>::new();let mut root_covered=HashSet::new();
    if let Some(&(first,_))=ranked.iter().find(|(id,_)|owners.insert(identity(&units[*id]))) {
        roots.push(first);*per_file.entry(units[first].file.clone()).or_default()+=1;
        root_covered.extend(coverage[&identity(&units[first])].iter().copied());
    }
    while roots.len()<4 {
        let best=unique.iter().take(40).copied().filter(|id|!owners.contains(&identity(&units[*id]))
            &&*per_file.get(&units[*id].file).unwrap_or(&0)<2).max_by(|a,b|{
            let value=|id:usize| {
                let gain=coverage[&identity(&units[id])].iter().filter(|facet|!root_covered.contains(facet))
                    .map(|facet|facet_weights[*facet]).sum::<f64>();
                let relevance=*scores.get(&id).unwrap_or(&0.0);
                (gain,relevance/(1.0+(bytes(&units[id]) as f64/2400.0).ln_1p()))
            };
            let va=value(*a);let vb=value(*b);
            va.0.total_cmp(&vb.0).then(va.1.total_cmp(&vb.1)).then_with(||b.cmp(a))
        });
        let Some(id)=best else{break;};
        let adds=coverage[&identity(&units[id])].iter().any(|facet|!root_covered.contains(facet));
        if !adds{break;}
        owners.insert(identity(&units[id]));roots.push(id);*per_file.entry(units[id].file.clone()).or_default()+=1;
        root_covered.extend(coverage[&identity(&units[id])].iter().copied());
    }
    for &(id,_) in ranked {
        if roots.len()>=4{break;}let u=&units[id];
        if *per_file.get(&u.file).unwrap_or(&0)>=2||!owners.insert(identity(u)){continue;}
        roots.push(id);*per_file.entry(u.file.clone()).or_default()+=1;
    }'''
s=sub(s,old,new)
# Add generic tests: many repeated generic matches must not crowd out a file/function
# that covers the other stated parts of a natural-language request.
anchor='''#[cfg(test)]
mod packet_tests{'''
tests='''#[cfg(test)] mod marginal_coverage_tests {
    use super::*;
    #[test] fn cold_file_budget_keeps_a_multi_facet_behavior_among_generic_noise() {
        let d=tempfile::tempdir().unwrap();
        for n in 0..70 {fs::write(d.path().join(format!("session_{n}.rs")),
            format!("// 新建会话终端\\nfn create_session_{n}() {{ start_terminal(); }}\\n")).unwrap();}
        fs::write(d.path().join("layout.rs"),
            "// 新建会话页面的终端不继承上次展开状态\\nfn home_state() { set_opened(false); }\\n").unwrap();
        let out=polaris(d.path(),serde_json::json!({"task":"进入新建会话页面后，终端为什么不会继承上次展开状态？","maxBytes":12000})).unwrap();
        assert!(out.contains("fn home_state"),"{out}");
    }
    #[test] fn packet_roots_choose_reuse_behavior_over_redundant_terminal_mentions() {
        let d=tempfile::tempdir().unwrap();
        for n in 0..10 {fs::write(d.path().join(format!("panel_{n}.ts")),
            format!("// 终端侧边栏\\nexport function panel{n}() {{ showTerminal(); }}\\n")).unwrap();}
        fs::write(d.path().join("sessions.ts"),
            "// 收起侧边栏再打开时复用原来的命令行进程，不重新启动\\nexport function remountTerminal(tab:any,parent:any){ parent.append(tab.host); if(!tab.ready) tab.ready=createShell(); }\\n").unwrap();
        let out=polaris(d.path(),serde_json::json!({"task":"收起侧边栏再打开时，如何复用原来的命令行进程而不是重新启动？","maxBytes":12000})).unwrap();
        assert!(out.contains("function remountTerminal"),"{out}");
    }
}

'''
s=sub(s,anchor,tests+anchor)
p.write_text(s,encoding="utf-8")
print("Applied generic marginal-goal coverage to cold file selection and final working-set roots.")
