use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant, UNIX_EPOCH};
use wait_timeout::ChildExt;
use walkdir::WalkDir;

#[cfg(unix)]
use std::os::unix::io::AsRawFd;
#[cfg(windows)]
use std::os::windows::io::AsRawHandle;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

// Keep in lockstep with scripts/ctx-index.mjs INDEX_CACHE_VERSION.
const CACHE_VERSION: u32 = 14;
const MAX_INDEX_FILE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_WALK_FILES: usize = 8_000;
const MAX_HITS_PER_FILE: usize = 60;
const MAX_HIT_LINES: usize = 6_000;
const MAX_LINE_CHARS: usize = 240;
const DEFAULT_HARD_BYTES: usize = 32 * 1024;
const MIN_HARD_BYTES: usize = 8 * 1024;
const MAX_HARD_BYTES: usize = 64 * 1024;
const DEFAULT_BUDGET: usize = 600;
const MIN_BUDGET: usize = 100;
const MAX_BUDGET: usize = 1200;
const MAX_CANDIDATES: usize = 8;
const MAX_UNITS_PER_FILE: usize = 4;
const FULL_FILE_MAX: usize = 100;
const EXPLICIT_FULL_MAX: usize = 300;
const SUBJECT_FULL_MAX: usize = 800;
const MAX_SUBJECT_UNITS: usize = 30;
const SUBJECT_BONUS: f64 = 600.0;
/// 高频泛词（命中行数超过该值）不作为种子定义，避免同名函数把噪声反馈回排名。
const SEED_FREQ_CAP: usize = 200;
const DID_YOU_MEAN_MAX: usize = 6;
/// 锚点拆词参与建议的最短词长：更短的泛词噪声大且检索贵。
const DID_YOU_MEAN_MIN_WORD: usize = 4;
const MAX_FILES: usize = 4;
const MAX_DEPS: usize = 8;
const MAX_DEP_FILES: usize = 4;
const MAX_IMPACT: usize = 20;
const MAX_GRAPH_TERMS: usize = 12;
/// 反向 import 图无论变更规模都做增量补丁：大仓库一次 rebase/批量改动不应触发
/// O(全部 import × resolve_specifier) 的整图重建；补丁只动受影响边（先按 importer
/// 删旧边，再按新 import 表加边），复杂度与变更文件数成正比。
///
/// 磁盘持久化最少间隔：整仓缓存的 bincode 序列化成本随仓库线性增长，异步写盘
/// 按该间隔合并；间隔内的中间态只保留在内存 MEMO 中，冷启动时按 size+mtime 重扫
/// 补差即可，不阻塞查询。
const PERSIST_INTERVAL_MS: u64 = 30_000;
/// 文件清单（git ls-files）缓存的有效期。大仓库列全仓文件是百毫秒到秒级的固定
/// 开销，与本次查询的关键词无关；TTL 内复用，过期后台刷新，查询路径不再每次列仓。
const FILE_LIST_TTL_MS: u64 = 30_000;
/// 查询发现覆盖代码及承载实现细节的文本资源；资源文件不进入符号索引。
const CODE_FILE_EXTENSIONS: &[&str] = &[
    "js", "jsx", "mjs", "cjs", "ts", "tsx", "mts", "cts", "vue", "svelte", "rs", "py", "pyi", "go",
    "java", "kt", "kts", "cs", "c", "h", "cc", "cpp", "hpp", "swift", "php", "scala", "dart", "m",
    "mm", "zig", "sql", "md",
];
const MAX_SEARCH_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
const MAX_FILE_LIST_OUTPUT_BYTES: usize = 32 * 1024 * 1024;
/// 全仓 rg 的单阶段硬上限。正常热查询应远低于此值；磁盘/杀软异常或并发争用时
/// 宁可返回当前有界结果，也不能无限占住 blocking worker 并放大后续查询。
const SEARCH_DEADLINE_MS: u64 = 3_000;
const SEARCH_SNAPSHOT_VERSION: u32 = 4;
const SEARCH_INDEX_MIN_TOKEN: usize = 2;
const SEARCH_GIT_POLL_MS: u64 = 2_000;
const SEARCH_COMPACT_INTERVAL_MS: u64 = 15 * 60 * 1_000;
const SEARCH_COMPACT_CHANGED_FILES: usize = 256;
const SEARCH_MAX_INCREMENTAL_FILES: usize = 2_000;
const SEARCH_INDEX_MAX_CANDIDATES: usize = 512;
const CO_CHANGE_DEADLINE_MS: u64 = 750;
const CO_CHANGE_MAX_OUTPUT_BYTES: usize = 4 * 1024 * 1024;
const CO_CHANGE_DEFAULT_HISTORY_DAYS: u64 = 730;
const CO_CHANGE_CACHE_FILES: usize = 1_024;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Symbol {
    ln: usize,
    end: usize,
    depth: usize,
    kind: String,
    name: String,
    sig: String,
    exp: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ImportRef {
    name: String,
    from: String,
    /// `import { a as b }` / `use x::a as b` 里的原始名 a；非别名导入为 None。
    /// 反向 import 图用它把别名调用点归到真实符号上。
    orig: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FileEntry {
    size: u64,
    modified_ns: u128,
    total: usize,
    syms: Vec<Symbol>,
    imports: Vec<ImportRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DiskCache {
    version: u32,
    root: String,
    files: HashMap<String, FileEntry>,
    /// 反向 import 图随缓存持久化：无文件变更的调用直接复用，不再每次全量重建。
    #[serde(default)]
    reverse: HashMap<String, Vec<ReverseImport>>,
}

#[derive(Debug, Clone)]
struct SearchRow {
    file: String,
    ln: usize,
    text: String,
}

#[derive(Clone, Serialize, Deserialize)]
struct SearchSnapshot {
    version: u32,
    root: String,
    head: String,
    git_signature: String,
    dirty_files: Vec<String>,
    postings: HashMap<String, Vec<String>>,
    /// 索引构建时的源码快照。查询在内存中验证具体行，避免候选阶段再次逐文件 IO。
    contents: HashMap<String, String>,
    file_tokens: HashMap<String, Vec<String>>,
}

#[derive(Serialize, Deserialize)]
struct SearchDelta {
    version: u32,
    root: String,
    base_head: String,
    base_signature: String,
    head: String,
    git_signature: String,
    dirty_files: Vec<String>,
    files: HashMap<String, Option<String>>,
}

#[derive(Clone)]
struct SearchIndex {
    base_head: String,
    base_signature: String,
    head: String,
    git_signature: String,
    dirty_files: Arc<HashSet<String>>,
    postings: Arc<HashMap<String, Vec<String>>>,
    contents: Arc<HashMap<String, String>>,
    file_tokens: Arc<HashMap<String, Vec<String>>>,
    delta_files: Arc<HashMap<String, Option<String>>>,
}

struct SearchGitState {
    head: String,
    signature: String,
    dirty_files: HashSet<String>,
}

#[derive(Debug, Clone)]
struct Definition {
    file: String,
    symbol: Symbol,
}

#[derive(Default)]
struct IndexView {
    files: HashMap<String, FileEntry>,
    defs: HashMap<String, Vec<Definition>>,
    imports: HashMap<String, HashMap<String, String>>,
}

/// 反向 import 图的一条边：importer 文件以本地名 local 引用了目标文件。
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ReverseImport {
    importer: String,
    local: String,
    orig: Option<String>,
}

/// 目标文件 → 引用方清单。由全量增量缓存构建（不限于本次闭包），
/// 使用方按当前文件内容复核，陈旧条目自然失效。
type ReverseMap = HashMap<String, Vec<ReverseImport>>;

#[derive(Clone)]
struct Source {
    lines: Vec<String>,
    syms: Vec<Symbol>,
}

#[derive(Clone)]
struct Block {
    start: usize,
    end: usize,
    label: String,
    tag: &'static str,
    score: f64,
    required: bool,
}

#[derive(Clone)]
struct PlannedFile {
    file: String,
    source: Source,
    section: &'static str,
    full: bool,
    blocks: Vec<Block>,
    rank: usize,
}

/// 打包期因 soft 字节顶被暂缓的块；回填阶段按序补回。
struct Deferred {
    file: String,
    block: Block,
    rank: usize,
}

/// shrink 期被删的内容：整个 FULL 文件或单个块。删除顺序即价值升序，
/// 回填时后删的（相对高价值）优先补回。
enum Dropped {
    Full(PlannedFile),
    Block(String, Block),
}

#[derive(Clone)]
struct UnitCandidate {
    file: String,
    start: usize,
    end: usize,
    label: String,
    tag: &'static str,
    score: f64,
    hits: Vec<usize>,
    keywords: HashSet<String>,
    unit: Option<Symbol>,
    seed_weight: usize,
    role: &'static str,
    required: bool,
    utility: f64,
    body: String,
    estimated_bytes: usize,
    obligation: Option<String>,
}

static MEMO: OnceLock<Mutex<HashMap<String, Arc<DiskCache>>>> = OnceLock::new();
static CONTEXT_DATA_ROOT: OnceLock<PathBuf> = OnceLock::new();
static CACHE_LOCKS: OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> = OnceLock::new();
static CO_CHANGE_CACHE: OnceLock<Mutex<HashMap<String, Vec<(String, usize)>>>> = OnceLock::new();
/// 文件清单按规范化根路径缓存：(清单, 入库时刻, HEAD 指纹)。指纹用 rev-parse HEAD
/// （非 git 仓库回退 .git 目录 mtime），提交/切换分支会使旧清单失效。
struct FileListCacheEntry {
    files: Arc<Vec<String>>,
    at: Instant,
    fingerprint: String,
}
static FILE_LIST_CACHE: OnceLock<Mutex<HashMap<String, FileListCacheEntry>>> = OnceLock::new();
/// 防止同一根路径的过期清单被多个并发查询重复后台刷新。
static FILE_LIST_REFRESHING: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
/// 异步持久化节流：root → 最近一次写盘发起时刻；按 PERSIST_INTERVAL_MS 合并写盘，
/// 间隔内的中间态只保留在内存 MEMO 中，冷启动按 size+mtime 重扫补差。
static PERSIST_LAST: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();
/// 全文倒排索引：符号 index.bin 只覆盖定义/import；这里负责字符串和自然语言候选发现。
static SEARCH_INDEXES: OnceLock<Mutex<HashMap<String, SearchIndex>>> = OnceLock::new();
static SEARCH_INDEX_BUILDING: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
static SEARCH_INDEX_BUILD_GATE: OnceLock<Mutex<()>> = OnceLock::new();
static SEARCH_INDEX_UPDATERS: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
static SEARCH_INDEX_COMPACTED: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();

fn search_deadline() -> Option<Instant> {
    (!cfg!(test)).then(|| Instant::now() + Duration::from_millis(SEARCH_DEADLINE_MS))
}

/// Per-stage wall-clock logging for fast_context. Always on: a slow lookup must be diagnosable
/// from stderr without rerunning under a special env. Output is a single line per stage.
fn trace(label: &str, start: Instant) {
    eprintln!(
        "[nova-tools-profile] {label}: {:.2}ms",
        start.elapsed().as_secs_f64() * 1000.0
    );
}

fn normalize_root(root: &Path) -> String {
    let value = root
        .canonicalize()
        .unwrap_or_else(|_| root.to_path_buf())
        .to_string_lossy()
        .replace('\\', "/")
        .trim_start_matches("//?/")
        .to_string();
    if cfg!(windows) {
        value.to_lowercase()
    } else {
        value
    }
}

fn workspace_cache_key(root: &Path) -> String {
    let normalized = normalize_root(root);
    let key = format!("{:x}", Sha256::digest(normalized.as_bytes()));
    key[..16].to_string()
}

fn cache_location_label(root: &Path) -> String {
    format!(
        "$NOVA_DATA_DIR/codemap/{}/cache.json",
        workspace_cache_key(root)
    )
}

pub fn set_data_root(root: PathBuf) {
    let _ = CONTEXT_DATA_ROOT.set(root);
}

fn cache_path(root: &Path) -> PathBuf {
    let key = workspace_cache_key(root);
    let data = CONTEXT_DATA_ROOT
        .get()
        .cloned()
        .or_else(|| std::env::var_os("NOVA_DATA_DIR").map(PathBuf::from))
        .unwrap_or_else(|| {
            std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".nova")
        });
    data.join("codemap-v3-native").join(key).join("index.bin")
}

fn search_snapshot_path(root: &Path) -> PathBuf {
    cache_path(root).with_file_name("search-snapshot-v4.bin")
}

fn search_delta_path(root: &Path) -> PathBuf {
    cache_path(root).with_file_name("search-delta-v4.wal")
}

fn metadata_stamp(path: &Path) -> Option<(u64, u128)> {
    let meta = fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_INDEX_FILE_BYTES {
        return None;
    }
    let ns = meta
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_nanos();
    Some((meta.len(), ns))
}

/// Load the shared cache for `root` without copying it. Mutation happens on a local copy under
/// the per-root lock and is published back atomically, so readers never deep-copy the large map.
fn load_cache(root: &Path) -> Arc<DiskCache> {
    let key = normalize_root(root);
    let memo = MEMO.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(cache) = memo.lock().unwrap().get(&key).cloned() {
        return cache;
    }
    // A root opened after startup gets one mmap load on first use instead of silently discarding an
    // existing index and rebuilding it from source. Publish it immediately so concurrent callers
    // share the same warm baseline; the per-root update lock serializes their incremental commits.
    let cache = Arc::new(mmap_cache(root, &key).unwrap_or_else(|| DiskCache {
        version: CACHE_VERSION,
        root: key.clone(),
        files: HashMap::new(),
        reverse: HashMap::new(),
    }));
    memo.lock().unwrap().insert(key, cache.clone());
    cache
}

fn mmap_cache(root: &Path, key: &str) -> Option<DiskCache> {
    with_mapped_file(&cache_path(root), |bytes| {
        bincode::deserialize::<DiskCache>(bytes).ok()
    })
    .filter(|cache| cache.version == CACHE_VERSION && cache.root == key)
}

/// Load persisted FastContext indexes once into the shared process at startup. mmap avoids an
/// additional file-sized read buffer; bincode materializes the mutable in-memory cache used by
/// later incremental updates. A workspace first opened after startup uses the same mmap path once.
pub fn preload_indexes(roots: &[String]) -> usize {
    let memo = MEMO.get_or_init(|| Mutex::new(HashMap::new()));
    let mut loaded = 0usize;
    for root in roots {
        let root = Path::new(root);
        if !root.is_dir() {
            continue;
        }
        let key = normalize_root(root);
        if memo.lock().unwrap().contains_key(&key) {
            continue;
        }
        if let Some(cache) = mmap_cache(root, &key) {
            memo.lock().unwrap().insert(key, Arc::new(cache));
            loaded += 1;
        }
        // 符号索引只解决定义/import；同时预载或后台构建全文倒排索引，避免首次
        // Polaris 查询仍退回全仓 rg。构建门保证多个项目不会同时扫盘。
        // 全文索引只认 git 仓库：非 git 目录跳后台构建与常驻轮询。
        if root.join(".git").exists() {
            let _ = search_index_now(root);
        }
    }
    loaded
}

fn store_cache(root: &Path, cache: Arc<DiskCache>, persist: bool) {
    if persist {
        write_cache_async(root, cache.clone());
    }
    MEMO.get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap()
        .insert(cache.root.clone(), cache);
}

/// 异步、按根路径节流与去重的整仓缓存写盘：同一根在写盘进行中只保留一个后台任务，
/// 期间再次请求写盘直接跳过（新版本已由 MEMO 发布，下次节流窗口过后再收敛）；
/// 大仓库下查询线程只付出一次 Arc 克隆，序列化与磁盘替换都在后台进行。
fn write_cache_async(root: &Path, cache: Arc<DiskCache>) {
    let key = cache.root.clone();
    {
        let last = PERSIST_LAST.get_or_init(|| Mutex::new(HashMap::new()));
        let mut guard = last.lock().unwrap();
        if let Some(previous) = guard.get(&key) {
            if previous.elapsed() < Duration::from_millis(PERSIST_INTERVAL_MS) {
                // 距上次写盘不久：更新 MEMO 即可（store_cache 已做），跳过本次写盘。
                return;
            }
        }
        guard.insert(key.clone(), Instant::now());
    }
    let path = cache_path(root);
    thread::spawn(move || {
        write_cache_at(&path, &cache);
    });
}

fn write_cache_at(path: &Path, cache: &DiskCache) {
    if let Ok(bytes) = bincode::serialize(cache) {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let temp = path.with_extension(format!("{}.tmp", std::process::id()));
        if fs::write(&temp, bytes).is_ok() {
            if path.exists() {
                let _ = fs::remove_file(path);
            }
            let _ = fs::rename(&temp, path);
        }
    }
}

fn normalize_rel(value: &str) -> String {
    value
        .replace('\\', "/")
        .trim_start_matches("./")
        .to_string()
}

fn is_code_file(file: &str) -> bool {
    let lower = file.to_ascii_lowercase();
    let ext = Path::new(&lower)
        .extension()
        .and_then(|v| v.to_str())
        .unwrap_or("");
    matches!(
        ext,
        "js" | "jsx"
            | "mjs"
            | "cjs"
            | "ts"
            | "tsx"
            | "mts"
            | "cts"
            | "vue"
            | "svelte"
            | "rs"
            | "py"
            | "pyi"
            | "go"
            | "java"
            | "kt"
            | "kts"
            | "cs"
            | "c"
            | "h"
            | "cc"
            | "cpp"
            | "hpp"
            | "swift"
            | "php"
            | "scala"
            | "dart"
            | "m"
            | "mm"
            | "zig"
    ) && !lower.contains("src-tauri/target")
        && !lower.contains("node_modules")
        && !lower.contains("package-lock")
        && !lower.ends_with(".png")
        && !lower.contains("/dist/")
        && !lower.contains("/coverage/")
        && !lower.ends_with(".min.js")
        && !lower.ends_with(".lock")
        && !lower.contains(".generated.")
}

fn is_searchable_implementation_file(file: &str) -> bool {
    let lower = file.to_ascii_lowercase();
    is_code_file(file) || lower.ends_with(".sql") || lower.ends_with(".md")
}

fn hidden_command(program: &str) -> Command {
    #[allow(unused_mut)]
    let mut command = Command::new(program);
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    command
}

fn run_command(root: &Path, program: &str, args: &[String]) -> Option<Vec<u8>> {
    hidden_command(program)
        .args(args)
        .current_dir(root)
        .output()
        .ok()
        .map(|output| output.stdout)
}

fn run_command_until(
    root: &Path,
    program: &str,
    args: &[String],
    deadline: Instant,
) -> Option<Vec<u8>> {
    run_command_until_limited(root, program, args, Some(deadline), usize::MAX)
}

/// Run an external search with an optional wall-clock deadline and a stdout cap. Closing the
/// stdout pipe once the cap is reached makes producers such as rg/git stop promptly instead of
/// buffering hundreds of megabytes before the caller can apply its result limit. A `None`
/// deadline waits for the process to finish (bounded only by the stdout cap).
fn run_command_until_limited(
    root: &Path,
    program: &str,
    args: &[String],
    deadline: Option<Instant>,
    max_output_bytes: usize,
) -> Option<Vec<u8>> {
    let remaining = deadline.and_then(|limit| limit.checked_duration_since(Instant::now()));
    if deadline.is_some() && remaining.is_none() {
        return None;
    }
    let mut child = hidden_command(program)
        .args(args)
        .current_dir(root)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    // Drain concurrently so a large result cannot fill the OS pipe and block the child. A bounded
    // reader drops the pipe at the limit; most CLI producers then terminate immediately.
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        if max_output_bytes == usize::MAX {
            stdout.read_to_end(&mut bytes).map(|_| bytes).ok()
        } else {
            stdout
                .take(max_output_bytes.saturating_add(1) as u64)
                .read_to_end(&mut bytes)
                .ok()?;
            bytes.truncate(max_output_bytes);
            Some(bytes)
        }
    });
    let completed = match remaining {
        Some(limit) => child.wait_timeout(limit).ok()?.is_some(),
        None => {
            // No deadline: wait for the reader to finish (stdout cap bounds the work) then reap.
            let bytes = reader.join().ok().flatten();
            let _ = child.wait();
            return bytes;
        }
    };
    if !completed {
        let _ = child.kill();
        let _ = child.wait();
    }
    let bytes = reader.join().ok().flatten();
    if completed {
        bytes
    } else {
        None
    }
}

fn walk_code_files(root: &Path) -> Vec<String> {
    let mut files = Vec::new();
    let walker = WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| {
            if entry.depth() == 0 {
                return true;
            }
            if !entry.file_type().is_dir() {
                return true;
            }
            let name = entry.file_name().to_str().unwrap_or("");
            !name.starts_with('.')
                && !matches!(
                    name,
                    "node_modules"
                        | "target"
                        | "dist"
                        | "coverage"
                        | ".git"
                        | ".venv"
                        | "venv"
                        | "__pycache__"
                        | "build"
                        | "out"
                        | "vendor"
                )
        });
    for entry in walker.filter_map(Result::ok).take(MAX_WALK_FILES) {
        if !entry.file_type().is_file() {
            continue;
        }
        if let Ok(rel) = entry.path().strip_prefix(root) {
            let rel = normalize_rel(&rel.to_string_lossy());
            if is_searchable_implementation_file(&rel) {
                files.push(rel);
            }
        }
    }
    files
}

/// 仓库 HEAD 指纹：git 仓库用 rev-parse HEAD（提交/切分支即变），非 git 仓库
/// 回退为根目录元数据 mtime（不可靠但不影响正确性，只是让缓存提前失效）。
fn repo_head_fingerprint(root: &Path) -> String {
    let head = {
        let args = vec!["rev-parse".to_string(), "HEAD".to_string()];
        run_command(root, "git", &args)
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "unknown".into())
    };
    if head != "unknown" {
        return head;
    }
    fs::metadata(root)
        .and_then(|meta| meta.modified())
        .map(|time| {
            let since = time
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0);
            format!("mtime:{since}")
        })
        .unwrap_or_else(|_| "unknown".into())
}

fn list_code_files_uncached(root: &Path) -> Vec<String> {
    const EXTENSIONS: &[&str] = &[
        "rs", "ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs", "go", "java", "kt", "kts",
        "swift", "c", "cc", "cpp", "cxx", "h", "hh", "hpp", "hxx", "cs", "php", "scala", "dart",
        "m", "mm", "zig", "vue", "svelte", "py", "pyi", "sql", "md",
    ];
    let mut args = vec![
        "ls-files".into(),
        "-c".into(),
        "-o".into(),
        "--exclude-standard".into(),
        "-z".into(),
        "--".into(),
    ];
    args.extend(EXTENSIONS.iter().map(|extension| format!("*.{extension}")));
    if let Some(stdout) =
        run_command_until_limited(root, "git", &args, None, MAX_FILE_LIST_OUTPUT_BYTES)
    {
        let mut seen = HashSet::new();
        let files: Vec<_> = stdout
            .split(|byte| *byte == 0)
            .filter_map(|raw| std::str::from_utf8(raw).ok())
            .map(normalize_rel)
            .filter(|file| is_searchable_implementation_file(file) && seen.insert(file.clone()))
            .collect();
        if !files.is_empty() {
            return files;
        }
    }
    walk_code_files(root)
}

fn search_index_tokens(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_' && ch != '-' && ch != '.')
        .filter(|token| token.len() >= SEARCH_INDEX_MIN_TOKEN)
        .map(str::to_ascii_lowercase)
}

fn search_head(root: &Path) -> String {
    git_value(root, &["rev-parse", "HEAD"])
}

fn parse_git_paths(bytes: &[u8], rename_pairs: bool) -> HashSet<String> {
    let mut paths = HashSet::new();
    let fields = bytes.split(|byte| *byte == 0).collect::<Vec<_>>();
    let mut index = 0usize;
    while index < fields.len() {
        let raw = fields[index];
        index += 1;
        if raw.is_empty() {
            continue;
        }
        let text = std::str::from_utf8(raw).unwrap_or("");
        let path = if text.len() >= 3 && text.as_bytes().get(2) == Some(&b' ') {
            normalize_rel(&text[3..])
        } else {
            normalize_rel(text)
        };
        if !path.is_empty() && is_searchable_implementation_file(&path) {
            paths.insert(path);
        }
        if rename_pairs
            && text
                .as_bytes()
                .first()
                .is_some_and(|ch| matches!(ch, b'R' | b'C'))
        {
            if let Some(next) = fields.get(index) {
                index += 1;
                let old = normalize_rel(std::str::from_utf8(next).unwrap_or(""));
                if !old.is_empty() && is_searchable_implementation_file(&old) {
                    paths.insert(old);
                }
            }
        }
    }
    paths
}

fn search_git_state(root: &Path) -> Option<SearchGitState> {
    let head = search_head(root);
    if head == "unknown" {
        return None;
    }
    let args = vec![
        "status".into(),
        "--porcelain=v1".into(),
        "-z".into(),
        "--untracked-files=all".into(),
        "--ignore-submodules=all".into(),
    ];
    let status = run_command_until_limited(
        root,
        "git",
        &args,
        Some(Instant::now() + Duration::from_millis(1_500)),
        MAX_FILE_LIST_OUTPUT_BYTES,
    )?;
    Some(SearchGitState {
        head,
        signature: format!("{:x}", Sha256::digest(&status)),
        dirty_files: parse_git_paths(&status, true),
    })
}

fn search_changed_between(root: &Path, old_head: &str, new_head: &str) -> Option<HashSet<String>> {
    if old_head == new_head {
        return Some(HashSet::new());
    }
    let args = vec![
        "diff".into(),
        "--name-only".into(),
        "-z".into(),
        "--no-renames".into(),
        format!("{old_head}..{new_head}"),
        "--".into(),
    ];
    let bytes = run_command_until_limited(
        root,
        "git",
        &args,
        Some(Instant::now() + Duration::from_millis(2_000)),
        MAX_FILE_LIST_OUTPUT_BYTES,
    )?;
    Some(parse_git_paths(&bytes, false))
}

fn tokens_for_search_text(text: &str) -> Vec<String> {
    let mut tokens = search_index_tokens(text).collect::<Vec<_>>();
    tokens.sort();
    tokens.dedup();
    tokens
}

fn index_from_snapshot(snapshot: SearchSnapshot) -> SearchIndex {
    SearchIndex {
        base_head: snapshot.head.clone(),
        base_signature: snapshot.git_signature.clone(),
        head: snapshot.head,
        git_signature: snapshot.git_signature,
        dirty_files: Arc::new(snapshot.dirty_files.into_iter().collect()),
        postings: Arc::new(snapshot.postings),
        contents: Arc::new(snapshot.contents),
        file_tokens: Arc::new(snapshot.file_tokens),
        delta_files: Arc::new(HashMap::new()),
    }
}

fn build_search_snapshot(root: &Path) -> Option<SearchSnapshot> {
    let git = search_git_state(root)?;
    let files = list_code_files(root);
    if files.is_empty() {
        return None;
    }
    let mut postings = HashMap::<String, Vec<String>>::new();
    let mut contents = HashMap::<String, String>::new();
    let mut file_tokens = HashMap::<String, Vec<String>>::new();
    for file in files.iter() {
        let Ok(text) = fs::read_to_string(root.join(file)) else {
            continue;
        };
        let tokens = tokens_for_search_text(&text);
        for token in &tokens {
            postings
                .entry(token.clone())
                .or_default()
                .push(file.clone());
        }
        file_tokens.insert(file.clone(), tokens);
        contents.insert(file.clone(), text);
    }
    for files in postings.values_mut() {
        files.sort();
        files.dedup();
    }
    Some(SearchSnapshot {
        version: SEARCH_SNAPSHOT_VERSION,
        root: normalize_root(root),
        head: git.head,
        git_signature: git.signature,
        dirty_files: git.dirty_files.into_iter().collect(),
        postings,
        contents,
        file_tokens,
    })
}

fn write_search_snapshot(root: &Path, snapshot: &SearchSnapshot) -> bool {
    let Ok(bytes) = bincode::serialize(snapshot) else {
        return false;
    };
    let path = search_snapshot_path(root);
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let temp = path.with_extension(format!("{}.tmp", std::process::id()));
    if fs::write(&temp, bytes).is_err() {
        return false;
    }
    let _ = fs::remove_file(&path);
    fs::rename(temp, path).is_ok()
}

fn publish_search_snapshot(root: &Path, snapshot: SearchSnapshot) {
    let key = snapshot.root.clone();
    let index = index_from_snapshot(snapshot.clone());
    if write_search_snapshot(root, &snapshot) {
        let _ = fs::remove_file(search_delta_path(root));
    }
    SEARCH_INDEXES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap()
        .insert(key.clone(), index);
    SEARCH_INDEX_COMPACTED
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap()
        .insert(key.clone(), Instant::now());
    SEARCH_INDEX_BUILDING
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .unwrap()
        .remove(&key);
}

fn apply_search_delta(mut index: SearchIndex, delta: &SearchDelta) -> Option<SearchIndex> {
    if delta.version != SEARCH_SNAPSHOT_VERSION
        || delta.base_head != index.base_head
        || delta.base_signature != index.base_signature
    {
        return None;
    }
    let mut postings = (*index.postings).clone();
    let mut contents = (*index.contents).clone();
    let mut file_tokens = (*index.file_tokens).clone();
    for (file, content) in &delta.files {
        if let Some(tokens) = file_tokens.remove(file) {
            for token in tokens {
                if let Some(files) = postings.get_mut(&token) {
                    files.retain(|candidate| candidate != file);
                    if files.is_empty() {
                        postings.remove(&token);
                    }
                }
            }
        }
        contents.remove(file);
        if let Some(content) = content {
            let tokens = tokens_for_search_text(content);
            for token in &tokens {
                let files = postings.entry(token.clone()).or_default();
                if !files.iter().any(|candidate| candidate == file) {
                    files.push(file.clone());
                }
            }
            file_tokens.insert(file.clone(), tokens);
            contents.insert(file.clone(), content.clone());
        }
    }
    index.head = delta.head.clone();
    index.git_signature = delta.git_signature.clone();
    index.dirty_files = Arc::new(delta.dirty_files.iter().cloned().collect());
    index.postings = Arc::new(postings);
    index.contents = Arc::new(contents);
    index.file_tokens = Arc::new(file_tokens);
    index.delta_files = Arc::new(delta.files.clone());
    Some(index)
}

