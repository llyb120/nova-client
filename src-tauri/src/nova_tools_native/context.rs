use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::{Instant, UNIX_EPOCH};
use walkdir::WalkDir;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

const CACHE_VERSION: u32 = 10;
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
const MAX_FILES: usize = 6;
const MAX_DEPS: usize = 8;
const MAX_DEP_FILES: usize = 4;
const MAX_IMPACT: usize = 20;
const MAX_GRAPH_TERMS: usize = 12;

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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FileEntry {
    size: u64,
    modified_ns: u128,
    total: usize,
    syms: Vec<Symbol>,
    imports: Vec<ImportRef>,
}

#[derive(Debug, Serialize, Deserialize)]
struct DiskCache {
    version: u32,
    root: String,
    files: HashMap<String, FileEntry>,
}

#[derive(Debug, Clone)]
struct SearchRow {
    file: String,
    ln: usize,
    text: String,
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
}

static MEMO: OnceLock<Mutex<HashMap<String, DiskCache>>> = OnceLock::new();

fn trace(label: &str, start: Instant) {
    if std::env::var_os("NOVA_TOOLS_NATIVE_PROFILE").is_some() {
        eprintln!(
            "[nova-tools-profile] {label}: {:.2}ms",
            start.elapsed().as_secs_f64() * 1000.0
        );
    }
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

fn cache_path(root: &Path) -> PathBuf {
    let key = workspace_cache_key(root);
    let data = std::env::var_os("NOVA_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".nova")
        });
    data.join("codemap-v3-native").join(key).join("index.bin")
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

fn load_cache(root: &Path) -> DiskCache {
    let key = normalize_root(root);
    let memo = MEMO.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(cache) = memo.lock().unwrap().remove(&key) {
        return cache;
    }
    let path = cache_path(root);
    if let Ok(bytes) = fs::read(path) {
        if let Ok(cache) = bincode::deserialize::<DiskCache>(&bytes) {
            if cache.version == CACHE_VERSION && cache.root == key {
                return cache;
            }
        }
    }
    DiskCache {
        version: CACHE_VERSION,
        root: key,
        files: HashMap::new(),
    }
}

fn store_cache(root: &Path, cache: DiskCache, persist: bool) {
    if persist {
        if let Ok(bytes) = bincode::serialize(&cache) {
            let path = cache_path(root);
            if let Some(parent) = path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            let temp = path.with_extension(format!("{}.tmp", std::process::id()));
            if fs::write(&temp, bytes).is_ok() {
                if path.exists() {
                    let _ = fs::remove_file(&path);
                }
                let _ = fs::rename(&temp, &path);
            }
        }
    }
    MEMO.get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap()
        .insert(cache.root.clone(), cache);
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

fn hidden_command(program: &str) -> Command {
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
            if is_code_file(&rel) {
                files.push(rel);
            }
        }
    }
    files
}

