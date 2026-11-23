//! Candidate discovery for the model-facing catalog. The query is data, never a path to open.
use super::*;

pub(super) struct Recall {
    pub files: Vec<String>,
    pub hits: HashMap<String, Vec<usize>>,
    pub anchors: Vec<String>,
    pub explicit: HashSet<String>,
    pub terms: Vec<String>,
    pub entries: HashSet<String>,
    pub partial: Option<&'static str>,
    pub local_terms: Vec<String>,
}

pub(super) fn recall(root: &Path, all: &[String], params: &Value, nested: Option<(&Path, String)>) -> Recall {
    let task = params["task"].as_str().unwrap_or_default();
    // Correction is a fresh lexical query, not another pass over the same noisy task words.
    let focus = params["_focus"].as_array();
    let words = focus.or_else(|| params["keywords"].as_array()).into_iter().flatten().filter_map(Value::as_str);
    static TOKEN: OnceLock<Regex> = OnceLock::new();
    let words_in = TOKEN.get_or_init(|| Regex::new(r"[A-Za-z0-9_][A-Za-z0-9_./-]*").unwrap());
    let mut anchors: Vec<String> = words.map(str::to_owned).collect();
    let mut terms = Vec::new();
    if focus.is_none() {
        terms = task_tokens(task);
        for m in words_in.find_iter(task) {
            let s = m.as_str().trim_end_matches('.');
            if s.len() >= 3 && (s.contains(['/', '.', '_', '-']) || s.chars().skip(1).any(|c| c.is_ascii_uppercase())) {
                anchors.push(s.to_owned());
            }
        }
    }
    anchors.retain(|s| !s.trim().is_empty() && s.len() <= 256);
    anchors.sort(); anchors.dedup();
    for s in &anchors {
        terms.push(s.clone());
        terms.extend(s.split(['/', '.', ':']).filter(|s| s.len() >= 3 && !stop_word(&s.to_ascii_lowercase())).map(str::to_owned));
        if let Some((parent, last)) = s.rsplit_once('/') {
            if let Some(penultimate) = parent.rsplit('/').next().filter(|p| !p.is_empty() && !last.is_empty()) {
                terms.push(format!("/{penultimate}/{last}"));
            }
        }
    }
    let mut seen = HashSet::new();
    // An extension is not a module: e.g. `.tsx` in a filename must not promote every TSX component.
    terms.retain(|s| !s.is_empty() && !CODE_FILE_EXTENSIONS.contains(&s.to_ascii_lowercase().as_str()) && seen.insert(s.to_ascii_lowercase()));
    anchors.retain(|s| !CODE_FILE_EXTENSIONS.contains(&s.to_ascii_lowercase().as_str()));
    let explicit: HashSet<_> = params["files"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_owned)
        .chain(all.iter().filter(|f| focus.is_none() && task.contains(f.as_str())).cloned()).collect();
    let tests = retrieval_plan(task).tests;
    let eligible = |f: &str| explicit.contains(f) || tests || (!anchor_noise_path(f) && !f.ends_with("_test.go"));
    let allowed: HashSet<_> = all.iter().map(String::as_str).filter(|f| eligible(f)).collect();
    // Mix lexical and path evidence even when an explicit path or multiple module names were given.
    let needles: Vec<_> = terms.iter().map(|t| t.to_lowercase()).collect();
    let mut fields = HashMap::<String, Vec<u8>>::new();
    let mut hits = HashMap::<String, Vec<usize>>::new();
    for file in &allowed {
        let path = file.to_lowercase();
        for (i,t) in needles.iter().enumerate() {
            if path.split(['/', '.', '_', '-']).any(|s| s == t) || (t.contains('/') && path.contains(t)) {
                fields.entry((*file).into()).or_insert_with(|| vec![0; needles.len()])[i] = 6;
            }
        }
    }
    // Spend more on initial discovery than a focused follow-up; never renew the deadline per repo/probe.
    let budget = Duration::from_secs(if focus.is_none() { 6 } else { 4 });
    let deadline = Some(Instant::now() + budget);
    let search = SearchSession::with_deadline(root, deadline);
    let inner = nested.as_ref().map(|(repo, prefix)| {
        (*repo, prefix, SearchSession::with_deadline(repo, deadline))
    });
    // Separate high-specificity probes so a common task word cannot consume all matching rows first.
    let mut probes: Vec<_> = anchors.iter().filter(|s| !s.is_empty()).cloned().collect();
    probes.extend(terms.iter().filter(|s| s.starts_with('/')).cloned());
    probes.sort(); probes.dedup();
    let rest: Vec<_> = terms.iter().filter(|s| !probes.contains(s)).cloned().collect();
    let mut partial = None;
    let mut scan = |repo: &Path, query: &[String], session: &SearchSession| {
        let rows = search_text_scopes_until(repo, query, true, false, None, session);
        if session.incomplete.load(Ordering::Relaxed) {
            partial.get_or_insert(if session.deadline.is_some_and(|d| Instant::now() >= d) {
                "全仓正文扫描达到本轮时限"
            } else { "正文检索后端未正常完成" });
        }
        rows
    };
    let probe_keys: HashSet<_> = probes.iter().map(|s| s.to_lowercase()).collect();
    let mut found = HashSet::new();
    let mut local_terms = Vec::new();
    for (broad, query) in [(false, &probes), (true, &rest)] {
        if query.is_empty() { continue; }
        // Exact probes already located: derived route pieces and prose n-grams need not walk the repo again.
        if broad && !probe_keys.is_empty() && found.len() == probe_keys.len() {
            local_terms = query.iter().filter(|s| s.chars().all(is_han) || anchors.iter().any(|a|
                a.split(['/', '.', ':']).any(|part| part.eq_ignore_ascii_case(s)))).cloned().collect();
        }
        let global: Vec<_> = query.iter().filter(|s| !local_terms.contains(s)).cloned().collect();
        if global.is_empty() { continue; }
        let mut rows = if let Some((repo, prefix, session)) = &inner {
            let mut rows = scan(repo, &global, session);
            for row in &mut rows { row.file.insert_str(0, prefix); }
            rows
        } else { Vec::new() };
        rows.extend(scan(root, &global, &search));
        for row in rows {
            if !allowed.contains(row.file.as_str()) { continue; }
            let line = row.text.to_lowercase();
            hits.entry(row.file.clone()).or_default().push(row.ln);
            let weights = fields.entry(row.file).or_insert_with(|| vec![0; needles.len()]);
            for (i,t) in needles.iter().enumerate() {
                if line.contains(t) {
                    weights[i] = weights[i].max(1);
                    if !broad && probe_keys.contains(t) { found.insert(t.as_str()); }
                }
            }
        }
    }
    let mut df = vec![0usize; needles.len()];
    for weights in fields.values() { for (n,&w) in df.iter_mut().zip(weights) { *n += usize::from(w != 0); } }
    let mut ranked: Vec<_> = fields.into_iter().map(|(file,w)| {
        let score: f64 = w.iter().enumerate().filter(|(_,v)| **v > 0).map(|(i,&v)| {
            let idf = (1.0 + all.len() as f64 / df[i] as f64).ln();
            let specificity = if probes.iter().any(|p| p.eq_ignore_ascii_case(&terms[i])) { 4.0 } else { 1.0 };
            f64::from(v) * idf * specificity
        }).sum(); (file,score)
    }).collect();
    // Match whole route segments (not MetricsArchive); reserve a small entry excerpt before top-K.
    let suffixes: Vec<_> = probes.iter().filter(|p| p.starts_with('/') && !p.contains('.'))
        .map(|p| p.to_ascii_lowercase()).collect();
    let is_entry = |file: &str| {
        let path = Path::new(file);
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        if !matches!(path.extension().and_then(|s| s.to_str()), Some("tsx"|"jsx"|"ts"|"js"|"vue"|"svelte")) { return false; }
        let target = if matches!(stem, "index"|"page") { path.parent().unwrap_or(path).to_path_buf() } else { path.with_extension("") };
        let target = format!("/{}", target.to_string_lossy().replace('\\', "/").to_ascii_lowercase());
        suffixes.iter().any(|suffix| target.ends_with(suffix))
    };
    ranked.sort_by(|a,b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    let entries: HashSet<_> = if suffixes.is_empty() { HashSet::new() } else {
        ranked.iter().filter(|(f,_)| is_entry(f)).take(3).map(|(f,_)| f.clone()).collect()
    };
    let mut files: Vec<_> = explicit.iter().cloned().collect(); files.sort();
    let candidates = ranked.iter().filter(|(f,_)| !explicit.contains(f));
    files.extend(candidates.clone().filter(|(f,_)| entries.contains(f))
        .chain(candidates.filter(|(f,_)| !entries.contains(f))).take(32).map(|(f,_)| f.clone()));
    for lines in hits.values_mut() { lines.sort_unstable(); lines.dedup(); }
    if partial.is_some() { local_terms.clone_from(&terms); }
    Recall { files, hits, anchors, explicit, terms, entries, partial, local_terms }
}

/// Rank matching intervals before limiting them: precise late hits must survive early boilerplate.
pub(super) fn evidence_ranges(lines: &[String], hits: &[usize], anchors: &[String], stylesheet: bool) -> Vec<(usize,usize)> {
    if hits.is_empty() { return Vec::new(); }
    let depth = stylesheet.then(|| stripped_depth(lines, false));
    let mut windows = Vec::new();
    for &ln in hits {
        let mut span = (ln.saturating_sub(3).max(1), (ln+3).min(lines.len()));
        if let Some((code, before, after)) = &depth {
            // Local rule and containing at-rule guard when nearby. This is evidence, not a CSS AST.
            let i = ln - 1;
            let open = (i.saturating_sub(32)..=i).rev().find(|&j| code[j].contains('{') && (j == i || before[j] < before[i]));
            if let Some(j) = open {
                if let Some(k) = (i..(j+48).min(lines.len())).find(|&k| after[k] <= before[j]) {
                    span = (j+1,k+1);
                    if j > 0 && before[j] > 0 {
                        if let Some(p) = (0..j).rev().find(|&p| before[p] < before[j] && after[p] >= before[j]) {
                            if p+1 < span.0 && lines[p].trim_start().starts_with('@') { windows.push((p+1,p+1)); }
                        }
                    }
                }
            }
        }
        windows.push(span);
    }
    let mut ranges = merge_ranges(windows);
    if ranges.len() > 12 {
        ranges.sort_by_cached_key(|&(a,b)| {
            let matches = anchors.iter().filter(|s| lines[a-1..b].iter().any(|line| line.contains(s.as_str()))).count();
            (std::cmp::Reverse(matches),a)
        });
        ranges.truncate(12);
        ranges.sort_unstable();
    }
    ranges
}

/// A bounded preview of source vocabulary lets the selector follow names/literals without copying bodies.
pub(super) fn clues<'a>(lines: impl Iterator<Item=&'a String>) -> String {
    static TERMS: OnceLock<Regex> = OnceLock::new();
    let re = TERMS.get_or_init(|| Regex::new(r#"[\"'`]([^\"'`\r\n]{2,80})[\"'`]|\b([A-Za-z_][A-Za-z0-9_]{2,})\b"#).unwrap());
    let mut seen = HashSet::new();
    lines.flat_map(|line| re.captures_iter(line)).filter_map(|m| {
        let s = m.get(1).or_else(|| m.get(2))?.as_str();
        (!stop_word(&s.to_ascii_lowercase()) && seen.insert(s)).then_some(s)
    }).take(16).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn lookup(dir: &Path, task: &str, focus: Value) -> Value {
        serde_json::from_str(&polaris(dir, json!({"task":task,"_focus":focus,"_catalog":true})).unwrap()).unwrap()
    }

    #[test]
    fn polaris_discovers_across_modules_and_unconventional_resource_names() {
        let dir = tempfile::tempdir().unwrap();
        let files: Value = serde_json::from_str(include_str!("../../../../scripts/fixtures/polaris-route.json")).unwrap();
        for (file, body) in files.as_object().unwrap() {
            let file = if file.ends_with(".sql.md") { "assets/random.sql" } else { file };
            let path = dir.path().join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, body.as_str().unwrap().replace("## upsert", "-- binding: upsert").replace("## query_by_open_id", "-- binding: query_by_open_id")).unwrap();
        }
        let task = "shortcuts/save 是什么接口？路由 router/router.go，返回处理实现、请求响应结构、用户身份来源与落库表";
        let first = lookup(dir.path(), task, Value::Null);
        let menu = first["text"].as_str().unwrap();
        for hint in ["InitRouter", "SaveShortcutsRequest", "QueryShortcutsResponse", "persist"] { assert!(menu.contains(hint), "missing {hint}"); }
        let next = lookup(dir.path(), task, json!(["UseUserInfoPlus", "upsert", "query_by_open_id"]));
        assert!(next["text"].as_str().unwrap().contains("BeforeDoService"));
        let sql = next["snippets"].as_array().unwrap().iter().filter(|s| s["file"] == "assets/random.sql").map(|s| s["body"].as_str().unwrap()).collect::<Vec<_>>().join("\n");
        assert!(sql.contains("SELECT id") && sql.contains("INSERT INTO"));
        assert!(!sql.contains("DROP TABLE"));
    }

    #[test]
    fn polaris_offers_narrow_evidence_inside_large_dispatcher_without_framework_rules() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("bus.py"), format!("def connect():\n{}    install('lunar-pulse', wake_worker)\n", "    pass\n".repeat(100))).unwrap();
        fs::write(dir.path().join("remote.py"), "def wake_worker():\n    return 42\n").unwrap();
        let first = lookup(dir.path(), "lunar-pulse 是怎么处理的", Value::Null);
        let menu = first["text"].as_str().unwrap();
        assert!(menu.lines().any(|line| line.contains("connect [") && line.contains("wake_worker")), "定义应带实际命中行的线索：{menu}");
        assert!(menu.contains("匹配片段"), "{menu}");
        assert!(first["snippets"].as_array().unwrap().iter().any(|s| s["start"].as_u64().unwrap() > 90 && s["body"].as_str().unwrap().contains("lunar-pulse")));
        let next = lookup(dir.path(), "lunar-pulse 是怎么处理的", json!(["wake_worker"]));
        assert!(next["required"].as_array().unwrap().iter().any(|id| next["snippets"][id.as_u64().unwrap() as usize-1]["body"] == "def wake_worker():\n    return 42"));
    }

    #[test]
    fn polaris_catalog_includes_stylesheet_rule_and_conditional_guard() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("surface.tsx"), "export function Surface(){ return <span class=\"notice-pulse\"/>; }\n").unwrap();
        for ext in ["css", "scss", "less"] {
            fs::write(dir.path().join(format!("layout.{ext}")), format!("@media (width < 500px) {{\n{}  .notice-pulse {{\n    display: none;\n  }}\n}}", ".other { margin: 0; }\n".repeat(60))).unwrap();
        }
        let found = lookup(dir.path(), "Surface 的 .notice-pulse 样式和断点", Value::Null);
        for ext in ["css", "scss", "less"] {
            let file = format!("layout.{ext}");
            let parts: Vec<_> = found["snippets"].as_array().unwrap().iter().filter(|s| s["file"] == file).map(|s| s["body"].as_str().unwrap()).collect();
            assert!(parts.contains(&"@media (width < 500px) {"));
            assert!(parts.contains(&"  .notice-pulse {\n    display: none;\n  }"));
        }
    }

    #[test]
    fn polaris_scopes_derived_terms_only_after_exact_hits() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("control.ts"), format!("// FetchBalances\n{}// 切换语言\n", "// padding\n".repeat(30))).unwrap();
        fs::write(dir.path().join("other.ts"), "// 切换语言\n").unwrap();
        let run = |task: &str| lookup(dir.path(), task, Value::Null);
        let hit = run("FetchBalances 切换语言");
        assert!(!hit["text"].as_str().unwrap().contains("other.ts"));
        assert!(hit["snippets"].as_array().unwrap().iter().any(|s| s["file"] == "control.ts" && s["body"].as_str().unwrap().contains("切换语言")));
        for task in ["FetchBalances MissingHandler 切换语言", "切换语言"] {
            assert!(run(task)["text"].as_str().unwrap().contains("other.ts"));
        }
    }

    #[test]
    fn polaris_route_entry_survives_crowded_candidate_limit() {
        let dir = tempfile::tempdir().unwrap();
        let page = dir.path().join("pages/Account/Billing");
        fs::create_dir_all(&page).unwrap();
        // These descendants sort before index.tsx and share all the route keywords.
        for n in 0..40 {
            fs::write(page.join(format!("a{n}.tsx")), "export const label = 'account/billing';\n").unwrap();
        }
        fs::write(page.join("index.tsx"), "export { default } from './view';\n").unwrap();
        let found = lookup(dir.path(), "修复 /account/billing", Value::Null);
        let required = found["required"].as_array().unwrap();
        assert_eq!(required.len(), 1);
        assert_eq!(found["snippets"][required[0].as_u64().unwrap() as usize - 1]["file"], "pages/Account/Billing/index.tsx");
    }

    #[test]
    fn polaris_rejects_an_explicit_outside_candidate_before_loading_source() {
        let dir = tempfile::tempdir().unwrap();
        let other = tempfile::NamedTempFile::new().unwrap();
        let error = polaris(dir.path(), json!({"files":[other.path()],"_catalog":true})).unwrap_err();
        assert!(error.contains("越出"));
    }
}
