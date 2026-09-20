"""Prune dependency files that cannot contain the exact imported/member symbol
which justified opening them. Baseline is the accepted 20/20 production source.
This does not change initial recall, declaration ranking, packet budgets, models,
Reasonix, or caller discovery.
"""
from pathlib import Path
import hashlib
P=Path("src-tauri/src/nova_tools_native/polaris_demand_v2.rs")
data=P.read_bytes()
got=hashlib.sha1(b"blob "+str(len(data)).encode()+b"\0"+data).hexdigest()
assert got=="3c814ff3e30916e15b7e357bda2338a52c4aa341",got
s=data.decode("utf-8")
old='''        let mut direct=HashSet::new();let mut names=HashSet::new();let callers=seeds.iter().take(2).map(|(i,_)|(units[*i].file.clone(),units[*i].name.clone())).collect::<HashSet<_>>();'''
new='''        let mut direct=HashSet::new();let mut direct_names=HashMap::<String,HashSet<String>>::new();let mut names=HashSet::new();let callers=seeds.iter().take(2).map(|(i,_)|(units[*i].file.clone(),units[*i].name.clone())).collect::<HashSet<_>>();'''
assert s.count(old)==1
s=s.replace(old,new)
old='''            for import in u.imports.iter(){if u.calls.contains(&import.name)||u.members.iter().any(|(o,_)|o==&import.name){if let Some(f)=resolve_specifier(&import.from,&u.file,&files){direct.insert(f);names.insert(import.orig.clone().unwrap_or_else(||import.name.clone()));}}}
            for (object,member) in &u.members{if let Some(suffix)=object.strip_prefix("crate::"){names.insert(member.clone());if let Some(pos)=u.file.rfind("src/"){let prefix=&u.file[..pos+4];let module=suffix.replace("::","/");for f in [format!("{prefix}{module}.rs"),format!("{prefix}{module}/mod.rs")]{if files.contains(&f){direct.insert(f);}}}}}'''
new='''            for import in u.imports.iter(){
                if u.calls.contains(&import.name)||u.members.iter().any(|(o,_)|o==&import.name){
                    if let Some(f)=resolve_specifier(&import.from,&u.file,&files){
                        let symbol=import.orig.clone().unwrap_or_else(||import.name.clone());
                        direct.insert(f.clone());direct_names.entry(f).or_default().insert(symbol.clone());names.insert(symbol);
                    }
                }
            }
            for (object,member) in &u.members{
                if let Some(suffix)=object.strip_prefix("crate::"){
                    names.insert(member.clone());
                    if let Some(pos)=u.file.rfind("src/"){
                        let prefix=&u.file[..pos+4];let module=suffix.replace("::","/");
                        for f in [format!("{prefix}{module}.rs"),format!("{prefix}{module}/mod.rs")]{
                            if files.contains(&f){direct.insert(f.clone());direct_names.entry(f).or_default().insert(member.clone());}
                        }
                    }
                }
            }'''
assert s.count(old)==1
s=s.replace(old,new)
old='''        let mut neighbours=rows.iter().enumerate().filter(|(i,_)|!parsed.contains(i)).filter_map(|(i,r)|{
            let explicit=direct.contains(&r.file);let caller=callers.iter().any(|(_,name)|r.text.contains(name));
            ((explicit||caller)&&structural_candidate(&r.file,q)).then_some((i,if explicit{10000.0+r.score}else{r.score}))
        }).collect::<Vec<_>>();'''
new='''        let mut neighbours=rows.iter().enumerate().filter(|(i,_)|!parsed.contains(i)).filter_map(|(i,r)|{
            // A resolved dependency file is useful only if it still contains
            // the source symbol that led us there. This rejects stale/re-export
            // noise before tree-sitter, while callers keep the previous rule.
            let explicit=direct.contains(&r.file)&&direct_names.get(&r.file)
                .is_some_and(|wanted|wanted.iter().any(|name|name.len()>=2&&r.text.contains(name)));
            let caller=callers.iter().any(|(_,name)|r.text.contains(name));
            ((explicit||caller)&&structural_candidate(&r.file,q)).then_some((i,if explicit{10000.0+r.score}else{r.score}))
        }).collect::<Vec<_>>();'''
assert s.count(old)==1
s=s.replace(old,new)

# Generic source-graph regressions: exact imported/member target survives;
# unrelated imported files do not become dependencies just because they rank.
anchor='''#[cfg(test)]
mod demand_tests {'''
tests='''#[cfg(test)] mod dependency_prune_tests {
    use super::*;
    #[test] fn imported_exact_symbol_survives_dependency_pruning() {
        let d=tempfile::tempdir().unwrap();fs::create_dir(d.path().join("src")).unwrap();
        fs::write(d.path().join("src/ui.ts"),"import {attachExisting} from './terminal';\n// 复用终端进程\nexport function restore(){ attachExisting(); }\n").unwrap();
        fs::write(d.path().join("src/terminal.ts"),"export function attachExisting(){ return mountHost(); }\n").unwrap();
        let q=query::Query::parse(serde_json::json!({"task":"复用终端进程","maxBytes":12000})).unwrap();
        let (c,_)=demand_corpus(d.path(),&q,Instant::now()+Duration::from_secs(3)).unwrap();
        for name in ["restore","attachExisting"]{assert!(c.units.iter().any(|u|u.name==name),"{name}");}
    }
    #[test] fn rust_qualified_member_survives_dependency_pruning() {
        let d=tempfile::tempdir().unwrap();fs::create_dir_all(d.path().join("src/runtime")).unwrap();
        fs::write(d.path().join("src/lib.rs"),"mod runtime;\n// 恢复任务\npub fn restore(){ crate::runtime::resume_job(); }\n").unwrap();
        fs::write(d.path().join("src/runtime/mod.rs"),"pub fn resume_job(){ run(); }\n").unwrap();
        let q=query::Query::parse(serde_json::json!({"task":"恢复任务","maxBytes":12000})).unwrap();
        let (c,_)=demand_corpus(d.path(),&q,Instant::now()+Duration::from_secs(3)).unwrap();
        assert!(c.units.iter().any(|u|u.name=="resume_job"));
    }
}

'''
assert s.count(anchor)==1
s=s.replace(anchor,tests+anchor)
P.write_text(s,encoding="utf-8")
print("Pruned impossible direct dependency files before syntax parsing; initial recall and caller expansion unchanged.")
