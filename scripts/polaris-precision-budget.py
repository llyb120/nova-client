"""Compile query entity matchers once, preserve declared roots, and bound discovery work."""
from pathlib import Path
R=Path('src-tauri/src/nova_tools_native')
def edit(name,old,new):
 p=R/name;s=p.read_text();assert s.count(old)==1,(name,old[:100]);p.write_text(s.replace(old,new))
p=R/'polaris_query.rs';s=p.read_text();a=s.index('pub(super) fn focus_score(');s=s[:a]+'''#[derive(Clone,Debug)]
pub(super) struct Focus { patterns:regex::RegexSet,weights:Vec<f64> }
impl Focus {
    fn new(task:&str)->Result<Self,String> {
        const GROUPS:&[(&str,&str,f64)]=&[
            ("终端|命令行|terminal|shell|pty","terminal|shell|pty|终端|命令行",3.0),
            ("桌面|desktop","desktop|桌面",2.5),
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
        let task=task.to_lowercase();let active=GROUPS.iter().filter(|(trigger,_,_)|trigger.split('|').any(|t|task.contains(t))).collect::<Vec<_>>();
        let patterns=regex::RegexSetBuilder::new(active.iter().map(|(_,aliases,_)|aliases.split('|').map(regex::escape).collect::<Vec<_>>().join("|"))).case_insensitive(true).build().map_err(|e|e.to_string())?;
        Ok(Self{patterns,weights:active.iter().map(|(_,_,w)|*w).collect()})
    }
    pub(super) fn score<'a>(&self,parts:impl IntoIterator<Item=&'a str>)->f64 {
        if self.weights.is_empty(){return 1.0;}
        let mut mask=0u32;
        for part in parts {for i in self.patterns.matches(part){mask|=1<<i;}if mask.count_ones() as usize==self.weights.len(){return 1.0;}}
        let total=self.weights.iter().sum::<f64>();let matched=self.weights.iter().enumerate().filter(|(i,_)|mask&(1<<i)!=0).map(|(_,w)|*w).sum::<f64>();
        0.12+0.88*(matched/total)*(matched/total)
    }
}
''';s=s.replace('    pub params: Value,','    pub params: Value,\n    pub focus: Focus,');s=s.replace('Ok(Self{params,task,anchors,files,terms,test_intent,doc_intent,hard,lines})','Ok(Self{focus:Focus::new(&task)?,params,task,anchors,files,terms,test_intent,doc_intent,hard,lines})');p.write_text(s)
edit('polaris_query.rs','"键盘|按键|组合键|keyboard|keypress|keystroke|chord|key",','"桌面|desktop", "释放|松开|release|keyup", "解析|parse", "键盘|按键|组合键|keyboard|keypress|keystroke|chord|key",')
edit('polaris_demand_v2.rs','score*=query::focus_score(&q.task,&format!("{}\\n{}\\n{}",row.file,file.source.iter().take(5).map(String::as_str).collect::<Vec<_>>().join("\\n"),body));','score*=q.focus.score(std::iter::once(row.file.as_str()).chain(file.source.iter().take(5).map(String::as_str)).chain(std::iter::once(body.as_str())));')
edit('polaris_demand_v2.rs','row.score*=query::focus_score(&q.task,&format!("{}\\n{}",row.file,row.text));','row.score*=q.focus.score([row.file.as_str(),row.text.as_str()]);')
edit('polaris.rs','score*=query::focus_score(&q.task,&format!("{}\\n{}\\n{}",u.file,u.source.iter().take(5).map(String::as_str).collect::<Vec<_>>().join("\\n"),u.source[u.owner_start-1..u.owner_end].join("\\n")));','score*=*focus.entry(identity(u)).or_insert_with(||focus_source(q,u));')
edit('polaris.rs','    let mut rows=Vec::new();','    let mut rows=Vec::new();let mut focus=HashMap::new();')
edit('polaris.rs','fn lexical_rank(','''fn focus_source(q:&Query,u:&CodeUnit)->f64 {
    q.focus.score(std::iter::once(u.file.as_str()).chain(u.source.iter().take(5).map(String::as_str)).chain(u.source[u.owner_start-1..u.owner_end].iter().map(String::as_str)))
}
fn lexical_rank(''')
edit('polaris_rank.rs','    let mut out=scores.into_values().collect::<Vec<_>>();','    let mut out=scores.into_values().collect::<Vec<_>>();\n    for (id,score) in &mut out {if !q.anchors.iter().any(|a|a.eq_ignore_ascii_case(&units[*id].name)){*score*=focus_source(q,&units[*id]);}}')
edit('polaris_demand_v2.rs','row.score/=1.0+0.25*(1.0+row.text.len() as f64/16000.0).ln();','row.score/=1.0+0.8*(1.0+row.text.len() as f64/8000.0).ln();')
edit('polaris_demand_v2.rs','const INITIAL_FILES: usize = 24;','const INITIAL_FILES: usize = 16;')
edit('polaris_demand_v2.rs','&HashSet::new(),&HashSet::new(),80);','&HashSet::new(),&HashSet::new(),64);')
edit('polaris_packet.rs','    ordered.sort_by_key(|(_,role)|if *role=="primary"{0}else if matches!(*role,"callee-reference"|"command-reference"){1}else if *role=="type-reference"{2}else{3});','''    let root_rank=roots.iter().enumerate().map(|(n,i)|(identity(&units[*i]),n)).collect::<HashMap<_,_>>();
    for (id,role) in &mut ordered{if root_rank.contains_key(&identity(&units[*id])){*role="primary";}}
    ordered.sort_by_key(|(id,role)|root_rank.get(&identity(&units[*id])).copied().unwrap_or(if matches!(*role,"callee-reference"|"command-reference"){10}else if *role=="type-reference"{11}else{12}));''')
edit('polaris_demand_v2.rs','(!test||q.test_intent||q.files.iter().any(|f|f==file))','(!test||q.test_intent||q.anchors.iter().any(|a|a==&s.name)||q.files.iter().any(|f|f==file&&file_role(f)=="test"))')
edit('polaris.rs','||q.files.contains(&u.file)).cloned().collect::<Vec<_>>();','||q.anchors.iter().any(|a|a==&u.name)||(q.files.contains(&u.file)&&index::file_role_for_query(&u.file)!="implementation")).cloned().collect::<Vec<_>>();')
p=R/'polaris_index.rs';s=p.read_text();s+='\npub(super) fn file_role_for_query(file:&str)->&\'static str {file_role(file)}\n';p.write_text(s)
print('Applied compiled focus matchers, bounded candidate work, preserved root priority and implementation-only file scopes.')
