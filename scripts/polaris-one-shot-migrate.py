"""Apply reviewed source changes to the frozen PR11 baseline for validation.
No credentials, generated source transport, model calls, release or Reasonix changes.
"""
from pathlib import Path
import hashlib
ROOT=Path('src-tauri/src/nova_tools_native')
def load(name,sha):
    p=ROOT/name;b=p.read_bytes().replace(b'\r\n',b'\n')
    assert hashlib.sha1(b'blob '+str(len(b)).encode()+b'\0'+b).hexdigest()==sha,'Unexpected baseline: '+name
    return p,b.decode()
def replace(s,old,new):
    assert s.count(old)==1,old[:100]
    return s.replace(old,new)
p,s=load('polaris_query.rs','21af642ade59320176c16a9e0858f93feb20be73')
helper='''// Dictionary expansion for names avoids reparsing thousands of synthetic queries.
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
s=replace(s,'impl Query {',helper+'impl Query {')
s=replace(s,'"首页|新建|home|new|create",','"键盘|按键|组合键|keyboard|keypress|keystroke|chord|key", "首页|新建|home|new|create",')
p.write_text(s)
p,s=load('polaris_index.rs','fef1663c03bc1a5287c9b7d842eb3e427ca42e4b')
s=replace(s,'    let Ok(q)=query::Query::parse(serde_json::json!({"task":seed})) else{return Vec::new();};\n    q.terms.into_iter().filter(|(word,_)|!original.contains(word)).map(|(word,_)|word).collect()', '    query::identifier_concepts(&original)')
s=replace(s,'fn make_units(file:&str,text:&str)->Vec<Arc<CodeUnit>> {\n    let entry=scan_source(text,file);let source=Arc::new(text.lines().map(str::to_owned).collect::<Vec<_>>());\n    let imports=Arc::new(entry.imports);let file_hash=digest(text.as_bytes());', '''fn make_units(file:&str,text:&str)->Vec<Arc<CodeUnit>> {
    let entry=scan_source(text,file);let source=Arc::new(text.lines().map(str::to_owned).collect::<Vec<_>>());
    make_units_selected(file,text,&entry,source,None)
}
fn make_units_selected(file:&str,text:&str,entry:&FileEntry,source:Arc<Vec<String>>,selected:Option<&HashSet<usize>>)->Vec<Arc<CodeUnit>> {
    let imports=Arc::new(entry.imports.clone());let file_hash=digest(text.as_bytes());''')
s=replace(s,'    for (name,begin,finish,kind) in spans {','    for (name,begin,finish,kind) in spans {\n        if selected.is_some_and(|wanted|!wanted.contains(&begin)){continue;}')
s=replace(s,'        for offset in (begin..=finish).step_by(64) {','        let owner_references=references(&source[begin-1..finish].join("\\n"));\n        for offset in (begin..=finish).step_by(64) {')
s=replace(s,'            let (calls,events,commands,members)=references(&body);','            let (calls,events,commands,members)=owner_references.clone();')
s+='\ninclude!("polaris_demand_v2.rs");\n';p.write_text(s)
p,s=load('polaris.rs','a12a12b625f13ebfb58a3fce0facb7de041dd073')
s=replace(s,'mod rank {include!("polaris_rank.rs");}', 'mod rank {include!("polaris_rank.rs");}\nmod packet {include!("polaris_packet.rs");}')
s=replace(s,'let deadline=started+Duration::from_millis(3500);','let deadline=started+Duration::from_millis(900);')
s=replace(s,'let corpus=index::corpus(&root,started+Duration::from_millis(2200))?;', 'let (corpus,demand)=index::demand_corpus(&root,q,started+Duration::from_millis(600))?;')
s=replace(s,'let partial=corpus.partial||!gaps.is_empty()', 'let partial=corpus.partial||(evidence.is_empty()&&demand.candidate_files_omitted>0)||!gaps.is_empty()')
s=replace(s,'"files":corpus.files,"units":units.len(),"changedFiles":corpus.changed','"searchScope":"bounded_candidate_files","workingSet":"source_linked_not_root_cause_proof","demand":demand,"files":corpus.files,"units":units.len(),"changedFiles":corpus.changed')
s=replace(s,'let stale=ranked.iter().filter(|(i,_)|!index::verified(&root,&units[*i])).map(|(i,_)|units[*i].file.clone()).collect::<HashSet<_>>();', '''let mut verified=HashMap::<String,bool>::new();
    let stale=ranked.iter().filter(|(i,_)|!*verified.entry(units[*i].file.clone()).or_insert_with(||index::verified(&root,&units[*i]))).map(|(i,_)|units[*i].file.clone()).collect::<HashSet<_>>();''')
a=s.index('    let mut chosen=Vec::<(usize,&str)>::new();');b=s.index('    let time_budget_reached=',a)
s=s[:a]+'    let packet=packet::pack(&root,&units,&ranked,q,deadline);\n    let body=packet.body;let gaps=packet.gaps;let evidence=packet.evidence;let links=packet.links;\n'+s[b:]
s=replace(s,'"backend":meta["backend"],"indexPartial":meta["indexPartial"]','"backend":meta["backend"],"searchScope":meta["searchScope"],"demand":meta["demand"],"indexPartial":meta["indexPartial"]')
p.write_text(s)
p,s=load('polaris_rank.rs','a0596c0f0b3e452bf83280e21da1700709da36a2')
s=replace(s,'if q.files.contains(&u.file)||q.anchors.iter().any(|a|a.eq_ignore_ascii_case(&u.name)) {','if q.anchors.iter().any(|a|a.eq_ignore_ascii_case(&u.name)) {')
s=replace(s,'            let covered=facets.iter()', '            let call_terms=u.calls.iter().flat_map(|call|query::tokens(call)).collect::<HashSet<_>>();\n            let covered=facets.iter()')
s=replace(s,'u.calls.iter().any(|call|query::tokens(call).iter().any(|term|term==word))','call_terms.contains(word)')
p.write_text(s)
print('Applied declaration-first working sets; unchanged source remains byte-identical.')