fn load_search_snapshot(root: &Path) -> Option<SearchIndex> {
    // 全文索引只认 git 仓库；非 git 目录忽略已有快照文件。
    if !root.join(".git").exists() {
        return None;
    }
    let snapshot = with_mapped_file(&search_snapshot_path(root), |bytes| {
        bincode::deserialize::<SearchSnapshot>(bytes).ok()
    })?;
    if snapshot.version != SEARCH_SNAPSHOT_VERSION || snapshot.root != normalize_root(root) {
        return None;
    }
    let mut index = index_from_snapshot(snapshot);
    if let Some(delta) = with_mapped_file(&search_delta_path(root), |bytes| {
        bincode::deserialize::<SearchDelta>(bytes).ok()
    }) {
        if let Some(updated) = apply_search_delta(index.clone(), &delta) {
            index = updated;
        }
    }
    Some(index)
}

fn write_search_delta(root: &Path, delta: &SearchDelta) {
    let Ok(bytes) = bincode::serialize(delta) else {
        return;
    };
    let path = search_delta_path(root);
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let temp = path.with_extension(format!("{}.tmp", std::process::id()));
    if fs::write(&temp, bytes).is_ok() {
        let _ = fs::remove_file(&path);
        let _ = fs::rename(temp, path);
    }
}

fn compact_search_index(root: &Path, key: &str, index: &SearchIndex) {
    let snapshot = SearchSnapshot {
        version: SEARCH_SNAPSHOT_VERSION,
        root: key.to_string(),
        head: index.head.clone(),
        git_signature: index.git_signature.clone(),
        dirty_files: index.dirty_files.iter().cloned().collect(),
        postings: (*index.postings).clone(),
        contents: (*index.contents).clone(),
        file_tokens: (*index.file_tokens).clone(),
    };
    if write_search_snapshot(root, &snapshot) {
        let _ = fs::remove_file(search_delta_path(root));
        if let Some(current) = SEARCH_INDEXES
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap()
            .get_mut(key)
        {
            current.base_head = current.head.clone();
            current.base_signature = current.git_signature.clone();
            current.delta_files = Arc::new(HashMap::new());
        }
        SEARCH_INDEX_COMPACTED
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap()
            .insert(key.to_string(), Instant::now());
    }
}

fn refresh_search_index(root: &Path, key: &str, current: SearchIndex) -> Option<SearchIndex> {
    let git = search_git_state(root)?;
    if current.head == git.head && current.git_signature == git.signature {
        return Some(current);
    }
    let mut changed = search_changed_between(root, &current.head, &git.head)?;
    changed.extend(current.dirty_files.iter().cloned());
    changed.extend(git.dirty_files.iter().cloned());
    if changed.len() > SEARCH_MAX_INCREMENTAL_FILES {
        return None;
    }
    let mut files = current.delta_files.as_ref().clone();
    for file in changed {
        files.insert(file.clone(), fs::read_to_string(root.join(&file)).ok());
    }
    let delta = SearchDelta {
        version: SEARCH_SNAPSHOT_VERSION,
        root: key.to_string(),
        base_head: current.base_head.clone(),
        base_signature: current.base_signature.clone(),
        head: git.head,
        git_signature: git.signature,
        dirty_files: git.dirty_files.into_iter().collect(),
        files,
    };
    let updated = apply_search_delta(current, &delta)?;
    write_search_delta(root, &delta);
    Some(updated)
}

fn search_compaction_due(key: &str, index: &SearchIndex) -> bool {
    if index.delta_files.len() >= SEARCH_COMPACT_CHANGED_FILES {
        return true;
    }
    SEARCH_INDEX_COMPACTED
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap()
        .get(key)
        .is_some_and(|at| {
            !index.delta_files.is_empty()
                && at.elapsed() >= Duration::from_millis(SEARCH_COMPACT_INTERVAL_MS)
        })
}

fn search_updater_loop(root: PathBuf, key: String) {
    loop {
        thread::sleep(Duration::from_millis(SEARCH_GIT_POLL_MS));
        let current = SEARCH_INDEXES
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap()
            .get(&key)
            .cloned();
        let Some(current) = current else {
            continue;
        };
        if let Some(updated) = refresh_search_index(&root, &key, current) {
            SEARCH_INDEXES
                .get_or_init(|| Mutex::new(HashMap::new()))
                .lock()
                .unwrap()
                .insert(key.clone(), updated.clone());
            if search_compaction_due(&key, &updated) {
                compact_search_index(&root, &key, &updated);
            }
        } else {
            let _gate = SEARCH_INDEX_BUILD_GATE
                .get_or_init(|| Mutex::new(()))
                .lock()
                .unwrap();
            if let Some(snapshot) = build_search_snapshot(&root) {
                publish_search_snapshot(&root, snapshot);
            }
        }
    }
}

fn ensure_search_updater(root: &Path, key: &str) {
    let updaters = SEARCH_INDEX_UPDATERS.get_or_init(|| Mutex::new(HashSet::new()));
    if !updaters.lock().unwrap().insert(key.to_string()) {
        return;
    }
    let root = root.to_path_buf();
    let key = key.to_string();
    let _ = thread::Builder::new()
        .name("nova-context-git-index".into())
        .spawn(move || search_updater_loop(root, key));
}

fn spawn_search_index_build(root: &Path, key: String) {
    let building = SEARCH_INDEX_BUILDING.get_or_init(|| Mutex::new(HashSet::new()));
    if !building.lock().unwrap().insert(key.clone()) {
        return;
    }
    let root = root.to_path_buf();
    let _ = thread::Builder::new()
        .name("nova-context-text-index".into())
        .spawn(move || {
            let _gate = SEARCH_INDEX_BUILD_GATE
                .get_or_init(|| Mutex::new(()))
                .lock()
                .unwrap();
            if let Some(snapshot) = build_search_snapshot(&root) {
                publish_search_snapshot(&root, snapshot);
                ensure_search_updater(&root, &key);
            } else {
                SEARCH_INDEX_BUILDING
                    .get_or_init(|| Mutex::new(HashSet::new()))
                    .lock()
                    .unwrap()
                    .remove(&key);
            }
        });
}

fn search_index_now(root: &Path) -> Option<SearchIndex> {
    // 全文索引只认 git 仓库；非 git 目录不构建、不加载、不起轮询。
    if !root.join(".git").exists() {
        return None;
    }
    let key = normalize_root(root);
    let indexes = SEARCH_INDEXES.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(index) = indexes.lock().unwrap().get(&key).cloned() {
        ensure_search_updater(root, &key);
        return Some(index);
    }
    if cfg!(test) {
        return None;
    }
    if let Some(index) = load_search_snapshot(root) {
        indexes.lock().unwrap().insert(key.clone(), index.clone());
        SEARCH_INDEX_COMPACTED
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap()
            .insert(key.clone(), Instant::now());
        ensure_search_updater(root, &key);
        return Some(index);
    }
    spawn_search_index_build(root, key);
    None
}

fn search_index_rows(
    root: &Path,
    terms: &[String],
    ignore_case: bool,
    word: bool,
    dirs: Option<&[String]>,
    deadline: Option<Instant>,
) -> Option<Vec<SearchRow>> {
    let index = search_index_now(root)?;
    let candidates = indexed_candidate_files_from(&index, terms)?;
    let needles = terms
        .iter()
        .map(|term| {
            if ignore_case {
                term.to_lowercase()
            } else {
                term.clone()
            }
        })
        .collect::<Vec<_>>();
    let word_res = if word {
        terms
            .iter()
            .filter_map(|term| {
                Regex::new(&format!(
                    r"(?{}:\b{}\b)",
                    if ignore_case { "i" } else { "" },
                    regex::escape(term)
                ))
                .ok()
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let mut rows = Vec::new();
    'files: for file in candidates {
        if dirs
            .is_some_and(|dirs| !dirs.is_empty() && !dirs.iter().any(|dir| file.starts_with(dir)))
        {
            continue;
        }
        if deadline.is_some_and(|limit| Instant::now() >= limit) {
            break;
        }
        let Some(text) = index.contents.get(&file) else {
            return None;
        };
        let mut file_hits = 0usize;
        for (line_index, line) in text.split('\n').enumerate() {
            let matched = if word {
                word_res.iter().any(|regex| regex.is_match(line))
            } else if ignore_case {
                let hay = line.to_lowercase();
                needles.iter().any(|needle| hay.contains(needle))
            } else {
                needles.iter().any(|needle| line.contains(needle))
            };
            if matched {
                rows.push(SearchRow {
                    file: file.clone(),
                    ln: line_index + 1,
                    text: line.trim_end_matches('\r').to_string(),
                });
                file_hits += 1;
                if file_hits >= MAX_HITS_PER_FILE || rows.len() >= MAX_HIT_LINES {
                    if rows.len() >= MAX_HIT_LINES {
                        break 'files;
                    }
                    break;
                }
            }
        }
    }
    rows.sort_by(|a, b| a.file.cmp(&b.file).then(a.ln.cmp(&b.ln)));
    Some(rows)
}

fn indexed_candidate_files_from(index: &SearchIndex, terms: &[String]) -> Option<Vec<String>> {
    let mut scores = HashMap::<String, usize>::new();
    let mut wide_candidates = HashSet::<String>::new();
    let mut usable = false;
    for term in terms {
        let tokens = search_index_tokens(term).collect::<Vec<_>>();
        if tokens.is_empty() {
            continue;
        }
        let mut per_term: Option<HashSet<String>> = None;
        for token in tokens {
            let Some(files) = index.postings.get(&token) else {
                per_term = Some(HashSet::new());
                break;
            };
            let set = files.iter().cloned().collect::<HashSet<_>>();
            per_term = Some(match per_term {
                Some(current) => current.intersection(&set).cloned().collect(),
                None => set,
            });
        }
        let per_term = per_term.unwrap_or_default();
        // SQL/build/config 等超高频词不具备候选收敛能力。查询同时有 roblox 这类
        // 稀有锚点时跳过泛词，避免一个泛词把候选重新膨胀到全仓。
        if per_term.len() > SEARCH_INDEX_MAX_CANDIDATES {
            wide_candidates.extend(per_term);
            continue;
        }
        usable = true;
        for file in per_term {
            *scores.entry(file).or_default() += 1;
        }
    }
    if !usable && !wide_candidates.is_empty() {
        for file in wide_candidates {
            scores.insert(file, 1);
        }
        usable = true;
    }
    if !usable || scores.is_empty() {
        return None;
    }
    let mut candidates = scores.into_iter().collect::<Vec<_>>();
    candidates.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    candidates.truncate(SEARCH_INDEX_MAX_CANDIDATES);
    Some(candidates.into_iter().map(|(file, _)| file).collect())
}

#[cfg(test)]
fn indexed_candidate_files(root: &Path, terms: &[String]) -> Option<Vec<String>> {
    let index = search_index_now(root)?;
    indexed_candidate_files_from(&index, terms)
}

/// 带缓存的文件清单：TTL + HEAD 指纹双闸。TTL 内直接复用；过期但 HEAD 未变时先返回
/// 旧清单并后台刷新，查询路径不再每次为列全仓文件付出百毫秒到秒级的固定开销。
/// HEAD 变化（提交/切分支）时同步重列，保证索引与检出内容一致。
fn list_code_files(root: &Path) -> Arc<Vec<String>> {
    let key = normalize_root(root);
    let cache = FILE_LIST_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let entry = cache
        .lock()
        .unwrap()
        .get(&key)
        .map(|entry| FileListCacheEntry {
            files: entry.files.clone(),
            at: entry.at,
            fingerprint: entry.fingerprint.clone(),
        });
    if let Some(entry) = entry {
        if entry.at.elapsed() < Duration::from_millis(FILE_LIST_TTL_MS) {
            return entry.files;
        }
        let current = repo_head_fingerprint(root);
        if current == entry.fingerprint {
            // 内容未变但 TTL 已过：后台刷新以发现未提交改动，本次先用旧清单。
            let refreshing = FILE_LIST_REFRESHING.get_or_init(|| Mutex::new(HashSet::new()));
            if refreshing.lock().unwrap().insert(key.clone()) {
                let root = root.to_path_buf();
                let cache_key = key.clone();
                thread::spawn(move || {
                    let files = Arc::new(list_code_files_uncached(&root));
                    let fingerprint = repo_head_fingerprint(&root);
                    FILE_LIST_CACHE
                        .get_or_init(|| Mutex::new(HashMap::new()))
                        .lock()
                        .unwrap()
                        .insert(
                            cache_key.clone(),
                            FileListCacheEntry {
                                files,
                                at: Instant::now(),
                                fingerprint,
                            },
                        );
                    FILE_LIST_REFRESHING
                        .get_or_init(|| Mutex::new(HashSet::new()))
                        .lock()
                        .unwrap()
                        .remove(&cache_key);
                });
            }
            return entry.files;
        }
    }
    let files = Arc::new(list_code_files_uncached(root));
    let fingerprint = repo_head_fingerprint(root);
    cache.lock().unwrap().insert(
        key,
        FileListCacheEntry {
            files: files.clone(),
            at: Instant::now(),
            fingerprint,
        },
    );
    files
}

/// mmap 缓存文件上限：超限视为无缓存，避免反序列化一次性吃掉过多内存。
const MAX_MAPPED_CACHE_BYTES: usize = 512 * 1024 * 1024;

#[cfg(windows)]
fn with_mapped_file<T>(path: &Path, consume: impl FnOnce(&[u8]) -> Option<T>) -> Option<T> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Memory::{
        CreateFileMappingW, MapViewOfFile, UnmapViewOfFile, FILE_MAP_READ, PAGE_READONLY,
    };
    let file = fs::File::open(path).ok()?;
    let size = file.metadata().ok()?.len() as usize;
    if size == 0 || size > MAX_MAPPED_CACHE_BYTES {
        return None;
    }
    unsafe {
        let mapping = CreateFileMappingW(
            file.as_raw_handle() as _,
            std::ptr::null(),
            PAGE_READONLY,
            0,
            0,
            std::ptr::null(),
        );
        if mapping.is_null() {
            return None;
        }
        let view = MapViewOfFile(mapping, FILE_MAP_READ, 0, 0, size);
        if view.Value.is_null() {
            CloseHandle(mapping);
            return None;
        }
        let bytes = std::slice::from_raw_parts(view.Value.cast::<u8>(), size);
        let result = consume(bytes);
        UnmapViewOfFile(view);
        CloseHandle(mapping);
        result
    }
}

#[cfg(unix)]
fn with_mapped_file<T>(path: &Path, consume: impl FnOnce(&[u8]) -> Option<T>) -> Option<T> {
    let file = fs::File::open(path).ok()?;
    let size = file.metadata().ok()?.len() as usize;
    if size == 0 || size > MAX_MAPPED_CACHE_BYTES {
        return None;
    }
    unsafe {
        let view = libc::mmap(
            std::ptr::null_mut(),
            size,
            libc::PROT_READ,
            libc::MAP_PRIVATE,
            file.as_raw_fd(),
            0,
        );
        if view == libc::MAP_FAILED {
            return None;
        }
        let bytes = std::slice::from_raw_parts(view.cast::<u8>(), size);
        let result = consume(bytes);
        libc::munmap(view, size);
        result
    }
}

fn git_value(root: &Path, args: &[&str]) -> String {
    let args = args
        .iter()
        .map(|value| (*value).to_string())
        .collect::<Vec<_>>();
    run_command(root, "git", &args)
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "unknown".into())
}

fn short_rev(root: &Path) -> String {
    git_value(root, &["rev-parse", "--short", "HEAD"])
}

fn git_head(root: &Path) -> (String, String) {
    (
        git_value(root, &["rev-parse", "--abbrev-ref", "HEAD"]),
        short_rev(root),
    )
}

fn excluded_search_path(file: &str) -> bool {
    let lower = file.to_ascii_lowercase();
    lower.contains("src-tauri/target")
        || lower.contains("node_modules")
        || lower.contains("package-lock")
        || lower.ends_with(".png")
        || lower.contains("/dist/")
        || lower.starts_with("dist/")
        || lower.contains("/coverage/")
        || lower.starts_with("coverage/")
        || lower.ends_with(".min.js")
        || lower.ends_with(".lock")
        || lower.contains(".generated.")
}

fn parse_search_rows(bytes: Vec<u8>) -> Vec<SearchRow> {
    let Ok(text) = String::from_utf8(bytes) else {
        return Vec::new();
    };
    let mut rows = text
        .lines()
        .filter_map(|line| {
            let first = line.find(':')?;
            let second = line[first + 1..].find(':')? + first + 1;
            let file = normalize_rel(&line[..first]);
            let ln = line[first + 1..second].parse().ok()?;
            (!file.is_empty() && !excluded_search_path(&file)).then(|| SearchRow {
                file,
                ln,
                text: line[second + 1..].to_string(),
            })
        })
        .collect::<Vec<_>>();
    rows.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then(a.ln.cmp(&b.ln))
            .then(a.text.cmp(&b.text))
    });
    rows.truncate(MAX_HIT_LINES);
    rows
}

fn search_in_process(
    root: &Path,
    terms: &[String],
    ignore_case: bool,
    word: bool,
    scoped_files: &[String],
    deadline: Option<Instant>,
) -> Vec<SearchRow> {
    let files = if scoped_files.is_empty() {
        (*list_code_files(root)).clone()
    } else {
        scoped_files.to_vec()
    };
    let needles = terms
        .iter()
        .map(|term| {
            if ignore_case {
                term.to_lowercase()
            } else {
                term.clone()
            }
        })
        .collect::<Vec<_>>();
    let word_res = if word {
        terms
            .iter()
            .filter_map(|term| {
                Regex::new(&format!(
                    r"(?{}:\b{}\b)",
                    if ignore_case { "i" } else { "" },
                    regex::escape(term)
                ))
                .ok()
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let mut parts = Vec::with_capacity(files.len());
    'files: for file in files {
        if deadline.is_some_and(|limit| Instant::now() >= limit) {
            break;
        }
        let Ok(text) = fs::read_to_string(root.join(&file)) else {
            parts.push(Vec::new());
            continue;
        };
        let mut rows = Vec::new();
        for (index, line) in text.split('\n').enumerate() {
            if deadline.is_some_and(|limit| Instant::now() >= limit) {
                parts.push(rows);
                break 'files;
            }
            let hay = if ignore_case {
                line.to_lowercase()
            } else {
                line.to_string()
            };
            let matched = if word {
                word_res.iter().any(|regex| regex.is_match(line))
            } else {
                needles.iter().any(|needle| hay.contains(needle))
            };
            if matched {
                rows.push(SearchRow {
                    file: file.clone(),
                    ln: index + 1,
                    text: line.to_string(),
                });
                if rows.len() >= MAX_HITS_PER_FILE {
                    break;
                }
            }
        }
        parts.push(rows);
    }
    let mut rows = parts.into_iter().flatten().collect::<Vec<_>>();
    rows.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then(a.ln.cmp(&b.ln))
            .then(a.text.cmp(&b.text))
    });
    rows.truncate(MAX_HIT_LINES);
    rows
}

fn rg_available(root: &Path) -> bool {
    static RG_AVAILABLE: OnceLock<bool> = OnceLock::new();
    *RG_AVAILABLE.get_or_init(|| run_command(root, "rg", &["--version".into()]).is_some())
}

/// 单次 rg 检索。paths 为空时搜整个仓库；code_glob 为 true 时用扩展名 glob
/// 限定代码文件。rg 不可用或执行失败返回 None。
fn rg_search(
    root: &Path,
    terms: &[String],
    ignore_case: bool,
    word: bool,
    paths: &[String],
    code_glob: bool,
    deadline: Option<Instant>,
) -> Option<Vec<SearchRow>> {
    let mut args = vec![
        "-n".into(),
        "--with-filename".into(),
        "--no-heading".into(),
        "--color".into(),
        "never".into(),
        "-F".into(),
        "--max-count".into(),
        MAX_HITS_PER_FILE.to_string(),
        "--max-filesize".into(),
        "2M".into(),
        "--max-columns".into(),
        "1000".into(),
    ];
    if ignore_case {
        args.push("-i".into());
    }
    if word {
        args.push("-w".into());
    }
    for term in terms {
        args.push("-e".into());
        args.push(term.clone());
    }
    for glob in [
        "!**/node_modules/**",
        "!**/dist/**",
        "!**/target/**",
        "!**/coverage/**",
        "!**/package-lock.json",
        "!*.png",
        "!*.jpg",
        "!*.jpeg",
        "!*.gif",
        "!*.webp",
        "!*.ico",
        "!*.woff",
        "!*.woff2",
        "!*.ttf",
        "!*.bin",
    ] {
        args.push("--glob".into());
        args.push(glob.into());
    }
    if code_glob {
        // Positive --glob patterns whitelist matching files and therefore override .gitignore.
        // A custom rg type limits extensions while preserving normal ignore semantics.
        for extension in CODE_FILE_EXTENSIONS {
            args.push("--type-add".into());
            args.push(format!("novacode:*.{extension}"));
        }
        args.push("--type".into());
        args.push("novacode".into());
    }
    if paths.is_empty() {
        args.push(".".into());
    } else {
        args.extend(paths.iter().cloned());
    }
    let stdout = run_command_until_limited(root, "rg", &args, deadline, MAX_SEARCH_OUTPUT_BYTES)?;
    Some(parse_search_rows(stdout))
}

fn search_text_until(
    root: &Path,
    terms: &[String],
    ignore_case: bool,
    word: bool,
    files: &[String],
    deadline: Option<Instant>,
) -> Vec<SearchRow> {
    if files.is_empty() {
        if let Some(rows) = search_index_rows(root, terms, ignore_case, word, None, deadline) {
            return rows;
        }
    }
    let mut seen = HashSet::new();
    let terms = terms
        .iter()
        .filter(|term| {
            let key = if ignore_case {
                term.to_lowercase()
            } else {
                (*term).clone()
            };
            !term.is_empty() && seen.insert(key)
        })
        .cloned()
        .collect::<Vec<_>>();
    if terms.is_empty() {
        return Vec::new();
    }
    if files.len() > 128 {
        let mut rows = files
            .chunks(128)
            .flat_map(|chunk| search_text_until(root, &terms, ignore_case, word, chunk, deadline))
            .collect::<Vec<_>>();
        rows.sort_by(|a, b| {
            a.file
                .cmp(&b.file)
                .then(a.ln.cmp(&b.ln))
                .then(a.text.cmp(&b.text))
        });
        rows.truncate(MAX_HIT_LINES);
        return rows;
    }
    if rg_available(root) {
        // rg is the bounded primary path. A timeout/output-cap failure must not fall through to an
        // unbounded `git grep` over the same large repository.
        return rg_search(root, &terms, ignore_case, word, files, false, deadline)
            .unwrap_or_default();
    }
    let inside = git_value(root, &["rev-parse", "--is-inside-work-tree"]);
    if inside == "true" {
        let mut args = vec!["grep".into(), "-nI".into(), "--untracked".into()];
        if ignore_case {
            args.push("-i".into());
        }
        if word {
            args.push("-w".into());
        }
        args.push("-F".into());
        for term in &terms {
            args.push("-e".into());
            args.push(term.clone());
        }
        args.push("--".into());
        args.extend(files.iter().cloned());
        if let Some(stdout) = run_command(root, "git", &args) {
            return parse_search_rows(stdout);
        }
    }
    search_in_process(root, &terms, ignore_case, word, files, deadline)
}

#[derive(Clone, Copy, PartialEq)]
enum LexState {
    Code,
    Double,
    Template,
    BlockComment,
    Raw(usize),
}

fn regex_literal_allowed(output: &str) -> bool {
    let trimmed = output.trim_end();
    if trimmed.is_empty() {
        return true;
    }
    let last = trimmed.chars().last().unwrap_or('\0');
    if matches!(
        last,
        '(' | ','
            | '='
            | ':'
            | '['
            | '!'
            | '&'
            | '|'
            | '?'
            | '{'
            | ';'
            | '+'
            | '*'
            | '%'
            | '~'
            | '^'
    ) {
        return true;
    }
    static KEYWORD: OnceLock<Regex> = OnceLock::new();
    KEYWORD
        .get_or_init(|| {
            Regex::new(r"\b(?:return|case|typeof|instanceof|in|of|do|else|yield|await|new)$")
                .unwrap()
        })
        .is_match(trimmed)
}

fn stripped_depth(lines: &[String], rust: bool) -> (Vec<String>, Vec<usize>, Vec<usize>) {
    let mut state = LexState::Code;
    let mut depth = 0usize;
    let mut template_stack = Vec::<usize>::new();
    let mut starts = Vec::with_capacity(lines.len());
    let mut after = Vec::with_capacity(lines.len());
    let mut code = Vec::with_capacity(lines.len());
    for line in lines {
        starts.push(depth);
        let chars: Vec<char> = line.chars().collect();
        let mut out = String::new();
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            let next = chars.get(i + 1).copied().unwrap_or('\0');
            match state {
                LexState::BlockComment => {
                    if c == '*' && next == '/' {
                        state = LexState::Code;
                        i += 2;
                    } else {
                        i += 1;
                    }
                    continue;
                }
                LexState::Double => {
                    if c == '\\' {
                        i += 2;
                    } else if c == '"' {
                        state = LexState::Code;
                        i += 1;
                    } else {
                        i += 1;
                    }
                    continue;
                }
                LexState::Template => {
                    if c == '\\' {
                        i += 2;
                    } else if c == '`' {
                        state = LexState::Code;
                        i += 1;
                    } else if c == '$' && next == '{' {
                        template_stack.push(depth);
                        depth += 1;
                        state = LexState::Code;
                        i += 2;
                    } else {
                        i += 1;
                    }
                    continue;
                }
                LexState::Raw(hashes) => {
                    if c == '"' && (0..hashes).all(|n| chars.get(i + 1 + n) == Some(&'#')) {
                        state = LexState::Code;
                        i += 1 + hashes;
                    } else {
                        i += 1;
                    }
                    continue;
                }
                LexState::Code => {}
            }
            if c == '/' && next == '/' {
                break;
            }
            if c == '/' && next == '*' {
                state = LexState::BlockComment;
                i += 2;
                continue;
            }
            if c == '/' && regex_literal_allowed(&out) {
                let mut cursor = i + 1;
                let mut class = false;
                let mut closed = false;
                while cursor < chars.len() {
                    let value = chars[cursor];
                    if value == '\\' {
                        cursor += 2;
                        continue;
                    }
                    if class {
                        if value == ']' {
                            class = false;
                        }
                    } else if value == '[' {
                        class = true;
                    } else if value == '/' {
                        closed = true;
                        break;
                    }
                    cursor += 1;
                }
                if closed {
                    while cursor + 1 < chars.len() && chars[cursor + 1].is_ascii_lowercase() {
                        cursor += 1;
                    }
                    i = cursor + 1;
                    continue;
                }
            }
            if c == 'r'
                && !chars
                    .get(i.wrapping_sub(1))
                    .is_some_and(|previous| previous.is_ascii_alphanumeric() || *previous == '_')
            {
                let mut hashes = 0;
                while chars.get(i + 1 + hashes) == Some(&'#') {
                    hashes += 1;
                }
                if chars.get(i + 1 + hashes) == Some(&'"') {
                    state = LexState::Raw(hashes);
                    i += 2 + hashes;
                    continue;
                }
            }
            if c == '"' {
                state = LexState::Double;
                i += 1;
                continue;
            }
            if c == '`' {
                state = LexState::Template;
                i += 1;
                continue;
            }
            if c == '\'' {
                let char_literal_end = if chars.get(i + 1) == Some(&'\\') {
                    i + 3
                } else {
                    i + 2
                };
                let char_literal = chars.get(char_literal_end) == Some(&'\'')
                    && chars.get(i + 1).is_some_and(|next| *next != '\'');
                if char_literal {
                    i = char_literal_end + 1;
                    continue;
                }
                out.push(c);
                i += 1;
                continue;
            }
            if c == '{' {
                depth += 1;
                out.push(c);
            } else if c == '}' {
                depth = depth.saturating_sub(1);
                if template_stack.last().is_some_and(|saved| *saved == depth) {
                    template_stack.pop();
                    state = LexState::Template;
                } else {
                    out.push(c);
                }
            } else {
                out.push(c);
            }
            i += 1;
        }
        if matches!(state, LexState::Double) {
            let slash_count = line
                .chars()
                .rev()
                .take_while(|value| *value == '\\')
                .count();
            let continued = rust || slash_count % 2 == 1;
            if !continued {
                state = LexState::Code;
            }
        }
        code.push(out);
        after.push(depth);
    }
    (code, starts, after)
}

fn signature(line: &str) -> String {
    let compact = line.split_whitespace().collect::<Vec<_>>().join(" ");
    js_utf16_slice(&compact, 120)
}

