// Literal recall complements declaration recall. A missing AST node must never
// erase SQL, configuration, templates, or other original UTF-8 source text.
// These are source ranges, not invented function declarations or semantic proof.
fn structural_candidate(file: &str, q: &query::Query) -> bool {
    let role = file_role(file);
    is_searchable_implementation_file(file)
        && (role == "implementation" || (role == "test" && q.test_intent)
            || (role == "documentation" && q.doc_intent))
}
fn literal_candidate(file: &str, q: &query::Query) -> bool {
    // Match the legacy raw-search scope, still respecting the common generated
    // file exclusions and the walker's ignore rules. No SQL-only allowlist.
    !excluded_search_path(file) && (file_role(file) != "test" || q.test_intent)
}
struct LiteralQuery { matcher: regex::RegexSet, weights: Vec<f64> }
impl LiteralQuery {
    fn new(q: &query::Query) -> Result<Self, String> {
        let mut words = query::tokens(&format!("{} {}", q.task, q.anchors.join(" ")));
        words.retain(|w| (!w.is_ascii() || w.len() >= 3)
            && !matches!(w.as_str(), "查下" | "看看" | "请问" | "帮忙" | "查看"));
        words.sort(); words.dedup(); words.truncate(128);
        let patterns = words.iter().map(|w| regex::escape(w));
        let matcher = regex::RegexSetBuilder::new(patterns).case_insensitive(true)
            .size_limit(4 * 1024 * 1024).build().map_err(|e| e.to_string())?;
        // Longer overlapping CJK phrases distinguish a business object from a
        // generic word such as SQL. Repeated occurrences earn no extra votes.
        let weights = words.iter().map(|w| if w.is_ascii() { 1.0 }
            else { w.chars().count().saturating_sub(1) as f64 }).collect();
        Ok(Self { matcher, weights })
    }
    fn score(&self, file: &str, text: &str) -> f64 {
        let mut matched = self.matcher.matches(text).into_iter().collect::<HashSet<_>>();
        matched.extend(self.matcher.matches(file));
        matched.into_iter().map(|i| self.weights[i]).sum()
    }
}
fn literal_units(rows: &[Candidate], units: &[Arc<CodeUnit>], q: &query::Query,
    deadline: Instant) -> Result<(Vec<Arc<CodeUnit>>, bool), String> {
    let query = LiteralQuery::new(q)?;
    // Do not let a broad whole-file term match displace real implementations.
    // Recover only a better, localized literal match missing from the already
    // returned declarations. Existing code ranking is unchanged when no gap exists.
    let best = units.iter().filter(|u| u.role != "documentation")
        .map(|u| query.score(&u.file, &u.source[u.start - 1..u.end].join("\n")))
        .fold(0.0_f64, f64::max);
    let mut candidates = rows.iter().filter(|r| literal_candidate(&r.file, q)
        || q.files.contains(&r.file))
        .map(|r| (r, query.score(&r.file, &r.text)))
        .filter(|(_, score)| *score > best).collect::<Vec<_>>();
    candidates.sort_by(|a,b| b.1.total_cmp(&a.1).then(a.0.file.cmp(&b.0.file)));
    let mut selected = Vec::new(); let mut partial = false;
    // Text-only documents need no AST, and no second walk, grep process, model
    // request, or source read is needed: discovery already owns these bytes.
    for (row, _) in candidates.into_iter().take(16) {
        if Instant::now() >= deadline { partial = true; break; }
        let source = Arc::new(row.text.lines().map(str::to_owned).collect::<Vec<_>>());
        if source.is_empty() { continue; }
        let mut hits = Vec::new();
        for (i, line) in source.iter().enumerate() {
            if i % 256 == 0 && Instant::now() >= deadline { partial = true; break; }
            if query.matcher.is_match(line) { hits.push((i,query.score("",line))); }
        }
        if hits.is_empty() && query.matcher.is_match(&row.file) { hits.push((0,0.0)); }
        hits.sort_by(|a,b|b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        hits.truncate(64);
        let mut winner: Option<(usize, usize, f64)> = None;
        for &(hit, _) in &hits {
            if Instant::now() >= deadline { partial = true; break; }
            let start = hit.saturating_sub(8); let end = (hit + 48).min(source.len());
            // Byte and line caps bound tokenization, including huge/minified lines.
            let text = source[start..end].join("\n");
            let score = query.score(&row.file, &text);
            if score <= best { continue; }
            // Do not emit a second copy of an already selected declaration.
            if units.iter().any(|u| u.file == row.file && u.start <= start + 1 && u.end >= end) { continue; }
            if winner.as_ref().is_none_or(|(_,_,old)| score > *old) { winner = Some((start,end,score)); }
        }
        let Some((start,end,score)) = winner else { continue; };
        let body = source[start..end].join("\n");
        let mut terms = HashMap::<String,f64>::new();
        for token in query::tokens(&format!("{}\n{}",row.file,body.chars().take(6000).collect::<String>())) {
            if terms.len() < 2048 || terms.contains_key(&token) { *terms.entry(token).or_default() += 1.0; }
        }
        for value in terms.values_mut() { *value = (*value).min(12.0); }
        let length = terms.values().sum::<f64>().max(1.0);
        let passage = format!("Original source range: {}:{}-{}\n{}",row.file,start+1,end,
            body.chars().take(3000).collect::<String>());
        let u = CodeUnit { file:row.file.clone(), name:"<source-range>".into(),
            start:start+1,end,owner_start:1,owner_end:source.len(),hash:digest(passage.as_bytes()),
            file_hash:row.hash.clone(),role:"source-text",passage,terms,length,
            name_terms:HashSet::new(),members:Vec::new(),calls:HashSet::new(),events:Vec::new(),
            commands:HashSet::new(),source,imports:Arc::new(Vec::new()) };
        selected.push((Arc::new(u),score));
    }
    selected.sort_by(|a,b| b.1.total_cmp(&a.1).then(a.0.file.cmp(&b.0.file)));
    selected.truncate(4);
    Ok((selected.into_iter().map(|(u,_)|u).collect(),partial))
}
pub(super) fn prioritize_literal(ranked: &mut Vec<(usize,f64)>, units: &[Arc<CodeUnit>], q: &query::Query) {
    let Ok(query) = LiteralQuery::new(q) else { return; };
    let mut raw = units.iter().enumerate().filter(|(_,u)|u.role=="source-text")
        .map(|(i,u)|(i,query.score(&u.file,&u.source[u.start-1..u.end].join("\n"))))
        .collect::<Vec<_>>();
    raw.sort_by(|a,b|b.1.total_cmp(&a.1).then(units[a.0].file.cmp(&units[b.0].file)));
    if raw.is_empty() { return; }
    let ids=raw.iter().map(|(i,_)|*i).collect::<HashSet<_>>();
    ranked.retain(|(i,_)|!ids.contains(i));
    let top=ranked.first().map(|(_,s)|*s).unwrap_or(1.0);
    for (position,(i,_)) in raw.into_iter().enumerate().rev() {
        ranked.insert(0,(i,top+1.0/(position+1) as f64));
    }
}

#[cfg(test)] mod literal_recall_tests {
    use super::*;
    fn lookup(files:&[(&str,&str)],task:&str)->String {
        let d=tempfile::tempdir().unwrap();
        for (name,text) in files {let p=d.path().join(name);fs::create_dir_all(p.parent().unwrap()).unwrap();fs::write(p,text).unwrap();}
        super::super::polaris(d.path(),serde_json::json!({"task":task,"maxBytes":12000})).unwrap()
    }
    #[test] fn sql_in_markdown_does_not_require_documentation_intent() {
        let out=lookup(&[("docs/query.md","# 仓库库存汇总\n```sql\nSELECT sku, SUM(qty) FROM stock GROUP BY sku;\n```\n")],"查下仓库库存汇总的sql");
        assert!(out.contains("FROM stock GROUP BY sku"),"{out}");assert!(out.len()<=12000);
    }
    #[test] fn xml_and_yaml_are_searchable_without_explicit_paths() {
        for file in ["mapper/Report.xml","queries.yml","queries.conf"] {
            let out=lookup(&[(file,"<!-- 仓库库存汇总 -->\nSELECT sku, SUM(qty) FROM stock GROUP BY sku;\n")],"仓库库存汇总sql");
            assert!(out.contains("FROM stock GROUP BY sku"),"{file}: {out}");
        }
    }
    #[test] fn constants_outside_functions_are_not_erased() {
        for (file,text) in [("queries.ts","// 仓库库存汇总\nexport const REPORT = `SELECT sku FROM stock`;\nexport function ping() { return 1; }\n"),
            ("queries.py","# 仓库库存汇总\nREPORT = 'SELECT sku FROM stock'\ndef ping():\n    return 1\n")] {
            let out=lookup(&[(file,text)],"仓库库存汇总sql");assert!(out.contains("SELECT sku FROM stock"),"{out}");
        }
    }
    #[test] fn unrelated_function_hit_does_not_mask_the_literal_result() {
        let out=lookup(&[("runner.ts","export function sql() { return 'SELECT 1'; }\n"),
            ("docs/query.md","# 仓库库存汇总\n```sql\nSELECT sku FROM stock;\n```\n")],"仓库库存汇总sql");
        assert!(out.contains("SELECT sku FROM stock"),"{out}");
    }
    #[test] fn original_case_and_ignore_rules_are_preserved() {
        let out=lookup(&[(".gitignore","private.xml\n"),("private.xml","仓库库存汇总 SECRET_VALUE"),
            ("queries.toml","# 仓库库存汇总\nq='SELECT CaseSensitive FROM Warehouse'\n"),
            ("image.bin","仓库库存汇总\0DO_NOT_READ")],"仓库库存汇总sql");
        assert!(out.contains("SELECT CaseSensitive FROM Warehouse"),"{out}");
        assert!(!out.contains("SECRET_VALUE"));assert!(!out.contains("DO_NOT_READ"));
    }
    #[test] fn literal_cache_revalidates_edits_and_deletions() {
        let d=tempfile::tempdir().unwrap();let path=d.path().join("query.xml");
        let q=serde_json::json!({"task":"仓库库存汇总sql","maxBytes":12000});
        fs::write(&path,"仓库库存汇总\nSELECT old_value FROM stock;\n").unwrap();
        let stamp=fs::metadata(&path).unwrap().modified().unwrap();
        let old=super::super::polaris(d.path(),q.clone()).unwrap();assert!(old.contains("old_value"));
        fs::write(&path,"仓库库存汇总\nSELECT new_value FROM stock;\n").unwrap();
        fs::File::options().write(true).open(&path).unwrap().set_times(fs::FileTimes::new().set_modified(stamp)).unwrap();
        let new=super::super::polaris(d.path(),q.clone()).unwrap();assert!(new.contains("new_value"));assert!(!new.contains("old_value"));
        fs::remove_file(path).unwrap();let gone=super::super::polaris(d.path(),q).unwrap();assert!(!gone.contains("new_value"));
    }
    #[test] fn an_empty_repository_still_misses_without_inventing_code() {
        assert!(lookup(&[],"不存在的仓库库存汇总sql").starts_with("# CTX MISS"));
    }
}
