use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::UNIX_EPOCH;
use walkdir::WalkDir;

const CACHE_VERSION: u32 = 1;
const MAX_INDEX_FILE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_WALK_FILES: usize = 30_000;
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
const FULL_FILE_MAX: usize = 100;
const EXPLICIT_FULL_MAX: usize = 300;
const SUBJECT_FULL_MAX: usize = 800;
const MAX_SUBJECT_UNITS: usize = 30;
const MAX_FILES: usize = 6;
const MAX_DEPS: usize = 8;
const MAX_DEP_FILES: usize = 4;
const MAX_IMPACT: usize = 20;

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
    score: i64,
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

static MEMO: OnceLock<Mutex<HashMap<String, DiskCache>>> = OnceLock::new();

fn normalize_root(root: &Path) -> String {
    let value = root
        .canonicalize()
        .unwrap_or_else(|_| root.to_path_buf())
        .to_string_lossy()
        .replace('\\', "/");
    if cfg!(windows) {
        value.to_lowercase()
    } else {
        value
    }
}

fn cache_path(root: &Path) -> PathBuf {
    let normalized = normalize_root(root);
    let key = format!("{:x}", Sha256::digest(normalized.as_bytes()));
    let data = std::env::var_os("NOVA_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".nova")
        });
    data.join("codemap-v3-native")
        .join(&key[..16])
        .join("index.bin")
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

fn store_cache(root: &Path, cache: DiskCache) {
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
            | "rb"
            | "php"
    ) && !lower.contains("/node_modules/")
        && !lower.contains("/target/")
        && !lower.contains("/dist/")
        && !lower.contains("/coverage/")
        && !lower.contains("/vendor/")
}