fn declaration(text: &str, depth: usize) -> Option<(String, String)> {
    static DECLS: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    let decls = DECLS.get_or_init(|| vec![
        (Regex::new(r#"^(?:pub(?:\([^)]*\))?\s+)?(?:default\s+)?(?:const\s+)?(?:async\s+)?(?:unsafe\s+)?(?:extern\s+(?:"[^"]*"\s+)?)?fn\s+([A-Za-z_]\w*)"#).unwrap(), "fn"),
        (Regex::new(r"^(?:pub(?:\([^)]*\))?\s+)?(?:struct|enum|trait|union)\s+([A-Za-z_]\w*)").unwrap(), "type"),
        (Regex::new(r"^(?:pub(?:\([^)]*\))?\s+)?mod\s+([A-Za-z_]\w*)").unwrap(), "mod"),
        (Regex::new(r"^impl(?:\s*<[^>]*>)?\s+(?:[\w:]+\s+for\s+)?([A-Za-z_][\w:]*)").unwrap(), "impl"),
        (Regex::new(r"^macro_rules!\s+([A-Za-z_]\w*)").unwrap(), "macro"),
        (Regex::new(r"^(?:export\s+)?(?:default\s+)?(?:declare\s+)?(?:abstract\s+)?class\s+([A-Za-z_$][\w$]*)").unwrap(), "class"),
        (Regex::new(r"^(?:export\s+)?(?:default\s+)?(?:declare\s+)?(?:async\s+)?function\s*\*?\s*([A-Za-z_$][\w$]*)").unwrap(), "fn"),
        (Regex::new(r"^(?:export\s+)?(?:declare\s+)?(?:interface|namespace)\s+([A-Za-z_$][\w$]*)").unwrap(), "type"),
        (Regex::new(r"^(?:export\s+)?(?:declare\s+)?(?:const\s+)?enum\s+([A-Za-z_$][\w$]*)").unwrap(), "type"),
        (Regex::new(r"^(?:export\s+)?(?:declare\s+)?type\s+([A-Za-z_$][\w$]*)").unwrap(), "type"),
        (Regex::new(r"^(?:export\s+)?(?:pub(?:\([^)]*\))?\s+)?(?:declare\s+)?(?:const|let|var|static)\s+([A-Za-z_$][\w$]*)").unwrap(), "const"),
    ]);
    for (re, kind) in decls {
        if let Some(c) = re.captures(text) {
            return Some((c[1].to_string(), (*kind).into()));
        }
    }
    if depth > 0 {
        static METHOD: OnceLock<Regex> = OnceLock::new();
        let re = METHOD.get_or_init(|| Regex::new(r"^(?:(?:public|private|protected|readonly|static|async|get|set|override|abstract)\s+)*\*?\s*([A-Za-z_$][\w$]*)\s*(?:<[^>]*>)?\s*\(").unwrap());
        let control = |name: &str| {
            matches!(
                name,
                "if" | "else"
                    | "for"
                    | "while"
                    | "switch"
                    | "case"
                    | "catch"
                    | "try"
                    | "do"
                    | "return"
                    | "match"
                    | "loop"
                    | "function"
                    | "new"
                    | "typeof"
                    | "await"
                    | "yield"
                    | "throw"
                    | "with"
                    | "in"
                    | "of"
                    | "as"
                    | "is"
                    | "let"
                    | "const"
                    | "var"
                    | "import"
                    | "export"
                    | "require"
                    | "super"
                    | "this"
                    | "self"
                    | "and"
                    | "or"
                    | "not"
            )
        };
        if let Some(captures) = re.captures(text) {
            if !control(&captures[1]) {
                return Some((captures[1].to_string(), "method".into()));
            }
        }
        static PROP_BLOCK: OnceLock<Regex> = OnceLock::new();
        let prop = PROP_BLOCK.get_or_init(|| {
            Regex::new(r"^([A-Za-z_$][\w$]*)\s*:\s*(?:async\s*)?(?:function\b|\(|\{|$)").unwrap()
        });
        if let Some(captures) = prop.captures(text) {
            if !control(&captures[1]) {
                return Some((captures[1].to_string(), "prop".into()));
            }
        }
    }
    None
}

fn extract_imports(text: &str, file: &str) -> Vec<ImportRef> {
    let mut out = Vec::new();
    if file.ends_with(".py") || file.ends_with(".pyi") {
        return out;
    }
    if file.ends_with(".rs") {
        static RUST_USE: OnceLock<Regex> = OnceLock::new();
        let re = RUST_USE.get_or_init(|| Regex::new(r"(?m)^\s*(?:pub\s+)?use\s+([^;]+);").unwrap());
        for cap in re.captures_iter(text) {
            let raw = cap[1].trim();
            if let Some(open) = raw.rfind("::{") {
                let base = &raw[..open];
                let body = raw[open + 3..].trim_end_matches('}');
                for part in body.split(',').map(str::trim).filter(|v| !v.is_empty()) {
                    let pieces: Vec<_> = part.split_whitespace().collect();
                    let (orig, local) = if pieces.len() == 3 && pieces[1] == "as" {
                        (pieces[0], pieces[2])
                    } else {
                        (part, part)
                    };
                    if matches!(orig, "self" | "crate" | "super") {
                        continue;
                    }
                    out.push(ImportRef {
                        name: local.into(),
                        from: format!("{base}::{orig}"),
                        orig: (orig != local).then(|| orig.to_string()),
                    });
                }
            } else if let Some(pos) = raw.rfind("::") {
                let part = &raw[pos + 2..];
                let pieces: Vec<_> = part.split_whitespace().collect();
                let (orig, local) = if pieces.len() == 3 && pieces[1] == "as" {
                    (pieces[0], pieces[2])
                } else {
                    (part, part)
                };
                if matches!(orig, "self" | "crate" | "super") {
                    continue;
                }
                out.push(ImportRef {
                    name: local.into(),
                    from: format!("{}::{orig}", &raw[..pos]),
                    orig: (orig != local).then(|| orig.to_string()),
                });
            }
        }
        return out;
    }
    static JS_IMPORT: OnceLock<Regex> = OnceLock::new();
    static JS_NAMESPACE: OnceLock<Regex> = OnceLock::new();
    static JS_NAMED: OnceLock<Regex> = OnceLock::new();
    static JS_IDENT: OnceLock<Regex> = OnceLock::new();
    static JS_REEXPORT: OnceLock<Regex> = OnceLock::new();
    let re = JS_IMPORT.get_or_init(|| {
        Regex::new(r#"(?ms)^\s*import\s+(?:type\s+)?(.+?)\s+from\s*['\"]([^'\"]+)['\"]"#).unwrap()
    });
    let namespace_re =
        JS_NAMESPACE.get_or_init(|| Regex::new(r"\*\s+as\s+([A-Za-z_$][\w$]*)").unwrap());
    let named_re = JS_NAMED.get_or_init(|| Regex::new(r"\{([^}]*)\}").unwrap());
    let ident_re = JS_IDENT.get_or_init(|| Regex::new(r"[A-Za-z_$][\w$]*").unwrap());
    let reexport_re = JS_REEXPORT.get_or_init(|| {
        Regex::new(r#"(?m)^\s*export\s+(?:type\s+)?\{([^}]*)\}\s*from\s*['\"]([^'\"]+)['\"]"#)
            .unwrap()
    });
    let push_named = |out: &mut Vec<ImportRef>, body: &str, spec: &str| {
        for part in body
            .split(',')
            .map(|value| value.trim().trim_start_matches("type "))
            .filter(|value| !value.is_empty())
        {
            let pieces = part.split_whitespace().collect::<Vec<_>>();
            let (orig_name, name) = if pieces.len() == 3 && pieces[1] == "as" {
                (pieces[0], pieces[2])
            } else {
                (part, part)
            };
            if ident_re
                .find(name)
                .is_some_and(|found| found.as_str() == name)
            {
                out.push(ImportRef {
                    name: name.into(),
                    from: spec.into(),
                    orig: (orig_name != name).then(|| orig_name.to_string()),
                });
            }
        }
    };
    for cap in re.captures_iter(text) {
        let mut body = cap[1].trim().to_string();
        if body.starts_with('(') {
            continue;
        }
        let spec = cap[2].to_string();
        if let Some(ns) = namespace_re.captures(&body) {
            out.push(ImportRef {
                name: ns[1].into(),
                from: spec.clone(),
                orig: None,
            });
            body = body.replacen(ns.get(0).unwrap().as_str(), "", 1);
        }
        if let Some(named) = named_re.captures(&body) {
            push_named(&mut out, &named[1], &spec);
            body = body.replacen(named.get(0).unwrap().as_str(), "", 1);
        }
        let rest = body.replace(',', " ");
        if let Some(def) = ident_re.find(&rest) {
            out.push(ImportRef {
                name: def.as_str().into(),
                from: spec,
                orig: None,
            });
        }
    }
    for captures in reexport_re.captures_iter(text) {
        push_named(&mut out, &captures[1], &captures[2]);
    }
    out
}

fn scan_source(text: &str, file: &str) -> FileEntry {
    let mut lines: Vec<String> = text
        .split('\n')
        .map(|s| s.trim_end_matches('\r').to_string())
        .collect();
    if lines.len() > 1 && lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    if file.ends_with(".py") || file.ends_with(".pyi") {
        static PY_DECL: OnceLock<Regex> = OnceLock::new();
        let re = PY_DECL.get_or_init(|| {
            Regex::new(r"^([ \t]*)(?:async[ \t]+)?(def|class)[ \t]+([A-Za-z_]\w*)").unwrap()
        });
        let mut syms = Vec::new();
        for i in 0..lines.len() {
            let Some(cap) = re.captures(&lines[i]) else {
                continue;
            };
            let indent = cap[1].len();
            let mut end = i;
            for (j, line) in lines.iter().enumerate().skip(i + 1) {
                let trim = line.trim();
                if trim.is_empty() || trim.starts_with('#') {
                    continue;
                }
                let current = line.len() - line.trim_start_matches([' ', '\t']).len();
                if current <= indent {
                    break;
                }
                end = j;
            }
            let name = cap[3].to_string();
            syms.push(Symbol {
                ln: i + 1,
                end: end + 1,
                depth: usize::from(indent > 0),
                kind: if &cap[2] == "class" {
                    "class"
                } else if indent == 0 {
                    "fn"
                } else {
                    "method"
                }
                .into(),
                sig: signature(&lines[i]),
                exp: !name.starts_with('_'),
                name,
            });
        }
        return FileEntry {
            size: text.len() as u64,
            modified_ns: 0,
            total: lines.len(),
            syms,
            imports: Vec::new(),
        };
    }
    let (code, starts, after) = stripped_depth(&lines, file.ends_with(".rs"));
    let mut syms = Vec::new();
    for i in 0..lines.len() {
        let depth = starts[i];
        if depth > 2 {
            continue;
        }
        let stripped = code[i].trim_start();
        let Some((name, kind)) = declaration(stripped, depth) else {
            continue;
        };
        let current = code[i].trim_end();
        let end = if after[i] <= depth && current.ends_with([';', ',']) {
            i
        } else {
            let mut open = None;
            let mut early_end = None;
            for j in i..(i + 14).min(lines.len()) {
                if after[j] > depth {
                    open = Some(j);
                    break;
                }
                if j > i {
                    let value = code[j].trim_end();
                    if value.ends_with([';', ',']) {
                        early_end = Some(j);
                        break;
                    }
                    if value.trim().is_empty() {
                        early_end = Some(j - 1);
                        break;
                    }
                }
            }
            if let Some(end) = early_end {
                end
            } else if let Some(open) = open {
                (open..lines.len())
                    .find(|j| after[*j] <= depth)
                    .unwrap_or(lines.len() - 1)
            } else {
                i
            }
        };
        if (kind == "prop" || kind == "method")
            && end == i
            && !stripped.trim_end().ends_with(['(', '{'])
        {
            continue;
        }
        syms.push(Symbol {
            ln: i + 1,
            end: end + 1,
            depth,
            kind,
            name,
            sig: signature(&lines[i]),
            exp: stripped.starts_with("export") || stripped.starts_with("pub"),
        });
    }
    FileEntry {
        size: text.len() as u64,
        modified_ns: 0,
        total: lines.len(),
        syms,
        imports: extract_imports(text, file),
    }
}

fn resolve_specifier(spec: &str, from: &str, files: &HashSet<String>) -> Option<String> {
    let extensions = [
        "ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs", "vue", "svelte", "rs", "py",
    ];
    let try_base = |base: String| -> Option<String> {
        if files.contains(&base) {
            return Some(base);
        }
        for ext in extensions {
            let p = format!("{base}.{ext}");
            if files.contains(&p) {
                return Some(p);
            }
        }
        for ext in extensions {
            for leaf in ["index", "mod"] {
                let p = format!("{base}/{leaf}.{ext}");
                if files.contains(&p) {
                    return Some(p);
                }
            }
        }
        None
    };
    if spec.contains("::") {
        let mut segs: Vec<_> = spec.split("::").filter(|v| !v.is_empty()).collect();
        let mut base: Vec<String> = Vec::new();
        if segs.first() == Some(&"crate") {
            segs.remove(0);
            let dirs: Vec<_> = from.split('/').collect();
            let pos = dirs.iter().rposition(|v| *v == "src")?;
            base.extend(dirs[..=pos].iter().map(|v| v.to_string()));
        } else if !matches!(segs.first(), Some(&"self") | Some(&"super"))
            && segs.first().is_some_and(|seg| {
                !matches!(*seg, "std" | "core" | "alloc")
                    && seg.chars().next().is_some_and(|ch| ch.is_lowercase())
            })
            && from.split('/').any(|dir| dir == "src")
        {
            // Rust 2018 裸路径（如 `use server::foo`）：按 crate 根目录解析。
            // 外部 crate 名在仓库里没有对应 src/<crate>/ 路径，解析自然落空。
            let dirs: Vec<_> = from.split('/').collect();
            let pos = dirs.iter().rposition(|v| *v == "src")?;
            base.extend(dirs[..=pos].iter().map(|v| v.to_string()));
        } else if matches!(segs.first(), Some(&"self") | Some(&"super")) {
            base.extend(
                from.split('/').collect::<Vec<_>>()[..from.split('/').count() - 1]
                    .iter()
                    .map(|v| v.to_string()),
            );
            while segs.first() == Some(&"super") {
                segs.remove(0);
                base.pop();
            }
            if segs.first() == Some(&"self") {
                segs.remove(0);
            }
        } else {
            return None;
        }
        let mut module = base.clone();
        module.extend(
            segs.iter()
                .take(segs.len().saturating_sub(1))
                .map(|v| v.to_string()),
        );
        return try_base(module.join("/")).or_else(|| {
            let mut all = base;
            all.extend(segs.iter().map(|v| v.to_string()));
            try_base(all.join("/"))
        });
    }
    if !spec.starts_with('.') {
        return None;
    }
    let mut parts: Vec<String> = from.split('/').map(str::to_string).collect();
    parts.pop();
    for component in Path::new(spec).components() {
        match component {
            Component::ParentDir => {
                parts.pop();
            }
            Component::Normal(v) => parts.push(v.to_string_lossy().to_string()),
            _ => {}
        }
    }
    try_base(parts.join("/"))
}

/// 反向 import 图由全量缓存（含历史扫描）构建，让一次调用就能利用仓库级
/// import 关系；个别条目可能陈旧，消费侧读当前文件验证 local 名的 import
/// 行后再采信。
fn reverse_from_files(files: &HashMap<String, FileEntry>, all_set: &HashSet<String>) -> ReverseMap {
    let mut reverse: ReverseMap = HashMap::new();
    for (file, entry) in files {
        for import in &entry.imports {
            if let Some(target) = resolve_specifier(&import.from, file, all_set) {
                reverse.entry(target).or_default().push(ReverseImport {
                    importer: file.clone(),
                    local: import.name.clone(),
                    orig: import.orig.clone(),
                });
            }
        }
    }
    for edges in reverse.values_mut() {
        edges.sort_by(|a, b| a.importer.cmp(&b.importer).then(a.local.cmp(&b.local)));
        edges.dedup_by(|a, b| a.importer == b.importer && a.local == b.local);
    }
    reverse
}

fn build_index(
    root: &Path,
    wanted: Option<&HashSet<String>>,
    dependency_depth: usize,
    known_files: Option<&[String]>,
) -> (IndexView, Arc<Vec<String>>, Arc<ReverseMap>) {
    // load_cache temporarily moves this root's cache out of MEMO. Serialize all users of the same
    // workspace so concurrent Lyra sessions and bridge calls cannot observe an empty cache.
    let cache_lock = CACHE_LOCKS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap()
        .entry(normalize_root(root))
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone();
    let _cache_guard = cache_lock.lock().unwrap();
    let all = known_files
        .map(|files| Arc::new(files.to_vec()))
        .unwrap_or_else(|| list_code_files(root));
    let all_set: HashSet<_> = all.iter().cloned().collect();
    // Shared snapshot. Mutations go into a local `fresh` copy and are published back atomically,
    // so holding the per-root lock never requires deep-copying the shared cache.
    let cache = load_cache(root);
    let initially_empty = cache.files.is_empty();
    let mut fresh = DiskCache {
        version: cache.version,
        root: cache.root.clone(),
        files: cache.files.clone(),
        reverse: cache.reverse.clone(),
    };
    let mut targets: HashSet<String> = wanted.cloned().unwrap_or_else(|| all_set.clone());
    let mut frontier: Vec<String> = targets.iter().cloned().collect();
    let mut dirty = false;
    let mut changed = 0usize;
    let mut removed_files: Vec<String> = Vec::new();
    let mut rescanned_files: Vec<String> = Vec::new();
    for depth in 0..=dependency_depth {
        let current = frontier;
        frontier = Vec::new();
        for file in current {
            let path = root.join(&file);
            let Some((size, modified_ns)) = metadata_stamp(&path) else {
                if fresh.files.remove(&file).is_some() {
                    dirty = true;
                    changed += 1;
                    removed_files.push(file.clone());
                }
                continue;
            };
            let stale = fresh
                .files
                .get(&file)
                .map(|e| e.size != size || e.modified_ns != modified_ns)
                .unwrap_or(true);
            if stale {
                if let Ok(text) = fs::read_to_string(&path) {
                    let mut entry = scan_source(&text, &file);
                    entry.size = size;
                    entry.modified_ns = modified_ns;
                    fresh.files.insert(file.clone(), entry);
                    dirty = true;
                    changed += 1;
                    rescanned_files.push(file.clone());
                }
            }
            if depth < dependency_depth {
                for import in fresh
                    .files
                    .get(&file)
                    .map(|e| e.imports.as_slice())
                    .unwrap_or(&[])
                {
                    if let Some(dep) = resolve_specifier(&import.from, &file, &all_set) {
                        if targets.insert(dep.clone()) {
                            frontier.push(dep);
                        }
                    }
                }
            }
        }
    }
    if wanted.is_none() {
        let pruned: Vec<String> = fresh
            .files
            .keys()
            .filter(|file| !all_set.contains(*file))
            .cloned()
            .collect();
        for file in &pruned {
            fresh.files.remove(file);
        }
        if !pruned.is_empty() {
            dirty = true;
            changed += pruned.len();
            removed_files.extend(pruned);
        }
    }
    // Focused results must depend on this request, not files left by older cache history.
    // Persist the full incremental cache, but expose only the requested dependency closure.
    let selected = if wanted.is_some() {
        targets
            .iter()
            .filter_map(|file| {
                fresh
                    .files
                    .get(file)
                    .map(|entry| (file.clone(), entry.clone()))
            })
            .collect()
    } else {
        fresh.files.clone()
    };
    // 反向图随缓存持久化：无文件变更的调用直接复用。有变更时始终增量补丁
    //（先按 importer 删旧边，再按新 import 表加边），复杂度与变更文件数成正比，
    // 与仓库规模无关；大仓库一次 rebase/批量改动不再触发整图重建。
    if changed == 0 && fresh.reverse.is_empty() && !fresh.files.is_empty() {
        fresh.reverse = reverse_from_files(&fresh.files, &all_set);
    } else if changed > 0 {
        for file in removed_files.iter().chain(rescanned_files.iter()) {
            for edges in fresh.reverse.values_mut() {
                edges.retain(|edge| edge.importer != *file);
            }
        }
        fresh.reverse.retain(|_, edges| !edges.is_empty());
        for file in &rescanned_files {
            if let Some(entry) = fresh.files.get(file) {
                for import in &entry.imports {
                    if let Some(target) = resolve_specifier(&import.from, file, &all_set) {
                        fresh
                            .reverse
                            .entry(target)
                            .or_default()
                            .push(ReverseImport {
                                importer: file.clone(),
                                local: import.name.clone(),
                                orig: import.orig.clone(),
                            });
                    }
                }
            }
        }
        for edges in fresh.reverse.values_mut() {
            edges.sort_by(|a, b| a.importer.cmp(&b.importer).then(a.local.cmp(&b.local)));
            edges.dedup_by(|a, b| a.importer == b.importer && a.local == b.local);
        }
    }
    let reverse = Arc::new(fresh.reverse.clone());
    // 少量变更只更新内存 MEMO；整仓缓存的 bincode 全量序列化写盘按
    // PERSIST_INTERVAL_MS 节流并异步执行，查询线程不再阻塞在磁盘上。首次建缓存
    // （initially_empty）立即持久化，保证冷启动有基线可用。
    let persist = dirty && (initially_empty || changed > 0);
    // Publish the updated cache before releasing the per-root mutation lock. Persistence is
    // queued to a background thread, so another request can use the warm in-memory snapshot
    // instead of waiting for a full bincode serialization and disk replacement.
    let updated = Arc::new(fresh);
    store_cache(root, updated.clone(), persist);
    drop(_cache_guard);
    let mut view = IndexView {
        files: selected,
        ..Default::default()
    };
    let mut indexed_files = view.files.keys().cloned().collect::<Vec<_>>();
    indexed_files.sort();
    for file in &indexed_files {
        let entry = &view.files[file];
        for symbol in &entry.syms {
            if symbol.kind == "prop" || symbol.depth > 1 {
                continue;
            }
            if symbol.depth == 1
                && !matches!(symbol.kind.as_str(), "fn" | "method" | "type" | "class")
            {
                continue;
            }
            view.defs
                .entry(symbol.name.clone())
                .or_default()
                .push(Definition {
                    file: file.clone(),
                    symbol: symbol.clone(),
                });
        }
    }
    for file in &indexed_files {
        let entry = &view.files[file];
        let mut imports = HashMap::new();
        for import in &entry.imports {
            if let Some(target) = resolve_specifier(&import.from, file, &all_set) {
                imports.insert(import.name.clone(), target);
            }
        }
        if !imports.is_empty() {
            view.imports.insert(file.clone(), imports);
        }
    }
    for definitions in view.defs.values_mut() {
        definitions.sort_by(|a, b| a.file.cmp(&b.file).then(a.symbol.ln.cmp(&b.symbol.ln)));
    }
    (view, all, reverse)
}

fn source(root: &Path, file: &str, entry: Option<&FileEntry>) -> Option<Source> {
    let text = fs::read_to_string(root.join(file)).ok()?;
    let mut lines: Vec<_> = text.split('\n').map(str::to_string).collect();
    if lines.len() > 1 && lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    let syms = if entry.is_some_and(|e| e.total == lines.len()) {
        entry.unwrap().syms.clone()
    } else {
        scan_source(&text, file).syms
    };
    Some(Source { lines, syms })
}

fn js_utf16_len(value: &str) -> usize {
    value.encode_utf16().count()
}

fn js_utf16_slice(value: &str, units: usize) -> String {
    let utf16 = value.encode_utf16().take(units).collect::<Vec<_>>();
    String::from_utf16_lossy(&utf16)
}

fn clip(line: &str) -> String {
    let length = js_utf16_len(line);
    if length <= MAX_LINE_CHARS {
        line.into()
    } else {
        format!(
            "{}…(+{}c)",
            js_utf16_slice(line, MAX_LINE_CHARS),
            length - MAX_LINE_CHARS
        )
    }
}

fn range_cost(source: &Source, start: usize, end: usize) -> usize {
    48 + source.lines[start - 1..end]
        .iter()
        .map(|line| clip(line).len() + 1)
        .sum::<usize>()
}
fn noise_path(file: &str) -> bool {
    static NOISE: OnceLock<Regex> = OnceLock::new();
    NOISE
        .get_or_init(|| Regex::new(r"(?i)\.(test|spec)\.[^.]+$|/__tests__/|/tests?/").unwrap())
        .is_match(file)
}

#[derive(Default)]
struct RetrievalPlan {
    active: bool,
    errors: bool,
    callers: bool,
    tests: bool,
    config: bool,
    state: bool,
}

fn retrieval_plan(task: &str) -> RetrievalPlan {
    let text = task.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|word| text.contains(word));
    RetrievalPlan {
        active: !task.trim().is_empty(),
        errors: has(&[
            "错误",
            "失败",
            "异常",
            "error",
            "exception",
            "fail",
            "throw",
            "catch",
        ]),
        callers: has(&["调用方", "兼容", "api", "返回", "return", "caller", "签名"]),
        tests: has(&["测试", "回归", "test", "spec"]),
        config: has(&["配置", "设置", "config", "setting", "option"]),
        state: has(&[
            "状态", "会话", "缓存", "并发", "锁", "state", "session", "cache", "concurr", "lock",
            "mutex",
        ]),
    }
}

fn plan_terms_from_bodies(
    plan: &RetrievalPlan,
    index: &IndexView,
    bodies: &[(String, String)],
    existing: &[String],
) -> Vec<String> {
    if !plan.active {
        return Vec::new();
    }
    static IDENT: OnceLock<Regex> = OnceLock::new();
    static LOCAL: OnceLock<Regex> = OnceLock::new();
    let ident = IDENT.get_or_init(|| Regex::new(r"[A-Za-z_$][A-Za-z0-9_$]*").unwrap());
    let local = LOCAL.get_or_init(|| Regex::new(r"\b(?:const|let|var|function|fn|struct|enum|class|type|interface)\s+([A-Za-z_$][\w$]*)").unwrap());
    let blocked = existing
        .iter()
        .map(|term| term.to_lowercase())
        .collect::<HashSet<_>>();
    let mut candidates = HashMap::<String, (i64, usize)>::new();
    for (file, body) in bodies {
        let locals = local
            .captures_iter(body)
            .map(|captures| captures[1].to_string())
            .collect::<HashSet<_>>();
        for found in ident.find_iter(body) {
            let name = found.as_str();
            let lower = name.to_lowercase();
            if name.len() < 3
                || blocked.contains(&lower)
                || stop_word(&lower)
                || locals.contains(name)
                || (found.start() > 0 && body.as_bytes()[found.start() - 1] == b'.')
                || resolve_ref(index, name, file).is_none()
            {
                continue;
            }
            let prefix = body[..found.start()]
                .chars()
                .rev()
                .take(40)
                .collect::<String>()
                .chars()
                .rev()
                .collect::<String>();
            let suffix = body[found.end()..].chars().take(8).collect::<String>();
            let call = suffix.trim_start().starts_with('(') || suffix.trim_start().starts_with('<');
            let mut score = 12
                + if call { 12 } else { 0 }
                + if name.chars().next().is_some_and(char::is_uppercase) {
                    5
                } else {
                    0
                };
            let prefix_lower = prefix.to_lowercase();
            if plan.errors
                && ["throw", "catch", "instanceof", "reject", "fail", "new"]
                    .iter()
                    .any(|word| prefix_lower.trim_end().ends_with(word))
            {
                score += 24;
            }
            if plan.callers && call {
                score += 8;
            }
            if plan.config
                && ["Config", "Settings", "Option", "Options", "Policy"]
                    .iter()
                    .any(|suffix| name.ends_with(suffix))
            {
                score += 6;
            }
            if plan.state
                && [
                    "State", "Store", "Session", "Cache", "Lock", "Mutex", "Queue", "Manager",
                ]
                .iter()
                .any(|suffix| name.ends_with(suffix))
            {
                score += 6;
            }
            let entry = candidates.entry(name.to_string()).or_insert((0, 0));
            entry.0 += score;
            entry.1 += 1;
        }
    }
    let mut ranked = candidates.into_iter().collect::<Vec<_>>();
    ranked.sort_by(|a, b| {
        b.1 .0
            .cmp(&a.1 .0)
            .then_with(|| b.1 .1.cmp(&a.1 .1))
            .then_with(|| a.0.cmp(&b.0))
    });
    ranked
        .into_iter()
        .take(MAX_GRAPH_TERMS)
        .map(|(name, _)| name)
        .collect()
}

fn score_path(file: &str) -> i64 {
    let mut score = 0;
    if !is_searchable_implementation_file(file) {
        score -= 90;
    }
    if noise_path(file) {
        score -= 55;
    }
    if file.contains("scripts/legacy-context") {
        score -= 60;
    }
    if file.starts_with("src/") || file.starts_with("src-tauri/src/") {
        score += 14;
    }
    if file.starts_with("scripts/") {
        score += 10;
    }
    let lower = file.to_ascii_lowercase();
    if lower.ends_with(".md")
        || lower.ends_with(".json")
        || lower.ends_with(".yaml")
        || lower.ends_with(".yml")
        || lower.ends_with(".toml")
        || lower.ends_with(".txt")
    {
        score -= 40;
    }
    score
}
fn unit_for_hit<'a>(symbols: &'a [Symbol], line: usize) -> (Option<&'a Symbol>, Vec<&'a Symbol>) {
    let mut chain = symbols
        .iter()
        .filter(|symbol| symbol.ln <= line && symbol.end >= line)
        .collect::<Vec<_>>();
    chain.sort_by_key(|symbol| (symbol.depth, symbol.ln));
    let span = |symbol: &Symbol| symbol.end - symbol.ln + 1;
    let mut picked = chain
        .iter()
        .rev()
        .find(|symbol| span(symbol) >= 12)
        .copied()
        .or_else(|| chain.last().copied());
    if picked.is_some_and(|symbol| span(symbol) > 80) {
        if let Some(compact) = chain.iter().rev().find(|symbol| {
            let size = span(symbol);
            size >= 4 && size <= 80
        }) {
            picked = Some(*compact);
        }
    }
    (picked, chain)
}

fn unit_label(chain: &[&Symbol], unit: &Symbol) -> String {
    let mut parts = chain
        .iter()
        .filter(|symbol| symbol.depth < unit.depth)
        .rev()
        .take(2)
        .map(|symbol| symbol.name.clone())
        .collect::<Vec<_>>();
    parts.reverse();
    parts.push(if matches!(unit.kind.as_str(), "prop" | "method") {
        unit.name.clone()
    } else {
        format!("{} {}", unit.kind, unit.name)
    });
    parts.join(" > ")
}

fn contains_ascii_word(text: &str, needle: &str) -> bool {
    text.match_indices(needle).any(|(start, value)| {
        let before = text[..start].chars().next_back();
        let end = start + value.len();
        let after = text[end..].chars().next();
        let is_word = |ch: char| ch.is_ascii_alphanumeric() || ch == '_';
        before.is_none_or(|ch| !is_word(ch)) && after.is_none_or(|ch| !is_word(ch))
    })
}

fn merge_ranges(mut ranges: Vec<(usize, usize)>) -> Vec<(usize, usize)> {
    if ranges.is_empty() {
        return ranges;
    }
    ranges.sort_unstable();
    let mut merged = Vec::new();
    let (mut start, mut end) = ranges[0];
    for (next_start, next_end) in ranges.into_iter().skip(1) {
        if next_start <= end + 1 {
            end = end.max(next_end);
        } else {
            merged.push((start, end));
            start = next_start;
            end = next_end;
        }
    }
    merged.push((start, end));
    merged
}

fn shown_ranges(plan: &PlannedFile) -> Vec<(usize, usize)> {
    if plan.full {
        vec![(1, plan.source.lines.len())]
    } else {
        merge_ranges(
            plan.blocks
                .iter()
                .map(|block| (block.start, block.end))
                .collect(),
        )
    }
}
fn covered(plan: &PlannedFile, line: usize) -> bool {
    shown_ranges(plan)
        .iter()
        .any(|(a, b)| line >= *a && line <= *b)
}

fn is_han(value: char) -> bool {
    matches!(
        value as u32,
        0x3400..=0x4DBF
            | 0x4E00..=0x9FFF
            | 0xF900..=0xFAFF
            | 0x20000..=0x2FA1F
            | 0x30000..=0x323AF
    )
}

fn task_tokens(task: &str) -> Vec<String> {
    const MAX_TASK_TOKENS: usize = 24;
    const MAX_ASCII_TASK_TOKENS: usize = 8;
    const CJK_NGRAM_MIN: usize = 3;
    const CJK_NGRAM_MAX: usize = 5;
    static ASCII_TOKEN: OnceLock<Regex> = OnceLock::new();
    let token_re = ASCII_TOKEN.get_or_init(|| Regex::new(r"[A-Za-z_$][A-Za-z0-9_$]{3,}").unwrap());
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut add = |token: String| {
        let lower = token.to_lowercase();
        if !token.is_empty()
            && out.len() < MAX_TASK_TOKENS
            && !stop_word(&lower)
            && seen.insert(lower)
        {
            out.push(token);
        }
    };
    for found in token_re.find_iter(task).take(MAX_ASCII_TASK_TOKENS) {
        add(found.as_str().to_string());
    }
    let mut phrase = Vec::new();
    let flush = |phrase: &mut Vec<char>, add: &mut dyn FnMut(String)| {
        if phrase.len() >= CJK_NGRAM_MIN {
            add(phrase[phrase.len() - CJK_NGRAM_MIN..].iter().collect());
            for size in (CJK_NGRAM_MIN..=CJK_NGRAM_MAX.min(phrase.len())).rev() {
                for chars in phrase.windows(size) {
                    add(chars.iter().collect());
                }
            }
        }
        phrase.clear();
    };
    for ch in task.chars() {
        if is_han(ch) {
            phrase.push(ch);
        } else {
            flush(&mut phrase, &mut add);
        }
    }
    flush(&mut phrase, &mut add);
    out
}

fn explicit_anchor(value: &str) -> bool {
    static ANCHOR: OnceLock<Regex> = OnceLock::new();
    ANCHOR
        .get_or_init(|| Regex::new(r"^[A-Za-z_$][A-Za-z0-9_$-]{2,}$").unwrap())
        .is_match(value)
}

fn naming_variants(value: &str) -> Vec<String> {
    if !explicit_anchor(value) {
        return (!value.is_empty())
            .then(|| value.to_string())
            .into_iter()
            .collect();
    }
    static ACRONYM: OnceLock<Regex> = OnceLock::new();
    static CAMEL: OnceLock<Regex> = OnceLock::new();
    // 替换串里的 $1_ 会被当成名为 "1_" 的分组（下划线是 name 字符）而替换成空，
    // 必须用 ${1}_${2} 显式界定，否则 camelCase 锚点的 snake/kebab 变体全部丢失。
    let separated = ACRONYM
        .get_or_init(|| Regex::new(r"([A-Z]+)([A-Z][a-z])").unwrap())
        .replace_all(value, "${1}_${2}");
    let separated = CAMEL
        .get_or_init(|| Regex::new(r"([a-z0-9])([A-Z])").unwrap())
        .replace_all(&separated, "${1}_${2}");
    let words = separated
        .split(['-', '_', '$'])
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect::<Vec<_>>();
    if words.is_empty() {
        return vec![value.to_string()];
    }
    let pascal = words
        .iter()
        .map(|word| {
            let mut chars = word.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().collect::<String>() + chars.as_str())
                .unwrap_or_default()
        })
        .collect::<String>();
    let camel = words[0].clone() + &pascal[words[0].len()..];
    let mut seen = HashSet::new();
    [
        value.to_string(),
        words.join("_"),
        words.join("-"),
        camel,
        pascal,
    ]
    .into_iter()
    .filter(|variant| seen.insert(variant.clone()))
    .collect()
}

fn anchor_noise_path(file: &str) -> bool {
    static NOISE: OnceLock<Regex> = OnceLock::new();
    NOISE
        .get_or_init(|| Regex::new(r"(?i)(?:^|/)(?:docs?|examples?|fixtures?)/|(?:^|/)readme(?:\.|$)|\.(?:test|spec|eval|bench)\.[^.]+$|/(?:__tests__|tests?|benches?)/").unwrap())
        .is_match(file)
}

fn production_anchor_hit(rows: &[SearchRow], keyword: &str) -> bool {
    let variants = naming_variants(keyword)
        .into_iter()
        .map(|variant| variant.to_lowercase())
        .collect::<Vec<_>>();
    rows.iter().any(|row| {
        if !is_searchable_implementation_file(&row.file) || anchor_noise_path(&row.file) {
            return false;
        }
        let line = row.text.to_lowercase();
        variants.iter().any(|variant| line.contains(variant))
    })
}

/// 短语关键词（含空白）拆成可检索单词；非短语返回空。
fn phrase_words(keyword: &str) -> Vec<String> {
    if !keyword.chars().any(char::is_whitespace) {
        return Vec::new();
    }
    keyword
        .split_whitespace()
        .filter(|part| part.len() >= 2 && explicit_anchor(part))
        .map(str::to_string)
        .collect()
}

/// 软降级命中判定：短语关键词看拆出的单词是否命中生产代码。
fn keyword_hit(rows: &[SearchRow], keyword: &str) -> bool {
    let words = phrase_words(keyword);
    if words.is_empty() {
        production_anchor_hit(rows, keyword)
    } else {
        words.iter().any(|word| production_anchor_hit(rows, word))
    }
}

fn source_scope(file: &str) -> &str {
    if file.starts_with("src-tauri/src/") {
        "src-tauri/src/"
    } else {
        file.find('/').map(|slash| &file[..=slash]).unwrap_or("")
    }
}

/// 种子文件的目录 scope 集合（source_scope 语义）。任一种子文件位于仓库根
///（无目录 scope）时返回 None，退回全仓检索，保持旧 files_in_source_scopes
/// 含 "" 时全量展开的语义。
fn scope_dirs(seed_files: &[String]) -> Option<Vec<String>> {
    let mut dirs = HashSet::<String>::new();
    for file in seed_files {
        let scope = source_scope(file);
        if scope.is_empty() {
            return None;
        }
        dirs.insert(scope.to_string());
    }
    if dirs.is_empty() {
        return None;
    }
    let mut list = dirs.into_iter().collect::<Vec<_>>();
    list.sort();
    Some(list)
}

/// 按目录 scope 检索：把目录直接作为 rg 的搜索路径，单进程完成（替代过去把
/// 展开后的整份文件列表按 128 个切片、逐片起 rg 的做法）。None = 全仓。
/// 两种情形都用扩展名 glob 限定代码文件，与旧的按 all 展开过滤语义一致。
/// rg 不可用时把目录展开成代码文件列表退回 search_text。
fn search_text_scopes_until(
    root: &Path,
    terms: &[String],
    ignore_case: bool,
    word: bool,
    dirs: Option<&[String]>,
    deadline: Option<Instant>,
) -> Vec<SearchRow> {
    if let Some(rows) = search_index_rows(root, terms, ignore_case, word, dirs, deadline) {
        return rows;
    }
    if rg_available(root) {
        let rows = match dirs {
            Some(dirs) if !dirs.is_empty() => {
                rg_search(root, terms, ignore_case, word, dirs, true, deadline)
            }
            _ => rg_search(root, terms, ignore_case, word, &[], true, deadline),
        };
        if let Some(rows) = rows {
            return rows;
        }
    }
    let files = match dirs {
        Some(dirs) if !dirs.is_empty() => list_code_files(root)
            .iter()
            .filter(|file| dirs.iter().any(|dir| file.starts_with(dir)))
            .cloned()
            .collect::<Vec<_>>(),
        _ => Vec::new(),
    };
    search_text_until(root, terms, ignore_case, word, &files, deadline)
}

fn search_text_scopes(
    root: &Path,
    terms: &[String],
    ignore_case: bool,
    word: bool,
    dirs: Option<&[String]>,
) -> Vec<SearchRow> {
    search_text_scopes_until(root, terms, ignore_case, word, dirs, search_deadline())
}

fn compact_evidence_miss(
    revision: &str,
    keywords: &[String],
    task: &str,
    suggestions: &[String],
) -> String {
    let mut lines = vec![
        format!("# CTX MISS @{revision}"),
        format!("query: {}", keywords.join(",")),
    ];
    if !task.is_empty() {
        lines.push(format!("task: {task}"));
    }
    lines.extend([
        "status: no production definition or reference".into(),
        "evidence: exact/ignore-case/naming-variant search found 0 production occurrences; test/eval/doc mentions ignored".into(),
        "checked: symbol names, references, snake/kebab/pascal variants, string bindings".into(),
    ]);
    if !suggestions.is_empty() {
        lines.push(format!("did-you-mean: {}", suggestions.join(", ")));
    }
    lines.extend([
        "fallback: disabled; natural-language task terms cannot establish an explicit edit target"
            .into(),
        "next: provide the missing source/path or correct the symbol name; or retry with a did-you-mean symbol / known entry file via files".into(),
    ]);
    lines.join("\n")
}

/// 硬 MISS 时的"你是不是想找"：把未命中锚点拆成词，一次批量检索后从命中行提取
/// 包含这些词的真实标识符，按覆盖词数/频次/是否定义行打分。只给生产代码里的
/// 符号；测试/文档命中不算。找不到相近符号时返回空，MISS 保持原样。
fn suggest_symbols(root: &Path, anchors: &[String]) -> Vec<String> {
    let mut words = Vec::<String>::new();
    for anchor in anchors {
        for variant in naming_variants(anchor) {
            for word in variant
                .to_lowercase()
                .split(|ch: char| !ch.is_ascii_alphanumeric())
                .filter(|word| !word.is_empty())
            {
                if word.len() >= DID_YOU_MEAN_MIN_WORD
                    && !stop_word(word)
                    && !words.iter().any(|existing| existing == word)
                {
                    words.push(word.to_string());
                }
            }
        }
    }
    words.truncate(8);
    if words.is_empty() {
        return Vec::new();
    }
    let rows = search_text_scopes(root, &words, true, false, None);
    static IDENT: OnceLock<Regex> = OnceLock::new();
    let ident = IDENT.get_or_init(|| Regex::new(r"[A-Za-z_$][\w$]{2,}").unwrap());
    static DEF_LINE: OnceLock<Regex> = OnceLock::new();
    let def_line = DEF_LINE.get_or_init(|| Regex::new(r"^\s*(?:(?:pub(?:\([^)]*\))?|export|async|unsafe|default|static|const|move)\s+)*(?:fn|struct|enum|trait|impl|type|class|interface|function|def|mod)\b").unwrap());
    struct Scored {
        hits: usize,
        words: HashSet<String>,
        location: String,
        def: bool,
    }
    let mut scored = HashMap::<String, Scored>::new();
    for row in &rows {
        if !is_code_file(&row.file) || anchor_noise_path(&row.file) {
            continue;
        }
        for found in ident.find_iter(&row.text) {
            let name = found.as_str();
            let low = name.to_lowercase();
            if low.len() < 5 || words.contains(&low) || stop_word(&low) {
                continue;
            }
            let matched = words
                .iter()
                .filter(|word| low.contains(word.as_str()))
                .collect::<Vec<_>>();
            if matched.is_empty() {
                continue;
            }
            let entry = scored.entry(name.to_string()).or_insert_with(|| Scored {
                hits: 0,
                words: HashSet::new(),
                location: format!("{}:{}", row.file, row.ln),
                def: false,
            });
            entry.hits += 1;
            for word in matched {
                entry.words.insert(word.clone());
            }
            if def_line.is_match(&row.text) {
                entry.def = true;
            }
        }
    }
    // typo 感知：与某个锚点编辑距离 ≤2 的候选提到最前，让“modeChoics”这类
    // 拼写错误的真身排在仅共享词根的远亲前面，也给自动锚点更正提供候选。
    let normalize = |value: &str| {
        value
            .chars()
            .filter(|ch| ch.is_ascii_alphanumeric())
            .map(|ch| ch.to_ascii_lowercase())
            .collect::<String>()
    };
    let normalized_anchors = anchors.iter().map(|a| normalize(a)).collect::<Vec<_>>();
    let score_of = |name: &str, item: &Scored| {
        let base = item.words.len() * 10 + item.hits.min(5) + usize::from(item.def) * 4;
        let close = normalized_anchors
            .iter()
            .any(|anchor| !anchor.is_empty() && levenshtein(anchor, &normalize(name), 3) <= 2);
        base + usize::from(close) * 100
    };
    let mut list = scored.into_iter().collect::<Vec<_>>();
    list.sort_by(|(a_name, a), (b_name, b)| {
        score_of(b_name, b)
            .cmp(&score_of(a_name, a))
            .then_with(|| a_name.cmp(b_name))
    });
    list.truncate(DID_YOU_MEAN_MAX);
    list.into_iter()
        .map(|(name, item)| format!("{name} ({})", item.location))
        .collect()
}

/// 文件名按分隔符与 camelCase 切段，查询词须完整命中一个段才算主题文件，
/// 避免短泛词 mode 子串匹配 model_cache.rs 之类文件造成噪声。
fn file_segments(file: &str) -> Vec<String> {
    let base = file.rsplit('/').next().unwrap_or(file);
    let stem = base.rsplit_once('.').map(|(stem, _)| stem).unwrap_or(base);
    let mut segments = Vec::new();
    for part in stem.split(|ch: char| !ch.is_ascii_alphanumeric()) {
        let chars: Vec<char> = part.chars().collect();
        let mut piece = String::new();
        for (index, ch) in chars.iter().enumerate() {
            let prev = if index > 0 {
                Some(chars[index - 1])
            } else {
                None
            };
            if ch.is_ascii_uppercase()
                && prev.map_or(false, |p| p.is_ascii_lowercase() || p.is_ascii_digit())
            {
                if !piece.is_empty() {
                    segments.push(piece.to_lowercase());
                }
                piece = String::new();
            }
            piece.push(*ch);
        }
        if !piece.is_empty() {
            segments.push(piece.to_lowercase());
        }
    }
    segments
}

/// 词频表：大小写变体取最大命中行数，衡量词稀有度。
fn term_freq_map(counts: &HashMap<String, usize>) -> HashMap<String, usize> {
    let mut freq = HashMap::new();
    for (key, n) in counts {
        let low = key.to_lowercase();
        freq.entry(low)
            .and_modify(|value: &mut usize| *value = (*value).max(*n))
            .or_insert(*n);
    }
    freq
}

/// 主题加分按词稀有度衰减：build/mode 这类命中数百上千行的泛词，仅凭文件名
/// 不能把 build.rs / build.bat 顶进 EDIT 槽位；低频符号名仍拿满分。
fn subject_match(file: &str, subject_terms: &[String], term_freq: &HashMap<String, usize>) -> f64 {
    if subject_terms.is_empty() {
        return 0.0;
    }
    let segments = file_segments(file);
    // 驼峰/连字符文件名的合并形态（ChatView.tsx → chatview）：subject term 是
    // 整词小写，按段比较永远落空，导致文件名与关键词同名的文件反而拿不到主题加分。
    let joined = segments.concat();
    let mut best = 0.0_f64;
    for term in subject_terms {
        if joined != *term && !segments.iter().any(|segment| segment == term) {
            continue;
        }
        let freq = term_freq.get(term).copied().unwrap_or(0).max(1) as f64;
        best = best.max(SUBJECT_BONUS * (80.0 / freq).clamp(0.2, 1.0));
    }
    best
}

fn is_subject_file(file: &str, subject_terms: &[String]) -> bool {
    if subject_terms.is_empty() {
        return false;
    }
    let segments = file_segments(file);
    let joined = segments.concat();
    subject_terms
        .iter()
        .any(|term| joined == *term || segments.iter().any(|segment| segment == term))
}

fn stop_word(value: &str) -> bool {
    matches!(
        value,
        "self"
            | "this"
            | "true"
            | "false"
            | "null"
            | "none"
            | "some"
            | "void"
            | "undefined"
            | "async"
            | "await"
            | "const"
            | "let"
            | "var"
            | "function"
            | "return"
            | "export"
            | "import"
            | "from"
            | "default"
            | "class"
            | "extends"
            | "implements"
            | "interface"
            | "type"
            | "enum"
            | "struct"
            | "trait"
            | "impl"
            | "pub"
            | "crate"
            | "super"
            | "match"
            | "while"
            | "break"
            | "continue"
            | "else"
            | "catch"
            | "throw"
            | "typeof"
            | "instanceof"
            | "string"
            | "number"
            | "boolean"
            | "object"
            | "symbol"
            | "bigint"
            | "never"
            | "unknown"
            | "any"
            | "array"
            | "promise"
            | "record"
            | "partial"
            | "readonly"
            | "static"
            | "public"
            | "private"
            | "protected"
            | "delete"
            | "error"
            | "result"
            | "option"
            | "vec"
            | "hashmap"
            | "value"
            | "data"
            | "text"
            | "name"
            | "path"
            | "file"
            | "line"
            | "lines"
            | "args"
            | "options"
            | "opts"
            | "params"
            | "props"
            | "state"
            | "index"
            | "item"
            | "items"
            | "json"
            | "utf8"
            | "length"
            | "push"
            | "slice"
            | "split"
            | "join"
            | "test"
            | "exec"
            | "clone"
            | "unwrap"
            | "expect"
            | "into"
            | "iter"
            | "collect"
            | "format"
            | "println"
            | "console"
            | "process"
            | "require"
            | "with"
            | "then"
            | "when"
            | "that"
            // 工具名常出现在任务描述里（排查 polaris 自身时），不是代码锚点。
            | "polaris"
    )
}

fn resolve_ref<'a>(index: &'a IndexView, name: &str, from_file: &str) -> Option<&'a Definition> {
    if let Some(entry) = index.files.get(from_file) {
        if let Some(symbol) = entry
            .syms
            .iter()
            .find(|symbol| symbol.name == name && symbol.depth == 0 && symbol.kind != "prop")
        {
            return index.defs.get(name)?.iter().find(|definition| {
                definition.file == from_file && definition.symbol.ln == symbol.ln
            });
        }
    }
    let target = index.imports.get(from_file)?.get(name)?;
    index
        .defs
        .get(name)?
        .iter()
        .find(|definition| &definition.file == target)
}

