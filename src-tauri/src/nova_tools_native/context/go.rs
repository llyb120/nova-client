//! Go declarations use package scope, not JS method/property heuristics.
use super::*;

/// Keep line numbers and balanced placeholders for literals, including multiline raw SQL.
pub(super) fn code(text: &str) -> Vec<String> {
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    let mut quote = None;
    let mut block = false;
    let mut comment = false;
    while let Some(c) = chars.next() {
        if c == '\n' { out.push(c); comment = false; continue; }
        if comment { out.push(' '); continue; }
        if block {
            if c == '*' && chars.peek() == Some(&'/') { chars.next(); block = false; }
            out.push(' '); continue;
        }
        if let Some(q) = quote {
            if c == q { quote = None; out.push(')'); }
            else if c == '\\' && q != '`' { chars.next(); out.push(' '); }
            else { out.push(' '); }
            continue;
        }
        if c == '/' && chars.peek() == Some(&'/') { chars.next(); comment = true; out.push(' '); }
        else if c == '/' && chars.peek() == Some(&'*') { chars.next(); block = true; out.push(' '); }
        else if matches!(c, '"' | '\'' | '`') { quote = Some(c); out.push('('); }
        else { out.push(c); }
    }
    out.lines().map(str::to_owned).collect()
}

fn end(code: &[String], start: usize) -> usize {
    let mut balance = 0isize;
    for (i, line) in code.iter().enumerate().skip(start) {
        for ch in line.chars() {
            match ch { '(' | '[' | '{' => balance += 1, ')' | ']' | '}' => balance -= 1, _ => {} }
        }
        if balance <= 0 && !line.trim_end().ends_with(['=', ',', '+', '-', '*', '/', '|', '&']) { return i; }
    }
    code.len().saturating_sub(1)
}

pub(super) fn receiver(sig: &str) -> Option<(String, String)> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let cap = RE.get_or_init(|| Regex::new(r"^func\s*\(\s*(\w+)\s+\*?(\w+)(?:\[[^]]*\])?\s*\)").unwrap()).captures(sig)?;
    Some((cap[1].into(), cap[2].into()))
}

pub(super) fn scan(text: &str) -> FileEntry {
    static FUNCTION: OnceLock<Regex> = OnceLock::new();
    static DECL: OnceLock<Regex> = OnceLock::new();
    static IMPORT: OnceLock<Regex> = OnceLock::new();
    let function = FUNCTION.get_or_init(|| Regex::new(r"^func\s+(?:\([^)]*\)\s*)?([\p{L}_][\p{L}\p{N}_]*)\s*(?:\[|\()").unwrap());
    let decl = DECL.get_or_init(|| Regex::new(r"^([\p{L}_][\p{L}\p{N}_]*(?:\s*,\s*[\p{L}_][\p{L}\p{N}_]*)*)\b").unwrap());
    let import = IMPORT.get_or_init(|| Regex::new(r#"^(?:(\w+|\.)\s+)?["`]([^"`]+)["`]"#).unwrap());
    let lines: Vec<_> = text.lines().collect();
    let code = code(text);
    let mut syms = Vec::new();
    let mut imports = Vec::new();
    let mut group: Option<(&str, usize, usize)> = None;
    let mut i = 0;
    while i < code.len() {
        let line = code[i].trim();
        if line.is_empty() { i += 1; continue; }
        if group.is_some() && line == ")" { group = None; i += 1; continue; }
        let mut value = line;
        let mut kind = group.map(|g| g.0);
        if kind.is_none() {
            for word in ["var", "const", "type", "import"] {
                if let Some(rest) = line.strip_prefix(word).filter(|rest| rest.starts_with(char::is_whitespace)) {
                    kind = Some(word); value = rest.trim(); break;
                }
            }
            if kind.is_some() && value == "(" {
                group = Some((kind.unwrap(), i, end(&code, i))); i += 1; continue;
            }
        }
        let finish = end(&code, i);
        if kind == Some("import") {
            let raw = lines[i].trim().strip_prefix("import").unwrap_or(lines[i].trim()).trim();
            if let Some(cap) = import.captures(raw) {
                let name = cap.get(1).map(|m| m.as_str()).unwrap_or_else(|| cap[2].rsplit('/').next().unwrap());
                if name != "_" { imports.push(ImportRef { name:name.into(), from:cap[2].into(), orig:None }); }
            }
        } else {
            let callable = function.captures(line);
            let names = callable.as_ref().map(|c| c[1].to_string())
                .or_else(|| kind.and_then(|_| decl.captures(value)).map(|c| c[1].to_string()));
            if let Some(names) = names {
                let (start, last) = group.filter(|g| g.0 == "const").map(|g| (g.1, g.2)).unwrap_or((i, finish));
                for name in names.split(',').map(str::trim).filter(|n| *n != "_") {
                    syms.push(Symbol { name:name.into(), ln:start + 1, end:last + 1, depth:0,
                        kind:if callable.is_some() { if receiver(line).is_some() { "method" } else { "fn" } }
                            else if kind == Some("type") { "type" } else { "const" }.into(),
                        sig:signature(lines[i]), exp:name.chars().next().is_some_and(char::is_uppercase) });
                }
            }
        }
        i = finish + 1;
    }
    FileEntry { size:text.len() as u64, modified_ns:0, total:lines.len(), syms, imports }
}

/// Local module paths only: never guess an external package from its basename.
pub(super) fn package_dir<'a>(module: &str, import: &'a str) -> Option<&'a str> {
    if module.is_empty() { return None; }
    if import == module { Some("") } else { import.strip_prefix(module)?.strip_prefix('/') }
}

