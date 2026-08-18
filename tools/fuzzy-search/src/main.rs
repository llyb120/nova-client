//! fuzzy-search — 独立模糊符号检索原型（零外部依赖）
//!
//! 用法：
//!   fuzzy-search "鉴权失败后的重试" [--limit 10] [--json] [--bench] [--root DIR]
//!   fuzzy-search "getIndx"     # 拼写纠错走 n-gram 通道
//!   fuzzy-search "AHR"         # 缩写走标识符通道
//!
//! 职责边界：只做模糊召回与排序，输出候选句柄 + 证据 + 置信度；
//! 高置信句柄交给 polaris 做精确解析（path + symbol + lines 即精确定位输入）。

mod gitignore;
mod scanner;
mod search;

use gitignore::GitignoreStack;
use scanner::{is_code_file, scan_source, Sym};
use search::{fuzzy_search, prepare, SearchResult, DEFAULT_ALIAS};
use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::{Instant, UNIX_EPOCH};

const EXCLUDE_DIRS: &[&str] = &[
    "node_modules", "dist", "target", "coverage", ".git", ".svn", ".hg", "vendor", "build",
    "out", ".next", ".nuxt", "__pycache__", ".cargo-target", ".idea", ".vscode",
];
const MAX_WALK_FILES: usize = 8000;
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
const CACHE_VERSION: u32 = 3;

// ---------------------------------------------------------------- 文件遍历

fn is_excluded_file(name: &str) -> bool {
    name.ends_with(".min.js")
        || name.ends_with(".lock")
        || name == "package-lock.json"
        || name.contains(".generated.")
}

fn read_gitignore(dir: &Path) -> Option<String> {
    fs::read_to_string(dir.join(".gitignore")).ok()
}

fn walk(dir: &Path, rel: &str, out: &mut Vec<String>, stack: &mut GitignoreStack) {
    if out.len() >= MAX_WALK_FILES {
        return;
    }
    let gi = read_gitignore(dir);
    stack.push(rel, gi.as_deref());
    let Ok(rd) = fs::read_dir(dir) else {
        stack.pop(rel);
        return;
    };
    let entries: Vec<_> = rd.flatten().collect();
    for e in entries {
        if out.len() >= MAX_WALK_FILES {
            break;
        }
        let name = e.file_name().to_string_lossy().into_owned();
        let r = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
        let Ok(ft) = e.file_type() else { continue };
        let is_dir = ft.is_dir();
        if is_dir {
            // 隐藏目录统一跳过（.git/.idea 等）；gitignore 优先于硬编码排除，后者作兜底
            if name.starts_with('.')
                || stack.is_ignored(&r, true)
                || EXCLUDE_DIRS.contains(&name.as_str())
            {
                continue;
            }
            walk(&e.path(), &r, out, stack);
        } else if ft.is_file() {
            if stack.is_ignored(&r, false) {
                continue;
            }
            if is_code_file(&name) && !is_excluded_file(&name) {
                out.push(r);
            }
        }
    }
    stack.pop(rel);
}

fn list_code_files(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = GitignoreStack::new();
    walk(root, "", &mut out, &mut stack);
    out.sort();
    out
}

// ---------------------------------------------------------------- 磁盘缓存（按 size+mtime 逐文件失效）

type CacheEntry = (u64, u64, u32, Vec<Sym>); // size, mtime_secs, mtime_nanos, syms

fn cache_path(root: &Path) -> PathBuf {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    root.to_string_lossy().to_lowercase().hash(&mut h);
    let dir = env::temp_dir().join("fuzzy-search-cache");
    let _ = fs::create_dir_all(&dir);
    dir.join(format!("symbols-{:x}-v{}.txt", h.finish(), CACHE_VERSION))
}

fn esc(s: &str) -> String {
    s.replace('\t', " ").replace('\n', " ")
}