fn collect_dependencies(
    index: &IndexView,
    plans: &[PlannedFile],
    owned: &HashSet<(String, usize)>,
    keywords: &HashSet<String>,
    priority: &HashSet<String>,
) -> Vec<(String, Definition)> {
    static IDENT: OnceLock<Regex> = OnceLock::new();
    static LOCAL: OnceLock<Regex> = OnceLock::new();
    let ident = IDENT.get_or_init(|| Regex::new(r"[A-Za-z_$][A-Za-z0-9_$]*").unwrap());
    let local = LOCAL.get_or_init(|| Regex::new(r"\b(?:const|let|var|function|fn|struct|enum|class|type|interface)\s+([A-Za-z_$][\w$]*)").unwrap());
    #[derive(Clone)]
    struct Seen {
        name: String,
        count: usize,
        call: bool,
        files: Vec<String>,
    }
    let mut seen = Vec::<Seen>::new();
    let mut positions = HashMap::<String, usize>::new();
    let mut locals = HashSet::<String>::new();
    for plan in plans {
        // JS collectDeps receives one unitTexts entry per shown range. Keep ranges separate:
        // concatenating them can merge boundary tokens and change stable score tie ordering.
        for (start, end) in shown_ranges(plan) {
            let text = plan.source.lines[start - 1..end].join("\n");
            for captures in local.captures_iter(&text) {
                locals.insert(captures[1].to_string());
            }
            for found in ident.find_iter(&text) {
                let name = found.as_str();
                if name.len() < 4 || stop_word(&name.to_lowercase()) {
                    continue;
                }
                if found.start() > 0 && text.as_bytes()[found.start() - 1] == b'.' {
                    continue;
                }
                let after = text.as_bytes().get(found.end()).copied();
                let call = matches!(after, Some(b'(' | b'<'));
                if let Some(position) = positions.get(name).copied() {
                    let entry = &mut seen[position];
                    entry.count += 1;
                    entry.call |= call;
                    if !entry.files.contains(&plan.file) {
                        entry.files.push(plan.file.clone());
                    }
                } else {
                    positions.insert(name.to_string(), seen.len());
                    seen.push(Seen {
                        name: name.into(),
                        count: 1,
                        call,
                        files: vec![plan.file.clone()],
                    });
                }
            }
        }
    }
    let mut candidates = Vec::<(String, Definition, i64, usize)>::new();
    for info in seen {
        if keywords.contains(&info.name) || locals.contains(&info.name) {
            continue;
        }
        let mut definition = None;
        for file in &info.files {
            if let Some(found) = resolve_ref(index, &info.name, file) {
                definition = Some(found.clone());
                break;
            }
        }
        let Some(definition) = definition else {
            continue;
        };
        if owned.contains(&(definition.file.clone(), definition.symbol.ln))
            || matches!(definition.symbol.kind.as_str(), "mod" | "impl")
        {
            continue;
        }
        let size = definition.symbol.end - definition.symbol.ln + 1;
        let mut score = info.count as i64 * 3 + if info.call { 8 } else { 0 };
        // 种子签名里的类型名提权：改签名/改 API 的任务里它就是必看依赖，
        // 不能输给正文里一票普通 helper 调用。
        if priority.contains(&info.name) {
            score += 30;
        }
        if size <= 40 {
            score += 6;
        }
        if definition.file.starts_with("src/")
            || definition.file.starts_with("src-tauri/src/")
            || definition.file.starts_with("scripts/")
        {
            score += 3;
        }
        candidates.push((info.name, definition, score, size));
    }
    candidates.sort_by(|a, b| b.2.cmp(&a.2));
    let mut per_file = HashMap::<String, usize>::new();
    let mut picked = Vec::new();
    for (name, definition, _, _) in candidates {
        if picked.len() >= MAX_DEPS {
            break;
        }
        let count = per_file.entry(definition.file.clone()).or_default();
        if *count >= 3 {
            continue;
        }
        *count += 1;
        picked.push((name, definition));
    }
    picked
}

/// git 共改耦合：种子文件近期触及提交里的高频共改文件（可选开关），抓文本
/// 零耦合的关联文件（DI 注册表、路由表、配套样式）。历史窗口、执行时间和输出均
/// 有硬上限，避免稀有文件在超大历史仓库中遍历全部提交。
fn co_changed_files(
    root: &Path,
    seed_files: &[String],
    exclude: &HashSet<String>,
    limit: usize,
    tests_only: bool,
) -> Vec<(String, usize)> {
    if seed_files.is_empty() {
        return Vec::new();
    }
    let mut seeds = seed_files.iter().take(4).cloned().collect::<Vec<_>>();
    seeds.sort();
    seeds.dedup();
    let history_days = std::env::var("NOVA_CONTEXT_GIT_HISTORY_DAYS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(CO_CHANGE_DEFAULT_HISTORY_DAYS)
        .clamp(30, 3650);
    let cache_key = format!(
        "{}\0{}\0{}\0{}",
        normalize_root(root),
        short_rev(root),
        history_days,
        seeds.join("\0")
    );
    let cache = CO_CHANGE_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let cached = cache.lock().unwrap().get(&cache_key).cloned();
    // 共改耦合是显式开启的附加信息（coupling: true），调用方期待即时返回；git log
    // 本身有 CO_CHANGE_DEADLINE_MS 硬上限，未命中缓存时同步计算一次并入库，后续
    // 相同 HEAD+种子的查询零等待复用。
    let counts = if let Some(cached) = cached {
        cached
    } else {
        let counts = compute_co_changed_counts(root, &seeds, history_days);
        let mut guard = cache.lock().unwrap();
        if guard.len() >= 64 {
            guard.clear();
        }
        guard.insert(cache_key, counts.clone());
        counts
    };
    let mut list = counts
        .into_iter()
        .filter(|(file, _)| {
            !exclude.contains(file)
                && root.join(file).is_file()
                && (!tests_only || noise_path(file))
        })
        .collect::<Vec<_>>();
    list.truncate(limit);
    list
}

/// 共改历史统计：--full-diff 保持提交选择按路径限定，同时列出每个入选提交改动的
/// 全部文件，避免旧的 log + 巨型多 SHA show 配对。带 CO_CHANGE_DEADLINE_MS 硬上限，
/// 结果写入 CO_CHANGE_CACHE 供相同 HEAD+种子的后续查询零等待复用。
fn compute_co_changed_counts(
    root: &Path,
    seeds: &[String],
    history_days: u64,
) -> Vec<(String, usize)> {
    const COMMIT_MARK: &str = "@@NOVA_COMMIT@@";
    let mut args = vec![
        "log".to_string(),
        format!("--format={COMMIT_MARK}%H"),
        "--name-only".to_string(),
        "--full-diff".to_string(),
        "--no-renames".to_string(),
        "--first-parent".to_string(),
        "--max-count=120".to_string(),
        format!("--since={history_days}.days.ago"),
        "--".to_string(),
    ];
    args.extend(seeds.iter().cloned());
    let deadline = Instant::now() + Duration::from_millis(CO_CHANGE_DEADLINE_MS);
    let Some(bytes) = run_command_until_limited(
        root,
        "git",
        &args,
        Some(deadline),
        CO_CHANGE_MAX_OUTPUT_BYTES,
    ) else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&bytes);
    let seed_set = seeds.iter().collect::<HashSet<_>>();
    let mut counts = HashMap::<String, usize>::new();
    for commit in text.split(COMMIT_MARK).skip(1) {
        let mut lines = commit
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty());
        let _sha = lines.next();
        let files = lines.map(normalize_rel).collect::<Vec<_>>();
        if !files.iter().any(|file| seed_set.contains(file)) {
            continue;
        }
        for file in files {
            if !seed_set.contains(&file) && is_code_file(&file) {
                *counts.entry(file).or_default() += 1;
            }
        }
    }
    let mut counts = counts.into_iter().collect::<Vec<_>>();
    counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    counts.truncate(CO_CHANGE_CACHE_FILES);
    counts
}

/// 伴生测试文件：改实现通常要同步改断言了该实现的测试，但测试文件与任务文本
/// 零重叠且是反向 import 图的叶子，常规检索必然漏召。git 共改 ≥2 次的测试文件
/// 优先，同目录 `*.test.*` / `*.spec.*` / `*_test.*` 命名约定兜底。
fn companion_test_files(
    root: &Path,
    seed_files: &[String],
    exclude: &HashSet<String>,
    limit: usize,
) -> Vec<String> {
    if seed_files.is_empty() {
        return Vec::new();
    }
    let mut companions = Vec::new();
    // One batched history walk avoids paying the bounded git-log deadline once per seed in large
    // repositories. Keep extra candidates before the final cap so one seed cannot consume all
    // companion slots.
    let history_seeds = seed_files.iter().take(3).cloned().collect::<Vec<_>>();
    for (file, count) in co_changed_files(
        root,
        &history_seeds,
        exclude,
        limit.saturating_mul(history_seeds.len().max(1)),
        true,
    ) {
        if count >= 2 && !companions.contains(&file) {
            companions.push(file);
        }
    }
    for seed in seed_files.iter().take(4) {
        let path = Path::new(seed);
        let (Some(stem), Some(ext)) = (
            path.file_stem().and_then(|s| s.to_str()),
            path.extension().and_then(|s| s.to_str()),
        ) else {
            continue;
        };
        let dir = path
            .parent()
            .map(|d| d.to_string_lossy())
            .unwrap_or_default();
        let base = if dir.is_empty() || dir == "." {
            String::new()
        } else {
            format!("{dir}/")
        };
        for candidate in [
            format!("{base}{stem}.test.{ext}"),
            format!("{base}{stem}.spec.{ext}"),
            format!("{base}{stem}_test.{ext}"),
        ] {
            if !companions.contains(&candidate)
                && is_code_file(&candidate)
                && root.join(&candidate).is_file()
            {
                companions.push(candidate);
            }
        }
    }
    companions.truncate(limit);
    companions
}

