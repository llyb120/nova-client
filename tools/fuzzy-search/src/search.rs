//! 模糊检索核心：拆词 / 三通道（BM25F + 标识符 + n-gram）/ RRF 融合 / 置信度门控。

use crate::scanner::Sym;
use std::collections::{HashMap, HashSet};
use std::time::Instant;

// ---------------------------------------------------------------- 拆词 / 缩写 / n-gram

/// camelCase / PascalCase / snake_case / 数字边界 拆成 lowercase 子词（长度 >= 2）。
pub fn split_ident(name: &str) -> Vec<String> {
    let chars: Vec<char> = name.chars().collect();
    let cls = |c: char| -> u8 {
        if c.is_ascii_uppercase() {
            2
        } else if c.is_ascii_lowercase() {
            1
        } else if c.is_ascii_digit() {
            3
        } else {
            0
        }
    };
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    for (i, &c) in chars.iter().enumerate() {
        let cc = cls(c);
        if cc == 0 {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            continue;
        }
        if !cur.is_empty() {
            let prev = cls(chars[i - 1]);
            let next = chars.get(i + 1).map(|&x| cls(x)).unwrap_or(0);
            let boundary = match (prev, cc) {
                (1, 2) => true,          // aB
                (3, 2) => true,          // 1B
                (3, 1) => true,          // 1a
                (1, 3) | (2, 3) => true, // a1 B1
                (2, 2) => next == 1,     // HTTPServer：在下一个小写前断开
                _ => false,
            };
            if boundary {
                out.push(std::mem::take(&mut cur));
            }
        }
        cur.push(c.to_ascii_lowercase());
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out.retain(|t| t.chars().count() >= 2);
    out
}

fn dedupe(v: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    v.into_iter().filter(|t| seen.insert(t.clone())).collect()
}

/// 路径 → 目录/文件名子词（去扩展名）。
pub fn path_tokens(file: &str) -> Vec<String> {
    let mut out = Vec::new();
    for part in file.to_lowercase().split('/') {
        let stem = match part.rfind('.') {
            Some(p) => &part[..p],
            None => part,
        };
        out.extend(split_ident(stem));
    }
    dedupe(out)
}

/// 签名中的标识符子词。
fn sig_tokens(sig: &str) -> Vec<String> {
    let mut idents = Vec::new();
    let mut cur = String::new();
    for c in sig.chars() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '$' {
            cur.push(c);
        } else if !cur.is_empty() {
            idents.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        idents.push(cur);
    }
    dedupe(idents.iter().flat_map(|i| split_ident(i)).collect())
}

/// 驼峰缩写：AuthHttpRetry → ahr。
pub fn acronym(name: &str) -> String {
    split_ident(name).iter().filter_map(|t| t.chars().next()).collect()
}

/// 字符 3-gram 集合（小写、首尾补齐）。
pub fn trigrams(s: &str) -> HashSet<String> {
    let padded = format!(" {} ", s.to_lowercase());
    let chars: Vec<char> = padded.chars().collect();
    chars.windows(3).map(|w| w.iter().collect()).collect()
}

// ---------------------------------------------------------------- 中英文别名（原型内置小表，可外置扩展）

pub const DEFAULT_ALIAS: &[(&str, &[&str])] = &[
    ("鉴权", &["auth", "token"]),
    ("认证", &["auth"]),
    ("登录", &["login", "auth"]),
    ("重试", &["retry"]),
    ("缓存", &["cache"]),
    ("日志", &["log"]),
    ("配置", &["config", "settings"]),
    ("设置", &["settings"]),
    ("搜索", &["search", "query"]),
    ("查询", &["query", "search"]),
    ("索引", &["index"]),
    ("模糊", &["fuzzy"]),
    ("文件", &["file"]),
    ("目录", &["dir", "path"]),
    ("路径", &["path"]),
    ("上下文", &["context", "ctx"]),
    ("符号", &["symbol"]),
    ("工具", &["tool"]),
    ("消息", &["message"]),
    ("会话", &["session"]),
    ("对话", &["chat", "conversation"]),
    ("模型", &["model"]),
    ("终端", &["terminal"]),
    ("编辑器", &["editor"]),
    ("窗口", &["window"]),
    ("主题", &["theme"]),
    ("快捷键", &["keybinding", "shortcut"]),
    ("插件", &["plugin"]),
    ("测试", &["test"]),
    ("构建", &["build"]),
    ("打包", &["package", "bundle"]),
    ("更新", &["update"]),
    ("失败", &["fail", "error"]),
    ("错误", &["error"]),
    ("超时", &["timeout"]),
    ("权限", &["permission"]),
];

// ---------------------------------------------------------------- 查询解析

pub struct Parsed {
    pub idents: Vec<String>,
    pub terms: Vec<String>,
    pub exact: Vec<String>,
    pub path_hints: Vec<String>,
}

fn cjk_segments(q: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for c in q.chars() {
        if ('\u{4e00}'..='\u{9fff}').contains(&c) {
            cur.push(c);
        } else if !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// 从自然语言查询中抽取信号：标识符 token、BM25 词项、引号精确串、路径软提示、中文别名扩展。
pub fn parse_query(q: &str, alias: &[(&str, &[&str])]) -> Parsed {
    // 引号精确串
    let mut exact = Vec::new();
    let chars: Vec<char> = q.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '"' || chars[i] == '\'' {
            let q0 = chars[i];
            let mut buf = String::new();
            let mut j = i + 1;
            while j < chars.len() && chars[j] != q0 {
                buf.push(chars[j]);
                j += 1;
            }
            if j < chars.len() && !buf.is_empty() {
                exact.push(buf);
            }
            i = j + 1;
        } else {
            i += 1;
        }
    }
    // 标识符 token
    let mut raw_idents = Vec::new();
    let mut cur = String::new();
    let mut flush = |cur: &mut String, out: &mut Vec<String>| {
        if cur.chars().count() >= 2 {
            out.push(std::mem::take(cur));
        } else {
            cur.clear();
        }
    };
    for c in q.chars() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '$' {
            cur.push(c);
        } else {
            flush(&mut cur, &mut raw_idents);
        }
    }
    flush(&mut cur, &mut raw_idents);
    // 路径软提示：含 `/` 的片段
    let path_hints: Vec<String> = q
        .split(char::is_whitespace)
        .filter(|t| t.contains('/') && t.len() > 1)
        .map(|t| t.to_lowercase())
        .collect();
    // 中文别名扩展
    let mut aliased = Vec::new();
    for seg in cjk_segments(q) {
        for (zh, en) in alias {
            if seg.contains(zh) {
                aliased.extend(en.iter().map(|s| s.to_string()));
            }
        }
    }
    let terms = dedupe(
        raw_idents
            .iter()
            .flat_map(|t| split_ident(t))
            .chain(exact.iter().flat_map(|e| split_ident(e)))
            .chain(aliased.iter().cloned())
            .collect(),
    );
    let idents = dedupe(
        raw_idents
            .iter()
            .map(|t| t.to_lowercase())
            .chain(aliased.iter().cloned())
            .collect(),
    );
    Parsed { idents, terms, exact, path_hints }
}

// ---------------------------------------------------------------- 文档构建

pub struct Doc {
    pub id: usize,
    pub file: String,
    pub name: String,
    pub kind: String,
    pub ln: u32,
    pub end: u32,
    pub sig: String,
    pub noise: bool,
    pub name_subs: Vec<String>,
    pub acr: String,
    pub tri: HashSet<String>,
    pub fields: [Vec<String>; 3], // name / path / sig
}

fn is_noise_path(file: &str) -> bool {
    let f = file.to_lowercase();
    f.contains(".test.")
        || f.contains(".spec.")
        || f.contains("/__tests__/")
        || f.contains("/tests/")
        || f.contains("/test/")
}

pub struct Prepared {
    pub docs: Vec<Doc>,
    pub bm25: Bm25f,
}

/// 由符号表构建检索文档 + BM25F 统计（一次性，常驻内存复用）。
pub fn prepare(files: &HashMap<String, Vec<Sym>>) -> Prepared {
    let mut docs = Vec::new();
    for (file, syms) in files {
        for s in syms {
            let name_subs = split_ident(&s.name);
            let mut name_field = name_subs.clone();
            name_field.push(s.name.to_lowercase());
            docs.push(Doc {
                id: docs.len(),
                file: file.clone(),
                name: s.name.clone(),
                kind: s.kind.to_string(),
                ln: s.ln,
                end: s.end,
                sig: s.sig.clone(),
                noise: is_noise_path(file),
                acr: acronym(&s.name),
                tri: trigrams(&s.name),
                name_subs,
                fields: [name_field, path_tokens(file), sig_tokens(&s.sig)],
            });
        }
    }
    let bm25 = Bm25f::new(&docs);
    Prepared { docs, bm25 }
}

// ---------------------------------------------------------------- 通道一：BM25F

const FIELDS: [(&str, f64, f64); 3] = [
    ("name", 3.0, 0.5),
    ("path", 1.0, 0.7),
    ("sig", 0.6, 0.8),
];
const BM25_K1: f64 = 1.2;

type DocFields = [(HashMap<String, u32>, u32); 3];

pub struct Bm25f {
    n: usize,
    df: HashMap<String, u32>,
    avg_len: [f64; 3],
    docs: Vec<DocFields>,
}

impl Bm25f {
    pub fn new(docs: &[Doc]) -> Self {
        let n = docs.len();
        let mut df: HashMap<String, u32> = HashMap::new();
        let mut sum_len = [0f64; 3];
        let mut recs: Vec<DocFields> = Vec::with_capacity(n);
        for doc in docs {
            let mut rec: DocFields = [
                (HashMap::new(), 0),
                (HashMap::new(), 0),
                (HashMap::new(), 0),
            ];
            for (fi, toks) in doc.fields.iter().enumerate() {
                let mut tf: HashMap<String, u32> = HashMap::new();
                for t in toks {
                    *tf.entry(t.clone()).or_insert(0) += 1;
                }
                sum_len[fi] += toks.len() as f64;
                rec[fi] = (tf, toks.len().max(1) as u32);
            }
            let mut seen: HashSet<&str> = HashSet::new();
            for fld in rec.iter() {
                for t in fld.0.keys() {
                    seen.insert(t.as_str());
                }
            }
            for t in seen {
                *df.entry(t.to_string()).or_insert(0) += 1;
            }
            recs.push(rec);
        }
        let avg_len = [
            sum_len[0] / n.max(1) as f64,
            sum_len[1] / n.max(1) as f64,
            sum_len[2] / n.max(1) as f64,
        ];
        Bm25f { n, df, avg_len, docs: recs }
    }

    /// BM25F 打分，返回按分数降序的 (docId, score)。
    pub fn score(&self, terms: &[String]) -> Vec<(usize, f64)> {
        let mut out: HashMap<usize, f64> = HashMap::new();
        for t in terms {
            let Some(&df) = self.df.get(t) else { continue };
            let idf = (1.0 + (self.n as f64 - df as f64 + 0.5) / (df as f64 + 0.5)).ln();
            for (d, rec) in self.docs.iter().enumerate() {
                let mut wtf = 0.0;
                for (fi, fld) in rec.iter().enumerate() {
                    let tf = *fld.0.get(t).unwrap_or(&0) as f64;
                    if tf == 0.0 {
                        continue;
                    }
                    let (_, w, b) = FIELDS[fi];
                    let norm = 1.0 - b + b * (fld.1 as f64 / self.avg_len[fi]);
                    wtf += w * tf / norm;
                }
                if wtf > 0.0 {
                    *out.entry(d).or_insert(0.0) += idf * (wtf * (BM25_K1 + 1.0)) / (wtf + BM25_K1);
                }
            }
        }
        let mut v: Vec<_> = out.into_iter().collect();
        v.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
        v
    }
}

// ---------------------------------------------------------------- 通道二：标识符（精确/缩写/前缀/包含/子词）

pub fn ident_score(doc: &Doc, qi: &str) -> f64 {
    let lower = doc.name.to_lowercase();
    if lower == qi {
        return 1.0;
    }
    if doc.acr == qi {
        return 0.85; // 缩写：ahr → AuthHttpRetry
    }
    if lower.starts_with(qi) {
        return 0.7;
    }
    if lower.contains(qi) {
        return 0.5;
    }
    let qsubs = split_ident(qi);
    if !qsubs.is_empty() && qsubs.iter().all(|t| doc.name_subs.contains(t)) {
        return 0.6; // 子词全覆盖（顺序无关）
    }
    0.0
}

// ---------------------------------------------------------------- 通道三：n-gram（拼写纠错/残缺输入）

/// 查询侧 3-gram 包含率，容忍目标名更长。
pub fn ngram_score(doc: &Doc, qtri: &HashSet<String>) -> f64 {
    if qtri.is_empty() {
        return 0.0;
    }
    let hit = qtri.iter().filter(|g| doc.tri.contains(*g)).count();
    hit as f64 / qtri.len() as f64
}

// ---------------------------------------------------------------- 融合：RRF + 证据 + 置信度

const RRF_K: f64 = 60.0;

pub struct Candidate {
    pub path: String,
    pub symbol: String,
    pub kind: String,
    pub lines: (u32, u32),
    pub score: f64,
    pub confidence: String,
    pub evidence: Vec<String>,
    pub snippet: String,
}

pub struct SearchResult {
    pub verdict: String,
    pub elapsed_ms: f64,
    pub doc_count: usize,
    pub candidates: Vec<Candidate>,
}

/// 模糊检索主入口：只召回排序，不做精确解析。
pub fn fuzzy_search(p: &Prepared, query: &str, limit: usize, alias: &[(&str, &[&str])]) -> SearchResult {
    let t0 = Instant::now();
    let parsed = parse_query(query, alias);

    let bm25_list: Vec<(usize, f64)> = p.bm25.score(&parsed.terms).into_iter().take(100).collect();

    let mut ident_list: Vec<(usize, f64)> = Vec::new();
    if !parsed.idents.is_empty() {
        for doc in &p.docs {
            let best = parsed
                .idents
                .iter()
                .map(|qi| ident_score(doc, qi))
                .fold(0.0, f64::max);
            if best > 0.0 {
                ident_list.push((doc.id, best));
            }
        }
        ident_list.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
        ident_list.truncate(100);
    }

    let qtris: Vec<HashSet<String>> = parsed
        .idents
        .iter()
        .filter(|t| t.chars().count() >= 3)
        .map(|t| trigrams(t))
        .collect();
    let mut ngram_list: Vec<(usize, f64)> = Vec::new();
    if !qtris.is_empty() {
        for doc in &p.docs {
            let best = qtris.iter().map(|tri| ngram_score(doc, tri)).fold(0.0, f64::max);
            if best >= 0.5 {
                // 包含率 <0.5 视为噪声
                ngram_list.push((doc.id, best));
            }
        }
        ngram_list.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
        ngram_list.truncate(100);
    }

    // RRF 融合 + 证据记录
    let mut fused: HashMap<usize, f64> = HashMap::new();
    let mut evidence: HashMap<usize, Vec<String>> = HashMap::new();
    let channels: [(&str, f64, &Vec<(usize, f64)>); 3] = [
        ("bm25", 1.0, &bm25_list),
        ("ident", 1.2, &ident_list),
        ("ngram", 0.8, &ngram_list),
    ];
    for (ch, w, list) in channels {
        for (rank, (id, s)) in list.iter().enumerate() {
            *fused.entry(*id).or_insert(0.0) += w / (RRF_K + rank as f64 + 1.0);
            let ev = match ch {
                "bm25" => format!("bm25#{}({:.2})", rank + 1, s),
                "ident" => format!(
                    "ident:{}",
                    if *s >= 1.0 {
                        "exact"
                    } else if *s >= 0.85 {
                        "acronym"
                    } else if *s >= 0.7 {
                        "prefix"
                    } else if *s >= 0.6 {
                        "subtokens"
                    } else {
                        "contains"
                    }
                ),
                _ => format!("ngram:{:.2}", s),
            };
            evidence.entry(*id).or_default().push(ev);
        }
    }

    // 引号精确证据 / 路径软提示 / 测试文件降权
    let mut exact_hits: HashSet<usize> = HashSet::new();
    for e in &parsed.exact {
        for doc in &p.docs {
            if &doc.name == e || doc.sig.contains(e.as_str()) {
                exact_hits.insert(doc.id);
            }
        }
    }

    let mut raw: Vec<(usize, f64, bool, Vec<String>)> = fused
        .into_iter()
        .map(|(id, mut s)| {
            let doc = &p.docs[id];
            let exact = exact_hits.contains(&id);
            if exact {
                s *= 1.3;
            }
            let ph = !parsed.path_hints.is_empty()
                && parsed.path_hints.iter().any(|h| doc.file.to_lowercase().contains(h));
            if ph {
                s *= 1.2;
            }
            if doc.noise {
                s *= 0.85;
            }
            let mut ev = evidence.remove(&id).unwrap_or_default();
            if exact {
                ev.push("exact:quoted".into());
            }
            if ph {
                ev.push("path:hint".into());
            }
            (id, s, exact, ev)
        })
        .collect();
    raw.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    raw.truncate(limit);

    // 置信度门控：Top1 绝对证据 + Top1/Top2 margin（阈值待真实查询集校准）
    let mut verdict = "none";
    if let Some(top1) = raw.first() {
        let margin = raw
            .get(1)
            .map(|t2| top1.1 / t2.1.max(1e-9))
            .unwrap_or(f64::INFINITY);
        let strong = top1.2 || top1.3.iter().any(|e| e.starts_with("ident:exact"));
        verdict = if strong || margin >= 1.5 {
            "high"
        } else if margin >= 1.15 || top1.3.len() >= 2 {
            "medium"
        } else {
            "low"
        };
    }

    let candidates = raw
        .into_iter()
        .enumerate()
        .map(|(i, (id, s, _exact, ev))| {
            let doc = &p.docs[id];
            Candidate {
                path: doc.file.clone(),
                symbol: doc.name.clone(),
                kind: doc.kind.clone(),
                lines: (doc.ln, doc.end),
                score: (s * 10000.0).round() / 10000.0,
                confidence: if i == 0 {
                    verdict.to_string()
                } else if verdict == "high" && i < 3 {
                    "medium".into()
                } else {
                    "low".into()
                },
                evidence: ev,
                snippet: doc.sig.clone(),
            }
        })
        .collect();

    SearchResult {
        verdict: verdict.to_string(),
        elapsed_ms: t0.elapsed().as_secs_f64() * 1000.0,
        doc_count: p.docs.len(),
        candidates,
    }
}

// ---------------------------------------------------------------- 测试

#[cfg(test)]
mod tests {
    use super::*;

    fn sym(name: &str) -> Sym {
        Sym {
            ln: 1,
            end: 10,
            depth: 0,
            kind: "fn",
            name: name.to_string(),
            sig: format!("export function {name}(root)"),
            exp: true,
        }
    }

    fn prep_one(name: &str) -> Prepared {
        let mut files = HashMap::new();
        files.insert("src/demo.ts".to_string(), vec![sym(name)]);
        prepare(&files)
    }

    #[test]
    fn split_ident_cases() {
        assert_eq!(split_ident("getIndex"), vec!["get", "index"]);
        assert_eq!(split_ident("HTTPServer"), vec!["http", "server"]);
        assert_eq!(split_ident("snake_case"), vec!["snake", "case"]);
        assert_eq!(split_ident("sha256Hash"), vec!["sha", "256", "hash"]);
    }

    #[test]
    fn acronym_case() {
        assert_eq!(acronym("AuthHttpRetry"), "ahr");
    }

    #[test]
    fn typo_recall_via_ngram() {
        let p = prep_one("getIndex");
        let res = fuzzy_search(&p, "getindx", 5, DEFAULT_ALIAS);
        assert_eq!(res.candidates[0].symbol, "getIndex", "{:?}", res.candidates.iter().map(|c| &c.symbol).collect::<Vec<_>>());
    }

    #[test]
    fn exact_ident_is_high_confidence() {
        let p = prep_one("retryAuth");
        let res = fuzzy_search(&p, "retryAuth", 5, DEFAULT_ALIAS);
        assert_eq!(res.verdict, "high");
        assert_eq!(res.candidates[0].symbol, "retryAuth");
    }

    #[test]
    fn chinese_alias_recall() {
        let p = prep_one("retryAuth");
        let res = fuzzy_search(&p, "鉴权重试", 5, DEFAULT_ALIAS);
        assert_eq!(res.candidates[0].symbol, "retryAuth");
    }

    #[test]
    fn acronym_channel_recall() {
        let p = prep_one("AuthHttpRetry");
        let res = fuzzy_search(&p, "ahr", 5, DEFAULT_ALIAS);
        assert_eq!(res.candidates[0].symbol, "AuthHttpRetry");
        assert!(res.candidates[0].evidence.iter().any(|e| e == "ident:acronym"));
    }
}
