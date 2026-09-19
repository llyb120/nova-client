"""Reviewed migration for PR11's frozen production baseline; no release/Reasonix changes.
This is applied in the validation worktree, then the exact tested diff is committed.
"""
from pathlib import Path
import hashlib
ROOT=Path('src-tauri/src/nova_tools_native')
def load(name,sha):
    p=ROOT/name;b=p.read_bytes()
    assert hashlib.sha1(b'blob '+str(len(b)).encode()+b'\0'+b).hexdigest()==sha, 'Unexpected baseline: '+name
    return p,b.decode()
def replace(s,old,new):
    assert s.count(old)==1,old[:100]
    return s.replace(old,new)
p,s=load('polaris_query.rs','21af642ade59320176c16a9e0858f93feb20be73')
helper='''// Identifiers need dictionary aliases, not full user-query parsing thousands of times.
pub(super) fn identifier_concepts(original:&HashSet<String>)->Vec<String> {
    static LOOKUP:OnceLock<HashMap<&'static str,Vec<usize>>>=OnceLock::new();
    let lookup=LOOKUP.get_or_init(||{
        let mut map=HashMap::<&'static str,Vec<usize>>::new();
        for (i,group) in CONCEPTS.iter().enumerate(){for word in group.split('|'){map.entry(word).or_default().push(i);}}map
    });
    let mut groups=std::collections::BTreeSet::new();
    for word in original {if let Some(ids)=lookup.get(word.as_str()){groups.extend(ids.iter().copied());}}
    let mut seen=original.clone();let mut out=Vec::new();
    for i in groups {for word in CONCEPTS[i].split('|'){if seen.insert(word.into()){out.push(word.into());}}}out
}
'''
s=replace(s,'impl Query {',helper+'impl Query {');p.write_text(s)
p,s=load('polaris_index.rs','fef1663c03bc1a5287c9b7d842eb3e427ca42e4b')
s=replace(s,'    let Ok(q)=query::Query::parse(serde_json::json!({"task":seed})) else{return Vec::new();};\n    q.terms.into_iter().filter(|(word,_)|!original.contains(word)).map(|(word,_)|word).collect()', '    query::identifier_concepts(&original)')
s+='\ninclude!("polaris_demand.rs");\n';p.write_text(s)
p,s=load('polaris.rs','a12a12b625f13ebfb58a3fce0facb7de041dd073')
s=replace(s,'let deadline=started+Duration::from_millis(3500);','let deadline=started+Duration::from_millis(900);')
s=replace(s,'let corpus=index::corpus(&root,started+Duration::from_millis(2200))?;', 'let (corpus,demand)=index::demand_corpus(&root,q,started+Duration::from_millis(600))?;')
s=replace(s,'let partial=corpus.partial||!gaps.is_empty()', 'let partial=corpus.partial||(evidence.is_empty()&&demand.candidate_files_omitted>0)||!gaps.is_empty()')
s=replace(s,'"files":corpus.files,"units":units.len()', '"searchScope":"bounded_candidate_files","demand":demand,"files":corpus.files,"units":units.len()')
# Avoid re-reading/hash-checking the same large file for every overlapping candidate slice.
s=replace(s,'let stale=ranked.iter().filter(|(i,_)|!index::verified(&root,&units[*i])).map(|(i,_)|units[*i].file.clone()).collect::<HashSet<_>>();', '''let mut verified=HashMap::<String,bool>::new();
    let stale=ranked.iter().filter(|(i,_)|!*verified.entry(units[*i].file.clone()).or_insert_with(||index::verified(&root,&units[*i]))).map(|(i,_)|units[*i].file.clone()).collect::<HashSet<_>>();''')
# Required closure competes with alternative candidates, not only whatever budget is left.
s=replace(s,'    let mut body=String::new();let mut gaps=Vec::new();', '''    // Keep the best main implementation first, then its discovered dependencies,
    // before alternative primary guesses consume the entire context allowance.
    if chosen.len()>1 {
        let first=chosen.remove(0);let mut support=Vec::new();let mut alternatives=Vec::new();
        for row in chosen {if row.1=="primary"{alternatives.push(row);}else{support.push(row);}}
        chosen=vec![first];chosen.extend(support);chosen.extend(alternatives);
    }
    let mut body=String::new();let mut gaps=Vec::new();''')
p.write_text(s)
print('Applied query-first retrieval; original context.rs, model defaults and Reasonix are untouched.')
