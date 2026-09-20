//! Versioned, content-addressed conversation persistence. Runtime/transport Thread stays unchanged.
use crate::threads::{Item, Thread};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{collections::HashSet, fs, io::Write, path::Path};

const VERSION: u32 = 1;
const CHUNK_ITEMS: usize = 64;
const BLOB_BYTES: usize = 32 * 1024;

/// Display-only projection. Models, exports, roaming and checkpoints keep the complete Thread API.
pub(crate) fn view(mut thread: Thread, assets: &Path) -> Result<Value, String> {
    let items = std::mem::take(&mut thread.items);
    let tail = items.len().saturating_sub(128);
    let last_answer = items
        .iter()
        .rposition(|item| matches!(item, Item::Assistant { .. }));
    let mut values = Vec::with_capacity(items.len());
    for (index, item) in items.into_iter().enumerate() {
        let deferred = index < tail
            && Some(index) != last_answer
            && !matches!(item, Item::User { .. } | Item::Turn { .. });
        let value = if deferred {
            match item {
                Item::Tool { id, ts, call } => serde_json::json!({
                    "type": "tool", "id": id, "ts": ts, "toolCallId": call.tool_call_id,
                    "title": call.title, "kind": call.kind, "status": call.status,
                    "locations": call.locations, "content": [], "deferred": true,
                }),
                Item::Assistant { id, ts, .. } => {
                    serde_json::json!({"type":"assistant", "id":id, "ts":ts, "text":"", "deferred":true})
                }
                Item::Thought { id, ts, .. } => {
                    serde_json::json!({"type":"thought", "id":id, "ts":ts, "text":"", "deferred":true})
                }
                Item::System { id, ts, level, .. } => {
                    serde_json::json!({"type":"system", "id":id, "ts":ts, "level":level, "text":"", "deferred":true})
                }
                _ => unreachable!(),
            }
        } else {
            view_item(item, assets)?
        };
        values.push(value);
    }
    let mut result = serde_json::to_value(thread).map_err(|e| e.to_string())?;
    result["items"] = Value::Array(values);
    Ok(result)
}

pub(crate) fn view_item(mut item: Item, assets: &Path) -> Result<Value, String> {
    use base64::Engine;
    if let Item::User { images, .. } = &mut item {
        for image in images {
            if !image.mime_type.starts_with("image/") {
                continue;
            }
            let Some(data) = &image.data else {
                continue;
            };
            // Malformed legacy data stays inline instead of making the entire history unreadable.
            let extension = match image.mime_type.as_str() {
                "image/jpeg" => "jpg",
                "image/webp" => "webp",
                "image/gif" => "gif",
                "image/svg+xml" => "svg",
                "image/bmp" => "bmp",
                _ => "png",
            };
            // Hash the encoded source so repeated opens do not decode every historical screenshot.
            let path = assets.join(format!("{}.{extension}", hash(data.as_bytes())));
            if !path.exists() {
                let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(data) else {
                    continue;
                };
                if let Err(error) = fs::create_dir_all(assets)
                    .map_err(|e| e.to_string())
                    .and_then(|_| atomic_write(&path, &bytes))
                {
                    eprintln!("[threads] 图片缓存失败，保留内嵌内容：{error}");
                    continue;
                }
            }
            let size = fs::metadata(&path).map_err(|e| e.to_string())?.len();
            let path = path.to_string_lossy().replace('\\', "/");
            // Existing consumers use file URI + percent_decode/convertFileSrc.
            image.uri = Some(format!(
                "file://{}",
                path.replace('%', "%25")
                    .replace('#', "%23")
                    .replace('?', "%3F")
            ));
            image.size = Some(size);
            image.data = None;
        }
    }
    serde_json::to_value(item).map_err(|e| e.to_string())
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Manifest {
    storage_version: u32,
    thread: Value,
    item_count: usize,
    chunks: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct Chunk {
    items: Value,
    blobs: Vec<Blob>,
}

#[derive(Serialize, Deserialize)]
struct Blob {
    pointer: String,
    hash: String,
}

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Never unlink the destination before replacing it: the previous commit must survive failure.
pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .map_err(|e| e.to_string())?;
        file.write_all(bytes)
            .and_then(|_| file.sync_all())
            .map_err(|e| e.to_string())?;
        drop(file);
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            use windows_sys::Win32::Storage::FileSystem::{
                MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
            };
            let from: Vec<u16> = tmp.as_os_str().encode_wide().chain(Some(0)).collect();
            let to: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
            if unsafe {
                MoveFileExW(
                    from.as_ptr(),
                    to.as_ptr(),
                    MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
                )
            } == 0
            {
                return Err(std::io::Error::last_os_error().to_string());
            }
        }
        #[cfg(not(windows))]
        {
            fs::rename(&tmp, path).map_err(|e| e.to_string())?;
            fs::File::open(path.parent().ok_or("missing parent")?)
                .and_then(|f| f.sync_all())
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(tmp);
    }
    result
}

