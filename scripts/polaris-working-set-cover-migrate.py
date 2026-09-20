"""Use the same leading source comments for declaration selection that the
final CodeUnit already preserves. The baseline is accepted literal-recall source.
No query labels, repo names, output-budget changes, model calls, or Reasonix changes.
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
    assert s.count(a)==1,a[:160]
    return s.replace(a,b)

p,s=load("polaris_demand_v2.rs","82626c7fa9538fd54011cb08b5163a15d92f07fb")
old='''            let first=s.ln.saturating_sub(1);let end=s.end.min(file.source.len());if first>=end{continue;}
            let body=file.source[first..end.min(first+320)].join("\n");
            let header=file.source[first.saturating_sub(4)..(first+4).min(end)].join("\n");
            let matched=matcher.matches(&body);let head=matcher.matches(&header);
            let mut score=0.0;
            for (i,(term,weight)) in terms.iter().enumerate(){
                if file.names[&s.ln].contains(term){score+=weight*idf[i]*6.0;}
                if head.matched(i){score+=weight*idf[i]*2.0;}
                if matched.matched(i){score+=weight*idf[i];}
            }'''
new='''            let first=s.ln.saturating_sub(1);let end=s.end.min(file.source.len());if first>=end{continue;}
            // Natural-language intent is often documented immediately above a
            // function while the implementation itself contains only generic
            // API names. Final CodeUnits already preserve up to eight leading
            // comment/attribute/blank lines; declaration selection must score
            // the same evidence or it can parse the right file yet materialize
            // the wrong neighboring function.
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
            }'''
s=sub(s,old,new)
# Generic regression: two neighboring functions in the same file; only the
# leading comment disambiguates which behavior the natural-language request means.
anchor='''#[cfg(test)]
mod demand_tests {'''
tests='''#[cfg(test)] mod comment_intent_selection_tests {
    use super::*;
    #[test] fn leading_comment_selects_the_behavior_not_generic_neighbor() {
        let d=tempfile::tempdir().unwrap();
        fs::write(d.path().join("panel.ts"),
            "export function createPanel(){ return true; }\n/** Existing process is remounted; hiding the panel never restarts it. */\nexport function attachExisting(){ if (!ready) mountHost(); }\n").unwrap();
        for n in 0..32 {fs::write(d.path().join(format!("noise_{n}.ts")),
            format!("export function panel{n}(){{ createPanel(); }}\n")).unwrap();}
        let q=query::Query::parse(serde_json::json!({"task":"收起面板再打开时复用原进程，不重新启动","maxBytes":12000})).unwrap();
        let (c,_)=demand_corpus(d.path(),&q,Instant::now()+Duration::from_secs(3)).unwrap();
        assert!(c.units.iter().any(|u|u.name=="attachExisting"),"{:?}",c.units.iter().map(|u|u.name.clone()).collect::<Vec<_>>());
    }
    #[test] fn leading_comment_preserves_negative_state_intent() {
        let d=tempfile::tempdir().unwrap();
        fs::write(d.path().join("layout.ts"),
            "export function open(){ return true; }\n/** New visits never inherit the previously opened state. */\nexport function freshState(){ setOpened(false); }\n").unwrap();
        let q=query::Query::parse(serde_json::json!({"task":"新页面不会继承上次展开状态","maxBytes":12000})).unwrap();
        let (c,_)=demand_corpus(d.path(),&q,Instant::now()+Duration::from_secs(3)).unwrap();
        assert!(c.units.iter().any(|u|u.name=="freshState"),"{:?}",c.units.iter().map(|u|u.name.clone()).collect::<Vec<_>>());
    }
}

'''
s=sub(s,anchor,tests+anchor)
p.write_text(s,encoding="utf-8")
print("Declaration selection now scores the same leading source comments preserved in final context.")
