//! Incremental durable history manifests. Unchanged chunks are neither cloned,
//! encoded nor rewritten. First migration keeps the old JSON as a backup.
use crate::{history_assets::{atomic_write,hash,valid_hash,AssetStore},threads::Thread,transcript::{Chunk,TranscriptItems}};
use serde::{Deserialize,Serialize};
use std::{collections::HashMap,fs,path::{Path,PathBuf},sync::Arc};

const FORMAT:&str="nova-history-chunks-v1";
#[derive(Serialize,Deserialize)]
struct Manifest {format:String,thread:Thread,chunks:Vec<String>,item_count:usize}
#[derive(Default,Debug)]
pub struct WriteStats {pub serialized_chunks:usize,pub written_bytes:usize}
fn chunk_path(dir:&Path,id:&str)->PathBuf {dir.join("chunks").join(format!("{id}.json"))}
fn sidecar(path:&Path,suffix:&str)->PathBuf {PathBuf::from(format!("{}{suffix}",path.display()))}

pub fn read_thread(path:&Path)->Result<Thread,String>{
    let mut cache=HashMap::new();read_cached(path,&mut cache)
}
pub fn read_cached(path:&Path,cache:&mut HashMap<String,Arc<Chunk>>)->Result<Thread,String> {
    let result=read_exact(path,cache);
    if let Err(error)=&result {
        let backup=sidecar(path,".previous");
        if backup.exists() {
            if let Ok(thread)=read_exact(&backup,cache) {eprintln!("[threads] {} 损坏，使用上一份完整提交：{error}",path.display());return Ok(thread);}
        }
    }result
}
fn read_exact(path:&Path,cache:&mut HashMap<String,Arc<Chunk>>)->Result<Thread,String>{
    let bytes=fs::read(path).map_err(|e|e.to_string())?;
    let value:serde_json::Value=serde_json::from_slice(&bytes).map_err(|e|e.to_string())?;
    if value.get("format").and_then(|v|v.as_str())!=Some(FORMAT) {
        return serde_json::from_value(value).map_err(|e|e.to_string());
    }
    let manifest:Manifest=serde_json::from_value(value).map_err(|e|e.to_string())?;
    let mut chunks=Vec::with_capacity(manifest.chunks.len());let dir=path.parent().ok_or("历史文件无父目录")?;
    for id in manifest.chunks {
        if !valid_hash(&id){return Err("历史分块引用无效".into());}
        let chunk=if let Some(c)=cache.get(&id) {c.clone()} else {
            let bytes=fs::read(chunk_path(dir,&id)).map_err(|e|format!("读取历史分块 {id}: {e}"))?;
            if hash(&bytes)!=id {return Err(format!("历史分块校验失败：{id}"));}
            let items=serde_json::from_slice(&bytes).map_err(|e|e.to_string())?;
            let c=Arc::new(Chunk::new(items));let _=c.persisted_hash.set(id.clone());cache.insert(id,c.clone());c
        };
        chunks.push(chunk);
    }
    let mut thread=manifest.thread;thread.items=TranscriptItems::from_chunks(chunks)?;
    if thread.items.len()!=manifest.item_count {return Err("历史总条数与分块不一致".into());}Ok(thread)
}