fn put_object(dir: &Path, bytes: &[u8]) -> Result<String, String> {
    let key = hash(bytes);
    let path = dir.join(&key);
    // Existing immutable objects are not rewritten. The reader validates their digest.
    if fs::read(&path)
        .map(|existing| hash(&existing) != key)
        .unwrap_or(true)
    {
        atomic_write(&path, bytes)?;
    }
    Ok(key)
}

fn read_object(dir: &Path, key: &str) -> Result<Vec<u8>, String> {
    if key.len() != 64
        || !key
            .bytes()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    {
        return Err("invalid conversation object key".into());
    }
    let bytes = fs::read(dir.join(key)).map_err(|e| format!("object {key}: {e}"))?;
    if hash(&bytes) != key {
        return Err(format!("conversation object checksum mismatch: {key}"));
    }
    Ok(bytes)
}

fn externalize(
    value: &mut Value,
    pointer: &str,
    dir: &Path,
    blobs: &mut Vec<Blob>,
) -> Result<(), String> {
    match value {
        Value::String(text) if text.len() >= BLOB_BYTES || text.starts_with("data:image/") => {
            let key = put_object(dir, text.as_bytes())?;
            blobs.push(Blob {
                pointer: pointer.into(),
                hash: key,
            });
            text.clear();
        }
        Value::Array(values) => {
            for (i, child) in values.iter_mut().enumerate() {
                externalize(child, &format!("{pointer}/{i}"), dir, blobs)?;
            }
        }
        Value::Object(values) => {
            if values
                .get("mimeType")
                .and_then(Value::as_str)
                .is_some_and(|mime| mime.starts_with("image/"))
            {
                // Edited/reused display attachments must remain portable. Persist owned image
                // references as blobs too, rather than pinning a history entry to this machine's path.
                if values.get("data").and_then(Value::as_str).is_none() {
                    if let Some(path) = values
                        .get("uri")
                        .and_then(Value::as_str)
                        .and_then(crate::threads::file_uri_to_local_path)
                    {
                        if let Some(root) = dir.parent().and_then(Path::parent) {
                            if Path::new(&path).starts_with(root.join("thread-assets")) {
                                use base64::Engine;
                                let bytes = fs::read(path).map_err(|e| e.to_string())?;
                                values.insert(
                                    "data".into(),
                                    Value::String(
                                        base64::engine::general_purpose::STANDARD.encode(bytes),
                                    ),
                                );
                                values.remove("uri");
                            }
                        }
                    }
                }
                if let Some(Value::String(data)) = values.get_mut("data") {
                    let key = put_object(dir, data.as_bytes())?;
                    blobs.push(Blob {
                        pointer: format!("{pointer}/data"),
                        hash: key,
                    });
                    data.clear();
                }
            }
            for (key, child) in values {
                let key = key.replace('~', "~0").replace('/', "~1");
                externalize(child, &format!("{pointer}/{key}"), dir, blobs)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn decode_manifest(path: &Path, bytes: &[u8]) -> Result<Thread, String> {
    let value: Value = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    if value.get("storageVersion").is_none() {
        return serde_json::from_value(value).map_err(|e| e.to_string());
    }
    let manifest: Manifest = serde_json::from_value(value).map_err(|e| e.to_string())?;
    if manifest.storage_version != VERSION {
        return Err("unsupported conversation storage version".into());
    }
    let dir = path.with_extension("parts");
    let mut items = Vec::new();
    for key in &manifest.chunks {
        let mut chunk: Chunk =
            serde_json::from_slice(&read_object(&dir, key)?).map_err(|e| e.to_string())?;
        for blob in chunk.blobs {
            let slot = chunk
                .items
                .pointer_mut(&blob.pointer)
                .ok_or("invalid blob pointer")?;
            if slot.as_str() != Some("") {
                return Err("invalid blob placeholder".into());
            }
            *slot = Value::String(
                String::from_utf8(read_object(&dir, &blob.hash)?).map_err(|e| e.to_string())?,
            );
        }
        let part = chunk
            .items
            .as_array_mut()
            .ok_or("invalid conversation chunk")?;
        items.append(part);
    }
    if items.len() != manifest.item_count {
        return Err("conversation item count mismatch".into());
    }
    let mut thread = manifest.thread;
    thread["items"] = Value::Array(items);
    serde_json::from_value(thread).map_err(|e| e.to_string())
}

pub(crate) fn read(path: &Path) -> Result<Thread, String> {
    let bytes = fs::read(path).map_err(|e| e.to_string())?;
    if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
        if value
            .get("storageVersion")
            .is_some_and(|version| version.as_u64() != Some(VERSION as u64))
        {
            return Err("unsupported conversation storage version; original file retained".into());
        }
    }
    match decode_manifest(path, &bytes) {
        Ok(thread) => Ok(thread),
        Err(error) => {
            // Only recover when the live index exists. Deleted conversations must never reappear.
            for backup in [
                path.with_extension("json.previous"),
                path.with_extension("json.backup"),
            ] {
                if let Ok(thread) = fs::read(&backup)
                    .map_err(|e| e.to_string())
                    .and_then(|bytes| decode_manifest(path, &bytes))
                {
                    eprintln!(
                        "[threads] {}: {error}; recovered from {}",
                        path.display(),
                        backup.display()
                    );
                    return Ok(thread);
                }
            }
            Err(error)
        }
    }
}

pub(crate) fn is_chunked(path: &Path) -> bool {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Manifest>(&bytes).ok())
        .is_some_and(|manifest| manifest.storage_version == VERSION)
}

pub(crate) fn write(path: &Path, thread: &mut Thread) -> Result<(), String> {
    if let Ok(bytes) = fs::read(path) {
        if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
            if value
                .get("storageVersion")
                .is_some_and(|version| version.as_u64() != Some(VERSION as u64))
            {
                return Err(
                    "refusing to overwrite unsupported conversation storage version".into(),
                );
            }
        }
    }
    let dir = path.with_extension("parts");
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    // ponytail: direct mutations of Thread.items require an O(history) comparison pass.
    // Disk writes are incremental; a future item revision counter can eliminate this serialization pass.
    // Serialize only metadata, restoring items even if serialization fails. Serialize
    // one chunk at a time below, so migration does not allocate another whole history.
    let items = std::mem::take(&mut thread.items);
    let metadata = serde_json::to_value(&*thread);
    thread.items = items;
    let mut metadata = metadata.map_err(|e| e.to_string())?;
    metadata
        .as_object_mut()
        .ok_or("invalid thread")?
        .remove("items");
    let mut chunks = Vec::new();
    for part in thread.items.chunks(CHUNK_ITEMS) {
        let mut chunk = Chunk {
            items: serde_json::to_value(part).map_err(|e| e.to_string())?,
            blobs: Vec::new(),
        };
        externalize(&mut chunk.items, "", &dir, &mut chunk.blobs)?;
        chunks.push(put_object(
            &dir,
            &serde_json::to_vec(&chunk).map_err(|e| e.to_string())?,
        )?);
    }
    let manifest = Manifest {
        storage_version: VERSION,
        thread: metadata,
        item_count: thread.items.len(),
        chunks,
    };
    let bytes = serde_json::to_vec(&manifest).map_err(|e| e.to_string())?;
    let mut migrating = false;
    if let Ok(old) = fs::read(path) {
        if old == bytes {
            return Ok(());
        }
        // A failed/corrupt old commit must not replace the last usable recovery point.
        if decode_manifest(path, &old).is_ok() {
            let is_legacy = serde_json::from_slice::<Value>(&old)
                .map_err(|e| e.to_string())?
                .get("storageVersion")
                .is_none();
            if is_legacy {
                migrating = true;
                let backup = path.with_extension("json.backup");
                if !backup.exists() {
                    atomic_write(&backup, &old)?;
                }
            }
            atomic_write(&path.with_extension("json.previous"), &old)?;
        }
    }
    // Migration is verified end-to-end before switching formats. Normal writes already
    // validate object hashes in put_object; do not hydrate a second full history on every save.
    if migrating {
        decode_manifest(path, &bytes)?;
    }
    atomic_write(path, &bytes)?;
    // Keep the current and previous commit; abandoned writes can be collected after a successful commit.
    if let Err(error) = collect_objects(path, &manifest) {
        eprintln!(
            "[threads] deferred object cleanup {}: {error}",
            path.display()
        );
    }
    Ok(())
}

fn collect_objects(path: &Path, current: &Manifest) -> Result<(), String> {
    let dir = path.with_extension("parts");
    let mut live = HashSet::new();
    let previous = fs::read(path.with_extension("json.previous"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Manifest>(&bytes).ok());
    for manifest in std::iter::once(current).chain(previous.as_ref()) {
        for key in &manifest.chunks {
            live.insert(key.clone());
            let chunk: Chunk =
                serde_json::from_slice(&read_object(&dir, key)?).map_err(|e| e.to_string())?;
            live.extend(chunk.blobs.into_iter().map(|blob| blob.hash));
        }
    }
    for entry in fs::read_dir(dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.len() == 64 && name.bytes().all(|b| b.is_ascii_hexdigit()) && !live.contains(&name)
        {
            fs::remove_file(entry.path()).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::threads::{AgentKind, PromptImage, ThreadStore};

    fn write(path: &Path, thread: &Thread) -> Result<(), String> {
        super::write(path, &mut thread.clone())
    }

    fn conversation(count: usize) -> Thread {
        let mut thread = Thread::new("project".into(), AgentKind::Lyra, None, None, None, false);
        thread.items = (0..count)
            .map(|id| Item::Assistant {
                id: id as u64,
                ts: 1,
                text: format!("answer {id}"),
            })
            .collect();
        thread
    }

    fn manifest(path: &Path) -> Manifest {
        serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
    }

    #[test]
    fn migration_preserves_original_assets_and_incrementally_rewrites_changed_chunks() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("conversation.json");
        let mut thread = conversation(130);
        thread.items[0] = Item::User {
            id: 0,
            ts: 1,
            text: "截图".into(),
            images: vec![PromptImage {
                name: "screen.png".into(),
                mime_type: "image/png".into(),
                data: Some("A".repeat(100_000)),
                uri: None,
                size: None,
            }],
        };
        let original = serde_json::to_vec(&thread).unwrap();
        fs::write(&path, &original).unwrap();
        write(&path, &thread).unwrap();
        assert_eq!(
            fs::read(path.with_extension("json.backup")).unwrap(),
            original
        );
        assert_eq!(
            serde_json::to_value(read(&path).unwrap()).unwrap(),
            serde_json::to_value(&thread).unwrap()
        );
        let first = manifest(&path);
        assert_eq!(first.chunks.len(), 3);
        assert!(fs::metadata(&path).unwrap().len() < 4096);
        let object = path.with_extension("parts").join(&first.chunks[0]);
        let modified = fs::metadata(&object).unwrap().modified().unwrap();
        thread.items.push(Item::Assistant {
            id: 130,
            ts: 2,
            text: "tail".into(),
        });
        write(&path, &thread).unwrap();
        let second = manifest(&path);
        assert_eq!(&first.chunks[..2], &second.chunks[..2]);
        assert_ne!(first.chunks[2], second.chunks[2]);
        assert_eq!(fs::metadata(&object).unwrap().modified().unwrap(), modified);
        thread.items[70] = Item::Assistant {
            id: 70,
            ts: 3,
            text: "updated old tool/message".into(),
        };
        write(&path, &thread).unwrap();
        let third = manifest(&path);
        assert_ne!(second.chunks[1], third.chunks[1]);
        assert_eq!(second.chunks[0], third.chunks[0]);
        thread.items.truncate(2);
        write(&path, &thread).unwrap();
        assert_eq!(read(&path).unwrap().items.len(), 2);
        assert_eq!(
            fs::read(path.with_extension("json.backup")).unwrap(),
            original
        );
    }

    #[test]
    fn interrupted_commit_and_corrupt_index_recover_without_resurrecting_deleted_threads() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("conversation.json");
        let mut thread = conversation(5);
        write(&path, &thread).unwrap();
        let orphan = put_object(&path.with_extension("parts"), b"abandoned write").unwrap();
        assert_eq!(read(&path).unwrap().items.len(), 5);
        thread.title = "second commit".into();
        write(&path, &thread).unwrap();
        assert!(!path.with_extension("parts").join(orphan).exists());
        fs::write(&path, b"{interrupted").unwrap();
        assert_eq!(read(&path).unwrap().title, "新会话");
        fs::remove_file(&path).unwrap();
        assert!(read(&path).is_err());
    }

    #[test]
    fn malformed_objects_cannot_escape_storage_and_corruption_is_detected() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("conversation.json");
        let thread = conversation(1);
        write(&path, &thread).unwrap();
        let mut m = manifest(&path);
        let key = m.chunks[0].clone();
        fs::write(path.with_extension("parts").join(&key), b"corrupt").unwrap();
        assert!(read(&path).is_err());
        // A later valid runtime save repairs a damaged object, rather than trusting exists().
        write(&path, &thread).unwrap();
        assert!(read(&path).is_ok());
        m.chunks[0] = "../outside".into();
        assert!(decode_manifest(&path, &serde_json::to_vec(&m).unwrap()).is_err());
    }

    #[test]
    fn failed_migration_preserves_source_and_unreadable_history_survives_cleanup() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("threads");
        fs::create_dir(&dir).unwrap();
        let thread = conversation(3);
        let path = dir.join(format!("{}.json", thread.id));
        let source = serde_json::to_vec(&thread).unwrap();
        fs::write(&path, &source).unwrap();
        fs::write(path.with_extension("parts"), b"block directory creation").unwrap();
        fs::write(dir.join("damaged.json"), b"unreadable historical data").unwrap();
        let mut store = ThreadStore::load(temp.path().to_path_buf());
        assert_eq!(store.get(&thread.id).unwrap().items.len(), 3);
        store.save_now();
        assert_eq!(fs::read(path).unwrap(), source);
        assert_eq!(
            fs::read(dir.join("damaged.json")).unwrap(),
            b"unreadable historical data"
        );
    }

    #[test]
    fn exit_snapshot_cannot_be_overwritten_by_older_background_save() {
        let temp = tempfile::tempdir().unwrap();
        let mut store = ThreadStore::load(temp.path().to_path_buf());
        let thread = conversation(1);
        let id = thread.id.clone();
        store.threads.push(thread);
        store.save_thread(&id);
        let mut old = store.take_persist_snapshot().unwrap();
        store.get_mut(&id).unwrap().title = "newest".into();
        store.save_now();
        ThreadStore::write_persist_snapshot(&mut old).unwrap();
        assert_eq!(
            ThreadStore::load(temp.path().to_path_buf())
                .get(&id)
                .unwrap()
                .title,
            "newest"
        );
    }

    #[test]
    fn aggregate_migration_never_overwrites_an_unreadable_live_conversation() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("threads");
        fs::create_dir(&dir).unwrap();
        let thread = conversation(1);
        let path = dir.join(format!("{}.json", thread.id));
        fs::write(&path, b"damaged but potentially recoverable original").unwrap();
        let aggregate = temp.path().join("threads.json");
        fs::write(
            &aggregate,
            serde_json::to_vec(&serde_json::json!({"threads":[thread]})).unwrap(),
        )
        .unwrap();
        let mut store = ThreadStore::load(temp.path().to_path_buf());
        store.save_now();
        assert!(aggregate.exists());
        assert_eq!(
            fs::read(path).unwrap(),
            b"damaged but potentially recoverable original"
        );
    }

    #[test]
    fn display_index_keeps_history_order_and_image_reuse_is_transport_compatible() {
        let temp = tempfile::tempdir().unwrap();
        let mut thread = conversation(300);
        thread.items[0] = Item::User {
            id: 0,
            ts: 1,
            text: "original prompt".into(),
            images: vec![PromptImage {
                name: "screen.png".into(),
                mime_type: "image/png".into(),
                data: Some("aGVsbG8=".into()),
                uri: None,
                size: None,
            }],
        };
        let projected = view(thread.clone(), temp.path()).unwrap();
        assert_eq!(projected["items"].as_array().unwrap().len(), 300);
        assert_eq!(projected["items"][0]["text"], "original prompt");
        assert_eq!(projected["items"][1]["deferred"], true);
        assert_eq!(projected["items"][299]["text"], "answer 299");
        let mut image: PromptImage =
            serde_json::from_value(projected["items"][0]["images"][0].clone()).unwrap();
        assert!(image.data.is_none());
        crate::threads::embed_attachment_data(std::slice::from_mut(&mut image));
        assert_eq!(image.data.as_deref(), Some("aGVsbG8="));
        assert!(image.uri.is_none());
        // The complete runtime/transport history is never replaced by display placeholders.
        assert_eq!(
            serde_json::to_value(&thread.items[1]).unwrap()["text"],
            "answer 1"
        );
    }

    #[test]
    fn edited_image_references_remain_portable_and_cache_failure_keeps_inline_data() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("中文#%");
        let assets = root.join("thread-assets");
        let user = Item::User {
            id: 1,
            ts: 1,
            text: "reuse".into(),
            images: vec![PromptImage {
                name: "screen.png".into(),
                mime_type: "image/png".into(),
                data: Some("aGVsbG8=".into()),
                uri: None,
                size: None,
            }],
        };
        let projected = view_item(user.clone(), &assets).unwrap();
        let image_path =
            crate::threads::file_uri_to_local_path(projected["images"][0]["uri"].as_str().unwrap())
                .unwrap();
        assert_eq!(fs::read(&image_path).unwrap(), b"hello");
        let mut thread = conversation(0);
        thread
            .items
            .push(serde_json::from_value(projected).unwrap());
        let path = root.join("threads/reused.json");
        write(&path, &thread).unwrap();
        fs::remove_file(image_path).unwrap();
        let loaded = read(&path).unwrap();
        let value = serde_json::to_value(&loaded.items[0]).unwrap();
        assert_eq!(value["images"][0]["data"], "aGVsbG8=");
        assert!(value["images"][0].get("uri").is_none());
        let blocked = temp.path().join("not-a-directory");
        fs::write(&blocked, b"blocked").unwrap();
        assert_eq!(
            view_item(user, &blocked).unwrap()["images"][0]["data"],
            "aGVsbG8="
        );
        let mut newer = manifest(&path);
        newer.storage_version = VERSION + 1;
        fs::write(&path, serde_json::to_vec(&newer).unwrap()).unwrap();
        assert!(read(&path).unwrap_err().contains("unsupported"));
    }
}
