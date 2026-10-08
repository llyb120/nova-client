use crate::AppState;
use base64::Engine;
use serde::Serialize;
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};
use tauri::{Emitter, State};

const TEXT_LIMIT: u64 = 256 * 1024;
const DOCUMENT_LIMIT: u64 = 16 * 1024 * 1024;

fn text_limit(path: &Path) -> u64 {
    match path.extension().and_then(|ext| ext.to_str()).unwrap_or("").to_ascii_lowercase().as_str() {
        "md" | "markdown" | "html" | "htm" => DOCUMENT_LIMIT,
        _ => TEXT_LIMIT,
    }
}
const SHEET_LIMIT: u64 = 16 * 1024 * 1024;
const IMAGE_LIMIT: u64 = 8 * 1024 * 1024;

// 用户拖入或在侧栏打开的文件；授权精确到文件，随应用退出清空。
static DROPPED_FILES: std::sync::LazyLock<std::sync::Mutex<std::collections::HashSet<PathBuf>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashSet::new()));

pub(crate) fn allow_dropped_files(paths: &[PathBuf]) {
    if let Ok(mut allowed) = DROPPED_FILES.lock() {
        for path in paths {
            if let Ok(path) = path.canonicalize() {
                if path.is_file() { allowed.insert(path); }
            }
        }
    }
}
const ENTRY_LIMIT: usize = 500;

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct GitEntry {
    path: String,
    old_path: Option<String>,
    index: String,
    worktree: String,
}

fn git_entries(repo: &str) -> Result<Vec<GitEntry>, String> {
    let status = crate::gitwt::run_raw(
        repo,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )?;
    let mut records = status.split('\0').filter(|s| !s.is_empty());
    let mut entries = Vec::new();
    while let Some(record) = records.next() {
        if record.len() < 4 || !record.is_char_boundary(3) {
            return Err("Git 状态格式无效".into());
        }
        let index = &record[..1];
        let worktree = &record[1..2];
        let old_path = if matches!(index, "R" | "C") || matches!(worktree, "R" | "C") {
            Some(records.next().ok_or("Git 重命名记录不完整")?.to_string())
        } else {
            None
        };
        entries.push(GitEntry {
            path: record[3..].into(),
            old_path,
            index: index.into(),
            worktree: worktree.into(),
        });
    }
    Ok(entries)
}

#[tauri::command]
pub async fn workspace_git_status(
    state: State<'_, AppState>,
    thread_id: String,
) -> Result<serde_json::Value, String> {
    let cwd = root(&state, &thread_id)?;
    tauri::async_runtime::spawn_blocking(move || {
        let repo = crate::gitwt::run(
            cwd.to_str().ok_or("路径编码无效")?,
            &["rev-parse", "--show-toplevel"],
        )?;
        Ok(serde_json::json!({ "repo": repo, "files": git_entries(&repo)? }))
    })
    .await
    .map_err(|e| e.to_string())?
}

fn git_patch(repo: &str, path: &str, staged: bool, full_context: bool) -> Result<String, String> {
    // Validate against Git's own inventory, including deleted paths that cannot be canonicalized.
    let entry = git_entries(repo)?
        .into_iter()
        .find(|e| e.path == path)
        .ok_or("文件已不在 Git 变动列表中，请刷新")?;
    if entry.index == "?" {
        if staged {
            return Err("未跟踪文件没有暂存差异".into());
        }
        let preview = read_preview(Path::new(repo), path)?;
        return Ok(match preview.text {
            Some(text) => format!(
                "--- /dev/null\n+++ {}\n@@ -0,0 +1,{} @@\n{}",
                path,
                text.lines().count(),
                text.split_inclusive('\n')
                    .map(|line| format!("+{line}"))
                    .collect::<String>()
            ),
            None => "二进制文件或文件超过预览上限，请打开文件查看".into(),
        });
    }
    let mut args = vec![
        "--literal-pathspecs",
        "diff",
        "--no-ext-diff",
        "--no-textconv",
        "--no-color",
        if full_context { "--unified=1000000" } else { "--unified=3" },
    ];
    if staged {
        args.push("--cached");
    }
    args.extend(["--", path]);
    if let Some(old) = entry.old_path.as_deref() {
        args.push(old);
    }
    let patch = crate::gitwt::run_raw(repo, &args)?;
    // ponytail: 单文件完整上下文最多 2 MB；更大差异改用按 hunk 分页。
    if patch.len() > 2 * 1024 * 1024 {
        return Err("差异超过 2 MB，请在外部编辑器查看".into());
    }
    Ok(patch)
}

