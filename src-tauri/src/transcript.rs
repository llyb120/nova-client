//! Copy-on-write history. Snapshots share immutable 32-item chunks instead of
//! copying every old tool result/image under ThreadStore's mutex. Wire serde is
//! deliberately still an ordinary Item array (sharing, providers and restores).
use crate::threads::Item;
use serde::{Deserialize, Deserializer, Serialize, Serializer, ser::SerializeSeq};
use std::{ops::{Index, IndexMut}, sync::{Arc, OnceLock}};

pub(crate) const CHUNK_ITEMS: usize = 32;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HistoryStats {
    pub items: usize,
    pub users: usize,
    pub turns: usize,
    pub estimated_bytes: usize,
    pub inline_asset_bytes: usize,
    pub total_tokens: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub max_id: u64,
}
impl HistoryStats {
    fn add(&mut self, other: Self) {
        self.items += other.items;
        self.users += other.users;
        self.turns += other.turns;
        self.estimated_bytes = self.estimated_bytes.saturating_add(other.estimated_bytes);
        self.inline_asset_bytes = self.inline_asset_bytes.saturating_add(other.inline_asset_bytes);
        self.total_tokens = self.total_tokens.saturating_add(other.total_tokens);
        self.input_tokens = self.input_tokens.saturating_add(other.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(other.output_tokens);
        self.cache_read_tokens = self.cache_read_tokens.saturating_add(other.cache_read_tokens);
        self.cache_write_tokens = self.cache_write_tokens.saturating_add(other.cache_write_tokens);
        self.max_id = self.max_id.max(other.max_id);
    }
}

pub(crate) fn value_bytes(value: &serde_json::Value) -> usize {
    use serde_json::Value;
    match value {
        Value::String(s) => s.len().saturating_add(2),
        Value::Array(a) => a.iter().map(value_bytes).sum::<usize>().saturating_add(a.len()+2),
        Value::Object(o) => o.iter().map(|(k,v)| k.len()+4+value_bytes(v)).sum::<usize>()+2,
        _ => 24,
    }
}
fn image_bytes(v: &serde_json::Value) -> usize {
    match v {
        serde_json::Value::Array(a) => a.iter().map(image_bytes).sum(),
        serde_json::Value::Object(o) => {
            let own = if o.get("type").and_then(|v|v.as_str()) == Some("image") {
                o.get("data").and_then(|v|v.as_str()).map_or(0,str::len)
            } else { 0 };
            own + o.values().map(image_bytes).sum::<usize>()
        }
        _ => 0,
    }
}
pub(crate) fn item_stats(item: &Item) -> HistoryStats {
    let mut s = HistoryStats { items: 1, max_id: item.id(), estimated_bytes: 128, ..Default::default() };
    match item {
        Item::User { text, images, .. } => {
            s.users = 1;
            s.inline_asset_bytes = images.iter().filter_map(|i| i.data.as_ref()).map(String::len).sum();
            s.estimated_bytes += text.len()+s.inline_asset_bytes+images.iter().map(|i|i.name.len()+i.uri.as_ref().map_or(0,String::len)+128).sum::<usize>();
        }
        Item::Assistant { text, .. } | Item::Thought { text, .. } | Item::System { text, .. } => s.estimated_bytes += text.len(),
        Item::Tool { call, .. } => {
            s.estimated_bytes += call.title.len()+call.content.iter().map(value_bytes).sum::<usize>()+call.locations.iter().map(value_bytes).sum::<usize>()
                +call.raw_input.as_ref().map_or(0,value_bytes)+call.raw_output.as_ref().map_or(0,value_bytes);
            s.inline_asset_bytes = call.content.iter().map(image_bytes).sum::<usize>()+call.raw_output.as_ref().map_or(0,image_bytes);
        }
        Item::Turn { total_tokens, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, .. } => {
            s.turns = 1;
            s.total_tokens = total_tokens.unwrap_or(0); s.input_tokens = input_tokens.unwrap_or(0); s.output_tokens = output_tokens.unwrap_or(0);
            s.cache_read_tokens = cache_read_tokens.unwrap_or(0); s.cache_write_tokens = cache_write_tokens.unwrap_or(0);
        }
    }
    s
}

#[derive(Debug)]
pub struct Chunk {
    pub items: Vec<Item>,
    pub persisted_hash: OnceLock<String>,
    stats: OnceLock<HistoryStats>,
}
impl Clone for Chunk {
    fn clone(&self) -> Self { Self::new(self.items.clone()) }
}
impl Chunk {
    pub fn new(items: Vec<Item>) -> Self { Self { items, persisted_hash: OnceLock::new(), stats: OnceLock::new() } }
    pub fn stats(&self) -> HistoryStats {
        *self.stats.get_or_init(|| { let mut s=HistoryStats::default(); for item in &self.items { s.add(item_stats(item)); } s })
    }
    fn writable(&mut self) -> &mut Vec<Item> {
        self.persisted_hash.take(); self.stats.take(); &mut self.items
    }
}

#[derive(Clone, Debug)]
pub struct TranscriptItems {
    pub(crate) chunks: Vec<Arc<Chunk>>,
    len: usize,
    // Replaced only when indices can move (truncate/retain/restore). Appending
    // and streaming updates preserve pagination cursors; old process cursors do not.
    generation: Arc<str>,
}
impl Default for TranscriptItems {
    fn default() -> Self { Self { chunks: Vec::new(), len:0, generation: uuid::Uuid::new_v4().to_string().into() } }
}
impl TranscriptItems {
    pub fn len(&self) -> usize { self.len }
    pub fn is_empty(&self) -> bool { self.len == 0 }
    pub fn generation(&self) -> &str { &self.generation }
    pub fn get(&self, index: usize) -> Option<&Item> { self.chunks.get(index/CHUNK_ITEMS)?.items.get(index%CHUNK_ITEMS) }
    pub fn get_mut(&mut self, index: usize) -> Option<&mut Item> {
        Arc::make_mut(self.chunks.get_mut(index/CHUNK_ITEMS)?).writable().get_mut(index%CHUNK_ITEMS)
    }
    pub fn first(&self) -> Option<&Item> { self.get(0) }
    pub fn last(&self) -> Option<&Item> { self.len.checked_sub(1).and_then(|i|self.get(i)) }
    pub fn last_mut(&mut self) -> Option<&mut Item> { self.len.checked_sub(1).and_then(|i|self.get_mut(i)) }
    pub fn push(&mut self, item: Item) {
        if self.len % CHUNK_ITEMS == 0 { self.chunks.push(Arc::new(Chunk::new(Vec::with_capacity(CHUNK_ITEMS)))); }
        Arc::make_mut(self.chunks.last_mut().unwrap()).writable().push(item); self.len+=1;
    }
    pub fn iter(&self) -> Iter<'_> { Iter { items:self, front:0, back:self.len } }
    pub fn iter_mut(&mut self) -> IterMut<'_> { self.chunks.iter_mut().flat_map(writable_chunk as fn(&mut Arc<Chunk>) -> std::slice::IterMut<'_,Item>) }
    pub fn range(&self, start: usize, end: usize) -> Iter<'_> { Iter {items:self,front:start.min(self.len),back:end.min(self.len).max(start.min(self.len))} }
    pub fn to_vec(&self) -> Vec<Item> { self.iter().cloned().collect() }
    pub fn clear(&mut self) { *self=Self::default(); }
    pub fn truncate(&mut self, len: usize) {
        if len >= self.len { return; }
        self.generation=uuid::Uuid::new_v4().to_string().into();
        self.chunks.truncate(len.div_ceil(CHUNK_ITEMS));
        if len%CHUNK_ITEMS != 0 { Arc::make_mut(self.chunks.last_mut().unwrap()).writable().truncate(len%CHUNK_ITEMS); }
        self.len=len;
    }
    pub fn retain(&mut self, mut keep: impl FnMut(&Item)->bool) {
        let indices: Vec<usize>=self.iter().enumerate().filter_map(|(i,x)|keep(x).then_some(i)).collect();
        if indices.len()==self.len { return; }
        *self=indices.into_iter().map(|i|self[i].clone()).collect();
    }
    pub fn stats(&self) -> HistoryStats { let mut s=HistoryStats::default(); for c in &self.chunks {s.add(c.stats());} s }
    pub fn stats_before(&self, end: usize) -> HistoryStats {
        let end=end.min(self.len); let mut s=HistoryStats::default();
        for c in self.chunks.iter().take(end/CHUNK_ITEMS) { s.add(c.stats()); }
        for item in self.range(end/CHUNK_ITEMS*CHUNK_ITEMS,end) {s.add(item_stats(item));} s
    }
    pub(crate) fn from_chunks(chunks: Vec<Arc<Chunk>>) -> Result<Self,String> {
        if chunks.iter().enumerate().any(|(i,c)| c.items.is_empty() || c.items.len()>CHUNK_ITEMS || (i+1<chunks.len()&&c.items.len()!=CHUNK_ITEMS)) {
            return Err("会话历史分块长度损坏".into());
        }
        Ok(Self {len:chunks.iter().map(|c|c.items.len()).sum(),chunks,..Self::default()})
    }
    /// Install only off-thread asset migrations of chunks that have not changed
    /// since the snapshot. Never overwrite a concurrently streamed tail/restore.
    pub fn install_unchanged(&mut self, original:&Self, normalized:&Self) {
        if self.generation != original.generation || original.len != normalized.len {return;}
        for (i,(before,after)) in original.chunks.iter().zip(&normalized.chunks).enumerate() {
            if let Some(current)=self.chunks.get_mut(i) {if Arc::ptr_eq(current,before) {*current=after.clone();}}
        }
    }
}
impl From<Vec<Item>> for TranscriptItems { fn from(items:Vec<Item>)->Self {items.into_iter().collect()} }
impl FromIterator<Item> for TranscriptItems { fn from_iter<T:IntoIterator<Item=Item>>(iter:T)->Self {let mut s=Self::default();s.extend(iter);s} }
impl Extend<Item> for TranscriptItems {fn extend<T:IntoIterator<Item=Item>>(&mut self,iter:T) {for item in iter {self.push(item);}}}
impl Index<usize> for TranscriptItems {type Output=Item;fn index(&self,i:usize)->&Item {self.get(i).expect("history index")}}
impl IndexMut<usize> for TranscriptItems {fn index_mut(&mut self,i:usize)->&mut Item {self.get_mut(i).expect("history index")}}
#[derive(Clone)]
pub struct Iter<'a> {items:&'a TranscriptItems,front:usize,back:usize}
impl<'a> Iterator for Iter<'a> {type Item=&'a Item;fn next(&mut self)->Option<Self::Item> {if self.front==self.back{return None;}let i=self.front;self.front+=1;self.items.get(i)}fn size_hint(&self)->(usize,Option<usize>){let n=self.back-self.front;(n,Some(n))}}
impl DoubleEndedIterator for Iter<'_> {fn next_back(&mut self)->Option<Self::Item>{if self.front==self.back{return None;}self.back-=1;self.items.get(self.back)}}
impl ExactSizeIterator for Iter<'_> {}
fn writable_chunk(c:&mut Arc<Chunk>)->std::slice::IterMut<'_,Item>{Arc::make_mut(c).writable().iter_mut()}
pub type IterMut<'a>=std::iter::FlatMap<std::slice::IterMut<'a,Arc<Chunk>>,std::slice::IterMut<'a,Item>,fn(&'a mut Arc<Chunk>)->std::slice::IterMut<'a,Item>>;
impl<'a> IntoIterator for &'a TranscriptItems {type Item=&'a Item;type IntoIter=Iter<'a>;fn into_iter(self)->Self::IntoIter {self.iter()}}
impl<'a> IntoIterator for &'a mut TranscriptItems {type Item=&'a mut Item;type IntoIter=IterMut<'a>;fn into_iter(self)->Self::IntoIter {self.iter_mut()}}
impl IntoIterator for TranscriptItems {type Item=Item;type IntoIter=std::vec::IntoIter<Item>;fn into_iter(self)->Self::IntoIter {self.chunks.into_iter().flat_map(|c|Arc::try_unwrap(c).unwrap_or_else(|c|(*c).clone()).items).collect::<Vec<_>>().into_iter()}}
impl Serialize for TranscriptItems {fn serialize<S:Serializer>(&self,s:S)->Result<S::Ok,S::Error>{let mut seq=s.serialize_seq(Some(self.len))?;for item in self {seq.serialize_element(item)?;}seq.end()}}
impl<'de> Deserialize<'de> for TranscriptItems {fn deserialize<D:Deserializer<'de>>(d:D)->Result<Self,D::Error>{Vec::<Item>::deserialize(d).map(Self::from)}}

#[cfg(test)]
mod tests {
    use super::*;
    fn items(n:usize)->TranscriptItems {(0..n).map(|i|Item::Assistant{id:i as u64,text:"x".repeat(1024),ts:0}).collect()}
    #[test] fn snapshot_and_tail_updates_only_copy_one_chunk() {
        let mut live=items(10000); let snapshot=live.clone();
        assert!(live.chunks.iter().zip(&snapshot.chunks).all(|(a,b)|Arc::ptr_eq(a,b)));
        let Item::Assistant{text,..}=live.last_mut().unwrap() else {panic!()};text.push('!');
        assert_eq!(live.chunks.iter().zip(&snapshot.chunks).filter(|(a,b)|!Arc::ptr_eq(a,b)).count(),1);
        assert_eq!(snapshot.last().map(|i|match i{Item::Assistant{text,..}=>text.len(),_=>0}),Some(1024));
        assert_eq!(live.generation(),snapshot.generation());
    }
    #[test] fn reverse_mutation_is_lazy_and_flat_serde_is_compatible() {
        let mut live=items(100);let snap=live.clone();
        if let Some(Item::Assistant{text,..})=live.iter_mut().rev().next(){text.push('!');}
        assert!(Arc::ptr_eq(&live.chunks[0],&snap.chunks[0]));
        let bytes=serde_json::to_vec(&live).unwrap();let decoded:TranscriptItems=serde_json::from_slice(&bytes).unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap(),serde_json::to_value(live.to_vec()).unwrap());
        assert_eq!(live.iter().rposition(|i|i.id()==2),Some(2));
    }
    #[test] fn cursor_generation_and_concurrent_migration() {
        let mut live=items(70);let original=live.clone();let mut normalized=original.clone();
        if let Item::Assistant{text,..}=&mut normalized[0]{*text="migrated".into();}
        live.push(Item::Assistant{id:70,text:"tail".into(),ts:0});
        live.install_unchanged(&original,&normalized);
        assert!(matches!(&live[0],Item::Assistant{text,..} if text=="migrated"));assert_eq!(live.len(),71);
        let gen=live.generation().to_string();live.truncate(2);assert_ne!(gen,live.generation());
        live.install_unchanged(&original,&normalized);assert_eq!(live.len(),2);
    }
}
