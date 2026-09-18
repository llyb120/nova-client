//! UI-only, bounded transcript views. The authoritative history and provider /
//! export APIs remain lossless. No thumbnail or abbreviated text is written back.
use crate::{AppState, threads::{Item, Thread, PromptImage}, transcript::{HistoryStats, TranscriptItems}, history_assets::{AssetStore, AssetInfo}};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{sync::{Arc, OnceLock}, time::Instant};
use tauri::{AppHandle, Manager};
use base64::Engine;

const PAGE_ITEMS: usize = 80;
const PAGE_BYTES: usize = 512 * 1024;
const TEXT_BYTES: usize = 24 * 1024;
const VALUE_BYTES: usize = 12 * 1024;

#[derive(Default, Clone, Deserialize)]
#[serde(rename_all="camelCase", deny_unknown_fields)]
pub struct PageRequest {
    pub cursor: Option<String>,
    pub direction: Option<String>,
    pub around_id: Option<u64>,
    pub limit: Option<usize>,
    pub byte_limit: Option<usize>,
}
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all="camelCase")]
pub struct HistoryPage {
    pub thread: Value,
    pub generation: String,
    pub start: usize,
    pub end: usize,
    pub total_items: usize,
    pub turn_offset: usize,
    pub before_cursor: Option<String>,
    pub after_cursor: Option<String>,
    pub stats: HistoryStats,
    pub prefix_stats: HistoryStats,
    pub suffix_stats: HistoryStats,
    pub payload_bytes: usize,
    pub elapsed_ms: f64,
}
fn bounded(text:&str, limit:usize) -> &str {
    let mut end=text.len().min(limit);while !text.is_char_boundary(end){end-=1;}&text[..end]
}
fn picture_ref(thread:&Thread,id:u64,kind:&str,index:usize)->String {
    format!("nova-history://{}/{}/{id}/{kind}/{index}",thread.id,thread.items.generation())
}
fn markdown_images() -> &'static regex::Regex {
    static RE:OnceLock<regex::Regex>=OnceLock::new();
    // Preserve the original Markdown outside the URL. Complex unsupported
    // syntax still remains in full item details rather than guessed file paths.
    RE.get_or_init(||regex::Regex::new(r"!\[[^\]\n]*\]\((?:<([^>\n]+)>|([^\s\)]+))").unwrap())
}
fn display_text(thread:&Thread,id:u64,text:&str,limit:usize)->String {
    let mut output=String::new();let mut pos=0;
    for (index,capture) in markdown_images().captures_iter(text).enumerate() {
        let source=capture.get(1).or_else(||capture.get(2)).unwrap();
        if output.len()>=limit {break;}
        output.push_str(bounded(&text[pos..source.start()],limit-output.len()));
        if output.len()>=limit {break;}
        // Only local/data sources need native derivatives. Remote sources keep
        // their original URL, with the same bounded frontend cache/queue.
        let raw=source.as_str();
        if raw.starts_with("data:image/") || raw.starts_with("file://") || std::path::Path::new(raw).is_absolute()
            || (raw.len()>2 && raw.as_bytes()[1]==b':') {
            output.push_str(&picture_ref(thread,id,"m",index));
        } else {output.push_str(bounded(raw,limit-output.len()));}
        pos=source.end();
    }
    if output.len()<limit {output.push_str(bounded(&text[pos..],limit-output.len()));}
    output
}
/// A bounded, allocation-limited JSON display copy. It never serializes large
/// raw values first merely to discover they exceed the UI budget.
fn display_value(value:&Value,budget:&mut usize,depth:usize,deferred:&mut bool)->Value {
    if *budget<64 || depth>12 {*deferred=true;return json!("[详情按需加载]");}
    *budget-=32;
    match value {
        Value::String(s)=>{let text=bounded(s,(*budget).min(VALUE_BYTES));*deferred|=text.len()!=s.len();*budget=budget.saturating_sub(text.len());json!(text)},
        Value::Array(a)=>{let mut out=Vec::new();for v in a {if *budget<64 {*deferred=true;break;}out.push(display_value(v,budget,depth+1,deferred));}Value::Array(out)},
        Value::Object(o)=>{
            if o.get("type").and_then(Value::as_str)==Some("image") && o.get("data").and_then(Value::as_str).is_some() {
                *deferred=true;return json!({"type":"text","text":"[原图保留，展开详情查看图片]"});
            }
            let mut out=serde_json::Map::new();for (key,v) in o {if *budget<64 {*deferred=true;break;}if key.len()>256 {*deferred=true;continue;}
                *budget=budget.saturating_sub(key.len());out.insert(key.clone(),display_value(v,budget,depth+1,deferred));}Value::Object(out)
        },_=>value.clone(),
    }
}
pub fn project_item(thread:&Thread,item:&Item,index:usize,full:bool)->Value {
    let mut deferred=false;
    let text_limit=if full {usize::MAX} else {TEXT_BYTES};
    let text_view=|text:&str|display_text(thread,item.id(),text,text_limit);
    let mut value=match item {
        Item::User{id,text,ts,images}=>{
            deferred=text.len()>text_limit || (!full && images.len()>64);
            let images:Vec<Value>=images.iter().take(if full {usize::MAX}else{64}).enumerate().map(|(i,image)|json!({
                "name":bounded(&image.name,512),"mimeType":image.mime_type,"size":image.size,
                // Short source identity, not a multi-megabyte data URL. Original
                // resolution is deferred until this image is actually visible.
                "uri":picture_ref(thread,*id,"u",i)
            })).collect();
            json!({"type":"user","id":id,"ts":ts,"text":bounded(text,text_limit),"images":images})
        },
        Item::Assistant{id,text,ts}=>{deferred=text.len()>text_limit;json!({"type":"assistant","id":id,"ts":ts,"text":text_view(text)})},
        Item::Thought{id,text,ts}=>{deferred=text.len()>text_limit;json!({"type":"thought","id":id,"ts":ts,"text":text_view(text)})},
        Item::System{id,text,ts,level}=>{deferred=text.len()>text_limit;json!({"type":"system","id":id,"ts":ts,"text":bounded(text,text_limit),"level":level})},
        Item::Turn{..}=>serde_json::to_value(item).expect("serializable turn"),
        Item::Tool{id,ts,call}=>{
            let mut budget=if full {usize::MAX} else {VALUE_BYTES};
            let content:Vec<Value>=call.content.iter().take(if full{usize::MAX}else{128}).map(|v|display_value(v,&mut budget,0,&mut deferred)).collect();
            deferred|=content.len()!=call.content.len();
            let locations:Vec<Value>=call.locations.iter().take(128).map(|v|display_value(v,&mut budget,0,&mut deferred)).collect();
            deferred|=locations.len()!=call.locations.len();
            let raw_input=call.raw_input.as_ref().map(|v|display_value(v,&mut budget,0,&mut deferred));
            let raw_output=call.raw_output.as_ref().map(|v|display_value(v,&mut budget,0,&mut deferred));
            json!({"type":"tool","id":id,"ts":ts,"toolCallId":call.tool_call_id,"title":bounded(&call.title,2048),"kind":call.kind,
                "status":call.status,"content":content,"locations":locations,"rawInput":raw_input,"rawOutput":raw_output})
        },
    };
    value["historyIndex"]=json!(index);
    if deferred {value["detailDeferred"]=json!(true);value["sourceBytes"]=json!(crate::transcript::item_stats(item).estimated_bytes);}
    value
}
fn header(thread:&Thread,items:Vec<Value>)->Value {
    let mut budget=16*1024;let mut deferred=false;
    json!({"id":thread.id,"title":bounded(&thread.title,4096),"cwd":thread.cwd,"agentKind":thread.agent_kind,
        "model":thread.model,"mode":thread.mode,"reasoningEffort":thread.reasoning_effort,
        "createdAt":thread.created_at,"updatedAt":thread.updated_at,"starred":thread.starred,"ephemeral":thread.ephemeral,
        "roamingRole":thread.roaming_role,"roamingPeer":thread.roaming_peer,"roamingPeerName":thread.roaming_peer_name,
        "quotaPeer":thread.quota_peer,"quotaPeerName":thread.quota_peer_name,"worktree":thread.worktree,
        "parentThreadId":thread.parent_thread_id,"stageSourceThreadId":thread.stage_source_thread_id,
        "activeClueCardId":thread.active_clue_card_id,"items":items,
        "plan":thread.plan.as_ref().map(|v|display_value(v,&mut budget,0,&mut deferred))})
}
fn cursor(items:&TranscriptItems,index:usize)->String {format!("{}:{index}",items.generation())}
fn parse_cursor(items:&TranscriptItems,raw:&str)->Result<usize,String> {
    let (generation,index)=raw.rsplit_once(':').ok_or("历史游标无效")?;
    if generation!=items.generation(){return Err("HISTORY_CHANGED: 历史已恢复或编辑，请重新加载".into());}
    let index:usize=index.parse().map_err(|_|"历史游标无效")?;
    if index>items.len(){return Err("HISTORY_CHANGED: 历史范围已变化".into());}Ok(index)
}
pub fn page(thread:&Thread,request:PageRequest)->Result<HistoryPage,String> {
    let started=Instant::now();let len=thread.items.len();
    let limit=request.limit.unwrap_or(PAGE_ITEMS).clamp(1,128);
    let budget=request.byte_limit.unwrap_or(PAGE_BYTES).clamp(4096,1024*1024);
    let direction=request.direction.as_deref().unwrap_or("before");
    if !matches!(direction,"before"|"after"){return Err("历史分页方向无效".into());}
    let around=match request.around_id {Some(id)=>Some(thread.items.iter().position(|i|i.id()==id).ok_or("消息不存在或已恢复")?),None=>None};
    if around.is_some()&&request.cursor.is_some(){return Err("aroundId 与 cursor 不能同时使用".into());}
    let boundary=if let Some(index)=around {(index+limit/2).min(len)} else if let Some(c)=&request.cursor {parse_cursor(&thread.items,c)?} else {len};
    let (mut start,mut end)=(boundary,boundary);let mut bytes=0;let mut selected=Vec::new();
    for i in 0..limit {
        let index=if direction=="after" && around.is_none() {boundary+i} else {let Some(index)=boundary.checked_sub(i+1)else{break};index};
        let Some(item)=thread.items.get(index) else{break;};
        let mut value=project_item(thread,item,index,false);
        // A single message can exceed a page (e.g. thousands of attachments).
        // Return an explicit deferred stub, never an oversized first page.
        let mut size=serde_json::to_vec(&value).map_err(|e|e.to_string())?.len();
        if selected.is_empty() && size>budget {
            if let Some(images)=value.get_mut("images").and_then(Value::as_array_mut){images.truncate(4);}
            for key in ["text","content","rawInput","rawOutput","locations"] {if value.get(key).is_some(){value[key]=match key {"text"=>json!(bounded(value[key].as_str().unwrap_or(""),512)),"content"|"locations"=>json!([]),_=>Value::Null};}}
            value["detailDeferred"]=json!(true);size=serde_json::to_vec(&value).map_err(|e|e.to_string())?.len();
        }
        if bytes+size>budget {break;}
        bytes+=size;selected.push(value);start=start.min(index);end=end.max(index+1);
    }
    selected.sort_by_key(|i|i["historyIndex"].as_u64().unwrap_or(0));
    let stats=thread.items.stats();let prefix=thread.items.stats_before(start);let through=thread.items.stats_before(end);
    let suffix=HistoryStats{items:stats.items-through.items,users:stats.users-through.users,turns:stats.turns-through.turns,
        total_tokens:stats.total_tokens.saturating_sub(through.total_tokens),input_tokens:stats.input_tokens.saturating_sub(through.input_tokens),output_tokens:stats.output_tokens.saturating_sub(through.output_tokens),
        cache_read_tokens:stats.cache_read_tokens.saturating_sub(through.cache_read_tokens),cache_write_tokens:stats.cache_write_tokens.saturating_sub(through.cache_write_tokens),..Default::default()};
    Ok(HistoryPage{thread:header(thread,selected),generation:thread.items.generation().into(),start,end,total_items:len,turn_offset:prefix.users,
        before_cursor:(start>0).then(||cursor(&thread.items,start)),after_cursor:(end<len).then(||cursor(&thread.items,end)),stats,prefix_stats:prefix,suffix_stats:suffix,payload_bytes:bytes,elapsed_ms:started.elapsed().as_secs_f64()*1000.0})
}
fn snapshot(app:&AppHandle,id:&str)->Result<Thread,String> {
    let state=app.state::<AppState>();let store=state.store.lock().unwrap();store.get(id).cloned().ok_or_else(||"线程不存在".into())
}
#[tauri::command]
pub async fn get_thread_page(app:AppHandle,thread_id:String,request:Option<PageRequest>)->Result<HistoryPage,String> {
    tauri::async_runtime::spawn_blocking(move||{let thread=snapshot(&app,&thread_id)?;page(&thread,request.unwrap_or_default())}).await.map_err(|e|e.to_string())?
}
#[tauri::command]
pub async fn get_thread_display_items(app:AppHandle,thread_id:String,ids:Vec<u64>,generation:String)->Result<Value,String> {
    if ids.len()>128{return Err("一次最多刷新128条消息".into());}
    tauri::async_runtime::spawn_blocking(move||{
        let thread=snapshot(&app,&thread_id)?;
        if generation!=thread.items.generation(){return Err("HISTORY_CHANGED: 历史已变化".into());}
        let wanted:std::collections::HashSet<u64>=ids.into_iter().collect();let mut result=Vec::new();
        // Most streaming updates are in the last chunk; scan from the tail and
        // stop as soon as all requested identities have been found.
        for (index,item) in thread.items.iter().enumerate().rev() {
            if wanted.contains(&item.id()){result.push(project_item(&thread,item,index,false));if result.len()==wanted.len(){break;}}
        }
        result.sort_by_key(|v|v["historyIndex"].as_u64().unwrap_or(0));
        Ok(json!({"generation":generation,"items":result,"totalItems":thread.items.len(),"stats":thread.items.stats()}))
    }).await.map_err(|e|e.to_string())?
}
#[tauri::command]
pub async fn get_thread_item_detail(app:AppHandle,thread_id:String,item_id:u64)->Result<Value,String> {
    tauri::async_runtime::spawn_blocking(move||{
        let thread=snapshot(&app,&thread_id)?;
        let (index,item)=thread.items.iter().enumerate().find(|(_,i)|i.id()==item_id).ok_or("消息不存在")?;
        // Explicit details are lossless text/JSON. User originals are addressed
        // by refs so opening a text editor does not decode every attachment.
        let mut value=serde_json::to_value(item).map_err(|e|e.to_string())?;
        if let Item::User{..}=item {value=project_item(&thread,item,index,true);}
        value["historyIndex"]=json!(index);Ok(value)
    }).await.map_err(|e|e.to_string())?
}
#[tauri::command]
pub async fn get_thread_outline(app:AppHandle,thread_id:String)->Result<Value,String> {
    tauri::async_runtime::spawn_blocking(move||{
        let thread=snapshot(&app,&thread_id)?;
        let prompts:Vec<Value>=thread.items.iter().enumerate().filter_map(|(index,i)|match i{Item::User{id,text,..}=>Some(json!({"id":id,"text":text,"index":index})),_=>None}).collect();
        Ok(json!({"generation":thread.items.generation(),"prompts":prompts,"stats":thread.items.stats()}))
    }).await.map_err(|e|e.to_string())?
}
fn resolve_picture(app:&AppHandle,reference:&str)->Result<PromptImage,String> {
    let parts:Vec<&str>=reference.strip_prefix("nova-history://").ok_or("附件引用无效")?.split('/').collect();
    if parts.len()!=5 {return Err("附件引用无效".into());}
    let thread=snapshot(app,parts[0])?;
    if thread.items.generation()!=parts[1]{return Err("历史已变化，请重新打开图片".into());}
    let id:u64=parts[2].parse().map_err(|_|"附件消息ID无效")?;
    let index:usize=parts[4].parse().map_err(|_|"附件序号无效")?;
    let item=thread.items.iter().find(|i|i.id()==id).ok_or("图片所属消息不存在")?;
    if parts[3]=="u" {if let Item::User{images,..}=item {return images.get(index).cloned().ok_or_else(||"附件不存在".into());}}
    if parts[3]=="m" {
        let text=match item {Item::Assistant{text,..}|Item::Thought{text,..}=>text,_=>return Err("消息不是图片来源".into())};
        let captures=markdown_images().captures_iter(text).nth(index).ok_or("图片引用已变化")?;
        let source=captures.get(1).or_else(||captures.get(2)).unwrap().as_str();
        if let Some(data)=source.strip_prefix("data:") {
            let (mime,data)=data.split_once(";base64,").ok_or("图片编码无效")?;
            return Ok(PromptImage{name:"生成图片".into(),mime_type:mime.into(),data:Some(data.into()),uri:None,size:None});
        }
        let path=if source.starts_with("file://"){crate::threads::file_uri_to_local_path(source).ok_or("图片路径无效")?}else{source.into()};
        if !std::path::Path::new(&path).is_absolute() {return Err("图片不是绝对路径".into());}
        return Ok(PromptImage{name:"生成图片".into(),mime_type:"image/png".into(),data:None,uri:Some(crate::history_assets::file_uri(std::path::Path::new(&path))),size:None});
    }
    Err("图片引用类型无效".into())
}
pub fn resolve_images(app:&AppHandle,images:Vec<PromptImage>)->Result<Vec<PromptImage>,String> {
    images.into_iter().map(|image|match image.uri.as_deref() {Some(uri) if uri.starts_with("nova-history://")=>resolve_picture(app,uri),_=>Ok(image)}).collect()
}
#[tauri::command]
pub async fn get_history_image(app:AppHandle,reference:String,max_edge:Option<u32>,original:Option<bool>)->Result<AssetInfo,String> {
    static SEM:OnceLock<Arc<tokio::sync::Semaphore>>=OnceLock::new();
    let permit=SEM.get_or_init(||Arc::new(tokio::sync::Semaphore::new(2))).clone().acquire_owned().await.map_err(|e|e.to_string())?;
    tauri::async_runtime::spawn_blocking(move||{
        let _permit=permit;let image=resolve_picture(&app,&reference)?;
        let assets=AssetStore::new(&crate::nova_data_dir(&app));
        let info=if let Some(info)=assets.image_metadata(&image){info}else{
            let bytes=if let Some(data)=&image.data{base64::engine::general_purpose::STANDARD.decode(data).map_err(|e|e.to_string())?}else{
                let path=crate::threads::file_uri_to_local_path(image.uri.as_deref().ok_or("原始附件不存在")?).ok_or("原始附件路径无效")?;
                if std::fs::metadata(&path).map_err(|e|e.to_string())?.len()>128*1024*1024{return Err("图片过大，请直接打开原文件".into());}
                std::fs::read(path).map_err(|e|e.to_string())?
            };assets.store(&bytes,&image.mime_type)?
        };
        if original.unwrap_or(false){Ok(info)}else{assets.thumbnail(&info.attachment_id,max_edge.unwrap_or(480))}
    }).await.map_err(|e|e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    fn thread(n:usize)->Thread {let mut t=Thread::new("/test".into(),crate::threads::AgentKind::Devin,None,None,None,false);for i in 0..n {t.items.push(Item::Assistant{id:i as u64,text:"history".repeat(400),ts:0});}t}
    #[test] fn pages_are_bounded_and_cover_history_without_overlap() {
        let t=thread(10000);let mut req=PageRequest::default();let mut seen=std::collections::HashSet::new();
        loop {let p=page(&t,req).unwrap();assert!(p.payload_bytes<=PAGE_BYTES);assert!(p.end-p.start<=128);
            for item in p.thread["items"].as_array().unwrap(){assert!(seen.insert(item["id"].as_u64().unwrap()));}
            let Some(cursor)=p.before_cursor else{break;};req=PageRequest{cursor:Some(cursor),..Default::default()};}
        assert_eq!(seen.len(),10000);
    }
    #[test] fn inline_images_and_huge_tools_do_not_cross_ui_ipc() {
        let mut t=thread(0);t.items.push(Item::User{id:1,text:"x".repeat(200000),ts:0,images:vec![PromptImage{name:"large.png".into(),mime_type:"image/png".into(),data:Some("A".repeat(10_000_000)),uri:None,size:Some(7_500_000)}]});
        let p=page(&t,PageRequest::default()).unwrap();assert!(p.payload_bytes<32*1024);
        assert_eq!(p.thread["items"][0]["detailDeferred"],true);assert!(p.thread["items"][0]["images"][0].get("data").is_none());
        assert_eq!(t.items.stats().inline_asset_bytes,10_000_000);
    }
    #[test] fn append_keeps_cursor_restore_invalidates_it() {
        let mut t=thread(300);let before=page(&t,PageRequest::default()).unwrap().before_cursor;
        t.items.push(Item::Assistant{id:301,text:"new".into(),ts:0});assert!(page(&t,PageRequest{cursor:before.clone(),..Default::default()}).is_ok());
        t.items.truncate(250);assert!(page(&t,PageRequest{cursor:before,..Default::default()}).unwrap_err().contains("HISTORY_CHANGED"));
    }
}