pub(super) fn resolve<'a>(index: &'a IndexView, name: &str, file: &str, qualifier: Option<&str>, module: &str, caller: &Symbol) -> Option<&'a Definition> {
    let definitions = index.defs.get(name)?;
    let entry = index.files.get(file)?;
    let own = Path::new(file).parent()?;
    let recv = receiver(&caller.sig);
    let (dir, owner) = match qualifier {
        Some(q) if recv.as_ref().is_some_and(|r| r.0 == q) => (own, recv.as_ref().map(|r| r.1.as_str())),
        Some(q) => (Path::new(package_dir(module, &entry.imports.iter().find(|i| i.name == q)?.from)?), None),
        None => (own, None),
    };
    let mut found = definitions.iter().filter(|d| d.file.ends_with(".go")
        && Path::new(&d.file).parent() == Some(dir)
        && match owner { Some(t) => receiver(&d.symbol.sig).is_some_and(|r| r.1 == t), None => d.symbol.kind != "method" });
    let first = found.next()?;
    found.next().is_none().then_some(first)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> serde_json::Map<String, Value> {
        serde_json::from_str::<Value>(include_str!("../../../../scripts/fixtures/polaris-go.json")).unwrap().as_object().unwrap().clone()
    }
    #[test]
    fn go_scanner_keeps_functions_receivers_maps_and_raw_sql() {
        let files = fixture();
        let entry = scan(files["services/entry.go"].as_str().unwrap());
        assert_eq!(entry.syms.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["Table", "Select", "Helper", "GetMetricTableSqlFieldStringBySouce"]);
        assert_eq!((entry.syms[1].ln, entry.syms[1].end), (6, 12));
        assert_eq!((entry.imports[0].name.as_str(), entry.imports[0].from.as_str()), ("sqlstore", "example.test/shop/dao"));
        let entry = scan(files["dao/metrics.go"].as_str().unwrap());
        assert_eq!(entry.syms.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["First", "Second", "metricTableAlineaPconsoleSelectField", "rawSQL", "unused", "Select", "Generic"]);
        assert_eq!((entry.syms[0].ln, entry.syms[0].end), (2,5));
        assert_eq!((entry.syms[2].ln, entry.syms[2].end), (7,10));
        assert_eq!((entry.syms[3].ln, entry.syms[3].end), (11,13));
        assert_eq!(package_dir("example.test/shop", "example.test/shopper/dao"), None);
        assert_eq!(package_dir("example.test/shop", "example.test/shop/dao"), Some("dao"));
    }
    #[test]
    #[ignore = "read-only regression against the repository and tasks supplied through NOVA_POLARIS_GO_CASES"]
    fn go_repository_recall() {
        let value: Value = serde_json::from_str(&fs::read_to_string(std::env::var("NOVA_POLARIS_GO_CASES").unwrap()).unwrap()).unwrap();
        let root = Path::new(value["root"].as_str().unwrap());
        for (n, task) in value["tasks"].as_array().unwrap().iter().enumerate() {
            let start = Instant::now();
            let bundle: Value = serde_json::from_str(&polaris(root, serde_json::json!({"task":task,"_catalog":true})).unwrap()).unwrap();
            let retrieval_ms = start.elapsed().as_millis();
            let snippets = bundle["snippets"].as_array().unwrap();
            for needle in ["func GetMetricTableSqlFieldStringBySouce(", "func getSqlFieldStringBySouce(", "metricTableAlineaPconsoleSelectField = map"] {
                assert!(snippets.iter().any(|s| s["body"].as_str().unwrap().contains(needle)), "case {n}: missing {needle}");
            }
            assert!(snippets.iter().any(|s| !s["deps"].as_array().unwrap().is_empty()));
            for s in snippets {
                let source = fs::read_to_string(root.join(s["file"].as_str().unwrap())).unwrap();
                let begin = s["start"].as_u64().unwrap() as usize;
                let count = s["end"].as_u64().unwrap() as usize - begin + 1;
                assert_eq!(s["body"].as_str().unwrap(), source.lines().skip(begin - 1).take(count).collect::<Vec<_>>().join("\n"));
            }
            println!("GO_RECALL case={n} ms={retrieval_ms} verificationMs={} snippets={} required={} catalogBytes={}", start.elapsed().as_millis() - retrieval_ms, snippets.len(), bundle["required"].as_array().unwrap().len(), bundle["text"].as_str().unwrap().len());
        }
    }
    #[test]
    fn go_catalog_recalls_late_named_definitions_and_resolves_package_aliases() {
        let d = tempfile::tempdir().unwrap();
        let files = fixture();
        for (path, text) in &files {
            let path = d.path().join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text.as_str().unwrap()).unwrap();
        }
        let task = "these ordinary words before named identifiers must not hide required definitions: GetMetricTableSqlFieldStringBySouce getSqlFieldStringBySouce metricTableAlineaPconsoleSelectField";
        let raw = polaris(d.path(), serde_json::json!({"task":task,"_catalog":true})).unwrap();
        let bundle: Value = serde_json::from_str(&raw).unwrap();
        assert!(!bundle["text"].as_str().unwrap().lines().any(|l| l.contains(" Generic [")), "unrelated definitions must not enter the model menu");
        let snippets = bundle["snippets"].as_array().unwrap();
        let find = |needle: &str| snippets.iter().find(|s| s["body"].as_str().unwrap().contains(needle)).unwrap();
        let deps = |s: &Value| s["deps"].as_array().unwrap().iter().map(|id| snippets[id.as_u64().unwrap() as usize - 1]["body"].as_str().unwrap()).collect::<Vec<_>>().join("\n");
        let method = find("func (t *Table) Select");
        let references = deps(method);
        assert!(references.contains("func Select") && references.contains("func getSqlFieldStringBySouce") && references.contains("func (t *Table) Helper"), "{references}");
        assert!(!references.contains("wrong package"));
        assert!(deps(find("func Select(metric")).contains("metricTableAlineaPconsoleSelectField = map"));
        for needle in ["func GetMetricTableSqlFieldStringBySouce", "func getSqlFieldStringBySouce", "metricTableAlineaPconsoleSelectField = map"] {
            assert!(bundle["required"].as_array().unwrap().iter().any(|id| snippets[id.as_u64().unwrap() as usize - 1]["body"].as_str().unwrap().contains(needle)), "{needle}");
        }
        let suffix: Value = serde_json::from_str(&polaris(d.path(), serde_json::json!({"task":"查询 basegame_revenue 的 SQL 映射", "_catalog":true})).unwrap()).unwrap();
        assert!(suffix["text"].as_str().unwrap().contains("const metricTableAlineaPconsoleSelectField"), "source-qualified field names must reach the model menu");
        for s in snippets {
            assert!(!s["file"].as_str().unwrap().ends_with("_test.go"));
            let source = files[s["file"].as_str().unwrap()].as_str().unwrap();
            let start = s["start"].as_u64().unwrap() as usize;
            let len = s["end"].as_u64().unwrap() as usize - start + 1;
            assert_eq!(s["body"].as_str().unwrap(), source.lines().skip(start - 1).take(len).collect::<Vec<_>>().join("\n"));
        }
    }
}

