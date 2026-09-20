"""Use source comments plus distinct natural-language intent coverage when selecting declarations.
The repository/file discovery already found the correct files in the remaining failures.
This fixes declaration-beam ranking without labels, repo-specific names, models, or Reasonix.
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
    assert s.count(a)==1,a[:180]
    return s.replace(a,b)

p,s=load("polaris_demand_v2.rs","82626c7fa9538fd54011cb08b5163a15d92f07fb")
intro='''fn select_spans(rows:&[Candidate],ids:&[usize],cache:&DemandCache,q:&query::Query,
    matcher:&regex::RegexSet,terms:&[(String,f64)],idf:&[f64],names:&HashSet<String>,caller_names:&HashSet<(String,String)>,cap:usize)->Vec<(usize,usize)>{
    let mut ranked=Vec::new();'''
intro_new='''fn select_spans(rows:&[Candidate],ids:&[usize],cache:&DemandCache,q:&query::Query,
    matcher:&regex::RegexSet,terms:&[(String,f64)],idf:&[f64],names:&HashSet<String>,caller_names:&HashSet<(String,String)>,cap:usize)->Vec<(usize,usize)>{
    let facets=q.facets();
    let facet_weights=facets.iter().map(|group|group.split('|').filter_map(|word|
        q.terms.iter().find(|(term,_)|term==word).map(|(_,weight)|*weight)
    ).fold(1.0_f64,f64::max)).collect::<Vec<_>>();
    let mut ranked=Vec::new();'''
s=sub(s,intro,intro_new)
old=r'''            let first=s.ln.saturating_sub(1);let end=s.end.min(file.source.len());if first>=end{continue;}
            let body=file.source[first..end.min(first+320)].join("\n");
            let header=file.source[first.saturating_sub(4)..(first+4).min(end)].join("\n");
            let matched=matcher.matches(&body);let head=matcher.matches(&header);
            let mut score=0.0;
            for (i,(term,weight)) in terms.iter().enumerate(){
                if file.names[&s.ln].contains(term){score+=weight*idf[i]*6.0;}
                if head.matched(i){score+=weight*idf[i]*2.0;}
                if matched.matched(i){score+=weight*idf[i];}
            }'''
new=r'''            let first=s.ln.saturating_sub(1);let end=s.end.min(file.source.len());if first>=end{continue;}
            // Selection must see the same leading intent comments later emitted
            // in the CodeUnit; otherwise the right file can be parsed while the
            // neighboring generic declaration is the only one materialized.
            let mut context=first;
            while context>0&&first-context<8 {
                let line=file.source[context-1].trim();
                if line.starts_with("//")||line.starts_with("/*")||line.starts_with('*')
                    ||line.starts_with('#')||line.is_empty(){context-=1;}else{break;}
            }
            let body=file.source[first..end.min(first+320)].join("\n");
            let leading=file.source[context..first].join("\n");
            let header=file.source[context..(first+4).min(end)].join("\n");
            let matched=matcher.matches(&body);let lead=matcher.matches(&leading);let head=matcher.matches(&header);
            let mut score=0.0;
            for (i,(term,weight)) in terms.iter().enumerate(){
                if file.names[&s.ln].contains(term){score+=weight*idf[i]*6.0;}
                if lead.matched(i){score+=weight*idf[i]*3.0;}
                else if head.matched(i){score+=weight*idf[i]*2.0;}
                if matched.matched(i){score+=weight*idf[i];}
            }
            // Repetition is not evidence of broader intent. Reward independent
            // requested facets covered by the symbol/comment/body. This remains
            // a multiplier on actual lexical evidence, never a fabricated hit.
            let mut surface=file.names[&s.ln].clone();
            surface.extend(query::tokens(&leading));
            surface.extend(query::tokens(&body));
            let covered=facets.iter().enumerate().filter_map(|(facet,group)|
                group.split('|').any(|word|surface.contains(word)).then_some(facet)
            ).collect::<Vec<_>>();
            if score>0.0&&!covered.is_empty(){
                let distinct=covered.iter().map(|facet|facet_weights[*facet]).sum::<f64>();
                score*=1.0+distinct.min(10.0)*0.30+(covered.len().min(7) as f64)*0.12;
            }'''
s=sub(s,old,new)
# facets are now declared once at function entry.
s=sub(s,'''    // Keep source-connected execution callers before file diversity consumes
    // the bounded declaration budget. No additional global index is built.
    let facets=q.facets();
    let mut seeds=caller_names.iter().collect::<Vec<_>>();''','''    // Keep source-connected execution callers before file diversity consumes
    // the bounded declaration budget. No additional global index is built.
    let mut seeds=caller_names.iter().collect::<Vec<_>>();''')
anchor='''#[cfg(test)]
mod demand_tests {'''
tests=r'''#[cfg(test)] mod intent_coverage_selection_tests {
    use super::*;
    #[test] fn leading_comment_and_multi_goal_coverage_select_the_state_behavior() {
        let d=tempfile::tempdir().unwrap();
        for n in 0..48 {fs::write(d.path().join(format!("noise_{n}.ts")),
            format!("// terminal session\nexport function createTerminal{n}() {{ startTerminal(); }}\n")).unwrap();}
        fs::write(d.path().join("state.ts"),
            "export function open(){ return true; }\n/** Home terminal never inherits the saved open state on a new visit. */\nexport function freshHomeTerminalState(){ setOpened(false); }\n").unwrap();
        let q=query::Query::parse(serde_json::json!({"task":"进入新页面后，终端不会继承上次展开状态","maxBytes":12000})).unwrap();
        let (c,_)=demand_corpus(d.path(),&q,Instant::now()+Duration::from_secs(3)).unwrap();
        assert!(c.units.iter().any(|u|u.name=="freshHomeTerminalState"),"{:?}",c.units.iter().map(|u|u.name.clone()).collect::<Vec<_>>());
    }
    #[test] fn independent_hide_reuse_process_restart_facets_select_existing_instance_behavior() {
        let d=tempfile::tempdir().unwrap();
        for n in 0..48 {fs::write(d.path().join(format!("panel_{n}.ts")),
            format!("// terminal panel\nexport function terminalPanel{n}() {{ showTerminal(); }}\n")).unwrap();}
        fs::write(d.path().join("session.ts"),
            "export function createTerminal(){ startShell(); }\n/** Hiding the panel remounts the same shell process and never restarts it. */\nexport function attachExistingTerminal(){ if (!ready) mountHost(); }\n").unwrap();
        let q=query::Query::parse(serde_json::json!({"task":"收起面板再打开时复用原来的命令行进程，不重新启动","maxBytes":12000})).unwrap();
        let (c,_)=demand_corpus(d.path(),&q,Instant::now()+Duration::from_secs(3)).unwrap();
        assert!(c.units.iter().any(|u|u.name=="attachExistingTerminal"),"{:?}",c.units.iter().map(|u|u.name.clone()).collect::<Vec<_>>());
    }
}

'''
s=sub(s,anchor,tests+anchor)
p.write_text(s,encoding="utf-8")
print("Declaration selection now rewards distinct natural-language intent facets using real source evidence.")
