/// Parse actual Rust/TypeScript/JavaScript declarations. In particular, a call
/// followed by an unrelated block must never masquerade as a method definition.
/// The original scanner/engine remains unchanged for exact queries and A/B.
fn scan_source(text: &str, file: &str) -> FileEntry {
    use tree_sitter::{Language, Parser};
    let language: Option<Language> = match file.rsplit('.').next() {
        Some("rs") => Some(tree_sitter_rust::LANGUAGE.into()),
        Some("tsx" | "jsx") => Some(tree_sitter_typescript::LANGUAGE_TSX.into()),
        Some("ts" | "mts" | "cts" | "js" | "mjs" | "cjs") => Some(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),
        _ => None,
    };
    let Some(language) = language else { return super::scan_source(text, file); };
    let lines = text.lines().collect::<Vec<_>>();
    let mut result = FileEntry { size: text.len() as u64, modified_ns: 0,
        total: lines.len(), syms: Vec::new(), imports: extract_imports(text, file) };
    let mut parser = Parser::new();
    if parser.set_language(&language).is_err() { return result; }
    // This bound applies to a single changed file, not a whole corpus rebuild.
    #[allow(deprecated)]
    parser.set_timeout_micros(150_000);
    let Some(tree) = parser.parse(text, None) else { return result; };
    let mut cursor = tree.walk();
    let mut depth = 0usize;
    loop {
        let node = cursor.node();
        let kind = node.kind();
        let selected = match kind {
            "function_item" | "function_declaration" | "generator_function_declaration" | "method_definition" =>
                node.child_by_field_name("name").map(|name| (name, "fn")),
            "variable_declarator" => {
                let value = node.child_by_field_name("value");
                if value.is_some_and(|n| matches!(n.kind(), "arrow_function" | "function_expression" | "generator_function")) {
                    node.child_by_field_name("name").filter(|n| n.kind() == "identifier").map(|n| (n, "fn"))
                } else { None }
            },
            "pair" => {
                let value = node.child_by_field_name("value");
                if value.is_some_and(|n| matches!(n.kind(), "arrow_function" | "function_expression")) {
                    node.child_by_field_name("key").filter(|n| matches!(n.kind(), "property_identifier" | "identifier")).map(|n| (n, "fn"))
                } else { None }
            },
            "struct_item" | "enum_item" | "trait_item" | "union_item" | "type_item" |
            "interface_declaration" | "type_alias_declaration" | "enum_declaration" =>
                node.child_by_field_name("name").map(|name| (name, "type")),
            "class_declaration" => node.child_by_field_name("name").map(|name| (name, "class")),
            "mod_item" => node.child_by_field_name("name").map(|name| (name, "mod")),
            "const_item" | "static_item" => node.child_by_field_name("name").map(|name| (name, "const")),
            _ => None,
        };
        if let Some((name, kind)) = selected {
            if !name.is_error() && !name.is_missing() {
                if let Ok(name) = name.utf8_text(text.as_bytes()) {
                    let start = node.start_position().row + 1;
                    let end = (node.end_position().row + 1).min(lines.len());
                    if !name.is_empty() && name.len() <= 200 && start <= end {
                        let line = lines.get(start - 1).copied().unwrap_or("");
                        // A top-level external Rust module is an explicit
                        // namespace binding, not a guess from a matching filename.
                        if kind == "mod" && node.child_by_field_name("body").is_none()
                            && node.parent().is_some_and(|n|n.kind()=="source_file")
                            && !lines[start.saturating_sub(4)..start-1].iter().any(|s|s.contains("#[path")) {
                            let leaf=file.rsplit('/').next().unwrap_or(file);
                            let spec=if matches!(leaf,"lib.rs"|"main.rs"|"mod.rs") {
                                format!("./{name}")
                            }else {format!("./{}/{name}",leaf.trim_end_matches(".rs"))};
                            result.imports.push(ImportRef{name:name.into(),from:spec,orig:None});
                        }
                        result.syms.push(Symbol { ln: start, end, depth: if kind == "const" { 0 } else { depth },
                            kind: kind.into(), name: name.into(), sig: signature(line),
                            exp: line.trim_start().starts_with("pub") || line.trim_start().starts_with("export") });
                    }
                }
            }
        }
        if cursor.goto_first_child() { depth += 1; continue; }
        loop {
            if cursor.goto_next_sibling() { break; }
            if !cursor.goto_parent() { result.syms.sort_by_key(|s| s.ln); return result; }
            depth = depth.saturating_sub(1);
        }
    }
}

#[cfg(test)]
mod syntax_regressions {
    use super::*;
    #[test]
    fn rust_results_and_call_expressions_are_not_methods() {
        let src = "fn make() -> Result<Foo, ()> {\n Ok(Foo { field: 1 })\n}\nfn another() {\n make();\n if true { done(); }\n}\n";
        let e = scan_source(src, "core.rs");
        assert_eq!(e.syms.iter().map(|s|s.name.as_str()).collect::<Vec<_>>(), ["make", "another"]);
        assert_eq!((e.syms[0].ln,e.syms[0].end),(1,3));
    }
    #[test]
    fn inline_generic_methods_properties_and_nested_handlers_have_real_ranges() {
        let src = "class Jobs {\n cancel<T>(id: T): void { stop(id); }\n}\nexport const api = {\n stopWork: (id: string) => invoke('cancel_work', {id}),\n};\nexport function build() {\n const retry = () => run();\n const count = 3;\n sendRequest({count});\n}\n";
        let e=scan_source(src,"jobs.ts");
        let names=e.syms.iter().map(|s|s.name.as_str()).collect::<Vec<_>>();
        for name in ["Jobs","cancel","stopWork","build","retry"] {assert!(names.contains(&name),"{name}: {names:?}");}
        for name in ["api","count","sendRequest","invoke","run"] {assert!(!names.contains(&name),"{name}: {names:?}");}
        let cancel=e.syms.iter().find(|s|s.name=="cancel").unwrap();assert_eq!((cancel.ln,cancel.end),(2,2));
    }
    #[test]
    fn strings_comments_and_jsx_do_not_add_phantom_declarations() {
        let src = "// function invented() {}\nexport function View() {\n const text = `function fake() {}`;\n return <div onClick={() => run()}>{text}</div>;\n}\n";
        let e=scan_source(src,"View.tsx");
        assert_eq!(e.syms.iter().map(|s|s.name.as_str()).collect::<Vec<_>>(),["View"]);
        assert_eq!(e.syms[0].end,5);
    }
}
