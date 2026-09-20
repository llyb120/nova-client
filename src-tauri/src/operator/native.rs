use super::{Model,ModelFuture,Reply};
use crate::lyra::{config::Resolved,provider::stream_chat,tools::Tool};
use serde_json::{json,Value};
use base64::Engine;
use std::sync::{Arc,atomic::AtomicBool};

pub(super) struct NativeModel {http:reqwest::Client, resolved:Resolved, id:String}
impl NativeModel {pub fn new(http:reqwest::Client,resolved:Resolved)->Self{Self{http,resolved,id:format!("operator-model-{}",uuid::Uuid::new_v4())}}}

/// Never follow paths from a model decision. frame.currentObservations is assembled
/// exclusively from native tool receipts; missing images are reported, never silently
/// replaced with old pictures. The metadata ties each image to its own coordinate space.
pub(crate) async fn image_parts(frame:&Value) -> Result<Vec<Value>,String> {
    let mut parts=Vec::new();let mut total=0usize;
    if let Some(observations)=frame["currentObservations"].as_object(){
        for (tool,result) in observations {
            if result["historical"]==true || result["operatorOrdinal"].as_u64().unwrap_or(0)<frame["lastEffectOrdinal"].as_u64().unwrap_or(0) {continue;}
            if let Some(images)=result["images"].as_array(){
                if images.len()>16{return Err("more than 16 images: request a narrower native screenshot".into());}
                for image in images {
                    let path=image["path"].as_str().ok_or("native screenshot has no path")?;
                    let size=tokio::fs::metadata(path).await.map_err(|e|format!("native image unavailable: {e}"))?.len();
                    if size>16*1024*1024 || total.saturating_add(size as usize)>32*1024*1024{return Err("native image budget exceeded; use a window/region screenshot".into());}
                    let bytes=tokio::fs::read(path).await.map_err(|e|format!("native image read failed: {e}"))?;if bytes.len()>16*1024*1024 || total.saturating_add(bytes.len())>32*1024*1024{return Err("native image changed beyond the image budget".into());}total+=bytes.len();
                    parts.push(json!({"type":"text","text":format!("{} snapshot={} image={} (coordinates belong only to this image)",tool,result["snapshotId"],image["imageId"])}));
                    parts.push(json!({"type":"image","mimeType":"image/png","data":base64::engine::general_purpose::STANDARD.encode(bytes)}));
                }
            }
        }
    }
    Ok(parts)
}
impl Model for NativeModel {
    fn identity(&self)->Value{json!({"agent":"lyra","provider":self.resolved.model.provider,"model":self.resolved.model.id,"reasoningEffort":self.resolved.thinking_level,"inherited":true})}
    fn decide<'a>(&'a mut self,frame:&'a Value,schema:&'a Value,_epoch:usize,cancel:&'a Arc<AtomicBool>)->ModelFuture<'a>{Box::pin(async move{
        let mut prompt=frame.clone();let contracts=prompt.as_object_mut().and_then(|m|m.remove("nativeTools")).unwrap_or(Value::Null);
        let system=format!("{}\nNative tools (capability descriptions and exact schemas, not a fixed routing policy):\n{}",super::core::SYSTEM,contracts);
        let mut content=vec![json!({"type":"text","text":prompt.to_string()})];
        let images=image_parts(frame).await?;
        if !images.is_empty()&&!self.resolved.model.supports_images{return Err("当前实际模型未声明图像输入能力；Operator 不会换模型或在看不到图片时输入。请使用支持图像的模型或限定 DOM 任务。".into());}
        content.extend(images);
        let tools=vec![Tool{name:"operator_decision",description:"Choose the next bounded task segment, finish with current evidence, or report a blocker.".into(),parameters:schema.clone()}];
        let result=stream_chat(&self.http,&self.resolved.model,&self.resolved.api_key,self.resolved.thinking_level.as_deref(),&system,&[json!({"role":"user","content":content})],&tools,Some(&self.id),cancel,&mut |_|{}).await?;
        if result.stop_reason=="aborted"{return Err("operator model cancelled".into());}
        if result.stop_reason=="error"{return Err(result.error_message.unwrap_or_else(||"operator provider error".into()));}
        let calls:Vec<_>=result.content.iter().filter(|p|p["type"]=="toolCall").collect();
        let text=if calls.len()==1&&calls[0]["name"]=="operator_decision" {calls[0]["arguments"].to_string()}
            else if calls.is_empty(){result.content.iter().filter(|p|p["type"]=="text").filter_map(|p|p["text"].as_str()).collect::<Vec<_>>().join("")}
            else {String::from("invalid: exactly one operator_decision is required; no actions executed")};
        let u=&result.usage;
        let usage=match(u["input"].as_u64(),u["output"].as_u64()){
            (Some(i),Some(o))=>Some(json!({"inputTokens":i.saturating_add(u["cacheRead"].as_u64().unwrap_or(0)).saturating_add(u["cacheWrite"].as_u64().unwrap_or(0)),"outputTokens":o,"cacheReadTokens":u["cacheRead"].as_u64().unwrap_or(0),"cacheWriteTokens":u["cacheWrite"].as_u64().unwrap_or(0)})),_=>None};
        Ok(Reply{text,usage})
    })}
}
