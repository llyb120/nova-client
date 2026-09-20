"""Apply intent-facet champion selection only to the first natural-language recall pass.
Later passes already have explicit dependency names/callers and should use the cheaper original
declaration ordering. Baseline is the landed 20/20 engine; no labels/models/Reasonix changes.
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

p,s=load("polaris_demand_v2.rs","3c814ff3e30916e15b7e357bda2338a52c4aa341")
old='''    let facets=q.facets();
    let mut term_facets=vec![0u64;terms.len()];
    let mut name_facets=HashMap::<String,u64>::new();
    for (facet,group) in facets.iter().take(64).enumerate() {'''
new='''    // Only the initial natural-language pass needs intent-facet competition.
    // Dependency passes already receive explicit names/callers from verified
    // source edges; recomputing intent coverage there adds work and can perturb
    // an otherwise deterministic dependency closure.
    let intent_champion=names.is_empty()&&caller_names.is_empty();
    let intent_facets=if intent_champion{q.facets()}else{Vec::new()};
    let mut term_facets=vec![0u64;terms.len()];
    let mut name_facets=HashMap::<String,u64>::new();
    for (facet,group) in intent_facets.iter().take(64).enumerate() {'''
s=sub(s,old,new)
old='''    // Keep the same one-representative-per-file breadth. Only choose that
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
    for &(id,line,_,_) in &ranked {if selected.len()>=cap{break;}if !selected.contains(&(id,line)){selected.push((id,line));}}'''
new='''    if intent_champion {
        // Initial breadth: one representative per parsed file, chosen by
        // independent intent coverage; lexical score breaks coverage ties.
        let mut champions=HashMap::<usize,(usize,f64,u32)>::new();
        for &(id,line,score,covered) in &ranked {
            let replace=champions.get(&id).is_none_or(|(_,old_score,old_covered)|
                covered>*old_covered||(covered==*old_covered&&score>*old_score));
            if replace{champions.insert(id,(line,score,covered));}
        }
        // Coverage chooses the declaration but must not reorder files.
        let mut champions=champions.into_iter().map(|(id,(line,score,covered))|(id,line,score,covered)).collect::<Vec<_>>();
        champions.sort_by(|a,b|b.2.total_cmp(&a.2).then(rows[a.0].file.cmp(&rows[b.0].file)));
        for (id,line,_,_) in champions {
            if selected.len()>=cap{break;}
            if seen.insert(id)&&!selected.contains(&(id,line)){selected.push((id,line));}
        }
    } else {
        // Explicit dependency/name passes use the original cheap file-diverse
        // ordering. At this point source edges, not prose facets, are authoritative.
        for &(id,line,_,_) in &ranked {
            if selected.len()>=cap{break;}
            if seen.insert(id)&&!selected.contains(&(id,line)){selected.push((id,line));}
        }
    }
    for &(id,line,_,_) in &ranked {if selected.len()>=cap{break;}if !selected.contains(&(id,line)){selected.push((id,line));}}'''
s=sub(s,old,new)
p.write_text(s,encoding="utf-8")
print("Intent champion limited to initial natural-language recall; dependency passes use source edges.")
