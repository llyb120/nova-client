use super::*;
use std::collections::BTreeMap;

#[derive(Clone)]
pub(super) struct CodeUnit {
    pub file:String, pub name:String, pub start:usize, pub end:usize,
    pub owner_start:usize,pub owner_end:usize,pub hash:String,pub file_hash:String,
    pub role:&'static str,pub passage:String,pub terms:HashMap<String,f64>,pub length:f64,
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
    if l.ends_with(".md"){"documentation"}else if l.contains("/tests/")||l.contains("/test/")||l.contains(".test.")||l.contains(".spec.")||l.starts_with("bench/")||l.starts_with("tests/")||l.starts_with("test/"){"test"}else{"implementation"}
}
fn make_units(file:&str,text:&str)->Vec<Arc<CodeUnit>> {
    let entry=scan_source(text,file);let source=Arc::new(text.lines().map(str::to_owned).collect::<Vec<_>>());
    let imports=Arc::new(entry.imports);let file_hash=digest(text.as_bytes());
    let tests=entry.syms.iter().filter(|s|s.kind=="mod"&&s.name=="tests").map(|s|(s.ln,s.end)).collect::<Vec<_>>();
    let mut spans=entry.syms.iter().filter(|s|matches!(s.kind.as_str(),"fn"|"method"|"class"|"type")||(s.kind=="const"&&s.depth<=1)).map(|s|(s.name.clone(),s.ln,s.end)).collect::<Vec<_>>();
    if spans.is_empty()&&!source.is_empty(){spans.push(("<module>".into(),1,source.len()));}
    let mut out=Vec::new();
    for (name,begin,finish) in spans {
        let begin=begin.max(1);let finish=finish.min(source.len()).max(begin);
        if begin>source.len(){continue;}
        let mut comment=begin-1;
        while comment>0&&begin-comment<=8 {let s=source[comment-1].trim();if s.starts_with("//")||s.starts_with('#')||s.starts_with('*')||s.is_empty(){comment-=1}else{break;}}
        let role=if tests.iter().any(|(a,b)|begin>=*a&&begin<=*b){"test"}else{file_role(file)};
        for offset in (begin..=finish).step_by(64) {
            let end=(offset+79).min(finish);let start=if offset==begin{comment+1}else{offset};
            let prefix=source[comment..begin].join("\n");
            let body=source[start-1..end].join("\n");
            let passage=format!("{file}\n{name}\n{prefix}\n{body}").chars().take(5000).collect::<String>();
            let hash=digest(passage.as_bytes());let mut terms=HashMap::<String,f64>::new();
            for (field,weight) in [(name.as_str(),4.0),(file,1.5),(prefix.as_str(),2.0),(body.as_str(),1.0)]{
                for term in query::tokens(field){*terms.entry(term).or_default()+=weight;}
            }
            for tf in terms.values_mut(){*tf=(*tf).min(12.0);}
            let length=terms.values().sum::<f64>().max(1.0);
            out.push(Arc::new(CodeUnit{file:file.into(),name:name.clone(),start,end,owner_start:begin,owner_end:finish,hash,file_hash:file_hash.clone(),role,passage,terms,length,source:source.clone(),imports:imports.clone()}));
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
