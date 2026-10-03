// Test-role classification is based on parsed Rust attribute boundaries, not
// module naming or searching for "#[test]" inside a comment/string literal.
// Preserve test source for explicit test queries, never promote it as production.
fn cfg_without_test(expression: &str) -> Option<bool> {
    if expression.len() > 16384 { return None; }
    cfg_without_test_at(expression, 0)
}
fn cfg_without_test_at(expression: &str, recursion: usize) -> Option<bool> {
    if recursion >= 64 { return None; }
    let expression = expression.trim();
    if expression == "test" { return Some(false); }
    let Some((operator, tail)) = expression.split_once('(') else { return None; };
    let inner = tail.strip_suffix(')')?;
    let mut args = Vec::new();
    let mut depth = 0usize;
    let mut quoted = false;
    let mut escaped = false;
    let mut start = 0;
    for (i, ch) in inner.char_indices() {
        if quoted {
            if escaped { escaped = false; }
            else if ch == '\\' { escaped = true; }
            else if ch == '"' { quoted = false; }
            continue;
        }
        match ch {
            '"' => quoted = true,
            '(' => depth += 1,
            ')' => depth = depth.checked_sub(1)?,
            ',' if depth == 0 => { args.push(inner[start..i].trim()); start = i + 1; }
            _ => {}
        }
    }
    if quoted || depth != 0 { return None; }
    if !inner[start..].trim().is_empty() { args.push(inner[start..].trim()); }
    let values = args.into_iter().map(|arg| cfg_without_test_at(arg, recursion + 1)).collect::<Vec<_>>();
    match operator.trim() {
        "not" if values.len() == 1 => values[0].map(|v| !v),
        "all" if values.contains(&Some(false)) => Some(false),
        "all" if values.iter().all(|v| *v == Some(true)) => Some(true),
        "any" if values.contains(&Some(true)) => Some(true),
        "any" if values.iter().all(|v| *v == Some(false)) => Some(false),
        _ => None,
    }
}
fn test_only_attribute(attribute: &str) -> bool {
    let text = attribute.trim();
    let Some(inner) = text.strip_prefix("#[").or_else(|| text.strip_prefix("#!["))
        .and_then(|s| s.strip_suffix(']')) else { return false; };
    let name = inner.split('(').next().unwrap_or("").trim();
    if matches!(name.rsplit("::").next(), Some("test" | "bench")) { return true; }
    name == "cfg" && inner.split_once('(').and_then(|(_,s)|s.strip_suffix(')'))
        .and_then(cfg_without_test) == Some(false)
}
fn rust_test_scope(mut node: tree_sitter::Node<'_>, source: &[u8]) -> bool {
    loop {
        let mut sibling = node.prev_named_sibling();
        while let Some(attribute) = sibling {
            match attribute.kind() {
                "attribute_item" => {
                    if attribute.utf8_text(source).ok().is_some_and(test_only_attribute) { return true; }
                }
                "line_comment" | "block_comment" => {}
                _ => break,
            }
            sibling = attribute.prev_named_sibling();
        }
        // Inner crate/module attributes apply to each declaration in that scope.
        if matches!(node.kind(), "source_file" | "declaration_list") {
            let mut walk = node.walk();
            for child in node.named_children(&mut walk) {
                match child.kind() {
                    "inner_attribute_item" => if child.utf8_text(source).ok().is_some_and(test_only_attribute) { return true; },
                    "line_comment" | "block_comment" => {},
                    _ => break,
                }
            }
        }
        match node.parent() { Some(parent) => node = parent, None => return false }
    }
}

#[cfg(test)]
mod test_scope_regressions {
    use super::*;
    #[test]
    fn cfg_test_is_not_a_word_search() {
        for attr in ["#[test]", "#[tokio::test(flavor = \"multi_thread\")]", "#[cfg(test)]",
            "#[cfg(all(unix, test))]", "#[cfg(all(test, feature = \"fixtures\"))]",
            "#[cfg(any(test, all(test, windows)))]"] { assert!(test_only_attribute(attr), "{attr}"); }
        for attr in ["#[cfg(not(test))]", "#[cfg(any(test, unix))]", "#[cfg(feature = \"test\")]",
            "#[cfg_attr(test, derive(Debug))]", "#[doc = \"#[test]\"]"] { assert!(!test_only_attribute(attr), "{attr}"); }
    }
    #[test]
    fn malformed_or_pathological_attributes_have_bounded_cost() {
        let deep = format!("{}test{}", "not(".repeat(80), ")".repeat(80));
        assert_eq!(cfg_without_test(&deep), None);
        assert_eq!(cfg_without_test(&" ".repeat(20000)), None);
        assert_eq!(cfg_without_test("all(test, \"unterminated)"), None);
    }
    #[test]
    fn arbitrary_test_module_names_and_top_level_tests_keep_their_role() {
        let text = r##"
pub fn actual() { let text = "#[test]"; consume(text); }
#[cfg(all(unix, test))]
mod verification {
    pub fn helper() { actual(); }
    mod nested { pub fn fixture() {} }
}
#[test]
fn direct_check() { actual(); }
#[cfg(not(test))]
pub fn normal_build() {}
"##;
        let scanned = scan_source(text, "src/service.rs");
        for name in ["helper", "fixture", "direct_check"] {
            assert!(scanned.syms.iter().any(|s|s.name == name && s.kind.starts_with("test:")), "{name}");
        }
        for name in ["actual", "normal_build"] {
            assert!(scanned.syms.iter().any(|s|s.name == name && s.kind == "fn"), "{name}");
        }
    }
    #[test]
    fn test_bodies_are_available_only_when_requested() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("engine.rs"), "pub fn production() {}\n#[cfg(test)]\nmod verification {\n // 排队请求去重\n pub fn fixture_dispatch() { fixture_only(); }\n}\n").unwrap();
        let corpus = index::corpus(dir.path(), Instant::now() + Duration::from_secs(5)).unwrap();
        assert!(corpus.units.iter().any(|u|u.name == "fixture_dispatch" && u.role == "test"));
        let ordinary = polaris(dir.path(), serde_json::json!({"task":"排队请求去重"})).unwrap();
        assert!(!ordinary.contains("fixture_only()"), "{ordinary}");
        let testing = polaris(dir.path(), serde_json::json!({"task":"排队请求去重的单元测试"})).unwrap();
        assert!(testing.contains("fixture_only()"), "{testing}");
    }
}
