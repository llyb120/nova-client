"""Complete the validated one-shot candidate with source-linked caller coverage.
No task names, repository paths or expected answers are used by the algorithm.
Apply after the existing one-shot migrations; refuse an unexpected source shape.
"""
from pathlib import Path
p=Path('src-tauri/src/nova_tools_native/polaris_packet.rs')
s=p.read_text()
def change(old,new):
    global s
    assert s.count(old)==1,old[:100]
    s=s.replace(old,new)
change('    let mut verified=HashMap::<String,bool>::new();let mut emitted=HashSet::new();', '''    // Score the complete owner once. A parser's name may match strongly while
    // the execution/cleanup branch is in a longer caller and a different slice.
    let facets=q.facets();
    let coverage=unique.iter().map(|&id| {
        let u=&units[id];
        let mut words=query::tokens(&u.source[u.owner_start-1..u.owner_end].join("\\n")).into_iter().collect::<HashSet<_>>();
        words.extend(u.name_terms.iter().cloned());
        let mask=facets.iter().enumerate().filter_map(|(n,g)|g.split('|').any(|w|words.contains(w)).then_some(n)).collect::<HashSet<_>>();
        (identity(u),mask)
    }).collect::<HashMap<_,_>>();
    let gains=|from:usize,to:usize|coverage[&identity(&units[to])].difference(&coverage[&identity(&units[from])]).count();
    let mut execution_callers=HashSet::new();
    let mut verified=HashMap::<String,bool>::new();let mut emitted=HashSet::new();''')
change('                    if role=="caller-reference"&&bytes(u)>q.hard/4&&(!q.focus.active()||focus_source(q,u)<0.95){continue;}\n                    let priority=if role=="caller-reference"&&q.focus.active()&&focus_source(q,u)>=0.95{4}else{match role{"callee-reference"|"command-reference"=>3,"type-reference"=>2,_=>1}};\n                    neighbours.push((i,role,priority,*scores.get(&i).unwrap_or(&0.0),bytes(u)));', '''                    let gain=gains(parent,i);
                    if role=="caller-reference"&&bytes(u)>q.hard/4&&gain==0{continue;}
                    let priority=match role{"callee-reference"|"command-reference"=>3,"type-reference"=>2,_=>1};
                    let relevance=*scores.get(&i).unwrap_or(&0.0)+if role=="caller-reference"{gain as f64*2.0}else{0.0};
                    neighbours.push((i,role,priority,relevance,bytes(u)));''')
change('            for (i,role,_,_,_) in neighbours.into_iter().take(if depth==0{4}else{2}){', '''            // At most one extra execution caller for each leading seed. It must
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
            for (i,role,_,_,_) in neighbours.into_iter().filter(|(_,role,_,_,_)|*role!="caller-reference").take(if depth==0{4}else{2}){''')
old='''    ordered.sort_by_key(|(id,role)|{
        if let Some(&n)=root_rank.get(&identity(&units[*id])){return if n>=2&&q.focus.active()&&focus_source(q,&units[*id])<0.8{10+n}else{n};}
        if *role=="caller-reference"&&q.focus.active()&&focus_source(q,&units[*id])>=0.95{4}
        else if matches!(*role,"callee-reference"|"command-reference"){6}
        else if *role=="type-reference"{7}else{20}
    });'''
new='''    ordered.sort_by_key(|(id,role)|{
        if let Some(&n)=root_rank.get(&identity(&units[*id])){return if n<2{n}else{6+n};}
        if execution_callers.contains(&identity(&units[*id])){2}
        else if matches!(*role,"callee-reference"|"command-reference"){3}
        else if *role=="type-reference"{4}else{20}
    });'''
change(old,new)
s+='''
#[cfg(test)] mod connected_execution_tests {
    use super::*;
    #[test] fn a_long_execution_caller_is_not_replaced_by_an_unrelated_short_match() {
        let d=tempfile::tempdir().unwrap();
        let mut source=String::from("// 解析输入协议\\nfn parse_message() -> bool { true }\\n// 发送输入并释放资源\\nfn execute_message() -> bool {\\n let parsed = parse_message();\\n");
        for n in 0..75 { source.push_str(&format!(" let value_{n} = {n};\\n")); }
        source.push_str(" release_resource();\\n parsed\\n}\\nfn release_resource() {}\\n");
        fs::write(d.path().join("driver.rs"),source).unwrap();
        let out=polaris(d.path(),serde_json::json!({"task":"解析输入协议后发送，释放资源","keywords":["parse_message"],"maxBytes":12000})).unwrap();
        assert!(out.contains("fn parse_message"),"{out}");
        assert!(out.contains("fn execute_message"),"{out}");
        assert!(out.contains("release_resource();"),"{out}");
        assert!(out.len()<=12000);
    }
}
'''
p.write_text(s)
print('Prioritized source-connected execution coverage and bounded required helpers; budgets and labels unchanged.')
