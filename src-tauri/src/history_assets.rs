//! Lossless content-addressed attachments. Original bytes are never resized or
//! removed; thumbnails are disposable, lazy derivatives for the transcript UI.
use crate::threads::{Item, PromptImage, file_uri_to_local_path};
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{fs, io::{Cursor, Write}, path::{Path, PathBuf}};
use xcap::image::{ImageFormat, ImageReader, Limits};

pub(crate) fn hash(bytes: &[u8]) -> String { format!("{:x}", Sha256::digest(bytes)) }
pub(crate) fn valid_hash(id: &str) -> bool { id.len()==64 && id.bytes().all(|b|b.is_ascii_digit()||(b'a'..=b'f').contains(&b)) }
/// Same-directory durable replacement; never delete the old file before rename.
pub(crate) fn atomic_write(path:&Path, bytes:&[u8]) -> Result<(),String> {
    let parent=path.parent().ok_or("文件没有父目录")?; fs::create_dir_all(parent).map_err(|e|e.to_string())?;
    let temp=parent.join(format!(".{}.tmp",uuid::Uuid::new_v4()));
    let result=(|| {
        let mut file=fs::OpenOptions::new().write(true).create_new(true).open(&temp).map_err(|e|e.to_string())?;
        file.write_all(bytes).and_then(|_|file.sync_all()).map_err(|e|e.to_string())?;drop(file);
        #[cfg(windows)] {
            use std::os::windows::ffi::OsStrExt;
            use windows_sys::Win32::Storage::FileSystem::{MoveFileExW,MOVEFILE_REPLACE_EXISTING,MOVEFILE_WRITE_THROUGH};
            let src:Vec<u16>=temp.as_os_str().encode_wide().chain(Some(0)).collect();
            let dst:Vec<u16>=path.as_os_str().encode_wide().chain(Some(0)).collect();
            if unsafe{MoveFileExW(src.as_ptr(),dst.as_ptr(),MOVEFILE_REPLACE_EXISTING|MOVEFILE_WRITE_THROUGH)}==0 { return Err(std::io::Error::last_os_error().to_string()); }
        }
        #[cfg(not(windows))] {
            fs::rename(&temp,path).map_err(|e|e.to_string())?;
            fs::File::open(parent).and_then(|f|f.sync_all()).map_err(|e|e.to_string())?;
        }
        Ok(())
    })();
    if result.is_err(){let _=fs::remove_file(temp);}result
}
pub(crate) fn file_uri(path:&Path) -> String {
    let path=path.to_string_lossy().replace('\\',"/").replace('%',"%25").replace('#',"%23").replace('?',"%3F").replace(' ',"%20");
    format!("file://{path}")
}
#[derive(Clone)]
pub struct AssetStore { root:PathBuf }
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all="camelCase")]
pub struct AssetInfo {
    pub attachment_id:String,
    pub uri:String,
    pub size:u64,
    pub width:Option<u32>,
    pub height:Option<u32>,
    pub thumbnail_uri:Option<String>,
}
impl AssetStore {
    pub fn new(data_dir:&Path)->Self { Self {root:data_dir.join("attachments-v1")} }
    fn original(&self,id:&str)->PathBuf {self.root.join(format!("{id}.original"))}
    fn info_path(&self,id:&str)->PathBuf {self.root.join(format!("{id}.json"))}
    pub fn store(&self,bytes:&[u8],mime:&str)->Result<AssetInfo,String> {
        let id=hash(bytes);let raw=self.original(&id);
        // Content-addressing is also a correctness check, not just deduplication.
        if fs::read(&raw).ok().as_deref().map(hash).as_deref()!=Some(&id) {atomic_write(&raw,bytes)?;}
        let ext=match mime {"image/png"=>"png","image/jpeg"=>"jpg","image/webp"=>"webp","image/gif"=>"gif","image/bmp"=>"bmp",_=>"bin"};
        let path=self.root.join(format!("{id}.{ext}"));
        // Named original is usable by existing native providers and asset protocol.
        // Hard link when possible; fallback remains a byte-for-byte durable copy.
        if !path.exists() {if fs::hard_link(&raw,&path).is_err(){atomic_write(&path,bytes)?;}}
        else if fs::read(&path).ok().as_deref().map(hash).as_deref()!=Some(&id) {atomic_write(&path,bytes)?;}
        let dims=ImageReader::new(Cursor::new(bytes)).with_guessed_format().ok().and_then(|r|r.into_dimensions().ok());
        let info=AssetInfo{attachment_id:id.clone(),uri:file_uri(&path),size:bytes.len() as u64,width:dims.map(|x|x.0),height:dims.map(|x|x.1),thumbnail_uri:None};
        if !self.info_path(&id).exists() {atomic_write(&self.info_path(&id),&serde_json::to_vec(&info).map_err(|e|e.to_string())?)?;}
        Ok(info)
    }
    pub fn info(&self,id:&str)->Result<AssetInfo,String> {
        if !valid_hash(id){return Err("附件 ID 无效".into());}
        let info:AssetInfo=serde_json::from_slice(&fs::read(self.info_path(id)).map_err(|e|e.to_string())?).map_err(|e|e.to_string())?;
        if info.attachment_id!=id || self.id_from_uri(&info.uri).as_deref()!=Some(id) {return Err("附件元数据身份不一致".into());}Ok(info)
    }
    pub fn id_from_uri(&self,uri:&str)->Option<String> {
        let path=PathBuf::from(file_uri_to_local_path(uri)?);
        if path.parent()?!=self.root {return None;}
        let id=path.file_stem()?.to_str()?;valid_hash(id).then(||id.to_string())
    }
    pub fn externalize(&self,image:&mut PromptImage)->Result<(),String> {
        let Some(data)=image.data.as_ref() else{return Ok(())};
        // Decoding is performed only on blocking workers, never under ThreadStore.
        let bytes=base64::engine::general_purpose::STANDARD.decode(data).map_err(|e|e.to_string())?;
        let info=self.store(&bytes,&image.mime_type)?;
        image.uri=Some(info.uri);image.size=Some(info.size);image.data=None;Ok(())
    }
    pub fn externalize_item(&self,item:&mut Item)->Result<(),String> {
        match item {
            Item::User{images,..}=>{for image in images {self.externalize(image)?;}},
            Item::Tool{call,..}=>{for value in &mut call.content {self.externalize_value(value)?;}if let Some(v)=&mut call.raw_output {self.externalize_value(v)?;}},
            _=>{},
        }Ok(())
    }
    fn externalize_value(&self,value:&mut Value)->Result<(),String> {
        match value {
            Value::Array(a)=>{for v in a {self.externalize_value(v)?;}},
            Value::Object(o)=>{
                if o.get("type").and_then(Value::as_str)==Some("image") {
                    if let Some(data)=o.get("data").and_then(Value::as_str) {
                        let mime=o.get("mimeType").or_else(||o.get("mime_type")).and_then(Value::as_str).unwrap_or("image/png");
                        let bytes=base64::engine::general_purpose::STANDARD.decode(data).map_err(|e|e.to_string())?;
                        let info=self.store(&bytes,mime)?;
                        o.remove("data");o.insert("uri".into(),json!(info.uri));o.insert("attachmentId".into(),json!(info.attachment_id));
                        o.insert("width".into(),json!(info.width));o.insert("height".into(),json!(info.height));
                    }
                }
                for v in o.values_mut(){self.externalize_value(v)?;}
            }, _=>{},
        }Ok(())
    }
    pub fn image_metadata(&self,image:&PromptImage)->Option<AssetInfo> { self.info(&self.id_from_uri(image.uri.as_deref()?)?).ok() }
    pub fn thumbnail(&self,id:&str,max_edge:u32)->Result<AssetInfo,String> {
        let mut info=self.info(id)?;
        let edge=match max_edge {0..=480=>480,481..=960=>960,_=>1536};
        let target=self.root.join(format!("{id}-thumb-{edge}.png"));
        if !target.exists() {
            let (w,h)=(info.width.ok_or("附件不是支持的图片")?,info.height.ok_or("附件不是支持的图片")?);
            if u64::from(w)*u64::from(h)>32_000_000 || info.size>64*1024*1024 {return Err("图片超出缩略图解码预算，请打开原图查看".into());}
            let mut reader=ImageReader::open(self.original(id)).map_err(|e|e.to_string())?.with_guessed_format().map_err(|e|e.to_string())?;
            let mut limits=Limits::default();limits.max_alloc=Some(160*1024*1024);reader.limits(limits);
            let image=reader.decode().map_err(|e|e.to_string())?;
            let small=image.thumbnail(edge,edge);
            let mut bytes=Cursor::new(Vec::new());small.write_to(&mut bytes,ImageFormat::Png).map_err(|e|e.to_string())?;
            atomic_write(&target,bytes.get_ref())?;
        }
        info.thumbnail_uri=Some(file_uri(&target));Ok(info)
    }
}
/// Rehydrate tools as well as user attachments when crossing machine boundaries.
/// UI thumbnails are never used as provider/sharing source images.
pub fn embed_tool_images(value:&mut Value) {
    match value {
        Value::Array(a)=>for v in a {embed_tool_images(v)},
        Value::Object(o)=>{
            if o.get("type").and_then(Value::as_str)==Some("image") && !o.contains_key("data") {
                if let Some(path)=o.get("uri").and_then(Value::as_str).and_then(file_uri_to_local_path) {
                    if fs::metadata(&path).is_ok_and(|m|m.len()<=crate::threads::MAX_EMBED_BYTES) {
                        if let Ok(bytes)=fs::read(path) {
                            o.insert("data".into(),json!(base64::engine::general_purpose::STANDARD.encode(bytes)));
                            for key in ["uri","attachmentId","thumbnailUri","width","height"] {o.remove(key);}
                        }
                    }
                }
            }
            for v in o.values_mut(){embed_tool_images(v)}
        },_=>{},
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn original_roundtrips_and_thumbnail_is_lazy() {
        let dir=tempfile::tempdir().unwrap();let assets=AssetStore::new(dir.path());
        let image=xcap::image::RgbaImage::from_pixel(1920,1080,xcap::image::Rgba([10,20,30,255]));
        let mut bytes=Cursor::new(Vec::new());image.write_to(&mut bytes,ImageFormat::Png).unwrap();let original=bytes.into_inner();
        let mut attachment=PromptImage{name:"截图.png".into(),mime_type:"image/png".into(),data:Some(base64::engine::general_purpose::STANDARD.encode(&original)),uri:None,size:None};
        assets.externalize(&mut attachment).unwrap();assert!(attachment.data.is_none());
        let info=assets.image_metadata(&attachment).unwrap();assert_eq!((info.width,info.height),(Some(1920),Some(1080)));assert!(info.thumbnail_uri.is_none());
        assert_eq!(fs::read(file_uri_to_local_path(&info.uri).unwrap()).unwrap(),original);
        let preview=assets.thumbnail(&info.attachment_id,480).unwrap();let p=file_uri_to_local_path(&preview.thumbnail_uri.unwrap()).unwrap();
        assert_eq!(ImageReader::open(p).unwrap().into_dimensions().unwrap(),(480,270));
        crate::threads::embed_attachment_data(std::slice::from_mut(&mut attachment));
        assert_eq!(base64::engine::general_purpose::STANDARD.decode(attachment.data.unwrap()).unwrap(),original);
        assert!(assets.thumbnail("../secret",480).is_err());
    }
    #[test] fn failed_externalization_retains_original_data() {
        let dir=tempfile::tempdir().unwrap();let assets=AssetStore::new(dir.path());fs::write(&assets.root,b"not a directory").unwrap();
        let mut image=PromptImage{name:"file".into(),mime_type:"image/png".into(),data:Some("aGVsbG8=".into()),uri:None,size:None};
        assert!(assets.externalize(&mut image).is_err());assert_eq!(image.data.as_deref(),Some("aGVsbG8="));
    }
}