/// 图片扩展名对应的 MIME；非图片返回 None。
fn image_mime(path: &str) -> Option<&'static str> {
    Some(match Path::new(path).extension()?.to_str()?.to_lowercase().as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        "avif" => "image/avif",
        "svg" => "image/svg+xml",
        _ => return None,
    })
}

/// 图片变动的两份内容（base64 data URI），供源码管理内联对比。
/// before：暂存差异取 HEAD 版本，工作区差异取索引版本；after：索引版本 / 工作区文件。
fn git_image(repo: &str, path: &str, staged: bool) -> Result<serde_json::Value, String> {
    let entry = git_entries(repo)?
        .into_iter()
        .find(|e| e.path == path)
        .ok_or("文件已不在 Git 变动列表中，请刷新")?;
    let mime = image_mime(path).ok_or("只有图片支持内联对比")?;
    // 缺失的版本（新增/删除/未跟踪）以及超限的图都留空，由前端标注。
    let blob = |spec: &str| -> Option<Vec<u8>> {
        crate::gitwt::run_bytes(repo, &["show", spec])
            .ok()
            .filter(|bytes| bytes.len() as u64 <= IMAGE_LIMIT)
    };
    let index = format!(":{path}");
    let (before, after) = if staged {
        (
            blob(&format!("HEAD:{}", entry.old_path.as_deref().unwrap_or(path))),
            blob(&index),
        )
    } else {
        let working = resolve(Path::new(repo), path)
            .ok()
            .and_then(|p| fs::read(p).ok())
            .filter(|bytes| bytes.len() as u64 <= IMAGE_LIMIT);
        (blob(&index), working)
    };
    let data_uri = |bytes: Option<Vec<u8>>| {
        bytes.map(|b| format!("data:{mime};base64,{}", base64::engine::general_purpose::STANDARD.encode(b)))
    };
    Ok(serde_json::json!({ "before": data_uri(before), "after": data_uri(after) }))
}

