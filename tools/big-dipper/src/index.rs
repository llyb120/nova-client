//! 索引构建：文件遍历（遵守 .gitignore）、符号磁盘缓存（size+mtime 逐文件失效）、检索文档与 JSON 输出。

use crate::gitignore::GitignoreStack;
use crate::scanner::{is_code_file, scan_source, Sym};
use crate::search::{prepare, Prepared, SearchResult};
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

pub fn list_code_files(root: &Path) -> Vec<String> {
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
    let dir = env::temp_dir().join("big-dipper-cache");
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
pub fn build_symbol_cache(root: &Path, use_disk: bool) -> (HashMap<String, Vec<Sym>>, IndexStats) {
    let t0 = Instant::now();
    let list = list_code_files(root);
    let file_set: HashSet<&String> = list.iter().collect();
    let mut cache: HashMap<String, CacheEntry> =
        if use_disk { load_cache(root) } else { HashMap::new() };
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

/// 一次性准备：建/更新符号缓存 + 构建检索文档与 BM25F 统计。
pub fn prepare_root(root: &Path, use_disk: bool) -> Prepared {
    let (files, _stats) = build_symbol_cache(root, use_disk);
    prepare(&files)
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

pub fn to_json(res: &SearchResult) -> String {
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

// ---------------------------------------------------------------- 交接协议（handoff）
//
// big_dipper_search 与 polaris（北极星）的交接契约：
//   high   → decision=expand，targets 取 candidates[0].handle，可直接调 polaris
//   medium → decision=disambiguate，targets 取前 3 个，由调用方选定后再调 polaris
//   low/none → decision=clarify/none，targets 为空，调用方应补充信息或报未找到
// 行号仅在同一次进程交接内有效；跨会话传递必须退回 path+symbol 重新定位。

pub fn handoff_json(res: &SearchResult, root: &Path) -> String {
    // snapshot：git 仓库取 HEAD commit，否则取内容戳（文件数:符号数）
    let snapshot = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(root)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("content:{}", res.doc_count));
    let (decision, take) = match res.verdict.as_str() {
        "high" => ("expand", 1),
        "medium" => ("disambiguate", 3),
        "low" => ("clarify", 0),
        _ => ("none", 0),
    };
    let mut s = String::from("{\n");
    s.push_str(&format!("  \"snapshot\": \"{}\",\n", json_escape(&snapshot)));
    s.push_str(&format!("  \"verdict\": \"{}\",\n", res.verdict));
    s.push_str(&format!("  \"decision\": \"{}\",\n", decision));
    s.push_str("  \"targets\": [\n");
    let picks: Vec<_> = res.candidates.iter().take(take).collect();
    for (i, c) in picks.iter().enumerate() {
        s.push_str(&format!(
            "    {{ \"path\": \"{}\", \"symbol\": \"{}\", \"kind\": \"{}\", \"lines\": [{}, {}], \"score\": {}, \"evidence\": [{}] }}{}\n",
            json_escape(&c.path),
            json_escape(&c.symbol),
            c.kind,
            c.lines.0,
            c.lines.1,
            c.score,
            c.evidence.iter().map(|e| format!("\"{}\"", json_escape(e))).collect::<Vec<_>>().join(", "),
            if i + 1 < picks.len() { "," } else { "" }
        ));
    }
    s.push_str("  ],\n");
    // 给调用方的下一步建议
    let hint = match decision {
        "expand" => "polaris_exact(targets, expand={definitions:true,directCallers:true}, budget=600)",
        "disambiguate" => "present targets to caller; on selection call polaris_exact with the chosen handle",
        "clarify" => "ask for module/path/kind hint and retry big_dipper_search",
        _ => "report not found; do NOT guess",
    };
    s.push_str(&format!("  \"next\": \"{}\"\n", hint));
    s.push_str("}\n");
    s
}

pub fn print_human(res: &SearchResult) {
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