fn save_cache(root: &Path, cache: &HashMap<String, CacheEntry>) {
    let mut s = String::from("#v1\n");
    let mut keys: Vec<_> = cache.keys().collect();
    keys.sort();
    for f in keys {
        let (size, sec, nano, syms) = &cache[f];
        s.push_str(&format!("F\t{size}\t{sec}\t{nano}\t{}\n", esc(f)));
        for y in syms {
            s.push_str(&format!(
                "S\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                y.ln,
                y.end,
                y.depth,
                y.kind,
                y.exp as u8,
                esc(&y.name),
                esc(&y.sig)
            ));
        }
    }
    let _ = fs::write(cache_path(root), s); // 缓存失败不影响功能
}

fn load_cache(root: &Path) -> HashMap<String, CacheEntry> {
    let mut out = HashMap::new();
    let Ok(text) = fs::read_to_string(cache_path(root)) else { return out };
    let mut cur_file: Option<String> = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("F\t") {
            let parts: Vec<&str> = rest.splitn(4, '\t').collect();
            if parts.len() != 4 {
                continue;
            }
            let (Ok(size), Ok(sec), Ok(nano)) = (
                parts[0].parse::<u64>(),
                parts[1].parse::<u64>(),
                parts[2].parse::<u32>(),
            ) else {
                continue;
            };
            cur_file = Some(parts[3].to_string());
            out.insert(parts[3].to_string(), (size, sec, nano, Vec::new()));
        } else if let Some(rest) = line.strip_prefix("S\t") {
            let Some(f) = &cur_file else { continue };
            let parts: Vec<&str> = rest.splitn(7, '\t').collect();
            if parts.len() != 7 {
                continue;
            }
            let (Ok(ln), Ok(end), Ok(depth)) = (
                parts[0].parse::<u32>(),
                parts[1].parse::<u32>(),
                parts[2].parse::<u32>(),
            ) else {
                continue;
            };
            let kind: &'static str = match parts[3] {
                "fn" => "fn",
                "class" => "class",
                "interface" => "interface",
                "type" => "type",
                "enum" => "enum",
                "struct" => "struct",
                "union" => "union",
                "trait" => "trait",
                "mod" => "mod",
                "impl" => "impl",
                "method" => "method",
                "prop" => "prop",
                _ => continue,
            };
            if let Some(ent) = out.get_mut(f) {
                ent.3.push(Sym {
                    ln,
                    end,
                    depth,
                    kind,
                    name: parts[5].to_string(),
                    sig: parts[6].to_string(),
                    exp: parts[4] == "1",
                });
            }
        }
    }
    out
}

pub struct IndexStats {
    pub total: usize,
    pub scanned: usize,
    pub elapsed_ms: f64,
}

/// 构建/更新符号缓存：只重扫过期文件。
fn build_symbol_cache(root: &Path, use_disk: bool) -> (HashMap<String, Vec<Sym>>, IndexStats) {
    let t0 = Instant::now();
    let list = list_code_files(root);
    let file_set: HashSet<&String> = list.iter().collect();
    let mut cache: HashMap<String, CacheEntry> = if use_disk { load_cache(root) } else { HashMap::new() };
    cache.retain(|f, _| file_set.contains(f));
    let mut scanned = 0;
    let mut result: HashMap<String, Vec<Sym>> = HashMap::new();
    for file in &list {
        let p = root.join(file.replace('/', std::path::MAIN_SEPARATOR_STR));
        let Ok(md) = fs::metadata(&p) else { continue };
        if md.len() > MAX_FILE_BYTES {
            continue;
        }
        let mtime = md
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| (d.as_secs(), d.subsec_nanos()))
            .unwrap_or((0, 0));
        if let Some(ent) = cache.get(file) {
            if ent.0 == md.len() && ent.1 == mtime.0 && ent.2 == mtime.1 {
                result.insert(file.clone(), ent.3.clone());
                continue;
            }
        }
        let Ok(text) = fs::read_to_string(&p) else { continue };
        let (_, syms) = scan_source(&text, file);
        cache.insert(file.clone(), (md.len(), mtime.0, mtime.1, syms.clone()));
        result.insert(file.clone(), syms);
        scanned += 1;
    }
    if use_disk && scanned > 0 {
        save_cache(root, &cache);
    }
    (
        result,
        IndexStats { total: list.len(), scanned, elapsed_ms: t0.elapsed().as_secs_f64() * 1000.0 },
    )
}

