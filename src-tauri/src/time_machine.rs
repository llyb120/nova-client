use crate::threads::{now_ms, Item, Thread};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

const STORE_VERSION: u32 = 2;
// 世界线只管理源码/配置等常规文件。大型构建产物可能单个数百 MB；同步读取、哈希会让
// Tauri 命令长时间占住执行线程。它们在创建与恢复快照时都被排除，因此不会被误删。
const MAX_SNAPSHOT_FILE_BYTES: u64 = 32 * 1024 * 1024;
const IGNORED_DIRECTORIES: &[&str] = &[
    ".git",
    ".nova",
    "node_modules",
    "target",
    "dist",
    "build",
    ".cache",
    ".next",
    ".turbo",
    "coverage",
];

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PatchEntry {
    pub path: String,
    pub blob: String,
    #[serde(default)]
    pub executable: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
struct Checkpoint {
    id: String,
    parent_id: Option<String>,
    source_thread_id: String,
    title: String,
    created_at: i64,
    workspace_root: String,
    entries: Vec<PatchEntry>,
    thread_snapshot: Thread,
    #[serde(default)]
    automatic: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
struct Timeline {
    id: String,
    root_thread_id: String,
    #[serde(default)]
    thread_ids: Vec<String>,
    /// 每个真实会话当前位于树上的哪个节点；切回旧分支时仍从正确父节点继续。
    #[serde(default)]
    thread_heads: HashMap<String, String>,
    current_checkpoint_id: Option<String>,
    checkpoints: Vec<Checkpoint>,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct StoreFile {
    #[serde(default = "store_version")]
    version: u32,
    #[serde(default)]
    timelines: Vec<Timeline>,
}

impl Default for StoreFile {
    fn default() -> Self {
        Self {
            version: STORE_VERSION,
            timelines: Vec::new(),
        }
    }
}

fn store_version() -> u32 {
    STORE_VERSION
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub struct PromptSummary {
    pub id: u64,
    pub text: String,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CheckpointSummary {
    pub id: String,
    pub parent_id: Option<String>,
    pub source_thread_id: String,
    pub title: String,
    pub created_at: i64,
    pub changed_files: usize,
    pub automatic: bool,
    pub prompts: Vec<PromptSummary>,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct TimelineView {
    pub id: String,
    pub root_thread_id: String,
    pub current_checkpoint_id: Option<String>,
    pub checkpoints: Vec<CheckpointSummary>,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RestoreResult {
    pub thread_id: String,
    pub timeline: TimelineView,
}

fn time_machine_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("time-machine")
}

fn store_path(data_dir: &Path) -> PathBuf {
    time_machine_dir(data_dir).join("timelines.json")
}

fn load_store(data_dir: &Path) -> Result<StoreFile, String> {
    let path = store_path(data_dir);
    if !path.exists() {
        return Ok(StoreFile::default());
    }
    let bytes = fs::read(&path).map_err(|e| format!("读取世界线数据失败：{e}"))?;
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|e| format!("解析世界线数据失败：{e}"))?;
    if value.get("version").and_then(serde_json::Value::as_u64) != Some(STORE_VERSION as u64) {
        return Ok(StoreFile::default());
    }
    serde_json::from_value(value).map_err(|e| format!("解析世界线数据失败：{e}"))
}

fn save_store(data_dir: &Path, store: &StoreFile) -> Result<(), String> {
    let dir = time_machine_dir(data_dir);
    fs::create_dir_all(&dir).map_err(|e| format!("创建世界线目录失败：{e}"))?;
    let path = store_path(data_dir);
    let tmp = path.with_extension(format!("json.{}.tmp", uuid::Uuid::new_v4()));
    let bytes = serde_json::to_vec(store).map_err(|e| format!("序列化世界线数据失败：{e}"))?;
    fs::write(&tmp, bytes).map_err(|e| format!("写入世界线数据失败：{e}"))?;
    match fs::rename(&tmp, &path) {
        Ok(()) => Ok(()),
        Err(_) if path.exists() => {
            // Windows 不允许 rename 覆盖已有文件；先保留旧文件备份，提交成功后再删除。
            let backup = path.with_extension("json.backup");
            let _ = fs::remove_file(&backup);
            fs::rename(&path, &backup).map_err(|e| format!("备份世界线数据失败：{e}"))?;
            match fs::rename(&tmp, &path) {
                Ok(()) => {
                    let _ = fs::remove_file(backup);
                    Ok(())
                }
                Err(error) => {
                    let _ = fs::rename(&backup, &path);
                    Err(format!("提交世界线数据失败：{error}"))
                }
            }
        }
        Err(error) => Err(format!("提交世界线数据失败：{error}")),
    }
}

fn object_path(data_dir: &Path, hash: &str) -> PathBuf {
    let (prefix, rest) = hash.split_at(2);
    time_machine_dir(data_dir)
        .join("objects")
        .join(prefix)
        .join(rest)
}

fn put_blob(data_dir: &Path, bytes: &[u8]) -> Result<String, String> {
    let hash = format!("{:x}", Sha256::digest(bytes));
    let path = object_path(data_dir, &hash);
    if path.exists() {
        return Ok(hash);
    }
    let parent = path.parent().ok_or("无效的世界线对象路径")?;
    fs::create_dir_all(parent).map_err(|e| format!("创建世界线对象目录失败：{e}"))?;
    let tmp = parent.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
    fs::write(&tmp, bytes).map_err(|e| format!("写入世界线对象失败：{e}"))?;
    match fs::rename(&tmp, &path) {
        Ok(()) => Ok(hash),
        Err(_) if path.exists() => {
            let _ = fs::remove_file(tmp);
            Ok(hash)
        }
        Err(e) => Err(format!("提交世界线对象失败：{e}")),
    }
}

fn get_blob(data_dir: &Path, hash: &str) -> Result<Vec<u8>, String> {
    fs::read(object_path(data_dir, hash)).map_err(|e| format!("读取世界线对象 {hash} 失败：{e}"))
}

fn workspace_root(cwd: &Path) -> Result<PathBuf, String> {
    let root =
        fs::canonicalize(cwd).map_err(|e| format!("无法访问工作目录 {}：{e}", cwd.display()))?;
    if !root.is_dir() {
        return Err(format!("工作目录不是文件夹：{}", root.display()));
    }
    Ok(root)
}

fn safe_relative(path: &str) -> Result<PathBuf, String> {
    let candidate = PathBuf::from(path);
    if candidate.as_os_str().is_empty()
        || candidate.is_absolute()
        || candidate
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(format!("工作区包含不安全路径：{path}"));
    }
    Ok(candidate)
}

#[cfg(unix)]
fn executable(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn executable(_metadata: &fs::Metadata) -> bool {
    false
}

fn directory_files(root: &Path) -> Result<Vec<String>, String> {
    fn visit(root: &Path, directory: &Path, paths: &mut Vec<String>) -> Result<(), String> {
        let children = fs::read_dir(directory)
            .map_err(|e| format!("读取目录失败 {}：{e}", directory.display()))?;
        for child in children {
            let child =
                child.map_err(|e| format!("读取目录项失败 {}：{e}", directory.display()))?;
            let path = child.path();
            let metadata = fs::symlink_metadata(&path)
                .map_err(|e| format!("读取文件属性失败 {}：{e}", path.display()))?;
            if metadata.file_type().is_symlink() {
                continue;
            }
            if metadata.is_dir() {
                let name = child.file_name();
                if IGNORED_DIRECTORIES
                    .iter()
                    .any(|ignored| name == std::ffi::OsStr::new(ignored))
                {
                    continue;
                }
                visit(root, &path, paths)?;
            } else if metadata.is_file() {
                let relative = path
                    .strip_prefix(root)
                    .map_err(|_| format!("文件不在工作目录中：{}", path.display()))?;
                let relative = relative
                    .to_str()
                    .ok_or_else(|| format!("目录包含非 UTF-8 路径：{}", path.display()))?
                    .replace('\\', "/");
                safe_relative(&relative)?;
                paths.push(relative);
            }
        }
        Ok(())
    }

    let mut paths = Vec::new();
    visit(root, root, &mut paths)?;
    paths.sort();
    Ok(paths)
}

fn capture_manifest(data_dir: &Path, root: &Path) -> Result<Vec<PatchEntry>, String> {
    let mut entries = Vec::new();
    for path in directory_files(root)? {
        let absolute = root.join(safe_relative(&path)?);
        let metadata = fs::symlink_metadata(&absolute)
            .map_err(|e| format!("读取文件属性失败 {}：{e}", absolute.display()))?;
        if metadata.len() > MAX_SNAPSHOT_FILE_BYTES {
            continue;
        }
        let bytes = fs::read(&absolute)
            .map_err(|e| format!("读取工作区文件失败 {}：{e}", absolute.display()))?;
        entries.push(PatchEntry {
            path,
            blob: put_blob(data_dir, &bytes)?,
            executable: executable(&metadata),
        });
    }
    Ok(entries)
}

fn view_for(timeline: &Timeline, thread_id: &str) -> TimelineView {
    TimelineView {
        id: timeline.id.clone(),
        root_thread_id: timeline.root_thread_id.clone(),
        current_checkpoint_id: timeline
            .thread_heads
            .get(thread_id)
            .cloned()
            .or_else(|| timeline.current_checkpoint_id.clone()),
        checkpoints: timeline
            .checkpoints
            .iter()
            .map(|checkpoint| CheckpointSummary {
                id: checkpoint.id.clone(),
                parent_id: checkpoint.parent_id.clone(),
                source_thread_id: checkpoint.source_thread_id.clone(),
                title: checkpoint.title.clone(),
                created_at: checkpoint.created_at,
                changed_files: checkpoint.entries.len(),
                automatic: checkpoint.automatic,
                prompts: checkpoint
                    .thread_snapshot
                    .items
                    .iter()
                    .filter_map(|item| match item {
                        Item::User { id, text, .. } => Some(PromptSummary {
                            id: *id,
                            text: text.clone(),
                        }),
                        _ => None,
                    })
                    .collect(),
            })
            .collect(),
    }
}

fn timeline_index(store: &StoreFile, thread_id: &str) -> Option<usize> {
    store.timelines.iter().position(|timeline| {
        timeline.root_thread_id == thread_id
            || timeline.thread_ids.iter().any(|id| id == thread_id)
            || timeline
                .checkpoints
                .iter()
                .any(|checkpoint| checkpoint.source_thread_id == thread_id)
    })
}

fn latest_prompt_title(thread: &Thread) -> String {
    thread
        .items
        .iter()
        .rev()
        .find_map(|item| match item {
            Item::User { text, .. } if !text.trim().is_empty() => Some(text.trim().to_string()),
            _ => None,
        })
        .unwrap_or_else(|| thread.title.clone())
}

fn checkpoint_workspace(
    data_dir: &Path,
    timeline: &Timeline,
    thread: &Thread,
) -> Result<(PathBuf, Vec<PatchEntry>), String> {
    // 漫游 guest 的工作目录位于对端，本机只保存会话快照。
    let remote = thread.is_roaming_guest();
    let root = if remote {
        PathBuf::from(&thread.cwd)
    } else {
        workspace_root(Path::new(&thread.cwd))?
    };
    if let Some(first) = timeline.checkpoints.first() {
        if first.workspace_root != root.to_string_lossy() {
            return Err("工作目录已变化，不能继续写入原世界线时间线".into());
        }
    }
    let entries = if remote {
        Vec::new()
    } else {
        capture_manifest(data_dir, &root)?
    };
    Ok((root, entries))
}

fn append_checkpoint_captured(
    timeline: &mut Timeline,
    thread: &Thread,
    automatic: bool,
    root: &Path,
    entries: Vec<PatchEntry>,
) -> String {
    let id = uuid::Uuid::new_v4().to_string();
    let parent_id = timeline
        .thread_heads
        .get(&thread.id)
        .cloned()
        .or_else(|| timeline.current_checkpoint_id.clone());
    let checkpoint = Checkpoint {
        id: id.clone(),
        parent_id,
        source_thread_id: thread.id.clone(),
        // 时间树节点始终用用户提示词命名，便于在右侧直接识别会话分支。
        title: latest_prompt_title(thread),
        created_at: now_ms(),
        workspace_root: root.to_string_lossy().to_string(),
        entries,
        thread_snapshot: thread.clone(),
        automatic,
    };
    if !timeline.thread_ids.iter().any(|id| id == &thread.id) {
        timeline.thread_ids.push(thread.id.clone());
    }
    timeline.thread_heads.insert(thread.id.clone(), id.clone());
    timeline.current_checkpoint_id = Some(id.clone());
    timeline.checkpoints.push(checkpoint);
    id
}

fn append_checkpoint(
    data_dir: &Path,
    timeline: &mut Timeline,
    thread: &Thread,
    automatic: bool,
) -> Result<String, String> {
    let (root, entries) = checkpoint_workspace(data_dir, timeline, thread)?;
    Ok(append_checkpoint_captured(
        timeline, thread, automatic, &root, entries,
    ))
}

pub fn create_checkpoint(data_dir: &Path, thread: &Thread) -> Result<TimelineView, String> {
    let mut store = load_store(data_dir)?;
    let index = match timeline_index(&store, &thread.id) {
        Some(index) => index,
        None => {
            store.timelines.push(Timeline {
                id: uuid::Uuid::new_v4().to_string(),
                root_thread_id: thread.id.clone(),
                thread_ids: vec![thread.id.clone()],
                thread_heads: HashMap::new(),
                current_checkpoint_id: None,
                checkpoints: Vec::new(),
            });
            store.timelines.len() - 1
        }
    };
    append_checkpoint(data_dir, &mut store.timelines[index], thread, false)?;
    save_store(data_dir, &store)?;
    Ok(view_for(&store.timelines[index], &thread.id))
}

pub fn get_timeline(data_dir: &Path, thread_id: &str) -> Result<Option<TimelineView>, String> {
    let store = load_store(data_dir)?;
    Ok(timeline_index(&store, thread_id).map(|index| view_for(&store.timelines[index], thread_id)))
}

pub fn checkpoint_preview(
    data_dir: &Path,
    thread_id: &str,
    checkpoint_id: &str,
) -> Result<Thread, String> {
    let store = load_store(data_dir)?;
    let index = timeline_index(&store, thread_id).ok_or("会话没有世界线时间线")?;
    store.timelines[index]
        .checkpoints
        .iter()
        .find(|checkpoint| checkpoint.id == checkpoint_id)
        .map(|checkpoint| checkpoint.thread_snapshot.clone())
        .ok_or_else(|| "时间点不存在".into())
}

fn prompt_signature(item: &Item) -> Option<PromptSummary> {
    match item {
        Item::User { id, text, .. } => Some(PromptSummary {
            id: *id,
            text: text.clone(),
        }),
        _ => None,
    }
}

/// 删除指定用户提示词所属的完整轮次：从该 user item 起，直到下一条 user item 之前。
/// 使用 id + text 双重匹配，避免历史分叉复用 item id 时误删另一条已编辑分支。
pub fn remove_prompt_turns(thread: &mut Thread, prompts: &[PromptSummary]) -> usize {
    let targets: HashSet<PromptSummary> = prompts.iter().cloned().collect();
    if targets.is_empty() {
        return 0;
    }
    let mut removed_prompts = 0;
    let mut drop_turn = false;
    thread.items.retain(|item| {
        if let Some(prompt) = prompt_signature(item) {
            drop_turn = targets.contains(&prompt);
            if drop_turn {
                removed_prompts += 1;
            }
        }
        !drop_turn
    });
    if removed_prompts > 0 {
        thread.plan = None;
        thread.updated_at = now_ms();
    }
    removed_prompts
}

fn checkpoint_prompt_path(checkpoint: &Checkpoint) -> Vec<PromptSummary> {
    checkpoint
        .thread_snapshot
        .items
        .iter()
        .filter_map(prompt_signature)
        .collect()
}

/// 上下文被手动编辑后，同步改写所有包含目标节点的快照，并按新的提示词前缀关系
/// 重建 checkpoint 父子关系。这样右侧世界线不会把已删除节点作为幽灵旁支继续展示。
pub fn rewrite_after_context_edit(
    data_dir: &Path,
    thread: &Thread,
    prompts: &[PromptSummary],
) -> Result<TimelineView, String> {
    let mut store = load_store(data_dir)?;
    let index = match timeline_index(&store, &thread.id) {
        Some(index) => index,
        None => {
            store.timelines.push(Timeline {
                id: uuid::Uuid::new_v4().to_string(),
                root_thread_id: thread.id.clone(),
                thread_ids: vec![thread.id.clone()],
                thread_heads: HashMap::new(),
                current_checkpoint_id: None,
                checkpoints: Vec::new(),
            });
            store.timelines.len() - 1
        }
    };
    let timeline = &mut store.timelines[index];
    for checkpoint in &mut timeline.checkpoints {
        if remove_prompt_turns(&mut checkpoint.thread_snapshot, prompts) > 0 {
            checkpoint.thread_snapshot.acp_session_id = None;
            checkpoint.thread_snapshot.provider_checkpoints.clear();
            checkpoint.thread_snapshot.pending_native_restore = None;
            checkpoint.thread_snapshot.codex_usage_snapshot = None;
            checkpoint.thread_snapshot.handoff_from =
                Some(checkpoint.thread_snapshot.agent_kind.clone());
            checkpoint.title = latest_prompt_title(&checkpoint.thread_snapshot);
        }
    }

    append_checkpoint(data_dir, timeline, thread, true)?;

    // 删除中间节点后，旧 parentId 可能指向不再是前缀的路径。按当前快照内容重新组装树。
    let paths: Vec<Vec<PromptSummary>> = timeline
        .checkpoints
        .iter()
        .map(checkpoint_prompt_path)
        .collect();
    for child_index in 0..timeline.checkpoints.len() {
        let child = &paths[child_index];
        let mut parent: Option<(usize, usize)> = None;
        for candidate_index in 0..child_index {
            let candidate = &paths[candidate_index];
            if candidate.len() <= child.len()
                && child.starts_with(candidate)
                && parent.map_or(true, |(_, len)| candidate.len() >= len)
            {
                parent = Some((candidate_index, candidate.len()));
            }
        }
        let parent_id =
            parent.map(|(parent_index, _)| timeline.checkpoints[parent_index].id.clone());
        timeline.checkpoints[child_index].parent_id = parent_id;
    }
    save_store(data_dir, &store)?;
    Ok(view_for(&store.timelines[index], &thread.id))
}

/// 编辑历史消息前同时保存“共同历史”和“原会话”两个节点，再把当前会话的头指回
/// 共同历史。编辑后的会话因此从共同历史继续，原来的后续则成为一条可恢复的旁支。
pub fn record_edit_fork(
    data_dir: &Path,
    thread: &Thread,
    item_id: u64,
) -> Result<TimelineView, String> {
    let item_index = thread
        .items
        .iter()
        .position(|item| item.id() == item_id)
        .ok_or("待编辑消息不存在")?;
    let fork_prompt = match &thread.items[item_index] {
        Item::User { text, .. } => text.trim().to_string(),
        _ => return Err("待编辑消息不是用户提示词".into()),
    };
    let mut base_thread = thread.clone();
    base_thread.items.truncate(item_index);
    base_thread.plan = None;
    base_thread.updated_at = now_ms();

    let mut store = load_store(data_dir)?;
    let index = match timeline_index(&store, &thread.id) {
        Some(index) => index,
        None => {
            store.timelines.push(Timeline {
                id: uuid::Uuid::new_v4().to_string(),
                root_thread_id: thread.id.clone(),
                thread_ids: vec![thread.id.clone()],
                thread_heads: HashMap::new(),
                current_checkpoint_id: None,
                checkpoints: Vec::new(),
            });
            store.timelines.len() - 1
        }
    };
    let timeline = &mut store.timelines[index];
    // 共同历史和原会话记录的是同一瞬间的工作区。只扫描、读取、哈希一次；此前大型工作区
    // 会被完整处理两遍，编辑重发看起来像应用立即卡死。
    let (root, entries) = checkpoint_workspace(data_dir, timeline, thread)?;
    let base_id = append_checkpoint_captured(timeline, &base_thread, true, &root, entries.clone());
    if let Some(checkpoint) = timeline.checkpoints.last_mut() {
        checkpoint.title = fork_prompt.clone();
    }
    append_checkpoint_captured(timeline, thread, true, &root, entries);
    if let Some(checkpoint) = timeline.checkpoints.last_mut() {
        checkpoint.title = fork_prompt;
    }
    timeline
        .thread_heads
        .insert(thread.id.clone(), base_id.clone());
    timeline.current_checkpoint_id = Some(base_id);
    save_store(data_dir, &store)?;
    Ok(view_for(&store.timelines[index], &thread.id))
}

fn set_executable(path: &Path, value: bool) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let metadata = fs::metadata(path).map_err(|e| format!("读取权限失败：{e}"))?;
        let mut mode = metadata.permissions().mode();
        if value {
            mode |= 0o111;
        } else {
            mode &= !0o111;
        }
        fs::set_permissions(path, fs::Permissions::from_mode(mode))
            .map_err(|e| format!("恢复文件权限失败：{e}"))?;
    }
    let _ = (path, value);
    Ok(())
}

fn remove_path(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Ok(());
    }
    let metadata = fs::symlink_metadata(path).map_err(|e| format!("读取待删除文件失败：{e}"))?;
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        return Err(format!("拒绝用文件时间点删除目录：{}", path.display()));
    }
    fs::remove_file(path).map_err(|e| format!("删除文件失败 {}：{e}", path.display()))
}

fn write_blob(
    data_dir: &Path,
    root: &Path,
    path: &str,
    hash: &str,
    executable: bool,
) -> Result<(), String> {
    let target = root.join(safe_relative(path)?);
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建恢复目录失败：{e}"))?;
    }
    let bytes = get_blob(data_dir, hash)?;
    let tmp = target.with_extension(format!("nova-{}.tmp", uuid::Uuid::new_v4()));
    fs::write(&tmp, bytes).map_err(|e| format!("写入恢复临时文件失败：{e}"))?;
    if target.exists() {
        remove_path(&target)?;
    }
    fs::rename(&tmp, &target).map_err(|e| format!("恢复文件失败 {}：{e}", target.display()))?;
    set_executable(&target, executable)
}