/// 行数 ≤ FULL_FILE_MAX 才值得无命中 FULL 打包；大文件必须有关键词命中才保留。
fn file_is_small(root: &Path, file: &str) -> bool {
    std::fs::read_to_string(root.join(file))
        .map(|text| text.lines().count() <= FULL_FILE_MAX)
        .unwrap_or(false)
}

pub fn code_map(root: &Path, params: Value) -> Result<String, String> {
    let scope = normalize_rel(
        params
            .get("scope")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim(),
    );
    let (index, _, _) = build_index(root, None, 0, None);
    let mut files: Vec<_> = index
        .files
        .keys()
        .filter(|f| scope.is_empty() || f.starts_with(&scope))
        .cloned()
        .collect();
    files.sort();
    if files.is_empty() {
        return Ok(format!(
            "# CODEMAP\n无匹配文件{}",
            if scope.is_empty() {
                "".into()
            } else {
                format!(": {scope}")
            }
        ));
    }
    let name = root
        .file_name()
        .and_then(|v| v.to_str())
        .unwrap_or("workspace");
    let (branch, revision) = git_head(root);
    let mut out = vec![format!(
        "# CODEMAP {name} @{branch} {revision}  {} files  cache: {}",
        files.len(),
        cache_location_label(root)
    )];
    if scope.is_empty() {
        out.push("# 行数 符号数 文件  (要看符号大纲请带 scope=<目录前缀>)".into());
        for f in files {
            let e = &index.files[&f];
            let n = e.syms.iter().filter(|s| s.depth == 0).count();
            out.push(format!("{:>6} {:>4}  {f}", e.total, n));
        }
    } else {
        out.push(format!("# scope={scope}  (每个文件: 行数 + 顶层符号)"));
        for f in files {
            let e = &index.files[&f];
            out.push(format!("## {f} ({}L)", e.total));
            let syms: Vec<_> = e
                .syms
                .iter()
                .filter(|s| s.depth == 0)
                .map(|s| {
                    format!(
                        "{} {}{}",
                        s.ln,
                        if s.kind == "impl" { "impl " } else { "" },
                        s.name
                    )
                })
                .collect();
            if !syms.is_empty() {
                out.push(syms.join(" | "));
            }
        }
    }
    Ok(out.join("\n"))
}

/// 回填辅助：把块补回已有 plan 或为其新建 plan（源从 candidates/索引按需读）。
/// 返回 Some(是否新建了 plan)；重复/整文件已展示/无源时返回 None。
fn backfill_block(
    root: &Path,
    index: &IndexView,
    sources: &HashMap<String, Source>,
    plans: &mut Vec<PlannedFile>,
    sigs: &mut Vec<(String, usize, String)>,
    file: &str,
    block: &Block,
    rank: usize,
) -> Option<bool> {
    if let Some(position) = plans
        .iter()
        .position(|plan| plan.file == file && !plan.full)
    {
        if plans[position]
            .blocks
            .iter()
            .any(|existing| existing.start == block.start && existing.end == block.end)
        {
            return None;
        }
        plans[position].blocks.push(block.clone());
        sigs.retain(|(sig_file, ln, _)| {
            !(sig_file == file && *ln >= block.start && *ln <= block.end)
        });
        return Some(false);
    }
    if plans.iter().any(|plan| plan.file == file) {
        return None;
    }
    let src = sources
        .get(file)
        .cloned()
        .or_else(|| source(root, file, index.files.get(file)))?;
    plans.push(PlannedFile {
        file: file.to_string(),
        source: src,
        section: if matches!(block.tag, "dep" | "dep2") {
            "dep"
        } else {
            "edit"
        },
        full: false,
        blocks: vec![block.clone()],
        rank,
    });
    sigs.retain(|(sig_file, ln, _)| !(sig_file == file && *ln >= block.start && *ln <= block.end));
    Some(true)
}

pub fn fast_context(root: &Path, params: Value) -> Result<String, String> {
    polaris(root, params)
}

pub fn polaris(root: &Path, params: Value) -> Result<String, String> {
    let out = fast_context_run(root, &params)?;
    if params
        .get("_anchorRetry")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Ok(out);
    }
    // 特性② typo 自动更正：硬 MISS 且 did-you-mean 里有编辑距离足够小的真实符号时，
    // 视为锚点 typo，自动替换重试一次；重试仍 MISS 则返回原始 MISS（附建议）。
    let Some((anchor, suggestion)) = anchor_correction(&out, &params) else {
        return Ok(out);
    };
    let mut retry = params.clone();
    if let Some(values) = retry.get_mut("keywords").and_then(Value::as_array_mut) {
        for value in values.iter_mut() {
            if value.as_str() == Some(anchor.as_str()) {
                *value = Value::String(suggestion.clone());
            }
        }
    }
    if let Some(object) = retry.as_object_mut() {
        object.insert("_anchorRetry".into(), Value::Bool(true));
    }
    let retried = fast_context_run(root, &retry)?;
    if retried.starts_with("# CTX MISS") {
        return Ok(out);
    }
    Ok(format!(
        "# 锚点更正: {anchor} → {suggestion} (自动降级重试, 原锚点无生产命中)\n{retried}"
    ))
}

/// 硬 MISS 输出里的 did-you-mean 与显式锚点做编辑距离匹配：够像就视为 typo，
/// 返回 (原锚点, 建议符号) 供自动更正重试。
fn anchor_correction(out: &str, params: &Value) -> Option<(String, String)> {
    if !out.starts_with("# CTX MISS") {
        return None;
    }
    let line = out
        .lines()
        .find(|line| line.starts_with("did-you-mean: "))?;
    let suggestions = line["did-you-mean: ".len()..]
        .split(", ")
        .filter_map(|item| item.split_whitespace().next())
        .map(str::to_string)
        .collect::<Vec<_>>();
    let anchors = params
        .get("keywords")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|keyword| explicit_anchor(keyword))
        .collect::<Vec<_>>();
    for anchor in &anchors {
        for suggestion in &suggestions {
            if similar_enough(anchor, suggestion) {
                return Some((anchor.to_string(), suggestion.clone()));
            }
        }
    }
    None
}

/// 归一化（小写字母数字）后编辑距离 ≤1 视为 typo；距离 2 仅接受更长符号
///（避免锚点被相差 2 的远亲符号劫持，错纠正反而制造幻觉锚点）。
fn similar_enough(anchor: &str, suggestion: &str) -> bool {
    let normalize = |value: &str| {
        value
            .chars()
            .filter(|ch| ch.is_ascii_alphanumeric())
            .map(|ch| ch.to_ascii_lowercase())
            .collect::<String>()
    };
    let a = normalize(anchor);
    let b = normalize(suggestion);
    if a.is_empty() || b.is_empty() {
        return false;
    }
    let distance = levenshtein(&a, &b, 3);
    distance == 1 || (distance == 2 && a.chars().count().max(b.chars().count()) >= 12)
}

/// 带上限的 Levenshtein：任一行最小值超 cap 即提前返回 cap+1。
fn levenshtein(a: &str, b: &str, cap: usize) -> usize {
    let a = a.chars().collect::<Vec<_>>();
    let b = b.chars().collect::<Vec<_>>();
    if a.len().abs_diff(b.len()) > cap {
        return cap + 1;
    }
    let mut prev = (0..=b.len()).collect::<Vec<_>>();
    for (i, ca) in a.iter().enumerate() {
        let mut row = vec![i + 1; b.len() + 1];
        let mut min = row[0];
        for (j, cb) in b.iter().enumerate() {
            row[j + 1] = (prev[j] + usize::from(ca != cb))
                .min(prev[j + 1] + 1)
                .min(row[j] + 1);
            min = min.min(row[j + 1]);
        }
        if min > cap {
            return cap + 1;
        }
        prev = row;
    }
    prev[b.len()]
}