// ---------------------------------------------------------------- 输出

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn to_json(res: &SearchResult) -> String {
    let mut s = String::from("{\n");
    s.push_str(&format!(
        "  \"verdict\": \"{}\",\n  \"elapsedMs\": {:.1},\n  \"docCount\": {},\n  \"candidates\": [\n",
        res.verdict, res.elapsed_ms, res.doc_count
    ));
    for (i, c) in res.candidates.iter().enumerate() {
        let ev: Vec<String> = c.evidence.iter().map(|e| format!("\"{}\"", json_escape(e))).collect();
        s.push_str("    {\n");
        s.push_str(&format!(
            "      \"handle\": {{ \"path\": \"{}\", \"symbol\": \"{}\", \"kind\": \"{}\", \"lines\": [{}, {}] }},\n",
            json_escape(&c.path),
            json_escape(&c.symbol),
            c.kind,
            c.lines.0,
            c.lines.1
        ));
        s.push_str(&format!("      \"score\": {},\n", c.score));
        s.push_str(&format!("      \"confidence\": \"{}\",\n", c.confidence));
        s.push_str(&format!("      \"evidence\": [{}],\n", ev.join(", ")));
        s.push_str(&format!("      \"snippet\": \"{}\"\n", json_escape(&c.snippet)));
        s.push_str(if i + 1 < res.candidates.len() { "    },\n" } else { "    }\n" });
    }
    s.push_str("  ]\n}\n");
    s
}

fn print_human(res: &SearchResult) {
    println!(
        "verdict: {}  query: {:.1}ms  docs: {}",
        res.verdict, res.elapsed_ms, res.doc_count
    );
    if res.candidates.is_empty() {
        println!("(no candidates)");
        return;
    }
    for (i, c) in res.candidates.iter().enumerate() {
        println!(
            "#{} [{}] {}:{} {} {}",
            i + 1, c.score, c.path, c.lines.0, c.kind, c.symbol
        );
        println!("    evidence: {}", c.evidence.join(", "));
        println!("    sig: {}", c.snippet);
    }
}

// ---------------------------------------------------------------- CLI

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let mut root = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut limit = 10usize;
    let mut json = false;
    let mut bench = false;
    let mut query: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--root" => {
                i += 1;
                if i < args.len() {
                    root = PathBuf::from(&args[i]);
                }
            }
            "--limit" => {
                i += 1;
                if i < args.len() {
                    limit = args[i].parse().unwrap_or(10);
                }
            }
            "--json" => json = true,
            "--bench" => bench = true,
            a if !a.starts_with("--") && query.is_none() => query = Some(a.to_string()),
            _ => {}
        }
        i += 1;
    }
    if query.is_none() && !bench {
        eprintln!("usage: fuzzy-search \"query\" [--limit N] [--json] [--bench] [--root DIR]");
        std::process::exit(2);
    }

    let t_idx = Instant::now();
    let (files, stats) = build_symbol_cache(&root, true);
    let t_prep = Instant::now();
    let prep = prepare(&files);
    eprintln!(
        "index: {} files (+{} scanned, {:.0}ms), {} symbols, prepare {:.0}ms",
        stats.total,
        stats.scanned,
        stats.elapsed_ms,
        prep.docs.len(),
        t_prep.elapsed().as_secs_f64() * 1000.0
    );
    let _ = t_idx;

    if bench {
        let queries = [
            "getIndex",
            "getIndx",
            "scanSource",
            "鉴权重试",
            "context recall eval",
            "render message list",
            "STRIP",
        ];
        let _ = fuzzy_search(&prep, "warmup", 1, DEFAULT_ALIAS); // 预热
        for q in queries {
            let r = fuzzy_search(&prep, q, 5, DEFAULT_ALIAS);
            println!(
                "{:>8.1}ms  {:<6}  {:<24}  ->  {}",
                r.elapsed_ms,
                r.verdict,
                q,
                r.candidates.first().map(|c| c.symbol.as_str()).unwrap_or("(none)")
            );
        }
        return;
    }

    let res = fuzzy_search(&prep, query.as_deref().unwrap(), limit, DEFAULT_ALIAS);
    if json {
        print!("{}", to_json(&res));
    } else {
        print_human(&res);
    }
}
