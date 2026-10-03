"""Scope-aware recall and dependency closure. No project paths or answer names in ranking."""
from pathlib import Path
R=Path('src-tauri/src/nova_tools_native')
def edit(name,old,new):
 p=R/name;s=p.read_text();assert s.count(old)==1,(name,old[:90]);p.write_text(s.replace(old,new))
edit('polaris_demand_v2.rs','    for _ in 0..2{','    let mut required_names=HashSet::<String>::new();\n    for _ in 0..2{')
edit('polaris_demand_v2.rs','        for &(id,_) in seeds.iter().take(6){','''        let mut expand=seeds.iter().take(6).map(|(id,_)|*id).collect::<Vec<_>>();
        let mut identities=expand.iter().map(|i|super::identity(&units[*i])).collect::<HashSet<_>>();
        for (i,u) in units.iter().enumerate(){if required_names.contains(&u.name)&&identities.insert(super::identity(u)){expand.push(i);if expand.len()>=64{break;}}}
        for id in expand{''')
edit('polaris_demand_v2.rs','        let mut neighbours=rows.iter()', '        required_names.extend(names.iter().cloned());\n        let mut neighbours=rows.iter()')
p=R/'polaris_query.rs';s=p.read_text();s+='''
// Generic entities distinguish a requested object from surrounding UI prose.
// Match literal current source/module text, not generated concept aliases.
pub(super) fn focus_score(task:&str,source:&str)->f64 {
    const GROUPS:&[(&str,&str,f64)]=&[
        ("终端|命令行|terminal|shell|pty","terminal|shell|pty|终端|命令行",3.0),
        ("桌面|desktop","desktop|native desktop|桌面",2.5),
        ("键盘|按键|组合键|keyboard|keystroke","keyboard|key|press|键盘|按键|组合键",1.5),
        ("释放|松开|release key|keyup","release|keyup|释放|松开",1.5),
        ("附件|attachment","attachment|附件",2.0),
        ("队列|排队|queue","queue|pending|队列|排队",2.0),
        ("加密|encrypt","encrypt|cipher|加密",2.0),
        ("解密|decrypt","decrypt|cipher|解密",2.0),
        ("剪贴板|clipboard","clipboard|剪贴板",2.0),
        ("窗口|window","window|窗口",1.5),
        ("未读|unread","unread|未读",2.0),
    ];
    let task=task.to_lowercase();let source=source.to_lowercase();let mut total=0.0;let mut found=0.0;
    for &(triggers,aliases,weight) in GROUPS {if triggers.split('|').any(|t|task.contains(t)){
        total+=weight;if aliases.split('|').any(|word|source.contains(word)){found+=weight;}
    }}
    if total==0.0 {1.0}else{0.12+0.88*(found/total)*(found/total)}
}
''';p.write_text(s)
edit('polaris_demand_v2.rs','            if names.contains(&s.name){score+=10000.0;}','''            score*=query::focus_score(&q.task,&format!("{}\\n{}\\n{}",row.file,file.source.iter().take(5).map(String::as_str).collect::<Vec<_>>().join("\\n"),body));
            if names.contains(&s.name){score+=10000.0;}''')
edit('polaris_demand_v2.rs','        row.score/=1.0+0.25*', '        row.score*=query::focus_score(&q.task,&format!("{}\\n{}",row.file,row.text));\n        row.score/=1.0+0.25*')
edit('polaris.rs','        if score>0.0 {rows.push','''        score*=query::focus_score(&q.task,&format!("{}\\n{}\\n{}",u.file,u.source.iter().take(5).map(String::as_str).collect::<Vec<_>>().join("\\n"),u.source[u.owner_start-1..u.owner_end].join("\\n")));
        if score>0.0 {rows.push''')
edit('polaris_packet.rs','    let mut lines_left=q.lines;', '''    ordered.sort_by_key(|(_,role)|if *role=="primary"{0}else if matches!(*role,"callee-reference"|"command-reference"){1}else if *role=="type-reference"{2}else{3});
    let mut lines_left=q.lines;''')
edit('polaris_demand_v2.rs','    let entry=scan_source(&row.text,&row.file);let source=', '    let started=Instant::now();let entry=scan_source(&row.text,&row.file);let parsed_ms=started.elapsed().as_secs_f64()*1000.0;let source=')
edit('polaris_demand_v2.rs','    LightFile{hash:row.hash.clone(),entry,source,names,built:BTreeMap::new()}', '''    if std::env::var_os("NOVA_POLARIS_TRACE_PARSE").is_some(){eprintln!("[polaris-parse] {}",serde_json::json!({"file":row.file,"bytes":row.text.len(),"parseMs":parsed_ms,"totalMs":started.elapsed().as_secs_f64()*1000.0,"declarations":entry.syms.len()}));}
    LightFile{hash:row.hash.clone(),entry,source,names,built:BTreeMap::new()}''')
p=R/'polaris_packet.rs';s=p.read_text();s+='''
#[cfg(test)] mod transitive_context_tests {
    use super::*;
    #[test]fn constant_referenced_only_by_helper_is_included_without_a_second_read(){
        let d=tempfile::tempdir().unwrap();
        fs::write(d.path().join("work.rs"),"const RETRY_CAP: u8 = 3;\\nstruct Attempt { used: u8 }\\n// 请求重试\\nfn retry_request(a: Attempt) -> bool { within_limit(a.used) }\\nfn within_limit(used: u8) -> bool { used < RETRY_CAP }\\n").unwrap();
        let out=polaris(d.path(),serde_json::json!({"task":"请求重试","maxBytes":12000})).unwrap();
        for code in ["fn retry_request", "fn within_limit", "struct Attempt", "const RETRY_CAP"]{assert!(out.contains(code),"{code}: {out}");}
    }
}
''';p.write_text(s)
print('Applied transitive dependency expansion, entity-aware recall and primary-first budget.')