fn fast_context_run(root: &Path, params: &Value) -> Result<String, String> {
    let mut keyword_seen = HashSet::new();
    let keywords: Vec<String> = params
        .get("keywords")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty() && keyword_seen.insert((*value).to_string()))
        .take(5)
        .map(str::to_string)
        .collect();
    let task = params
        .get("task")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .chars()
        .take(300)
        .collect::<String>();
    let task_terms = task_tokens(&task)
        .into_iter()
        .filter(|token| {
            !keywords
                .iter()
                .any(|value| value.eq_ignore_ascii_case(token))
        })
        .collect::<Vec<_>>();
    let mut anchor_terms = Vec::new();
    for keyword in &keywords {
        // 短语关键词（含空白）本身不是可检索锚点：拆成单词参与检索与命中判定，
        // 避免 "plan mode" 这类自然语言短语整体零命中把真实泛词也拖进 MISS。
        let words = phrase_words(keyword);
        let variants = if words.is_empty() {
            naming_variants(keyword)
        } else {
            words
                .iter()
                .flat_map(|word| naming_variants(word))
                .collect::<Vec<_>>()
        };
        for variant in variants {
            if !anchor_terms
                .iter()
                .any(|value: &String| value.eq_ignore_ascii_case(&variant))
            {
                anchor_terms.push(variant);
            }
        }
    }
    let mut terms = anchor_terms.clone();
    for token in &task_terms {
        if !terms.iter().any(|value| value.eq_ignore_ascii_case(token)) {
            terms.push(token.clone());
        }
    }
    let explicit_anchors = keywords
        .iter()
        .filter(|keyword| explicit_anchor(keyword))
        .cloned()
        .collect::<Vec<_>>();
    let initial_terms = if explicit_anchors.is_empty() {
        &terms
    } else {
        &anchor_terms
    };
    let plan_intent = retrieval_plan(&task);
    let mut file_seen = HashSet::new();
    let files: Vec<String> = params
        .get("files")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(|value| normalize_rel(value.trim()))
        .filter(|value| !value.is_empty() && file_seen.insert(value.clone()))
        .take(6)
        .collect();
    if terms.is_empty() && files.is_empty() {
        return Ok("错误: 需要 keywords / task / files 至少其一".into());
    }
    let budget = params
        .get("budget")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_BUDGET as u64) as usize;
    let budget = budget.clamp(MIN_BUDGET, MAX_BUDGET);
    let hard = params
        .get("maxBytes")
        .or_else(|| params.get("maxChars"))
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_HARD_BYTES as u64) as usize;
    let hard = hard.clamp(MIN_HARD_BYTES, MAX_HARD_BYTES);
    let soft_bytes = hard * 64 / 100;
    let total_start = Instant::now();
    let stage = Instant::now();
    let (all, rows, revision) = std::thread::scope(|scope| {
        let files = scope.spawn(|| list_code_files(root));
        let search = scope.spawn(|| {
            if initial_terms.is_empty() {
                Vec::new()
            } else {
                search_text_until(root, initial_terms, true, false, &[], search_deadline())
            }
        });
        let revision = scope.spawn(|| short_rev(root));
        (
            files.join().unwrap_or_default(),
            search.join().unwrap_or_default(),
            revision.join().unwrap_or_else(|_| "unknown".into()),
        )
    });
    let mut all = (*all).clone();
    trace("fast_context.search_and_files", stage);
    let resolved_anchors = explicit_anchors
        .iter()
        .filter(|keyword| production_anchor_hit(&rows, keyword))
        .count();
    // 软降级：显式锚点全灭但其它关键词（如短语拆出的泛词）命中生产代码时继续检索，
    // 未命中锚点由头部"未命中关键词"标注；只有全部关键词零命中才硬 MISS（附 did-you-mean）。
    if !explicit_anchors.is_empty()
        && resolved_anchors == 0
        && files.is_empty()
        && !keywords.iter().any(|keyword| keyword_hit(&rows, keyword))
    {
        return Ok(compact_evidence_miss(
            &revision,
            &explicit_anchors,
            &task,
            &suggest_symbols(root, &explicit_anchors),
        ));
    }
    let loose_kw = keywords
        .iter()
        .filter(|keyword| {
            production_anchor_hit(&rows, keyword)
                && !rows
                    .iter()
                    .any(|row| !anchor_noise_path(&row.file) && row.text.contains(keyword.as_str()))
        })
        .cloned()
        .collect::<Vec<_>>();
    // 预先记录哪些关键词（或短语拆出的单词）命中生产代码，供 ingest 之后判定 missed_all。
    let production_hits: HashSet<String> = keywords
        .iter()
        .flat_map(|keyword| {
            let words = phrase_words(keyword);
            if words.is_empty() {
                vec![keyword.clone()]
            } else {
                words
            }
        })
        .filter(|term| production_anchor_hit(&rows, term))
        .collect();
    let mut all_set: HashSet<String> = all.iter().cloned().collect();
    for file in rows.iter().map(|row| &row.file).chain(files.iter()) {
        if all_set.insert(file.clone())
            && root.join(file).is_file()
            && is_searchable_implementation_file(file)
        {
            all.push(file.clone());
        }
    }
    let mut hit_files: HashMap<String, Vec<SearchRow>> = HashMap::new();
    let mut file_keywords = HashMap::<String, HashSet<String>>::new();
    let mut line_keywords = HashMap::<(String, usize), HashSet<String>>::new();
    let mut keyword_counts = HashMap::<String, usize>::new();
    let mut hit_order = Vec::new();
    for row in rows {
        if !hit_files.contains_key(&row.file) {
            hit_order.push(row.file.clone());
        }
        let lower = row.text.to_lowercase();
        for term in &terms {
            if row.text.contains(term) || lower.contains(&term.to_lowercase()) {
                file_keywords
                    .entry(row.file.clone())
                    .or_default()
                    .insert(term.clone());
                line_keywords
                    .entry((row.file.clone(), row.ln))
                    .or_default()
                    .insert(term.clone());
                *keyword_counts.entry(term.clone()).or_default() += 1;
            }
        }
        let file_rows = hit_files.entry(row.file.clone()).or_default();
        if file_rows.iter().any(|existing| existing.ln == row.ln)
            || file_rows.len() >= MAX_HITS_PER_FILE
        {
            continue;
        }
        file_rows.push(row);
    }
    // 短语关键词按拆出的单词判定命中；非锚点关键词看是否出现在命中行。
    let missed_all = keywords
        .iter()
        .filter(|keyword| {
            let words = phrase_words(keyword);
            if !words.is_empty() {
                return !words.iter().any(|word| {
                    production_hits.contains(word) || keyword_counts.contains_key(word)
                });
            }
            if explicit_anchor(keyword) {
                !production_hits.contains(*keyword)
            } else {
                !keyword_counts.contains_key(*keyword)
            }
        })
        .cloned()
        .collect::<Vec<_>>();
    let subject_source = if explicit_anchors.is_empty() {
        &terms
    } else {
        &anchor_terms
    };
    let mut subject_terms = Vec::<String>::new();
    for term in subject_source {
        let low = term.to_lowercase();
        if js_utf16_len(&low) >= 4
            && low
                .chars()
                .all(|ch| ch.is_alphanumeric() || matches!(ch, '_' | '$'))
            && !subject_terms.contains(&low)
        {
            subject_terms.push(low);
        }
    }
    let term_freq = term_freq_map(&keyword_counts);
    static DEFINITION_LINE: OnceLock<Regex> = OnceLock::new();
    let definition_line = DEFINITION_LINE.get_or_init(|| Regex::new(r"^\s*(?:(?:pub(?:\([^)]*\))?|export|async|unsafe|default|static|const|move)\s+)*(?:fn|struct|enum|trait|impl|type|class|interface|function|def|mod)\b").unwrap());
    let mut preliminary: Vec<(String, i64)> = hit_order
        .iter()
        .filter_map(|f| hit_files.get(f).map(|rows| (f, rows)))
        .map(|(f, r)| {
            (
                f.clone(),
                score_path(f)
                    + r.len().min(8) as i64 * 4
                    // 超过 8 行的命中每 4 行再记 1 分（封顶 60 行）：密集命中是"本文件就在讲这个主题"
                    // 的直接证据，不应与零星命中同分（命中上百行的引擎文件原来与 8 命中文件打平被挤出候选）。
                    + r.len().saturating_sub(8).min(52) as i64 / 4
                    + file_keywords
                        .get(f)
                        .map(|set| {
                            set.iter()
                                .map(|term| {
                                    let count = *keyword_counts.get(term).unwrap_or(&1);
                                    let weight = if count <= 40 {
                                        1.0
                                    } else if count <= 200 {
                                        0.6
                                    } else {
                                        0.25
                                    };
                                    (30.0
                                        * weight
                                        * if keywords.contains(term) { 1.0 } else { 0.5 })
                                        as i64
                                })
                                .sum::<i64>()
                        })
                        .unwrap_or(0)
                    + if r.iter().any(|row| definition_line.is_match(&row.text)) {
                        120
                    } else {
                        0
                    }
                    + subject_match(f, &subject_terms, &term_freq) as i64
                    + if files.contains(f) { 500 } else { 0 },
            )
        })
        .collect();
    // 文件名段命中查询词是最强的定位信号，泛词高频时不应被衰减门槛挡住（命名为 *Plan*
    // 的目标文件在 plan 命中数高时会被转录噪声文件挤出候选）。实现路径上的 subject 文件
    // 直接提为候选；score_path>0 排除 docs/build 等非生产路径。
    for f in files.iter().chain(all.iter().filter(|file| {
        subject_match(file, &subject_terms, &term_freq) >= 300.0
            || (score_path(file) > 0 && is_subject_file(file, &subject_terms))
    })) {
        if !preliminary.iter().any(|(x, _)| x == f) {
            preliminary.push((f.clone(), if files.contains(f) { 1000 } else { 550 }));
        }
    }
    // 同分优先生产路径（src/ > scripts/ > docs/）：原字母序兜底在泛词同分时把
    // scripts/docs 噪声排在 src/ 目标之前。
    preliminary.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then_with(|| score_path(&b.0).cmp(&score_path(&a.0)))
            .then_with(|| a.0.cmp(&b.0))
    });
    preliminary.dedup_by(|a, b| a.0 == b.0);
    if preliminary.is_empty() {
        return Ok(format!(
            "# CTX @{}\n无命中: {}\n提示: 换更短的符号名/字符串片段，或用 grep 定位后用 read。",
            short_rev(root),
            terms.join(" ")
        ));
    }
    let mut candidates = preliminary
        .iter()
        .filter(|(file, _)| is_searchable_implementation_file(file) || files.contains(file))
        .take(MAX_CANDIDATES)
        .cloned()
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        candidates = preliminary.iter().take(3).cloned().collect();
    }
    let mut wanted: HashSet<_> = candidates.iter().map(|value| value.0.clone()).collect();
    if !subject_terms.is_empty() {
        for file in &all {
            if is_subject_file(file, &subject_terms) {
                wanted.insert(file.clone());
            }
        }
    }
    let stage = Instant::now();
    let (index, _, reverse) = build_index(root, Some(&wanted), 3, Some(all.as_slice()));
    trace("fast_context.index", stage);
    let stage = Instant::now();
    let mut def_names = index.defs.keys().cloned().collect::<Vec<_>>();
    def_names.sort();
    let mut seeds = Vec::<(Definition, String, usize)>::new();
    let mut seed_positions = HashMap::<(String, usize), usize>::new();
    // task-only 调用也应建立目标定义。只纳入有限数量的 ASCII 标识符，避免中文
    // n-gram 和自然语言词把 defs 全表匹配放大成二次扫描。
    let mut seed_terms = anchor_terms.clone();
    if explicit_anchors.is_empty() {
        for term in task_terms.iter().filter(|term| {
            term.len() >= 3
                && term
                    .chars()
                    .next()
                    .is_some_and(|ch| ch.is_ascii_alphabetic() || matches!(ch, '_' | '$'))
                && term
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '$'))
        }) {
            if seed_terms.len() >= anchor_terms.len() + MAX_GRAPH_TERMS {
                break;
            }
            if !seed_terms
                .iter()
                .any(|value| value.eq_ignore_ascii_case(term))
            {
                seed_terms.push(term.clone());
            }
        }
    }
    // 高频泛词（如 build/mode 命中数百行）不作为种子：其"定义"多半是无关同名函数，
    // 会经由 plannedTerms 二次检索把 build 脚本等噪声反馈回排名。
    seed_terms
        .retain(|term| term_freq.get(&term.to_lowercase()).copied().unwrap_or(0) <= SEED_FREQ_CAP);
    for keyword in &seed_terms {
        let lower = keyword.to_lowercase();
        for name in &def_names {
            let weight = if name == keyword {
                3
            } else if name.to_lowercase() == lower {
                2
            } else if keyword.len() >= 5 && name.to_lowercase().contains(&lower) {
                1
            } else {
                0
            };
            if weight == 0 {
                continue;
            }
            for definition in index.defs.get(name).into_iter().flatten() {
                let key = (definition.file.clone(), definition.symbol.ln);
                if let Some(position) = seed_positions.get(&key).copied() {
                    if seeds[position].2 < weight {
                        seeds[position] = (definition.clone(), name.clone(), weight);
                    }
                } else {
                    seed_positions.insert(key, seeds.len());
                    seeds.push((definition.clone(), name.clone(), weight));
                }
            }
        }
    }
    // 计划驱动二次检索：先看目标定义体，再搜索错误/配置/状态符号的处理方。
    let seed_bodies = seeds
        .iter()
        .filter_map(|(definition, _, _)| {
            source(root, &definition.file, index.files.get(&definition.file)).map(|src| {
                (
                    definition.file.clone(),
                    src.lines[definition.symbol.ln - 1..definition.symbol.end].join("\n"),
                )
            })
        })
        .collect::<Vec<_>>();
    let planned_terms = plan_terms_from_bodies(&plan_intent, &index, &seed_bodies, &terms);
    // 反向图 discover 兜底的词根预取：把所有种子目标的文件名词根合并成一次批量
    // 检索（原先每个种子目标单独全仓搜一次，串行）。
    let mut discover_stems = Vec::<String>::new();
    {
        let mut seen_targets = HashSet::<String>::new();
        for (definition, _, _) in &seeds {
            if !seen_targets.insert(definition.file.clone()) {
                continue;
            }
            let base = definition
                .file
                .rsplit('/')
                .next()
                .unwrap_or(&definition.file);
            let stem = base
                .rsplit_once('.')
                .map(|(stem, _)| stem)
                .unwrap_or(base)
                .to_string();
            if stem.len() >= 3
                && !discover_stems
                    .iter()
                    .any(|value| value.eq_ignore_ascii_case(&stem))
            {
                discover_stems.push(stem);
            }
        }
    }
    let seed_body_files = seed_bodies
        .iter()
        .map(|(file, _)| file.clone())
        .collect::<Vec<_>>();
    let planned_scope = scope_dirs(&seed_body_files);
    // 计划驱动的二次检索与反向图词根检索互相独立，并行执行。
    let search_stage = Instant::now();
    let plan_deadline = search_deadline();
    let (planned_rows, discover_rows) = std::thread::scope(|scope| {
        let planned = scope.spawn(|| {
            if planned_terms.is_empty() {
                Vec::<SearchRow>::new()
            } else {
                search_text_scopes_until(
                    root,
                    &planned_terms,
                    false,
                    false,
                    planned_scope.as_deref(),
                    plan_deadline,
                )
            }
        });
        let stems = scope.spawn(|| {
            if discover_stems.is_empty() {
                Vec::<SearchRow>::new()
            } else {
                search_text_scopes_until(root, &discover_stems, true, false, None, plan_deadline)
            }
        });
        (
            planned.join().unwrap_or_default(),
            stems.join().unwrap_or_default(),
        )
    });
    trace("fast_context.plan_search", search_stage);
    if !planned_terms.is_empty() {
        terms.extend(planned_terms.iter().cloned());
        for row in planned_rows {
            if !hit_files.contains_key(&row.file) {
                hit_order.push(row.file.clone());
            }
            let lower = row.text.to_lowercase();
            for term in &planned_terms {
                if row.text.contains(term) || lower.contains(&term.to_lowercase()) {
                    file_keywords
                        .entry(row.file.clone())
                        .or_default()
                        .insert(term.clone());
                    line_keywords
                        .entry((row.file.clone(), row.ln))
                        .or_default()
                        .insert(term.clone());
                    *keyword_counts.entry(term.clone()).or_default() += 1;
                }
            }
            let file_rows = hit_files.entry(row.file.clone()).or_default();
            if !file_rows.iter().any(|existing| existing.ln == row.ln)
                && file_rows.len() < MAX_HITS_PER_FILE
            {
                file_rows.push(row);
            }
        }
    }
    // 二次检索后词频已变，重建稀有度表供 ranked 的主题加分使用。
    let term_freq = term_freq_map(&keyword_counts);

    // 批量词根检索结果按词根分组（词匹配大小写不敏感）：discover 优先从这里
    // 取行，只有动态发现的中转目标才退回单独检索。
    let stem_lowers = discover_stems
        .iter()
        .map(|stem| stem.to_lowercase())
        .collect::<Vec<_>>();
    let mut stem_rows = HashMap::<String, Vec<SearchRow>>::new();
    for row in discover_rows {
        let lower = row.text.to_lowercase();
        for (index, stem) in discover_stems.iter().enumerate() {
            if lower.contains(&stem_lowers[index]) {
                stem_rows.entry(stem.clone()).or_default().push(row.clone());
            }
        }
    }

    // 特性① 反向 import 图：精确调用方发现。文本检索看不见别名调用点
    //（import { a as b } 之后调用 b(...)）和 barrel 改名透传（export { a as c }），
    // import 边把这些调用行作为精确命中注入，角色/IMPACT 打 [import图] 标记。
    // 深度 2 仅经“未本地定义种子名且 import 了种子名”的中转文件（re-export），
    // 避免把无关同名引用当成调用方。缓存条目可能陈旧：读当前文件验证 import 行。
    let mut graph_rows = HashSet::<(String, usize)>::new();
    let mut exact_callers = HashMap::<String, HashSet<String>>::new();
    // 文本检索看不见的调用方（别名调用 / barrel 透传）：子模覆盖时按文件独立成类，
    // 不被同行为类别的普通调用方代表挤掉；普通 import 调用方仍按行为类别去重。
    let mut invisible_callers = HashSet::<String>::new();
    const MAX_GRAPH_IMPORTERS: usize = 12;
    const MAX_GRAPH_ROWS_PER_FILE: usize = 6;
    if !seeds.is_empty() {
        let all_paths = all.iter().cloned().collect::<HashSet<_>>();
        let mut searched_targets = HashSet::<String>::new();
        // 发现 target 的引用方：反向图（增量缓存）+ 词根搜索兜底（冷启动时
        // 未扫描过的引用方不在缓存里，用文件名词根搜 import 行再精确解析）。
        let discover = |target: &str,
                        searched: &mut HashSet<String>,
                        reverse: &ReverseMap|
         -> Vec<ReverseImport> {
            let mut edges = reverse.get(target).cloned().unwrap_or_default();
            let base = target.rsplit('/').next().unwrap_or(target);
            let stem = base
                .rsplit_once('.')
                .map(|(stem, _)| stem)
                .unwrap_or(base)
                .to_string();
            if stem.len() >= 3 && searched.insert(target.to_string()) {
                // 种子目标的词根行来自预取的批量检索；动态发现的中转目标才单独补搜。
                let rows = stem_rows.get(&stem).cloned().unwrap_or_else(|| {
                    search_text_scopes_until(
                        root,
                        &[stem.clone()],
                        true,
                        false,
                        None,
                        search_deadline(),
                    )
                });
                let mut seen_files = HashSet::<String>::new();
                for row in rows {
                    if row.file == target || !is_code_file(&row.file) {
                        continue;
                    }
                    let line = &row.text;
                    if !(line.contains("import")
                        || line.contains("use ")
                        || line.contains("export"))
                    {
                        continue;
                    }
                    // 词根必须出现在模块说明符位置，才值得整文件解析 import 表
                    let specifier_like = line.contains(&format!("'{stem}"))
                        || line.contains(&format!("\"{stem}"))
                        || line.contains(&format!("/{stem}"))
                        || line.contains(&format!("::{stem}"))
                        || line.contains(&format!("/{stem}."));
                    if !specifier_like || !seen_files.insert(row.file.clone()) {
                        continue;
                    }
                    let Ok(text) = fs::read_to_string(root.join(&row.file)) else {
                        continue;
                    };
                    for import in extract_imports(&text, &row.file) {
                        if resolve_specifier(&import.from, &row.file, &all_paths).as_deref()
                            == Some(target)
                        {
                            edges.push(ReverseImport {
                                importer: row.file.clone(),
                                local: import.name,
                                orig: import.orig,
                            });
                        }
                    }
                }
            }
            edges.sort_by(|a, b| a.importer.cmp(&b.importer).then(a.local.cmp(&b.local)));
            edges.dedup_by(|a, b| a.importer == b.importer && a.local == b.local);
            edges
        };
        // (种子名, 中转文件, 引用方, 引用方本地名, 深度)
        let mut graph_queue = Vec::<(String, String, String, String, usize)>::new();
        let mut graph_seen = HashSet::<(String, String)>::new();
        for (definition, name, _) in &seeds {
            for edge in discover(&definition.file, &mut searched_targets, reverse.as_ref()) {
                if edge.local == *name || edge.orig.as_deref() == Some(name.as_str()) {
                    graph_queue.push((
                        name.clone(),
                        definition.file.clone(),
                        edge.importer.clone(),
                        edge.local.clone(),
                        0,
                    ));
                }
            }
        }
        let mut cursor = 0usize;
        while cursor < graph_queue.len() {
            let (seed_name, via, importer, local, depth) = graph_queue[cursor].clone();
            cursor += 1;
            if importer == via || !graph_seen.insert((importer.clone(), local.clone())) {
                continue;
            }
            if !exact_callers.contains_key(&importer) && exact_callers.len() >= MAX_GRAPH_IMPORTERS
            {
                continue;
            }
            if !is_code_file(&importer) || !root.join(&importer).is_file() {
                continue;
            }
            let Some(src) = source(root, &importer, index.files.get(&importer)) else {
                continue;
            };
            // 复核：当前文件必须仍存在提及 local 名的 import/use/export 行（剔除陈旧缓存）。
            let has_import_line = src.lines.iter().any(|line| {
                (line.contains("import") || line.contains("use ") || line.contains("export"))
                    && contains_ascii_word(line, &local)
            });
            if !has_import_line {
                continue;
            }
            // 深度 2：经由未本地定义种子名的中转文件（re-export barrel）继续追踪。
            if depth == 0
                && !src.syms.iter().any(|symbol| {
                    symbol.name == seed_name && symbol.depth <= 1 && symbol.kind != "prop"
                })
            {
                for edge in discover(&importer, &mut searched_targets, reverse.as_ref()) {
                    if edge.local == local || edge.orig.as_deref() == Some(local.as_str()) {
                        graph_queue.push((
                            seed_name.clone(),
                            importer.clone(),
                            edge.importer.clone(),
                            edge.local.clone(),
                            1,
                        ));
                    }
                }
            }
            if depth > 0 || local != seed_name {
                invisible_callers.insert(importer.clone());
            }
            exact_callers
                .entry(importer.clone())
                .or_default()
                .insert(local.clone());
            let mut added = 0usize;
            for (row_index, line) in src.lines.iter().enumerate() {
                if added >= MAX_GRAPH_ROWS_PER_FILE {
                    break;
                }
                if !contains_ascii_word(line, &local) {
                    continue;
                }
                let ln = row_index + 1;
                if !hit_files.contains_key(&importer) {
                    hit_order.push(importer.clone());
                }
                let file_rows = hit_files.entry(importer.clone()).or_default();
                if file_rows.iter().any(|row| row.ln == ln) || file_rows.len() >= MAX_HITS_PER_FILE
                {
                    continue;
                }
                file_rows.push(SearchRow {
                    file: importer.clone(),
                    ln,
                    text: line.clone(),
                });
                file_keywords
                    .entry(importer.clone())
                    .or_default()
                    .insert(seed_name.clone());
                line_keywords
                    .entry((importer.clone(), ln))
                    .or_default()
                    .insert(seed_name.clone());
                *keyword_counts.entry(seed_name.clone()).or_default() += 1;
                graph_rows.insert((importer.clone(), ln));
                added += 1;
            }
            if all_set.insert(importer.clone()) {
                all.push(importer.clone());
            }
        }
    }

    let seed_files = seeds
        .iter()
        .map(|(definition, _, _)| definition.file.clone())
        .collect::<HashSet<_>>();
    let seed_names = seeds
        .iter()
        .map(|(_, name, _)| name.clone())
        .collect::<HashSet<_>>();
    let mut ordered_seed_files = seed_files.iter().cloned().collect::<Vec<_>>();
    ordered_seed_files.sort();
    let mut ranked = hit_order
        .iter()
        .filter_map(|file| hit_files.get(file).map(|rows| (file, rows)))
        .map(|(file, rows)| {
            let mut score = score_path(file) as f64 + rows.len().min(8) as f64 * 4.0;
            if let Some(values) = file_keywords.get(file) {
                for term in values {
                    let count = *keyword_counts.get(term).unwrap_or(&1);
                    let weight = if count <= 40 {
                        1.0
                    } else if count <= 200 {
                        0.6
                    } else {
                        0.25
                    };
                    score += 30.0 * weight * if keywords.contains(term) { 1.0 } else { 0.5 };
                }
            }
            if seed_files.contains(file) {
                score += 120.0;
            }
            score += subject_match(file, &subject_terms, &term_freq);
            if files.contains(file) {
                score += 500.0;
            }
            if exact_callers.contains_key(file) {
                score += if plan_intent.callers { 170.0 } else { 70.0 };
            }
            let references_seed = rows
                .iter()
                .any(|row| seed_names.iter().any(|name| row.text.contains(name)));
            let calls_seed = rows.iter().any(|row| {
                seed_names
                    .iter()
                    .any(|name| row.text.contains(&format!("{name}(")))
            });
            if plan_intent.callers && calls_seed {
                score += 180.0;
            }
            if plan_intent.tests && references_seed && noise_path(file) {
                score += 220.0;
            }
            if rows
                .iter()
                .any(|row| planned_terms.iter().any(|term| row.text.contains(term)))
            {
                score += 140.0;
            }
            (file.clone(), score)
        })
        .collect::<Vec<_>>();
    for file in files.iter().rev() {
        if !ranked.iter().any(|(existing, _)| existing == file) {
            ranked.insert(0, (file.clone(), 1000.0));
        }
    }
    for file in &ordered_seed_files {
        if !ranked.iter().any(|(existing, _)| existing == file) {
            ranked.push((file.clone(), 100.0));
        }
    }
    // 仅限名字有区分度（加分≥300）的文件：泛词同名且正文零命中的文件纯属噪声。
    for file in &all {
        if subject_match(file, &subject_terms, &term_freq) >= 300.0
            && !ranked.iter().any(|(existing, _)| existing == file)
        {
            ranked.push((file.clone(), 550.0));
        }
    }
    // 伴生测试：默认把与精确种子共改（或同目录 *.test.* 命名）的测试文件并入闭包。
    // 改实现通常要同步改测试，而测试文件被 noise_path 过滤且任务文本零重叠。
    // 仅限全等匹配种子（weight≥2）：contains 弱匹配种子经由 git 共改会把与查询
    // 零重叠的测试文件顶进 EDIT 区，反而挤掉真正的目标文件。
    let mut ordered_strong_seed_files = seeds
        .iter()
        .filter(|(_, _, weight)| *weight >= 2)
        .map(|(definition, _, _)| definition.file.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    ordered_strong_seed_files.sort();
    let companion_tests = if ordered_strong_seed_files.is_empty() {
        Vec::new()
    } else {
        // 只排除非测试文件：已进 ranked 的测试文件仍会被 noise 过滤丢弃，
        // 必须允许它们经由伴生通道重新进入闭包；重复 push 由下方去重拦截。
        let exclude = ranked
            .iter()
            .filter(|(file, _)| !noise_path(file))
            .map(|(file, _)| file.clone())
            .collect::<HashSet<_>>();
        // 零命中且超过 FULL 上限的伴生无法产出任何块，只会挤占候选槽位。
        companion_test_files(root, &ordered_strong_seed_files, &exclude, 4)
            .into_iter()
            .filter(|file| hit_files.contains_key(file) || file_is_small(root, file))
            .take(3)
            .collect::<Vec<_>>()
    };
    for file in &companion_tests {
        if let Some(existing) = ranked.iter_mut().find(|(existing, _)| existing == file) {
            existing.1 = existing.1.max(520.0);
        } else {
            ranked.push((file.clone(), 520.0));
        }
    }
    ranked.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    // 特性④ git 共改耦合（可选开关）：抓文本零耦合的关联文件（DI 注册表、路由表、
    // 配套样式）。只出提示行，不占 EDIT 预算。
    let coupling_files = if params
        .get("coupling")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        && !ordered_seed_files.is_empty()
    {
        let exclude = ranked
            .iter()
            .map(|(file, _)| file.clone())
            .collect::<HashSet<_>>();
        co_changed_files(root, &ordered_seed_files, &exclude, 3, false)
    } else {
        Vec::new()
    };
    let base_candidate_limit = MAX_CANDIDATES + hard.saturating_sub(DEFAULT_HARD_BYTES) / 8192 * 2;
    let candidate_limit = base_candidate_limit.max(MAX_CANDIDATES).min(12);
    let base_file_limit = MAX_FILES + hard.saturating_sub(DEFAULT_HARD_BYTES) / 16384 * 2;
    let file_limit = base_file_limit.max(MAX_FILES).min(8);
    let units_per_file =
        (MAX_UNITS_PER_FILE + hard.saturating_sub(DEFAULT_HARD_BYTES) / 16384).min(8);
    let mut final_candidates = ranked
        .iter()
        .filter(|(file, _)| {
            (is_searchable_implementation_file(file) || files.contains(file))
                && (plan_intent.tests
                    || !noise_path(file)
                    || files.contains(file)
                    || seed_files.contains(file)
                    || companion_tests.contains(file))
        })
        .take(candidate_limit)
        .cloned()
        .collect::<Vec<_>>();
    if final_candidates.is_empty() {
        final_candidates = ranked.iter().take(3).cloned().collect();
    }
    let mut sources = HashMap::<String, Source>::new();
    for (file, _) in &final_candidates {
        if let Some(source) = source(root, file, index.files.get(file)) {
            sources.insert(file.clone(), source);
        }
    }
    let file_rank = final_candidates
        .iter()
        .enumerate()
        .map(|(rank, (file, _))| (file.clone(), rank))
        .collect::<HashMap<_, _>>();
    let task_tokens = task_terms
        .iter()
        .map(|value| value.to_lowercase())
        .collect::<Vec<_>>();
    let mut units = Vec::<UnitCandidate>::new();
    for (file, file_score) in &final_candidates {
        if std::env::var_os("NOVA_CTX_DEBUG").is_some() {
            eprintln!(
                "[ctx] candidate {file} score={file_score:.1} hits={} src={}",
                hit_files.get(file).map(|rows| rows.len()).unwrap_or(0),
                sources.contains_key(file)
            );
        }
        let Some(source) = sources.get(file) else {
            continue;
        };
        let mut grouped = Vec::<(String, UnitCandidate)>::new();
        for hit in hit_files.get(file).into_iter().flatten() {
            if hit.ln > source.lines.len() {
                continue;
            }
            let (unit, chain) = unit_for_hit(&source.syms, hit.ln);
            let (key, start, end, label, owned_unit) = if let Some(symbol) = unit {
                (
                    format!("{}-{}", symbol.ln, symbol.end),
                    symbol.ln,
                    symbol.end,
                    unit_label(&chain, symbol),
                    Some(symbol.clone()),
                )
            } else {
                let start = hit.ln.saturating_sub(8).max(1);
                let end = (hit.ln + 8).min(source.lines.len());
                (format!("w{}", hit.ln / 20), start, end, String::new(), None)
            };
            let position = grouped.iter().position(|(existing, _)| existing == &key);
            let index = position.unwrap_or_else(|| {
                grouped.push((
                    key,
                    UnitCandidate {
                        file: file.clone(),
                        start,
                        end,
                        label,
                        tag: "hit",
                        score: 0.0,
                        hits: Vec::new(),
                        keywords: HashSet::new(),
                        unit: owned_unit,
                        seed_weight: 0,
                        role: "related",
                        required: false,
                        utility: 0.0,
                        body: String::new(),
                        estimated_bytes: 96,
                        obligation: None,
                    },
                ));
                grouped.len() - 1
            });
            grouped[index].1.hits.push(hit.ln);
            if let Some(values) = line_keywords.get(&(file.clone(), hit.ln)) {
                grouped[index].1.keywords.extend(values.iter().cloned());
            }
        }
        for (definition, _, weight) in &seeds {
            if definition.file != *file {
                continue;
            }
            let key = format!("{}-{}", definition.symbol.ln, definition.symbol.end);
            let position = grouped.iter().position(|(existing, _)| existing == &key);
            let index = position.unwrap_or_else(|| {
                let mut chain = source
                    .syms
                    .iter()
                    .filter(|symbol| {
                        symbol.ln <= definition.symbol.ln && symbol.end >= definition.symbol.end
                    })
                    .collect::<Vec<_>>();
                chain.sort_by_key(|symbol| (symbol.depth, symbol.ln));
                let unit = chain
                    .last()
                    .copied()
                    .cloned()
                    .unwrap_or_else(|| definition.symbol.clone());
                let label = unit_label(&chain, &unit);
                grouped.push((
                    key,
                    UnitCandidate {
                        file: file.clone(),
                        start: definition.symbol.ln,
                        end: definition.symbol.end,
                        label: if label.is_empty() {
                            format!("{} {}", definition.symbol.kind, definition.symbol.name)
                        } else {
                            label
                        },
                        tag: "hit",
                        score: 0.0,
                        hits: Vec::new(),
                        keywords: HashSet::new(),
                        unit: Some(unit),
                        seed_weight: 0,
                        role: "related",
                        required: false,
                        utility: 0.0,
                        body: String::new(),
                        estimated_bytes: 96,
                        obligation: None,
                    },
                ));
                grouped.len() - 1
            });
            grouped[index].1.tag = "def";
            grouped[index].1.seed_weight = grouped[index].1.seed_weight.max(*weight);
        }
        for (_, mut unit) in grouped {
            if std::env::var_os("NOVA_CTX_DEBUG").is_some() {
                eprintln!(
                    "[ctx] grouped {}:{}-{} hits={:?}",
                    unit.file, unit.start, unit.end, unit.hits
                );
            }
            let size = unit.end - unit.start + 1;
            let body = source.lines[unit.start - 1..unit.end].join("\n");
            let references_seed = seed_names.iter().any(|name| body.contains(name));
            let calls_seed = seed_names
                .iter()
                .any(|name| body.contains(&format!("{name}(")))
                || exact_callers.get(file).is_some_and(|locals| {
                    locals
                        .iter()
                        .any(|local| body.contains(&format!("{local}(")))
                });
            let planned_relation = planned_terms.iter().any(|name| body.contains(name));
            unit.role = if unit.tag == "def" {
                "target"
            } else if plan_intent.tests && noise_path(file) && references_seed {
                "test"
            } else if noise_path(file) && companion_tests.contains(file) {
                // 伴生测试：实现改动通常要同步改断言，按 test 类别参与打包。
                "test"
            } else if plan_intent.errors && planned_relation {
                "handler"
            } else if plan_intent.callers && calls_seed {
                "caller"
            } else {
                "related"
            };
            // caller/test 的 required 是行为类别义务，不是“所有引用都必须展开”；
            // 子模覆盖会保留每类代表，其余留在 IMPACT。
            unit.required = unit.role == "target" || (unit.role == "handler" && plan_intent.errors);
            let mut score = *file_score * 0.35;
            for keyword in &unit.keywords {
                let count = *keyword_counts.get(keyword).unwrap_or(&1);
                let weight = if count <= 40 {
                    1.0
                } else if count <= 200 {
                    0.6
                } else {
                    0.25
                };
                score += 45.0 * weight * if keywords.contains(keyword) { 1.0 } else { 0.5 };
            }
            score += unit.hits.len().min(6) as f64 * 8.0;
            if unit.tag == "def" {
                score += if unit.seed_weight >= 3 {
                    200.0
                } else if unit.seed_weight == 2 {
                    120.0
                } else {
                    30.0
                };
            }
            if unit.unit.as_ref().is_some_and(|symbol| {
                matches!(symbol.kind.as_str(), "fn" | "method" | "class" | "type")
            }) {
                score += 20.0;
            }
            if !task_tokens.is_empty() {
                let body_lower = body.to_lowercase();
                for token in &task_tokens {
                    if body_lower.contains(token) {
                        score += 15.0;
                    }
                }
            }
            if unit.required {
                score += 260.0;
            }
            if size > 150 {
                score -= (size - 150) as f64 / 8.0;
            }
            if *file_rank.get(file).unwrap_or(&9) < 2 {
                score += 12.0;
            }
            unit.score = score;
            unit.estimated_bytes = body.len().max(96);
            unit.utility = score / (unit.estimated_bytes as f64 / 1024.0).max(1.0);
            unit.body = body;
            units.push(unit);
        }
    }
    // 子模覆盖：按新增闭包义务/字节选择，不让同构调用方和重复文本命中垄断预算。
    let unit_features = |unit: &UnitCandidate| {
        let mut features = Vec::<(String, f64)>::new();
        if unit.role == "target" {
            features.push((format!("target:{}:{}", unit.file, unit.start), 120.0));
        } else if unit.role == "handler" {
            features.push(("handler".into(), 85.0));
        } else if unit.role == "test" {
            features.push(("test".into(), 70.0));
        } else if unit.role == "caller" {
            if invisible_callers.contains(&unit.file) {
                // 文本不可见的精确调用方按文件独立成类：它们有别名/透传证据，
                // 不应被同行为类别的普通调用方代表挤掉。
                features.push((format!("caller:exact:{}", unit.file), 70.0));
            } else {
                let behavior = if contains_ascii_word(&unit.body, "try")
                    || contains_ascii_word(&unit.body, "catch")
                {
                    "caller:error"
                } else if seed_names
                    .iter()
                    .any(|name| unit.body.contains(&format!("return await {name}(")))
                {
                    "caller:await"
                } else if seed_names
                    .iter()
                    .any(|name| unit.body.contains(&format!("return {name}(")))
                {
                    "caller:return"
                } else if unit.body.contains("await") {
                    "caller:await-consume"
                } else {
                    "caller:invoke"
                };
                features.push((behavior.into(), 65.0));
            }
        }
        for keyword in &unit.keywords {
            features.push((format!("term:{}", keyword.to_lowercase()), 12.0));
        }
        if unit.role == "related" {
            features.push((format!("related:{}", unit.file), 6.0));
        }
        features
    };
    let mut remaining = units;
    let mut units = Vec::<UnitCandidate>::new();
    let mut covered_features = HashSet::<String>::new();
    while !remaining.is_empty() {
        let mut best_index = 0usize;
        let mut best_value = f64::NEG_INFINITY;
        for (index, unit) in remaining.iter().enumerate() {
            let mut gain = if unit.required { 1000.0 } else { 0.0 };
            for (feature, weight) in unit_features(unit) {
                if !covered_features.contains(&feature) {
                    gain += weight;
                }
            }
            if noise_path(&unit.file) && unit.role != "test" {
                gain -= 50.0;
            }
            let value =
                gain / (unit.estimated_bytes as f64 / 1024.0).max(1.0) + unit.score / 1000.0;
            if value > best_value {
                best_value = value;
                best_index = index;
            }
        }
        let mut picked = remaining.remove(best_index);
        picked.obligation = if picked.role == "caller" {
            unit_features(&picked)
                .into_iter()
                .find_map(|(feature, _)| feature.starts_with("caller:").then_some(feature))
        } else if picked.role == "test" {
            if companion_tests.contains(&picked.file) {
                // 伴生测试按文件保底：每个伴生文件至少打包一个块，
                // 而不是整个 test 类别只留一个代表。
                Some(format!("test:{}", picked.file))
            } else {
                Some("test".into())
            }
        } else {
            None
        };
        for (feature, _) in unit_features(&picked) {
            covered_features.insert(feature);
        }
        units.push(picked);
    }
    let mut plans = Vec::<PlannedFile>::new();
    let mut sigs = Vec::<(String, usize, String)>::new();
    let mut deferred = Vec::<Deferred>::new();
    let mut dropped = Vec::<Dropped>::new();
    let mut used = 0usize;
    let mut used_bytes = 0usize;
    let push_sig = |sigs: &mut Vec<(String, usize, String)>, file: &str, line: usize, sig: &str| {
        if sig.len() >= 3
            && !sigs
                .iter()
                .any(|(existing, existing_line, _)| existing == file && *existing_line == line)
        {
            sigs.push((file.into(), line, sig.into()));
        }
    };
    for file in &files {
        let Some(source) = sources.get(file).cloned() else {
            continue;
        };
        if source.lines.len() > EXPLICIT_FULL_MAX {
            continue;
        }
        let cost = range_cost(&source, 1, source.lines.len());
        if used + source.lines.len() <= budget && used_bytes + cost <= soft_bytes {
            used += source.lines.len();
            used_bytes += cost;
            plans.push(PlannedFile {
                file: file.clone(),
                source,
                section: "edit",
                full: true,
                blocks: Vec::new(),
                rank: *file_rank.get(file).unwrap_or(&99),
            });
        }
    }
    // 伴生测试：小文件直接 FULL（断言分散，按块易漏）；大文件依赖命中的 unit 管线。
    for file in &companion_tests {
        let Some(source) = sources.get(file).cloned() else {
            continue;
        };
        if source.lines.len() > FULL_FILE_MAX
            || plans.iter().any(|plan| plan.file == *file && plan.full)
        {
            continue;
        }
        let cost = range_cost(&source, 1, source.lines.len());
        if used + source.lines.len() <= budget && used_bytes + cost <= soft_bytes {
            used += source.lines.len();
            used_bytes += cost;
            plans.push(PlannedFile {
                file: file.clone(),
                source,
                section: "edit",
                full: true,
                blocks: Vec::new(),
                rank: *file_rank.get(file).unwrap_or(&99),
            });
        }
    }
    // 已有明确 seed 时，先保证目标闭包；文件名主题扩展不能抢走目标、调用方和依赖预算。
    // 仅文件名命中、没有可解析 seed 时，保留原来的主题文件通读行为。
    let subject_list = final_candidates
        .iter()
        .map(|(file, _)| file)
        .filter(|file| {
            seeds.is_empty()
                && subject_match(file, &subject_terms, &term_freq) >= 300.0
                && !noise_path(file)
                && sources.contains_key(*file)
        })
        .cloned()
        .collect::<Vec<_>>();
    for file in &subject_list {
        let Some(source) = sources.get(file).cloned() else {
            continue;
        };
        if source.lines.len() > SUBJECT_FULL_MAX
            || plans.iter().any(|plan| plan.file == *file && plan.full)
        {
            continue;
        }
        let cost = range_cost(&source, 1, source.lines.len());
        if used + source.lines.len() <= budget && used_bytes + cost <= soft_bytes {
            used += source.lines.len();
            used_bytes += cost;
            plans.push(PlannedFile {
                file: file.clone(),
                source,
                section: "edit",
                full: true,
                blocks: Vec::new(),
                rank: *file_rank.get(file).unwrap_or(&99),
            });
        }
    }
    for file in &subject_list {
        if plans.iter().any(|plan| plan.file == *file && plan.full) {
            continue;
        }
        let Some(source) = sources.get(file).cloned() else {
            continue;
        };
        if !plans.iter().any(|plan| plan.file == *file) {
            plans.push(PlannedFile {
                file: file.clone(),
                source: source.clone(),
                section: "edit",
                full: false,
                blocks: Vec::new(),
                rank: *file_rank.get(file).unwrap_or(&99),
            });
        }
        let plan_index = plans.iter().position(|plan| plan.file == *file).unwrap();
        let hit_lines = hit_files
            .get(file)
            .map(|rows| rows.iter().map(|row| row.ln).collect::<Vec<_>>())
            .unwrap_or_default();
        let mut eligible = source
            .syms
            .iter()
            .filter(|symbol| {
                if symbol.depth > 1
                    || (symbol.kind == "const" && symbol.end == symbol.ln)
                    || matches!(
                        symbol.name.to_ascii_lowercase().as_str(),
                        "test" | "tests" | "spec"
                    )
                {
                    return false;
                }
                !source.syms.iter().any(|parent| {
                    parent.ln <= symbol.ln
                        && parent.end >= symbol.end
                        && parent.depth < symbol.depth
                        && matches!(
                            parent.name.to_ascii_lowercase().as_str(),
                            "test" | "tests" | "spec"
                        )
                })
            })
            .cloned()
            .collect::<Vec<_>>();
        eligible.sort_by_key(|symbol| {
            (
                !hit_lines
                    .iter()
                    .any(|line| *line >= symbol.ln && *line <= symbol.end),
                symbol.ln,
            )
        });
        for symbol in eligible {
            if plans[plan_index].blocks.len() >= MAX_SUBJECT_UNITS {
                break;
            }
            if plans[plan_index]
                .blocks
                .iter()
                .any(|block| symbol.ln >= block.start && symbol.end <= block.end)
            {
                continue;
            }
            let lines = symbol.end - symbol.ln + 1;
            let cost = range_cost(&source, symbol.ln, symbol.end.min(source.lines.len()));
            if used + lines > budget || used_bytes + cost > soft_bytes {
                if symbol.depth == 0 {
                    push_sig(&mut sigs, file, symbol.ln, &symbol.sig);
                }
                continue;
            }
            let mut chain = source
                .syms
                .iter()
                .filter(|parent| {
                    parent.ln <= symbol.ln
                        && parent.end >= symbol.end
                        && parent.depth < symbol.depth
                })
                .collect::<Vec<_>>();
            chain.sort_by_key(|parent| (parent.depth, parent.ln));
            chain.push(&symbol);
            plans[plan_index].blocks.push(Block {
                start: symbol.ln,
                end: symbol.end.min(source.lines.len()),
                label: unit_label(&chain, &symbol),
                tag: "hit",
                score: 0.0,
                required: false,
            });
            used += lines;
            used_bytes += cost;
        }
    }
    if std::env::var_os("NOVA_CTX_DEBUG").is_some() {
        eprintln!("[ctx] total units after greedy: {}", units.len());
    }
    let closure_roles = units
        .iter()
        .map(|unit| (unit.file.clone(), unit.start, unit.role))
        .collect::<Vec<_>>();
    // 行为覆盖只能在单元实际进入 plan 后提交。显式要求 caller/test 时，每个调用行为
    // 类别/测试类别的首个可装入代表获得 required 预算和结构上限豁免。
    let mut packed_obligations = HashSet::<String>::new();
    for unit in units {
        if unit
            .obligation
            .as_ref()
            .is_some_and(|value| packed_obligations.contains(value))
        {
            continue;
        }
        let Some(source) = sources.get(&unit.file).cloned() else {
            continue;
        };
        let plan_index = plans.iter().position(|plan| plan.file == unit.file);
        if plan_index.is_some_and(|index| plans[index].full) {
            if let Some(obligation) = unit.obligation {
                packed_obligations.insert(obligation);
            }
            continue;
        }
        let required_representative = unit.obligation.is_some()
            && ((unit.role == "caller" && plan_intent.callers)
                || (unit.role == "test"
                    && (plan_intent.tests || companion_tests.contains(&unit.file))));
        let effective_required = unit.required || required_representative;
        if std::env::var_os("NOVA_CTX_DEBUG").is_some() {
            eprintln!(
                "[ctx] unit {}:{}-{} role={} tag={} obl={:?} req={} plan_idx={:?} hits={:?} score={:.1}",
                unit.file, unit.start, unit.end, unit.role, unit.tag, unit.obligation,
                effective_required,
                plans.iter().position(|plan| plan.file == unit.file),
                unit.hits, unit.score
            );
        }
        if plan_index.is_none() && plans.len() >= file_limit && !required_representative {
            continue;
        }
        if plan_index.is_some_and(|index| plans[index].blocks.len() >= units_per_file)
            && !required_representative
        {
            continue;
        }
        if !unit.hits.is_empty()
            && unit
                .hits
                .iter()
                .all(|line| plan_index.is_some_and(|index| covered(&plans[index], *line)))
        {
            if let Some(obligation) = unit.obligation {
                packed_obligations.insert(obligation);
            }
            continue;
        }
        if plan_index.is_none()
            && source.lines.len() <= FULL_FILE_MAX
            && (*file_rank.get(&unit.file).unwrap_or(&9) < 3 || unit.tag == "def")
        {
            let cost = range_cost(&source, 1, source.lines.len());
            let required_bytes = soft_bytes.max(hard * 86 / 100);
            if (effective_required || used + source.lines.len() <= budget)
                && used_bytes + cost
                    <= if effective_required {
                        required_bytes
                    } else {
                        soft_bytes
                    }
            {
                used += source.lines.len();
                used_bytes += cost;
                let rank = *file_rank.get(&unit.file).unwrap_or(&99);
                if let Some(obligation) = unit.obligation {
                    packed_obligations.insert(obligation);
                }
                plans.push(PlannedFile {
                    file: unit.file,
                    source,
                    section: "edit",
                    full: true,
                    blocks: Vec::new(),
                    rank,
                });
                continue;
            }
        }
        let lines = unit.end - unit.start + 1;
        let cost = range_cost(&source, unit.start, unit.end.min(source.lines.len()));
        let required_bytes = soft_bytes.max(hard * 86 / 100);
        if (!effective_required && used + lines > budget)
            || used_bytes + cost
                > if effective_required {
                    required_bytes
                } else {
                    soft_bytes
                }
        {
            if let Some(symbol) = &unit.unit {
                push_sig(&mut sigs, &unit.file, unit.start, &symbol.sig);
            }
            deferred.push(Deferred {
                file: unit.file.clone(),
                block: Block {
                    start: unit.start,
                    end: unit.end.min(source.lines.len()),
                    label: unit.label.clone(),
                    tag: match unit.role {
                        "target" => "def",
                        "related" => unit.tag,
                        role => role,
                    },
                    score: unit.score,
                    required: effective_required,
                },
                rank: *file_rank.get(&unit.file).unwrap_or(&99),
            });
            continue;
        }
        let index = if let Some(index) = plan_index {
            index
        } else {
            plans.push(PlannedFile {
                file: unit.file.clone(),
                source: source.clone(),
                section: "edit",
                full: false,
                blocks: Vec::new(),
                rank: *file_rank.get(&unit.file).unwrap_or(&99),
            });
            plans.len() - 1
        };
        plans[index].blocks.push(Block {
            start: unit.start,
            end: unit.end.min(source.lines.len()),
            label: unit.label,
            tag: match unit.role {
                "target" => "def",
                "related" => unit.tag,
                role => role,
            },
            score: unit.score,
            required: effective_required,
        });
        used += lines;
        used_bytes += cost;
        if let Some(obligation) = unit.obligation {
            packed_obligations.insert(obligation);
        }
    }
    let original_count = plans.len();
    let owned: HashSet<_> = plans
        .iter()
        .flat_map(|p| {
            p.source
                .syms
                .iter()
                .filter(|s| covered(p, s.ln))
                .map(move |s| (p.file.clone(), s.ln))
        })
        .collect();
    let keyword_set = keywords
        .iter()
        .chain(task_terms.iter())
        .cloned()
        .collect::<HashSet<_>>();
    // 特性③ 种子签名里的类型名是“改签名”类任务的必看依赖：提权进入 DEPS。
    let mut sig_terms = HashSet::<String>::new();
    {
        static SIG_IDENT: OnceLock<Regex> = OnceLock::new();
        let ident = SIG_IDENT.get_or_init(|| Regex::new(r"[A-Za-z_$][A-Za-z0-9_$]*").unwrap());
        for (definition, _, _) in &seeds {
            for found in ident.find_iter(&definition.symbol.sig) {
                let name = found.as_str();
                if name.len() >= 4
                    && !stop_word(&name.to_lowercase())
                    && !keyword_set.contains(name)
                    && !seed_names.contains(name)
                {
                    sig_terms.insert(name.to_string());
                }
            }
        }
    }
    let mut dep_seen = HashSet::<(String, usize)>::new();
    let mut dep_queue = collect_dependencies(&index, &plans, &owned, &keyword_set, &sig_terms);
    let mut dependencies = Vec::<(String, Definition, usize)>::new();
    for dep_depth in 0..3 {
        let wave = std::mem::take(&mut dep_queue);
        let mut next_plans = Vec::new();
        for (dep_name, def) in wave {
            let dep_key = (def.file.clone(), def.symbol.ln);
            if dep_seen.contains(&dep_key) || (dep_depth > 0 && !def.symbol.exp) {
                continue;
            }
            dep_seen.insert(dep_key);
            dependencies.push((dep_name, def.clone(), dep_depth));
            if dep_depth < 2 {
                if let Some(src) = source(root, &def.file, index.files.get(&def.file)) {
                    next_plans.push(PlannedFile {
                        file: def.file,
                        source: src,
                        section: "dep",
                        full: false,
                        blocks: vec![Block {
                            start: def.symbol.ln,
                            end: def.symbol.end,
                            label: String::new(),
                            tag: "dep",
                            score: 0.0,
                            required: false,
                        }],
                        rank: 99,
                    });
                }
            }
        }
        if dep_depth < 2 && !next_plans.is_empty() {
            let mut excluded = owned.clone();
            excluded.extend(dep_seen.iter().cloned());
            dep_queue.extend(collect_dependencies(
                &index,
                &next_plans,
                &excluded,
                &keyword_set,
                &sig_terms,
            ));
        }
    }
    for (dep_name, def, dep_depth) in dependencies {
        if plans
            .iter()
            .filter(|p| p.section == "dep")
            .map(|p| &p.file)
            .collect::<HashSet<_>>()
            .len()
            >= MAX_DEP_FILES
            && !plans.iter().any(|p| p.file == def.file)
        {
            push_sig(&mut sigs, &def.file, def.symbol.ln, &def.symbol.sig);
            continue;
        }
        let Some(src) = source(root, &def.file, index.files.get(&def.file)) else {
            continue;
        };
        let n = def.symbol.end - def.symbol.ln + 1;
        let bytes = range_cost(&src, def.symbol.ln, def.symbol.end);
        let required = dep_depth == 0;
        let required_bytes = soft_bytes.max(hard * 86 / 100);
        if (!required && used + n > budget)
            || used_bytes + bytes > if required { required_bytes } else { soft_bytes }
        {
            push_sig(&mut sigs, &def.file, def.symbol.ln, &def.symbol.sig);
            deferred.push(Deferred {
                file: def.file.clone(),
                block: Block {
                    start: def.symbol.ln,
                    end: def.symbol.end,
                    label: format!("{} {}", def.symbol.kind, dep_name),
                    tag: if dep_depth == 0 { "dep" } else { "dep2" },
                    score: 120.0 - dep_depth as f64 * 30.0,
                    required,
                },
                rank: *file_rank.get(&def.file).unwrap_or(&99),
            });
            continue;
        }
        used += n;
        used_bytes += bytes;
        if let Some(plan) = plans.iter_mut().find(|p| p.file == def.file) {
            plan.blocks.push(Block {
                start: def.symbol.ln,
                end: def.symbol.end,
                label: format!("{} {}", def.symbol.kind, dep_name),
                tag: if dep_depth == 0 { "dep" } else { "dep2" },
                score: 120.0 - dep_depth as f64 * 30.0,
                required: dep_depth == 0,
            });
        } else {
            plans.push(PlannedFile {
                file: def.file.clone(),
                source: src,
                section: "dep",
                full: false,
                blocks: vec![Block {
                    start: def.symbol.ln,
                    end: def.symbol.end,
                    label: format!("{} {}", def.symbol.kind, dep_name),
                    tag: if dep_depth == 0 { "dep" } else { "dep2" },
                    score: 120.0 - dep_depth as f64 * 30.0,
                    required: dep_depth == 0,
                }],
                rank: *file_rank.get(&def.file).unwrap_or(&99),
            });
        }
    }
    let seed_names = seed_names.into_iter().collect::<Vec<_>>();
    let render = |plans: &Vec<PlannedFile>,
                  sigs: &Vec<(String, usize, String)>,
                  impact_limit: usize,
                  compact_index: bool| {
        let mut body = Vec::new();
        let mut order = plans.iter().collect::<Vec<_>>();
        order.sort_by_key(|plan| plan.rank);
        let mut block_count = 0usize;
        for section in ["edit", "dep"] {
            let group = order
                .iter()
                .copied()
                .filter(|plan| plan.section == section)
                .collect::<Vec<_>>();
            if group.is_empty() {
                continue;
            }
            body.push(if section == "edit" {
                "## EDIT".into()
            } else {
                "## DEPS (依赖定义, 完整单元)".into()
            });
            for plan in group {
                // import 图精确调用方（别名/透传证据）在文件头打标，无论正文是否覆盖
                let exact_mark = if exact_callers.contains_key(&plan.file) {
                    " [import图]"
                } else {
                    ""
                };
                if plan.full {
                    body.push(format!(
                        "### {} ({}L) FULL{exact_mark}",
                        plan.file,
                        plan.source.lines.len()
                    ));
                    body.extend(plan.source.lines.iter().map(|line| clip(line)));
                    block_count += 1;
                } else {
                    let mut ranges = shown_ranges(plan);
                    ranges.sort_unstable();
                    let shown = ranges
                        .iter()
                        .map(|(a, b)| {
                            if a == b {
                                a.to_string()
                            } else {
                                format!("{a}-{b}")
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(",");
                    body.push(format!(
                        "### {} ({}L) shown={shown}{exact_mark}",
                        plan.file,
                        plan.source.lines.len()
                    ));
                    let mut blocks = plan.blocks.clone();
                    blocks.sort_by_key(|block| block.start);
                    for block in blocks {
                        block_count += 1;
                        body.push(format!(
                            "@@ {}-{} {}{}",
                            block.start,
                            block.end,
                            block.label,
                            if block.tag == "hit" {
                                "".into()
                            } else {
                                format!(" [{}]", block.tag)
                            }
                        ));
                        body.extend(
                            plan.source.lines[block.start - 1..block.end]
                                .iter()
                                .map(|line| clip(line)),
                        );
                    }
                    let rest = plan
                        .source
                        .syms
                        .iter()
                        .filter(|symbol| {
                            symbol.depth == 0
                                && !covered(plan, symbol.ln)
                                && !(symbol.kind == "const" && symbol.end == symbol.ln)
                        })
                        .map(|symbol| {
                            format!(
                                "{} {}{}",
                                symbol.ln,
                                if symbol.kind == "impl" { "impl " } else { "" },
                                symbol.name
                            )
                        })
                        .collect::<Vec<_>>();
                    let subject_file =
                        subject_match(&plan.file, &subject_terms, &term_freq) >= 300.0;
                    let cap = if subject_file {
                        24
                    } else if plan.rank < 1 {
                        8
                    } else {
                        0
                    };
                    if !rest.is_empty() && cap > 0 {
                        let shown_rest = rest.iter().take(cap).cloned().collect::<Vec<_>>();
                        body.push(format!(
                            "~ {}{}",
                            shown_rest.join(" | "),
                            if rest.len() > cap {
                                format!(" | +{}", rest.len() - cap)
                            } else {
                                String::new()
                            }
                        ));
                    }
                }
                body.push(String::new());
            }
        }
        let mut impacts = Vec::new();
        for (file, _) in ranked.iter().filter(|(file, _)| {
            (plan_intent.callers || plan_intent.tests || seeds.is_empty())
                && (plan_intent.tests || !noise_path(file))
        }) {
            let Some(rows) = hit_files.get(file) else {
                continue;
            };
            for row in rows {
                if plans
                    .iter()
                    .any(|plan| plan.file == *file && covered(plan, row.ln))
                {
                    continue;
                }
                let graph_hit = graph_rows.contains(&(file.clone(), row.ln));
                let seed_ref = if seed_names.is_empty() {
                    keywords.iter().any(|keyword| row.text.contains(keyword))
                } else {
                    seed_names.iter().any(|name| row.text.contains(name)) || graph_hit
                };
                if seed_ref {
                    impacts.push(format!(
                        "{}:{} {}{}",
                        file,
                        row.ln,
                        js_utf16_slice(row.text.trim(), 120),
                        if graph_hit { " [import图]" } else { "" }
                    ));
                }
            }
        }
        impacts.sort();
        if !impacts.is_empty() {
            body.push(format!(
                "## IMPACT (调用方/引用清单 {}/{}, 仅行; 确需函数体按 path:ln 补读)",
                impacts.len().min(impact_limit),
                impacts.len()
            ));
            body.extend(impacts.into_iter().take(impact_limit));
            body.push(String::new());
        }
        let target_count = seeds
            .iter()
            .filter(|(definition, _, _)| {
                plans
                    .iter()
                    .any(|plan| plan.file == definition.file && covered(plan, definition.symbol.ln))
            })
            .count();
        let dep_count = plans
            .iter()
            .filter(|plan| plan.section == "dep")
            .map(|plan| plan.blocks.len())
            .sum::<usize>();
        let role_count = |role: &str| {
            closure_roles
                .iter()
                .filter(|(file, start, unit_role)| {
                    *unit_role == role
                        && plans
                            .iter()
                            .any(|plan| plan.file == *file && covered(plan, *start))
                })
                .map(|(file, _, _)| file)
                .collect::<HashSet<_>>()
                .len()
        };
        body.push("## PROOF (任务闭包检查)".into());
        body.push(format!(
            "符号关系: {}",
            if planned_terms.is_empty() {
                "无可解析扩展边".into()
            } else {
                format!("已解析 {}", planned_terms.len())
            }
        ));
        body.push(format!(
            "目标定义: {}",
            if target_count > 0 {
                format!("已闭合 {target_count}")
            } else {
                "缺口".into()
            }
        ));
        body.push(format!(
            "依赖定义: {}",
            if dep_count > 0 {
                format!("已闭合 {dep_count}")
            } else {
                "未发现".into()
            }
        ));
        if plan_intent.errors {
            let count = role_count("handler");
            body.push(format!(
                "错误处理: {}",
                if count > 0 {
                    format!("已闭合 {count}")
                } else {
                    "缺口".into()
                }
            ));
        }
        if plan_intent.callers {
            let count = role_count("caller");
            body.push(format!(
                "关键调用方: {}",
                if count > 0 {
                    format!("已闭合 {count}")
                } else {
                    "缺口".into()
                }
            ));
        }
        if plan_intent.tests {
            let count = role_count("test");
            body.push(format!(
                "相关测试: {}",
                if count > 0 {
                    format!("已闭合 {count}")
                } else {
                    "缺口".into()
                }
            ));
        }
        body.push(String::new());
        if !sigs.is_empty() {
            body.push("## SIG (预算内放不下或最终回退的定义, 仅签名)".into());
            for (file, line, sig) in sigs {
                body.push(format!("{file}:{line} {sig}"));
            }
            body.push(String::new());
        }
        let mut notes = Vec::new();
        if !compact_index {
            if !missed_all.is_empty() {
                notes.push(format!("未命中关键词: {}", missed_all.join(" ")));
            }
            if !loose_kw.is_empty() {
                notes.push(format!("忽略大小写才命中: {}", loose_kw.join(" ")));
            }
            if !coupling_files.is_empty() {
                notes.push(format!(
                    "共改耦合(git): {}",
                    coupling_files
                        .iter()
                        .map(|(file, count)| format!("{file} ({count}x)"))
                        .collect::<Vec<_>>()
                        .join(" ")
                ));
            }
            let mut unexpanded = ranked
                .iter()
                .filter(|(file, _)| !plans.iter().any(|plan| plan.file == *file))
                .map(|(file, _)| file)
                .collect::<Vec<_>>();
            unexpanded.sort();
            if !unexpanded.is_empty() {
                notes.push(format!(
                    "其它命中文件(未展开): {}{}",
                    unexpanded
                        .iter()
                        .take(10)
                        .copied()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(" "),
                    if unexpanded.len() > 10 {
                        format!(" +{}", unexpanded.len() - 10)
                    } else {
                        String::new()
                    }
                ));
            }
        }
        let content = body.join("\n");
        let shown_lines = order
            .iter()
            .map(|plan| {
                shown_ranges(plan)
                    .iter()
                    .map(|(a, b)| b - a + 1)
                    .sum::<usize>()
            })
            .sum::<usize>();
        let mut head = format!("# CTX {}{}{} @{}  {}文件/{}块 {}行 {:.1}KB\n# 契约: 已按修改计划构建符号关系、计算任务闭包并做缺口证明；正文均为完整单元/完整文件。已展示行段禁止重读。",
            if keywords.is_empty() { String::new() } else { format!("q={}", keywords.join(",")) },
            if task.is_empty() { String::new() } else { format!(" task=\"{}\"", js_utf16_slice(&task, 80)) },
            if files.is_empty() { String::new() } else { format!(" files={}", files.join(",")) },
            revision, order.len(), block_count, shown_lines, content.len() as f64 / 1024.0);
        for note in notes {
            head.push_str(&format!("\n# {note}"));
        }
        head.push_str("\n\n");
        format!("{head}{content}")
    };
    trace("fast_context.plan", stage);
    let render_start = Instant::now();
    let mut impact_limit = MAX_IMPACT;
    let mut compact_index = false;
    let mut text = render(&plans, &sigs, impact_limit, compact_index);
    while text.len() > hard {
        let Some(index) = plans
            .iter()
            .enumerate()
            .filter(|(_, plan)| plan.full || !plan.blocks.is_empty())
            .max_by_key(|(_, plan)| (if plan.section == "dep" { 1 } else { 0 }, plan.rank))
            .map(|(index, _)| index)
        else {
            break;
        };
        if plans[index].full {
            let removed = plans.remove(index);
            let mut found = false;
            for symbol in removed
                .source
                .syms
                .iter()
                .filter(|symbol| symbol.depth <= 1 && !symbol.sig.is_empty())
            {
                if !sigs
                    .iter()
                    .any(|(file, line, _)| file == &removed.file && *line == symbol.ln)
                {
                    sigs.push((removed.file.clone(), symbol.ln, symbol.sig.clone()));
                }
                found = true;
            }
            if !found {
                let sig = removed
                    .source
                    .lines
                    .first()
                    .map(|line| js_utf16_slice(line.trim(), 120))
                    .unwrap_or_default();
                if !sig.is_empty() {
                    sigs.push((removed.file.clone(), 1, sig));
                }
            }
            dropped.push(Dropped::Full(removed));
        } else {
            let block = plans[index]
                .blocks
                .iter()
                .enumerate()
                .min_by(|(_, a), (_, b)| {
                    a.required
                        .cmp(&b.required)
                        .then_with(|| {
                            a.score
                                .partial_cmp(&b.score)
                                .unwrap_or(std::cmp::Ordering::Equal)
                        })
                        // JS renderOutput() sorts blocks by source line before each budget drop.
                        // Preserve that stable tie order without mutating the render plan.
                        .then(a.start.cmp(&b.start))
                })
                .map(|(position, _)| position);
            if let Some(position) = block {
                let block = plans[index].blocks.remove(position);
                let file = plans[index].file.clone();
                let source = &plans[index].source;
                let mut found = false;
                for symbol in source.syms.iter().filter(|symbol| {
                    symbol.depth <= 1
                        && symbol.ln >= block.start
                        && symbol.end <= block.end
                        && !symbol.sig.is_empty()
                }) {
                    if !sigs
                        .iter()
                        .any(|(existing, line, _)| existing == &file && *line == symbol.ln)
                    {
                        sigs.push((file.clone(), symbol.ln, symbol.sig.clone()));
                    }
                    found = true;
                }
                if !found {
                    let sig = source
                        .lines
                        .get(block.start - 1)
                        .map(|line| js_utf16_slice(line.trim(), 120))
                        .unwrap_or_default();
                    if !sig.is_empty() {
                        sigs.push((file.clone(), block.start, sig));
                    }
                }
                dropped.push(Dropped::Block(file, block));
            }
            if plans[index].blocks.is_empty() {
                plans.remove(index);
            }
        }
        text = render(&plans, &sigs, impact_limit, compact_index);
    }
    while text.len() > hard && impact_limit > 0 {
        impact_limit = impact_limit.saturating_sub(5);
        text = render(&plans, &sigs, impact_limit, compact_index);
    }
    if text.len() > hard {
        compact_index = true;
        text = render(&plans, &sigs, impact_limit, compact_index);
    }
    // 特性⑤ 预算回填：shrink 只删不补、打包按 soft 封顶，当 IMPACT/SIG/表头开销
    // 小于预留时输出会显著低于硬顶。把 shrink 删掉的（后删=相对高价值优先）与
    // 打包暂缓的块按序补回，直到贴近硬顶；补不进的回退，继续试下一个。
    if std::env::var_os("NOVA_CTX_DEBUG").is_some() {
        eprintln!(
            "[ctx] backfill: text={} hard={} dropped={} deferred={}",
            text.len(),
            hard,
            dropped.len(),
            deferred.len()
        );
    }
    if text.len() < hard * 9 / 10 {
        while let Some(item) = dropped.pop() {
            if text.len() >= hard * 9 / 10 {
                break;
            }
            match item {
                Dropped::Full(plan) => {
                    if plans.iter().any(|existing| existing.file == plan.file) {
                        continue;
                    }
                    let file = plan.file.clone();
                    plans.push(plan);
                    let trial = render(&plans, &sigs, impact_limit, compact_index);
                    if trial.len() <= hard {
                        text = trial;
                    } else {
                        plans.retain(|existing| existing.file != file);
                    }
                }
                Dropped::Block(file, block) => {
                    let created = backfill_block(
                        root, &index, &sources, &mut plans, &mut sigs, &file, &block, 99,
                    );
                    let Some(created) = created else {
                        continue;
                    };
                    let trial = render(&plans, &sigs, impact_limit, compact_index);
                    if trial.len() <= hard {
                        text = trial;
                    } else if let Some(position) = plans
                        .iter()
                        .position(|plan| plan.file == file && !plan.full)
                    {
                        plans[position].blocks.retain(|existing| {
                            !(existing.start == block.start && existing.end == block.end)
                        });
                        if created && plans[position].blocks.is_empty() {
                            plans.remove(position);
                        }
                    }
                }
            }
        }
        for item in &deferred {
            if text.len() >= hard * 9 / 10 {
                break;
            }
            let created = backfill_block(
                root,
                &index,
                &sources,
                &mut plans,
                &mut sigs,
                &item.file,
                &item.block,
                item.rank,
            );
            let Some(created) = created else {
                continue;
            };
            let trial = render(&plans, &sigs, impact_limit, compact_index);
            if trial.len() <= hard {
                text = trial;
            } else if let Some(position) = plans
                .iter()
                .position(|plan| plan.file == item.file && !plan.full)
            {
                plans[position].blocks.retain(|existing| {
                    !(existing.start == item.block.start && existing.end == item.block.end)
                });
                if created && plans[position].blocks.is_empty() {
                    plans.remove(position);
                }
            }
        }
    }
    let _ = original_count;
    trace("fast_context.render", render_start);
    trace("fast_context.total", total_start);
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    #[test]
    fn chinese_task_extracts_generic_ngrams_without_stop_words() {
        let tokens = task_tokens("检查支持中心的问题反馈处理器");
        assert!(tokens.iter().any(|token| token == "支持中心"));
        assert!(tokens.iter().any(|token| token == "问题反馈"));
        assert!(tokens.iter().any(|token| token == "处理器"));
        assert!(tokens.len() <= 24);
    }

    #[test]
    fn scanner_tracks_js_blocks() {
        let e = scan_source(
            "export function outer() {\n const inner = () => {\n return 1;\n };\n}\n",
            "a.ts",
        );
        let outer = e.syms.iter().find(|s| s.name == "outer").unwrap();
        assert_eq!((outer.ln, outer.end), (1, 5));
        let inner = e.syms.iter().find(|s| s.name == "inner").unwrap();
        assert_eq!((inner.ln, inner.end), (2, 4));
    }
    #[test]
    fn context_returns_complete_unit() {
        let d = tempdir().unwrap();
        fs::write(
            d.path().join("a.ts"),
            "export function target() {\n return 1;\n}\n",
        )
        .unwrap();
        let out = fast_context(d.path(), serde_json::json!({"keywords":["target"]})).unwrap();
        assert!(out.contains("export function target"));
        assert!(out.contains("\n}"));
        assert!(!out.contains("partial"));
    }

    #[test]
    fn explicit_symbol_miss_stops_before_task_fallback() {
        let d = tempdir().unwrap();
        let anchor = ["add", "Compare"].concat();
        fs::write(
            d.path().join("filter.ts"),
            "export function filterRequestParams(value) { return value; }\n",
        )
        .unwrap();
        fs::write(
            d.path().join("compare.test.ts"),
            format!("export const requestedName = '{anchor}';\n"),
        )
        .unwrap();
        let out = fast_context(
            d.path(),
            serde_json::json!({
                "keywords":[anchor],
                "task":"修复接口参数过滤问题；定位实现、调用方和测试"
            }),
        )
        .unwrap();
        assert!(out.starts_with("# CTX MISS"), "{out}");
        assert!(
            out.contains("status: no production definition or reference"),
            "{out}"
        );
        assert!(!out.contains("filterRequestParams"), "{out}");
        assert!(out.len() < 2048, "{}", out.len());
    }

    #[test]
    fn invented_anchor_with_phrase_words_soft_degrades() {
        let d = tempdir().unwrap();
        fs::create_dir_all(d.path().join("src")).unwrap();
        fs::write(
            d.path().join("src/store.ts"),
            "export const UNIFIED_MODES = [{ id: 'build', name: 'Build' }, { id: 'plan', name: 'Plan' }];\nexport function modeChoices() { return UNIFIED_MODES; }\nexport function setThreadMode(mode) { return UNIFIED_MODES.find((m) => m.id === mode); }\n",
        )
        .unwrap();
        // 锚点拼接构造：字面量写进本文件会污染真实仓库的召回评测（production_anchor_hit 命中）
        let invented_p = ["Plan", "Mode"].concat();
        let invented_a = ["agent", "Mode"].concat();
        let out = fast_context(
            d.path(),
            serde_json::json!({
                "keywords":["plan mode", invented_p, invented_a],
                "task":"Remove plan mode UI selection; keep only build as default"
            }),
        )
        .unwrap();
        assert!(!out.starts_with("# CTX MISS"), "{out}");
        assert!(out.contains("modeChoices"), "{out}");
        assert!(out.contains("UNIFIED_MODES"), "{out}");
        // 臆造锚点如实标注；短语泛词 plan mode 不算未命中
        let note = out
            .lines()
            .find(|line| line.contains("未命中关键词"))
            .unwrap_or_default();
        assert!(
            note.contains(&invented_p) && note.contains(&invented_a),
            "{out}"
        );
        assert!(!note.contains("plan mode"), "{out}");
    }

    #[test]
    fn hard_miss_suggests_similar_production_symbols() {
        let d = tempdir().unwrap();
        fs::create_dir_all(d.path().join("src/components")).unwrap();
        fs::write(
            d.path().join("src/components/PlanCard.tsx"),
            "export function PlanCard(props) { return props.plan; }\n",
        )
        .unwrap();
        fs::write(
            d.path().join("src/store.ts"),
            "export const proposedPlan = null;\nexport function dismissProposedPlan() {}\n",
        )
        .unwrap();
        let invented_p = ["Plan", "Mode"].concat();
        let out = fast_context(
            d.path(),
            serde_json::json!({"keywords":[invented_p], "task":"switch to plan mode"}),
        )
        .unwrap();
        assert!(out.starts_with("# CTX MISS"), "{out}");
        assert!(
            out.contains("status: no production definition or reference"),
            "{out}"
        );
        assert!(
            out.contains("PlanCard (src/components/PlanCard.tsx:1)"),
            "{out}"
        );
        assert!(out.len() < 2048, "{out}");
    }

    #[test]
    fn subject_match_requires_full_segment_and_decays_with_frequency() {
        let terms = vec!["mode".to_string(), "build".to_string(), "plan".to_string()];
        // mode 不是 model 的完整段：不匹配
        assert_eq!(
            subject_match("src-tauri/src/model_cache.rs", &terms, &HashMap::new()),
            0.0
        );
        assert_eq!(
            subject_match("tmp_search_model_merge.js", &terms, &HashMap::new()),
            0.0
        );
        // 稀有词满分
        let rare = vec!["suggestions".to_string()];
        assert_eq!(
            subject_match("src/components/slashSuggestions.ts", &rare, &HashMap::new()),
            600.0
        );
        // 高频泛词衰减到不足主题通读阈值
        let mut freq = HashMap::new();
        freq.insert("build".to_string(), 223);
        freq.insert("plan".to_string(), 428);
        let damped = subject_match("src-tauri/build.rs", &terms, &freq);
        assert!(damped < 300.0 && damped >= 100.0, "{damped}");
        // camelCase 切段后仍可命中
        assert!(subject_match("src/components/PlanActionCard.tsx", &terms, &freq) > 0.0);
        // 整词小写形式命中驼峰合并段：chatview ↔ ChatView.tsx（前端组件命名的常态）
        let camel = vec!["chatview".to_string()];
        assert_eq!(
            subject_match("src/components/ChatView.tsx", &camel, &HashMap::new()),
            600.0
        );
        assert!(is_subject_file("src/components/ChatView.tsx", &camel));
        assert!(is_subject_file("src/components/chat-view.tsx", &camel));
        assert!(!is_subject_file("src/components/Chat.tsx", &camel));
    }

    #[test]
    fn event_string_bridge_includes_emit_and_listen_ends() {
        let d = tempdir().unwrap();
        fs::write(
            d.path().join("publisher.ts"),
            "import { emit } from '@tauri-apps/api/event';\nexport function publishRefresh(payload) { return emit('workspace-refresh', payload); }\n",
        )
        .unwrap();
        fs::write(
            d.path().join("subscriber.ts"),
            "import { listen } from '@tauri-apps/api/event';\nexport function subscribeRefresh(handler) { return listen('workspace-refresh', handler); }\n",
        )
        .unwrap();
        let out = fast_context(
            d.path(),
            serde_json::json!({
                "keywords":["workspace-refresh"],
                "task":"修改 workspace-refresh 事件，检查 emit 和 listen 调用方"
            }),
        )
        .unwrap();
        assert!(!out.contains("# CTX MISS"), "{out}");
        assert!(out.contains("emit('workspace-refresh'"), "{out}");
        assert!(out.contains("listen('workspace-refresh'"), "{out}");
    }

    #[test]
    fn indexed_candidates_ignore_wide_terms_when_a_rare_anchor_exists() {
        let root = tempdir().unwrap();
        // 全文索引只认 git 仓库：测试目录补一个 .git 占位。
        fs::create_dir(root.path().join(".git")).unwrap();
        fs::write(root.path().join("rare.go"), "roblox sql query").unwrap();
        for index in 0..SEARCH_INDEX_MAX_CANDIDATES + 2 {
            fs::write(root.path().join(format!("wide-{index}.go")), "sql query").unwrap();
        }
        let files: Vec<String> = list_code_files(root.path()).iter().cloned().collect();
        let mut postings = HashMap::<String, Vec<String>>::new();
        let mut contents = HashMap::<String, String>::new();
        let mut file_tokens = HashMap::<String, Vec<String>>::new();
        for file in &files {
            let text = fs::read_to_string(root.path().join(file)).unwrap();
            let tokens = tokens_for_search_text(&text);
            for token in &tokens {
                postings
                    .entry(token.clone())
                    .or_default()
                    .push(file.clone());
            }
            file_tokens.insert(file.clone(), tokens);
            contents.insert(file.clone(), text);
        }
        for files in postings.values_mut() {
            files.sort();
            files.dedup();
        }
        let snapshot = SearchSnapshot {
            version: SEARCH_SNAPSHOT_VERSION,
            root: normalize_root(root.path()),
            head: "unknown".into(),
            git_signature: String::new(),
            dirty_files: Vec::new(),
            postings,
            contents,
            file_tokens,
        };
        let key = normalize_root(root.path());
        SEARCH_INDEXES
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap()
            .insert(key, index_from_snapshot(snapshot));

        let candidates =
            indexed_candidate_files(root.path(), &["roblox".into(), "sql".into()]).unwrap();
        assert_eq!(candidates, vec!["rare.go"]);
    }

    #[test]
    fn scoped_search_only_returns_requested_files() {
        let d = tempdir().unwrap();
        fs::create_dir(d.path().join("src")).unwrap();
        fs::write(
            d.path().join("src/a.ts"),
            "export const bridge = 'shared';\n",
        )
        .unwrap();
        fs::write(
            d.path().join("src/b.ts"),
            "export const bridge = 'shared';\n",
        )
        .unwrap();
        let rows = search_text_until(
            d.path(),
            &["shared".into()],
            false,
            false,
            &["src/b.ts".into()],
            None,
        );
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].file, "src/b.ts");
    }

    #[test]
    fn context_packs_imported_dependency_definition() {
        let d = tempdir().unwrap();
        fs::create_dir(d.path().join("src")).unwrap();
        let filler = (0..130)
            .map(|i| format!("export const PAD_{i} = {i};"))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(
            d.path().join("src/target.ts"),
            format!("import {{ helperFn }} from './helper';\n\nexport function targetFn(input) {{\n  return helperFn(input);\n}}\n{filler}"),
        ).unwrap();
        fs::write(
            d.path().join("src/helper.ts"),
            "export function helperFn(value) {\n  return `${value}!`;\n}\n",
        )
        .unwrap();
        let out = fast_context(d.path(), serde_json::json!({"keywords":["targetFn"]})).unwrap();
        assert!(out.contains("@@ 3-5 fn targetFn [def]"), "{out}");
        assert!(out.contains("## DEPS"), "{out}");
        assert!(out.contains("export function helperFn"), "{out}");
    }

    #[test]
    fn plan_searches_error_handlers_from_target_body() {
        let d = tempdir().unwrap();
        fs::create_dir_all(d.path().join("src/auth")).unwrap();
        fs::create_dir_all(d.path().join("src/ui")).unwrap();
        let filler = (0..130)
            .map(|i| format!("export const PAD_{i} = {i};"))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(
            d.path().join("src/auth/refreshSession.ts"),
            format!("import {{ SessionExpiredError }} from './errors';\nexport function refreshSession(token) {{\n  if (!token) throw new SessionExpiredError('expired');\n  return token;\n}}\n{filler}"),
        ).unwrap();
        fs::write(
            d.path().join("src/auth/errors.ts"),
            "export class SessionExpiredError extends Error {}\n",
        )
        .unwrap();
        fs::write(
            d.path().join("src/ui/sessionBoundary.ts"),
            "import { SessionExpiredError } from '../auth/errors';\nexport function handleSessionFailure(error) {\n  if (error instanceof SessionExpiredError) return 'login';\n  throw error;\n}\n",
        ).unwrap();

        let plain =
            fast_context(d.path(), serde_json::json!({"keywords":["refreshSession"]})).unwrap();
        assert!(
            !plain.contains("export function handleSessionFailure"),
            "{plain}"
        );
        let planned = fast_context(
            d.path(),
            serde_json::json!({
                "keywords":["refreshSession"],
                "task":"修改 refreshSession 的失败和错误处理，保持会话过期行为"
            }),
        )
        .unwrap();
        assert!(
            planned.contains("export function handleSessionFailure"),
            "{planned}"
        );
        assert!(planned.contains("SessionExpiredError"), "{planned}");
        assert!(planned.contains("## PROOF (任务闭包检查)"), "{planned}");
        assert!(planned.contains("错误处理: 已闭合"), "{planned}");
    }

    #[test]
    fn relation_graph_does_not_require_semantic_name_suffixes() {
        let d = tempdir().unwrap();
        fs::create_dir_all(d.path().join("src/auth")).unwrap();
        fs::create_dir_all(d.path().join("src/ui")).unwrap();
        let filler = (0..125)
            .map(|i| format!("export const PAD_{i} = {i};"))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(
            d.path().join("src/auth/authorize.ts"),
            format!("import {{ AuthFault }} from './faults';\nexport function authorize(token) {{\n  if (!token) throw new AuthFault('denied');\n  return token;\n}}\n{filler}"),
        ).unwrap();
        fs::write(
            d.path().join("src/auth/faults.ts"),
            "export class AuthFault { constructor(message) { this.message = message; } }\n",
        )
        .unwrap();
        fs::write(
            d.path().join("src/ui/authBoundary.ts"),
            "import { AuthFault } from '../auth/faults';\nexport function routeFault(reason) {\n  if (reason instanceof AuthFault) return 'login';\n  return 'unknown';\n}\n",
        ).unwrap();
        let out = fast_context(
            d.path(),
            serde_json::json!({"keywords":["authorize"],"task":"修改 authorize 的失败处理"}),
        )
        .unwrap();
        assert!(out.contains("export function routeFault"), "{out}");
        assert!(out.contains("符号关系: 已解析"), "{out}");
        assert!(out.contains("错误处理: 已闭合"), "{out}");
    }

    #[test]
    fn task_closure_packs_second_level_import_dependency() {
        let d = tempdir().unwrap();
        fs::create_dir(d.path().join("src")).unwrap();
        let filler = (0..120)
            .map(|i| format!("export const PAD_{i} = {i};"))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(
            d.path().join("src/target.ts"),
            format!("import {{ buildEnvelope }} from './helper';\nexport function targetFlow(value) {{ return buildEnvelope(value); }}\n{filler}"),
        ).unwrap();
        fs::write(
            d.path().join("src/helper.ts"),
            "import { ResultEnvelope } from './types';\nexport function buildEnvelope(value) { return new ResultEnvelope(value); }\n",
        ).unwrap();
        fs::write(
            d.path().join("src/types.ts"),
            "export class ResultEnvelope {\n  constructor(value) { this.value = value; }\n}\n",
        )
        .unwrap();
        let out = fast_context(d.path(), serde_json::json!({"keywords":["targetFlow"]})).unwrap();
        assert!(out.contains("buildEnvelope"), "{out}");
        assert!(out.contains("ResultEnvelope"), "{out}");
        assert!(out.contains("[dep2]"), "{out}");
        assert!(out.contains("## PROOF (任务闭包检查)"), "{out}");
    }

    #[test]
    fn resolved_target_beats_subject_file_sweep_and_test_noise() {
        let d = tempdir().unwrap();
        fs::create_dir(d.path().join("src")).unwrap();
        let filler = (0..830)
            .map(|i| format!("export const PAD_{i} = {i};"))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(
            d.path().join("src/zebra_engine.ts"),
            format!("export function zebraMarker() {{\n  return 'zebra';\n}}\n{filler}\nexport function laterFlow() {{\n  return 42;\n}}"),
        ).unwrap();
        fs::write(
            d.path().join("src/zebra.test.ts"),
            "import { zebraMarker } from './zebra_engine';\nexport const sees = zebraMarker();\n",
        )
        .unwrap();
        let out = fast_context(d.path(), serde_json::json!({"keywords":["zebra"]})).unwrap();
        assert!(out.contains("### src/zebra_engine.ts"), "{out}");
        assert!(out.contains("export function zebraMarker"), "{out}");
        assert!(!out.contains("export function laterFlow"), "{out}");
        assert!(!out.contains("### src/zebra.test.ts"), "{out}");
        assert!(
            out.contains("其它命中文件(未展开): src/zebra.test.ts"),
            "{out}"
        );
    }

    #[test]
    fn required_target_can_exceed_compatibility_line_budget() {
        let d = tempdir().unwrap();
        let body = (0..140)
            .map(|i| format!("  const value_{i} = {i};"))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(
            d.path().join("large.ts"),
            format!("export function requiredTarget() {{\n{body}\n  return value_139;\n}}"),
        )
        .unwrap();
        let out = fast_context(
            d.path(),
            serde_json::json!({"keywords":["requiredTarget"],"budget":100,"maxBytes":32768}),
        )
        .unwrap();
        assert!(out.contains("@@ 1-143 fn requiredTarget [def]"), "{out}");
        assert!(out.contains("return value_139;\n}"), "{out}");
    }

    #[test]
    fn explicit_callers_and_tests_get_representatives_under_tight_line_budget() {
        let d = tempdir().unwrap();
        let target_body = (0..120)
            .map(|i| format!("  const target_{i} = {i};"))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(
            d.path().join("target.ts"),
            format!("export function targetApi(input) {{\n{target_body}\n  return input;\n}}\n"),
        )
        .unwrap();
        fs::write(
            d.path().join("entryCaller.ts"),
            "import { targetApi } from './target';\nexport function entryCaller(input) { const entry = targetApi(input); return entry; }\n",
        )
        .unwrap();
        fs::write(
            d.path().join("errorCaller.ts"),
            "import { targetApi } from './target';\nexport function errorCaller(input) { try { return targetApi(input); } catch { return null; } }\n",
        )
        .unwrap();
        fs::write(
            d.path().join("target.test.ts"),
            "import { targetApi } from './target';\nexport function targetContract() { return targetApi('x') === 'x'; }\n",
        )
        .unwrap();
        let out = fast_context(
            d.path(),
            serde_json::json!({
                "keywords":["targetApi"],
                "task":"修改 targetApi 签名，检查调用方和测试",
                "budget":100,
                "maxBytes":12288
            }),
        )
        .unwrap();
        assert!(out.contains("export function entryCaller"), "{out}");
        assert!(out.contains("export function errorCaller"), "{out}");
        assert!(out.contains("export function targetContract"), "{out}");
    }

    #[test]
    fn task_only_identifier_seeds_target_definition() {
        let d = tempdir().unwrap();
        fs::write(
            d.path().join("target.ts"),
            "export function taskOnlyTarget() {\n  return 7;\n}\n",
        )
        .unwrap();
        let out = fast_context(
            d.path(),
            serde_json::json!({"task":"inspect taskOnlyTarget implementation"}),
        )
        .unwrap();
        assert!(out.contains("export function taskOnlyTarget"), "{out}");
        assert!(out.contains("目标定义: 已闭合"), "{out}");
    }

    #[test]
    fn tight_budget_keeps_whole_units_and_indexes_remainder() {
        let d = tempdir().unwrap();
        fs::create_dir(d.path().join("src")).unwrap();
        for i in 0..8 {
            let body = (0..35)
                .map(|n| format!("  const value_{i}_{n} = {n};"))
                .collect::<Vec<_>>()
                .join("\n");
            fs::write(
                d.path().join(format!("src/f{i}.ts")),
                format!("export function bounded{i}() {{\n{body}\n  return 1;\n}}"),
            )
            .unwrap();
        }
        let out = fast_context(
            d.path(),
            serde_json::json!({"keywords":["bounded"],"maxBytes":8192}),
        )
        .unwrap();
        assert!(out.len() <= 8192, "{}", out.len());
        assert!(
            out.contains("## SIG") || out.contains("其它命中文件(未展开)"),
            "{out}"
        );
        assert!(!out.contains("partial"));
        for block in out.split("@@ ").skip(1) {
            assert!(block.contains("\n}"), "{block}");
        }
    }

    fn git(root: &Path, args: &[&str]) {
        let args = args
            .iter()
            .map(|value| (*value).to_string())
            .collect::<Vec<_>>();
        let status = hidden_command("git")
            .args(args)
            .current_dir(root)
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[test]
    fn git_head_matches_git_rev_parse() {
        let d = tempdir().unwrap();
        git(d.path(), &["init", "-q"]);
        git(
            d.path(),
            &["config", "user.email", "native-test@nova.local"],
        );
        git(d.path(), &["config", "user.name", "Nova Native Test"]);
        fs::write(d.path().join("a.ts"), "export const value = 1;\n").unwrap();
        git(d.path(), &["add", "-A"]);
        git(d.path(), &["commit", "-qm", "fixture"]);
        let (branch, revision) = git_head(d.path());
        assert_eq!(
            branch,
            git_value(d.path(), &["rev-parse", "--abbrev-ref", "HEAD"])
        );
        assert_eq!(
            revision,
            git_value(d.path(), &["rev-parse", "--short", "HEAD"])
        );
    }

    #[test]
    fn file_listing_matches_git_tracked_and_untracked_ignore_rules() {
        let d = tempdir().unwrap();
        git(d.path(), &["init", "-q"]);
        fs::create_dir(d.path().join("src")).unwrap();
        fs::write(d.path().join(".gitignore"), "ignored.ts\n").unwrap();
        fs::write(d.path().join("ignored.ts"), "export const hidden = 1;\n").unwrap();
        fs::write(d.path().join("src/a.ts"), "export const visible = 1;\n").unwrap();
        let first = list_code_files(d.path());
        assert!(first.contains(&"src/a.ts".to_string()));
        assert!(!first.contains(&"ignored.ts".to_string()));
        fs::write(d.path().join("src/new.ts"), "export const fresh = 1;\n").unwrap();
        // 清单按 TTL 缓存：过期路径会先后台刷新，需触发刷新后再断言新文件可见。
        let key = normalize_root(d.path());
        FILE_LIST_CACHE
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap()
            .remove(&key);
        assert!(list_code_files(d.path()).contains(&"src/new.ts".to_string()));
    }

    #[test]
    fn code_filtered_search_preserves_gitignore() {
        let d = tempdir().unwrap();
        git(d.path(), &["init", "-q"]);
        fs::create_dir(d.path().join("src")).unwrap();
        fs::create_dir(d.path().join("generated")).unwrap();
        fs::write(d.path().join(".gitignore"), "generated/\n").unwrap();
        fs::write(
            d.path().join("src/visible.ts"),
            "const context_anchor = 1;\n",
        )
        .unwrap();
        fs::write(
            d.path().join("generated/bundle.mjs"),
            "const context_anchor = 2;\n",
        )
        .unwrap();

        let rows = rg_search(
            d.path(),
            &["context_anchor".to_string()],
            false,
            false,
            &[],
            true,
            None,
        )
        .unwrap();
        assert!(
            rows.iter().any(|row| row.file == "src/visible.ts"),
            "{rows:?}"
        );
        assert!(
            rows.iter().all(|row| row.file != "generated/bundle.mjs"),
            "{rows:?}"
        );
    }

    #[cfg(windows)]
    #[test]
    fn command_deadline_terminates_child() {
        let start = Instant::now();
        let args = vec![
            "-NoProfile".to_string(),
            "-Command".to_string(),
            "Start-Sleep -Seconds 5; Write-Output late".to_string(),
        ];
        let output = run_command_until(
            Path::new("."),
            "powershell.exe",
            &args,
            start + Duration::from_millis(150),
        );
        assert!(output.is_none());
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn limited_command_caps_large_output() {
        let d = tempdir().unwrap();
        fs::write(d.path().join("many.txt"), "bounded-output\n".repeat(20_000)).unwrap();
        let args = vec![
            "-n".to_string(),
            "bounded-output".to_string(),
            "many.txt".to_string(),
        ];
        let output = run_command_until_limited(
            d.path(),
            "rg",
            &args,
            Some(Instant::now() + Duration::from_secs(2)),
            1024,
        )
        .unwrap();
        assert_eq!(output.len(), 1024);
    }

    fn preload_indexes_uses_mmap_and_queries_do_not_reload_disk() {
        let d = tempdir().unwrap();
        let key = normalize_root(d.path());
        let mut files = HashMap::new();
        files.insert(
            "src/cached.ts".into(),
            FileEntry {
                size: 21,
                modified_ns: 1,
                total: 1,
                syms: Vec::new(),
                imports: Vec::new(),
            },
        );
        let cache = DiskCache {
            version: CACHE_VERSION,
            root: key.clone(),
            files,
            reverse: HashMap::new(),
        };
        let path = cache_path(d.path());
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, bincode::serialize(&cache).unwrap()).unwrap();
        MEMO.get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap()
            .remove(&key);

        assert_eq!(
            preload_indexes(&[d.path().to_string_lossy().into_owned()]),
            1
        );
        fs::remove_file(path).unwrap();
        let loaded = load_cache(d.path());
        assert!(loaded.files.contains_key("src/cached.ts"));
        store_cache(d.path(), loaded, false);
    }

    #[test]
    fn code_map_distinguishes_defs() {
        let d = tempdir().unwrap();
        fs::create_dir(d.path().join("src")).unwrap();
        fs::write(
            d.path().join("src/a.ts"),
            "export function pick() {\n  return 1;\n}\n",
        )
        .unwrap();
        fs::write(
            d.path().join("src/b.ts"),
            "import { pick } from './a';\nexport const value = pick();\n",
        )
        .unwrap();
        let map = code_map(d.path(), serde_json::json!({"scope":"src/"})).unwrap();
        assert!(map.contains("## src/a.ts (3L)"), "{map}");
        assert!(map.contains("1 pick"), "{map}");
    }

    #[test]
    fn reverse_import_graph_finds_alias_and_barrel_callers() {
        let d = tempdir().unwrap();
        fs::create_dir_all(d.path().join("src")).unwrap();
        fs::write(
            d.path().join("src/core.ts"),
            "export interface WidgetConfig {\n  title: string;\n}\nexport function renderWidget(config: WidgetConfig) {\n  return config.title;\n}\n",
        )
        .unwrap();
        // 别名调用：文本检索只能命中 import 行，命中不到 showWidget( 调用点
        fs::write(
            d.path().join("src/alias.ts"),
            "import { renderWidget as showWidget } from './core';\n\nexport function mountAliasPanel(config) {\n  return showWidget(config);\n}\n",
        )
        .unwrap();
        // barrel 改名透传：deep.ts 全文不出现 renderWidget，纯文本检索完全不可见
        fs::write(
            d.path().join("src/barrel.ts"),
            "export { renderWidget as widget } from './core';\n",
        )
        .unwrap();
        fs::write(
            d.path().join("src/deep.ts"),
            "import { widget } from './barrel';\n\nexport function mountB(config) {\n  return widget(config);\n}\n",
        )
        .unwrap();
        let out = fast_context(
            d.path(),
            serde_json::json!({"keywords":["renderWidget"],"task":"找出所有调用方 caller 兼容"}),
        )
        .unwrap();
        assert!(out.contains("renderWidget"), "{out}");
        assert!(out.contains("showWidget("), "{out}");
        assert!(out.contains("mountB"), "{out}");
        assert!(out.contains("widget("), "{out}");
        assert!(out.contains("[import图]"), "{out}");
    }

    #[test]
    fn typo_anchor_auto_corrects_and_retries() {
        let d = tempdir().unwrap();
        fs::create_dir_all(d.path().join("src")).unwrap();
        fs::write(
            d.path().join("src/store.ts"),
            "export const UNIFIED_MODES = ['build'];\nexport function modeChoices() { return UNIFIED_MODES; }\n",
        )
        .unwrap();
        // 锚点拼接构造：字面量写进本文件会污染真实仓库的召回评测（production_anchor_hit 命中）
        let typo = ["mode", "Choics"].concat();
        let out = fast_context(
            d.path(),
            serde_json::json!({"keywords":[typo],"task":"switch mode handling"}),
        )
        .unwrap();
        assert!(!out.starts_with("# CTX MISS"), "{out}");
        assert!(out.contains("锚点更正"), "{out}");
        assert!(out.contains("modeChoices"), "{out}");
        // 距离远的锚点不重试，保持 MISS
        let far = ["agent", "Mode"].concat();
        let miss = fast_context(
            d.path(),
            serde_json::json!({"keywords":[far],"task":"switch agent mode"}),
        )
        .unwrap();
        assert!(miss.starts_with("# CTX MISS"), "{miss}");
    }

    #[test]
    fn similar_enough_thresholds() {
        // 锚点拼接构造：字面量写进本文件会污染真实仓库的召回评测
        let typo = ["mode", "Choics"].concat();
        let far = ["agent", "Mode"].concat();
        assert!(similar_enough(&typo, "modeChoices"));
        assert!(similar_enough("fastContex", "fast_context"));
        assert!(!similar_enough(&far, "modeChoices"));
        assert!(!similar_enough("ab", "xyz"));
    }

    #[test]
    fn signature_types_get_dependency_priority() {
        let d = tempdir().unwrap();
        fs::create_dir_all(d.path().join("src")).unwrap();
        fs::write(
            d.path().join("src/types.ts"),
            "export interface WidgetConfig {\n  title: string;\n  mode: string;\n  retries: number;\n}\n",
        )
        .unwrap();
        let mut imports = vec!["import { WidgetConfig } from './types';".to_string()];
        let mut calls = Vec::new();
        for i in 0..10 {
            fs::write(
                d.path().join(format!("src/h{i}.ts")),
                format!("export function helperAlpha{i}(value) {{\n  return value + {i};\n}}\n"),
            )
            .unwrap();
            imports.push(format!("import {{ helperAlpha{i} }} from './h{i}';"));
            calls.push(format!("  acc = helperAlpha{i}(acc);"));
        }
        fs::write(
            d.path().join("src/service.ts"),
            format!(
                "{}\n\nexport function mountWidget(config: WidgetConfig) {{\n  let acc = config.retries;\n{}\n  return acc;\n}}\n",
                imports.join("\n"),
                calls.join("\n")
            ),
        )
        .unwrap();
        let out = fast_context(
            d.path(),
            serde_json::json!({"keywords":["mountWidget"],"task":"修改挂载入口签名"}),
        )
        .unwrap();
        assert!(out.contains("## DEPS"), "{out}");
        assert!(out.contains("### src/types.ts"), "{out}");
    }

    #[test]
    fn backfill_restores_deferred_blocks_within_hard_cap() {
        let d = tempdir().unwrap();
        fs::create_dir_all(d.path().join("src")).unwrap();
        fs::write(
            d.path().join("src/core.ts"),
            "export function alphaTarget(v) {\n  return v * 2;\n}\n",
        )
        .unwrap();
        // 3 文件 × 4 调用函数：总量远超 8KB 硬顶，打包按 soft 封顶后暂缓一批，
        // 回填应在硬顶内补回。文件数/每文件单元数都不触发结构性上限。
        for m in 0..3 {
            let mut units = vec!["import { alphaTarget } from './core';\n".to_string()];
            for n in 0..4 {
                let pad = (0..22)
                    .map(|i| format!("  const pad{m}_{n}_{i} = \"padding-value-{m}-{n}-{i}-aaaaaaaaaaaaaaaaaaaaaaaa\";"))
                    .collect::<Vec<_>>()
                    .join("\n");
                units.push(format!(
                    "export function useTarget{m}_{n}() {{\n  // block-{m}-{n}-marker\n{pad}\n  return alphaTarget({n});\n}}\n"
                ));
            }
            fs::write(d.path().join(format!("src/mod{m}.ts")), units.join("\n")).unwrap();
        }
        let out = fast_context(
            d.path(),
            serde_json::json!({
                "keywords":["alphaTarget"],
                "maxBytes":8192
            }),
        )
        .unwrap();
        assert!(out.len() <= 8192, "{}", out.len());
        let markers = (0..3)
            .flat_map(|m| (0..4).map(move |n| (m, n)))
            .filter(|(m, n)| out.contains(&format!("block-{m}-{n}-marker")))
            .count();
        assert!(markers >= 5, "markers={markers}\n{out}");
    }

    #[test]
    fn coupling_note_lists_co_changed_files() {
        let d = tempdir().unwrap();
        git(d.path(), &["init", "-q"]);
        git(
            d.path(),
            &["config", "user.email", "native-test@nova.local"],
        );
        git(d.path(), &["config", "user.name", "Nova Native Test"]);
        fs::create_dir_all(d.path().join("src")).unwrap();
        for round in 0..3 {
            fs::write(
                d.path().join("src/core.ts"),
                format!("export function coupledTarget() {{\n  return {round};\n}}\n"),
            )
            .unwrap();
            // registry.ts 与 core.ts 文本零耦合，但历史上总是同提交变更
            fs::write(
                d.path().join("src/registry.ts"),
                format!("export const registryVersion = {round};\n"),
            )
            .unwrap();
            git(d.path(), &["add", "-A"]);
            git(d.path(), &["commit", "-qm", "change"]);
        }
        let out = fast_context(
            d.path(),
            serde_json::json!({"keywords":["coupledTarget"],"coupling":true}),
        )
        .unwrap();
        assert!(out.contains("共改耦合(git)"), "{out}");
        assert!(out.contains("registry.ts"), "{out}");
        // 默认关闭：无提示行
        let plain =
            fast_context(d.path(), serde_json::json!({"keywords":["coupledTarget"]})).unwrap();
        assert!(!plain.contains("共改耦合(git)"), "{plain}");
    }

    #[test]
    fn companion_test_files_are_packed_by_default() {
        // 伴生测试：改实现默认带上共改的测试文件，即使任务文本未提测试、
        // 测试文件与关键词零重叠（noise_path 不再拦截伴生测试）。
        let d = tempdir().unwrap();
        git(d.path(), &["init", "-q"]);
        git(
            d.path(),
            &["config", "user.email", "native-test@nova.local"],
        );
        git(d.path(), &["config", "user.name", "Nova Native Test"]);
        fs::create_dir_all(d.path().join("src")).unwrap();
        for round in 0..3 {
            fs::write(
                d.path().join("src/core.ts"),
                format!("export function companionTarget() {{\n  return {round};\n}}\n"),
            )
            .unwrap();
            // core.test.ts 与 core.ts 高频共改，但正文与关键词零重叠
            fs::write(
                d.path().join("src/core.test.ts"),
                format!("assert.equal(coreResult, {round});\n"),
            )
            .unwrap();
            git(d.path(), &["add", "-A"]);
            git(d.path(), &["commit", "-qm", "change"]);
        }
        let out = fast_context(
            d.path(),
            serde_json::json!({"keywords":["companionTarget"],"task":"修改实现逻辑"}),
        )
        .unwrap();
        assert!(out.contains("### src/core.test.ts"), "{out}");
        // 命名约定兜底：无 git 历史时同目录 *.test.* 也要进闭包
        let d2 = tempdir().unwrap();
        fs::create_dir_all(d2.path().join("lib")).unwrap();
        fs::write(
            d2.path().join("lib/widget.ts"),
            "export function widgetTarget() {\n  return 1;\n}\n",
        )
        .unwrap();
        fs::write(
            d2.path().join("lib/widget.test.ts"),
            "expect(widgetResult).toBe(1);\n",
        )
        .unwrap();
        let out2 = fast_context(
            d2.path(),
            serde_json::json!({"keywords":["widgetTarget"],"task":"修改实现逻辑"}),
        )
        .unwrap();
        assert!(out2.contains("### lib/widget.test.ts"), "{out2}");
    }

    #[test]
    fn companion_tests_skip_weak_contains_seeds() {
        // contains 弱匹配种子（weight 1）不该触发共改伴生注入：跨语言同名符号的
        // 共改测试与查询零重叠，FULL 打包后会把真正的目标文件挤出 EDIT 区。
        let d = tempdir().unwrap();
        git(d.path(), &["init", "-q"]);
        git(
            d.path(),
            &["config", "user.email", "native-test@nova.local"],
        );
        git(d.path(), &["config", "user.name", "Nova Native Test"]);
        fs::create_dir_all(d.path().join("src")).unwrap();
        for round in 0..3 {
            // 只有 contains 弱匹配定义（missing_beta_view_defaults 含 beta_view），
            // 仓库中不存在 BetaView 的精确定义。
            fs::write(
                d.path().join("src/settings.ts"),
                format!("export function missing_beta_view_defaults() {{\n  return {round};\n}}\n"),
            )
            .unwrap();
            fs::write(
                d.path().join("src/settings.test.ts"),
                format!("assert.equal(settingsResult, {round});\n"),
            )
            .unwrap();
            fs::write(
                d.path().join("src/App.ts"),
                format!("// renders BetaView here\nexport function renderApp() {{\n  return {round};\n}}\n"),
            )
            .unwrap();
            git(d.path(), &["add", "-A"]);
            git(d.path(), &["commit", "-qm", "change"]);
        }
        let out = fast_context(d.path(), serde_json::json!({"keywords":["BetaView"]})).unwrap();
        assert!(!out.contains("# CTX MISS"), "{out}");
        assert!(!out.contains("### src/settings.test.ts"), "{out}");
    }
    #[test]
    fn fast_context_run_includes_sql_and_markdown_candidates() {
        let d = tempdir().unwrap();
        fs::create_dir_all(d.path().join("services/game")).unwrap();
        fs::create_dir_all(d.path().join("docs/queries")).unwrap();
        fs::write(
            d.path().join("services/game/minigame.go"),
            "package game\ntype MinigameDataQuery struct{}\n",
        )
        .unwrap();
        fs::write(
            d.path().join("docs/queries/minigame.md"),
            "# MinigameDataQuery\n```sql\nSELECT game_id, genre FROM minigame_data;\n```\n",
        )
        .unwrap();
        fs::write(
            d.path().join("docs/queries/minigame.sql"),
            "SELECT game_id, genre FROM minigame_data;\n",
        )
        .unwrap();

        let out = fast_context_run(
            d.path(),
            &serde_json::json!({"keywords":["MinigameDataQuery"]}),
        )
        .unwrap();
        assert!(out.contains("services/game/minigame.go"), "{out}");
        assert!(
            out.contains("docs/queries/minigame.sql") || out.contains("docs/queries/minigame.md"),
            "{out}"
        );
    }

    #[test]
    fn subject_named_file_survives_generic_word_noise() {
        // 目标文件名段命中查询词（planActionCard），但 "plan" 只以驼峰内嵌形式出现在
        // 标识符里，词边界检索零命中；泛词 "plan" 同时制造 300+ 行噪声命中（高频使
        // subject 衰减到候选门槛以下）。文件名 subject 文件应提为候选，不被挤出。
        let d = tempdir().unwrap();
        fs::create_dir_all(d.path().join("src")).unwrap();
        fs::write(
            d.path().join("src/planActionCard.tsx"),
            "export function PlanActionCard() {\n  return proposedPlanView;\n}\n",
        )
        .unwrap();
        for i in 0..12 {
            let mut body = String::from("export function noiseNotes() {\n");
            for n in 0..25 {
                body.push_str(&format!("  const plan{n} = \"plan mode {i}\";\n"));
            }
            body.push_str("  return plan0;\n}\n");
            fs::write(d.path().join(format!("src/notes{i}.ts")), body).unwrap();
        }
        let out = fast_context_run(
            d.path(),
            &serde_json::json!({"keywords":["plan mode"],"task":"remove plan mode selection"}),
        )
        .unwrap();
        assert!(out.contains("### src/planActionCard.tsx"), "{out}");
    }
}