fn list_code_files(root: &Path) -> Vec<String> {
    if let Ok(output) = Command::new("git")
        .args(["ls-files", "-c", "-o", "--exclude-standard", "-z"])
        .current_dir(root)
        .output()
    {
        if output.status.success() {
            let mut files: Vec<_> = output
                .stdout
                .split(|b| *b == 0)
                .filter_map(|raw| std::str::from_utf8(raw).ok())
                .map(normalize_rel)
                .filter(|f| is_code_file(f))
                .collect();
            files.sort();
            files.dedup();
            if !files.is_empty() {
                return files;
            }
        }
    }
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
            !matches!(
                entry.file_name().to_str().unwrap_or(""),
                ".git"
                    | "node_modules"
                    | "target"
                    | "dist"
                    | "coverage"
                    | "vendor"
                    | "build"
                    | "out"
                    | ".venv"
                    | "venv"
                    | "__pycache__"
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
    files.sort();
    files
}

fn rev(root: &Path, args: &[&str]) -> String {
    Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".into())
}

fn parse_rg_line(line: &str) -> Option<SearchRow> {
    let mut parts = line.splitn(3, ':');
    let file = normalize_rel(parts.next()?);
    let ln = parts.next()?.parse().ok()?;
    let text = parts.next()?.to_string();
    Some(SearchRow { file, ln, text })
}

fn search_text(
    root: &Path,
    terms: &[String],
    ignore_case: bool,
    word: bool,
    files: &[String],
) -> Vec<SearchRow> {
    if terms.is_empty() {
        return Vec::new();
    }
    let mut cmd = Command::new("rg");
    cmd.current_dir(root).args([
        "-n",
        "--no-heading",
        "--color",
        "never",
        "-F",
        "--path-separator",
        "/",
        "--max-count",
        &MAX_HITS_PER_FILE.to_string(),
    ]);
    if ignore_case {
        cmd.arg("-i");
    }
    if word {
        cmd.arg("-w");
    }
    for term in terms {
        cmd.args(["-e", term]);
    }
    for glob in [
        "!**/node_modules/**",
        "!**/dist/**",
        "!**/target/**",
        "!**/coverage/**",
        "!**/vendor/**",
    ] {
        cmd.args(["--glob", glob]);
    }
    cmd.arg(".");
    if let Ok(output) = cmd.output() {
        if output.status.success() || output.status.code() == Some(1) {
            if let Ok(text) = String::from_utf8(output.stdout) {
                return text
                    .lines()
                    .filter_map(parse_rg_line)
                    .take(MAX_HIT_LINES)
                    .collect();
            }
        }
    }
    let needles: Vec<_> = terms
        .iter()
        .map(|t| {
            if ignore_case {
                t.to_lowercase()
            } else {
                t.clone()
            }
        })
        .collect();
    let word_res: Vec<_> = if word {
        terms
            .iter()
            .filter_map(|t| {
                Regex::new(&format!(
                    r"(?{}:\b{}\b)",
                    if ignore_case { "i" } else { "" },
                    regex::escape(t)
                ))
                .ok()
            })
            .collect()
    } else {
        Vec::new()
    };
    let mut rows = Vec::new();
    for file in files {
        let Ok(text) = fs::read_to_string(root.join(file)) else {
            continue;
        };
        let mut count = 0;
        for (i, line) in text.lines().enumerate() {
            let matched = if word {
                word_res.iter().any(|re| re.is_match(line))
            } else {
                let hay = if ignore_case {
                    line.to_lowercase()
                } else {
                    line.to_string()
                };
                needles.iter().any(|needle| hay.contains(needle))
            };
            if matched {
                rows.push(SearchRow {
                    file: file.clone(),
                    ln: i + 1,
                    text: line.to_string(),
                });
                count += 1;
                if count >= MAX_HITS_PER_FILE || rows.len() >= MAX_HIT_LINES {
                    break;
                }
            }
        }
        if rows.len() >= MAX_HIT_LINES {
            break;
        }
    }
    rows
}

#[derive(Clone, Copy, PartialEq)]
enum LexState {
    Code,
    Single,
    Double,
    Template,
    BlockComment,
    Raw(usize),
}

fn stripped_depth(lines: &[String], rust: bool) -> (Vec<String>, Vec<usize>, Vec<usize>) {
    let mut state = LexState::Code;
    let mut depth = 0usize;
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
                LexState::Single => {
                    if c == '\\' {
                        i += 2;
                    } else if c == '\'' {
                        state = LexState::Code;
                        i += 1;
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
            if rust && c == 'r' {
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
                if rust {
                    let tail: String = chars[i..].iter().take(5).collect();
                    if Regex::new(r"^'(\\.|[^\\'])'").unwrap().is_match(&tail) {
                        state = LexState::Single;
                    } else {
                        out.push(c);
                    }
                } else {
                    state = LexState::Single;
                }
                i += 1;
                continue;
            }
            if c == '{' {
                depth += 1;
                out.push(c);
            } else if c == '}' {
                depth = depth.saturating_sub(1);
                out.push(c);
            } else {
                out.push(c);
            }
            i += 1;
        }
        if matches!(state, LexState::Single | LexState::Double) && !rust && !line.ends_with('\\') {
            state = LexState::Code;
        }
        code.push(out);
        after.push(depth);
    }
    (code, starts, after)
}

fn signature(line: &str) -> String {
    let compact = line.split_whitespace().collect::<Vec<_>>().join(" ");
    compact.chars().take(180).collect()
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
        (Regex::new(r"^(?:export\s+)?(?:declare\s+)?(?:interface|namespace|enum|type)\s+([A-Za-z_$][\w$]*)").unwrap(), "type"),
        (Regex::new(r"^(?:export\s+)?(?:declare\s+)?(?:const|let|var|static)\s+([A-Za-z_$][\w$]*)").unwrap(), "const"),
    ]);
    for (re, kind) in decls {
        if let Some(c) = re.captures(text) {
            return Some((c[1].to_string(), (*kind).into()));
        }
    }
    if depth > 0 {
        static METHOD: OnceLock<Regex> = OnceLock::new();
        let re = METHOD.get_or_init(|| Regex::new(r"^(?:(?:public|private|protected|readonly|static|async|get|set|override|abstract)\s+)*\*?\s*([A-Za-z_$][\w$]*)\s*(?:<[^>]*>)?\s*\(").unwrap());
        if let Some(c) = re.captures(text) {
            if !matches!(
                &c[1],
                "if" | "for"
                    | "while"
                    | "switch"
                    | "catch"
                    | "return"
                    | "match"
                    | "loop"
                    | "function"
                    | "new"
            ) {
                return Some((c[1].to_string(), "method".into()));
            }
        }
    }
    None
}

