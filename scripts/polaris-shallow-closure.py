"""Preserve caller type context, bound cache retention and skip non-declaration syntax."""
from pathlib import Path
R=Path('src-tauri/src/nova_tools_native')
def edit(name,old,new):
 p=R/name;s=p.read_text();assert s.count(old)==1,(name,old[:90]);p.write_text(s.replace(old,new))
edit('polaris_packet.rs','let mut frontier=std::collections::VecDeque::from([(seed,0usize)])','let mut frontier=std::collections::VecDeque::from([(seed,0usize,false)])')
edit('polaris_packet.rs','while let Some((parent,depth))=frontier.pop_front()','while let Some((parent,depth,type_only))=frontier.pop_front()')
edit('polaris_packet.rs','let rel=related(&units[parent],u,&files).or_else(||data_link(&units[parent],u,&files,&data_names).then_some("type-reference"));','let rel=(if type_only{None}else{related(&units[parent],u,&files)}).or_else(||data_link(&units[parent],u,&files,&data_names).then_some("type-reference"));')
edit('polaris_packet.rs','if matches!(role,"callee-reference"|"command-reference"){frontier.push_back((i,depth+1));}','if matches!(role,"callee-reference"|"command-reference"|"caller-reference"){frontier.push_back((i,depth+1,role=="caller-reference"));}')
edit('polaris_demand_v2.rs','    if !partial {if let Some((old,c,previous))=cache.results.get(&query_key)','    cache.results.retain(|_,(old,_,_)|old==&fingerprint);\n    if !partial {if let Some((old,c,previous))=cache.results.get(&query_key)')
edit('polaris_scan.rs','        if cursor.goto_first_child() { depth += 1; continue; }','''        // Tree-sitter still parses the file. This check only skips expression
        // traversal when a Rust body has no possible nested declaration marker.
        static NESTED:OnceLock<Regex>=OnceLock::new();
        let skip=if kind=="function_item" {
            node.child_by_field_name("body").and_then(|body|body.utf8_text(text.as_bytes()).ok()).is_some_and(|body|{
                !NESTED.get_or_init(||Regex::new(r"\\b(?:fn|struct|enum|trait|union|type|const|static|mod)\\b").unwrap()).is_match(body)
            })
        }else{matches!(kind,"struct_item"|"enum_item"|"type_item")};
        if !skip && cursor.goto_first_child() { depth += 1; continue; }''')
p=R/'polaris_scan.rs';s=p.read_text();s+='''
#[cfg(test)] mod shallow_walk_regressions {
    use super::*;
    #[test]fn nested_declarations_survive_expression_skipping(){
        let source="fn outer() {\\n fn inner() { work(); }\\n const CAP: usize = 3;\\n inner();\\n}\\nfn plain() { work(); }\\n";
        let scanned=scan_source(source,"work.rs");
        for name in ["outer","inner","CAP","plain"]{assert!(scanned.syms.iter().any(|s|s.name==name),"{name}");}
        assert!(!scanned.syms.iter().any(|s|s.name=="work"));
    }
}
''';p.write_text(s)
print('Applied caller type closure, content-version cache invalidation and declaration-only AST walk.')