pub fn write_thread(dir:&Path,path:&Path,thread:&mut Thread)->Result<WriteStats,String>{
    let assets=AssetStore::new(dir.parent().ok_or("历史目录无父目录")?);
    let mut stats=WriteStats::default();let mut hashes=Vec::with_capacity(thread.items.chunks.len());
    for c in &mut thread.items.chunks {
        if c.stats().inline_asset_bytes>0 {
            // Expensive copying/decoding takes place on this worker. The live
            // thread adopts this normalized chunk only if its identity is unchanged.
            let mut normalized=Chunk::new(c.items.clone());
            let result=normalized.items.iter_mut().try_for_each(|item|assets.externalize_item(item));
            if result.is_ok(){*c=Arc::new(normalized);} else {eprintln!("[threads] 附件迁移稍后重试，原数据保留：{}",result.unwrap_err());}
        }
        let id=if let Some(id)=c.persisted_hash.get().filter(|id|chunk_path(dir,id).is_file()) {id.clone()} else {
            let bytes=serde_json::to_vec(&c.items).map_err(|e|e.to_string())?;let id=hash(&bytes);let file=chunk_path(dir,&id);
            stats.serialized_chunks+=1;
            if fs::read(&file).ok().as_deref().map(hash).as_deref()!=Some(&id) {atomic_write(&file,&bytes)?;stats.written_bytes+=bytes.len();}
            let _=c.persisted_hash.set(id.clone());id
        };hashes.push(id);
    }
    let mut header=thread.clone();header.items=TranscriptItems::default();
    let bytes=serde_json::to_vec(&Manifest{format:FORMAT.into(),thread:header,chunks:hashes,item_count:thread.items.len()}).map_err(|e|e.to_string())?;
    if let Ok(old)=fs::read(path) {
        if let Ok(value)=serde_json::from_slice::<serde_json::Value>(&old) {
            if value.get("format").and_then(|v|v.as_str())==Some(FORMAT) {atomic_write(&sidecar(path,".previous"),&old)?;}
            else if !sidecar(path,".pre-chunks").exists(){atomic_write(&sidecar(path,".pre-chunks"),&old)?;}
        }
    }
    // Publish the small manifest only after every referenced immutable file is durable.
    atomic_write(path,&bytes)?;stats.written_bytes+=bytes.len();Ok(stats)
}
pub fn remove_thread_files(path:&Path)->Result<(),String>{
    for p in [path.to_path_buf(),sidecar(path,".previous"),sidecar(path,".pre-chunks")] {match fs::remove_file(p) {Ok(())=>{},Err(e) if e.kind()==std::io::ErrorKind::NotFound=>{},Err(e)=>return Err(e.to_string())}}
    // Shared chunks/assets may also be referenced by worldline snapshots/trash.
    // Keep them; reclamation needs a complete reference scan, not per-thread deletion.
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;use crate::threads::{AgentKind,Item};
    fn thread()->Thread{let mut t=Thread::new("test".into(),AgentKind::Lyra,None,None,None,false);for i in 0..320{t.items.push(Item::Assistant{id:i,text:"x".repeat(10000),ts:0});}t}
    #[test] fn repeated_saves_encode_only_changed_chunks_and_keep_legacy_backup(){
        let dir=tempfile::tempdir().unwrap();let root=dir.path().join("threads");fs::create_dir(&root).unwrap();let path=root.join("a.json");let mut t=thread();
        let original=serde_json::to_vec(&t).unwrap();fs::write(&path,&original).unwrap();
        assert_eq!(write_thread(&root,&path,&mut t).unwrap().serialized_chunks,10);
        assert_eq!(fs::read(sidecar(&path,".pre-chunks")).unwrap(),original);
        assert_eq!(write_thread(&root,&path,&mut t).unwrap().serialized_chunks,0);
        if let Item::Assistant{text,..}=t.items.last_mut().unwrap(){text.push('!');}
        assert_eq!(write_thread(&root,&path,&mut t).unwrap().serialized_chunks,1);
        assert_eq!(serde_json::to_value(read_thread(&path).unwrap()).unwrap(),serde_json::to_value(&t).unwrap());
        let generation=t.items.generation().to_string();let mut restored=read_thread(&path).unwrap();assert_ne!(generation,restored.items.generation());
        restored.items.truncate(12);write_thread(&root,&path,&mut restored).unwrap();assert_eq!(read_thread(&path).unwrap().items.len(),12);
    }
    #[test] fn corrupt_manifest_recovers_previous_and_missing_chunk_never_becomes_empty_history(){
        let dir=tempfile::tempdir().unwrap();let path=dir.path().join("a.json");let mut t=thread();
        write_thread(dir.path(),&path,&mut t).unwrap();t.title="second".into();write_thread(dir.path(),&path,&mut t).unwrap();
        fs::write(&path,b"broken").unwrap();assert_eq!(read_thread(&path).unwrap().items.len(),320);
        fs::remove_dir_all(dir.path().join("chunks")).unwrap();assert!(read_thread(&path).is_err());
    }
}