#[tauri::command]
pub async fn workspace_git_diff(
    state: State<'_, AppState>,
    thread_id: String,
    path: String,
    staged: bool,
    full_context: Option<bool>,
) -> Result<String, String> {
    let cwd = root(&state, &thread_id)?;
    tauri::async_runtime::spawn_blocking(move || {
        let repo = crate::gitwt::run(
            cwd.to_str().ok_or("路径编码无效")?,
            &["rev-parse", "--show-toplevel"],
        )?;
        git_patch(&repo, &path, staged, full_context.unwrap_or(false))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// 图片文件的新旧两份内容（base64 data URI），让源码管理里直接看到图而不是「Binary files differ」。
#[tauri::command]
pub async fn workspace_git_image(
    state: State<'_, AppState>,
    thread_id: String,
    path: String,
    staged: bool,
) -> Result<serde_json::Value, String> {
    let cwd = root(&state, &thread_id)?;
    tauri::async_runtime::spawn_blocking(move || {
        let repo = crate::gitwt::run(
            cwd.to_str().ok_or("路径编码无效")?,
            &["rev-parse", "--show-toplevel"],
        )?;
        git_image(&repo, &path, staged)
    })
    .await
    .map_err(|e| e.to_string())?
}

pub(crate) fn root(state: &AppState, id: &str) -> Result<PathBuf, String> {
    let store = state.store.lock().map_err(|e| e.to_string())?;
    let thread = store.get(id).ok_or("会话不存在")?;
    if thread.roaming_role.as_deref() == Some("guest") {
        return Err("请先召回漫游会话，再浏览本地文件".into());
    }
    Ok(PathBuf::from(&thread.cwd))
}

pub(crate) fn resolve(root: &Path, path: &str) -> Result<PathBuf, String> {
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    let path = root.join(path).canonicalize().map_err(|e| e.to_string())?;
    if !path.starts_with(&root) && (!path.is_file() || !DROPPED_FILES.lock().map_err(|e| e.to_string())?.contains(&path)) {
        return Err("只能操作当前工作目录内的文件".into());
    }
    Ok(path)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    name: String,
    path: String,
    directory: bool,
}

#[derive(Serialize)]
pub struct Listing {
    entries: Vec<Entry>,
    truncated: bool,
}

#[tauri::command]
pub async fn list_workspace_directory(
    state: State<'_, AppState>,
    thread_id: String,
    path: String,
) -> Result<Listing, String> {
    let root = root(&state, &thread_id)?;
    tauri::async_runtime::spawn_blocking(move || {
        let path = resolve(&root, &path)?;
        let mut entries = Vec::new();
        let mut truncated = false;
        // ponytail: 单层最多 500 项，超大目录用外部文件管理器；需要全量时升级为游标分页。
        for entry in fs::read_dir(path)
            .map_err(|e| e.to_string())?
            .take(ENTRY_LIMIT + 1)
        {
            let entry = entry.map_err(|e| e.to_string())?;
            if entries.len() == ENTRY_LIMIT {
                truncated = true;
                break;
            }
            entries.push(Entry {
                name: entry.file_name().to_string_lossy().into_owned(),
                path: entry.path().to_string_lossy().into_owned(),
                directory: entry.file_type().map_err(|e| e.to_string())?.is_dir(),
            });
        }
        entries.sort_by_cached_key(|e| (!e.directory, e.name.to_lowercase()));
        Ok(Listing { entries, truncated })
    })
    .await
    .map_err(|e| e.to_string())?
}

#[derive(Serialize)]
pub struct Preview {
    path: String,
    kind: &'static str,
    text: Option<String>,
    size: u64,
    data: Option<String>,
}

fn search_names(root: &Path, query: &str) -> Result<Listing, String> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return Ok(Listing {
            entries: vec![],
            truncated: false,
        });
    }
    if query.len() > 256 {
        return Err("搜索词过长".into());
    }
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    let start = std::time::Instant::now();
    let mut entries = Vec::new();
    let mut truncated = false;
    // ponytail: 搜索最多 200 条、10 万目录项或 1.5 秒；超大项目缩小关键词，后续可升级文件名索引。
    let walker = ignore::WalkBuilder::new(&root)
        .hidden(false)
        .require_git(false)
        .follow_links(false)
        .filter_entry(|entry| {
            entry.depth() == 0
                || !entry.file_type().is_some_and(|t| t.is_dir())
                || !matches!(
                    entry.file_name().to_str(),
                    Some(".git" | "node_modules" | "target")
                )
        })
        .build();
    for (count, entry) in walker.enumerate() {
        if count >= 100_000 || start.elapsed().as_millis() > 1500 {
            truncated = true;
            break;
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                truncated = true;
                continue;
            }
        };
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let name = entry.file_name().to_string_lossy();
        if !name.to_lowercase().contains(&query) {
            continue;
        }
        if entries.len() == 200 {
            truncated = true;
            break;
        }
        entries.push(Entry {
            name: name.into_owned(),
            path: entry.path().to_string_lossy().into_owned(),
            directory: false,
        });
    }
    entries.sort_by_cached_key(|entry| (entry.name.to_lowercase(), entry.path.clone()));
    Ok(Listing { entries, truncated })
}