fn list_code_files(root: &Path) -> Vec<String> {
    const EXTENSIONS: &[&str] = &[
        "rs", "ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs", "go", "java", "kt", "kts",
        "swift", "c", "cc", "cpp", "cxx", "h", "hh", "hpp", "hxx", "cs", "php", "scala", "dart",
        "m", "mm", "zig", "vue", "svelte", "py", "pyi",
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
    if let Some(stdout) = run_command(root, "git", &args) {
        let mut seen = HashSet::new();
        let files: Vec<_> = stdout
            .split(|byte| *byte == 0)
            .filter_map(|raw| std::str::from_utf8(raw).ok())
            .map(normalize_rel)
            .filter(|file| is_code_file(file) && seen.insert(file.clone()))
            .collect();
        if !files.is_empty() {
            return files;
        }
    }
    walk_code_files(root)
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
) -> Vec<SearchRow> {
    let files = list_code_files(root);
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
    for file in files {
        let Ok(text) = fs::read_to_string(root.join(&file)) else {
            parts.push(Vec::new());
            continue;
        };
        let mut rows = Vec::new();
        for (index, line) in text.split('\n').enumerate() {
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

fn search_text(
    root: &Path,
    terms: &[String],
    ignore_case: bool,
    word: bool,
    _files: &[String],
) -> Vec<SearchRow> {
    let mut seen = HashSet::new();
    let terms = terms
        .iter()
        .filter(|term| !term.is_empty() && seen.insert((*term).clone()))
        .cloned()
        .collect::<Vec<_>>();
    if terms.is_empty() {
        return Vec::new();
    }
    static RG_AVAILABLE: OnceLock<bool> = OnceLock::new();
    let rg_available =
        *RG_AVAILABLE.get_or_init(|| run_command(root, "rg", &["--version".into()]).is_some());
    if rg_available {
        let mut args = vec![
            "-n".into(),
            "--no-heading".into(),
            "--color".into(),
            "never".into(),
            "-F".into(),
            "--max-count".into(),
            MAX_HITS_PER_FILE.to_string(),
        ];
        if ignore_case {
            args.push("-i".into());
        }
        if word {
            args.push("-w".into());
        }
        for term in &terms {
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
        args.push(".".into());
        if let Some(stdout) = run_command(root, "rg", &args) {
            return parse_search_rows(stdout);
        }
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
        if let Some(stdout) = run_command(root, "git", &args) {
            return parse_search_rows(stdout);
        }
    }
    search_in_process(root, &terms, ignore_case, word)
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
            let name = if pieces.len() == 3 && pieces[1] == "as" {
                pieces[2]
            } else {
                part
            };
            if ident_re
                .find(name)
                .is_some_and(|found| found.as_str() == name)
            {
                out.push(ImportRef {
                    name: name.into(),
                    from: spec.into(),
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
        if kind == "method" && end == i && !stripped.trim_end().ends_with(['(', '{']) {
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

fn build_index(
    root: &Path,
    wanted: Option<&HashSet<String>>,
    dependency_depth: usize,
    known_files: Option<&[String]>,
) -> (IndexView, Vec<String>) {
    let all = known_files
        .map(<[String]>::to_vec)
        .unwrap_or_else(|| list_code_files(root));
    let all_set: HashSet<_> = all.iter().cloned().collect();
    let mut cache = load_cache(root);
    let mut targets: HashSet<String> = wanted.cloned().unwrap_or_else(|| all_set.clone());
    let mut frontier: Vec<String> = targets.iter().cloned().collect();
    let mut dirty = false;
    for depth in 0..=dependency_depth {
        let current = frontier;
        frontier = Vec::new();
        for file in current {
            let path = root.join(&file);
            let Some((size, modified_ns)) = metadata_stamp(&path) else {
                dirty |= cache.files.remove(&file).is_some();
                continue;
            };
            let stale = cache
                .files
                .get(&file)
                .map(|e| e.size != size || e.modified_ns != modified_ns)
                .unwrap_or(true);
            if stale {
                if let Ok(text) = fs::read_to_string(&path) {
                    let mut entry = scan_source(&text, &file);
                    entry.size = size;
                    entry.modified_ns = modified_ns;
                    cache.files.insert(file.clone(), entry);
                    dirty = true;
                }
            }
            if depth < dependency_depth {
                for import in cache
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
        let before = cache.files.len();
        cache.files.retain(|f, _| all_set.contains(f));
        dirty |= cache.files.len() != before;
    }
    let selected = cache.files.clone();
    store_cache(root, cache, dirty);
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
    (view, all)
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
    if !is_code_file(file) {
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
    const MAX_TASK_TOKENS: usize = 1250;
    const CJK_NGRAM_MIN: usize = 2;
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
    for found in token_re.find_iter(task) {
        add(found.as_str().to_string());
    }
    let mut phrase = Vec::new();
    let flush = |phrase: &mut Vec<char>, add: &mut dyn FnMut(String)| {
        if phrase.len() >= CJK_NGRAM_MIN {
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
        let mut text = String::new();
        for (start, end) in shown_ranges(plan) {
            text.push_str(&plan.source.lines[start - 1..end].join("\n"));
        }
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

pub fn find_symbols(root: &Path, params: Value) -> Result<String, String> {
    let mut seen_names = HashSet::new();
    let names: Vec<String> = params
        .get("names")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty() && seen_names.insert((*value).to_string()))
        .take(12)
        .map(str::to_string)
        .collect();
    if names.is_empty() {
        return Ok("错误: names 不能为空".into());
    }
    let (all, rows, revision) = std::thread::scope(|scope| {
        let files = scope.spawn(|| list_code_files(root));
        let search = scope.spawn(|| search_text(root, &names, false, true, &[]));
        let revision = scope.spawn(|| short_rev(root));
        (
            files.join().unwrap_or_default(),
            search.join().unwrap_or_default(),
            revision.join().unwrap_or_else(|_| "unknown".into()),
        )
    });
    let wanted: HashSet<_> = rows.iter().map(|r| r.file.clone()).collect();
    let (index, _) = build_index(root, Some(&wanted), 0, Some(&all));
    let mut out = vec![format!("# 符号定位 @{revision}")];
    for name in names {
        let defs = index.defs.get(&name).cloned().unwrap_or_default();
        let word_re = Regex::new(&format!(r"\b{}\b", regex::escape(&name))).unwrap();
        let hits: Vec<_> = rows.iter().filter(|r| word_re.is_match(&r.text)).collect();
        out.push(String::new());
        out.push(format!(
            "## {name}  defs={} refs={}",
            defs.len(),
            hits.len()
        ));
        for d in defs.iter().take(6) {
            out.push(format!(
                "DEF {}:{}-{} {}",
                d.file, d.symbol.ln, d.symbol.end, d.symbol.sig
            ));
        }
        let rest = hits
            .iter()
            .filter(|h| !defs.iter().any(|d| d.file == h.file && d.symbol.ln == h.ln))
            .collect::<Vec<_>>();
        for h in rest.iter().take(24) {
            out.push(format!(
                "    {}:{} {}",
                h.file,
                h.ln,
                js_utf16_slice(h.text.trim(), 110)
            ));
        }
        if rest.len() > 24 {
            out.push(format!("    … +{}", rest.len() - 24));
        }
        if defs.is_empty() && hits.is_empty() {
            out.push("(无命中)".into());
        }
    }
    Ok(out.join("\n"))
}

pub fn code_map(root: &Path, params: Value) -> Result<String, String> {
    let scope = normalize_rel(
        params
            .get("scope")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim(),
    );
    let (index, _) = build_index(root, None, 0, None);
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

pub fn fast_context(root: &Path, params: Value) -> Result<String, String> {
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
    let mut terms = keywords.clone();
    for token in task_tokens(&task) {
        if !terms.iter().any(|value| value.eq_ignore_ascii_case(&token)) {
            terms.push(token);
        }
    }
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
    let (mut all, mut rows, revision) = std::thread::scope(|scope| {
        let files = scope.spawn(|| list_code_files(root));
        let search = scope.spawn(|| {
            if terms.is_empty() {
                Vec::new()
            } else {
                search_text(root, &terms, false, false, &[])
            }
        });
        let revision = scope.spawn(|| short_rev(root));
        (
            files.join().unwrap_or_default(),
            search.join().unwrap_or_default(),
            revision.join().unwrap_or_else(|_| "unknown".into()),
        )
    });
    trace("fast_context.search_and_files", stage);
    let exact_hits: HashSet<_> = terms
        .iter()
        .filter(|term| rows.iter().any(|row| row.text.contains(term.as_str())))
        .cloned()
        .collect();
    let missing = terms
        .iter()
        .filter(|term| !exact_hits.contains(*term))
        .cloned()
        .collect::<Vec<_>>();
    let mut loose_kw = Vec::new();
    if !missing.is_empty() {
        let extra = search_text(root, &missing, true, false, &all);
        for term in &missing {
            let lower = term.to_lowercase();
            if extra
                .iter()
                .any(|row| row.text.to_lowercase().contains(&lower))
            {
                loose_kw.push(term.clone());
            }
        }
        rows.extend(extra);
    }
    let missed_all = keywords
        .iter()
        .filter(|keyword| {
            !rows
                .iter()
                .any(|row| row.text.to_lowercase().contains(&keyword.to_lowercase()))
        })
        .cloned()
        .collect::<Vec<_>>();
    for file in rows.iter().map(|row| &row.file).chain(files.iter()) {
        if !all.contains(file) && root.join(file).is_file() && is_code_file(file) {
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
    let subject: HashSet<_> = all
        .iter()
        .filter(|f| {
            let base = f.rsplit('/').next().unwrap_or(f).to_lowercase();
            terms
                .iter()
                .any(|t| t.len() >= 4 && base.contains(&t.to_lowercase()))
        })
        .cloned()
        .collect();
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
                    + if subject.contains(f) { 600 } else { 0 }
                    + if files.contains(f) { 500 } else { 0 },
            )
        })
        .collect();
    for f in files
        .iter()
        .chain(all.iter().filter(|file| subject.contains(*file)))
    {
        if !preliminary.iter().any(|(x, _)| x == f) {
            preliminary.push((f.clone(), if files.contains(f) { 1000 } else { 550 }));
        }
    }
    preliminary.sort_by(|a, b| b.1.cmp(&a.1));
    preliminary.dedup_by(|a, b| a.0 == b.0);
    if preliminary.is_empty() {
        return Ok(format!("# CTX @{}\n无命中: {}\n提示: 换更短的符号名/字符串片段，或改用 find_symbols / grep 定位后用 read。",short_rev(root),terms.join(" ")));
    }
    let mut candidates = preliminary
        .iter()
        .filter(|(file, _)| is_code_file(file) || files.contains(file))
        .take(MAX_CANDIDATES)
        .cloned()
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        candidates = preliminary.iter().take(3).cloned().collect();
    }
    let wanted: HashSet<_> = candidates.iter().map(|value| value.0.clone()).collect();
    let stage = Instant::now();
    let (index, _) = build_index(root, Some(&wanted), 2, Some(&all));
    trace("fast_context.index", stage);
    let stage = Instant::now();
    let mut def_names = index.defs.keys().cloned().collect::<Vec<_>>();
    def_names.sort();
    let mut seeds = Vec::<(Definition, String, usize)>::new();
    let mut seed_positions = HashMap::<(String, usize), usize>::new();
    for keyword in &keywords {
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
    if !planned_terms.is_empty() {
        let planned_rows = search_text(root, &planned_terms, false, false, &all);
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
            if subject.contains(file) {
                score += 600.0;
            }
            if files.contains(file) {
                score += 500.0;
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
    for file in &all {
        if subject.contains(file) && !ranked.iter().any(|(existing, _)| existing == file) {
            ranked.push((file.clone(), 550.0));
        }
    }
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let candidate_limit =
        (MAX_CANDIDATES + hard.saturating_sub(DEFAULT_HARD_BYTES) / 8192 * 2).min(20);
    let file_limit = (MAX_FILES + hard.saturating_sub(DEFAULT_HARD_BYTES) / 16384 * 2).min(12);
    let units_per_file =
        (MAX_UNITS_PER_FILE + hard.saturating_sub(DEFAULT_HARD_BYTES) / 16384).min(8);
    let mut final_candidates = ranked
        .iter()
        .filter(|(file, _)| is_code_file(file) || files.contains(file))
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
    let task_tokens = terms
        .iter()
        .skip(keywords.len())
        .map(|value| value.to_lowercase())
        .collect::<Vec<_>>();
    let mut units = Vec::<UnitCandidate>::new();
    for (file, file_score) in &final_candidates {
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
                    },
                ));
                grouped.len() - 1
            });
            grouped[index].1.tag = "def";
            grouped[index].1.seed_weight = grouped[index].1.seed_weight.max(*weight);
        }
        for (_, mut unit) in grouped {
            let size = unit.end - unit.start + 1;
            let body = source.lines[unit.start - 1..unit.end].join("\n");
            let references_seed = seed_names.iter().any(|name| body.contains(name));
            let calls_seed = seed_names
                .iter()
                .any(|name| body.contains(&format!("{name}(")));
            let planned_relation = planned_terms.iter().any(|name| body.contains(name));
            unit.role = if unit.tag == "def" {
                "target"
            } else if plan_intent.tests && noise_path(file) && references_seed {
                "test"
            } else if plan_intent.errors && planned_relation {
                "handler"
            } else if plan_intent.callers && calls_seed {
                "caller"
            } else {
                "related"
            };
            unit.required = matches!(unit.role, "target" | "handler" | "caller" | "test");
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
            let estimated_bytes = body.len().max(96) as f64;
            unit.utility = score / (estimated_bytes / 1024.0).max(1.0);
            units.push(unit);
        }
    }
    units.sort_by(|a, b| {
        b.required
            .cmp(&a.required)
            .then_with(|| {
                b.utility
                    .partial_cmp(&a.utility)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });
    let mut plans = Vec::<PlannedFile>::new();
    let mut sigs = Vec::<(String, usize, String)>::new();
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
    let subject_list = final_candidates
        .iter()
        .map(|(file, _)| file)
        .filter(|file| subject.contains(*file) && !noise_path(file) && sources.contains_key(*file))
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
    let closure_roles = units
        .iter()
        .map(|unit| (unit.file.clone(), unit.start, unit.role))
        .collect::<Vec<_>>();
    for unit in units {
        let Some(source) = sources.get(&unit.file).cloned() else {
            continue;
        };
        let plan_index = plans.iter().position(|plan| plan.file == unit.file);
        if plan_index.is_some_and(|index| plans[index].full) {
            continue;
        }
        if plan_index.is_none() && plans.len() >= file_limit {
            continue;
        }
        if plan_index.is_some_and(|index| plans[index].blocks.len() >= units_per_file) {
            continue;
        }
        if !unit.hits.is_empty()
            && unit
                .hits
                .iter()
                .all(|line| plan_index.is_some_and(|index| covered(&plans[index], *line)))
        {
            continue;
        }
        if plan_index.is_none()
            && source.lines.len() <= FULL_FILE_MAX
            && (*file_rank.get(&unit.file).unwrap_or(&9) < 3 || unit.tag == "def")
        {
            let cost = range_cost(&source, 1, source.lines.len());
            if used + source.lines.len() <= budget && used_bytes + cost <= soft_bytes {
                used += source.lines.len();
                used_bytes += cost;
                let rank = *file_rank.get(&unit.file).unwrap_or(&99);
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
        if used + lines > budget || used_bytes + cost > soft_bytes {
            if let Some(symbol) = &unit.unit {
                push_sig(&mut sigs, &unit.file, unit.start, &symbol.sig);
            }
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
            required: unit.required,
        });
        used += lines;
        used_bytes += cost;
    }
    let original_count = plans.len();
    let mut owned: HashSet<_> = plans
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
        .chain(terms.iter().skip(keywords.len()))
        .cloned()
        .collect::<HashSet<_>>();
    let mut dep_seen = HashSet::<(String, usize)>::new();
    for dep_depth in 0..2 {
        let deps = collect_dependencies(&index, &plans, &owned, &keyword_set);
        let mut added = false;
        for (dep_name, def) in deps {
            let dep_key = (def.file.clone(), def.symbol.ln);
            if dep_seen.contains(&dep_key) || (dep_depth > 0 && !def.symbol.exp) {
                continue;
            }
            dep_seen.insert(dep_key.clone());
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
            if used + n > budget || used_bytes + bytes > soft_bytes {
                push_sig(&mut sigs, &def.file, def.symbol.ln, &def.symbol.sig);
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
                    file: def.file,
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
                    rank: 99,
                });
            }
            owned.insert(dep_key);
            added = true;
        }
        if !added {
            break;
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
                if plan.full {
                    body.push(format!(
                        "### {} ({}L) FULL",
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
                        "### {} ({}L) shown={shown}",
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
                    let subject_file = subject.contains(&plan.file);
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
        let mut total_refs = 0usize;
        for (file, _) in &ranked {
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
                let seed_ref = if seed_names.is_empty() {
                    keywords.iter().any(|keyword| row.text.contains(keyword))
                } else {
                    seed_names.iter().any(|name| row.text.contains(name))
                };
                if !seed_ref {
                    continue;
                }
                total_refs += 1;
                if impacts.len() < impact_limit {
                    impacts.push(format!(
                        "{}:{} {}",
                        file,
                        row.ln,
                        js_utf16_slice(row.text.trim(), 120)
                    ));
                }
            }
        }
        if !impacts.is_empty() {
            body.push(format!(
                "## IMPACT (调用方/引用清单 {}/{total_refs}, 仅行; 确需函数体按 path:ln 补读)",
                impacts.len()
            ));
            body.extend(impacts);
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
                .count()
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
            let unexpanded = ranked
                .iter()
                .filter(|(file, _)| !plans.iter().any(|plan| plan.file == *file))
                .map(|(file, _)| file)
                .collect::<Vec<_>>();
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
                    sigs.push((removed.file, 1, sig));
                }
            }
        } else {
            let block = plans[index]
                .blocks
                .iter()
                .enumerate()
                .min_by(|(_, a), (_, b)| {
                    a.required.cmp(&b.required).then_with(|| {
                        a.score
                            .partial_cmp(&b.score)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    })
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
        assert!(tokens.len() <= 1250);
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
    fn large_subject_packs_hit_and_later_units_without_test_noise() {
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
        assert!(out.contains("export function laterFlow"), "{out}");
        assert!(out.find("src/zebra_engine.ts") < out.find("src/zebra.test.ts"));
    }

    #[test]
    fn tight_budget_downgrades_whole_units_to_signatures() {
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
        assert!(out.contains("## SIG"), "{out}");
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
        assert!(list_code_files(d.path()).contains(&"src/new.ts".to_string()));
    }

    #[test]
    fn symbol_locations_and_code_map_distinguish_defs() {
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
        let symbols = find_symbols(d.path(), serde_json::json!({"names":["pick"]})).unwrap();
        assert!(symbols.contains("defs=1 refs=3"), "{symbols}");
        assert!(symbols.contains("DEF src/a.ts:1-3"), "{symbols}");
        let map = code_map(d.path(), serde_json::json!({"scope":"src/"})).unwrap();
        assert!(map.contains("## src/a.ts (3L)"), "{map}");
        assert!(map.contains("1 pick"), "{map}");
    }
}