fn restore_manifest(data_dir: &Path, checkpoint: &Checkpoint) -> Result<(), String> {
    let root = workspace_root(Path::new(&checkpoint.workspace_root))?;
    let current_entries = capture_manifest(data_dir, &root)?;
    let current: HashMap<&str, &PatchEntry> = current_entries
        .iter()
        .map(|entry| (entry.path.as_str(), entry))
        .collect();
    let target: HashMap<&str, &PatchEntry> = checkpoint
        .entries
        .iter()
        .map(|entry| (entry.path.as_str(), entry))
        .collect();
    let paths: BTreeSet<&str> = current.keys().chain(target.keys()).copied().collect();

    // 所有受影响文件先进入对象库。后续任一步失败都能按原内容回滚。
    let mut originals = Vec::new();
    for path in &paths {
        let absolute = root.join(safe_relative(path)?);
        if absolute.exists() {
            let metadata =
                fs::symlink_metadata(&absolute).map_err(|e| format!("读取恢复前文件失败：{e}"))?;
            if !metadata.is_file() {
                return Err(format!("恢复路径不是普通文件：{path}"));
            }
            let bytes = fs::read(&absolute).map_err(|e| format!("备份恢复前文件失败：{e}"))?;
            originals.push((
                (*path).to_string(),
                Some(put_blob(data_dir, &bytes)?),
                executable(&metadata),
            ));
        } else {
            originals.push(((*path).to_string(), None, false));
        }
    }

    let apply = || -> Result<(), String> {
        for path in &paths {
            match target.get(path) {
                Some(entry) => write_blob(data_dir, &root, path, &entry.blob, entry.executable)?,
                None => remove_path(&root.join(safe_relative(path)?))?,
            }
        }
        Ok(())
    };

    if let Err(error) = apply() {
        for (path, blob, mode) in originals {
            let result = match blob {
                Some(hash) => write_blob(data_dir, &root, &path, &hash, mode),
                None => remove_path(&root.join(safe_relative(&path)?)),
            };
            if let Err(rollback) = result {
                return Err(format!("{error}；且回滚 {path} 失败：{rollback}"));
            }
        }
        return Err(error);
    }
    Ok(())
}