#[tauri::command]
pub async fn search_workspace_files(
    state: State<'_, AppState>,
    thread_id: String,
    query: String,
) -> Result<Listing, String> {
    let root = root(&state, &thread_id)?;
    tauri::async_runtime::spawn_blocking(move || search_names(&root, &query))
        .await
        .map_err(|e| e.to_string())?
}

fn read_preview(root: &Path, path: &str) -> Result<Preview, String> {
    let path = resolve(root, path)?;
    let mut file = fs::File::open(&path).map_err(|e| e.to_string())?;
    let meta = file.metadata().map_err(|e| e.to_string())?;
    if !meta.is_file() {
        return Err("请选择普通文件".into());
    }
    let ext = path
        .extension()
        .and_then(|v| v.to_str())
        .unwrap_or("")
        .to_lowercase();
    let mut result = Preview {
        path: path.to_string_lossy().into_owned(),
        kind: "external",
        text: None,
        size: meta.len(),
        data: None,
    };
    if matches!(
        ext.as_str(),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" | "avif" | "svg"
    ) {
        if meta.len() <= 16 * 1024 * 1024 {
            result.kind = "image";
        }
        return Ok(result);
    }
    // Univer 在 WebView 内编辑；旧 Office 格式继续交给系统打开。
    if ext == "xlsx" {
        if meta.len() > SHEET_LIMIT {
            return Err("表格超过 16 MB，请使用系统打开".into());
        }
        let mut bytes = Vec::new();
        (&mut file).take(SHEET_LIMIT + 1).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
        if bytes.len() as u64 > SHEET_LIMIT {
            return Err("表格超过 16 MB，请使用系统打开".into());
        }
        result.kind = "spreadsheet";
        result.data = Some(base64::engine::general_purpose::STANDARD.encode(bytes));
        return Ok(result);
    }
    let limit = text_limit(&path);
    if meta.len() > limit {
        return Ok(result);
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > limit || bytes.contains(&0) {
        return Ok(result);
    }
    if let Ok(text) = String::from_utf8(bytes) {
        result.kind = match ext.as_str() {
            "md" | "markdown" => "markdown",
            "html" | "htm" => "html",
            _ => "text",
        };
        result.text = Some(text);
    }
    Ok(result)
}

#[tauri::command]
pub async fn preview_workspace_file(
    state: State<'_, AppState>,
    thread_id: String,
    path: String,
) -> Result<Preview, String> {
    let root = root(&state, &thread_id)?;
    tauri::async_runtime::spawn_blocking(move || open_preview(&root, &path))
        .await
        .map_err(|e| e.to_string())?
}

fn open_preview(root: &Path, path: &str) -> Result<Preview, String> {
    // 与系统打开一致，用户点击的绝对路径可以位于工作区外；不放开目录遍历。
    if Path::new(path).is_absolute() {
        allow_dropped_files(&[PathBuf::from(path)]);
    }
    read_preview(root, path)
}

fn save_text(root: &Path, path: &str, original: &str, text: &str) -> Result<(), String> {
    let resolved = resolve(root, path)?;
    if resolved.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("xlsx")) {
        if original.len() as u64 > SHEET_LIMIT * 4 / 3 + 4 || text.len() as u64 > SHEET_LIMIT * 4 / 3 + 4 {
            return Err("表格超过 16 MB，无法保存".into());
        }
        let decode = |value: &str| base64::engine::general_purpose::STANDARD.decode(value).map_err(|e| e.to_string());
        let original = decode(original)?;
        let bytes = decode(text)?;
        if bytes.len() as u64 > SHEET_LIMIT || !bytes.starts_with(b"PK\x03\x04") {
            return Err("无效或过大的 XLSX 文件".into());
        }
        return save_bytes(&resolved, &original, &bytes, SHEET_LIMIT);
    }
    let limit = text_limit(&resolved);
    if text.len() as u64 > limit || text.contains('\0') {
        return Err(format!("保存内容必须为不超过 {} KB 的文本", limit / 1024));
    }
    if read_preview(root, path)?.text.as_deref() != Some(original) {
        return Err("文件已被外部修改或不支持编辑。草稿已保留，请重新读取文件后合并修改。".into());
    }
    save_bytes(&resolved, original.as_bytes(), text.as_bytes(), limit)
}

