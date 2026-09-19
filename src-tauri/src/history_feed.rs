//! Small UI invalidations instead of repeated transcript/base64 snapshots.
//! The existing raw event remains untouched for roaming/remote consumers.
//! The UI fetches current authoritative items once per coalesced notification,
//! so snapshot hand-over never replays a delta already included in a page.
use serde::{Deserialize, Serialize};
use serde_json::{json,Value};
use std::{collections::{HashMap,HashSet},sync::{Arc,atomic::{AtomicBool,AtomicUsize,Ordering},mpsc},time::{Duration,Instant}};
use tauri::{AppHandle,Emitter,Listener};
const MAX_QUEUE_BYTES:usize=8*1024*1024;
const MAX_IDS:usize=256;
#[derive(Deserialize)]
struct Identity {id:u64}
#[derive(Deserialize)]
#[serde(tag="t")]
enum Op {
    #[serde(rename="upsert")] Upsert{item:Identity},
    #[serde(rename="delta",rename_all="camelCase")] Delta{item_id:u64,text:String},
    #[serde(rename="remove",rename_all="camelCase")] Remove{item_id:u64},
    #[serde(rename="plan")] Plan{plan:Value},
    #[serde(rename="mode")] Mode{mode:String},
    #[serde(rename="usage")] Usage{usage:Value},
    #[serde(rename="proposed_plan")] ProposedPlan{text:Option<String>},
}
#[derive(Deserialize)]
#[serde(rename_all="camelCase")]
struct Update {thread_id:String,op:Option<Op>,ops:Option<Vec<Op>>}
#[derive(Default,Serialize)]
#[serde(rename_all="camelCase")]
struct Notice {ids:HashSet<u64>,removed:HashSet<u64>,ops:Vec<Value>,chars:usize,reset:bool}
impl Notice {
    fn apply(&mut self,op:Op) {
        match op {
            Op::Upsert{item}=>{self.ids.insert(item.id);},
            Op::Delta{item_id,text}=>{self.ids.insert(item_id);self.chars=self.chars.saturating_add(text.chars().count());},
            Op::Remove{item_id}=>{self.removed.insert(item_id);self.reset=true;},
            Op::Plan{plan}=>self.ops.push(json!({"t":"plan","plan":plan})),
            Op::Mode{mode}=>self.ops.push(json!({"t":"mode","mode":mode})),
            Op::Usage{usage}=>self.ops.push(json!({"t":"usage","usage":usage})),
            Op::ProposedPlan{text}=>self.ops.push(json!({"t":"proposed_plan","text":text})),
        }
        if self.ids.len()>MAX_IDS || self.ops.len()>32 {self.ids.clear();self.ops.clear();self.reset=true;}
    }
}
pub fn register(app:&AppHandle) {
    let (tx,rx)=mpsc::sync_channel::<String>(64);
    let pending=Arc::new(AtomicUsize::new(0));let overflow=Arc::new(AtomicBool::new(false));
    let amount=pending.clone();let dropped=overflow.clone();
    app.listen(crate::acp::EV_UPDATE,move|event|{
        let bytes=event.payload().len();
        if bytes>MAX_QUEUE_BYTES || amount.fetch_add(bytes,Ordering::AcqRel)+bytes>MAX_QUEUE_BYTES {
            if bytes<=MAX_QUEUE_BYTES{amount.fetch_sub(bytes,Ordering::AcqRel);}dropped.store(true,Ordering::Release);return;
        }
        if tx.try_send(event.payload().to_owned()).is_err(){amount.fetch_sub(bytes,Ordering::AcqRel);dropped.store(true,Ordering::Release);}
    });
    let app=app.clone();
    std::thread::Builder::new().name("history-display-feed".into()).spawn(move||{
        let mut batches:HashMap<String,Notice>=HashMap::new();let mut last=Instant::now();
        loop {
            match rx.recv_timeout(Duration::from_millis(25)) {
                Ok(payload)=>{
                    pending.fetch_sub(payload.len(),Ordering::AcqRel);
                    // Serde discards unrequested item fields without allocating
                    // their base64 strings or nested tool payloads.
                    if let Ok(update)=serde_json::from_str::<Update>(&payload) {
                        let batch=batches.entry(update.thread_id).or_default();
                        for op in update.ops.into_iter().flatten().chain(update.op){batch.apply(op);}
                    }else{overflow.store(true,Ordering::Release);}
                },
                Err(mpsc::RecvTimeoutError::Disconnected)=>break,
                Err(mpsc::RecvTimeoutError::Timeout)=>{},
            }
            if last.elapsed()>=Duration::from_millis(40) {
                for (thread_id,notice) in batches.drain(){let _=app.emit("acp:history",json!({"threadId":thread_id,"notice":notice}));}
                if overflow.swap(false,Ordering::AcqRel){let _=app.emit("acp:history",json!({"resync":true}));}
                last=Instant::now();
            }
        }
    }).expect("history display worker");
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn pictures_are_not_deserialized_or_forwarded_to_the_ui() {
        let payload=json!({"threadId":"a","op":{"t":"upsert","item":{"type":"user","id":7,"images":[{"data":"X".repeat(2000000)}]}}}).to_string();
        let update:Update=serde_json::from_str(&payload).unwrap();let mut notice=Notice::default();notice.apply(update.op.unwrap());
        assert!(notice.ids.contains(&7));assert!(serde_json::to_vec(&notice).unwrap().len()<200);
    }
    #[test] fn bounded_invalidation_resyncs_instead_of_losing_history(){let mut n=Notice::default();for id in 0..300{n.apply(Op::Upsert{item:Identity{id}});}assert!(n.reset);assert!(n.ids.len()<=MAX_IDS);}
}
