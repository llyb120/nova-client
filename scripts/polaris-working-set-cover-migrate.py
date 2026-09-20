"""Give leading candidate files enough declaration depth before broad file diversity.
This fixes a structural beam-search failure: the right file was already found, but only
its first matching nested/member declaration was materialized. No labels or repo names.
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
    assert s.count(a)==1,a[:140]
    return s.replace(a,b)

p,s=load("polaris_demand_v2.rs","82626c7fa9538fd54011cb08b5163a15d92f07fb")
old='''    for &(id,line,_) in &ranked {if seen.insert(id){selected.push((id,line));if selected.len()>=cap{break;}}}
    for &(id,line,_) in &ranked {if selected.len()>=cap{break;}if !selected.contains(&(id,line)){selected.push((id,line));}}
    selected
}'''
new='''    // The old diversity-first beam picked only one declaration per file until
    // nearly the whole cap was consumed. A correct file could therefore be
    // parsed but its actual implementation (the second declaration in that
    // file) never materialized. Keep bounded breadth, but reserve declaration
    // depth for the strongest files before broadening.
    let mut file_order=Vec::new();let mut file_seen=HashSet::new();
    for &(id,_,_) in &ranked {if file_seen.insert(id){file_order.push(id);}}
    let depth_files=file_order.iter().copied().take(12).collect::<HashSet<_>>();
    let mut per_file=HashMap::<usize,usize>::new();
    // First beam: up to two declarations from each of the leading files.
    for &(id,line,_) in &ranked {
        if selected.len()>=cap{break;}
        if !depth_files.contains(&id)||selected.contains(&(id,line)){continue;}
        let count=per_file.entry(id).or_default();
        if *count>=2{continue;}
        selected.push((id,line));*count+=1;seen.insert(id);
    }
    // Breadth beam: one declaration from every other parsed candidate file.
    for &(id,line,_) in &ranked {
        if selected.len()>=cap{break;}
        if selected.contains(&(id,line)){continue;}
        if per_file.get(&id).copied().unwrap_or(0)>0{continue;}
        selected.push((id,line));per_file.insert(id,1);seen.insert(id);
    }
    // Remaining budget is score-ordered with a small per-file cap. This keeps
    // helpers/nested members available without letting a huge file monopolize.
    for &(id,line,_) in &ranked {
        if selected.len()>=cap{break;}
        if selected.contains(&(id,line)){continue;}
        let count=per_file.entry(id).or_default();
        if *count>=3{continue;}
        selected.push((id,line));*count+=1;
    }
    selected
}'''
s=sub(s,old,new)
# Generic regression: correct file is known, but the relevant function is not
# its first lexical declaration and many other files compete for diversity.
anchor='''#[cfg(test)]
mod demand_tests {'''
tests='''#[cfg(test)] mod declaration_beam_tests {
    use super::*;
    #[test] fn relevant_second_declaration_survives_file_diversity() {
        let d=tempfile::tempdir().unwrap();
        for n in 0..45 {fs::write(d.path().join(format!("noise_{n}.ts")),
            format!("// 会话终端\\nexport function noise{n}() {{ createSession(); }}\\n")).unwrap();}
        fs::write(d.path().join("state.ts"),
            "// 会话终端\\nexport function genericOpen() { return true; }\\n// 新建会话页面不继承上次展开状态\\nexport function resetVisitState() { setOpened(false); }\\n").unwrap();
        let q=query::Query::parse(serde_json::json!({"task":"进入新建会话页面后，终端不会继承上次展开状态","maxBytes":12000})).unwrap();
        let (c,_)=demand_corpus(d.path(),&q,Instant::now()+Duration::from_secs(3)).unwrap();
        assert!(c.units.iter().any(|u|u.name=="resetVisitState"),"{:?}",c.units.iter().map(|u|u.name.clone()).collect::<Vec<_>>());
    }
    #[test] fn relevant_second_behavior_is_kept_with_many_terminal_files() {
        let d=tempfile::tempdir().unwrap();
        for n in 0..35 {fs::write(d.path().join(format!("panel_{n}.ts")),
            format!("// 终端面板\\nexport function panel{n}() {{ showTerminal(); }}\\n")).unwrap();}
        fs::write(d.path().join("session.ts"),
            "// 终端标签\\nexport function createTab() { startShell(); }\\n// 收起再打开复用原进程，不重新启动\\nexport function attachExisting() { if (!ready) mountHost(); }\\n").unwrap();
        let q=query::Query::parse(serde_json::json!({"task":"收起侧边栏再打开时复用原来的命令行进程，不重新启动","maxBytes":12000})).unwrap();
        let (c,_)=demand_corpus(d.path(),&q,Instant::now()+Duration::from_secs(3)).unwrap();
        assert!(c.units.iter().any(|u|u.name=="attachExisting"),"{:?}",c.units.iter().map(|u|u.name.clone()).collect::<Vec<_>>());
    }
}

'''
s=sub(s,anchor,tests+anchor)
p.write_text(s,encoding="utf-8")
print("Applied bounded two-declaration beam for leading natural-language candidate files.")