fn save_bytes(path: &Path, original: &[u8], bytes: &[u8], limit: u64) -> Result<(), String> {
    // ponytail: 串行化 Nova 内的保存；外部编辑器采用写入前内容校验，不提供跨进程事务。
    static SAVE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = SAVE_LOCK.lock().map_err(|e| e.to_string())?;
    let unchanged = || -> Result<bool, String> {
        let mut current = Vec::new();
        fs::File::open(path).map_err(|e| e.to_string())?.take(limit + 1)
            .read_to_end(&mut current).map_err(|e| e.to_string())?;
        Ok(current == original)
    };
    if !unchanged()? {
        return Err("文件已被外部修改或不支持编辑。草稿已保留，请重新读取文件后合并修改。".into());
    }
    let permissions = fs::metadata(&path)
        .map_err(|e| e.to_string())?
        .permissions();
    if permissions.readonly() {
        return Err("文件为只读，无法保存".into());
    }
    let temp = path.with_file_name(format!(".nova-save-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(|e| e.to_string())?;
        file.write_all(bytes).map_err(|e| e.to_string())?;
        fs::set_permissions(&temp, permissions).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        drop(file);
        if !unchanged()? {
            return Err("保存期间文件发生变化，草稿已保留".into());
        }
        // XLSX 转换不承诺保留图表等高级对象；原文件始终留一份可恢复的副本。
        if limit == SHEET_LIMIT {
            let backup = path.with_file_name(format!(".{}.nova-backup-{}.xlsx", path.file_stem().unwrap_or_default().to_string_lossy(), uuid::Uuid::new_v4()));
            let mut backup_file = fs::OpenOptions::new().write(true).create_new(true).open(backup).map_err(|e| format!("备份失败，未覆盖原文件：{e}"))?;
            backup_file.write_all(original).and_then(|_| backup_file.sync_all()).map_err(|e| format!("备份失败，未覆盖原文件：{e}"))?;
            if !unchanged()? {
                return Err("备份期间文件发生变化，草稿已保留".into());
            }
        }
        // 同目录替换，不先删除原文件；写入/替换失败时原文件保持完整。
        fs::rename(&temp, &path).map_err(|e| format!("保存失败：{e}"))
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}

#[tauri::command]
pub async fn save_workspace_file(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    thread_id: String,
    path: String,
    original: String,
    text: String,
) -> Result<(), String> {
    let root = root(&state, &thread_id)?;
    let saved_path = path.clone();
    tauri::async_runtime::spawn_blocking(move || save_text(&root, &path, &original, &text))
        .await
        .map_err(|e| e.to_string())??;
    let mut store = state.store.lock().map_err(|e| e.to_string())?;
    if let Some(thread) = store.get_mut(&thread_id) {
        let item = crate::threads::Item::Tool {
            id: thread.next_item_id(),
            ts: crate::threads::now_ms(),
            call: crate::threads::ToolCall {
                tool_call_id: format!("workspace-save-{}", uuid::Uuid::new_v4()),
                title: format!("手动保存 {saved_path}"),
                kind: "edit".into(),
                status: "completed".into(),
                content: vec![],
                locations: vec![serde_json::json!({"path": saved_path})],
                raw_input: None,
                raw_output: None,
            },
        };
        thread.items.push(item.clone());
        thread.updated_at = crate::threads::now_ms();
        store.save_thread(&thread_id);
        let _ = app.emit(
            crate::acp::EV_UPDATE,
            serde_json::json!({"threadId":thread_id,"op":{"t":"upsert","item":item}}),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn document_preview_and_save_share_the_larger_limit() {
        let dir = tempfile::tempdir().unwrap();
        let text = "x".repeat(3 * 1024 * 1024);
        for name in ["report.html", "report.HTM", "report.md", "report.markdown"] {
            fs::write(dir.path().join(name), &text).unwrap();
            assert_eq!(read_preview(dir.path(), name).unwrap().text.as_deref(), Some(text.as_str()));
            save_text(dir.path(), name, &text, &(text.clone() + "edited")).unwrap();
            assert!(save_text(dir.path(), name, &text, "stale").is_err());
            fs::File::create(dir.path().join(name)).unwrap().set_len(DOCUMENT_LIMIT + 1).unwrap();
            assert_eq!(read_preview(dir.path(), name).unwrap().kind, "external");
        }
    }
    #[test]
    fn clicked_external_file_can_be_previewed_without_allowing_its_directory() {
        let workspace = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        let file = external.path().join("修复包 0.1.4.zip");
        fs::write(&file, b"PK\x03\x04\0").unwrap();
        let sibling = external.path().join("private.txt");
        fs::write(&sibling, "private").unwrap();
        let preview = open_preview(workspace.path(), file.to_str().unwrap()).unwrap();
        assert_eq!(preview.kind, "external");
        assert_eq!(PathBuf::from(preview.path), file.canonicalize().unwrap());
        assert!(read_preview(workspace.path(), sibling.to_str().unwrap()).is_err());
        assert!(open_preview(workspace.path(), external.path().to_str().unwrap()).is_err());
    }

    #[test]
    fn dropped_files_allow_only_explicit_files_outside_workspace() {
        let workspace = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        let file = external.path().join("dropped.txt");
        let sibling = external.path().join("private.txt");
        fs::write(&file, "original").unwrap();
        fs::write(&sibling, "private").unwrap();
        let path = file.to_str().unwrap();
        assert!(read_preview(workspace.path(), path).is_err());
        allow_dropped_files(&[file.clone(), external.path().to_path_buf()]);
        assert_eq!(read_preview(workspace.path(), path).unwrap().text.as_deref(), Some("original"));
        save_text(workspace.path(), path, "original", "edited").unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "edited");
        assert!(save_text(workspace.path(), path, "original", "stale").is_err());
        assert!(read_preview(workspace.path(), sibling.to_str().unwrap()).is_err());
        assert!(resolve(workspace.path(), external.path().to_str().unwrap()).is_err());
    }
    #[test]
    fn spreadsheet_save_preserves_backup_and_rejects_conflicts() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let old = b"PK\x03\x04original";
        let new = b"PK\x03\x04edited";
        let encode = |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);
        fs::write(root.join("table.xlsx"), old).unwrap();
        let preview = read_preview(root, "table.xlsx").unwrap();
        assert_eq!(preview.data.as_deref(), Some(encode(old).as_str()));
        assert!(preview.text.is_none());
        save_text(root, "table.xlsx", &encode(old), &encode(new)).unwrap();
        assert_eq!(fs::read(root.join("table.xlsx")).unwrap(), new);
        let backups: Vec<_> = fs::read_dir(root).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().contains("nova-backup")).collect();
        assert_eq!(backups.len(), 1);
        assert_eq!(fs::read(backups[0].path()).unwrap(), old);
        assert!(save_text(root, "table.xlsx", &encode(old), &encode(new)).is_err());
        assert!(save_text(root, "table.xlsx", &encode(new), "bad").is_err());
        assert!(save_text(root, "table.xlsx", &encode(new), &encode(b"plain text")).is_err());
        assert_eq!(fs::read(root.join("table.xlsx")).unwrap(), new);
        let large = fs::File::create(root.join("large.xlsx")).unwrap();
        large.set_len(SHEET_LIMIT + 1).unwrap();
        assert!(read_preview(root, "large.xlsx").is_err());
    }
    #[test]
    fn git_index_worktree_and_untracked_diffs() {
        let dir = std::env::temp_dir().join(format!("nova-git-preview-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let repo = dir.to_str().unwrap();
        crate::gitwt::run(repo, &["init"]).unwrap();
        fs::write(dir.join("中文 file.txt"), "base\n").unwrap();
        crate::gitwt::run(repo, &["add", "."]).unwrap();
        assert!(git_patch(repo, "中文 file.txt", true, false)
            .unwrap()
            .contains("+base"));
        crate::gitwt::run(
            repo,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-m",
                "base",
            ],
        )
        .unwrap();
        fs::write(dir.join("中文 file.txt"), "staged\n").unwrap();
        crate::gitwt::run(repo, &["add", "."]).unwrap();
        fs::write(dir.join("中文 file.txt"), "working\n").unwrap();
        fs::write(dir.join("new.txt"), "new\n").unwrap();
        let entries = git_entries(repo).unwrap();
        assert!(entries
            .iter()
            .any(|e| e.path == "中文 file.txt" && e.index == "M" && e.worktree == "M"));
        let staged = git_patch(repo, "中文 file.txt", true, false).unwrap();
        assert!(staged.contains("-base\n+staged\n"));
        let working = git_patch(repo, "中文 file.txt", false, false).unwrap();
        assert!(working.contains("-staged\n+working\n"));
        assert!(git_patch(repo, "new.txt", false, false)
            .unwrap()
            .contains("+new\n"));
        assert!(git_patch(repo, "../outside", false, false).is_err());
        crate::gitwt::run(repo, &["restore", "--staged", "中文 file.txt"]).unwrap();
        crate::gitwt::run(repo, &["mv", "中文 file.txt", "renamed.txt"]).unwrap();
        assert!(git_entries(repo)
            .unwrap()
            .iter()
            .any(|e| e.path == "renamed.txt" && e.old_path.as_deref() == Some("中文 file.txt")));
        fs::remove_file(dir.join("renamed.txt")).unwrap();
        assert!(git_patch(repo, "renamed.txt", true, false)
            .unwrap()
            .contains("rename to renamed.txt"));
        assert!(git_patch(repo, "renamed.txt", false, false)
            .unwrap()
            .contains("-base"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn git_patch_loads_full_context_only_on_request() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().to_str().unwrap();
        crate::gitwt::run(repo, &["init"]).unwrap();
        let original: String = (0..50_000).map(|i| format!("line {i}: unchanged content for diff preview\n")).collect();
        fs::write(dir.path().join("large.txt"), &original).unwrap();
        crate::gitwt::run(repo, &["add", "large.txt"]).unwrap();
        fs::write(dir.path().join("large.txt"), original.replace("line 25000:", "edited 25000:")).unwrap();
        let patch = git_patch(repo, "large.txt", false, false).unwrap();
        assert!(patch.contains("-line 25000:") && patch.contains("+edited 25000:"));
        assert!(patch.len() < 1_000, "small edits must not transfer the whole file");
        // The full file exceeds the existing preview cap; collapsed diffs must still work.
        assert!(git_patch(repo, "large.txt", false, true).unwrap_err().contains("2 MB"));
        let smaller = original.lines().take(100).collect::<Vec<_>>().join("\n") + "\n";
        fs::write(dir.path().join("large.txt"), &smaller).unwrap();
        crate::gitwt::run(repo, &["add", "large.txt"]).unwrap();
        fs::write(dir.path().join("large.txt"), smaller.replace("line 50:", "edited 50:")).unwrap();
        let full = git_patch(repo, "large.txt", false, true).unwrap();
        assert!(full.contains(" line 0:") && full.contains(" line 99:"));
        assert!(full.contains("-line 50:") && full.contains("+edited 50:"));
    }
    #[test]
    fn git_image_returns_before_and_after_data_uris() {
        let dir = std::env::temp_dir().join(format!("nova-git-image-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let repo = dir.to_str().unwrap();
        crate::gitwt::run(repo, &["init"]).unwrap();
        let encode = |bytes: &[u8]| {
            format!(
                "data:image/png;base64,{}",
                base64::engine::general_purpose::STANDARD.encode(bytes)
            )
        };
        let old = b"\x89PNG\r\n\x1a\nold";
        let new = b"\x89PNG\r\n\x1a\nnew";
        fs::write(dir.join("icon.png"), old).unwrap();
        fs::write(dir.join("readme.txt"), "text\n").unwrap();
        crate::gitwt::run(repo, &["add", "."]).unwrap();
        // 工作区差异：before = 索引版本，after = 工作区文件。
        fs::write(dir.join("icon.png"), new).unwrap();
        let working = git_image(repo, "icon.png", false).unwrap();
        assert_eq!(working["before"], encode(old));
        assert_eq!(working["after"], encode(new));
        assert!(git_image(repo, "readme.txt", false).is_err());
        // 暂存差异：before = HEAD 版本，after = 索引版本。
        crate::gitwt::run(repo, &["add", "."]).unwrap();
        crate::gitwt::run(
            repo,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-m",
                "icon",
            ],
        )
        .unwrap();
        fs::write(dir.join("icon.png"), b"\x89PNG\r\n\x1a\nstaged").unwrap();
        crate::gitwt::run(repo, &["add", "."]).unwrap();
        fs::write(dir.join("icon.png"), b"\x89PNG\r\n\x1a\nlater").unwrap();
        let staged = git_image(repo, "icon.png", true).unwrap();
        assert_eq!(staged["before"], encode(new));
        assert_eq!(staged["after"], encode(b"\x89PNG\r\n\x1a\nstaged"));
        // 新增的未跟踪文件没有旧版本。
        fs::write(dir.join("fresh.svg"), "<svg/>").unwrap();
        assert!(git_image(repo, "fresh.svg", false).unwrap()["before"].is_null());
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn bounded_preview_and_workspace_boundary() {
        let root = std::env::temp_dir().join(format!("nova-preview-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("readme.md"), "# hello").unwrap();
        assert_eq!(read_preview(&root, "readme.md").unwrap().kind, "markdown");
        fs::write(root.join("large.txt"), vec![b'x'; TEXT_LIMIT as usize + 1]).unwrap();
        assert_eq!(read_preview(&root, "large.txt").unwrap().kind, "external");
        fs::write(root.join("binary"), [0, 1, 2]).unwrap();
        assert_eq!(read_preview(&root, "binary").unwrap().kind, "external");
        fs::write(root.join("table.xlsx"), [0x50, 0x4B, 3, 4]).unwrap();
        assert_eq!(read_preview(&root, "table.xlsx").unwrap().kind, "spreadsheet");
        assert!(resolve(&root, "..").is_err());
        assert!(read_preview(&root, "missing").is_err());
        save_text(&root, "readme.md", "# hello", "中文\r\n").unwrap();
        assert_eq!(
            fs::read_to_string(root.join("readme.md")).unwrap(),
            "中文\r\n"
        );
        assert!(save_text(&root, "readme.md", "# hello", "stale").is_err());
        assert!(save_text(
            &root,
            "readme.md",
            "中文\r\n",
            &"x".repeat(DOCUMENT_LIMIT as usize + 1)
        )
        .is_err());
        assert_eq!(
            fs::read_to_string(root.join("readme.md")).unwrap(),
            "中文\r\n"
        );
        assert_eq!(fs::read_dir(&root).unwrap().count(), 5); // Four inputs plus the save backup.
        fs::create_dir(root.join("nested")).unwrap();
        fs::write(root.join("nested/Result.md"), "ok").unwrap();
        fs::write(root.join(".gitignore"), "hidden-result.md\n").unwrap();
        fs::write(root.join("hidden-result.md"), "ignored").unwrap();
        let results = search_names(&root, "RESULT").unwrap();
        assert_eq!(results.entries.len(), 1);
        assert_eq!(results.entries[0].name, "Result.md");
        assert!(search_names(&root, " ").unwrap().entries.is_empty());
        fs::remove_dir_all(root).unwrap();
    }
}