pub fn restore_checkpoint(
    data_dir: &Path,
    checkpoint_id: &str,
    current_thread: &Thread,
    restore_files: bool,
) -> Result<(Thread, RestoreResult), String> {
    let mut store = load_store(data_dir)?;
    let timeline_index = store
        .timelines
        .iter()
        .position(|timeline| timeline.checkpoints.iter().any(|cp| cp.id == checkpoint_id))
        .ok_or("时间点不存在")?;
    if self::timeline_index(&store, &current_thread.id) != Some(timeline_index) {
        return Err("不能从另一个会话时间线跳转到该时间点".into());
    }
    let checkpoint_index = store.timelines[timeline_index]
        .checkpoints
        .iter()
        .position(|cp| cp.id == checkpoint_id)
        .ok_or("时间点不存在")?;

    let should_auto_save = {
        let timeline = &store.timelines[timeline_index];
        let current_head = timeline
            .thread_heads
            .get(&current_thread.id)
            .map(String::as_str)
            .or(timeline.current_checkpoint_id.as_deref());
        if let Some(current_id) = current_head {
            let current_checkpoint = timeline.checkpoints.iter().find(|cp| cp.id == current_id);
            current_checkpoint.map_or(true, |cp| {
                serde_json::to_value(&cp.thread_snapshot.items).ok()
                    != serde_json::to_value(&current_thread.items).ok()
                    || cp.thread_snapshot.plan != current_thread.plan
                    || capture_manifest(data_dir, Path::new(&cp.workspace_root))
                        .map(|entries| entries != cp.entries)
                        .unwrap_or(true)
            })
        } else {
            true
        }
    };
    if should_auto_save {
        append_checkpoint(
            data_dir,
            &mut store.timelines[timeline_index],
            current_thread,
            true,
        )?;
    }

    let target = store.timelines[timeline_index].checkpoints[checkpoint_index].clone();
    if restore_files && !current_thread.is_roaming_guest() {
        restore_manifest(data_dir, &target)?;
    }

    let mut thread = target.thread_snapshot.clone();
    // guest 的 host 路由绑定现有 guest thread id；恢复时原位替换镜像，不能生成一个
    // 收不到 host 增量的新 id。普通本地/host/worktree 会话仍创建独立会话分支。
    thread.id = if current_thread.is_roaming_guest() {
        current_thread.id.clone()
    } else {
        uuid::Uuid::new_v4().to_string()
    };
    thread.acp_session_id = None;
    thread.provider_checkpoints.clear();
    thread.pending_native_restore = None;
    thread.codex_usage_snapshot = None;
    thread.handoff_from = Some(thread.agent_kind.clone());
    thread.created_at = now_ms();
    thread.updated_at = thread.created_at;

    let timeline = &mut store.timelines[timeline_index];
    if !timeline.thread_ids.iter().any(|id| id == &thread.id) {
        timeline.thread_ids.push(thread.id.clone());
    }
    timeline
        .thread_heads
        .insert(thread.id.clone(), checkpoint_id.to_string());
    timeline.current_checkpoint_id = Some(checkpoint_id.to_string());
    save_store(data_dir, &store)?;
    let timeline_view = view_for(&store.timelines[timeline_index], &thread.id);
    Ok((
        thread.clone(),
        RestoreResult {
            thread_id: thread.id,
            timeline: timeline_view,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_paths_that_escape_repository() {
        assert!(safe_relative("../secret").is_err());
        assert!(safe_relative("/tmp/secret").is_err());
        assert!(safe_relative("src/main.rs").is_ok());
    }

    #[test]
    fn removes_complete_prompt_turns_and_keeps_neighboring_context() {
        let mut thread = Thread::new(
            ".".into(),
            crate::threads::AgentKind::Alkaid,
            None,
            None,
            None,
            false,
        );
        let first = thread.push_user("first".into(), Vec::new());
        thread.push_system("first result".into(), "info");
        let second = thread.push_user("second".into(), Vec::new());
        thread.push_system("second result".into(), "info");
        thread.push_user("third".into(), Vec::new());

        let removed = remove_prompt_turns(
            &mut thread,
            &[PromptSummary {
                id: second.id(),
                text: "second".into(),
            }],
        );

        assert_eq!(removed, 1);
        let prompts: Vec<_> = thread.items.iter().filter_map(prompt_signature).collect();
        assert_eq!(prompts.len(), 2);
        assert_eq!(prompts[0].id, first.id());
        assert_eq!(prompts[0].text, "first");
        assert_eq!(prompts[1].text, "third");
        assert!(!thread
            .items
            .iter()
            .any(|item| { matches!(item, Item::System { text, .. } if text == "second result") }));
    }

    #[test]
    fn old_worldline_store_is_discarded_without_migration() {
        let root = std::env::temp_dir().join(format!(
            "nova-time-machine-old-store-test-{}",
            uuid::Uuid::new_v4()
        ));
        let data = root.join("data");
        fs::create_dir_all(time_machine_dir(&data)).unwrap();
        fs::write(
            store_path(&data),
            br#"{"version":1,"timelines":[{"id":"old"}]}"#,
        )
        .unwrap();

        let store = load_store(&data).unwrap();
        assert_eq!(store.version, STORE_VERSION);
        assert!(store.timelines.is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn managed_snapshot_ignores_generated_directories() {
        let root = std::env::temp_dir().join(format!(
            "nova-time-machine-ignore-test-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
        fs::create_dir_all(root.join(".git/objects")).unwrap();
        fs::write(root.join("src/main.ts"), b"source").unwrap();
        fs::write(root.join("node_modules/pkg/index.js"), b"dependency").unwrap();
        fs::write(root.join(".git/objects/object"), b"git").unwrap();

        assert_eq!(directory_files(&root).unwrap(), vec!["src/main.ts"]);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn large_files_stay_outside_snapshot_and_restore_scope() {
        let root = std::env::temp_dir().join(format!(
            "nova-time-machine-large-file-test-{}",
            uuid::Uuid::new_v4()
        ));
        let project = root.join("project");
        let data = root.join("data");
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join("source.txt"), b"checkpoint").unwrap();
        let large = project.join("debug-bin.exe");
        let file = fs::File::create(&large).unwrap();
        file.set_len(MAX_SNAPSHOT_FILE_BYTES + 1).unwrap();
        drop(file);

        let thread = Thread::new(
            project.to_string_lossy().to_string(),
            crate::threads::AgentKind::Codex,
            None,
            None,
            None,
            false,
        );
        let checkpoint = create_checkpoint(&data, &thread).unwrap();
        let checkpoint_id = checkpoint.current_checkpoint_id.unwrap();
        fs::write(project.join("source.txt"), b"current").unwrap();
        {
            use std::io::Write;
            let mut file = fs::OpenOptions::new().write(true).open(&large).unwrap();
            file.write_all(b"large file remains unmanaged").unwrap();
        }

        restore_checkpoint(&data, &checkpoint_id, &thread, true).unwrap();
        assert_eq!(fs::read(project.join("source.txt")).unwrap(), b"checkpoint");
        assert_eq!(
            fs::metadata(&large).unwrap().len(),
            MAX_SNAPSHOT_FILE_BYTES + 1
        );
        {
            use std::io::Read;
            let mut prefix = vec![0; b"large file remains unmanaged".len()];
            fs::File::open(&large)
                .unwrap()
                .read_exact(&mut prefix)
                .unwrap();
            assert_eq!(prefix, b"large file remains unmanaged");
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn snapshots_and_restores_a_directory_without_git() {
        let root = std::env::temp_dir().join(format!(
            "nova-time-machine-dir-test-{}",
            uuid::Uuid::new_v4()
        ));
        let project = root.join("project");
        let data = root.join("data");
        fs::create_dir_all(project.join("nested")).unwrap();
        fs::write(project.join("kept.txt"), b"first\n").unwrap();
        fs::write(project.join("nested/removed-later.txt"), b"present\n").unwrap();

        let thread = Thread::new(
            project.to_string_lossy().to_string(),
            crate::threads::AgentKind::Codex,
            None,
            None,
            None,
            false,
        );
        let first = create_checkpoint(&data, &thread).unwrap();
        let first_id = first.current_checkpoint_id.unwrap();

        fs::write(project.join("kept.txt"), b"second\n").unwrap();
        fs::remove_file(project.join("nested/removed-later.txt")).unwrap();
        fs::write(project.join("added-later.txt"), b"new\n").unwrap();
        create_checkpoint(&data, &thread).unwrap();

        restore_checkpoint(&data, &first_id, &thread, true).unwrap();
        assert_eq!(fs::read(project.join("kept.txt")).unwrap(), b"first\n");
        assert_eq!(
            fs::read(project.join("nested/removed-later.txt")).unwrap(),
            b"present\n"
        );
        assert!(!project.join("added-later.txt").exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn roaming_guest_timeline_does_not_require_local_workspace() {
        let root = std::env::temp_dir().join(format!(
            "nova-time-machine-roaming-test-{}",
            uuid::Uuid::new_v4()
        ));
        let data = root.join("data");
        let mut thread = Thread::new(
            "/path/that/only/exists/on/host".into(),
            crate::threads::AgentKind::Codex,
            None,
            None,
            None,
            false,
        );
        thread.roaming_role = Some("guest".into());
        thread.roaming_peer = Some("peer".into());
        thread.roaming_remote_id = Some("host-thread".into());
        thread.items.push(Item::User {
            id: 1,
            text: "远端任务".into(),
            ts: now_ms(),
            images: Vec::new(),
        });

        let view = record_edit_fork(&data, &thread, 1).unwrap();
        let checkpoint_id = view.current_checkpoint_id.unwrap();
        let (restored, _) = restore_checkpoint(&data, &checkpoint_id, &thread, true).unwrap();
        assert_eq!(restored.id, thread.id);
        assert_eq!(restored.roaming_remote_id, thread.roaming_remote_id);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn crossing_timeline_without_checkpoint_restore_keeps_workspace_files() {
        let root = std::env::temp_dir().join(format!(
            "nova-time-machine-no-file-restore-test-{}",
            uuid::Uuid::new_v4()
        ));
        let project = root.join("project");
        let data = root.join("data");
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join("file.txt"), b"checkpoint\n").unwrap();

        let thread = Thread::new(
            project.to_string_lossy().to_string(),
            crate::threads::AgentKind::Codex,
            None,
            None,
            None,
            false,
        );
        let checkpoint = create_checkpoint(&data, &thread).unwrap();
        let checkpoint_id = checkpoint.current_checkpoint_id.unwrap();
        fs::write(project.join("file.txt"), b"current workspace\n").unwrap();

        restore_checkpoint(&data, &checkpoint_id, &thread, false).unwrap();
        assert_eq!(
            fs::read(project.join("file.txt")).unwrap(),
            b"current workspace\n"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn editing_a_prompt_creates_a_named_branch_tree() {
        let root = std::env::temp_dir().join(format!(
            "nova-time-machine-edit-tree-test-{}",
            uuid::Uuid::new_v4()
        ));
        let project = root.join("project");
        let data = root.join("data");
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join("file.txt"), b"working\n").unwrap();
        let mut thread = Thread::new(
            project.to_string_lossy().to_string(),
            crate::threads::AgentKind::Codex,
            None,
            None,
            None,
            false,
        );
        thread.items.push(Item::User {
            id: 1,
            text: "实现右侧时间树".into(),
            ts: now_ms(),
            images: Vec::new(),
        });

        let view = record_edit_fork(&data, &thread, 1).unwrap();
        assert_eq!(view.checkpoints.len(), 2);
        assert_eq!(view.checkpoints[0].title, "实现右侧时间树");
        assert_eq!(view.checkpoints[1].title, "实现右侧时间树");
        assert_eq!(
            view.checkpoints[1].parent_id.as_deref(),
            Some(view.checkpoints[0].id.as_str())
        );
        assert_eq!(
            view.current_checkpoint_id.as_deref(),
            Some(view.checkpoints[0].id.as_str())
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn jumping_to_the_current_branch_point_preserves_workspace_as_a_child() {
        let root = std::env::temp_dir().join(format!(
            "nova-time-machine-current-head-test-{}",
            uuid::Uuid::new_v4()
        ));
        let project = root.join("project");
        let data = root.join("data");
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join("file.txt"), b"branch point\n").unwrap();
        let thread = Thread::new(
            project.to_string_lossy().to_string(),
            crate::threads::AgentKind::Codex,
            None,
            None,
            None,
            false,
        );
        let first = create_checkpoint(&data, &thread).unwrap();
        let first_id = first.current_checkpoint_id.unwrap();

        fs::write(project.join("file.txt"), b"current workspace\n").unwrap();
        let (_, restored) = restore_checkpoint(&data, &first_id, &thread, true).unwrap();
        assert_eq!(
            fs::read(project.join("file.txt")).unwrap(),
            b"branch point\n"
        );
        let saved_workspace = restored
            .timeline
            .checkpoints
            .iter()
            .find(|checkpoint| checkpoint.id != first_id)
            .expect("当前工作区应在跳转前自动保存");
        assert_eq!(
            saved_workspace.parent_id.as_deref(),
            Some(first_id.as_str())
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn restores_files_and_forks_from_the_selected_checkpoint() {
        let root =
            std::env::temp_dir().join(format!("nova-time-machine-test-{}", uuid::Uuid::new_v4()));
        let repo = root.join("repo");
        let data = root.join("data");
        fs::create_dir_all(&repo).unwrap();
        fs::write(repo.join("tracked.txt"), b"base\n").unwrap();

        let thread = Thread::new(
            repo.to_string_lossy().to_string(),
            crate::threads::AgentKind::Codex,
            None,
            None,
            None,
            false,
        );
        fs::write(repo.join("tracked.txt"), b"checkpoint-a\n").unwrap();
        fs::write(repo.join("new-a.txt"), b"new-a\n").unwrap();
        let first = create_checkpoint(&data, &thread).unwrap();
        let first_id = first.current_checkpoint_id.unwrap();

        fs::write(repo.join("tracked.txt"), b"checkpoint-b\n").unwrap();
        fs::remove_file(repo.join("new-a.txt")).unwrap();
        fs::write(repo.join("new-b.txt"), b"new-b\n").unwrap();
        let second = create_checkpoint(&data, &thread).unwrap();
        let second_id = second.current_checkpoint_id.unwrap();
        assert_ne!(first_id, second_id);

        let (fork, restored) = restore_checkpoint(&data, &first_id, &thread, true).unwrap();
        assert_eq!(
            fs::read(repo.join("tracked.txt")).unwrap(),
            b"checkpoint-a\n"
        );
        assert_eq!(fs::read(repo.join("new-a.txt")).unwrap(), b"new-a\n");
        assert!(!repo.join("new-b.txt").exists());
        assert_ne!(fork.id, thread.id);
        assert_eq!(
            restored.timeline.current_checkpoint_id.as_deref(),
            Some(first_id.as_str())
        );

        fs::write(repo.join("tracked.txt"), b"fork\n").unwrap();
        let forked = create_checkpoint(&data, &fork).unwrap();
        let latest = forked.checkpoints.last().unwrap();
        assert_eq!(latest.parent_id.as_deref(), Some(first_id.as_str()));
        let _ = fs::remove_dir_all(root);
    }
}
