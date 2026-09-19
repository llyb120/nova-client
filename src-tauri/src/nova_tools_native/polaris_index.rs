use super::*;
use std::collections::BTreeMap;

#[derive(Clone)]
pub(super) struct CodeUnit {
    pub file:String, pub name:String, pub start:usize, pub end:usize,
    pub owner_start:usize,pub owner_end:usize,pub hash:String,pub file_hash:String,
    pub role:&'static str,pub passage:String,pub terms:HashMap<String,f64>,pub length:f64,
    pub name_terms:HashSet<String>,pub members:Vec<(String,String)>,pub calls:HashSet<String>,pub events:Vec<(String,String)>,pub commands:HashSet<String>,
    pub source:Arc<Vec<String>>,pub imports:Arc<Vec<ImportRef>>,
}
#[derive(Clone)]
struct Entry {stamp:(u64,u128),units:Vec<Arc<CodeUnit>>,bytes:usize}
#[derive(Clone,Default)]
pub(super) struct Corpus {pub units:Arc<Vec<Arc<CodeUnit>>>,pub df:Arc<HashMap<String,usize>>,pub average:f64,pub partial:bool,pub files:usize,pub changed:usize}
#[derive(Default)]
struct Cache {entries:BTreeMap<String,Entry>,snapshot:Option<Corpus>}
static CORPORA:OnceLock<Mutex<BTreeMap<String,Arc<Mutex<Cache>>>>>=OnceLock::new();
pub(super) fn digest(bytes:&[u8])->String {format!("{:x}",Sha256::digest(bytes))}
pub(super) fn cache_file(root:&Path)->PathBuf {cache_path(root).with_file_name("polaris-vectors-v1.json")}
fn safe_file(root:&Path,rel:&str)->bool {root.join(rel).canonicalize().ok().is_some_and(|p|p.starts_with(root))}
fn file_role(file:&str)->&'static str {
    let l=file.to_ascii_lowercase();
    if l.ends_with(".md"){"documentation"}else if l.contains("/tests/")||l.contains("/test/")||l.contains(".test.")||l.contains(".spec.")||l.ends_with("/tests.rs")||l.starts_with("bench/")||l.starts_with("tests/")||l.starts_with("test/"){"test"}else{"implementation"}
}
fn references(text:&str)->(HashSet<String>,Vec<(String,String)>,HashSet<String>,Vec<(String,String)>){
    static CALLS:OnceLock<Regex>=OnceLock::new();static EVENTS:OnceLock<Regex>=OnceLock::new();static MEMBERS:OnceLock<Regex>=OnceLock::new();static RUST_CALLS:OnceLock<Regex>=OnceLock::new();static CALLBACKS:OnceLock<Regex>=OnceLock::new();
    let bounded=text.chars().take(20000).collect::<String>();
    let mut calls:HashSet<String>=CALLS.get_or_init(||Regex::new(r"\b([A-Za-z_$][A-Za-z0-9_$]*)\s*(?:<[^;{}]{0,100}>)?\s*\(").unwrap()).captures_iter(&bounded).take(256).map(|c|c[1].to_string()).collect();
    // Passing a declared function to a standard higher-order operation is a
    // source reference too: rows.map(convert) must reach convert's body.
    calls.extend(CALLBACKS.get_or_init(||Regex::new(r"\b(?:map|flatMap|filter|forEach|then|catch)\s*\(\s*([A-Za-z_$][\w$]*)\s*[,)]").unwrap())
        .captures_iter(&bounded).take(64).map(|c|c[1].to_string()));
    let mut commands=HashSet::new();let mut events=Vec::new();
    for c in EVENTS.get_or_init(||Regex::new(r#"\b(emit|listen|on|invoke)\s*(?:<[^;{}]{0,100}>)?\s*\(\s*["']([A-Za-z0-9_:/.-]{4,128})["']"#).unwrap()).captures_iter(&bounded).take(64){
        if &c[1]=="invoke"{commands.insert(c[2].to_string());}else{events.push((c[1].to_string(),c[2].to_string()));}
    }
    let mut members:Vec<(String,String)>=MEMBERS.get_or_init(||Regex::new(r"\b([A-Za-z_$][\w$]*)(?:\.|::)([A-Za-z_$][\w$]*)\s*(?:<[^;{}]{0,100}>)?\s*\(").unwrap()).captures_iter(&bounded).take(128).map(|c|(c[1].to_string(),c[2].to_string())).collect();
    members.extend(RUST_CALLS.get_or_init(||Regex::new(r"\b((?:crate|self|super)(?:::[A-Za-z_][\w]*)+)::([A-Za-z_][\w]*)\s*\(").unwrap())
        .captures_iter(&bounded).take(64).map(|c|(c[1].to_string(),c[2].to_string())));
    (calls,events,commands,members)
}
fn is_retrieval_unit(symbol:&Symbol,lines:&[String])->bool {
    match symbol.kind.strip_prefix("test:").unwrap_or(&symbol.kind) {
        "fn"|"class"|"type"=>true,
        "const"=>symbol.depth==0||(symbol.depth==1&&lines.get(symbol.ln.saturating_sub(1)).is_some_and(|l|l.contains("=>")||l.contains("function"))),
        "prop"=>{
            let first=symbol.ln.saturating_sub(1).min(lines.len());let end=symbol.end.min(first+6).min(lines.len());
            let header=lines[first..end].join("\n");header.contains("=>")||header.contains("function")
        },
        "method"=>{
            static METHOD_BODY:OnceLock<Regex>=OnceLock::new();
            let first=symbol.ln.saturating_sub(1).min(lines.len());
            let last=symbol.end.min(first+16).min(lines.len());
            let header=lines[first..last].join("\n");
            if header.trim_start().starts_with("def ")||header.trim_start().starts_with("async def "){return true;}
            METHOD_BODY.get_or_init(||Regex::new(r"(?s)^\s*(?:(?:public|private|protected|readonly|static|async|get|set|override|abstract)\s+)*\*?\s*[A-Za-z_$][\w$]*\s*(?:<[^>]*>)?\s*\([^;{}]*\)\s*(?::[^=;{}]+)?\s*\{").unwrap()).is_match(&header)
        },_=>false
    }
}
/// Reuse the query's generic software vocabulary in the opposite direction.
/// These are dictionary aliases of actual identifiers, NOT generated summaries
/// or claims that a function implements an inferred behavior. No query labels,
/// project-specific symbol names, or repository paths occur in this expansion.
fn identifier_aliases(text:&str)->Vec<String> {
    let mut words=Vec::new();let mut seen=HashSet::new();
    for token in query::tokens(text).into_iter().filter(|s|s.is_ascii()).take(24) {
        let mut variants=vec![token.clone()];
        for suffix in ["s","es","d","ed","ing"] {
            if let Some(stem)=token.strip_suffix(suffix).filter(|s|s.len()>=3) {
                variants.push(stem.to_string());
                if suffix=="ing" {variants.push(format!("{stem}e"));}
            }
        }
        for word in variants {if seen.insert(word.clone()){words.push(word);}}
    }
    // A source name like bytesToString states a conversion; annotate that
    // naming convention without inventing either input or output identifiers.
    if words.iter().any(|w|w=="to") {words.push("convert".into());}
    let seed=words.join(" ");
    let original=query::tokens(&seed).into_iter().collect::<HashSet<_>>();
    let Ok(q)=query::Query::parse(serde_json::json!({"task":seed})) else{return Vec::new();};
    q.terms.into_iter().filter(|(word,_)|!original.contains(word)).map(|(word,_)|word).collect()
}
fn make_units(file:&str,text:&str)->Vec<Arc<CodeUnit>> {
    let entry=scan_source(text,file);let source=Arc::new(text.lines().map(str::to_owned).collect::<Vec<_>>());
    let imports=Arc::new(entry.imports);let file_hash=digest(text.as_bytes());
    let tests=entry.syms.iter().filter(|s|s.kind=="mod"&&s.name=="tests").map(|s|(s.ln,s.end)).collect::<Vec<_>>();
    let mut spans=entry.syms.iter().filter(|s|is_retrieval_unit(s,&source)).map(|s|(s.name.clone(),s.ln,s.end,s.kind.clone())).collect::<Vec<_>>();
    if spans.is_empty()&&!source.is_empty(){spans.push(("<module>".into(),1,source.len(),"module".into()));}
    let module_doc=source.iter().take(16).filter(|s|s.trim_start().starts_with("//!"))
        .map(|s|s.trim_start().trim_start_matches("//!").trim()).collect::<Vec<_>>().join(" ");
    let module_doc=module_doc.chars().take(240).collect::<String>();
    let mut out=Vec::new();
    for (name,begin,finish,kind) in spans {
        let begin=begin.max(1);let finish=finish.min(source.len()).max(begin);
        if begin>source.len(){continue;}
        let mut comment=begin-1;
        while comment>0&&begin-comment<=8 {let s=source[comment-1].trim();if s.starts_with("//")||s.starts_with("/*")||s.starts_with('#')||s.starts_with('*')||s.is_empty(){comment-=1}else{break;}}
        let role=if kind.starts_with("test:")||tests.iter().any(|(a,b)|begin>=*a&&begin<=*b){"test"}else{file_role(file)};
        let kind=kind.strip_prefix("test:").unwrap_or(&kind);
        let callable=matches!(kind,"fn"|"method"|"prop");
        let names=if callable{identifier_aliases(&name)}else{Vec::new()};
        let signature=source[begin-1..(begin+11).min(finish)].join("\n");
        let signature=signature.split('{').next().unwrap_or("").chars().take(700).collect::<String>();
        let signature_aliases=if callable{identifier_aliases(&signature)}else{Vec::new()};
        for offset in (begin..=finish).step_by(64) {
            let end=(offset+79).min(finish);let start=if offset==begin{comment+1}else{offset};
            let prefix=source[comment..begin].join("\n");
            let body=source[start-1..end].join("\n");
            let words=query::tokens(&name);let mut name_terms=words.iter().cloned().collect::<HashSet<_>>();
            name_terms.extend(names.iter().cloned());
            let comments=body.lines().filter(|l|{let l=l.trim();l.starts_with("//")||l.starts_with("/*")||l.starts_with('#')||l.starts_with('*')}).collect::<Vec<_>>().join("\n");
            // Do not let a long dictionary glossary/path consume the encoder's
            // token budget before it sees the actual behavior and source comments.
            let passage=format!("Defined {kind}: {name} ({})\n{}\n{}\n{}\nCode:\n{}\nFile: {file}",
                words.join(" "),prefix.chars().take(250).collect::<String>(),module_doc,
                comments.chars().take(350).collect::<String>(),body.chars().take(2200).collect::<String>()).chars().take(3400).collect::<String>();
            let hash=digest(passage.as_bytes());let mut terms=HashMap::<String,f64>::new();
            for (field,weight) in [(name.as_str(),4.0),(file,1.5),(prefix.as_str(),2.0),(module_doc.as_str(),1.0),(body.as_str(),1.0)]{
                for term in query::tokens(&field.chars().take(6000).collect::<String>()){if terms.len()<2048||terms.contains_key(&term){*terms.entry(term).or_default()+=weight;}}
            }
            // Recognized source-level fallback idioms carry default semantics
            // even when a developer did not spell the word default in a comment.
            if ["unwrap_or(","unwrap_or_else(","unwrap_or_default("," ?? "].iter().any(|s|body.contains(s)) {
                for term in ["default","fallback","默认","缺省"] {*terms.entry(term.into()).or_default()+=1.5;}
            }
            for term in &names {*terms.entry(term.clone()).or_default()+=4.0;}
            for term in &signature_aliases {*terms.entry(term.clone()).or_default()+=1.25;}
            for tf in terms.values_mut(){*tf=(*tf).min(12.0);}
            let length=terms.values().sum::<f64>().max(1.0);
            let (calls,events,commands,members)=references(&body);
            out.push(Arc::new(CodeUnit{file:file.into(),name:name.clone(),start,end,owner_start:begin,owner_end:finish,hash,file_hash:file_hash.clone(),role,passage,terms,length,name_terms,members,calls,events,commands,source:source.clone(),imports:imports.clone()}));
            if end==finish{break;}
        }
    }out
}
/// A complete fresh file walk honours .gitignore, including newly created/deleted files.
/// Only changed file contents are parsed; final evidence is independently rehashed.
pub(super) fn corpus(root:&Path,deadline:Instant)->Result<Corpus,String>{
    let root=root.canonicalize().map_err(|e|e.to_string())?;let key=normalize_root(&root);
    let slot={let mut roots=CORPORA.get_or_init(||Mutex::new(BTreeMap::new())).lock().map_err(|_|"索引锁失效")?;
        if !roots.contains_key(&key)&&roots.len()>=2 {if let Some(k)=roots.keys().next().cloned(){roots.remove(&k);}}
        roots.entry(key).or_default().clone()};
    let mut cache=slot.try_lock().map_err(|_|"同一仓库正在更新索引，请稍后重试")?;
    let mut walker=ignore::WalkBuilder::new(&root);walker.hidden(false).require_git(false).follow_links(false);
    walker.filter_entry(|e| !e.file_type().is_some_and(|f|f.is_dir())||!matches!(e.file_name().to_str(),Some(".git"|"node_modules"|"target"|"dist"|"vendor"|".venv"|"coverage"|".codegraph")));
    let mut paths=Vec::new();let mut partial=false;
    for result in walker.build(){if Instant::now()>deadline{partial=true;break;}let Ok(e)=result else{partial=true;continue;};if !e.file_type().is_some_and(|t|t.is_file()){continue;}
        let Some(file)=e.path().strip_prefix(&root).ok().and_then(|p|p.to_str()).map(|p|p.replace('\\',"/")) else{continue;};
        if is_searchable_implementation_file(&file){paths.push(file);if paths.len()>=8000{partial=true;break;}}
    }
    paths.sort();paths.dedup();let mut found=HashSet::new();let mut bytes=0;let mut changed=0;
    for file in paths {if Instant::now()>deadline{partial=true;break;}
        if !safe_file(&root,&file){continue;}let path=root.join(&file);let Some(stamp)=metadata_stamp(&path)else{partial=true;continue;};
        bytes+=stamp.0 as usize;if bytes>64*1024*1024{partial=true;break;}
        found.insert(file.clone());
        if cache.entries.get(&file).is_some_and(|e|e.stamp==stamp){continue;}
        match fs::read_to_string(&path){Ok(text) if metadata_stamp(&path)==Some(stamp)=>{
            let units=make_units(&file,&text);cache.entries.insert(file,Entry{stamp,units,bytes:text.len()});changed+=1;
        },_=>{cache.entries.remove(&file);partial=true;}}
    }
    // Never expose a removed/unvisited file from a previous snapshot.
    let before=cache.entries.len();cache.entries.retain(|p,_|found.contains(p));
    if changed==0&&before==cache.entries.len()&&!partial {
        if let Some(snapshot)=&cache.snapshot {if !snapshot.partial {let mut out=snapshot.clone();out.changed=0;return Ok(out);}}
    }
    let mut units=Vec::new();let mut df=HashMap::new();
    for e in cache.entries.values(){let _=e.bytes;for u in &e.units{if units.len()>=50000{partial=true;break;}units.push(u.clone());}}
    for u in &units{for t in u.terms.keys(){*df.entry(t.clone()).or_default()+=1;}}
    let average=(units.iter().map(|u|u.length).sum::<f64>()/units.len().max(1) as f64).max(1.0);
    let out=Corpus{units:Arc::new(units),df:Arc::new(df),partial,files:cache.entries.len(),changed,average};
    cache.snapshot=Some(out.clone());Ok(out)
}
pub(super) fn verified(root:&Path,u:&CodeUnit)->bool {safe_file(root,&u.file)&&fs::read(root.join(&u.file)).ok().is_some_and(|b|digest(&b)==u.file_hash)}
/// An explicitly named file may be configuration or deliberately ignored.
/// This is a targeted read, never an expansion outside the repository.
pub(super) fn explicit_units(root:&Path,files:&[String],known:&HashSet<String>,deadline:Instant)->Vec<Arc<CodeUnit>> {
    let mut out=Vec::new();
    for file in files {
        if known.contains(file)||Instant::now()>deadline||!safe_file(root,file){continue;}
        let path=root.join(file);
        if fs::metadata(&path).ok().is_none_or(|m|m.len()>2*1024*1024){continue;}
        if let Ok(text)=fs::read_to_string(path){out.extend(make_units(file,&text));}
    }out
}
pub(super) fn invalidate(root:&Path, files:&HashSet<String>){
    let slot=CORPORA.get().and_then(|roots|roots.lock().ok().and_then(|all|all.get(&normalize_root(root)).cloned()));
    if let Some(slot)=slot {if let Ok(mut c)=slot.lock(){c.entries.retain(|f,_|!files.contains(f));c.snapshot=None;}}
}

#[cfg(test)]
mod glossary_tests {
    use super::*;
    #[test]
    fn generic_identifier_glossary_does_not_confuse_opposite_operations() {
        let encrypt=identifier_aliases("encryptPayload");
        assert!(encrypt.contains(&"加密".into()));assert!(!encrypt.contains(&"解密".into()));
        let decrypt=identifier_aliases("decryptPayload");
        assert!(decrypt.contains(&"解密".into()));assert!(!decrypt.contains(&"加密".into()));
        let queue=identifier_aliases("removeQueuedPrompts");
        assert!(queue.contains(&"队列".into()));assert!(queue.contains(&"发送".into()));
    }
    #[test]
    fn glossary_is_metadata_and_never_changes_original_source() {
        let src="pub fn encrypt_payload(request_id: &str) { encrypt(request_id); }\n";
        let units=make_units("src/crypto.rs",src);
        let unit=units.iter().find(|u|u.name=="encrypt_payload").unwrap();
        assert!(unit.passage.contains("Defined fn: encrypt_payload"));
        assert!(unit.name_terms.contains("加密"));
        assert_eq!(unit.source.join("\n"),src.trim_end());
        assert_eq!(unit.file_hash,digest(src.as_bytes()));
    }
    #[test]
    fn first_line_of_block_documentation_is_not_dropped() {
        let source="/** Configuration never inherits another workspace.\n * Explicit changes remain local. */\nexport function isolated() { return false; }\n";
        let units=make_units("src/state.ts",source);
        let unit=units.iter().find(|u|u.name=="isolated").unwrap();
        assert!(unit.passage.contains("never inherits another workspace"));
        assert_eq!(unit.source.join("\n"),source.trim_end());
    }

}
