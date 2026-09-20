"""Choose each parsed file's representative declaration by distinct query-facet coverage.
The file walk and parse budget are unchanged. One declaration per parsed file is still selected
before global fill; only WHICH declaration represents that file changes. No labels/models/Reasonix.
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
    let facet_terms=facets.iter().map(|group|terms.iter().enumerate().filter_map(|(i,(term,_))|
        group.split('|').any(|word|word==term).then_some(i)
    ).collect::<HashSet<_>>()).collect::<Vec<_>>();
    // ranked = file, declaration line, lexical score, distinct-facet count.
    let mut ranked=Vec::<(usize,usize,f64,usize)>::new();'''
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
            // Include the same bounded leading comment evidence that final
            // CodeUnits expose, without tokenizing every declaration again.
            let mut context=first;
            while context>0&&first-context<8 {
                let line=file.source[context-1].trim();
                if line.starts_with("//")||line.starts_with("/*")||line.starts_with('*')
                    ||line.starts_with('#')||line.is_empty(){context-=1;}else{break;}
            }
            let body=file.source[first..end.min(first+320)].join("\n");
            let header=file.source[context..(first+4).min(end)].join("\n");
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
            if score>0.0 {
                let covered=facet_terms.iter().filter(|indexes|indexes.iter().any(|i|
                    file.names[&s.ln].contains(&terms[*i].0)||head.matched(*i)||matched.matched(*i)
                )).count();
                ranked.push((id,s.ln,score/(1.0+0.04*((end-first) as f64/80.0).ln_1p()),covered));
            }'''
s=sub(s,old,new)
s=sub(s,'''        if file.names.is_empty()&&!file.source.is_empty(){ranked.push((id,1,row.score));}''','''        if file.names.is_empty()&&!file.source.is_empty(){ranked.push((id,1,row.score,0));}''')
s=sub(s,'''    ranked.sort_by(|a,b|b.2.total_cmp(&a.2).then(rows[a.0].file.cmp(&rows[b.0].file)).then(a.1.cmp(&b.1)));''','''    ranked.sort_by(|a,b|b.2.total_cmp(&a.2).then(rows[a.0].file.cmp(&rows[b.0].file)).then(a.1.cmp(&b.1)));''')
# Adapt callers' relevance tuple lookup.
s=sub(s,'''            let relevance=ranked.iter().find(|r|r.0==id&&r.1==symbol.ln).map(|r|r.2).unwrap_or(0.0);''','''            let relevance=ranked.iter().find(|r|r.0==id&&r.1==symbol.ln).map(|r|r.2).unwrap_or(0.0);''')
# Replace diversity loop: one coverage champion per file, then global score fill.
old_sel='''    for &(id,line,_) in &ranked {if seen.insert(id){selected.push((id,line));if selected.len()>=cap{break;}}}
    for &(id,line,_) in &ranked {if selected.len()>=cap{break;}if !selected.contains(&(id,line)){selected.push((id,line));}}
    selected'''
new_sel='''    // Preserve one representative per parsed file, but choose the declaration
    // covering the most independent query facets. Ties keep original lexical
    // score ordering. This costs no extra parse/materialization slots.
    let mut champions=HashMap::<usize,(usize,f64,usize)>::new();
    for &(id,line,score,covered) in &ranked {
        let replace=champions.get(&id).is_none_or(|(_,old_score,old_covered)|
            covered>*old_covered||(covered==*old_covered&&score>*old_score));
        if replace{champions.insert(id,(line,score,covered));}
    }
    let mut champions=champions.into_iter().map(|(id,(line,score,covered))|(id,line,score,covered)).collect::<Vec<_>>();
    champions.sort_by(|a,b|b.3.cmp(&a.3).then(b.2.total_cmp(&a.2)).then(rows[a.0].file.cmp(&rows[b.0].file)));
    for (id,line,_,_) in champions {
        if selected.len()>=cap{break;}
        if seen.insert(id)&&!selected.contains(&(id,line)){selected.push((id,line));}
    }
    for &(id,line,_,_) in &ranked {if selected.len()>=cap{break;}if !selected.contains(&(id,line)){selected.push((id,line));}}
    selected'''
s=sub(s,old_sel,new_sel)
# Existing loops destructure ranked tuples elsewhere.
s=s.replace('for &(id,line,_) in &ranked {','for &(id,line,_,_) in &ranked {')
# Generic regression: the desired declaration is not the highest raw-score member
# of its file, but covers more of the user's independent intent.
anchor='''#[cfg(test)]
mod demand_tests {'''
tests=r'''#[cfg(test)] mod per_file_champion_tests {
    use super::*;
    #[test] fn file_representative_prefers_state_inherit_intent_over_generic_open_member() {
        let d=tempfile::tempdir().unwrap();
        fs::write(d.path().join("layout.ts"),
            "export function open(){ return openPanel(); }\n/** Home terminal never inherits the saved open state on a new visit. */\nexport function freshHomeTerminalState(){ setOpened(false); }\n").unwrap();
        for n in 0..40 {fs::write(d.path().join(format!("noise_{n}.ts")),
            format!("export function createSession{n}(){{ openTerminal(); }}\n")).unwrap();}
        let q=query::Query::parse(serde_json::json!({"task":"进入新页面后终端不会继承上次展开状态","maxBytes":12000})).unwrap();
        let (c,_)=demand_corpus(d.path(),&q,Instant::now()+Duration::from_secs(3)).unwrap();
        assert!(c.units.iter().any(|u|u.name=="freshHomeTerminalState"),"{:?}",c.units.iter().map(|u|u.name.clone()).collect::<Vec<_>>());
    }
    #[test] fn file_representative_prefers_hide_reuse_restart_behavior_over_creation() {
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
print("Per-file declaration representative now maximizes distinct query-facet coverage.")
