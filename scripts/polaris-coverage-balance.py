"""Keep late query constraints represented and discover callers within source scope."""
from pathlib import Path
R=Path('src-tauri/src/nova_tools_native')
def edit(name,old,new):
 p=R/name;s=p.read_text();assert s.count(old)==1,(name,old[:90]);p.write_text(s.replace(old,new))
edit('polaris_query.rs','        for group in CONCEPTS {if group.split(\'|\').any(|s|original.contains(s)){for s in group.split(\'|\'){if terms.len()<112&&seen.insert(s.into()){terms.push((s.into(),0.55));}}}}', '''        // Every recognized concept gets one English representative before
        // any early noun consumes the remaining synonym budget.
        let expansions=CONCEPTS.iter().filter(|group|group.split('|').any(|s|original.contains(s)))
            .map(|g|g.split('|').filter(|w|w.is_ascii()).chain(g.split('|').filter(|w|!w.is_ascii())).collect::<Vec<_>>()).collect::<Vec<_>>();
        for column in 0..expansions.iter().map(Vec::len).max().unwrap_or(0) {
            for group in &expansions {if let Some(&word)=group.get(column){if terms.len()<112&&seen.insert(word.into()){terms.push((word.into(),0.55));}}}
        }''')
edit('polaris_query.rs','    out.retain(|s|!noise(s));out','''    // Source prose uses inflected verbs. Retain the exact tokens and add
    // bounded base-form alternatives, without modifying source evidence.
    let mut stems=Vec::new();
    for word in &out {if !word.is_ascii()||word.len()<5||!word.chars().all(|c|c.is_ascii_alphabetic()){continue;}
        if let Some(base)=word.strip_suffix("ies"){if base.len()>=3{stems.push(format!("{base}y"));}}
        else if !word.ends_with("ss")&&!word.ends_with("us")&&!word.ends_with("is") {if let Some(base)=word.strip_suffix('s'){stems.push(base.into());}}
        for suffix in ["ed","ing"] {if let Some(base)=word.strip_suffix(suffix).filter(|b|b.len()>=3){stems.push(base.into());stems.push(format!("{base}e"));}}
    }
    out.extend(stems);out.retain(|s|!noise(s));out''')
edit('polaris_demand_v2.rs','caller_names:&HashSet<String>','caller_names:&HashSet<(String,String)>')
edit('polaris_demand_v2.rs','            if caller_names.iter().any(|name|body.contains(name)){score+=100.0;}','            if caller_names.iter().any(|(file,name)|row.file==*file&&body.contains(name)){score+=3000.0;}')
edit('polaris_demand_v2.rs','callers.insert(u.name.clone());','callers.insert((u.file.clone(),u.name.clone()));')
edit('polaris_demand_v2.rs','let caller=callers.iter().any(|s|r.text.contains(s));','let caller=callers.iter().any(|(_,name)|r.text.contains(name));')
edit('polaris_query.rs',"    pub(super) fn score<'a>","    pub(super) fn active(&self)->bool {!self.weights.is_empty()}\n    pub(super) fn score<'a>")
edit('polaris_packet.rs','                    if role=="caller-reference"&&bytes(u)>q.hard/4{continue;}','                    if role=="caller-reference"&&bytes(u)>q.hard/4&&(!q.focus.active()||focus_source(q,u)<0.95){continue;}')
edit('polaris_packet.rs','ordered.sort_by_key(|(id,role)|root_rank.get(&identity(&units[*id])).copied().unwrap_or(if matches!(*role,"callee-reference"|"command-reference"){10}else if *role=="type-reference"{11}else{12}));','''ordered.sort_by_key(|(id,role)|{
        if let Some(&n)=root_rank.get(&identity(&units[*id])){return if n>=2&&q.focus.active()&&focus_source(q,&units[*id])<0.8{10+n}else{n};}
        if *role=="caller-reference"&&q.focus.active()&&focus_source(q,&units[*id])>=0.95{4}
        else if matches!(*role,"callee-reference"|"command-reference"){6}
        else if *role=="type-reference"{7}else{20}
    });''')
p=R/'polaris_query.rs';s=p.read_text();s+='''
#[cfg(test)] mod balanced_expansion_tests {
    use super::*;
    #[test]fn late_constraints_get_english_terms_before_noun_synonyms() {
        let q=Query::parse(serde_json::json!({"task":"新建会话页面终端不会继承上次展开状态"})).unwrap();
        for word in ["terminal","inherit","expand","state"]{assert!(q.terms.iter().any(|(t,_)|t==word),"{word}");}
    }
    #[test]fn source_prose_retains_its_token_and_a_base_form(){
        let words=tokens("inherits hiding restarts retained status");
        for word in ["inherits","inherit","hide","restart","retain","status"]{assert!(words.contains(&word.into()),"{word}");}
        assert!(!words.contains(&"statu".into()));
    }
}
''';p.write_text(s)
print('Applied balanced concept expansion, inflected source matching and scoped caller discovery.')