fn extract_imports(text: &str, file: &str) -> Vec<ImportRef> {
    let mut out = Vec::new();
    if file.ends_with(".rs") {
        let re = Regex::new(r"(?m)^\s*(?:pub\s+)?use\s+([^;]+);").unwrap();
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
                out.push(ImportRef {
                    name: local.into(),
                    from: format!("{}::{orig}", &raw[..pos]),
                });
            }
        }
        return out;
    }
    let re =
        Regex::new(r#"(?ms)^\s*import\s+(?:type\s+)?(.+?)\s+from\s*['\"]([^'\"]+)['\"]"#).unwrap();
    for cap in re.captures_iter(text) {
        let mut body = cap[1].trim().to_string();
        let spec = cap[2].to_string();
        if let Some(ns) = Regex::new(r"\*\s+as\s+([A-Za-z_$][\w$]*)")
            .unwrap()
            .captures(&body)
        {
            out.push(ImportRef {
                name: ns[1].into(),
                from: spec.clone(),
            });
        }
        if let Some(named) = Regex::new(r"\{([^}]*)\}").unwrap().captures(&body) {
            for part in named[1]
                .split(',')
                .map(|v| v.trim().trim_start_matches("type "))
                .filter(|v| !v.is_empty())
            {
                let pieces: Vec<_> = part.split_whitespace().collect();
                let name = if pieces.len() == 3 && pieces[1] == "as" {
                    pieces[2]
                } else {
                    part
                };
                if Regex::new(r"^[A-Za-z_$][\w$]*$").unwrap().is_match(name) {
                    out.push(ImportRef {
                        name: name.into(),
                        from: spec.clone(),
                    });
                }
            }
            body = body.replace(named.get(0).unwrap().as_str(), "");
        }
        if let Some(def) = Regex::new(r"[A-Za-z_$][\w$]*").unwrap().find(&body) {
            out.push(ImportRef {
                name: def.as_str().into(),
                from: spec,
            });
        }
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
        let re = Regex::new(r"^([ \t]*)(?:async[ \t]+)?(def|class)[ \t]+([A-Za-z_]\w*)").unwrap();
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
        let mut open = None;
        if after[i] > depth {
            open = Some(i);
        } else {
            for j in i + 1..(i + 14).min(lines.len()) {
                if after[j] > depth {
                    open = Some(j);
                    break;
                }
                if code[j].trim().is_empty() || code[j].trim_end().ends_with([';', ',']) {
                    break;
                }
            }
        }
        let end = if let Some(open) = open {
            (open..lines.len())
                .find(|j| after[*j] <= depth)
                .unwrap_or(lines.len() - 1)
        } else {
            i
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
) -> (IndexView, Vec<String>) {
    let all = list_code_files(root);
    let all_set: HashSet<_> = all.iter().cloned().collect();
    let mut cache = load_cache(root);
    let mut targets: HashSet<String> = wanted.cloned().unwrap_or_else(|| all_set.clone());
    let mut frontier: Vec<String> = targets.iter().cloned().collect();
    for depth in 0..=dependency_depth {
        let current = frontier;
        frontier = Vec::new();
        for file in current {
            let path = root.join(&file);
            let Some((size, modified_ns)) = metadata_stamp(&path) else {
                cache.files.remove(&file);
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
        cache.files.retain(|f, _| all_set.contains(f));
    }
    let selected: HashMap<_, _> = cache
        .files
        .iter()
        .filter(|(f, _)| wanted.is_none() || targets.contains(*f))
        .map(|(f, e)| (f.clone(), e.clone()))
        .collect();
    store_cache(root, cache);
    let mut view = IndexView {
        files: selected,
        ..Default::default()
    };
    for (file, entry) in &view.files {
        for symbol in &entry.syms {
            if symbol.kind != "prop" && symbol.depth <= 1 {
                view.defs
                    .entry(symbol.name.clone())
                    .or_default()
                    .push(Definition {
                        file: file.clone(),
                        symbol: symbol.clone(),
                    });
            }
        }
    }
    for (file, entry) in &view.files {
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
    (view, all)
}

fn source(root: &Path, file: &str, entry: Option<&FileEntry>) -> Option<Source> {
    let text = fs::read_to_string(root.join(file)).ok()?;
    let mut lines: Vec<_> = text
        .split('\n')
        .map(|v| v.trim_end_matches('\r').to_string())
        .collect();
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

fn clip(line: &str) -> String {
    if line.chars().count() <= MAX_LINE_CHARS {
        line.into()
    } else {
        format!("{}…", line.chars().take(MAX_LINE_CHARS).collect::<String>())
    }
}
fn contains_word(text: &str, word: &str) -> bool {
    Regex::new(&format!(r"\b{}\b", regex::escape(word))).is_ok_and(|r| r.is_match(text))
}
fn score_path(file: &str) -> i64 {
    let mut s = 0;
    if file.contains("/src/") || file.starts_with("src/") {
        s += 14;
    }
    if file.starts_with("scripts/") {
        s += 10;
    }
    if file.contains("test") || file.contains("spec") {
        s -= 55;
    }
    s
}
fn shown_ranges(plan: &PlannedFile) -> Vec<(usize, usize)> {
    if plan.full {
        vec![(1, plan.source.lines.len())]
    } else {
        plan.blocks.iter().map(|b| (b.start, b.end)).collect()
    }
}
fn covered(plan: &PlannedFile, line: usize) -> bool {
    shown_ranges(plan)
        .iter()
        .any(|(a, b)| line >= *a && line <= *b)
}

pub fn find_symbols(root: &Path, params: Value) -> Result<String, String> {
    let names: Vec<String> = params
        .get("names")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .take(12)
        .map(str::to_string)
        .collect();
    if names.is_empty() {
        return Ok("错误: names 不能为空".into());
    }
    let all = list_code_files(root);
    let rows = search_text(root, &names, false, true, &all);
    let wanted: HashSet<_> = rows.iter().map(|r| r.file.clone()).collect();
    let (index, _) = build_index(root, Some(&wanted), 0);
    let mut out = vec![format!(
        "# 符号定位 @{}",
        rev(root, &["rev-parse", "--short", "HEAD"])
    )];
    for name in names {
        let defs = index.defs.get(&name).cloned().unwrap_or_default();
        let hits: Vec<_> = rows
            .iter()
            .filter(|r| contains_word(&r.text, &name))
            .collect();
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
        for h in hits
            .iter()
            .filter(|h| !defs.iter().any(|d| d.file == h.file && d.symbol.ln == h.ln))
            .take(24)
        {
            out.push(format!(
                "    {}:{} {}",
                h.file,
                h.ln,
                h.text.trim().chars().take(110).collect::<String>()
            ));
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
    let (index, _) = build_index(root, None, 0);
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
    let branch = rev(root, &["rev-parse", "--abbrev-ref", "HEAD"]);
    let revision = rev(root, &["rev-parse", "--short", "HEAD"]);
    let mut out = vec![format!(
        "# CODEMAP {name} @{branch} {revision}  {} files  cache: native-bincode",
        files.len()
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
    let mut keywords: Vec<String> = params
        .get("keywords")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .take(5)
        .map(str::to_string)
        .collect();
    keywords.sort();
    keywords.dedup();
    let task = params
        .get("task")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .chars()
        .take(300)
        .collect::<String>();
    let token_re = Regex::new(r"[A-Za-z_$][\w$]{3,}").unwrap();
    let mut terms = keywords.clone();
    for m in token_re.find_iter(&task).take(5) {
        if !terms.iter().any(|v| v.eq_ignore_ascii_case(m.as_str())) {
            terms.push(m.as_str().into());
        }
    }
    let files: Vec<String> = params
        .get("files")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(normalize_rel)
        .filter(|v| !v.is_empty())
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
    let all = list_code_files(root);
    let mut rows = search_text(root, &terms, false, false, &all);
    let exact_hits: HashSet<_> = terms
        .iter()
        .filter(|term| rows.iter().any(|row| row.text.contains(term.as_str())))
        .cloned()
        .collect();
    if !exact_hits.is_empty() && exact_hits.len() < terms.len() {
        let missing = terms
            .iter()
            .filter(|term| !exact_hits.contains(*term))
            .cloned()
            .collect::<Vec<_>>();
        rows.extend(search_text(root, &missing, true, false, &all));
    }
    let mut hit_files: HashMap<String, Vec<SearchRow>> = HashMap::new();
    for row in rows {
        hit_files.entry(row.file.clone()).or_default().push(row);
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
    let definition_line = Regex::new(r"^\s*(?:(?:pub(?:\([^)]*\))?|export|async|unsafe|default|static|const|move)\s+)*(?:fn|struct|enum|trait|impl|type|class|interface|function|def|mod)\b").unwrap();
    let mut ranked: Vec<(String, i64)> = hit_files
        .iter()
        .map(|(f, r)| {
            (
                f.clone(),
                score_path(f)
                    + r.len().min(8) as i64 * 4
                    + if r.iter().any(|row| definition_line.is_match(&row.text)) {
                        160
                    } else {
                        0
                    }
                    + if subject.contains(f) { 600 } else { 0 }
                    + if files.contains(f) { 500 } else { 0 },
            )
        })
        .collect();
    for f in files.iter().chain(subject.iter()) {
        if !ranked.iter().any(|(x, _)| x == f) {
            ranked.push((f.clone(), if files.contains(f) { 1000 } else { 550 }));
        }
    }
    ranked.sort_by(|a, b| b.1.cmp(&a.1));
    ranked.dedup_by(|a, b| a.0 == b.0);
    if ranked.is_empty() {
        return Ok(format!("# CTX @{}\n无命中: {}\n提示: 换更短的符号名/字符串片段，或改用 find_symbols / grep 定位后用 read。",rev(root,&["rev-parse","--short","HEAD"]),terms.join(" ")));
    }
    let wanted: HashSet<_> = ranked
        .iter()
        .take(MAX_CANDIDATES)
        .map(|v| v.0.clone())
        .collect();
    let (index, _) = build_index(root, Some(&wanted), 1);
    let mut plans = Vec::<PlannedFile>::new();
    let mut sigs = Vec::<(String, usize, String)>::new();
    let mut used = 0usize;
    for (rank, (file, _)) in ranked.iter().take(MAX_FILES).enumerate() {
        let Some(src) = source(root, file, index.files.get(file)) else {
            continue;
        };
        let hits = hit_files.get(file).cloned().unwrap_or_default();
        let explicit = files.contains(file);
        let subj = subject.contains(file) && !file.contains("test") && !file.contains("spec");
        if (explicit && src.lines.len() <= EXPLICIT_FULL_MAX)
            || (subj && src.lines.len() <= SUBJECT_FULL_MAX)
            || (src.lines.len() <= FULL_FILE_MAX && rank < 3)
        {
            if used + src.lines.len() <= budget {
                used += src.lines.len();
                plans.push(PlannedFile {
                    file: file.clone(),
                    source: src,
                    section: "edit",
                    full: true,
                    blocks: Vec::new(),
                    rank,
                });
                continue;
            }
        }
        let mut blocks = BTreeMap::<(usize, usize), Block>::new();
        for hit in &hits {
            let chain: Vec<_> = src
                .syms
                .iter()
                .filter(|s| s.ln <= hit.ln && s.end >= hit.ln)
                .collect();
            if let Some(sym) = chain.last() {
                blocks.entry((sym.ln, sym.end)).or_insert(Block {
                    start: sym.ln,
                    end: sym.end,
                    label: format!("{} {}", sym.kind, sym.name),
                    tag: if keywords.iter().any(|keyword| {
                        sym.name.eq_ignore_ascii_case(keyword)
                            || (keyword.len() >= 5
                                && sym.name.to_lowercase().contains(&keyword.to_lowercase()))
                    }) {
                        "def"
                    } else {
                        "hit"
                    },
                    score: 100,
                });
            } else {
                let a = hit.ln.saturating_sub(8).max(1);
                let b = (hit.ln + 8).min(src.lines.len());
                blocks.entry((a, b)).or_insert(Block {
                    start: a,
                    end: b,
                    label: String::new(),
                    tag: "hit",
                    score: 20,
                });
            }
        }
        for kw in &keywords {
            for sym in src.syms.iter().filter(|s| {
                s.name.eq_ignore_ascii_case(kw)
                    || kw.len() >= 5 && s.name.to_lowercase().contains(&kw.to_lowercase())
            }) {
                blocks.entry((sym.ln, sym.end)).or_insert(Block {
                    start: sym.ln,
                    end: sym.end,
                    label: format!("{} {}", sym.kind, sym.name),
                    tag: "def",
                    score: 200,
                });
            }
        }
        if subj {
            let hit_lines = hits.iter().map(|hit| hit.ln).collect::<Vec<_>>();
            for sym in src
                .syms
                .iter()
                .filter(|symbol| {
                    symbol.depth <= 1
                        && !(symbol.kind == "const" && symbol.ln == symbol.end)
                        && !matches!(
                            symbol.name.to_ascii_lowercase().as_str(),
                            "test" | "tests" | "spec"
                        )
                })
                .take(MAX_SUBJECT_UNITS)
            {
                let has_hit = hit_lines
                    .iter()
                    .any(|line| *line >= sym.ln && *line <= sym.end);
                blocks.entry((sym.ln, sym.end)).or_insert(Block {
                    start: sym.ln,
                    end: sym.end,
                    label: format!("{} {}", sym.kind, sym.name),
                    tag: "hit",
                    score: if has_hit { 140 } else { 60 },
                });
            }
        }
        let mut selected = Vec::new();
        let mut options: Vec<_> = blocks.into_values().collect();
        options.sort_by(|a, b| b.score.cmp(&a.score));
        for block in options {
            let n = block.end - block.start + 1;
            if used + n <= budget {
                used += n;
                selected.push(block);
            } else {
                if let Some(sym) = src.syms.iter().find(|s| s.ln == block.start) {
                    sigs.push((file.clone(), sym.ln, sym.sig.clone()));
                }
            }
        }
        if !selected.is_empty() {
            plans.push(PlannedFile {
                file: file.clone(),
                source: src,
                section: "edit",
                full: false,
                blocks: selected,
                rank,
            });
        }
    }
    // Candidate definitions excluded by the file cap remain actionable through SIG.
    for (file, _) in ranked
        .iter()
        .skip(MAX_FILES)
        .take(MAX_CANDIDATES - MAX_FILES)
    {
        if let Some(entry) = index.files.get(file) {
            for symbol in &entry.syms {
                if symbol.depth > 1
                    || !keywords.iter().any(|keyword| {
                        symbol.name.eq_ignore_ascii_case(keyword)
                            || (keyword.len() >= 5
                                && symbol.name.to_lowercase().contains(&keyword.to_lowercase()))
                    })
                {
                    continue;
                }
                if !sigs
                    .iter()
                    .any(|(existing_file, line, _)| existing_file == file && *line == symbol.ln)
                {
                    sigs.push((file.clone(), symbol.ln, symbol.sig.clone()));
                }
            }
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
    let mut deps = Vec::<Definition>::new();
    for plan in plans.clone() {
        let mut text = String::new();
        for (a, b) in shown_ranges(&plan) {
            for line in &plan.source.lines[a - 1..b] {
                text.push_str(line);
                text.push('\n');
            }
        }
        for cap in Regex::new(r"[A-Za-z_$][A-Za-z0-9_$]{3,}")
            .unwrap()
            .find_iter(&text)
        {
            let name = cap.as_str();
            let local = index.defs.get(name).and_then(|defs| {
                defs.iter()
                    .find(|definition| definition.file == plan.file && definition.symbol.depth == 0)
            });
            let target = index.imports.get(&plan.file).and_then(|m| m.get(name));
            let def = local.or_else(|| {
                target.and_then(|f| {
                    index
                        .defs
                        .get(name)
                        .and_then(|v| v.iter().find(|d| &d.file == f))
                })
            });
            if let Some(def) = def {
                if !owned.contains(&(def.file.clone(), def.symbol.ln))
                    && !deps
                        .iter()
                        .any(|d| d.file == def.file && d.symbol.ln == def.symbol.ln)
                {
                    deps.push(def.clone());
                    if deps.len() >= MAX_DEPS {
                        break;
                    }
                }
            }
        }
    }
    for def in deps {
        if plans
            .iter()
            .filter(|p| p.section == "dep")
            .map(|p| &p.file)
            .collect::<HashSet<_>>()
            .len()
            >= MAX_DEP_FILES
            && !plans.iter().any(|p| p.file == def.file)
        {
            sigs.push((def.file, def.symbol.ln, def.symbol.sig));
            continue;
        }
        let Some(src) = source(root, &def.file, index.files.get(&def.file)) else {
            continue;
        };
        let n = def.symbol.end - def.symbol.ln + 1;
        if used + n > budget {
            sigs.push((def.file, def.symbol.ln, def.symbol.sig));
            continue;
        }
        used += n;
        if let Some(plan) = plans.iter_mut().find(|p| p.file == def.file) {
            plan.blocks.push(Block {
                start: def.symbol.ln,
                end: def.symbol.end,
                label: format!("{} {}", def.symbol.kind, def.symbol.name),
                tag: "dep",
                score: 10,
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
                    label: format!("{} {}", def.symbol.kind, def.symbol.name),
                    tag: "dep",
                    score: 10,
                }],
                rank: 99,
            });
        }
    }
    let render = |plans: &Vec<PlannedFile>, sigs: &Vec<(String, usize, String)>| {
        let mut body = Vec::new();
        for section in ["edit", "dep"] {
            let group: Vec<_> = plans.iter().filter(|p| p.section == section).collect();
            if group.is_empty() {
                continue;
            }
            body.push(if section == "edit" {
                "## EDIT".into()
            } else {
                "## DEPS (依赖定义, 完整单元)".into()
            });
            for p in group {
                if p.full {
                    body.push(format!("### {} ({}L) FULL", p.file, p.source.lines.len()));
                    body.extend(p.source.lines.iter().map(|l| clip(l)));
                } else {
                    let ranges = shown_ranges(p)
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
                        "### {} ({}L) shown={ranges}",
                        p.file,
                        p.source.lines.len()
                    ));
                    let mut blocks = p.blocks.clone();
                    blocks.sort_by_key(|b| b.start);
                    for b in blocks {
                        body.push(format!(
                            "@@ {}-{} {}{}",
                            b.start,
                            b.end,
                            b.label,
                            if b.tag == "hit" {
                                ""
                            } else {
                                if b.tag == "dep" {
                                    " [dep]"
                                } else {
                                    " [def]"
                                }
                            }
                        ));
                        body.extend(p.source.lines[b.start - 1..b.end].iter().map(|l| clip(l)));
                    }
                }
                body.push(String::new());
            }
        }
        let mut impacts = Vec::new();
        for (file, rows) in &hit_files {
            for row in rows {
                if plans.iter().any(|p| p.file == *file && covered(p, row.ln)) {
                    continue;
                }
                if impacts.len() < MAX_IMPACT {
                    impacts.push(format!(
                        "{}:{} {}",
                        file,
                        row.ln,
                        row.text.trim().chars().take(120).collect::<String>()
                    ));
                }
            }
        }
        if !impacts.is_empty() {
            body.push(format!(
                "## IMPACT (调用方/引用清单 {}/{}, 仅行; 确需函数体按 path:ln 补读)",
                impacts.len(),
                impacts.len()
            ));
            body.extend(impacts);
            body.push(String::new());
        }
        if !sigs.is_empty() {
            body.push("## SIG (预算内放不下或最终回退的定义, 仅签名)".into());
            for (f, l, s) in sigs {
                body.push(format!("{f}:{l} {s}"));
            }
            body.push(String::new());
        }
        let blocks: usize = plans
            .iter()
            .map(|p| if p.full { 1 } else { p.blocks.len() })
            .sum();
        let lines: usize = plans
            .iter()
            .map(|p| {
                shown_ranges(p)
                    .iter()
                    .map(|(a, b)| b - a + 1)
                    .sum::<usize>()
            })
            .sum();
        let mut head = format!(
            "# CTX {}{}{} @{}  {}文件/{}块 {}行",
            if keywords.is_empty() {
                "".into()
            } else {
                format!("q={}", keywords.join(","))
            },
            if task.is_empty() {
                "".into()
            } else {
                format!(" task=\"{}\"", task.chars().take(80).collect::<String>())
            },
            if files.is_empty() {
                "".into()
            } else {
                format!(" files={}", files.join(","))
            },
            rev(root, &["rev-parse", "--short", "HEAD"]),
            plans.len(),
            blocks,
            lines
        );
        let content = body.join("\n");
        head.push_str(&format!(" {:.1}KB\n# 契约: 以上均为完整单元/完整文件，可直接据此编辑；已展示行段禁止重读。SIG/IMPACT 仅索引，确需其函数体时按 path:ln 精确补读。\n\n",content.len()as f64/1024.0));
        format!("{head}{content}")
    };
    let mut text = render(&plans, &sigs);
    while text.len() > hard && !plans.is_empty() {
        let idx = plans
            .iter()
            .enumerate()
            .max_by_key(|(_, p)| (if p.section == "dep" { 1 } else { 0 }, p.rank))
            .map(|(i, _)| i)
            .unwrap();
        let mut p = plans.remove(idx);
        if p.full {
            for s in p.source.syms.iter().filter(|s| s.depth <= 1) {
                sigs.push((p.file.clone(), s.ln, s.sig.clone()));
            }
        } else if let Some(block) = p.blocks.pop() {
            if let Some(s) = p.source.syms.iter().find(|s| s.ln == block.start) {
                sigs.push((p.file.clone(), s.ln, s.sig.clone()));
            }
            if !p.blocks.is_empty() {
                plans.push(p);
            }
        }
        text = render(&plans, &sigs);
    }
    let _ = original_count;
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
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
