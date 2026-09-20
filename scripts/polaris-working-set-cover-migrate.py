"""Select each parsed file's representative by distinct query-facet coverage.
This is the lean version: reuse the existing header/body RegexSet matches and a u64 facet mask.
No extra source tokenization, wider comment scan, parse/read, model call, labels, or Reasonix changes.
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
    let mut term_facets=vec![0u64;terms.len()];
    let mut name_facets=HashMap::<String,u64>::new();
    for (facet,group) in facets.iter().take(64).enumerate() {
        let bit=1u64<<facet;
        for (i,(term,_)) in terms.iter().enumerate() {
            if group.split('|').any(|word|word==term) {
                term_facets[i]|=bit;name_facets.entry(term.clone()).and_modify(|mask|*mask|=bit).or_insert(bit);
            }
        }
    }
    // ranked = file, declaration line, lexical score, distinct-facet count.
    let mut ranked=Vec::<(usize,usize,f64,u32)>::new();'''
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
            }
            score*=q.focus.score(std::iter::once(row.file.as_str()).chain(file.source.iter().take(5).map(String::as_str)).chain(std::iter::once(body.as_str())));
            if names.contains(&s.name){score+=10000.0;}
            if q.anchors.iter().any(|a|a.eq_ignore_ascii_case(&s.name)){score+=20000.0;}
            if caller_names.iter().any(|(file,name)|row.file==*file&&body.contains(name)){score+=3000.0*q.focus.score(std::iter::once(row.file.as_str()).chain(file.source.iter().take(5).map(String::as_str)).chain(std::iter::once(body.as_str())));}
            if score>0.0 {ranked.push((id,s.ln,score/(1.0+0.04*((end-first) as f64/80.0).ln_1p())));}'''
new=r'''            let first=s.ln.saturating_sub(1);let end=s.end.min(file.source.len());if first>=end{continue;}
            let body=file.source[first..end.min(first+320)].join("\n");
            let header=file.source[first.saturating_sub(4)..(first+4).min(end)].join("\n");
            let matched=matcher.matches(&body);let head=matcher.matches(&header);
            let mut score=0.0;let mut facet_mask=0u64;
            for (i,(term,weight)) in terms.iter().enumerate(){
                if file.names[&s.ln].contains(term){score+=weight*idf[i]*6.0;facet_mask|=term_facets[i];}
                if head.matched(i){score+=weight*idf[i]*2.0;facet_mask|=term_facets[i];}
                if matched.matched(i){score+=weight*idf[i];facet_mask|=term_facets[i];}
            }
            // Identifier aliases can include concept words not present in the
            // matcher after term caps; fold those into the same bounded mask.
            for term in &file.names[&s.ln] {if let Some(mask)=name_facets.get(term){facet_mask|=*mask;}}
            score*=q.focus.score(std::iter::once(row.file.as_str()).chain(file.source.iter().take(5).map(String::as_str)).chain(std::iter::once(body.as_str())));
            if names.contains(&s.name){score+=10000.0;}
            if q.anchors.iter().any(|a|a.eq_ignore_ascii_case(&s.name)){score+=20000.0;}
            if caller_names.iter().any(|(file,name)|row.file==*file&&body.contains(name)){score+=3000.0*q.focus.score(std::iter::once(row.file.as_str()).chain(file.source.iter().take(5).map(String::as_str)).chain(std::iter::once(body.as_str())));}
            if score>0.0 {ranked.push((id,s.ln,score/(1.0+0.04*((end-first) as f64/80.0).ln_1p()),facet_mask.count_ones()));}'''
s=sub(s,old,new)
s=sub(s,'''        if file.names.is_empty()&&!file.source.is_empty(){ranked.push((id,1,row.score));}''','''        if file.names.is_empty()&&!file.source.is_empty(){ranked.push((id,1,row.score,0));}''')
old_sel='''    for &(id,line,_) in &ranked {if seen.insert(id){selected.push((id,line));if selected.len()>=cap{break;}}}
    for &(id,line,_) in &ranked {if selected.len()>=cap{break;}if !selected.contains(&(id,line)){selected.push((id,line));}}
    selected'''
new_sel='''    // Keep the same one-representative-per-file breadth. Only choose that
    // representative by independent intent coverage; ties use lexical score.
    let mut champions=HashMap::<usize,(usize,f64,u32)>::new();
    for &(id,line,score,covered) in &ranked {
        let replace=champions.get(&id).is_none_or(|(_,old_score,old_covered)|
            covered>*old_covered||(covered==*old_covered&&score>*old_score));
        if replace{champions.insert(id,(line,score,covered));}
    }
    // Preserve original file priority by the best lexical score in each file;
    // coverage chooses the declaration, not which file jumps the queue.
    let mut champions=champions.into_iter().map(|(id,(line,score,covered))|(id,line,score,covered)).collect::<Vec<_>>();
    champions.sort_by(|a,b|b.2.total_cmp(&a.2).then(rows[a.0].file.cmp(&rows[b.0].file)));
    for (id,line,_,_) in champions {
        if selected.len()>=cap{break;}
        if seen.insert(id)&&!selected.contains(&(id,line)){selected.push((id,line));}
    }
    for &(id,line,_,_) in &ranked {if selected.len()>=cap{break;}if !selected.contains(&(id,line)){selected.push((id,line));}}
    selected'''
s=sub(s,old_sel,new_sel)
s=s.replace('for &(id,line,_) in &ranked {','for &(id,line,_,_) in &ranked {')
anchor='''#[cfg(test)]
mod demand_tests {'''
tests=r'''#[cfg(test)] mod per_file_intent_champion_tests {
    use super::*;
    #[test] fn representative_prefers_state_inherit_behavior_over_generic_open() {
        let d=tempfile::tempdir().unwrap();
        fs::write(d.path().join("layout.ts"),
            "export function open(){ return openPanel(); }\n/** Home terminal never inherits the saved open state on a new visit. */\nexport function freshHomeTerminalState(){ setOpened(false); }\n").unwrap();
        for n in 0..40 {fs::write(d.path().join(format!("noise_{n}.ts")),
            format!("export function createSession{n}(){{ openTerminal(); }}\n")).unwrap();}
        let q=query::Query::parse(serde_json::json!({"task":"进入新页面后终端不会继承上次展开状态","maxBytes":12000})).unwrap();
        let (c,_)=demand_corpus(d.path(),&q,Instant::now()+Duration::from_secs(3)).unwrap();
        assert!(c.units.iter().any(|u|u.name=="freshHomeTerminalState"),"{:?}",c.units.iter().map(|u|u.name.clone()).collect::<Vec<_>>());
    }
    #[test] fn representative_prefers_hide_reuse_restart_behavior_over_creation() {
        let d=tempfile::tempdir().unwrap();
        fs::write(d.path().join("session.ts"),
            "export function createTerminal(){ return startShell(); }\n/** Hiding the panel remounts the same shell process and never restarts it. */\nexport function attachExistingTerminal(){ if (!ready) mountHost(); }\n").unwrap();
        for n in 0..40 {fs::write(d.path().join(format!("noise_{n}.ts")),
            format!("export function terminalPanel{n}(){{ showTerminal(); }}\n")).unwrap();}
        let q=query::Query::parse(serde_json::json!({"task":"收起面板再打开时复用原来的命令行进程而不是重新启动","maxBytes":12000})).unwrap();
        let (c,_)=demand_corpus(d.path(),&q,Instant::now()+Duration::from_secs(3)).unwrap();
        assert!(c.units.iter().any(|u|u.name=="attachExistingTerminal"),"{:?}",c.units.iter().map(|u|u.name.clone()).collect::<Vec<_>>());
    }
}

'''
s=sub(s,anchor,tests+anchor)
p.write_text(s,encoding="utf-8")
print("Lean per-file intent champion uses existing RegexSet matches and a facet bitmask.")
